package storage

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"reflect"
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
		getBlock: func(_ context.Context, id string) ([]byte, error) {
			loaded = append(loaded, id)
			return blocks[id], nil
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
