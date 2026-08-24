package server

import (
	"context"
	"encoding/binary"
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
	"github.com/go-chi/chi/v5"
)

const (
	videoFMP4IdleTTL     = 10 * time.Minute
	videoFMP4StartWait   = 45 * time.Second
	maxVideoFMP4Sessions = 4
)

type videoFMP4Session struct {
	ID               string
	FileID           string
	Start            float64
	RequestedStart   float64
	Duration         float64
	Dir              string
	Path             string
	MIMEType         string
	VideoContentType string
	AudioContentType string
	VideoCodec       string
	AudioCodec       string
	Width            int
	Height           int
	Bitrate          int64
	FrameRate        float64

	mu          sync.RWMutex
	lastAccess  time.Time
	done        bool
	err         string
	cancel      context.CancelFunc
	doneCh      chan struct{}
	doneOnce    sync.Once
	destroyOnce sync.Once
}

func (session *videoFMP4Session) touch() {
	session.mu.Lock()
	session.lastAccess = time.Now()
	session.mu.Unlock()
}

func (session *videoFMP4Session) snapshot() (time.Time, bool, string) {
	session.mu.RLock()
	defer session.mu.RUnlock()
	return session.lastAccess, session.done, session.err
}

func (session *videoFMP4Session) finish(err error) {
	session.mu.Lock()
	session.done = true
	if err != nil && !errors.Is(err, context.Canceled) {
		session.err = err.Error()
	}
	session.mu.Unlock()
	session.doneOnce.Do(func() { close(session.doneCh) })
}

func (session *videoFMP4Session) destroy() {
	session.destroyOnce.Do(func() {
		if session.cancel != nil {
			session.cancel()
		}
		select {
		case <-session.doneCh:
		case <-time.After(3 * time.Second):
		}
		_ = os.RemoveAll(session.Dir)
	})
}

type startVideoFMP4Request struct {
	Start float64 `json:"start"`
}

type startVideoFMP4Response struct {
	SessionID       string  `json:"session_id"`
	StreamURL       string  `json:"stream_url"`
	Start           float64 `json:"start"`
	RequestedStart  float64 `json:"requested_start"`
	Duration        float64 `json:"duration"`
	MIMEType        string  `json:"mime_type"`
	VideoMIMEType   string  `json:"video_mime_type"`
	AudioMIMEType   string  `json:"audio_mime_type,omitempty"`
	VideoCodec      string  `json:"video_codec"`
	AudioCodec      string  `json:"audio_codec,omitempty"`
	Width           int     `json:"width"`
	Height          int     `json:"height"`
	Bitrate         int64   `json:"bitrate"`
	FrameRate       float64 `json:"frame_rate"`
}

type fmp4Probe struct {
	Streams []struct {
		CodecType string `json:"codec_type"`
		CodecName string `json:"codec_name"`
		Profile   string `json:"profile"`
		Level     int    `json:"level"`
		Width     int    `json:"width"`
		Height    int    `json:"height"`
		FrameRate string `json:"avg_frame_rate"`
		Bitrate   string `json:"bit_rate"`
		Channels   int    `json:"channels"`
		SampleRate string `json:"sample_rate"`
	} `json:"streams"`
	Format struct {
		Duration string `json:"duration"`
		Bitrate  string `json:"bit_rate"`
	} `json:"format"`
}

type fmp4MediaInfo struct {
	duration, start, frameRate float64
	videoCodec, audioCodec     string
	mimeType                   string
	videoContentType           string
	audioContentType           string
	width, height              int
	bitrate                    int64
}

func (s *Server) startVideoFMP4(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isVideoSource(f) {
		problem(w, http.StatusNotFound, "ready video file not found")
		return
	}
	var in startVideoFMP4Request
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Start < 0 || in.Start > 7*24*60*60 {
		problem(w, http.StatusBadRequest, "invalid fMP4 start time")
		return
	}
	if _, err := exec.LookPath(s.cfg.FFmpegPath); err != nil {
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusServiceUnavailable, "ffmpeg is unavailable")
		return
	}
	if _, err := ffprobeFor(s.cfg.FFmpegPath); err != nil {
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusServiceUnavailable, "ffprobe is unavailable")
		return
	}
	select {
	case s.videoFMP4Slots <- struct{}{}:
	default:
		s.log.Warn("fMP4 creation deferred", "file", f.ID, "reason", "too many remux sessions")
		problem(w, http.StatusTooManyRequests, "too many fMP4 streams are active")
		return
	}

	dir, err := os.MkdirTemp("", "revaro-video-fmp4-")
	if err != nil {
		<-s.videoFMP4Slots
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusInternalServerError, "could not create fMP4 stream")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	sourceURL, closeSource, err := s.startMediaHLSSource(ctx, f)
	if err != nil {
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusBadGateway, "could not open video for fMP4 remux")
		return
	}
	info, err := probeFMP4Source(ctx, s.cfg.FFmpegPath, sourceURL, in.Start)
	if err != nil {
		closeSource()
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "video codecs cannot be remuxed to browser fMP4")
		return
	}
	if info.start >= info.duration {
		closeSource()
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		problem(w, http.StatusBadRequest, "fMP4 start time is beyond the video duration")
		return
	}
	session := &videoFMP4Session{
		ID: ids.New(), FileID: f.ID, Start: info.start, RequestedStart: in.Start, Duration: info.duration,
		Dir: dir, Path: filepath.Join(dir, "stream.mp4"), MIMEType: info.mimeType,
		VideoContentType: info.videoContentType, AudioContentType: info.audioContentType,
		VideoCodec: info.videoCodec, AudioCodec: info.audioCodec, Width: info.width, Height: info.height,
		Bitrate: info.bitrate, FrameRate: info.frameRate, lastAccess: time.Now(), cancel: cancel, doneCh: make(chan struct{}),
	}
	s.videoFMP4Mu.Lock()
	s.videoFMP4Sessions[session.ID] = session
	s.videoFMP4Mu.Unlock()
	s.pruneVideoFMP4Sessions(session.ID)
	go s.runVideoFMP4(ctx, f, sourceURL, closeSource, session)
	if err := waitForVideoFMP4(r.Context(), session); err != nil {
		s.removeVideoFMP4Session(session.ID)
		if errors.Is(err, context.DeadlineExceeded) {
			problem(w, http.StatusGatewayTimeout, "fMP4 stream took too long to start")
		} else {
			s.log.Warn("fMP4 creation failed", "file", f.ID, "session", session.ID, "error", err)
			problem(w, http.StatusBadGateway, "fMP4 stream failed to start")
		}
		return
	}
	s.log.Info("video playback selected", "file", f.ID, "mode", "mse-fmp4", "video_codec", info.videoCodec, "audio_codec", info.audioCodec, "start", info.start)
	writeJSON(w, http.StatusCreated, startVideoFMP4Response{
		SessionID: session.ID, StreamURL: "/api/video/fmp4/" + session.ID + "/stream.mp4",
		Start: session.Start, RequestedStart: session.RequestedStart, Duration: session.Duration,
		MIMEType: session.MIMEType, VideoMIMEType: session.VideoContentType, AudioMIMEType: session.AudioContentType,
		VideoCodec: session.VideoCodec, AudioCodec: session.AudioCodec, Width: session.Width, Height: session.Height,
		Bitrate: session.Bitrate, FrameRate: session.FrameRate,
	})
}

func probeFMP4Source(ctx context.Context, ffmpeg, sourceURL string, requestedStart float64) (fmp4MediaInfo, error) {
	ffprobe, err := ffprobeFor(ffmpeg)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	cmd := exec.CommandContext(ctx, ffprobe, "-v", "error", "-show_entries",
		"format=duration,bit_rate:stream=codec_type,codec_name,profile,level,width,height,avg_frame_rate,bit_rate,channels,sample_rate",
		"-of", "json", sourceURL)
	output := &limitedBuffer{limit: 2 << 20}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stdout, cmd.Stderr = output, stderr
	if err := cmd.Run(); err != nil {
		return fmp4MediaInfo{}, mediaCommandError("ffprobe fMP4 metadata", err, ctx.Err(), stderr.String())
	}
	var probe fmp4Probe
	if err := json.Unmarshal([]byte(output.String()), &probe); err != nil {
		return fmp4MediaInfo{}, fmt.Errorf("decode ffprobe fMP4 metadata: %w", err)
	}
	info := fmp4MediaInfo{start: requestedStart}
	info.duration, _ = strconv.ParseFloat(probe.Format.Duration, 64)
	info.bitrate, _ = strconv.ParseInt(probe.Format.Bitrate, 10, 64)
	var videoProfile string
	var videoLevel int
	for _, stream := range probe.Streams {
		switch stream.CodecType {
		case "video":
			if info.videoCodec == "" {
				info.videoCodec, videoProfile, videoLevel = stream.CodecName, stream.Profile, stream.Level
				info.width, info.height = stream.Width, stream.Height
				info.frameRate = parseFMP4FrameRate(stream.FrameRate)
				if bitrate, parseErr := strconv.ParseInt(stream.Bitrate, 10, 64); parseErr == nil && bitrate > 0 {
					info.bitrate = bitrate
				}
			}
		case "audio":
			if info.audioCodec == "" {
				info.audioCodec = stream.CodecName
			}
		}
	}
	if info.duration <= 0 || info.videoCodec == "" {
		return fmp4MediaInfo{}, errors.New("video duration or codec is unavailable")
	}
	videoCodecString, err := fmp4VideoCodecString(info.videoCodec, videoProfile, videoLevel)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	audioCodecString, err := fmp4AudioCodecString(info.audioCodec)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	info.videoContentType = `video/mp4; codecs="` + videoCodecString + `"`
	codecList := videoCodecString
	if audioCodecString != "" {
		info.audioContentType = `audio/mp4; codecs="` + audioCodecString + `"`
		codecList += ", " + audioCodecString
	}
	info.mimeType = `video/mp4; codecs="` + codecList + `"`
	if info.bitrate <= 0 {
		info.bitrate = 8_000_000
	}
	if info.frameRate <= 0 {
		info.frameRate = 30
	}
	if requestedStart > 0 {
		keyframe, keyframeErr := probeFMP4Keyframe(ctx, ffprobe, sourceURL, requestedStart)
		if keyframeErr == nil && keyframe >= 0 && keyframe <= requestedStart+.25 {
			info.start = keyframe
		}
	}
	info.start = float64(int64(max(0, info.start)*1000)) / 1000
	return info, nil
}

func probeFMP4Keyframe(ctx context.Context, ffprobe, sourceURL string, target float64) (float64, error) {
	interval := strconv.FormatFloat(target, 'f', 3, 64) + "%+#1"
	cmd := exec.CommandContext(ctx, ffprobe, "-v", "error", "-read_intervals", interval, "-select_streams", "v:0",
		"-show_entries", "packet=pts_time", "-of", "default=noprint_wrappers=1:nokey=1", sourceURL)
	output, err := cmd.Output()
	if err != nil {
		return 0, err
	}
	line := strings.TrimSpace(strings.SplitN(string(output), "\n", 2)[0])
	value, err := strconv.ParseFloat(line, 64)
	if err != nil {
		return 0, err
	}
	return value, nil
}

func parseFMP4FrameRate(value string) float64 {
	parts := strings.Split(value, "/")
	if len(parts) == 2 {
		numerator, numeratorErr := strconv.ParseFloat(parts[0], 64)
		denominator, denominatorErr := strconv.ParseFloat(parts[1], 64)
		if numeratorErr == nil && denominatorErr == nil && denominator > 0 {
			return numerator / denominator
		}
	}
	result, _ := strconv.ParseFloat(value, 64)
	return result
}

func fmp4VideoCodecString(codec, profile string, level int) (string, error) {
	switch strings.ToLower(codec) {
	case "hevc", "h265":
		if level <= 0 {
			level = 120
		}
		if strings.Contains(strings.ToLower(profile), "10") {
			return fmt.Sprintf("hvc1.2.4.L%d.B0", level), nil
		}
		return fmt.Sprintf("hvc1.1.6.L%d.B0", level), nil
	case "h264", "avc":
		if level <= 0 {
			level = 40
		}
		prefix := "6400"
		lowerProfile := strings.ToLower(profile)
		if strings.Contains(lowerProfile, "baseline") {
			prefix = "42E0"
		} else if strings.Contains(lowerProfile, "main") {
			prefix = "4D40"
		}
		return fmt.Sprintf("avc1.%s%02X", prefix, level), nil
	default:
		return "", fmt.Errorf("video codec %q is not supported by the fMP4 remux path", codec)
	}
}

func fmp4AudioCodecString(codec string) (string, error) {
	switch strings.ToLower(codec) {
	case "":
		return "", nil
	case "aac":
		return "mp4a.40.2", nil
	case "eac3":
		return "ec-3", nil
	case "ac3":
		return "ac-3", nil
	case "mp3":
		return "mp4a.6B", nil
	default:
		return "", fmt.Errorf("audio codec %q is not supported by the fMP4 remux path", codec)
	}
}

func (s *Server) runVideoFMP4(ctx context.Context, f File, sourceURL string, closeSource func(), session *videoFMP4Session) {
	defer func() { <-s.videoFMP4Slots }()
	defer closeSource()
	args := []string{"-hide_banner", "-loglevel", "error", "-y"}
	if session.RequestedStart > 0 {
		// Seek with the requested timestamp so FFmpeg selects the same preceding
		// keyframe that probeFMP4Keyframe reported. The response exposes that
		// keyframe as Start, allowing the browser to seek within the remuxed
		// timeline without losing subtitle alignment.
		args = append(args, "-ss", strconv.FormatFloat(session.RequestedStart, 'f', 3, 64))
	}
	args = append(args, "-i", sourceURL, "-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn", "-c:v", "copy", "-c:a", "copy")
	if session.VideoCodec == "hevc" || session.VideoCodec == "h265" {
		args = append(args, "-tag:v", "hvc1")
	} else if session.VideoCodec == "h264" || session.VideoCodec == "avc" {
		args = append(args, "-tag:v", "avc1")
	}
	args = append(args,
		"-avoid_negative_ts", "make_zero", "-max_muxing_queue_size", "2048",
		// delay_moov still emits an MSE initialization segment before the first
		// moof, but lets FFmpeg inspect the first audio packets first. EAC3 needs
		// that inspection to populate dec3; empty_moov would fail before remuxing.
		"-movflags", "+frag_keyframe+delay_moov+default_base_moof+omit_tfhd_offset+disable_chpl",
		"-frag_duration", "2000000", "-f", "mp4", session.Path,
	)
	cmd := exec.CommandContext(ctx, s.cfg.FFmpegPath, args...)
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	err := cmd.Run()
	if err != nil {
		if ctx.Err() != nil {
			err = ctx.Err()
		} else {
			err = mediaCommandError("ffmpeg fMP4 remux", err, nil, stderr.String())
		}
	}
	if err != nil && !errors.Is(err, context.Canceled) {
		s.log.Warn("fMP4 remux stopped", "file", f.ID, "session", session.ID, "error", err)
	}
	session.finish(err)
}

func waitForVideoFMP4(requestCtx context.Context, session *videoFMP4Session) error {
	timer := time.NewTimer(videoFMP4StartWait)
	defer timer.Stop()
	ticker := time.NewTicker(75 * time.Millisecond)
	defer ticker.Stop()
	for {
		initReady, fragmentReady := fmp4FileState(session.Path)
		if initReady && fragmentReady {
			return nil
		}
		_, done, sessionErr := session.snapshot()
		if done {
			if sessionErr == "" {
				return errors.New("ffmpeg produced no complete fMP4 fragment")
			}
			return errors.New(sessionErr)
		}
		select {
		case <-requestCtx.Done():
			return requestCtx.Err()
		case <-timer.C:
			return context.DeadlineExceeded
		case <-ticker.C:
		}
	}
}

func fmp4FileState(path string) (bool, bool) {
	file, err := os.Open(path)
	if err != nil {
		return false, false
	}
	defer file.Close()
	stat, err := file.Stat()
	if err != nil {
		return false, false
	}
	size := stat.Size()
	var offset int64
	initReady, fragmentReady := false, false
	header := make([]byte, 16)
	for offset+8 <= size {
		if _, err := file.ReadAt(header[:8], offset); err != nil {
			break
		}
		boxSize := int64(binary.BigEndian.Uint32(header[:4]))
		headerSize := int64(8)
		if boxSize == 1 {
			if offset+16 > size {
				break
			}
			if _, err := file.ReadAt(header, offset); err != nil {
				break
			}
			boxSize, headerSize = int64(binary.BigEndian.Uint64(header[8:16])), 16
		}
		if boxSize == 0 || boxSize < headerSize || offset+boxSize > size {
			break
		}
		boxType := string(header[4:8])
		if boxType == "moov" {
			initReady = true
		}
		if initReady && boxType == "mdat" {
			fragmentReady = true
		}
		offset += boxSize
	}
	return initReady, fragmentReady
}

func (s *Server) videoFMP4Stream(w http.ResponseWriter, r *http.Request) {
	session := s.videoFMP4Session(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "fMP4 stream not found")
		return
	}
	session.touch()
	file, err := os.Open(session.Path)
	if err != nil {
		problem(w, http.StatusNotFound, "fMP4 stream is not ready")
		return
	}
	defer file.Close()
	w.Header().Set("Content-Type", session.MIMEType)
	w.Header().Set("Cache-Control", "private, no-store")
	w.Header().Set("Accept-Ranges", "bytes")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	_, done, _ := session.snapshot()
	if r.Header.Get("Range") != "" || done {
		if done {
			http.ServeContent(w, r, "stream.mp4", time.Time{}, file)
			return
		}
		s.serveGrowingFMP4Range(w, r, session, file)
		return
	}
	flusher, _ := w.(http.Flusher)
	buffer := make([]byte, 256<<10)
	for {
		n, readErr := file.Read(buffer)
		if n > 0 {
			if _, err := w.Write(buffer[:n]); err != nil {
				return
			}
			if flusher != nil {
				flusher.Flush()
			}
			session.touch()
		}
		if readErr != nil && !errors.Is(readErr, io.EOF) {
			return
		}
		if !errors.Is(readErr, io.EOF) {
			continue
		}
		_, finished, _ := session.snapshot()
		if finished {
			return
		}
		select {
		case <-r.Context().Done():
			return
		case <-session.doneCh:
		case <-time.After(50 * time.Millisecond):
		}
	}
}

func (s *Server) serveGrowingFMP4Range(w http.ResponseWriter, r *http.Request, session *videoFMP4Session, file *os.File) {
	rangeValue := strings.TrimSpace(strings.TrimPrefix(r.Header.Get("Range"), "bytes="))
	parts := strings.SplitN(rangeValue, "-", 2)
	if len(parts) != 2 || parts[0] == "" {
		problem(w, http.StatusRequestedRangeNotSatisfiable, "unsupported fMP4 byte range")
		return
	}
	start, err := strconv.ParseInt(parts[0], 10, 64)
	if err != nil || start < 0 {
		problem(w, http.StatusRequestedRangeNotSatisfiable, "invalid fMP4 byte range")
		return
	}
	deadline := time.NewTimer(30 * time.Second)
	defer deadline.Stop()
	var size int64
	for {
		if stat, statErr := file.Stat(); statErr == nil {
			size = stat.Size()
		}
		_, done, _ := session.snapshot()
		if start < size || done {
			break
		}
		select {
		case <-r.Context().Done():
			return
		case <-deadline.C:
			problem(w, http.StatusRequestedRangeNotSatisfiable, "fMP4 byte range is not ready")
			return
		case <-time.After(50 * time.Millisecond):
		}
	}
	if start >= size {
		w.Header().Set("Content-Range", "bytes */"+strconv.FormatInt(size, 10))
		problem(w, http.StatusRequestedRangeNotSatisfiable, "fMP4 byte range is outside the stream")
		return
	}
	end := size - 1
	if parts[1] != "" {
		requestedEnd, parseErr := strconv.ParseInt(parts[1], 10, 64)
		if parseErr != nil || requestedEnd < start {
			problem(w, http.StatusRequestedRangeNotSatisfiable, "invalid fMP4 byte range")
			return
		}
		end = min(end, requestedEnd)
	}
	_, done, _ := session.snapshot()
	total := "*"
	if done {
		total = strconv.FormatInt(size, 10)
	}
	length := end - start + 1
	w.Header().Set("Content-Range", fmt.Sprintf("bytes %d-%d/%s", start, end, total))
	w.Header().Set("Content-Length", strconv.FormatInt(length, 10))
	w.WriteHeader(http.StatusPartialContent)
	_, _ = file.Seek(start, io.SeekStart)
	_, _ = io.CopyN(w, file, length)
}

func (s *Server) stopVideoFMP4(w http.ResponseWriter, r *http.Request) {
	if s.removeVideoFMP4Session(chi.URLParam(r, "session")) == nil {
		problem(w, http.StatusNotFound, "fMP4 stream not found")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) videoFMP4Session(id string) *videoFMP4Session {
	s.videoFMP4Mu.RLock()
	defer s.videoFMP4Mu.RUnlock()
	return s.videoFMP4Sessions[id]
}

func (s *Server) removeVideoFMP4Session(id string) *videoFMP4Session {
	s.videoFMP4Mu.Lock()
	session := s.videoFMP4Sessions[id]
	delete(s.videoFMP4Sessions, id)
	s.videoFMP4Mu.Unlock()
	if session != nil {
		session.destroy()
	}
	return session
}

func (s *Server) pruneVideoFMP4Sessions(keepID string) {
	type cachedSession struct {
		id         string
		lastAccess time.Time
	}
	s.videoFMP4Mu.RLock()
	items := make([]cachedSession, 0, len(s.videoFMP4Sessions))
	for id, session := range s.videoFMP4Sessions {
		lastAccess, _, _ := session.snapshot()
		items = append(items, cachedSession{id: id, lastAccess: lastAccess})
	}
	s.videoFMP4Mu.RUnlock()
	for len(items) > maxVideoFMP4Sessions {
		oldest := -1
		for index := range items {
			if items[index].id == keepID {
				continue
			}
			if oldest < 0 || items[index].lastAccess.Before(items[oldest].lastAccess) {
				oldest = index
			}
		}
		if oldest < 0 {
			break
		}
		s.removeVideoFMP4Session(items[oldest].id)
		items = append(items[:oldest], items[oldest+1:]...)
	}
}

func (s *Server) cleanupVideoFMP4Sessions() {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-s.audioHLSCtx.Done():
			return
		case now := <-ticker.C:
			var expired []string
			s.videoFMP4Mu.RLock()
			for id, session := range s.videoFMP4Sessions {
				lastAccess, _, _ := session.snapshot()
				if now.Sub(lastAccess) > videoFMP4IdleTTL {
					expired = append(expired, id)
				}
			}
			s.videoFMP4Mu.RUnlock()
			for _, id := range expired {
				s.removeVideoFMP4Session(id)
			}
		}
	}
}
