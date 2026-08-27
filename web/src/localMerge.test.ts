import { describe, expect, it } from 'vitest'
import { classifyLocalMergeFile, localCoverScore, localMergeAudioExts, localMergeCoverExts, localMergeTopLevelName, localNaturalLess, localSubtitlePriority, selectLocalCover } from './localMerge'

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

describe('local merge directory scan', () => {
  it('classifies audio, subtitle and cover files by extension', () => {
    expect(classifyLocalMergeFile('track1.wav')).toBe('audio')
    expect(classifyLocalMergeFile('track1.mp3')).toBe('audio')
    expect(classifyLocalMergeFile('track1.flac')).toBe('audio')
    expect(classifyLocalMergeFile('track1.m4a')).toBe('audio')
    expect(classifyLocalMergeFile('track1.wav.vtt')).toBe('subtitle')
    expect(classifyLocalMergeFile('track1.vtt')).toBe('subtitle')
    expect(classifyLocalMergeFile('cover.jpg')).toBe('cover')
    expect(classifyLocalMergeFile('notes.txt')).toBeNull()
    expect(classifyLocalMergeFile('README')).toBeNull()
  })
  it('keeps only top-level files from a webkitdirectory relative path', () => {
    expect(localMergeTopLevelName('Album/01.wav')).toBe('01.wav')
    expect(localMergeTopLevelName('Album/01.wav.vtt')).toBe('01.wav.vtt')
    expect(localMergeTopLevelName('Album/sub/02.wav')).toBeNull()
    expect(localMergeTopLevelName('Album')).toBeNull()
  })
  it('recognizes audio and the matching .wav.vtt subtitles in a folder', () => {
    const names = ['02.wav', '01.wav.vtt', '01.wav', '02.wav.vtt', 'cover.jpg', 'notes.txt']
    const picks = names.map(name => ({ name, kind: classifyLocalMergeFile(name) }))
    const audios = picks
      .filter(pick => pick.kind === 'audio')
      .sort((a, b) => (localNaturalLess(a.name, b.name) ? -1 : 1))
    expect(audios.map(audio => audio.name)).toEqual(['01.wav', '02.wav'])
    for (const audio of audios) {
      const subtitle = picks
        .filter(pick => pick.kind === 'subtitle')
        .map(pick => ({ name: pick.name, priority: localSubtitlePriority(audio.name, pick.name) }))
        .filter(match => match.priority >= 0)
        .sort((a, b) => a.priority - b.priority)[0]
      expect(subtitle?.name).toBe(`${audio.name}.vtt`)
    }
    const cover = picks.filter(pick => pick.kind === 'cover').map(pick => pick.name)
    expect(cover).toEqual(['cover.jpg'])
    expect(selectLocalCover(cover)).toBe('cover.jpg')
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
