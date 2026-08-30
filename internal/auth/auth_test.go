package auth

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/database"
	"github.com/pquerna/otp/totp"
)

var testParams = Params{Memory: 8 * 1024, Iterations: 1, Parallelism: 1, SaltLength: 16, KeyLength: 32}

func TestLoginSessionAndExpiry(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	svc := &Service{DB: db, Params: testParams}
	if _, err := svc.Initialize(context.Background(), "admin", "a-secure-test-password"); err != nil {
		t.Fatal(err)
	}
	if _, _, err := svc.Login(context.Background(), "admin", "wrong-password", ""); err == nil {
		t.Fatal("wrong password was accepted")
	}
	token, _, err := svc.Login(context.Background(), "admin", "a-secure-test-password", "")
	if err != nil {
		t.Fatal(err)
	}
	if got, err := svc.Authenticate(context.Background(), token); err != nil || got != "admin" {
		t.Fatalf("authenticate = %q, %v", got, err)
	}
	if _, err := db.Exec(`UPDATE sessions SET expires_at=? WHERE token_hash=?`, time.Now().Add(-time.Hour).UTC().Format(time.RFC3339Nano), TokenHash(token)); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.Authenticate(context.Background(), token); err == nil {
		t.Fatal("expired session was accepted")
	}
}

func TestInitializeGeneratesOneTimeCredentials(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	svc := &Service{DB: db, Params: testParams}
	credentials, err := svc.Initialize(context.Background(), "", "")
	if err != nil {
		t.Fatal(err)
	}
	if !credentials.Created || !credentials.Generated || credentials.Username != "admin" || len(credentials.Password) < 12 {
		t.Fatalf("unexpected credentials: created=%v generated=%v username=%q password_length=%d", credentials.Created, credentials.Generated, credentials.Username, len(credentials.Password))
	}
	if _, _, err := svc.Login(context.Background(), credentials.Username, credentials.Password, ""); err != nil {
		t.Fatalf("generated credentials cannot log in: %v", err)
	}
	again, err := svc.Initialize(context.Background(), "other", "another-secure-password")
	if err != nil {
		t.Fatal(err)
	}
	if again.Created || again.Generated || again.Password != "" {
		t.Fatalf("credentials were exposed again: %+v", again)
	}
}

func TestChangeCredentialsRevokesSessions(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	svc := &Service{DB: db, Params: testParams}
	if _, err := svc.Initialize(context.Background(), "admin", "a-secure-test-password"); err != nil {
		t.Fatal(err)
	}
	token, _, err := svc.Login(context.Background(), "admin", "a-secure-test-password", "")
	if err != nil {
		t.Fatal(err)
	}
	if err := svc.ChangeCredentials(context.Background(), "admin", "a-secure-test-password", "owner", "a-new-secure-password"); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.Authenticate(context.Background(), token); err == nil {
		t.Fatal("existing session was not revoked")
	}
	if _, _, err := svc.Login(context.Background(), "admin", "a-secure-test-password", ""); err == nil {
		t.Fatal("old credentials were accepted")
	}
	if _, _, err := svc.Login(context.Background(), "owner", "a-new-secure-password", ""); err != nil {
		t.Fatalf("new credentials were rejected: %v", err)
	}
}

func TestChangeUsernameKeepsSessionAndPassword(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	svc := &Service{DB: db, Params: testParams}
	if _, err := svc.Initialize(context.Background(), "admin", "a-secure-test-password"); err != nil {
		t.Fatal(err)
	}
	token, _, err := svc.Login(context.Background(), "admin", "a-secure-test-password", "")
	if err != nil {
		t.Fatal(err)
	}
	if err := svc.ChangeUsername(context.Background(), " owner "); err != nil {
		t.Fatal(err)
	}
	if got, err := svc.Authenticate(context.Background(), token); err != nil || got != "owner" {
		t.Fatalf("renamed session authenticate = %q, %v", got, err)
	}
	if _, _, err := svc.Login(context.Background(), "owner", "a-secure-test-password", ""); err != nil {
		t.Fatalf("renamed login was rejected: %v", err)
	}
}

func TestResetCredentialsRecoversExistingDatabase(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	svc := &Service{DB: db, Params: testParams}
	if _, err := svc.Initialize(context.Background(), "admin", "a-secure-test-password"); err != nil {
		t.Fatal(err)
	}
	credentials, err := svc.ResetCredentials(context.Background(), "owner")
	if err != nil {
		t.Fatal(err)
	}
	if !credentials.Generated || credentials.Username != "owner" || credentials.Password == "" {
		t.Fatalf("unexpected reset credentials: generated=%v username=%q password_length=%d", credentials.Generated, credentials.Username, len(credentials.Password))
	}
	if _, _, err := svc.Login(context.Background(), "admin", "a-secure-test-password", ""); err == nil {
		t.Fatal("old credentials were accepted after reset")
	}
	if _, _, err := svc.Login(context.Background(), credentials.Username, credentials.Password, ""); err != nil {
		t.Fatalf("reset credentials were rejected: %v", err)
	}
}

func TestPasswordHashIsSalted(t *testing.T) {
	a, err := HashPassword("correct horse battery staple", testParams)
	if err != nil {
		t.Fatal(err)
	}
	b, err := HashPassword("correct horse battery staple", testParams)
	if err != nil {
		t.Fatal(err)
	}
	if a == b {
		t.Fatal("password hashes must use independent salts")
	}
	if ok, err := VerifyPassword("correct horse battery staple", a); err != nil || !ok {
		t.Fatalf("verification = %v, %v", ok, err)
	}
}

func TestPasswordHashRejectsResourceExhaustionParameters(t *testing.T) {
	for _, encoded := range []string{
		"$argon2id$v=19$m=4294967295,t=3,p=2$c2FsdHNhbHRzYWx0c2FsdA$AAAAAAAAAAAAAAAAAAAAAA",
		"$argon2id$v=19$m=65536,t=4294967295,p=2$c2FsdHNhbHRzYWx0c2FsdA$AAAAAAAAAAAAAAAAAAAAAA",
		"$argon2id$v=19$m=65536,t=3,p=255$c2FsdHNhbHRzYWx0c2FsdA$AAAAAAAAAAAAAAAAAAAAAA",
	} {
		if _, err := VerifyPassword("password", encoded); err == nil {
			t.Fatalf("unsafe parameters accepted: %s", encoded)
		}
	}
	if _, err := HashPassword("password", Params{Memory: maxArgonMemory + 1, Iterations: 1, Parallelism: 1, SaltLength: 16, KeyLength: 32}); err == nil {
		t.Fatal("unsafe hash parameters accepted")
	}
}

func TestTOTPLifecycleRecoveryReplayAndPasswordChange(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	now := time.Date(2026, 8, 22, 1, 0, 0, 0, time.UTC)
	svc := &Service{DB: db, Params: testParams, Now: func() time.Time { return now }}
	const password = "a-secure-test-password"
	if _, err := svc.Initialize(context.Background(), "admin", password); err != nil {
		t.Fatal(err)
	}
	currentToken, _, err := svc.Login(context.Background(), "admin", password, "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := svc.BeginTOTPSetup(context.Background(), "admin", "wrong-password"); !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("setup with wrong password = %v", err)
	}
	setup, err := svc.BeginTOTPSetup(context.Background(), "admin", password)
	if err != nil {
		t.Fatal(err)
	}
	code, err := totp.GenerateCode(setup.Secret, now)
	if err != nil {
		t.Fatal(err)
	}
	recovery, err := svc.ConfirmTOTPSetup(context.Background(), "admin", password, code, currentToken)
	if err != nil {
		t.Fatal(err)
	}
	if len(recovery) != recoveryCount {
		t.Fatalf("recovery code count = %d", len(recovery))
	}
	status, err := svc.TOTPStatus(context.Background())
	if err != nil || !status.Enabled || status.RecoveryCodes != recoveryCount {
		t.Fatalf("status = %+v, %v", status, err)
	}
	if _, _, err := svc.Login(context.Background(), "admin", password, ""); !errors.Is(err, ErrTOTPRequired) {
		t.Fatalf("login without TOTP = %v", err)
	}
	if _, _, err := svc.Login(context.Background(), "admin", password, "000000"); !errors.Is(err, ErrInvalidSecondFactor) {
		t.Fatalf("login with bad TOTP = %v", err)
	}

	// The encrypted envelope carries its KDF parameters, so a future server
	// configuration change cannot make an existing TOTP secret unreadable.
	svc.Params = Params{Memory: 2 * 1024, Iterations: 2, Parallelism: 1, SaltLength: 16, KeyLength: 32}
	now = now.Add(30 * time.Second)
	code, _ = totp.GenerateCode(setup.Secret, now)
	loginToken, _, err := svc.Login(context.Background(), "admin", password, code)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := svc.Login(context.Background(), "admin", password, code); !errors.Is(err, ErrInvalidSecondFactor) {
		t.Fatalf("replayed TOTP = %v", err)
	}
	if _, _, err := svc.Login(context.Background(), "admin", password, recovery[0]); err != nil {
		t.Fatalf("recovery login = %v", err)
	}
	if _, _, err := svc.Login(context.Background(), "admin", password, recovery[0]); !errors.Is(err, ErrInvalidSecondFactor) {
		t.Fatalf("replayed recovery code = %v", err)
	}
	status, _ = svc.TOTPStatus(context.Background())
	if status.RecoveryCodes != recoveryCount-1 {
		t.Fatalf("recovery codes remaining = %d", status.RecoveryCodes)
	}

	const newPassword = "a-new-secure-password"
	if err := svc.ChangeCredentials(context.Background(), "admin", password, "owner", newPassword); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.Authenticate(context.Background(), loginToken); err == nil {
		t.Fatal("password change did not revoke the TOTP-authenticated session")
	}
	now = now.Add(30 * time.Second)
	code, _ = totp.GenerateCode(setup.Secret, now)
	disableToken, _, err := svc.Login(context.Background(), "owner", newPassword, code)
	if err != nil {
		t.Fatalf("TOTP secret was not re-encrypted for new password: %v", err)
	}

	now = now.Add(30 * time.Second)
	code, _ = totp.GenerateCode(setup.Secret, now)
	newRecovery, err := svc.RegenerateRecoveryCodes(context.Background(), "owner", newPassword, code)
	if err != nil || len(newRecovery) != recoveryCount {
		t.Fatalf("regenerate recovery codes = %d, %v", len(newRecovery), err)
	}
	if _, _, err := svc.Login(context.Background(), "owner", newPassword, recovery[1]); !errors.Is(err, ErrInvalidSecondFactor) {
		t.Fatalf("old recovery code survived regeneration: %v", err)
	}

	now = now.Add(30 * time.Second)
	code, _ = totp.GenerateCode(setup.Secret, now)
	if err := svc.DisableTOTP(context.Background(), "owner", newPassword, code, disableToken); err != nil {
		t.Fatal(err)
	}
	status, err = svc.TOTPStatus(context.Background())
	if err != nil || status.Enabled {
		t.Fatalf("status after disable = %+v, %v", status, err)
	}
	if _, _, err := svc.Login(context.Background(), "owner", newPassword, ""); err != nil {
		t.Fatalf("password-only login after disable = %v", err)
	}
}

func TestResetCredentialsDisablesTOTP(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	now := time.Date(2026, 8, 22, 1, 0, 0, 0, time.UTC)
	svc := &Service{DB: db, Params: testParams, Now: func() time.Time { return now }}
	const password = "a-secure-test-password"
	if _, err := svc.Initialize(context.Background(), "admin", password); err != nil {
		t.Fatal(err)
	}
	token, _, _ := svc.Login(context.Background(), "admin", password, "")
	setup, _ := svc.BeginTOTPSetup(context.Background(), "admin", password)
	code, _ := totp.GenerateCode(setup.Secret, now)
	if _, err := svc.ConfirmTOTPSetup(context.Background(), "admin", password, code, token); err != nil {
		t.Fatal(err)
	}
	credentials, err := svc.ResetCredentials(context.Background(), "owner")
	if err != nil {
		t.Fatal(err)
	}
	status, err := svc.TOTPStatus(context.Background())
	if err != nil || status.Enabled {
		t.Fatalf("TOTP survived reset: %+v, %v", status, err)
	}
	if _, _, err := svc.Login(context.Background(), credentials.Username, credentials.Password, ""); err != nil {
		t.Fatalf("reset credentials require TOTP: %v", err)
	}
}
