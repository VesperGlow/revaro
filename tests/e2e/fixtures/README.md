# Revaro browser fixture

`preview.webm` is a checked in VP9 sample used by the Rust media viewer test.
The test uploads it through the application and exercises native video playback,
playback controls against a real `revaro` process.

Run the current browser tests from this directory:

```sh
npm ci
npx playwright install chromium
npm test
```

`preview.flac` is an original 30-second mono silent FLAC (8 kHz, 16-bit),
encoded once with libFLAC for native audio tests. The shared fixture at
`crates/revaro-server/tests/fixtures/preview-chapters.flac` keeps those same
samples and adds six native CUESHEET/Vorbis chapters at 0, 5, 10, 15, 20 and 25
seconds. It lives in the Cargo workspace so Rust API tests can also compile in
the Docker build, where browser tests are excluded. Rust API tests verify those
titles/times survive a conflicting chapter sidecar and a cached probe.

`audio-playback.spec.ts` covers ordinary categories, chapter/subtitle sidecars,
seeking, buffered ranges, next-track metadata, automatic advancement, refresh
resume and HTTP HEAD/206. `music-cards-and-player.spec.ts` checks square cards,
native chapters, transcript highlighting/scrolling/seeking and the shared full
player at desktop and mobile widths.

For the optional large-file browser check, import the supplied FLAC and its
same-name VTT and set `E2E_LARGE_AUDIO_ID` to that file's ID. Its six embedded
chapters provide the seek targets; no synthetic chapter file is needed.
