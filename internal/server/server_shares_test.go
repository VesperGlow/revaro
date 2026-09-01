package server

import (
	"net/http"
	"net/url"
	"testing"
)

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
