package storage

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/config"
	"github.com/aws/aws-sdk-go-v2/aws"
	awsconfig "github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/credentials"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
	"github.com/aws/smithy-go"
)

var ErrObjectTooLarge = errors.New("object exceeds read limit")

type ObjectInfo struct {
	Size int64
	ETag string
}

type CompletedPart struct {
	PartNumber int32  `json:"part_number"`
	ETag       string `json:"etag"`
}

// ObjectRef describes one listed object, used by the garbage collector.
type ObjectRef struct {
	Key          string
	Size         int64
	LastModified time.Time
}

// Storage is the object-storage control plane. Logical files are opaque whole
// objects under blobs/<UUID>; multipart is transport-only.
type Storage interface {
	Ping(context.Context) error

	// Opaque whole-file blobs. Multipart is transport-only: completion creates
	// one normal S3 object at key, never a persistent set of application blocks.
	PresignPutObject(context.Context, string, string, time.Duration) (string, error)
	CreateMultipart(context.Context, string, string) (string, error)
	PresignUploadPart(context.Context, string, string, int32, time.Duration) (string, error)
	CompleteMultipart(context.Context, string, string, []CompletedPart) (ObjectInfo, error)
	AbortMultipart(context.Context, string, string) error
	HeadObject(context.Context, string) (ObjectInfo, error)
	StoreBlob(context.Context, string, string, io.Reader, int64) (ObjectInfo, error)

	// Reading one opaque blob as a seekable, range-backed stream.
	Open(context.Context, string) (ReadSeekCloserAt, error)
	ReadFile(context.Context, string, int64) ([]byte, error)

	// Raw single objects (avatar, legacy objects, GC cleanup).
	PutObject(context.Context, string, string, []byte) (ObjectInfo, error)
	OpenRaw(context.Context, string) (io.ReadCloser, error)
	GetObject(context.Context, string, int64) ([]byte, error)
	DeleteObject(context.Context, string) error
	PresignGetObject(context.Context, string, string, string, bool, time.Duration) (string, error)
	ListPrefix(context.Context, string) ([]ObjectRef, error)
	WalkPrefix(context.Context, string, func([]ObjectRef) error) error
	DeleteObjects(context.Context, []string) error
	// PutImmutable stores a content-addressed derived object (thumbnail
	// cache); an existing object is never overwritten.
	PutImmutable(context.Context, string, string, []byte) error
}

type S3 struct {
	client  *s3.Client
	presign *s3.PresignClient
	bucket  string
}

func NewS3(ctx context.Context, c config.Config) (*S3, error) {
	return newS3(ctx, c)
}

func newS3(ctx context.Context, c config.Config) (*S3, error) {
	options := []func(*awsconfig.LoadOptions) error{
		awsconfig.WithRegion(c.S3Region),
		awsconfig.WithCredentialsProvider(credentials.NewStaticCredentialsProvider(c.S3AccessKey, c.S3SecretKey, "")),
	}
	if c.S3Endpoint != "" {
		options = append(options, awsconfig.WithBaseEndpoint(c.S3Endpoint))
	}
	awscfg, err := awsconfig.LoadDefaultConfig(ctx, options...)
	if err != nil {
		return nil, fmt.Errorf("load AWS config: %w", err)
	}
	// UpCloud currently rejects the AWS SDK v2 flexible-checksum trailer used
	// by default for PutObject. Limit checksums to operations that require one;
	// TLS plus revaro's SHA-256 content addressing still protects block writes.
	if c.IsUpCloud() {
		awscfg.RequestChecksumCalculation = aws.RequestChecksumCalculationWhenRequired
		awscfg.ResponseChecksumValidation = aws.ResponseChecksumValidationWhenRequired
	}
	client := s3.NewFromConfig(awscfg, func(o *s3.Options) { o.UsePathStyle = c.S3PathStyle })
	presignConfig := awscfg
	if c.S3PublicEndpoint != "" {
		presignConfig.BaseEndpoint = aws.String(c.S3PublicEndpoint)
	}
	presignClient := s3.NewFromConfig(presignConfig, func(o *s3.Options) {
		o.UsePathStyle = c.S3PathStyle
		if c.S3PublicEndpoint != "" {
			o.BaseEndpoint = aws.String(c.S3PublicEndpoint)
		}
	})
	return &S3{
		client: client, presign: s3.NewPresignClient(presignClient), bucket: c.S3Bucket,
	}, nil
}

func (s *S3) Ping(ctx context.Context) error {
	_, err := s.client.HeadBucket(ctx, &s3.HeadBucketInput{Bucket: aws.String(s.bucket)})
	return err
}

func (s *S3) PresignPutObject(ctx context.Context, key, mime string, expiry time.Duration) (string, error) {
	in := &s3.PutObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.presign.PresignPutObject(ctx, in, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

func (s *S3) CreateMultipart(ctx context.Context, key, mime string) (string, error) {
	in := &s3.CreateMultipartUploadInput{Bucket: aws.String(s.bucket), Key: aws.String(key)}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.client.CreateMultipartUpload(ctx, in)
	if err != nil {
		return "", err
	}
	return aws.ToString(out.UploadId), nil
}

func (s *S3) PresignUploadPart(ctx context.Context, key, uploadID string, partNumber int32, expiry time.Duration) (string, error) {
	if partNumber < 1 || partNumber > 10000 {
		return "", errors.New("multipart part number is out of range")
	}
	out, err := s.presign.PresignUploadPart(ctx, &s3.UploadPartInput{
		Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID), PartNumber: aws.Int32(partNumber),
	}, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

func (s *S3) CompleteMultipart(ctx context.Context, key, uploadID string, parts []CompletedPart) (ObjectInfo, error) {
	completed := make([]types.CompletedPart, len(parts))
	for i, part := range parts {
		completed[i] = types.CompletedPart{PartNumber: aws.Int32(part.PartNumber), ETag: aws.String(part.ETag)}
	}
	out, err := s.client.CompleteMultipartUpload(ctx, &s3.CompleteMultipartUploadInput{
		Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID),
		MultipartUpload: &types.CompletedMultipartUpload{Parts: completed},
	})
	if err != nil {
		return ObjectInfo{}, err
	}
	info, err := s.HeadObject(ctx, key)
	if err != nil {
		return ObjectInfo{}, err
	}
	if info.ETag == "" {
		info.ETag = aws.ToString(out.ETag)
	}
	return info, nil
}

func (s *S3) AbortMultipart(ctx context.Context, key, uploadID string) error {
	if uploadID == "" {
		return nil
	}
	_, err := s.client.AbortMultipartUpload(ctx, &s3.AbortMultipartUploadInput{
		Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID),
	})
	return err
}

func (s *S3) HeadObject(ctx context.Context, key string) (ObjectInfo, error) {
	out, err := s.client.HeadObject(ctx, &s3.HeadObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err != nil {
		return ObjectInfo{}, err
	}
	return ObjectInfo{Size: aws.ToInt64(out.ContentLength), ETag: strings.Trim(aws.ToString(out.ETag), `"`)}, nil
}

const (
	storeBlobMultipartThreshold = int64(16 << 20)
	storeBlobDefaultPartSize    = int64(16 << 20)
	storeBlobUnknownPartSize    = int64(128 << 20)
)

func storeBlobPartSize(size int64) (int64, error) {
	if size < 0 {
		// Start small. Subsequent unknown-size parts grow to 128 MiB so a short
		// stream does not reserve a large buffer while a 1 TiB stream still fits
		// within S3's 10,000-part allowance.
		return storeBlobDefaultPartSize, nil
	}
	partSize := storeBlobDefaultPartSize
	if size > partSize*10000 {
		partSize = ((size+9999)/10000 + (1 << 20) - 1) / (1 << 20) * (1 << 20)
	}
	if _, err := ValidMultipartPartCount(size, partSize); err != nil {
		return 0, err
	}
	return partSize, nil
}

func (s *S3) abortMultipartAfterFailure(key, uploadID string, cause error) error {
	abortCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	if err := s.AbortMultipart(abortCtx, key, uploadID); err != nil {
		return errors.Join(cause, fmt.Errorf("abort multipart upload: %w", err))
	}
	return cause
}

func (s *S3) storeBlobMultipart(ctx context.Context, key, mime string, body io.Reader, size int64) (ObjectInfo, error) {
	partSize, err := storeBlobPartSize(size)
	if err != nil {
		return ObjectInfo{}, err
	}
	uploadID, err := s.CreateMultipart(ctx, key, mime)
	if err != nil {
		return ObjectInfo{}, err
	}
	fail := func(err error) (ObjectInfo, error) {
		return ObjectInfo{}, s.abortMultipartAfterFailure(key, uploadID, err)
	}
	completed := make([]CompletedPart, 0, 8)
	buf := make([]byte, int(partSize))
	var total int64
	for partNumber := int32(1); ; partNumber++ {
		if partNumber > 10000 {
			return fail(errors.New("stream exceeds S3 multipart part limit"))
		}
		readSize := partSize
		if size < 0 && partNumber > 1 {
			readSize = storeBlobUnknownPartSize
			if int64(cap(buf)) < readSize {
				buf = make([]byte, int(readSize))
			}
		}
		if size >= 0 {
			remaining := size - total
			if remaining < 0 {
				return fail(fmt.Errorf("stream size exceeds declared size %d", size))
			}
			if remaining == 0 {
				var extra [1]byte
				n, readErr := io.ReadFull(body, extra[:])
				if n != 0 || readErr == nil {
					return fail(fmt.Errorf("stream size exceeds declared size %d", size))
				}
				if readErr != io.EOF {
					return fail(readErr)
				}
				break
			}
			readSize = min(readSize, remaining)
		}
		n, readErr := io.ReadFull(body, buf[:readSize])
		if readErr != nil && readErr != io.EOF && readErr != io.ErrUnexpectedEOF {
			return fail(readErr)
		}
		if n == 0 {
			if size >= 0 && total != size {
				return fail(fmt.Errorf("stream ended at %d bytes, want %d", total, size))
			}
			break
		}
		if size >= 0 && int64(n) != readSize {
			return fail(fmt.Errorf("stream ended at %d bytes, want %d", total+int64(n), size))
		}
		out, uploadErr := s.client.UploadPart(ctx, &s3.UploadPartInput{
			Bucket: aws.String(s.bucket), Key: aws.String(key), UploadId: aws.String(uploadID), PartNumber: aws.Int32(partNumber),
			Body: bytes.NewReader(buf[:n]), ContentLength: aws.Int64(int64(n)),
		})
		if uploadErr != nil {
			return fail(uploadErr)
		}
		completed = append(completed, CompletedPart{PartNumber: partNumber, ETag: aws.ToString(out.ETag)})
		total += int64(n)
		if size < 0 && (readErr == io.EOF || readErr == io.ErrUnexpectedEOF) {
			break
		}
	}
	if len(completed) == 0 {
		if err := s.AbortMultipart(ctx, key, uploadID); err != nil {
			return ObjectInfo{}, err
		}
		return s.StoreBlob(ctx, key, mime, bytes.NewReader(nil), 0)
	}
	info, err := s.CompleteMultipart(ctx, key, uploadID, completed)
	if err != nil {
		return fail(err)
	}
	wantSize := total
	if size >= 0 {
		wantSize = size
	}
	if info.Size != wantSize {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
		defer cancel()
		_ = s.DeleteObject(cleanupCtx, key)
		return ObjectInfo{}, fmt.Errorf("stored blob size %d, want %d", info.Size, wantSize)
	}
	return info, nil
}

func (s *S3) StoreBlob(ctx context.Context, key, mime string, body io.Reader, size int64) (ObjectInfo, error) {
	if size < 0 || size >= storeBlobMultipartThreshold {
		return s.storeBlobMultipart(ctx, key, mime, body, size)
	}
	in := &s3.PutObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key), Body: body, ContentLength: aws.Int64(size)}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.client.PutObject(ctx, in)
	if err != nil {
		return ObjectInfo{}, err
	}
	info, err := s.HeadObject(ctx, key)
	if err != nil {
		return ObjectInfo{}, err
	}
	if info.Size != size {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
		defer cancel()
		_ = s.DeleteObject(cleanupCtx, key)
		return ObjectInfo{}, fmt.Errorf("stored blob size %d, want %d", info.Size, size)
	}
	if info.ETag == "" {
		info.ETag = strings.Trim(aws.ToString(out.ETag), `"`)
	}
	return info, nil
}

func BlobKey(id string) string { return "blobs/" + id }

func ValidMultipartPartCount(size, partSize int64) (int, error) {
	if size < 0 || partSize < 5<<20 {
		return 0, errors.New("invalid multipart size")
	}
	count := int((size + partSize - 1) / partSize)
	if count > 10000 {
		return 0, fmt.Errorf("multipart upload needs %s parts", strconv.Itoa(count))
	}
	return count, nil
}

// putConditional stores an immutable content-addressed object. A concurrent
// upload of identical content loses the race with 412, which is treated as
// success because the object that exists is identical by construction.
func (s *S3) putConditional(ctx context.Context, key, mime string, data []byte) error {
	in := &s3.PutObjectInput{
		Bucket:        aws.String(s.bucket),
		Key:           aws.String(key),
		Body:          bytes.NewReader(data),
		ContentLength: aws.Int64(int64(len(data))),
		IfNoneMatch:   aws.String("*"),
	}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	_, err := s.client.PutObject(ctx, in)
	if err == nil {
		return nil
	}
	var apiErr smithy.APIError
	if errors.As(err, &apiErr) && apiErr.ErrorCode() == "PreconditionFailed" {
		return nil
	}
	return err
}

func (s *S3) ListPrefix(ctx context.Context, prefix string) ([]ObjectRef, error) {
	var out []ObjectRef
	err := s.WalkPrefix(ctx, prefix, func(page []ObjectRef) error { out = append(out, page...); return nil })
	return out, err
}

func (s *S3) WalkPrefix(ctx context.Context, prefix string, visit func([]ObjectRef) error) error {
	paginator := s3.NewListObjectsV2Paginator(s.client, &s3.ListObjectsV2Input{
		Bucket: aws.String(s.bucket),
		Prefix: aws.String(prefix),
	})
	for paginator.HasMorePages() {
		page, err := paginator.NextPage(ctx)
		if err != nil {
			return err
		}
		refs := make([]ObjectRef, 0, len(page.Contents))
		for _, obj := range page.Contents {
			refs = append(refs, ObjectRef{
				Key:          aws.ToString(obj.Key),
				Size:         aws.ToInt64(obj.Size),
				LastModified: aws.ToTime(obj.LastModified),
			})
		}
		if err := visit(refs); err != nil {
			return err
		}
	}
	return ctx.Err()
}

func (s *S3) DeleteObjects(ctx context.Context, keys []string) error {
	if len(keys) == 0 {
		return nil
	}
	if len(keys) > 1000 {
		return errors.New("S3 delete batch exceeds 1000 keys")
	}
	objects := make([]types.ObjectIdentifier, len(keys))
	for i, key := range keys {
		objects[i] = types.ObjectIdentifier{Key: aws.String(key)}
	}
	out, err := s.client.DeleteObjects(ctx, &s3.DeleteObjectsInput{Bucket: aws.String(s.bucket), Delete: &types.Delete{Objects: objects, Quiet: aws.Bool(true)}})
	if err != nil {
		return err
	}
	if len(out.Errors) > 0 {
		return fmt.Errorf("delete object %s: %s", aws.ToString(out.Errors[0].Key), aws.ToString(out.Errors[0].Message))
	}
	return nil
}

func (s *S3) PresignGetObject(ctx context.Context, key, filename, mime string, inline bool, expiry time.Duration) (string, error) {
	disposition := "attachment"
	if inline {
		disposition = "inline"
	}
	disposition += "; filename*=UTF-8''" + strings.ReplaceAll(url.PathEscape(filename), "+", "%20")
	in := &s3.GetObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key), ResponseContentDisposition: aws.String(disposition)}
	if mime != "" {
		in.ResponseContentType = aws.String(mime)
	}
	out, err := s.presign.PresignGetObject(ctx, in, s3.WithPresignExpires(expiry))
	if err != nil {
		return "", err
	}
	return out.URL, nil
}

func (s *S3) PutObject(ctx context.Context, key, mime string, data []byte) (ObjectInfo, error) {
	in := &s3.PutObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key), Body: bytes.NewReader(data), ContentLength: aws.Int64(int64(len(data)))}
	if mime != "" {
		in.ContentType = aws.String(mime)
	}
	out, err := s.client.PutObject(ctx, in)
	if err != nil {
		return ObjectInfo{}, err
	}
	return ObjectInfo{Size: int64(len(data)), ETag: aws.ToString(out.ETag)}, nil
}

func (s *S3) OpenRaw(ctx context.Context, key string) (io.ReadCloser, error) {
	out, err := s.client.GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err != nil {
		return nil, err
	}
	return out.Body, nil
}

func (s *S3) GetObject(ctx context.Context, key string, limit int64) ([]byte, error) {
	body, err := s.OpenRaw(ctx, key)
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

// IsNotFound reports whether the error means the S3 object is absent.
func IsNotFound(err error) bool {
	if err == nil {
		return false
	}
	var apiErr smithy.APIError
	if errors.As(err, &apiErr) {
		switch apiErr.ErrorCode() {
		case "NoSuchKey", "NotFound":
			return true
		}
	}
	return false
}

// PutImmutable stores a content-addressed derived object (thumbnail cache);
// concurrent identical uploads race harmlessly and existing objects are
// never overwritten.
func (s *S3) PutImmutable(ctx context.Context, key, mime string, data []byte) error {
	return s.putConditional(ctx, key, mime, data)
}

// DeleteObject is idempotent: deleting an already-absent key is not an error.
func (s *S3) DeleteObject(ctx context.Context, key string) error {
	if key == "" {
		return nil
	}
	_, err := s.client.DeleteObject(ctx, &s3.DeleteObjectInput{Bucket: aws.String(s.bucket), Key: aws.String(key)})
	if err == nil {
		return nil
	}
	if IsNotFound(err) {
		return nil
	}
	return err
}
