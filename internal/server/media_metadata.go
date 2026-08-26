package server

import (
	"context"
	"encoding/json"
	"errors"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

type probedMediaMetadata struct {
	DurationMS int64
	Container  string
	VideoCodec string
	AudioCodec string
	Width      int
	Height     int
	Bitrate    int64
	Chapters   []storedAudioChapter
}

func (s *Server) scheduleMediaAnalysis(f File) {
	if !isAudioSource(f) && !isVideoSource(f) {
		return
	}
	go func() {
		ctx, cancel := context.WithTimeout(s.audioHLSCtx, 2*time.Minute)
		defer cancel()
		if _, err := s.ensureMediaMetadata(ctx, f); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Warn("media metadata analysis failed", "file", f.ID, "error", err)
		}
	}()
}

func (s *Server) ensureMediaMetadata(ctx context.Context, f File) (probedMediaMetadata, error) {
	var metadata probedMediaMetadata
	var chaptersJSON string
	err := s.db.QueryRowContext(ctx, `SELECT duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json FROM media_metadata WHERE file_id=?`, f.ID).
		Scan(&metadata.DurationMS, &metadata.Container, &metadata.VideoCodec, &metadata.AudioCodec, &metadata.Width, &metadata.Height, &metadata.Bitrate, &chaptersJSON)
	if err == nil {
		_ = json.Unmarshal([]byte(chaptersJSON), &metadata.Chapters)
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
		"format=duration,format_name,bit_rate:stream=codec_type,codec_name,width,height:chapter=start_time,end_time:chapter_tags=title", "-of", "json", sourceURL)
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
			Type   string `json:"codec_type"`
			Codec  string `json:"codec_name"`
			Width  int    `json:"width"`
			Height int    `json:"height"`
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
			}
		case "audio":
			if metadata.AudioCodec == "" {
				metadata.AudioCodec = strings.ToLower(stream.Codec)
			}
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
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(ctx, `INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET duration_ms=excluded.duration_ms,container=excluded.container,video_codec=excluded.video_codec,audio_codec=excluded.audio_codec,width=excluded.width,height=excluded.height,bitrate=excluded.bitrate,chapters_json=excluded.chapters_json,analyzed_at=excluded.analyzed_at`,
		f.ID, metadata.DurationMS, metadata.Container, metadata.VideoCodec, metadata.AudioCodec, metadata.Width, metadata.Height, max(metadata.Bitrate, int64(0)), string(chapters), now)
	if err == nil {
		s.log.Info("media metadata analyzed", "file", f.ID, "container", metadata.Container, "video_codec", metadata.VideoCodec, "audio_codec", metadata.AudioCodec, "duration_ms", metadata.DurationMS, "chapters", len(metadata.Chapters))
	}
	return metadata, err
}
