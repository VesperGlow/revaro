package storage

import (
	"context"
	"errors"
	"io"
	"time"
)

var ErrObjectTooLarge = errors.New("object exceeds read limit")
var ErrNotFound = errors.New("object not found")
var ErrArchivePasswordRequired = errors.New("archive password is required")
var ErrArchiveWrongPassword = errors.New("archive password is incorrect")

type ObjectInfo struct {
	Size int64  `json:"size"`
	ETag string `json:"etag"`
}
type CompletedPart struct {
	PartNumber int32  `json:"part_number"`
	ETag       string `json:"etag"`
}
type ObjectRef struct {
	Key          string
	Size         int64
	LastModified time.Time
}

type Storage interface {
	Ping(context.Context) error
	PresignPutObject(context.Context, string, string, time.Duration) (string, error)
	CreateMultipart(context.Context, string, string) (string, error)
	PresignUploadPart(context.Context, string, string, int32, time.Duration) (string, error)
	CompleteMultipart(context.Context, string, string, []CompletedPart) (ObjectInfo, error)
	AbortMultipart(context.Context, string, string) error
	HeadObject(context.Context, string) (ObjectInfo, error)
	StoreBlob(context.Context, string, string, io.Reader, int64) (ObjectInfo, error)
	Open(context.Context, string) (ReadSeekCloserAt, error)
	ReadFile(context.Context, string, int64) ([]byte, error)
	PutObject(context.Context, string, string, []byte) (ObjectInfo, error)
	OpenRaw(context.Context, string) (io.ReadCloser, error)
	GetObject(context.Context, string, int64) ([]byte, error)
	DeleteObject(context.Context, string) error
	PresignGetObject(context.Context, string, string, string, bool, time.Duration) (string, error)
	ListPrefix(context.Context, string) ([]ObjectRef, error)
	WalkPrefix(context.Context, string, func([]ObjectRef) error) error
	DeleteObjects(context.Context, []string) error
	PutImmutable(context.Context, string, string, []byte) error
}

type ArchiveProgress struct {
	Phase           string `json:"phase"`
	Entries         int    `json:"entries"`
	ExpandedBytes   int64  `json:"expanded_bytes"`
	DownloadedBytes int64  `json:"downloaded_bytes"`
}

type ArchiveExtractor interface {
	ExtractArchive(context.Context, string, string, int64, string) (string, error)
	ArchiveProgress(context.Context, string) (ArchiveProgress, error)
	CancelArchive(context.Context, string) error
}
type MediaChapter struct {
	Title   string `json:"title"`
	StartMS int64  `json:"start_ms"`
	EndMS   int64  `json:"end_ms"`
}
type MediaSubtitle struct {
	Index    int    `json:"index"`
	Codec    string `json:"codec"`
	Language string `json:"language"`
	Title    string `json:"title"`
	Default  bool   `json:"default"`
	Forced   bool   `json:"forced"`
}
type MediaProbe struct {
	DurationMS   int64           `json:"duration_ms"`
	Container    string          `json:"container"`
	VideoCodec   string          `json:"video_codec"`
	AudioCodec   string          `json:"audio_codec"`
	Width        int             `json:"width"`
	Height       int             `json:"height"`
	Bitrate      int64           `json:"bitrate"`
	FrameRate    string          `json:"frame_rate"`
	VideoProfile string          `json:"video_profile"`
	VideoLevel   int             `json:"video_level"`
	Chapters     []MediaChapter  `json:"chapters"`
	Subtitles    []MediaSubtitle `json:"subtitles"`
}
type MediaEngine interface {
	ProbeMedia(context.Context, string) (MediaProbe, error)
	MediaThumbnail(context.Context, string, int) ([]byte, error)
	StreamFMP4(context.Context, string, float64, bool, bool) (io.ReadCloser, error)
	GenerateHLS(context.Context, string, string, float64, bool) (MediaHLS, error)
	MergeAudio(context.Context, []string, []string, string, string, string) (MediaAudioMerge, error)
	DecorateAudio(context.Context, string, string, string) error
	SubtitleWebVTT(context.Context, string, string, *int) ([]byte, error)
}
type MediaHLS struct {
	DurationMS  int64  `json:"duration_ms"`
	VideoCodec  string `json:"video_codec"`
	AudioCodec  string `json:"audio_codec"`
	Transcoding bool   `json:"transcoding"`
	JobID       string `json:"job_id"`
}
type MediaHLSJobStatus struct {
	Done  bool   `json:"done"`
	Error string `json:"error"`
}
type MediaHLSJobEngine interface {
	HLSJobStatus(context.Context, string) (MediaHLSJobStatus, error)
	CancelHLSJob(context.Context, string) error
}
type MediaAudioMerge struct {
	DurationsMS []int64 `json:"durations_ms"`
	Size        int64   `json:"size"`
}

type TorrentFile struct {
	Components []string `json:"components"`
	Name       string   `json:"name"`
	Length     int64    `json:"length"`
	Included   bool     `json:"included"`
}
type TorrentDetails struct {
	ID          int           `json:"id"`
	InfoHash    string        `json:"info_hash"`
	Name        string        `json:"name"`
	Files       []TorrentFile `json:"files"`
	TotalPieces int           `json:"total_pieces"`
}
type TorrentAddResult struct {
	ID      int            `json:"id"`
	Details TorrentDetails `json:"details"`
}
type TorrentStats struct {
	ProgressBytes int64 `json:"progress_bytes"`
	TotalBytes    int64 `json:"total_bytes"`
	DownloadSpeed int64 `json:"download_speed"`
	Peers         int   `json:"peers"`
	Finished      bool  `json:"finished"`
}
type TorrentImportFile struct {
	Index int    `json:"index"`
	Key   string `json:"key"`
	MIME  string `json:"mime"`
	Size  int64  `json:"size"`
}
type TorrentImportedFile struct {
	Index int    `json:"index"`
	Key   string `json:"key"`
	Size  int64  `json:"size"`
	ETag  string `json:"etag"`
}
type TorrentEngine interface {
	AddTorrent(context.Context, string, string, []int, bool) (TorrentAddResult, error)
	TorrentDetails(context.Context, int) (TorrentDetails, error)
	TorrentStats(context.Context, int) (TorrentStats, error)
	SelectTorrentFiles(context.Context, int, []int) error
	StartTorrent(context.Context, int) error
	PauseTorrent(context.Context, int) error
	ImportTorrent(context.Context, int, []TorrentImportFile) ([]TorrentImportedFile, error)
	DeleteTorrent(context.Context, int) error
	StreamTorrent(context.Context, int, int, int64, int64) (io.ReadCloser, error)
}

const storeBlobMultipartThreshold = int64(64 << 20)
const storeBlobDefaultPartSize = int64(16 << 20)
const storeBlobUnknownPartSize = int64(128 << 20)

func storeBlobPartSize(size int64) (int64, error) {
	if size < 0 {
		return storeBlobUnknownPartSize, nil
	}
	partSize := storeBlobDefaultPartSize
	if needed := (size + 9999) / 10000; needed > partSize {
		partSize = ((needed + (1 << 20) - 1) >> 20) << 20
	}
	return partSize, nil
}
func BlobKey(id string) string { return "blobs/" + id }
func ValidMultipartPartCount(size, partSize int64) (int, error) {
	if size < 0 || partSize <= 0 {
		return 0, errors.New("invalid multipart size")
	}
	if size == 0 {
		return 0, nil
	}
	parts := (size + partSize - 1) / partSize
	if parts > 10000 {
		return 0, errors.New("multipart upload exceeds 10000 parts")
	}
	return int(parts), nil
}

func IsNotFound(err error) bool {
	if errors.Is(err, ErrNotFound) {
		return true
	}
	var problem *dataPlaneError
	return errors.As(err, &problem) && problem.Status == 404
}
