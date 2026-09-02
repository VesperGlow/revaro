package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/reader"
	"github.com/VesperGlow/revaro/internal/reader/layout"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
	"golang.org/x/net/html"
)

// 服务端固定分页的 HTTP 面（渐进式）：
//   - GET  /api/files/{id}/book/layouts/capabilities  → 引擎可用性
//   - POST /api/files/{id}/book/layouts                → 提交生成（幂等；带
//     start_anchor 时优先生成该位置所在章，之后向两侧螺旋展开）
//   - GET  /api/files/{id}/book/layouts                → 已生成 profile 列表
//   - GET  /api/files/{id}/book/layouts/{profile}      → 任务状态（phase/进度）
//   - GET  /api/files/{id}/book/layouts/{profile}/manifest → manifest 快照
//     （渐进式生成期间每完成一章发布一个新快照；complete=false 表示未完）
//   - GET  /api/files/{id}/book/layouts/{profile}/spines/{spine}/pages/{col}
//   - GET  /api/reader.css 与 /api/reader/fonts/*      → 共享样式/WebFont
//
// 产物写对象存储 layouts/{bookObjectKey}/{profileID}/；页面对象按 (spine,
// col) 寻址、一经写入永久稳定；全局页码只在 manifest 快照里按前缀和装配。
// 快照由服务端内存作业状态重建（进程重启后按需重新提交即可，旧产物由
// GC 回收）。

type readerLayoutJob struct {
	FileID      string
	ProfileID   string
	Profile     layout.Profile
	StartAnchor *layout.Anchor
	Status      string // queued | running | done | error
	Phase       string // window | background
	Error       string
	Pages       int // 已生成页数
	PageCount   int // 最新快照页数（partial 时小于最终值）
	Complete    bool
	SpinesDone  int
	SpinesTotal int
	CreatedAt   time.Time
	FinishedAt  time.Time

	results    map[int]*spineResult // spine -> 分页结果（内存态，快照重建用）
	doneSpines map[int]bool
	tocSpine   []int // 目录条目 -> 章序号
	gen        int64 // supersede 版本号
}

type spinePageRec struct {
	start layout.Anchor
	end   layout.Anchor
	bytes int64
}

type spineResult struct {
	cols  int
	pages []spinePageRec
	toc   map[int]int // 目录条目序号 -> 章内栏号
}

type layoutProfileInfo struct {
	ProfileID   string `json:"profile_id"`
	PageCount   int    `json:"page_count"`
	GeneratedAt string `json:"generated_at"`
}

func (s *Server) layoutJobKey(fileID, profileID string) string { return fileID + "/" + profileID }

// spineOrder 返回渐进分页顺序：目标章优先（当前位置立即可读），
// 之后向两侧螺旋展开（阅读方向附近优先，索引稳定性由快照装配保证）。
func spineOrder(total, target int) []int {
	if total <= 0 {
		return nil
	}
	if target < 0 {
		target = 0
	}
	if target >= total {
		target = total - 1
	}
	order := make([]int, 0, total)
	seen := make([]bool, total)
	order = append(order, target)
	seen[target] = true
	for step := 1; len(order) < total; step++ {
		if a := target + step; a < total && !seen[a] {
			seen[a] = true
			order = append(order, a)
		}
		if b := target - step; b >= 0 && !seen[b] {
			seen[b] = true
			order = append(order, b)
		}
	}
	return order
}

// validProfileID 放行空值与本实现产出的 "v{n}-{hex64}" 形态。
func validProfileID(id string) bool {
	if id == "" {
		return true
	}
	if len(id) > 128 {
		return false
	}
	head, hexPart, ok := strings.Cut(id, "-")
	if !ok || len(head) < 2 || head[0] != 'v' || len(hexPart) != 64 {
		return false
	}
	for i := 1; i < len(head); i++ {
		if head[i] < '0' || head[i] > '9' {
			return false
		}
	}
	for i := 0; i < len(hexPart); i++ {
		c := hexPart[i]
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

// layoutScheduler 惰性创建共享 Chromium 调度器（进程内单实例复用）。
func (s *Server) layoutScheduler() *layout.Scheduler {
	s.layoutMu.Lock()
	defer s.layoutMu.Unlock()
	if s.layoutSched == nil {
		s.layoutSched = layout.NewScheduler(s.cfg.ChromeBinary)
	}
	return s.layoutSched
}

// readerLayoutCapabilities 报告布局引擎可用性。
func (s *Server) readerLayoutCapabilities(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.readerFile(w, r); !ok {
		return
	}
	info := layout.DetectEngine()
	writeJSON(w, http.StatusOK, map[string]any{
		"available":    info.Available,
		"engine":       info.Engine,
		"version":      info.Version,
		"reason":       info.Reason,
		"experimental": false,
	})
}

// createReaderLayout 提交一次分页生成。幂等：同 profile 已在生成/已完成时
// 直接返回现状；新提交会让更早的生成任务在下个章边界中止（最新优先）。
func (s *Server) createReaderLayout(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	var in struct {
		Profile     layout.Profile `json:"profile"`
		StartAnchor *layout.Anchor `json:"start_anchor"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if err := in.Profile.Validate(); err != nil {
		problem(w, http.StatusBadRequest, "无效的阅读排版配置："+err.Error())
		return
	}
	if in.StartAnchor != nil && !in.StartAnchor.Valid() {
		problem(w, http.StatusBadRequest, "start anchor is invalid")
		return
	}
	sched := s.layoutScheduler()
	if !sched.Available() {
		problem(w, http.StatusServiceUnavailable, "分页引擎不可用："+layout.DetectEngine().Reason)
		return
	}
	profileID := in.Profile.ID(f.objectKey)
	key := s.layoutJobKey(f.ID, profileID)

	s.layoutMu.Lock()
	if job, exists := s.layoutJobs[key]; exists && (job.Status == "queued" || job.Status == "running" || job.Status == "done") {
		s.layoutMu.Unlock()
		s.writeLayoutJobStatus(w, job)
		return
	}
	s.layoutMu.Unlock()

	// 重启后内存索引丢失：对象存储里有完整 manifest 即视为完成。
	if data, err := s.objects.Get(r.Context(), layout.ManifestObjectKey(f.objectKey, profileID), 64<<20); err == nil {
		var m layout.Manifest
		if json.Unmarshal(data, &m) == nil && m.Complete {
			writeJSON(w, http.StatusOK, map[string]any{
				"profile_id": profileID,
				"status":     "done",
				"complete":   true,
				"manifest":   "/api/files/" + f.ID + "/book/layouts/" + profileID + "/manifest",
			})
			return
		}
	}

	gen := s.layoutGen.Add(1) // 最新优先：旧任务在下一个章边界中止
	job := &readerLayoutJob{
		FileID:      f.ID,
		ProfileID:   profileID,
		Profile:     in.Profile,
		StartAnchor: in.StartAnchor,
		Status:      "queued",
		Phase:       "window",
		CreatedAt:   time.Now().UTC(),
		results:     make(map[int]*spineResult),
		doneSpines:  make(map[int]bool),
		gen:         gen,
	}
	s.layoutMu.Lock()
	s.layoutJobs[key] = job
	s.layoutMu.Unlock()

	if !s.runBackground(func() { s.generateReaderLayout(job, f) }) {
		s.layoutMu.Lock()
		job.Status = "error"
		job.Error = "服务正在关闭"
		s.layoutMu.Unlock()
	}
	s.writeLayoutJobStatus(w, job)
}

func (s *Server) writeLayoutJobStatus(w http.ResponseWriter, job *readerLayoutJob) {
	s.layoutMu.RLock()
	defer s.layoutMu.RUnlock()
	writeJSON(w, http.StatusOK, map[string]any{
		"profile_id":   job.ProfileID,
		"status":       job.Status,
		"phase":        job.Phase,
		"error":        job.Error,
		"pages":        job.Pages,
		"page_count":   job.PageCount,
		"complete":     job.Complete,
		"spines_done":  job.SpinesDone,
		"spines_total": job.SpinesTotal,
		"manifest":     "/api/files/" + job.FileID + "/book/layouts/" + job.ProfileID + "/manifest",
	})
}

func (s *Server) generateReaderLayout(job *readerLayoutJob, f File) {
	started := time.Now()
	gen := job.gen
	ctx, cancel := context.WithTimeout(s.layoutCtx, 30*time.Minute)
	defer cancel()

	fail := func(err error) {
		s.layoutMu.Lock()
		job.Status = "error"
		job.Error = err.Error()
		job.FinishedAt = time.Now().UTC()
		s.layoutMu.Unlock()
		s.log.Error("reader layout generation failed", "file", f.ID, "profile", job.ProfileID, "error", err)
	}
	superseded := func() bool {
		return s.layoutGen.Load() != gen || ctx.Err() != nil
	}

	s.layoutMu.Lock()
	job.Status = "running"
	job.Phase = "window"
	s.layoutMu.Unlock()

	book, err := s.loadBook(ctx, f)
	if err != nil {
		fail(fmt.Errorf("解析书籍失败：%w", err))
		return
	}
	var chapters []layout.ChapterInput
	txt := book.Format == "txt"
	tocSpine := make([]int, len(book.TOC))
	if txt {
		offsets := make([]int64, 0, len(book.TOC))
		for _, entry := range book.TOC {
			offsets = append(offsets, entry.Offset)
		}
		chunks, tocChunk := layout.TXTChaptersWithTOC(book.Text, offsets)
		for i, chunk := range chunks {
			chapters = append(chapters, layout.ChapterInput{Spine: i, HTML: chunk})
		}
		copy(tocSpine, tocChunk)
	} else {
		for i, ch := range book.Chapters {
			chapters = append(chapters, layout.ChapterInput{Spine: i, HTML: ch.HTML})
		}
		spineByPath := make(map[string]int, len(book.Chapters))
		for i, ch := range book.Chapters {
			if p := chapterSourcePath(ch.HTML); p != "" {
				spineByPath[p] = i
			}
		}
		for i, entry := range book.TOC {
			spine, ok := spineByPath[entry.Path]
			if !ok {
				spine = 0
			}
			tocSpine[i] = spine
		}
	}

	target := 0
	if job.StartAnchor != nil && job.StartAnchor.Spine >= 0 && job.StartAnchor.Spine < len(chapters) {
		target = job.StartAnchor.Spine
	}
	s.layoutMu.Lock()
	job.SpinesTotal = len(chapters)
	job.tocSpine = tocSpine
	s.layoutMu.Unlock()

	pager := layout.NewPager(s.cfg.ChromeBinary, fmt.Sprintf("/api/files/%s/book/assets", f.ID), book.Assets)
	sched := s.layoutScheduler()
	if !sched.Available() {
		fail(fmt.Errorf("分页引擎不可用：%s", layout.DetectEngine().Reason))
		return
	}

	for _, spine := range spineOrder(len(chapters), target) {
		if superseded() {
			s.layoutMu.Lock()
			job.Status = "error"
			job.Error = "superseded by a newer layout request"
			job.FinishedAt = time.Now().UTC()
			s.layoutMu.Unlock()
			return
		}
		var targets []layout.TOCTarget
		for i, sp := range tocSpine {
			if sp != spine {
				continue
			}
			if txt {
				targets = append(targets, layout.TOCTarget{Index: i, Kind: "toc-anchor", Spine: spine})
			} else {
				targets = append(targets, layout.TOCTarget{Index: i, Kind: "fragment", Spine: spine, Fragment: book.TOC[i].Fragment})
			}
		}
		res, err := pager.PaginateSpine(ctx, sched, chapters[spine], job.Profile, txt, targets)
		if err != nil {
			if superseded() {
				return
			}
			fail(fmt.Errorf("第 %d 章分页失败：%w", spine, err))
			return
		}
		// 写页面对象（按章寻址，一经写入永久稳定）
		for col, pg := range res.Pages {
			key := layout.SpinePageObjectKey(f.objectKey, job.ProfileID, spine, col)
			if _, err := s.objects.Put(ctx, key, "text/html; charset=utf-8", []byte(pg.HTML)); err != nil {
				fail(fmt.Errorf("写入页面对象失败：%w", err))
				return
			}
			s.layoutPages.Add(1)
			s.layoutBytes.Add(int64(len(pg.HTML)))
		}
		result := &spineResult{cols: res.Cols, pages: make([]spinePageRec, 0, len(res.Pages)), toc: make(map[int]int)}
		for _, pg := range res.Pages {
			result.pages = append(result.pages, spinePageRec{start: pg.Start, end: pg.End, bytes: int64(len(pg.HTML))})
		}
		for i := range targets {
			if i < len(res.TOCPages) {
				result.toc[targets[i].Index] = res.TOCPages[i]
			}
		}
		s.layoutMu.Lock()
		job.results[spine] = result
		job.doneSpines[spine] = true
		job.SpinesDone++
		job.Pages += len(res.Pages)
		if job.Phase == "window" {
			job.Phase = "background"
		}
		s.layoutMu.Unlock()

		manifest := s.assembleReaderManifest(job, f, book)
		s.layoutMu.Lock()
		job.PageCount = manifest.PageCount
		job.Complete = manifest.Complete
		s.layoutMu.Unlock()
		if err := s.publishReaderManifest(ctx, f, job.ProfileID, manifest); err != nil {
			fail(err)
			return
		}
		for _, d := range res.Diagnostics {
			s.log.Warn("reader layout diagnostic", "file", f.ID, "profile", job.ProfileID, "type", d.Type, "spine", d.Spine, "col", d.Col)
		}
	}

	s.layoutMu.Lock()
	job.Status = "done"
	job.Complete = true
	job.FinishedAt = time.Now().UTC()
	s.layoutIndex[f.ID] = append(s.layoutIndex[f.ID], layoutProfileInfo{
		ProfileID: job.ProfileID, PageCount: job.PageCount, GeneratedAt: time.Now().UTC().Format(time.RFC3339),
	})
	s.layoutMu.Unlock()
	s.log.Info("reader layout generated",
		"file", f.ID, "profile", job.ProfileID, "pages", job.PageCount,
		"spines", job.SpinesTotal, "seconds", time.Since(started).Seconds(),
	)
}

// assembleReaderManifest 从作业的章级结果重建 manifest 快照：页数组按
// spine 顺序装配（前缀和 = 全局页码），未生成章跳过；Complete 表示全书
// 生成完毕。快照之间锚点单调性保持（已生成前缀固定）。
func (s *Server) assembleReaderManifest(job *readerLayoutJob, f File, book *reader.Book) *layout.Manifest {
	s.layoutMu.RLock()
	defer s.layoutMu.RUnlock()
	m := &layout.Manifest{
		Version:     1,
		BookHash:    f.objectKey,
		ProfileID:   job.ProfileID,
		Profile:     job.Profile.Normalized(),
		Complete:    len(job.doneSpines) >= job.SpinesTotal,
		GeneratedAt: time.Now().UTC().Format(time.RFC3339),
	}
	base := 0
	for spine := 0; spine < job.SpinesTotal; spine++ {
		r, ok := job.results[spine]
		if !ok {
			continue
		}
		for col, p := range r.pages {
			m.Pages = append(m.Pages, layout.PageMeta{
				Index: base + col,
				Spine: spine,
				Start: p.start,
				End:   p.end,
				URL:   fmt.Sprintf("/api/files/%s/book/layouts/%s/spines/%d/pages/%d", f.ID, job.ProfileID, spine, col),
				Bytes: p.bytes,
			})
		}
		for i := range book.TOC {
			if job.tocSpine[i] != spine {
				continue
			}
			if localCol, ok := r.toc[i]; ok {
				entry := book.TOC[i]
				m.TOC = append(m.TOC, layout.TOCMeta{Label: entry.Label, Page: base + localCol, Depth: entry.Depth, Spine: spine, Fragment: entry.Fragment})
			}
		}
		base += r.cols
	}
	m.PageCount = len(m.Pages)
	return m
}

func (s *Server) publishReaderManifest(ctx context.Context, f File, profileID string, m *layout.Manifest) error {
	raw, err := json.Marshal(m)
	if err != nil {
		return fmt.Errorf("序列化 manifest 失败：%w", err)
	}
	if _, err := s.objects.Put(ctx, layout.ManifestObjectKey(f.objectKey, profileID), "application/json", raw); err != nil {
		return fmt.Errorf("写入 manifest 失败：%w", err)
	}
	return nil
}

func (s *Server) listReaderLayouts(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	s.layoutMu.RLock()
	infos := append([]layoutProfileInfo(nil), s.layoutIndex[f.ID]...)
	s.layoutMu.RUnlock()
	writeJSON(w, http.StatusOK, map[string]any{"profiles": infos})
}

func (s *Server) readerLayoutStatus(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	profileID := chi.URLParam(r, "profile")
	if !validProfileID(profileID) || profileID == "" {
		problem(w, http.StatusBadRequest, "profile id is invalid")
		return
	}
	s.layoutMu.RLock()
	job, exists := s.layoutJobs[s.layoutJobKey(f.ID, profileID)]
	s.layoutMu.RUnlock()
	if !exists {
		if data, err := s.objects.Get(r.Context(), layout.ManifestObjectKey(f.objectKey, profileID), 64<<20); err == nil {
			var m layout.Manifest
			if json.Unmarshal(data, &m) == nil && m.Complete {
				writeJSON(w, http.StatusOK, map[string]any{
					"profile_id": profileID,
					"status":     "done",
					"complete":   true,
					"page_count": m.PageCount,
					"manifest":   "/api/files/" + f.ID + "/book/layouts/" + profileID + "/manifest",
				})
				return
			}
		}
		problem(w, http.StatusNotFound, "layout profile not found")
		return
	}
	s.writeLayoutJobStatus(w, job)
}

func (s *Server) readerLayoutManifest(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	profileID := chi.URLParam(r, "profile")
	if !validProfileID(profileID) || profileID == "" {
		problem(w, http.StatusBadRequest, "profile id is invalid")
		return
	}
	data, err := s.objects.Get(r.Context(), layout.ManifestObjectKey(f.objectKey, profileID), 64<<20)
	if err != nil {
		problem(w, http.StatusNotFound, "manifest not found（分页尚未生成或已过期）")
		return
	}
	complete := false
	var m layout.Manifest
	if json.Unmarshal(data, &m) == nil {
		complete = m.Complete
	}
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	if complete {
		w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	} else {
		// 渐进式快照会被后续快照替换：不缓存
		w.Header().Set("Cache-Control", "private, no-cache")
	}
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

func (s *Server) readerLayoutPage(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	profileID := chi.URLParam(r, "profile")
	if !validProfileID(profileID) || profileID == "" {
		problem(w, http.StatusBadRequest, "profile id is invalid")
		return
	}
	spine, err := strconv.Atoi(chi.URLParam(r, "spine"))
	if err != nil || spine < 0 || spine > 1<<20 {
		problem(w, http.StatusBadRequest, "spine index is invalid")
		return
	}
	col, err := strconv.Atoi(chi.URLParam(r, "col"))
	if err != nil || col < 0 || col > 1<<20 {
		problem(w, http.StatusBadRequest, "page index is invalid")
		return
	}
	data, err := s.objects.Get(r.Context(), layout.SpinePageObjectKey(f.objectKey, profileID, spine, col), 16<<20)
	if err != nil {
		problem(w, http.StatusNotFound, "page not found")
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

// ---- layout 缓存 GC ----

// collectLayoutCache 回收 layouts/ 前缀：已删除书的产物立即清理；其余按
// TTL 与容量上限淘汰最旧对象。返回删除的对象数。
func (s *Server) collectLayoutCache(ctx context.Context, referenced map[string]bool) int {
	type candidate struct {
		key  string
		size int64
		mod  time.Time
	}
	var candidates []candidate
	var survivors int64
	deleted := 0
	cutoff := time.Now().UTC().Add(-s.cfg.LayoutCacheTTL)

	err := s.objects.WalkPrefix(ctx, "layouts/", func(objects []storage.ObjectRef) error {
		for _, object := range objects {
			blobKey := layoutBlobKey(object.Key)
			if blobKey == "" {
				continue
			}
			if !referenced[blobKey] {
				// 书已删除：整个 profile 目录都成孤儿
				if s.objects.DeleteMany(ctx, []string{object.Key}, "layout orphan cleanup") == nil {
					deleted++
				}
				continue
			}
			if s.cfg.LayoutCacheTTL > 0 && object.LastModified.Before(cutoff) {
				if s.objects.DeleteMany(ctx, []string{object.Key}, "layout ttl cleanup") == nil {
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
		s.log.Warn("layout cache walk failed", "error", err)
		return deleted
	}
	// 容量上限：从最旧开始淘汰，直到总量收敛到上限以内
	sort.Slice(candidates, func(i, j int) bool { return candidates[i].mod.Before(candidates[j].mod) })
	for _, c := range candidates {
		if survivors <= s.cfg.LayoutCacheCapacity {
			break
		}
		if s.objects.DeleteMany(ctx, []string{c.key}, "layout capacity cleanup") == nil {
			deleted++
			survivors -= c.size
		}
	}
	return deleted
}

// layoutBlobKey 从 layouts/blobs/<uuid>/<profile>/... 键里取回书的
// blob 键（objectKey 自身含 "blobs/" 前缀，因此是前两段）。
func layoutBlobKey(key string) string {
	rest := strings.TrimPrefix(key, "layouts/")
	if rest == key {
		return ""
	}
	head, tail, ok := strings.Cut(rest, "/")
	if !ok || head != "blobs" {
		return ""
	}
	uuidPart, _, ok := strings.Cut(tail, "/")
	if !ok {
		return ""
	}
	return "blobs/" + uuidPart
}

// removeLayoutCache 删除一本书的全部 layout 产物（永久删除书时调用）。
func (s *Server) removeLayoutCache(ctx context.Context, f File) int {
	deleted := 0
	err := s.objects.WalkPrefix(ctx, layout.LayoutPrefix(f.objectKey), func(objects []storage.ObjectRef) error {
		keys := make([]string, 0, len(objects))
		for _, object := range objects {
			keys = append(keys, object.Key)
		}
		for len(keys) > 0 {
			n := min(len(keys), 1000)
			if err := s.objects.DeleteMany(ctx, keys[:n], "book layout cleanup"); err != nil {
				return err
			}
			deleted += n
			keys = keys[n:]
		}
		return ctx.Err()
	})
	if err != nil && ctx.Err() == nil {
		s.log.Warn("remove layout cache failed", "file", f.ID, "error", err)
	}
	return deleted
}

// readerFont 提供共享 WebFont：服务端分页与客户端渲染使用同一字体文件，
// 保证分页结果一致。
func (s *Server) readerFont(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "font/woff2")
	w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(layout.FontBytes())
}

// readerCSS 提供共享样式表（内容规则 + @font-face）。数值排版参数由页面
// 容器 inline 锁定，样式表本身与 profile 无关。
func (s *Server) readerCSS(w http.ResponseWriter, _ *http.Request) {
	body := layout.SharedCSS() + "\n" +
		layout.FontFaceCSS("/api/reader/fonts/revaro-serif.woff2?v="+layout.FontVersion())
	w.Header().Set("Content-Type", "text/css; charset=utf-8")
	w.Header().Set("Cache-Control", "private, max-age=3600")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write([]byte(body))
}

// chapterSourcePath 返回章节 HTML 里第一个 data-source-path 属性值
// （清洗器在每个块上注入该属性，首块即章节源路径）。
func chapterSourcePath(chapterHTML string) string {
	doc, err := html.Parse(strings.NewReader(chapterHTML))
	if err != nil {
		return ""
	}
	var found string
	var walk func(*html.Node) bool
	walk = func(n *html.Node) bool {
		if n.Type == html.ElementNode {
			for _, attr := range n.Attr {
				if attr.Key == "data-source-path" {
					found = attr.Val
					return true
				}
			}
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			if walk(c) {
				return true
			}
		}
		return false
	}
	walk(doc)
	return found
}
