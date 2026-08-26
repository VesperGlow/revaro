package server

import (
	"bufio"
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"html"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
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
	CreatedAt    string
	UpdatedAt    string
	cancel       context.CancelFunc
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
		InputCount: j.InputCount, CreatedAt: j.CreatedAt, UpdatedAt: j.UpdatedAt,
	}
}

func (j *audioMergeJob) update(status string, progress int, message string) {
	j.mu.Lock()
	defer j.mu.Unlock()
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
}

func (j *audioMergeJob) finish(status, message, jobError string) {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.Status, j.Message, j.Error = status, message, jobError
	if status == "done" {
		j.Progress = 100
	}
	j.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
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

func (s *Server) prepareMergedSubtitles(ctx context.Context, sources []*File, durations []time.Duration) ([]storedAudioSubtitle, error) {
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
		reader, err := s.openMergeSource(ctx, source)
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

func (s *Server) createAudioMerge(w http.ResponseWriter, r *http.Request) {
	var in struct {
		ParentID  string   `json:"parent_id"`
		Name      string   `json:"name"`
		FileIDs   []string `json:"file_ids"`
		Format    string   `json:"format"`
		CoverJPEG string   `json:"cover_jpeg"`
		CoverFile string   `json:"cover_file_id"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if len(in.FileIDs) < 2 || len(in.FileIDs) > maxAudioMergeInputs {
		problem(w, http.StatusBadRequest, "select between 2 and 256 audio files")
		return
	}
	profile, ok := audioOutput(in.Format)
	if !ok {
		problem(w, http.StatusBadRequest, "audio merge format must be flac, alac, or aac")
		return
	}
	if filepath.Ext(in.Name) == "" {
		in.Name += profile.Extension
	}
	if err := validateName(in.Name); err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	if !strings.EqualFold(filepath.Ext(in.Name), profile.Extension) {
		problem(w, http.StatusBadRequest, "merged audio filename does not match the selected format")
		return
	}
	parent, err := s.file(r.Context(), in.ParentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		problem(w, http.StatusBadRequest, "parent directory is invalid")
		return
	}
	seen := make(map[string]struct{}, len(in.FileIDs))
	inputs := make([]File, 0, len(in.FileIDs))
	var totalSize int64
	for _, id := range in.FileIDs {
		if _, duplicate := seen[id]; duplicate {
			problem(w, http.StatusBadRequest, "an audio file was selected more than once")
			return
		}
		seen[id] = struct{}{}
		f, err := s.file(r.Context(), id)
		if err != nil || !isAudioSource(f) {
			problem(w, http.StatusUnsupportedMediaType, "every selected item must be a ready audio file")
			return
		}
		if f.Size > maxLogicalFileSize-totalSize {
			problem(w, http.StatusRequestEntityTooLarge, "selected audio is too large")
			return
		}
		totalSize += f.Size
		inputs = append(inputs, f)
	}
	if _, err := exec.LookPath(s.cfg.FFmpegPath); err != nil {
		problem(w, http.StatusServiceUnavailable, "ffmpeg is unavailable")
		return
	}
	if in.CoverJPEG != "" && in.CoverFile != "" {
		problem(w, http.StatusBadRequest, "choose either an uploaded cover or a directory image")
		return
	}
	cover, err := decodeAudioCover(in.CoverJPEG)
	if err == nil && in.CoverFile != "" {
		cover, err = s.audioCoverFromFile(r.Context(), in.ParentID, in.CoverFile)
	}
	if err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	subtitles, err := s.findAudioSubtitles(r.Context(), inputs)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not match audio subtitles")
		return
	}

	now := time.Now().UTC().Format(time.RFC3339Nano)
	outputID := ids.New()
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,0,?,'pending',?,?)`, outputID, in.ParentID, in.Name, "file", profile.MimeType, now, now)
	if isConflict(err) {
		problem(w, http.StatusConflict, "an item with that name already exists")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not reserve merged audio")
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), audioMergeTimeout)
	job := &audioMergeJob{
		ID: ids.New(), Status: "queued", Progress: 1, Message: "等待合并任务开始",
		OutputName: in.Name, OutputFormat: profile.Format, OutputFileID: outputID, ParentID: in.ParentID, InputCount: len(inputs),
		CreatedAt: now, UpdatedAt: now, cancel: cancel,
	}
	s.audioMergeMu.Lock()
	s.audioMergeJobs[job.ID] = job
	s.audioMergeMu.Unlock()
	go s.runAudioMerge(ctx, job, inputs, subtitles, profile, cover)
	writeJSON(w, http.StatusAccepted, job.snapshot())
}

func (s *Server) listAudioMerges(w http.ResponseWriter, _ *http.Request) {
	s.audioMergeMu.RLock()
	jobs := make([]*audioMergeJob, 0, len(s.audioMergeJobs))
	for _, job := range s.audioMergeJobs {
		jobs = append(jobs, job)
	}
	s.audioMergeMu.RUnlock()
	snapshots := make([]audioMergeSnapshot, 0, len(jobs))
	for _, job := range jobs {
		snapshots = append(snapshots, job.snapshot())
	}
	sort.Slice(snapshots, func(i, j int) bool { return snapshots[i].CreatedAt > snapshots[j].CreatedAt })
	writeJSON(w, http.StatusOK, map[string]any{"items": snapshots})
}

func (s *Server) getAudioMerge(w http.ResponseWriter, r *http.Request) {
	s.audioMergeMu.RLock()
	job := s.audioMergeJobs[chi.URLParam(r, "id")]
	s.audioMergeMu.RUnlock()
	if job == nil {
		problem(w, http.StatusNotFound, "audio merge job not found")
		return
	}
	writeJSON(w, http.StatusOK, job.snapshot())
}

func (s *Server) cancelAudioMerge(w http.ResponseWriter, r *http.Request) {
	s.audioMergeMu.RLock()
	job := s.audioMergeJobs[chi.URLParam(r, "id")]
	s.audioMergeMu.RUnlock()
	if job == nil {
		problem(w, http.StatusNotFound, "audio merge job not found")
		return
	}
	snapshot := job.snapshot()
	if snapshot.Status == "done" || snapshot.Status == "failed" || snapshot.Status == "cancelled" {
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
	} else {
		job.update("cancelling", snapshot.Progress, "正在取消合并")
		job.cancel()
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) runAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, subtitles []*File, profile audioOutputProfile, cover []byte) {
	defer job.cancel()
	err := s.executeAudioMerge(ctx, job, inputs, subtitles, profile, cover)
	if err == nil {
		job.finish("done", "合并完成", "")
		s.log.Info("audio merge completed", "job", job.ID, "file", job.OutputFileID, "format", profile.Format, "inputs", len(inputs))
	} else {
		_, _ = s.db.ExecContext(context.Background(), `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
		if errors.Is(err, context.Canceled) {
			job.finish("cancelled", "合并已取消", "")
		} else if errors.Is(err, context.DeadlineExceeded) {
			job.finish("failed", "合并失败", "合并任务运行时间超过 24 小时")
		} else {
			var userError audioMergeUserError
			if errors.As(err, &userError) {
				job.finish("failed", "合并失败", userError.Error())
			} else {
				job.finish("failed", "合并失败", "音频格式不兼容、文件损坏或临时空间不足")
			}
		}
		s.log.Error("audio merge failed", "job", job.ID, "output", job.OutputName, "error", err)
	}
	time.AfterFunc(6*time.Hour, func() {
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
	})
}

func (s *Server) executeAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, subtitleSources []*File, profile audioOutputProfile, cover []byte) error {
	select {
	case s.audioMergeSlots <- struct{}{}:
		defer func() { <-s.audioMergeSlots }()
	case <-ctx.Done():
		return ctx.Err()
	}
	job.update("preparing", 3, "正在从对象存储准备音频")
	tempRoot := s.cfg.DataDir
	workDir, err := os.MkdirTemp(tempRoot, ".revaro-audio-merge-")
	if err != nil {
		return fmt.Errorf("create audio merge workspace: %w", err)
	}
	defer os.RemoveAll(workDir)

	var totalBytes int64
	for _, input := range inputs {
		totalBytes += input.Size
	}
	var copiedBytes int64
	paths := make([]string, 0, len(inputs))
	durations := make([]time.Duration, 0, len(inputs))
	probes := make([]audioProbe, 0, len(inputs))
	probe, probeErr := ffprobeFor(s.cfg.FFmpegPath)
	if probeErr != nil {
		return fmt.Errorf("ffprobe is required for chaptered audio merge: %w", probeErr)
	}
	for index, input := range inputs {
		if err := ctx.Err(); err != nil {
			return err
		}
		ext := strings.ToLower(filepath.Ext(input.Name))
		if !audioSourceExts[ext] {
			ext = ".audio"
		}
		path := filepath.Join(workDir, fmt.Sprintf("input-%04d%s", index, ext))
		out, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
		if err != nil {
			return err
		}
		reader, err := s.openMergeSource(ctx, input)
		if err != nil {
			out.Close()
			return err
		}
		progressReader := &mergeProgressReader{ctx: ctx, r: reader, onRead: func(n int64) {
			copiedBytes += n
			if totalBytes > 0 {
				job.update("preparing", 3+int(copiedBytes*34/totalBytes), fmt.Sprintf("正在准备第 %d / %d 段", index+1, len(inputs)))
			}
		}}
		n, copyErr := io.CopyBuffer(out, progressReader, make([]byte, 256<<10))
		closeErr := out.Close()
		reader.Close()
		if copyErr != nil {
			return copyErr
		}
		if closeErr != nil {
			return closeErr
		}
		if n != input.Size {
			return fmt.Errorf("audio source %s size mismatch: got %d, want %d", input.ID, n, input.Size)
		}
		paths = append(paths, path)
		if probe != "" {
			info, probeErr := probeAudio(ctx, probe, path)
			if probeErr != nil {
				return fmt.Errorf("probe audio input %s: %w", input.ID, probeErr)
			}
			durations = append(durations, info.Duration)
			probes = append(probes, info)
		}
	}

	metadataPath, err := writeChapterMetadata(workDir, job.OutputName, inputs, durations)
	if err != nil {
		return err
	}
	coverPath := ""
	if len(cover) > 0 {
		coverPath = filepath.Join(workDir, "cover.jpg")
		if err := os.WriteFile(coverPath, cover, 0o600); err != nil {
			return fmt.Errorf("write audio cover: %w", err)
		}
	}
	storedSubtitles := make([]storedAudioSubtitle, 0)
	subtitlePath := ""
	if profile.Subtitles {
		storedSubtitles, err = s.prepareMergedSubtitles(ctx, subtitleSources, durations)
		if err != nil {
			return err
		}
		if len(storedSubtitles) > 0 {
			subtitlePath, err = writeMergedWebVTT(workDir, storedSubtitles)
			if err != nil {
				return err
			}
		}
	}
	outputPath := filepath.Join(workDir, "merged"+profile.Extension)
	if err := runAudioFFmpeg(ctx, s.cfg.FFmpegPath, paths, metadataPath, coverPath, subtitlePath, job.OutputName, durations, probes, profile, outputPath, 86, job); err != nil {
		return err
	}

	job.update("saving", 88, "正在写入 Revaro 对象存储")
	key, masterSize, masterETag, err := s.storeAudioArtifact(ctx, outputPath, 88, 99, "正在保存合并音频", job)
	if err != nil {
		return fmt.Errorf("store merged audio: %w", err)
	}
	if len(cover) > 0 {
		thumb, err := resizeToJPEG(cover, thumbMaxDim)
		if err != nil {
			return fmt.Errorf("prepare audio cover thumbnail: %w", err)
		}
		if err := s.storage.PutImmutable(ctx, thumbnailKey(key), "image/jpeg", thumb); err != nil {
			return fmt.Errorf("store audio cover thumbnail: %w", err)
		}
	}
	chaptersJSON, err := json.Marshal(buildStoredAudioChapters(inputs, durations))
	if err != nil {
		return fmt.Errorf("encode audio chapters: %w", err)
	}
	subtitlesJSON, err := json.Marshal(storedSubtitles)
	if err != nil {
		return fmt.Errorf("encode audio subtitles: %w", err)
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	result, err := tx.ExecContext(ctx, `UPDATE files SET object_key=?,size=?,mime_type=?,etag=?,status='ready',updated_at=? WHERE id=? AND status='pending'`, key, masterSize, profile.MimeType, masterETag, now, job.OutputFileID)
	if err != nil {
		return fmt.Errorf("commit merged audio file: %w", err)
	}
	if changed, _ := result.RowsAffected(); changed == 0 {
		return errors.New("merged audio placeholder was removed")
	}
	_, err = tx.ExecContext(ctx, `INSERT INTO audio_media(file_id,duration_ms,chapters_json,subtitles_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)`,
		job.OutputFileID, durationMilliseconds(durations), string(chaptersJSON), string(subtitlesJSON), key, masterSize, masterETag, len(cover) > 0, now, now)
	if err != nil {
		return fmt.Errorf("commit audio chapters and stream: %w", err)
	}
	return tx.Commit()
}

func (s *Server) storeAudioArtifact(ctx context.Context, path string, progressStart, progressEnd int, message string, job *audioMergeJob) (string, int64, string, error) {
	input, err := os.Open(path)
	if err != nil {
		return "", 0, "", err
	}
	defer input.Close()
	info, err := input.Stat()
	if err != nil {
		return "", 0, "", err
	}
	if info.Size() <= 0 {
		return "", 0, "", errors.New("ffmpeg produced an empty audio file")
	}
	var stored int64
	reader := &mergeProgressReader{ctx: ctx, r: input, onRead: func(n int64) {
		stored += n
		job.update("saving", progressStart+int(stored*int64(progressEnd-progressStart)/info.Size()), message)
	}}
	key, stored, err := s.storeBlob(ctx, reader, info.Size(), profile.MimeType)
	if err != nil {
		return "", 0, "", err
	}
	return key, stored.Size, stored.ETag, nil
}

func (s *Server) openMergeSource(ctx context.Context, f File) (io.ReadCloser, error) {
	if storage.IsManifestKey(f.objectKey) {
		return s.storage.Open(storage.WithDynamicReadAhead(ctx), f.objectKey)
	}
	return s.storage.OpenRaw(ctx, f.objectKey)
}

type mergeProgressReader struct {
	ctx    context.Context
	r      io.Reader
	onRead func(int64)
}

func (r *mergeProgressReader) Read(p []byte) (int, error) {
	if err := r.ctx.Err(); err != nil {
		return 0, err
	}
	n, err := r.r.Read(p)
	if n > 0 && r.onRead != nil {
		r.onRead(int64(n))
	}
	return n, err
}

func ffprobeFor(ffmpeg string) (string, error) {
	resolved, err := exec.LookPath(ffmpeg)
	if err != nil {
		return "", err
	}
	candidate := filepath.Join(filepath.Dir(resolved), "ffprobe")
	if info, statErr := os.Stat(candidate); statErr == nil && !info.IsDir() {
		return candidate, nil
	}
	return exec.LookPath("ffprobe")
}

type audioProbe struct {
	Duration   time.Duration
	SampleRate int
	Channels   int
	Layout     string
}

func probeAudio(ctx context.Context, ffprobe, path string) (audioProbe, error) {
	out, err := exec.CommandContext(ctx, ffprobe, "-v", "error", "-select_streams", "a:0", "-show_entries", "format=duration:stream=sample_rate,channels,channel_layout", "-of", "json", path).Output()
	if err != nil {
		return audioProbe{}, err
	}
	var result struct {
		Format struct {
			Duration string `json:"duration"`
		} `json:"format"`
		Streams []struct {
			SampleRate    string `json:"sample_rate"`
			Channels      int    `json:"channels"`
			ChannelLayout string `json:"channel_layout"`
		} `json:"streams"`
	}
	if err := json.Unmarshal(out, &result); err != nil || len(result.Streams) == 0 {
		return audioProbe{}, errors.New("audio stream information is unavailable")
	}
	seconds, err := strconv.ParseFloat(result.Format.Duration, 64)
	if err != nil || seconds <= 0 {
		return audioProbe{}, errors.New("audio duration is unavailable")
	}
	rate, err := strconv.Atoi(result.Streams[0].SampleRate)
	if err != nil || rate < 8000 || rate > 384000 || result.Streams[0].Channels < 1 || result.Streams[0].Channels > 8 {
		return audioProbe{}, errors.New("audio stream format is unsupported")
	}
	layout, ok := normalizeAudioLayout(result.Streams[0].ChannelLayout, result.Streams[0].Channels)
	if !ok {
		return audioProbe{}, errors.New("audio channel layout is unavailable")
	}
	return audioProbe{Duration: time.Duration(seconds * float64(time.Second)), SampleRate: rate, Channels: result.Streams[0].Channels, Layout: layout}, nil
}

func normalizeAudioLayout(layout string, channels int) (string, bool) {
	if layout == "" || layout == "unknown" {
		if channels == 1 {
			return "mono", true
		}
		if channels == 2 {
			return "stereo", true
		}
		return "", false
	}
	for _, r := range layout {
		if !(r >= 'a' && r <= 'z') && !(r >= 'A' && r <= 'Z') && !(r >= '0' && r <= '9') && !strings.ContainsRune("._()+-", r) {
			return "", false
		}
	}
	return layout, true
}

func audioMergeTarget(probes []audioProbe) (int, string) {
	rate, channels, layout := 48000, 2, "stereo"
	if len(probes) > 0 {
		rate, channels, layout = probes[0].SampleRate, probes[0].Channels, probes[0].Layout
	}
	for _, probe := range probes[1:] {
		if probe.SampleRate > rate {
			rate = probe.SampleRate
		}
		if probe.Channels > channels {
			channels, layout = probe.Channels, probe.Layout
		}
	}
	return rate, layout
}

func buildStoredAudioChapters(inputs []File, durations []time.Duration) []storedAudioChapter {
	chapters := make([]storedAudioChapter, 0, len(inputs))
	var start int64
	for index, input := range inputs {
		end := start + durations[index].Milliseconds()
		chapters = append(chapters, storedAudioChapter{
			Title:   strings.TrimSuffix(input.Name, filepath.Ext(input.Name)),
			StartMS: start,
			EndMS:   max(end, start+1),
		})
		start = end
	}
	return chapters
}

func writeChapterMetadata(workDir, outputName string, inputs []File, durations []time.Duration) (string, error) {
	var body strings.Builder
	body.WriteString(";FFMETADATA1\n")
	body.WriteString("title=" + escapeFFMetadata(strings.TrimSuffix(outputName, filepath.Ext(outputName))) + "\n")
	var start int64
	for index, input := range inputs {
		end := start + durations[index].Milliseconds()
		fmt.Fprintf(&body, "[CHAPTER]\nTIMEBASE=1/1000\nSTART=%d\nEND=%d\ntitle=%s\n", start, end, escapeFFMetadata(strings.TrimSuffix(input.Name, filepath.Ext(input.Name))))
		start = end
	}
	path := filepath.Join(workDir, "chapters.ffmetadata")
	return path, os.WriteFile(path, []byte(body.String()), 0o600)
}

func escapeFFMetadata(value string) string {
	replacer := strings.NewReplacer("\\", "\\\\", "=", "\\=", ";", "\\;", "#", "\\#", "\n", " ", "\r", " ")
	return replacer.Replace(value)
}

func runAudioFFmpeg(ctx context.Context, ffmpeg string, paths []string, metadataPath, coverPath, subtitlePath, outputName string, durations []time.Duration, probes []audioProbe, profile audioOutputProfile, outputPath string, progressEnd int, job *audioMergeJob) error {
	args := []string{"-hide_banner", "-loglevel", "error", "-y"}
	for _, path := range paths {
		args = append(args, "-i", path)
	}
	nextInputIndex := len(paths)
	metadataIndex := -1
	if metadataPath != "" {
		metadataIndex = nextInputIndex
		nextInputIndex++
		args = append(args, "-f", "ffmetadata", "-i", metadataPath)
	}
	coverIndex := -1
	if coverPath != "" {
		coverIndex = nextInputIndex
		nextInputIndex++
		args = append(args, "-i", coverPath)
	}
	subtitleIndex := -1
	if subtitlePath != "" {
		subtitleIndex = nextInputIndex
		args = append(args, "-i", subtitlePath)
	}
	parts := make([]string, 0, len(paths)+1)
	labels := make([]string, 0, len(paths))
	targetRate, targetLayout := audioMergeTarget(probes)
	targetSampleFormat := "fltp"
	if profile.Lossless {
		targetSampleFormat = "s32"
	}
	for index := range paths {
		label := fmt.Sprintf("a%d", index)
		parts = append(parts, fmt.Sprintf("[%d:a:0]aresample=%d,aformat=sample_fmts=%s:sample_rates=%d:channel_layouts=%s,asetpts=N/SR/TB[%s]", index, targetRate, targetSampleFormat, targetRate, targetLayout, label))
		labels = append(labels, "["+label+"]")
	}
	parts = append(parts, strings.Join(labels, "")+fmt.Sprintf("concat=n=%d:v=0:a=1[outa]", len(paths)))
	args = append(args, "-filter_complex", strings.Join(parts, ";"), "-map", "[outa]")
	if metadataIndex >= 0 {
		index := strconv.Itoa(metadataIndex)
		args = append(args, "-map_metadata", index)
		if profile.Chapters {
			args = append(args, "-map_chapters", index)
		}
	}
	if coverIndex >= 0 {
		args = append(args, "-map", strconv.Itoa(coverIndex)+":v:0", "-c:v", "copy", "-disposition:v:0", "attached_pic", "-metadata:s:v:0", "title=Cover", "-metadata:s:v:0", "comment=Cover (front)")
	} else {
		args = append(args, "-vn")
	}
	if subtitleIndex >= 0 {
		args = append(args, "-map", strconv.Itoa(subtitleIndex)+":s:0", "-c:s", "mov_text", "-metadata:s:s:0", "language=und", "-metadata:s:s:0", "title=字幕", "-disposition:s:0", "default")
	}
	args = append(args, "-metadata", "title="+strings.TrimSuffix(outputName, filepath.Ext(outputName)), "-c:a", profile.Codec)
	args = append(args, profile.EncoderArgs...)
	args = append(args, "-progress", "pipe:1", "-nostats", "-f", profile.Container, outputPath)
	cmd := exec.CommandContext(ctx, ffmpeg, args...)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	if err := cmd.Start(); err != nil {
		return err
	}
	job.update("merging", 40, "FFmpeg 正在按顺序合并音频")
	var total time.Duration
	for _, duration := range durations {
		total += duration
	}
	scanner := bufio.NewScanner(stdout)
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "out_time_us=") && total > 0 {
			micros, parseErr := strconv.ParseInt(strings.TrimPrefix(line, "out_time_us="), 10, 64)
			if parseErr == nil {
				progress := 40 + int(time.Duration(micros)*time.Microsecond*time.Duration(progressEnd-40)/total)
				job.update("merging", min(progress, progressEnd), "FFmpeg 正在按顺序合并音频")
			}
		}
	}
	if err := scanner.Err(); err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}
	if err := cmd.Wait(); err != nil {
		return fmt.Errorf("ffmpeg: %w: %s", err, strings.TrimSpace(stderr.String()))
	}
	job.update("merging", progressEnd, "音频母版编码完成")
	return nil
}

type limitedBuffer struct {
	buf   bytes.Buffer
	limit int
}

func (b *limitedBuffer) Write(p []byte) (int, error) {
	original := len(p)
	remaining := b.limit - b.buf.Len()
	if remaining > 0 {
		_, _ = b.buf.Write(p[:min(len(p), remaining)])
	}
	return original, nil
}

func (b *limitedBuffer) String() string { return b.buf.String() }
