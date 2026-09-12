# CI and image builds

Only `linux/amd64` is built. Quality checks and the real-container E2E job run in parallel; GHCR publication waits for both to succeed. Rust fmt, Clippy and tests remain in the Dockerfile dependency chain. No checks are skipped for speed.

- `.dockerignore` excludes Go and Web unit tests from production image inputs. The quality job still tests the full checkout. Rust inline tests are retained. Test-only edits no longer invalidate production source-copy layers.
- The Go dependency layer also compiles the standard library with the final binary's `CGO_ENABLED=0`, Linux and `-trimpath` settings. Its filesystem build cache is exported with the layer, so fresh runners can reuse it after application source changes.
- The container job is the sole writer of the `revaro-image-amd64-v2` BuildKit cache (`mode=max` preserves intermediate build stages). Publication reads it without exporting it a second time. Cache failures can cause rebuilding but never bypass checks.
- Main builds are not cancelled midway through cache export. PR runs still cancel superseded work. Node, Go, Playwright and cargo-audit caches remain enabled.

The preceding successful main run, [34674782858](https://github.com/VesperGlow/revaro/actions/runs/34674782858), spent about 326 seconds building the checked image, 121 seconds running E2E and only 6 seconds publishing the cached image. This is why publication still uses BuildKit reuse instead of introducing a large cross-job image artifact. Timings depend on cache availability and runner load; the next build also pays for new cache layers.

References: [Docker cache optimization](https://docs.docker.com/build/cache/optimize/) and [GitHub Actions cache backend](https://docs.docker.com/build/cache/backends/gha/).
