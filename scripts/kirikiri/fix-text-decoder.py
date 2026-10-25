#!/usr/bin/env python3
"""Keep automatic text detection local to each file in the pinned Web runtime.

Run after export-input-bridge.py. No game scripts or archive bytes are changed.
The two stores in tTVPTextReadStream's constructor cache an automatically chosen
UTF-8/GBK converter globally. A later Shift-JIS file then fails strict decoding.
Replace only those stores with same-length nops. Explicit Scripts.textEncoding
and Storages.setTextEncoding selections retain their original strict behavior.
See SOURCE.md for the equivalent source change and compatibility limits.
"""
import argparse
import hashlib
from pathlib import Path

INPUT_SHA256 = '86c91161d26e5300bfb812b25b79fec03e0a35d6bd6d2545581cd188c6acd99c'
OUTPUT_SHA256 = '0b585e23f60c251cbe62ac8557ad31accc279be15798ecd4ac3eb66c5c6905aa'
# Exact sites in function 15501; function indices and all data/ABI remain stable.
STORES = (
    (8789255, bytes.fromhex('41d8f4e40241c835360200')),
    (8789284, bytes.fromhex('41d8f4e40241c935360200')),
)


def patch(data, *, reverse=False):
    expected = OUTPUT_SHA256 if reverse else INPUT_SHA256
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError('Unknown runtime: expected the exact pinned text-decoder revision')
    result = bytearray(data)
    for offset, store in STORES:
        before, after = (b'\x01' * len(store), store) if reverse else (store, b'\x01' * len(store))
        if result[offset:offset + len(before)] != before:
            raise ValueError('Unexpected automatic-converter store')
        result[offset:offset + len(after)] = after
    result = bytes(result)
    target = INPUT_SHA256 if reverse else OUTPUT_SHA256
    if hashlib.sha256(result).hexdigest() != target:
        raise ValueError('Unexpected text-decoder output')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input_wasm', type=Path)
    parser.add_argument('output_wasm', type=Path)
    args = parser.parse_args()
    args.output_wasm.write_bytes(patch(args.input_wasm.read_bytes()))


if __name__ == '__main__':
    main()
