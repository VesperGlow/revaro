package server

import (
	"context"
	"io"

	"github.com/VesperGlow/revaro/internal/storage"
)

// MediaPipeline is the single Go-side entry to the Rust media engine. It keeps
// command construction, progress parsing and cancellation in the data plane.
type MediaPipeline struct {
	engine storage.MediaEngine
	jobs   storage.MediaHLSJobEngine
}

func newMediaPipeline(store storage.Storage, _ *ResourceGovernor) *MediaPipeline {
	p := &MediaPipeline{}
	p.engine, _ = store.(storage.MediaEngine)
	p.jobs, _ = store.(storage.MediaHLSJobEngine)
	return p
}
func (p *MediaPipeline) available() error {
	if p.engine == nil {
		return appError("media_unavailable", "媒体处理服务暂不可用", nil, true)
	}
	return nil
}
func (p *MediaPipeline) Probe(ctx context.Context, key string) (storage.MediaProbe, error) {
	if err := p.available(); err != nil {
		return storage.MediaProbe{}, err
	}
	v, err := p.engine.ProbeMedia(ctx, key)
	if err != nil {
		return v, appError("media_probe_failed", "无法读取媒体信息", err, true)
	}
	return v, nil
}
func (p *MediaPipeline) Thumbnail(ctx context.Context, key string, size int) ([]byte, error) {
	if err := p.available(); err != nil {
		return nil, err
	}
	v, err := p.engine.MediaThumbnail(ctx, key, size)
	if err != nil {
		return nil, appError("thumbnail_failed", "无法生成缩略图", err, true)
	}
	return v, nil
}
func (p *MediaPipeline) AudioCover(ctx context.Context, key string, size int) ([]byte, error) {
	if err := p.available(); err != nil {
		return nil, err
	}
	v, err := p.engine.MediaAudioCover(ctx, key, size)
	if err != nil {
		return nil, appError("cover_failed", "无法提取音频封面", err, true)
	}
	return v, nil
}
func (p *MediaPipeline) StreamFMP4(ctx context.Context, key string, start float64, audio, transcode bool) (io.ReadCloser, error) {
	if err := p.available(); err != nil {
		return nil, err
	}
	v, err := p.engine.StreamFMP4(ctx, key, start, audio, transcode)
	if err != nil {
		return nil, appError("transcode_failed", "无法启动视频转换", err, true)
	}
	return v, nil
}
func (p *MediaPipeline) HLS(ctx context.Context, key, dir string, start float64, audio bool) (storage.MediaHLS, error) {
	if err := p.available(); err != nil {
		return storage.MediaHLS{}, err
	}
	v, err := p.engine.GenerateHLS(ctx, key, dir, start, audio)
	if err != nil {
		return v, appError("hls_failed", "无法生成媒体流", err, true)
	}
	return v, nil
}
func (p *MediaPipeline) HLSStatus(ctx context.Context, id string) (storage.MediaHLSJobStatus, error) {
	if p.jobs == nil {
		return storage.MediaHLSJobStatus{}, appError("media_unavailable", "媒体处理服务暂不可用", nil, true)
	}
	return p.jobs.HLSJobStatus(ctx, id)
}
func (p *MediaPipeline) CancelHLS(ctx context.Context, id string) error {
	if p.jobs == nil {
		return nil
	}
	return p.jobs.CancelHLSJob(ctx, id)
}
func (p *MediaPipeline) MergeAudio(ctx context.Context, paths, names []string, out, format, title string) (storage.MediaAudioMerge, error) {
	if err := p.available(); err != nil {
		return storage.MediaAudioMerge{}, err
	}
	v, err := p.engine.MergeAudio(ctx, paths, names, out, format, title)
	if err != nil {
		return v, appError("audio_merge_failed", "音频合并失败", err, true)
	}
	return v, nil
}
func (p *MediaPipeline) DecorateAudio(ctx context.Context, out, cover, subtitle string) error {
	if err := p.available(); err != nil {
		return err
	}
	if err := p.engine.DecorateAudio(ctx, out, cover, subtitle); err != nil {
		return appError("audio_metadata_failed", "无法写入音频封面或字幕", err, true)
	}
	return nil
}
func (p *MediaPipeline) Subtitle(ctx context.Context, key, format string, index *int) ([]byte, error) {
	if err := p.available(); err != nil {
		return nil, err
	}
	v, err := p.engine.SubtitleWebVTT(ctx, key, format, index)
	if err != nil {
		return nil, appError("subtitle_failed", "字幕转换失败", err, true)
	}
	return v, nil
}
