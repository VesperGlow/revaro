package config

import (
	"path/filepath"
	"testing"
)

func TestLoadDefaults(t *testing.T) {
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.DataDir != "/data" || c.WorkDir != "/work" || c.MediaCacheCapacity != 2<<30 {
		t.Fatalf("defaults: %+v", c)
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
	t.Setenv("MEDIA_CACHE_CAPACITY", "-1")
	if _, err := Load(); err == nil {
		t.Fatal("invalid media cache accepted")
	}
}

func TestTrustedProxyCIDRs(t *testing.T) {
	t.Setenv("TRUSTED_PROXIES", "10.0.0.0/8, 2001:db8::/32")
	c, err := Load()
	if err != nil || len(c.TrustedProxies) != 2 {
		t.Fatalf("trusted proxies = %v, %v", c.TrustedProxies, err)
	}
	t.Setenv("TRUSTED_PROXIES", "not-a-cidr")
	if _, err := Load(); err == nil {
		t.Fatal("invalid trusted proxy CIDR accepted")
	}
}
