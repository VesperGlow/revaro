# Revaro Go Backend → Rust Port: Porting Inventory & Audit

> **Status:** read-only source audit. Every fact below was taken from the source at the paths cited; nothing was compiled (no Go toolchain in this environment).
> **Repository root:** `/config/revaro` · **Go module:** `github.com/VesperGlow/revaro` · **Go version:** 1.26 (`go.mod`)
> **Scope audited:** `cmd/server` and all packages under `internal/` (the Rust `data-plane/` is referenced only as an external dependency; `web/` is out of scope).
> **Audience:** engineers re-implementing the backend in Rust, and reviewers of that work.

---

## 0. Repository map and size metrics

```
/config/revaro
├── cmd/server/main.go                 # process entrypoint + reset-admin subcommand
├── internal/
│   ├── auth/        auth.go totp.go            # credentials, sessions, TOTP
│   ├── cache/       cache.go                   # unified global cache manager (L1/L2/LRU)
│   ├── config/      config.go                  # env config
│   ├── database/    database.go migrations/001_local_product.sql 002_file_cleanup.sql
│   ├── dataplane/   process.go                 # spawn/supervise the Rust sidecar
│   ├── ids/         ids.go                     # UUIDv4 generator
│   ├── reader/      reader.go epub_parse.go epub_render.go text_reader.go cache.go
│   │   └── flow/    anchor.go manifest.go objects.go build.go
│   ├── server/      30 non-test .go files      # chi router, all HTTP handlers, managers
│   ├── storage/     storage.go local.go dataplane.go reader.go   # object store + client
│   └── webui/       webui.go                   # //go:embed dist/* SPA
├── data-plane/      Rust axum/ffmpeg/libarchive sidecar (separate loopback process)
└── docs/            reader-flow.md data-plane.md ci.md
```

Non-test / test line counts (computed with `wc -l`):

| Package | non-test files | non-test LOC | test files | test LOC |
|---|---:|---:|---:|---:|
| `cmd/server` | 1 | 203 | 1 | 38 |
| `internal/auth` | 2 | 918 | 1 | 314 |
| `internal/cache` | 1 | 980 | 1 | 378 |
| `internal/config` | 1 | 144 | 1 | 41 |
| `internal/database` | 1 | 102 | 2 | 181 |
| `internal/dataplane` | 1 | 105 | 1 | 30 |
| `internal/ids` | 1 | 28 | 1 | 37 |
| `internal/reader` | 9 | 2 384 | 2 | 1 139 |
| `internal/reader/flow` | 4 | 1 166 | 1 | 848 |
| `internal/server` | 30 | 6 664 | 23 | 3 603 |
| `internal/storage` | 4 | 729 | 2 | 209 |
| `internal/webui` | 1 | 35 | 1 | 83 |
| **Total** | **56** | **≈13 460** | **36** | **≈6 900** |

Third-party modules (`go.mod`): `github.com/go-chi/chi/v5`, `github.com/pquerna/otp`, `golang.org/x/crypto` (argon2), `golang.org/x/image` (bmp/webp/draw), `golang.org/x/net` (html parser), `golang.org/x/sync` (singleflight), `golang.org/x/sys` (unix), `golang.org/x/text` (GBK), `modernc.org/sqlite` (pure-Go SQLite). Indirect: barcode, go-humanize, google/uuid, go-isatty, go-strftime, bigfft, testify.

---

## 1. Module inventory

### 1.1 `internal/auth` — 2 files, 918 LOC
Responsibilities: administrator credential storage (Argon2id), session token lifecycle, and the full TOTP second-factor state machine. Owns no HTTP; the server package calls into it.

Exported API (`auth.go`):
- `ErrInvalidCredentials = errors.New("invalid credentials")` (`auth.go:23`)
- `InitialCredentials{Created, Generated bool; Username, Password string}` (`auth.go:30-35`)
- `Params{Memory, Iterations uint32; Parallelism uint8; SaltLength, KeyLength uint32}` (`auth.go:37-43`)
- `DefaultParams = {Memory: 64*1024, Iterations: 3, Parallelism: 2, SaltLength: 16, KeyLength: 32}` (`auth.go:45`)
- `Service{DB *sql.DB; Username string; Params Params; Now func() time.Time}` (`auth.go:61-66`)
- methods: `Initialize(ctx, user, pass)`, `Login(ctx, user, pass, secondFactor) (token, expires, err)`, `ChangeCredentials(ctx, curUser, curPass, newUser, newPass)`, `ChangeUsername(ctx, newName)`, `ResetCredentials(ctx, user)`, `Authenticate(ctx, token) (username, err)`, `Logout(ctx, token)`, `Cleanup(ctx)` (`auth.go:68,112,142,206,225,260,277,282`)
- package funcs: `TokenHash(token) string`, `HashPassword(pass, Params) (string, error)`, `VerifyPassword(pass, encoded) (bool, error)` (`auth.go:350,355,367`)

TOTP API (`totp.go`): `TOTPSetup{Secret, URI}`, `TOTPStatus{Enabled bool; RecoveryCodes int}`, errors `ErrTOTPRequired`, `ErrInvalidSecondFactor`, `ErrTOTPAlreadyEnabled`, `ErrTOTPNotEnabled`, `ErrTOTPSetupExpired` (`totp.go:39-55`); methods `TOTPStatus`, `BeginTOTPSetup`, `ConfirmTOTPSetup`, `RegenerateRecoveryCodes`, `DisableTOTP` (`totp.go:73,99,137,214,236`).

External deps: stdlib `crypto/{rand,sha256,subtle,aes,cipher}`, `encoding/{base32,base64,json}`, `database/sql`; third-party `golang.org/x/crypto/argon2`, `github.com/pquerna/otp` + `/totp`.

### 1.2 `internal/cache` — 1 file, 980 LOC
Responsibilities: one process-wide byte-budgeted cache manager with per-class namespaces, memory L1 + disk L2, TTL, priority/soft-quota eviction, hand-rolled singleflight, external-provider registration, metrics. (Detailed in §7.1.)

Exported API: `Class`, `ClassStats`, `ExternalStats`, `ExternalBudget`, `Stats`, `Manager`; `New(diskDir, memoryLimit, diskLimit)`, `RegisterClass`, `RegisterExternal`, `Load`, `Has`, `Put`, `Delete`, `Invalidate`, `Prune`, `Stats`, `Close`. Uses only stdlib. Consumed by `internal/server` and `internal/reader`.

### 1.3 `internal/config` — 1 file, 144 LOC
Responsibilities: load/validate environment configuration. `Config` fields and env vars are tabulated in §5.1. Exported: `Config`, `Load()`, `Config.DatabasePath()`. Stdlib only (`net/netip`, `net/url`, `os`, `time`).

### 1.4 `internal/database` — 1 file + 2 migrations, 102 LOC
Responsibilities: open the SQLite file with security/perf pragmas and run embedded migrations. Exported: `Open(path) (*sql.DB, error)`. `//go:embed migrations/*.sql` (`database.go:19-20`). Third-party: `modernc.org/sqlite`. Details in §3.1 and §5.8.

### 1.5 `internal/dataplane` — 1 file, 105 LOC
Responsibilities: generate the sidecar bearer token, launch the Rust data-plane binary with `REVARO_DATA_PLANE_ADDR`/`REVARO_DATA_PLANE_TOKEN`, poll `/v1/health` until `{"status":"ok","protocol":1}`, and terminate the child process group. Exported: `Process`, `Start(ctx, binary, addr, logger)`, `Token()`, `Addr()`, `Done()`, `Close()`. OS-specific (`syscall.SysProcAttr{Setpgid}`, `syscall.Kill`). Details in §5.5.

### 1.6 `internal/ids` — 1 file, 28 LOC
Responsibilities: dependency-free RFC 4122 v4 UUID string generator. Exported `New() string`; sets version/variant nibbles, lower-case hex, 8-4-4-4-12; panics if `crypto/rand` fails (`ids.go:10-27`). The root directory UUID is a separate server constant `RootID = "00000000-0000-0000-0000-000000000000"` (`server.go:26`).

### 1.7 `internal/reader` — 9 files, 2 384 LOC
Responsibilities: parse EPUB and TXT into an immutable `Book`, sanitize EPUB chapter HTML to a whitelist, extract TOC/cover/assets, detect TXT GBK encoding, and provide an in-memory parsed-book LRU. Exported: `Parse`, `Book`, `Chapter`, `Asset`, `TocEntry`, `Cache` (`NewCache/Get/Put/Stats/TrimTo`), `AssetContentType`, `NormalizePath`, and limit constants `MaxTXT=16<<20`, `MaxEPUB=128<<20` (`reader.go:15-28`). Third-party `golang.org/x/net/html`, `golang.org/x/text/encoding/simplifiedchinese`. Details in §8.

### 1.8 `internal/reader/flow` — 4 files, 1 166 LOC
Responsibilities: turn a parsed `Book` into a deterministic continuous "reading flow" — global block numbering, chunk partitioning, TOC navigation locators, object-key layout, and the anchor/locator model. Exported: `Build`, `Anchor`, `Manifest`, `SpineMeta`, `ChunkMeta`, `TOCTarget`, `Chunk`, `Built`, `ObjectPrefix/VersionDir/ManifestObjectKey/ChunkObjectKey/BookKeyFromObject/BookFingerprint`, `FlowFormatVersion=4`. Third-party `golang.org/x/net/html` + `/atom`. Details in §8.

### 1.9 `internal/server` — 30 non-test files, 6 664 LOC
The application core: chi router, ~74 route patterns, all handlers, and the background managers (jobs, tasks, cleanup, objects, archive, thumbnails, media, cache assembly, system status). File-by-file responsibilities:

| File | LOC | Responsibility |
|---|---:|---|
| `server.go` | 370 | `Server` struct, constants, `New`, `Close`, `Handler` (route table), security headers, origin guard, `File` struct |
| `server_helpers.go` | 144 | name validation, JSON decode with limits, `writeJSON`/`problem`/`problemCode`, login limiter |
| `server_auth.go` | 434 | login/logout/me, credential & password & username change, TOTP HTTP handlers, avatar, `requireAuth`, `clientIP` |
| `server_files.go` | 782 | file scan/children/breadcrumbs, directory & document CRUD, patch/copy/delete, trash/list/restore/purge/expiry, download/preview dispatch |
| `server_stream_share.go` | 354 | Range streaming, `responseMime`, `safeDeliveryMime`, shares CRUD, public share, durable cleanup-queue processing |
| `server_uploads.go` | 605 | upload session create/get/complete/abort, multipart part ack, expired-upload cleanup, referenced-key scan, GC |
| `upload_content.go` | 70 | streamed PUT of single/multipart upload bytes |
| `download_batch.go` | 325 | batch-download token store + streaming ZIP |
| `archive.go` | 560 | archive extraction job state machine, password wait, import/commit extracted tree, staging |
| `thumb.go` | 376 | thumbnail keys, schedulers, image resize, video/audio cover generation |
| `video_media.go` | 376 | video info, subtitle discovery/matching/language, WebVTT conversion + cache |
| `audio_media.go` | 45 | audio info (duration, chapters, cover URL) |
| `audio_source.go` | 12 | `audioSourceExts` set and `isAudioSource` predicate (`.mp3 .wav .flac .m4a .aac .ogg .oga .opus .wma .aif .aiff .ape`, or `audio/*`) |
| `media_metadata.go` | 182 | ffprobe metadata scheduler/probe/upsert/reanalyze |
| `media_pipeline.go` | 65 | thin wrapper over the Rust `MediaEngine` with error taxonomy |
| `media_progress.go` | 63 | playback position get/save |
| `book.go` | 212 | reader endpoints (info/assets/cover/progress), book source L2 open |
| `reader_flow.go` | 229 | flow manifest/chunk endpoints, idempotent build, self-heal, flow GC |
| `library.go` | 281 | cross-directory media library aggregation and counts |
| `system_status.go` | 189 | status snapshot, SSE stream, 15 s refresh ticker |
| `jobs.go` | 119 | `JobManager` change bus + `/api/events` SSE |
| `tasks.go` | 247 | task list/get/cancel/retry/input/delete + boot recovery |
| `task_manager.go` | 65 | durable task transitions + resource admission |
| `task_adapters.go` | 118 | archive-task persistence/restore, runtime task wrappers |
| `resources.go` | 37 | CPU=1 / IO=3 governor |
| `cleanup_manager.go` | 122 | periodic cleanup registry with backoff |
| `object_manager.go` | 148 | object-store retry/verify/delete policy |
| `cache.go` | 62 | global cache assembly (classes + book cache) |
| `errors.go` | 36 | `AppError` + `publicError` |
| `integrity.go` | 36 | sha256 streaming verification |

(That is 30 non-test files; the table above lists all of them, one row each.)

Deps: `go-chi/chi/v5`, `golang.org/x/sync/singleflight`, `golang.org/x/image/{bmp,draw,webp}`, `golang.org/x/sys/unix` (archive.go), `golang.org/x/net` indirectly via reader. (`golang.org/x/net` is a direct module requirement; the server imports `chi`, `singleflight`, `x/image`, `x/sys/unix`.)

### 1.10 `internal/storage` — 4 files, 729 LOC
Responsibilities: abstract object store interface; `Local` implementation on the filesystem using `os.Root` confinement, atomic temp-write+rename, create-only hard links, multipart staging, and a Go client (`DataPlane`) for the Rust sidecar. Exported: `Storage`, `MediaEngine`, `ArchiveExtractor`, `Local`, `DataPlane`, `NewLocal`, `NewDataPlane`, `BlobKey`, `ValidMultipartPartCount`, `IsNotFound`, `ObjectInfo`, `ObjectRef`, `CompletedPart`, `ArchiveProgress`, `MediaProbe/Chapter/Subtitle`, `ReadSeekCloserAt`, error values `ErrNotFound`, `ErrObjectTooLarge`, `ErrNoCover`, `ErrArchivePasswordRequired`, `ErrArchiveWrongPassword`. Details in §6.

### 1.11 `internal/webui` — 1 file, 35 LOC
Responsibilities: serve the embedded Vite SPA. `//go:embed dist/*` (`webui.go:10-11`); `http.FileServer` over the `dist` subtree; content-hashed `/assets/*` get `Cache-Control: public, max-age=31536000, immutable`; everything else falls back to `index.html` (`webui.go:19-34`). Vite writes to `internal/webui/dist` (`web/vite.config.ts`). Details in §5.9.

### 1.12 `cmd/server` — 1 file, 203 LOC
Responsibilities: parse the single `reset-admin` subcommand; load config; verify the work dir is writable; start the data plane; open SQLite; initialize the admin; build local storage; construct the server; register two extra cleanup jobs; start `net/http`; handle signals and drain in LIFO order. Details in §5.7. Uses stdlib only.

---

## 2. HTTP API surface

Router: `go-chi/chi/v5` (`server.go:228`). Global middleware chain, in order: `middleware.RequestID`, `middleware.Recoverer`, `s.securityHeaders`, `s.originGuard` (`server.go:229`).

**Route counts:** **74 registered route patterns**:
- 3 non-API app routes: `GET /healthz`, `GET /readyz`, `GET /s/{token}`.
- 70 under `/api`: 1 unauthenticated (`POST /api/auth/login`), 69 behind `s.requireAuth`.
- 1 SPA catch-all `/*` (webui).
- Plus 1 API sub-router `NotFound` handler (`/api/*` → JSON 404), which is not a registered pattern. The tables below number 75 rows because the sentinel is shown as its own row (#4).

Auth legend: **—** = public; **cookie** = `revaro_session` session cookie required (401 `{"error":{"status":401,"message":"authentication required"}}` on missing/invalid). All write methods additionally require a same-origin `Origin` header (see §5.4).

### 2.1 Non-API routes

| # | Method | Path | Handler (file:line) | Auth | Request | Response |
|---|---|---|---|---|---|---|
| 1 | GET | `/healthz` | `server.go:230` | — | — | `{"status":"ok"}` |
| 2 | GET | `/readyz` | `ready` `server.go:317` | — | — | 200 `{"status":"ready"}`; 503 `{"error":{...,"message":"database unavailable"\|"object storage unavailable"}}` |
| 3 | GET | `/s/{token}` | `publicShare` `server_stream_share.go:320` | — (token) | token length 32–128 | streamed file (`http.ServeContent`), `inline`/`attachment`; extra headers `Cache-Control: no-store`, `Referrer-Policy: no-referrer`, `X-Robots-Tag: noindex, nofollow, noarchive`, CSP `sandbox; default-src 'none'; …`; 404, 429 (`Retry-After: 5` when 8 share slots busy) |
| 4 | ANY | `/api/*` unmatched | `server.go:236-238` | — | — | 404 `{"error":{"status":404,"message":"api endpoint not found"}}` |
| 5 | ANY | `/*` | `webui.Handler()` `server.go:313` | — | — | embedded SPA / `index.html` fallback |

### 2.2 Authentication (5)

| # | Method | Path | Handler | Auth | Request body | Response / status |
|---|---|---|---|---|---|---|
| 6 | POST | `/api/auth/login` | `login` `server_auth.go:21` | — | `{username, password, second_factor}` | 200 `{"username":…,"has_avatar":bool}` + `Set-Cookie: revaro_session`; 401 `{error:{status,message}}` or `{error:{status,code:"totp_required"\|"invalid_second_factor",message}}`; 429 `Retry-After:60` or `Retry-After:2` |
| 7 | POST | `/api/auth/logout` | `logout` `server_auth.go:103` | cookie | — | 204, clears cookie |
| 8 | GET | `/api/auth/me` | `me` `server_auth.go:110` | cookie | — | `{"username":…,"has_avatar":bool}` |
| 9 | PATCH | `/api/auth/credentials` | `changeCredentials` `server_auth.go:337` | cookie | `{current_password, username, password}` | 204 (clears cookie); 400 if password <12 or username empty/>128; 401 wrong current password |
| 10 | PATCH | `/api/auth/password` | `changePassword` `server_auth.go:365` | cookie | `{current_password, password}` | 204 (clears cookie); 400/401 as above |

### 2.3 TOTP (5)

| # | Method | Path | Handler | Request | Response |
|---|---|---|---|---|---|
| 11 | GET | `/api/auth/totp` | `totpStatus` `server_auth.go:114` | — | `TOTPStatus` = `{"enabled":bool,"recovery_codes":int}` |
| 12 | POST | `/api/auth/totp/setup` | `beginTOTPSetup` `server_auth.go:124` | `{current_password}` | 201 `{"secret","uri","qr_data_url":"data:image/png;base64,…"}`; 400/401/409 |
| 13 | POST | `/api/auth/totp/enable` | `enableTOTP` `server_auth.go:171` | `{current_password, code}` | 200 `{"enabled":true,"recovery_codes":[…10 strings…]}`; 400/401/409/410 |
| 14 | POST | `/api/auth/totp/recovery-codes` | `regenerateTOTPRecoveryCodes` `server_auth.go:189` | `{current_password, code}` | 200 `{"enabled":true,"recovery_codes":[…10…]}` |
| 15 | DELETE | `/api/auth/totp` | `disableTOTP` `server_auth.go:207` | `{current_password, code}` | 204; 400/401/409 |

Error mapping for 11–15 is centralized in `totpProblem` (`server_auth.go:236-252`): invalid password → 401 "current password is incorrect"; invalid code → 401; already enabled → 409; not enabled → 409; setup expired → **410 Gone**.

### 2.4 Profile (4)

| # | Method | Path | Handler | Request | Response |
|---|---|---|---|---|---|
| 16 | GET | `/api/profile/avatar` | `getAvatar` `server_auth.go:259` | — | raw image bytes, `Content-Type` from `settings.avatar_mime`, `Content-Disposition: inline`; 404 if unset |
| 17 | PUT | `/api/profile/avatar` | `updateAvatar` `server_auth.go:282` | `{data_url:"data:image/…;base64,…"}` | 204; 400 invalid; 413 >2 MiB; 415 not JPEG/PNG/GIF/WebP; 502 write failure |
| 18 | DELETE | `/api/profile/avatar` | `deleteAvatar` `server_auth.go:324` | — | 204 |
| 19 | PATCH | `/api/profile/username` | `changeUsername` `server_auth.go:392` | `{username}` | 204; 400 empty/>128 (trimmed) |

### 2.5 Storage / library / system / tasks / events (14)

| # | Method | Path | Handler | Request | Response |
|---|---|---|---|---|---|
| 20 | GET | `/api/storage/stats` | `storageStats` `server_files.go:151` | — | `{"total_bytes":int64,"file_count":int64}` (ready, non-deleted files) |
| 21 | GET | `/api/library?type={book\|image\|video\|audio\|file}` | `library` `library.go:209` | query `type` (default `file`) | `{"type":…,"items":[libraryItem…],"counts":libraryCounts}`; 400 unknown type |
| 22 | GET | `/api/library/all` | `libraryAll` `library.go:236` | — | `{"items":{"book":[…],"image":[…],"video":[…],"audio":[…]},"counts":libraryCounts}` |
| 23 | GET | `/api/library/counts` | `libraryCounts` `library.go:200` | — | `{"book":n,"image":n,"video":n,"audio":n,"file":n}` |
| 24 | GET | `/api/system/status` | `systemStatus` `system_status.go:128` | — | `systemStatusResponse` (see §2.9) |
| 25 | GET | `/api/system/status/stream` | `systemStatusStream` `system_status.go:152` | — | SSE: `event: status\ndata: <systemStatusResponse JSON>\n\n`, keepalive comment every 20 s |
| 26 | GET | `/api/events` | `jobEvents` `jobs.go:74` | — | SSE: initial and every change `event: jobs\ndata: {"changed":true}\n\n`, 20 s keepalive |
| 27 | GET | `/api/tasks` | `listTasks` `tasks.go:59` | — | `{"items":[Task…]}` — only types `upload\|archive_extract\|subtitle`, terminal rows only for 30 min, `LIMIT 500`, newest first |
| 28 | GET | `/api/tasks/{id}` | `getTask` `tasks.go:80` | — | `Task`; 404 |
| 29 | POST | `/api/tasks/{id}/cancel` | `cancelTask` `tasks.go:89` | — | 204; 404; 409 already terminal |
| 30 | POST | `/api/tasks/{id}/retry` | `retryTask` `tasks.go:111` | — | 202; 404; 409 not retryable |
| 31 | POST | `/api/tasks/{id}/input` | `taskInput` `tasks.go:169` | `{password}` | 202 archiveJob snapshot; 400 not archive/empty/>1024; 409 not waiting/closed; 503 shutting down |
| 32 | DELETE | `/api/tasks/{id}` | `deleteTask` `tasks.go:212` | — | 204; 404; 409 active |

### 2.6 Files & documents (30)

| # | Method | Path | Handler | Request | Response / status |
|---|---|---|---|---|---|
| 33 | GET | `/api/files/{id}` | `getFile` `server_files.go:50` | — | `{"file":File,"breadcrumbs":[File…]}`; 404 |
| 34 | GET | `/api/files/{id}/children` | `children` `server_files.go:84` | — | `{"items":[File…],"total_bytes":n,"file_count":n}`; 404 not a directory; adds `has_cover` for audio |
| 35 | GET | `/api/files/{id}/download` | `download`→`streamFile` `server_files.go:769,771` | Range | streamed bytes (`http.ServeContent`), `Content-Disposition: attachment; filename*=UTF-8''…`, ETag; 404 |
| 36 | GET | `/api/files/{id}/preview` | `preview` `server_files.go:770` | Range | inline stream; 404; 415 not previewable (video/audio/image only) |
| 37 | POST | `/api/files/batch-download/prepare` | `prepareBatchDownload` `download_batch.go:116` | `{ids:[…]}` | 200 `{"token":"…"}`; 400/404/409/503 |
| 38 | GET | `/api/files/batch-download/{token}` | `batchDownload` `download_batch.go:200` | — | streaming ZIP: `Content-Type: application/zip`, `Content-Disposition: attachment; filename="revaro-download.zip"`; 404 token invalid/expired |
| 39 | GET | `/api/files/{id}/audio` | `audioMediaInfo` `audio_media.go:21` | — | `{"duration":float sec,"chapters":[{"id","title","start","end"}],"cover_url":str,"has_cover":bool}`; 404 |
| 40 | GET | `/api/files/{id}/video` | `videoMediaInfo` `video_media.go:70` | — | `{"subtitles":[videoSubtitleResponse…]}` |
| 41 | POST | `/api/files/{id}/media/reanalyze` | `reanalyzeMedia` `media_metadata.go:161` | — | 200 `{"status":"ready","subtitles":n}`; 404; 422 re-analysis failed |
| 42 | GET | `/api/files/{id}/video/subtitles/{subtitle}` | `videoSubtitle` `video_media.go:269` | — | `text/vtt; charset=utf-8`, `Cache-Control: private, max-age=3600`; 404; 413; 422 |
| 43 | GET | `/api/files/{id}/media/progress` | `mediaProgress` `media_progress.go:18` | — | `{"position":sec,"duration":sec,"updated_at"}` (zero value if absent); 404 |
| 44 | PUT | `/api/files/{id}/media/progress` | `saveMediaProgress` `media_progress.go:38` | `{position,duration}` | same shape; 400 invalid/NaN/Inf/>7 days or position>duration+5; 404 |
| 45 | GET | `/api/files/{id}/content` | `getDocument` `server_files.go:295` | — | `{"content":str,"etag":str,"updated_at":str}`; 404; 413 >1 MiB; 415 not editable/not UTF-8 |
| 46 | PUT | `/api/files/{id}/content` | `updateDocument` `server_files.go:325` | `{content, etag?}` | 200 updated `File`; 400; 404; 409 stale ETag; 502 integrity |
| 47 | GET | `/api/files/{id}/book` | `bookInfo` `book.go:68` | — | `{"format","title","name","cover":bool,"toc":[TocEntry…]}`; 415; 422 parse error; 413 too large |
| 48 | GET | `/api/files/{id}/book/assets/{index}` | `bookAsset` `book.go:87` | — | asset bytes, `Cache-Control: private, max-age=31536000, immutable`; 404 |
| 49 | GET | `/api/files/{id}/book/cover` | `bookCover` `book.go:111` | — | cover bytes, `Cache-Control: private, max-age=3600`; 404 no cover; 422 |
| 50 | GET | `/api/files/{id}/book/progress` | `bookProgress` `book.go:145` | — | `{"anchor":{"spine","block","path","offset"}}` when set; `{}` otherwise |
| 51 | PUT | `/api/files/{id}/book/progress` | `saveBookProgress` `book.go:171` | `{anchor}` | 204; 400 invalid anchor |
| 52 | GET | `/api/files/{id}/book/flow` | `bookFlow` `reader_flow.go:125` | — | flow `Manifest` JSON, `Cache-Control: private, no-cache`; 415/413/422 |
| 53 | GET | `/api/files/{id}/book/flow/chunks/{index}` | `bookFlowChunk` `reader_flow.go:147` | index 0..2^22 | chunk HTML `text/html; charset=utf-8`, `Cache-Control: private, max-age=31536000, immutable`; 400 bad index; 404 missing |
| 54 | GET | `/api/files/{id}/thumbnail` | `thumbnail` `thumb.go:162` | — | `image/jpeg`, `Cache-Control: private, max-age=31536000, immutable`, `ETag` = sha256 of bytes; 404 while video thumb generating / unavailable |
| 55 | GET | `/api/files/{id}/share` | `getShare` `server_stream_share.go:273` | — | `{"active":false}` or `{"active":true,"url":"<BaseURL>/s/<token>","created_at":…}` |
| 56 | POST | `/api/files/{id}/share` | `createShare` `server_stream_share.go:288` | — | 201 `{"active":true,"url","created_at"}`; 404 |
| 57 | DELETE | `/api/files/{id}/share` | `revokeShare` `server_stream_share.go:310` | — | 204 |
| 58 | POST | `/api/directories` | `createDirectory` `server_files.go:160` | `{parent_id,name}` | 201 `File`; 400; 409 name conflict/parent gone |
| 59 | POST | `/api/documents` | `createDocument` `server_files.go:246` | `{parent_id,name,content}` | 201 `File`; 400; 409; 502 |
| 60 | PATCH | `/api/files/{id}` | `patchFile` `server_files.go:375` | `{name?, parent_id?}` | 200 `File`; 400 root/cycle/validation; 404; 409 |
| 61 | POST | `/api/files/{id}/copy` | `copyFile` `server_files.go:449` | `{parent_id}` | 201 `File`; 400; 404; 409 |
| 62 | POST | `/api/files/{id}/extract` | `startArchiveExtract` `archive.go:165` | — | 202 `archiveJob` snapshot; 404; 503 no extractor/shutting down |
| 63 | DELETE | `/api/files/{id}` | `deleteFile` `server_files.go:547` | — | 204; 400 root; 404; 409 racing |
| 64 | GET | `/api/trash` | `trash` `server_files.go:604` | — | `{"items":[File…],"total_bytes":n,"file_count":n}` |
| 65 | DELETE | `/api/trash` | `emptyTrash` `server_files.go:708` | — | 204 |
| 66 | POST | `/api/trash/{id}/restore` | `restoreTrash` `server_files.go:636` | — | 204; 404; 409 name conflict |
| 67 | DELETE | `/api/trash/{id}` | `purgeTrash` `server_files.go:678` | — | 204; 404 |

### 2.7 Uploads (8)

| # | Method | Path | Handler | Request | Response / status |
|---|---|---|---|---|---|
| 68 | POST | `/api/uploads` | `createUpload` `server_uploads.go:77` | `createUploadInput{parent_id,name,size,mime_type}` | 201 `{"upload_id","file_id","mode","url","part_size","part_count","expires_at"}`; 400; 409; 502 |
| 69 | GET | `/api/uploads/{id}` | `getUpload` `server_uploads.go:203` | — | `{"upload_id","file_id","mode","url","part_size","part_count","expected_size","mime_type","status","expires_at","parts":[{"part_number","size","etag","content_hash"}…]}`; 404 |
| 70 | PUT | `/api/uploads/{id}/data` | `uploadContent` `upload_content.go:15` | raw bytes | 204 + `ETag`; 400 size mismatch/part on single; 404; 499 cancelled |
| 71 | PUT | `/api/uploads/{id}/data/{part}` | `uploadContent` `upload_content.go:15` | raw bytes | 204 + `ETag`; 400; 404; 499 |
| 72 | POST | `/api/uploads/{id}/parts` | `uploadParts` `server_uploads.go:273` | `{part_numbers:[int32]}` (1–100 entries) | 200 `{"parts":[{"part_number","url"}…]}`; 400; 404 |
| 73 | PUT | `/api/uploads/{id}/parts/{part}` | `recordUploadPart` `server_uploads.go:234` | `{etag,size,content_hash}` | 204; 400; 404 |
| 74 | POST | `/api/uploads/{id}/complete` | `completeUpload` `server_uploads.go:304` | `{parts:[{part_number,etag}]}` (≤2 MiB) | 200 ready `File`; 400; 404; 409; 499/500/502 |
| 75 | DELETE | `/api/uploads/{id}` | `abortUpload` `server_uploads.go:484` | — | 204 `{"error":…}`; 404 |

> Note: the *same* handler serves `PUT /uploads/{id}/data` and `PUT /uploads/{id}/data/{part}`; the part variant only applies to multipart uploads.

### 2.8 Response shape reference (actual structs / JSON tags)

- `File` — `server.go:74-91`:
  `id` string, `parent_id` *string, `name` string, `kind` `"file"|"directory"`, `size` int64, `mime_type,omitempty`, `etag,omitempty`, `content_hash,omitempty`, `hash_algorithm,omitempty`, `status` `"pending"|"ready"|"deleting"|"failed"`, `created_at`, `updated_at`, `deleted_at,omitempty`, `restore_parent_id,omitempty`, `has_cover,omitempty` bool. The `objectKey` field is unexported.
- `Task` — `tasks.go:13-32`: `id, type, status, phase, progress float64, speed int64, eta_seconds *int64(omitempty), retry_count, max_retries, error(omitempty), source_type(omitempty), source_id(omitempty), cancel_requested bool, created_at, started_at(omitempty), finished_at(omitempty), updated_at, name`.
- `libraryItem` — `library.go:18-22`: embeds `File`, plus `folder_path []libraryFolderRef{id,name}`, `duration_ms int64(omitempty)`.
- `libraryCounts` — `library.go:24-30`: `{book,image,video,audio,file int}`.
- `mediaProgressResponse` — `media_progress.go:12-16`: `position float64`, `duration float64`, `updated_at(omitempty)` (seconds, converted to/from ms in SQL).
- `audioChapterResponse` — `audio_media.go:14-19`: `{id int, title string, start float64, end float64}`.
- `videoSubtitleResponse` — `video_media.go:29-37`: `{id,name,label,language,url string, default bool, forced bool}`.
- `archiveJob` — `archive.go:41-63`: `{id,file_id,parent_id,name,status,progress int,message,output_id(omitempty),output_name(omitempty),error(omitempty),created_at,updated_at}`.
- `TOTPStatus`, `TOTPSetup` — `totp.go:47-55`.
- `systemStatusResponse` — `system_status.go:29-46`:
  ```json
  {"status":"ok|degraded",
   "database":{"status":"ok|degraded","bytes":n},
   "storage":{"status":..,"bytes":n,"trash_bytes":n,"file_count":n},
   "cache":{"status":..,"memory_bytes":n,"disk_bytes":n,"memory_entries":n,"disk_entries":n,
            "classes":{"<class>":{"hits":n,"misses":n,"loads":n,"load_errors":n,"evictions":n,
                                  "memory_bytes":n,"memory_entries":n,"disk_bytes":n,"disk_entries":n}}}}
  ```
  (`systemComponent`/`systemClassStat` at `system_status.go:12-27`; `omitempty` on all but `status`.)
- Reader flow types — `flow/manifest.go`, `flow/anchor.go`: see §8.3.

---

## 3. Domain model

### 3.1 SQLite schema

Migrations are embedded and applied lexicographically by numeric filename prefix, each in a single transaction, recorded in `schema_migrations(version, applied_at)` (`database.go:58-101`). Only `001_local_product.sql` and `002_file_cleanup.sql` exist.

`001_local_product.sql` tables:

| Table | Columns (type / constraints) |
|---|---|
| `sessions` | `id TEXT PK`, `token_hash TEXT UNIQUE NOT NULL`, `created_at TEXT`, `expires_at TEXT` — indexes on `token_hash`, `expires_at` |
| `settings` | `key TEXT PK`, `value TEXT NOT NULL`, `updated_at TEXT NOT NULL` (generic KV: admin creds, avatar mime, TOTP state, book progress) |
| `shares` | `file_id TEXT PK → files(id) ON DELETE CASCADE`, `token TEXT NOT NULL UNIQUE`, `created_at TEXT` — unique index on token |
| `files` | `id TEXT PK`, `parent_id TEXT → files(id)`, `name TEXT NOT NULL`, `kind TEXT CHECK IN ('file','directory')`, `object_key TEXT`, `size INTEGER ≥0`, `mime_type TEXT`, `etag TEXT`, `status TEXT CHECK IN ('pending','ready','deleting','failed') DEFAULT 'ready'`, `created_at`, `updated_at`, `deleted_at TEXT`, `restore_parent_id TEXT`, `trash_root_id TEXT`, `content_hash TEXT`, `hash_algorithm TEXT`; CHECK `(kind='directory' AND object_key IS NULL) OR kind='file'` |
| `media_progress` | `file_id TEXT PK → files ON DELETE CASCADE`, `position_ms INTEGER ≥0`, `duration_ms INTEGER ≥0 DEFAULT 0`, `updated_at TEXT` |
| `uploads` | `id TEXT PK`, `file_id → files CASCADE`, `mode CHECK IN ('single','multipart')`, `object_key`, `multipart_id TEXT`, `part_size INTEGER >0`, `expected_size INTEGER ≥0`, `mime_type`, `status CHECK IN ('pending','completed','aborted','failed')`, `created_at`, `expires_at`, `content_hash`, `completed_at`; CHECK single⇒multipart_id NULL, multipart⇒multipart_id NOT NULL |
| `media_metadata` | `file_id TEXT PK → files CASCADE`, `duration_ms ≥0`, `container`, `video_codec`, `audio_codec`, `width ≥0`, `height ≥0`, `bitrate ≥0`, `chapters_json DEFAULT '[]'`, `analyzed_at`, `frame_rate`, `video_profile`, `video_level`, `subtitles_json DEFAULT '[]'`, `source_etag`, `probe_version INTEGER DEFAULT 0` — index on `analyzed_at` |
| `directory_stats` | `directory_id TEXT PK → files CASCADE`, `file_count ≥0`, `total_bytes ≥0` |
| `upload_parts` | `(upload_id, part_number) PK`, `part_number CHECK BETWEEN 1 AND 10000`, `size >0`, `etag`, `content_hash`, `completed_at`; FK `upload_id → uploads CASCADE` |
| `tasks` | `id TEXT PK`, `type TEXT`, `status CHECK IN ('queued','running','waiting_input','retrying','completed','failed','cancelled')`, `phase DEFAULT ''`, `progress REAL 0..100`, `speed ≥0`, `eta_seconds`, `retry_count ≥0 DEFAULT 0`, `max_retries ≥0 DEFAULT 3`, `error DEFAULT ''`, `source_type`, `source_id`, `payload_json DEFAULT '{}'`, `cancel_requested 0/1`, `created_at`, `started_at`, `finished_at`, `heartbeat_at`, `updated_at` — unique partial index on `(source_type, source_id)` where both non-null; index `(status, updated_at)` |
| `task_files` | `(task_id, file_id, role) PK`, `role DEFAULT 'output'`; FKs to `tasks`/`files` CASCADE |
| `object_cleanup` | `object_key TEXT PK`, `reason TEXT`, `retry_count INTEGER DEFAULT 0`, `created_at`, `updated_at` |

`files` indexes: `parent_id`; unique `(parent_id,name) WHERE deleted_at IS NULL`; `deleted_at`; `trash_root_id`; partial active-children `(parent_id, kind DESC, name COLLATE NOCASE) WHERE deleted_at IS NULL`.

Triggers in 001:
- `directory_stats_directory_insert` — insert a `directory_stats` row for every new directory.
- `directory_stats_file_insert` / `directory_stats_file_delete` — recursive-CTE update of every ancestor's file_count/total_bytes when a ready, non-deleted file is inserted/deleted.
- `directory_stats_file_update` — on `UPDATE OF parent_id,size,status,deleted_at`, subtract old contribution, add new (recursive over both old and new ancestors).
- `directory_stats_directory_move` — on directory reparent, move the subtree's aggregate between ancestor chains.
- Seed row: `INSERT INTO files … VALUES ('00000000-0000-0000-0000-000000000000', NULL, '', 'directory', 'ready', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)` (`001:220-221`) — the root directory.

`002_file_cleanup.sql` (18 lines):
- `ALTER TABLE object_cleanup ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;` (`002:3`)
- `files_delete_cleanup` AFTER DELETE ON files — when a file blob is deleted, upsert `object_cleanup` (`reason='file deleted'`) and on conflict bump `generation` (`002:4-10`).
- `files_replace_cleanup` AFTER UPDATE OF object_key — same for `reason='file replaced'` (`002:12-17`).

### 3.2 Go struct ↔ schema mapping and exact JSON

| Go struct (file:line) | Table | JSON field names |
|---|---|---|
| `File` (`server.go:74`) | `files` | `id, parent_id, name, kind, size, mime_type, etag, content_hash, hash_algorithm, status, created_at, updated_at, deleted_at, restore_parent_id, has_cover` (all as tagged; `omitempty` on mime/etag/content_hash/hash_algorithm/deleted_at/restore_parent_id/has_cover) |
| `uploadRecord` (`server_uploads.go:187`) | `uploads` + `upload_parts` | **no struct tags** — serialized by hand into response maps |
| `Task` (`tasks.go:13`) | `tasks` (+ `task_files` name subquery) | see §2.8 |
| `probedMediaMetadata` (`media_metadata.go:66`) | `media_metadata` | fields are **unexported JSON** (never serialized directly); `chapters_json` ↔ `[]storedAudioChapter`, `subtitles_json` ↔ `[]embeddedSubtitle` |
| `storedAudioChapter` (`audio_media.go:9`) | `media_metadata.chapters_json` | `title, start_ms, end_ms` |
| `embeddedSubtitle` (`media_metadata.go:84`) | `media_metadata.subtitles_json` | `index, codec, language(omitempty), title(omitempty), default, forced` |
| `mediaProgressResponse` (`media_progress.go:12`) | `media_progress` | `position` (s), `duration` (s), `updated_at`; DB stores ms |
| `archiveJob` (`archive.go:41`) | `tasks` + `task_files` | see §2.8 |
| `TocEntry` (`reader.go:42`) | — (in-memory) | `label, path(omitempty), fragment(omitempty), offset(omitempty), depth` |
| `flow.Manifest` (`manifest.go:11`) | — (object/JSON) | `version, format, total_chars, book_key(omitempty), spines, chunks, toc, generated_at(omitempty)` |
| `flow.SpineMeta` (`manifest.go:27`) | — | `block_start, block_count` |
| `flow.ChunkMeta` (`manifest.go:35`) | — | `index, block_start, block_count, chars, bytes(omitempty), url(omitempty)` |
| `flow.TOCTarget` (`manifest.go:56`) | — | `label, depth, spine, block, nav_anchor(omitempty), text_path(omitempty), text_offset(omitempty), chunk, source_path(omitempty), source_fragment(omitempty)` |
| `flow.Anchor` (`anchor.go:26`) | `settings['book_progress/<fileID>']` JSON | `spine, block, path(omitempty), offset` |
| `systemStatusResponse` | live metrics | see §2.8 |
| `InitialCredentials` | — | not serialized |
| `TOTPStatus`/`TOTPSetup` | `settings` | `enabled, recovery_codes` / `secret, uri` |
| `mediaProbe`/`MediaChapter`/`MediaSubtitle` (`storage/storage.go:62-88`) | — (data-plane wire) | `duration_ms, container, video_codec, audio_codec, width, height, bitrate, frame_rate, video_profile, video_level, chapters[{title,start_ms,end_ms}], subtitles[{index,codec,language,title,default,forced}]` |

Settings keys (all in `settings` KV): `admin_username`, `admin_password_hash`, `avatar_mime`, `admin_totp_config`, `admin_totp_recovery_codes`, `admin_totp_last_step`, `admin_totp_pending`, `book_progress/<fileID>`.

---

## 4. Error model

### 4.1 Serialization primitives (`server_helpers.go:66-76`)
```go
func writeJSON(w, status, v)                       // Content-Type: application/json; status; json.Encoder.Encode
func problem(w, status, message)                   // {"error":{"status":<int>,"message":<string>}}
func problemCode(w, status, code, message)         // {"error":{"status":<int>,"code":<string>,"message":<string>}}
```
Every handler-level failure goes through `problem`/`problemCode`. There is no HTML error page on API routes; unknown `/api/*` returns the JSON 404 sentinel (`server.go:236-238`). `writeJSON` uses a trailing newline from `Encoder.Encode`.

### 4.2 Status-code conventions
Observed distribution (grep over `internal/server`):

| Code | Meaning in this codebase |
|---|---|
| 200 | normal read/update |
| 201 | created (directory, document, copy, share, TOTP setup) |
| 202 | accepted async work (archive extract, task retry, task input) |
| 204 | success with no body (logout, deletes, upload data, part ack, progress saves, avatar) |
| 206 | Range responses (via `http.ServeContent`) |
| 400 | bad JSON / validation / bad part number / parent invalid / root-immutable / directory cycle / unknown library type |
| 401 | missing/invalid session; login failure; wrong current password; invalid TOTP code |
| 403 | origin guard (missing or mismatched `Origin` on writes) |
| 404 | missing file/ready-file/upload/share/asset/chunk/task; also used for "not found or not in expected state" |
| 405 | chi's method-not-allowed for retired/read-only routes |
| 409 | name conflict; stale ETag; restore conflict; task not cancelable/retryable; upload no longer pending; source gone |
| 410 | TOTP setup expired |
| 413 | 1 MiB document, 2 MiB avatar, oversized subtitle |
| 415 | not previewable / not an editable document / not a readable book / bad avatar type |
| 416 | unsatisfiable Range |
| 422 | book/flow/archive/media parse or conversion failure |
| 429 | login rate limit (`Retry-After:60`), login verifier busy (`Retry-After:2`), public share slot busy (`Retry-After:5`) |
| 499 | client closed/cancelled during upload (`problem(w,499,…)`, `upload_content.go:18`, `server_uploads.go:307`); non-standard nginx-style code |
| 500 | database or internal failure |
| 502 | object-storage/upstream failure, integrity failure |
| 503 | `/readyz` dependency down, extractor unavailable, shutting down, batch-token store full |

### 4.3 Application error type (`errors.go`)
```go
type AppError struct { Code, Message string; Cause error; Retryable, ActionRequired bool }
func (e *AppError) Error() // "<code>: <cause|message>"
func (e *AppError) Unwrap() error
func appError(code, message string, cause error, retryable bool) *AppError
func publicError(err error, fallback string) string   // Message if AppError, else fallback
```
`Code`/`Retryable`/`ActionRequired` are **internal only** — they are never serialized to HTTP; `Message` is used for task `error` fields and safe log messages. `ActionRequired` is declared but never set anywhere.

Codes actually constructed:
- `internal/server/object_manager.go`: `cancelled`, `object_<op>_failed` (`object_stat_failed`, `object_put_failed`, `object_delete_failed`), `object_read_failed`, `multipart_commit_failed`, `size_mismatch`, `hash_failed`, `hash_mismatch`.
- `internal/server/media_pipeline.go`: `media_unavailable`, `media_probe_failed`, `thumbnail_failed`, `cover_failed`, `subtitle_failed`.
Messages are Chinese-localized strings (e.g. `"对象存储暂时不可用，请稍后重试"`).

### 4.4 HTTP-only error code
The only `code` ever emitted over HTTP is via `problemCode` on login:
- `"totp_required"` — 401, `"enter your authenticator or recovery code"` (`server_auth.go:51`)
- `"invalid_second_factor"` — 401, `"the authenticator or recovery code is incorrect"` (`server_auth.go:54`)

Everything else is `{"error":{"status":n,"message":"…"}}`.

### 4.5 Typed errors that drive control flow
`auth.ErrInvalidCredentials`, `auth.ErrTOTPRequired`, `auth.ErrInvalidSecondFactor`, `auth.ErrTOTPAlreadyEnabled`, `auth.ErrTOTPNotEnabled`, `auth.ErrTOTPSetupExpired`; `storage.ErrNotFound`, `storage.ErrObjectTooLarge`, `storage.ErrNoCover`, `storage.ErrArchivePasswordRequired`, `storage.ErrArchiveWrongPassword`; `errUploadNotPending` (`server_uploads.go:440`); `errBatchDownloadTokenLimit` (`download_batch.go:46`); `batchDownloadValidationError` carrying a status; `archiveJob` sentinel errors. `isConflict(err)` (`server_helpers.go:46`) converts SQLite UNIQUE/constraint text into 409.

---

## 5. Cross-cutting concerns

### 5.1 Configuration (`config.go`)
| Field | Env | Default | Validation |
|---|---|---|---|
| `Addr` | `APP_ADDR` | `:8080` | — |
| `DataDir` | `APP_DATA_DIR` | `/data` | — |
| `WorkDir` | `APP_WORK_DIR` | `/work` | non-empty |
| `BaseURL` | `APP_BASE_URL` | `http://localhost:8080` | absolute http(s), non-empty host, trailing `/` trimmed |
| `CookieSecure` | `COOKIE_SECURE` | `BaseURL` starts `https://` | ParseBool |
| `AdminUsername` | `ADMIN_USERNAME` | `""` (→ `admin`) | ≤128 |
| `AdminPassword` | `ADMIN_PASSWORD` | `""` (→ generated) | 12..1024 |
| `MediaCacheCapacity` | `MEDIA_CACHE_CAPACITY` | 2 GiB | 0..1 TiB |
| `UploadExpires` | `UPLOAD_EXPIRES` | 24h | >0 |
| `TrashRetention` | `TRASH_RETENTION` | 30d | ≥0 (0 disables expiry) |
| `GCInterval` | `GC_INTERVAL` | 1h | ≥0 (0 disables orphan scan) |
| `DataPlaneAddr` | `DATA_PLANE_ADDR` | `127.0.0.1:7081` | numeric loopback, checked in `dataplane.Start` |
| `DataPlaneBinary` | `DATA_PLANE_BINARY` | `revaro-data-plane` | — |
| `FlowCacheTTL` | `FLOW_CACHE_TTL` | 720h | ≥0 |
| `FlowCacheCapacity` | `FLOW_CACHE_CAPACITY` | 1 GiB | 0..1 TiB |
| `TrustedProxies` | `TRUSTED_PROXIES` | empty | comma-separated CIDRs, each `netip.ParsePrefix`, stored `.Masked()` |

`env()` treats empty string as unset (`config.go:106-111`). `DatabasePath() = <DataDir>/revaro.db`; object storage root is `<DataDir>/objects`; cache root is `<WorkDir>/cache`; extraction staging `<WorkDir>/revaro-extract-<jobID>`; credentials file `<DataDir>/initial-admin-credentials`.

### 5.2 Sessions and cookies
- Token: 32 bytes `crypto/rand` → `base64.RawURLEncoding` (43 chars); stored only as `TokenHash = base64.RawURL(sha256(token))`; DB row id is a UUIDv4 (`auth.go:131-139,350-353`).
- Lifetime fixed at 30 days (`sessionLifetime`, `auth.go:21`); **no rotation on use**, expiry never refreshed.
- `Authenticate` (`auth.go:260-275`) joins `sessions` to `settings['admin_username']`; invalid or expired → error, and an expired row is deleted.
- Cookie: name `revaro_session`; `Path=/`, `HttpOnly`, `Secure=cfg.CookieSecure`, `SameSite=Lax`, `Expires`, `MaxAge = int(time.Until(expires).Seconds())`; no `Domain` (`server_auth.go:65`). Cleared with empty value, `MaxAge:-1`, `Expires: time.Unix(1,0)` (`server_auth.go:415`).
- Invalidation: `ChangeCredentials`, `ResetCredentials` delete **all** sessions; TOTP enable/disable delete all except the current one (`revokeOtherSessions`); `ChangeUsername` intentionally keeps sessions; logout deletes one; a 15-min auth cleanup job deletes expired rows.
- `auth.Cleanup` registered in `main.go:94`; session-cookie parsing in `requireAuth` (`server_auth.go:420-433`) which injects `userKey{}` → username into the request context.

### 5.3 Passwords and TOTP
- Argon2id; default `m=65536 KiB (64 MiB), t=3, p=2, salt=16 B, key=32 B`; encoded `$argon2id$v=19$m=…,t=…,p=…$<rawstd salt>$<rawstd key>`; verify enforces hash ≤1024 chars and parameter bounds (m 1024..262144, t 1..10, p 1..8, salt 8..64, key 16..64). A process-global semaphore of **2** serializes all KDF/encrypt work (`kdfSlots`, `auth.go:28`).
- Login rate limiting: per-IP 5 failures / 15 min, global 30 / 15 min, map capped at 10 000 entries (expired pruned opportunistically); 2 concurrent login verifications; 429 with `Retry-After: 60` (blocked) / `2` (busy) (`server_helpers.go:78-144`, `server_auth.go:23-44`).
- TOTP: issuer `revaro`, 30 s period, 6 digits, SHA-1, 20-byte secret; ±1 step window; replay protection via `admin_totp_last_step` (reject `step <= last`); secret encrypted at rest with AES-256-GCM under Argon2id(password) with AAD `revaro-totp-v1`; pending setup TTL 10 min; 10 single-use recovery codes from 10 random bytes base32 (no padding) formatted `XXXX-XXXX-XXXX-XXXX`, hashed `base64.RawURL(sha256("revaro-recovery-v1:"+normalized))` (unsalted), constant-time compared.
- QR: `otp.NewKeyFromURL(uri).Image(256,256)` PNG → `data:image/png;base64,…`.

### 5.4 CSRF / origin guard and security headers
- `originGuard` (`server.go:350-369`): every **non-GET/HEAD/OPTIONS** request must carry an `Origin` header whose scheme+host case-insensitively equals `cfg.BaseURL`'s; missing → 403 `"origin required"`, mismatch → 403 `"origin not allowed"`. This is a hard gate for all writes, including `POST /api/auth/login` and uploads; non-browser clients (curl) must send an `Origin`.
- `securityHeaders` (`server.go:330-348`): `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: same-origin`, `Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(), usb=()`, and a strict CSP (`default-src 'self'; script-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self' 'unsafe-inline'; connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; frame-src 'none'; frame-ancestors 'none'`). `Strict-Transport-Security: max-age=31536000` only when `BaseURL` is https. All `/api/*` responses get `Cache-Control: no-store`.
- Public share overrides CSP with `sandbox; default-src 'none'; base-uri 'none'; form-action 'none'` and adds `Referrer-Policy: no-referrer`, `X-Robots-Tag: noindex, nofollow, noarchive`, `Cache-Control: no-store` (`server_stream_share.go:338-341`). Share access is logged with an 8-char token prefix (`:352`).

### 5.5 Request ID, body limits, streaming limits
- `middleware.RequestID` + `middleware.Recoverer` from chi (`server.go:229`).
- JSON body limits via `http.MaxBytesReader`: default `maxJSONBody = 7 MiB` (`server.go:27`), upload-complete 2 MiB (`server_uploads.go:319`), upload content `size+1` with a 30-minute read deadline via `http.NewResponseController(w).SetReadDeadline` (`upload_content.go:48-49`).
- Constants: `maxDocumentBytes = 1 MiB` (`server.go:28`), `maxAvatarBytes = 2 MiB` (`server.go:29`), `maxLogicalFileSize = 1 TiB` (`server.go:31`), `maxFlowObject = 8 MiB` (`reader_flow.go:30`), `maxVideoSubtitleBytes = 16 MiB` / `maxConvertedSubtitleBytes = 32 MiB` (`video_media.go:18-19`), thumbnail `maxThumbBytes = 512 KiB`, `maxThumbSource = 64 MiB`, `maxThumbPixels = 40M`, `maxThumbSide = 30 000` (`thumb.go:29-37`).
- Decode uses `json.Decoder.DisallowUnknownFields()` and rejects a second JSON value (`server_helpers.go:52-65`) — unknown fields are a 400.

### 5.6 Data-plane client and sidecar supervision
- `dataplane.Start` generates a 32-byte hex token, requires a numeric loopback `host:port`, launches the binary with `REVARO_DATA_PLANE_ADDR`/`REVARO_DATA_PLANE_TOKEN`, inherits stdout/stderr, puts the child in its own process group, polls `GET /v1/health` every 100 ms for up to 45 s requiring HTTP 200 + `{"status":"ok","protocol":1}` (`process.go:30-88`). Shutdown sends `SIGTERM` to the process group, waits 10 s, then `SIGKILL` (`process.go:90-103`).
- Client (`storage/dataplane.go`): bearer auth, JSON bodies, 64 KiB bounded error decode (`{"error","code"}`), 404 joined with `ErrNotFound`, response decode capped at 2 MiB; transport tuned to 64 idle conns / 32 per host / 90 s idle. Endpoints: `/v1/media/probe`, `/v1/media/thumbnail` (`code:"artwork"` → `ErrNoCover`, 8 MiB cap), `/v1/media/subtitle` (32 MiB cap), `/v1/archive/extract`, `/v1/archive/{job}/progress`, `/v1/archive/{job}/cancel`. Exact wire field names are in §3.2 / `storage.go:50-94`.

### 5.7 Startup / shutdown (`cmd/server/main.go`)
No flags; a single positional `reset-admin` subcommand. Order: slog text logger → config → signal context (SIGINT/SIGTERM) → `ensureWorkDir` (mkdir 0700 + temp-file write probe) → **start data plane before DB** → `database.Open` → auth `Initialize` → build `DataPlane` client + `Local` store → `store.Ping` (10 s) → `server.New` → register `temporary-uploads` (1h/5m) and `auth` (15m/5m) cleanup jobs → `net.Listen` → write `initial-admin-credentials` (0600) if generated → `http.Server{ReadHeaderTimeout:10s, ReadTimeout:30s, WriteTimeout:0, IdleTimeout:120s, MaxHeaderBytes:1MiB}` → serve → select on signal / serve error / sidecar death → `Shutdown` with 15 s → LIFO defers close app workers, store, DB, sidecar. `reset-admin` opens the same DB, resets credentials, writes the 0600 file.

### 5.8 Database open and pool (`database.go`)
DSN `file:<path>?_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)`; pool `MaxOpenConns=4`, `MaxIdleConns=2`, `ConnMaxIdleTime=5m`; dir chmod 0700 and DB chmod 0600; 10 s ping; migrations as described in §3.1. No `synchronous` pragma, no explicit WAL checkpoint.

### 5.9 Web UI serving (`webui.go`)
Compile-time `//go:embed dist/*`. Paths that exist under `dist` are served by `http.FileServer`; `/assets/*` gets `immutable` long cache; every other path (including `/`, `/f/...`, `/read/...`) is rewritten to `/` and served `index.html` with status 200. `/index.html` itself returns 301 to `/` (FileServer canonicalization). The handler is mounted after the `/api` group so unknown API paths never hit it.

---

## 6. Storage layer

### 6.1 Interface (`storage.go`)
`Storage` (`storage.go:31-48`): `Ping`, `CreateMultipart`, `CompleteMultipart`, `AbortMultipart`, `HeadObject`, `StoreBlob`, `Open`, `ReadFile`, `PutObject`, `OpenRaw`, `GetObject`, `DeleteObject`, `ListPrefix`, `WalkPrefix`, `DeleteObjects`, `PutImmutable`. Optional capabilities are type-asserted: `MediaEngine` (probe/thumbnail/audio-cover/subtitle, `:89-94`) and `ArchiveExtractor` (extract/progress/cancel, `:57-61`). `ReadSeekCloserAt` = `io.{Reader,ReaderAt,Seeker,Closer}` + `Size() int64`.

### 6.2 Object key layout
| Key / directory | Produced by |
|---|---|
| `blobs/<uuid>` | `storage.BlobKey(id)` (`storage.go:96`); created per uploaded file/document/extracted entry |
| `thumbs/<2 hex>/<62 hex>.jpg` | `derivedThumbnailKey(objectKey, namespace)` = first hex char pair of `sha256(objectKey+"|"+namespace)` as directory, remainder + `.jpg` (`thumb.go:104-124`) |
| namespaces: `thumb-v2`, `image-thumb-v1`, `audio-thumb-v1`, `video-thumb-v3` | legacy/typed thumbnail variants; GC knows all four |
| `flows/<bookObjectKey>/f4/manifest.json` | `flow.ManifestObjectKey` (`objects.go:30-32`) |
| `flows/<bookObjectKey>/f4/chunks/<index>.html` | `flow.ChunkObjectKey` (`objects.go:35-37`) |
| `profile/avatar` | `avatarObjectKey` (`server.go:30`) |
| `.multipart/<uuid>/<sha256hex(blobKey)>/<part>` | `multipartDir` (`local.go:278-284`) |
| `.upload-<uuid>` | temp file in the destination directory during `Local.write` (`local.go:139`) |
| `<WorkDir>/cache/<16 hex>.cache` + `.cache.meta` | `cache.diskPath`/`diskName` (`cache.go:848-855`) |
| `<WorkDir>/revaro-extract-<jobID>` | archive extraction staging (`archive.go:312`) |

### 6.3 `Local` implementation (`local.go`)
- Confinement: `os.OpenRoot(dir)` (`local.go:32`) and every operation goes through `l.root`; `validKey` rejects empty, `.`, `..`, backslashes, leading `/`, non-clean paths, and `../` prefixes (`local.go:39-41`). Reads additionally verify the target is a regular file (`:81-84`).
- Writes (`write`, `:132-187`): mkdir parents 0700, create temp `.upload-<uuid>` O_EXCL 0600 in the same directory, `io.Copy` through a context-aware reader bounded to `size+1`, enforce exact size, `fsync` the file, close; then either `Rename(temp,key)` (mutable) or `Link(temp,key)` (immutable) — a hard link that fails with `EEXIST` means another writer won and the existing object is returned via `HeadObject`; finally `fsync` the parent directory. Temp file is removed by defer on any failure.
- `HeadObject` returns `ObjectInfo{Size, ETag}` where `ETag = fmt.Sprintf("%x-%x", size, modTime.UnixNano())` (`local.go:117`) — the ETag is derived from size + mtime, **not** content or inode, so it is stable only while the file is untouched.
- `GetObject` refuses when `limit < 0` or `Size > limit` returning `ErrObjectTooLarge` (`:93-106`).
- `DeleteObject` treats missing as success; `DeleteObjects` is sequential and stops at the first error.
- `WalkPrefix` pages 256 objects at a time, skips dotfiles/dot-directories, validates the prefix, and tolerates missing directories (`:219-271`).
- Multipart: `CreateMultipart` allocates `.multipart/<uuid>/<hash>`; `UploadPart` requires the directory to exist and part ∈ 1..10000; `CompleteMultipart` validates consecutive part numbers, compares **ETags** (trimmed of quotes) against `HeadObject`, concatenates via `io.Pipe` into a fresh `write`, aborts the staging dir on success (`:309-355`); `AbortMultipart` `RemoveAll`s the dir then removes the parent (`:356-369`).
- `CleanupTemporary(age)`: removes `.multipart/<id>` trees and `.upload-*` files older than `age` (`:372-404`); registered as `temporary-uploads` with `cfg.UploadExpires`.
- `Ping` creates and deletes a `.probe-<uuid>` file (`:42-54`).

### 6.4 Object manager policy (`object_manager.go`)
All object mutations funnel through `ObjectManager`: `retry` = 3 attempts with 100 ms/200 ms interruptible backoff, not-found short-circuits, and a final retryable `appError("object_<op>_failed", …)`. `Stream`/`Open`/`CompleteMultipart` are single-shot because streams cannot be replayed; `CompleteMultipart` treats an existing object as success. `Verify` checks size then streams sha256 (`contentHashAlgorithm = "sha256"`, `integrity.go:11`) and returns `size_mismatch`/`hash_failed`/`hash_mismatch`. `Delete`/`DeleteMany` enqueue a durable `object_cleanup` row on failure.

### 6.5 Cleanup queue and generations
Enqueue upserts `object_cleanup` bumping `retry_count` and `generation` (`server_stream_share.go:175-181`); DB triggers also enqueue on file delete/object replacement (migration 002). `CleanupObjects` claims up to 1000 rows ordered by `updated_at`, rebuilds the live key sets (`referencedStorageKeys` scans all file object keys and derives the four thumbnail key variants), skips keys still referenced (deleting the row only when `generation` matches, so a concurrent re-enqueue is preserved), and for `blobs/` keys first deletes all `flows/<key>/` derivatives and all four thumbnail variants. Failures increment `retry_count` and keep the row — there is **no retry cap and no dead-letter**; a full 1000-row batch immediately re-wakes the job. Registered 15m/5m with `runNow=true`.

---

## 7. Background systems

### 7.1 `internal/cache` — global cache manager
- `Class{Name, Priority, SoftQuota, Memory, Disk}`; `Load` path is memory L1 → disk L2 → source (singleflight, success backfills both tiers). Stats count one hit if either tier hits, one miss otherwise; `Loads`/`LoadErrors` count loader executions. Loader runs under a **detached** `context.WithTimeout(m.ctx, 10m)`, so a caller cancelling does not abort the shared load; each waiter selects its own ctx against `flight.ready`.
- Key space is `class + "\x00" + key`. Classes registered by `server/cache.go`: `reader/flow-manifest` (priority 90, 8 MiB, memory-only), `reader/flow-chunk` (70, 64 MiB, memory-only), `reader/source` (40, 512 MiB, disk-only), `media/subtitle` (20, 64 MiB, memory+disk), plus external `reader/books` (parsed-book LRU, 4 entries / 128 MiB). Global memory limit 96 MiB, global disk limit = `MEDIA_CACHE_CAPACITY` (2 GiB default).
- Memory eviction: on every `putMemory`, first converge each class over its soft quota, then evict globally to the memory limit; victim = lowest class priority, tie-broken by LRU (walk from the back). Disk eviction happens only in `Prune` (every 5 min): rescan dir → per-class soft quota → external pruners → global disk budget → memory budget minus external memory. The disk "LRU" is a snapshot scan using `accessed` (process-local) falling back to file mtime.
- Disk file naming: `<WorkDir>/cache/<16 hex>.cache` where the name is `sha256(class\x00key)[:8]` hex; a sibling `.meta` file holds `class\x00key\n<expiresUnixNano>` (0 = no expiry). Writes are temp+rename then meta write; a missing/malformed meta on startup is treated as an orphan and removed. Disk reads never write metadata (mtime untouched).
- `Has` checks memory then the in-memory disk index only. `Invalidate(prefix)` removes matching keys across classes in both tiers. `Close` marks closed (new `Load`s return `context.Canceled`), cancels the context and waits for in-flight loaders. Unregistered classes **panic**.
- `internal/reader.Cache` is a separate object LRU (container/list, newest at back, oversized books rejected) registered externally for stats/budget.

### 7.2 `internal/server/tasks.go` + `task_manager.go` + `task_adapters.go`
- Task row is the single durable state; `taskLifecycleUpdateSet` is the only transition definition: `started_at` set (COALESCE-first-wins) only when status becomes `running`; `finished_at` set for completed/failed/cancelled and reset to NULL otherwise; `heartbeat_at` refreshed only while running. `Ensure` inserts `queued` with `ON CONFLICT DO NOTHING` then selects by `(source_type, source_id)`.
- Task types surfaced: `upload`, `archive_extract`, `subtitle` (list filters to these). Statuses: `queued, running, waiting_input, retrying, completed, failed, cancelled`.
- `RecoverTasks` at startup cancels non-terminal `subtitle` tasks, moves `running` (retry budget left) to `retrying` with `retry_count+1`, and fails the rest. `restorePersistentTasks` re-queues archive tasks in `queued|running|retrying|waiting_input`; restored `waiting_input` gets a fresh 30-minute password deadline. There is no periodic heartbeat writer and no lease-based reaper — recovery is startup-only.
- Runtime wrapper `startRuntimeTask`/`finishRuntimeTask` maps `context.Canceled` → `cancelled`, other errors → `failed` with `publicError`.
- Scheduler: none in these files. Concurrency is bounded by the `ResourceGovernor` (`cpu=1`, `io=3`) and by `Server.runBackground` admission; the manager's `Heavy()` takes both CPU and IO, `IO()` takes one IO slot. Cleanup passes consume the same IO budget as uploads.

### 7.3 `jobs.go` — change bus + SSE
`JobManager` is a payload-less change bus: `Changed()` non-blockingly sends to each subscriber (cap-1 channel), coalescing bursts; `Subscribe` returns an idempotent unsubscribe; `Close` closes all channels. `/api/events` sends an initial `event: jobs\ndata: {"changed":true}\n\n`, repeats on each change, keepalive `: keepalive\n\n` every 20 s. Status constants in Go are only `queued/running/completed/failed/cancelled`; `waiting_input`/`retrying` exist only as strings in SQL/task code.

### 7.4 `cleanup_manager.go`
Single goroutine with a timer; jobs are `{name, interval, timeout, next, failures, run}`. `Register` stores and wakes; `Start` launches the loop; `Close` cancels and waits (jobs run synchronously inside the loop, so an in-flight pass is drained). `Wake(name)` sets `next=now`. Due jobs run sequentially, each under `context.WithTimeout(job.timeout)` holding one IO slot. Failure backoff is `1<<min(failures,6)` minutes, applied only if earlier than the already-scheduled `next` (cap 64 min); success resets failures. Timer floor 1 s.
Registered jobs: **server** `archive-password` 1m/1m no-run-now, `cache` 5m/1m no, `uploads` 15m/5m run-now, `object-cleanup` 15m/5m run-now, `trash` 15m/10m run-now, `orphan-objects` `GCInterval`(1h)/10m run-now (only if >0); **main** `temporary-uploads` 1h/5m run-now, `auth` 15m/5m run-now.

### 7.5 Uploads (`server_uploads.go`, `upload_content.go`)
- Threshold: `size >= 16 MiB` → multipart; part size default 16 MiB, grows and rounds to MiB to stay ≤10 000 parts; single-mode `part_size = max(size,1)`. `ValidMultipartPartCount` rejects >10 000.
- Per-upload serialization uses a ref-counted token map (`lockUpload`), covering local storage + SQLite; `CompleteMultipart` and `abortPendingUpload` take the lock, `uploadContent` as well.
- `createUpload` writes a `pending` `files` row and an `uploads` row in one transaction (`WHERE EXISTS` guards the parent), then creates an `upload` task. On metadata failure after `CreateMultipart`, a deferred abort fires.
- `completeUpload`: idempotent for already-completed uploads (returns the file if the object still matches); multipart completion merges persisted `upload_parts` when the body omits them, requires the exact part count, consecutive numbering and non-empty ETags; size mismatch deletes the object (400); then streams sha256 verification under an IO permit; then `finalizeUpload` flips `files` + `uploads` in one transaction. Task phases: `uploading` → `verifying` (99) → `committing` → `completed` (100), with `retrying` on transient failures.
- Abort/cancel: commits `uploads.status='aborted'` and deletes the pending file row before deleting bytes; object cleanup is deferred (30 s detached ctx) and failures remain durable.
- Expired-upload cleanup selects `status='pending' AND expires_at<=now` and aborts with `expiredOnly=true`, producing task phase `expired`.
- GC (`CollectGarbage`) deletes unreferenced `blobs/` and `thumbs/` objects older than `UploadExpires + 1h` (batched ≤1000) and calls `collectFlowCache`; disabled when `GCInterval=0`.

### 7.6 Archive extraction (`archive.go`)
- `archiveJob` state machine: `queued → checking → (downloading) → extracting → importing → done | failed | cancelled`, with the special `waiting_password` state (30-min TTL, message `压缩包已加密，请输入密码后继续`, wrong-password message `压缩包密码错误，请重新输入`). Archive job progress is mirrored to `tasks` via `persistArchiveTask`; statuses map through `taskStatusForArchive`.
- Extraction runs inside `runBackground` holding `tasks.Heavy()` and a global `archiveSlots` (cap 1). A goroutine polls the Rust `/v1/archive/{id}/progress` every 400 ms and maps phases `downloading` (2–8%) / `extracting` (25%).
- Import validates every walked path with `validateArchivePath` (no absolute, no `..`, no drive prefixes, every component passes `validateName`), rejects symlinks/special files, enforces `maxArchiveEntries = 100 000` and an expansion limit `archiveExpandedLimit = max(64 GiB, min(4 GiB, archiveSize*100))` logic (global 64 GiB; if archive < limit/100 then `max(4 GiB, archiveSize*100)`), streams each file to `blobs/`, hash-verifies via `io.TeeReader` + object hash, and commits a directory tree transaction (`availableArchiveName` falls back to `解压文件`, then `name (2)`…).
- Failure messages are Chinese and structured: disk-space failures are detected via `unix.ENOSPC`/`EDQUOT` or text (`解压失败：临时磁盘空间不足…`); cancellation → `cancelled`/`解压已取消`; other → `解压失败：<err>`. Cancelling a job must not delete an active worker's staging dir. Staging is cleaned on completion/failure/close.

### 7.7 Thumbnails (`thumb.go`)
- Key selection: video extensions → `video-thumb-v3`; audio source → `audio-thumb-v1`; else `image-thumb-v1`; legacy fallback read from `thumb-v2` for non-video.
- Image/EPUB covers: `resizeToJPEG` decodes config first and rejects >40 MP or side >30 000, then CatmullRom-scales to a 640 px longest side and JPEG-encodes at quality 82; two global image slots. Video: async only — request returns 404 and a `thumbnailScheduler` (cap 1) generates via the Rust engine (5-min timeout) and stores with `PutImmutable`; a second request after generation serves bytes. Audio covers use a singleflight group keyed `fileID:etag` plus one slot, validate the JPEG magic `FF D8`, and support re-generation when the cache entry is missing.
- `serveThumb` sets `image/jpeg`, exact `Content-Length`, `Cache-Control: private, max-age=31536000, immutable`, and `ETag = "hex(sha256(bytes))"`; supports HEAD.

### 7.8 Video/audio media (`video_media.go`, `audio_media.go`, `media_metadata.go`, `media_pipeline.go`)
- Metadata: `mediaAnalysisScheduler` (cap 2, dedupes by file ID) runs `ensureMediaMetadata` under a singleflight keyed by file ID; a persisted `media_metadata` row is reused only when `source_etag` matches and `probe_version == 2` **and** (subtitles exist or the row is younger than 24 h — empty probes are refreshed). Probe results are upserted with `max(bitrate,0)`.
- `reanalyzeMedia` drains the in-flight analysis, deletes the row, forgets the singleflight key, clears the subtitle cache, and re-probes synchronously; 422 on failure.
- Video info discovers embedded subtitles (supported codecs `ass,ssa,subrip,srt,webvtt,text,mov_text`) and external sidecars in the same directory plus up to two levels of `Subs`/`Subtitles` folders; matching priorities 0/1/2 and language token inference (`zh-CN`, `zh-TW`, `en`, `ja`, `ko`; labels include `内嵌字幕 N`, `· 强制`, `· 默认`).
- Subtitle conversion is cached through `media/subtitle` with key `embedded-v2:<fileID>:<etag>:<updatedAt>:<index>` or `external-v2:<subtitleID>:<etag>:<updatedAt>` and TTL 2 h; conversion runs under `tasks.Heavy()` and creates a `subtitle` task; responses are `text/vtt; charset=utf-8`, `Cache-Control: private, max-age=3600`. The Rust engine does the actual ffmpeg work.
- Audio info derives chapters (ms→s), `has_cover = video_codec != ""`, and `cover_url = /api/files/<id>/thumbnail?v=<etag>`.

### 7.9 System status (`system_status.go`)
A snapshot is recomputed every 15 s (and on demand after purge/empty-trash) and broadcast to SSE subscribers (cap-1 channels, non-blocking). It reports DB size (`PRAGMA page_count*page_size`), storage ping plus ready-file bytes/trash bytes/count, and cache stats. Status degrades to `"degraded"` per component on errors. Tests pin that `backup`, `tasks`, and `object_cleanup` are **not** exposed.

---

## 8. Reader subsystem

### 8.1 Limits and constants
`MaxTXT = 16 MiB`, `MaxEPUB = 128 MiB`, `maxDecompressedTotal = 256 MiB`, `maxDecompressedEntry = 64 MiB`, `maxRenderedHTML = 64 MiB`, `maxArchiveEntries = 10 000` (`reader.go:15-28`). Flow: `FlowFormatVersion = 4`, `chunkCharsTarget = 7000` UTF-16 units, `chunkBytesTarget = 96 KiB`, `txtBlockCharsTarget = 2000`, `MaxSpines = 1<<14`, `MaxBlocks = 1<<24` (`flow/build.go:18-44`). TXT TOC cap 500 entries, fallback segmentation every 30 000 units when no TOC and >60 000 units. Anchor validation bounds: spine <2^20, block <2^26, path length ≤48, each path index <2^20, offset ∈ [-1, 2^26).

### 8.2 EPUB parse (`epub_parse.go`) and sanitize/render (`epub_render.go`)
- Parse: `zip.NewReader` over a mutex-guarded `ReadSeekerAt` adapter; entry-count cap; read `META-INF/container.xml` → rootfile path → OPF parsed with `encoding/xml`; manifest items resolved relative to the OPF path via `resolvePath`/`NormalizePath` (percent-decode once, collapse `.`/`..` without escaping root); spine idrefs drive chapter order; per-entry and cumulative decompression budgets are enforced (`readZipEntry`); TOC is EPUB3 nav (`properties` contains `nav`, pick `<nav>` with `epub:type="toc"` else the first nav, collect `<a href>` recursively with list depth) with an NCX fallback (`spine toc` id or any `application/x-dtbncx+xml`, recursive navPoints); cover selection is `<meta name="cover" content=id>`, then `properties` containing `cover-image`, then a filename regex `(?i)(^|[/_.\-])cover([/_.\-]|$)`; TXT is decoded as UTF-8 or GBK and chapter titles are found with `txtChapterRe` (`第…章/节/卷/部/回/篇` or `chapter|part N`), offsets counted in UTF-16 units.
- Sanitize/render: an allowlist walk of the parsed body. `script,style,iframe,object,embed,form,input,button,meta,base,link,head,title,noscript` are dropped; `style`/`align`/`on*` attributes are removed; `href` is kept only if not `javascript:`/`vbscript:`/`data:`; `src`/`srcset` are dropped (images are re-emitted as `/api/files/<id>/book/assets/<idx>?v=<etag>` with intrinsic width/height sniffed from the bytes); block tags with only empty/whitespace content are dropped but their `id`s are preserved into the next emitted block's `data-frag-ids`; `<svg>` containing exactly one `<image>` is rewritten to `<img>`; external `data:`/`blob:`/`http(s):` image sources are dropped. Each block gets `data-source-path="<chapter path>"` (Go `%q` formatting, **not** HTML-escaped). Assets are deduplicated by zip path and exposed via a per-book index. Cover is stored raw plus its extension.
- Server integration: `loadBook` first checks the in-memory `reader.Cache` (keyed by object key), else parses from the L2 `reader/source` cache (blobs ≤64 MiB) or streams directly from storage; `bookInfo`, `bookAsset`, `bookCover`, `bookFlow`, `bookFlowChunk` all call `readerFile`, which enforces `.epub`/`.txt` and `MaxEPUB`.

### 8.3 Flow model (`flow/manifest.go`, `flow/objects.go`, `flow/anchor.go`, `flow/build.go`)
`Manifest` JSON:
```json
{"version":4,"format":"epub|txt","total_chars":<UTF-16 units>,"book_key":"<16 hex>",
 "spines":[{"block_start":n,"block_count":n}],
 "chunks":[{"index":n,"block_start":n,"block_count":n,"chars":<UTF-16>,"bytes":n,"url":null}],
 "toc":[{"label":s,"depth":n,"spine":n,"block":n,"nav_anchor":s?,"text_path":[ints]?,"text_offset":n?,
         "chunk":n,"source_path":s?,"source_fragment":s?}],
 "generated_at":s?}
```
`ChunkMeta.URL` and `Manifest.GeneratedAt` are never populated by `Build`; `book_key` is injected by the server as `sha256(blobKey)[:8]` hex (`reader_flow.go:82`, `objects.go:46-49`).

`Anchor{spine, block, path, offset}` is the stable reading position; JSON unmarshalling migrates legacy `{spine, path, offset}` (no `block` field) by promoting `path[0]` to `block` and dropping it from `path` (`anchor.go:104-125`). `Compare` is a total order; `Valid` enforces the bounds above.

Build algorithm (`build.go`):
- EPUB: for each chapter HTML, `html.ParseFragment` into a synthetic `<body>`, keep top-level element nodes as blocks, collect locatable IDs (element `id` + `data-frag-ids`); resolve each TOC entry to `(spine, block)` by matching source path (first block's `data-source-path`) and fragment, then walk the block subtree in document order to bind the first visible content: a media element gets a synthetic `data-rv-anchor="rvn-N"` (deduped by `(spine, fragment)`, IDs assigned in TOC order), a text node records its `text_path` (childNodes index chain from the block) + `text_offset` (UTF-16 offset of the first non-`unicode.IsSpace` rune); then serialize the block tree (so injected anchors are present).
- TXT: split into chapters at TOC UTF-16 offsets (or every 30 000 units, line-aligned, when there is no TOC), then into line-aligned blocks targeting 2000 UTF-16 units; each block is `<div class="txt-blk">escaped text</div>`.
- Assemble: write `data-block="<global index>"` into each block's first start tag (and `data-spine-start` on the first block of every spine except the whole-book first block), injecting before a trailing `/` for void elements; chunk by accumulating block chars/bytes until `chunkCharsTarget`/`chunkBytesTarget`, with an extra early flush when the next block begins a TOC nav anchor and the current chunk is already ≥half-target; compute `total_chars`; map each TOC entry's chapter-local block to a global block and to its chunk.
- Determinism is a stated invariant and is tested byte-for-byte, but see §10.2 for a caveat: EPUB TOC/cover selection iterates Go maps, whose order is randomized.

Object layout: `flows/<bookObjectKey>/f<version>/manifest.json` and `.../chunks/<index>.html`. Server `ensureFlow` short-circuits if the manifest is in the memory cache or exists in storage (HEAD), otherwise singleflights `flow.Build` + object writes with the manifest written last; `flowChunkData` self-heals by rebuilding if a chunk object is missing; `flowCacheKey` = `"manifest/"+objectKey+"/f4"`, chunk = `"chunk/"+objectKey+"/f4/"+index`. `collectFlowCache` GC deletes orphans of deleted books, then TTL (`FLOW_CACHE_TTL`, default 720 h) and capacity (`FLOW_CACHE_CAPACITY`, default 1 GiB, oldest-first) survivors.

HTTP behavior: manifest `private, no-cache`; chunks `private, max-age=31536000, immutable`; both byte-identical across requests (tested). Book progress is persisted in `settings['book_progress/<id>']` as `{"anchor":…}`.

---

## 9. Test inventory (Rust test spec)

36 test files, 157 `Test*` functions, ~6 053 lines. No build tags. Classification legend: **U** pure unit, **FS** filesystem, **DB** SQLite, **HTTP** loopback HTTP server, **DP** needs the data-plane/media engine (ffmpeg).

### 9.1 File-by-file

| Test file | Pkg | Tests | Pins down | Class |
|---|---|---|---|---|
| `cmd/server/main_test.go` | main | `TestWriteCredentialsUsesPrivateFile`, `TestEnsureWorkDirChecksWrites` | 0600 credentials file; work-dir write probe leaves nothing | FS |
| `internal/ids/ids_test.go` | ids | `TestNewIsValidRFC4122V4`, `TestNewIsUnique` | UUIDv4 regex/version/variant; 1000 unique | U |
| `internal/config/config_test.go` | config | `TestLoadDefaults`, `TestDatabasePath`, `TestLoadRejectsInvalidActiveSettings`, `TestTrustedProxyCIDRs` | defaults `/data`,`/work`,2 GiB; `<DataDir>/revaro.db`; bad capacity rejected; CIDR parsing | U |
| `internal/dataplane/process_test.go` | dataplane | `TestStartCancellationReapsUnreadyProcess` | unready child reaped under a 150 ms ctx; no live handle | DP (skips Windows) |
| `internal/database/database_test.go` | database | `TestOpenCreatesSchemaAndMigrations`, `TestOpenIsIdempotent`, `TestOpenCreatesDataDirectory` | exact table set, exactly 2 migrations, root UUID, `foreign_keys=1`, `journal_mode=wal`; idempotent reopen; nested dirs | DB |
| `internal/database/directory_stats_test.go` | database | `TestDirectoryStatsTrackDeepMoveTrashRestoreAndDelete` | trigger-maintained `directory_stats` through move/trash/restore/delete | DB |
| `internal/auth/auth_test.go` | auth | 9 tests (login/session/expiry, initialize, change credentials/username, reset, salted hash, param rejection, full TOTP lifecycle, reset disables TOTP) | Argon2id format+salt; session lifecycle; one-time credentials; bulk revocation; rename keeps session; TOTP replay/recovery/re-encryption | DB |
| `internal/cache/cache_test.go` | cache | 12 tests | tier fallback+backfill; disk-only never memory; TTL; priority/quota eviction; disk prune; external stats/budget; incremental counters; disk hit keeps mtime; singleflight; prefix invalidate; unregistered class panics; `.meta` bytes `disk\x00external\n0` | U+FS |
| `internal/reader/reader_test.go` | reader | 11 tests | EPUB pipeline (title/TOC/chapter HTML/asset rewrite/cover/script+`javascript:` stripping); TXT TOC UTF-16 offsets + GBK; path normalize/resolve; budget + zip-bomb rejection; LRU cache + oversized rejection; image dims; href sanitize | U |
| `internal/reader/flow/build_test.go` | flow | 12 tests | byte-deterministic manifest+chunks; contiguous blocks/chunks; `data-block` continuity; `TotalChars`; text locator round-trip; `rvn-N` media anchors only; spine-start injection; void `/>`; TXT continuity/TOC; anchor legacy migration; Compare/Valid; manifest lookups | U |
| `internal/webui/webui_test.go` | webui | 4 tests | embedded assets; immutable `/assets/*`; SPA fallback; traversal rejected. **Fails (not skips)** without built `dist/assets/*.js` | FS+HTTP |
| `internal/storage/local_test.go` | storage | 3 tests | size-verified atomic writes, cancellation, immutable no-overwrite, read limit, symlink/traversal confinement; multipart survives reopen + ETag verification; `WalkPrefix` scoping/missing dirs | FS |
| `internal/storage/dataplane_test.go` | storage | `TestLocalBlobLifecycle` | 20 MiB store→seek→read→list→delete | FS |
| `internal/server/server_test.go` | server | `TestClientIPOnlyTrustsConfiguredProxy`, `TestLoginLimiterHasBoundedState`, `TestServerCloseWaitsForOwnedWorkAndRejectsNewWork` (+ the `newTestApp` harness) | rightmost-untrusted XFF; limiter bounded; `Close` drains and rejects new work | U/DB+HTTP |
| `internal/server/server_server_test.go` | server | `TestLegacyTaskQueryEndpointsAreRemoved`, `TestDeletePendingFile`, `TestUnknownAPIEndpointReturnsJSON404` | retired endpoints 404/405; pending-file delete cascades upload; JSON 404 | DB+HTTP |
| `internal/server/server_files_test.go` | server | 8 tests | root immutability, duplicate 409; Origin required 403; directory cycle 400; document CRUD/ETag rotation/stale 409; copy preserves audio metadata + `album - 副本.m4a`; integrity metadata | DB+HTTP |
| `internal/server/server_auth_test.go` | server | 4 tests | credential change requires password + revokes; field endpoints; full TOTP API incl. `code:"totp_required"` and 10 codes; avatar PUT/GET/DELETE + `has_avatar` | DB+HTTP |
| `internal/server/server_trash_test.go` | server | 4 tests | trash tree hides descendants, bytes/count, restore, 409, purge; trash stays readable; empty trash; 30-day expiry only | DB+FS+HTTP |
| `internal/server/server_uploads_test.go` | server | 13 tests | single/multipart/empty lifecycles, idempotent complete, task visibility, `blobs/` opacity, sha256 metadata, GC batching ≤1000 + referenced retention (incl. audio covers), expiry, part size, abort/complete race, finalize after row delete, deferred cleanup, parent-deleted-mid-write | DB+FS+HTTP |
| `internal/server/server_media_test.go` | server | 9 tests | MIME inference + `safeDeliveryMime`; children stats/preview; TXT/EPUB book endpoints, assets, cover, 415; JPEG thumbnails + immutable cache; async video thumb 404; typed thumbnail namespaces; audio cover singleflight self-heal; ffmpeg video frame; media progress | DB+FS+HTTP (+DP for ffmpeg) |
| `internal/server/server_shares_test.go` | server | 2 tests | share create/read/rotate/revoke; public range 206 exact bytes | DB+HTTP |
| `internal/server/download_batch_test.go` | server | 6 tests | ZIP bytes/content-disposition, duplicate name `file (2).txt`, traversal stripping, 400/404/409, auth required, one-time token | DB+HTTP+FS |
| `internal/server/reader_flow_test.go` | server | 8 tests | flow manifest/chunk headers + byte identity; no rebuild on second open; self-heal missing chunk; source L2; TXT total chars; anchor round-trip/legacy; legacy endpoints 404; flow GC | DB+FS+HTTP |
| `internal/server/local_storage_test.go` | server | 2 tests | real local store end-to-end: upload URL, auth, short body, sha256, range `bytes 2-4/6`, 416, share, trash, GC aging; multipart HTTP resume | FS+DB+HTTP |
| `internal/server/local_cleanup_test.go` | server | 6 tests | shared/derived object retention, durable queue across disk failure/restart, explicit delete bypasses orphan grace, orphan GC freshness/pending, metadata rollback keeps blob | FS+DB+HTTP |
| `internal/server/library_test.go` | server | 2 tests | counts `{book:2,image:1,video:1,audio:1,file:6}`, folder path ancestry, per-type + `/all`, unknown type 400, trash excluded | DB+HTTP |
| `internal/server/thumb_test.go` | server | 2 tests | canonical thumbnail key layout/length; scheduler dedupe + concurrency 1 | U |
| `internal/server/media_metadata_test.go` | server | 2 tests | stale `probe_version` re-probe exactly once + embedded subtitles playable; analysis scheduler cap 2 + dedupe | DB+HTTP / U |
| `internal/server/video_media_test.go` | server | 5 tests | sidecar match priority/language; conversion survives request cancellation + caches; subtitle API `text/vtt; charset=utf-8`; embedded ASS timeline preserved | U (1–3); DB+HTTP+DP (4–5) |
| `internal/server/native_playback_test.go` | server | 1 test | preview serves original bytes with Range; retired HLS/fmp4 endpoints 404 | DB+HTTP |
| `internal/server/system_status_test.go` | server | 2 tests | 401 unauth, components `ok`, removed components absent; storage byte progression | DB+HTTP |
| `internal/server/task_manager_test.go` | server | 1 test | `Update` by selector ≡ `UpdateID`: status/phase/progress + started/heartbeat/finished stamps | DB |
| `internal/server/jobs_test.go` | server | 2 tests | cancel/notify race safety; coalesced change delivered ≤1 s | U |
| `internal/server/managers_test.go` | server | 2 tests | `AppError` keeps cause + safe message; cleanup retries a failed pass | U |
| `internal/server/resources_test.go` | server | 2 tests | Heavy serialized; IO bounded at 3 | U |
| `internal/server/archive_test.go` | server | 6 tests | cancel preserves active staging; name/suffix/base-name table; `解压失败：invalid archive` + structured log; ENOSPC message; password resume state machine; password TTL cleans staging | U+FS |

### 9.2 Test harness (must be re-created in Rust)
- `newTestApp` (`server_test.go:308`): temp SQLite DB, cheap Argon2 params, in-memory `mockStorage` (`:91-284` — raw map, multipart map, ordered `putKeys`, `deleteBatchSizes`, injectable errors, `age()`), a real loopback `httptest.NewServer` for blob URLs, `BaseURL: http://example.test`, login + session cookie, cleanup registration.
- Helpers: `readyFile`, `createUpload`, `request`/`requestRaw`/`requestH`/`rawRequest`, generic `decode[T]`, `fakePNG`/`realPNG`/`realJPEG`, `buildEPUB`; `localTestApp` (real `storage.Local`); `uploadLocalFile`; `deletionFailureStore`; batch-download and library helpers; `storeFlowPuts`; `gatedUploadStorage`, `parentDeletingStorage`.
- 31 sites reach unexported internals (`a.srv.objects.store`, `a.srv.cache`, `a.srv.books`, `a.srv.tasks`, `a.srv.probeMediaSource`, `a.srv.generateAudioCover`, `a.srv.thumbnails`, `a.srv.cleanup`), so the Rust port needs a same-crate white-box test module and injectable function fields/seams plus a mutable `Storage` mock.

### 9.3 Exact-string / byte contracts (port byte-exactly)
- Batch ZIP: `Content-Disposition: attachment; filename="revaro-download.zip"`; entry names `file.txt`, `file (2).txt`; no separators/`.`/`..`.
- Cache `.meta`: `disk\x00external\n0`.
- Thumbnail key length/shape: `thumbs/` + 2-char namespace + 62-char digest + `.jpg`.
- Archive strings: `解压失败：invalid archive`, `正在验证密码`, `超时`, `临时磁盘空间不足`; structured WARN log fields `file`, `job`, `status`, `error`.
- Flow: manifest/chunk byte-identical across builds and requests; `data-block="0"`, `data-spine-start`, `data-rv-anchor="rvn-0"`, void `/>`, `TextPath []int{1,1,0}`/`TextOffset 1`.
- Subtitles: `text/vtt; charset=utf-8`; cached body exactly `"WEBVTT\n"`; ASS cues `00:00.000 --> 00:01.000`, `06:15.000 --> 06:20.000`, `六分钟字幕`; cache keys `embedded-v2:<id>:<etag>:<updatedAt>:<idx>`.
- HTTP: share body `name: value\n`; `Content-Range: bytes 2-4/6`; upload URL `/api/uploads/<id>/data`; copy name `album - 副本.m4a`; `application/yaml; charset=utf-8`; thumbnail JPEG magic `FF D8`.
- EPUB render: `data-source-path="OEBPS/ch1.xhtml"`, `/api/files/f1/book/assets/0?v=test-etag" width="10" height="20"`, `alt="插图"`, no `<script`/`javascript:`/`alert`.
- Auth: `"code":"totp_required"`, `otpauth://totp/` prefix, `data:image/png;base64,`, exactly 10 recovery codes, plaintext code absent from `settings`.

### 9.4 Skips, flakiness, environment
- Skips: `dataplane/process_test.go` on Windows; `video_media_test.go` and `server_media_test.go` `TestVideoThumbnailWithFFmpeg` without `ffmpeg`/`ffprobe`; `requireMediaEngine` skips the three media-engine tests unless storage implements `MediaEngine`.
- Hard failure without artifacts: `webui_test.go` fails unless `dist/assets/*.js` exists.
- No external network; tests bind loopback sockets.
- Time-dependent: cache TTL sleeps (250/20 ms), thumbnail/scheduler negative windows (20–50 ms), cleanup 3 s polls with `os.Chtimes(-48h)`, trash back-dated 31 days, 100 ms upload abort/complete race, TOTP fixed clock `2026-08-22 01:00 UTC` advanced in 30 s steps.
- Asserted status codes include 200/201/204/206/301/400/401/403/404/405/409/415/416/502.

---

## 10. Hard-to-port items and risks

### 10.1 Go-runtime / language specifics
1. **`os.Root` confinement + `os.OpenRoot`** (`local.go:32`) — the whole path-safety model is Go-API-specific; Rust needs a dir-fd/`openat2(RESOLVE_BENEATH)`-style equivalent or a vetted crate, plus the same `validKey` rules.
2. **`os.Root.Link` create-only hard links + `Rename` + parent `fsync`** (`local.go:166-185`) — atomic publication semantics must be reproduced exactly; `EEXIST` means "another writer won".
3. **`syscall.SysProcAttr{Setpgid}` + `Kill(-pid, SIGTERM/SIGKILL)`** (`process.go:43,95,99`) — process-group lifecycle needs `nix`/`CommandExt` and careful reaping.
4. **`golang.org/x/net/html` parser semantics** — HTML5 tree construction; foreign-attribute rewriting (`xlink:href` → Namespace `xlink`, Key `href`) means the Go sanitizer re-emits it as plain `href`; `html.Render` void-element serialization (`<img .../>`) is relied on by `injectDataBlock`. Rust `html5ever` keeps qualified names — a port must normalize explicitly or change output.
5. **`fmt` `%q` in attribute emission** (`epub_render.go:163-166`) — Go's `%q` is not HTML escaping; reproducing it is necessary for byte-compatibility but carries attribute-injection risk (see §10.3).
6. **`encoding/xml`/`archive/zip` quirks** — the parser normalizes entry paths itself, tolerates missing/odd entries, and enforces decompression budgets; Rust zip/xml crates differ on lenient parsing and encoding.
7. **Go `regexp`** (RE2) for TXT chapter detection and cover heuristics — no backreferences; Rust `regex` is compatible but `.`-vs-newline and `(?im)` handling must match, and `FindAllStringSubmatchIndex` semantics must be replicated.
8. **`utf16.RuneLen`/manual UTF-16 counting** everywhere (`reader/text_reader.go:71`, `flow/build.go:804-836`) — all offsets are UTF-16 code units (JS `string.length`); Rust `String` is UTF-8, so every offset/limit conversion must be explicit.
9. **`container/list` intrusive LRU with `*list.Element` back-pointers** (`reader/cache.go`, `cache.go:88-96`) — Rust needs an index-based structure (`lru`/`indexmap`) with identical byte accounting.
10. **Hand-rolled `singleflight` over a shared `chan struct{}` broadcast** (`cache.go:98-102,310-339`; `singleflight.Group` in server) — the loader is **detached from the caller context** but dies with the manager; waiters race their own `ctx.Done()`.
11. **`context.Context` cancellation/timeout tree** used pervasively; the loader/caller independence rule must be modeled with explicit cancellation tokens.
12. **Channel-semaphore concurrency (`chan struct{}`)** for CPU/IO/slots (`resources.go:13`, `thumb.go:45`, `server.go:104-108`) — no fairness guarantee in Go; Rust `Semaphore` ordering must be chosen deliberately.
13. **`sync.Once`/`defer`-based cleanup and two-lock nesting (`diskMu → mu`)** — deadlock ordering must be preserved.
14. **`panic` as programmer-error handling** (`cache.go:194,205`; `ids.go:13`) — Rust should convert to typed errors/closed enums.
15. **`rand.Text()`** (Go 1.24+) for generated admin passwords (`auth.go:82,232`) — must pin an alphabet/length for compatibility.
16. **`time.RFC3339Nano` string timestamps everywhere** with `time.Now().UTC()` — Rust must emit/parse nanosecond precision (trailing-zero trimming differences matter for string comparisons); the injectable `Service.Now` is used inconsistently (some paths call `time.Now()` directly).

### 10.2 Correctness/consistency hazards discovered
1. **EPUB TOC/cover selection iterates Go maps** (`epub_parse.go:183,196`, `epub_render.go:369,377`) — map order is randomized, so which nav doc / cover item wins can differ between runs. This contradicts the flow's "byte-deterministic" invariant and makes the exact-output tests order-sensitive. A Rust port should make the choice deterministic (e.g. sorted manifest order); doing so **changes behavior** and may require updating golden expectations.
2. **`%q` attribute injection** (`epub_render.go:163-166`): `data-source-path` and `data-frag-ids` are emitted with Go `%q`, not HTML escaping. A `"` in a zip entry name or element id can break out of the attribute in the sanitized output. A Rust port must decide between bug-compatibility and HTML escaping.
3. **`renderBudget.used` is shared and never reset across chapters** (`epub_parse.go:58,101`) — `maxRenderedHTML = 64 MiB` is a whole-book cap, not per-chapter.
4. **`Manifest.SpineForBlock` returns the last spine for out-of-range blocks** (`manifest.go:79-96`) despite the comment saying 0 — only reachable for malformed input.
5. **`BookFingerprint` hashes 8 bytes (16 hex chars)**, comments say 16 bytes (`objects.go:43-49`).
6. **`reader.Cache` memoizes `Book.size` outside the mutex** (`reader.go:75-88`) → data race under concurrent Get/Put; `trimLocked` deletes by O(n) scan (`cache.go:91-96`).
7. **`ChunkMeta.URL` and `Manifest.GeneratedAt` are never set**; server injects `book_key`.
8. **`JobManager.Subscribe` after `Close`** returns a channel that is never closed, so a waiter blocks until its request context ends (`jobs.go:42-60`).
9. **`object_cleanup` has no retry cap or dead-letter** (`server_stream_share.go:136-145`); `tasks.retry_count` is only enforced by the manual retry endpoint and startup recovery, never by an automatic retry worker.
10. **`Local` ETag is `size-modtime`** (`local.go:117`) — not content-addressed; media metadata/subtitle caches keyed on it can go stale if an object is replaced with identical size and preserved mtime (unlikely but not impossible).
11. **`ChangeUsername` keeps sessions but changes the username returned by `Authenticate`** (it re-reads `settings`), so the session identity silently changes.
12. **`deleteFile` on a non-ready file returns 204 without a body even when the pending upload was aborted** (`server_files.go:560-585`), while other delete paths return 204 too — clients must not expect a payload.

### 10.3 cgo / OS / external-tool dependencies
- **No cgo.** `modernc.org/sqlite` is pure Go; `golang.org/x/image`, `x/net`, `x/text`, `x/crypto` are pure Go. The Rust port can use pure-Rust SQLite (`rusqlite` bundled, `libsql`) but must reproduce the same schema/triggers and pragmas.
- **OS specifics:** `syscall.SysProcAttr{Setpgid}` + process-group kill (Unix only; `process_test.go` skips Windows); `golang.org/x/sys/unix` `ENOSPC`/`EDQUOT` checks in archive error mapping; file modes 0700/0600; `os.Root` symlink confinement.
- **ffmpeg:** all probing, thumbnails, audio covers and subtitle conversion are delegated to the Rust data plane (ffmpeg/libav). Go itself never links ffmpeg. The three tests requiring it skip if absent.
- **libarchive:** archive extraction is delegated to the Rust data plane (`libarchive2-sys`); Go only validates/imports the resulting directory tree and reports progress.
- **Rust sidecar contract** must be preserved exactly: loopback-only bearer auth, `{"status":"ok","protocol":1}` health, and the JSON field names/endpoints in §5.6.

### 10.4 Top 10 riskiest areas
1. **Reader flow build** (`flow/build.go`, 841 LOC of HTML parsing, UTF-16 arithmetic, anchor binding, deterministic chunking) — byte-exact output is required and HTML5 tree semantics differ across parsers.
2. **EPUB parse + sanitize** (`epub_parse.go`/`epub_render.go`) — zip/XML leniency, decompression budgets, whitelist emission, `%q` attribute behavior, map-order nondeterminism.
3. **Global cache manager** (`cache.go`, 980 LOC) — L1/L2/LRU+priority/soft-quota/external-budget eviction, singleflight, restart-time disk index reconciliation, exact on-disk `.meta` format.
4. **SQLite schema + triggers + recursive-CTE directory stats** — must be recreated verbatim or reimplemented with identical invariants; migrations are the compatibility contract.
5. **Upload lifecycle** (create/parts/complete/abort/expiry, per-upload locking, idempotency, integrity verification, object cleanup generations) — many concurrency edge cases pinned by tests.
6. **Archive extraction state machine** (staging, password wait, Rust progress polling, path validation, expansion limits, import/commit, Chinese error strings).
7. **Media metadata/subtitle caching** (probe versioning, empty-probe TTL, singleflight, cache keys keyed on ETag+UpdatedAt, embedded vs external subtitle matching, WebVTT conversion delegation).
8. **Object cleanup queue + GC** (generations, referenced-key reconstruction including four thumbnail namespaces and `flows/` derivatives, no dead-letter, durable retries).
9. **Auth/TOTP** (Argon2id string compatibility, unpadded base64, AES-GCM secret encryption with AAD, replay step store, unsalted recovery-code hashes, session invalidation matrix).
10. **Cross-cutting HTTP semantics** (chi route table, `DisallowUnknownFields`, strict JSON single-value decode, origin guard on every write, `http.ServeContent` Range behavior, `net/http` cookie/MIME/canonicalization quirks, byte-exact SSE framing).

---

## 11. Surprising / undocumented behavior (candidates for a porting decision log)

1. **TOTP recovery-code hashes are unsalted SHA-256** with a fixed prefix (`revaro-recovery-v1:`) and are stored as a JSON array in `settings` — no per-code salt, no KDF (`totp.go:417-420`).
2. **TOTP secret encryption uses the account password as the Argon2 input** and AES-GCM AAD `revaro-totp-v1`; changing the password re-encrypts it in the same transaction but changing the username does not (`auth.go:149-168`).
3. **`originGuard` rejects any write without an `Origin` header**, including non-browser API clients and `POST /api/auth/login` — curl without `-H 'Origin: …'` gets 403, not 401 (`server.go:350-369`).
4. **Public-share tokens are exactly the same format as session tokens and batch-download tokens** (32 random bytes, base64 RawURL, 43 chars), generated by `newShareToken` (`server_stream_share.go:265-271`).
5. **`problem(w, 499, …)` is returned for client-cancelled uploads** (`upload_content.go:18`, `server_uploads.go:307`), a non-standard nginx-style status, despite the client being gone.
6. **Copy names use the Chinese suffix `" - 副本"`** (`server_files.go:529`), and archive extraction names fall back to `解压文件` (`archive.go:492`). Tests pin `album - 副本.m4a`.
7. **`Local.HeadObject` ETag is derived from file size and mtime** (`fmt.Sprintf("%x-%x", size, modTime.UnixNano())`), not from content (`local.go:117`).
8. **Upload object keys are `blobs/<new UUID>` allocated at session creation**, so a failed upload leaves an object key that GC must reclaim; ETags are never used for uploads (parts use ETags only for staging integrity).
9. **`ChangeCredentials`/`ResetCredentials` delete all sessions but `changePassword`/`changeCredentials` handlers also clear the caller's cookie**, forcing immediate re-login; no graceful session migration.
10. **TOTP setup enabling/deleting revokes all *other* sessions** but keeps the current one (`revokeOtherSessions`), while password change revokes *all*.
11. **`listTasks` deliberately hides completed/cancelled tasks after 30 minutes** while keeping the durable rows forever (`tasks.go:59-62`).
12. **`media_probe_version = 2`**: a metadata row is reused only when the stored version matches; empty probe results (no subtitles) are refreshed after 24 h (`media_metadata.go:81-136`).
13. **Empty media probes are considered "incomplete"** and re-probed, so a media file with no subtitles is re-probed daily.
14. **`archiveExpandedLimit` can be larger than the archive's own size by 100×** with a 4 GiB floor (`archive.go:229-235`) — a deliberate zip-bomb policy that must be preserved.
15. **`has_cover` on `File` in children listings is derived from `media_metadata.video_codec <> ''` with a matching `source_etag`** (`server_files.go:113-138`), not from a stored flag.
16. **Book progress is stored in the generic `settings` table under `book_progress/<fileID>`**, while media progress uses the `media_progress` table — two different persistence models for similar concepts.
17. **Reader flow manifests are cached memory-only** and validated by a storage HEAD, while chunks are also memory-only; the manifest is written last so a reader never sees a manifest with missing chunks (`reader_flow.go:42-96`).
18. **`ensureFlow` is idempotent and self-healing**: a missing chunk object triggers a full rebuild on the next chunk request (`reader_flow.go:108-120`).
19. **`internal/reader/flow` depends on `internal/reader`** (for `Book`, `NormalizePath`) — the dependency direction means the Rust port must keep the parse model and flow builder in the same crate or re-seat the shared normalization helper.
20. **`docs/reader-flow.md` still references S3 and HLS** (“miss 后直接回源 S3”, `media/hls` class) though the product is local-only and no HLS module exists in `internal/`; the doc is stale relative to the code (current code uses local storage and has no HLS cache class).
21. **`internal/webui/dist` is gitignored**, yet `//go:embed dist/*` makes the Go build fail without it; the Rust port needs an equivalent build-order/embed contract.
22. **`Server.Close` closes the status SSE broadcaster and cancels `workCtx` *before* waiting for in-flight work**, and archive staging cleanup happens after `lifecycleWG.Wait()` (`server.go:183-225`) — shutdown ordering is subtle.
23. **`cache.Manager` keeps working after `Close()` for `Put`/`Get`/`Prune`**; only `Load` is gated (`cache.go:306-309,733-741`).
24. **`JobManager` status constants omit `waiting_input`/`retrying`** even though they are first-class DB statuses (`jobs.go:11-17` vs `001_local_product.sql:111`).
25. **`/api/system/status` intentionally omits `backup`, `tasks`, and `object_cleanup`**; tests assert their absence, so adding them in the Rust port would break the contract.

---

### Appendix A — Response/limits quick reference
| Constant | Value | Location |
|---|---|---|
| JSON body max | 7 MiB | `server.go:27` |
| Document max | 1 MiB | `server.go:28` |
| Avatar max | 2 MiB | `server.go:29` |
| Logical file max | 1 TiB | `server.go:31` |
| Multipart threshold / default part | 16 MiB / 16 MiB | `server_uploads.go:64-65` |
| Max parts | 10 000 | `storage.go:105`, `local.go:301` |
| Batch download files / TTL / tokens | 1000 / 2 min / 256 | `download_batch.go:18-22` |
| Public share concurrency | 8 | `server.go:104` |
| Media analysis / thumbnails / audio cover / archive slots | 2 / 1 / 1 / 1 | `server.go:105-108` |
| Image thumbnail slots | 2 (package global) | `thumb.go:45` |
| Resource governor CPU / IO | 1 / 3 | `resources.go:13` |
| Book cache | 4 books / 128 MiB | `server/cache.go:25-26` |
| Global cache memory / disk | 96 MiB / `MEDIA_CACHE_CAPACITY` (2 GiB) | `server/cache.go:23,45` |
| Session lifetime | 30 days | `auth.go:21` |
| TOTP setup TTL / password wait | 10 min / 30 min | `totp.go:32`, `archive.go:30` |
| Login: per-IP / global / window / slots | 5 / 30 / 15 min / 2 | `server_helpers.go:89-92,108,116` |
| HTTP server timeouts | header 10 s / read 30 s / write 0 / idle 120 s / header bytes 1 MiB | `main.go:98` |
| DB pool | max 4 / idle 2 / idle time 5 min; busy_timeout 5000 ms; WAL | `database.go:30,38-40` |

### Appendix B — Method to reproduce
All facts were gathered by reading the cited files with line-numbered reads and by targeted `grep`/`wc` over `cmd/` and `internal/`. No Go compilation or test execution was performed (the Go toolchain is not installed in this environment); runtime claims are derived from source semantics and the test suite, not from execution.
