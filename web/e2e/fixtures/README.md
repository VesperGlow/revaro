`preview.webm` is a generated 30-second, silent solid-color clip for browser playback tests. It contains no third-party media.

Regenerate from the repository root:

```sh
ffmpeg -f lavfi -i 'color=c=0x364a55:s=640x360:r=10:d=30' -an -c:v libvpx-vp9 -b:v 25k -y web/e2e/fixtures/preview.webm
```

`media-ui.spec.ts` generates its WAV audio and SVG artwork in memory and mocks the API. Run the media and reader checks from `web` with:

```sh
npx playwright test -c playwright.ui.config.ts --grep-invert '@benchmark'
```

Set `PLAYWRIGHT_EXECUTABLE_PATH` to use an installed Chromium instead of Playwright's managed browser.
