#!/usr/bin/env python3
"""Isolated real HTTP/SQLite benchmark. Never modifies an existing installation."""
import argparse, http.cookiejar, json, os, pathlib, socket, sqlite3, statistics, subprocess, tempfile, time, urllib.request
ROOT = '00000000-0000-0000-0000-000000000000'
p = argparse.ArgumentParser()
p.add_argument('--binary', required=True)
p.add_argument('--output', required=True)
p.add_argument('--batch', action='store_true')
p.add_argument('--latency-ms', type=float, default=0)
a = p.parse_args()
with tempfile.TemporaryDirectory(prefix='revaro-performance-') as tmp:
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    env = dict(os.environ, APP_ADDR=f'127.0.0.1:{port}', APP_BASE_URL=base, APP_DATA_DIR=tmp+'/db', APP_OBJECTS_DIR=tmp+'/objects', APP_CACHES_DIR=tmp+'/caches', APP_WEB_DIR='/nonexistent', ADMIN_USERNAME='admin', ADMIN_PASSWORD='performance-fixture-password')
    with open(tmp+'/server.log', 'w') as log:
        server = subprocess.Popen([os.path.abspath(a.binary)], env=env, stdout=log, stderr=log)
        try:
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            def request(path, method='GET', data=None):
                body = None if data is None else json.dumps(data).encode()
                req = urllib.request.Request(base+path, data=body, method=method, headers={'Origin':base,'Content-Type':'application/json'})
                if a.latency_ms: time.sleep(a.latency_ms/1000)
                with opener.open(req, timeout=120) as response:
                    raw = response.read()
                    return json.loads(raw) if raw else None
            for _ in range(300):
                if server.poll() is not None: raise RuntimeError(pathlib.Path(tmp+'/server.log').read_text())
                try: request('/healthz'); break
                except OSError: time.sleep(.1)
            request('/api/auth/login', 'POST', {'username':'admin','password':'performance-fixture-password','second_factor':''})
            database = next(pathlib.Path(tmp+'/db').glob('*.db'))
            with sqlite3.connect(database) as db:
                db.executemany("INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,'file',?,20,'text/plain','bench','ready','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')", [(f'00000000-0000-4000-8000-{i:012d}',ROOT,f'Book {i:05d}.txt',f'benchmark/{i}') for i in range(10000)])
                db.execute("UPDATE library_items SET metadata_etag='bench'")
            ids = [f'00000000-0000-4000-8000-{i:012d}' for i in range(5000)]
            paths = {'directory':'/api/files?parent_id='+ROOT, 'books':'/api/library/items?kind=book&group_stacks=true', 'search':'/api/files?q=Book&parent_id='+ROOT}
            result = {'files':10000, 'shelf_members':200, 'stack_members':5000, 'batch':a.batch, 'injected_request_latency_ms':a.latency_ms, 'reads':{}}
            def elapsed(fn):
                start = time.perf_counter(); fn(); return round((time.perf_counter()-start)*1000,3)
            for name,path in paths.items():
                cold = elapsed(lambda:request(path))
                samples = [elapsed(lambda:request(path)) for _ in range(20)]
                result['reads'][name] = {'cold_ms':cold,'warm_median_ms':round(statistics.median(samples),3),'warm_p95_ms':sorted(samples)[18]}
            def shelf():
                write_start = time.perf_counter()
                c = request('/api/library/collections','POST',{'name':'Benchmark shelf','kind':'book', **({'file_ids':ids[:200]} if a.batch else {})})
                if not a.batch:
                    for id in ids[:200]: request(f"/api/library/collections/{c['id']}/items/{id}",'PUT')
                result['shelf_write_ms'] = round((time.perf_counter()-write_start)*1000,3)
                read_start = time.perf_counter()
                listing = request('/api/library/items?kind=book&collection='+c['id'])
                result['shelf_read_ms'] = round((time.perf_counter()-read_start)*1000,3)
                assert listing['total'] == 200
            result['create_shelf_with_200_ms'] = elapsed(shelf)
            result['create_stack_5000_ms'] = elapsed(lambda:request('/api/library/stacks','POST',{'name':'Benchmark stack','file_ids':ids}))
            result['list_stacks_ms'] = elapsed(lambda:request('/api/library/stacks'))
            result['system'] = request('/api/system/status')
            pathlib.Path(a.output).write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
            print(json.dumps({k:v for k,v in result.items() if k != 'system'},indent=2),flush=True)
        finally:
            server.terminate()
            try: server.wait(timeout=10)
            except subprocess.TimeoutExpired: server.kill(); server.wait()
