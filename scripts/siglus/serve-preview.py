#!/usr/bin/env python3
"""Loopback-only compatibility harness. Not a Revaro share or production server."""
from pathlib import Path
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler
import zipfile,struct,json,secrets,mimetypes,hashlib,urllib.parse,re,argparse
from http.cookies import SimpleCookie, CookieError
ROOT=Path(__file__).resolve().parent
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--archive',type=Path,required=True)
parser.add_argument('--runtime-dir',type=Path,required=True)
parser.add_argument('--key-config',type=Path)
parser.add_argument('--port',type=int,default=18081)
args=parser.parse_args()
ZIP=args.archive.resolve(strict=True)
RUNTIME=args.runtime_dir.resolve(strict=True)
ASSETS=ROOT/'prototype'
PORT=args.port
SECRET=secrets.token_hex(24)
entries=[]
with zipfile.ZipFile(ZIP) as z,ZIP.open('rb') as f:
 names=[i.filename for i in z.infolist() if not i.is_dir()]
 strip_root=bool(names) and all('/' in n for n in names) and len({n.split('/',1)[0] for n in names})==1
 seen=set()
 for i in z.infolist():
  if i.is_dir():continue
  name=i.filename.split('/',1)[1] if strip_root else i.filename
  if '\\' in name or '\0' in name or any(part in ('','..','.') for part in name.split('/')):raise ValueError('invalid game path')
  if name.lower() in seen:raise ValueError('duplicate game path')
  seen.add(name.lower())
  if i.compress_type not in (0,8) or i.flag_bits&1:raise ValueError('unsupported ZIP method/encryption')
  f.seek(i.header_offset);h=f.read(30)
  if h[:4]!=b'PK\x03\x04':raise ValueError('invalid local header')
  a,b=struct.unpack_from('<HH',h,26)
  entries.append({'path':name,'size':i.file_size,'packed':i.compress_size,'offset':i.header_offset+30+a+b,'method':i.compress_type,'crc':i.CRC})
identity=hashlib.sha256((str(ZIP)+':'+str(ZIP.stat().st_size)+':'+str(ZIP.stat().st_mtime_ns)).encode()).hexdigest()
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_HEAD(self):self.serve(True)
 def do_GET(self):self.serve(False)
 def send(self,status,data=b'',mime='application/json',extra=None,head=False):
  self.send_response(status)
  for k,v in {'Content-Type':mime,'Content-Length':str(len(data)),'Cache-Control':'no-store','Cross-Origin-Opener-Policy':'same-origin','Cross-Origin-Embedder-Policy':'require-corp','Cross-Origin-Resource-Policy':'same-origin','X-Content-Type-Options':'nosniff','X-Frame-Options':'DENY','Referrer-Policy':'no-referrer','Content-Security-Policy':"default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; worker-src 'self' blob:; object-src 'none'; base-uri 'none'",**(extra or {})}.items():self.send_header(k,v)
  self.end_headers()
  if not head:self.wfile.write(data)
 def serve(self,head):
  if self.headers.get('Host') not in (f'localhost:{PORT}',f'127.0.0.1:{PORT}'):return self.send(403,b'{}',head=head)
  p=urllib.parse.urlsplit(self.path).path
  if p=='/':return self.send(200,(ASSETS/'prototype.html').read_bytes(),'text/html; charset=utf-8',{'Set-Cookie':f'siglus_probe={SECRET}; HttpOnly; SameSite=Strict; Path=/'},head)
  if p in ('/game-index','/archive','/probe-key'):
   try:
    cookie=SimpleCookie(self.headers.get('Cookie',''));token=cookie.get('siglus_probe')
   except CookieError:
    token=None
   if token is None or not secrets.compare_digest(token.value,SECRET):return self.send(403,b'{}',head=head)
  if p=='/game-index':return self.send(200,json.dumps({'entries':entries,'save_space':'revaro-siglus-prototype-'+identity,'name':ZIP.name},ensure_ascii=False).encode(),head=head)
  if p=='/probe-key':
   key=args.key_config
   if key is None or not key.is_file():return self.send(404,b'{}',head=head)
   return self.send(200,key.read_bytes(),'text/plain; charset=utf-8',head=head)
  if p=='/archive':
   size=ZIP.stat().st_size
   if head:return self.send(200,b'',mime='application/zip',extra={'Content-Length':str(size),'Accept-Ranges':'bytes'},head=True)
   match=re.fullmatch(r'bytes=(\d+)-(\d+)',self.headers.get('Range',''))
   if not match:return self.send(400,b'{"error":"Range required"}')
   start,end=map(int,match.groups())
   if start>end or end>=size or end-start+1>512*1024*1024:return self.send(416,b'',extra={'Content-Range':f'bytes */{size}'})
   self.send_response(206)
   for k,v in {'Content-Type':'application/zip','Content-Length':str(end-start+1),'Accept-Ranges':'bytes','Content-Range':f'bytes {start}-{end}/{size}','Cache-Control':'no-store','Cross-Origin-Resource-Policy':'same-origin','X-Content-Type-Options':'nosniff'}.items():self.send_header(k,v)
   self.end_headers()
   try:
    with ZIP.open('rb') as f:
     f.seek(start);remaining=end-start+1
     while remaining:
      data=f.read(min(remaining,1024*1024))
      if not data:raise EOFError('archive changed during request')
      self.wfile.write(data);remaining-=len(data)
   except (BrokenPipeError,ConnectionResetError,EOFError):self.close_connection=True
   return
  allowed={'/prototype.js','/fflate.js','/FFLATE-LICENSE.txt','/pkg/ENGINE-LICENSE.txt','/pkg/game_launcher.js','/pkg/game_launcher_bg.wasm'}
  if p not in allowed:return self.send(404,b'{}',head=head)
  target=(RUNTIME/p.removeprefix('/pkg/')) if p.startswith('/pkg/') else ASSETS/p.lstrip('/')
  return self.send(200,target.read_bytes(),mimetypes.guess_type(target.name)[0] or 'application/octet-stream',head=head)
print(f'Compatibility harness http://localhost:{PORT} ; original ZIP is read-only',flush=True)
ThreadingHTTPServer(('127.0.0.1',PORT),Handler).serve_forever()
