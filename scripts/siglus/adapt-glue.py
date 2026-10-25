#!/usr/bin/env python3
"""Replace CPAL's one constant eval probe; keep the player's strict CSP."""
import argparse
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("glue", type=Path)
args = parser.parse_args()
source = args.glue.read_text()
original = "const ret = eval(getStringFromWasm0(arg0, arg1));"
replacement = '''const expression = getStringFromWasm0(arg0, arg1);
            if (expression !== "typeof AudioContext !== 'undefined'") {
                throw new Error("Unsupported dynamic JavaScript evaluation");
            }
            const ret = typeof globalThis.AudioContext !== 'undefined';'''
if source.count(original) != 1:
    raise SystemExit("Unexpected wasm-bindgen eval import; review before adapting")
before = hashlib.sha256(args.glue.read_bytes()).hexdigest()
args.glue.write_text(source.replace(original, replacement))
print(json.dumps({"before": before, "after": hashlib.sha256(args.glue.read_bytes()).hexdigest()}))
