package server

import (
	"context"
	"errors"
	"fmt"
	"net/http"
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
	Default  bool   `json:"default"`
	Forced   bool   `json:"forced"`
}

type embeddedVideoSubtitle struct {
	Index    int
	Codec    string
	Language string
	Title    string
	Default  bool
	Forced   bool
}

func (s *Server) clearVideoSubtitleCache(fileID string) {
	s.cache.Invalidate("embedded-v2:" + fileID + ":")
	s.cache.Invalidate("external-v2:" + fileID + ":")
}

// cachedVideoSubtitle keeps conversion work independent from the lifetime of
// the browser's <track> request. Switching tracks may cancel that request;
// subtitle extraction should still finish once and be reused on the next request.
// 字幕转换是真正临时的产物：media/subtitle class 带 TTL，由统一缓存管理器
// 提供内存 L1 + 磁盘 L2 + singleflight。
func (s *Server) cachedVideoSubtitle(ctx context.Context, key string, convert func(context.Context) ([]byte, error)) ([]byte, error) {
	data, err := s.cache.Load(ctx, cacheClassMediaSubtitle, key, videoSubtitleCacheTTL, convert)
	if err != nil {
		s.log.Warn("video subtitle background conversion failed", "subtitle", key, "error", err)
	}
	return data, err
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
	s.scheduleMediaAnalysis(video)
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
			URL:     "/api/files/" + video.ID + "/video/subtitles/" + id,
			Default: subtitle.Default, Forced: subtitle.Forced,
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
	s.log.Info("video subtitles discovered", "file", video.ID, "embedded", len(embedded), "external", len(files), "total", len(tracks))
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
	metadata, err := s.ensureMediaMetadata(ctx, video)
	if err != nil {
		return nil, err
	}
	tracks := make([]embeddedVideoSubtitle, 0, len(metadata.Subtitles))
	for _, stream := range metadata.Subtitles {
		if !supportedEmbeddedSubtitleCodec(stream.Codec) {
			continue
		}
		tracks = append(tracks, embeddedVideoSubtitle{Index: stream.Index, Codec: stream.Codec, Language: stream.Language, Title: stream.Title, Default: stream.Default, Forced: stream.Forced})
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
		SELECT files.id,files.parent_id,files.name,files.kind,COALESCE(files.object_key,''),files.size,files.mime_type,files.etag,files.content_hash,files.hash_algorithm,files.status,files.created_at,files.updated_at,files.deleted_at,files.restore_parent_id
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
		cacheKey := fmt.Sprintf("embedded-v2:%s:%s:%s:%d", video.ID, video.ETag, video.UpdatedAt, index)
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
		s.log.Info("video subtitle served", "file", video.ID, "subtitle", subtitleID, "codec", "embedded", "bytes", len(vtt))
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
	cacheKey := fmt.Sprintf("external-v2:%s:%s:%s", subtitle.ID, subtitle.ETag, subtitle.UpdatedAt)
	vtt, err := s.cachedVideoSubtitle(r.Context(), cacheKey, func(workCtx context.Context) ([]byte, error) {
		return s.subtitleAsWebVTT(workCtx, *subtitle)
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
	s.log.Info("video subtitle served", "file", video.ID, "subtitle", subtitleID, "codec", filepath.Ext(subtitle.Name), "bytes", len(vtt))
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
	taskID := s.startRuntimeTask(ctx, "", "subtitle", "subtitle", video.ID)
	release, err := s.tasks.Heavy(ctx)
	if err != nil {
		s.finishRuntimeTask(taskID, "subtitle", err)
		return nil, err
	}
	defer release()
	out, err := s.media.Subtitle(ctx, video.objectKey, "", &streamIndex)
	s.finishRuntimeTask(taskID, "subtitle", err)
	return out, err
}

func (s *Server) subtitleAsWebVTT(ctx context.Context, subtitle File) ([]byte, error) {
	format := strings.TrimPrefix(strings.ToLower(filepath.Ext(subtitle.Name)), ".")
	taskID := s.startRuntimeTask(ctx, "", "subtitle", "subtitle", subtitle.ID)
	release, err := s.tasks.Heavy(ctx)
	if err != nil {
		s.finishRuntimeTask(taskID, "subtitle", err)
		return nil, err
	}
	defer release()
	out, err := s.media.Subtitle(ctx, subtitle.objectKey, format, nil)
	s.finishRuntimeTask(taskID, "subtitle", err)
	return out, err
}
