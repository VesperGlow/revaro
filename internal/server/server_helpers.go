package server

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

func validateName(name string) error {
	if name == "" || name == "." || name == ".." {
		return errors.New("invalid name")
	}
	if strings.TrimSpace(name) != name {
		return errors.New("name cannot start or end with whitespace")
	}
	if len(name) > 1024 || utf8.RuneCountInString(name) > 255 {
		return errors.New("name is too long")
	}
	if strings.ContainsAny(name, "/\\") {
		return errors.New("name cannot contain path separators")
	}
	for _, r := range name {
		if r < 32 || r == 127 {
			return errors.New("name contains control characters")
		}
	}
	return nil
}
func isPreviewable(f File) bool {
	mimeType := responseMime(f)
	if strings.HasPrefix(mimeType, "video/") || strings.HasPrefix(mimeType, "audio/") {
		return true
	}
	switch mimeType {
	case "image/jpeg", "image/png", "image/webp", "image/gif", "image/avif":
		return true
	default:
		return false
	}
}
func isConflict(err error) bool {
	return err != nil && (strings.Contains(err.Error(), "UNIQUE constraint failed") || strings.Contains(err.Error(), "constraint failed"))
}
func decodeJSON(w http.ResponseWriter, r *http.Request, dst any) error {
	return decodeJSONLimit(w, r, dst, maxJSONBody)
}
func decodeJSONLimit(w http.ResponseWriter, r *http.Request, dst any, limit int64) error {
	r.Body = http.MaxBytesReader(w, r.Body, limit)
	dec := json.NewDecoder(r.Body)
	dec.DisallowUnknownFields()
	if err := dec.Decode(dst); err != nil {
		problem(w, 400, "invalid JSON request")
		return err
	}
	if err := dec.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		problem(w, 400, "request must contain one JSON value")
		return errors.New("multiple JSON values")
	}
	return nil
}
func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
func problem(w http.ResponseWriter, status int, message string) {
	writeJSON(w, status, map[string]any{"error": map[string]any{"status": status, "message": message}})
}
func problemCode(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]any{"error": map[string]any{"status": status, "code": code, "message": message}})
}

type loginAttempt struct {
	count int
	reset time.Time
}
type loginLimiter struct {
	mu       sync.Mutex
	attempts map[string]loginAttempt
	global   loginAttempt
	slots    chan struct{}
}

const maxLoginLimiterEntries = 10000

func newLoginLimiter() *loginLimiter {
	return &loginLimiter{attempts: map[string]loginAttempt{}, slots: make(chan struct{}, 2)}
}
func (l *loginLimiter) allow(ip string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := time.Now()
	if len(l.attempts) >= maxLoginLimiterEntries {
		for key, attempt := range l.attempts {
			if now.After(attempt.reset) {
				delete(l.attempts, key)
			}
		}
	}
	if now.After(l.global.reset) {
		l.global = loginAttempt{}
	}
	if l.global.count >= 30 {
		return false
	}
	a := l.attempts[ip]
	if now.After(a.reset) {
		delete(l.attempts, ip)
		return true
	}
	return a.count < 5
}
func (l *loginLimiter) fail(ip string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := time.Now()
	a := l.attempts[ip]
	if now.After(a.reset) {
		a = loginAttempt{reset: now.Add(15 * time.Minute)}
	}
	a.count++
	if _, exists := l.attempts[ip]; exists || len(l.attempts) < maxLoginLimiterEntries {
		l.attempts[ip] = a
	}
	if now.After(l.global.reset) {
		l.global = loginAttempt{reset: now.Add(15 * time.Minute)}
	}
	l.global.count++
}
func (l *loginLimiter) success(ip string) { l.mu.Lock(); delete(l.attempts, ip); l.mu.Unlock() }
func (l *loginLimiter) acquire() bool {
	select {
	case l.slots <- struct{}{}:
		return true
	default:
		return false
	}
}
func (l *loginLimiter) release() { <-l.slots }
