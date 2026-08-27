package server

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"image"
	"image/jpeg"
	"image/png"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/auth"
	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/database"
	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/aws/smithy-go"
	"github.com/pquerna/otp/totp"
)

// notFoundError emulates the S3 NoSuchKey API error the real store returns.
func notFoundError() error {
	return &smithy.GenericAPIError{Code: "NoSuchKey", Message: "object not found", Fault: smithy.FaultClient}
}

type testBlock struct {
	ID   string
	Size int64
}
type testManifest struct {
	Version int
	Size    int64
	Blocks  []testBlock
}

func (m testManifest) ID() string  { raw, _ := json.Marshal(m); return sha256hex(raw) }
func (m testManifest) Key() string { return "manifests/" + m.ID() }

// mockStorage emulates object storage in memory. Legacy-shaped maps remain in
// this test helper only so old database fixtures can be exercised.
type mockStorage struct {
	mu               sync.RWMutex
	blocks           map[string][]byte // by block id
	manifests        map[string]testManifest
	raw              map[string][]byte // raw object key -> content
	rawMime          map[string]string
	modified         map[string]time.Time
	blockSize        int64
	presignErr       error
	putManifestErr   error
	getManifestErr   error
	omitManifestList bool
	multipart        map[string]string
	rawURL           string
}

func newMockStorage(blockSize int64) *mockStorage {
	if blockSize <= 0 {
		blockSize = 4 << 20
	}
	return &mockStorage{
		blocks:    map[string][]byte{},
		manifests: map[string]testManifest{},
		raw:       map[string][]byte{},
		rawMime:   map[string]string{},
		modified:  map[string]time.Time{},
		blockSize: blockSize,
		multipart: map[string]string{},
	}
}

func (m *mockStorage) Ping(context.Context) error { return nil }
func (m *mockStorage) PresignPutObject(_ context.Context, key, _ string, _ time.Duration) (string, error) {
	if m.presignErr != nil {
		return "", m.presignErr
	}
	return "https://s3.example/put/" + key, nil
}
func (m *mockStorage) CreateMultipart(_ context.Context, key, _ string) (string, error) {
	if m.presignErr != nil {
		return "", m.presignErr
	}
	id := ids.New()
	m.multipart[id] = key
	return id, nil
}
func (m *mockStorage) PresignUploadPart(_ context.Context, key, uploadID string, partNumber int32, _ time.Duration) (string, error) {
	if m.multipart[uploadID] != key {
		return "", notFoundError()
	}
	return "https://s3.example/multipart/" + uploadID + "/" + fmt.Sprint(partNumber), nil
}
func (m *mockStorage) CompleteMultipart(_ context.Context, key, uploadID string, _ []storage.CompletedPart) (storage.ObjectInfo, error) {
	if m.multipart[uploadID] != key {
		return storage.ObjectInfo{}, notFoundError()
	}
	delete(m.multipart, uploadID)
	return m.HeadObject(context.Background(), key)
}
func (m *mockStorage) AbortMultipart(_ context.Context, key, uploadID string) error {
	if m.multipart[uploadID] == key {
		delete(m.multipart, uploadID)
	}
	return nil
}
func (m *mockStorage) HeadObject(_ context.Context, key string) (storage.ObjectInfo, error) {
	data, ok := m.raw[key]
	if !ok {
		return storage.ObjectInfo{}, notFoundError()
	}
	return storage.ObjectInfo{Size: int64(len(data)), ETag: "etag"}, nil
}
func (m *mockStorage) StoreBlob(_ context.Context, key, mimeType string, r io.Reader, size int64) (storage.ObjectInfo, error) {
	data, err := io.ReadAll(r)
	if err != nil {
		return storage.ObjectInfo{}, err
	}
	if size >= 0 && int64(len(data)) != size {
		return storage.ObjectInfo{}, errors.New("size mismatch")
	}
	size = int64(len(data))
	m.raw[key] = data
	m.rawMime[key] = mimeType
	return storage.ObjectInfo{Size: size, ETag: "etag"}, nil
}
func (m *mockStorage) PresignBlockPut(_ context.Context, id string, _ time.Duration) (string, error) {
	if m.presignErr != nil {
		return "", m.presignErr
	}
	return "https://s3.example/put/" + id, nil
}
func (m *mockStorage) PutBlock(_ context.Context, id string, data []byte) error {
	if sha256hex(data) != id {
		return errors.New("block hash mismatch")
	}
	if _, ok := m.blocks[id]; !ok {
		m.blocks[id] = append([]byte(nil), data...)
	}
	return nil
}
func (m *mockStorage) HeadBlock(_ context.Context, id string) (testBlock, error) {
	data, ok := m.blocks[id]
	if !ok {
		return testBlock{}, notFoundError()
	}
	return testBlock{ID: id, Size: int64(len(data))}, nil
}
func (m *mockStorage) GetBlock(_ context.Context, id string) ([]byte, error) {
	data, ok := m.blocks[id]
	if !ok {
		return nil, notFoundError()
	}
	return append([]byte(nil), data...), nil
}
func (m *mockStorage) ListBlocks(context.Context) ([]storage.ObjectRef, error) {
	var out []storage.ObjectRef
	for id, data := range m.blocks {
		key := "blocks/" + id
		out = append(out, storage.ObjectRef{Key: key, Size: int64(len(data)), LastModified: m.modified[key]})
	}
	return out, nil
}
func (m *mockStorage) PutManifest(_ context.Context, mm testManifest) (string, error) {
	if m.putManifestErr != nil {
		return "", m.putManifestErr
	}
	key := mm.Key()
	m.manifests[key] = mm
	return key, nil
}
func (m *mockStorage) GetManifest(_ context.Context, key string) (testManifest, error) {
	if m.getManifestErr != nil {
		return testManifest{}, m.getManifestErr
	}
	mm, ok := m.manifests[key]
	if !ok {
		return testManifest{}, notFoundError()
	}
	return mm, nil
}
func (m *mockStorage) ListManifests(context.Context) ([]storage.ObjectRef, error) {
	if m.omitManifestList {
		return []storage.ObjectRef{}, nil
	}
	var out []storage.ObjectRef
	for key := range m.manifests {
		out = append(out, storage.ObjectRef{Key: key, Size: 1, LastModified: m.modified[key]})
	}
	return out, nil
}
func (m *mockStorage) Store(_ context.Context, r io.Reader) (string, testManifest, error) {
	data, err := io.ReadAll(r)
	if err != nil {
		return "", testManifest{}, err
	}
	mm := testManifest{Version: 1}
	for len(data) > 0 {
		n := len(data)
		if int64(n) > m.blockSize {
			n = int(m.blockSize)
		}
		chunk := data[:n]
		data = data[n:]
		id := sha256hex(chunk)
		m.blocks[id] = append([]byte(nil), chunk...)
		mm.Blocks = append(mm.Blocks, testBlock{ID: id, Size: int64(len(chunk))})
		mm.Size += int64(len(chunk))
	}
	key, err := m.PutManifest(context.Background(), mm)
	return key, mm, err
}
func (m *mockStorage) Open(_ context.Context, key string) (storage.ReadSeekCloserAt, error) {
	mm, ok := m.manifests[key]
	if !ok {
		if data, rawOK := m.raw[key]; rawOK {
			return nopReadSeekCloser{Reader: bytes.NewReader(data)}, nil
		}
		return nil, notFoundError()
	}
	var buf bytes.Buffer
	for _, b := range mm.Blocks {
		data, ok := m.blocks[b.ID]
		if !ok {
			return nil, notFoundError()
		}
		buf.Write(data)
	}
	return nopReadSeekCloser{Reader: bytes.NewReader(buf.Bytes())}, nil
}

type nopReadSeekCloser struct{ *bytes.Reader }

func (nopReadSeekCloser) Close() error  { return nil }
func (r nopReadSeekCloser) Size() int64 { return r.Reader.Size() }

func (m *mockStorage) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	rc, err := m.Open(ctx, key)
	if err != nil {
		return nil, err
	}
	defer rc.Close()
	data, err := io.ReadAll(io.LimitReader(rc, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, storage.ErrObjectTooLarge
	}
	return data, nil
}
func (m *mockStorage) PutObject(_ context.Context, key, mimeType string, data []byte) (storage.ObjectInfo, error) {
	m.raw[key] = append([]byte(nil), data...)
	m.rawMime[key] = mimeType
	return storage.ObjectInfo{Size: int64(len(data)), ETag: `"etag"`}, nil
}
func (m *mockStorage) PutImmutable(_ context.Context, key, mimeType string, data []byte) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if _, ok := m.raw[key]; ok {
		return nil // 内容寻址对象已存在：视为成功
	}
	m.raw[key] = append([]byte(nil), data...)
	m.rawMime[key] = mimeType
	return nil
}
func (m *mockStorage) OpenRaw(_ context.Context, key string) (io.ReadCloser, error) {
	data, ok := m.raw[key]
	if !ok {
		return nil, notFoundError()
	}
	return io.NopCloser(bytes.NewReader(data)), nil
}
func (m *mockStorage) GetObject(ctx context.Context, key string, limit int64) ([]byte, error) {
	rc, err := m.OpenRaw(ctx, key)
	if err != nil {
		return nil, err
	}
	defer rc.Close()
	data, err := io.ReadAll(io.LimitReader(rc, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, storage.ErrObjectTooLarge
	}
	return data, nil
}
func (m *mockStorage) DeleteObject(_ context.Context, key string) error {
	delete(m.raw, key)
	delete(m.rawMime, key)
	delete(m.manifests, key)
	delete(m.modified, key)
	if id := strings.TrimPrefix(key, "blocks/"); id != key {
		delete(m.blocks, strings.ReplaceAll(id, "/", ""))
	}
	return nil
}

func (m *mockStorage) PresignGetObject(_ context.Context, key, _, _ string, _ bool, _ time.Duration) (string, error) {
	if m.rawURL != "" {
		return m.rawURL + "/" + key, nil
	}
	return "https://s3.example/get", nil
}
func (m *mockStorage) ListPrefix(_ context.Context, prefix string) ([]storage.ObjectRef, error) {
	var out []storage.ObjectRef
	for key, data := range m.raw {
		if strings.HasPrefix(key, prefix) {
			out = append(out, storage.ObjectRef{Key: key, Size: int64(len(data)), LastModified: m.modified[key]})
		}
	}
	return out, nil
}

// test helpers
func (m *mockStorage) putBlock(id string, data []byte) { m.blocks[id] = append([]byte(nil), data...) }
func (m *mockStorage) age(key string, t time.Time)     { m.modified[key] = t }

func sha256hex(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

type testApp struct {
	t       *testing.T
	db      *sql.DB
	store   *mockStorage
	srv     *Server
	handler http.Handler
	cookie  *http.Cookie
}

func newTestApp(t *testing.T) *testApp {
	return newTestAppWithBlockSize(t, 4<<20)
}
func newTestAppWithBlockSize(t *testing.T, blockSize int64) *testApp {
	t.Helper()
	db, err := database.Open(t.TempDir() + "/revaro.db")
	if err != nil {
		t.Fatal(err)
	}
	a := &auth.Service{DB: db, Params: auth.Params{Memory: 8 * 1024, Iterations: 1, Parallelism: 1, SaltLength: 16, KeyLength: 32}}
	if _, err := a.Initialize(context.Background(), "admin", "a-secure-test-password"); err != nil {
		t.Fatal(err)
	}
	store := newMockStorage(blockSize)
	rawServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		key := strings.TrimPrefix(r.URL.Path, "/")
		store.mu.RLock()
		data, ok := store.raw[key]
		data = append([]byte(nil), data...)
		mimeType := store.rawMime[key]
		store.mu.RUnlock()
		if !ok {
			http.NotFound(w, r)
			return
		}
		if mimeType != "" {
			w.Header().Set("Content-Type", mimeType)
		}
		http.ServeContent(w, r, filepath.Base(key), time.Time{}, bytes.NewReader(data))
	}))
	store.rawURL = rawServer.URL
	t.Cleanup(rawServer.Close)
	dataDir := t.TempDir()
	cfg := config.Config{DataDir: dataDir, WorkDir: filepath.Join(dataDir, "work"), BaseURL: "http://example.test", ProxyTransfers: true, PresignExpires: time.Minute, UploadExpires: time.Hour, TrashRetention: 30 * 24 * time.Hour, MediaCacheCapacity: 2 << 30, FFmpegPath: "ffmpeg"}
	app := &testApp{t: t, db: db, store: store}
	app.srv = New(db, store, a, cfg, nil)
	app.handler = app.srv.Handler()
	resp := app.request("POST", "/api/auth/login", map[string]any{"username": "admin", "password": "a-secure-test-password"}, false)
	if resp.Code != 200 {
		t.Fatalf("login status %d: %s", resp.Code, resp.Body.String())
	}
	app.cookie = resp.Result().Cookies()[0]
	t.Cleanup(func() { app.srv.Close(); db.Close() })
	return app
}

// readyFile stores one opaque blob and inserts a ready file row.
func (a *testApp) readyFile(t *testing.T, name string, content []byte) File {
	t.Helper()
	id := ids.New()
	key := storage.BlobKey(id)
	a.store.mu.Lock()
	a.store.raw[key] = append([]byte(nil), content...)
	a.store.modified[key] = time.Now().UTC()
	a.store.mu.Unlock()
	etag := sha256hex(content)
	parent := RootID
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`, id, parent, name, "file", key, len(content), "application/octet-stream", etag, "ready", now, now); err != nil {
		t.Fatal(err)
	}
	return File{ID: id, ParentID: &parent, Name: name, Kind: "file", Size: int64(len(content)), MimeType: "application/octet-stream", ETag: etag, Status: "ready", CreatedAt: now, UpdatedAt: now, objectKey: key}
}

type createdUpload struct {
	UploadID  string `json:"upload_id"`
	FileID    string `json:"file_id"`
	Mode      string `json:"mode"`
	URL       string `json:"url"`
	PartSize  int64  `json:"part_size"`
	PartCount int    `json:"part_count"`
}

func (a *testApp) createUpload(t *testing.T, name string, size int64) createdUpload {
	t.Helper()
	rr := a.request("POST", "/api/uploads", map[string]any{"parent_id": RootID, "name": name, "size": size, "mime_type": "application/octet-stream"}, true)
	if rr.Code != http.StatusCreated {
		t.Fatalf("create upload=%d: %s", rr.Code, rr.Body.String())
	}
	return decode[createdUpload](t, rr)
}

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

func (a *testApp) request(method, path string, body any, authenticated bool) *httptest.ResponseRecorder {
	return a.requestH(method, path, body, authenticated, nil)
}
func (a *testApp) requestRaw(method, path string, body []byte, authenticated bool) *httptest.ResponseRecorder {
	a.t.Helper()
	req := httptest.NewRequest(method, path, bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/octet-stream")
	req.Header.Set("Origin", "http://example.test")
	if authenticated && a.cookie != nil {
		req.AddCookie(a.cookie)
	}
	rr := httptest.NewRecorder()
	a.handler.ServeHTTP(rr, req)
	return rr
}
func (a *testApp) requestH(method, path string, body any, authenticated bool, headers map[string]string) *httptest.ResponseRecorder {
	a.t.Helper()
	var data []byte
	if body != nil {
		data, _ = json.Marshal(body)
	}
	req := httptest.NewRequest(method, path, bytes.NewReader(data))
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	// 浏览器对写请求总是携带 Origin；测试默认模拟同源浏览器（baseURL
	// 为 http://example.test），需要验证跨源行为的用例显式覆盖该头。
	if method != http.MethodGet && method != http.MethodHead && method != http.MethodOptions {
		if _, ok := req.Header["Origin"]; !ok {
			req.Header.Set("Origin", "http://example.test")
		}
	}
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	if authenticated && a.cookie != nil {
		req.AddCookie(a.cookie)
	}
	rr := httptest.NewRecorder()
	a.handler.ServeHTTP(rr, req)
	return rr
}
func decode[T any](t *testing.T, rr *httptest.ResponseRecorder) T {
	t.Helper()
	var v T
	if err := json.Unmarshal(rr.Body.Bytes(), &v); err != nil {
		t.Fatal(err)
	}
	return v
}

func TestRootProtectionAndNameConflict(t *testing.T) {
	a := newTestApp(t)
	rr := a.request("PATCH", "/api/files/"+RootID, map[string]any{"name": "changed"}, true)
	if rr.Code != 400 {
		t.Fatalf("root rename status=%d", rr.Code)
	}
	first := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Photos"}, true)
	if first.Code != 201 {
		t.Fatalf("create status=%d: %s", first.Code, first.Body.String())
	}
	second := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Photos"}, true)
	if second.Code != 409 {
		t.Fatalf("duplicate status=%d", second.Code)
	}
}

func TestWriteRequestsRequireMatchingOrigin(t *testing.T) {
	a := newTestApp(t)
	body := map[string]any{"parent_id": RootID, "name": "OriginTest"}
	noOrigin := a.requestH("POST", "/api/directories", body, true, map[string]string{"Origin": ""})
	if noOrigin.Code != http.StatusForbidden {
		t.Fatalf("write without Origin status=%d", noOrigin.Code)
	}
	crossOrigin := a.requestH("POST", "/api/directories", body, true, map[string]string{"Origin": "http://evil.example"})
	if crossOrigin.Code != http.StatusForbidden {
		t.Fatalf("cross-origin write status=%d", crossOrigin.Code)
	}
	sameOrigin := a.request("POST", "/api/directories", body, true)
	if sameOrigin.Code != http.StatusCreated {
		t.Fatalf("same-origin write status=%d: %s", sameOrigin.Code, sameOrigin.Body.String())
	}
	// 读请求不受 Origin 限制
	if got := a.request("GET", "/api/storage/stats", nil, true); got.Code != http.StatusOK {
		t.Fatalf("GET with no Origin status=%d", got.Code)
	}
}

func TestDirectoryCannotMoveIntoDescendant(t *testing.T) {
	a := newTestApp(t)
	parentRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Parent"}, true)
	parent := decode[File](t, parentRR)
	childRR := a.request("POST", "/api/directories", map[string]any{"parent_id": parent.ID, "name": "Child"}, true)
	child := decode[File](t, childRR)
	rr := a.request("PATCH", "/api/files/"+parent.ID, map[string]any{"parent_id": child.ID}, true)
	if rr.Code != 400 {
		t.Fatalf("cycle status=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestTrashRestoresTreeAndProtectsContentFromGC(t *testing.T) {
	a := newTestApp(t)
	parentRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Archive"}, true)
	parent := decode[File](t, parentRR)
	childRR := a.request("POST", "/api/directories", map[string]any{"parent_id": parent.ID, "name": "Nested"}, true)
	child := decode[File](t, childRR)
	f := a.readyFile(t, "kept.txt", []byte("recover me"))
	if rr := a.request("PATCH", "/api/files/"+f.ID, map[string]any{"parent_id": child.ID}, true); rr.Code != http.StatusOK {
		t.Fatalf("move file=%d: %s", rr.Code, rr.Body.String())
	}
	shareRR := a.request("POST", "/api/files/"+f.ID+"/share", nil, true)
	share := decode[struct {
		URL string `json:"url"`
	}](t, shareRR)
	shareURL, _ := url.Parse(share.URL)

	if rr := a.request("DELETE", "/api/files/"+parent.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("trash tree=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("trashed descendant remains readable: %d", rr.Code)
	}
	if rr := a.request("GET", shareURL.Path, nil, false); rr.Code != http.StatusNotFound {
		t.Fatalf("trashed share remains readable: %d", rr.Code)
	}
	trashRR := a.request("GET", "/api/trash", nil, true)
	trash := decode[struct {
		Items      []File `json:"items"`
		TotalBytes int64  `json:"total_bytes"`
		FileCount  int64  `json:"file_count"`
	}](t, trashRR)
	if len(trash.Items) != 1 || trash.Items[0].ID != parent.ID || trash.Items[0].DeletedAt == "" {
		t.Fatalf("trash roots=%+v", trash.Items)
	}
	if trash.TotalBytes != int64(len("recover me")) || trash.FileCount != 1 {
		t.Fatalf("trash recursive stats=%+v", trash)
	}
	if rr := a.request("POST", "/api/trash/"+parent.ID+"/restore", nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("restore=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusOK {
		t.Fatalf("restored descendant unavailable: %d", rr.Code)
	}
	if rr := a.request("GET", shareURL.Path, nil, false); rr.Code != http.StatusOK {
		t.Fatalf("restored share unavailable: %d", rr.Code)
	}

	if rr := a.request("DELETE", "/api/files/"+parent.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("trash again=%d", rr.Code)
	}
	if rr := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Archive"}, true); rr.Code != http.StatusCreated {
		t.Fatalf("reuse trashed name=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("POST", "/api/trash/"+parent.ID+"/restore", nil, true); rr.Code != http.StatusConflict {
		t.Fatalf("restore conflict=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("DELETE", "/api/trash/"+parent.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("purge tree=%d: %s", rr.Code, rr.Body.String())
	}
	var remaining int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id IN (?,?,?)`, parent.ID, child.ID, f.ID).Scan(&remaining); err != nil || remaining != 0 {
		t.Fatalf("purged tree remains: count=%d err=%v", remaining, err)
	}
}

func TestTrashFilesRemainReadableUntilPurged(t *testing.T) {
	a := newTestApp(t)
	photo := a.readyFile(t, "photo.png", realPNG(t, 320, 180))
	video := a.readyFile(t, "clip.mp4", []byte("video preview"))
	audio := a.readyFile(t, "song.mp3", []byte("audio preview"))
	book := a.readyFile(t, "novel.txt", []byte("第一章\n回收站里的正文"))
	document := a.readyFile(t, "notes.md", []byte("# 仍可查看"))

	for _, f := range []File{photo, video, audio, book, document} {
		if rr := a.request("DELETE", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("trash %s=%d: %s", f.Name, rr.Code, rr.Body.String())
		}
	}
	for _, f := range []File{photo, video, audio} {
		if rr := a.request("GET", "/api/files/"+f.ID+"/preview", nil, true); rr.Code != http.StatusOK {
			t.Fatalf("trashed preview %s=%d: %s", f.Name, rr.Code, rr.Body.String())
		}
	}
	if rr := a.request("GET", "/api/files/"+photo.ID+"/thumbnail", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed thumbnail=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+audio.ID+"/download", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed download=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+book.ID+"/book/content", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed book=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+document.ID+"/content", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed document=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("DELETE", "/api/trash/"+photo.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("purge photo=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+photo.ID+"/preview", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("purged preview remains readable: %d", rr.Code)
	}
}

func TestEmptyTrashRemovesEveryDeletedTree(t *testing.T) {
	a := newTestApp(t)
	first := a.readyFile(t, "first.txt", []byte("first"))
	second := a.readyFile(t, "second.txt", []byte("second"))
	for _, f := range []File{first, second} {
		if rr := a.request("DELETE", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("trash %s=%d", f.Name, rr.Code)
		}
	}
	if rr := a.request("DELETE", "/api/trash", nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("empty trash=%d: %s", rr.Code, rr.Body.String())
	}
	trash := a.request("GET", "/api/trash", nil, true)
	items := decode[struct {
		Items []File `json:"items"`
	}](t, trash)
	if len(items.Items) != 0 {
		t.Fatalf("trash is not empty: %+v", items.Items)
	}
}

func TestExpiredTrashCleanupRemovesOnlyExpiredTrees(t *testing.T) {
	a := newTestApp(t)
	parentRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Expired"}, true)
	parent := decode[File](t, parentRR)
	child := a.readyFile(t, "child.txt", []byte("expired content"))
	if rr := a.request("PATCH", "/api/files/"+child.ID, map[string]any{"parent_id": parent.ID}, true); rr.Code != http.StatusOK {
		t.Fatalf("move child=%d: %s", rr.Code, rr.Body.String())
	}
	recent := a.readyFile(t, "recent.txt", []byte("recent content"))
	for _, id := range []string{parent.ID, recent.ID} {
		if rr := a.request("DELETE", "/api/files/"+id, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("trash %s=%d: %s", id, rr.Code, rr.Body.String())
		}
	}
	old := time.Now().UTC().Add(-31 * 24 * time.Hour).Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`UPDATE files SET deleted_at=? WHERE trash_root_id=?`, old, parent.ID); err != nil {
		t.Fatal(err)
	}
	if roots := a.srv.CleanupExpiredTrash(context.Background()); roots != 1 {
		t.Fatalf("expired roots=%d, want 1", roots)
	}
	var expiredItems int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id IN (?,?)`, parent.ID, child.ID).Scan(&expiredItems); err != nil || expiredItems != 0 {
		t.Fatalf("expired tree remains: count=%d err=%v", expiredItems, err)
	}
	var recentItems int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=? AND deleted_at IS NOT NULL`, recent.ID).Scan(&recentItems); err != nil || recentItems != 1 {
		t.Fatalf("recent trash was deleted: count=%d err=%v", recentItems, err)
	}
	a.srv.CollectGarbage(context.Background())
	if _, ok := a.store.manifests[child.objectKey]; ok {
		t.Fatal("content of expired trash was not reclaimed")
	}
	if roots := a.srv.CleanupExpiredTrash(context.Background()); roots != 0 {
		t.Fatalf("second cleanup removed %d roots", roots)
	}
}

func TestShareLinkCanBeReadRotatedAndRevoked(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "profile.yaml", []byte("name: value\n"))
	createdRR := a.request("POST", "/api/files/"+f.ID+"/share", nil, true)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create share=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[struct {
		Active bool   `json:"active"`
		URL    string `json:"url"`
	}](t, createdRR)
	if !created.Active {
		t.Fatal("created share is inactive")
	}
	shareURL, err := url.Parse(created.URL)
	if err != nil {
		t.Fatal(err)
	}
	publicRR := a.request("GET", shareURL.Path, nil, false)
	if publicRR.Code != http.StatusOK || publicRR.Body.String() != "name: value\n" {
		t.Fatalf("public share=%d body=%q", publicRR.Code, publicRR.Body.String())
	}
	statusRR := a.request("GET", "/api/files/"+f.ID+"/share", nil, true)
	status := decode[struct {
		Active bool   `json:"active"`
		URL    string `json:"url"`
	}](t, statusRR)
	if !status.Active || status.URL != created.URL {
		t.Fatalf("share status=%+v", status)
	}
	rotatedRR := a.request("POST", "/api/files/"+f.ID+"/share", nil, true)
	rotated := decode[struct {
		URL string `json:"url"`
	}](t, rotatedRR)
	if rotated.URL == created.URL {
		t.Fatal("rotating share reused token")
	}
	if oldRR := a.request("GET", shareURL.Path, nil, false); oldRR.Code != http.StatusNotFound {
		t.Fatalf("old share remains active: %d", oldRR.Code)
	}
	if revokedRR := a.request("DELETE", "/api/files/"+f.ID+"/share", nil, true); revokedRR.Code != http.StatusNoContent {
		t.Fatalf("revoke share=%d", revokedRR.Code)
	}
	rotatedURL, _ := url.Parse(rotated.URL)
	if publicRR := a.request("GET", rotatedURL.Path, nil, false); publicRR.Code != http.StatusNotFound {
		t.Fatalf("revoked share remains active: %d", publicRR.Code)
	}
}

func TestPublicShareStreamsMultiBlockFiles(t *testing.T) {
	a := newTestAppWithBlockSize(t, 8)
	content := []byte("0123456789ABCDEFGHIJ")
	f := a.readyFile(t, "clip.mp4", content)
	share := a.request("POST", "/api/files/"+f.ID+"/share", nil, true)
	created := decode[struct {
		URL string `json:"url"`
	}](t, share)
	u, _ := url.Parse(created.URL)
	rr := a.requestH("GET", u.Path, nil, false, map[string]string{"Range": "bytes=5-13"})
	if rr.Code != http.StatusPartialContent {
		t.Fatalf("shared range status=%d: %s", rr.Code, rr.Body.String())
	}
	if rr.Body.String() != string(content[5:14]) {
		t.Fatalf("shared range body=%q", rr.Body.String())
	}
}

func TestResponseMimeRecognizesYAML(t *testing.T) {
	got := responseMime(File{Name: "profile.yaml", MimeType: "application/octet-stream"})
	if got != "application/yaml; charset=utf-8" {
		t.Fatalf("yaml content type=%q", got)
	}
}

func TestMediaPreviewAndStorageStats(t *testing.T) {
	a := newTestApp(t)
	files := []struct {
		name, mime string
		size       int64
	}{
		{"clip.mp4", "application/octet-stream", 2048},
		{"song.wav", "application/octet-stream", 4096},
		{"animated.gif", "image/gif", 1024},
	}
	created := make([]File, 0, len(files))
	for _, f := range files {
		created = append(created, a.readyFile(t, f.name, bytes.Repeat([]byte("x"), int(f.size))))
	}
	folderRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Nested"}, true)
	folder := decode[File](t, folderRR)
	if moved := a.request("PATCH", "/api/files/"+created[1].ID, map[string]any{"parent_id": folder.ID}, true); moved.Code != http.StatusOK {
		t.Fatalf("move nested media=%d: %s", moved.Code, moved.Body.String())
	}
	rr := a.request("GET", "/api/files/"+RootID+"/children", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("children=%d: %s", rr.Code, rr.Body.String())
	}
	items := decode[struct {
		Items      []File `json:"items"`
		TotalBytes int64  `json:"total_bytes"`
		FileCount  int64  `json:"file_count"`
	}](t, rr)
	if items.TotalBytes != 7168 || items.FileCount != 3 {
		t.Fatalf("recursive directory stats=%+v", items)
	}
	for _, item := range items.Items {
		if item.Kind != "file" {
			continue
		}
		preview := a.request("GET", "/api/files/"+item.ID+"/preview", nil, true)
		if preview.Code != http.StatusOK {
			t.Fatalf("preview %s=%d: %s", item.Name, preview.Code, preview.Body.String())
		}
	}
	if got := responseMime(File{Name: "song.mp3", MimeType: "application/octet-stream"}); got != "audio/mpeg" {
		t.Fatalf("mp3 content type=%q", got)
	}
	statsRR := a.request("GET", "/api/storage/stats", nil, true)
	if statsRR.Code != http.StatusOK {
		t.Fatalf("storage stats=%d: %s", statsRR.Code, statsRR.Body.String())
	}
	stats := decode[struct {
		TotalBytes int64 `json:"total_bytes"`
		FileCount  int64 `json:"file_count"`
	}](t, statsRR)
	if stats.TotalBytes != 7168 || stats.FileCount != 3 {
		t.Fatalf("storage stats=%+v", stats)
	}
}

func TestCreateReadAndUpdateDocument(t *testing.T) {
	a := newTestApp(t)
	createdRR := a.request("POST", "/api/documents", map[string]any{"parent_id": RootID, "name": "notes.md", "content": "# First\n"}, true)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create document=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[File](t, createdRR)
	if created.Status != "ready" || created.MimeType != "text/markdown; charset=utf-8" {
		t.Fatalf("created document=%+v", created)
	}
	var firstKey string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.ID).Scan(&firstKey); err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(firstKey, "blobs/") {
		t.Fatalf("document object key=%q", firstKey)
	}
	readRR := a.request("GET", "/api/files/"+created.ID+"/content", nil, true)
	if readRR.Code != http.StatusOK {
		t.Fatalf("read document=%d: %s", readRR.Code, readRR.Body.String())
	}
	read := decode[struct {
		Content string `json:"content"`
		ETag    string `json:"etag"`
	}](t, readRR)
	if read.Content != "# First\n" || read.ETag == "" {
		t.Fatalf("document content=%q etag=%q", read.Content, read.ETag)
	}
	conflictRR := a.request("PUT", "/api/files/"+created.ID+"/content", map[string]any{"content": "changed", "etag": "wrong"}, true)
	if conflictRR.Code != http.StatusConflict {
		t.Fatalf("stale edit=%d: %s", conflictRR.Code, conflictRR.Body.String())
	}
	updatedRR := a.request("PUT", "/api/files/"+created.ID+"/content", map[string]any{"content": "# Saved\n", "etag": read.ETag}, true)
	if updatedRR.Code != http.StatusOK {
		t.Fatalf("update document=%d: %s", updatedRR.Code, updatedRR.Body.String())
	}
	var secondKey string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.ID).Scan(&secondKey); err != nil {
		t.Fatal(err)
	}
	if secondKey == firstKey {
		t.Fatal("updated document kept the old manifest")
	}
	reread := a.request("GET", "/api/files/"+created.ID+"/content", nil, true)
	got := decode[struct {
		Content string `json:"content"`
	}](t, reread)
	if got.Content != "# Saved\n" {
		t.Fatalf("updated content=%q", got.Content)
	}
}

func TestSingleObjectUploadLifecycle(t *testing.T) {
	a := newTestApp(t)
	content := []byte("hello, world")
	created := a.createUpload(t, "hello.txt", int64(len(content)))
	if created.Mode != "single" || created.URL == "" || created.PartCount != 0 {
		t.Fatalf("created upload=%+v", created)
	}
	var key string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.FileID).Scan(&key); err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(key, "blobs/") || strings.Contains(key, "hello") {
		t.Fatalf("opaque object key=%q", key)
	}
	if rr := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []any{}}, true); rr.Code != http.StatusBadGateway {
		t.Fatalf("complete before PUT=%d: %s", rr.Code, rr.Body.String())
	}
	a.store.raw[key] = append([]byte(nil), content...)
	doneRR := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []any{}}, true)
	if doneRR.Code != http.StatusOK {
		t.Fatalf("complete=%d: %s", doneRR.Code, doneRR.Body.String())
	}
	download := a.request("GET", "/api/files/"+created.FileID+"/download", nil, true)
	if download.Code != http.StatusOK || download.Body.String() != string(content) {
		t.Fatalf("proxied download=%d body=%q", download.Code, download.Body.String())
	}
}

func TestMultipartUploadLifecycle(t *testing.T) {
	a := newTestApp(t)
	size := multipartUploadThreshold
	created := a.createUpload(t, "large.bin", size)
	if created.Mode != "multipart" || created.PartCount != 1 || created.PartSize < 5<<20 {
		t.Fatalf("created multipart=%+v", created)
	}
	partsRR := a.request("POST", "/api/uploads/"+created.UploadID+"/parts", map[string]any{"part_numbers": []int{1}}, true)
	if partsRR.Code != http.StatusOK || !strings.Contains(partsRR.Body.String(), "multipart") {
		t.Fatalf("parts=%d: %s", partsRR.Code, partsRR.Body.String())
	}
	if rr := a.request("POST", "/api/uploads/"+created.UploadID+"/parts", map[string]any{"part_numbers": []int{2}}, true); rr.Code != http.StatusBadRequest {
		t.Fatalf("invalid part=%d", rr.Code)
	}
	var key string
	_ = a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.FileID).Scan(&key)
	a.store.raw[key] = make([]byte, size)
	done := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []map[string]any{{"part_number": 1, "etag": "part-etag"}}}, true)
	if done.Code != http.StatusOK {
		t.Fatalf("multipart complete=%d: %s", done.Code, done.Body.String())
	}
}

func TestEmptyFileUpload(t *testing.T) {
	a := newTestApp(t)
	created := a.createUpload(t, "empty.bin", 0)
	var key string
	_ = a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.FileID).Scan(&key)
	a.store.raw[key] = []byte{}
	doneRR := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []map[string]any{}}, true)
	if doneRR.Code != http.StatusOK {
		t.Fatalf("complete=%d: %s", doneRR.Code, doneRR.Body.String())
	}
	dl := a.request("GET", "/api/files/"+created.FileID+"/download", nil, true)
	if dl.Code != http.StatusOK {
		t.Fatalf("empty download=%d", dl.Code)
	}
}

func TestDeletePendingFile(t *testing.T) {
	a := newTestApp(t)
	created := a.createUpload(t, "pending.bin", 100)
	// 上传失败/中断遗留的 pending 行可以直接删除，uploads 记录级联清理。
	if rr := a.request("DELETE", "/api/files/"+created.FileID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("delete pending=%d: %s", rr.Code, rr.Body.String())
	}
	var n int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, created.FileID).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatal("pending file row not deleted")
	}
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM uploads WHERE id=?`, created.UploadID).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatal("uploads row not cascaded")
	}
}

func TestAbortUpload(t *testing.T) {
	a := newTestApp(t)
	created := a.createUpload(t, "pending.bin", 100)
	if rr := a.request("DELETE", "/api/uploads/"+created.UploadID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("abort=%d: %s", rr.Code, rr.Body.String())
	}
	var n int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, created.FileID).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatal("aborted upload left its file row behind")
	}
}

func TestGarbageCollector(t *testing.T) {
	a := newTestApp(t)
	live := a.readyFile(t, "live.bin", []byte("live"))
	orphan := storage.BlobKey(ids.New())
	a.store.raw[orphan] = []byte("orphan")
	a.store.age(orphan, time.Now().Add(-48*time.Hour))
	a.store.age(live.objectKey, time.Now().Add(-48*time.Hour))
	a.srv.CollectGarbage(context.Background())
	if _, ok := a.store.raw[live.objectKey]; !ok {
		t.Fatal("referenced blob was collected")
	}
	if _, ok := a.store.raw[orphan]; ok {
		t.Fatal("orphaned blob was not collected")
	}
}

func TestGarbageCollectorKeepsAudioStreamAndCover(t *testing.T) {
	a := newTestAppWithBlockSize(t, 8)
	master := a.readyFile(t, "book.flac", []byte("lossless-master"))
	streamKey := master.objectKey
	coverKey := thumbnailKey(master.objectKey)
	a.store.raw[coverKey] = []byte("jpeg-cover")
	a.store.age(coverKey, time.Now().Add(-48*time.Hour))
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`,
		master.ID, 1000, `[{"title":"Part 1","start_ms":0,"end_ms":1000}]`, streamKey, master.Size, master.ETag, true, now, now); err != nil {
		t.Fatal(err)
	}
	a.srv.CollectGarbage(context.Background())
	if _, ok := a.store.raw[streamKey]; !ok {
		t.Fatal("referenced audio stream blob was collected")
	}
	if _, ok := a.store.raw[coverKey]; !ok {
		t.Fatal("referenced audio cover was collected")
	}
}

func TestCopyPreservesAudioMetadata(t *testing.T) {
	a := newTestApp(t)
	audio := a.readyFile(t, "album.m4a", []byte("audio-master"))
	now := time.Now().UTC().Format(time.RFC3339Nano)
	chapters := `[{"title":"第一节","start_ms":0,"end_ms":10000},{"title":"第二节","start_ms":10000,"end_ms":25000}]`
	if _, err := a.db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`, audio.ID, 25000, chapters, audio.objectKey, audio.Size, audio.ETag, false, now, now); err != nil {
		t.Fatal(err)
	}

	copyRR := a.request("POST", "/api/files/"+audio.ID+"/copy", map[string]any{"parent_id": RootID}, true)
	if copyRR.Code != http.StatusCreated {
		t.Fatalf("copy=%d: %s", copyRR.Code, copyRR.Body.String())
	}
	copied := decode[File](t, copyRR)
	if copied.Name != "album - 副本.m4a" || copied.Size != audio.Size || copied.ETag != audio.ETag {
		t.Fatalf("copied file=%+v", copied)
	}
	storedCopy, err := a.srv.file(context.Background(), copied.ID)
	if err != nil || storedCopy.objectKey != audio.objectKey {
		t.Fatalf("copy object key=%q err=%v", storedCopy.objectKey, err)
	}
	infoRR := a.request("GET", "/api/files/"+copied.ID+"/audio", nil, true)
	if infoRR.Code != http.StatusOK {
		t.Fatalf("copied audio info=%d: %s", infoRR.Code, infoRR.Body.String())
	}
	info := decode[struct {
		Chapters []audioChapterResponse `json:"chapters"`
	}](t, infoRR)
	if len(info.Chapters) != 2 || info.Chapters[1].Title != "第二节" {
		t.Fatalf("copied chapters=%+v", info.Chapters)
	}
}

func TestCopyRejectsDirectoryAndInvalidTarget(t *testing.T) {
	a := newTestApp(t)
	directoryRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "folder"}, true)
	directory := decode[File](t, directoryRR)
	if rr := a.request("POST", "/api/files/"+directory.ID+"/copy", map[string]any{"parent_id": RootID}, true); rr.Code != http.StatusNotFound {
		t.Fatalf("directory copy=%d: %s", rr.Code, rr.Body.String())
	}
	file := a.readyFile(t, "note.txt", []byte("hello"))
	if rr := a.request("POST", "/api/files/"+file.ID+"/copy", map[string]any{"parent_id": file.ID}, true); rr.Code != http.StatusBadRequest {
		t.Fatalf("invalid target=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestSafeDeliveryMimeBlocksActiveWebContent(t *testing.T) {
	for _, value := range []string{"text/html; charset=utf-8", "image/svg+xml", "application/javascript", "application/xhtml+xml"} {
		if got := safeDeliveryMime(value); got != "application/octet-stream" {
			t.Fatalf("safeDeliveryMime(%q)=%q", value, got)
		}
	}
	if got := safeDeliveryMime("image/png"); got != "image/png" {
		t.Fatalf("safe image MIME changed to %q", got)
	}
}

func TestDocumentSaveConflictOnStaleEtag(t *testing.T) {
	a := newTestApp(t)
	doc := a.readyFile(t, "note.md", []byte("v1"))
	first := a.request("PUT", "/api/files/"+doc.ID+"/content", map[string]any{"content": "v2", "etag": doc.ETag}, true)
	if first.Code != http.StatusOK {
		t.Fatalf("first save=%d: %s", first.Code, first.Body.String())
	}
	// 用过期 etag 再保存：必须 409，而不是静默覆盖
	stale := a.request("PUT", "/api/files/"+doc.ID+"/content", map[string]any{"content": "v3", "etag": doc.ETag}, true)
	if stale.Code != http.StatusConflict {
		t.Fatalf("stale etag save=%d, want 409", stale.Code)
	}
	// 不带 etag 的保存仍然放行（旧客户端兼容）
	noEtag := a.request("PUT", "/api/files/"+doc.ID+"/content", map[string]any{"content": "v4"}, true)
	if noEtag.Code != http.StatusOK {
		t.Fatalf("no-etag save=%d", noEtag.Code)
	}
}

func TestExpiredUploadCannotComplete(t *testing.T) {
	a := newTestApp(t)
	u := a.createUpload(t, "expired.bin", 100)
	// 把 expires_at 改为过去，模拟过期但尚未被定时清理的窗口期
	if _, err := a.db.Exec(`UPDATE uploads SET expires_at=? WHERE id=?`, time.Now().UTC().Add(-time.Minute).Format(time.RFC3339Nano), u.UploadID); err != nil {
		t.Fatal(err)
	}
	body := map[string]any{"parts": []any{}}
	if rr := a.request("POST", "/api/uploads/"+u.UploadID+"/complete", body, true); rr.Code != http.StatusNotFound {
		t.Fatalf("expired complete=%d, want 404: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("POST", "/api/uploads/"+u.UploadID+"/parts", map[string]any{"part_numbers": []int{1}}, true); rr.Code != http.StatusNotFound {
		t.Fatalf("expired parts=%d, want 404", rr.Code)
	}
}

func TestUnknownAPIEndpointReturnsJSON404(t *testing.T) {
	a := newTestApp(t)
	rr := a.request("GET", "/api/does-not-exist", nil, true)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("unknown api=%d, want 404", rr.Code)
	}
	if ct := rr.Header().Get("Content-Type"); !strings.HasPrefix(ct, "application/json") {
		t.Fatalf("content-type=%q, want JSON", ct)
	}
}

func TestMultipartPartSizeStaysWithinS3Limit(t *testing.T) {
	partSize := multipartPartSize(1 << 40)
	if count, err := storage.ValidMultipartPartCount(1<<40, partSize); err != nil || count > 10000 {
		t.Fatalf("part size=%d count=%d err=%v", partSize, count, err)
	}
}

// ---- 内置阅读器 ----

func fakePNG(w, h int) []byte {
	data := make([]byte, 33)
	copy(data, []byte("\x89PNG\r\n\x1a\n"))
	data[12], data[13], data[14], data[15] = 'I', 'H', 'D', 'R'
	data[16] = byte(w >> 24)
	data[17] = byte(w >> 16)
	data[18] = byte(w >> 8)
	data[19] = byte(w)
	data[20] = byte(h >> 24)
	data[21] = byte(h >> 16)
	data[22] = byte(h >> 8)
	data[23] = byte(h)
	return data
}

func realPNG(t *testing.T, w, h int) []byte {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for i := range img.Pix {
		img.Pix[i] = 0x88
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func realJPEG(t *testing.T, w, h int) []byte {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, img, &jpeg.Options{Quality: 80}); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func (a *testApp) rawRequest(method, path string, body []byte, authenticated bool) *httptest.ResponseRecorder {
	a.t.Helper()
	req := httptest.NewRequest(method, path, bytes.NewReader(body))
	if body != nil {
		req.Header.Set("Content-Type", "application/octet-stream")
	}
	// 同源浏览器语义：写请求带 Origin
	if method != http.MethodGet && method != http.MethodHead && method != http.MethodOptions {
		req.Header.Set("Origin", "http://example.test")
	}
	if authenticated && a.cookie != nil {
		req.AddCookie(a.cookie)
	}
	rr := httptest.NewRecorder()
	a.handler.ServeHTTP(rr, req)
	return rr
}

func buildEPUB(t *testing.T) []byte {
	t.Helper()
	buf := &bytes.Buffer{}
	zw := zip.NewWriter(buf)
	write := func(name, content string) {
		w, err := zw.Create(name)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = w.Write([]byte(content))
	}
	write("mimetype", "application/epub+zip")
	write("META-INF/container.xml", `<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>`)
	write("OEBPS/content.opf", `<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>测试书</dc:title><meta name="cover" content="cover-img"/></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="cover-img" href="img/cover.png" media-type="image/png"/><item id="fig" href="img/fig.png" media-type="image/png"/></manifest><spine><itemref idref="ch1"/></spine></package>`)
	write("OEBPS/nav.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#sec1">第一章</a></li></ol></nav></body></html>`)
	write("OEBPS/ch1.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="sec1">第一章 开始</h1><p>你好世界</p><script>alert(1)</script><p><img src="img/fig.png" alt="插图"/></p></body></html>`)
	write("OEBPS/img/cover.png", string(realPNG(t, 300, 400)))
	write("OEBPS/img/fig.png", string(fakePNG(10, 20)))
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestBookEndpointsTXT(t *testing.T) {
	a := newTestApp(t)
	content := []byte("第一章 开始\n正文一行\n第二章 继续\n")
	f := a.readyFile(t, "book.txt", content)
	info := a.request("GET", "/api/files/"+f.ID+"/book", nil, true)
	if info.Code != http.StatusOK {
		t.Fatalf("book info=%d: %s", info.Code, info.Body.String())
	}
	meta := decode[struct {
		Format string `json:"format"`
		Title  string `json:"title"`
		Cover  bool   `json:"cover"`
		TOC    []struct {
			Label  string `json:"label"`
			Offset int64  `json:"offset"`
		} `json:"toc"`
	}](t, info)
	if meta.Format != "txt" || meta.Cover || len(meta.TOC) != 2 || meta.TOC[0].Label != "第一章 开始" {
		t.Fatalf("meta=%+v", meta)
	}
	body := a.request("GET", "/api/files/"+f.ID+"/book/content", nil, true)
	if body.Code != http.StatusOK {
		t.Fatalf("content=%d: %s", body.Code, body.Body.String())
	}
	model := decode[struct {
		Kind string `json:"kind"`
		Text string `json:"text"`
	}](t, body)
	if model.Kind != "txt" || model.Text != string(content) {
		t.Fatalf("model=%+v", model)
	}
	put := a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{"page": 3, "total_pages": 10}, true)
	if put.Code != http.StatusNoContent {
		t.Fatalf("save progress=%d: %s", put.Code, put.Body.String())
	}
	got := a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true)
	progress := decode[struct {
		Page       int64  `json:"page"`
		TotalPages *int64 `json:"total_pages"`
	}](t, got)
	if progress.Page != 3 || progress.TotalPages == nil || *progress.TotalPages != 10 {
		t.Fatalf("progress=%+v", progress)
	}
	pdf := a.readyFile(t, "doc.pdf", []byte("x"))
	if rr := a.request("GET", "/api/files/"+pdf.ID+"/book", nil, true); rr.Code != http.StatusUnsupportedMediaType {
		t.Fatalf("pdf book=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestBookEndpointsEPUB(t *testing.T) {
	a := newTestApp(t)
	fixture := buildEPUB(t)
	f := a.readyFile(t, "book.epub", fixture)
	info := a.request("GET", "/api/files/"+f.ID+"/book", nil, true)
	if info.Code != http.StatusOK {
		t.Fatalf("book info=%d: %s", info.Code, info.Body.String())
	}
	meta := decode[struct {
		Format string `json:"format"`
		Title  string `json:"title"`
		Cover  bool   `json:"cover"`
	}](t, info)
	if meta.Format != "epub" || meta.Title != "测试书" || !meta.Cover {
		t.Fatalf("meta=%+v", meta)
	}
	body := a.request("GET", "/api/files/"+f.ID+"/book/content", nil, true)
	if body.Code != http.StatusOK {
		t.Fatalf("content=%d: %s", body.Code, body.Body.String())
	}
	model := decode[struct {
		Kind     string `json:"kind"`
		Chapters []struct {
			HTML string `json:"html"`
		} `json:"chapters"`
	}](t, body)
	if model.Kind != "epub" || len(model.Chapters) != 1 || !strings.Contains(model.Chapters[0].HTML, "你好世界") || strings.Contains(model.Chapters[0].HTML, "<script") {
		t.Fatalf("chapters=%+v", model.Chapters)
	}
	asset := a.request("GET", "/api/files/"+f.ID+"/book/assets/0", nil, true)
	if asset.Code != http.StatusOK || asset.Header().Get("Content-Type") != "image/png" || asset.Body.Len() != 33 {
		t.Fatalf("asset=%d type=%q bytes=%d", asset.Code, asset.Header().Get("Content-Type"), asset.Body.Len())
	}
	cover := a.request("GET", "/api/files/"+f.ID+"/book/cover", nil, true)
	if cover.Code != http.StatusOK || cover.Header().Get("Content-Type") != "image/png" || cover.Body.Len() == 0 {
		t.Fatalf("cover=%d type=%q bytes=%d", cover.Code, cover.Header().Get("Content-Type"), cover.Body.Len())
	}
	if missing := a.request("GET", "/api/files/"+f.ID+"/book/assets/9", nil, true); missing.Code != http.StatusNotFound {
		t.Fatalf("missing asset=%d", missing.Code)
	}
}

func TestThumbnails(t *testing.T) {
	a := newTestApp(t)

	// 图片：服务端生成 JPEG 缩略图并持久化到 S3（内容寻址），缓存头可长期缓存。
	photo := a.readyFile(t, "photo.png", realPNG(t, 640, 320))
	rr := a.request("GET", "/api/files/"+photo.ID+"/thumbnail", nil, true)
	if rr.Code != http.StatusOK || rr.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("thumb=%d type=%q", rr.Code, rr.Header().Get("Content-Type"))
	}
	if !bytes.HasPrefix(rr.Body.Bytes(), []byte{0xFF, 0xD8}) {
		t.Fatalf("thumb is not a JPEG: % x", rr.Body.Bytes()[:4])
	}
	if cc := rr.Header().Get("Cache-Control"); !strings.Contains(cc, "immutable") {
		t.Fatalf("cache-control=%q", cc)
	}
	// 第二次请求应直接命中已落盘的缩略图对象
	if again := a.request("GET", "/api/files/"+photo.ID+"/thumbnail", nil, true); again.Code != http.StatusOK || !bytes.Equal(again.Body.Bytes(), rr.Body.Bytes()) {
		t.Fatal("cached thumbnail not served consistently")
	}

	// 视频：前端抽帧后 PUT 上传，之后 GET 命中；非 JPEG 拒绝。
	video := a.readyFile(t, "clip.mp4", []byte("video-bytes"))
	if missing := a.request("GET", "/api/files/"+video.ID+"/thumbnail", nil, true); missing.Code != http.StatusNotFound {
		t.Fatalf("missing video thumb=%d", missing.Code)
	}
	thumb := realJPEG(t, 48, 27)
	if put := a.rawRequest("PUT", "/api/files/"+video.ID+"/thumbnail", thumb, true); put.Code != http.StatusNoContent {
		t.Fatalf("put thumb=%d: %s", put.Code, put.Body.String())
	}
	got := a.request("GET", "/api/files/"+video.ID+"/thumbnail", nil, true)
	if got.Code != http.StatusOK || got.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("stored video thumb=%d type=%q", got.Code, got.Header().Get("Content-Type"))
	}
	decoded, format, err := image.Decode(bytes.NewReader(got.Body.Bytes()))
	if err != nil || format != "jpeg" {
		t.Fatalf("stored video thumb is not a valid JPEG: format=%q err=%v", format, err)
	}
	if bounds := decoded.Bounds(); bounds.Dx() != 48 || bounds.Dy() != 27 {
		t.Fatalf("stored video thumb size=%dx%d", bounds.Dx(), bounds.Dy())
	}
	if bad := a.rawRequest("PUT", "/api/files/"+video.ID+"/thumbnail", []byte("not-a-jpeg"), true); bad.Code != http.StatusBadRequest {
		t.Fatalf("bad thumb accepted: %d", bad.Code)
	}

	// EPUB：缩略图 = 缩小后的内嵌封面。
	epub := a.readyFile(t, "book.epub", buildEPUB(t))
	et := a.request("GET", "/api/files/"+epub.ID+"/thumbnail", nil, true)
	if et.Code != http.StatusOK || !bytes.HasPrefix(et.Body.Bytes(), []byte{0xFF, 0xD8}) {
		t.Fatalf("epub thumb=%d", et.Code)
	}

	// 无封面/不支持的类型返回 404，前端回退原图或图标。
	txt := a.readyFile(t, "notes.txt", []byte("hello"))
	if rr := a.request("GET", "/api/files/"+txt.ID+"/thumbnail", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("txt thumb=%d", rr.Code)
	}
}

func TestVideoThumbnailWithFFmpeg(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	a := newTestApp(t)
	// 用 ffmpeg 现场生成一段 2 秒测试视频，确保服务端在第 1 秒抽帧时
	// 不会刚好落在媒体结尾。
	tmp := t.TempDir() + "/test.mp4"
	cmd := exec.Command("ffmpeg", "-hide_banner", "-loglevel", "error",
		"-f", "lavfi", "-i", "testsrc=size=160x90:rate=10", "-t", "2", "-pix_fmt", "yuv420p", tmp)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("ffmpeg fixture failed: %v %s", err, out)
	}
	data, err := os.ReadFile(tmp)
	if err != nil {
		t.Fatal(err)
	}
	f := a.readyFile(t, "clip.mp4", data)
	rr := a.request("GET", "/api/files/"+f.ID+"/thumbnail", nil, true)
	if rr.Code != http.StatusOK || rr.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("video thumb=%d type=%q", rr.Code, rr.Header().Get("Content-Type"))
	}
	if !bytes.HasPrefix(rr.Body.Bytes(), []byte{0xFF, 0xD8}) {
		t.Fatalf("video thumb is not a JPEG: % x", rr.Body.Bytes()[:4])
	}
	// 第二次应直接命中持久化对象
	if again := a.request("GET", "/api/files/"+f.ID+"/thumbnail", nil, true); again.Code != http.StatusOK || !bytes.Equal(again.Body.Bytes(), rr.Body.Bytes()) {
		t.Fatal("persisted video thumbnail not served consistently")
	}
}

func TestMediaProgressSync(t *testing.T) {
	a := newTestApp(t)
	audio := a.readyFile(t, "episode.mp3", []byte("audio"))

	empty := a.request("GET", "/api/files/"+audio.ID+"/media/progress", nil, true)
	if empty.Code != http.StatusOK {
		t.Fatalf("empty progress=%d: %s", empty.Code, empty.Body.String())
	}
	put := a.request("PUT", "/api/files/"+audio.ID+"/media/progress", map[string]any{"position": 123.456, "duration": 600}, true)
	if put.Code != http.StatusOK {
		t.Fatalf("save progress=%d: %s", put.Code, put.Body.String())
	}
	got := a.request("GET", "/api/files/"+audio.ID+"/media/progress", nil, true)
	progress := decode[mediaProgressResponse](t, got)
	if progress.Position != 123.456 || progress.Duration != 600 || progress.UpdatedAt == "" {
		t.Fatalf("progress=%+v", progress)
	}
}
