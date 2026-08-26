package config

import (
	"path/filepath"
	"testing"
	"time"
)

func TestLoadDefaults(t *testing.T) {
	t.Setenv("S3_BUCKET", "bucket")
	t.Setenv("S3_ACCESS_KEY", "key")
	t.Setenv("S3_SECRET_KEY", "secret")
	t.Setenv("S3_ENDPOINT", "")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.Addr != ":8080" || c.DataDir != "/data" || c.BaseURL != "http://localhost:8080" {
		t.Fatalf("defaults: %+v", c)
	}
	if c.S3Region != "us-east-1" || c.S3PathStyle || c.PresignExpires != 15*time.Minute || c.UploadExpires != 24*time.Hour || c.TrashRetention != 30*24*time.Hour || c.GCInterval != time.Hour || c.BlockMinSize != 1<<20 || c.BlockSize != 4<<20 || c.BlockMaxSize != 16<<20 {
		t.Fatalf("defaults: %+v", c)
	}
	if c.BlockRAMCacheCapacity != 256<<20 || c.BlockSSDCacheCapacity != 8<<30 || c.BlockCacheMinFree != 2<<30 || c.BlockReadAhead != 512<<20 || c.BlockCacheDir != filepath.Join(c.DataDir, "block-cache") {
		t.Fatalf("block cache defaults: %+v", c)
	}
	if c.FFmpegPath != "ffmpeg" {
		t.Fatalf("ffmpeg default=%q", c.FFmpegPath)
	}
	if !c.BTEnabled || c.BTListenPort != 51413 || c.BTMaxFiles != 10000 || c.BTMaxTotalSize != 1<<40 || c.BTMetadataWait != 30*time.Minute || c.BTStaleAfter != 48*time.Hour {
		t.Fatalf("torrent defaults: %+v", c)
	}
	if c.CookieSecure {
		t.Fatal("cookie must default to insecure for http base URL")
	}
	if c.S3PublicEndpoint != "" {
		t.Fatalf("public endpoint must default to empty: %q", c.S3PublicEndpoint)
	}
	if c.ProxyTransfers {
		t.Fatal("generic S3 must default to direct block uploads")
	}
}

func TestUpCloudDefaultsToProxiedBlockUploads(t *testing.T) {
	t.Setenv("S3_BUCKET", "bucket")
	t.Setenv("S3_ACCESS_KEY", "key")
	t.Setenv("S3_SECRET_KEY", "secret")
	t.Setenv("S3_ENDPOINT", "https://kj964-private.upcloudobjects.com")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if !c.IsUpCloud() || !c.ProxyTransfers {
		t.Fatalf("upcloud compatibility was not enabled: %+v", c)
	}

	// Operators can still force direct presigned uploads when they expose a
	// public endpoint and configure bucket CORS themselves.
	t.Setenv("S3_PROXY_TRANSFERS", "false")
	c, err = Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.ProxyTransfers {
		t.Fatal("explicit S3_PROXY_TRANSFERS=false was ignored")
	}
}

func TestLoadOverrides(t *testing.T) {
	t.Setenv("S3_BUCKET", "bucket")
	t.Setenv("S3_ACCESS_KEY", "key")
	t.Setenv("S3_SECRET_KEY", "secret")
	t.Setenv("APP_BASE_URL", "https://drive.example.com/")
	t.Setenv("BLOCK_SIZE", "8388608")
	t.Setenv("FASTCDC_MIN_SIZE", "2097152")
	t.Setenv("FASTCDC_MAX_SIZE", "33554432")
	t.Setenv("S3_ENDPOINT", "https://minio.example.com/")
	t.Setenv("S3_PATH_STYLE", "true")
	t.Setenv("COOKIE_SECURE", "true")
	t.Setenv("FFMPEG_PATH", "/usr/bin/ffmpeg")
	t.Setenv("S3_PUBLIC_ENDPOINT", "https://minio-public.example.com")
	t.Setenv("TRASH_RETENTION", "168h")
	t.Setenv("BLOCK_RAM_CACHE_CAPACITY", "134217728")
	t.Setenv("BLOCK_SSD_CACHE_CAPACITY", "4294967296")
	t.Setenv("BLOCK_CACHE_MIN_FREE", "1073741824")
	t.Setenv("BLOCK_READ_AHEAD", "134217728")
	t.Setenv("BLOCK_CACHE_DIR", "/cache/blocks")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.BaseURL != "https://drive.example.com" {
		t.Fatalf("base url must be trimmed: %q", c.BaseURL)
	}
	if !c.CookieSecure || !c.S3PathStyle || c.BlockMinSize != 2<<20 || c.BlockSize != 8<<20 || c.BlockMaxSize != 32<<20 || c.TrashRetention != 7*24*time.Hour || c.FFmpegPath != "/usr/bin/ffmpeg" {
		t.Fatalf("overrides: %+v", c)
	}
	if c.S3PublicEndpoint != "https://minio-public.example.com" {
		t.Fatalf("public endpoint override: %q", c.S3PublicEndpoint)
	}
	if c.BlockRAMCacheCapacity != 128<<20 || c.BlockSSDCacheCapacity != 4<<30 || c.BlockCacheMinFree != 1<<30 || c.BlockReadAhead != 128<<20 || c.BlockCacheDir != "/cache/blocks" {
		t.Fatalf("block cache overrides: %+v", c)
	}
	// 未显式配置时公网 endpoint 回退到 S3_ENDPOINT
	t.Setenv("S3_PUBLIC_ENDPOINT", "")
	c2, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c2.S3PublicEndpoint != "https://minio.example.com" {
		t.Fatalf("public endpoint fallback: %q", c2.S3PublicEndpoint)
	}
	t.Setenv("TRASH_RETENTION", "0")
	c3, err := Load()
	if err != nil || c3.TrashRetention != 0 {
		t.Fatalf("disabled trash retention=%s err=%v", c3.TrashRetention, err)
	}
}

func TestLoadValidations(t *testing.T) {
	cases := []struct {
		name   string
		mutate func(*testing.T)
	}{
		{"missing s3 credentials", func(t *testing.T) { t.Setenv("S3_ACCESS_KEY", "") }},
		{"bad block size", func(t *testing.T) { t.Setenv("BLOCK_SIZE", "1024") }},
		{"bad fastcdc min", func(t *testing.T) { t.Setenv("FASTCDC_MIN_SIZE", "1024") }},
		{"bad fastcdc max", func(t *testing.T) { t.Setenv("FASTCDC_MAX_SIZE", "1024") }},
		{"bad ram cache", func(t *testing.T) { t.Setenv("BLOCK_RAM_CACHE_CAPACITY", "-1") }},
		{"bad ssd cache", func(t *testing.T) { t.Setenv("BLOCK_SSD_CACHE_CAPACITY", "1099511627777") }},
		{"bad cache reserve", func(t *testing.T) { t.Setenv("BLOCK_CACHE_MIN_FREE", "-1") }},
		{"bad read ahead", func(t *testing.T) { t.Setenv("BLOCK_READ_AHEAD", "1024") }},
		{"bad base url", func(t *testing.T) { t.Setenv("APP_BASE_URL", "not-a-url") }},
		{"bad upload expires", func(t *testing.T) { t.Setenv("UPLOAD_EXPIRES", "0s") }},
		{"bad trash retention", func(t *testing.T) { t.Setenv("TRASH_RETENTION", "-1s") }},
		{"bad gc interval", func(t *testing.T) { t.Setenv("GC_INTERVAL", "-1s") }},
		{"bad bool", func(t *testing.T) { t.Setenv("S3_PATH_STYLE", "maybe") }},
		{"bad proxy bool", func(t *testing.T) { t.Setenv("S3_PROXY_TRANSFERS", "maybe") }},
		{"bad torrent bool", func(t *testing.T) { t.Setenv("BT_ENABLED", "maybe") }},
		{"bad torrent port", func(t *testing.T) { t.Setenv("BT_LISTEN_PORT", "80") }},
		{"bad torrent file limit", func(t *testing.T) { t.Setenv("BT_MAX_FILES", "0") }},
		{"bad torrent size limit", func(t *testing.T) { t.Setenv("BT_MAX_TOTAL_SIZE", "0") }},
		{"bad torrent metadata timeout", func(t *testing.T) { t.Setenv("BT_METADATA_TIMEOUT", "0s") }},
		{"bad torrent stale timeout", func(t *testing.T) { t.Setenv("BT_STALE_AFTER", "0s") }},
		{"oversized proxied block", func(t *testing.T) {
			t.Setenv("S3_PROXY_TRANSFERS", "true")
			t.Setenv("FASTCDC_MAX_SIZE", "134217728")
		}},
		{"bad duration", func(t *testing.T) { t.Setenv("PRESIGN_EXPIRES", "soon") }},
		{"bad endpoint", func(t *testing.T) { t.Setenv("S3_ENDPOINT", "minio://host") }},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("S3_BUCKET", "bucket")
			t.Setenv("S3_ACCESS_KEY", "key")
			t.Setenv("S3_SECRET_KEY", "secret")
			t.Setenv("APP_BASE_URL", "http://localhost:8080")
			t.Setenv("BLOCK_SIZE", "4194304")
			t.Setenv("FASTCDC_MIN_SIZE", "1048576")
			t.Setenv("FASTCDC_MAX_SIZE", "16777216")
			t.Setenv("UPLOAD_EXPIRES", "24h")
			t.Setenv("TRASH_RETENTION", "720h")
			t.Setenv("GC_INTERVAL", "1h")
			t.Setenv("S3_PATH_STYLE", "false")
			t.Setenv("S3_PROXY_TRANSFERS", "false")
			t.Setenv("PRESIGN_EXPIRES", "15m")
			t.Setenv("BT_ENABLED", "true")
			t.Setenv("BT_LISTEN_PORT", "51413")
			t.Setenv("BT_MAX_FILES", "10000")
			t.Setenv("BT_MAX_TOTAL_SIZE", "1099511627776")
			t.Setenv("BT_METADATA_TIMEOUT", "30m")
			t.Setenv("BT_STALE_AFTER", "48h")
			t.Setenv("S3_ENDPOINT", "http://minio:9000")
			tc.mutate(t)
			if _, err := Load(); err == nil {
				t.Fatal("Load must fail")
			}
		})
	}
	// 上界内的 BLOCK_SIZE 合法（重新提供凭据，清掉上面 subtest 的污染）
	t.Setenv("S3_BUCKET", "bucket")
	t.Setenv("S3_ACCESS_KEY", "key")
	t.Setenv("S3_SECRET_KEY", "secret")
	t.Setenv("BLOCK_SIZE", "1073741824")
	t.Setenv("FASTCDC_MIN_SIZE", "268435456")
	t.Setenv("FASTCDC_MAX_SIZE", "1073741824")
	t.Setenv("S3_PROXY_TRANSFERS", "false")
	t.Setenv("S3_ENDPOINT", "")
	if _, err := Load(); err != nil {
		t.Fatalf("max block size rejected: %v", err)
	}
}

func TestDatabasePath(t *testing.T) {
	c := Config{DataDir: t.TempDir()}
	want := filepath.Join(c.DataDir, "revaro.db")
	if got := c.DatabasePath(); got != want {
		t.Fatalf("database path=%q, want %q", got, want)
	}
}
