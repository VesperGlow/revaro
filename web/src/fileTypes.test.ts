import { describe, expect, it } from 'vitest'
import type { DriveFile } from './api'
import { hasAudioCover, isAudio, isImage, isVideo, readerDisplayTitle } from './fileTypes'

const file = (name:string, mime_type:string, has_cover?:boolean):DriveFile => ({
  id:name, parent_id:null, name, kind:'file', size:1, mime_type, etag:'v1',
  status:'ready', created_at:'', updated_at:'', has_cover,
})

describe('file card media routing', () => {
  it('keeps audio, image, and video types distinct', () => {
    const audio=file('asmr.flac','audio/flac',true)
    expect(isAudio(audio)).toBe(true)
    expect(isImage(audio)).toBe(false)
    expect(isVideo(audio)).toBe(false)
    expect(hasAudioCover(audio)).toBe(true)
    expect(hasAudioCover(file('plain.mp3','audio/mpeg'))).toBe(false)
  })
})

describe('readerDisplayTitle',()=>{
  it.each([
    ['长标题.epub','长标题'],
    ['notes.TXT','notes'],
    ['archive.epub.pdf.md','archive'],
    ['book.markdown','book'],
    ['comic.cbz','comic'],
    ['report.docx','report.docx'],
    ['.epub','.epub'],
  ])('normalizes %s for display without changing unsupported names',(name,want)=>{
    expect(readerDisplayTitle(name)).toBe(want)
  })
})
