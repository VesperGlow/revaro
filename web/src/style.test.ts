import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'

const styleModules=['shell','browser','uploads','dialogs','media','responsive']
const css=styleModules.map(name=>readFileSync(new URL(`./styles/${name}.css`,import.meta.url),'utf8')).join('\n')
const taskCenter=readFileSync(new URL('./components/TaskCenter.vue',import.meta.url),'utf8')

describe('task center flyout',()=>{
  it('keeps its surface opaque and completion actions touchable',()=>{
    expect(css).toContain('z-index: 20')
    expect(taskCenter).toContain('.task-panel{z-index:25;isolation:isolate;background:#fff}')
    expect(taskCenter).toContain('min-height:38px')
    expect(taskCenter).toContain('min-height:40px')
    expect(taskCenter).toContain('清除完成')
    expect(taskCenter).toContain('ChevronDown')
  })
})
