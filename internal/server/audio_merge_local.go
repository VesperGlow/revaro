package server

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/go-chi/chi/v5"
)

// Local-directory audio merge: the browser uploads WAV + VTT + cover files
// from a user-selected folder into a job staging directory under APP_WORK_DIR
// (chunked, resumable, with progress), and the merge pipeline then encodes a
// lossless ALAC .m4a from those local files. Only the final master is ever
// written to object storage as a normal blobs/<UUID> object; source material
// never creates S3 objects, multipart uploads or file-tree entries.

const (
	localMergeChunkSize           = int64(8 << 20) // 8 MiB per uploaded chunk
	maxLocalMergeUploadingJobs    = 8              // concurrent upload-phase jobs
	localMergeUploadConcurrency   = 4              // concurrent chunk writes
	localMergeMinFreeBytes        = int64(1 << 30) // keep at least 1 GiB free
	maxLocalMergeFiles            = 1024
	maxLocalMergeAudioBytes       = int64(64 << 30) // per-file audio cap
	maxLocalMergeTotalBytes       = int64(128 << 30)
	maxLocalMergeSubtitles        = 512
	maxLocalMergeCovers           = 64
	localMergeUploadProgressStart = 2
	localMergeUploadProgressEnd   = 48
)

var localMergeCoverKeywords = []string{"cover", "folder", "front", "album", "art", "poster", "thumb", "封面"}

// localMergeFile is one file of a local merge manifest. chunkDone tracks which
// chunks have been durably stored so uploads can be retried and resumed.
type localMergeFile struct {
	Name      string
	Size      int64
	Kind      string // "audio" | "subtitle" | "cover"
	Chunks    int
	Uploaded  int64
	chunkDone []bool
}

type localMergeInputFile struct {
	Name string `json:"name"`
	Size int64  `json:"size"`
}

type createLocalAudioMergeInput struct {
	ParentID string                `json:"parent_id"`
	Name     string                `json:"name"`
	Files    []localMergeInputFile `json:"files"`
	Order    []string              `json:"order"`
	Cover    *string               `json:"cover"`
}

type localMergeFileInfo struct {
	Name       string `json:"name"`
	Size       int64  `json:"size"`
	Kind       string `json:"kind"`
	ChunkCount int    `json:"chunk_count"`
}

type localMergeCreateResponse struct {
	audioMergeSnapshot
	ChunkSize int64                `json:"chunk_size"`
	Files     []localMergeFileInfo `json:"files"`
}

// naturalLess orders names the way humans expect: case-insensitively, with
// numeric runs compared by value ("track2.wav" before "track10.wav").
func naturalLess(a, b string) bool {
	ai, bi := 0, 0
	for ai < len(a) && bi < len(b) {
		ca, cb := a[ai], b[bi]
		da, db := ca >= '0' && ca <= '9', cb >= '0' && cb <= '9'
		if da && db {
			endA, endB := ai, bi
			for endA < len(a) && a[endA] >= '0' && a[endA] <= '9' {
				endA++
			}
			for endB < len(b) && b[endB] >= '0' && b[endB] <= '9' {
				endB++
			}
			if c := compareDigitRuns(a[ai:endA], b[bi:endB]); c != 0 {
				return c < 0
			}
			ai, bi = endA, endB
			continue
		}
		la, lb := ca, cb
		if la >= 'A' && la <= 'Z' {
			la += 'a' - 'A'
		}
		if lb >= 'A' && lb <= 'Z' {
			lb += 'a' - 'A'
		}
		if la != lb {
			return la < lb
		}
		ai++
		bi++
	}
	return len(a) < len(b)
}

func compareDigitRuns(a, b string) int {
	ta, tb := strings.TrimLeft(a, "0"), strings.TrimLeft(b, "0")
	if len(ta) != len(tb) {
		if len(ta) < len(tb) {
			return -1
		}
		return 1
	}
	for i := range ta {
		if ta[i] != tb[i] {
			if ta[i] < tb[i] {
				return -1
			}
			return 1
		}
	}
	if len(a) != len(b) { // numerically equal: fewer leading zeros first
		if len(a) < len(b) {
			return -1
		}
		return 1
	}
	return 0
}

// localCoverScore ranks cover candidates by well-known cover names. Higher is
// better; zero means no cover-like name was detected.
func localCoverScore(name string) int {
	base := strings.ToLower(strings.TrimSuffix(name, filepath.Ext(name)))
	for rank, keyword := range localMergeCoverKeywords {
		if base == keyword {
			return 100 - rank
		}
		if len(base) > len(keyword) && strings.HasPrefix(base, keyword) {
			switch base[len(keyword)] {
			case ' ', '_', '-', '.', '(', '[':
				return 80 - rank
			}
		}
		if strings.Contains(base, keyword) {
			return 60 - rank
		}
	}
	return 0
}

// selectLocalMergeCover picks the default cover: the only image, or the best
// cover-named image when several are present. With several images and no
// cover-like name the choice is left to the user.
func selectLocalMergeCover(files []localMergeFile) string {
	var candidates []string
	for _, file := range files {
		if file.Kind == "cover" {
			candidates = append(candidates, file.Name)
		}
	}
	if len(candidates) == 0 {
		return ""
	}
	if len(candidates) == 1 {
		return candidates[0]
	}
	best, bestScore := "", -1
	for _, candidate := range candidates {
		if score := localCoverScore(candidate); score > bestScore {
			best, bestScore = candidate, score
		}
	}
	if bestScore <= 0 {
		return ""
	}
	return best
}

// mergeDiskAvailable reports free bytes on the local merge staging volume.
// Tests may replace it through Server.diskFree.
func (s *Server) mergeDiskAvailable() (int64, error) {
	if s.diskFree != nil {
		return s.diskFree(s.cfg.WorkDir)
	}
	return archiveDiskAvailable(s.cfg.WorkDir)
}

// mergeDiskEnough requires the requested bytes plus a safety margin so a
// merge can never fill the disk completely.
func mergeDiskEnough(available, needed int64) bool {
	return available >= needed+localMergeMinFreeBytes
}

func classifyLocalMergeFile(name string) string {
	ext := strings.ToLower(filepath.Ext(name))
	switch {
	case audioSourceExts[ext]:
		return "audio"
	case ext == ".vtt":
		return "subtitle"
	case audioCoverSourceExts[ext]:
		return "cover"
	default:
		return ""
	}
}

func findLocalMergeFile(files []localMergeFile, name string) int {
	for index := range files {
		if strings.EqualFold(files[index].Name, name) {
			return index
		}
	}
	return -1
}

func (s *Server) createLocalAudioMerge(w http.ResponseWriter, r *http.Request) {
	var in createLocalAudioMergeInput
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if len(in.Files) < 2 || len(in.Files) > maxLocalMergeFiles {
		problem(w, http.StatusBadRequest, "select a folder with at least two audio files")
		return
	}
	if err := validateName(in.Name); err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	if !strings.EqualFold(filepath.Ext(in.Name), ".m4a") {
		problem(w, http.StatusBadRequest, "local merges always produce an ALAC .m4a")
		return
	}
	parent, err := s.file(r.Context(), in.ParentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		problem(w, http.StatusBadRequest, "parent directory is invalid")
		return
	}

	files := make([]localMergeFile, 0, len(in.Files))
	seenNames := make(map[string]struct{}, len(in.Files))
	var audioCount, subtitleCount, coverCount, totalSize int64
	for _, input := range in.Files {
		if err := validateName(input.Name); err != nil {
			problem(w, http.StatusBadRequest, fmt.Sprintf("文件名「%s」无效", input.Name))
			return
		}
		lower := strings.ToLower(input.Name)
		if _, duplicate := seenNames[lower]; duplicate {
			problem(w, http.StatusBadRequest, fmt.Sprintf("「%s」在目录中重复，请勿选择同名文件", input.Name))
			return
		}
		seenNames[lower] = struct{}{}
		if input.Size < 1 || input.Size > maxLogicalFileSize {
			problem(w, http.StatusBadRequest, fmt.Sprintf("「%s」的大小无效", input.Name))
			return
		}
		kind := classifyLocalMergeFile(input.Name)
		if kind == "" {
			problem(w, http.StatusBadRequest, fmt.Sprintf("「%s」不是受支持的音频、字幕或封面文件", input.Name))
			return
		}
		switch kind {
		case "audio":
			audioCount++
			if input.Size > maxLocalMergeAudioBytes {
				problem(w, http.StatusRequestEntityTooLarge, fmt.Sprintf("音频「%s」超过 64 GiB，无法合并", input.Name))
				return
			}
		case "subtitle":
			subtitleCount++
			if input.Size > maxAudioSubtitleBytes {
				problem(w, http.StatusRequestEntityTooLarge, fmt.Sprintf("字幕「%s」超过 8 MiB", input.Name))
				return
			}
		case "cover":
			coverCount++
			if input.Size > maxAudioCoverSourceBytes {
				problem(w, http.StatusRequestEntityTooLarge, fmt.Sprintf("封面「%s」超过 16 MiB", input.Name))
				return
			}
		}
		if totalSize > maxLocalMergeTotalBytes-input.Size || totalSize > maxLogicalFileSize-input.Size {
			problem(w, http.StatusRequestEntityTooLarge, "所选素材合计超过大小限制")
			return
		}
		totalSize += input.Size
		files = append(files, localMergeFile{
			Name: input.Name, Size: input.Size, Kind: kind,
			Chunks: int((input.Size + localMergeChunkSize - 1) / localMergeChunkSize),
		})
	}
	if audioCount < 2 || audioCount > maxAudioMergeInputs {
		problem(w, http.StatusBadRequest, "请选择 2 到 256 个音频文件")
		return
	}
	if subtitleCount > maxLocalMergeSubtitles || coverCount > maxLocalMergeCovers {
		problem(w, http.StatusBadRequest, "字幕或封面文件数量过多")
		return
	}
	for index := range files {
		files[index].chunkDone = make([]bool, files[index].Chunks)
	}

	// Play order defaults to natural sort; an explicit order must be a
	// permutation of the audio names.
	audioOrder := make([]int, 0, audioCount)
	if len(in.Order) == 0 {
		ordered := make([]localMergeFile, 0, audioCount)
		for index := range files {
			if files[index].Kind == "audio" {
				ordered = append(ordered, files[index])
			}
		}
		sortLocalMergeFiles(ordered)
		for _, file := range ordered {
			audioOrder = append(audioOrder, findLocalMergeFile(files, file.Name))
		}
	} else {
		if int64(len(in.Order)) != audioCount {
			problem(w, http.StatusBadRequest, "播放顺序必须恰好包含每个音频文件一次")
			return
		}
		seenOrder := make(map[string]struct{}, len(in.Order))
		for _, name := range in.Order {
			lower := strings.ToLower(name)
			if _, duplicate := seenOrder[lower]; duplicate {
				problem(w, http.StatusBadRequest, "播放顺序中出现了重复的音频文件")
				return
			}
			seenOrder[lower] = struct{}{}
			index := findLocalMergeFile(files, name)
			if index < 0 || files[index].Kind != "audio" {
				problem(w, http.StatusBadRequest, "播放顺序包含不是音频的文件")
				return
			}
			audioOrder = append(audioOrder, index)
		}
	}

	subtitleFor := make([]int, len(audioOrder))
	for position, fileIndex := range audioOrder {
		subtitleFor[position] = -1
		best, bestPriority := -1, 0
		for candidate := range files {
			if files[candidate].Kind != "subtitle" {
				continue
			}
			priority, ok := audioSubtitleMatchPriority(files[fileIndex].Name, files[candidate].Name)
			if ok && (best < 0 || priority < bestPriority) {
				best, bestPriority = candidate, priority
			}
		}
		subtitleFor[position] = best
	}

	coverIndex := -1
	if in.Cover == nil {
		if name := selectLocalMergeCover(files); name != "" {
			coverIndex = findLocalMergeFile(files, name)
		}
	} else if *in.Cover != "" {
		coverIndex = findLocalMergeFile(files, *in.Cover)
		if coverIndex < 0 {
			problem(w, http.StatusBadRequest, "封面必须是所选目录中的图片")
			return
		}
	}

	if err := os.MkdirAll(s.cfg.WorkDir, 0o700); err != nil {
		problem(w, http.StatusInternalServerError, "无法创建本地合并工作目录")
		return
	}
	if err := s.ensureMergeDisk(totalSize); err != nil {
		problem(w, http.StatusInsufficientStorage, err.Error())
		return
	}
	select {
	case s.localMergeJobSlots <- struct{}{}:
	default:
		problem(w, http.StatusTooManyRequests, "同时进行的本地合并上传过多，请稍后再试")
		return
	}
	fail := func(status int, message string) {
		<-s.localMergeJobSlots
		problem(w, status, message)
	}
	staging, err := os.MkdirTemp(s.cfg.WorkDir, "revaro-local-merge-")
	if err != nil {
		fail(http.StatusInternalServerError, "无法创建本地合并暂存目录")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	outputID := ids.New()
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,0,'audio/mp4','pending',?,?)`, outputID, in.ParentID, in.Name, "file", now, now)
	if err != nil {
		_ = os.RemoveAll(staging)
		if isConflict(err) {
			fail(http.StatusConflict, "an item with that name already exists")
		} else {
			fail(http.StatusInternalServerError, "could not reserve merged audio")
		}
		return
	}
	mergeCtx, cancel := context.WithTimeout(s.audioHLSCtx, audioMergeTimeout)
	job := &audioMergeJob{
		changed: s.jobs.Changed,
		ID:      ids.New(), Status: "uploading", Progress: localMergeUploadProgressStart, Message: "正在等待上传本地素材",
		OutputName: in.Name, OutputFormat: "alac", OutputFileID: outputID, ParentID: in.ParentID,
		InputCount: int(audioCount), Source: "local", CreatedAt: now, UpdatedAt: now, cancel: cancel, mergeCtx: mergeCtx,
		localUpload: true, stagingDir: staging, uploadSlotHeld: true,
		files: files, audioOrder: audioOrder, subtitleFor: subtitleFor, coverIndex: coverIndex,
	}
	s.audioMergeMu.Lock()
	s.audioMergeJobs[job.ID] = job
	s.audioMergeMu.Unlock()
	s.log.Info("local audio merge created", "job", job.ID, "output", in.Name, "audio", audioCount, "subtitle", subtitleCount, "cover", coverCount, "bytes", totalSize)
	infos := make([]localMergeFileInfo, 0, len(files))
	for _, file := range files {
		infos = append(infos, localMergeFileInfo{Name: file.Name, Size: file.Size, Kind: file.Kind, ChunkCount: file.Chunks})
	}
	writeJSON(w, http.StatusCreated, localMergeCreateResponse{audioMergeSnapshot: job.snapshot(), ChunkSize: localMergeChunkSize, Files: infos})
}

// ensureMergeDisk rejects a local merge when the staging volume cannot hold
// the requested bytes while keeping the safety margin free.
func (s *Server) ensureMergeDisk(needed int64) error {
	available, err := s.mergeDiskAvailable()
	if err != nil {
		return errors.New("无法检查本地磁盘剩余空间")
	}
	if !mergeDiskEnough(available, needed) {
		return fmt.Errorf("本地磁盘剩余空间不足（需要约 %s），已中止以免写满磁盘", localMergeBytes(needed))
	}
	return nil
}

func localMergeBytes(bytes int64) string {
	const unit = 1024
	if bytes < unit {
		return fmt.Sprintf("%d B", bytes)
	}
	div, exp := int64(unit), 0
	for n := bytes / unit; n >= unit; n /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %ciB", float64(bytes)/float64(div), "KMGTPE"[exp])
}

func sortLocalMergeFiles(files []localMergeFile) {
	// insertion sort keeps this dependency-free; manifests are small
	for i := 1; i < len(files); i++ {
		for j := i; j > 0 && naturalLess(files[j].Name, files[j-1].Name); j-- {
			files[j], files[j-1] = files[j-1], files[j]
		}
	}
}

func (s *Server) uploadLocalMergeChunk(w http.ResponseWriter, r *http.Request) {
	fileIndex, err := strconv.Atoi(chi.URLParam(r, "fileIndex"))
	chunkIndex, chunkErr := strconv.Atoi(chi.URLParam(r, "chunkIndex"))
	if err != nil || chunkErr != nil || fileIndex < 0 || chunkIndex < 0 {
		problem(w, http.StatusBadRequest, "invalid chunk coordinates")
		return
	}
	s.audioMergeMu.RLock()
	job := s.audioMergeJobs[chi.URLParam(r, "id")]
	s.audioMergeMu.RUnlock()
	if job == nil || !job.localUpload {
		problem(w, http.StatusNotFound, "local merge job not found")
		return
	}
	job.mu.Lock()
	if job.Status != "uploading" {
		job.mu.Unlock()
		problem(w, http.StatusConflict, "this merge is no longer accepting uploads")
		return
	}
	if fileIndex >= len(job.files) || chunkIndex >= job.files[fileIndex].Chunks {
		job.mu.Unlock()
		problem(w, http.StatusBadRequest, "chunk is outside the file layout")
		return
	}
	expected := localMergeChunkSize
	if chunkIndex == job.files[fileIndex].Chunks-1 {
		expected = job.files[fileIndex].Size - localMergeChunkSize*int64(job.files[fileIndex].Chunks-1)
	}
	stagingDir := job.stagingDir
	job.mu.Unlock()

	select {
	case s.localMergeUploads <- struct{}{}:
		defer func() { <-s.localMergeUploads }()
	case <-r.Context().Done():
		return
	}
	chunksDir := filepath.Join(stagingDir, "chunks")
	if err := os.MkdirAll(chunksDir, 0o700); err != nil {
		problem(w, http.StatusInternalServerError, "无法写入本地暂存目录")
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, localMergeChunkSize+1)
	tmp, err := os.CreateTemp(chunksDir, ".tmp-")
	if err != nil {
		problem(w, http.StatusInternalServerError, "无法写入本地暂存目录")
		return
	}
	tmpPath := tmp.Name()
	written, copyErr := io.Copy(tmp, io.LimitReader(r.Body, localMergeChunkSize+1))
	closeErr := tmp.Close()
	if copyErr != nil || closeErr != nil || written != expected {
		_ = os.Remove(tmpPath)
		if written > expected {
			problem(w, http.StatusRequestEntityTooLarge, "chunk exceeds the expected size")
		} else {
			problem(w, http.StatusBadRequest, "chunk size does not match the file layout")
		}
		return
	}
	finalPath := filepath.Join(chunksDir, fmt.Sprintf("f%d-c%d.part", fileIndex, chunkIndex))
	job.mu.Lock()
	if job.files[fileIndex].chunkDone[chunkIndex] {
		// Idempotent retry: the chunk is already stored.
		job.mu.Unlock()
		_ = os.Remove(tmpPath)
		w.WriteHeader(http.StatusNoContent)
		return
	}
	if available, diskErr := s.mergeDiskAvailable(); diskErr == nil {
		remaining := job.uploadedBytes + expected
		if !mergeDiskEnough(available, remaining) {
			job.mu.Unlock()
			_ = os.Remove(tmpPath)
			problem(w, http.StatusInsufficientStorage, "本地磁盘剩余空间不足，无法继续上传素材")
			return
		}
	}
	if err := os.Rename(tmpPath, finalPath); err != nil {
		job.mu.Unlock()
		_ = os.Remove(tmpPath)
		problem(w, http.StatusInternalServerError, "无法保存上传的分块")
		return
	}
	job.files[fileIndex].chunkDone[chunkIndex] = true
	job.files[fileIndex].Uploaded += written
	job.uploadedBytes += written
	progress, message := localMergeUploadProgress(job)
	job.mu.Unlock()
	job.update("uploading", progress, message)
	w.WriteHeader(http.StatusNoContent)
}

func localMergeUploadProgress(job *audioMergeJob) (int, string) {
	total := job.uploadedBytes
	var all int64
	for _, file := range job.files {
		all += file.Size
	}
	filesDone := 0
	for _, file := range job.files {
		if file.Uploaded == file.Size {
			filesDone++
		}
	}
	progress := localMergeUploadProgressStart
	if all > 0 {
		progress += int(total * (localMergeUploadProgressEnd - localMergeUploadProgressStart) / all)
	}
	return progress, fmt.Sprintf("正在上传本地素材 %d / %d 个文件", filesDone, len(job.files))
}

func (s *Server) completeLocalAudioMerge(w http.ResponseWriter, r *http.Request) {
	s.audioMergeMu.RLock()
	job := s.audioMergeJobs[chi.URLParam(r, "id")]
	s.audioMergeMu.RUnlock()
	if job == nil || !job.localUpload {
		problem(w, http.StatusNotFound, "local merge job not found")
		return
	}
	job.mu.Lock()
	if job.Status != "uploading" {
		job.mu.Unlock()
		problem(w, http.StatusConflict, "this merge is no longer accepting uploads")
		return
	}
	for fileIndex := range job.files {
		for chunk := 0; chunk < job.files[fileIndex].Chunks; chunk++ {
			if !job.files[fileIndex].chunkDone[chunk] {
				job.mu.Unlock()
				problem(w, http.StatusBadRequest, "还有素材分块没有上传完成")
				return
			}
		}
	}
	var audioBytes int64
	for _, fileIndex := range job.audioOrder {
		audioBytes += job.files[fileIndex].Size
	}
	job.mu.Unlock()

	// Merging roughly doubles the staging footprint (source + ALAC output);
	// require the audio payload to fit alongside the safety margin.
	if err := s.ensureMergeDisk(audioBytes); err != nil {
		s.abortLocalMergeUpload(job, err.Error())
		problem(w, http.StatusInsufficientStorage, err.Error())
		return
	}
	job.mu.Lock()
	job.Status = "queued"
	job.Progress = localMergeUploadProgressEnd
	job.Message = "素材上传完成，等待合并"
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	job.releaseUploadSlot(s)
	if !s.runBackground(func() { s.runLocalAudioMerge(job) }) {
		s.abortLocalMergeUpload(job, "服务正在关闭")
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}
	writeJSON(w, http.StatusOK, job.snapshot())
}

// abortLocalMergeUpload drops an upload-phase job and every trace of it.
func (s *Server) abortLocalMergeUpload(job *audioMergeJob, message string) {
	s.audioMergeMu.Lock()
	delete(s.audioMergeJobs, job.ID)
	s.audioMergeMu.Unlock()
	job.finish("failed", "合并失败", message)
	job.cleanupStaging(s.log)
	job.releaseUploadSlot(s)
	cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	_, _ = s.db.ExecContext(cleanupCtx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
	cancel()
	s.log.Warn("local merge upload aborted", "job", job.ID, "output", job.OutputName, "error", message)
}

func (s *Server) runLocalAudioMerge(job *audioMergeJob) {
	defer job.cancel()
	profile, _ := audioOutput("alac")
	err := s.executeLocalAudioMerge(job.mergeCtx, job)
	s.audioMergeFinished(job, err, profile, job.InputCount)
}

func (s *Server) executeLocalAudioMerge(ctx context.Context, job *audioMergeJob) error {
	select {
	case s.audioMergeSlots <- struct{}{}:
		defer func() { <-s.audioMergeSlots }()
	case <-ctx.Done():
		return ctx.Err()
	}
	job.mu.Lock()
	workDir := job.stagingDir
	job.mu.Unlock()
	job.update("preparing", 50, "正在整理本地素材")
	if err := s.assembleLocalMerge(ctx, job, workDir); err != nil {
		return err
	}
	inputs := make([]File, 0, len(job.audioOrder))
	paths := make([]string, 0, len(job.audioOrder))
	for position, fileIndex := range job.audioOrder {
		if err := ctx.Err(); err != nil {
			return err
		}
		file := job.files[fileIndex]
		ext := strings.ToLower(filepath.Ext(file.Name))
		if !audioSourceExts[ext] {
			ext = ".audio"
		}
		path := filepath.Join(workDir, fmt.Sprintf("input-%04d%s", position, ext))
		inputs = append(inputs, File{Name: file.Name, Size: file.Size, Kind: "file", Status: "ready"})
		paths = append(paths, path)
	}
	cover, err := s.localMergeCover(ctx, job, workDir)
	if err != nil {
		return err
	}
	subtitleSources := make([]*File, len(inputs))
	openLocal := func(_ context.Context, f File) (io.ReadCloser, error) {
		return os.Open(f.objectKey)
	}
	for position, fileIndex := range job.subtitleFor {
		if fileIndex < 0 {
			continue
		}
		file := job.files[fileIndex]
		source := File{Name: file.Name, Size: file.Size, Kind: "file", Status: "ready", objectKey: filepath.Join(workDir, fmt.Sprintf("subtitle-%04d.vtt", position))}
		subtitleSources[position] = &source
	}
	profile, _ := audioOutput("alac")
	return s.encodeMergedAudio(ctx, job, profile, workDir, inputs, paths, subtitleSources, openLocal, cover)
}

func (s *Server) localMergeCover(ctx context.Context, job *audioMergeJob, workDir string) ([]byte, error) {
	if job.coverIndex < 0 {
		return nil, nil
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(filepath.Join(workDir, "cover.raw"))
	if err != nil {
		return nil, fmt.Errorf("read local cover: %w", err)
	}
	normalized, err := normalizeAudioCover(raw)
	if err != nil {
		return nil, audioMergeUserError{message: fmt.Sprintf("封面「%s」不是有效的图片", job.files[job.coverIndex].Name)}
	}
	return normalized, nil
}

// assembleLocalMerge concatenates the uploaded chunks into the input-* /
// subtitle-* / cover.raw staging files the shared pipeline consumes.
func (s *Server) assembleLocalMerge(ctx context.Context, job *audioMergeJob, workDir string) error {
	chunksDir := filepath.Join(workDir, "chunks")
	var totalBytes int64
	for _, file := range job.files {
		totalBytes += file.Size
	}
	var assembled int64
	for position, fileIndex := range job.audioOrder {
		if err := ctx.Err(); err != nil {
			return err
		}
		file := job.files[fileIndex]
		ext := strings.ToLower(filepath.Ext(file.Name))
		if !audioSourceExts[ext] {
			ext = ".audio"
		}
		dest := filepath.Join(workDir, fmt.Sprintf("input-%04d%s", position, ext))
		n, err := concatLocalMergeChunks(chunksDir, fileIndex, file.Chunks, dest)
		if err != nil {
			return fmt.Errorf("assemble audio %s: %w", file.Name, err)
		}
		if n != file.Size {
			return fmt.Errorf("assemble audio %s: size mismatch: got %d, want %d", file.Name, n, file.Size)
		}
		assembled += n
		if totalBytes > 0 {
			job.update("preparing", 50+int(assembled*8/totalBytes), fmt.Sprintf("正在整理第 %d / %d 段", position+1, len(job.audioOrder)))
		}
	}
	for position, fileIndex := range job.subtitleFor {
		if fileIndex < 0 {
			continue
		}
		file := job.files[fileIndex]
		dest := filepath.Join(workDir, fmt.Sprintf("subtitle-%04d.vtt", position))
		n, err := concatLocalMergeChunks(chunksDir, fileIndex, file.Chunks, dest)
		if err != nil {
			return fmt.Errorf("assemble subtitle %s: %w", file.Name, err)
		}
		if n != file.Size {
			return fmt.Errorf("assemble subtitle %s: size mismatch", file.Name)
		}
	}
	if job.coverIndex >= 0 {
		file := job.files[job.coverIndex]
		dest := filepath.Join(workDir, "cover.raw")
		n, err := concatLocalMergeChunks(chunksDir, job.coverIndex, file.Chunks, dest)
		if err != nil {
			return fmt.Errorf("assemble cover %s: %w", file.Name, err)
		}
		if n != file.Size {
			return fmt.Errorf("assemble cover %s: size mismatch", file.Name)
		}
	}
	return os.RemoveAll(chunksDir)
}

func concatLocalMergeChunks(chunksDir string, fileIndex, chunkCount int, dest string) (int64, error) {
	out, err := os.OpenFile(dest, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return 0, err
	}
	var total int64
	buf := make([]byte, 256<<10)
	for chunk := 0; chunk < chunkCount; chunk++ {
		src, openErr := os.Open(filepath.Join(chunksDir, fmt.Sprintf("f%d-c%d.part", fileIndex, chunk)))
		if openErr != nil {
			out.Close()
			return total, openErr
		}
		n, copyErr := io.CopyBuffer(out, src, buf)
		closeErr := src.Close()
		if copyErr != nil {
			out.Close()
			return total, copyErr
		}
		if closeErr != nil {
			out.Close()
			return total, closeErr
		}
		total += n
	}
	return total, out.Close()
}

// CleanupExpiredLocalMerges abandons upload-phase local merge jobs whose
// creator disappeared, freeing their staging directories and slots.
func (s *Server) CleanupExpiredLocalMerges(ctx context.Context) {
	cutoff := time.Now().UTC().Add(-s.cfg.UploadExpires)
	s.audioMergeMu.Lock()
	var stale []*audioMergeJob
	for _, job := range s.audioMergeJobs {
		if !job.localUpload {
			continue
		}
		job.mu.Lock()
		uploading := job.Status == "uploading"
		job.mu.Unlock()
		if !uploading {
			continue
		}
		if created, err := time.Parse(time.RFC3339Nano, job.CreatedAt); err == nil && created.Before(cutoff) {
			stale = append(stale, job)
		}
	}
	for _, job := range stale {
		delete(s.audioMergeJobs, job.ID)
		job.cleanupStaging(s.log)
		job.releaseUploadSlot(s)
	}
	s.audioMergeMu.Unlock()
	for _, job := range stale {
		_, _ = s.db.ExecContext(ctx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
		s.log.Info("expired local merge upload cleaned", "job", job.ID, "output", job.OutputName)
	}
}
