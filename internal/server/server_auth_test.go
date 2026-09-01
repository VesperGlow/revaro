package server

import (
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/auth"
	"github.com/pquerna/otp/totp"
)

func TestChangeCredentialsRequiresCurrentPasswordAndRevokesSession(t *testing.T) {
	a := newTestApp(t)
	wrong := a.request("PATCH", "/api/auth/credentials", map[string]any{"current_password": "wrong-password", "username": "owner", "password": "a-new-secure-password"}, true)
	if wrong.Code != http.StatusUnauthorized {
		t.Fatalf("wrong current password status=%d: %s", wrong.Code, wrong.Body.String())
	}
	changed := a.request("PATCH", "/api/auth/credentials", map[string]any{"current_password": "a-secure-test-password", "username": "owner", "password": "a-new-secure-password"}, true)
	if changed.Code != http.StatusNoContent {
		t.Fatalf("change status=%d: %s", changed.Code, changed.Body.String())
	}
	me := a.request("GET", "/api/auth/me", nil, true)
	if me.Code != http.StatusUnauthorized {
		t.Fatalf("old session remains valid: %d", me.Code)
	}
	oldLogin := a.request("POST", "/api/auth/login", map[string]any{"username": "admin", "password": "a-secure-test-password"}, false)
	if oldLogin.Code != http.StatusUnauthorized {
		t.Fatalf("old login status=%d", oldLogin.Code)
	}
	newLogin := a.request("POST", "/api/auth/login", map[string]any{"username": "owner", "password": "a-new-secure-password"}, false)
	if newLogin.Code != http.StatusOK {
		t.Fatalf("new login status=%d: %s", newLogin.Code, newLogin.Body.String())
	}
}

func TestAccountFieldEndpoints(t *testing.T) {
	a := newTestApp(t)
	rename := a.request("PATCH", "/api/profile/username", map[string]any{"username": " owner "}, true)
	if rename.Code != http.StatusNoContent {
		t.Fatalf("rename status=%d: %s", rename.Code, rename.Body.String())
	}
	me := a.request("GET", "/api/auth/me", nil, true)
	if me.Code != http.StatusOK || !strings.Contains(me.Body.String(), `"username":"owner"`) {
		t.Fatalf("renamed session status=%d: %s", me.Code, me.Body.String())
	}
	wrong := a.request("PATCH", "/api/auth/password", map[string]any{"current_password": "wrong-password", "password": "a-new-secure-password"}, true)
	if wrong.Code != http.StatusUnauthorized {
		t.Fatalf("wrong password status=%d: %s", wrong.Code, wrong.Body.String())
	}
	changed := a.request("PATCH", "/api/auth/password", map[string]any{"current_password": "a-secure-test-password", "password": "a-new-secure-password"}, true)
	if changed.Code != http.StatusNoContent {
		t.Fatalf("password change status=%d: %s", changed.Code, changed.Body.String())
	}
	if me = a.request("GET", "/api/auth/me", nil, true); me.Code != http.StatusUnauthorized {
		t.Fatalf("password change kept old session: %d", me.Code)
	}
	login := a.request("POST", "/api/auth/login", map[string]any{"username": "owner", "password": "a-new-secure-password"}, false)
	if login.Code != http.StatusOK {
		t.Fatalf("new password login status=%d: %s", login.Code, login.Body.String())
	}
}

func TestTOTPAPISetupLoginRecoveryAndDisable(t *testing.T) {
	a := newTestApp(t)
	now := time.Date(2026, 8, 22, 1, 0, 0, 0, time.UTC)
	a.srv.auth.Now = func() time.Time { return now }

	statusResponse := a.request("GET", "/api/auth/totp", nil, true)
	if statusResponse.Code != http.StatusOK {
		t.Fatalf("initial TOTP status=%d: %s", statusResponse.Code, statusResponse.Body.String())
	}
	initial := decode[auth.TOTPStatus](t, statusResponse)
	if initial.Enabled {
		t.Fatal("TOTP is enabled before setup")
	}
	setupResponse := a.request("POST", "/api/auth/totp/setup", map[string]any{"current_password": "a-secure-test-password"}, true)
	if setupResponse.Code != http.StatusCreated {
		t.Fatalf("begin TOTP setup=%d: %s", setupResponse.Code, setupResponse.Body.String())
	}
	setup := decode[struct {
		Secret    string `json:"secret"`
		URI       string `json:"uri"`
		QRDataURL string `json:"qr_data_url"`
	}](t, setupResponse)
	if setup.Secret == "" || !strings.HasPrefix(setup.URI, "otpauth://totp/") || !strings.HasPrefix(setup.QRDataURL, "data:image/png;base64,") {
		t.Fatalf("invalid setup response: secret=%t uri=%q qr=%q", setup.Secret != "", setup.URI, setup.QRDataURL)
	}
	code, err := totp.GenerateCode(setup.Secret, now)
	if err != nil {
		t.Fatal(err)
	}
	enableResponse := a.request("POST", "/api/auth/totp/enable", map[string]any{"current_password": "a-secure-test-password", "code": code}, true)
	if enableResponse.Code != http.StatusOK {
		t.Fatalf("enable TOTP=%d: %s", enableResponse.Code, enableResponse.Body.String())
	}
	enabled := decode[struct {
		Enabled       bool     `json:"enabled"`
		RecoveryCodes []string `json:"recovery_codes"`
	}](t, enableResponse)
	if !enabled.Enabled || len(enabled.RecoveryCodes) != 10 {
		t.Fatalf("enable response = enabled=%v recovery=%d", enabled.Enabled, len(enabled.RecoveryCodes))
	}
	var storedRecovery string
	if err := a.db.QueryRow(`SELECT value FROM settings WHERE key='admin_totp_recovery_codes'`).Scan(&storedRecovery); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(storedRecovery, enabled.RecoveryCodes[0]) {
		t.Fatal("plaintext recovery code was stored")
	}
	if me := a.request("GET", "/api/auth/me", nil, true); me.Code != http.StatusOK {
		t.Fatalf("enabling TOTP revoked current session: %d", me.Code)
	}

	missing := a.request("POST", "/api/auth/login", map[string]any{"username": "admin", "password": "a-secure-test-password"}, false)
	if missing.Code != http.StatusUnauthorized || !strings.Contains(missing.Body.String(), `"code":"totp_required"`) {
		t.Fatalf("password-only login=%d: %s", missing.Code, missing.Body.String())
	}
	now = now.Add(30 * time.Second)
	code, _ = totp.GenerateCode(setup.Secret, now)
	verified := a.request("POST", "/api/auth/login", map[string]any{"username": "admin", "password": "a-secure-test-password", "second_factor": code}, false)
	if verified.Code != http.StatusOK {
		t.Fatalf("TOTP login=%d: %s", verified.Code, verified.Body.String())
	}
	recovered := a.request("POST", "/api/auth/login", map[string]any{"username": "admin", "password": "a-secure-test-password", "second_factor": enabled.RecoveryCodes[0]}, false)
	if recovered.Code != http.StatusOK {
		t.Fatalf("recovery login=%d: %s", recovered.Code, recovered.Body.String())
	}
	status := decode[auth.TOTPStatus](t, a.request("GET", "/api/auth/totp", nil, true))
	if status.RecoveryCodes != 9 {
		t.Fatalf("recovery codes remaining=%d", status.RecoveryCodes)
	}

	now = now.Add(30 * time.Second)
	code, _ = totp.GenerateCode(setup.Secret, now)
	disabled := a.request("DELETE", "/api/auth/totp", map[string]any{"current_password": "a-secure-test-password", "code": code}, true)
	if disabled.Code != http.StatusNoContent {
		t.Fatalf("disable TOTP=%d: %s", disabled.Code, disabled.Body.String())
	}
	if status := decode[auth.TOTPStatus](t, a.request("GET", "/api/auth/totp", nil, true)); status.Enabled {
		t.Fatal("TOTP remained enabled")
	}
}

func TestAvatarCanBeUploadedReadAndRemoved(t *testing.T) {
	a := newTestApp(t)
	const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
	uploaded := a.request("PUT", "/api/profile/avatar", map[string]any{"data_url": "data:image/png;base64," + png}, true)
	if uploaded.Code != http.StatusNoContent {
		t.Fatalf("upload avatar=%d: %s", uploaded.Code, uploaded.Body.String())
	}
	me := a.request("GET", "/api/auth/me", nil, true)
	profile := decode[struct {
		HasAvatar bool `json:"has_avatar"`
	}](t, me)
	if !profile.HasAvatar {
		t.Fatal("profile does not report uploaded avatar")
	}
	avatar := a.request("GET", "/api/profile/avatar", nil, true)
	if avatar.Code != http.StatusOK || avatar.Header().Get("Content-Type") != "image/png" || avatar.Body.Len() == 0 {
		t.Fatalf("get avatar=%d type=%q bytes=%d", avatar.Code, avatar.Header().Get("Content-Type"), avatar.Body.Len())
	}
	removed := a.request("DELETE", "/api/profile/avatar", nil, true)
	if removed.Code != http.StatusNoContent {
		t.Fatalf("delete avatar=%d: %s", removed.Code, removed.Body.String())
	}
	if missing := a.request("GET", "/api/profile/avatar", nil, true); missing.Code != http.StatusNotFound {
		t.Fatalf("deleted avatar remains available: %d", missing.Code)
	}
}
