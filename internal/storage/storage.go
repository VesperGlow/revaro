package storage

import (
	"context"
	"errors"
	"io"
	"time"
)

var ErrObjectTooLarge = errors.New("object exceeds read limit")
var ErrNotFound = errors.New("object not found")
var ErrNoCover = errors.New("media has no embedded cover")
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
	CreateMultipart(context.Context, string, string) (string, error)
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
	MediaAudioCover(context.Context, string, int) ([]byte, error)
	SubtitleWebVTT(context.Context, string, string, *int) ([]byte, error)
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
