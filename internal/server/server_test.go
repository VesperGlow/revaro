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
	"net/netip"
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
)

func TestClientIPOnlyTrustsConfiguredProxy(t *testing.T) {
	s := &Server{cfg: config.Config{TrustedProxies: []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}}}
	r := httptest.NewRequest(http.MethodPost, "/api/auth/login", nil)
	r.RemoteAddr = "10.1.2.3:1234"
	r.Header.Set("X-Forwarded-For", "198.51.100.7, 203.0.113.8")
	if got := s.clientIP(r); got != "203.0.113.8" {
		t.Fatalf("trusted proxy client IP = %q", got)
	}
	r.RemoteAddr = "192.0.2.10:1234"
	if got := s.clientIP(r); got != "192.0.2.10" {
		t.Fatalf("untrusted peer spoofed client IP = %q", got)
	}
}

func TestLoginLimiterHasBoundedState(t *testing.T) {
	l := newLoginLimiter()
	for i := 0; i < maxLoginLimiterEntries+100; i++ {
		l.fail(fmt.Sprintf("192.0.2.%d", i))
	}
	if len(l.attempts) > maxLoginLimiterEntries {
		t.Fatalf("limiter entries = %d", len(l.attempts))
	}
}

func TestServerCloseWaitsForOwnedWorkAndRejectsNewWork(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	s := &Server{
		audioHLSCtx: ctx, audioHLSCancel: cancel,
		jobs:              NewJobManager(),
		audioHLSSessions:  make(map[string]*audioHLSSession),
		videoHLSSessions:  make(map[string]*videoHLSSession),
		videoFMP4Sessions: make(map[string]*videoFMP4Session),
		archiveJobs:       make(map[string]*archiveJob),
	}
	started := make(chan struct{})
	finished := make(chan struct{})
	if !s.runBackground(func() {
		close(started)
		<-ctx.Done()
		close(finished)
	}) {
		t.Fatal("background work was rejected before shutdown")
	}
	<-started
	s.Close()
	select {
	case <-finished:
	default:
		t.Fatal("Close returned before owned work exited")
	}
	if s.runBackground(func() {}) {
		t.Fatal("background work was admitted after shutdown")
	}
}

// notFoundError emulates the S3 NoSuchKey API error the real store returns.
func notFoundError() error {
	return storage.ErrNotFound
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
	deleteBatchSizes []int
	storeBlobErr     error
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
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.multipart[uploadID] != key {
		return storage.ObjectInfo{}, notFoundError()
	}
	delete(m.multipart, uploadID)
	data, ok := m.raw[key]
	if !ok {
		return storage.ObjectInfo{}, notFoundError()
	}
	return storage.ObjectInfo{Size: int64(len(data)), ETag: "etag"}, nil
}
func (m *mockStorage) AbortMultipart(_ context.Context, key, uploadID string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.multipart[uploadID] == key {
		delete(m.multipart, uploadID)
	}
	return nil
}
func (m *mockStorage) HeadObject(_ context.Context, key string) (storage.ObjectInfo, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	data, ok := m.raw[key]
	if !ok {
		return storage.ObjectInfo{}, notFoundError()
	}
	return storage.ObjectInfo{Size: int64(len(data)), ETag: "etag"}, nil
}
func (m *mockStorage) StoreBlob(_ context.Context, key, mimeType string, r io.Reader, size int64) (storage.ObjectInfo, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.storeBlobErr != nil {
		return storage.ObjectInfo{}, m.storeBlobErr
	}
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
	m.mu.Lock()
	defer m.mu.Unlock()
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
	m.mu.RLock()
	defer m.mu.RUnlock()
	var out []storage.ObjectRef
	for id, data := range m.blocks {
		key := "blocks/" + id
		out = append(out, storage.ObjectRef{Key: key, Size: int64(len(data)), LastModified: m.modified[key]})
	}
	return out, nil
}
func (m *mockStorage) PutManifest(_ context.Context, mm testManifest) (string, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.putManifestErr != nil {
		return "", m.putManifestErr
	}
	key := mm.Key()
	m.manifests[key] = mm
	return key, nil
}
func (m *mockStorage) GetManifest(_ context.Context, key string) (testManifest, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
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
	m.mu.RLock()
	defer m.mu.RUnlock()
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
	m.mu.Lock()
	defer m.mu.Unlock()
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
	key := mm.Key()
	m.manifests[key] = mm
	return key, mm, nil
}
func (m *mockStorage) Open(_ context.Context, key string) (storage.ReadSeekCloserAt, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
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
	m.mu.Lock()
	defer m.mu.Unlock()
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
	m.mu.RLock()
	defer m.mu.RUnlock()
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
	m.mu.Lock()
	defer m.mu.Unlock()
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
	m.mu.RLock()
	defer m.mu.RUnlock()
	var out []storage.ObjectRef
	for key, data := range m.raw {
		if strings.HasPrefix(key, prefix) {
			out = append(out, storage.ObjectRef{Key: key, Size: int64(len(data)), LastModified: m.modified[key]})
		}
	}
	return out, nil
}
func (m *mockStorage) WalkPrefix(ctx context.Context, prefix string, visit func([]storage.ObjectRef) error) error {
	objects, err := m.ListPrefix(ctx, prefix)
	if err != nil {
		return err
	}
	const pageSize = 100
	for len(objects) > 0 {
		n := min(pageSize, len(objects))
		if err := visit(objects[:n]); err != nil {
			return err
		}
		objects = objects[n:]
	}
	return ctx.Err()
}
func (m *mockStorage) DeleteObjects(ctx context.Context, keys []string) error {
	m.deleteBatchSizes = append(m.deleteBatchSizes, len(keys))
	for _, key := range keys {
		if err := m.DeleteObject(ctx, key); err != nil {
			return err
		}
	}
	return nil
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

// requireMediaEngine skips tests that exercise the Rust libav/archive/torrent
// engines, which are only present when the real data plane is wired in.
func (a *testApp) requireMediaEngine(t *testing.T) {
	t.Helper()
	if _, ok := a.srv.storage.(storage.MediaEngine); !ok {
		t.Skip("Rust media engine is unavailable")
	}
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
	cfg := config.Config{DataDir: dataDir, WorkDir: filepath.Join(dataDir, "work"), BaseURL: "http://example.test", ProxyTransfers: true, PresignExpires: time.Minute, UploadExpires: time.Hour, TrashRetention: 30 * 24 * time.Hour, MediaCacheCapacity: 2 << 30}
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
