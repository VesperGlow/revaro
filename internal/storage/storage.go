package storage

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/fastcdc"
	"github.com/aws/aws-sdk-go-v2/aws"
	awsconfig "github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/credentials"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
	"github.com/aws/smithy-go"
	"golang.org/x/sync/singleflight"
)

var ErrObjectTooLarge = errors.New("object exceeds read limit")
var ErrBlockHashMismatch = errors.New("block content hash does not match block id")

type ObjectInfo struct {
	Size int64
	ETag string
}

type CompletedPart struct {
	PartNumber int32  `json:"part_number"`
	ETag       string `json:"etag"`
}

// ObjectRef describes one listed object, used by the garbage collector.
type ObjectRef struct {
	Key          string
	Size         int64
	LastModified time.Time
}

// Storage is the object-storage control plane. New logical files are opaque
// whole objects; the manifest/block methods below are read/GC compatibility
// hooks for installations that still contain pre-migration FastCDC data.
type Storage interface {
	Ping(context.Context) error

	// Opaque whole-file blobs. Multipart is transport-only: completion creates
	// one normal S3 object at key, never a persistent set of application blocks.
	PresignPutObject(context.Context, string, string, time.Duration) (string, error)
	CreateMultipart(context.Context, string, string) (string, error)
	PresignUploadPart(context.Context, string, string, int32, time.Duration) (string, error)
	CompleteMultipart(context.Context, string, string, []CompletedPart) (ObjectInfo, error)
	AbortMultipart(context.Context, string, string) error
	HeadObject(context.Context, string) (ObjectInfo, error)
	StoreBlob(context.Context, string, string, io.Reader, int64) (ObjectInfo, error)

	// Legacy blocks: variable-size chunks under blocks/xx/<sha256>.
	GetBlock(context.Context, string) ([]byte, error)
	ListBlocks(context.Context) ([]ObjectRef, error)

	// Manifests: JSON block lists under manifests/xx/<sha256-of-json>.
	PutManifest(context.Context, Manifest) (string, error)
	GetManifest(context.Context, string) (Manifest, error)
	ListManifests(context.Context) ([]ObjectRef, error)

	// Reading a legacy logical file back as a stream.
	Open(context.Context, string) (ReadSeekCloserAt, error)
	ReadFile(context.Context, string, int64) ([]byte, error)

	// Raw single objects (avatar, legacy objects, GC cleanup).
	PutObject(context.Context, string, string, []byte) (ObjectInfo, error)
	OpenRaw(context.Context, string) (io.ReadCloser, error)
	GetObject(context.Context, string, int64) ([]byte, error)
	DeleteObject(context.Context, string) error
	PresignGetObject(context.Context, string, string, string, bool, time.Duration) (string, error)
	ListPrefix(context.Context, string) ([]ObjectRef, error)
	// PutImmutable stores a content-addressed derived object (thumbnail
	// cache); an existing object is never overwritten.
	PutImmutable(context.Context, string, string, []byte) error
}

type S3 struct {
	client        *s3.Client
	presign       *s3.PresignClient
	bucket        string
	maxBlockSize  int64
	blockMinSize  int64
	blockSize     int64
	blockCacheDir string
	blockDiskCap  int64
	blockMinFree  int64
	manifestDB    *sql.DB
	legacyOnce    sync.Once
	legacyErr     error
	chunking      fastcdc.Config
	cacheOnce     sync.Once
	blockCache    *blockLRU
	diskCache     *diskBlockCache
	ramCapacity   int64
	manifests     *manifestIndex
	blockFlightMu sync.Mutex
	blockFlights  map[string]*blockDownload
	manifestGroup singleflight.Group
}

type blockDownload struct {
	ctx     context.Context
	cancel  context.CancelFunc
	done    chan struct{}
	waiters int
	data    []byte
	err     error
}

func NewS3(ctx context.Context, c config.Config) (*S3, error) {
	return newS3(ctx, c, nil)
}

// NewS3WithDB enables the persistent SQLite manifest index used by the
// application. NewS3 remains available for storage-only callers and tests.
func NewS3WithDB(ctx context.Context, c config.Config, db *sql.DB) (*S3, error) {
	return newS3(ctx, c, db)
}

func newS3(ctx context.Context, c config.Config, db *sql.DB) (*S3, error) {
	options := []func(*awsconfig.LoadOptions) error{
		awsconfig.WithRegion(c.S3Region),
		awsconfig.WithCredentialsProvider(credentials.NewStaticCredentialsProvider(c.S3AccessKey, c.S3SecretKey, "")),
	}
	if c.S3Endpoint != "" {
		options = append(options, awsconfig.WithBaseEndpoint(c.S3Endpoint))
	}
	awscfg, err := awsconfig.LoadDefaultConfig(ctx, options...)
	if err != nil {
		return nil, fmt.Errorf("load AWS config: %w", err)
	}
	// UpCloud currently rejects the AWS SDK v2 flexible-checksum trailer used
	// by default for PutObject. Limit checksums to operations that require one;
	// TLS plus revaro's SHA-256 content addressing still protects block writes.
	if c.IsUpCloud() {
		awscfg.RequestChecksumCalculation = aws.RequestChecksumCalculationWhenRequired
		awscfg.ResponseChecksumValidation = aws.ResponseChecksumValidationWhenRequired
	}
	client := s3.NewFromConfig(awscfg, func(o *s3.Options) { o.UsePathStyle = c.S3PathStyle })
	presignConfig := awscfg
	if c.S3PublicEndpoint != "" {
		presignConfig.BaseEndpoint = aws.String(c.S3PublicEndpoint)
	}
	presignClient := s3.NewFromConfig(presignConfig, func(o *s3.Options) {
		o.UsePathStyle = c.S3PathStyle
		if c.S3PublicEndpoint != "" {
			o.BaseEndpoint = aws.String(c.S3PublicEndpoint)
		}
	})
	minimum, average, maximum := c.ChunkSizes()
	return &S3{
		client: client, presign: s3.NewPresignClient(presignClient), bucket: c.S3Bucket,
		maxBlockSize: maximum, blockMinSize: minimum, blockSize: average,
		blockCacheDir: c.BlockCacheDir, blockDiskCap: c.BlockSSDCacheCapacity, blockMinFree: c.BlockCacheMinFree,
		ramCapacity: c.BlockRAMCacheCapacity, manifestDB: db,
	}, nil
}

// initLegacy initializes the FastCDC compatibility data plane on first use.
// Normal blobs/<UUID> operations never allocate block caches, scan the cache
// directory, or construct the manifest index.
func (s *S3) initLegacy() error {
	s.legacyOnce.Do(func() {
		chunking, err := fastcdc.NewConfig(int(s.blockMinSize), int(s.blockSize), int(s.maxBlockSize))
		if err != nil {
			s.legacyErr = fmt.Errorf("configure FastCDC: %w", err)
			return
		}
		diskCache, err := newDiskBlockCache(s.blockCacheDir, s.blockDiskCap, s.blockMinFree, s.maxBlockSize)
		if err != nil {
			s.legacyErr = fmt.Errorf("initialize persistent block cache: %w", err)
			return
		}
		s.chunking = chunking
		s.diskCache = diskCache
		s.manifests = newManifestIndex(s.manifestDB)
	})
	return s.legacyErr
}

func (s *S3) Ping(ctx context.Context) error {
	_, err := s.client.HeadBucket(ctx, &s3.HeadBucketInput{Bucket: aws.String(s.bucket)})
	return err
}

func (s *S3) PresignPutObject(ctx context.Context, key, mime string, expiry time.Duration) (string, error) {
	in := &s3.PutObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.presign.PresignPutObject(ctx, in, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

func (s *S3) CreateMultipart(ctx context.Context, key, mime string) (string, error) {
	in := &s3.CreateMultipartUploadInput{Bucket: aws.String(s.bucket), Key: aws.String(key)}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.client.CreateMultipartUpload(ctx, in)
	if err != nil {
		return "", err
	}
	return aws.ToString(out.UploadId), nil
}

func (s *S3) PresignUploadPart(ctx context.Context, key, uploadID string, partNumber int32, expiry time.Duration) (string, error) {
	if partNumber < 1 || partNumber > 10000 {
		return "", errors.New("multipart part number is out of range")
	}
	out, err := s.presign.PresignUploadPart(ctx, &s3.UploadPartInput{
		Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID), PartNumber: aws.Int32(partNumber),
	}, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

func (s *S3) CompleteMultipart(ctx context.Context, key, uploadID string, parts []CompletedPart) (ObjectInfo, error) {
	completed := make([]types.CompletedPart, len(parts))
	for i, part := range parts {
		completed[i] = types.CompletedPart{PartNumber: aws.Int32(part.PartNumber), ETag: aws.String(part.ETag)}
	}
	out, err := s.client.CompleteMultipartUpload(ctx, &s3.CompleteMultipartUploadInput{
		Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID),
		MultipartUpload: &types.CompletedMultipartUpload{Parts: completed},
	})
	if err != nil {
		return ObjectInfo{}, err
	}
	info, err := s.HeadObject(ctx, key)
	if err != nil {
		return ObjectInfo{}, err
	}
	if info.ETag == "" {
		info.ETag = aws.ToString(out.ETag)
	}
	return info, nil
}

func (s *S3) AbortMultipart(ctx context.Context, key, uploadID string) error {
	if uploadID == "" {
		return nil
	}
	_, err := s.client.AbortMultipartUpload(ctx, &s3.AbortMultipartUploadInput{
		Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID),
	})
	return err
}

func (s *S3) HeadObject(ctx context.Context, key string) (ObjectInfo, error) {
	out, err := s.client.HeadObject(ctx, &s3.HeadObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err != nil {
		return ObjectInfo{}, err
	}
	return ObjectInfo{Size: aws.ToInt64(out.ContentLength), ETag: strings.Trim(aws.ToString(out.ETag), `"`)}, nil
}

const (
	storeBlobMultipartThreshold = int64(16 << 20)
	storeBlobDefaultPartSize    = int64(16 << 20)
	storeBlobUnknownPartSize    = int64(128 << 20)
)

func storeBlobPartSize(size int64) (int64, error) {
	if size < 0 {
		// Start small. Subsequent unknown-size parts grow to 128 MiB so a short
		// stream does not reserve a large buffer while a 1 TiB stream still fits
		// within S3's 10,000-part allowance.
		return storeBlobDefaultPartSize, nil
	}
	partSize := storeBlobDefaultPartSize
	if size > partSize*10000 {
		partSize = ((size+9999)/10000 + (1 << 20) - 1) / (1 << 20) * (1 << 20)
	}
	if _, err := ValidMultipartPartCount(size, partSize); err != nil {
		return 0, err
	}
	return partSize, nil
}

func (s *S3) abortMultipartAfterFailure(key, uploadID string, cause error) error {
	abortCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	if err := s.AbortMultipart(abortCtx, key, uploadID); err != nil {
		return errors.Join(cause, fmt.Errorf("abort multipart upload: %w", err))
	}
	return cause
}

func (s *S3) storeBlobMultipart(ctx context.Context, key, mime string, body io.Reader, size int64) (ObjectInfo, error) {
	partSize, err := storeBlobPartSize(size)
	if err != nil {
		return ObjectInfo{}, err
	}
	uploadID, err := s.CreateMultipart(ctx, key, mime)
	if err != nil {
		return ObjectInfo{}, err
	}
	fail := func(err error) (ObjectInfo, error) {
		return ObjectInfo{}, s.abortMultipartAfterFailure(key, uploadID, err)
	}
	completed := make([]CompletedPart, 0, 8)
	buf := make([]byte, int(partSize))
	var total int64
	for partNumber := int32(1); ; partNumber++ {
		if partNumber > 10000 {
			return fail(errors.New("stream exceeds S3 multipart part limit"))
		}
		readSize := partSize
		if size < 0 && partNumber > 1 {
			readSize = storeBlobUnknownPartSize
			if int64(cap(buf)) < readSize {
				buf = make([]byte, int(readSize))
			}
		}
		if size >= 0 {
			remaining := size - total
			if remaining < 0 {
				return fail(fmt.Errorf("stream size exceeds declared size %d", size))
			}
			if remaining == 0 {
				var extra [1]byte
				n, readErr := io.ReadFull(body, extra[:])
				if n != 0 || readErr == nil {
					return fail(fmt.Errorf("stream size exceeds declared size %d", size))
				}
				if readErr != io.EOF {
					return fail(readErr)
				}
				break
			}
			readSize = min(readSize, remaining)
		}
		n, readErr := io.ReadFull(body, buf[:readSize])
		if readErr != nil && readErr != io.EOF && readErr != io.ErrUnexpectedEOF {
			return fail(readErr)
		}
		if n == 0 {
			if size >= 0 && total != size {
				return fail(fmt.Errorf("stream ended at %d bytes, want %d", total, size))
			}
			break
		}
		if size >= 0 && int64(n) != readSize {
			return fail(fmt.Errorf("stream ended at %d bytes, want %d", total+int64(n), size))
		}
		out, uploadErr := s.client.UploadPart(ctx, &s3.UploadPartInput{
			Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID), PartNumber: aws.Int32(partNumber),
			Body: bytes.NewReader(buf[:n]), ContentLength: aws.Int64(int64(n)),
		})
		if uploadErr != nil {
			return fail(uploadErr)
		}
		completed = append(completed, CompletedPart{PartNumber: partNumber, ETag: aws.ToString(out.ETag)})
		total += int64(n)
		if size < 0 && (readErr == io.EOF || readErr == io.ErrUnexpectedEOF) {
			break
		}
	}
	if len(completed) == 0 {
		if err := s.AbortMultipart(ctx, key, uploadID); err != nil {
			return ObjectInfo{}, err
		}
		return s.StoreBlob(ctx, key, mime, bytes.NewReader(nil), 0)
	}
	info, err := s.CompleteMultipart(ctx, key, uploadID, completed)
	if err != nil {
		return fail(err)
	}
	wantSize := total
	if size >= 0 {
		wantSize = size
	}
	if info.Size != wantSize {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
		defer cancel()
		_ = s.DeleteObject(cleanupCtx, key)
		return ObjectInfo{}, fmt.Errorf("stored blob size %d, want %d", info.Size, wantSize)
	}
	return info, nil
}

func (s *S3) StoreBlob(ctx context.Context, key, mime string, body io.Reader, size int64) (ObjectInfo, error) {
	if size < 0 || size >= storeBlobMultipartThreshold {
		return s.storeBlobMultipart(ctx, key, mime, body, size)
	}
	in := &s3.PutObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key), Body: body, ContentLength: aws.Int64(size)}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.client.PutObject(ctx, in)
	if err != nil {
		return ObjectInfo{}, err
	}
	info, err := s.HeadObject(ctx, key)
	if err != nil {
		return ObjectInfo{}, err
	}
	if info.Size != size {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
		defer cancel()
		_ = s.DeleteObject(cleanupCtx, key)
		return ObjectInfo{}, fmt.Errorf("stored blob size %d, want %d", info.Size, size)
	}
	if info.ETag == "" {
		info.ETag = strings.Trim(aws.ToString(out.ETag), `"`)
	}
	return info, nil
}

func BlobKey(id string) string { return "blobs/" + id }

func ValidMultipartPartCount(size, partSize int64) (int, error) {
	if size < 0 || partSize < 5<<20 {
		return 0, errors.New("invalid multipart size")
	}
	count := int((size + partSize - 1) / partSize)
	if count > 10000 {
		return 0, fmt.Errorf("multipart upload needs %s parts", strconv.Itoa(count))
	}
	return count, nil
}

// PresignBlockPut issues a conditional PUT URL for one block. The URL binds
// If-None-Match: * as a signed header and the SHA-256 checksum as a signed
// query parameter (AWS SigV4 hoists eligible x-amz-* headers). Existing
// content-addressed blocks therefore cannot be overwritten, and S3 rejects
// payloads whose checksum does not match their key.
func (s *S3) PresignBlockPut(ctx context.Context, id string, expiry time.Duration) (string, error) {
	checksum, err := BlockChecksumSHA256(id)
	if err != nil {
		return "", err
	}
	in := &s3.PutObjectInput{
		Bucket:         aws.String(s.bucket),
		Key:            aws.String(BlockKey(id)),
		ContentType:    aws.String("application/octet-stream"),
		IfNoneMatch:    aws.String("*"),
		ChecksumSHA256: aws.String(checksum),
	}
	out, err := s.presign.PresignPutObject(ctx, in, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

// PutBlock stores a block received through the application upload proxy. The
// body must match the content-addressed key so a client cannot poison a block.
func (s *S3) PutBlock(ctx context.Context, id string, data []byte) error {
	if err := s.initLegacy(); err != nil {
		return err
	}
	if !ValidBlockID(id) || hashBytes(data) != id {
		return ErrBlockHashMismatch
	}
	if int64(len(data)) > s.maxBlockSize {
		return errors.New("block exceeds configured block size")
	}
	if err := s.putConditional(ctx, BlockKey(id), blockMime, data); err != nil {
		return err
	}
	_ = s.diskCache.put(id, data)
	s.cachedBlocks().put(id, data)
	return nil
}

func (s *S3) HeadBlock(ctx context.Context, id string) (Block, error) {
	out, err := s.client.HeadObject(ctx, &s3.HeadObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(BlockKey(id))})
	if err != nil {
		return Block{}, err
	}
	return Block{ID: id, Size: aws.ToInt64(out.ContentLength)}, nil
}

func (s *S3) GetBlock(ctx context.Context, id string) ([]byte, error) {
	if err := s.initLegacy(); err != nil {
		return nil, err
	}
	return s.getBlock(ctx, Block{ID: id, Size: -1})
}

func (s *S3) getBlock(ctx context.Context, block Block) ([]byte, error) {
	if err := s.initLegacy(); err != nil {
		return nil, err
	}
	id := block.ID
	if !ValidBlockID(id) {
		return nil, fmt.Errorf("invalid block id %q", id)
	}
	cache := s.cachedBlocks()
	if data, ok := cache.get(id); ok {
		if block.Size < 0 || int64(len(data)) == block.Size {
			return data, nil
		}
	}
	if data, ok := s.diskCache.get(id, block.Size); ok {
		cache.put(id, data)
		return data, nil
	}
	return s.joinBlockDownload(ctx, block)
}

// joinBlockDownload coalesces concurrent misses without assigning ownership
// of the S3 request to an arbitrary caller. Every waiter has independent
// cancellation; the shared GET is cancelled as soon as the last waiter leaves.
func (s *S3) joinBlockDownload(ctx context.Context, block Block) ([]byte, error) {
	s.blockFlightMu.Lock()
	if s.blockFlights == nil {
		s.blockFlights = make(map[string]*blockDownload)
	}
	flight := s.blockFlights[block.ID]
	if flight == nil {
		flightCtx, cancel := context.WithCancel(context.WithoutCancel(ctx))
		flight = &blockDownload{ctx: flightCtx, cancel: cancel, done: make(chan struct{})}
		s.blockFlights[block.ID] = flight
		go s.runBlockDownload(block, flight)
	}
	flight.waiters++
	s.blockFlightMu.Unlock()

	select {
	case <-ctx.Done():
		s.leaveBlockDownload(block.ID, flight)
		return nil, ctx.Err()
	case <-flight.done:
		return flight.data, flight.err
	}
}

func (s *S3) leaveBlockDownload(id string, flight *blockDownload) {
	s.blockFlightMu.Lock()
	defer s.blockFlightMu.Unlock()
	if s.blockFlights[id] != flight {
		return
	}
	flight.waiters--
	if flight.waiters <= 0 {
		delete(s.blockFlights, id)
		flight.cancel()
	}
}

func (s *S3) runBlockDownload(block Block, flight *blockDownload) {
	flight.data, flight.err = s.fetchBlockRemote(flight.ctx, block)
	s.blockFlightMu.Lock()
	if s.blockFlights[block.ID] == flight {
		delete(s.blockFlights, block.ID)
	}
	close(flight.done)
	s.blockFlightMu.Unlock()
}

func (s *S3) fetchBlockRemote(ctx context.Context, block Block) ([]byte, error) {
	id := block.ID
	out, err := s.client.GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(BlockKey(id))})
	if err != nil {
		return nil, err
	}
	defer out.Body.Close()
	if block.Size >= 0 && out.ContentLength != nil && aws.ToInt64(out.ContentLength) != block.Size {
		return nil, fmt.Errorf("block %s size mismatch: S3 says %d bytes, manifest says %d", id, aws.ToInt64(out.ContentLength), block.Size)
	}
	data, err := io.ReadAll(io.LimitReader(out.Body, s.maxBlockSize+1))
	if err != nil {
		return nil, err
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if int64(len(data)) > s.maxBlockSize {
		return nil, fmt.Errorf("block %s exceeds configured block size", id)
	}
	if block.Size >= 0 && int64(len(data)) != block.Size {
		return nil, fmt.Errorf("block %s size mismatch: stored %d bytes, manifest says %d", id, len(data), block.Size)
	}
	if hashBytes(data) != id {
		return nil, ErrBlockHashMismatch
	}
	_ = s.diskCache.put(id, data)
	s.cachedBlocks().put(id, data)
	return data, nil
}

func (s *S3) cachedBlocks() *blockLRU {
	s.cacheOnce.Do(func() {
		if s.blockCache == nil {
			s.blockCache = newBlockLRU(s.ramCapacity)
		}
	})
	return s.blockCache
}

// putConditional stores an immutable content-addressed object. A concurrent
// upload of identical content loses the race with 412, which is treated as
// success because the object that exists is identical by construction.
func (s *S3) putConditional(ctx context.Context, key, mime string, data []byte) error {
	in := &s3.PutObjectInput{
		Bucket:        aws.String(s.bucket),
		Key:           aws.String(key),
		Body:          bytes.NewReader(data),
		ContentLength: aws.Int64(int64(len(data))),
		IfNoneMatch:   aws.String("*"),
	}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	_, err := s.client.PutObject(ctx, in)
	if err == nil {
		return nil
	}
	var apiErr smithy.APIError
	if errors.As(err, &apiErr) && apiErr.ErrorCode() == "PreconditionFailed" {
		return nil
	}
	return err
}

func (s *S3) ListBlocks(ctx context.Context) ([]ObjectRef, error) {
	return s.ListPrefix(ctx, blockPrefix)
}
func (s *S3) ListManifests(ctx context.Context) ([]ObjectRef, error) {
	return s.ListPrefix(ctx, manifestPrefix)
}

func (s *S3) ListPrefix(ctx context.Context, prefix string) ([]ObjectRef, error) {
	var out []ObjectRef
	paginator := s3.NewListObjectsV2Paginator(s.client, &s3.ListObjectsV2Input{
		Bucket: aws.String(s.bucket),
		Prefix: aws.String(prefix),
	})
	for paginator.HasMorePages() {
		page, err := paginator.NextPage(ctx)
		if err != nil {
			return nil, err
		}
		for _, obj := range page.Contents {
			out = append(out, ObjectRef{
				Key:          aws.ToString(obj.Key),
				Size:         aws.ToInt64(obj.Size),
				LastModified: aws.ToTime(obj.LastModified),
			})
		}
	}
	return out, nil
}

func (s *S3) PresignGetObject(ctx context.Context, key, filename, mime string, inline bool, expiry time.Duration) (string, error) {
	disposition := "attachment"
	if inline {
		disposition = "inline"
	}
	disposition += "; filename*=UTF-8''" + strings.ReplaceAll(url.PathEscape(filename), "+", "%20")
	in := &s3.GetObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key), ResponseContentDisposition: aws.String(disposition)}
	if mime != "" {
		in.ResponseContentType = aws.String(mime)
	}
	out, err := s.presign.PresignGetObject(ctx, in, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

func (s *S3) PutObject(ctx context.Context, key, mime string, data []byte) (ObjectInfo, error) {
	in := &s3.PutObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key), Body: bytes.NewReader(data), ContentLength: aws.Int64(int64(len(data)))}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.client.PutObject(ctx, in)
	if err != nil {
		return ObjectInfo{}, err
	}
	return ObjectInfo{Size: int64(len(data)), ETag: aws.ToString(out.ETag)}, nil
}

func (s *S3) OpenRaw(ctx context.Context, key string) (io.ReadCloser, error) {
	out, err := s.client.GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err != nil {
		return nil, err
	}
	return out.Body, nil
}

func (s *S3) GetObject(ctx context.Context, key string, limit int64) ([]byte, error) {
	body, err := s.OpenRaw(ctx, key)
	if err != nil {
		return nil, err
	}
	defer body.Close()
	data, err := io.ReadAll(io.LimitReader(body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, ErrObjectTooLarge
	}
	return data, nil
}

// IsNotFound reports whether the error means the S3 object is absent.
func IsNotFound(err error) bool {
	if err == nil {
		return false
	}
	var apiErr smithy.APIError
	if errors.As(err, &apiErr) {
		switch apiErr.ErrorCode() {
		case "NoSuchKey", "NotFound":
			return true
		}
	}
	return false
}

// PutImmutable stores a content-addressed derived object (thumbnail cache);
// concurrent identical uploads race harmlessly and existing objects are
// never overwritten.
func (s *S3) PutImmutable(ctx context.Context, key, mime string, data []byte) error {
	return s.putConditional(ctx, key, mime, data)
}

// DeleteObject is idempotent: deleting an already-absent key is not an error.
func (s *S3) DeleteObject(ctx context.Context, key string) error {
	if key == "" {
		return nil
	}
	_, err := s.client.DeleteObject(ctx, &s3.DeleteObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err == nil {
		return newManifestIndex(s.manifestDB).delete(ctx, key)
	}
	if IsNotFound(err) {
		return newManifestIndex(s.manifestDB).delete(ctx, key)
	}
	return err
}
