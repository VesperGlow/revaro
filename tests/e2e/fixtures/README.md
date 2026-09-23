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
