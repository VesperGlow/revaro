package server

import (
	"github.com/VesperGlow/revaro/internal/cache"
	"github.com/VesperGlow/revaro/internal/reader"
)

// 统一 Global CacheManager 的服务端装配。所有缓存的生命周期、容量、LRU、
// singleflight、统计与失效策略统一由 internal/cache 管理；cache class 之间
// 允许不同 tier/策略，但共享全局容量并按 priority/soft quota 协调淘汰：
//
//	reader/flow     flow manifest/chunk（内容寻址 immutable，memory+disk）
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
	cacheClassReaderFlow    = "reader/flow"
	cacheClassReaderSource  = "reader/source"
	cacheClassReaderBooks   = "reader/books"
	cacheClassMediaSubtitle = "media/subtitle"
	cacheClassMediaHLS      = "media/hls"
)

// newGlobalCache 装配统一缓存管理器：注册各 cache class 与 external
// 提供者（解析书 LRU、HLS 会话工作区）。
func newGlobalCache(workDir string, diskLimit int64, books *reader.Cache) *cache.Manager {
	m := cache.New(workDir, serverCacheMemoryBytes, diskLimit)
	// flow 产物与书源 blob 由内容哈希寻址、immutable：不设 TTL，依赖
	// 内容/版本键 + 容量 LRU。
	m.RegisterClass(cache.Class{Name: cacheClassReaderFlow, Priority: 50, SoftQuota: 512 << 20, Memory: true, Disk: true})
	m.RegisterClass(cache.Class{Name: cacheClassReaderSource, Priority: 40, SoftQuota: 512 << 20, Memory: false, Disk: true})
	m.RegisterClass(cache.Class{Name: cacheClassMediaSubtitle, Priority: 20, SoftQuota: 64 << 20, Memory: true, Disk: true})
	// 解析 Book 与 HLS 会话工作区保留自身策略（对象 LRU / 会话生命周期），
	// 但纳入全局统计与容量协调。
	m.RegisterExternal(cacheClassReaderBooks, books.Stats, books.Trim)
	m.RegisterExternal(cacheClassMediaHLS, nil, nil)
	return m
}
