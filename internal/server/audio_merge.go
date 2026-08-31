package server

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"html"
	"io"
	"log/slog"
	"net/http"
	"os"
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
	if j.changed != nil {
		defer j.changed()
	}
}

func (j *audioMergeJob) finish(status, message, jobError string) {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.Status, j.Message, j.Error = status, message, jobError
	if j.changed != nil {
		defer j.changed()
	}
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

	ctx, cancel := context.WithTimeout(s.audioHLSCtx, audioMergeTimeout)
	job := &audioMergeJob{
		changed: s.jobs.Changed,
		ID:      ids.New(), Status: "queued", Progress: 1, Message: "等待合并任务开始",
		OutputName: in.Name, OutputFormat: profile.Format, OutputFileID: outputID, ParentID: in.ParentID, InputCount: len(inputs),
		Source: "revaro", CreatedAt: now, UpdatedAt: now, cancel: cancel,
	}
	s.audioMergeMu.Lock()
	s.audioMergeJobs[job.ID] = job
	s.audioMergeMu.Unlock()
	if !s.runBackground(func() { s.runAudioMerge(ctx, job, inputs, subtitles, profile, cover) }) {
		cancel()
		s.audioMergeFinished(job, context.Canceled, profile, len(inputs))
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}
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
		job.cleanupStaging(s.log)
		job.releaseUploadSlot(s)
	} else if snapshot.Status == "uploading" {
		// Local-directory upload in progress: drop the staging directory,
		// the reserved output placeholder and the upload slot immediately.
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
		job.cleanupStaging(s.log)
		job.releaseUploadSlot(s)
		_, _ = s.db.ExecContext(r.Context(), `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
	} else {
		job.update("cancelling", snapshot.Progress, "正在取消合并")
		job.cancel()
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) runAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, subtitles []*File, profile audioOutputProfile, cover []byte) {
	defer job.cancel()
	err := s.executeAudioMerge(ctx, job, inputs, subtitles, profile, cover)
	s.audioMergeFinished(job, err, profile, len(inputs))
}

// audioMergeFinished records the outcome of a merge, removes the pending
// output placeholder on failure, releases shared resources and schedules the
// terminal job removal. Shared by object-storage and local-directory merges.
func (s *Server) audioMergeFinished(job *audioMergeJob, err error, profile audioOutputProfile, inputCount int) {
	if err == nil {
		job.finish("done", "合并完成", "")
		s.log.Info("audio merge completed", "job", job.ID, "file", job.OutputFileID, "format", profile.Format, "inputs", inputCount)
	} else {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		_, _ = s.db.ExecContext(cleanupCtx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
		cancel()
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
	job.cleanupStaging(s.log)
	job.releaseUploadSlot(s)
	s.scheduleAudioMergeRemoval(job, 6*time.Hour)
}

func (s *Server) scheduleAudioMergeRemoval(job *audioMergeJob, after time.Duration) bool {
	return s.runBackground(func() {
		timer := time.NewTimer(after)
		defer timer.Stop()
		select {
		case <-s.audioHLSCtx.Done():
			return
		case <-timer.C:
		}
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
		job.cleanupStaging(s.log)
		job.releaseUploadSlot(s)
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
	tempRoot := s.cfg.WorkDir
	workDir, err := os.MkdirTemp(tempRoot, "revaro-audio-merge-")
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
	}

	return s.encodeMergedAudio(ctx, job, profile, workDir, inputs, paths, subtitleSources, s.openMergeSource, cover)
}

// encodeMergedAudio runs the shared FFmpeg pipeline for an audio merge whose
// source files are already materialized at local paths: chapter metadata,
// cover embedding, subtitle time-shifting, ALAC/FLAC/AAC encoding and finally
// storing the master artifact as a normal blobs/<UUID> object.
func (s *Server) encodeMergedAudio(ctx context.Context, job *audioMergeJob, profile audioOutputProfile, workDir string, inputs []File, paths []string, subtitleSources []*File, openSubtitle func(context.Context, File) (io.ReadCloser, error), cover []byte) error {
	engine, ok := s.storage.(storage.MediaEngine)
	if !ok {
		return errors.New("Rust media engine is unavailable")
	}
	outputPath := filepath.Join(workDir, "merged"+profile.Extension)
	job.update("merging", 40, "Rust media engine 正在按顺序合并音频")
	inputNames := make([]string, len(inputs))
	for index := range inputs {
		inputNames[index] = inputs[index].Name
	}
	merged, err := engine.MergeAudio(ctx, paths, inputNames, outputPath, profile.Format, job.OutputName)
	if err != nil {
		return err
	}
	durations := make([]time.Duration, len(merged.DurationsMS))
	for index, milliseconds := range merged.DurationsMS {
		durations[index] = time.Duration(milliseconds) * time.Millisecond
	}
	if len(durations) != len(inputs) {
		return errors.New("Rust media engine returned inconsistent audio durations")
	}
	storedSubtitles := make([]storedAudioSubtitle, 0)
	subtitlePath := ""
	if profile.Subtitles {
		storedSubtitles, err = s.prepareMergedSubtitles(ctx, subtitleSources, durations, openSubtitle)
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
	coverPath := ""
	if len(cover) > 0 {
		embeddedCover, coverErr := resizeToJPEG(cover, 2048)
		if coverErr != nil {
			return fmt.Errorf("prepare embedded audio cover: %w", coverErr)
		}
		coverPath = filepath.Join(workDir, "embedded-cover.jpg")
		if err := os.WriteFile(coverPath, embeddedCover, 0o600); err != nil {
			return fmt.Errorf("write embedded audio cover: %w", err)
		}
	}
	if coverPath != "" || subtitlePath != "" {
		job.update("merging", 82, "正在嵌入封面与字幕")
		if err := engine.DecorateAudio(ctx, outputPath, coverPath, subtitlePath); err != nil {
			return err
		}
	}
	job.update("merging", 86, "音频母版编码完成")
	return s.finalizeAudioMerge(ctx, job, outputPath, profile, cover, inputs, durations, storedSubtitles)
}

// finalizeAudioMerge uploads the encoded master to object storage as a normal
// blobs/<UUID> object, stores the cover thumbnail and commits the file record
// with chapters and subtitle metadata.
func (s *Server) finalizeAudioMerge(ctx context.Context, job *audioMergeJob, outputPath string, profile audioOutputProfile, cover []byte, inputs []File, durations []time.Duration, storedSubtitles []storedAudioSubtitle) error {
	job.update("saving", 88, "正在写入 Revaro 对象存储")
	key, masterSize, masterETag, err := s.storeAudioArtifact(ctx, outputPath, profile.MimeType, 88, 99, "正在保存合并音频", job)
	if err != nil {
		return fmt.Errorf("store merged audio: %w", err)
	}
	if len(cover) > 0 {
		thumb, err := resizeToJPEG(cover, thumbMaxDim)
		if err != nil {
			return fmt.Errorf("prepare audio cover thumbnail: %w", err)
		}
		if err := s.storage.PutImmutable(ctx, audioThumbnailKey(key), "image/jpeg", thumb); err != nil {
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

func (s *Server) storeAudioArtifact(ctx context.Context, path, mimeType string, progressStart, progressEnd int, message string, job *audioMergeJob) (string, int64, string, error) {
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
		return "", 0, "", errors.New("audio encoder produced an empty file")
	}
	var storedBytes int64
	reader := &mergeProgressReader{ctx: ctx, r: input, onRead: func(n int64) {
		storedBytes += n
		job.update("saving", progressStart+int(storedBytes*int64(progressEnd-progressStart)/info.Size()), message)
	}}
	key, stored, err := s.storeBlob(ctx, reader, info.Size(), mimeType)
	if err != nil {
		return "", 0, "", err
	}
	return key, stored.Size, stored.ETag, nil
}

func (s *Server) openMergeSource(ctx context.Context, f File) (io.ReadCloser, error) {
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
