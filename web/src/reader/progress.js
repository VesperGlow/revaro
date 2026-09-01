export function createProgressStore(getActive,api,delay=1200){
  let chain=Promise.resolve(),timer=null
  function commit(bookId,totalPages,page){chain=chain.then(()=>api(`/api/files/${bookId}/book/progress`,{method:'PUT',headers:{'content-type':'application/json'},body:JSON.stringify({page,total_pages:totalPages}),keepalive:true})).catch(error=>console.error('保存阅读进度失败',error))}
  function queue(page){const active=getActive();if(!active)return;const bookId=active.bookId,totalPages=active.pageCount;if(timer)clearTimeout(timer);timer=setTimeout(()=>{timer=null;commit(bookId,totalPages,page)},delay)}
  function save(){const active=getActive();if(!active)return;if(timer){clearTimeout(timer);timer=null}commit(active.bookId,active.pageCount,active.currentPage)}
  return {queue,save}
}
