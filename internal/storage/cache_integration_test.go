package storage

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/database"
)

func testS3Config(endpoint string) config.Config {
	return config.Config{
		S3Endpoint: endpoint, S3Region: "us-east-1", S3Bucket: "revaro",
		S3AccessKey: "access-key", S3SecretKey: "secret-key", S3PathStyle: true,
		BlockSize: 1 << 20, BlockMinSize: 64 << 10, BlockMaxSize: 4 << 20,
		BlockRAMCacheCapacity: 1 << 20,
	}
}

func TestGetBlockConcurrentMissUsesSingleS3Request(t *testing.T) {
	data := []byte("one immutable block")
	id := hashBytes(data)
	var requests atomic.Int32
	started := make(chan struct{})
	release := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		select {
		case <-started:
		default:
			close(started)
		}
		<-release
		_, _ = w.Write(data)
	}))
	defer server.Close()
	store, err := NewS3(context.Background(), testS3Config(server.URL))
	if err != nil {
		t.Fatal(err)
	}
	const readers = 12
	errs := make(chan error, readers)
	var wg sync.WaitGroup
	for range readers {
		wg.Add(1)
		go func() {
			defer wg.Done()
			got, err := store.GetBlock(context.Background(), id)
			if err == nil && string(got) != string(data) {
				err = &testValueError{got: string(got), want: string(data)}
			}
			errs <- err
		}()
	}
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("S3 request did not start")
	}
	time.Sleep(20 * time.Millisecond)
	close(release)
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	if got := requests.Load(); got != 1 {
		t.Fatalf("concurrent cache miss made %d S3 GETs, want 1", got)
	}
}

func TestGetBlockCancelsSharedS3RequestAfterLastWaiterLeaves(t *testing.T) {
	data := []byte("cancelled immutable block")
	id := hashBytes(data)
	started := make(chan struct{})
	cancelled := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(started)
		<-r.Context().Done()
		close(cancelled)
	}))
	defer server.Close()
	store, err := NewS3(context.Background(), testS3Config(server.URL))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { _, err := store.GetBlock(ctx, id); done <- err }()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("S3 request did not start")
	}
	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("cancelled waiter returned no error")
		}
	case <-time.After(time.Second):
		t.Fatal("cancelled waiter did not return")
	}
	select {
	case <-cancelled:
	case <-time.After(time.Second):
		t.Fatal("last waiter cancellation did not stop S3 request")
	}
}

func TestGetManifestFallsBackOnceThenUsesSQLite(t *testing.T) {
	m := Manifest{Version: 1, Size: 5, Blocks: []Block{{ID: hashBytes([]byte("block")), Size: 5}}}
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		w.Header().Set("Content-Type", manifestMime)
		_, _ = w.Write(m.bytes())
	}))
	defer server.Close()
	db, err := database.Open(filepath.Join(t.TempDir(), "revaro.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	store, err := NewS3WithDB(context.Background(), testS3Config(server.URL), db)
	if err != nil {
		t.Fatal(err)
	}
	for range 2 {
		got, err := store.GetManifest(context.Background(), m.Key())
		if err != nil {
			t.Fatal(err)
		}
		if got.ID() != m.ID() {
			t.Fatalf("manifest id=%s, want %s", got.ID(), m.ID())
		}
	}
	if got := requests.Load(); got != 1 {
		t.Fatalf("two manifest opens made %d S3 GETs, want one recovery fetch", got)
	}
}

func TestLegacyCachesInitializeOnlyWhenManifestIsRead(t *testing.T) {
	m := Manifest{Version: 1, Size: 5, Blocks: []Block{{ID: hashBytes([]byte("block")), Size: 5}}}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", manifestMime)
		_, _ = w.Write(m.bytes())
	}))
	defer server.Close()
	db, err := database.Open(filepath.Join(t.TempDir(), "revaro.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	cacheDir := filepath.Join(t.TempDir(), "legacy-block-cache")
	cfg := testS3Config(server.URL)
	cfg.BlockSSDCacheCapacity = 1 << 20
	cfg.BlockCacheDir = cacheDir
	store, err := NewS3WithDB(context.Background(), cfg, db)
	if err != nil {
		t.Fatal(err)
	}
	if store.diskCache != nil || store.manifests != nil {
		t.Fatal("legacy cache or manifest index initialized during S3 construction")
	}
	if _, err := store.PresignPutObject(context.Background(), "blobs/new", "application/octet-stream", time.Minute); err != nil {
		t.Fatal(err)
	}
	if store.diskCache != nil || store.manifests != nil {
		t.Fatal("new blob path initialized legacy machinery")
	}
	if _, err := os.Stat(cacheDir); !os.IsNotExist(err) {
		t.Fatalf("legacy cache directory exists before manifest read: %v", err)
	}
	if _, err := store.GetManifest(context.Background(), m.Key()); err != nil {
		t.Fatal(err)
	}
	if store.diskCache == nil || store.manifests == nil {
		t.Fatal("manifest read did not initialize legacy machinery")
	}
	if _, err := os.Stat(cacheDir); err != nil {
		t.Fatalf("legacy cache directory was not created: %v", err)
	}
}

type testValueError struct{ got, want string }

func (e *testValueError) Error() string { return "got " + e.got + ", want " + e.want }
