# Frontend Migration Inventory — Vue 3 + TypeScript → Leptos (Rust/WASM, CSR)

Audit of `/config/revaro/web`. Every statement below was derived by reading the actual
source files listed; no file other than this report was modified and no build/test was run.

Companion source of truth for the reader design: `/config/revaro/docs/reader-flow.md`
(server + client reading-flow architecture).

---

## 0. Scope, size and method

| Area | Files | LOC |
|---|---|---|
| Root SFCs — `src/App.vue`, `src/Reader.vue`, `src/VideoThumb.vue` | 3 | 262 |
| `src/components/**/*.vue` | 31 | 1,963 |
| `src/composables/**/*.ts` | 14 | 2,154 |
| `src/reader/**/*.ts` (non-test) | 6 | 559 |
| Root `src/*.ts` app modules (`api`, `app-controller`, `fileTypes`, `format`, `imageGeometry`, `library`, `taskStatus`, `types`, `videoPlayer`, `main`, `vite-env.d.ts`, `fileSystemAccess.d.ts`) | 12 | 913 |
| **Non-test TS + Vue total** | **66** | **5,851** |
| Global CSS (`src/*.css` + `src/styles/*.css`) | 16 | 4,549 |
| Vitest unit tests | 13 | 669 |
| Playwright e2e (specs + helpers) | 8 | 1,850 |
| **Grand total (source + CSS + tests)** | | **12,919** |

External runtime dependencies (`web/package.json`): `vue@3.5.41`, `@lucide/vue@1.41.0`
(icon components), `marked@17.0.5` (Markdown), `dompurify@3.4.14` (HTML sanitising).
Build: Vite 8 + `vue-tsc`, output to `../internal/webui/dist`. There is **no router
package, no state-management package, no HTTP library, no CSS framework and no
PostCSS/Tailwind pipeline**.

The most important structural fact for the migration: **`src/App.vue` contains only a
`<template>`; its script is `src/app-controller.ts` loaded via
`<script lang="ts" src="./app-controller.ts">` (App.vue:1)**. That controller is a
378-line god-object that owns essentially all application state, routing, and the
modal/dialog machine.

---

## 1. Component inventory

### 1.1 Root components

#### `web/src/App.vue` (119 lines)
- **Responsibility:** application shell. Renders, in order: crash-proof splash
  (`checking`), `LoginPage` when `!user`, otherwise `.app-shell` containing
  `AppTopbar`, `AppSidebar`, `<section class="content">` (either `LibraryView` or the
  file browser), a drag-and-drop overlay, the single `.modal-backdrop` with
  `rename` / `MoveCopyDialog` / `account` / `DocumentEditor` / `ShareDialog` /
  `MediaPreview` branches, the `Reader` overlay, `AppDialog`, and the toast.
- **Props/emits:** none (root).
- **Local state:** none in the SFC — *all* state and handlers come from
  `app-controller.ts` (App.vue:1).
- **Notable markup coupling:**
  - Hidden `<input type="file" multiple>` (`fileInput`) and
    `<input type="file" multiple webkitdirectory>` (`folderInput`) at App.vue:15–16;
    avatar input at App.vue:37.
  - `.modal-backdrop` state classes computed from one `modal` ref:
    `previewing | audio-previewing | video-previewing | editing | reading | accounting`
    (App.vue:27); `.app-shell` gets `sidebar-collapsed | library-mode` (App.vue:8).
  - Drag & drop on `.app-shell`: `@dragover.prevent`, `@dragleave.self`, `@drop.prevent`.
  - The account dialog (App.vue:30–110) is inlined here, not a component: password form,
    TOTP setup/enable/disable/regenerate flows, recovery-code copy/download.
- **API endpoints:** none directly; transitively all of §3.

#### `web/src/Reader.vue` (109 lines)
- **Responsibility:** reader chrome + mounting the pagination DOM. Delegates all logic
  to `useReaderFlow(file)` and focus-trap/escape handling to `usePreviewDialog`.
- **Props:** `{ file: DriveFile }`. **Emits:** `close`.
- **Local state:** `readerEl` ref; `watch(tocOpen)` focuses `#toc-close`.
- **DOM contract (load-bearing for e2e):** `#reader-view.reader-shell[role=dialog]`,
  `#reader-back`, `#reader-title`, `#page-label.reader-progress-ring` (SVG ring with
  `pathLength=100` and `stroke-dashoffset = 100 - percent`),
  `#viewport.reader-viewport.rf-viewport`, `.rf-pager`, `#flow.rf-flow.revaro-content`,
  `#loading`, `#prev-zone` / `#center-zone` / `#next-zone`, `#toc-scrim`,
  `#toc-drawer` (with `data-preview-sheet` while open), `#toc-list`, `.toc-item`
  (indent via `--toc-indent`), `#font-popover`, `#font-smaller`, `#font-slider`,
  `#font-larger`, `.v2-lineheight`, `.reader-footer`, `#toc-button`, `#font-button`,
  `#theme-button`.
- **State read from the composable:** `FONT_MAX, FONT_MIN, LINE_HEIGHTS, adjustFont,
  clamp, closeToc, errorText, flowEl, fontOpen, isDark, jumpToc, loadingText, next,
  onFontInput, openToc, pageLabel, percentNow, prefs, previous, setLineHeight, stage,
  title, toc, tocActive, tocOpen, toggleTheme, toggleTools, toolsVisible, viewportEl,
  zoneGuard`.
- **API endpoints:** via `useReaderFlow` (§5).

#### `web/src/VideoThumb.vue` (34 lines)
- **Responsibility:** video poster thumbnail with bounded retry while the server's
  background thumbnail queue is still generating.
- **Props:** `{ file: DriveFile }`. **Emits:** `(e:'failed')`.
- **Local state:** `loaded`, `failed`, `attempt`, `timer`;
  `src = /api/files/{id}/thumbnail?v={etag}&retry={attempt}`.
- **Retry backoff:** `[800, 1600, 3200, 6400, 12800]` ms; after exhaustion emits
  `failed` and hides the `<img>`, leaving the `slot` fallback icon visible.
- **API endpoints:** `GET /api/files/{id}/thumbnail`.

### 1.2 `web/src/components/` (31 files)

| File | LOC | Responsibility | Props | Emits | Local state | API endpoints |
|---|---|---|---|---|---|---|
| `AppDialog.vue` | 44 | Generic confirm/prompt modal with icon, message, optional text input | `title, message, confirmLabel, cancelLabel, tone:'default'\|'danger', input:boolean, value, placeholder?` | `confirm, cancel, update:value` | `inputElement` ref; `onMounted` → focus | — |
| `AppSidebar.vue` | 111 | Category nav (5 sections), per-category path tree, trash entry, rail/drawer behaviour | `section, collapsed, mobileOpen, counts, trees, activeFolderId, currentFolderId, reloadToken` | `select-category, select-folder, navigate-directory, toggle-collapse, toggle-mobile, close-mobile, open-trash` | `mobile` (matchMedia `(max-width: 850px)`), `rail` computed, `expanded` (persisted `localStorage['revaro:sidebar:expanded']`), `mediaQuery`, document keydown Escape | — |
| `AppTopbar.vue` | 84 | Logo, task center, system status, trash, account; mobile `<details>` account/tools menu | `user, hasAvatar, avatarUrl, tasks` | `home, trash, account, avatarError, cancelTask, retryTask, tasksChanged` | `mobile` (matchMedia 850px), `mobileAccountMenu`, `mobileTaskCenter` (instance ref → `openCenter()`), `activeTasks`/`failedTasks` computed, document `pointerdown`/`keydown` | — |
| `AudioPlayer.vue` | 220 | Full-screen chapter audio player | `item:DriveFile` | — | `audio`, `panelOpen`, `playerEl`, `coverFailed`, `media`, `loading`, `waiting`, `playing`, `currentTime`, `nativeDuration`, `buffered`, `rate`, `error`, `seekPreview`, `seekHover`, `volume`/`muted` (localStorage `revaro-audio-volume`/`-muted`) | `GET /api/files/{id}/media/progress`, `PUT` same, `GET /api/files/{id}/audio`; `<audio src>` = `/api/files/{id}/preview`; `keepalive` PUT on unmount |
| `BookCover.vue` | 16 | Cover `<img>` with initial fallback | `item, showTitle?` | — | `broken` | `GET /api/files/{id}/thumbnail` (img src) |
| `BookShelf.vue` | 62 | Bookshelf; same-series volumes fan into one fixed-size card | `items: LibraryItem[]` | `open(item)` | `series` computed (`groupBookSeries`); constants `FIRST_WIDTH=58`, `FAN_SCALE=1.06`; `fanMetrics`/`fanStyle` inline style geometry | — |
| `DirectoryPicker.vue` | 78 | Directory flyout with breadcrumbs + child list, `Teleport to="body"`, `<Transition name="directory-flyout">` | `modelValue, excludedIds?, disabled?` | `change(folderId, folderName)` | `root/trigger/panel` refs, `expanded, currentId, current, breadcrumbs, folders, loading, error, panelStyle, requestSeq` | `GET /api/files/{id}`, `GET /api/files/{id}/children` |
| `DocumentEditor.vue` | 19 | Markdown/plain-text editor with edit/split/preview modes | `isNew, readonly, name, content, mode, busy, error, dirty, bytes, markdown, renderedMarkdown` | `close, save, update:name, update:content, update:mode` | — (`v-html="renderedMarkdown"`) | — |
| `FileBrowserHeader.vue` | 112 | Breadcrumbs, title, directory stats, grid/list switch, create/upload `<details>` menus | `breadcrumbs, current, itemCount, totalBytes, fileCount, trashMode, viewMode?` | `openFolder, newDocument, createFolder, uploadFiles, uploadFolder, leaveTrash, emptyTrash, update:viewMode` | `createMenu`, `uploadMenu`, `breadcrumbNav` refs; window `pointerdown`/`keydown`; `revealCurrentPath` scroll | — |
| `FileCard.vue` | 106 | Tile **and** list-row rendering of one `DriveFile`; type icons inline SVG | `item, selected?, selectable?, selectionMode?, trashMode?, layout?:'tile'\|'list', cover?, subtitle?` | `open(item), select(item)` | `thumbFallbackTried`/`imageBroken`/`coverBroken` reactive maps; `holdTimer`, `heldResetTimer`, `held`, `holdX/Y`; `defineExpose({openItem})` | thumbnail src `/api/files/{id}/thumbnail`, fallback `/api/files/{id}/preview` |
| `FileGrid.vue` | 13 | Grid of `FileCard` | `items, selectedIds:Set<string>, trashMode?` | `open, select` | — | — |
| `FileRows.vue` | 24 | List of `FileCard layout="list"` with subtitle/trailing duration | `items, selectedIds?, selectable?, mode?:'default'\|'audio'` | `open, select` | `subtitle()`, `duration()` helpers | — |
| `FullBleedProgress.vue` | 217 | Reusable full-bleed `<input type=range>` progress with buffer bar, chapter markers, hover tooltip; `inheritAttrs:false` + `v-bind="$attrs"` | `percent, bufferedPercent?, markers?, tooltip?, variant?:'reader'\|'audio'` | (native input events fall through) | `playedWidth`/`bufferedWidth` computed | — |
| `GalleryGrid.vue` | 26 | Image/video library grid, `all` or grouped `albums` mode | `items: LibraryItem[], mode: GalleryMode` | `open(item)` | `sorted`, `albums` computed | — |
| `LibraryView.vue` | 57 | Media library section (bookshelf/gallery/audio grid-or-list) | `type, items, loading, error, filterLabel` | `open, refresh, upload` | `galleryMode` and `mediaMode` via `usePersistentMode` (localStorage) | — |
| `LoginPage.vue` | 21 | Login form incl. conditional TOTP/recovery-code field | `login:{username,password,secondFactor,totpRequired,busy,error,notice}` | `submit, username, password, secondFactor` | — | — |
| `MediaPreview.vue` | 155 | Preview modal for image / video / audio; image gallery navigation, zoom/pan/pinch, chrome auto-hide, filmstrip | `selected, items` | `close, change, download, move, copy` | `root`, `stageEl`, `galleryItems`/`galleryIndex`/`hasGalleryNavigation` computed, `chromeVisible`, `thumbnailsOpen`, `loading`, `imageError`, `natural`, `stageSize`, `fitted`, `zoom`, `pan`, `pointers:Map<number,Point>`, `drag`, `gestureMoved`, `pinched`, `clickTimer`, `observer:ResizeObserver` | image src `/api/files/{id}/preview`, thumbnails `/api/files/{id}/thumbnail` |
| `MoveCopyDialog.vue` | 22 | Move/copy dialog wrapping `DirectoryPicker` | `mode:'move'\|'copy', targets, busy, initialId?` | `close, select(folderId)` | `targetId` | — |
| `PreviewMenu.vue` | 30 | Generic `<details>` popover menu with outside-click + Escape + focus restore | `label` | `change(open)` | `menu` ref; document `pointerdown` | — |
| `SelectionToolbar.vue` | 36 | Selection action toolbar (restore/purge or open/extract/download/share/rename/move/delete) | `selectedItems, selectedBytes, selectedFiles, singleSelected, itemCount, trashMode` | `clear, restore, purge, selectAll, open, extract, download, share, rename, move, remove` | — | — |
| `ServiceCard.vue` | 11 | Status card with icon slot + `StatusBadge` | `title, detail, badge, tone?` | — | — | — |
| `ShareDialog.vue` | 21 | Share link dialog (create/copy/revoke/regenerate) | `file, active, url, createdAt, busy, error, copied` | `close, copy, revoke, create(regenerate)` | — | — |
| `SidebarDirectoryNode.vue` | 42 | **Recursive** file-tree node; lazy-loads child directories | `id, name, depth, currentId, reloadToken, autoExpand?` | `navigate(id)` | `expanded`, `children`, `loading` | `GET /api/files/{id}/children` |
| `SidebarFileTree.vue` | 43 | Root of the file-category tree (fixed ROOT id), renders `SidebarDirectoryNode` | `currentId, reloadToken` | `navigate(id)` | `expanded=true`, `children`, `loading` | `GET /api/files/00000000-0000-0000-0000-000000000000/children` |
| `SidebarPathTree.vue` | 25 | **Recursive** library folder-path tree (from manifest-derived nodes) | `node: LibraryFolderNode, activeFolderId, depth` | `select(id\|null)` | `expanded` (`depth<1`); `--depth` CSS var | — |
| `StatusBadge.vue` | 10 | Pill badge; **exports `type StatusTone`** | `tone?:StatusTone, size?:'sm'\|'md'` | — | — | — |
| `SystemStatus.vue` | 64 | Live system-status orb + panel driven by SSE | `hideTrigger?` | — | `panel` ref, `status`, `error`, `EventSource`, `reconnect`, `retryDelay`, `stopped`; `defineExpose({openPanel, closePanel})` | SSE `GET /api/system/status/stream` |
| `TaskCenter.vue` | 56 | Background-task popover grouped active/completed/failed + archive-password prompt (`Teleport`) | `tasks, hideTrigger?` | `changed, cancel(task), retry(task)` | `center` ref, `passwordTask`, `password`, `error`, `showAllCompleted`, `active/completed/failed/visibleCompleted/progress` computed; `defineExpose({openCenter, closeCenter})` | `POST /api/tasks/{id}/input`, `DELETE /api/tasks/{id}` |
| `VideoControls.vue` | 36 | Video control bar: seek, play, time, volume, subtitles, settings, fullscreen | 17 props (`visible, playing, starting, autoplayPending, duration, timelinePosition, progress, volumeState, volumeFeedback, volumePercent, effectiveVolume, subtitles, activeSubtitle, fullscreen, rate, playbackInfo, formatTime`) | 17 events (`togglePlayback, previewSeek, commitSeek, cancelSeek, toggleMute, volumeStart, volumeEnd, changeVolume, chooseSubtitle, changeRate, download, move, copy, toggleFullscreen, hover, interact`) | — (`--video-progress` inline var) | — |
| `VideoPlayer.vue` | 189 | Video playback shell: direct-file playback, native clock sampler, subtitle overlay, fullscreen | `item:DriveFile` | `close, download(item), move(item), copy(item)` | `shell`, `video`, `directMode`, `directSource`, `starting`, `buffering`, `playing`, `error`, `currentTime`, `duration`, `controlsVisible`, `volume`, `muted`, `volumeFeedback`, `rate`, `pendingSeek`, `fullscreen`, `autoplayPending`, `player`, 6 timers, `clockFrame`, `lastAudibleVolume`, `videoResizeObserver` | `GET /api/files/{id}/video`, `GET`+`PUT /api/files/{id}/media/progress`; `<video src>` = `/api/files/{id}/preview`, poster = thumbnail |
| `VideoStatusOverlay.vue` | 13 | Back/title shade, center play, buffering/error overlay | `itemName, controlsVisible, playing, starting, error, buffering` | `close, togglePlayback, retry` | — | — |

**Notable presentational ↔ CSS couplings**

- Only 10 components carry `<style scoped>` (see §7); `VideoPlayer.vue:189` uses
  `<style src="../styles/video-player.css">` — **unscoped, therefore global**.
- `ServiceCard.vue:2` imports a *type* from an SFC (`import type { StatusTone } from
  './StatusBadge.vue'`), which Leptos must model as a plain Rust enum.
- `FileCard.vue` is the single most reused component (grid, list, library, gallery,
  albums); it renders either `.file-card` or `.file-row` depending on `layout`.
- `TaskCenter` and `SystemStatus` both expose imperative methods via `defineExpose`,
  called through template refs from `AppTopbar.vue` (`mobileTaskCenter.value?.openCenter()`).

---

## 2. Composables and supporting modules

### 2.1 `web/src/composables/` (14 files, 2,154 LOC)

| File | LOC | Responsibility | State ownership | Side effects | Interactions |
|---|---|---|---|---|---|
| `useAuthSession.ts` | 14 | Session check, login, logout | Writes `user`, `hasAvatar`, `checking`, `login.*`, `items`, `tasks`, `backgroundTasks` (all injected refs) | `GET /api/auth/me`, `POST /api/auth/login`, `POST /api/auth/logout`; starts/stops job SSE; calls `openRoute()` | Consumed by `app-controller`; reads `error.code` (`totp_required`, `invalid_second_factor`) from the API error envelope |
| `useDialogs.ts` | 11 | Promise-based confirm/prompt dialog | Owns `dialog` reactive + `resolveDialog` closure | None (pure promise plumbing) | `askDialog` → `AppDialog`; `confirmDialog` returns `boolean`, `promptDialog` returns trimmed `string\|null` |
| `useLocalView.ts` | 21 | `usePersistentMode<T>(key, fallback, allowed)` — localStorage-backed view preference | Owns returned `Ref<T>` | `localStorage.getItem` on init, `watch` → `setItem` | Used by `LibraryView` (gallery/media mode) and `app-controller` (`fileViewMode`) |
| `useBackgroundTasks.ts` | 38 | Background job list + status-transition notifications | Owns `backgroundTasks` ref; guards concurrent refreshes (`jobRefreshRunning`/`jobRefreshPending`) | `GET /api/tasks`, `POST /api/tasks/{id}/cancel`, `POST /api/tasks/{id}/retry`; notifies on completed/failed; re-opens folder after `archive_extract` | Owns `useJobEvents`; delegates upload-task cancel/retry to `useUploads` when `source_type==='upload'` |
| `useJobEvents.ts` | 20 | SSE `/api/events` with exponential reconnect + polling fallback | `source`, `fallback`, `reconnect`, `retryDelay`, `stopped` | `EventSource('/api/events')`, listens `jobs`; on error closes, polls every 30 s, reconnects with backoff `min(delay*2, 30_000)`; `onBeforeUnmount` stop | `refreshJobsFromEvent` callback |
| `useLibrary.ts` | 92 | Category counts, per-type item lists, path trees, audio durations | Owns `counts`, `itemsByType`, `activeType`, `loading`, `error`, `durations`; computed `items`, `trees`; module-level `loaded`, `seq`, `durationQueue`, `activeDurationFetches` | `GET /api/library/all` once (or forced), `GET /api/files/{id}/audio` for duration backfill (max 3 concurrent, silent failure) | `app-controller` filters `library.items` by `libraryFolderId`; `treeToken`/`reloadToken` do **not** invalidate it — `refresh()` is explicit |
| `usePreviewDialog.ts` | 46 | Focus trap + body scroll lock + Escape layering for modal viewers | `previous` element, `previousOverflow` | `onMounted`: focus root, `body.style.overflow='hidden'`, keydown listener; `onBeforeUnmount` restores | Used by `MediaPreview` and `Reader`; Escape closes the *innermost* layer (open `details` → parent handler); Tab cycles within `[data-preview-sheet]` when positioned |
| `useReaderFlow.ts` | 958 | Reader state machine: open, pagination, navigation, progress, relayout, lifecycle | Owns all reader state — see §5 | Network (`fetchBookTitle/fetchFlow/fetchProgress/saveProgress/fetchChunk`), IndexedDB L2, `document.title`, window/document listeners, WAAPI animations, touch handlers, progress debounce | Orchestrates `useReaderWindow` + `useReaderPositioning`; consumed by `Reader.vue` |
| `useReaderPositioning.ts` | 429 | Anchor ↔ live multi-column DOM geometry | Stateless w.r.t. Vue refs; pure DOM readers over injected `viewportEl`/`flowEl`/`manifest`/metrics accessors | None beyond DOM reads (`getClientRects`, `createRange`, `caretRangeFromPoint`, `elementsFromPoint`, `createTreeWalker`) | Called by `useReaderFlow` for every navigation/measure operation |
| `useReaderWindow.ts` | 171 | Incremental chunk DOM window + L1/L2 chunk loading | `chunkHTML: PageCache(24)`, `chunkFlight: InFlight`, `first`, `last` | L1 → IndexedDB L2 → `GET /api/files/{id}/book/flow/chunks/{index}`; DOM insert/remove; batched `insertBefore` with `DocumentFragment`; up to 6 concurrent loads | Drives `.rf-chunk[data-chunk]` children of `#flow`; reads `stableWindowRange`/`chunkForBlock` |
| `useUploads.ts` | 170 | Upload queue: single + multipart, resume, retry, cancel, folder-structure preservation | `tasks` (injected array), `activeUploads`, timers, `verificationRequests: WeakMap<UploadTask,AbortController>`; persistence in `localStorage['revaro.uploads.v1']` | `POST /api/uploads`, `GET/DELETE /api/uploads/{id}`, `POST .../parts`, `PUT .../parts/{n}`, `POST .../complete`, `POST /api/directories`, `GET /api/files/{id}/children`; **`XMLHttpRequest` PUT to presigned URLs for progress**; `crypto.randomUUID()` | Called by `app-controller` and by `useBackgroundTasks` for upload-source tasks |
| `useVideoProgress.ts` | 14 | Video resume position (server + localStorage) | `positionKey`, `progressLoaded`, `restoredPosition`, `serverPosition`, `userSeeked` | `GET`/`PUT /api/files/{id}/media/progress`; `localStorage['revaro-video-position:{id}']` | Used by `VideoPlayer` |
| `useVideoSubtitles.ts` | 28 | Subtitle tracks, cue parsing, overlay lines/placement, letterbox insets | `subtitleElement`, `subtitles`, `activeSubtitle`, `activeSubtitleLines`, `subtitlePlacement`, `subtitleImageBottom/Inset`, `cueTrack` | `video.textTracks` mode manipulation (`hidden`/`disabled`), `cuechange` listener, `DOMParser` HTML-entity decode, `fetch(url,{cache:'no-store'})` diagnostic on error | Used by `VideoPlayer`; consumes `containedVideoInsets`, `setExclusiveSubtitleTrack` |

### 2.2 `web/src/app-controller.ts` (378 lines) — the application store

Exported as the default `defineComponent({ setup() })` used by `App.vue`. It owns:

- **Refs/reactive state:** `user`, `hasAvatar`, `avatarVersion`, `checking`,
  `login`, `currentId` (default ROOT), `current`, `items`, `breadcrumbs`, `loading`,
  `dragActive`, `toast`, `tasks`, `trashMode`, `selected`, `selectedIds:Set<string>`,
  `moveTargets`, `transferMode`, `modal`, `readerFile`, `renameValue`, `modalBusy`,
  `share`, `editor`, `directoryStats`, `section`, `sidebarCollapsed`, `sidebarMobileOpen`,
  `libraryFolderId`, `treeToken`, `navActions`, `fileInput`/`folderInput`/`avatarInput`.
- **Computeds:** `editorDirty`, `editorBytes` (`new Blob([content]).size`),
  `editorIsMarkdown`, `renderedMarkdown` (`DOMPurify.sanitize(marked.parse(...))`),
  `selectedItems`, `selectedBytes`, `selectedFiles`, `singleSelected`, `libraryItems`
  (filtered by `libraryFolderId`), `libraryFilterLabel`, `mediaSection`, `previewItems`.
- **Async components:** `MediaPreview` and `Reader` are `defineAsyncComponent(...)`
  (app-controller.ts:30–31) — Leptos will need lazy code-split chunks or eager imports.
- **Routing:** `openRoute()`, `openDeepLink()`, `folderURL()`, `libraryURL()`,
  `openCategory()`, `navigateDirectory()`, `goHome()`, `openFolder()` (with
  monotonic `folderSeq` race guard), `handlePopState()` and the `navActions` stack
  (§4).
- **Modals:** `openModal`/`closeModal`/`closeBackdrop`; rename, move/copy, share,
  account, editor, reader, preview.
- **File operations:** `removeSelected`, `restoreSelected`, `purgeSelected`,
  `emptyTrash`, `saveRename`, `showMove`/`showMoveSelected`/`transferTo`,
  `extractArchive`, `download` (synthetic `<a download>`),
  `downloadSelected` (batch ZIP via prepare-token).
- **Selection:** `toggleSelection`, `clearSelection`, `selectAll`,
  `clearSelectionFromBlank` (click-target heuristic on
  `button,a,input,textarea,select,[role="toolbar"],.file-card,.file-row`).
- **Documents:** `newDocument`, `openEditor`, `saveDocument` (extension allow-list +
  1 MiB cap + ETag), `closeEditor` (dirty-confirm).
- **Lifecycle:** `onMounted` registers `popstate` and runs
  `checkSession().then(() => { if (user) { jobEvents.connect(); refreshJobsFromEvent() } })`;
  `onBeforeUnmount` removes the listener and calls `disposeUploads()`.

**Return statement size:** the `setup()` return (app-controller.ts:376) exports ~150
names into the template. In Leptos this becomes a single reactive store (e.g. a
`Copy`-able struct of signals) plus handler functions.

### 2.3 Pure/utility modules

| File | LOC | Exports | Notes |
|---|---|---|---|
| `library.ts` | 232 | `LibraryType`, `GalleryMode`, `ViewMode`, `ROOT_FOLDER_ID`, `LibraryFolderRef/Item/Counts/Response`, `EMPTY_COUNTS`, `LIBRARY_CATEGORIES`, `isLibraryType`, `folderKey`, `folderLabel`, `LibraryFolderNode`, `buildFolderTree`, `AlbumGroup`, `groupAlbums`, `BookSeries`, `bookSeriesInfo`, `groupBookSeries`, `isSeries`, `sortByUpdatedDesc`, `sortByName`, `formatDuration`, `libraryViewStorageKey` | Pure. Chinese numeral volume parsing (`零〇一二三四五六七八九十百千两`) and `localeCompare('zh-Hans-CN', {numeric:true})` sorting. `formatDuration` → `--:--` / `M:SS` / `H:MM:SS`. |
| `fileTypes.ts` | 21 | `isBook`, `isEpub`, `isImage`, `isVideo`, `isAudio`, `hasAudioCover`, `isMedia`, `isArchive`, `isEditable`, `thumbSRC`, `previewURL`, `readerDisplayTitle` | Pure; MIME **and** extension rules; `thumbSRC` = `/api/files/{id}/thumbnail?v={etag}`; `previewURL` = `/api/files/{id}/preview`. |
| `format.ts` | 24 | `formatSize`, `formatDate`, `formatMediaTime` | Pure; `formatDate` uses `Intl.DateTimeFormat('zh-CN',{month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'})`. |
| `taskStatus.ts` | 7 | `isActiveTaskStatus` | Pure; active = `queued\|running\|waiting_input\|retrying`. |
| `imageGeometry.ts` | 16 | `Point`, `Size`, `fitImage`, `clampImagePan`, `zoomImagePan` | Pure zoom/pan math (pixel-exact e2e assertions). |
| `videoPlayer.ts` | 102 | `VideoPlaybackMode`, `VideoCursorState`, `shouldHideVideoCursor`, `containedVideoInsets`, `setExclusiveSubtitleTrack`, `subtitleLineClass`, `initialSubtitleIndex`, `authoritativeSeekTarget`, `mediaElementTimelineTime`, `shouldSyncMediaClock`, `shouldContinueMediaClock`, `UnifiedVideoPlayer`, `createUnifiedVideoPlayer` | Mostly pure; `createUnifiedVideoPlayer` wraps an `HTMLVideoElement` (play/pause/seek/volume/setSubtitle/requestFullscreen/destroy) with a `webkitEnterFullscreen` fallback. |
| `types.ts` | 50 | `UploadTask`, `TaskStatus`, `BackgroundTask`, `ShareResponse`, `ProfileResponse`, `StorageStats`, `SystemStatusCacheClass`, `SystemStatus`, `TOTP*`, `AudioChapter`, `AudioMediaResponse`, `VideoSubtitleTrack`, `VideoMediaResponse`, `ArchiveJob` | All API payload shapes. |

### 2.4 `web/src/reader/` (6 non-test files, 559 LOC)

| File | LOC | Responsibility |
|---|---|---|
| `types.ts` | 73 | `ReadingAnchor {spine,block,path[],offset}`, `FlowSpineMeta`, `FlowChunkMeta`, `FlowTocEntry` (`nav_anchor`, `text_path`, `text_offset`, `chunk`, `source_path`, `source_fragment`), `FlowManifest`, `BookProgress`, `ReaderPrefs`. Mirrors `internal/reader/flow` JSON. |
| `api.ts` | 41 | `fetchBookTitle`, `fetchFlow` (120 s timeout), `fetchProgress`, `saveProgress`, `fetchChunk` (raw `fetch`, expects non-JSON HTML, `credentials:'same-origin'`). |
| `flow.ts` | 98 | `compareAnchor` (total order identical to Go: spine → block → path elementwise → offset), `spineForBlock`, `chunkForBlock`, `chunkPrefix`, `locateChar` (binary search, clamped), `tocActiveIndex`, `totalBlocks`, `spineOriginChunk`, `stableWindowRange`. |
| `prefs.ts` | 50 | `FONT_MIN=14`, `FONT_MAX=32`, `LINE_HEIGHTS=[1.4,1.7,2.0]`, `loadPrefs`/`savePrefs` (`localStorage['revaro-reader-prefs']`), `clamp`, `computeMargins` (MAX_COLUMN 720; mobile ≤850 px). |
| `cache.ts` | 56 | `PageCache` (LRU, key = chunk index, capacity 24) and `InFlight` (single-flight dedupe). |
| `clientCache.ts` | 241 | Persistent L2 `ClientCacheManager` over IndexedDB DB `revaro-reader-cache` v1, store `entries`; key space `m:<fileId>` / `c:<bookKey>:v<version>:<index>`; byte-budget LRU (default 64 MiB, UTF-16 ×2 accounting); `getManifest`/`putManifest` (purges chunks on book_key/version change), `getChunk`/`putChunk`, `purgeChunks`/`purgeBook`; `stats {hits,misses,puts,evictions,bytes}`; `PersistentKV` interface allows injection (gracious degradation to all-miss when IndexedDB is unavailable). |

---

## 3. API client contract and endpoint-usage table

### 3.1 The `api()` wrapper — `web/src/api.ts` (26 lines)

```ts
export async function api<T>(path: string, init: RequestInit = {}, timeoutMs = 60000): Promise<T>
```

Behaviour:

1. Copies `init.headers` into a `Headers`; if a body is present and no
   `Content-Type` is set, adds `Content-Type: application/json`.
2. Creates an `AbortController`; when `timeoutMs > 0`, `window.setTimeout` aborts it
   after that many ms. `timeoutMs === 0` disables the fixed timeout (used for chunk
   verification/`complete` calls, whose lifetime is controlled by the caller).
3. `const signal = init.signal ? AbortSignal.any([init.signal, controller.signal]) : controller.signal`
   — caller cancellation and timeout are merged.
4. `fetch(path, { ...init, headers, signal, credentials: 'same-origin' })`.
5. On `!response.ok`: best-effort `response.json()` and read
   `payload.error.message`; fall back to `` `请求失败 (${status})` ``. Throws an
   `Error` augmented with `status` and `data` (`ApiError`).
6. `204` → `undefined as T`; otherwise `response.json() as Promise<T>`.
7. `finally` clears the timeout.

**Error-envelope assumption.** The wrapper only depends on
`{ error: { message: string } }`, but the backend writes the full envelope
(`internal/server/server_helpers.go:74-75`):

```json
{"error":{"status":404,"code":"not_found","message":"..."}}
```

`useAuthSession.submitLogin` is the only call site that inspects `error.code`
(`totp_required`, `invalid_second_factor`). `useUploads.ensureUploadDirectory` and
`runUpload` inspect `error.status` (`409`, `404`). Everywhere else only `message` is
used. A Leptos port must preserve `status`, `code`, `message` and the 60 s default
timeout / `0`-means-infinite override.

**Non-`api()` request paths** (raw `fetch` or native element URLs):

- `reader/api.ts:29` — raw `fetch` for chunk HTML (not JSON), `credentials:'same-origin'`.
- `AudioPlayer.vue:166` and `VideoPlayer.vue:172` — raw `fetch` with
  `keepalive: true` on unmount, bypassing the `api()` timeout (deliberate: the page
  is unloading).
- `useVideoSubtitles.ts:26` — diagnostic `fetch(url, {cache:'no-store'})` on `<track>` error.
- `<img>`/`<audio>`/`<video>` `src` attributes (thumbnail, preview, cover, avatar).

### 3.2 Complete endpoint-usage table

46 distinct path patterns / 60 method+path rows. "Call sites" counts every invocation
location; `api()` wrappers are used unless marked *raw*.

| # | Method | Path | Request body / params | Expected response | Call sites (file:line) |
|---|---|---|---|---|---|
| 1 | GET | `/api/auth/me` | — | `ProfileResponse {username, has_avatar}` | `useAuthSession.ts:9` |
| 2 | POST | `/api/auth/login` | `{username,password,second_factor}` | `ProfileResponse`; error `code` ∈ `totp_required`,`invalid_second_factor` | `useAuthSession.ts:10` |
| 3 | POST | `/api/auth/logout` | — | (204/200, body ignored) | `useAuthSession.ts:11` |
| 4 | PATCH | `/api/auth/password` | `{current_password,password}` | (ignored) | `useAccountSettings.ts:73` |
| 5 | GET | `/api/auth/totp` | — | `TOTPStatusResponse {enabled, recovery_codes}` | `useAccountSettings.ts:82` |
| 6 | POST | `/api/auth/totp/setup` | `{current_password}` | `TOTPSetupResponse {secret,uri,qr_data_url}` | `useAccountSettings.ts:91` |
| 7 | POST | `/api/auth/totp/enable` | `{current_password,code}` | `TOTPRecoveryResponse` | `useAccountSettings.ts:102` |
| 8 | POST | `/api/auth/totp/recovery-codes` | `{current_password,code}` | `TOTPRecoveryResponse` | `useAccountSettings.ts:113` |
| 9 | DELETE | `/api/auth/totp` | `{current_password,code}` | (ignored) | `useAccountSettings.ts:124` |
| 10 | PATCH | `/api/profile/username` | `{username}` | (ignored) | `useAccountSettings.ts:33` |
| 11 | PUT | `/api/profile/avatar` | `{data_url}` (FileReader data URL) | (ignored) | `useAccountSettings.ts:56` |
| 12 | DELETE | `/api/profile/avatar` | — | (ignored) | `useAccountSettings.ts:63` |
| 13 | GET | `/api/profile/avatar?v={avatarVersion}` | — | image (element src) | `useAccountSettings.ts:17` (`avatarURL`) |
| 14 | GET | `/api/files/{id}` | — | `{file: DriveFile, breadcrumbs: DriveFile[]}` | `app-controller.ts:155,205`; `DirectoryPicker.vue:26` |
| 15 | GET | `/api/files/{id}/children` | — | `{items: DriveFile[], total_bytes?, file_count?}` | `app-controller.ts:205`; `DirectoryPicker.vue:26`; `SidebarFileTree.vue:18`; `SidebarDirectoryNode.vue:17`; `useUploads.ts:28` |
| 16 | PATCH | `/api/files/{id}` | `{name}` (rename) | (ignored; reload) | `app-controller.ts:268` |
| 17 | PATCH | `/api/files/{id}` | `{parent_id}` (move) | (ignored; reload) | `app-controller.ts:288` |
| 18 | DELETE | `/api/files/{id}` | — | (ignored) | `app-controller.ts:257` |
| 19 | POST | `/api/files/{id}/copy` | `{parent_id}` | (ignored; reload) | `app-controller.ts:287` |
| 20 | GET | `/api/files/{id}/content` | — | `{content: string, etag: string}` | `app-controller.ts:311` |
| 21 | PUT | `/api/files/{id}/content` | `{content, etag}` | `DriveFile` | `app-controller.ts:325` |
| 22 | POST | `/api/documents` | `{parent_id,name,content}` | `DriveFile` | `app-controller.ts:324` |
| 23 | GET | `/api/files/{id}/download` | — | file (synthetic `<a download>` href) | `app-controller.ts:347` |
| 24 | POST | `/api/files/batch-download/prepare` | `{ids: string[]}` | `{token: string}` | `app-controller.ts:355` |
| 25 | GET | `/api/files/batch-download/{token}` | — | ZIP (synthetic `<a download>` href) | `app-controller.ts:357` |
| 26 | POST | `/api/files/{id}/extract` | — | (ignored; refresh tasks) | `app-controller.ts:367` |
| 27 | GET | `/api/files/{id}/share` | — | `ShareResponse {active,url?,created_at?}` | `app-controller.ts:300` |
| 28 | POST | `/api/files/{id}/share` | — | `ShareResponse` | `app-controller.ts:301` |
| 29 | DELETE | `/api/files/{id}/share` | — | (ignored) | `app-controller.ts:302` |
| 30 | POST | `/api/directories` | `{parent_id,name}` | `DriveFile`; `409` handled specially | `app-controller.ts:249`; `useUploads.ts:25` |
| 31 | GET | `/api/files/{id}/thumbnail?v={etag}` (opt. `&retry=n`) | — | image | `fileTypes.ts:12`; `VideoThumb.vue:13`; `BookCover.vue`; `FileCard.vue`; `MediaPreview.vue:152` |
| 32 | GET | `/api/files/{id}/preview` | Range-aware | media/image | `fileTypes.ts:13`; `AudioPlayer.vue:39`; `VideoPlayer.vue:19`; `MediaPreview.vue:112,135` |
| 33 | GET | `/api/trash` | — | `{items,total_bytes,file_count}` | `app-controller.ts:248` |
| 34 | DELETE | `/api/trash` | — | (ignored) | `app-controller.ts:266` |
| 35 | POST | `/api/trash/{id}/restore` | — | (ignored) | `app-controller.ts:264` |
| 36 | DELETE | `/api/trash/{id}` | — | (ignored) | `app-controller.ts:265` |
| 37 | GET | `/api/library/all` | — | `{items:{book,image,video,audio: LibraryItem[]}, counts: LibraryCounts}` | `useLibrary.ts:42` |
| 38 | GET | `/api/files/{id}/audio` | — | `AudioMediaResponse {duration,chapters[],cover_url,has_cover}` | `AudioPlayer.vue:161`; `useLibrary.ts:80` |
| 39 | GET | `/api/files/{id}/video` | — | `VideoMediaResponse {subtitles: VideoSubtitleTrack[]}` | `VideoPlayer.vue:161` |
| 40 | GET | `/api/files/{id}/media/progress` | — | `{position: number}` | `AudioPlayer.vue:58`; `useVideoProgress.ts:8` |
| 41 | PUT | `/api/files/{id}/media/progress` | `{position,duration}` | (ignored) | `AudioPlayer.vue:71,166` *(166 raw + keepalive)*; `useVideoProgress.ts:10`; `VideoPlayer.vue:172` *(raw + keepalive)* |
| 42 | GET | `/api/files/{id}/book` | — | `{title?,name?,format?,cover?,toc?}` (only `name`/`title` used) | `reader/api.ts:5` |
| 43 | GET | `/api/files/{id}/book/flow` | — | `FlowManifest`; 120 s timeout; `no-cache` | `reader/api.ts:12` |
| 44 | GET | `/api/files/{id}/book/flow/chunks/{index}` | — | chunk HTML (raw fetch, immutable) | `reader/api.ts:29` |
| 45 | GET | `/api/files/{id}/book/progress` | — | `{anchor?: ReadingAnchor}` | `reader/api.ts:16` |
| 46 | PUT | `/api/files/{id}/book/progress` | `{anchor: ReadingAnchor}` | (204) | `reader/api.ts:21` |
| 47 | GET | `/api/tasks` | — | `{items: BackgroundTask[]}` | `useBackgroundTasks.ts:21` |
| 48 | POST | `/api/tasks/{id}/cancel` | — | (ignored; refresh) | `useBackgroundTasks.ts:30` |
| 49 | POST | `/api/tasks/{id}/retry` | — | (ignored; refresh) | `useBackgroundTasks.ts:34` |
| 50 | POST | `/api/tasks/{id}/input` | `{password}` | (ignored) | `TaskCenter.vue:27` |
| 51 | DELETE | `/api/tasks/{id}` | — | (ignored) | `TaskCenter.vue:28` |
| 52 | GET | `/api/events` | SSE, event name `jobs` | event stream | `useJobEvents.ts:12` |
| 53 | GET | `/api/system/status/stream` | SSE, event name `status` | `SystemStatus` JSON per event | `SystemStatus.vue:29` |
| 54 | POST | `/api/uploads` | `{parent_id,name,size,mime_type}` | `{upload_id,mode:'single'\|'multipart',url?,part_size,part_count,status?,parts?}` | `useUploads.ts:80,81` |
| 55 | GET | `/api/uploads/{id}` | — | same `CreatedUpload`; `404` → forget resume state | `useUploads.ts:80` |
| 56 | DELETE | `/api/uploads/{id}` | — | (ignored) | `useUploads.ts:165` |
| 57 | POST | `/api/uploads/{id}/complete` | `{parts: CompletedPart[]}` | (ignored); called with `timeoutMs=0` + `AbortSignal` | `useUploads.ts:87,97` |
| 58 | POST | `/api/uploads/{id}/parts` | `{part_numbers:number[]}` (≤100/batch) | `{parts:[{part_number,url}]}` | `useUploads.ts:112` |
| 59 | PUT | `/api/uploads/{id}/parts/{partNumber}` | `{etag,size}` | (ignored) | `useUploads.ts:126` |
| 60 | PUT | `{presigned url}` (external) | raw `Blob` part or whole file | `ETag` response header | `useUploads.ts:90,123` via `xhrPut` |

**Endpoints referenced by tests only** (not by app code): `/api/uploads/slow/complete`,
`/api/uploads/new/data`, `/api/uploads/resumed`, `/api/uploads/resumed/data/3`,
`/api/uploads/resumed/parts` (`src/api.test.ts`, `src/useUploads.test.ts`).

---

## 4. Client-side state and routing

### 4.1 Routing — manual History API, no router

There is **no vue-router and no hash routing**. `app-controller.ts` implements a small
URL scheme with `history.pushState` / `replaceState` / `popstate`:

| URL pattern | Meaning | Source |
|---|---|---|
| `/` | root folder of the file category | `folderURL` (app-controller.ts:159) |
| `/f/{id}` | a directory in the file category | `folderURL` |
| `/library/{type}` where type ∈ `book\|image\|video\|audio\|file` | category view | `libraryURL` (160) |
| `/library/{type}/f/{folderId}` | category view filtered to a library path | `libraryURL` |
| `/read/{fileId}` | transient deep-link that opens the reader | `openReader` (126), `openDeepLink` (150–158) |

- `openRoute()` (128–148) parses `location.pathname` at startup, applies the sections,
  suppresses history writes while doing so (`suppressHistory`), and finally calls
  `openDeepLink()`. If the requested folder id does not exist, it
  `replaceState`s back to `/` (142).
- `navActions: Ref<NavAction[]>` is an in-memory stack of
  `{kind:'folder',id} | {kind:'section',section,folderId} | {kind:'modal-close'}`.
  Every in-app navigation pushes `history.pushState({revaroNav:true},'')` and a stack
  entry; `handlePopState` pops one entry and undoes it (so the OS back button closes
  the current modal first, then walks directories/sections backwards).
- `closeModal()` is literally `window.history.back()` (246) — all dialog dismissal
  goes through the history stack. `popChain` serialises concurrent pops into a
  promise chain.
- While the reader is open, `` history.replaceState(..., '/read/'+item.id) `` is set
  (126) so a refresh can deep-link; on pop it is replaced with the folder/category URL
  (223).
- `document.title` is managed imperatively: `useReaderFlow` sets `"{file.name} · revaro"`
  on mount and restores `"revaro · 私人网盘"` on unmount (useReaderFlow.ts:894,921).

**Implication for Leptos:** reimplementing this without a router crate is a bad idea;
either keep the same hand-rolled History-API state machine or adopt `leptos_router`
with `hash`/`history` mode while preserving exactly these URL shapes (deep links are
part of the HTTP contract with the Go server, which serves the SPA at these paths).

### 4.2 Auth / session state

- Held as `user: Ref<string|null>` + `hasAvatar: Ref<boolean>` in `app-controller`,
  populated by `GET /api/auth/me` on mount. `checking` gates the splash.
- There is **no token in JS**: auth is a same-origin session cookie
  (`credentials:'same-origin'` on every request); `<img>`/`<video>` requests rely on
  the cookie as well.
- `login` is a reactive object `{username:'admin', password:'', secondFactor:'',
  totpRequired:false, busy, error, notice}`. TOTP is a second step inside the same
  form (`LoginPage.vue` renders the extra field once `totpRequired`).
- Logout clears `user`, `hasAvatar`, `items`, `tasks`, `backgroundTasks` and stops SSE.
- Password change forces a re-login (`login.notice = '密码已更新，请重新登录'`, and
  `user.value=null`) — App-level state, not a redirect.

### 4.3 Section / library category model

- `section: Ref<LibraryType>` where `LibraryType = 'book'|'image'|'video'|'audio'|'file'`
  (`library.ts:4`). `LIBRARY_CATEGORIES` (library.ts:44–50) is the ordered sidebar
  list with Chinese labels/hints.
- `libraryFolderId: Ref<string|null>` filters media-library items by *last* folder in
  each item's `folder_path` (`app-controller.ts:96–100`). Counts come from the
  library payload, paths from `buildFolderTree`.
- `SidebarPathTree` uses `folderKey()` (last `folder_path` id, or `ROOT_FOLDER_ID`)
  and `folderLabel()` (` / `-joined names) from `library.ts`.
- Grouping sugar: `groupAlbum(s)` (albums = immediate folder), `groupBookSeries`
  (volume-marker stripping), `sortByUpdatedDesc`, `sortByName`.
- The **file** category is not served by `/api/library/all`; it is the directory
  browser (`openFolder`), and its sidebar subtree is `SidebarFileTree` loading
  `/api/files/ROOT/children` with `reloadToken` invalidation.
- View preferences persisted in localStorage:
  `revaro:library:media:file` (file grid/list), `revaro:library:gallery:{image|video}`
  (`all`/`albums`), `revaro:library:media:audio` (`grid`/`list`),
  `revaro:sidebar:collapsed`, `revaro:sidebar:expanded`.

### 4.4 Dialogs, task center and upload queue

- **Dialogs:** one `modal` ref selects at most one of
  `rename|move|preview|share|account|editor|reader`. `AppDialog` is separate
  (`dialog.open`) and driven by the promise-returning `askDialog`/`confirmDialog`/
  `promptDialog` from `useDialogs` — the only await-based UI in the app. Opening a
  modal pushes a `modal-close` nav action (242–245).
- **Task center:** `backgroundTasks` from `useBackgroundTasks` is rendered by
  `TaskCenter` (popover, grouping, cancel/retry, archive-password prompt) and consumed
  by `AppTopbar` for the activity badge. Server pushes via SSE `/api/events`
  (`jobs`), with a 30 s polling fallback and exponential reconnect.
- **Upload queue:** `tasks: UploadTask[]` in `app-controller`, mutated by `useUploads`.
  It is *not* a Zustand-like store — it is a plain reactive array passed around by
  reference. Queue pump: 3 concurrent file uploads, 4 concurrent multipart parts,
  100 part-URLs per batch request, 5 retries with `min(8000, 500·2^n)+jitter` backoff.
  Resume state is persisted to `localStorage['revaro.uploads.v1']` keyed by
  `{uploadId,parentId,name,size,lastModified}`. After success a 250 ms debounce
  reloads the current folder.

---

## 5. Reader subsystem (the most complex part)

Files: `web/src/Reader.vue`, `web/src/composables/useReaderFlow.ts` (958),
`useReaderPositioning.ts` (429), `useReaderWindow.ts` (171),
`web/src/reader/{types,api,flow,prefs,cache,clientCache}.ts`.
Design rationale is also documented in `/config/revaro/docs/reader-flow.md`.

### 5.1 Core model

- The **server** produces one continuous, normalised *reading flow*: each EPUB spine
  (or TXT chapter) becomes an ordered run of **blocks**; each top-level block element
  gets a global `data-block="N"`, and the first block of every spine after the first
  gets `data-spine-start`. Blocks are grouped into **chunks** (~7k UTF-16 chars) that
  never cross a block boundary.
- The **client** loads a small contiguous window of chunks into a single continuous
  multi-column DOM (`#flow.rf-flow.revaro-content`) and lets the browser paginate with
  native **CSS columns**. Page turns are pure `transform: translateX()` — no layout is
  recomputed while turning.
- Positions are **`ReadingAnchor {spine, block, path: number[], offset: number}`** —
  `path` is the `childNodes` index chain from the block element to the text node,
  `offset` is a UTF-16 offset inside it, and `offset === -1` means "element boundary"
  (block start = empty path + `-1`). Anchors depend only on sanitised content, never
  on layout, so they survive font-size, line-height, orientation and window changes.

### 5.2 Pagination algorithm (reimplementable specification)

**Setup (`applyMetrics`, useReaderFlow.ts:110–139)**

1. Measure `viewportEl.clientWidth/Height` (fallback `window.innerWidth/Height`).
2. `computeMargins(w,h)` (prefs.ts:42–49):
   - `side = round(clamp(w * 0.055, 16, 44))`; if `w - 2*side > 720` then
     `side = round((w - 720) / 2)` (content column capped at 720 px, centred).
   - `mobile = w <= 850`; `top = mobile ? round(clamp(h*0.025,16,28)) : 60`;
     `bottom = mobile ? round(clamp(h*0.018,12,22)) : 24`.
3. Write inline styles on `#flow`:
   `width = w`, `height = h`, `box-sizing:border-box`,
   `padding = top side bottom`, `columnWidth = max(1, w - 2*side)`,
   `columnGap = 2*side`, `columnFill = 'auto'`, `transform = translateX(0)`.
   Because `columnWidth + columnGap === w`, **one column pitch equals one screen
   width**, so page turns are whole-viewport translations.
4. Set CSS custom properties on `#flow`: `--revaro-font-family` (serif stack
   `"Noto Serif SC","Songti SC",Georgia,"Times New Roman","STSong",SimSun,serif`),
   `--revaro-font-size` (`prefs.fontSize`, 14–32), `--revaro-line-height`
   (1.4/1.7/2.0) and `--revaro-col-height = h - top - bottom`.
   `flow.classList.toggle('txt', manifest.format === 'txt')` (TXT uses
   `white-space: pre-wrap`).
5. `metrics = {width,height,side,top,bottom,pitch:w,colHeight:h-top-bottom}`.
   **`pitch` is the screen width, not the column width.**

**Column count (`measureCols`, 142–149)**
`cols = max(1, round(max(pitch, flow.scrollWidth) / pitch))`.

**Window management (`useReaderWindow`)**

- The DOM window is always `[firstChunk, lastChunk]` where the **start is a stable
  pagination origin**: `stableWindowRange(manifest, block, ahead=3)`
  (flow.ts:90–97):
  - `lo = spineOriginChunk(manifest, block)` = the chunk that *contains the first
    block of the current spine* (`spineOriginChunk` → `spineForBlock` →
    `chunkForBlock`), so the layout prefix of the spine is always retained;
  - `hi = max(lo, min(chunkForBlock(block) + ahead, chunkCount-1))`.
  - Starting from an arbitrary chunk would let window insert/remove shift every later
    page boundary ("phase drift"), because CSS columns lay out from the first content
    box. Hence the invariant: **the window start never moves backwards within a spine**
    (asserted by `reader/flow.test.ts` and `reader-flow.spec.ts`).
- `ensureWindow(newFirst,newLast)` (46–119): clamps, computes existing
  `.rf-chunk[data-chunk]` children, removes out-of-window chunks (`child.remove()`),
  collects missing indices, loads them with up to **6 concurrent** readers (preserving
  LRU insertion order by re-`set`ting the cache in chunk order after load), then
  inserts them as `DocumentFragment`s of `<div class="rf-chunk" data-chunk="i">`
  (raw `innerHTML`) before the correct reference node, batching contiguous runs.
- Chunk loading order (`chunkText`, 25–44): **L1 `PageCache(24)` → IndexedDB L2
  (`bookKey` + `version` keyed) → network `fetchChunk`**, deduped by `InFlight`, and
  every network/IDB hit is written back to L1 and (bookKey present) asynchronously to
  L2.
- `prefetchSurrounding(block)` warms `center±1` and `center±2`.

**Page turn (`turn`, 286–320)**

- `turnBusy`/`syncing` guards: while an animation or window re-origin is in flight,
  requested turns are accumulated in `pendingTurns` (a signed counter) and replayed
  one at a time when the current operation completes (both `turn` and `windowSync`
  consume the queue).
- `target = currentCol + dir`. If outside `[0, cols)`, the window is first extended
  **by one chunk in the direction of travel** via
  `rebaseForCenter(first-1,last)` or `rebaseForCenter(first,last+1)`
  (273–284), which calls `ensureWindow`, `measureCols`, then `alignToAnchor()`.
  Backwards extension may cross the spine's stable boundary; the forced
  `break-before: column` on `data-spine-start` keeps current-spine page boundaries
  intact, and absolute column numbers are re-derived by re-anchoring.
- Then `goToCol(target, animate=true)`: `currentCol = clamp(target,0,cols-1)`,
  `animateX(lastX, -currentCol*pitch)` using **WAAPI**
  (`flow.animate([...], {duration:260, easing:'cubic-bezier(.22,.72,.26,1)'})`),
  temporarily setting `will-change: transform` (`setPromote(true)`) and releasing it
  when the animation finishes/cancels. After the animation the top anchor is captured
  (`captureTop()`) and `scheduleWindowSync()` is armed.

**Alignment without animation (`alignToAnchor`, 193–201)**
`col = colForAnchor(topAnchor ?? anchorAtTopOfCurrentCol())`; set
`currentCol = clamp(col,0,cols-1)`, `setX(-currentCol*pitch)`, refresh TOC/progress.

**Delayed window sync (`windowSync`, 519–549)**
200 ms after the last navigation: if not closing, not in a programmatic TOC nav
(`navDepth === 0`), not turning (`!turnBusy && !syncing`) and a `topAnchor` exists,
call `ensureWindowForBlock(topAnchor.block)`; only if the window actually changed,
`measureCols()` + `alignToAnchor()`. Then replay any `pendingTurns`.

**Relayout (`relayout`, 593–607)**
On font-size/line-height change (debounced 120 ms) or viewport resize (debounced
250 ms, also `window.visualViewport` resize): keep the current anchor, re-run
`applyMetrics()` + `measureCols()`, then `goToCol(colForAnchor(keep), false, keep)` —
the third argument preserves the anchor object so repeated relayouts cannot drift.
**No server request is made for relayout.**

**Reader open (`open`, 738–782)**

1. `stage='loading'`, `gen++` (guards stale async work).
2. `fetchBookTitle` (best-effort) → title; `fetchProgress` → saved anchor (best-effort).
3. `persistentCache.getManifest(id)` → if present, `setupView` immediately (**L2 fast
   open**, zero network for the first paint).
4. `fetchFlow(id)` (**always**, `no-cache`) → `putManifest` into L2.
5. If the cached manifest was rendered and `sameLayout(cached, flow)` (version,
   format, total_chars, book_key, and every chunk/spine/toc entry structurally equal)
   → **return without re-fetching any chunk**. Otherwise `resetFlow()` and rebuild on
   the new flow, resuming at `topAnchor ?? progress.anchor ?? first spine block`.

**`setupView` (681–707):** store manifest, `applyMetrics`, choose anchor
(saved → validated `block ∈ [0,total)`, else first spine's `block_start`),
`ensureWindowForBlock(anchor.block)`, `measureCols`, `stage='reading'`,
`currentCol = clamp(colForAnchor(anchor),…)`, `setX(-currentCol*pitch)`,
`topAnchor = anchor`, `captureTop()`, `prefetchSurrounding`, `queueProgressSave`.

### 5.3 Anchor capture and geometry (`useReaderPositioning.ts`)

- `colFromRect(rect)` — `rel = rect.left - flow.getBoundingClientRect().left`;
  `col = clamp(floor((rel - side + 1) / pitch), 0, cols-1)`.
- `rectHasBox` rejects zero-area rects (hidden/unloaded/no-size media).
- `visualStart(el)` — finds the **first actually visible content** inside an element:
  1. first text node with a non-whitespace character (`/\S/` search skips NBSP/U+3000),
     measured with a collapsed `Range` (`range.getClientRects()[0]`, fallback
     `getBoundingClientRect()`);
  2. first visible `img,svg,video,canvas,iframe,embed,object,table` descendant;
  3. whichever comes first in document order (`compareDocumentPosition` with
     `DOCUMENT_POSITION_FOLLOWING`), falling back to the element's own first rect.
  This matters because a block container's first fragment can remain in the *previous*
  column when `break-inside: avoid` pushes a full-page image down a column.
- `anchorFromNode(blockEl, node, offset)` — walks up to `blockEl` collecting
  `childIndexOf` values, yielding `{spine,block,path,offset}` with
  `offset = node.nodeType===TEXT_NODE ? max(0,offset) : -1`.
- `anchorNode(anchor)` — resolves `path` back to a live node inside `blockElFor(block)`
  (queries `[data-block="N"]`).
- `colForAnchor(anchor)` — text anchors use a **collapsed caret Range** at
  `clamp(offset,0,len)`; element anchors use `visualStartRect` first, then
  `firstRectOf`, then the element rect.
- `captureTopAnchor()` — `document.caretRangeFromPoint(contentOrigin().x, y)` where
  `contentOrigin = {x: viewport.left + side + 2, y: viewport.top + top + 2}`. If the
  caret lands in another column (e.g. full-page image), it falls back to
  `blockAtPoint(x,y)` → `anchorFromVisibleBlock`. Failing that,
  `anchorAtTopOfCurrentCol()` scans `[data-block]` elements for the first one with a
  rect in the current column.
- `anchorFromVisibleBlock(blockEl,bounds)` — filters `blockEl.getClientRects()` to the
  current column; if `visualStart` is in-column returns that; else walks text nodes and
  for each in-column line fragment binary-searches the **first caret offset inside the
  current column** (`anchorAtTextFragment`, using monotonic offset→column ordering
  rather than `caretRangeFromPoint`, which the page-turn hot zones would intercept);
  also considers in-column media; final fallback is the block start.
- `fragmentTargetEl(blockEl, fragment)` — matches element `id` or `data-frag-ids`
  against the raw and percent-decoded fragment, **by attribute value comparison**
  (never by injecting the fragment into a CSS selector — special characters would
  break it).

### 5.4 TOC navigation (`jumpToTocEntry`, useReaderFlow.ts:389–449)

Ordered strategy, each step guarded by `navDepth++` (which blocks the in-flight
`windowSync`) and preceded by cancelling any pending `syncTimer`:

1. `ensureWindowForBlock(entry.block)`, `measureCols()`.
2. **Text target** — `resolveTextLocator(entry)` walks `entry.text_path` from the block
   element, requires a real `Text` node, clamps `entry.text_offset` to the node length,
   builds a collapsed caret `Range` and lands on `colFromRect(rect)` if the rect has a
   box; commits the resolved anchor via `commitNavAnchor` (which only overrides the
   previous top anchor if `captureTop` did not already produce a new object).
3. **Media target** — element with `data-rv-anchor === entry.nav_anchor`; uses
   `anchorElRect` (first non-zero `getClientRects()` fragment, else element rect) and
   derives the anchor from the media node.
4. **Fallback** — `entry.source_fragment` resolved inside `blockElFor(block)` via
   `fragmentTargetEl` + `visualStart`.
5. **Last resort** — `jumpToBlock(block)` → `seekToAnchor({spine,block,path:[],offset:-1})`.

`seekToAnchor` (331–343) clamps the block into the book, `ensureWindowForBlock`,
`measureCols`, `goToCol(colForAnchor(fixed), false)`, `commitNavAnchor`, then
`queueProgressSave()`.

### 5.5 Progress

- `topAnchor` is the current reading position. `refreshTocUi()` derives
  `tocActive = tocActiveIndex(manifest, topAnchor.block)`,
  `percentNow = percentOfAnchor(topAnchor)`, `pageLabel = "{n}%"`.
- `percentOfAnchor` (232–248) = `(charsBeforeAnchor / total_chars) * 100`, where
  `charsBeforeAnchor` sums `chunk[i].chars` for chunks before the anchor's chunk plus
  `child.textContent.length` for preceding `[data-block]` children of the anchor chunk.
- Saving: `queueProgressSave()` debounces 1200 ms → `saveNow()` →
  `saveProgress(fileId,{anchor})`. Flush paths: `visibilitychange` hidden, `pagehide`,
  `beforeunload`, `window.blur`, and on unmount (`flushProgress`), all via `PUT`.
- The reader also supports a `0–1000` slider (`onSeekInput` → `seekFraction`) that maps
  a text fraction to `locateChar` → chunk → `blockContainingOffset` (walking the chunk
  DOM accumulating `textContent.length`) → `jumpToBlock`. Note: `Reader.vue` does
  **not** render this slider (there is no `.reader-seek`); only the handler exists.

### 5.6 Interaction

- **Hot zones** (`#prev-zone`/`#center-zone`/`#next-zone`) each `zoneGuard`-wrapped so
  a click within 500–600 ms of a swipe is swallowed.
- **Keyboard** (`onKey`, 575–582): ArrowLeft/PageUp → previous, ArrowRight/PageDown/
  Space → next, Escape closes the TOC; ignored while focus is in
  `input, select, button:not(.page-zone), summary`.
- **Touch** (`bindSwipe`, 786–885): `touchstart` tracks single-touch; `touchmove`
  starts dragging after 8 px horizontal and only if `|dx| > 1.2·|dy|`, applies
  `translateX(-currentCol*pitch + dx)` with `dx/3` rubber-banding at window edges;
  `touchend` decides a turn if it was a flick (`<300 ms && |dx| > 30`) or `|dx| >
  pitch*0.25`, else animates back; `touchcancel` restores position.
- **Chrome:** `toggleTools()` flips `tools-hidden` (the CSS deliberately overlays the
  bar over the text so pagination never changes); `toggleTheme()` flips
  `prefs.theme` and saves.
- **TOC drawer** focus is managed in `Reader.vue:19` and the Escape layering in
  `usePreviewDialog`.
- `useReaderPositioning` is also used by the TOC for `#toc-indent`
  (`min(4, depth) * 16px`).

### 5.7 Caches

| Layer | Implementation | Key | Capacity/eviction |
|---|---|---|---|
| L1 | `PageCache` (in-memory `Map`) | chunk index | 24 entries, LRU on `get`/`set` |
| in-flight | `InFlight` (`Map<number,Promise>`), cleared on `reset()` | chunk index | — |
| L2 | `ClientCacheManager` over IndexedDB `revaro-reader-cache` v1, store `entries` | manifest `m:<fileId>`; chunk `c:<bookKey>:v<version>:<index>` | 64 MiB byte budget, LRU by monotonic `tick()` timestamp, UTF-16 ×2 byte accounting; `putManifest` purges chunks when `book_key`/`version` changes or when the new manifest has no `book_key` |
| HTTP | browser cache; manifest `no-cache`, chunks immutable | — | — |

L2 statistics (`hits/misses/puts/evictions/bytes`) are exposed for tests only.

---

## 6. Media playback

### 6.1 `VideoPlayer.vue` + `VideoControls.vue` + `VideoStatusOverlay.vue` + `videoPlayer.ts`

- **Source:** always the *original file* via `/api/files/{id}/preview` (HTTP Range).
  There is **no HLS/DASH/transcode path** — `directMode` is a `ref(true)` constant and
  `createUnifiedVideoPlayer('direct', el)` is the only mode
  (`VideoPlaybackMode = 'direct'`). An undecodable file surfaces
  `HTMLMediaElement.error.code === 4` with the message
  “浏览器无法播放此原始格式，请下载后使用本地播放器打开”. An e2e test explicitly
  asserts no `hls|fmp4|transcode|audio/stream` request is ever made.
- **Autoplay:** `<video autoplay playsinline preload="metadata" :poster=thumbSRC>`;
  on mount the player calls `el.load()` then `player.play()` (browsers still treat this
  as part of the opening click). `<video crossorigin="anonymous">` is set (needed for
  `<track>`).
- **Clock authority:** `requestAnimationFrame` sampler (`runPlaybackClock`, 92–104)
  keeps `currentTime` in sync while the element exists and is not paused;
  `shouldSyncMediaClock(starting,paused)= !starting || !paused`, i.e. only paused
  teardown events are suppressed. `onTimeUpdate` additionally drives
  `syncPlaybackClock()`.
- **Seeking:** `pendingSeek` shows the preview position while dragging; `commitSeek`
  calls `seekTo` which clamps, marks `markUserSeeked()` (so resume progress can never
  overwrite an explicit zero seek — `authoritativeSeekTarget`), sets
  `el.currentTime` and `currentTime`. Keyboard: ←/→ = ∓5 s, Space/`k` play/pause,
  `m` mute, `f` fullscreen.
- **Volume:** 0–1 range, `volumeState` derived (`muted|low(<0.5)|high`), mute toggle
  restores `lastAudibleVolume`; persisted to `localStorage['revaro-video-volume']`.
- **Rate:** `[0.5,0.75,1,1.25,1.5,2]`, applied to `el.playbackRate`, persisted to
  `localStorage['revaro-video-rate']`.
- **Controls auto-hide:** `showControls(persist)` resets a 2800 ms timer while playing;
  suppressed while `controlsHovered`, while `pendingSeek !== null`, or while a
  `details[open]`/`:focus-visible` exists inside the shell. Cursor is hidden
  (`cursor-hidden`) only when `shouldHideVideoCursor({playing:true,controlsVisible:false,starting:false,buffering:false,error:''})`.
- **Fullscreen:** `player.requestFullscreen(shell)` via native `requestFullscreen({navigationUI:'hide'})`
  with a `document.fullscreenElement` toggle and a `webkitEnterFullscreen()` fallback;
  `fullscreenchange` listener keeps the button state.
- **Subtitles** (`useVideoSubtitles` + `videoPlayer.ts`):
  tracks come from `GET /api/files/{id}/video`; the selected track is rendered as a
  native `<track kind="subtitles">` whose `TextTrack.mode` is forced to `hidden` for
  the selected track and `disabled` for all others (`setExclusiveSubtitleTrack`) so
  the browser parses cues and fires `cuechange` **without** drawing its own captions;
  the app draws `.video-subtitle-overlay` itself.
  - Cue text is de-tagged (`b|i|u|ruby|rt`, `<v>`, `<c.x>`, `</v></c>`), HTML-decoded
    via `DOMParser().parseFromString(...,'text/html').body.textContent`, split on
    newlines and trimmed; line 0 gets class `''`, later lines get
    `video-subtitle-secondary-line` (deliberately **not** the global `secondary`
    button class).
  - Placement (`top|middle|bottom`) comes from the cue's numeric `line` when
    `!cue.snapToLines` (`<=25 → top`, `<75 → middle`, else `bottom`).
  - Letterbox insets are computed by `containedVideoInsets(el.clientWidth,
    el.clientHeight, el.videoWidth, el.videoHeight)` and published as CSS variables
    `--subtitle-image-bottom`/`--subtitle-image-inset`; recomputed on a
    `ResizeObserver` over the `<video>`.
  - Initial track = first `default`, else first `forced`, else 0.
- **Progress:** `useVideoProgress` persists `PUT /api/files/{id}/media/progress`
  debounced 5 s while playing, immediately on pause, and with a raw `keepalive: true`
  fetch on unmount; localStorage mirror `revaro-video-position:{id}`. Restore happens
  only once (`restoredPosition`) and only if `saved > 0 && saved < duration - 5`.
- **Cleanup:** `resetPlayback()` destroys the player, pauses the element, removes
  `src` and calls `load()`; subtitle tracks are disabled.

### 6.2 `AudioPlayer.vue`

- `<audio src=/api/files/{id}/preview autoplay playsinline preload="metadata">` with
  `el.load(); el.play()` on mount (same click-adjacent autoplay rationale).
- Buffered percentage from `el.buffered.end(el.buffered.length-1)/duration`.
- Chapters from `GET /api/files/{id}/audio` (`AudioMediaResponse`); if absent, one
  synthetic chapter covering the whole file. `currentChapterIndex` is derived from
  `currentTime`; `previousChapter` restarts the current chapter when `>3 s` in,
  otherwise jumps to the previous; `nextChapter` seeks and plays.
- `FullBleedProgress variant="audio"` renders the scrubber with chapter markers
  (`chapterMarkers = chapters.slice(1).map(start/duration*100)`) and a pointer hover
  tooltip; `previewSeek`/`commitSeek` mirror the video behaviour.
- Rate `[0.75,1,1.25,1.5,2]` applied to `el.playbackRate` (not persisted).
- Volume/mute persisted to `localStorage['revaro-audio-volume']`/`-muted`.
- Keyboard: Escape closes the chapter panel; Space toggles; ←/→ = ∓15/+30 s.
- Progress persistence mirrors the video path (5 s debounce, pause, keepalive fetch on
  unmount, localStorage mirror `revaro-audio-position:{id}`).
- Cover art from `media.cover_url` with a fallback icon and `coverFailed` guard.
- The chapter panel is a bottom sheet (`[data-preview-sheet]`), traps focus via
  `usePreviewDialog`'s Tab logic, and restores focus to the panel trigger on close.

### 6.3 `FullBleedProgress.vue`

Reusable transparent `<input type=range>` over a custom rail: buffer span, played
span, marker `<i>` elements, draggable thumb, and a clamped tooltip. `inheritAttrs:
false` + `v-bind="$attrs"` forwards `min/max/step/value/disabled/aria-*` and all input
events to the range, so callers bind `@input`/`@change`/`@pointermove`.

---

## 7. Styling

### 7.1 Structure and load order

16 stylesheets, **4,549 lines / ~119 KB**, **~1,085 top-level rule blocks / 1,301
selectors** (selector count independently re-verified), 67 custom properties,
10 `@keyframes` (2 dead), 5 `prefers-reduced-motion` blocks. Plus **10 SFC
`<style scoped>` blocks = 249 rules / ~23.2 KB ≈ 18.7 % of all rules**, and 1 global
`<style src>` block.

- `src/main.ts:3–10` imports, in order: `style.css`, `account.css`, `ui.css`,
  `styles/selection-toolbar.css`, `styles/share-dialog.css`,
  `styles/document-editor.css`, `styles/reader-flow.css`, `styles/reader-chrome.css`.
- `src/style.css:1–7` `@import`s (hoisted in place by Vite, so they precede the rest of
  `style.css`): `styles/shell.css`, `browser.css`, `uploads.css`, `dialogs.css`,
  `media.css`, `responsive.css`, `library.css`.
- `VideoPlayer.vue:189` — `<style src="../styles/video-player.css"></style>`,
  **no `scoped` attribute → global**, loaded lazily when that module is evaluated.

Effective cascade order (load-bearing; ~200 rules are last-wins overrides):
`shell → browser → uploads → dialogs → media → responsive → library → account → ui →
selection-toolbar → share-dialog → document-editor → reader-flow → reader-chrome →
video-player`.

File roles:

| File | Lines | Rules / selectors | Role |
|---|---|---|---|
| `styles/shell.css` | 926 | 162 / 170 | `:root` reset, splash, login page, `.app-shell` grid, `.topbar`, `.content`, `.file-card`, modals, toast |
| `styles/browser.css` | 315 | 64 / 65 | brand/logo, connection pulse, storage bar, mobile stats, modal-backdrop states |
| `styles/uploads.css` | 701 | 129 / 144 | file tiles + inline file-type SVG icons, `video-thumb`, create/upload menus, selection mode |
| `styles/dialogs.css` | 35 | 7 / 7 | `chapter-equalizer` audio animation |
| `styles/media.css` | 136 | 108 / 124 | preview modal, command bar, filmstrip, image stage, audio player, chapter panel |
| `styles/responsive.css` | 493 | 98 / 140 | dvh/intrinsic-size guards, canonical 37-token `:root`, breakpoint re-theming |
| `styles/library.css` | 931 | 142 / 157 | sidebar/drawer, category rows, path trees, bookshelf, albums, file rows |
| `ui.css` | 63 | 41 / 107 | shared minified primitives (dialog, account, avatar, username) + duplicate `:root` tokens |
| `account.css` | 588 | 91 / 109 | account modal, avatar settings, password, TOTP/recovery panels |
| `styles/selection-toolbar.css` | 6 | 17 / 17 | selection toolbar |
| `styles/share-dialog.css` | 5 | 24 / 24 | share dialog |
| `styles/document-editor.css` | 8 | 62 / 66 | editor chrome + markdown preview typography |
| `styles/reader-chrome.css` | 90 | 52 / 64 | reader bar/footer/zones/TOC drawer/font popover, light+dark paper palettes |
| `styles/reader-flow.css` | 175 | 24 / 37 | `.revaro-content` typography, `.rf-viewport/.rf-pager/.rf-flow/.rf-chunk`, `[data-spine-start]{break-before:column}` |
| `styles/video-player.css` | 70 | 64 / 70 | video shell, subtitle overlay, controls, range styling |
| `style.css` | 7 | 0 / 0 | pure `@import` manifest |

**Scoped vs global:** exactly **10 SFCs use `<style scoped>`** — `StatusBadge` (8 rules),
`ServiceCard` (15), `AppDialog` (14), `DirectoryPicker` (31), `TaskCenter` (63),
`FullBleedProgress` (18), `SystemStatus` (32), `FileBrowserHeader` (17),
`MoveCopyDialog` (6), `AppTopbar` (45) — together
**249 rules / ~23.2 KB ≈ 18.7 % of all rules**. The other 25 `.vue` files
(including `App.vue`, `Reader.vue`, `LibraryView.vue`, `MediaPreview.vue`,
`AudioPlayer.vue`, `FileCard.vue`, `BookShelf.vue`) have no style block at all.
`VideoPlayer.vue` is the one global `<style src>`.

### 7.2 Theming

- **No dark-mode media query, no `[data-theme]`, no `color-scheme`.** The only theme
  switch is the reader's `.dark` class, applied at `Reader.vue:28` from
  `prefs.theme`, consumed by exactly two blocks: `reader-chrome.css:22`
  (`#reader-view.reader-shell.dark`) and `reader-flow.css:154` (`#reader-view.dark`
  content palette).
- Token sets:
  - `shell.css:1–17` — `--ink:#15202b`, `--muted`, `--line`, `--blue:#2563eb`, `--navy`, font stack/background.
  - `responsive.css:355–393` — canonical 37-token design system (`--ink/-subtle`,
    `--muted/-soft`, `--line/-strong`, `--surface`, `--paper`, `--surface-subtle`,
    `--surface-hover`, `--surface-selected`, `--accent`, `--accent-hover`,
    `--accent-soft`, `--accent-faint`, `--accent-border`, `--blue`, `--blue-hover`,
    `--blue-soft`, `--danger`, `--success`, `--radius-sm/md/lg/xl`,
    `--shadow-popover`, `--shadow-dialog`, `--focus-ring`, `--control-sm/md/lg`,
    `--space-1…6`).
  - `ui.css:2–12` — a **byte-identical duplicate of those 37 tokens with exactly one
    divergence**: `--shadow-dialog: 0 24px 64px #17212b33` (ui.css) vs
    `0 24px 64px #17212b29` (`responsive.css:382`). Because `ui.css` loads later, the
    `#17212b33` value wins app-wide. `ui.css` also references classes for components
    that no longer exist (`transfer-center`, `download-center`, `merge-center`,
    `archive-center`, `download-ring`, `merge-ring`) — **~25 of its 42 class names are
    dead**.
  - `library.css:3–7` — `--drawer-width: min(300px,78vw)`, `--drawer-duration: 250ms`,
    `--drawer-ease`.
  - `reader-flow.css:8–15` — `--revaro-font-family/size/line-height/ink/paper/muted`.
  - Section-scoped: `#reader-view.reader-shell` paper palette (`reader-chrome.css:4–21`),
    `.preview-modal` media palette + dark override (`media.css:15–27,52–55`),
    `.video-player-shell` `--player-*` (`video-player.css:2`),
    `FullBleedProgress.vue` `--progress-*` (scoped), `SystemStatus.vue` `--orb/--pulse/--halo-*` (scoped).
- **JS-injected variables** (must become Leptos reactive inline styles):
  `useReaderFlow.ts:134–137` (`--revaro-font-family/size/line-height/col-height`),
  `useVideoSubtitles.ts:17` (`--subtitle-image-bottom/-inset`), plus Vue `:style`
  object bindings for `--depth` (`SidebarPathTree.vue:16`, `SidebarDirectoryNode.vue:33`),
  `--toc-indent` (`Reader.vue:74`), `--video-progress` (`VideoControls.vue:12`),
  `--progress-*` (`FullBleedProgress.vue:37–46`), and geometry styles in `BookShelf`,
  `MediaPreview`, `DirectoryPicker`.

### 7.3 Breakpoints

Primary mobile breakpoint **850 px**, defined in 9 files (`shell.css`, `browser.css`,
`uploads.css`, `responsive.css`, `library.css`, `account.css`, `ui.css`,
`selection-toolbar.css`, plus `AppTopbar.vue` and `FileBrowserHeader.vue`). Other
`max-width` values: 1100, 1024, 760, 720, 640, 600, 520, 460, 430, 350; `min-width`
851 (SystemStatus panel docked top-right). Feature queries: `(hover: none)` ×2,
`(hover: none), (pointer: coarse)`, and `(prefers-reduced-motion: reduce)` ×5.

| Breakpoint | Files that change at it |
|---|---|
| ≤1100 | `shell.css`, `library.css`, `document-editor.css` |
| ≤1024 | `uploads.css` (grid minmax) |
| ≤850 | `shell.css`, `browser.css`, `uploads.css`, `responsive.css`, `library.css`, `account.css`, `ui.css`, `selection-toolbar.css`, `share-dialog.css`, `AppTopbar.vue`, `FileBrowserHeader.vue` |
| ≤760 | `media.css`, `video-player.css` |
| ≤720 | `account.css`, `document-editor.css`, `share-dialog.css` |
| ≤640 | `uploads.css` (grid becomes 2 columns) |
| ≤600 | `DirectoryPicker.vue`, `MoveCopyDialog.vue` |
| ≤520 | `shell.css`, `browser.css`, `uploads.css`, `library.css`, `document-editor.css` |
| ≤460 | `account.css` |
| ≤430 | `responsive.css`, `ui.css`, `AppTopbar.vue`, `ServiceCard.vue`, `SystemStatus.vue` |
| ≤350 | `AppTopbar.vue` |

Key transitions: at ≤850 px `.app-sidebar` becomes a fixed off-canvas drawer
(`position:fixed; z-index:60; transform:translateX(-100%); visibility:hidden`, animated
with `--drawer-*`) with `.sidebar-handle` (z 70) and `.sidebar-backdrop` (z 55), and
`.app-shell` collapses to one column; the login visual column is dropped; `.audio-panel`
becomes a bottom sheet at ≤760; the account modal collapses at ≤720/460; the editor goes
full-bleed at ≤720; `dvh` fallbacks (`height: 100vh; height: 100dvh`) are paired.

**Cascade trap:** the same selector is often re-themed in several files by design —
e.g. `.app-shell` `grid-template-columns` is `230px 1fr` (`shell.css:251`), `1fr`
(`uploads.css:57`), and `236px minmax(0,1fr)` (`library.css:11`), where **library.css
wins at desktop (236 px)**. Flattening or reordering the sheets during the port will
change layouts; preserve declaration order.

### 7.4 How much CSS can be reused verbatim

**~98.5 % of the 1,334 rule blocks can be copied verbatim**, requiring no declaration
changes. Justification and caveats:

- Zero preprocessor syntax, zero CSS nesting, zero `@layer`/`@container`/`:has()`/
  `@scope`/`@supports`/`@property`, no CSS Modules, no PostCSS/Tailwind. Everything is
  standard CSS applied to class names, ids, element selectors and `[open]`/`data-*`
  attributes.
- Vue coupling is compiler-level only: the generated `[data-v-*]` attribute for the 249
  scoped rules, plus 3 explicit `:deep()` rules and 1 `<Transition name="directory-flyout">`
  (4 selectors). Those are the only declarations needing edits
  (`AppTopbar.vue:80` `.top-actions :deep(.system-status>summary)`;
  `ServiceCard.vue:10` `.service-icon :deep(svg)` ×2; `DirectoryPicker.vue:77`
  `.directory-flyout-*-active/from/to`).
- Global CSS (~1,070 of 1,085 rules) ports as-is provided the markup keeps the same
  class/`id`/`data-*` contract and the 15-file cascade order is preserved. `#app` is
  referenced in `responsive.css:2–8` — keep that mount id or amend the rule.
- For the 10 scoped blocks, the work is a **scoping decision**, not a rewrite:
  either prefix their class names and ship one global stylesheet, or use a
  CSS-modules crate. Do **not** paste them unprefixed: `AppDialog.vue:42–43` contains
  bare `input`, `footer`, `button` element selectors, and `TaskCenter` uses generic
  `.actions`/`.task-list`, `.tone-*`, `.size-*` that collide with globals.
- Recommended deletions rather than ports: the dead `body.reader-open` rules
  (`reader-chrome.css:2–3`), `@keyframes audio-pulse` and `zoom-notice-in`, the ~25 dead
  `ui.css` classes, and one of the two duplicate `:root` token blocks (keep
  `--shadow-dialog: 0 24px 64px #17212b33`).
- Features that deserve an explicit review checklist:
  - **CSS multi-column fragmentation** (`break-before: column` at
    `reader-flow.css:149–151`, `break-inside: avoid` at `reader-flow.css:41,65,88–92`,
    `orphans`/`widows` at `reader-flow.css:21–22`, plus `column-fill: auto` and
    `column-width`/`column-gap` injected from JS) — this is the reader's pagination
    engine, not decoration, and it is browser-version sensitive.
  - `color-mix(in srgb,…)` (2 uses, `SystemStatus.vue` halo + `@keyframes breathe`) —
    the one declaration that may need a fallback on the project's HarmonyOS/Android
    WebView baseline.
  - 62 `env(safe-area-inset-*)` uses, 79 `max()`, 32 `min()`, 9 `clamp()`, 22 `dvh` —
    requires `viewport-fit=cover` in the new `index.html`; paired
    `height:100vh; height:100dvh` fallbacks (`responsive.css:13–14,19–20,32–33,43–44`,
    `shell.css:32–33`, `document-editor.css:8`, `account.css:586–588`) must be preserved
    in that order.
  - 16 `backdrop-filter` uses with no `@supports` fallback; `isolation: isolate` at
    `library.css:532` (`.series-stage`) and `TaskCenter.vue:55` (asserted by tests);
    `clip-path` (`uploads.css:490` polygon corner-select, `media.css:50` `inset(50%)`
    sr-only); `aspect-ratio` ×8; `accent-color` ×4; `appearance:none` +
    `::-webkit-slider-runnable-track/-thumb` / `::-moz-range-track/-thumb` range
    styling (`video-player.css:25–29`, `FullBleedProgress.vue:187–212`);
    `:fullscreen` (`video-player.css:5`); `mask-image` in `FileBrowserHeader.vue:111`
    with **no `-webkit-mask-image` fallback**; 13 distinct `-webkit-` prefixed
    properties (incl. the legacy line-clamp triple at `library.css:488–494`).
  - 10 `@keyframes`: `spin` (`shell.css:47`, reused by `media.css:103`),
    `connection-breathe`/`connection-pulse` (`browser.css:63,76`), `storage-breathe`
    (`browser.css:162`), `sidebar-paths-in` (`library.css:201`), `chapter-wave`
    (`dialogs.css:31`), `video-spin` (`video-player.css:55`), `breathe`
    (`SystemStatus.vue`), and 2 dead: `audio-pulse` (`dialogs.css:2`) and
    `zoom-notice-in` (`media.css:2`).
  - **Formatting is inconsistent and occasionally load-bearing**:
    `document-editor.css` packs 62 rules into 8 lines, `selection-toolbar.css` 17 into 6,
    `share-dialog.css` 24 into 5; `media.css` and `ui.css` are one-rule-per-line. The
    `style.test.ts` assertions (`toContain` on raw text) depend on this exact
    minification, so reformatting these files will break that test before any Leptos
    code exists.

---

## 8. Browser API usage

Legend: **W** = directly available in `web-sys`/`wasm-bindgen` (usually a thin
wrapper); **S** = needs a small hand-written JS shim or careful binding; **G** = needs
a third-party crate or a substantial shim.

| API / capability | Where used | Need | Notes |
|---|---|---|---|
| `fetch` + `RequestInit`/`Headers`/`Response` | `api.ts:14`, `reader/api.ts:29`, `AudioPlayer.vue:166`, `VideoPlayer.vue:172`, `useVideoSubtitles.ts:26` | W | `web-sys` `fetch` + `wasm-bindgen-futures`; JSON body/`response.json()`, 204 handling, `credentials:'same-origin'` |
| `AbortController` / `AbortSignal.any` | `api.ts:10–12`, `useUploads.ts:96` | W/S | `AbortSignal.any` is newer — verify `web-sys` coverage; trivial JS shim otherwise |
| `XMLHttpRequest` (`upload.onprogress`, `getResponseHeader('ETag')`) | `useUploads.ts:147–161` (`xhrPut`) | S | **Mandatory for upload progress**; `fetch` has no upload-progress events. Keep a tiny JS shim (`window.__revaroXhrPut`) or use `web_sys::XmlHttpRequest` (available, but progress callback plumbing is verbose) |
| `EventSource` (SSE) | `useJobEvents.ts:12`, `SystemStatus.vue:29` | S/G | Not in `web-sys`; use a small JS shim or a community crate. Must preserve named events (`jobs`, `status`), `readyState`, exponential reconnect, and the 30 s polling fallback |
| `localStorage` | `app-controller.ts:33–37,180`; `useLocalView.ts`; `useUploads.ts:15–17`; `AppSidebar.vue:52,58`; `reader/prefs.ts`; `AudioPlayer.vue:28–30,54,69,150,152`; `VideoPlayer.vue:30,76,132,158`; `useVideoProgress.ts:7,10` | W | Keys to preserve: `revaro:sidebar:collapsed`, `revaro:sidebar:expanded`, `revaro:library:*`, `revaro.uploads.v1`, `revaro-reader-prefs`, `revaro-audio-volume`, `revaro-audio-muted`, `revaro-audio-position:{id}`, `revaro-video-volume`, `revaro-video-rate`, `revaro-video-position:{id}` |
| IndexedDB | `reader/clientCache.ts:59–107` (DB `revaro-reader-cache`, store `entries`) | S/G | Keep the DB name/version/store and key strings for e2e parity; `web-sys` has IndexedDB bindings but a small JS shim (`idbGet/Set/GetAll/Delete`) is likely less code |
| `history.pushState/replaceState/back`, `popstate`, `location.pathname` | `app-controller.ts:126–246` | W | URL shapes and the `{revaroNav:true}` state object are contractual |
| `navigator.clipboard.writeText` | `app-controller.ts:303`, `useAccountSettings.ts:130` | W | Requires secure context; fallback message on failure |
| `navigator.vibrate` | `FileCard.vue:52` (18 ms on long-press select) | S | Not in `web-sys`; trivial shim or feature-detect skip |
| `crypto.randomUUID` | `useUploads.ts:21` | W | Task ids |
| `File` / `FileList` / `Blob` / `File.slice` | `types.ts:3`, `App.vue:15–16,37`, `useUploads.ts:21,122`, `useAccountSettings.ts:51–55` | W | `File.slice(start,end)` for multipart parts; `file.type`, `file.size`, `file.lastModified`, `file.webkitRelativePath` |
| `<input type="file" multiple webkitdirectory>` | `App.vue:16`, `useUploads.acceptFolder` (42) | W | Non-standard `webkitdirectory` attribute — keep in the DOM or set via shim |
| `FileReader.readAsDataURL` | `useAccountSettings.ts:55` | W | Avatar upload |
| `URL.createObjectURL` / `revokeObjectURL` | `useAccountSettings.ts:135–137` | W | Recovery-codes `.txt` download |
| Synthetic `<a download>` + `click()` | `app-controller.ts:347,357` | W | Single and batch-ZIP downloads. The e2e suite asserts **no iframes and no JS blob buffering** |
| `document.createElement` / `appendChild` / `insertBefore` / `DocumentFragment` / `replaceChildren` / `remove()` | `useReaderWindow.ts:97–114,152`, `useAccountSettings.ts:136` | W | Reader DOM window insertion is performance-sensitive |
| `innerHTML` on chunk elements | `useReaderWindow.ts:103` | W/S | Server HTML is sanitised server-side; Leptos must insert raw HTML here (`inner_html` or `set_inner_html`) rather than parse into virtual DOM |
| `element.getClientRects()` / `getBoundingClientRect()` | `useReaderPositioning.ts` (many), `useUploads`-unrelated, `DirectoryPicker.vue:35`, `AudioPlayer.vue:110`, `MediaPreview.vue:40` | W | Pixel-exact layout measurement — the core of reader positioning |
| `Range` (`createRange`, `setStart`, `collapse`, `getClientRects`, `getBoundingClientRect`) | `useReaderPositioning.ts:103–106,156–159,266–270` | W | Caret rects for anchors |
| `document.caretRangeFromPoint` (with `document.caretPositionFromPoint` absent) | `useReaderPositioning.ts:362` | S | Non-standard; behind a feature check; needs a JS shim wrapper |
| `document.elementsFromPoint` | `useReaderPositioning.ts:208` | W | Block hit-testing under the top-left content origin |
| `document.createTreeWalker` + `NodeFilter.SHOW_TEXT` | `useReaderPositioning.ts:99,319` | W | Text-node traversal |
| `Node.compareDocumentPosition` / `DOCUMENT_POSITION_FOLLOWING`, `Node.TEXT_NODE` | `useReaderPositioning.ts:58,122,154,162` | W | |
| `DOMParser` (HTML entity decode) | `useVideoSubtitles.ts:19` | W | Subtitle cue text |
| `DOMTokenList` / `classList.toggle` | `useReaderFlow.ts:133` (`.txt`) | W | |
| Inline style / CSS variable writes (`style.setProperty`) | `useReaderFlow.ts:126–138,154,157,167` | W | Pagination geometry |
| Web Animations API (`element.animate`, `Animation.onfinish/oncancel/cancel`) | `useReaderFlow.ts:177–188` | W | Page-turn animation (260 ms) |
| `requestAnimationFrame` / `cancelAnimationFrame` | `VideoPlayer.vue:101,103` | W | Native media clock sampler |
| `ResizeObserver` | `MediaPreview.vue:114` (image stage), `VideoPlayer.vue:157` (subtitle bounds) | W | Must disconnect on unmount |
| `window.matchMedia` + `MediaQueryList` change events | `AppSidebar.vue:33–45`, `AppTopbar.vue:42`, `FileCard.vue:40` | W | `(max-width: 850px)`, `(hover: none), (pointer: coarse)` |
| `window.visualViewport.resize` | `useReaderFlow.ts:897,916` | W | Reader relayout on mobile URL-bar changes |
| `document.fullscreenElement`, `element.requestFullscreen({navigationUI:'hide'})`, `document.exitFullscreen`, `fullscreenchange` | `videoPlayer.ts:96–98`, `VideoPlayer.vue:137,156`; guarded in `usePreviewDialog.ts:12` | W | Plus a `webkitEnterFullscreen()` fallback for iOS |
| Media element APIs — `HTMLMediaElement` (`play/pause/currentTime/duration/volume/muted/playbackRate/buffered/readyState/paused/error.code`), `HTMLVideoElement.videoWidth/videoHeight`, `load()`, `removeAttribute('src')` | `AudioPlayer.vue`, `VideoPlayer.vue`, `videoPlayer.ts` | W | `play()` returns a Promise in modern browsers (autoplay rejection handled) |
| `HTMLMediaElement.textTracks` / `TextTrack.mode` / `cuechange` / `VTTCue.text/line/snapToLines` / `HTMLTrackElement.track/readyState` | `useVideoSubtitles.ts:19–26`, `videoPlayer.ts:28–33` | W | Track mode `hidden` vs `disabled` is behavioural |
| `<track kind="subtitles" src srclang label>` | `VideoPlayer.vue:180` | W | |
| Pointer Events + `setPointerCapture` | `MediaPreview.vue:65–94` (drag/pinch/`pointercancel`), `FileCard.vue:42–56` (long-press) | W | Multi-pointer `Map<pointerId,Point>` pinch math |
| Touch Events (`touchstart/move/end/cancel`, `touches`, `changedTouches`) | `useReaderFlow.ts:812–884` (`bindSwipe`, all `{passive:true}`) | W | Reader swipe paging with rubber-banding |
| Wheel event | `MediaPreview.vue:100` (zoom), `DirectoryPicker` scroll reposition (capture) | W | `deltaMode` handling |
| `:fullscreen` / `[open]` / `:focus-visible` / `:hover` CSS state | `video-player.css`, many | — | Markup/attribute driven |
| `getComputedStyle` | `usePreviewDialog.ts:23,25` | W | Focus trap filter + `[data-preview-sheet]` position detection |
| `Element.closest`, `querySelector(All)`, `hasAttribute`, `dataset` | pervasive | W | |
| `element.scrollIntoView({block,inline})`, `scrollTo({behavior})` | `AudioPlayer.vue:76`, `MediaPreview.vue:108`, `FileBrowserHeader.vue:33` | W | |
| `Intl.DateTimeFormat('zh-CN', …)` | `format.ts:12` | W/G | Needs `js-sys` Intl bindings or a JS shim |
| `String.localeCompare(…, 'zh-Hans-CN', {numeric:true})` | `library.ts:96,123,199,213,217` | S | ICU collation; use `js-sys`/shim rather than reimplementing |
| `window.setTimeout/clearTimeout/setInterval/clearInterval` | pervasive | W | Many debounce timers |
| `document.title` | `useReaderFlow.ts:894,921` | W | |
| `document.body.style.overflow` (scroll lock) | `usePreviewDialog.ts:37,43` | W | |
| `window.addEventListener('pagehide'/'beforeunload'/'blur')` | `useReaderFlow.ts:899–901` | W | Progress flush |
| `navigator` clipboard/`vibrate`/UA | above | S/W | |
| `window.showDirectoryPicker` (+ `FileSystemHandle` types) | declared in `fileSystemAccess.d.ts` but **never called** | — | Dead declaration; do not port the API surface |
| `IntersectionObserver`, `MutationObserver`, `WebSocket`, `Worker`, `serviceWorker`, `Notification`, `BroadcastChannel`, `requestIdleCallback`, `structuredClone`, canvas 2D/WebGL (`getContext`), `requestVideoFrameCallback`, `disablePictureInPicture`, `captureStream` | **not used anywhere** | — | Confirmed by grep |

---

## 9. Existing tests — parity checklist

Totals: 13 Vitest files / 669 lines; 7 Playwright specs + `helpers.ts` / 1,850 lines;
3 Playwright configs.

### 9.1 Vitest (run with `vitest run src --pool=vmThreads --maxWorkers=1`)

| File | Lines | Asserts | Portability |
|---|---|---|---|
| `src/api.test.ts` | 20 | `api(path,{signal},0)` does not auto-abort after 180 s (fake timers) while an explicit `controller.abort()` still rejects. Pins the third `timeoutMs` argument semantics and abort propagation. | Behavioural port (harness changes) |
| `src/app-controller.test.ts` | 33 | (a) `Object.keys(App.components).sort()` equals exactly 14 names; (b) `App.setup.toString()` contains `'/api/files/batch-download/prepare'` and `/api/files/batch-download/${encodeURIComponent(prepared.token)}`, and does **not** contain `response.blob`, `ArrayBuffer` or `createElement('iframe')`. | **Rewrite** (source-text + Vue registry) |
| `src/fileTypes.test.ts` | 33 | audio/image/video routing and `hasAudioCover`; `readerDisplayTitle` table incl. `长标题.epub`, `notes.TXT`, `archive.epub.pdf.md→archive`, `book.markdown`, `comic.cbz`, unchanged `report.docx` and `.epub`. | Near-verbatim |
| `src/format.test.ts` | 16 | `formatMediaTime`: `0→'0:00'`, `65.9→'1:05'`, `3661→'1:01:01'`, invalid (`-1`,`NaN`,`Infinity`)→`'0:00'`. | Near-verbatim |
| `src/imageGeometry.test.ts` | 23 | `fitImage` no-upscale/portrait/landscape cases; pointer-anchored zoom invariance; pinch anchor movement; letterbox centering + edge clamping. Exact numbers. | Near-verbatim |
| `src/library.test.ts` | 70 | Chinese/Latin volume markers (`第03卷`,`(2)`,`Vol.4`,`5`,`第三卷`), series grouping/order, folder-tree ancestor counting + `Photos / Trips` paths + root label `我的文件`, album grouping by immediate folder, `formatDuration` (`--:--`,`0:59`,`1:02:03`). | Near-verbatim |
| `src/style.test.ts` | 17 | Concatenated CSS contains `z-index: 20`; `TaskCenter.vue` contains the literal `.task-panel{z-index:25;isolation:isolate;background:#fff}`, `min-height:38px`, `min-height:40px`, `清除完成`, `ChevronDown`. | **Rewrite** as computed-style/DOM assertions |
| `src/taskStatus.test.ts` | 10 | Active statuses are exactly `queued|running|waiting_input|retrying`. | Near-verbatim |
| `src/useUploads.test.ts` | 80 | Uploads survive malformed saved state + throwing `localStorage.setItem`; resumed multipart reports finite progress `>90`, samples progress once, calls `/complete` with timeout `0` + `AbortSignal`; a `503` fails the task but retains `uploadId` and makes exactly one API call. Fakes `XMLHttpRequest` and `localStorage`. | **Rewrite** (Vue composable + XHR/storage doubles) |
| `src/videoPlayer.test.ts` | 62 | Cursor hiding truth table; exclusive subtitle tracks (`hidden`/`disabled`); `video-subtitle-secondary-line` (never `secondary`); `containedVideoInsets` exact values; default→forced→0 track selection; `authoritativeSeekTarget(0,86,true)===0`; native clock authority predicates. | Near-verbatim |
| `src/reader/cache.test.ts` | 75 | `PageCache` LRU (capacity 3, `get`/re-`set` recency), `InFlight` dedupe, `computeMargins` desktop (`1400×900` → top 60/bottom 24/side 340) and mobile (`390×600`, `390×1000`, `844×390`) pixel values, `clamp` at `FONT_MIN`/`FONT_MAX`. | Near-verbatim |
| `src/reader/clientCache.test.ts` | 111 | manifest/chunk round-trip; chunk keys bound to `book_key` + version; `putManifest` purges on version/content change; byte-budget LRU with UTF-16 accounting and `stats.evictions`; `purgeBook`; corrupt manifest → miss; `stats.hits/misses/puts`. | Near-verbatim (inject an in-memory KV) |
| `src/reader/flow.test.ts` | 119 | `compareAnchor` total order **identical to the Go backend**; `spineForBlock`/`chunkForBlock(-1)`/`totalBlocks`; `chunkPrefix`/`locateChar` boundaries + clamping; `tocActiveIndex`; `spineOriginChunk` incl. a chunk spanning a spine; `stableWindowRange` prefix preservation, `ahead` prefetch, end clamping, never-backwards window start. | Near-verbatim — **highest-value port** |

### 9.2 Playwright configs and specs

| Config | baseURL | webServer | testMatch | Notes |
|---|---|---|---|---|
| `playwright.config.ts` | `E2E_BASE_URL` or `http://127.0.0.1:18080` | none — **expects a real backend + storage** | all specs | chromium/Desktop Chrome, workers 1, timeout 45 s, `PLAYWRIGHT_EXECUTABLE_PATH` override |
| `playwright.ui.config.ts` | `http://127.0.0.1:18779` | `npm run dev -- --host 127.0.0.1 --port 18779 --strictPort` | `media-ui|reader-flow|library-ui` | 60 s; all API route-mocked |
| `playwright.v2.config.ts` | `http://localhost:18777` | `npm run dev -- --port 18777 --strictPort`, `reuseExistingServer:false` | `reader-flow.*\.spec\.ts` only | 90 s; reader-focused |

| Spec | Lines | Flows covered |
|---|---|---|
| `e2e/helpers.ts` | 15 | `login()` (labels `用户名`/`密码`, button `进入我的网盘`, heading `我的文件`), `selectCard()` (`.file-card` + `选择项目` button) |
| `e2e/auth-status.spec.ts` | 35 | SSE-driven status orb/panel: `.system-status.ok summary`, `[aria-label="打开系统状态"]`, `.status-panel`, 3 `.status-grid` cells stacked vertically (same x ±1 px), no `任务`/`清理队列`/`备份`, no `刷新` button, 1 `<i>` in summary / 0 in panel, Escape + outside-click close; 401 for unauthenticated `/api/system/status` and `/stream` |
| `e2e/files.spec.ts` | 74 | Full CRUD on real storage (create folder → real upload → move → delete → trash → restore); multi-select ZIP download with `frame-src 'none'` CSP, zero iframes, filename `revaro-download.zip`; desktop task-center open/close with `overflow:hidden` and no `新建下载` |
| `e2e/library-ui.spec.ts` | 278 | Five-category switching and each view; bookshelf series card (2 cards, main + 2 fans, series size == single size, cover fan covers the square stage); gallery grid (3 cards) and albums (2 albums), path filter shows `2 个项目`; audio list rows + `2:05`; file list 4 rows; sidebar collapse classes; path accordion exclusivity; 12-volume series still one square card with 1 main + 11 fans; mobile drawer (handle 32×52 at vertical centre 422, active row 48–52 px at full drawer width, no expand/count/paths on mobile, `.content` unmoved, backdrop/Esc/category close) |
| `e2e/media-ui.spec.ts` | 178 | Audio at 1440/390/320 (readyState 4, chapter jump updates `第二章 · 在林间停留`, ±15/30 s seeking, no overflow, Escape layering, focus return); undecodable file → `error.code===4` + `浏览器无法播放` + no transcode/HLS request; image actual-size/fit/filmstrip/pan-clamp/layered Escape/command-bar hide+Tab; video at 1440/390/320 (autoplay, time + subtitle visible, rate 1.5 applies, settings hold controls open, idle auto-hide, subtitle y stable, no overflow); touch (tap toggles controls only, pinch zoom, cancelled gesture must not page-turn, swipe advances) |
| `e2e/mobile.spec.ts` | 33 | 390×844 mobile: status ball + panel, `打开任务与工具菜单` must **not** exist, `打开账户与工具菜单` menu contents (no `系统状态`), task panel opaque white + hit-testable with `.file-grid` still visible, account modal opens |
| `e2e/reader-flow.spec.ts` | 985 | 17 tests: bar layout/title/progress/TOC-entry-point; windowed prefetch (`[0,1,2,3]` exactly once, heat path zero network, ≤5 `.rf-chunk`); font/line-height relayout with zero requests and position preserved; TOC seek writes anchor + reopen resumes (chunk 0 never requested); backward paging past start; text-locator same-block multi-column landing, encoded-fragment fallback, lost-locator → block start; `@benchmark` long-single-spine deep TOC (full prefix retained, no pull-back); parent TOC no-rebound after delayed window sync; locator in an unloaded chunk; deterministic random TOC seeks; spine-switch page-boundary stability; continuous paging probe distance −1/page; orientation relayout; full-page-image NavAnchor; fragment-less entry → book start; persistent L2 (reopen zero chunk requests, version bump re-fetches); reader visuals at 390×844 (icon centring, underlapping content, immersive/theme toggles leave `transform` byte-identical, focus management) |
| `e2e/reader-real-epub.spec.ts` | 252 | Real EPUBs from `REVARO_EPUB_FIXTURE_DIR`/`/config/donwload`/`/config/Downloads` (skips when absent; expects 3): after each forward turn and the delayed window sync, `syncedTop >= animationTop - 3`. **The inline comment states this assertion intentionally fails on the current implementation** (stale `topAnchor` re-alignment pulls the page back). Also drives `caretRangeFromPoint` top-anchor capture, `#flow` children as the chunk window, and transform/M41 column math |

### 9.3 Consolidated parity checklist

Grouped behaviours the Leptos rewrite must reproduce (source test in brackets).

**Auth / shell / status**
1. Login labels `用户名`/`密码`, button `进入我的网盘`, then heading `我的文件` [helpers, reader-real-epub, library-ui].
2. Unauthenticated `/api/system/status` and `/api/system/status/stream` → 401 [auth-status].
3. `.system-status.ok summary` orb; `[aria-label="打开系统状态"]` opens `.status-panel` [auth-status, mobile].
4. Panel shows `数据库` + `网盘存储使用量`; exactly 3 `.status-grid > *` cells, vertically stacked (same x ±1 px, second below first); never `任务`/`清理队列`/`备份`; no `刷新`; summary has 1 `<i>`, panel has 0 [auth-status].
5. Panel closes on Escape and outside click [auth-status, mobile].
6. Desktop task center opens from `title="任务中心"` as `.task-panel`, CSS `overflow:hidden`, no `新建下载`, closes on Escape [files].
7. Task-center flyout opaque (`background:#fff`, `z-index:25` + `isolation:isolate` over base `z-index:20`), touch targets ≥38/40 px, label `清除完成` [style.test].
8. Mobile: no `打开任务与工具菜单`; `打开账户与工具菜单` menu contains `任务中心`/`回收站`/`账户设置` but not `系统状态`; task panel opaque `rgb(255,255,255)` and hit-testable while `.file-grid` stays visible; account modal opens [mobile].
9. `isActiveTaskStatus` = `queued|running|waiting_input|retrying`, shared by task center and topbar [taskStatus.test].

**File browser and operations**
10. Root heading `我的文件`; each entry a `.file-card`; selection via the `选择项目` button [helpers, files, library-ui].
11. Create folder: `新建文件夹` → placeholder `文件夹名称` → `创建` → card appears [files].
12. Real upload through `input[type=file]` → visible `.file-card` within 20 s [files].
13. Move: toolbar role `toolbar` name `所选项目操作` → `移动` → `.directory-trigger` → region `选择目标目录` → target → `.move-copy-dialog` `移动` [files].
14. Delete to trash (`删除` → `移入回收站`) and restore (`回到我的文件` → `打开回收站` → `恢复`) [files].
15. Batch download: `下载 (2)` → one `revaro-download.zip` produced by `POST /api/files/batch-download/prepare` + a native anchor to `/api/files/batch-download/<token>`; no JS blob buffering and no iframe [files, app-controller.test].
16. CSP contains `frame-src 'none'`; zero `iframe` elements at all times [files].
17. Download label includes the selected file count [files].

**Uploads and HTTP**
18. Uploads survive malformed persisted state and failing `localStorage.setItem` [useUploads.test].
19. Resumed multipart upload resumes, progress finite and >90 before completion, `/complete` called with timeout `0` + `AbortSignal` [useUploads.test].
20. Transient `503` → task failed, `uploadId` retained, exactly one API call [useUploads.test].
21. `api()` honours `timeoutMs=0` (no implicit abort) while explicit `abort()` still rejects [api.test].

**Media**
22. Audio/image/video routing stays distinct; `hasAudioCover` only for audio with `has_cover` [fileTypes.test].
23. `readerDisplayTitle` strips `.epub/.txt/.markdown/.cbz` incl. multi-extension, leaves unsupported names and dotfiles [fileTypes.test].
24. `formatMediaTime` and `formatDuration` exact outputs incl. invalid-input normalisation [format.test, library.test].
25. Audio preview: readyState 4; chapter button (exact `章节`) opens the panel; `[data-chapter-index="1"]` updates `.audio-chapter-current h1`; ±15/30 s seeking; no horizontal overflow at 320/390/1440; Escape layering; focus returns to the originating card [media-ui].
26. Undecodable media → `error.code===4` + `role="alert"` with `浏览器无法播放`; no HLS/fMP4/transcode/stream fallback [media-ui].
27. Image: `实际大小` = 100 % + natural width; `适应窗口` fits; filmstrip lists siblings; `Escape` closes only the innermost layer; click-away hides and Tab restores the command bar; drag pan clamps [media-ui].
28. Image geometry: no upscaling, pointer-anchored zoom, pinch anchor tracking, letterbox centring, edge clamping [imageGeometry.test].
29. Video: autoplays, `.video-time` + `.video-subtitle-overlay` visible, rate applies, settings interaction holds controls open, idle auto-hide, subtitle y stable, no overflow at 320/390/1440 [media-ui].
30. Touch: tap toggles controls without pausing; pinch zooms; cancelled gesture must not page-turn; completed swipe advances [media-ui].
31. Native-media clock authority, paused-teardown-only suppression, sampler survival, explicit-zero-seek precedence [videoPlayer.test].
32. Cursor hiding only during unobstructed playback [videoPlayer.test].
33. Subtitle exclusivity (`hidden`/`disabled`), default→forced→0 selection, `video-subtitle-secondary-line` class [videoPlayer.test].
34. Subtitle letterbox insets exact values [videoPlayer.test].

**Library and sidebar**
35. Five `[data-category="book|image|video|audio|file"]` entries [library-ui].
36. Bookshelf: square fixed-size `.shelf-card`; one `.series-card` per multi-volume series with `同系列 · N 本`, 1 `.series-cover-main` + N−1 `.series-cover-fan`; same size as a single card at 3 and 12 volumes; fan covers the whole `.series-stage` [library-ui].
37. Volume-marker parsing incl. Chinese numerals and bare numbers; folder-tree ancestor counting and ` / ` paths; album grouping with root label `我的文件` [library.test].
38. Gallery: `.gallery-switch`, `.album-list`/`.album`, path filter meta `2 个项目`; audio `.view-switch` + `.file-rows .file-row` + duration `2:05`; file `.content-head h1` + 4 rows [library-ui].
39. Sidebar collapse classes and path-accordion exclusivity [library-ui].
40. Mobile drawer: handle aria-label toggling, `/mobile-open/`/`/open/` classes, no expand/count/path children, active row 48–52 px at full width, `.sidebar-foot` divider, `.content` unmoved, handle 32×52 pinned to the drawer's right edge vertically centred at mid-screen, close via backdrop/Escape/category [library-ui].

**Reader** — see §9.3 items 45–66 of the source analysis; the compact list:
41. Open → `#reader-view` visible, `#loading` hidden, `#flow .rf-chunk` present, `#page-label` non-empty [reader-flow, reader-real-epub].
42. Bar geometry: `#reader-title` extension-stripped and centred, no `#reader-kind`, `document.title` format, TOC entry only in `.reader-footer`, back button 44×44 transparent with a centred 24×24 icon, progress ring 44 px, mirrored controls [reader-flow].
43. Mobile reader chrome: footer flush, no `.reader-seek`, equal-width ≥60 px vertically centred buttons, ≥12 px bottom clearance [reader-flow].
44. Prefetch/paging: initial window `[0,1,2,3]` each requested exactly once; heat-path zero network; ≤5 `.rf-chunk` in DOM [reader-flow].
45. Pure-client relayout on font/line-height change (zero requests, position preserved within 2 %) [reader-flow].
46. TOC seek writes a `readingAnchor`; reopen resumes at the anchor's chunk (chunk 0 not requested) [reader-flow].
47. Backward paging past the start is safe [reader-flow].
48. Text locator precision (same-block multi-column, encoded fragments, `text_path` with spaces/non-ASCII, fallback to block start) [reader-flow].
49. Deep single-spine seek keeps the full spine prefix (`#flow .rf-chunk === chunkCount`) and is not pulled back by delayed window sync; `@benchmark` stays taggable and excludable [reader-flow].
50. Parent-chapter jump does not rebound after window sync; stale chunks released; saved progress inside the target chapter [reader-flow].
51. TOC targets in unloaded chunks fetch on demand and land precisely [reader-flow].
52. Deterministic TOC landing (repeated/round-trip seeks identical; spine-start block → column 0) [reader-flow].
53. Spine switching releases earlier chunks while page boundaries stay fixed [reader-flow].
54. Continuous paging moves a probe block exactly −1 page per turn [reader-flow].
55. Orientation relayout keeps the reading position [reader-flow].
56. Media NavAnchor (`data-rv-anchor`) positioning survives window sync and relayout [reader-flow].
57. Fragment-less TOC entry falls back to the server text locator [reader-flow].
58. Persistent L2: reopen = 0 chunk requests + 1 manifest fetch; version bump invalidates chunks [reader-flow].
59. Reader visuals: content underlaps the bar, `#flow` height = viewport height, `--revaro-col-height > 800` at 844 px, immersive/theme toggles leave the transform byte-identical, unified dark/light chrome, popover inside the viewport, focus/`aria-hidden` management for the TOC drawer [reader-flow].
60. `localStorage['revaro-reader-prefs']` is read at startup and honoured [reader-real-epub].
61. `caretRangeFromPoint`-based anchor capture and `#flow` children with `data-chunk` [reader-real-epub].
62. `computeMargins` pixel values; `PageCache`/`InFlight` semantics; `ClientCacheManager` key derivation/invalidation/LRU/stats; `compareAnchor` Go-parity; `spineForBlock`/`chunkForBlock`/`totalPrefix`/`locateChar`/`tocActiveIndex`; `spineOriginChunk`/`stableWindowRange` monotonicity [reader/cache.test, reader/clientCache.test, reader/flow.test].
63. No horizontal overflow at 1440/390/320 for audio/video/image [media-ui].
64. Screenshot evidence set: `bookshelf.png`, `gallery-albums.png`, `music-list.png`, `files-list.png`, `sidebar-collapsed.png`, `bookshelf-many.png`, `mobile-drawer.png`, `audio-{width}.png`, `image-desktop.png`, `video-{width}.png`, `reader-light.png`, `reader-immersive.png`, `reader-dark.png` [library-ui, media-ui, reader-flow].
65. JSON attachments: `reader-long-spine-benchmark.json`, `reader-real-epub-evidence`, `reader-drift-*` [reader-flow, reader-real-epub].

### 9.4 Portability summary

- **Port nearly verbatim (pure functions, 9 files):** `api.test.ts` (behavioural),
  `fileTypes.test.ts`, `format.test.ts`, `imageGeometry.test.ts`, `library.test.ts`,
  `taskStatus.test.ts`, `videoPlayer.test.ts`, `reader/cache.test.ts`,
  `reader/clientCache.test.ts`, `reader/flow.test.ts`.
- **Rewrite:** `app-controller.test.ts` (Vue component registry + source-text),
  `style.test.ts` (raw CSS/SFC substrings), `useUploads.test.ts` (Vue composable +
  XHR/localStorage doubles).
- **Playwright:** all 7 specs are DOM-contract coupled. If the Leptos markup preserves
  the exact ids/classes/`data-*`/ARIA names, `files.spec.ts` and the role-based parts
  are reusable nearly as-is; `library-ui.spec.ts`, `reader-flow.spec.ts` and
  `media-ui.spec.ts` need a new dev-server config (baseURL/ports) and re-validation of
  pixel geometry; `auth-status.spec.ts`, `mobile.spec.ts`, `reader-real-epub.spec.ts`
  need a real backend at `E2E_BASE_URL`. Note `reader-real-epub.spec.ts` currently
  encodes an **expected failure**.

---

## 10. Migration risk ranking (easiest → hardest)

Ordering is by combined cost: framework coupling + DOM/browser-API depth + test
constraint density.

### Tier 0 — Port almost mechanically (pure Rust, no DOM; unit-testable 1:1)

1. **`format.ts`** (24) — string/number formatting; `Intl`/`localeCompare` shims needed only elsewhere.
2. **`taskStatus.ts`** (7) — one predicate.
3. **`imageGeometry.ts`** (16) — arithmetic only; exact numbers pinned by tests.
4. **`fileTypes.ts`** (21) — regex/MIME routing.
5. **`library.ts`** (232) — pure but Chinese-numeral parsing + ICU collation (`localeCompare('zh-Hans-CN',{numeric:true})`) must be reproduced exactly.
6. **`reader/flow.ts`** (98) — pure, but the `compareAnchor` total order **must match the Go backend**; this is a contract, not an implementation detail.
7. **`reader/prefs.ts`** (50) — pure except `loadPrefs`/`savePrefs` localStorage.
8. **`reader/cache.ts`** (56) — `PageCache`/`InFlight`, trivially portable.
9. **`videoPlayer.ts`** (102) — pure predicates plus a thin media-element wrapper (the wrapper carries the risk, not the predicates).

### Tier 1 — Leaf presentational components (no state machine, no network)

10. `StatusBadge.vue`, `ServiceCard.vue`, `BookCover.vue`, `FileGrid.vue`, `FileRows.vue`,
    `GalleryGrid.vue`, `LoginPage.vue`, `ShareDialog.vue`, `DocumentEditor.vue`,
    `VideoStatusOverlay.vue`, `SelectionToolbar.vue`, `PreviewMenu.vue`,
    `MoveCopyDialog.vue`.
    Reasons: props→Rust props, emits→callbacks; only `PreviewMenu` (outside-click +
    Escape + focus) and `DocumentEditor` (uncontrolled textarea + `v-html` Markdown)
    need care.

### Tier 2 — Components with local behaviour and durable DOM contracts

11. `AppDialog.vue` — focus on mount; bare element selectors in its scoped CSS force a
    scoping decision.
12. `BookShelf.vue` — computed series fan geometry with **pixel-exact e2e assertions**
    (square card, cover union covering the stage). The math is simple; the asserted
    pixel identity is the risk.
13. `FullBleedProgress.vue` — `inheritAttrs:false` + `$attrs` forwarding must be
    modelled explicitly in Leptos (no implicit attribute fallthrough).
14. `LibraryView.vue` — `usePersistentMode` ×2 plus view branching.
15. `VideoControls.vue` — 17 props/17 events, drag-seek preview semantics.
16. `SidebarPathTree.vue` / `SidebarDirectoryNode.vue` / `SidebarFileTree.vue` —
    recursive components (Leptos recursion is fine but needs `Box`/function
    indirection), lazy `children` fetch, `reloadToken` invalidation, `--depth` var.
17. `FileCard.vue` — the most reused component; long-press selection with
    `navigator.vibrate`, `matchMedia('(hover: none), (pointer: coarse)')`, pointer
    timers, `defineExpose`, and thumbnail→preview fallback with per-item reactive maps.

### Tier 3 — Components with overlays, positioning and streaming

18. `DirectoryPicker.vue` — `Teleport to="body"`, viewport-collision positioning
    (`getBoundingClientRect`, `window.innerHeight/innerWidth`, scroll-capture listener),
    one `<Transition>`, outside-click/Escape.
19. `AppSidebar.vue` / `AppTopbar.vue` — `matchMedia` + change listeners,
    localStorage-backed accordion, imperative calls into child components
    (`defineExpose`) that Leptos must replace with signals/`NodeRef`s, document-level
    pointer/keydown handlers.
20. `TaskCenter.vue` — grouping/progress computations, Teleport password dialog, two
    API calls, `defineExpose`, opaque-flyout parity test.
21. `SystemStatus.vue` — **`EventSource`** (no first-class `web-sys` support), manual
    exponential reconnect, `color-mix` halo, `defineExpose`.
22. `useJobEvents.ts` / `useBackgroundTasks.ts` — SSE + polling fallback + de-duplicated
    refresh + notification side effects.
23. `useAccountSettings.ts` — TOTP state machine, `FileReader` data URL, Clipboard,
    `URL.createObjectURL` + synthetic anchor, avatar cache-busting version.
24. `useAuthSession.ts` / `useDialogs.ts` / `useLocalView.ts` — small but load-bearing
    (error-`code` handling, promise-based dialogs, persisted preferences).

### Tier 4 — Hard: async state machines and rich browser interaction

25. **`useUploads.ts` (170)** — three-level upload protocol (single/multipart/resume),
    **`XMLHttpRequest` for upload progress** (requires a JS shim), `ETag` handling,
    5-retry exponential backoff, 3/4-way concurrency, 100-part batching, folder-structure
    reconstruction with `409` reconciliation, localStorage resume ledger, cancellation
    via `AbortController` + XHR abort, and a fragile-but-tested progress invariant.
26. **`MediaPreview.vue` (155)** — multi-pointer drag/pinch/zoom/pan math, pointer
    capture, `pointercancel` semantics, wheel zoom with `deltaMode`, double-click vs
    click timers, `ResizeObserver`, chrome auto-hide/inert toggle, filmstrip,
    layered Escape, image preloading via `new Image()`.
27. **`AudioPlayer.vue` (220)** — Range playback, chapter model + navigation, buffered
    tracking, seek preview/hover tooltip, rate/volume persistence, keyboard shortcuts,
    three persistence timers plus a `keepalive` fetch on unmount, bottom-sheet focus
    management.
28. **`VideoPlayer.vue` (189) + `useVideoSubtitles.ts` + `videoPlayer.ts`** — native
    media element clock via `requestAnimationFrame`, autoplay policy handling, fullscreen
    + `webkitEnterFullscreen` fallback, `TextTrack` mode juggling so the browser parses
    cues without drawing them, VTTCue line→placement mapping, `DOMParser` entity decode,
    letterbox inset computation driven by `ResizeObserver`, `crossorigin` `<track>`,
    `error.code` surfacing with **no transcode fallback**, and a 6-timer cleanup dance.
29. **`app-controller.ts` (378)** — the application store: hand-rolled History-API
    routing with a nav-action stack, modal-as-history, selection model, directory race
    guard, batch-ZIP download architecture (pinned by a source-text test), document
    editor with ETag/dirty state, `DOMPurify`+`marked` Markdown pipeline, async component
    loading, ~150 exported bindings. This is a rewrite of the app's spine, not a port.
30. **`useReaderFlow.ts` (958) + `useReaderPositioning.ts` (429) + `useReaderWindow.ts`
    (171) + `reader/clientCache.ts` (241) + `Reader.vue` (109)** — the hardest by a wide
    margin. It combines: CSS multi-column pagination driven from Rust; a stable
    spine-prefix window invariant; incremental, bounded, concurrent DOM insertion of raw
    chunk HTML; L1+L2 (IndexedDB) caching with content-fingerprint invalidation;
    `Range`/caret-rect geometry and `caretRangeFromPoint` top-anchor capture;
    `elementsFromPoint` block hit-testing; WAAPI page-turn animation with
    `will-change` lifecycle; touch swipe with rubber-banding and
    `turnBusy`/`syncing`/`navDepth`/`pendingTurns`/`gen` interlock; four debounced
    relayout/sync/progress timers; layered TOC locator fallback chain; and the largest,
    strictest e2e suite in the repo (17 tests, including request-count invariants,
    byte-identical `transform` assertions, and pixel geometry).

### Top 10 hardest parts to port

| # | Part | Why it is hard |
|---|---|---|
| 1 | `useReaderFlow.ts` (958) | Reader state machine: pagination math, anchor capture/restore, four interlocking async guards, WAAPI animation, touch gestures, four debounce timers, progress persistence, L2 fast-open + manifest validation. Nothing can be simplified without breaking documented e2e invariants. |
| 2 | `useReaderPositioning.ts` (429) | Anchor ↔ CSS-columns geometry using `Range` caret rects, `caretRangeFromPoint`, `elementsFromPoint`, `createTreeWalker`, `compareDocumentPosition`, binary search for the first in-column caret. Pure `web-sys` interop with subtle correctness requirements. |
| 3 | `useReaderWindow.ts` + `reader/clientCache.ts` (412) | Incremental stable-prefix DOM window with bounded concurrency and ordered cache re-touch, plus an IndexedDB L2 with content-fingerprint keying, LRU byte budget and stats. |
| 4 | `app-controller.ts` (378) | History-API routing + nav stack, modal machine, selection, upload/task/library wiring, batch download architecture, Markdown pipeline, ~150 template bindings. |
| 5 | `useUploads.ts` (170) | Multipart/resume protocol with `XMLHttpRequest` progress (needs a JS shim), ETag verification, retry/backoff, 409 folder reconciliation, cancellation, persisted resume ledger. |
| 6 | `VideoPlayer.vue` + `useVideoSubtitles.ts` + `videoPlayer.ts` (319) | Native media clock, autoplay + fullscreen fallbacks, `TextTrack` mode juggling with an own overlay, cue parsing/placement, `ResizeObserver` insets, no-transcode error semantics, six-timer cleanup. |
| 7 | `MediaPreview.vue` (155) | Multi-pointer drag/pinch/zoom/pan, pointer capture, wheel/`deltaMode`, gesture-vs-click disambiguation, `ResizeObserver`, layered Escape and `inert` chrome. |
| 8 | `AudioPlayer.vue` (220) | Range playback + chapter model + three persistence timers + `keepalive` unmount flush + bottom-sheet focus management. |
| 9 | `Reader.vue` + `reader-flow.css`/`reader-chrome.css` (265 CSS lines) | The reader DOM/ARIA/id contract and CSS-columns geometry are pinned by the strictest tests; `break-before: column`, `column-fill: auto`, injected `--revaro-*` variables and transform-based paging must behave identically. |
| 10 | SSE stack: `useJobEvents.ts` + `SystemStatus.vue` | `EventSource` has no first-class `web-sys` support; needs a shim plus exact reconnect/backoff/fallback and named-event handling. (Runner-up: `BookShelf.vue`'s series-fan geometry because of pixel-exact assertions.) |

---

## Appendix A — Files read for this audit

`web/src/App.vue`, `main.ts`, `api.ts`, `app-controller.ts`, `library.ts`, `types.ts`,
`fileTypes.ts`, `format.ts`, `taskStatus.ts`, `imageGeometry.ts`, `videoPlayer.ts`,
`fileSystemAccess.d.ts`, `Reader.vue`, `VideoThumb.vue`;
all 31 files in `web/src/components/`;
all 14 files in `web/src/composables/`;
all 6 non-test files in `web/src/reader/`;
all 16 CSS files;
all 13 Vitest files and all 8 Playwright files/configs;
`/config/revaro/docs/reader-flow.md`;
`internal/server/server_helpers.go` / `errors.go` (error-envelope confirmation only).

## Appendix B — Inventory counts (for planning)

- Components: **34** (3 root + 31 under `components/`).
- Composables: **14**; supporting modules: **7** (`app-controller`, `library`,
  `fileTypes`, `format`, `taskStatus`, `imageGeometry`, `videoPlayer`); reader modules: **6**.
- HTTP API surface consumed: **46 distinct paths / 60 method+path rows / 58 `/api/*`
  routes + 1 presigned external PUT**; **63 `api()` call sites in app code (36 typed
  `api<T>(…)` + 27 untyped `api(…)`, plus 1 more in `api.test.ts`)** and
  **5 raw `fetch()` call sites** (one of which is the `api()` implementation itself),
  plus 5 native URL/`src`/anchor usages.
- localStorage keys: **13 distinct**.
- CSS: **16 files, 4,549 lines, 1,085 rule blocks, 67 custom properties, 10 keyframes
  (2 dead), 5 `prefers-reduced-motion` blocks, 10 scoped SFC blocks (249 rules)**.
- Tests: **13 Vitest files (669 lines) + 7 Playwright specs + 1 helper (1,850 lines) +
  3 configs**; ~65 distinct asserted behaviours.
