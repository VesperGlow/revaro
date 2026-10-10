#!/usr/bin/env python3
"""Read-only public H2/H3 acceptance. Never substitutes a local endpoint."""
import argparse
import datetime as dt
import hashlib
import ipaddress
import json
import random
import socket
import statistics
import subprocess
import tempfile
import time
from pathlib import Path
from urllib.parse import urlsplit


def public_url(value):
    parsed = urlsplit(value)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError("A public HTTPS URL without embedded credentials is required")
    addresses = sorted({entry[4][0] for entry in socket.getaddrinfo(parsed.hostname, parsed.port or 443, type=socket.SOCK_STREAM)})
    if not addresses or any(not ipaddress.ip_address(address).is_global for address in addresses):
        raise ValueError("Refusing a loopback, private, or non-public endpoint")
    return addresses


def stamp():
    now = dt.datetime.now(dt.timezone.utc)
    beijing = now.astimezone(dt.timezone(dt.timedelta(hours=8)))
    return {"utc": now.isoformat(), "beijing": beijing.isoformat(), "inBeijingPeakWindow": 18 <= beijing.hour < 24}


def request(url, protocol, cookie, *, method="GET", range_value=None, headers=(), seconds=12, connect_address=None):
    begun = time.monotonic()
    sample = {**stamp(), "requestedProtocol": protocol, "method": method, "range": range_value}
    with tempfile.TemporaryDirectory(prefix="revaro-public-probe-") as directory:
        header_path = str(Path(directory) / "headers")
        command = ["curl", "--http3-only" if protocol == "h3" else "--http2", "--silent", "--show-error",
                   "--connect-timeout", "5", "--max-time", str(seconds), "--header", "Accept-Encoding: identity",
                   "--dump-header", header_path, "--write-out", "%{stderr}\n%{json}\n"]
        if cookie:
            command += ["--cookie", cookie]
        if connect_address:
            parsed = urlsplit(url)
            address = f"[{connect_address}]" if ":" in connect_address else connect_address
            command += ["--resolve", f"{parsed.hostname}:{parsed.port or 443}:{address}"]
        if method == "HEAD":
            command += ["--head", "--output", "/dev/null"]
        if range_value is not None:
            command += ["--range", range_value]
            start, separator, end = range_value.partition("-")
            if separator and start.isdigit() and end.isdigit() and int(end) >= int(start):
                # A backend ignoring Range must not turn an 8 MiB probe into
                # a multi-gigabyte download. Unknown-length bodies are capped too.
                command += ["--max-filesize", str(int(end) - int(start) + 1)]
        for header in headers:
            command += ["--header", header]
        process = subprocess.Popen(command + [url], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        received, first, last, largest_gap = 0, None, None, 0
        bins, digest = {}, hashlib.sha256()
        while chunk := process.stdout.read1(65536):
            now = time.monotonic()
            first = first if first is not None else now - begun
            if last is not None:
                largest_gap = max(largest_gap, now - last)
            last = now
            second = int(now - begun)
            bins[second] = bins.get(second, 0) + len(chunk)
            received += len(chunk)
            digest.update(chunk)
        error = process.stderr.read().decode()
        process.wait()
        metadata = next((json.loads(line) for line in reversed(error.splitlines()) if line.startswith("{")), {})
        allowed = ("http_version", "http_code", "remote_ip", "remote_port", "time_namelookup", "time_connect",
                   "time_appconnect", "time_starttransfer", "time_total", "size_download", "speed_download", "exitcode", "num_connects")
        sample.update({key: metadata.get(key) for key in allowed})
        selected = {}
        for line in Path(header_path).read_text().splitlines():
            key, separator, value = line.partition(":")
            if separator and key.lower() in ("alt-svc", "server", "via", "content-range", "content-length", "etag", "cache-control", "content-encoding", "accept-ranges", "x-accel-buffering"):
                selected[key.lower()] = value.strip()
        elapsed = time.monotonic() - begun
        sample.update({"headers": selected, "bodyBytes": received, "sha256": digest.hexdigest() if received else None,
                       "firstBodyByteSeconds": first, "largestBodyGapSeconds": largest_gap,
                       "observedMbps": received * 8 / max(elapsed, 0.001) / 1e6, "bytesPerSecond": bins,
                       "protocolVerified": metadata.get("http_version") == ("3" if protocol == "h3" else "2")})
        return sample


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--file-url")
    parser.add_argument("--cookie-jar")
    parser.add_argument("--output", required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--prefix-seconds", type=int, default=8)
    parser.add_argument("--connect-address", help="Public origin IP diagnostic; preserves HTTPS host/SNI and certificate validation")
    args = parser.parse_args()
    if args.repeats < 1 or args.prefix_seconds < 1:
        parser.error("repeats and prefix-seconds must be positive")
    addresses = public_url(args.url)
    if args.connect_address and not ipaddress.ip_address(args.connect_address).is_global:
        raise ValueError("Diagnostic connection address must be public")
    if args.file_url:
        public_url(args.file_url)
        if urlsplit(args.file_url).netloc != urlsplit(args.url).netloc:
            raise ValueError("File probe must use the same origin")
    report = {"scope": "public Internet, current client egress; no local server or network emulation", "target": args.url,
              "dnsAddresses": addresses, "client": subprocess.check_output(["curl", "--version"], text=True).splitlines()[0],
              "diagnosticConnectAddress": args.connect_address,
              "peakWindow": "Asia/Shanghai 18:00-24:00", "clientGeographicLocation": "unverified",
              "started": stamp(), "negotiation": [], "boundedRanges": [], "randomRanges": [], "continuousRange": [],
              "notes": ["HTTP/3 is strict and never silently counted after H2 fallback.",
                        "A timed continuous prefix deliberately cancels a large open-ended range; it is not a complete-file success.",
                        "Observed body gaps alone cannot identify Nginx buffering without deployment-side evidence."]}
    destination = Path(args.output)
    destination.parent.mkdir(parents=True, exist_ok=True)
    def save():
        destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    def probe(*arguments, **options):
        return request(*arguments, connect_address=args.connect_address, **options)
    for repeat in range(args.repeats):
        for protocol in (["h2", "h3"] if repeat % 2 == 0 else ["h3", "h2"]):
            sample = probe(args.url, protocol, None)
            report["negotiation"].append(sample)
            print(json.dumps({"stage": "negotiation", "protocol": protocol, "actual": sample["http_version"], "status": sample["http_code"], "exit": sample["exitcode"]}), flush=True)
            save()
    if args.file_url:
        head = probe(args.file_url, "h2", args.cookie_jar, method="HEAD")
        report["fileHead"] = head
        size = int(head["headers"].get("content-length", 0))
        etag = head["headers"].get("etag")
        enabled = [protocol for protocol in ("h2", "h3") if any(s["requestedProtocol"] == protocol and s["protocolVerified"] and s["http_code"] == 200 for s in report["negotiation"])]
        if head["http_code"] == 200 and size > 0:
            length = min(size, 8 * 1024 * 1024)
            for repeat in range(args.repeats):
                for protocol in (enabled if repeat % 2 == 0 else list(reversed(enabled))):
                    sample = probe(args.file_url, protocol, args.cookie_jar, range_value=f"0-{length-1}")
                    sample["rangeContractVerified"] = sample["http_code"] == 206 and sample["bodyBytes"] == length and sample["headers"].get("content-range") == f"bytes 0-{length-1}/{size}"
                    report["boundedRanges"].append(sample)
                    print(json.dumps({"stage": "bounded", "protocol": protocol, "Mbps": round(sample["observedMbps"], 2), "verified": sample["rangeContractVerified"]}), flush=True)
                    save()
            rng = random.Random(42)
            offsets = [rng.randrange(max(1, size - 262144)) for _ in range(8)]
            for offset in offsets:
                for protocol in enabled:
                    end = min(size - 1, offset + 262143)
                    sample = probe(args.file_url, protocol, args.cookie_jar, range_value=f"{offset}-{end}", headers=([f"If-Range: {etag}"] if etag else []))
                    sample["rangeContractVerified"] = sample["http_code"] == 206 and sample["bodyBytes"] == end-offset+1 and sample["headers"].get("content-range") == f"bytes {offset}-{end}/{size}"
                    report["randomRanges"].append(sample)
                    save()
            for protocol in enabled:
                sample = probe(args.file_url, protocol, args.cookie_jar, range_value=f"{size//2}-", seconds=args.prefix_seconds)
                sample["intentionalTimedCancellation"] = sample["exitcode"] == 28 and sample["http_code"] == 206 and sample["bodyBytes"] > 0
                report["continuousRange"].append(sample)
                save()
    report["finished"] = stamp()
    report["summary"] = {}
    for protocol in ("h2", "h3"):
        samples = [s for s in report["boundedRanges"] if s["requestedProtocol"] == protocol and s["protocolVerified"] and s["rangeContractVerified"]]
        report["summary"][protocol] = {"completedRangeSamples": len(samples), "medianMbps": statistics.median(s["observedMbps"] for s in samples) if samples else None,
                                      "medianTtfbSeconds": statistics.median(s["time_starttransfer"] for s in samples) if samples else None}
    save()


if __name__ == "__main__":
    main()
