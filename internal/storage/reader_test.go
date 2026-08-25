package storage

import (
	"context"
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

func TestFileReaderPrefetchesNextThreeBlocksAndReusesResult(t *testing.T) {
	blocks := map[string][]byte{
		"first": []byte("aaaa"), "second": []byte("bbbb"), "third": []byte("cccc"), "fourth": []byte("dddd"), "fifth": []byte("eeee"),
	}
	manifest := Manifest{Size: 20, Blocks: []Block{
		{ID: "first", Size: 4}, {ID: "second", Size: 4}, {ID: "third", Size: 4}, {ID: "fourth", Size: 4}, {ID: "fifth", Size: 4},
	}}
	var mu sync.Mutex
	loads := make(map[string]int)
	reader := &fileReader{
		ctx: context.Background(), m: manifest, starts: []int64{0, 4, 8, 12, 16}, loadedIdx: -1,
		getBlock: func(_ context.Context, block Block) ([]byte, error) {
			mu.Lock()
			loads[block.ID]++
			mu.Unlock()
			return blocks[block.ID], nil
		},
	}
	if _, err := reader.Read(make([]byte, 1)); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(time.Second)
	for {
		mu.Lock()
		prefetched := loads["second"] == 1 && loads["third"] == 1 && loads["fourth"] == 1
		fifthLoads := loads["fifth"]
		mu.Unlock()
		if prefetched {
			if fifthLoads != 0 {
				t.Fatalf("prefetch crossed its three-block bound: fifth loads=%d", fifthLoads)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("next three blocks were not prefetched: %+v", loads)
		}
		time.Sleep(time.Millisecond)
	}
	if _, err := reader.Seek(4, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	buffer := make([]byte, 1)
	if _, err := reader.Read(buffer); err != nil || string(buffer) != "b" {
		t.Fatalf("read prefetched block=%q error=%v", buffer, err)
	}
	mu.Lock()
	secondLoads := loads["second"]
	mu.Unlock()
	if secondLoads != 1 {
		t.Fatalf("prefetched block fetched %d times, want once", secondLoads)
	}
}

func TestFileReaderDynamicReadAheadUsesByteWindow(t *testing.T) {
	blocks := map[string][]byte{}
	manifest := Manifest{Size: 24}
	starts := make([]int64, 6)
	for i, id := range []string{"a", "b", "c", "d", "e", "f"} {
		blocks[id] = []byte(id + id + id + id)
		manifest.Blocks = append(manifest.Blocks, Block{ID: id, Size: 4})
		starts[i] = int64(i * 4)
	}
	var mu sync.Mutex
	loads := make(map[string]int)
	reader := &fileReader{
		ctx: context.Background(), m: manifest, starts: starts, loadedIdx: -1, readAheadBytes: 16,
		getBlock: func(_ context.Context, block Block) ([]byte, error) {
			mu.Lock()
			loads[block.ID]++
			mu.Unlock()
			return blocks[block.ID], nil
		},
	}
	if _, err := reader.Read(make([]byte, 1)); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(time.Second)
	for {
		mu.Lock()
		ready := loads["b"] == 1 && loads["c"] == 1 && loads["d"] == 1 && loads["e"] == 1
		last := loads["f"]
		mu.Unlock()
		if ready {
			if last != 0 {
				t.Fatalf("read-ahead exceeded byte window: f loads=%d", last)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("dynamic read-ahead did not fill 16-byte window: %+v", loads)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestFileReaderSeekCancelsOldReadAhead(t *testing.T) {
	manifest := Manifest{Size: 20, Blocks: []Block{
		{ID: "a", Size: 4}, {ID: "b", Size: 4}, {ID: "c", Size: 4}, {ID: "d", Size: 4}, {ID: "e", Size: 4},
	}}
	started := make(chan struct{}, 4)
	cancelled := make(chan struct{}, 4)
	reader := &fileReader{
		ctx: context.Background(), m: manifest, starts: []int64{0, 4, 8, 12, 16}, loadedIdx: -1, readAheadBytes: 12,
		getBlock: func(ctx context.Context, block Block) ([]byte, error) {
			if block.ID == "a" || block.ID == "e" {
				return []byte(block.ID + block.ID + block.ID + block.ID), nil
			}
			started <- struct{}{}
			<-ctx.Done()
			cancelled <- struct{}{}
			return nil, ctx.Err()
		},
	}
	if _, err := reader.Read(make([]byte, 1)); err != nil {
		t.Fatal(err)
	}
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("old read-ahead did not start")
	}
	if _, err := reader.Seek(16, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	buffer := make([]byte, 1)
	if _, err := reader.Read(buffer); err != nil || string(buffer) != "e" {
		t.Fatalf("read after seek=%q error=%v", buffer, err)
	}
	select {
	case <-cancelled:
	case <-time.After(time.Second):
		t.Fatal("seek did not cancel old-direction read-ahead")
	}
}
