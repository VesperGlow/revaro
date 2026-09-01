package server

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
)

func (s *Server) executeAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, subtitleSources []*File, profile audioOutputProfile, cover []byte) error {
	release, err := s.tasks.Heavy(ctx)
	if err != nil {
		return err
	}
	defer release()
	select {
	case s.audioMergeSlots <- struct{}{}:
		defer func() { <-s.audioMergeSlots }()
	case <-ctx.Done():
		return ctx.Err()
	}
	job.update("preparing", 3, "正在从对象存储准备音频")
	tempRoot := s.cfg.WorkDir
	workDir, err := os.MkdirTemp(tempRoot, "revaro-audio-merge-")
	if err != nil {
		return fmt.Errorf("create audio merge workspace: %w", err)
	}
	defer os.RemoveAll(workDir)

	var totalBytes int64
	for _, input := range inputs {
		totalBytes += input.Size
	}
	var copiedBytes int64
	paths := make([]string, 0, len(inputs))
	for index, input := range inputs {
		if err := ctx.Err(); err != nil {
			return err
		}
		ext := strings.ToLower(filepath.Ext(input.Name))
		if !audioSourceExts[ext] {
			ext = ".audio"
		}
		path := filepath.Join(workDir, fmt.Sprintf("input-%04d%s", index, ext))
		out, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
		if err != nil {
			return err
		}
		reader, err := s.openMergeSource(ctx, input)
		if err != nil {
			out.Close()
			return err
		}
		progressReader := &mergeProgressReader{ctx: ctx, r: reader, onRead: func(n int64) {
			copiedBytes += n
			if totalBytes > 0 {
				job.update("preparing", 3+int(copiedBytes*34/totalBytes), fmt.Sprintf("正在准备第 %d / %d 段", index+1, len(inputs)))
			}
		}}
		n, copyErr := io.CopyBuffer(out, progressReader, make([]byte, 256<<10))
		closeErr := out.Close()
		reader.Close()
		if copyErr != nil {
			return copyErr
		}
		if closeErr != nil {
			return closeErr
		}
		if n != input.Size {
			return fmt.Errorf("audio source %s size mismatch: got %d, want %d", input.ID, n, input.Size)
		}
		paths = append(paths, path)
	}

	return s.encodeMergedAudio(ctx, job, profile, workDir, inputs, paths, subtitleSources, s.openMergeSource, cover)
}

// encodeMergedAudio runs the shared FFmpeg pipeline for an audio merge whose
// source files are already materialized at local paths: chapter metadata,
// cover embedding, subtitle time-shifting, ALAC/FLAC/AAC encoding and finally
// storing the master artifact as a normal blobs/<UUID> object.
func (s *Server) encodeMergedAudio(ctx context.Context, job *audioMergeJob, profile audioOutputProfile, workDir string, inputs []File, paths []string, subtitleSources []*File, openSubtitle func(context.Context, File) (io.ReadCloser, error), cover []byte) error {
	outputPath := filepath.Join(workDir, "merged"+profile.Extension)
	job.update("merging", 40, "Rust media engine 正在按顺序合并音频")
	inputNames := make([]string, len(inputs))
	for index := range inputs {
		inputNames[index] = inputs[index].Name
	}
	merged, err := s.media.MergeAudio(ctx, paths, inputNames, outputPath, profile.Format, job.OutputName)
	if err != nil {
		return err
	}
	durations := make([]time.Duration, len(merged.DurationsMS))
	for index, milliseconds := range merged.DurationsMS {
		durations[index] = time.Duration(milliseconds) * time.Millisecond
	}
	if len(durations) != len(inputs) {
		return errors.New("Rust media engine returned inconsistent audio durations")
	}
	storedSubtitles := make([]storedAudioSubtitle, 0)
	subtitlePath := ""
	if profile.Subtitles {
		storedSubtitles, err = s.prepareMergedSubtitles(ctx, subtitleSources, durations, openSubtitle)
		if err != nil {
			return err
		}
		if len(storedSubtitles) > 0 {
			subtitlePath, err = writeMergedWebVTT(workDir, storedSubtitles)
			if err != nil {
				return err
			}
		}
	}
	coverPath := ""
	if len(cover) > 0 {
		embeddedCover, coverErr := resizeToJPEG(cover, 2048)
		if coverErr != nil {
			return fmt.Errorf("prepare embedded audio cover: %w", coverErr)
		}
		coverPath = filepath.Join(workDir, "embedded-cover.jpg")
		if err := os.WriteFile(coverPath, embeddedCover, 0o600); err != nil {
			return fmt.Errorf("write embedded audio cover: %w", err)
		}
	}
	if coverPath != "" || subtitlePath != "" {
		job.update("merging", 82, "正在嵌入封面与字幕")
		if err := s.media.DecorateAudio(ctx, outputPath, coverPath, subtitlePath); err != nil {
			return err
		}
	}
	job.update("merging", 86, "音频母版编码完成")
	return s.finalizeAudioMerge(ctx, job, outputPath, profile, cover, inputs, durations, storedSubtitles)
}

// finalizeAudioMerge uploads the encoded master to object storage as a normal
// blobs/<UUID> object, stores the cover thumbnail and commits the file record
// with chapters and subtitle metadata.
func (s *Server) finalizeAudioMerge(ctx context.Context, job *audioMergeJob, outputPath string, profile audioOutputProfile, cover []byte, inputs []File, durations []time.Duration, storedSubtitles []storedAudioSubtitle) error {
	job.update("saving", 88, "正在写入 Revaro 对象存储")
	key, masterSize, masterETag, contentHash, err := s.storeAudioArtifact(ctx, outputPath, profile.MimeType, 88, 99, "正在保存合并音频", job)
	if err != nil {
		return fmt.Errorf("store merged audio: %w", err)
	}
	committed := false
	defer func() {
		if !committed {
			s.discardBlobs([]string{key, audioThumbnailKey(key)})
		}
	}()
	if len(cover) > 0 {
		thumb, err := resizeToJPEG(cover, thumbMaxDim)
		if err != nil {
			return fmt.Errorf("prepare audio cover thumbnail: %w", err)
		}
		if err := s.objects.PutImmutable(ctx, audioThumbnailKey(key), "image/jpeg", thumb); err != nil {
			return fmt.Errorf("store audio cover thumbnail: %w", err)
		}
	}
	chaptersJSON, err := json.Marshal(buildStoredAudioChapters(inputs, durations))
	if err != nil {
		return fmt.Errorf("encode audio chapters: %w", err)
	}
	subtitlesJSON, err := json.Marshal(storedSubtitles)
	if err != nil {
		return fmt.Errorf("encode audio subtitles: %w", err)
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	result, err := tx.ExecContext(ctx, `UPDATE files SET object_key=?,size=?,mime_type=?,etag=?,content_hash=?,hash_algorithm=?,status='ready',updated_at=? WHERE id=? AND status='pending'`, key, masterSize, profile.MimeType, masterETag, contentHash, contentHashAlgorithm, now, job.OutputFileID)
	if err != nil {
		return fmt.Errorf("commit merged audio file: %w", err)
	}
	if changed, _ := result.RowsAffected(); changed == 0 {
		return errors.New("merged audio placeholder was removed")
	}
	_, err = tx.ExecContext(ctx, `INSERT INTO audio_media(file_id,duration_ms,chapters_json,subtitles_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)`,
		job.OutputFileID, durationMilliseconds(durations), string(chaptersJSON), string(subtitlesJSON), key, masterSize, masterETag, len(cover) > 0, now, now)
	if err != nil {
		return fmt.Errorf("commit audio chapters and stream: %w", err)
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	committed = true
	return nil
}

func (s *Server) storeAudioArtifact(ctx context.Context, path, mimeType string, progressStart, progressEnd int, message string, job *audioMergeJob) (string, int64, string, string, error) {
	input, err := os.Open(path)
	if err != nil {
		return "", 0, "", "", err
	}
	defer input.Close()
	info, err := input.Stat()
	if err != nil {
		return "", 0, "", "", err
	}
	if info.Size() <= 0 {
		return "", 0, "", "", errors.New("audio encoder produced an empty file")
	}
	var storedBytes int64
	hasher := sha256.New()
	reader := &mergeProgressReader{ctx: ctx, r: io.TeeReader(input, hasher), onRead: func(n int64) {
		storedBytes += n
		job.update("saving", progressStart+int(storedBytes*int64(progressEnd-progressStart)/info.Size()), message)
	}}
	key, stored, err := s.storeBlob(ctx, reader, info.Size(), mimeType)
	if err != nil {
		return "", 0, "", "", err
	}
	streamHash := hex.EncodeToString(hasher.Sum(nil))
	objectHash, err := s.hashObject(ctx, key, stored.Size)
	if err != nil || objectHash != streamHash {
		s.discardBlob(key)
		if err == nil {
			err = errors.New("stored audio hash mismatch")
		}
		return "", 0, "", "", err
	}
	return key, stored.Size, stored.ETag, objectHash, nil
}

func (s *Server) openMergeSource(ctx context.Context, f File) (io.ReadCloser, error) {
	return s.objects.Open(ctx, f.objectKey)
}

type mergeProgressReader struct {
	ctx    context.Context
	r      io.Reader
	onRead func(int64)
}

func (r *mergeProgressReader) Read(p []byte) (int, error) {
	if err := r.ctx.Err(); err != nil {
		return 0, err
	}
	n, err := r.r.Read(p)
	if n > 0 && r.onRead != nil {
		r.onRead(int64(n))
	}
	return n, err
}

func buildStoredAudioChapters(inputs []File, durations []time.Duration) []storedAudioChapter {
	chapters := make([]storedAudioChapter, 0, len(inputs))
	var start int64
	for index, input := range inputs {
		end := start + durations[index].Milliseconds()
		chapters = append(chapters, storedAudioChapter{
			Title:   strings.TrimSuffix(input.Name, filepath.Ext(input.Name)),
			StartMS: start,
			EndMS:   max(end, start+1),
		})
		start = end
	}
	return chapters
}
