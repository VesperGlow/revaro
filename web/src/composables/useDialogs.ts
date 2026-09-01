import { reactive } from 'vue'

export function useDialogs(){
 const dialog=reactive({open:false,title:'',message:'',confirmLabel:'确定',cancelLabel:'取消',tone:'default' as 'default'|'danger',input:false,value:'',placeholder:''})
 let resolveDialog:((value:string|boolean|null)=>void)|null=null
 function askDialog(options:{title:string;message:string;confirmLabel?:string;cancelLabel?:string;tone?:'default'|'danger';input?:boolean;value?:string;placeholder?:string}){dialog.title=options.title;dialog.message=options.message;dialog.confirmLabel=options.confirmLabel||'确定';dialog.cancelLabel=options.cancelLabel||'取消';dialog.tone=options.tone||'default';dialog.input=!!options.input;dialog.value=options.value||'';dialog.placeholder=options.placeholder||'';dialog.open=true;return new Promise<string|boolean|null>(resolve=>{resolveDialog=resolve})}
 async function confirmDialog(options:{title:string;message:string;confirmLabel?:string;tone?:'default'|'danger'}){return await askDialog(options)===true}
 async function promptDialog(options:{title:string;message:string;value?:string;placeholder?:string;confirmLabel?:string}){const result=await askDialog({...options,input:true});return typeof result==='string'?result:null}
 function finishDialog(confirmed:boolean){const resolve=resolveDialog;resolveDialog=null;dialog.open=false;if(!resolve)return;resolve(confirmed?(dialog.input?dialog.value.trim():true):null)}
 return {dialog,askDialog,confirmDialog,promptDialog,finishDialog}
}
