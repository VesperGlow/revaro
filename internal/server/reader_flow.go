package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sort"
	"strconv"
	"time"

	"github.com/VesperGlow/revaro/internal/reader/flow"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

// 连续 reading flow 的 HTTP 面：
//   - GET /api/files/{id}/book/flow           → flow manifest（chunk 清单、
//     spine 块区间、目录目标；内容随书内容与 flow 版本固定，manifest 不缓存）
//   - GET /api/files/{id}/book/flow/chunks/{n} → 第 n 个 chunk 的 HTML 片段
//     （不可变、长缓存；块带 data-block 全局编号）
//
// flow 由解析缓存里的 Book 生成（纯函数、确定性），产物写对象存储
// flows/{bookObjectKey}/f{version}/，作为内容级缓存（幂等：manifest 命中
// 不重建；缺失才单飞构建 + 幂等覆盖；manifest 最后原子提交）。读取路径经
// 统一缓存管理器 reader/flow class：memory L1 + local-disk L2，再回源 S3
//（内容寻址 immutable，无 TTL）；GC 按孤儿/容量回收，删除书后产物随 GC
// 清理。

const maxFlowObject = 8 << 20

// flowCacheKey 返回 flow 产物在统一缓存里的逻辑键：直接采用对象存储键
//（自带 bookObjectKey 与 f{version} 目录，版本/内容变化时键自然失效）。
func flowCacheKey(objectKey string) string {
	return "manifest/" + objectKey + "/" + flow.VersionDir()
}

func flowChunkCacheKey(objectKey string, index int) string {
	return "chunk/" + objectKey + "/" + flow.VersionDir() + "/" + strconv.Itoa(index)
}

// ensureFlow 保证 flow 产物已生成。产物内容随书内容与 flow 版本固定：
// manifest 已存在（含统一缓存 L1/L2 副本）时直接复用，不再解析原书、
// 不再写对象；只在 manifest 缺失（首次打开、版本升级或 GC 回收后）时
// 单飞构建。manifest 最后写：读者永远看不到缺 chunk 的 manifest。
func (s *Server) ensureFlow(ctx context.Context, f File) error {
	if s.cache.Has(cacheClassReaderFlow, flowCacheKey(f.objectKey)) {
		return nil
	}
	if _, err := s.objects.Stat(ctx, flow.ManifestObjectKey(f.objectKey)); err == nil {
		return nil
	}
	_, err, _ := s.flowBuilds.Do(f.objectKey, func() (any, error) {
		// 双检：并发请求可能已完成构建
		if _, err := s.objects.Stat(ctx, flow.ManifestObjectKey(f.objectKey)); err == nil {
			return nil, nil
		}
		return nil, s.storeFlow(ctx, f)
	})
	return err
}

// rebuildFlow 强制重建 flow 产物：用于 manifest 存在但 chunk 对象已被
// 容量回收的恢复路径。
func (s *Server) rebuildFlow(ctx context.Context, f File) error {
	_, err, _ := s.flowBuilds.Do(f.objectKey, func() (any, error) {
		return nil, s.storeFlow(ctx, f)
	})
	return err
}

func (s *Server) storeFlow(ctx context.Context, f File) error {
	book, err := s.loadBook(ctx, f)
	if err != nil {
		return fmt.Errorf("解析书籍失败：%w", err)
	}
	built, err := flow.Build(book, fmt.Sprintf("/api/files/%s/book/assets", f.ID))
	if err != nil {
		return err
	}
	// book_key 是书 blob 的内容指纹：客户端持久缓存用它隔离 chunk 键。
	built.Manifest.BookKey = flow.BookFingerprint(f.objectKey)
	for _, ch := range built.Chunks {
		if _, err := s.objects.Put(ctx, flow.ChunkObjectKey(f.objectKey, ch.Meta.Index), "text/html; charset=utf-8", []byte(ch.HTML)); err != nil {
			return fmt.Errorf("写入 chunk %d 失败：%w", ch.Meta.Index, err)
		}
	}
	raw, err := json.Marshal(built.Manifest)
	if err != nil {
		return fmt.Errorf("序列化 flow manifest 失败：%w", err)
	}
	if _, err := s.objects.Put(ctx, flow.ManifestObjectKey(f.objectKey), "application/json", raw); err != nil {
		return fmt.Errorf("写入 flow manifest 失败：%w", err)
	}
	return nil
}

// flowManifestData 读取 manifest 内容：统一缓存 L1/L2 命中免回源 S3。
func (s *Server) flowManifestData(ctx context.Context, f File) ([]byte, error) {
	return s.cache.Load(ctx, cacheClassReaderFlow, flowCacheKey(f.objectKey), 0, func(ctx context.Context) ([]byte, error) {
		return s.objects.Get(ctx, flow.ManifestObjectKey(f.objectKey), maxFlowObject)
	})
}

// flowChunkData 读取一个 chunk：统一缓存 L1/L2 → S3。manifest 存在但
// chunk 对象已被容量回收时，强制重建一次再读（自愈）。
func (s *Server) flowChunkData(ctx context.Context, f File, index int) ([]byte, error) {
	load := func(ctx context.Context) ([]byte, error) {
		return s.objects.Get(ctx, flow.ChunkObjectKey(f.objectKey, index), maxFlowObject)
	}
	data, err := s.cache.Load(ctx, cacheClassReaderFlow, flowChunkCacheKey(f.objectKey, index), 0, load)
	if err == nil || !storage.IsNotFound(err) {
		return data, err
	}
	if err := s.rebuildFlow(ctx, f); err != nil {
		return nil, err
	}
	return s.cache.Load(ctx, cacheClassReaderFlow, flowChunkCacheKey(f.objectKey, index), 0, load)
}

// bookFlow 返回 flow manifest。manifest 体积小且语义由服务端 flow 版本决定，
// 用 no-cache（浏览器每次打开都重新取，保证与当前二进制一致；服务端侧则
// 经统一缓存，immutable 内容寻址）。
func (s *Server) bookFlow(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	if err := s.ensureFlow(r.Context(), f); err != nil {
		problem(w, http.StatusUnprocessableEntity, "无法生成阅读流："+err.Error())
		return
	}
	data, err := s.flowManifestData(r.Context(), f)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read flow manifest")
		return
	}
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Cache-Control", "private, no-cache")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

// bookFlowChunk 返回一个 chunk 的 HTML 片段。chunk 内容只随书内容与 flow
// 版本变化（manifest 总是先于 chunk 请求到达），因此可 immutable 长缓存。
func (s *Server) bookFlowChunk(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	index, err := strconv.Atoi(chi.URLParam(r, "index"))
	if err != nil || index < 0 || index > 1<<22 {
		problem(w, http.StatusBadRequest, "chunk index is invalid")
		return
	}
	if err := s.ensureFlow(r.Context(), f); err != nil {
		problem(w, http.StatusUnprocessableEntity, "无法生成阅读流："+err.Error())
		return
	}
	data, err := s.flowChunkData(r.Context(), f, index)
	if err != nil {
		problem(w, http.StatusNotFound, "chunk not found")
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

// ---- flow 缓存 GC ----

// collectFlowCache 回收 flows/ 前缀：已删除书的产物立即清理；其余按
// TTL 与容量上限淘汰最旧对象。返回删除的对象数。
func (s *Server) collectFlowCache(ctx context.Context, referenced map[string]bool) int {
	type candidate struct {
		key  string
		size int64
		mod  time.Time
	}
	var candidates []candidate
	var survivors int64
	deleted := 0
	cutoff := time.Now().UTC().Add(-s.cfg.FlowCacheTTL)

	err := s.objects.WalkPrefix(ctx, "flows/", func(objects []storage.ObjectRef) error {
		for _, object := range objects {
			blobKey := flow.BookKeyFromObject(object.Key)
			if blobKey == "" {
				continue
			}
			if !referenced[blobKey] {
				// 书已删除：整棵 flow 目录都成孤儿
				if s.objects.DeleteMany(ctx, []string{object.Key}, "flow orphan cleanup") == nil {
					deleted++
				}
				continue
			}
			if s.cfg.FlowCacheTTL > 0 && object.LastModified.Before(cutoff) {
				if s.objects.DeleteMany(ctx, []string{object.Key}, "flow ttl cleanup") == nil {
					deleted++
				}
				continue
			}
			candidates = append(candidates, candidate{key: object.Key, size: object.Size, mod: object.LastModified})
			survivors += object.Size
		}
		return ctx.Err()
	})
	if err != nil && ctx.Err() == nil {
		s.log.Warn("flow cache walk failed", "error", err)
		return deleted
	}
	// 容量上限：从最旧开始淘汰，直到总量收敛到上限以内（0 表示不限）。
	if s.cfg.FlowCacheCapacity > 0 {
		sort.Slice(candidates, func(i, j int) bool { return candidates[i].mod.Before(candidates[j].mod) })
		for _, c := range candidates {
			if survivors <= s.cfg.FlowCacheCapacity {
				break
			}
			if s.objects.DeleteMany(ctx, []string{c.key}, "flow capacity cleanup") == nil {
				deleted++
				survivors -= c.size
			}
		}
	}
	return deleted
}
