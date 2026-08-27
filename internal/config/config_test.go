package config

import (
	"path/filepath"
	"testing"
	"time"
)

func validEnv(t *testing.T) {
	t.Helper()
	t.Setenv("S3_BUCKET", "bucket")
	t.Setenv("S3_ACCESS_KEY", "key")
	t.Setenv("S3_SECRET_KEY", "secret")
	t.Setenv("S3_ENDPOINT", "")
}
func TestLoadDefaults(t *testing.T) {
	validEnv(t)
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.DataDir != "/data" || c.WorkDir != "/data/work" || c.MediaCacheCapacity != 2<<30 || c.PresignExpires != 15*time.Minute {
		t.Fatalf("defaults: %+v", c)
	}
}
func TestLegacyStorageEnvironmentIsIgnored(t *testing.T) {
	validEnv(t)
	t.Setenv("BLOCK_SIZE", "invalid")
	t.Setenv("FASTCDC_MIN_SIZE", "invalid")
	t.Setenv("BLOCK_RAM_CACHE_CAPACITY", "invalid")
	if _, err := Load(); err != nil {
		t.Fatalf("retired storage settings affected startup: %v", err)
	}
}
func TestDatabasePath(t *testing.T) {
	c := Config{DataDir: t.TempDir()}
	want := filepath.Join(c.DataDir, "revaro.db")
	if got := c.DatabasePath(); got != want {
		t.Fatalf("database path=%q want %q", got, want)
	}
}
func TestLoadRejectsInvalidActiveSettings(t *testing.T) {
	validEnv(t)
	t.Setenv("MEDIA_CACHE_CAPACITY", "-1")
	if _, err := Load(); err == nil {
		t.Fatal("invalid media cache accepted")
	}
}
