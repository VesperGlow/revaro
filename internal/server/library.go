package server

import (
	"context"
	"net/http"
	"path/filepath"
	"strings"
)

// 媒体库：跨目录按类型聚合文件，供前端「分类栏」的五大类视图使用。
// 文件类（file）不在此接口聚合，仍使用普通目录浏览。

type libraryFolderRef struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

type libraryItem struct {
	File
	FolderPath []libraryFolderRef `json:"folder_path"`
	DurationMS int64              `json:"duration_ms,omitempty"`
}

type libraryCounts struct {
	Book  int `json:"book"`
	Image int `json:"image"`
	Video int `json:"video"`
	Audio int `json:"audio"`
	File  int `json:"file"`
}

var libraryTypes = map[string]bool{"book": true, "image": true, "video": true, "audio": true, "file": true}

func isImageSource(f File) bool {
	if f.Kind != "file" || f.Status != "ready" {
		return false
	}
	switch strings.ToLower(responseMime(f)) {
	case "image/jpeg", "image/png", "image/webp", "image/gif", "image/avif":
		return true
	}
	switch strings.ToLower(filepath.Ext(f.Name)) {
	case ".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif", ".bmp":
		return true
	}
	return false
}

func isBookSource(f File) bool {
	if f.Kind != "file" || f.Status != "ready" {
		return false
	}
	switch strings.ToLower(filepath.Ext(f.Name)) {
	case ".epub", ".txt":
		return true
	}
	return false
}

func matchesLibraryType(f File, kind string) bool {
	switch kind {
	case "book":
		return isBookSource(f)
	case "image":
		return isImageSource(f)
	case "video":
		return f.Kind == "file" && f.Status == "ready" && isVideoSource(f)
	case "audio":
		return f.Kind == "file" && f.Status == "ready" && isAudioSource(f)
	case "file":
		return f.Kind == "file" && f.Status == "ready"
	default:
		return false
	}
}

// libraryFolders 返回目录 id -> 从根开始的名称路径。用于给每个文件标注所在位置。
func (s *Server) libraryFolders(ctx context.Context) (map[string][]libraryFolderRef, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT id,parent_id,name FROM files WHERE kind='directory' AND deleted_at IS NULL`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	type dir struct {
		parent string
		name   string
	}
	dirs := make(map[string]dir)
	order := make([]string, 0)
	for rows.Next() {
		var id, name string
		var parent *string
		if err := rows.Scan(&id, &parent, &name); err != nil {
			return nil, err
		}
		entry := dir{name: name}
		if parent != nil {
			entry.parent = *parent
		}
		dirs[id] = entry
		order = append(order, id)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	paths := make(map[string][]libraryFolderRef, len(dirs))
	var resolve func(id string, seen map[string]bool) []libraryFolderRef
	resolve = func(id string, seen map[string]bool) []libraryFolderRef {
		if cached, ok := paths[id]; ok {
			return cached
		}
		if seen[id] {
			return nil
		}
		seen[id] = true
		entry, ok := dirs[id]
		if !ok {
			return nil
		}
		parentPath := resolve(entry.parent, seen)
		if id == RootID {
			return nil
		}
		path := make([]libraryFolderRef, 0, len(parentPath)+1)
		path = append(path, parentPath...)
		path = append(path, libraryFolderRef{ID: id, Name: entry.name})
		paths[id] = path
		return path
	}
	for _, id := range order {
		resolve(id, map[string]bool{})
	}
	return paths, nil
}

// libraryFiles 一次性载入所有就绪文件、路径与媒体时长，供计数和分类复用。
func (s *Server) libraryFiles(ctx context.Context) ([]File, map[string][]libraryFolderRef, map[string]int64, error) {
	paths, err := s.libraryFolders(ctx)
	if err != nil {
		return nil, nil, nil, err
	}
	durations := make(map[string]int64)
	durationRows, err := s.db.QueryContext(ctx, `SELECT m.file_id,m.duration_ms FROM media_metadata m JOIN files f ON f.id=m.file_id WHERE m.source_etag=f.etag`)
	if err != nil {
		return nil, nil, nil, err
	}
	for durationRows.Next() {
		var id string
		var duration int64
		if err := durationRows.Scan(&id, &duration); err != nil {
			durationRows.Close()
			return nil, nil, nil, err
		}
		durations[id] = duration
	}
	if err := durationRows.Err(); err != nil {
		durationRows.Close()
		return nil, nil, nil, err
	}
	if err := durationRows.Close(); err != nil {
		return nil, nil, nil, err
	}
	rows, err := s.db.QueryContext(ctx, `SELECT `+fileColumns+` FROM files WHERE kind='file' AND status='ready' AND deleted_at IS NULL ORDER BY name COLLATE NOCASE`)
	if err != nil {
		return nil, nil, nil, err
	}
	defer rows.Close()
	files := make([]File, 0)
	for rows.Next() {
		f, err := scanFile(rows)
		if err != nil {
			return nil, nil, nil, err
		}
		files = append(files, f)
	}
	if err := rows.Err(); err != nil {
		return nil, nil, nil, err
	}
	return files, paths, durations, nil
}

func libraryTypeCounts(files []File) libraryCounts {
	var counts libraryCounts
	for _, f := range files {
		counts.File++
		switch {
		case isBookSource(f):
			counts.Book++
		case isImageSource(f):
			counts.Image++
		case isVideoSource(f):
			counts.Video++
		case isAudioSource(f):
			counts.Audio++
		}
	}
	return counts
}

func (s *Server) libraryCounts(w http.ResponseWriter, r *http.Request) {
	files, _, _, err := s.libraryFiles(r.Context())
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not load library")
		return
	}
	writeJSON(w, http.StatusOK, libraryTypeCounts(files))
}

func (s *Server) library(w http.ResponseWriter, r *http.Request) {
	kind := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("type")))
	if kind == "" {
		kind = "file"
	}
	if !libraryTypes[kind] {
		problem(w, http.StatusBadRequest, "unknown library type")
		return
	}
	files, paths, durations, err := s.libraryFiles(r.Context())
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not load library")
		return
	}
	counts := libraryTypeCounts(files)
	items := make([]libraryItem, 0)
	for _, f := range files {
		if !matchesLibraryType(f, kind) {
			continue
		}
		items = append(items, newLibraryItem(f, paths, durations))
	}
	s.warmLibraryMedia(files, kind)
	writeJSON(w, http.StatusOK, map[string]any{"type": kind, "items": items, "counts": counts})
}

// libraryAll 一次扫描返回四个媒体大类的条目，供分类栏的路径树与徽标使用。
func (s *Server) libraryAll(w http.ResponseWriter, r *http.Request) {
	files, paths, durations, err := s.libraryFiles(r.Context())
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not load library")
		return
	}
	grouped := make(map[string][]libraryItem, 4)
	for _, kind := range []string{"book", "image", "video", "audio"} {
		items := make([]libraryItem, 0)
		for _, f := range files {
			if matchesLibraryType(f, kind) {
				items = append(items, newLibraryItem(f, paths, durations))
			}
		}
		grouped[kind] = items
	}
	s.warmLibraryMedia(files, "video")
	s.warmLibraryMedia(files, "audio")
	writeJSON(w, http.StatusOK, map[string]any{"items": grouped, "counts": libraryTypeCounts(files)})
}

func newLibraryItem(f File, paths map[string][]libraryFolderRef, durations map[string]int64) libraryItem {
	path := []libraryFolderRef{}
	if f.ParentID != nil {
		if value, ok := paths[*f.ParentID]; ok {
			path = value
		}
	}
	return libraryItem{File: f, FolderPath: path, DurationMS: durations[f.ID]}
}

// warmLibraryMedia 让视频封面与音视频时长在后台补齐，列表稍后刷新即可看到。
func (s *Server) warmLibraryMedia(files []File, kind string) {
	for _, f := range files {
		if !matchesLibraryType(f, kind) {
			continue
		}
		switch kind {
		case "video":
			s.scheduleVideoThumbnail(f)
			s.scheduleMediaAnalysis(f)
		case "audio":
			s.scheduleMediaAnalysis(f)
		}
	}
}
