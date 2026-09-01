package server

import (
	"errors"
	"net/http"
)

// AppError is the shared boundary error. Cause is logged internally and never
// serialized; Message is safe for tasks and API clients.
type AppError struct {
	Code           string
	Message        string
	Cause          error
	Retryable      bool
	ActionRequired bool
}

func (e *AppError) Error() string {
	if e.Cause != nil {
		return e.Code + ": " + e.Cause.Error()
	}
	return e.Code + ": " + e.Message
}

func (e *AppError) Unwrap() error { return e.Cause }

func appError(code, message string, cause error, retryable bool) *AppError {
	return &AppError{Code: code, Message: message, Cause: cause, Retryable: retryable}
}

func publicError(err error, fallback string) string {
	var app *AppError
	if errors.As(err, &app) && app.Message != "" {
		return app.Message
	}
	return fallback
}

func problemError(w http.ResponseWriter, status int, err error) {
	var app *AppError
	if errors.As(err, &app) {
		writeJSON(w, status, map[string]any{"error": map[string]any{
			"status": status, "code": app.Code, "message": app.Message,
			"retryable": app.Retryable, "action_required": app.ActionRequired,
		}})
		return
	}
	problemCode(w, status, "internal_error", "操作失败，请稍后重试")
}
