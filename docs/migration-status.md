# Go control plane / Rust data plane migration status

The migration is implemented. Go remains the only SQLite and logical-filesystem
owner; Rust receives opaque `blobs/<UUID>` keys and local staging paths only.

## Implemented boundary

- Rust owns S3 Range reads, bounded streaming uploads, multipart operations and
  stale/orphan multipart cleanup.
- Rust owns librqbit downloads, progressive file streams, seek-driven piece
  prioritization, private-address peer blocking, JSON session persistence and
  fastresume.
- Rust owns libarchive extraction, password retry, download/extract progress,
  cancellation and bounded expansion.
- Rust owns media probe, thumbnail, subtitle conversion, fMP4, HLS and audio
  merge through libav. It selects libx264 for incompatible video and the libav
  AAC encoder for incompatible audio; it does not invoke ffmpeg/ffprobe CLI or
  implement codecs.
- Audio merge preserves mono or matching multichannel layouts, original
  filename chapters, embedded cover art and merged mov_text subtitles.
- Go owns public API/auth/UI, SQLite transactions, file metadata and final blob
  visibility. Existing public routes and the SQLite/blobs layout are unchanged.

## Runtime guarantees

- The data plane uses two Tokio workers, one concurrent media job, one archive
  job, four S3 transfer slots, one libx264 frame thread and bounded stream
  channels. librqbit initialization, worker threads and peers are bounded.
- SIGTERM/SIGINT cancels media/archive work, stops librqbit streams and then
  drains the private HTTP server. Go retains a ten-second forced-stop fallback.
- Media seeks normalize output timestamps to zero. Encoded audio uses a 48 kHz
  encoder time base, sample-count PTS and explicit resampler flush; encoded
  video retains frame rate/aspect ratio and emits GLOBAL_HEADER when required.
- Interrupted internal multipart uploads have a Drop abort guard. Client-side
  uploads can explicitly abort, and stale uploads are swept at worker startup.

## Acceptance coverage

`data-plane/tests/real-integration.sh` is the opt-in real fixture suite. It runs
against S3/MinIO and checks H.264/AAC stream copy, HEVC and VP9 to H.264, Opus to
AAC, fMP4, HLS seek, ten-minute A/V sync, mono and 5.1 audio, filename chapters,
embedded cover/subtitles, S3 Range/streaming, multipart complete/cancel/restart
cleanup, and archive cancel/password/restart. The Rust BT integration test uses
a real local seeder/client to check progressive tail seek and fastresume after
session restart.

Normal gates remain:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
go test ./...
go vet ./...
npm test && npm run build && npm run lint
```
