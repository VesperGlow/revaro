package server

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
)

const contentHashAlgorithm = "sha256"

// hashObject verifies the stored bytes with one bounded streaming pass. This
// is intentionally done before SQLite publishes a file as ready. Producers
// that already know a SHA-256 may supply it, but storage remains authoritative.
func (s *Server) hashObject(ctx context.Context, key string, expectedSize int64) (string, error) {
	_, hash, err := s.objects.Verify(ctx, key, expectedSize, "")
	return hash, err
}

func (s *Server) hashObjectRaw(ctx context.Context, key string, expectedSize int64) (string, error) {
	body, err := s.objects.Open(ctx, key)
	if err != nil {
		return "", err
	}
	defer body.Close()
	h := sha256.New()
	n, err := io.Copy(h, body)
	if err != nil {
		return "", fmt.Errorf("read object for integrity check: %w", err)
	}
	if n != expectedSize {
		return "", fmt.Errorf("integrity size mismatch: got %d want %d", n, expectedSize)
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}
