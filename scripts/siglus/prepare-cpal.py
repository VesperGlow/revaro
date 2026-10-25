#!/usr/bin/env python3
"""Prepare the pinned CPAL source and its WebAudio stream lifetime fix."""
import argparse
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import urllib.request

VERSION = "0.15.3"
CHECKSUM = "873dab07c8f743075e57f524c583985fbaf745602acbe916a01539364369a779"
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("source", type=Path)
args = parser.parse_args()
destination = args.source.resolve() / "vendor" / "cpal"
patch = Path(__file__).with_name("cpal-webaudio-lifecycle.patch")
if not destination.exists():
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    cached = list((cargo_home / "registry" / "cache").glob(f"*/cpal-{VERSION}.crate"))
    data = cached[0].read_bytes() if cached else urllib.request.urlopen(
        f"https://static.crates.io/crates/cpal/cpal-{VERSION}.crate", timeout=60
    ).read()
    if hashlib.sha256(data).hexdigest() != CHECKSUM:
        raise SystemExit("CPAL archive checksum mismatch")
    destination.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive.getmembers():
            relative = Path(member.name).relative_to(f"cpal-{VERSION}")
            target = destination / relative
            if not target.resolve().is_relative_to(destination) or not (member.isdir() or member.isfile()):
                raise SystemExit("Unexpected CPAL archive member")
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.extractfile(member).read())
reverse = subprocess.run(["git", "apply", "--reverse", "--check", str(patch)], cwd=destination, capture_output=True)
if reverse.returncode:
    subprocess.run(["git", "apply", "--check", str(patch)], cwd=destination, check=True)
    subprocess.run(["git", "apply", str(patch)], cwd=destination, check=True)
print(f"Prepared CPAL {VERSION} with retired-stream callback guard")
