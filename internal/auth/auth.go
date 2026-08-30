package auth

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"golang.org/x/crypto/argon2"
)

const sessionLifetime = 30 * 24 * time.Hour

var ErrInvalidCredentials = errors.New("invalid credentials")

// Argon2 deliberately consumes a large amount of memory. Keep all
// request-triggered password operations inside a small global pool so a burst
// of concurrent logins or credential changes cannot exhaust the process.
var kdfSlots = make(chan struct{}, 2)

type InitialCredentials struct {
	Created   bool
	Generated bool
	Username  string
	Password  string
}

type Params struct {
	Memory      uint32
	Iterations  uint32
	Parallelism uint8
	SaltLength  uint32
	KeyLength   uint32
}

var DefaultParams = Params{Memory: 64 * 1024, Iterations: 3, Parallelism: 2, SaltLength: 16, KeyLength: 32}

// Password hashes are persisted in SQLite and must be treated as untrusted
// input: a corrupted or manually modified row must not be able to make login
// allocate arbitrary memory or CPU time.
const (
	minArgonMemory      = 1024
	maxArgonMemory      = 256 * 1024
	maxArgonIterations  = 10
	maxArgonParallelism = 8
	minArgonSaltLength  = 8
	maxArgonSaltLength  = 64
	minArgonKeyLength   = 16
	maxArgonKeyLength   = 64
)

type Service struct {
	DB       *sql.DB
	Username string
	Params   Params
	Now      func() time.Time
}

func (s *Service) Initialize(ctx context.Context, username, password string) (InitialCredentials, error) {
	var existing string
	err := s.DB.QueryRowContext(ctx, `SELECT value FROM settings WHERE key='admin_password_hash'`).Scan(&existing)
	if err == nil {
		return InitialCredentials{}, nil
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return InitialCredentials{}, err
	}
	if username == "" {
		username = "admin"
	}
	generated := password == ""
	if generated {
		password = rand.Text()
	}
	if len(username) > 128 || len(password) < 12 || len(password) > 1024 {
		return InitialCredentials{}, errors.New("administrator username/password length is invalid (password minimum is 12 characters)")
	}
	hash, err := s.hashPassword(ctx, password)
	if err != nil {
		return InitialCredentials{}, err
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := s.DB.BeginTx(ctx, nil)
	if err != nil {
		return InitialCredentials{}, err
	}
	defer tx.Rollback()
	for key, value := range map[string]string{"admin_username": username, "admin_password_hash": hash} {
		if _, err := tx.ExecContext(ctx, `INSERT INTO settings(key,value,updated_at) VALUES(?,?,?)`, key, value, now); err != nil {
			return InitialCredentials{}, err
		}
	}
	if err := tx.Commit(); err != nil {
		return InitialCredentials{}, err
	}
	credentials := InitialCredentials{Created: true, Generated: generated, Username: username}
	if generated {
		credentials.Password = password
	}
	return credentials, nil
}

func (s *Service) Login(ctx context.Context, username, password, secondFactor string) (string, time.Time, error) {
	if err := s.verifyCredentials(ctx, username, password); err != nil {
		return "", time.Time{}, ErrInvalidCredentials
	}
	status, err := s.TOTPStatus(ctx)
	if err != nil {
		return "", time.Time{}, err
	}
	if status.Enabled {
		if strings.TrimSpace(secondFactor) == "" {
			return "", time.Time{}, ErrTOTPRequired
		}
		if err := s.consumeSecondFactor(ctx, password, secondFactor, s.now()); err != nil {
			if errors.Is(err, ErrInvalidSecondFactor) {
				return "", time.Time{}, ErrInvalidSecondFactor
			}
			return "", time.Time{}, err
		}
	}
	tokenBytes := make([]byte, 32)
	if _, err := rand.Read(tokenBytes); err != nil {
		return "", time.Time{}, err
	}
	token := base64.RawURLEncoding.EncodeToString(tokenBytes)
	now := s.now()
	expires := now.Add(sessionLifetime)
	_, err = s.DB.ExecContext(ctx, `INSERT INTO sessions(id, token_hash, created_at, expires_at) VALUES(?,?,?,?)`, ids.New(), TokenHash(token), now.Format(time.RFC3339Nano), expires.Format(time.RFC3339Nano))
	return token, expires, err
}

func (s *Service) ChangeCredentials(ctx context.Context, currentUsername, currentPassword, newUsername, newPassword string) error {
	if newUsername == "" || len(newUsername) > 128 || len(newPassword) < 12 || len(newPassword) > 1024 {
		return errors.New("administrator username/password length is invalid (password minimum is 12 characters)")
	}
	if err := s.verifyCredentials(ctx, currentUsername, currentPassword); err != nil {
		return ErrInvalidCredentials
	}
	var reencrypted *encryptedSecret
	var rawSecret string
	err := s.DB.QueryRowContext(ctx, `SELECT value FROM settings WHERE key=?`, totpConfigKey).Scan(&rawSecret)
	if err == nil {
		var encrypted encryptedSecret
		if err := json.Unmarshal([]byte(rawSecret), &encrypted); err != nil {
			return fmt.Errorf("decode TOTP secret: %w", err)
		}
		secret, err := s.decryptSecret(ctx, currentPassword, encrypted)
		if err != nil {
			return fmt.Errorf("decrypt TOTP secret: %w", err)
		}
		next, err := s.encryptSecret(ctx, newPassword, secret)
		if err != nil {
			return err
		}
		reencrypted = &next
	} else if !errors.Is(err, sql.ErrNoRows) {
		return err
	}
	newHash, err := s.hashPassword(ctx, newPassword)
	if err != nil {
		return err
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := s.DB.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, `UPDATE settings SET value=?,updated_at=? WHERE key='admin_username'`, newUsername, now); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `UPDATE settings SET value=?,updated_at=? WHERE key='admin_password_hash'`, newHash, now); err != nil {
		return err
	}
	if reencrypted != nil {
		raw, err := json.Marshal(reencrypted)
		if err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, `UPDATE settings SET value=?,updated_at=? WHERE key=?`, string(raw), now, totpConfigKey); err != nil {
			return err
		}
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM settings WHERE key=?`, totpPendingKey); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM sessions`); err != nil {
		return err
	}
	return tx.Commit()
}

// ChangeUsername updates the administrator's display/login name without
// invalidating existing sessions. Authentication is enforced by the HTTP
// handler before this method is called.
func (s *Service) ChangeUsername(ctx context.Context, newUsername string) error {
	newUsername = strings.TrimSpace(newUsername)
	if newUsername == "" || len(newUsername) > 128 {
		return errors.New("administrator username length is invalid")
	}
	result, err := s.DB.ExecContext(ctx, `UPDATE settings SET value=?,updated_at=? WHERE key='admin_username'`, newUsername, time.Now().UTC().Format(time.RFC3339Nano))
	if err != nil {
		return err
	}
	updated, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if updated != 1 {
		return errors.New("administrator username is not initialized")
	}
	return nil
}

func (s *Service) ResetCredentials(ctx context.Context, username string) (InitialCredentials, error) {
	if username == "" {
		username = "admin"
	}
	if len(username) > 128 {
		return InitialCredentials{}, errors.New("administrator username length is invalid")
	}
	password := rand.Text()
	hash, err := s.hashPassword(ctx, password)
	if err != nil {
		return InitialCredentials{}, err
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := s.DB.BeginTx(ctx, nil)
	if err != nil {
		return InitialCredentials{}, err
	}
	defer tx.Rollback()
	for key, value := range map[string]string{"admin_username": username, "admin_password_hash": hash} {
		if _, err := tx.ExecContext(ctx, `INSERT INTO settings(key,value,updated_at) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at`, key, value, now); err != nil {
			return InitialCredentials{}, err
		}
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM settings WHERE key IN (?,?,?,?)`, totpConfigKey, totpRecoveryKey, totpLastStepKey, totpPendingKey); err != nil {
		return InitialCredentials{}, err
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM sessions`); err != nil {
		return InitialCredentials{}, err
	}
	if err := tx.Commit(); err != nil {
		return InitialCredentials{}, err
	}
	return InitialCredentials{Generated: true, Username: username, Password: password}, nil
}

func (s *Service) Authenticate(ctx context.Context, token string) (string, error) {
	if token == "" {
		return "", errors.New("missing session")
	}
	var expiry, username string
	err := s.DB.QueryRowContext(ctx, `SELECT sess.expires_at,admin.value FROM sessions sess JOIN settings admin ON admin.key='admin_username' WHERE sess.token_hash=?`, TokenHash(token)).Scan(&expiry, &username)
	if err != nil {
		return "", errors.New("invalid session")
	}
	t, err := time.Parse(time.RFC3339Nano, expiry)
	if err != nil || !t.After(time.Now().UTC()) {
		s.Logout(ctx, token)
		return "", errors.New("expired session")
	}
	return username, nil
}

func (s *Service) Logout(ctx context.Context, token string) {
	if token != "" {
		_, _ = s.DB.ExecContext(ctx, `DELETE FROM sessions WHERE token_hash=?`, TokenHash(token))
	}
}
func (s *Service) Cleanup(ctx context.Context) {
	_, _ = s.DB.ExecContext(ctx, `DELETE FROM sessions WHERE expires_at <= ?`, time.Now().UTC().Format(time.RFC3339Nano))
}
func (s *Service) params() Params {
	if s.Params.Memory == 0 {
		return DefaultParams
	}
	return s.Params
}
func (s *Service) now() time.Time {
	if s.Now != nil {
		return s.Now().UTC()
	}
	return time.Now().UTC()
}
func (s *Service) verifyCredentials(ctx context.Context, username, password string) error {
	var savedUser, savedHash string
	if err := s.DB.QueryRowContext(ctx, `SELECT MAX(CASE WHEN key='admin_username' THEN value END),MAX(CASE WHEN key='admin_password_hash' THEN value END) FROM settings WHERE key IN ('admin_username','admin_password_hash')`).Scan(&savedUser, &savedHash); err != nil {
		return err
	}
	var valid bool
	err := withKDF(ctx, func() error {
		var verifyErr error
		valid, verifyErr = VerifyPassword(password, savedHash)
		return verifyErr
	})
	if err != nil || subtle.ConstantTimeCompare([]byte(username), []byte(savedUser)) != 1 || !valid {
		return ErrInvalidCredentials
	}
	return nil
}

func withKDF(ctx context.Context, fn func() error) error {
	select {
	case kdfSlots <- struct{}{}:
		defer func() { <-kdfSlots }()
		return fn()
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *Service) hashPassword(ctx context.Context, password string) (hash string, err error) {
	err = withKDF(ctx, func() error {
		var hashErr error
		hash, hashErr = HashPassword(password, s.params())
		return hashErr
	})
	return hash, err
}

func (s *Service) encryptSecret(ctx context.Context, password, secret string) (encrypted encryptedSecret, err error) {
	err = withKDF(ctx, func() error {
		var encryptErr error
		encrypted, encryptErr = encryptSecret(password, secret, s.params())
		return encryptErr
	})
	return encrypted, err
}

func (s *Service) decryptSecret(ctx context.Context, password string, encrypted encryptedSecret) (secret string, err error) {
	err = withKDF(ctx, func() error {
		var decryptErr error
		secret, decryptErr = decryptSecret(password, encrypted)
		return decryptErr
	})
	return secret, err
}
func TokenHash(token string) string {
	sum := sha256.Sum256([]byte(token))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func HashPassword(password string, p Params) (string, error) {
	if err := validateParams(p); err != nil {
		return "", err
	}
	salt := make([]byte, p.SaltLength)
	if _, err := rand.Read(salt); err != nil {
		return "", err
	}
	key := argon2.IDKey([]byte(password), salt, p.Iterations, p.Memory, p.Parallelism, p.KeyLength)
	return fmt.Sprintf("$argon2id$v=%d$m=%d,t=%d,p=%d$%s$%s", argon2.Version, p.Memory, p.Iterations, p.Parallelism, base64.RawStdEncoding.EncodeToString(salt), base64.RawStdEncoding.EncodeToString(key)), nil
}

func VerifyPassword(password, encoded string) (bool, error) {
	if len(encoded) > 1024 {
		return false, errors.New("password hash is too long")
	}
	parts := strings.Split(encoded, "$")
	if len(parts) != 6 || parts[1] != "argon2id" {
		return false, errors.New("invalid password hash")
	}
	var version int
	if _, err := fmt.Sscanf(parts[2], "v=%d", &version); err != nil || version != argon2.Version {
		return false, errors.New("unsupported argon2 version")
	}
	params := strings.Split(parts[3], ",")
	if len(params) != 3 {
		return false, errors.New("invalid argon2 parameters")
	}
	m, err := strconv.ParseUint(strings.TrimPrefix(params[0], "m="), 10, 32)
	if err != nil {
		return false, err
	}
	t, err := strconv.ParseUint(strings.TrimPrefix(params[1], "t="), 10, 32)
	if err != nil {
		return false, err
	}
	p, err := strconv.ParseUint(strings.TrimPrefix(params[2], "p="), 10, 8)
	if err != nil {
		return false, err
	}
	salt, err := base64.RawStdEncoding.DecodeString(parts[4])
	if err != nil {
		return false, err
	}
	want, err := base64.RawStdEncoding.DecodeString(parts[5])
	if err != nil {
		return false, err
	}
	parsed := Params{Memory: uint32(m), Iterations: uint32(t), Parallelism: uint8(p), SaltLength: uint32(len(salt)), KeyLength: uint32(len(want))}
	if err := validateParams(parsed); err != nil {
		return false, err
	}
	got := argon2.IDKey([]byte(password), salt, parsed.Iterations, parsed.Memory, parsed.Parallelism, parsed.KeyLength)
	return subtle.ConstantTimeCompare(got, want) == 1, nil
}

func validateParams(p Params) error {
	if p.Memory < minArgonMemory || p.Memory > maxArgonMemory ||
		p.Iterations < 1 || p.Iterations > maxArgonIterations ||
		p.Parallelism < 1 || p.Parallelism > maxArgonParallelism ||
		p.SaltLength < minArgonSaltLength || p.SaltLength > maxArgonSaltLength ||
		p.KeyLength < minArgonKeyLength || p.KeyLength > maxArgonKeyLength {
		return errors.New("argon2 parameters are outside supported limits")
	}
	return nil
}
