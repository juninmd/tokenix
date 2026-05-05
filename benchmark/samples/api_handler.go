// Package api provides HTTP handlers for the REST API.
// Registers routes under /api/v1 and handles authentication, pagination,
// validation, and error formatting.
package api

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strconv"
	"strings"
	"time"
)

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

// APIError is the canonical error shape returned to callers.
type APIError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Detail  string `json:"detail,omitempty"`
}

func (e *APIError) Error() string { return fmt.Sprintf("[%d] %s", e.Code, e.Message) }

// ErrNotFound signals a 404 response.
var ErrNotFound = &APIError{Code: 404, Message: "not found"}

// ErrUnauthorized signals a 401 response.
var ErrUnauthorized = &APIError{Code: 401, Message: "unauthorized"}

// ErrForbidden signals a 403 response.
var ErrForbidden = &APIError{Code: 403, Message: "forbidden"}

// ValidationError collects field-level errors.
type ValidationError struct {
	Fields map[string]string
}

func (v *ValidationError) Error() string {
	msgs := make([]string, 0, len(v.Fields))
	for k, m := range v.Fields {
		msgs = append(msgs, fmt.Sprintf("%s: %s", k, m))
	}
	return strings.Join(msgs, "; ")
}

// ---------------------------------------------------------------------------
// Request / response helpers
// ---------------------------------------------------------------------------

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, err error) {
	var apiErr *APIError
	if errors.As(err, &apiErr) {
		writeJSON(w, apiErr.Code, apiErr)
		return
	}
	var valErr *ValidationError
	if errors.As(err, &valErr) {
		writeJSON(w, http.StatusUnprocessableEntity, map[string]any{
			"code":    422,
			"message": "validation failed",
			"fields":  valErr.Fields,
		})
		return
	}
	slog.Error("internal error", "err", err)
	writeJSON(w, http.StatusInternalServerError, &APIError{Code: 500, Message: "internal server error"})
}

func readJSON(r *http.Request, v any) error {
	if r.ContentLength == 0 {
		return &APIError{Code: 400, Message: "request body required"}
	}
	dec := json.NewDecoder(r.Body)
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		return &APIError{Code: 400, Message: "invalid JSON", Detail: err.Error()}
	}
	return nil
}

func paginationParams(r *http.Request) (page, pageSize int, err error) {
	page, _ = strconv.Atoi(r.URL.Query().Get("page"))
	if page < 1 {
		page = 1
	}
	pageSize, _ = strconv.Atoi(r.URL.Query().Get("page_size"))
	switch {
	case pageSize < 1:
		pageSize = 20
	case pageSize > 100:
		return 0, 0, &APIError{Code: 400, Message: "page_size must be ≤ 100"}
	}
	return page, pageSize, nil
}

// ---------------------------------------------------------------------------
// Auth context
// ---------------------------------------------------------------------------

type ctxKey string

const currentUserKey ctxKey = "current_user"

type AuthenticatedUser struct {
	ID   string
	Role string
}

func currentUser(ctx context.Context) (*AuthenticatedUser, bool) {
	u, ok := ctx.Value(currentUserKey).(*AuthenticatedUser)
	return u, ok
}

func setCurrentUser(r *http.Request, u *AuthenticatedUser) *http.Request {
	return r.WithContext(context.WithValue(r.Context(), currentUserKey, u))
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

func loggingMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		rw := &responseWriter{ResponseWriter: w, status: 200}
		next.ServeHTTP(rw, r)
		slog.Info("request",
			"method", r.Method,
			"path", r.URL.Path,
			"status", rw.status,
			"duration_ms", time.Since(start).Milliseconds(),
		)
	})
}

type responseWriter struct {
	http.ResponseWriter
	status int
}

func (rw *responseWriter) WriteHeader(status int) {
	rw.status = status
	rw.ResponseWriter.WriteHeader(status)
}

func authMiddleware(tokenValidator func(string) (*AuthenticatedUser, error)) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			authHeader := r.Header.Get("Authorization")
			if authHeader == "" || !strings.HasPrefix(authHeader, "Bearer ") {
				writeError(w, ErrUnauthorized)
				return
			}
			token := strings.TrimPrefix(authHeader, "Bearer ")
			user, err := tokenValidator(token)
			if err != nil {
				writeError(w, ErrUnauthorized)
				return
			}
			next.ServeHTTP(w, setCurrentUser(r, user))
		})
	}
}

func requireRole(role string) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			u, ok := currentUser(r.Context())
			if !ok {
				writeError(w, ErrUnauthorized)
				return
			}
			if u.Role != role && u.Role != "admin" {
				writeError(w, ErrForbidden)
				return
			}
			next.ServeHTTP(w, r)
		})
	}
}

// ---------------------------------------------------------------------------
// User handlers
// ---------------------------------------------------------------------------

type UserService interface {
	GetUser(ctx context.Context, id string) (*User, error)
	ListUsers(ctx context.Context, page, pageSize int) (*Page[User], error)
	CreateUser(ctx context.Context, input CreateUserInput) (*User, error)
	UpdateUser(ctx context.Context, id string, input UpdateUserInput) (*User, error)
	DeleteUser(ctx context.Context, id string) error
}

type User struct {
	ID        string    `json:"id"`
	Email     string    `json:"email"`
	Username  string    `json:"username"`
	Role      string    `json:"role"`
	CreatedAt time.Time `json:"created_at"`
}

type Page[T any] struct {
	Items     []T `json:"items"`
	Total     int `json:"total"`
	Page      int `json:"page"`
	PageSize  int `json:"page_size"`
	TotalPages int `json:"total_pages"`
}

type CreateUserInput struct {
	Email    string `json:"email"`
	Username string `json:"username"`
	Password string `json:"password"`
	Role     string `json:"role"`
}

type UpdateUserInput struct {
	Username *string `json:"username,omitempty"`
	Role     *string `json:"role,omitempty"`
}

type userHandler struct{ svc UserService }

func (h *userHandler) getUser(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	user, err := h.svc.GetUser(r.Context(), id)
	if err != nil {
		writeError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, user)
}

func (h *userHandler) listUsers(w http.ResponseWriter, r *http.Request) {
	page, pageSize, err := paginationParams(r)
	if err != nil {
		writeError(w, err)
		return
	}
	result, err := h.svc.ListUsers(r.Context(), page, pageSize)
	if err != nil {
		writeError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, result)
}

func (h *userHandler) createUser(w http.ResponseWriter, r *http.Request) {
	var input CreateUserInput
	if err := readJSON(r, &input); err != nil {
		writeError(w, err)
		return
	}
	user, err := h.svc.CreateUser(r.Context(), input)
	if err != nil {
		writeError(w, err)
		return
	}
	writeJSON(w, http.StatusCreated, user)
}

func (h *userHandler) updateUser(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	var input UpdateUserInput
	if err := readJSON(r, &input); err != nil {
		writeError(w, err)
		return
	}
	user, err := h.svc.UpdateUser(r.Context(), id, input)
	if err != nil {
		writeError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, user)
}

func (h *userHandler) deleteUser(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if err := h.svc.DeleteUser(r.Context(), id); err != nil {
		writeError(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

func NewRouter(userSvc UserService, tokenValidator func(string) (*AuthenticatedUser, error)) http.Handler {
	mux := http.NewServeMux()

	uh := &userHandler{svc: userSvc}
	auth := authMiddleware(tokenValidator)
	adminOnly := requireRole("admin")

	mux.Handle("GET /api/v1/users", auth(http.HandlerFunc(uh.listUsers)))
	mux.Handle("POST /api/v1/users", auth(adminOnly(http.HandlerFunc(uh.createUser))))
	mux.Handle("GET /api/v1/users/{id}", auth(http.HandlerFunc(uh.getUser)))
	mux.Handle("PATCH /api/v1/users/{id}", auth(http.HandlerFunc(uh.updateUser)))
	mux.Handle("DELETE /api/v1/users/{id}", auth(adminOnly(http.HandlerFunc(uh.deleteUser))))

	mux.HandleFunc("GET /api/v1/health", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})

	return loggingMiddleware(mux)
}
