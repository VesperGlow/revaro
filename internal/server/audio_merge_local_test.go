package server

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/VesperGlow/revaro/internal/ids"
)

type localMergeTestFile struct {
	Name string `json:"name"`
	Size int64  `json:"size"`
}

type localMergeCreated struct {
	ID           string               `json:"id"`
	Status       string               `json:"status"`
	OutputName   string               `json:"output_name"`
	ChunkSize    int64                `json:"chunk_size"`
	Files        []localMergeFileInfo `json:"files"`
	OutputFileID string               `json:"output_file_id"`
}

func createLocalMerge(t *testing.T, a *testApp, name string, files []localMergeTestFile, order []string, cover *string) *httptest.ResponseRecorder {
	t.Helper()
	body := map[string]any{"parent_id": RootID, "name": name, "files": files, "order": order, "cover": cover}
	return a.request("POST", "/api/audio-merges/local", body, true)
}

func localMergeContent(name string, size int64) []byte {
	data := make([]byte, size)
	for i := range data {
		data[i] = byte((i*31 + len(name)*7) & 0xff)
	}
	return data
}

func uploadLocalMerge(t *testing.T, a *testApp, created localMergeCreated, contents map[string][]byte) {
	t.Helper()
	for fileIndex, file := range created.Files {
		data := contents[file.Name]
		if int64(len(data)) != file.Size {
			t.Fatalf("fixture size for %s: got %d want %d", file.Name, len(data), file.Size)
		}
		for chunk := 0; chunk < file.ChunkCount; chunk++ {
			start := chunk * int(created.ChunkSize)
			end := min(len(data), start+int(created.ChunkSize))
			rr := a.requestRaw("POST", fmt.Sprintf("/api/audio-merges/local/%s/files/%d/chunks/%d", created.ID, fileIndex, chunk), data[start:end], true)
			if rr.Code != http.StatusNoContent {
				t.Fatalf("upload chunk %s f%d c%d=%d: %s", created.ID, fileIndex, chunk, rr.Code, rr.Body.String())
			}
		}
	}
}

func completeLocalMerge(t *testing.T, a *testApp, created localMergeCreated) audioMergeSnapshot {
	t.Helper()
	rr := a.request("POST", "/api/audio-merges/local/"+created.ID+"/complete", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("complete local merge=%d: %s", rr.Code, rr.Body.String())
	}
	return decode[audioMergeSnapshot](t, rr)
}

func vttFixture(text string) []byte {
	return []byte("WEBVTT\n\n00:00:00.010 --> 00:00:00.150\n" + text + "\n")
}

func TestLocalAudioMergeEndToEndALAC(t *testing.T) {
	if _, err := exec.LookPath("ffprobe"); err != nil {
		t.Skip("ffprobe not available")
	}
	a := newTestApp(t)
	firstWAV := audioFixture(t, "wav", 440)
	secondWAV := audioFixture(t, "wav", 880)
	firstVTT := vttFixture("第一段字幕")
	secondVTT := vttFixture("第二段字幕")
	files := []localMergeTestFile{
		{Name: "track2.wav", Size: int64(len(secondWAV))},
		{Name: "track10.wav", Size: int64(len(firstWAV))},
		{Name: "track2.vtt", Size: int64(len(secondVTT))},
		{Name: "track10.wav.vtt", Size: int64(len(firstVTT))},
		{Name: "cover.jpg", Size: int64(len(audioCoverFixture(t)))},
	}
	createdRR := createLocalMerge(t, a, "本地合并.m4a", files, []string{"track2.wav", "track10.wav"}, nil)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create local merge=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[localMergeCreated](t, createdRR)
	if created.Status != "uploading" || created.OutputName != "本地合并.m4a" || created.ChunkSize != localMergeChunkSize {
		t.Fatalf("unexpected create response: %+v", created)
	}
	// WAV 自然排序由客户端提供顺序；服务端校验排列。
	if len(created.Files) != 5 {
		t.Fatalf("manifest files=%d", len(created.Files))
	}
	uploadLocalMerge(t, a, created, map[string][]byte{
		"track2.wav": secondWAV, "track10.wav": firstWAV,
		"track2.vtt": secondVTT, "track10.wav.vtt": firstVTT,
		"cover.jpg": audioCoverFixture(t),
	})
	job := completeLocalMerge(t, a, created)
	job = waitAudioMerge(t, a, job)
	if job.Status != "done" || job.Progress != 100 {
		t.Fatalf("local merge did not finish: %+v", job)
	}
	if job.Source != "local" || job.OutputFormat != "alac" {
		t.Fatalf("unexpected job snapshot: %+v", job)
	}
	merged, err := a.srv.file(context.Background(), job.OutputFileID)
	if err != nil {
		t.Fatal(err)
	}
	if merged.Status != "ready" || merged.MimeType != "audio/mp4" || merged.Size <= 0 {
		t.Fatalf("merged file=%+v", merged)
	}
	// 最终 M4A 必须是唯一进入对象存储的数据对象（另有一个封面缩略图）。
	blobKeys := 0
	for key := range a.store.raw {
		if strings.HasPrefix(key, "blobs/") {
			blobKeys++
		}
	}
	if blobKeys != 1 {
		t.Fatalf("expected exactly one blobs/ object, found %d: %v", blobKeys, keys(a.store.raw))
	}
	if len(a.store.multipart) != 0 {
		t.Fatalf("local merge started S3 multipart uploads: %v", a.store.multipart)
	}
	var uploadRows int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM uploads`).Scan(&uploadRows); err != nil {
		t.Fatal(err)
	}
	if uploadRows != 0 {
		t.Fatalf("local merge created %d upload records", uploadRows)
	}
	var fileRows int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE parent_id=? AND status='ready'`, RootID).Scan(&fileRows); err != nil {
		t.Fatal(err)
	}
	if fileRows != 1 {
		t.Fatalf("expected only the merged file in the tree, found %d", fileRows)
	}
	// 章节标题必须是 WAV 文件名（去掉扩展名），顺序按客户端指定。
	mediaRR := a.request("GET", "/api/files/"+merged.ID+"/audio", nil, true)
	if mediaRR.Code != http.StatusOK {
		t.Fatalf("audio metadata=%d: %s", mediaRR.Code, mediaRR.Body.String())
	}
	media := decode[audioMediaTestResponse](t, mediaRR)
	if len(media.Chapters) != 2 || media.Chapters[0].Title != "track2" || media.Chapters[1].Title != "track10" {
		t.Fatalf("chapters=%+v", media.Chapters)
	}
	if len(media.Subtitles) != 2 || media.Subtitles[0].Text != "第二段字幕" || media.Subtitles[1].Text != "第一段字幕" {
		t.Fatalf("subtitles=%+v", media.Subtitles)
	}
	if !media.HasCover || media.CoverURL == "" {
		t.Fatalf("cover missing: %+v", media)
	}
	// ALAC 编码校验。
	rc, err := a.store.Open(context.Background(), merged.objectKey)
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(rc)
	rc.Close()
	if err != nil {
		t.Fatal(err)
	}
	path := t.TempDir() + "/local-merged.m4a"
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	probe, err := exec.Command("ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_name", "-of", "default=nw=1:nk=1", path).Output()
	if err != nil || strings.TrimSpace(string(probe)) != "alac" {
		t.Fatalf("output codec=%q err=%v", probe, err)
	}
	// 成功后的暂存目录必须全部清理。
	staging, err := filepath.Glob(filepath.Join(a.srv.cfg.WorkDir, "revaro-local-merge-*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(staging) != 0 {
		t.Fatalf("stale staging directories remain: %v", staging)
	}
}

func keys(m map[string][]byte) []string {
	var out []string
	for key := range m {
		out = append(out, key)
	}
	return out
}

func TestLocalAudioMergeSubtitleNameCompatibility(t *testing.T) {
	a := newTestApp(t)
	// 兼容规则：track.vtt、track.wav.vtt、以及导出器保留旧音频扩展名的 track.mp3.vtt。
	files := []localMergeTestFile{
		{Name: "01 序章.wav", Size: 1}, {Name: "01 序章.vtt", Size: 1},
		{Name: "02 正片.wav", Size: 1}, {Name: "02 正片.wav.vtt", Size: 1},
		{Name: "03 尾声.wav", Size: 1}, {Name: "03 尾声.mp3.vtt", Size: 1},
	}
	createdRR := createLocalMerge(t, a, "兼容字幕.m4a", files, nil, nil)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[localMergeCreated](t, createdRR)
	a.srv.audioMergeMu.RLock()
	job := a.srv.audioMergeJobs[created.ID]
	a.srv.audioMergeMu.RUnlock()
	if job == nil {
		t.Fatal("job missing")
	}
	// 自然排序默认顺序：01 < 02 < 03。
	wantOrder := []string{"01 序章.wav", "02 正片.wav", "03 尾声.wav"}
	for position, fileIndex := range job.audioOrder {
		if job.files[fileIndex].Name != wantOrder[position] {
			t.Fatalf("audio order[%d]=%s want %s", position, job.files[fileIndex].Name, wantOrder[position])
		}
	}
	wantSubtitles := []string{"01 序章.vtt", "02 正片.wav.vtt", "03 尾声.mp3.vtt"}
	for position, fileIndex := range job.subtitleFor {
		if fileIndex < 0 || job.files[fileIndex].Name != wantSubtitles[position] {
			t.Fatalf("subtitle match[%d]=%d want %s", position, fileIndex, wantSubtitles[position])
		}
	}
}

func TestLocalAudioMergeCoverAutoSelection(t *testing.T) {
	a := newTestApp(t)
	audios := []localMergeTestFile{{Name: "track.wav", Size: 1}, {Name: "track2.wav", Size: 1}}
	// 只有一张图片时自动选择。
	createdRR := createLocalMerge(t, a, "单图.m4a", append(audios, localMergeTestFile{Name: "artwork.png", Size: 1}), nil, nil)
	created := decode[localMergeCreated](t, createdRR)
	a.srv.audioMergeMu.RLock()
	job := a.srv.audioMergeJobs[created.ID]
	a.srv.audioMergeMu.RUnlock()
	if job == nil || job.coverIndex < 0 || job.files[job.coverIndex].Name != "artwork.png" {
		t.Fatalf("single-image auto cover failed: %+v", job)
	}
	// 多张图片时优先 cover 命名。
	createdRR = createLocalMerge(t, a, "多图.m4a", append(audios, localMergeTestFile{Name: "photo1.jpg", Size: 1}, localMergeTestFile{Name: "Cover.jpg", Size: 1}), nil, nil)
	created = decode[localMergeCreated](t, createdRR)
	a.srv.audioMergeMu.RLock()
	job = a.srv.audioMergeJobs[created.ID]
	a.srv.audioMergeMu.RUnlock()
	if job == nil || job.coverIndex < 0 || job.files[job.coverIndex].Name != "Cover.jpg" {
		t.Fatalf("cover-named auto selection failed: %+v", job)
	}
	// 多张图片且没有封面名时，不自动选择。
	createdRR = createLocalMerge(t, a, "无封面名.m4a", append(audios, localMergeTestFile{Name: "photo1.jpg", Size: 1}, localMergeTestFile{Name: "photo2.jpg", Size: 1}), nil, nil)
	created = decode[localMergeCreated](t, createdRR)
	a.srv.audioMergeMu.RLock()
	job = a.srv.audioMergeJobs[created.ID]
	a.srv.audioMergeMu.RUnlock()
	if job == nil || job.coverIndex != -1 {
		t.Fatalf("ambiguous covers should not auto-select: %+v", job)
	}
	// 用户明确要求不使用封面。
	noCover := ""
	createdRR = createLocalMerge(t, a, "无封面.m4a", append(audios, localMergeTestFile{Name: "artwork.png", Size: 1}), nil, &noCover)
	created = decode[localMergeCreated](t, createdRR)
	a.srv.audioMergeMu.RLock()
	job = a.srv.audioMergeJobs[created.ID]
	a.srv.audioMergeMu.RUnlock()
	if job == nil || job.coverIndex != -1 {
		t.Fatalf("explicit no-cover failed: %+v", job)
	}
}

func TestLocalAudioMergeFailureCleansStaging(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	a := newTestApp(t)
	badVTT := []byte("这不是 WebVTT 内容\n")
	firstWAV := audioFixture(t, "wav", 440)
	secondWAV := audioFixture(t, "wav", 880)
	files := []localMergeTestFile{
		{Name: "track.wav", Size: int64(len(firstWAV))}, {Name: "track2.wav", Size: int64(len(secondWAV))},
		{Name: "track.vtt", Size: int64(len(badVTT))},
	}
	createdRR := createLocalMerge(t, a, "坏字幕.m4a", files, nil, nil)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[localMergeCreated](t, createdRR)
	uploadLocalMerge(t, a, created, map[string][]byte{
		"track.wav": firstWAV, "track2.wav": secondWAV, "track.vtt": badVTT,
	})
	job := completeLocalMerge(t, a, created)
	job = waitAudioMerge(t, a, job)
	if job.Status != "failed" {
		t.Fatalf("expected failure: %+v", job)
	}
	if !strings.Contains(job.Error, "不是有效的 WebVTT") {
		t.Fatalf("unexpected failure message: %q", job.Error)
	}
	assertNoStaging(t, a)
	var pending int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, job.OutputFileID).Scan(&pending); err != nil {
		t.Fatal(err)
	}
	if pending != 0 {
		t.Fatal("failed merge left its pending placeholder behind")
	}
}

func TestLocalAudioMergeCancelDuringUpload(t *testing.T) {
	a := newTestApp(t)
	first := localMergeContent("a.wav", 100)
	second := localMergeContent("b.wav", 100)
	// 8 MiB 分块，单文件 100 字节只有 1 个分块；只上传第一个文件后取消。
	files := []localMergeTestFile{{Name: "a.wav", Size: int64(len(first))}, {Name: "b.wav", Size: int64(len(second))}}
	createdRR := createLocalMerge(t, a, "取消上传.m4a", files, nil, nil)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[localMergeCreated](t, createdRR)
	rr := a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/0", first, true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("upload chunk=%d: %s", rr.Code, rr.Body.String())
	}
	cancel := a.request("DELETE", "/api/audio-merges/"+created.ID, nil, true)
	if cancel.Code != http.StatusNoContent {
		t.Fatalf("cancel=%d: %s", cancel.Code, cancel.Body.String())
	}
	assertNoStaging(t, a)
	if _, ok := a.srv.audioMergeJobs[created.ID]; ok {
		t.Fatal("cancelled upload job remained in the task map")
	}
	var pending int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, created.OutputFileID).Scan(&pending); err != nil {
		t.Fatal(err)
	}
	if pending != 0 {
		t.Fatal("cancelled upload left its pending placeholder behind")
	}
	// 后续分块必须 404。
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/1/chunks/0", second, true)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("chunk after cancel=%d", rr.Code)
	}
}

func TestLocalAudioMergeDiskSpaceInsufficient(t *testing.T) {
	a := newTestApp(t)
	// 创建时空间不足：直接拒绝。
	a.srv.diskFree = func(string) (int64, error) { return 0, nil }
	files := []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "b.wav", Size: 10}}
	createdRR := createLocalMerge(t, a, "空间不足.m4a", files, nil, nil)
	if createdRR.Code != http.StatusInsufficientStorage {
		t.Fatalf("create with full disk=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	if !strings.Contains(createdRR.Body.String(), "磁盘剩余空间不足") {
		t.Fatalf("missing disk error message: %s", createdRR.Body.String())
	}

	// 上传完成后空间不足：任务必须明确失败并清理。
	a.srv.diskFree = nil
	createdRR = createLocalMerge(t, a, "合并前空间不足.m4a", files, nil, nil)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[localMergeCreated](t, createdRR)
	uploadLocalMerge(t, a, created, map[string][]byte{"a.wav": make([]byte, 10), "b.wav": make([]byte, 10)})
	a.srv.diskFree = func(string) (int64, error) { return 0, nil }
	rr := a.request("POST", "/api/audio-merges/local/"+created.ID+"/complete", nil, true)
	if rr.Code != http.StatusInsufficientStorage {
		t.Fatalf("complete with full disk=%d: %s", rr.Code, rr.Body.String())
	}
	assertNoStaging(t, a)
	if _, ok := a.srv.audioMergeJobs[created.ID]; ok {
		t.Fatal("disk-aborted job remained in the task map")
	}
	var pending int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, created.OutputFileID).Scan(&pending); err != nil {
		t.Fatal(err)
	}
	if pending != 0 {
		t.Fatal("disk-aborted job left its pending placeholder behind")
	}
}

func TestLocalAudioMergeRejectsInvalidInputs(t *testing.T) {
	a := newTestApp(t)
	valid := []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "b.wav", Size: 10}}
	cover := "cover.jpg"
	for name, tc := range map[string]struct {
		files []localMergeTestFile
		order []string
		cover *string
	}{
		"path traversal name":  {files: []localMergeTestFile{{Name: "../evil.wav", Size: 10}, {Name: "b.wav", Size: 10}}},
		"separator in name":    {files: []localMergeTestFile{{Name: "a/b.wav", Size: 10}, {Name: "b.wav", Size: 10}}},
		"duplicate names":      {files: []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "A.WAV", Size: 10}}},
		"zero size":            {files: []localMergeTestFile{{Name: "a.wav", Size: 0}, {Name: "b.wav", Size: 10}}},
		"unknown file type":    {files: []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "notes.txt", Size: 10}}},
		"too few audio":        {files: []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "a.vtt", Size: 10}}},
		"oversized subtitle":   {files: []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "b.wav", Size: 10}, {Name: "b.vtt", Size: maxAudioSubtitleBytes + 1}}},
		"oversized cover":      {files: []localMergeTestFile{{Name: "a.wav", Size: 10}, {Name: "b.wav", Size: 10}, {Name: "cover.jpg", Size: maxAudioCoverSourceBytes + 1}}},
		"order has extra":      {files: valid, order: []string{"a.wav", "b.wav", "a.wav"}},
		"order missing file":   {files: valid, order: []string{"a.wav"}},
		"order unknown file":   {files: valid, order: []string{"a.wav", "nope.wav"}},
		"cover outside folder": {files: valid, cover: &cover},
		"wrong output format":  {files: valid},
	} {
		t.Run(name, func(t *testing.T) {
			output := "测试.m4a"
			if name == "wrong output format" {
				output = "测试.flac"
			}
			rr := createLocalMerge(t, a, output, tc.files, tc.order, tc.cover)
			if rr.Code < 400 || rr.Code >= 500 {
				t.Fatalf("invalid input accepted: %d %s", rr.Code, rr.Body.String())
			}
		})
	}
	assertNoStaging(t, a)
}

func TestLocalAudioMergeChunkValidation(t *testing.T) {
	a := newTestApp(t)
	files := []localMergeTestFile{{Name: "a.wav", Size: 100}, {Name: "b.wav", Size: 100}}
	createdRR := createLocalMerge(t, a, "分块校验.m4a", files, nil, nil)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[localMergeCreated](t, createdRR)
	if created.Files[0].ChunkCount != 1 || created.ChunkSize != localMergeChunkSize {
		t.Fatalf("unexpected layout: %+v", created)
	}
	// 分块越界。
	rr := a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/7", make([]byte, 1), true)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("out-of-range chunk=%d", rr.Code)
	}
	// 文件越界。
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/9/chunks/0", make([]byte, 1), true)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("out-of-range file=%d", rr.Code)
	}
	// 大小不符。
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/0", make([]byte, 50), true)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("wrong chunk size=%d", rr.Code)
	}
	// 未知任务。
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+ids.New()+"/files/0/chunks/0", make([]byte, 100), true)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("unknown job chunk=%d", rr.Code)
	}
	// 缺少分块就完成。
	rr = a.request("POST", "/api/audio-merges/local/"+created.ID+"/complete", nil, true)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("complete with missing chunks=%d", rr.Code)
	}
	// 重复上传同一分块是幂等的。
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/0", make([]byte, 100), true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("first chunk=%d", rr.Code)
	}
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/0", make([]byte, 100), true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("retried chunk=%d", rr.Code)
	}
	// 上传完成后不允许再传分块。
	uploadLocalMerge(t, a, created, map[string][]byte{"a.wav": make([]byte, 100), "b.wav": make([]byte, 100)})
	completeLocalMerge(t, a, created)
	rr = a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/0", make([]byte, 100), true)
	if rr.Code != http.StatusConflict {
		t.Fatalf("chunk after completion=%d", rr.Code)
	}
}

func TestLocalMergeConcurrencyLimit(t *testing.T) {
	a := newTestApp(t)
	audio := []localMergeTestFile{{Name: "a.wav", Size: 1}, {Name: "b.wav", Size: 1}}
	var created []localMergeCreated
	for i := 0; i < maxLocalMergeUploadingJobs+1; i++ {
		rr := createLocalMerge(t, a, fmt.Sprintf("并发%d.m4a", i), audio, nil, nil)
		if i < maxLocalMergeUploadingJobs {
			if rr.Code != http.StatusCreated {
				t.Fatalf("create %d=%d: %s", i, rr.Code, rr.Body.String())
			}
			created = append(created, decode[localMergeCreated](t, rr))
		} else if rr.Code != http.StatusTooManyRequests {
			t.Fatalf("expected 429 for the %dth job, got %d", i+1, rr.Code)
		}
	}
	for _, job := range created {
		if rr := a.request("DELETE", "/api/audio-merges/"+job.ID, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("cleanup cancel=%d", rr.Code)
		}
	}
}

func TestLocalMergeNaturalSort(t *testing.T) {
	ordered := []string{"track2.wav", "track10.wav", "01.wav", "1.wav", "b.wav", "A.wav", "第10节.wav", "第2节.wav"}
	want := []string{"1.wav", "01.wav", "A.wav", "b.wav", "track2.wav", "track10.wav", "第2节.wav", "第10节.wav"}
	for i := 0; i < len(ordered); i++ {
		for j := i + 1; j < len(ordered); j++ {
			if naturalLess(ordered[j], ordered[i]) {
				ordered[i], ordered[j] = ordered[j], ordered[i]
			}
		}
	}
	for i := range want {
		if ordered[i] != want[i] {
			t.Fatalf("natural sort mismatch at %d: got %q want %q (full %v)", i, ordered[i], want[i], ordered)
		}
	}
}

func TestLocalMergeDiskBudget(t *testing.T) {
	if !mergeDiskEnough(localMergeMinFreeBytes+100, 100) {
		t.Fatal("sufficient disk rejected")
	}
	if mergeDiskEnough(localMergeMinFreeBytes+99, 100) {
		t.Fatal("insufficient disk accepted")
	}
	if mergeDiskEnough(0, 0) {
		t.Fatal("empty disk accepted")
	}
}

func TestNewCleansStaleLocalMergeStaging(t *testing.T) {
	a := newTestApp(t)
	stale := filepath.Join(a.srv.cfg.WorkDir, "revaro-local-merge-stale-job")
	keep := filepath.Join(a.srv.cfg.WorkDir, "revaro-extract-something")
	for _, dir := range []string{stale, keep} {
		if err := os.MkdirAll(filepath.Join(dir, "chunks"), 0o700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dir, "chunks", "f0-c0.part"), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	_ = New(a.db, a.store, a.srv.auth, a.srv.cfg, nil)
	if _, err := os.Stat(stale); !os.IsNotExist(err) {
		t.Fatalf("stale local merge staging survived restart: %v", err)
	}
	if _, err := os.Stat(keep); err != nil {
		t.Fatalf("unrelated work directory was removed: %v", err)
	}
}

func TestLocalMergeUploadProgressAndSlotRelease(t *testing.T) {
	a := newTestApp(t)
	files := []localMergeTestFile{{Name: "a.wav", Size: 100}, {Name: "b.wav", Size: 100}}
	createdRR := createLocalMerge(t, a, "进度.m4a", files, nil, nil)
	created := decode[localMergeCreated](t, createdRR)
	rr := a.requestRaw("POST", "/api/audio-merges/local/"+created.ID+"/files/0/chunks/0", make([]byte, 100), true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("chunk=%d: %s", rr.Code, rr.Body.String())
	}
	state := a.request("GET", "/api/audio-merges/"+created.ID, nil, true)
	snapshot := decode[audioMergeSnapshot](t, state)
	if snapshot.Status != "uploading" || snapshot.Progress < localMergeUploadProgressStart || snapshot.Progress > localMergeUploadProgressEnd {
		t.Fatalf("upload progress snapshot=%+v", snapshot)
	}
	if !strings.Contains(snapshot.Message, "1 / 2") {
		t.Fatalf("upload message=%q", snapshot.Message)
	}
	// 完成后上传槽位必须释放。
	uploadLocalMerge(t, a, created, map[string][]byte{"a.wav": make([]byte, 100), "b.wav": make([]byte, 100)})
	completeLocalMerge(t, a, created)
	if held := func() bool {
		a.srv.audioMergeMu.RLock()
		defer a.srv.audioMergeMu.RUnlock()
		job := a.srv.audioMergeJobs[created.ID]
		if job == nil {
			return false
		}
		job.mu.Lock()
		defer job.mu.Unlock()
		return job.uploadSlotHeld
	}(); held {
		t.Fatal("upload slot stayed held after completion")
	}
}

func assertNoStaging(t *testing.T, a *testApp) {
	t.Helper()
	staging, err := filepath.Glob(filepath.Join(a.srv.cfg.WorkDir, "revaro-local-merge-*"))
	if err != nil {
		t.Fatal(err)
	}
	if len(staging) != 0 {
		t.Fatalf("staging directories were not cleaned: %v", staging)
	}
}
