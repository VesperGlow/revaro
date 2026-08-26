package config

import (
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	Addr             string
	DataDir          string
	BaseURL          string
	CookieSecure     bool
	AdminUsername    string
	AdminPassword    string
	S3Endpoint       string
	S3PublicEndpoint string
	S3Region         string
	S3Bucket         string
	S3AccessKey      string
	S3SecretKey      string
	S3PathStyle      bool
	ProxyTransfers   bool
	PresignExpires   time.Duration
	// BlockSize is the target average FastCDC chunk size. BlockMinSize and
	// BlockMaxSize bound the variable-size chunks around that target.
	BlockMinSize          int64
	BlockSize             int64
	BlockMaxSize          int64
	BlockRAMCacheCapacity int64
	BlockSSDCacheCapacity int64
	BlockCacheMinFree     int64
	BlockReadAhead        int64
	BlockCacheDir         string
	UploadExpires         time.Duration
	TrashRetention        time.Duration
	GCInterval            time.Duration
	FFmpegPath            string
	BTEnabled             bool
	BTListenPort          int
	BTMaxFiles            int
	BTMaxTotalSize        int64
	BTMetadataWait        time.Duration
	BTStaleAfter          time.Duration
}

func Load() (Config, error) {
	c := Config{
		Addr:             env("APP_ADDR", ":8080"),
		DataDir:          env("APP_DATA_DIR", "/data"),
		BaseURL:          strings.TrimRight(env("APP_BASE_URL", "http://localhost:8080"), "/"),
		AdminUsername:    os.Getenv("ADMIN_USERNAME"),
		AdminPassword:    os.Getenv("ADMIN_PASSWORD"),
		S3Endpoint:       strings.TrimRight(os.Getenv("S3_ENDPOINT"), "/"),
		S3PublicEndpoint: strings.TrimRight(os.Getenv("S3_PUBLIC_ENDPOINT"), "/"),
		S3Region:         env("S3_REGION", "us-east-1"),
		S3Bucket:         os.Getenv("S3_BUCKET"),
		S3AccessKey:      os.Getenv("S3_ACCESS_KEY"),
		S3SecretKey:      os.Getenv("S3_SECRET_KEY"),
		FFmpegPath:       os.Getenv("FFMPEG_PATH"),
	}
	if c.FFmpegPath == "" {
		c.FFmpegPath = "ffmpeg"
	}
	var err error
	if c.CookieSecure, err = boolEnv("COOKIE_SECURE", strings.HasPrefix(c.BaseURL, "https://")); err != nil {
		return c, err
	}
	if c.S3PathStyle, err = boolEnv("S3_PATH_STYLE", false); err != nil {
		return c, err
	}
	if c.ProxyTransfers, err = boolEnv("S3_PROXY_TRANSFERS", c.IsUpCloud()); err != nil {
		return c, err
	}
	if c.PresignExpires, err = durationEnv("PRESIGN_EXPIRES", 15*time.Minute); err != nil {
		return c, err
	}
	if c.UploadExpires, err = durationEnv("UPLOAD_EXPIRES", 24*time.Hour); err != nil {
		return c, err
	}
	if c.TrashRetention, err = durationEnv("TRASH_RETENTION", 30*24*time.Hour); err != nil {
		return c, err
	}
	if c.GCInterval, err = durationEnv("GC_INTERVAL", time.Hour); err != nil {
		return c, err
	}
	if c.BTEnabled, err = boolEnv("BT_ENABLED", true); err != nil {
		return c, err
	}
	btPort, err := int64Env("BT_LISTEN_PORT", 51413)
	if err != nil {
		return c, err
	}
	c.BTListenPort = int(btPort)
	btFiles, err := int64Env("BT_MAX_FILES", 10000)
	if err != nil {
		return c, err
	}
	c.BTMaxFiles = int(btFiles)
	if c.BTMaxTotalSize, err = int64Env("BT_MAX_TOTAL_SIZE", 1<<40); err != nil {
		return c, err
	}
	if c.BTMetadataWait, err = durationEnv("BT_METADATA_TIMEOUT", 30*time.Minute); err != nil {
		return c, err
	}
	if c.BTStaleAfter, err = durationEnv("BT_STALE_AFTER", 48*time.Hour); err != nil {
		return c, err
	}
	if c.BlockSize, err = int64Env("BLOCK_SIZE", 4*1024*1024); err != nil {
		return c, err
	}
	if c.BlockMinSize, err = int64Env("FASTCDC_MIN_SIZE", c.BlockSize/4); err != nil {
		return c, err
	}
	defaultMax := min(c.BlockSize*4, int64(1024*1024*1024))
	if c.BlockMaxSize, err = int64Env("FASTCDC_MAX_SIZE", defaultMax); err != nil {
		return c, err
	}
	if c.BlockRAMCacheCapacity, err = int64Env("BLOCK_RAM_CACHE_CAPACITY", 256*1024*1024); err != nil {
		return c, err
	}
	if c.BlockSSDCacheCapacity, err = int64Env("BLOCK_SSD_CACHE_CAPACITY", 8*1024*1024*1024); err != nil {
		return c, err
	}
	if c.BlockCacheMinFree, err = int64Env("BLOCK_CACHE_MIN_FREE", 2*1024*1024*1024); err != nil {
		return c, err
	}
	if c.BlockReadAhead, err = int64Env("BLOCK_READ_AHEAD", 512*1024*1024); err != nil {
		return c, err
	}
	c.BlockCacheDir = env("BLOCK_CACHE_DIR", filepath.Join(c.DataDir, "block-cache"))
	if c.BlockSize < 1*1024*1024 || c.BlockSize > 1024*1024*1024 {
		return c, errors.New("BLOCK_SIZE must be between 1 MiB and 1 GiB")
	}
	if c.BlockMinSize < 64*1024 || c.BlockMinSize > c.BlockSize {
		return c, errors.New("FASTCDC_MIN_SIZE must be between 64 KiB and BLOCK_SIZE")
	}
	if c.BlockMaxSize < c.BlockSize || c.BlockMaxSize > 1024*1024*1024 {
		return c, errors.New("FASTCDC_MAX_SIZE must be between BLOCK_SIZE and 1 GiB")
	}
	if c.ProxyTransfers && c.BlockMaxSize > 64*1024*1024 {
		return c, errors.New("FASTCDC_MAX_SIZE must not exceed 64 MiB when S3_PROXY_TRANSFERS is enabled")
	}
	if c.BlockRAMCacheCapacity < 0 || c.BlockRAMCacheCapacity > 16*1024*1024*1024 {
		return c, errors.New("BLOCK_RAM_CACHE_CAPACITY must be between 0 and 16 GiB")
	}
	if c.BlockSSDCacheCapacity < 0 || c.BlockSSDCacheCapacity > 1<<40 {
		return c, errors.New("BLOCK_SSD_CACHE_CAPACITY must be between 0 and 1 TiB")
	}
	if c.BlockCacheMinFree < 0 || c.BlockCacheMinFree > 1<<40 {
		return c, errors.New("BLOCK_CACHE_MIN_FREE must be between 0 and 1 TiB")
	}
	if c.BlockReadAhead < 0 || c.BlockReadAhead > 1024*1024*1024 || (c.BlockReadAhead > 0 && c.BlockReadAhead < 1024*1024) {
		return c, errors.New("BLOCK_READ_AHEAD must be 0 or between 1 MiB and 1 GiB")
	}
	if c.BlockCacheDir == "" {
		return c, errors.New("BLOCK_CACHE_DIR must not be empty")
	}
	if c.UploadExpires <= 0 {
		return c, errors.New("UPLOAD_EXPIRES must be positive")
	}
	if c.TrashRetention < 0 {
		return c, errors.New("TRASH_RETENTION must not be negative")
	}
	if c.GCInterval < 0 {
		return c, errors.New("GC_INTERVAL must not be negative")
	}
	if c.BTListenPort < 1024 || c.BTListenPort > 65535 {
		return c, errors.New("BT_LISTEN_PORT must be between 1024 and 65535")
	}
	if c.BTMaxFiles < 1 || c.BTMaxFiles > 100000 {
		return c, errors.New("BT_MAX_FILES must be between 1 and 100000")
	}
	if c.BTMaxTotalSize < 1 || c.BTMaxTotalSize > 1<<40 {
		return c, errors.New("BT_MAX_TOTAL_SIZE must be between 1 byte and 1 TiB")
	}
	if c.BTMetadataWait <= 0 || c.BTStaleAfter <= 0 {
		return c, errors.New("BT_METADATA_TIMEOUT and BT_STALE_AFTER must be positive")
	}
	base, err := url.Parse(c.BaseURL)
	if err != nil || base.Host == "" || (base.Scheme != "http" && base.Scheme != "https") {
		return c, errors.New("APP_BASE_URL must be an absolute http(s) URL")
	}
	if c.S3Bucket == "" || c.S3AccessKey == "" || c.S3SecretKey == "" {
		return c, errors.New("S3_BUCKET, S3_ACCESS_KEY and S3_SECRET_KEY are required")
	}
	if c.S3PublicEndpoint == "" {
		c.S3PublicEndpoint = c.S3Endpoint
	}
	for name, endpoint := range map[string]string{"S3_ENDPOINT": c.S3Endpoint, "S3_PUBLIC_ENDPOINT": c.S3PublicEndpoint} {
		if endpoint == "" {
			continue
		}
		u, parseErr := url.Parse(endpoint)
		if parseErr != nil || u.Host == "" || (u.Scheme != "http" && u.Scheme != "https") {
			return c, fmt.Errorf("%s must be an absolute http(s) URL", name)
		}
	}
	return c, nil
}

func (c Config) DatabasePath() string { return filepath.Join(c.DataDir, "revaro.db") }

// IsUpCloud reports whether the configured S3 endpoint belongs to UpCloud.
// Both public and private Managed Object Storage endpoints use this suffix.
func (c Config) IsUpCloud() bool {
	u, err := url.Parse(c.S3Endpoint)
	if err != nil {
		return false
	}
	host := strings.ToLower(strings.TrimSuffix(u.Hostname(), "."))
	return host == "upcloudobjects.com" || strings.HasSuffix(host, ".upcloudobjects.com")
}

// ChunkSizes also supplies useful defaults for tests and embedded callers
// that construct Config directly instead of using Load.
func (c Config) ChunkSizes() (minimum, average, maximum int64) {
	average = c.BlockSize
	if average <= 0 {
		average = 4 * 1024 * 1024
	}
	minimum = c.BlockMinSize
	if minimum <= 0 {
		minimum = max(1, average/4)
	}
	maximum = c.BlockMaxSize
	if maximum <= 0 {
		maximum = min(average*4, int64(1024*1024*1024))
	}
	return minimum, average, maximum
}

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}
func boolEnv(name string, fallback bool) (bool, error) {
	v := os.Getenv(name)
	if v == "" {
		return fallback, nil
	}
	b, err := strconv.ParseBool(v)
	if err != nil {
		return false, fmt.Errorf("%s: %w", name, err)
	}
	return b, nil
}
func durationEnv(name string, fallback time.Duration) (time.Duration, error) {
	v := os.Getenv(name)
	if v == "" {
		return fallback, nil
	}
	d, err := time.ParseDuration(v)
	if err != nil {
		return 0, fmt.Errorf("%s: %w", name, err)
	}
	return d, nil
}
func int64Env(name string, fallback int64) (int64, error) {
	v := os.Getenv(name)
	if v == "" {
		return fallback, nil
	}
	n, err := strconv.ParseInt(v, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("%s: %w", name, err)
	}
	return n, nil
}
