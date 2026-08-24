package server

import (
	"encoding/binary"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func fmp4TestBox(kind string, payloadSize int) []byte {
	box := make([]byte, 8+payloadSize)
	binary.BigEndian.PutUint32(box[:4], uint32(len(box)))
	copy(box[4:8], kind)
	return box
}

func TestFMP4FileStateRequiresCompleteInitAndMediaFragment(t *testing.T) {
	path := filepath.Join(t.TempDir(), "stream.mp4")
	initial := append(fmp4TestBox("ftyp", 12), fmp4TestBox("moov", 20)...)
	if err := os.WriteFile(path, initial, 0o600); err != nil {
		t.Fatal(err)
	}
	if initReady, fragmentReady := fmp4FileState(path); !initReady || fragmentReady {
		t.Fatalf("state=(%v,%v), want complete init only", initReady, fragmentReady)
	}
	fragment := append(fmp4TestBox("moof", 24), fmp4TestBox("mdat", 64)...)
	if err := os.WriteFile(path, append(initial, fragment[:len(fragment)-3]...), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, fragmentReady := fmp4FileState(path); fragmentReady {
		t.Fatal("truncated mdat was accepted as a complete fragment")
	}
	if err := os.WriteFile(path, append(initial, fragment...), 0o600); err != nil {
		t.Fatal(err)
	}
	if initReady, fragmentReady := fmp4FileState(path); !initReady || !fragmentReady {
		t.Fatalf("state=(%v,%v), want ready fMP4", initReady, fragmentReady)
	}
}

func TestFMP4CodecStringsPreserveHEVCAndEAC3(t *testing.T) {
	video, err := fmp4VideoCodecString("hevc", "Main 10", 120)
	if err != nil || video != "hvc1.2.4.L120.B0" {
		t.Fatalf("HEVC codec=%q error=%v", video, err)
	}
	audio, err := fmp4AudioCodecString("eac3")
	if err != nil || audio != "ec-3" {
		t.Fatalf("EAC3 codec=%q error=%v", audio, err)
	}
	if _, err := fmp4VideoCodecString("vp9", "", 0); err == nil || !strings.Contains(err.Error(), "vp9") {
		t.Fatalf("unsupported codec error=%v", err)
	}
}
