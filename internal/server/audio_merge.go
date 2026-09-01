package server

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"html"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

const maxAudioMergeInputs = 256
const audioMergeTimeout = 24 * time.Hour
const maxAudioCoverBytes = 2 << 20
const maxAudioCoverSourceBytes = 16 << 20
const audioCoverMaxDim = 1200
const maxAudioSubtitleBytes = 8 << 20
const maxMergedAudioSubtitleBytes = 32 << 20
const maxAudioSubtitleCues = 100000

var audioSourceExts = map[string]bool{
	".mp3": true, ".wav": true, ".flac": true, ".m4a": true, ".aac": true,
	".ogg": true, ".oga": true, ".opus": true, ".wma": true, ".aif": true,
	".aiff": true, ".ape": true,
}

var audioCoverSourceExts = map[string]bool{
	".jpg": true, ".jpeg": true, ".png": true, ".gif": true, ".webp": true, ".bmp": true,
}

type audioOutputProfile struct {
	Format      string
	Extension   string
	MimeType    string
	Codec       string
	Container   string
	Lossless    bool
	Chapters    bool
	Subtitles   bool
	EncoderArgs []string
}

func audioOutput(format string) (audioOutputProfile, bool) {
	switch strings.ToLower(format) {
	case "", "flac":
		return audioOutputProfile{Format: "flac", Extension: ".flac", MimeType: "audio/flac", Codec: "flac", Container: "flac", Lossless: true, EncoderArgs: []string{"-compression_level", "8"}}, true
	case "alac":
		return audioOutputProfile{Format: "alac", Extension: ".m4a", MimeType: "audio/mp4", Codec: "alac", Container: "ipod", Lossless: true, Chapters: true, Subtitles: true, EncoderArgs: []string{"-movflags", "+faststart"}}, true
	case "aac":
		return audioOutputProfile{Format: "aac", Extension: ".m4a", MimeType: "audio/mp4", Codec: "aac", Container: "ipod", Chapters: true, Subtitles: true, EncoderArgs: []string{"-b:a", "192k", "-movflags", "+faststart"}}, true
	default:
		return audioOutputProfile{}, false
	}
}

type audioMergeUserError struct{ message string }

func (e audioMergeUserError) Error() string { return e.message }

type audioMergeJob struct {
	mu           sync.Mutex
	ID           string
	Status       string
	Progress     int
	Message      string
	Error        string
	OutputName   string
	OutputFormat string
	OutputFileID string
	ParentID     string
	InputCount   int
	Source       string
	CreatedAt    string
	UpdatedAt    string
	cancel       context.CancelFunc
	mergeCtx     context.Context
	changed      func()
	// Local-directory merge state. Files are chunk-uploaded into stagingDir
	// under APP_WORK_DIR and never touch object storage.
	localUpload    bool
	stagingDir     string
	uploadSlotHeld bool
	uploadedBytes  int64
	files          []localMergeFile
	audioOrder     []int // staging file index per audio, in final play order
	subtitleFor    []int // subtitle file index per audio order entry, or -1
	coverIndex     int   // cover file index, or -1
}

type audioMergeSnapshot struct {
	ID           string `json:"id"`
	Status       string `json:"status"`
	Progress     int    `json:"progress"`
	Message      string `json:"message"`
	Error        string `json:"error,omitempty"`
	OutputName   string `json:"output_name"`
	OutputFormat string `json:"output_format"`
	OutputFileID string `json:"output_file_id,omitempty"`
	ParentID     string `json:"parent_id"`
	InputCount   int    `json:"input_count"`
	Source       string `json:"source,omitempty"`
	CreatedAt    string `json:"created_at"`
	UpdatedAt    string `json:"updated_at"`
}

func (j *audioMergeJob) snapshot() audioMergeSnapshot {
	j.mu.Lock()
	defer j.mu.Unlock()
	return audioMergeSnapshot{
		ID: j.ID, Status: j.Status, Progress: j.Progress, Message: j.Message,
		Error: j.Error, OutputName: j.OutputName, OutputFormat: j.OutputFormat, OutputFileID: j.OutputFileID,
		ParentID:   j.ParentID,
		InputCount: j.InputCount, Source: j.Source, CreatedAt: j.CreatedAt, UpdatedAt: j.UpdatedAt,
	}
}

// cleanupStaging removes the local merge staging directory once. Callers may
// invoke it any number of times; only the first call touches the filesystem.
func (j *audioMergeJob) cleanupStaging(log *slog.Logger) {
	j.mu.Lock()
	dir := j.stagingDir
	j.stagingDir = ""
	j.mu.Unlock()
	if dir == "" {
		return
	}
	if err := os.RemoveAll(dir); err != nil {
		log.Warn("local merge staging cleanup failed", "job", j.ID, "path", dir, "error", err)
	}
}

// releaseUploadSlot returns the bounded local-merge upload slot exactly once.
func (j *audioMergeJob) releaseUploadSlot(s *Server) {
	j.mu.Lock()
	held := j.uploadSlotHeld
	j.uploadSlotHeld = false
	j.mu.Unlock()
	if held {
		<-s.localMergeJobSlots
	}
}

func (j *audioMergeJob) update(status string, progress int, message string) {
	j.mu.Lock()
	if status != "" {
		j.Status = status
	}
	if progress > j.Progress {
		j.Progress = min(progress, 100)
	}
	if message != "" {
		j.Message = message
	}
	j.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	changed := j.changed
	j.mu.Unlock()
	if changed != nil {
		changed()
	}
}

func (j *audioMergeJob) finish(status, message, jobError string) {
	j.mu.Lock()
	j.Status, j.Message, j.Error = status, message, jobError
	if status == "done" {
		j.Progress = 100
	}
	j.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	changed := j.changed
	j.mu.Unlock()
	if changed != nil {
		changed()
	}
}

func isAudioSource(f File) bool {
	return f.Kind == "file" && f.Status == "ready" &&
		(strings.HasPrefix(strings.ToLower(responseMime(f)), "audio/") || audioSourceExts[strings.ToLower(filepath.Ext(f.Name))])
}

func decodeAudioCover(encoded string) ([]byte, error) {
	if encoded == "" {
		return nil, nil
	}
	if comma := strings.IndexByte(encoded, ','); comma >= 0 {
		encoded = encoded[comma+1:]
	}
	if len(encoded) > base64.StdEncoding.EncodedLen(maxAudioCoverBytes*4) {
		return nil, errors.New("cover image is too large")
	}
	raw, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil || len(raw) == 0 || len(raw) > maxAudioCoverBytes*4 {
		return nil, errors.New("cover image is invalid")
	}
	return normalizeAudioCover(raw)
}

func normalizeAudioCover(raw []byte) ([]byte, error) {
	normalized, err := resizeToJPEG(raw, audioCoverMaxDim)
	if err != nil || len(normalized) == 0 || len(normalized) > maxAudioCoverBytes {
		return nil, errors.New("cover image is invalid or too large")
	}
	return normalized, nil
}

func (s *Server) audioCoverFromFile(ctx context.Context, parentID, fileID string) ([]byte, error) {
	if fileID == "" {
		return nil, nil
	}
	f, err := s.file(ctx, fileID)
	if err != nil || f.Kind != "file" || f.Status != "ready" || f.DeletedAt != "" || f.ParentID == nil || *f.ParentID != parentID || !audioCoverSourceExts[strings.ToLower(filepath.Ext(f.Name))] {
		return nil, errors.New("cover must be a supported image from the output directory")
	}
	if f.Size <= 0 || f.Size > maxAudioCoverSourceBytes {
		return nil, errors.New("cover source image is too large")
	}
	r, err := s.openMergeSource(ctx, f)
	if err != nil {
		return nil, errors.New("cover source image could not be read")
	}
	defer r.Close()
	raw, err := io.ReadAll(io.LimitReader(r, maxAudioCoverSourceBytes+1))
	if err != nil || len(raw) == 0 || len(raw) > maxAudioCoverSourceBytes {
		return nil, errors.New("cover source image could not be read")
	}
	return normalizeAudioCover(raw)
}

func (s *Server) findAudioSubtitles(ctx context.Context, inputs []File) ([]*File, error) {
	matched := make([]*File, len(inputs))
	byParent := make(map[string][]File)
	for index, input := range inputs {
		if input.ParentID == nil {
			continue
		}
		candidates, loaded := byParent[*input.ParentID]
		if !loaded {
			rows, err := s.db.QueryContext(ctx, `SELECT `+fileColumns+` FROM files WHERE parent_id=? AND kind='file' AND status='ready' AND deleted_at IS NULL AND lower(name) LIKE '%.vtt'`, *input.ParentID)
			if err != nil {
				return nil, err
			}
			for rows.Next() {
				candidate, scanErr := scanFile(rows)
				if scanErr != nil {
					rows.Close()
					return nil, scanErr
				}
				candidates = append(candidates, candidate)
			}
			if err := rows.Err(); err != nil {
				rows.Close()
				return nil, err
			}
			if err := rows.Close(); err != nil {
				return nil, err
			}
			byParent[*input.ParentID] = candidates
		}
		bestPriority := 0
		for candidateIndex := range candidates {
			priority, ok := audioSubtitleMatchPriority(input.Name, candidates[candidateIndex].Name)
			if !ok || matched[index] != nil && priority >= bestPriority {
				continue
			}
			bestPriority = priority
			candidate := candidates[candidateIndex]
			matched[index] = &candidate
		}
	}
	return matched, nil
}

// WebVTT exporters commonly preserve the original audio extension, producing
// names such as "track.mp3.vtt" even after the audio was converted to WAV.
// Prefer the plain "track.vtt", then an exact "track.wav.vtt", and finally a
// matching title carrying any supported audio extension.
func audioSubtitleMatchPriority(audioName, subtitleName string) (int, bool) {
	audioTitle := strings.TrimSuffix(audioName, filepath.Ext(audioName))
	if !strings.EqualFold(filepath.Ext(subtitleName), ".vtt") {
		return 0, false
	}
	subtitleTitle := strings.TrimSuffix(subtitleName, filepath.Ext(subtitleName))
	if strings.EqualFold(subtitleTitle, audioTitle) {
		return 0, true
	}
	if strings.EqualFold(subtitleTitle, audioName) {
		return 1, true
	}
	sourceExt := strings.ToLower(filepath.Ext(subtitleTitle))
	if audioSourceExts[sourceExt] && strings.EqualFold(strings.TrimSuffix(subtitleTitle, filepath.Ext(subtitleTitle)), audioTitle) {
		return 2, true
	}
	return 0, false
}

func (s *Server) prepareMergedSubtitles(ctx context.Context, sources []*File, durations []time.Duration, openSource func(context.Context, File) (io.ReadCloser, error)) ([]storedAudioSubtitle, error) {
	merged := make([]storedAudioSubtitle, 0)
	var offset int64
	var mergedTextBytes int
	for index, duration := range durations {
		if index >= len(sources) || sources[index] == nil {
			offset += duration.Milliseconds()
			continue
		}
		source := *sources[index]
		if source.Size <= 0 || source.Size > maxAudioSubtitleBytes {
			return nil, audioMergeUserError{message: fmt.Sprintf("字幕「%s」为空或超过 8 MiB", source.Name)}
		}
		reader, err := openSource(ctx, source)
		if err != nil {
			return nil, fmt.Errorf("open subtitle %s: %w", source.ID, err)
		}
		raw, readErr := io.ReadAll(io.LimitReader(reader, maxAudioSubtitleBytes+1))
		closeErr := reader.Close()
		if readErr != nil {
			return nil, fmt.Errorf("read subtitle %s: %w", source.ID, readErr)
		}
		if closeErr != nil {
			return nil, closeErr
		}
		if len(raw) > maxAudioSubtitleBytes {
			return nil, audioMergeUserError{message: fmt.Sprintf("字幕「%s」超过 8 MiB", source.Name)}
		}
		cues, err := parseWebVTT(raw, duration)
		if err != nil {
			return nil, audioMergeUserError{message: fmt.Sprintf("字幕「%s」不是有效的 WebVTT：%v", source.Name, err)}
		}
		if len(cues) == 0 {
			return nil, audioMergeUserError{message: fmt.Sprintf("字幕「%s」没有可用的字幕条目", source.Name)}
		}
		if len(merged)+len(cues) > maxAudioSubtitleCues {
			return nil, audioMergeUserError{message: "合并后的字幕条目超过 100000 条"}
		}
		for _, cue := range cues {
			mergedTextBytes += len(cue.Text)
			if mergedTextBytes > maxMergedAudioSubtitleBytes {
				return nil, audioMergeUserError{message: "合并后的字幕文本超过 32 MiB"}
			}
			cue.StartMS += offset
			cue.EndMS += offset
			merged = append(merged, cue)
		}
		offset += duration.Milliseconds()
	}
	return merged, nil
}

func parseWebVTT(raw []byte, duration time.Duration) ([]storedAudioSubtitle, error) {
	text := strings.TrimPrefix(string(raw), "\ufeff")
	text = strings.ReplaceAll(strings.ReplaceAll(text, "\r\n", "\n"), "\r", "\n")
	lines := strings.Split(text, "\n")
	if len(lines) == 0 || !strings.HasPrefix(strings.TrimSpace(lines[0]), "WEBVTT") {
		return nil, errors.New("缺少 WEBVTT 文件头")
	}
	index := 1
	for index < len(lines) && strings.TrimSpace(lines[index]) != "" {
		index++
	}
	limit := max(duration.Milliseconds(), 1)
	cues := make([]storedAudioSubtitle, 0)
	for index < len(lines) {
		for index < len(lines) && strings.TrimSpace(lines[index]) == "" {
			index++
		}
		if index >= len(lines) {
			break
		}
		start := index
		for index < len(lines) && strings.TrimSpace(lines[index]) != "" {
			index++
		}
		block := lines[start:index]
		first := strings.TrimSpace(block[0])
		if first == "STYLE" || first == "REGION" || strings.HasPrefix(first, "NOTE") {
			continue
		}
		timingIndex := -1
		for candidate := 0; candidate < len(block) && candidate < 2; candidate++ {
			if strings.Contains(block[candidate], "-->") {
				timingIndex = candidate
				break
			}
		}
		if timingIndex < 0 {
			return nil, fmt.Errorf("第 %d 行附近缺少时间轴", start+1)
		}
		timing := strings.SplitN(block[timingIndex], "-->", 2)
		right := strings.Fields(strings.TrimSpace(timing[1]))
		if len(right) == 0 {
			return nil, fmt.Errorf("第 %d 行的结束时间无效", start+timingIndex+1)
		}
		startMS, err := parseWebVTTTimestamp(strings.TrimSpace(timing[0]))
		if err != nil {
			return nil, fmt.Errorf("第 %d 行的开始时间无效", start+timingIndex+1)
		}
		endMS, err := parseWebVTTTimestamp(right[0])
		if err != nil || endMS <= startMS {
			return nil, fmt.Errorf("第 %d 行的结束时间无效", start+timingIndex+1)
		}
		if startMS >= limit {
			continue
		}
		endMS = min(endMS, limit)
		payload := strings.TrimSpace(strings.Join(block[timingIndex+1:], "\n"))
		plain := plainVTTText(payload)
		if plain == "" {
			continue
		}
		cues = append(cues, storedAudioSubtitle{StartMS: startMS, EndMS: endMS, Text: plain})
	}
	return cues, nil
}

func parseWebVTTTimestamp(value string) (int64, error) {
	parts := strings.Split(value, ":")
	if len(parts) != 2 && len(parts) != 3 {
		return 0, errors.New("invalid timestamp")
	}
	secondsPart := strings.Split(parts[len(parts)-1], ".")
	if len(secondsPart) != 2 || len(secondsPart[1]) != 3 {
		return 0, errors.New("invalid timestamp")
	}
	seconds, err := strconv.Atoi(secondsPart[0])
	if err != nil || seconds < 0 || seconds >= 60 {
		return 0, errors.New("invalid timestamp")
	}
	millis, err := strconv.Atoi(secondsPart[1])
	if err != nil || millis < 0 || millis >= 1000 {
		return 0, errors.New("invalid timestamp")
	}
	minutes, err := strconv.Atoi(parts[len(parts)-2])
	if err != nil || minutes < 0 || minutes >= 60 {
		return 0, errors.New("invalid timestamp")
	}
	hours := 0
	if len(parts) == 3 {
		hours, err = strconv.Atoi(parts[0])
		if err != nil || hours < 0 {
			return 0, errors.New("invalid timestamp")
		}
	}
	return int64(((hours*60+minutes)*60+seconds)*1000 + millis), nil
}

func plainVTTText(value string) string {
	var body strings.Builder
	inTag := false
	for _, r := range value {
		switch r {
		case '<':
			inTag = true
		case '>':
			if inTag {
				inTag = false
			} else {
				body.WriteRune(r)
			}
		default:
			if !inTag {
				body.WriteRune(r)
			}
		}
	}
	return strings.TrimSpace(html.UnescapeString(body.String()))
}

func writeMergedWebVTT(workDir string, cues []storedAudioSubtitle) (string, error) {
	var body strings.Builder
	body.WriteString("WEBVTT\n\n")
	for index, cue := range cues {
		fmt.Fprintf(&body, "%d\n%s --> %s\n%s\n\n", index+1, formatWebVTTTimestamp(cue.StartMS), formatWebVTTTimestamp(cue.EndMS), cue.Text)
	}
	path := filepath.Join(workDir, "subtitles.vtt")
	if err := os.WriteFile(path, []byte(body.String()), 0o600); err != nil {
		return "", fmt.Errorf("write merged subtitles: %w", err)
	}
	return path, nil
}

func formatWebVTTTimestamp(milliseconds int64) string {
	if milliseconds < 0 {
		milliseconds = 0
	}
	hours := milliseconds / 3600000
	minutes := milliseconds / 60000 % 60
	seconds := milliseconds / 1000 % 60
	return fmt.Sprintf("%02d:%02d:%02d.%03d", hours, minutes, seconds, milliseconds%1000)
}
