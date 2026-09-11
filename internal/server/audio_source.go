package server

import (
	"path/filepath"
	"strings"
)

var audioSourceExts = map[string]bool{".mp3": true, ".wav": true, ".flac": true, ".m4a": true, ".aac": true, ".ogg": true, ".oga": true, ".opus": true, ".wma": true, ".aif": true, ".aiff": true, ".ape": true}

func isAudioSource(f File) bool {
	return f.Kind == "file" && f.Status == "ready" && (strings.HasPrefix(strings.ToLower(responseMime(f)), "audio/") || audioSourceExts[strings.ToLower(filepath.Ext(f.Name))])
}
