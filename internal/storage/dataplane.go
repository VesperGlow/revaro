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
	if body != nil && path != "/v1/backup/object" {
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

func (d *DataPlane) UploadDatabase(ctx context.Context, key string, body io.Reader, size int64) error {
	resp, err := d.request(ctx, http.MethodPut, "/v1/backup/object", url.Values{"key": {key}}, body, size)
	if err == nil {
		resp.Body.Close()
	}
	return err
}
func (d *DataPlane) ListDatabases(ctx context.Context) ([]ObjectRef, error) {
	var out []ObjectRef
	err := d.jsonRequest(ctx, http.MethodGet, "/v1/backup/objects", nil, nil, &out)
	return out, err
}
func (d *DataPlane) DeleteDatabases(ctx context.Context, keys []string) error {
	return d.jsonRequest(ctx, http.MethodDelete, "/v1/backup/objects", nil, map[string]any{"keys": keys}, nil)
}
