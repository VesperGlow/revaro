// Pure helpers shared by the "merge from local directory" flow. Kept
// dependency-free so they can be unit tested without a DOM.
export const localMergeAudioExts = new Set(['.mp3', '.wav', '.flac', '.m4a', '.aac', '.ogg', '.oga', '.opus', '.wma', '.aif', '.aiff', '.ape'])
export const localMergeCoverExts = new Set(['.jpg', '.jpeg', '.png', '.gif', '.webp', '.bmp'])
export const localMergeCoverKeywords = ['cover', 'folder', 'front', 'album', 'art', 'poster', 'thumb', '封面']

// naturalLess orders names the way humans expect: case-insensitively, with
// numeric runs compared by value ("track2.wav" before "track10.wav").
export function localNaturalLess(a: string, b: string): boolean {
  let ai = 0
  let bi = 0
  while (ai < a.length && bi < b.length) {
    const ca = a.charCodeAt(ai)
    const cb = b.charCodeAt(bi)
    const da = ca >= 48 && ca <= 57
    const db = cb >= 48 && cb <= 57
    if (da && db) {
      let ea = ai
      let eb = bi
      while (ea < a.length && a.charCodeAt(ea) >= 48 && a.charCodeAt(ea) <= 57) ea++
      while (eb < b.length && b.charCodeAt(eb) >= 48 && b.charCodeAt(eb) <= 57) eb++
      const c = localCompareDigitRuns(a.slice(ai, ea), b.slice(bi, eb))
      if (c !== 0) return c < 0
      ai = ea
      bi = eb
      continue
    }
    const la = ca >= 65 && ca <= 90 ? ca + 32 : ca
    const lb = cb >= 65 && cb <= 90 ? cb + 32 : cb
    if (la !== lb) return la < lb
    ai++
    bi++
  }
  return a.length < b.length
}

function localCompareDigitRuns(a: string, b: string): number {
  const ta = a.replace(/^0+/, '')
  const tb = b.replace(/^0+/, '')
  if (ta.length !== tb.length) return ta.length < tb.length ? -1 : 1
  if (ta < tb) return -1
  if (ta > tb) return 1
  if (a.length !== b.length) return a.length < b.length ? -1 : 1 // "1" < "01"
  return 0
}

// localCoverScore ranks cover candidates by well-known cover names. Higher is
// better; zero means no cover-like name was detected.
export function localCoverScore(name: string): number {
  const base = name.replace(/\.[^.]+$/, '').toLowerCase()
  for (let rank = 0; rank < localMergeCoverKeywords.length; rank++) {
    const keyword = localMergeCoverKeywords[rank]
    if (base === keyword) return 100 - rank
    if (base.length > keyword.length && base.startsWith(keyword) && ' _-.(['.includes(base[keyword.length])) return 80 - rank
    if (base.includes(keyword)) return 60 - rank
  }
  return 0
}

// selectLocalCover picks the default cover: the only image, or the best
// cover-named image when several are present. With several images and no
// cover-like name the choice is left to the user.
export function selectLocalCover(covers: string[]): string {
  if (!covers.length) return ''
  if (covers.length === 1) return covers[0]
  let best = ''
  let bestScore = -1
  for (const candidate of covers) {
    const score = localCoverScore(candidate)
    if (score > bestScore) {
      best = candidate
      bestScore = score
    }
  }
  return bestScore > 0 ? best : ''
}

// localSubtitlePriority mirrors the server's audioSubtitleMatchPriority:
// track.vtt beats track.wav.vtt, which beats a title carrying any supported
// audio extension (track.mp3.vtt).
export function localSubtitlePriority(audioName: string, subtitleName: string): number {
  if (!/\.vtt$/i.test(subtitleName)) return -1
  const audioTitle = audioName.replace(/\.[^.]+$/, '')
  const subtitleTitle = subtitleName.replace(/\.vtt$/i, '')
  if (subtitleTitle.localeCompare(audioTitle, undefined, { sensitivity: 'accent' }) === 0) return 0
  if (subtitleTitle.localeCompare(audioName, undefined, { sensitivity: 'accent' }) === 0) return 1
  const sourceExtension = subtitleTitle.match(/\.[^.]+$/)?.[0].toLowerCase() || ''
  if (localMergeAudioExts.has(sourceExtension) && subtitleTitle.slice(0, -sourceExtension.length).localeCompare(audioTitle, undefined, { sensitivity: 'accent' }) === 0) return 2
  return -1
}
