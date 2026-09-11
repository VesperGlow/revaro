package server

import (
	"context"

	"github.com/VesperGlow/revaro/internal/storage"
)

// MediaPipeline is the single Go-side entry to the Rust media engine. It keeps
// command construction, progress parsing and cancellation in the data plane.
type MediaPipeline struct {
	engine storage.MediaEngine
}

func newMediaPipeline(store storage.Storage, _ *ResourceGovernor) *MediaPipeline {
	p := &MediaPipeline{}
	p.engine, _ = store.(storage.MediaEngine)
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
