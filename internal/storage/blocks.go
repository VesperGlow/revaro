package storage

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/VesperGlow/revaro/internal/fastcdc"
	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

const (
	blockPrefix    = "blocks/"
	manifestPrefix = "manifests/"
	blockIDLen     = sha256.Size * 2
	manifestMime   = "application/json"
	blockMime      = "application/octet-stream"
)

// Block is one variable-size content-addressed chunk. ID is the lowercase
// hex SHA-256 of the block content, so equal content always maps to the
// same block object and deduplication falls out of the addressing scheme.
type Block struct {
	ID   string `json:"id"`
	Size int64  `json:"size"`
}

// Manifest is the Seafile "fs object" analogue: it maps one logical file
// to an ordered block list. ID is the SHA-256 of the canonical JSON form,
// so identical files share one manifest object.
type Manifest struct {
	Version int     `json:"version"`
	Size    int64   `json:"size"`
	Blocks  []Block `json:"blocks"`
}

// ValidBlockID accepts only canonical lowercase hex SHA-256 ids, so an
// uppercase variant can never address a different object than the one the
// hash actually produced.
func ValidBlockID(id string) bool {
	if len(id) != blockIDLen {
		return false
	}
	raw, err := hex.DecodeString(id)
	if err != nil {
		return false
	}
	return hex.EncodeToString(raw) == id
}

// BlockChecksumSHA256 converts the canonical hex block id into the checksum
// representation required by S3's x-amz-checksum-sha256 header.
func BlockChecksumSHA256(id string) (string, error) {
	if !ValidBlockID(id) {
		return "", errors.New("invalid block id")
	}
	raw, _ := hex.DecodeString(id)
	return base64.StdEncoding.EncodeToString(raw), nil
}

func BlockKey(id string) string     { return blockPrefix + id[:2] + "/" + id[2:] }
func ManifestKey(id string) string  { return manifestPrefix + id[:2] + "/" + id[2:] }
func IsManifestKey(key string) bool { return strings.HasPrefix(key, manifestPrefix) }

// ID returns the content hash of the canonical manifest JSON.
func (m Manifest) ID() string {
	sum := sha256.Sum256(m.bytes())
	return hex.EncodeToString(sum[:])
}

// Key returns the S3 object key this manifest is stored under.
func (m Manifest) Key() string { return ManifestKey(m.ID()) }

func (m Manifest) bytes() []byte {
	data, err := json.Marshal(m)
	if err != nil {
		// Manifest contains only string/int fields; Marshal cannot fail.
		panic(err)
	}
	return data
}

func (s *S3) PutManifest(ctx context.Context, m Manifest) (string, error) {
	if m.Version == 0 {
		m.Version = 1
	}
	if err := validateManifest(m); err != nil {
		return "", err
	}
	key := m.Key()
	if err := s.putConditional(ctx, key, manifestMime, m.bytes()); err != nil {
		return "", err
	}
	if err := s.manifests.put(ctx, key, m); err != nil {
		return "", fmt.Errorf("index manifest %s: %w", key, err)
	}
	return key, nil
}

func (s *S3) GetManifest(ctx context.Context, key string) (Manifest, error) {
	if m, ok, err := s.manifests.get(ctx, key); err == nil && ok {
		return m, nil
	} else if err != nil {
		// A partial or corrupted local row must never shadow the immutable S3
		// recovery copy. Remove it before the singleflight fallback rebuild.
		_ = s.manifests.delete(ctx, key)
	}
	result := s.manifestGroup.DoChan(key, func() (any, error) {
		if m, ok, err := s.manifests.get(ctx, key); err == nil && ok {
			return m, nil
		}
		m, err := s.getManifestRemote(ctx, key)
		if err != nil {
			return Manifest{}, err
		}
		if err := s.manifests.put(ctx, key, m); err != nil {
			return Manifest{}, fmt.Errorf("rebuild local manifest index %s: %w", key, err)
		}
		return m, nil
	})
	select {
	case <-ctx.Done():
		return Manifest{}, ctx.Err()
	case loaded := <-result:
		if loaded.Err != nil {
			return Manifest{}, loaded.Err
		}
		return loaded.Val.(Manifest), nil
	}
}

func (s *S3) getManifestRemote(ctx context.Context, key string) (Manifest, error) {
	out, err := s.client.GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err != nil {
		return Manifest{}, err
	}
	defer out.Body.Close()
	var m Manifest
	if err := json.NewDecoder(out.Body).Decode(&m); err != nil {
		return Manifest{}, fmt.Errorf("decode manifest %s: %w", key, err)
	}
	if err := validateManifest(m); err != nil {
		return Manifest{}, fmt.Errorf("manifest %s is invalid: %w", key, err)
	}
	if key != m.Key() {
		return Manifest{}, fmt.Errorf("manifest %s content hash does not match its key", key)
	}
	return m, nil
}

func validateManifest(m Manifest) error {
	if m.Version != 1 {
		return fmt.Errorf("unsupported version %d", m.Version)
	}
	if m.Size < 0 {
		return errors.New("negative size")
	}
	var total int64
	for _, b := range m.Blocks {
		if !ValidBlockID(b.ID) || b.Size <= 0 {
			return errors.New("invalid block entry")
		}
		if total > m.Size-b.Size {
			return errors.New("block sizes overflow or exceed manifest size")
		}
		total += b.Size
	}
	if total != m.Size {
		return errors.New("size does not match blocks")
	}
	return nil
}

// Store splits a stream with FastCDC, stores each block under its
// content hash (skipping blocks that already exist), then writes the
// manifest. It returns the manifest key and the manifest itself.
func (s *S3) Store(ctx context.Context, r io.Reader) (string, Manifest, error) {
	m := Manifest{Version: 1}
	chunker := fastcdc.New(r, s.chunking)
	for {
		chunk, err := chunker.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return "", Manifest{}, err
		}
		id := hashBytes(chunk)
		if err := s.putBlockIfMissing(ctx, id, chunk); err != nil {
			return "", Manifest{}, err
		}
		m.Blocks = append(m.Blocks, Block{ID: id, Size: int64(len(chunk))})
		m.Size += int64(len(chunk))
	}
	key, err := s.PutManifest(ctx, m)
	if err != nil {
		return "", Manifest{}, err
	}
	return key, m, nil
}

func hashBytes(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

// putBlockIfMissing avoids re-uploading blocks that already exist (the
// common dedup case); the conditional PUT makes the check race-safe.
func (s *S3) putBlockIfMissing(ctx context.Context, id string, data []byte) error {
	if _, err := s.HeadBlock(ctx, id); err == nil {
		return nil
	}
	return s.putConditional(ctx, BlockKey(id), blockMime, data)
}
