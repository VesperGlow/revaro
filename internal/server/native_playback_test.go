package server

import (
	"net/http"
	"testing"
)

func TestNativePlaybackServesOriginalBytesWithoutCompatibilityEndpoints(t *testing.T) {
	app := newTestApp(t)
	for _, name := range []string{"unsupported.wav", "unsupported.mkv"} {
		t.Run(name, func(t *testing.T) {
			// Deliberately invalid media: only the browser decides whether it
			// can decode the source; the server must not rewrite the payload.
			file := app.readyFile(t, name, []byte("original media bytes"))
			base := "/api/files/" + file.ID
			response := app.requestH("GET", base+"/preview", nil, true, map[string]string{"Range": "bytes=0-7"})
			if response.Code != http.StatusPartialContent || response.Body.String() != "original" {
				t.Fatalf("original range=%d %q", response.Code, response.Body.String())
			}
			for _, route := range []string{"/audio/stream", "/audio/hls", "/video/hls", "/video/fmp4"} {
				if response := app.request("GET", base+route, nil, true); response.Code != http.StatusNotFound {
					t.Fatalf("retired endpoint %s=%d", route, response.Code)
				}
			}
		})
	}
}
