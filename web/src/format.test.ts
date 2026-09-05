import { describe, expect, it } from 'vitest'
import { formatMediaTime } from './format'

describe('formatMediaTime',()=>{
  it('uses one duration format for audio and video controls',()=>{
    expect(formatMediaTime(0)).toBe('0:00')
    expect(formatMediaTime(65.9)).toBe('1:05')
    expect(formatMediaTime(3661)).toBe('1:01:01')
  })

  it('normalizes invalid timeline values',()=>{
    expect(formatMediaTime(-1)).toBe('0:00')
    expect(formatMediaTime(Number.NaN)).toBe('0:00')
    expect(formatMediaTime(Number.POSITIVE_INFINITY)).toBe('0:00')
  })
})
