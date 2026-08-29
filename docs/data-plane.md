# Go control plane / Rust data plane

Revaro has one metadata authority: the Go process and its SQLite connection.
The Rust process is deliberately unable to open the database. It operates only
on opaque object keys and ephemeral job IDs supplied by Go.

## Ownership

| Concern | Go control plane | Rust data plane |
|---|---|---|
| HTTP API, cookies, auth, UI | owns | never exposed publicly |
| SQLite and logical filesystem | sole reader/writer | no database path or SQL API |
| `blobs/<UUID>` allocation and references | owns | treats keys as opaque |
| S3, Range readers and multipart streams | requests and commits results | owns transport and retries |
| BT, URL import, archive and media jobs | validates intent and persists state | owns bounded execution and reports events |
| codecs and containers | selects user-visible policy | libav engine; no CLI and no codec implementation |

## Commit rule

A data-plane operation may create an unreferenced blob, but it can never make
that blob visible in the logical filesystem. Go validates the returned size and
ETag and commits the SQLite transaction. If the commit fails, Go schedules the
opaque key for idempotent deletion. Restart recovery follows the same rule:
SQLite is authoritative, while unreferenced objects are garbage collected after
the existing upload grace period.

## Transport and resource rules

The private protocol is served on loopback and authenticated with a random
per-process bearer token. Request cancellation closes the response/request body
and is propagated into the Rust task. Streaming bodies are bounded channels;
producers cannot outrun S3 or the public HTTP client. Limits are explicit for
concurrent jobs, buffered bytes, archive entries and expanded bytes. Seekable
reads use coalesced S3 Range windows and discard them on cancellation.

Large payloads are streamed in HTTP bodies. JSON carries only bounded control
messages and object facts. No media frame, archive member, BT piece, or S3 object
is encoded into JSON.

## Compatibility

Public routes and JSON shapes remain Go-owned. SQLite migrations, logical UUIDs,
object keys, trash semantics and browser presigned transfers therefore remain
compatible across the migration. The private protocol is versioned separately
under `/v1` and is not a public API.
