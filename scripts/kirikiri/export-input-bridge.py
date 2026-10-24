#!/usr/bin/env python3
"""Expose existing Web scroll/key-release handlers; never rewrite a function body.

Pinned upstream 2028cbccaa91498bd36f270d349ae711d0e8fed3.
Exports correspond to TVPWindowLayer::onMouseScroll(Event*) and
TVPMainScene::onKeyReleased(KeyCode, Event*) in
cpp/core/environ/cocos2d/MainScene.cpp. ABI metadata is documented in SOURCE.md.
"""
import argparse
import hashlib
from pathlib import Path

ORIGINAL_SHA256 = 'bb5f92e3e66ddb94893f81fed92705b1d29ff82c393ad679e8fa402ce52c495a'

def read_uleb(data, pos):
    value = shift = 0
    while True:
        byte = data[pos]
        pos += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, pos
        shift += 7
        if shift > 35:
            raise ValueError('invalid ULEB')

def uleb(value):
    out = bytearray()
    while value > 127:
        out.append((value & 127) | 128)
        value >>= 7
    out.append(value)
    return bytes(out)

def remap_enter(data, value):
    """Cocos KEY_ENTER(164) -> VK_RETURN(13), in pinned passive segment 436.

    CCGLViewImpl maps SDL_RETURN to KEY_ENTER, while CCKeyCodeConv omitted it.
    Only this lookup byte changes; native key remapping, state and dispatch remain intact.
    """
    pos = 8
    while pos < len(data):
        kind = data[pos]
        size, payload = read_uleb(data, pos + 1)
        if kind == 11:
            count, cursor = read_uleb(data, payload)
            for index in range(count):
                flags, cursor = read_uleb(data, cursor)
                if flags != 1:
                    raise ValueError('Expected pinned passive data segments')
                length, cursor = read_uleb(data, cursor)
                if index == 436:
                    offset = cursor + 44
                    if length <= 44 or data[offset] not in (0, 13):
                        raise ValueError('Unexpected Cocos key map')
                    return data[:offset] + bytes([value]) + data[offset + 1:]
                cursor += length
        pos = payload + size
    raise ValueError('Cocos key map not found')

def patch(data):
    if hashlib.sha256(data).hexdigest() != ORIGINAL_SHA256:
        raise ValueError('Input must be the exact pinned upstream WASM; refusing an unknown ABI')
    pos = 8
    while pos < len(data):
        start = pos
        section = data[pos]
        size, payload = read_uleb(data, pos + 1)
        end = payload + size
        if section == 7:
            count, entries = read_uleb(data, payload)
            exports = {b'revaro_mouse_scroll': 14591, b'revaro_key_release': 14688}
            extra = b''.join(uleb(len(name)) + name + b'\0' + uleb(index) for name, index in exports.items())
            payload = uleb(count + len(exports)) + data[entries:end] + extra
            return remap_enter(data[:start] + b'\7' + uleb(len(payload)) + payload + data[end:], 13)
        pos = end
    raise ValueError('export section not found')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('upstream_wasm', type=Path)
    parser.add_argument('output_wasm', type=Path)
    parser.add_argument('--glue', type=Path, help='Previous Revaro glue with the SDL button-order fix')
    parser.add_argument('--output-glue', type=Path)
    args = parser.parse_args()
    if bool(args.glue) != bool(args.output_glue):
        parser.error('--glue and --output-glue must be specified together')
    if args.glue:
        glue = args.glue.read_bytes()
        if hashlib.sha256(glue).hexdigest() != 'cf58bad2be094ab94d29e5da8e3eb028480f02f369df404f0a9bf2080e4ddac5':
            raise ValueError('Input glue must match the pinned Revaro SDL button-order revision')
        old = b'var promise=wrappedTick(arg);'
        if glue.count(old) != 1:
            raise ValueError('Unexpected frame scheduler')
        glue = glue.replace(old, b'var promise=globalThis.RevaroPlayer?globalThis.RevaroPlayer.tick(()=>wrappedTick(arg)):wrappedTick(arg);')
        args.output_glue.write_bytes(glue)
    args.output_wasm.write_bytes(patch(args.upstream_wasm.read_bytes()))

if __name__ == '__main__':
    main()
