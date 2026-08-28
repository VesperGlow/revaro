package server

import (
	"context"
	"encoding/json"
	"errors"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"time"
)

type mediaAnalysisScheduler struct {
	slots  chan struct{}
	mu     sync.Mutex
	active map[string]struct{}
}

func newMediaAnalysisScheduler(limit int) *mediaAnalysisScheduler {
	return &mediaAnalysisScheduler{slots: make(chan struct{}, limit), active: make(map[string]struct{})}
}

// schedule returns immediately. A file ID can be queued or running only once,
// and no more than two background ffprobe workers acquire a slot at a time.
func (q *mediaAnalysisScheduler) schedule(ctx context.Context, fileID string, work func(context.Context)) bool {
	q.mu.Lock()
	if _, exists := q.active[fileID]; exists {
		q.mu.Unlock()
		return false
	}
	q.active[fileID] = struct{}{}
	q.mu.Unlock()
	go func() {
		defer func() {
			q.mu.Lock()
			delete(q.active, fileID)
			q.mu.Unlock()
		}()
		select {
		case q.slots <- struct{}{}:
			defer func() { <-q.slots }()
		case <-ctx.Done():
			return
		}
		work(ctx)
	}()
	return true
}

type probedMediaMetadata struct {
	DurationMS   int64
	Container    string
	VideoCodec   string
	AudioCodec   string
	Width        int
	Height       int
	Bitrate      int64
	Chapters     []storedAudioChapter
	FrameRate    string
	VideoProfile string
	VideoLevel   int
	Subtitles    []embeddedSubtitle
}

type embeddedSubtitle struct {
	Index    int    `json:"index"`
	Codec    string `json:"codec"`
	Language string `json:"language,omitempty"`
	Title    string `json:"title,omitempty"`
	Default  bool   `json:"default"`
	Forced   bool   `json:"forced"`
}

func (s *Server) scheduleMediaAnalysis(f File) {
	if !isAudioSource(f) && !isVideoSource(f) {
		return
	}
	s.mediaAnalysis.schedule(s.audioHLSCtx, f.ID, func(parent context.Context) {
		ctx, cancel := context.WithTimeout(parent, 2*time.Minute)
		defer cancel()
		if _, err := s.ensureMediaMetadata(ctx, f); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Warn("media metadata analysis failed", "file", f.ID, "error", err)
		}
	})
}

func (s *Server) ensureMediaMetadata(ctx context.Context, f File) (probedMediaMetadata, error) {
	result := s.mediaProbeGroup.DoChan(f.ID, func() (any, error) {
		probeCtx, cancel := context.WithTimeout(s.audioHLSCtx, 2*time.Minute)
		defer cancel()
		return s.probeMediaMetadata(probeCtx, f)
	})
	select {
	case <-ctx.Done():
		return probedMediaMetadata{}, ctx.Err()
	case call := <-result:
		if call.Err != nil {
			return probedMediaMetadata{}, call.Err
		}
		return call.Val.(probedMediaMetadata), nil
	}
}

func (s *Server) probeMediaMetadata(ctx context.Context, f File) (probedMediaMetadata, error) {
	var metadata probedMediaMetadata
	var chaptersJSON, subtitlesJSON string
	err := s.db.QueryRowContext(ctx, `SELECT duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,frame_rate,video_profile,video_level,subtitles_json FROM media_metadata WHERE file_id=? AND source_etag=?`, f.ID, f.ETag).
		Scan(&metadata.DurationMS, &metadata.Container, &metadata.VideoCodec, &metadata.AudioCodec, &metadata.Width, &metadata.Height, &metadata.Bitrate, &chaptersJSON, &metadata.FrameRate, &metadata.VideoProfile, &metadata.VideoLevel, &subtitlesJSON)
	if err == nil {
		_ = json.Unmarshal([]byte(chaptersJSON), &metadata.Chapters)
		_ = json.Unmarshal([]byte(subtitlesJSON), &metadata.Subtitles)
		return metadata, nil
	}
	ffprobe, err := ffprobeFor(s.cfg.FFmpegPath)
	if err != nil {
		return metadata, err
	}
	sourceURL, cleanup, err := s.startMediaHLSSource(ctx, f)
	if err != nil {
		return metadata, err
	}
	defer cleanup()
	cmd := exec.CommandContext(ctx, ffprobe, "-v", "error", "-show_entries",
		"format=duration,format_name,bit_rate:stream=index,codec_type,codec_name,width,height,avg_frame_rate,profile,level:stream_tags=language,title:stream_disposition=default,forced:chapter=start_time,end_time:chapter_tags=title", "-of", "json", sourceURL)
	out := &limitedBuffer{limit: 2 << 20}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stdout, cmd.Stderr = out, stderr
	if err := cmd.Run(); err != nil {
		return metadata, mediaCommandError("ffprobe media metadata", err, ctx.Err(), stderr.String())
	}
	var result struct {
		Format struct {
			Duration string `json:"duration"`
			Name     string `json:"format_name"`
			Bitrate  string `json:"bit_rate"`
		} `json:"format"`
		Streams []struct {
			Index     int    `json:"index"`
			Type      string `json:"codec_type"`
			Codec     string `json:"codec_name"`
			Width     int    `json:"width"`
			Height    int    `json:"height"`
			FrameRate string `json:"avg_frame_rate"`
			Profile   string `json:"profile"`
			Level     int    `json:"level"`
			Tags      struct {
				Language string `json:"language"`
				Title    string `json:"title"`
			} `json:"tags"`
			Disposition struct {
				Default int `json:"default"`
				Forced  int `json:"forced"`
			} `json:"disposition"`
		} `json:"streams"`
		Chapters []struct {
			Start string `json:"start_time"`
			End   string `json:"end_time"`
			Tags  struct {
				Title string `json:"title"`
			} `json:"tags"`
		} `json:"chapters"`
	}
	if err := json.Unmarshal(out.buf.Bytes(), &result); err != nil {
		return metadata, err
	}
	duration, _ := strconv.ParseFloat(result.Format.Duration, 64)
	metadata.DurationMS = max(int64(duration*1000), 0)
	metadata.Container = strings.ToLower(result.Format.Name)
	metadata.Bitrate, _ = strconv.ParseInt(result.Format.Bitrate, 10, 64)
	for _, stream := range result.Streams {
		switch stream.Type {
		case "video":
			if metadata.VideoCodec == "" {
				metadata.VideoCodec, metadata.Width, metadata.Height = strings.ToLower(stream.Codec), stream.Width, stream.Height
				metadata.FrameRate, metadata.VideoProfile, metadata.VideoLevel = stream.FrameRate, stream.Profile, stream.Level
			}
		case "audio":
			if metadata.AudioCodec == "" {
				metadata.AudioCodec = strings.ToLower(stream.Codec)
			}
		case "subtitle":
			metadata.Subtitles = append(metadata.Subtitles, embeddedSubtitle{Index: stream.Index, Codec: strings.ToLower(stream.Codec), Language: stream.Tags.Language, Title: stream.Tags.Title, Default: stream.Disposition.Default != 0, Forced: stream.Disposition.Forced != 0})
		}
	}
	for index, chapter := range result.Chapters {
		start, _ := strconv.ParseFloat(chapter.Start, 64)
		end, _ := strconv.ParseFloat(chapter.End, 64)
		title := strings.TrimSpace(chapter.Tags.Title)
		if title == "" {
			title = "Chapter " + strconv.Itoa(index+1)
		}
		metadata.Chapters = append(metadata.Chapters, storedAudioChapter{Title: title, StartMS: max(int64(start*1000), 0), EndMS: max(int64(end*1000), 0)})
	}
	chapters, _ := json.Marshal(metadata.Chapters)
	subtitles, _ := json.Marshal(metadata.Subtitles)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(ctx, `INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET duration_ms=excluded.duration_ms,container=excluded.container,video_codec=excluded.video_codec,audio_codec=excluded.audio_codec,width=excluded.width,height=excluded.height,bitrate=excluded.bitrate,chapters_json=excluded.chapters_json,analyzed_at=excluded.analyzed_at,frame_rate=excluded.frame_rate,video_profile=excluded.video_profile,video_level=excluded.video_level,subtitles_json=excluded.subtitles_json,source_etag=excluded.source_etag`,
		f.ID, metadata.DurationMS, metadata.Container, metadata.VideoCodec, metadata.AudioCodec, metadata.Width, metadata.Height, max(metadata.Bitrate, int64(0)), string(chapters), now, metadata.FrameRate, metadata.VideoProfile, metadata.VideoLevel, string(subtitles), f.ETag)
	if err == nil {
		s.log.Info("media metadata analyzed", "file", f.ID, "container", metadata.Container, "video_codec", metadata.VideoCodec, "audio_codec", metadata.AudioCodec, "duration_ms", metadata.DurationMS, "chapters", len(metadata.Chapters))
	}
	return metadata, err
}
