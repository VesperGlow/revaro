export function findTocTarget(state,entry,index){
  const root=state.segments
  if(state.kind==='txt')return root.querySelector(`[data-toc="${index}"]`)
  if(entry.fragment){const byId=Array.from(root.querySelectorAll('[id], [data-frag-ids]')).find(element=>element.id===entry.fragment||(element.dataset.fragIds||'').split(' ').includes(entry.fragment));if(byId)return byId}
  return Array.from(root.querySelectorAll('[data-source-path]')).find(element=>element.dataset.sourcePath===entry.path)||null
}

export function tocTargetPage(state,entry,index){
  const target=findTocTarget(state,entry,index)
  if(!target)return Math.max(0,entry.page||0)
  const owner=target.closest('.book-content')
  if(!owner||typeof owner._startPage!=='number')return Math.max(0,entry.page||0)
  const page=owner._startPage+Math.round((target.getBoundingClientRect().left-owner.getBoundingClientRect().left)/state.pageStep)
  return Math.min(Math.max(0,page),Math.max(0,state.pageCount-1))
}
