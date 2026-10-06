#!/usr/bin/env python3
"""Measure the real Revaro HTTP/3 endpoint inside a disposable network namespace.

Requires Linux user namespaces, tc/ip, OpenSSL and a curl build with HTTP/3.
Never changes the host qdisc. Results include verified payload prefixes, per-second
goodput and raw tc counters; intentional timed cancellation is not a full download.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import ssl
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/revaro")
    parser.add_argument("--web", default="dist/web")
    parser.add_argument("--output", required=True)
    parser.add_argument("--seconds", type=int, default=15)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--loss", default="0,5,10,15")
    parser.add_argument("--modes", default="standard,aggressive")
    parser.add_argument("--target", default="auto", help="auto or fixed target Mbps")
    parser.add_argument("--line-mbps", type=int, default=20)
    parser.add_argument("--jitter-ms", type=int, default=10)
    parser.add_argument("--max-mbps", type=int, default=250)
    parser.add_argument("--global-max-mbps", type=int, default=1000)
    parser.add_argument("--payload-mib", type=int, default=64)
    parser.add_argument("--capacity-change", action="store_true", help="drop capacity to 1/4 at 5s; restore at 12s")
    parser.add_argument("--recovery", action="store_true")
    parser.add_argument("--browser", action="store_true", help="also verify Chromium Range recovery and h2 fallback")
    parser.add_argument("--namespace", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.namespace:
        env = os.environ.copy()
        env["REVARO_BENCH_PARENT_NS"] = os.readlink("/proc/self/ns/net")
        os.execvpe("unshare", ["unshare", "-Urn", sys.executable,
                    str(Path(__file__).resolve()), *sys.argv[1:], "--namespace"], env)
    if (os.geteuid() != 0 or not os.environ.get("REVARO_BENCH_PARENT_NS")
            or os.readlink("/proc/self/ns/net") == os.environ["REVARO_BENCH_PARENT_NS"]):
        raise SystemExit("Refusing to change qdisc outside the isolated benchmark namespace")
    tc = shutil.which("tc") or "/usr/sbin/tc"
    if "HTTP3" not in run("curl", "--version").stdout.decode():
        raise SystemExit("curl must support HTTP3")
    run("ip", "link", "set", "lo", "up")
    binary, web = str(Path(args.binary).resolve()), str(Path(args.web).resolve())
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="revaro-netem-"))
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(work / "key.pem"), "-out", str(work / "cert.pem"), "-days", "2",
        "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1")
    origin = "https://localhost:18443"
    opener = urllib.request.build_opener(urllib.request.HTTPSHandler(
        context=ssl._create_unverified_context()))
    cookie = ""

    def api(path, data=None, method=None, headers=None):
        h = {"Origin": origin, "Cookie": cookie, **(headers or {})}
        if isinstance(data, dict):
            data = json.dumps(data).encode()
            h["Content-Type"] = "application/json"
        request = urllib.request.Request(origin + path, data=data, headers=h, method=method)
        with opener.open(request, timeout=30) as response:
            raw = response.read()
            return response.headers, json.loads(raw) if raw else None

    # A generic binary file, not a media fixture. The same exact object is used
    # across every mode/loss cell. This pattern lets us validate timed prefixes.
    payload = bytes(range(256)) * (args.payload_mib * 1024 * 1024 // 256)
    results = []
    file_id = None
    server = None
    settings = dict(rtt_ms=120, jitter_per_direction_ms=args.jitter_ms, line_mbps=args.line_mbps,
                    target_mbps=args.target, max_mbps=args.max_mbps, global_max_mbps=args.global_max_mbps,
                    max_compensation_percent=125, seconds=args.seconds,
                    queue_limit=max(1000, args.line_mbps * 125_000 * 24 // 100 // 1200),
                    note="netem affects both directions on isolated loopback; timed prefix transfers")

    def netem(loss, rate=None):
        run(tc, "qdisc", "replace", "dev", "lo", "root", "netem", "delay", "60ms", f"{args.jitter_ms}ms",
            "loss", f"{loss}%", "rate", f"{rate or args.line_mbps}mbit", "limit", str(settings["queue_limit"]))

    def measure(mode, loss, repetition, dropout=False, capacity=False):
        netem(loss)
        name = f"{mode}-{loss}-{repetition}" + ("-dropout" if dropout else "-capacity" if capacity else "")
        headers_path = output / (name + ".headers")
        curl = subprocess.Popen(["curl", "-4ksS", "--http3-only", "--no-buffer",
            "--max-time", str(args.seconds), "-D", str(headers_path),
            "-H", "Cookie: " + cookie, "-H", "Range: bytes=0-",
            origin + f"/api/files/{file_id}/download"], stdout=subprocess.PIPE,
            stderr=subprocess.PIPE)
        sel = selectors.DefaultSelector()
        sel.register(curl.stdout, selectors.EVENT_READ)
        started = time.monotonic()
        received, bins, gaps, first, last, verified = 0, [0] * args.seconds, [], None, None, True
        digest = hashlib.sha256()
        outage, restored = False, False
        first_after_restore = None
        while time.monotonic() - started < args.seconds + 1:
            elapsed = time.monotonic() - started
            if capacity and elapsed >= 5 and not outage:
                netem(loss, max(1, args.line_mbps // 4))
                outage = True
            if capacity and elapsed >= 12 and not restored:
                netem(loss)
                restored = True
            if dropout and elapsed >= 5 and not outage:
                netem(100)
                outage = True
            if dropout and elapsed >= 7 and not restored:
                netem(loss)
                restored = True
            if not sel.select(0.05):
                if curl.poll() is not None:
                    break
                continue
            block = os.read(curl.stdout.fileno(), 64 * 1024)
            if not block:
                break
            now = time.monotonic() - started
            if dropout and restored and first_after_restore is None:
                first_after_restore = now
            verified = verified and block == payload[received:received + len(block)]
            digest.update(block)
            received += len(block)
            bins[min(int(now), args.seconds - 1)] += len(block)
            if first is None:
                first = now
            if last is not None:
                gaps.append(now - last)
            last = now
        elapsed = min(time.monotonic() - started, args.seconds)
        if curl.poll() is None:
            curl.terminate()
        _, stderr = curl.communicate(timeout=5)
        sel.close()
        tc_stats = json.loads(run(tc, "-s", "-j", "qdisc", "show", "dev", "lo").stdout)
        run(tc, "qdisc", "del", "dev", "lo", "root")
        headers = headers_path.read_text()
        if not verified or "HTTP/3 206" not in headers or not received:
            raise RuntimeError(f"invalid HTTP/3 transfer {name}: {headers}; {stderr.decode()}")
        rates = [b * 8 / 1e6 for b in bins]
        steady = rates[3:]
        if received == len(payload):
            # Do not average zeros after a successful early full transfer.
            steady = rates[3:int(elapsed)] or [received * 8 / elapsed / 1e6]
        row = dict(mode=mode, loss_percent=loss, repetition=repetition, dropout=dropout, capacity_change=capacity,
                   bytes=received, seconds=elapsed, first_byte_seconds=first,
                   goodput_mbps=received * 8 / elapsed / 1e6,
                   steady_mbps=statistics.mean(steady), per_second_mbps=rates,
                   max_progress_gap_seconds=max(gaps, default=0), prefix_verified=verified,
                   terminal_silence_seconds=max(0, elapsed - (last or 0)),
                   resume_after_outage_seconds=(first_after_restore - 7 if first_after_restore is not None else None),
                   prefix_sha256=digest.hexdigest(), tc=tc_stats, curl_exit=curl.returncode,
                   curl_stderr=stderr.decode())
        results.append(row)
        (output / "results.json").write_text(json.dumps(dict(settings=settings, samples=results), indent=2))
        print(json.dumps({k: row[k] for k in ["mode", "loss_percent", "repetition", "dropout",
              "steady_mbps", "max_progress_gap_seconds", "prefix_verified"]}), flush=True)
        time.sleep(0.2)

    try:
        for mode in args.modes.split(","):
            env = os.environ.copy()
            env.update(APP_ADDR="127.0.0.1:18081", APP_BASE_URL=origin,
                APP_TLS_ADDR="127.0.0.1:18443", APP_QUIC_ADDR="127.0.0.1:18443",
                APP_HTTP2_ADDR="127.0.0.1:18444", APP_HTTP2_BASE_URL="https://localhost:18444",
                APP_TLS_CERT=str(work / "cert.pem"), APP_TLS_KEY=str(work / "key.pem"),
                APP_DATA_DIR=str(work / "data"), APP_OBJECTS_DIR=str(work / "objects"),
                APP_CACHES_DIR=str(work / "caches"), APP_WEB_DIR=web,
                ADMIN_USERNAME="admin", ADMIN_PASSWORD="quic-benchmark-password",
                QUIC_CC_MODE=mode, QUIC_TARGET_MBPS=args.target, QUIC_MAX_MBPS=str(args.max_mbps),
                QUIC_GLOBAL_MAX_MBPS=str(args.global_max_mbps), QUIC_MAX_COMPENSATION_PERCENT="125")
            with (output / (mode + ".server.log")).open("w") as log:
                def boot_server():
                    process = subprocess.Popen([binary], env=env, stdout=log, stderr=subprocess.STDOUT)
                    for _ in range(100):
                        if process.poll() is not None:
                            raise RuntimeError("benchmark server exited; inspect server log")
                        try:
                            api("/readyz")
                            return process
                        except OSError:
                            time.sleep(0.1)
                    process.terminate()
                    process.wait(timeout=15)
                    raise RuntimeError("benchmark server not ready")
                server = boot_server()
                if not cookie:
                    headers, _ = api("/api/auth/login", dict(username="admin", password="quic-benchmark-password"))
                    cookie = headers["Set-Cookie"].split(";")[0]
                if file_id is None:
                    _, upload = api("/api/uploads", dict(parent_id="00000000-0000-0000-0000-000000000000",
                        name="quic-generic.dat", size=len(payload), mime_type="application/octet-stream"))
                    file_id = upload["file_id"]
                    parts = []
                    for start in range(0, len(payload), upload["part_size"]):
                        number = start // upload["part_size"] + 1
                        block = payload[start:start + upload["part_size"]]
                        headers, _ = api(f'/api/uploads/{upload["upload_id"]}/data/{number}', block, "PUT",
                            {"X-Content-SHA256": hashlib.sha256(block).hexdigest()})
                        parts.append(dict(part_number=number, etag=headers["ETag"]))
                    api(f'/api/uploads/{upload["upload_id"]}/complete', dict(parts=parts))
                for loss in map(int, args.loss.split(",")):
                    for repetition in range(1, args.repeats + 1):
                        measure(mode, loss, repetition)
                        # Cancelled large streams can leave packets in flight.
                        # Stop the old endpoint before another sample so neither
                        # old PTO probes nor old file bytes compete on netem.
                        server.send_signal(signal.SIGTERM)
                        server.wait(timeout=15)
                        server = boot_server()
                if args.recovery:
                    measure(mode, 5, 1, dropout=True)
                if args.capacity_change:
                    if args.recovery:
                        server.send_signal(signal.SIGTERM)
                        server.wait(timeout=15)
                        server = boot_server()
                    measure(mode, 5, 1, capacity=True)
                if args.browser and mode == "aggressive":
                    browser_env = os.environ.copy()
                    browser_env.update(REVARO_QUIC_CERT=str(work / "cert.pem"), REVARO_QUIC_FORCE="1",
                        REVARO_QUIC_RESULT=str(output / "browser.json"),
                        PW_EXPERIMENTAL_SERVICE_WORKER_NETWORK_EVENTS="1")
                    subprocess.run([shutil.which("node"), str(Path(__file__).with_name("browser.mjs")),
                        "--netem", "--block-udp"], env=browser_env, check=True, timeout=180)
                server.send_signal(signal.SIGTERM)
                server.wait(timeout=15)
                server = None
    finally:
        if server is not None:
            server.terminate()
            try:
                server.wait(timeout=15)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        subprocess.run([tc, "qdisc", "del", "dev", "lo", "root"], capture_output=True)
        shutil.rmtree(work)


if __name__ == "__main__":
    main()
