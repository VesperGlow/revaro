package server

import (
	"context"
	"io"
	"net/http"
	"strconv"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

// Upload URLs are authenticated application routes, never public object URLs.
func (s *Server) uploadContent(w http.ResponseWriter, r *http.Request) {
	release, err := s.lockUpload(r.Context(), chi.URLParam(r, "id"))
	if err != nil {
		problem(w, 499, "upload cancelled")
		return
	}
	defer release()
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" || u.expired(time.Now()) {
		problem(w, 404, "pending upload not found")
		return
	}
	size := u.ExpectedSize
	part := int64(0)
	if u.Mode == "multipart" {
		part, err = strconv.ParseInt(chi.URLParam(r, "part"), 10, 32)
		count, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
		if err != nil || part < 1 || part > int64(count) {
			problem(w, 400, "invalid part number")
			return
		}
		size = u.PartSize
		if part == int64(count) {
			size = u.ExpectedSize - (part-1)*u.PartSize
		}
	} else if chi.URLParam(r, "part") != "" {
		problem(w, 400, "single upload has no parts")
		return
	}
	if r.ContentLength >= 0 && r.ContentLength != size {
		problem(w, 400, "upload size mismatch")
		return
	}
	_ = http.NewResponseController(w).SetReadDeadline(time.Now().Add(30 * time.Minute))
	r.Body = http.MaxBytesReader(w, r.Body, size+1)
	var info storage.ObjectInfo
	if part > 0 {
		uploader, ok := s.storage.(interface {
			UploadPart(context.Context, string, string, int32, io.Reader, int64) (storage.ObjectInfo, error)
		})
		if !ok {
			problem(w, 500, "local multipart storage unavailable")
			return
		}
		info, err = uploader.UploadPart(r.Context(), u.ObjectKey, u.MultipartID, int32(part), r.Body, size)
	} else {
		info, err = s.objects.Stream(r.Context(), u.ObjectKey, u.MimeType, r.Body, size)
	}
	if err != nil {
		s.log.Warn("local upload failed", "upload", u.ID, "error", err)
		problem(w, 400, "file write failed or size mismatch")
		return
	}
	w.Header().Set("ETag", info.ETag)
	w.WriteHeader(http.StatusNoContent)
}
