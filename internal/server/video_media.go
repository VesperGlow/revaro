package server

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

const maxVideoSubtitleBytes = 16 << 20
const maxConvertedSubtitleBytes = 32 << 20
const videoSubtitleCacheTTL = 2 * time.Hour
const maxVideoSubtitleCacheEntries = 32
const maxVideoSubtitleCacheBytes = int64(64 << 20)

var videoSubtitleExts = map[string]bool{
	".vtt": true,
	".srt": true,
	".ass": true,
	".ssa": true,
}

type videoSubtitleResponse struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Label    string `json:"label"`
	Language string `json:"language"`
	URL      string `json:"url"`
}

type embeddedVideoSubtitle struct {
	Index    int
	Codec    string
	Language string
	Title    string
	Default  bool
	Forced   bool
}

type videoSubtitleCacheEntry struct {
	ready       chan struct{}
	data        []byte
	err         error
	completedAt time.Time
}

// cachedVideoSubtitle keeps conversion work independent from the lifetime of
// the browser's <track> request. HLS media attachment can legitimately replace
// the video element and cancel that request; the FFmpeg conversion should still
// finish once and be reused when the selected track is attached again.
func (s *Server) cachedVideoSubtitle(ctx context.Context, key string, convert func(context.Context) ([]byte, error)) ([]byte, error) {
	now := time.Now()
	s.videoSubtitleMu.Lock()
	entry := s.videoSubtitleCache[key]
	if entry == nil {
		for cacheKey, cached := range s.videoSubtitleCache {
			if !cached.completedAt.IsZero() && now.Sub(cached.completedAt) > videoSubtitleCacheTTL {
				s.videoSubtitleBytes -= int64(len(cached.data))
				delete(s.videoSubtitleCache, cacheKey)
			}
		}
		if len(s.videoSubtitleCache) >= maxVideoSubtitleCacheEntries {
			var oldestKey string
			var oldestTime time.Time
			for cacheKey, cached := range s.videoSubtitleCache {
				if cached.completedAt.IsZero() || (!oldestTime.IsZero() && !cached.completedAt.Before(oldestTime)) {
					continue
				}
				oldestKey, oldestTime = cacheKey, cached.completedAt
			}
			if oldestKey != "" {
				s.videoSubtitleBytes -= int64(len(s.videoSubtitleCache[oldestKey].data))
				delete(s.videoSubtitleCache, oldestKey)
			}
		}
		entry = &videoSubtitleCacheEntry{ready: make(chan struct{})}
		s.videoSubtitleCache[key] = entry
		go func() {
			workCtx, cancel := context.WithTimeout(s.audioHLSCtx, 10*time.Minute)
			defer cancel()
			data, err := convert(workCtx)
			s.videoSubtitleMu.Lock()
			entry.data, entry.err, entry.completedAt = data, err, time.Now()
			if err != nil {
				s.log.Warn("video subtitle background conversion failed", "subtitle", key, "error", err)
				// Failed conversions may be caused by a transient S3/FFmpeg issue.
				// Wake current waiters, but allow the next request to retry.
				delete(s.videoSubtitleCache, key)
			} else {
				s.videoSubtitleBytes += int64(len(data))
				for s.videoSubtitleBytes > maxVideoSubtitleCacheBytes {
					var oldestKey string
					var oldestTime time.Time
					for cacheKey, cached := range s.videoSubtitleCache {
						if cacheKey == key || cached.completedAt.IsZero() || (!oldestTime.IsZero() && !cached.completedAt.Before(oldestTime)) {
							continue
						}
						oldestKey, oldestTime = cacheKey, cached.completedAt
					}
					if oldestKey == "" {
						break
					}
					s.videoSubtitleBytes -= int64(len(s.videoSubtitleCache[oldestKey].data))
					delete(s.videoSubtitleCache, oldestKey)
				}
			}
			close(entry.ready)
			s.videoSubtitleMu.Unlock()
		}()
	}
	s.videoSubtitleMu.Unlock()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-entry.ready:
		return entry.data, entry.err
	}
}

func mediaCommandError(operation string, runErr, ctxErr error, stderr string) error {
	detail := strings.TrimSpace(stderr)
	base := runErr
	if ctxErr != nil {
		base = ctxErr
	}
	if base == nil {
		base = errors.New("command failed")
	}
	if detail == "" {
		return fmt.Errorf("%s: %w", operation, base)
	}
	return fmt.Errorf("%s: %w: %s", operation, base, detail)
}

func isVideoSource(f File) bool {
	return strings.HasPrefix(strings.ToLower(f.MimeType), "video/") || videoExts[strings.ToLower(filepath.Ext(f.Name))]
}

func (s *Server) videoMediaInfo(w http.ResponseWriter, r *http.Request) {
	video, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || video.Kind != "file" || video.Status != "ready" || !isVideoSource(video) {
		problem(w, http.StatusNotFound, "ready video file not found")
		return
	}
	files, err := s.findVideoSubtitles(r.Context(), video)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not find video subtitles")
		return
	}
	tracks := make([]videoSubtitleResponse, 0, len(files)+2)
	embedded, probeErr := s.findEmbeddedVideoSubtitles(r.Context(), video)
	if probeErr != nil {
		s.log.Warn("embedded video subtitle probe failed", "video", video.ID, "error", probeErr)
	}
	for _, subtitle := range embedded {
		language, languageLabel := embeddedSubtitleLanguage(subtitle.Language)
		label := strings.TrimSpace(subtitle.Title)
		if label == "" {
			label = languageLabel
		}
		if label == "" {
			label = fmt.Sprintf("内嵌字幕 %d", subtitle.Index+1)
		}
		if subtitle.Forced {
			label += " · 强制"
		} else if subtitle.Default {
			label += " · 默认"
		}
		id := "embedded-" + strconv.Itoa(subtitle.Index)
		tracks = append(tracks, videoSubtitleResponse{
			ID: id, Name: label, Label: label, Language: language,
			URL: "/api/files/" + video.ID + "/video/subtitles/" + id,
		})
	}
	for _, subtitle := range files {
		language, languageLabel := videoSubtitleLanguage(video.Name, subtitle.Name)
		label := subtitle.Name
		if languageLabel != "" {
			label = languageLabel + " · " + subtitle.Name
		}
		tracks = append(tracks, videoSubtitleResponse{
			ID: subtitle.ID, Name: subtitle.Name, Label: label, Language: language,
			URL: "/api/files/" + video.ID + "/video/subtitles/" + subtitle.ID,
		})
	}
	writeJSON(w, http.StatusOK, map[string]any{"subtitles": tracks})
}

func embeddedSubtitleLanguage(value string) (string, string) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "zh", "chi", "zho", "chs", "zh-cn", "zh-hans":
		return "zh-CN", "简体中文"
	case "cht", "zh-tw", "zh-hant":
		return "zh-TW", "繁體中文"
	case "en", "eng":
		return "en", "English"
	case "ja", "jpn":
		return "ja", "日本語"
	case "ko", "kor":
		return "ko", "한국어"
	default:
		if value == "" {
			return "und", ""
		}
		return value, strings.ToUpper(value)
	}
}

func supportedEmbeddedSubtitleCodec(codec string) bool {
	switch strings.ToLower(codec) {
	case "ass", "ssa", "subrip", "srt", "webvtt", "text", "mov_text":
		return true
	default:
		return false
	}
}

func (s *Server) findEmbeddedVideoSubtitles(ctx context.Context, video File) ([]embeddedVideoSubtitle, error) {
	ffprobe, err := ffprobeFor(s.cfg.FFmpegPath)
	if err != nil {
		return nil, err
	}
	sourceURL, cleanup, err := s.startMediaHLSSource(ctx, video)
	if err != nil {
		return nil, err
	}
	defer cleanup()
	cmd := exec.CommandContext(ctx, ffprobe, "-v", "error", "-select_streams", "s", "-show_entries", "stream=index,codec_name:stream_tags=language,title:stream_disposition=default,forced", "-of", "json", sourceURL)
	output := &limitedBuffer{limit: 2 << 20}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stdout, cmd.Stderr = output, stderr
	if err := cmd.Run(); err != nil {
		return nil, mediaCommandError("ffprobe subtitles", err, ctx.Err(), stderr.String())
	}
	var result struct {
		Streams []struct {
			Index int    `json:"index"`
			Codec string `json:"codec_name"`
			Tags  struct {
				Language string `json:"language"`
				Title    string `json:"title"`
			} `json:"tags"`
			Disposition struct {
				Default int `json:"default"`
				Forced  int `json:"forced"`
			} `json:"disposition"`
		} `json:"streams"`
	}
	if err := json.Unmarshal(output.buf.Bytes(), &result); err != nil {
		return nil, err
	}
	tracks := make([]embeddedVideoSubtitle, 0, len(result.Streams))
	for _, stream := range result.Streams {
		if !supportedEmbeddedSubtitleCodec(stream.Codec) {
			continue
		}
		tracks = append(tracks, embeddedVideoSubtitle{Index: stream.Index, Codec: stream.Codec, Language: stream.Tags.Language, Title: stream.Tags.Title, Default: stream.Disposition.Default != 0, Forced: stream.Disposition.Forced != 0})
	}
	return tracks, nil
}

func (s *Server) findVideoSubtitles(ctx context.Context, video File) ([]File, error) {
	if video.ParentID == nil {
		return []File{}, nil
	}
	// Search the video's directory plus two levels of conventional Subs/
	// Subtitles folders. Name matching below prevents unrelated episode tracks
	// from leaking into the player's language menu.
	rows, err := s.db.QueryContext(ctx, `
		WITH RECURSIVE subtitle_dirs(id,depth) AS (
			SELECT ?,0
			UNION ALL
			SELECT child.id,subtitle_dirs.depth+1
			FROM files AS child JOIN subtitle_dirs ON child.parent_id=subtitle_dirs.id
			WHERE child.kind='directory' AND child.status='ready' AND child.deleted_at IS NULL AND subtitle_dirs.depth<2
		)
		SELECT files.id,files.parent_id,files.name,files.kind,COALESCE(files.object_key,''),files.size,files.mime_type,files.etag,files.status,files.created_at,files.updated_at,files.deleted_at,files.restore_parent_id
		FROM files JOIN subtitle_dirs ON files.parent_id=subtitle_dirs.id
		WHERE files.kind='file' AND files.status='ready' AND files.deleted_at IS NULL`, *video.ParentID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	type match struct {
		file     File
		priority int
	}
	matches := make([]match, 0)
	for rows.Next() {
		candidate, scanErr := scanFile(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		if priority, ok := videoSubtitleMatchPriority(video.Name, candidate.Name); ok {
			matches = append(matches, match{file: candidate, priority: priority})
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	sort.Slice(matches, func(i, j int) bool {
		if matches[i].priority != matches[j].priority {
			return matches[i].priority < matches[j].priority
		}
		return strings.ToLower(matches[i].file.Name) < strings.ToLower(matches[j].file.Name)
	})
	out := make([]File, 0, len(matches))
	for _, item := range matches {
		out = append(out, item.file)
	}
	return out, nil
}

func videoSubtitleMatchPriority(videoName, subtitleName string) (int, bool) {
	ext := strings.ToLower(filepath.Ext(subtitleName))
	if !videoSubtitleExts[ext] {
		return 0, false
	}
	videoStem := strings.TrimSuffix(videoName, filepath.Ext(videoName))
	subtitleStem := strings.TrimSuffix(subtitleName, filepath.Ext(subtitleName))
	switch {
	case strings.EqualFold(subtitleStem, videoStem):
		return 0, true
	case strings.EqualFold(subtitleStem, videoName):
		return 1, true
	case len(subtitleStem) > len(videoStem) && strings.EqualFold(subtitleStem[:len(videoStem)], videoStem) && strings.Contains(" ._-[(", subtitleStem[len(videoStem):len(videoStem)+1]):
		return 2, true
	default:
		return 0, false
	}
}

func videoSubtitleLanguage(videoName, subtitleName string) (string, string) {
	videoStem := strings.ToLower(strings.TrimSuffix(videoName, filepath.Ext(videoName)))
	subtitleStem := strings.ToLower(strings.TrimSuffix(subtitleName, filepath.Ext(subtitleName)))
	suffix := strings.TrimPrefix(subtitleStem, videoStem)
	normalized := strings.NewReplacer("_", "-", ".", "-", "[", "-", "]", "-", "(", "-", ")", "-", " ", "-").Replace(suffix)
	switch {
	case strings.Contains(normalized, "zh-tw"), strings.Contains(normalized, "zh-hant"):
		return "zh-TW", "繁體中文"
	case strings.Contains(normalized, "zh-cn"), strings.Contains(normalized, "zh-hans"):
		return "zh-CN", "简体中文"
	}
	tokens := strings.FieldsFunc(suffix, func(r rune) bool {
		return r == '.' || r == '_' || r == '-' || r == '[' || r == ']' || r == '(' || r == ')' || r == ' '
	})
	for _, token := range tokens {
		switch token {
		case "zh", "chi", "zho", "chs", "sc", "zhcn":
			return "zh-CN", "简体中文"
		case "cht", "tc", "zhtw":
			return "zh-TW", "繁體中文"
		case "en", "eng", "english":
			return "en", "English"
		case "ja", "jpn", "jp", "japanese":
			return "ja", "日本語"
		case "ko", "kor", "kr", "korean":
			return "ko", "한국어"
		}
	}
	return "und", ""
}

func (s *Server) videoSubtitle(w http.ResponseWriter, r *http.Request) {
	video, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || video.Kind != "file" || video.Status != "ready" || !isVideoSource(video) {
		problem(w, http.StatusNotFound, "ready video file not found")
		return
	}
	subtitleID := chi.URLParam(r, "subtitle")
	if strings.HasPrefix(subtitleID, "embedded-") {
		index, parseErr := strconv.Atoi(strings.TrimPrefix(subtitleID, "embedded-"))
		if parseErr != nil || index < 0 {
			problem(w, http.StatusNotFound, "embedded subtitle not found")
			return
		}
		cacheKey := fmt.Sprintf("embedded-v1:%s:%s:%s:%d", video.ID, video.ETag, video.UpdatedAt, index)
		vtt, convertErr := s.cachedVideoSubtitle(r.Context(), cacheKey, func(workCtx context.Context) ([]byte, error) {
			allowed, probeErr := s.findEmbeddedVideoSubtitles(workCtx, video)
			if probeErr != nil {
				return nil, probeErr
			}
			for _, track := range allowed {
				if track.Index == index {
					return s.embeddedSubtitleAsWebVTT(workCtx, video, index)
				}
			}
			return nil, errors.New("embedded subtitle stream is unavailable")
		})
		if convertErr != nil {
			if errors.Is(convertErr, context.Canceled) || errors.Is(convertErr, context.DeadlineExceeded) && r.Context().Err() != nil {
				return
			}
			s.log.Warn("embedded video subtitle conversion failed", "video", video.ID, "stream", index, "error", convertErr)
			problem(w, http.StatusUnprocessableEntity, "embedded subtitle could not be converted to WebVTT")
			return
		}
		writeVideoSubtitle(w, vtt)
		return
	}
	allowed, err := s.findVideoSubtitles(r.Context(), video)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not find video subtitles")
		return
	}
	var subtitle *File
	for index := range allowed {
		if allowed[index].ID == subtitleID {
			subtitle = &allowed[index]
			break
		}
	}
	if subtitle == nil {
		problem(w, http.StatusNotFound, "matching subtitle not found")
		return
	}
	cacheKey := fmt.Sprintf("external-v1:%s:%s:%s", subtitle.ID, subtitle.ETag, subtitle.UpdatedAt)
	vtt, err := s.cachedVideoSubtitle(r.Context(), cacheKey, func(workCtx context.Context) ([]byte, error) {
		raw, readErr := s.readFileWithLimit(workCtx, *subtitle, maxVideoSubtitleBytes)
		if readErr != nil {
			return nil, readErr
		}
		return s.subtitleAsWebVTT(workCtx, subtitle.Name, raw)
	})
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) && r.Context().Err() != nil {
		return
	}
	if errors.Is(err, storage.ErrObjectTooLarge) {
		problem(w, http.StatusRequestEntityTooLarge, "subtitle is too large")
		return
	}
	if err != nil {
		s.log.Warn("video subtitle conversion failed", "video", video.ID, "subtitle", subtitle.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "subtitle could not be converted to WebVTT")
		return
	}
	writeVideoSubtitle(w, vtt)
}

func writeVideoSubtitle(w http.ResponseWriter, vtt []byte) {
	w.Header().Set("Content-Type", "text/vtt; charset=utf-8")
	w.Header().Set("Cache-Control", "private, max-age=3600")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(vtt)
}

func (s *Server) embeddedSubtitleAsWebVTT(ctx context.Context, video File, streamIndex int) ([]byte, error) {
	sourceURL, cleanup, err := s.startMediaHLSSource(ctx, video)
	if err != nil {
		return nil, err
	}
	defer cleanup()
	cmd := exec.CommandContext(ctx, s.cfg.FFmpegPath, "-hide_banner", "-loglevel", "error", "-i", sourceURL, "-map", fmt.Sprintf("0:%d", streamIndex), "-f", "webvtt", "pipe:1")
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	converted, readErr := io.ReadAll(io.LimitReader(stdout, maxConvertedSubtitleBytes+1))
	if readErr != nil || len(converted) > maxConvertedSubtitleBytes {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		if readErr != nil {
			return nil, readErr
		}
		return nil, errors.New("converted subtitle is too large")
	}
	if err := cmd.Wait(); err != nil {
		return nil, mediaCommandError("ffmpeg embedded subtitle conversion", err, ctx.Err(), stderr.String())
	}
	if !bytes.HasPrefix(bytes.TrimSpace(converted), []byte("WEBVTT")) {
		return nil, errors.New("ffmpeg produced invalid WebVTT")
	}
	return converted, nil
}

func (s *Server) readFileWithLimit(ctx context.Context, f File, limit int64) ([]byte, error) {
	if storage.IsManifestKey(f.objectKey) {
		return s.storage.ReadFile(ctx, f.objectKey, limit)
	}
	return s.storage.GetObject(ctx, f.objectKey, limit)
}

func (s *Server) subtitleAsWebVTT(ctx context.Context, name string, raw []byte) ([]byte, error) {
	raw = bytes.TrimPrefix(raw, []byte{0xef, 0xbb, 0xbf})
	if strings.EqualFold(filepath.Ext(name), ".vtt") {
		raw = bytes.TrimLeft(raw, " \t\r\n")
		if !bytes.HasPrefix(raw, []byte("WEBVTT")) {
			return nil, errors.New("invalid WebVTT header")
		}
		return raw, nil
	}
	if _, err := exec.LookPath(s.cfg.FFmpegPath); err != nil {
		return nil, err
	}
	inputFormat := strings.TrimPrefix(strings.ToLower(filepath.Ext(name)), ".")
	if inputFormat == "ssa" {
		inputFormat = "ass"
	}
	cmd := exec.CommandContext(ctx, s.cfg.FFmpegPath, "-hide_banner", "-loglevel", "error", "-f", inputFormat, "-i", "pipe:0", "-map", "0:s:0", "-f", "webvtt", "pipe:1")
	cmd.Stdin = bytes.NewReader(raw)
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	converted, readErr := io.ReadAll(io.LimitReader(stdout, maxConvertedSubtitleBytes+1))
	if readErr != nil || len(converted) > maxConvertedSubtitleBytes {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		if readErr != nil {
			return nil, readErr
		}
		return nil, errors.New("converted subtitle is too large")
	}
	if err := cmd.Wait(); err != nil {
		return nil, mediaCommandError("ffmpeg subtitle conversion", err, ctx.Err(), stderr.String())
	}
	if !bytes.HasPrefix(bytes.TrimSpace(converted), []byte("WEBVTT")) {
		return nil, errors.New("ffmpeg produced invalid WebVTT")
	}
	return converted, nil
}
