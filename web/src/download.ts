import type { DriveFile } from './api'

const batchDownloadPath='/api/files/batch-download'
const batchDownloadTarget='revaro-batch-download-frame'
let batchDownloadFrame:HTMLIFrameElement|null=null

function ensureBatchDownloadFrame(){
  if(batchDownloadFrame?.isConnected)return
  const frame=document.createElement('iframe')
  frame.name=batchDownloadTarget
  frame.hidden=true
  frame.setAttribute('aria-hidden','true')
  document.body.appendChild(frame)
  batchDownloadFrame=frame
}

export function downloadSelectedBatch(files:Pick<DriveFile,'id'>[]):void{
  if(!files.length)return
  ensureBatchDownloadFrame()
  const form=document.createElement('form')
  form.method='POST'
  form.action=batchDownloadPath
  form.target=batchDownloadTarget
  form.enctype='application/x-www-form-urlencoded'
  form.hidden=true
  for(const file of files){
    const input=document.createElement('input')
    input.type='hidden'
    input.name='ids'
    input.value=file.id
    form.appendChild(input)
  }
  document.body.appendChild(form)
  form.submit()
  form.remove()
}
