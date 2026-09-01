// Build bounded DOM segments for both TXT and EPUB books. Layout and paging
// stay in ReaderApp; this module only turns the parsed model into chapter nodes.
export function assembleSegments(model) {
  const wrapper = document.createElement('div')
  wrapper.className = 'book-segments'
  if (model.kind === 'txt') {
    const text = model.text || ''
    const marks = (model.toc || []).map((entry,index)=>({offset:Number(entry.offset)||0,index})).sort((a,b)=>a.offset-b.offset)
    const cuts=[0]
    for(const mark of marks)if(mark.offset>0&&mark.offset<text.length)cuts.push(mark.offset)
    cuts.push(text.length)
    if(!marks.length&&text.length>60000){cuts.length=0;for(let pos=0;pos<text.length;pos+=20000)cuts.push(pos);cuts.push(text.length)}
    for(let index=0;index<cuts.length-1;index++){
      const start=cuts[index],end=cuts[index+1]
      if(end<=start)continue
      const node=document.createElement('div');node.className='book-content txt'
      if(start>0)for(const mark of marks)if(mark.offset===start){const anchor=document.createElement('span');anchor.className='toc-anchor';anchor.dataset.toc=String(mark.index);node.appendChild(anchor)}
      node.appendChild(document.createTextNode(text.slice(start,end)));wrapper.appendChild(node)
    }
  } else {
    const chapters=model.chapters?.length?model.chapters:[{html:model.html||''}]
    for(const chapter of chapters){const node=document.createElement('div');node.className='book-content epub';node.innerHTML=chapter.html||'';wrapper.appendChild(node)}
  }
  if(!wrapper.childNodes.length)wrapper.appendChild(document.createElement('div'))
  return wrapper
}
