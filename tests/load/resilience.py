#!/usr/bin/env python3
"""Disposable personal-library load, interrupted-upload and restart validation."""
import argparse
import concurrent.futures
import hashlib
import http.cookiejar
import json
import os
import resource
from pathlib import Path
import signal
import socket
import sqlite3
import statistics
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

ROOT = "00000000-0000-0000-0000-000000000000"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/revaro")
    parser.add_argument("--files", type=int, default=100_000)
    parser.add_argument("--requests", type=int, default=1000)
    parser.add_argument("--workers", type=int, default=16)
    parser.add_argument("--max-p95-ms", type=float, default=3000,
                        help="latency acceptance limit for each mixed request path")
    parser.add_argument("--overload-workers", type=int, default=192,
                        help="simultaneous requests used to check bounded rejection and recovery; 0 disables")
    parser.add_argument("--output", default="/tmp/revaro-resilience-load.json")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="revaro-load-") as directory:
        directory = Path(directory)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        env = os.environ.copy()
        for key in ("APP_DOMAIN", "APP_QUIC_ADDR", "APP_TLS_ADDR", "APP_HTTP2_ORIGIN",
                    "ACME_DIRECTORY_URL", "UPLOAD_MIN_FREE_BYTES"):
            env.pop(key, None)
        env.update(APP_ADDR=f"127.0.0.1:{port}", APP_BASE_URL=origin,
                   APP_DATA_DIR=str(directory/"data"), APP_OBJECTS_DIR=str(directory/"objects"),
                   APP_CACHES_DIR=str(directory/"cache"), ADMIN_USERNAME="admin",
                   ADMIN_PASSWORD="temporary-load-check", COOKIE_SECURE="false", GC_INTERVAL="0")
        process = None
        log = (directory/"server.log").open("w")
        jar = http.cookiejar.CookieJar()
        client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

        def request(path, method="GET", data=None, extra=None, authenticated=True):
            headers = {"Origin": origin, **(extra or {})}
            if data is not None and not isinstance(data, bytes):
                data = json.dumps(data).encode()
                headers["Content-Type"] = "application/json"
            req = urllib.request.Request(origin+path, data=data, method=method, headers=headers)
            open_request = client.open if authenticated else urllib.request.urlopen
            with open_request(req, timeout=30) as response:
                return response.status, response.read(), response.headers

        def start():
            nonlocal process
            process = subprocess.Popen([str(Path(args.binary).resolve())], env=env, stdout=log, stderr=log)
            deadline = time.monotonic()+30
            while time.monotonic()<deadline:
                if process.poll() is not None:
                    raise RuntimeError((directory/"server.log").read_text())
                try:
                    request("/readyz", authenticated=False)
                    return
                except (OSError, urllib.error.URLError):
                    time.sleep(.05)
            raise TimeoutError("server readiness")

        def stop(crash=False):
            if process is not None and process.poll() is None:
                process.send_signal(signal.SIGKILL if crash else signal.SIGTERM)
                process.wait(timeout=15)

        results = {"files": args.files, "workers": args.workers, "requests": args.requests,
                   "max_p95_ms": args.max_p95_ms,
                   "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest()}
        try:
            start(); stop()
            db = sqlite3.connect(directory/"data/revaro.db")
            db.execute("PRAGMA foreign_keys=ON")
            folders = [str(uuid.uuid4()) for _ in range(100)]
            stamp = "2026-10-09T00:00:00Z"
            insert = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)"
            db.executemany(insert, [(key, ROOT, f"folder-{i:03}", "directory", None, 0, "", "", "ready", stamp, stamp) for i,key in enumerate(folders)])
            # Metadata-only scale fixtures; real byte transfers are validated below.
            db.executemany(insert, ((str(uuid.uuid4()),folders[i%100],f"image-{i:08}.png","file",str(uuid.uuid4()),128,"image/png",f"etag-{i}","ready",stamp,stamp) for i in range(args.files)))
            db.commit(); db.close()
            start()
            request("/api/auth/login", "POST", {"username":"admin","password":"temporary-load-check"})
            cookie = "; ".join(f"{c.name}={c.value}" for c in jar)
            paths = [f"/api/files?parent_id={folders[0]}&limit=100", "/api/library/items?kind=image&limit=60", "/api/library/items?recent=true&opened_only=true", "/api/files?q=image-00000&limit=100", "/api/shares", "/readyz"]
            failures = []
            def load(index):
                path = paths[index % len(paths)]
                started = time.monotonic()
                try:
                    _,body,_=request(path, extra={"Cookie":cookie}, authenticated=False)
                    if path==paths[0]:
                        listing=json.loads(body)
                        assert listing["total"]==(args.files+99)//100
                        assert len(listing["items"])==min(100,listing["total"])
                    elif path==paths[1]:
                        listing=json.loads(body)
                        assert listing["total"]==args.files
                        assert len(listing["items"])==min(60,args.files)
                    elif path==paths[2]:
                        listing=json.loads(body)
                        assert listing["total"]==0 and not listing["items"]
                    return path, (time.monotonic()-started)*1000
                except Exception as error:
                    failures.append({"path":path,"error":str(error)})
                    return path, (time.monotonic()-started)*1000
            with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as executor:
                samples=list(executor.map(load,range(args.requests)))
            results["failures"] = failures
            results["latencies"] = {}
            for path in paths:
                times=sorted(ms for p,ms in samples if p==path)
                if times:
                    results["latencies"][path]={"p50_ms":round(statistics.median(times),2),"p95_ms":round(times[min(len(times)-1,int(len(times)*.95))],2),"max_ms":round(max(times),2)}
            Path(args.output).write_text(json.dumps(results,ensure_ascii=False,indent=2)+"\n")
            print(json.dumps(results["latencies"],ensure_ascii=False),flush=True)
            if args.overload_workers:
                barrier=threading.Barrier(args.overload_workers)
                def pressure(index):
                    barrier.wait(timeout=30)
                    try:
                        status,_,_=request(paths[1],extra={"Cookie":cookie},authenticated=False)
                        return status
                    except urllib.error.HTTPError as error:
                        if error.code==503:
                            assert error.headers.get("Retry-After")=="2"
                        return error.code
                with concurrent.futures.ThreadPoolExecutor(max_workers=args.overload_workers) as executor:
                    statuses=list(executor.map(pressure,range(args.overload_workers)))
                results["overload_statuses"]={str(status):statuses.count(status) for status in sorted(set(statuses))}
                assert all(status in (200,503) for status in statuses), "overload produced an unexpected error or signed the user out"
                assert request("/api/auth/me")[0]==200, "the session did not recover after pressure"
            # One acknowledged part must survive abrupt process death. The final
            # content and directory stats must match after completion and restart.
            payload = bytes(range(256))*32768
            _, body, _=request("/api/uploads","POST",{"parent_id":ROOT,"name":"restart.bin","size":len(payload),"mime_type":"application/octet-stream","idempotency_key":str(uuid.uuid4())})
            upload=json.loads(body)
            part_size=upload["part_size"]
            first=payload[:part_size]
            request(f'/api/uploads/{upload["upload_id"]}/data/1',"PUT",first,{"X-Content-SHA256":hashlib.sha256(first).hexdigest()})
            stop(crash=True);start()
            _, body, _=request(f'/api/uploads/{upload["upload_id"]}')
            resumed=json.loads(body)
            results["resumed_parts"]=resumed.get("completed_parts",resumed.get("parts",[]))
            assert len(results["resumed_parts"])==1
            # Simulate exhausted storage without filling the host disk. Refused
            # writes must leave the durable first part available for a retry.
            stop();env["UPLOAD_MIN_FREE_BYTES"]="9223372036854775807";start()
            second=payload[part_size:part_size*2]
            try:
                request(f'/api/uploads/{upload["upload_id"]}/data/2',"PUT",second,{"X-Content-SHA256":hashlib.sha256(second).hexdigest()})
                raise AssertionError("write was accepted with no admitted free space")
            except urllib.error.HTTPError as error:
                results["storage_pressure_status"]=error.code
                assert error.code==507
            stop();env.pop("UPLOAD_MIN_FREE_BYTES");start()
            # Resending an already acknowledged part is also safe.
            for part in range(1,upload["part_count"]+1):
                data=payload[(part-1)*part_size:part*part_size]
                request(f'/api/uploads/{upload["upload_id"]}/data/{part}',"PUT",data,{"X-Content-SHA256":hashlib.sha256(data).hexdigest()})
            _, body, _=request(f'/api/uploads/{upload["upload_id"]}/complete',"POST",{"parts":[]})
            file=json.loads(body)
            def transfer(index):
                _, body, _=request(f'/api/files/{file["id"]}/download', extra={"Cookie":cookie}, authenticated=False)
                return hashlib.sha256(body).hexdigest()==hashlib.sha256(payload).hexdigest()
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
                results["concurrent_downloads_valid"]=all(executor.map(transfer,range(16)))
            stop(crash=True);start()
            _, downloaded, _=request(f'/api/files/{file["id"]}/download')
            results["completed_upload_survives_restart"]=hashlib.sha256(downloaded).digest()==hashlib.sha256(payload).digest()
            db=sqlite3.connect(directory/"data/revaro.db")
            results["integrity_check"]=db.execute("PRAGMA integrity_check").fetchone()[0]
            results["foreign_key_violations"]=db.execute("PRAGMA foreign_key_check").fetchall()
            db.close()
            stop()
            results["peak_server_rss_kib"]=resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
            results["logical_cpus"]=os.cpu_count()
            Path(args.output).write_text(json.dumps(results,ensure_ascii=False,indent=2)+"\n")
            print(json.dumps(results,ensure_ascii=False,indent=2))
            assert not failures and results["concurrent_downloads_valid"] and results["completed_upload_survives_restart"]
            assert results["integrity_check"]=="ok" and not results["foreign_key_violations"]
            assert all(t["p95_ms"]<=args.max_p95_ms for t in results["latencies"].values()), "latency acceptance limit exceeded"
        finally:
            stop();log.close()


if __name__ == "__main__":
    main()
