package config

import (
	"errors"
	"fmt"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	Addr               string
	DataDir            string
	WorkDir            string
	BaseURL            string
	CookieSecure       bool
	AdminUsername      string
	AdminPassword      string
	MediaCacheCapacity int64
	UploadExpires      time.Duration
	TrashRetention     time.Duration
	GCInterval         time.Duration
	DataPlaneAddr      string
	DataPlaneBinary    string
	FlowCacheTTL       time.Duration
	FlowCacheCapacity  int64
	TrustedProxies     []netip.Prefix
}

func Load() (Config, error) {
	c := Config{
		Addr:            env("APP_ADDR", ":8080"),
		DataDir:         env("APP_DATA_DIR", "/data"),
		BaseURL:         strings.TrimRight(env("APP_BASE_URL", "http://localhost:8080"), "/"),
		AdminUsername:   os.Getenv("ADMIN_USERNAME"),
		AdminPassword:   os.Getenv("ADMIN_PASSWORD"),
		DataPlaneAddr:   env("DATA_PLANE_ADDR", "127.0.0.1:7081"),
		DataPlaneBinary: env("DATA_PLANE_BINARY", "revaro-data-plane"),
	}
	c.WorkDir = env("APP_WORK_DIR", "/work")
	if raw := strings.TrimSpace(os.Getenv("TRUSTED_PROXIES")); raw != "" {
		for _, item := range strings.Split(raw, ",") {
			prefix, parseErr := netip.ParsePrefix(strings.TrimSpace(item))
			if parseErr != nil {
				return c, fmt.Errorf("TRUSTED_PROXIES contains invalid CIDR %q", item)
			}
			c.TrustedProxies = append(c.TrustedProxies, prefix.Masked())
		}
	}
	var err error
	if c.CookieSecure, err = boolEnv("COOKIE_SECURE", strings.HasPrefix(c.BaseURL, "https://")); err != nil {
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
	if c.MediaCacheCapacity, err = int64Env("MEDIA_CACHE_CAPACITY", 2*1024*1024*1024); err != nil {
		return c, err
	}
	if c.FlowCacheTTL, err = durationEnv("FLOW_CACHE_TTL", 720*time.Hour); err != nil {
		return c, err
	}
	if c.FlowCacheCapacity, err = int64Env("FLOW_CACHE_CAPACITY", 1<<30); err != nil {
		return c, err
	}
	if c.WorkDir == "" {
		return c, errors.New("APP_WORK_DIR must not be empty")
	}
	if c.MediaCacheCapacity < 0 || c.MediaCacheCapacity > 1<<40 {
		return c, errors.New("MEDIA_CACHE_CAPACITY must be between 0 and 1 TiB")
	}
	if c.FlowCacheTTL < 0 {
		return c, errors.New("FLOW_CACHE_TTL must not be negative")
	}
	if c.FlowCacheCapacity < 0 || c.FlowCacheCapacity > 1<<40 {
		return c, errors.New("FLOW_CACHE_CAPACITY must be between 0 and 1 TiB")
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
	base, err := url.Parse(c.BaseURL)
	if err != nil || base.Host == "" || (base.Scheme != "http" && base.Scheme != "https") {
		return c, errors.New("APP_BASE_URL must be an absolute http(s) URL")
	}
	return c, nil
}

func (c Config) DatabasePath() string { return filepath.Join(c.DataDir, "revaro.db") }

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
