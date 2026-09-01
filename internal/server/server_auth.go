package server

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/base64"
	"errors"
	"image/png"
	"net"
	"net/http"
	"net/netip"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/auth"
	"github.com/pquerna/otp"
)

func (s *Server) login(w http.ResponseWriter, r *http.Request) {
	ip := s.clientIP(r)
	if !s.limiter.allow(ip) {
		w.Header().Set("Retry-After", "60")
		problem(w, http.StatusTooManyRequests, "too many login attempts; try again later")
		return
	}
	var in struct {
		Username     string `json:"username"`
		Password     string `json:"password"`
		SecondFactor string `json:"second_factor"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if len(in.Username) > 128 || len(in.Password) > 1024 || len(in.SecondFactor) > 128 {
		s.limiter.fail(ip)
		problem(w, http.StatusUnauthorized, "invalid credentials")
		return
	}
	if !s.limiter.acquire() {
		w.Header().Set("Retry-After", "2")
		problem(w, http.StatusTooManyRequests, "login verification is busy; try again shortly")
		return
	}
	defer s.limiter.release()
	token, expires, err := s.auth.Login(r.Context(), in.Username, in.Password, in.SecondFactor)
	if err != nil {
		switch {
		case errors.Is(err, auth.ErrTOTPRequired):
			problemCode(w, http.StatusUnauthorized, "totp_required", "enter your authenticator or recovery code")
		case errors.Is(err, auth.ErrInvalidSecondFactor):
			s.limiter.fail(ip)
			problemCode(w, http.StatusUnauthorized, "invalid_second_factor", "the authenticator or recovery code is incorrect")
		case errors.Is(err, auth.ErrInvalidCredentials):
			s.limiter.fail(ip)
			problem(w, http.StatusUnauthorized, "invalid credentials")
		default:
			s.log.Error("login failed", "error", err)
			problem(w, http.StatusInternalServerError, "could not verify login")
		}
		return
	}
	s.limiter.success(ip)
	http.SetCookie(w, &http.Cookie{Name: "revaro_session", Value: token, Path: "/", HttpOnly: true, Secure: s.cfg.CookieSecure, SameSite: http.SameSiteLaxMode, Expires: expires, MaxAge: int(time.Until(expires).Seconds())})
	s.log.Info("user logged in", "user", in.Username)
	writeJSON(w, http.StatusOK, map[string]any{"username": in.Username, "has_avatar": s.hasAvatar(r.Context())})
}

func (s *Server) clientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		host = r.RemoteAddr
	}
	peer, err := netip.ParseAddr(strings.TrimSpace(host))
	if err != nil || !prefixContains(s.cfg.TrustedProxies, peer.Unmap()) {
		return host
	}
	// Walk right-to-left: trusted proxies append their peer. The first
	// untrusted address is the client; spoofed values farther left are ignored.
	chain := strings.Split(r.Header.Get("X-Forwarded-For"), ",")
	for i := len(chain) - 1; i >= 0; i-- {
		candidate, parseErr := netip.ParseAddr(strings.TrimSpace(chain[i]))
		if parseErr != nil {
			continue
		}
		candidate = candidate.Unmap()
		if !prefixContains(s.cfg.TrustedProxies, candidate) {
			return candidate.String()
		}
	}
	return peer.Unmap().String()
}

func prefixContains(prefixes []netip.Prefix, addr netip.Addr) bool {
	for _, prefix := range prefixes {
		if prefix.Contains(addr) {
			return true
		}
	}
	return false
}
func (s *Server) logout(w http.ResponseWriter, r *http.Request) {
	if c, err := r.Cookie("revaro_session"); err == nil {
		s.auth.Logout(r.Context(), c.Value)
	}
	s.clearSessionCookie(w)
	w.WriteHeader(http.StatusNoContent)
}
func (s *Server) me(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"username": r.Context().Value(userKey{}).(string), "has_avatar": s.hasAvatar(r.Context())})
}

func (s *Server) totpStatus(w http.ResponseWriter, r *http.Request) {
	status, err := s.auth.TOTPStatus(r.Context())
	if err != nil {
		s.log.Error("TOTP status read failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not read two-factor settings")
		return
	}
	writeJSON(w, http.StatusOK, status)
}

func (s *Server) beginTOTPSetup(w http.ResponseWriter, r *http.Request) {
	var in struct {
		CurrentPassword string `json:"current_password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.CurrentPassword == "" || len(in.CurrentPassword) > 1024 {
		problem(w, http.StatusBadRequest, "current password is required")
		return
	}
	username := r.Context().Value(userKey{}).(string)
	setup, err := s.auth.BeginTOTPSetup(r.Context(), username, in.CurrentPassword)
	if err != nil {
		s.totpProblem(w, err)
		return
	}
	key, err := otp.NewKeyFromURL(setup.URI)
	if err != nil {
		s.log.Error("TOTP QR key creation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not create authenticator QR code")
		return
	}
	image, err := key.Image(256, 256)
	if err != nil {
		s.log.Error("TOTP QR image creation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not create authenticator QR code")
		return
	}
	var pngData bytes.Buffer
	if err := png.Encode(&pngData, image); err != nil {
		s.log.Error("TOTP QR encoding failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not create authenticator QR code")
		return
	}
	writeJSON(w, http.StatusCreated, map[string]any{
		"secret":      setup.Secret,
		"uri":         setup.URI,
		"qr_data_url": "data:image/png;base64," + base64.StdEncoding.EncodeToString(pngData.Bytes()),
	})
}

type totpConfirmation struct {
	CurrentPassword string `json:"current_password"`
	Code            string `json:"code"`
}

func (s *Server) enableTOTP(w http.ResponseWriter, r *http.Request) {
	var in totpConfirmation
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if !validTOTPConfirmation(in) {
		problem(w, http.StatusBadRequest, "current password and verification code are required")
		return
	}
	codes, err := s.auth.ConfirmTOTPSetup(r.Context(), r.Context().Value(userKey{}).(string), in.CurrentPassword, in.Code, currentSessionToken(r))
	if err != nil {
		s.totpProblem(w, err)
		return
	}
	s.log.Info("TOTP two-factor authentication enabled", "user", r.Context().Value(userKey{}).(string))
	writeJSON(w, http.StatusOK, map[string]any{"enabled": true, "recovery_codes": codes})
}

func (s *Server) regenerateTOTPRecoveryCodes(w http.ResponseWriter, r *http.Request) {
	var in totpConfirmation
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if !validTOTPConfirmation(in) {
		problem(w, http.StatusBadRequest, "current password and verification code are required")
		return
	}
	codes, err := s.auth.RegenerateRecoveryCodes(r.Context(), r.Context().Value(userKey{}).(string), in.CurrentPassword, in.Code)
	if err != nil {
		s.totpProblem(w, err)
		return
	}
	s.log.Info("TOTP recovery codes regenerated", "user", r.Context().Value(userKey{}).(string))
	writeJSON(w, http.StatusOK, map[string]any{"enabled": true, "recovery_codes": codes})
}

func (s *Server) disableTOTP(w http.ResponseWriter, r *http.Request) {
	var in totpConfirmation
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if !validTOTPConfirmation(in) {
		problem(w, http.StatusBadRequest, "current password and verification code are required")
		return
	}
	username := r.Context().Value(userKey{}).(string)
	if err := s.auth.DisableTOTP(r.Context(), username, in.CurrentPassword, in.Code, currentSessionToken(r)); err != nil {
		s.totpProblem(w, err)
		return
	}
	s.log.Info("TOTP two-factor authentication disabled", "user", username)
	w.WriteHeader(http.StatusNoContent)
}

func validTOTPConfirmation(in totpConfirmation) bool {
	return in.CurrentPassword != "" && len(in.CurrentPassword) <= 1024 && strings.TrimSpace(in.Code) != "" && len(in.Code) <= 128
}

func currentSessionToken(r *http.Request) string {
	if cookie, err := r.Cookie("revaro_session"); err == nil {
		return cookie.Value
	}
	return ""
}

func (s *Server) totpProblem(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, auth.ErrInvalidCredentials):
		problem(w, http.StatusUnauthorized, "current password is incorrect")
	case errors.Is(err, auth.ErrInvalidSecondFactor):
		problem(w, http.StatusUnauthorized, "the authenticator or recovery code is incorrect")
	case errors.Is(err, auth.ErrTOTPAlreadyEnabled):
		problem(w, http.StatusConflict, "two-factor authentication is already enabled")
	case errors.Is(err, auth.ErrTOTPNotEnabled):
		problem(w, http.StatusConflict, "two-factor authentication is not enabled")
	case errors.Is(err, auth.ErrTOTPSetupExpired):
		problem(w, http.StatusGone, "two-factor setup expired; start again")
	default:
		s.log.Error("TOTP operation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update two-factor settings")
	}
}

func (s *Server) hasAvatar(ctx context.Context) bool {
	var value string
	return s.db.QueryRowContext(ctx, `SELECT value FROM settings WHERE key='avatar_mime'`).Scan(&value) == nil && value != ""
}

func (s *Server) getAvatar(w http.ResponseWriter, r *http.Request) {
	var contentType string
	if err := s.db.QueryRowContext(r.Context(), `SELECT value FROM settings WHERE key='avatar_mime'`).Scan(&contentType); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			problem(w, http.StatusNotFound, "avatar not found")
		} else {
			problem(w, http.StatusInternalServerError, "database error")
		}
		return
	}
	data, err := s.objects.Get(r.Context(), avatarObjectKey, maxAvatarBytes)
	if err != nil {
		s.log.Error("avatar read failed", "error", err)
		problem(w, http.StatusBadGateway, "could not read avatar")
		return
	}
	w.Header().Set("Content-Type", contentType)
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.Header().Set("Content-Disposition", "inline")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

func (s *Server) updateAvatar(w http.ResponseWriter, r *http.Request) {
	var in struct {
		DataURL string `json:"data_url"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	comma := strings.IndexByte(in.DataURL, ',')
	if comma < 0 || !strings.HasPrefix(in.DataURL, "data:image/") {
		problem(w, http.StatusBadRequest, "avatar must be a data URL")
		return
	}
	data, err := base64.StdEncoding.DecodeString(in.DataURL[comma+1:])
	if err != nil || len(data) == 0 {
		problem(w, http.StatusBadRequest, "avatar data is invalid")
		return
	}
	if len(data) > maxAvatarBytes {
		problem(w, http.StatusRequestEntityTooLarge, "avatar must not exceed 2 MiB")
		return
	}
	contentType := http.DetectContentType(data)
	switch contentType {
	case "image/jpeg", "image/png", "image/gif", "image/webp":
	default:
		problem(w, http.StatusUnsupportedMediaType, "avatar must be JPEG, PNG, GIF, or WebP")
		return
	}
	if _, err := s.objects.Put(r.Context(), avatarObjectKey, contentType, data); err != nil {
		s.log.Error("avatar write failed", "error", err)
		problem(w, http.StatusBadGateway, "could not save avatar")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := s.db.ExecContext(r.Context(), `INSERT INTO settings(key,value,updated_at) VALUES('avatar_mime',?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at`, contentType, now); err != nil {
		s.log.Error("avatar metadata write failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not save avatar")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) deleteAvatar(w http.ResponseWriter, r *http.Request) {
	if err := s.objects.Delete(r.Context(), avatarObjectKey, "avatar deletion"); err != nil {
		s.log.Error("avatar delete failed", "error", err)
		problem(w, http.StatusBadGateway, "could not delete avatar")
		return
	}
	if _, err := s.db.ExecContext(r.Context(), `DELETE FROM settings WHERE key='avatar_mime'`); err != nil {
		problem(w, http.StatusInternalServerError, "could not delete avatar")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) changeCredentials(w http.ResponseWriter, r *http.Request) {
	var in struct {
		CurrentPassword string `json:"current_password"`
		Username        string `json:"username"`
		Password        string `json:"password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Username == "" || len(in.Username) > 128 || len(in.Password) < 12 || len(in.Password) > 1024 || len(in.CurrentPassword) > 1024 {
		problem(w, http.StatusBadRequest, "username is required and password must be between 12 and 1024 characters")
		return
	}
	currentUsername := r.Context().Value(userKey{}).(string)
	if err := s.auth.ChangeCredentials(r.Context(), currentUsername, in.CurrentPassword, in.Username, in.Password); err != nil {
		if errors.Is(err, auth.ErrInvalidCredentials) {
			problem(w, http.StatusUnauthorized, "current password is incorrect")
			return
		}
		s.log.Error("credential change failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update credentials")
		return
	}
	s.clearSessionCookie(w)
	s.log.Info("administrator credentials changed", "previous_user", currentUsername, "user", in.Username)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) changePassword(w http.ResponseWriter, r *http.Request) {
	var in struct {
		CurrentPassword string `json:"current_password"`
		Password        string `json:"password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if len(in.Password) < 12 || len(in.Password) > 1024 || len(in.CurrentPassword) > 1024 {
		problem(w, http.StatusBadRequest, "password must be between 12 and 1024 characters")
		return
	}
	username := r.Context().Value(userKey{}).(string)
	if err := s.auth.ChangeCredentials(r.Context(), username, in.CurrentPassword, username, in.Password); err != nil {
		if errors.Is(err, auth.ErrInvalidCredentials) {
			problem(w, http.StatusUnauthorized, "current password is incorrect")
			return
		}
		s.log.Error("password change failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update password")
		return
	}
	s.clearSessionCookie(w)
	s.log.Info("administrator password changed", "user", username)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) changeUsername(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Username string `json:"username"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	in.Username = strings.TrimSpace(in.Username)
	if in.Username == "" || len(in.Username) > 128 {
		problem(w, http.StatusBadRequest, "username must be between 1 and 128 characters")
		return
	}
	previous := r.Context().Value(userKey{}).(string)
	if err := s.auth.ChangeUsername(r.Context(), in.Username); err != nil {
		s.log.Error("username change failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update username")
		return
	}
	s.log.Info("administrator username changed", "previous_user", previous, "user", in.Username)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) clearSessionCookie(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{Name: "revaro_session", Value: "", Path: "/", HttpOnly: true, Secure: s.cfg.CookieSecure, SameSite: http.SameSiteLaxMode, MaxAge: -1, Expires: time.Unix(1, 0)})
}

type userKey struct{}

func (s *Server) requireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c, err := r.Cookie("revaro_session")
		if err != nil {
			problem(w, http.StatusUnauthorized, "authentication required")
			return
		}
		user, err := s.auth.Authenticate(r.Context(), c.Value)
		if err != nil {
			problem(w, http.StatusUnauthorized, "authentication required")
			return
		}
		next.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), userKey{}, user)))
	})
}
