import { describe, expect, it } from 'vitest'
import { isActiveTaskStatus } from './taskStatus'
import type { TaskStatus } from './types'

describe('isActiveTaskStatus',()=>{
  it('keeps task-center and topbar activity semantics aligned',()=>{
    const statuses:TaskStatus[]=['queued','running','waiting_input','retrying','completed','failed','cancelled']
    expect(statuses.filter(isActiveTaskStatus)).toEqual(['queued','running','waiting_input','retrying'])
  })
})
