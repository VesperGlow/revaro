package storage

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"reflect"
	"sync"
	"testing"
	"time"
)

func TestFileReaderHTTPRangeLoadsOnlyIntersectingFastCDCBlock(t *testing.T) {
	blocks := map[string][]byte{
		"first":  []byte("abcdefgh"),
		"middle": []byte("ijklmnop"),
		"last":   []byte("qrstuvwx"),
	}
	manifest := Manifest{Size: 24, Blocks: []Block{
		{ID: "first", Size: 8}, {ID: "middle", Size: 8}, {ID: "last", Size: 8},
	}}
	var loaded []string
	reader := &fileReader{
		ctx: context.Background(), m: manifest, starts: []int64{0, 8, 16}, loadedIdx: -1,
		getBlock: func(_ context.Context, block Block) ([]byte, error) {
			loaded = append(loaded, block.ID)
			return blocks[block.ID], nil
		},
	}
	recorder := httptest.NewRecorder()
	request := httptest.NewRequest(http.MethodGet, "/source", nil)
	request.Header.Set("Range", "bytes=18-21")
	http.ServeContent(recorder, request, "video.mkv", time.Unix(0, 0), reader)
	response := recorder.Result()
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusPartialContent || string(body) != "stuv" {
		t.Fatalf("status=%d body=%q", response.StatusCode, body)
	}
	if !reflect.DeepEqual(loaded, []string{"last"}) {
		t.Fatalf("Range request loaded blocks %v, want only the intersecting block", loaded)
	}
}

func TestFileReaderReadAtSpansBlocksWithoutMovingCursorOrPrefetching(t *testing.T) {
	manifest := Manifest{Size: 12, Blocks: []Block{{ID: "a", Size: 4}, {ID: "b", Size: 4}, {ID: "c", Size: 4}}}
	blocks := map[string][]byte{"a": []byte("aaaa"), "b": []byte("bbbb"), "c": []byte("cccc")}
	var mu sync.Mutex
	var loaded []string
	reader := &fileReader{
		ctx: context.Background(), m: manifest, starts: []int64{0, 4, 8}, loadedIdx: -1,
		getBlock: func(_ context.Context, block Block) ([]byte, error) {
			mu.Lock()
			loaded = append(loaded, block.ID)
			mu.Unlock()
			return blocks[block.ID], nil
		},
	}
	data := make([]byte, 6)
	if n, err := reader.ReadAt(data, 3); n != 6 || err != nil || string(data) != "abbbbc" {
		t.Fatalf("ReadAt n=%d data=%q err=%v", n, data, err)
	}
	if reader.off != 0 {
		t.Fatalf("ReadAt moved sequential cursor to %d", reader.off)
	}
	mu.Lock()
	got := append([]string(nil), loaded...)
	mu.Unlock()
	if !reflect.DeepEqual(got, []string{"a", "b", "c"}) {
		t.Fatalf("ReadAt loads=%v, want only intersecting blocks", got)
	}
}

func TestFileReaderCloseCancelsDemandRead(t *testing.T) {
	started := make(chan struct{})
	reader := &fileReader{
		ctx: context.Background(), m: Manifest{Size: 4, Blocks: []Block{{ID: "a", Size: 4}}},
		starts: []int64{0}, loadedIdx: -1,
		getBlock: func(ctx context.Context, _ Block) ([]byte, error) {
			close(started)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	}
	reader.ctx, reader.cancel = context.WithCancel(reader.ctx)
	done := make(chan error, 1)
	go func() { _, err := reader.Read(make([]byte, 1)); done <- err }()
	<-started
	_ = reader.Close()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("read error=%v, want context canceled", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not cancel demand read")
	}
}
