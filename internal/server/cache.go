package server

import (
	"github.com/VesperGlow/revaro/internal/cache"
	"github.com/VesperGlow/revaro/internal/reader"
)

// 统一 Global CacheManager 的服务端装配。所有缓存的生命周期、容量、LRU、
// singleflight、统计与失效策略统一由 internal/cache 管理；cache class 之间
// 允许不同 tier/策略，但共享全局容量并按 priority/soft quota 协调淘汰：
//
//	reader/flow-manifest  flow manifest（memory-only，高 priority，小配额）
//	reader/flow-chunk     flow chunk（memory-only，byte-LRU，受控配额）
//	reader/source   书源 blob（内容寻址 immutable，disk-only，冷启动免回源）
//	reader/books    解析后的 Book（external memory LRU，注册进全局统计）
//	media/subtitle  字幕转换产物（带 TTL 的临时产物，memory+disk）
//	media/hls       音视频 HLS 会话工作区（external：会话自管目录，
//	                经 RegisterExternal 纳入全局统计与容量回收）
//
// 缩略图与图片资产本身持久化在 S3 thumbs/ 与 blobs/（内容寻址、immutable
// 长缓存头），不经本地缓存层；音视频 Range 由 S3/数据平面直接承担。

const (
	// serverCacheMemoryBytes 是全局 memory L1 的字节上限。
	serverCacheMemoryBytes = 96 << 20
	// bookCacheBytes / bookCacheEntries 是解析 Book 内存 LRU 的容量。
	bookCacheEntries = 4
	bookCacheBytes   = 128 << 20
	// maxCachedBookSource 之上的书源 blob 不进 L2（避免占满磁盘配额）。
	maxCachedBookSource = 64 << 20
)

const (
	cacheClassReaderFlowManifest = "reader/flow-manifest"
	cacheClassReaderFlowChunk    = "reader/flow-chunk"
	cacheClassReaderSource       = "reader/source"
	cacheClassReaderBooks        = "reader/books"
	cacheClassMediaSubtitle      = "media/subtitle"
	cacheClassMediaHLS           = "media/hls"

	readerFlowManifestQuota = 8 << 20
	readerFlowChunkQuota    = 64 << 20
)

// newGlobalCache 装配统一缓存管理器：注册各 cache class 与解析书 external
// provider。HLS workspace 由 Server 在拥有 session 状态后注册。
func newGlobalCache(workDir string, diskLimit int64, books *reader.Cache) *cache.Manager {
	m := cache.New(workDir, serverCacheMemoryBytes, diskLimit)
	// flow 产物与书源 blob 已持久化在 S3；服务端只保留 manifest/chunk 的
	// memory 工作缓存，书源 blob 仍保留 disk-only 冷启动优化。
	m.RegisterClass(cache.Class{Name: cacheClassReaderFlowManifest, Priority: 90, SoftQuota: readerFlowManifestQuota, Memory: true})
	m.RegisterClass(cache.Class{Name: cacheClassReaderFlowChunk, Priority: 70, SoftQuota: readerFlowChunkQuota, Memory: true})
	m.RegisterClass(cache.Class{Name: cacheClassReaderSource, Priority: 40, SoftQuota: 512 << 20, Memory: false, Disk: true})
	m.RegisterClass(cache.Class{Name: cacheClassMediaSubtitle, Priority: 20, SoftQuota: 64 << 20, Memory: true, Disk: true})
	// 解析 Book 保留自身对象 LRU，但以 memory tier 纳入全局统计和预算。
	m.RegisterExternal(cacheClassReaderBooks, func() cache.ExternalStats {
		bytes, entries := books.Stats()
		return cache.ExternalStats{MemoryBytes: bytes, MemoryEntries: entries}
	}, func(budget cache.ExternalBudget) {
		if budget.MemoryBytes >= 0 {
			books.TrimTo(budget.MemoryBytes)
		}
	})
	return m
}
