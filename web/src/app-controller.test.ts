import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './app-controller'
import { downloadSelectedBatch } from './download'

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

describe('app controller', () => {
  it('registers every component referenced by the external App template', () => {
    expect(Object.keys(App.components ?? {}).sort()).toEqual([
      'AppDialog',
      'AppTopbar',
      'DocumentEditor',
      'FileBrowserHeader',
      'FileGrid',
      'LoginPage',
      'MediaPreview',
      'MoveCopyDialog',
      'Reader',
      'SelectionToolbar',
      'ShareDialog',
    ])
  })

  it('downloads multiple selected files with one native form submission', () => {
    const fetchMock=vi.spyOn(globalThis,'fetch')
    const submit=vi.fn()
    const remove=vi.fn()
    const appendChild=vi.fn()
    const form={method:'',action:'',target:'',enctype:'',hidden:false,appendChild:vi.fn(),submit,remove} as unknown as HTMLFormElement
    const inputs:HTMLInputElement[]=[]
    vi.stubGlobal('document',{createElement:vi.fn((tag:string)=>{
      if(tag==='form')return form
      if(tag==='iframe')return {name:'',hidden:false,isConnected:true,setAttribute:vi.fn()} as unknown as HTMLIFrameElement
      const input={type:'',name:'',value:''} as unknown as HTMLInputElement
      inputs.push(input)
      return input
    }),body:{appendChild}})

    downloadSelectedBatch([{id:'id1'},{id:'id2'}])

    expect(fetchMock).not.toHaveBeenCalled()
    expect(App.setup?.toString() ?? '').not.toContain('response.blob')
    expect(form.method).toBe('POST')
    expect(form.action).toBe('/api/files/batch-download')
    expect(form.target).toBe('revaro-batch-download-frame')
    expect(form.enctype).toBe('application/x-www-form-urlencoded')
    expect(downloadSelectedBatch.toString()).not.toContain('response.blob')
    expect(inputs.map(input=>({type:input.type,name:input.name,value:input.value}))).toEqual([
      {type:'hidden',name:'ids',value:'id1'},
      {type:'hidden',name:'ids',value:'id2'},
    ])
    expect(form.appendChild).toHaveBeenCalledTimes(2)
    expect(appendChild).toHaveBeenCalledTimes(2)
    expect(submit).toHaveBeenCalledOnce()
    expect(remove).toHaveBeenCalledOnce()
  })

  it('does not request a batch ZIP for an empty file selection', async () => {
    const fetchMock=vi.spyOn(globalThis,'fetch')
    await downloadSelectedBatch([])
    expect(fetchMock).not.toHaveBeenCalled()
  })
})
