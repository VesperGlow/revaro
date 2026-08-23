package server

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
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

var audioSourceExts = map[string]bool{
	".mp3": true, ".wav": true, ".flac": true, ".m4a": true, ".aac": true,
	".ogg": true, ".oga": true, ".opus": true, ".wma": true, ".aif": true,
	".aiff": true, ".ape": true,
}

type audioOutputProfile struct {
	Format      string
	Extension   string
	MimeType    string
	Codec       string
	Container   string
	Lossless    bool
	Chapters    bool
	EncoderArgs []string
}

func audioOutput(format string) (audioOutputProfile, bool) {
	switch strings.ToLower(format) {
	case "", "flac":
		return audioOutputProfile{Format: "flac", Extension: ".flac", MimeType: "audio/flac", Codec: "flac", Container: "flac", Lossless: true, EncoderArgs: []string{"-compression_level", "8"}}, true
	case "alac":
		return audioOutputProfile{Format: "alac", Extension: ".m4a", MimeType: "audio/mp4", Codec: "alac", Container: "ipod", Lossless: true, Chapters: true, EncoderArgs: []string{"-movflags", "+faststart"}}, true
	case "aac":
		return audioOutputProfile{Format: "aac", Extension: ".m4a", MimeType: "audio/mp4", Codec: "aac", Container: "ipod", Chapters: true, EncoderArgs: []string{"-b:a", "192k", "-movflags", "+faststart"}}, true
	default:
		return audioOutputProfile{}, false
	}
}

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

func (s *Server) createAudioMerge(w http.ResponseWriter, r *http.Request) {
	var in struct {
		ParentID string   `json:"parent_id"`
		Name     string   `json:"name"`
		FileIDs  []string `json:"file_ids"`
		Format   string   `json:"format"`
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
		OutputName: in.Name, OutputFormat: profile.Format, OutputFileID: outputID, InputCount: len(inputs),
		CreatedAt: now, UpdatedAt: now, cancel: cancel,
	}
	s.audioMergeMu.Lock()
	s.audioMergeJobs[job.ID] = job
	s.audioMergeMu.Unlock()
	go s.runAudioMerge(ctx, job, inputs, profile)
	writeJSON(w, http.StatusAccepted, job.snapshot())
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
	if snapshot.Status != "done" && snapshot.Status != "failed" && snapshot.Status != "cancelled" {
		job.update("cancelling", snapshot.Progress, "正在取消合并")
		job.cancel()
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) runAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, profile audioOutputProfile) {
	defer job.cancel()
	err := s.executeAudioMerge(ctx, job, inputs, profile)
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
			job.finish("failed", "合并失败", "音频格式不兼容、文件损坏或临时空间不足")
		}
		s.log.Error("audio merge failed", "job", job.ID, "output", job.OutputName, "error", err)
	}
	time.AfterFunc(6*time.Hour, func() {
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
	})
}

func (s *Server) executeAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, profile audioOutputProfile) error {
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
	if probeErr != nil && profile.Lossless {
		return fmt.Errorf("ffprobe is required for lossless audio merge: %w", probeErr)
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
				if profile.Lossless {
					return fmt.Errorf("probe lossless audio input %s: %w", input.ID, probeErr)
				}
				probe = ""
				durations = nil
				probes = nil
			} else {
				durations = append(durations, info.Duration)
				probes = append(probes, info)
			}
		}
	}

	metadataPath := ""
	if len(durations) == len(inputs) {
		metadataPath, err = writeChapterMetadata(workDir, job.OutputName, inputs, durations)
		if err != nil {
			return err
		}
	}
	outputPath := filepath.Join(workDir, "merged"+profile.Extension)
	if err := runAudioFFmpeg(ctx, s.cfg.FFmpegPath, paths, metadataPath, job.OutputName, durations, probes, profile, outputPath, job); err != nil {
		return err
	}

	job.update("saving", 88, "正在写入 Revaro 块存储")
	output, err := os.Open(outputPath)
	if err != nil {
		return err
	}
	info, err := output.Stat()
	if err != nil {
		output.Close()
		return err
	}
	if info.Size() <= 0 {
		output.Close()
		return errors.New("ffmpeg produced an empty audio file")
	}
	var stored int64
	progressReader := &mergeProgressReader{ctx: ctx, r: output, onRead: func(n int64) {
		stored += n
		job.update("saving", 88+int(stored*11/info.Size()), "正在保存合并后的音频")
	}}
	key, manifest, err := s.storage.Store(ctx, progressReader)
	output.Close()
	if err != nil {
		return fmt.Errorf("store merged audio: %w", err)
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	result, err := s.db.ExecContext(ctx, `UPDATE files SET object_key=?,size=?,mime_type=?,etag=?,status='ready',updated_at=? WHERE id=? AND status='pending'`, key, manifest.Size, profile.MimeType, manifest.ID(), now, job.OutputFileID)
	if err != nil {
		return fmt.Errorf("commit merged audio metadata: %w", err)
	}
	if changed, _ := result.RowsAffected(); changed == 0 {
		return errors.New("merged audio placeholder was removed")
	}
	return nil
}

func (s *Server) openMergeSource(ctx context.Context, f File) (io.ReadCloser, error) {
	if storage.IsManifestKey(f.objectKey) {
		return s.storage.Open(ctx, f.objectKey)
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

func runAudioFFmpeg(ctx context.Context, ffmpeg string, paths []string, metadataPath, outputName string, durations []time.Duration, probes []audioProbe, profile audioOutputProfile, outputPath string, job *audioMergeJob) error {
	args := []string{"-hide_banner", "-loglevel", "error", "-y"}
	for _, path := range paths {
		args = append(args, "-i", path)
	}
	metadataIndex := -1
	if metadataPath != "" {
		metadataIndex = len(paths)
		args = append(args, "-f", "ffmetadata", "-i", metadataPath)
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
	args = append(args, "-metadata", "title="+strings.TrimSuffix(outputName, filepath.Ext(outputName)), "-vn", "-c:a", profile.Codec)
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
				progress := 40 + int(time.Duration(micros)*time.Microsecond*46/total)
				job.update("merging", min(progress, 86), "FFmpeg 正在按顺序合并音频")
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
	job.update("merging", 86, "音频编码完成")
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
