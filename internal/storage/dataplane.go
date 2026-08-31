package storage

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// DataPlane is the private Go client for the Rust object data plane. It knows
// nothing about logical files or SQLite and sends large values only as HTTP
// streams, never JSON/base64.
type DataPlane struct {
	base   string
	token  string
	client *http.Client
}

type dataPlaneError struct {
	Status int
	Code   string
	Text   string
}

func (e *dataPlaneError) Error() string { return fmt.Sprintf("data plane: %s (%d)", e.Text, e.Status) }

func NewDataPlane(addr, token string) *DataPlane {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.MaxIdleConns = 64
	transport.MaxIdleConnsPerHost = 32
	transport.IdleConnTimeout = 90 * time.Second
	return &DataPlane{base: "http://" + addr, token: token, client: &http.Client{Transport: transport}}
}

func (d *DataPlane) endpoint(path string, values url.Values) string {
	if len(values) == 0 {
		return d.base + path
	}
	return d.base + path + "?" + values.Encode()
}

func (d *DataPlane) request(ctx context.Context, method, path string, values url.Values, body io.Reader, length int64) (*http.Response, error) {
	req, err := http.NewRequestWithContext(ctx, method, d.endpoint(path, values), body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+d.token)
	if body != nil && path != "/v1/s3/object" && path != "/v1/s3/multipart/upload" {
		req.Header.Set("Content-Type", "application/json")
	}
	if length >= 0 {
		req.ContentLength = length
	}
	resp, err := d.client.Do(req)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return resp, nil
	}
	defer resp.Body.Close()
	var problem struct{ Error, Code string }
	limited, _ := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
	if json.Unmarshal(limited, &problem) != nil || problem.Error == "" {
		problem.Error = strings.TrimSpace(string(limited))
	}
	dpErr := &dataPlaneError{Status: resp.StatusCode, Code: problem.Code, Text: problem.Error}
	if resp.StatusCode == http.StatusNotFound {
		return nil, errors.Join(ErrNotFound, dpErr)
	}
	return nil, dpErr
}

func decodeResponse(resp *http.Response, out any) error {
	defer resp.Body.Close()
	return json.NewDecoder(io.LimitReader(resp.Body, 2<<20)).Decode(out)
}

func jsonBody(value any) (io.Reader, int64, error) {
	data, err := json.Marshal(value)
	return bytes.NewReader(data), int64(len(data)), err
}

func (d *DataPlane) jsonRequest(ctx context.Context, method, path string, values url.Values, in, out any) error {
	var body io.Reader
	var length int64 = 0
	if in != nil {
		var err error
		body, length, err = jsonBody(in)
		if err != nil {
			return err
		}
	}
	resp, err := d.request(ctx, method, path, values, body, length)
	if err != nil {
		return err
	}
	if out == nil {
		resp.Body.Close()
		return nil
	}
	return decodeResponse(resp, out)
}

func (d *DataPlane) Ping(ctx context.Context) error {
	resp, err := d.request(ctx, http.MethodGet, "/v1/s3/ping", nil, nil, 0)
	if err == nil {
		resp.Body.Close()
	}
	return err
}

func seconds(expiry time.Duration) string {
	return strconv.FormatInt(max(1, int64(expiry/time.Second)), 10)
}

func (d *DataPlane) PresignPutObject(ctx context.Context, key, mime string, expiry time.Duration) (string, error) {
	var out struct {
		URL string `json:"url"`
	}
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/s3/presign/put", url.Values{"key": {key}, "mime": {mime}, "expires_seconds": {seconds(expiry)}}, nil, &out)
	return out.URL, err
}

func (d *DataPlane) CreateMultipart(ctx context.Context, key, mime string) (string, error) {
	var out struct {
		UploadID string `json:"upload_id"`
	}
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/s3/multipart", nil, map[string]any{"key": key, "mime": mime}, &out)
	return out.UploadID, err
}

func (d *DataPlane) PresignUploadPart(ctx context.Context, key, uploadID string, partNumber int32, expiry time.Duration) (string, error) {
	var out struct {
		URL string `json:"url"`
	}
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/s3/multipart/part", url.Values{"key": {key}, "upload_id": {uploadID}, "part_number": {strconv.Itoa(int(partNumber))}, "expires_seconds": {seconds(expiry)}}, nil, &out)
	return out.URL, err
}

func (d *DataPlane) CompleteMultipart(ctx context.Context, key, uploadID string, parts []CompletedPart) (ObjectInfo, error) {
	var out ObjectInfo
	err := d.jsonRequest(ctx, http.MethodPut, "/v1/s3/multipart", nil, map[string]any{"key": key, "upload_id": uploadID, "parts": parts}, &out)
	return out, err
}

func (d *DataPlane) AbortMultipart(ctx context.Context, key, uploadID string) error {
	return d.jsonRequest(ctx, http.MethodDelete, "/v1/s3/multipart", nil, map[string]any{"key": key, "upload_id": uploadID}, nil)
}

func (d *DataPlane) HeadObject(ctx context.Context, key string) (ObjectInfo, error) {
	var out ObjectInfo
	err := d.jsonRequest(ctx, http.MethodGet, "/v1/s3/object/info", url.Values{"key": {key}}, nil, &out)
	return out, err
}

func (d *DataPlane) OpenRange(ctx context.Context, key string, start, end int64) (io.ReadCloser, error) {
	resp, err := d.request(ctx, http.MethodGet, "/v1/s3/object", url.Values{"key": {key}, "start": {strconv.FormatInt(start, 10)}, "end": {strconv.FormatInt(end, 10)}}, nil, 0)
	if err != nil {
		return nil, err
	}
	return resp.Body, nil
}

func (d *DataPlane) Open(ctx context.Context, key string) (ReadSeekCloserAt, error) {
	return openObject(ctx, d, key)
}
func (d *DataPlane) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	return d.GetObject(ctx, key, limit)
}

func (d *DataPlane) put(ctx context.Context, key, mime string, body io.Reader, size int64, immutable bool) (ObjectInfo, error) {
	values := url.Values{"key": {key}, "mime": {mime}, "size": {strconv.FormatInt(size, 10)}}
	if immutable {
		values.Set("immutable", "true")
	}
	resp, err := d.request(ctx, http.MethodPut, "/v1/s3/object", values, body, size)
	if err != nil {
		return ObjectInfo{}, err
	}
	var out ObjectInfo
	return out, decodeResponse(resp, &out)
}

func (d *DataPlane) uploadPart(ctx context.Context, key, uploadID string, partNumber int32, body io.Reader, size int64) (CompletedPart, error) {
	values := url.Values{"key": {key}, "upload_id": {uploadID}, "part_number": {strconv.Itoa(int(partNumber))}, "size": {strconv.FormatInt(size, 10)}}
	resp, err := d.request(ctx, http.MethodPut, "/v1/s3/multipart/upload", values, body, size)
	if err != nil {
		return CompletedPart{}, err
	}
	var out ObjectInfo
	if err := decodeResponse(resp, &out); err != nil {
		return CompletedPart{}, err
	}
	return CompletedPart{PartNumber: partNumber, ETag: out.ETag}, nil
}

func (d *DataPlane) StoreBlob(ctx context.Context, key, mime string, body io.Reader, size int64) (ObjectInfo, error) {
	values := url.Values{"key": {key}, "mime": {mime}}
	if size >= 0 {
		values.Set("size", strconv.FormatInt(size, 10))
	}
	resp, err := d.request(ctx, http.MethodPut, "/v1/s3/blob", values, body, size)
	if err != nil {
		return ObjectInfo{}, err
	}
	var out ObjectInfo
	return out, decodeResponse(resp, &out)
}

func (d *DataPlane) PutObject(ctx context.Context, key, mime string, data []byte) (ObjectInfo, error) {
	return d.put(ctx, key, mime, bytes.NewReader(data), int64(len(data)), false)
}
func (d *DataPlane) PutImmutable(ctx context.Context, key, mime string, data []byte) error {
	_, err := d.put(ctx, key, mime, bytes.NewReader(data), int64(len(data)), true)
	var dp *dataPlaneError
	if errors.As(err, &dp) && dp.Status == http.StatusPreconditionFailed {
		return nil
	}
	return err
}

func (d *DataPlane) OpenRaw(ctx context.Context, key string) (io.ReadCloser, error) {
	resp, err := d.request(ctx, http.MethodGet, "/v1/s3/object", url.Values{"key": {key}}, nil, 0)
	if err != nil {
		return nil, err
	}
	return resp.Body, nil
}

func (d *DataPlane) GetObject(ctx context.Context, key string, limit int64) ([]byte, error) {
	body, err := d.OpenRaw(ctx, key)
	if err != nil {
		return nil, err
	}
	defer body.Close()
	data, err := io.ReadAll(io.LimitReader(body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, ErrObjectTooLarge
	}
	return data, nil
}

func (d *DataPlane) DeleteObject(ctx context.Context, key string) error {
	if key == "" {
		return nil
	}
	resp, err := d.request(ctx, http.MethodDelete, "/v1/s3/object", url.Values{"key": {key}}, nil, 0)
	if err == nil {
		resp.Body.Close()
	}
	return err
}

func (d *DataPlane) PresignGetObject(ctx context.Context, key, filename, mime string, inline bool, expiry time.Duration) (string, error) {
	var out struct {
		URL string `json:"url"`
	}
	values := url.Values{"key": {key}, "filename": {filename}, "mime": {mime}, "inline": {strconv.FormatBool(inline)}, "expires_seconds": {seconds(expiry)}}
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/s3/presign/get", values, nil, &out)
	return out.URL, err
}

func (d *DataPlane) WalkPrefix(ctx context.Context, prefix string, visit func([]ObjectRef) error) error {
	continuation := ""
	for {
		values := url.Values{"prefix": {prefix}}
		if continuation != "" {
			values.Set("continuation", continuation)
		}
		var page struct {
			Objects []struct {
				Key      string `json:"key"`
				Size     int64  `json:"size"`
				Modified int64  `json:"last_modified_unix_ms"`
			} `json:"objects"`
			Continuation string `json:"continuation"`
		}
		if err := d.jsonRequest(ctx, http.MethodGet, "/v1/s3/objects", values, nil, &page); err != nil {
			return err
		}
		refs := make([]ObjectRef, len(page.Objects))
		for i, item := range page.Objects {
			refs[i] = ObjectRef{Key: item.Key, Size: item.Size, LastModified: time.UnixMilli(item.Modified)}
		}
		if err := visit(refs); err != nil {
			return err
		}
		continuation = page.Continuation
		if continuation == "" {
			return nil
		}
	}
}

func (d *DataPlane) ListPrefix(ctx context.Context, prefix string) ([]ObjectRef, error) {
	var out []ObjectRef
	err := d.WalkPrefix(ctx, prefix, func(page []ObjectRef) error { out = append(out, page...); return nil })
	return out, err
}
func (d *DataPlane) DeleteObjects(ctx context.Context, keys []string) error {
	return d.jsonRequest(ctx, http.MethodDelete, "/v1/s3/objects", nil, map[string]any{"keys": keys}, nil)
}

func (d *DataPlane) ExtractArchive(ctx context.Context, key, jobID string, archiveSize int64, password string) (string, error) {
	var out struct {
		OutputDir string `json:"output_dir"`
	}
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/archive/extract", nil, map[string]any{"key": key, "job_id": jobID, "archive_size": archiveSize, "password": password}, &out)
	var dp *dataPlaneError
	if errors.As(err, &dp) {
		switch dp.Code {
		case "archive_password_required":
			return "", errors.Join(ErrArchivePasswordRequired, err)
		case "archive_wrong_password":
			return "", errors.Join(ErrArchiveWrongPassword, err)
		}
	}
	return out.OutputDir, err
}

func (d *DataPlane) ArchiveProgress(ctx context.Context, jobID string) (ArchiveProgress, error) {
	var out ArchiveProgress
	err := d.jsonRequest(ctx, http.MethodGet, "/v1/archive/"+jobID+"/progress", nil, nil, &out)
	return out, err
}

func (d *DataPlane) CancelArchive(ctx context.Context, jobID string) error {
	return d.jsonRequest(ctx, http.MethodPost, "/v1/archive/"+jobID+"/cancel", nil, nil, nil)
}

func (d *DataPlane) ProbeMedia(ctx context.Context, key string) (MediaProbe, error) {
	var out MediaProbe
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/media/probe", nil, map[string]any{"key": key}, &out)
	return out, err
}

func (d *DataPlane) MediaThumbnail(ctx context.Context, key string, maxDimension int) ([]byte, error) {
	return d.mediaThumbnail(ctx, key, maxDimension, false)
}

func (d *DataPlane) MediaAudioCover(ctx context.Context, key string, maxDimension int) ([]byte, error) {
	return d.mediaThumbnail(ctx, key, maxDimension, true)
}

func (d *DataPlane) mediaThumbnail(ctx context.Context, key string, maxDimension int, attachedPictureOnly bool) ([]byte, error) {
	body, length, err := jsonBody(map[string]any{"key": key, "max_dimension": maxDimension, "attached_picture_only": attachedPictureOnly})
	if err != nil {
		return nil, err
	}
	resp, err := d.request(ctx, http.MethodPost, "/v1/media/thumbnail", nil, body, length)
	if err != nil {
		var dp *dataPlaneError
		if errors.As(err, &dp) && dp.Code == "artwork" {
			return nil, ErrNoCover
		}
		return nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 8<<20+1))
	if err != nil {
		return nil, err
	}
	if len(data) > 8<<20 {
		return nil, ErrObjectTooLarge
	}
	return data, nil
}

func (d *DataPlane) StreamFMP4(ctx context.Context, key string, start float64, includeAudio, transcodeAudio bool) (io.ReadCloser, error) {
	body, length, err := jsonBody(map[string]any{"key": key, "start_seconds": start, "include_audio": includeAudio, "transcode_audio": transcodeAudio})
	if err != nil {
		return nil, err
	}
	resp, err := d.request(ctx, http.MethodPost, "/v1/media/fmp4", nil, body, length)
	if err != nil {
		return nil, err
	}
	return resp.Body, nil
}
func (d *DataPlane) GenerateHLS(ctx context.Context, key, outputDir string, start float64, audioOnly bool) (MediaHLS, error) {
	var out MediaHLS
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/media/hls", nil, map[string]any{"key": key, "output_dir": outputDir, "start_seconds": start, "audio_only": audioOnly}, &out)
	return out, err
}
func (d *DataPlane) HLSJobStatus(ctx context.Context, jobID string) (MediaHLSJobStatus, error) {
	var out MediaHLSJobStatus
	err := d.jsonRequest(ctx, http.MethodGet, "/v1/media/hls/"+url.PathEscape(jobID), nil, nil, &out)
	return out, err
}
func (d *DataPlane) CancelHLSJob(ctx context.Context, jobID string) error {
	return d.jsonRequest(ctx, http.MethodDelete, "/v1/media/hls/"+url.PathEscape(jobID), nil, nil, nil)
}
func (d *DataPlane) MergeAudio(ctx context.Context, inputs, inputNames []string, output, format, title string) (MediaAudioMerge, error) {
	var out MediaAudioMerge
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/media/audio/merge", nil, map[string]any{"inputs": inputs, "input_names": inputNames, "output": output, "format": format, "title": title}, &out)
	return out, err
}
func (d *DataPlane) DecorateAudio(ctx context.Context, input, cover, subtitle string) error {
	return d.jsonRequest(ctx, http.MethodPost, "/v1/media/audio/decorate", nil, map[string]any{"input": input, "cover": emptyNil(cover), "subtitle": emptyNil(subtitle)}, nil)
}

func emptyNil(value string) any {
	if value == "" {
		return nil
	}
	return value
}
func (d *DataPlane) SubtitleWebVTT(ctx context.Context, key, format string, streamIndex *int) ([]byte, error) {
	in := map[string]any{"key": key, "format": format}
	if streamIndex != nil {
		in["stream_index"] = *streamIndex
	}
	body, length, err := jsonBody(in)
	if err != nil {
		return nil, err
	}
	resp, err := d.request(ctx, http.MethodPost, "/v1/media/subtitle", nil, body, length)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 32<<20+1))
	if len(data) > 32<<20 {
		return nil, ErrObjectTooLarge
	}
	return data, err
}

func (d *DataPlane) AddTorrent(ctx context.Context, sourceType, source string, selected []int, paused bool) (TorrentAddResult, error) {
	var out TorrentAddResult
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/bt", nil, map[string]any{"source_type": sourceType, "source": source, "selected": selected, "paused": paused}, &out)
	return out, err
}
func (d *DataPlane) TorrentDetails(ctx context.Context, id int) (TorrentDetails, error) {
	var out TorrentDetails
	err := d.jsonRequest(ctx, http.MethodGet, "/v1/bt/"+strconv.Itoa(id), nil, nil, &out)
	return out, err
}
func (d *DataPlane) TorrentStats(ctx context.Context, id int) (TorrentStats, error) {
	var out TorrentStats
	err := d.jsonRequest(ctx, http.MethodGet, "/v1/bt/"+strconv.Itoa(id)+"/stats", nil, nil, &out)
	return out, err
}
func (d *DataPlane) SelectTorrentFiles(ctx context.Context, id int, files []int) error {
	return d.jsonRequest(ctx, http.MethodPut, "/v1/bt/"+strconv.Itoa(id)+"/selection", nil, map[string]any{"files": files}, nil)
}
func (d *DataPlane) StartTorrent(ctx context.Context, id int) error {
	return d.jsonRequest(ctx, http.MethodPost, "/v1/bt/"+strconv.Itoa(id)+"/start", nil, nil, nil)
}
func (d *DataPlane) PauseTorrent(ctx context.Context, id int) error {
	return d.jsonRequest(ctx, http.MethodPost, "/v1/bt/"+strconv.Itoa(id)+"/pause", nil, nil, nil)
}
func (d *DataPlane) ImportTorrent(ctx context.Context, id int, files []TorrentImportFile) ([]TorrentImportedFile, error) {
	var out []TorrentImportedFile
	err := d.jsonRequest(ctx, http.MethodPost, "/v1/bt/"+strconv.Itoa(id)+"/import", nil, map[string]any{"files": files}, &out)
	return out, err
}
func (d *DataPlane) DeleteTorrent(ctx context.Context, id int) error {
	return d.jsonRequest(ctx, http.MethodDelete, "/v1/bt/"+strconv.Itoa(id), nil, nil, nil)
}

func (d *DataPlane) StreamTorrent(ctx context.Context, id, fileID int, start, end int64) (io.ReadCloser, error) {
	values := url.Values{"start": {strconv.FormatInt(start, 10)}, "end": {strconv.FormatInt(end, 10)}}
	resp, err := d.request(ctx, http.MethodGet, "/v1/bt/"+strconv.Itoa(id)+"/stream/"+strconv.Itoa(fileID), values, nil, 0)
	if err != nil {
		return nil, err
	}
	return resp.Body, nil
}
