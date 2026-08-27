import { describe, expect, it } from 'vitest'
import { localCoverScore, localMergeAudioExts, localMergeCoverExts, localNaturalLess, localSubtitlePriority, selectLocalCover } from './localMerge'

describe('local merge natural sort', () => {
  it('orders numeric runs by value', () => {
    expect(localNaturalLess('track2.wav', 'track10.wav')).toBe(true)
    expect(localNaturalLess('track10.wav', 'track2.wav')).toBe(false)
    expect(localNaturalLess('2.wav', '10.wav')).toBe(true)
    expect(localNaturalLess('第2节.wav', '第10节.wav')).toBe(true)
  })
  it('compares case-insensitively and handles ties by length', () => {
    expect(localNaturalLess('A.wav', 'b.wav')).toBe(true)
    expect(localNaturalLess('1.wav', '01.wav')).toBe(true)
    expect(localNaturalLess('01.wav', '1.wav')).toBe(false)
  })
  it('sorts a full directory listing naturally', () => {
    const names = ['track2.wav', 'track10.wav', '01.wav', '1.wav', 'b.wav', 'A.wav', '第10节.wav', '第2节.wav']
    const want = ['1.wav', '01.wav', 'A.wav', 'b.wav', 'track2.wav', 'track10.wav', '第2节.wav', '第10节.wav']
    expect([...names].sort((a, b) => (localNaturalLess(a, b) ? -1 : 1))).toEqual(want)
  })
})

describe('local merge file classification', () => {
  it('recognizes WAV audio, VTT subtitles and cover images', () => {
    expect(localMergeAudioExts.has('.wav')).toBe(true)
    expect(localMergeAudioExts.has('.WAV')).toBe(false) // extension check lowercases first
    expect(localMergeCoverExts.has('.webp')).toBe(true)
    expect(localMergeCoverExts.has('.txt')).toBe(false)
  })
})

describe('local merge cover selection', () => {
  it('auto-selects the only image', () => {
    expect(selectLocalCover(['artwork.png'])).toBe('artwork.png')
  })
  it('prefers cover / folder / front / album named images', () => {
    expect(selectLocalCover(['photo1.jpg', 'Cover.jpg'])).toBe('Cover.jpg')
    expect(selectLocalCover(['photo1.jpg', 'front.png', 'album art.jpg'])).toBe('front.png')
    expect(localCoverScore('Cover.jpg')).toBeGreaterThan(localCoverScore('photo1.jpg'))
  })
  it('leaves ambiguous folders to the user', () => {
    expect(selectLocalCover(['photo1.jpg', 'photo2.jpg'])).toBe('')
  })
  it('handles an empty folder', () => {
    expect(selectLocalCover([])).toBe('')
  })
})

describe('local merge subtitle matching', () => {
  it('matches track.vtt, track.wav.vtt and exporter track.mp3.vtt', () => {
    expect(localSubtitlePriority('track.wav', 'track.vtt')).toBe(0)
    expect(localSubtitlePriority('track.wav', 'track.wav.vtt')).toBe(1)
    expect(localSubtitlePriority('track.wav', 'track.mp3.vtt')).toBe(2)
  })
  it('rejects unrelated subtitles and non-VTT files', () => {
    expect(localSubtitlePriority('track.wav', 'other.vtt')).toBe(-1)
    expect(localSubtitlePriority('track.wav', 'track.srt')).toBe(-1)
  })
})
