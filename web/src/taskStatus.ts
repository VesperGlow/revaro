import type { TaskStatus } from './types'

const activeTaskStatuses:ReadonlySet<TaskStatus>=new Set(['queued','running','waiting_input','retrying'])

export function isActiveTaskStatus(status:TaskStatus){
  return activeTaskStatuses.has(status)
}
