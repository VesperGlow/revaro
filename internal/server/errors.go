package server

import (
	"errors"
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
