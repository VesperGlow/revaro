package server

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

const (
	videoFMP4IdleTTL     = 5 * time.Minute
	maxVideoFMP4Sessions = 4
	fmp4AudioCopy        = "copy"
	fmp4AudioAAC         = "aac"
)

// videoFMP4Session is only a cancellable description of one stdout stream.
// It owns no fragment files, window cache, playlist, index poller, or prewarm
// worker. A seek creates a new session and cancels the previous one.
type videoFMP4Session struct {
	ID               string
	FileID           string
	RequestedStart   float64
	Duration         float64
	MIMEType         string
	VideoContentType string
	AudioContentType string
	VideoCodec       string
	AudioCodec       string
	OutputAudioCodec string
	AudioMode        string
	AudioTranscoding bool
	Width            int
	Height           int
	Bitrate          int64
	FrameRate        float64
	File             File

	mu           sync.Mutex
	lastAccess   time.Time
	active       bool
	streamCancel context.CancelFunc
	ctx          context.Context
	cancel       context.CancelFunc
	destroyOnce  sync.Once
}

func (session *videoFMP4Session) snapshot() time.Time {
	session.mu.Lock()
	defer session.mu.Unlock()
	return session.lastAccess
}

func (session *videoFMP4Session) beginStream(cancel context.CancelFunc) bool {
	session.mu.Lock()
	defer session.mu.Unlock()
	if session.active || session.ctx.Err() != nil {
		return false
	}
	session.active = true
	session.streamCancel = cancel
	session.lastAccess = time.Now()
	return true
}

func (session *videoFMP4Session) finishStream() {
	session.mu.Lock()
	session.active = false
	session.streamCancel = nil
	session.lastAccess = time.Now()
	session.mu.Unlock()
}

func (session *videoFMP4Session) destroy() {
	session.destroyOnce.Do(func() {
		session.mu.Lock()
		streamCancel := session.streamCancel
		session.mu.Unlock()
		if streamCancel != nil {
			streamCancel()
		}
		if session.cancel != nil {
			session.cancel()
		}
	})
}

type startVideoFMP4Request struct {
	Start             float64 `json:"start"`
	AudioMode         string  `json:"audio_mode"`
	PreviousSessionID string  `json:"previous_session_id"`
	FreshSession      bool    `json:"fresh_session"`
	FallbackReason    string  `json:"fallback_reason"`
}

type videoFMP4MetadataResponse struct {
	Duration         float64 `json:"duration"`
	MIMEType         string  `json:"mime_type"`
	AACMIMEType      string  `json:"aac_mime_type"`
	VideoMIMEType    string  `json:"video_mime_type"`
	AudioMIMEType    string  `json:"audio_mime_type,omitempty"`
	AACAudioMIMEType string  `json:"aac_audio_mime_type,omitempty"`
	VideoCodec       string  `json:"video_codec"`
	AudioCodec       string  `json:"audio_codec,omitempty"`
	Width            int     `json:"width"`
	Height           int     `json:"height"`
	Bitrate          int64   `json:"bitrate"`
	FrameRate        float64 `json:"frame_rate"`
}

type startVideoFMP4Response struct {
	videoFMP4MetadataResponse
	SessionID        string  `json:"session_id"`
	StreamURL        string  `json:"stream_url"`
	Start            float64 `json:"start"`
	RequestedStart   float64 `json:"requested_start"`
	OutputAudioCodec string  `json:"output_audio_codec,omitempty"`
	AudioTranscoding bool    `json:"audio_transcoding"`
	SelectedMode     string  `json:"selected_mode"`
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
	} `json:"streams"`
	Format struct {
		Duration string `json:"duration"`
		Bitrate  string `json:"bit_rate"`
	} `json:"format"`
}

type fmp4MediaInfo struct {
	duration, frameRate                float64
	videoCodec, audioCodec             string
	videoCodecString, audioCodecString string
	mimeType, aacMIMEType              string
	videoContentType, audioContentType string
	aacAudioContentType                string
	width, height                      int
	bitrate                            int64
}

func (s *Server) videoFMP4Metadata(w http.ResponseWriter, r *http.Request) {
	f, ok := s.fmp4File(w, r)
	if !ok {
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 60*time.Second)
	defer cancel()
	info, err := s.probeFMP4File(ctx, f)
	if err != nil {
		s.log.Warn("fMP4 probe failed", "file", f.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "video codec cannot be remuxed to browser fMP4")
		return
	}
	writeJSON(w, http.StatusOK, fmp4MetadataResponse(info))
}

func (s *Server) startVideoFMP4(w http.ResponseWriter, r *http.Request) {
	f, ok := s.fmp4File(w, r)
	if !ok {
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
	if in.AudioMode == "" {
		in.AudioMode = fmp4AudioCopy
	}
	if in.AudioMode != fmp4AudioCopy && in.AudioMode != fmp4AudioAAC {
		problem(w, http.StatusBadRequest, "invalid fMP4 audio mode")
		return
	}
	// A seek/retry is a handoff, never a cache lookup. Cancel the old HTTP
	// stream and its FFmpeg/source Reader before probing and starting the new one.
	if in.PreviousSessionID != "" {
		previous := s.videoFMP4Session(in.PreviousSessionID)
		if previous != nil && previous.FileID == f.ID {
			s.removeVideoFMP4Session(in.PreviousSessionID)
		}
	}
	probeCtx, probeCancel := context.WithTimeout(r.Context(), 60*time.Second)
	defer probeCancel()
	info, err := s.probeFMP4File(probeCtx, f)
	if err != nil {
		s.log.Warn("fMP4 creation probe failed", "file", f.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "video codec cannot be remuxed to browser fMP4")
		return
	}
	if in.Start >= info.duration {
		problem(w, http.StatusBadRequest, "fMP4 start time is beyond the video duration")
		return
	}
	if in.AudioMode == fmp4AudioCopy && info.audioCodec != "" && info.audioCodecString == "" {
		problem(w, http.StatusUnprocessableEntity, "source audio cannot be copied into browser fMP4")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	mimeType, audioContentType, outputAudioCodec := fmp4OutputTypes(info, in.AudioMode)
	session := &videoFMP4Session{
		ID: ids.New(), FileID: f.ID, RequestedStart: in.Start, Duration: info.duration,
		MIMEType: mimeType, VideoContentType: info.videoContentType, AudioContentType: audioContentType,
		VideoCodec: info.videoCodec, AudioCodec: info.audioCodec, OutputAudioCodec: outputAudioCodec,
		AudioMode: in.AudioMode, AudioTranscoding: in.AudioMode == fmp4AudioAAC && info.audioCodec != "",
		Width: info.width, Height: info.height, Bitrate: info.bitrate, FrameRate: info.frameRate,
		File: f, lastAccess: time.Now(), ctx: ctx, cancel: cancel,
	}
	s.videoFMP4Mu.Lock()
	s.videoFMP4Sessions[session.ID] = session
	s.videoFMP4Mu.Unlock()
	s.pruneVideoFMP4Sessions(session.ID)
	s.logVideoFMP4Selection(f.ID, session, in.FallbackReason, in.FreshSession)
	writeJSON(w, http.StatusCreated, videoFMP4Response(session))
}

func (s *Server) fmp4File(w http.ResponseWriter, r *http.Request) (File, bool) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isVideoSource(f) {
		problem(w, http.StatusNotFound, "ready video file not found")
		return File{}, false
	}
	if _, ok := s.storage.(storage.MediaEngine); !ok {
		problem(w, http.StatusServiceUnavailable, "media engine is unavailable")
		return File{}, false
	}
	return f, true
}

func (s *Server) probeFMP4File(ctx context.Context, f File) (fmp4MediaInfo, error) {
	metadata, err := s.ensureMediaMetadata(ctx, f)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	info := fmp4MediaInfo{duration: float64(metadata.DurationMS) / 1000, videoCodec: metadata.VideoCodec, audioCodec: metadata.AudioCodec, width: metadata.Width, height: metadata.Height, bitrate: metadata.Bitrate, frameRate: parseFMP4FrameRate(metadata.FrameRate)}
	if info.duration <= 0 || info.videoCodec == "" {
		return fmp4MediaInfo{}, errors.New("video duration or codec is unavailable")
	}
	info.videoCodecString, err = fmp4VideoCodecString(info.videoCodec, metadata.VideoProfile, metadata.VideoLevel)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	info.audioCodecString, _ = fmp4AudioCodecString(info.audioCodec)
	info.videoContentType = `video/mp4; codecs="` + info.videoCodecString + `"`
	info.aacAudioContentType = `audio/mp4; codecs="mp4a.40.2"`
	info.mimeType = info.videoContentType
	if info.audioCodecString != "" {
		info.audioContentType = `audio/mp4; codecs="` + info.audioCodecString + `"`
		info.mimeType = `video/mp4; codecs="` + info.videoCodecString + `, ` + info.audioCodecString + `"`
	}
	info.aacMIMEType = `video/mp4; codecs="` + info.videoCodecString + `, mp4a.40.2"`
	if info.audioCodec == "" {
		info.aacMIMEType = info.mimeType
	}
	if info.bitrate <= 0 {
		info.bitrate = 8_000_000
	}
	if info.frameRate <= 0 {
		info.frameRate = 30
	}
	return info, nil
}

func (s *Server) logVideoFMP4Selection(fileID string, session *videoFMP4Session, fallbackReason string, fresh bool) {
	selectedMode := "mse-copy"
	if session.AudioTranscoding {
		selectedMode = "mse-copy-video-aac-audio"
	}
	s.log.Info("video playback selected", "file", fileID, "video_codec", session.VideoCodec,
		"audio_codec", session.AudioCodec, "selected_mode", selectedMode, "transport", "data-plane-http-mse",
		"video_transcoding", false, "audio_transcoding", session.AudioTranscoding,
		"fallback_reason", strings.TrimSpace(fallbackReason), "session", session.ID,
		"requested_start", session.RequestedStart, "fresh_recovery", fresh)
}

func fmp4MetadataResponse(info fmp4MediaInfo) videoFMP4MetadataResponse {
	return videoFMP4MetadataResponse{
		Duration: info.duration, MIMEType: info.mimeType, AACMIMEType: info.aacMIMEType,
		VideoMIMEType: info.videoContentType, AudioMIMEType: info.audioContentType,
		AACAudioMIMEType: info.aacAudioContentType, VideoCodec: info.videoCodec, AudioCodec: info.audioCodec,
		Width: info.width, Height: info.height, Bitrate: info.bitrate, FrameRate: info.frameRate,
	}
}

func videoFMP4Response(session *videoFMP4Session) startVideoFMP4Response {
	selectedMode := "mse-copy"
	if session.AudioTranscoding {
		selectedMode = "mse-copy-video-aac-audio"
	}
	metadata := videoFMP4MetadataResponse{
		Duration: session.Duration, MIMEType: session.MIMEType, AACMIMEType: session.MIMEType,
		VideoMIMEType: session.VideoContentType, AudioMIMEType: session.AudioContentType,
		AACAudioMIMEType: `audio/mp4; codecs="mp4a.40.2"`, VideoCodec: session.VideoCodec,
		AudioCodec: session.AudioCodec, Width: session.Width, Height: session.Height,
		Bitrate: session.Bitrate, FrameRate: session.FrameRate,
	}
	return startVideoFMP4Response{
		videoFMP4MetadataResponse: metadata, SessionID: session.ID,
		StreamURL: "/api/video/fmp4/" + session.ID + "/stream", Start: 0,
		RequestedStart: session.RequestedStart, OutputAudioCodec: session.OutputAudioCodec,
		AudioTranscoding: session.AudioTranscoding, SelectedMode: selectedMode,
	}
}

func fmp4OutputTypes(info fmp4MediaInfo, audioMode string) (string, string, string) {
	if info.audioCodec == "" {
		return info.videoContentType, "", ""
	}
	if audioMode == fmp4AudioAAC {
		return info.aacMIMEType, info.aacAudioContentType, "aac"
	}
	return info.mimeType, info.audioContentType, info.audioCodec
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
		return "", fmt.Errorf("audio codec %q cannot be copied into browser fMP4", codec)
	}
}

func (s *Server) streamVideoFMP4(w http.ResponseWriter, r *http.Request) {
	session := s.videoFMP4Session(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "fMP4 stream session not found")
		return
	}
	streamCtx, streamCancel := context.WithCancel(r.Context())
	defer streamCancel()
	stopSessionHook := context.AfterFunc(session.ctx, streamCancel)
	defer stopSessionHook()
	if !session.beginStream(streamCancel) {
		problem(w, http.StatusConflict, "fMP4 stream session is already active or closed")
		return
	}
	defer session.finishStream()

	select {
	case s.videoFMP4Slots <- struct{}{}:
		defer func() { <-s.videoFMP4Slots }()
	case <-streamCtx.Done():
		return
	case <-time.After(5 * time.Second):
		problem(w, http.StatusTooManyRequests, "another fMP4 stream is already active")
		return
	}
	engine := s.storage.(storage.MediaEngine)
	stdout, err := engine.StreamFMP4(streamCtx, session.File.objectKey, session.RequestedStart, session.AudioCodec != "", session.AudioTranscoding)
	if err != nil {
		problem(w, http.StatusBadGateway, "could not start fMP4 stream")
		return
	}
	defer stdout.Close()
	buffer := make([]byte, 128<<10)
	firstN, firstErr := stdout.Read(buffer)
	if firstN == 0 {
		if streamCtx.Err() == nil {
			s.log.Warn("fMP4 stdout ended before headers", "file", session.FileID, "session", session.ID,
				"error", firstErr)
			problem(w, http.StatusBadGateway, "fMP4 stream produced no data")
		}
		return
	}
	w.Header().Set("Content-Type", session.MIMEType)
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("X-Accel-Buffering", "no")
	w.Header().Set("Content-Disposition", "inline")
	w.WriteHeader(http.StatusOK)
	written, writeErr := w.Write(buffer[:firstN])
	bytesSent := int64(written)
	if writeErr == nil && firstErr == nil {
		n, copyErr := io.CopyBuffer(w, stdout, buffer)
		bytesSent += n
		writeErr = copyErr
	}
	if writeErr != nil {
		// Do not wait for a producer whose consumer is already gone. Killing
		// FFmpeg also closes its local Range request and the underlying Reader.
		streamCancel()
	}
	if streamCtx.Err() != nil || writeErr != nil {
		s.log.Info("fMP4 stdout stream cancelled", "file", session.FileID, "session", session.ID,
			"requested_start", session.RequestedStart, "bytes", bytesSent, "request_error", writeErr)
		return
	}
	if firstErr != nil && !errors.Is(firstErr, io.EOF) {
		s.log.Warn("fMP4 stdout stream failed", "file", session.FileID, "session", session.ID,
			"requested_start", session.RequestedStart, "bytes", bytesSent,
			"error", firstErr)
		return
	}
	s.log.Info("fMP4 stdout stream completed", "file", session.FileID, "session", session.ID,
		"requested_start", session.RequestedStart, "bytes", bytesSent)
}

func (s *Server) stopVideoFMP4(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "session")
	if s.videoFMP4Session(id) == nil {
		problem(w, http.StatusNotFound, "fMP4 stream session not found")
		return
	}
	s.removeVideoFMP4Session(id)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) videoFMP4Session(id string) *videoFMP4Session {
	s.videoFMP4Mu.RLock()
	defer s.videoFMP4Mu.RUnlock()
	return s.videoFMP4Sessions[id]
}

func (s *Server) removeVideoFMP4Session(id string) {
	if id == "" {
		return
	}
	s.videoFMP4Mu.Lock()
	session := s.videoFMP4Sessions[id]
	delete(s.videoFMP4Sessions, id)
	s.videoFMP4Mu.Unlock()
	if session != nil {
		session.destroy()
	}
}

func (s *Server) pruneVideoFMP4Sessions(keepID string) {
	type item struct {
		id   string
		when time.Time
	}
	s.videoFMP4Mu.RLock()
	items := make([]item, 0, len(s.videoFMP4Sessions))
	for id, session := range s.videoFMP4Sessions {
		if id != keepID {
			items = append(items, item{id: id, when: session.snapshot()})
		}
	}
	s.videoFMP4Mu.RUnlock()
	for len(items)+1 > maxVideoFMP4Sessions {
		oldest := 0
		for i := 1; i < len(items); i++ {
			if items[i].when.Before(items[oldest].when) {
				oldest = i
			}
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
				if now.Sub(session.snapshot()) > videoFMP4IdleTTL {
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
