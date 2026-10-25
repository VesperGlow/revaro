import init,{start_siglus_from_directory} from '/galgame/siglus/pkg/game_launcher.js';
const error=document.getElementById('error'),canvas=document.getElementById('siglus-canvas');
const setStatus=()=>{};
const token=location.pathname.split('/').filter(Boolean)[1];
const archiveUrl=`/g/${encodeURIComponent(token)}/archive`;
const profile=new URL(location.href).searchParams.get('profile')||'chinese';
const files=new Map(),overlay=new Map(),children=new Map();const reads=[];let meta,db;const cache=new Map();let cacheBytes=0;const cacheLimit=64*1024*1024;
window.siglusProbe={reads,profile,overlayReads:[],saveWrites:[]};
function fail(e){error.style.display='block';error.textContent=e?.stack||String(e);console.error(e)}
window.addEventListener('error',e=>fail(e.error||e.message));window.addEventListener('unhandledrejection',e=>fail(e.reason));
function norm(p){p=String(p).replaceAll('\\','/');if(p.split('/').includes('..')||p.includes('\0'))throw Error('Invalid game path');return p.split('/').filter(x=>x&&x!=='.').join('/').toLowerCase()}
function indexChildren(n){const parts=n.split('/');let dir='';for(const part of parts){if(!children.has(dir))children.set(dir,new Set());children.get(dir).add(part);dir=dir?dir+'/'+part:part}}
function index(path,value){const n=norm(path);files.set(n,value);indexChildren(n)}
function crc32(bytes){let crc=-1;for(let i=0;i<bytes.length;i++){crc^=bytes[i];for(let j=0;j<8;j++)crc=(crc>>>1)^((crc&1)?0xedb88320:0)}return (crc^-1)>>>0}
function read(path){const n=norm(path);if(overlay.has(n)){siglusProbe.overlayReads.push(n);return overlay.get(n);}const f=files.get(n);if(!f)throw Error('Missing game resource: '+path);if(cache.has(f.path)){const hit=cache.get(f.path);cache.delete(f.path);cache.set(f.path,hit);return hit;}if(!f.packed)return new Uint8Array();const xhr=new XMLHttpRequest();xhr.open('GET',archiveUrl,false);xhr.setRequestHeader('Range',`bytes=${f.offset}-${f.offset+f.packed-1}`);xhr.overrideMimeType('text/plain; charset=x-user-defined');xhr.send();if(xhr.status!==206)throw Error(`Range read failed (${xhr.status}): ${path}`);const s=xhr.responseText;const packed=new Uint8Array(s.length);for(let i=0;i<s.length;i++)packed[i]=s.charCodeAt(i)&255;const data=f.method===0?packed:fflate.inflateSync(packed);if(data.length!==f.size||crc32(data)!==f.crc)throw Error('Resource size/CRC mismatch: '+path);if(data.length<=cacheLimit){while(cacheBytes+data.length>cacheLimit&&cache.size){const oldest=cache.keys().next().value;cacheBytes-=cache.get(oldest).length;cache.delete(oldest);}cache.set(f.path,data);cacheBytes+=data.length;}reads.push({path,source:f.path,bytes:packed.length,status:xhr.status});console.log('[resource]',path,'<-',f.path,packed.length);return data}
globalThis.siglusFileExists=p=>overlay.has(norm(p))||files.has(norm(p));globalThis.siglusKnownFileCount=()=>files.size+overlay.size;globalThis.siglusListDir=p=>Array.from(children.get(norm(p))||[]);globalThis.siglusReadFile=read;
globalThis.gameFsWrite=(p,b)=>{const n=norm(p),v=new Uint8Array(b);overlay.set(n,v);indexChildren(n);const t=db.transaction('files','readwrite');t.objectStore('files').put(v,n);t.oncomplete=()=>siglusProbe.saveWrites.push({path:n,size:v.length});console.log('[save]',n,v.length)};globalThis.gameFsRemove=p=>{const n=norm(p);overlay.delete(n);const t=db.transaction('files','readwrite');t.objectStore('files').delete(n)};
function openDb(){return new Promise((resolve,reject)=>{const r=indexedDB.open(meta.save_space+'-v2-'+profile,1);r.onupgradeneeded=()=>r.result.createObjectStore('files');r.onerror=()=>reject(r.error);r.onsuccess=()=>resolve(r.result)})}
async function start(){try{setStatus('正在读取 ZIP 目录…');const r=await fetch(`/g/${encodeURIComponent(token)}/index`);if(!r.ok)throw Error('Game index unavailable');meta=await r.json();for(const f of meta.entries){if(!/^(savedata|save)\//i.test(f.path))index(f.path,f);}
if(profile==='chinese'){for(const [from,to] of [['scene.chs','Scene.pck'],['gameexe.chs','Gameexe.dat']]){const f=files.get(norm(from));if(!f)throw Error('Selected Chinese resource missing: '+from);index(to,f)}for(const f of meta.entries){if(f.path.toLowerCase().startsWith('g00/')&&f.path.toLowerCase().endsWith('.g01'))index(f.path.slice(0,-1)+'0',f)}}
db=await openDb();await new Promise((resolve,reject)=>{const q=db.transaction('files').objectStore('files').openCursor();q.onsuccess=()=>{const c=q.result;if(c){overlay.set(c.key,new Uint8Array(c.value));indexChildren(c.key);c.continue()}else resolve()};q.onerror=()=>reject(q.error)});
setStatus('正在启动 Siglus WASM…');await init();canvas.focus();await start_siglus_from_directory('siglus-canvas',JSON.stringify(meta.entries.map(f=>({path:f.path,size:f.size,lastModified:0}))));setStatus('运行中（SiglusEngine）');}catch(e){fail(e);setStatus('启动失败')}}
start();canvas.addEventListener('contextmenu',e=>e.preventDefault());canvas.addEventListener('wheel',e=>e.preventDefault(),{passive:false});

for(const type of ['keydown','keyup'])canvas.addEventListener(type,e=>{if(e.code==='F11'||e.code==='F12'||e.key==='F11'||e.key==='F12'||((e.ctrlKey||e.metaKey||e.altKey)&&!['Control','Alt','Meta','Shift'].includes(e.key)))e.stopImmediatePropagation()},{capture:true});

setInterval(()=>{if(globalThis.__siglusEnded)setStatus("游戏运行已结束（详情见兼容性记录）"},500);
