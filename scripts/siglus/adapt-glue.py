#!/usr/bin/env python3
"""Adapt pinned CPAL/WGPU browser imports without weakening the player's CSP."""
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
device_import = "const ret = getObject(arg0).requestDevice(getObject(arg1));"
device_replacement = '''const adapter = getObject(arg0);
            const descriptor = getObject(arg1);
            // Modern WebGPU replaced this legacy component limit with a
            // variable limit. Leave its native default when the old key is gone.
            if (descriptor.requiredLimits && !("maxInterStageShaderComponents" in adapter.limits)) {
                delete descriptor.requiredLimits.maxInterStageShaderComponents;
            }
            const ret = adapter.requestDevice(descriptor);'''
if source.count(device_import) != 1:
    raise SystemExit("Unexpected wasm-bindgen WebGPU device import; review before adapting")
before = hashlib.sha256(args.glue.read_bytes()).hexdigest()
args.glue.write_text(source.replace(original, replacement).replace(device_import, device_replacement))
print(json.dumps({"before": before, "after": hashlib.sha256(args.glue.read_bytes()).hexdigest()}))
