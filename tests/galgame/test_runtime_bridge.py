"""Audit the binary adaptation without executing any supplied game."""
import hashlib
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('bridge', ROOT / 'scripts/kirikiri/export-input-bridge.py')
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)

def sections(data):
    pos = 8
    while pos < len(data):
        kind = data[pos]
        size, start = bridge.read_uleb(data, pos + 1)
        yield kind, data[start:start + size]
        pos = start + size

class RuntimeBridge(unittest.TestCase):
    def test_packaged_runtime_only_adds_export_and_repairs_enter_map(self):
        packaged = (ROOT / 'crates/revaro-web/static/galgame/kirikiri2/index.wasm').read_bytes()
        reconstructed = bytearray(packaged[:8])
        for kind, payload in sections(packaged):
            if kind == 7:
                count, pos = bridge.read_uleb(payload, 0)
                entries = []
                for _ in range(count):
                    start = pos
                    length, pos = bridge.read_uleb(payload, pos)
                    name = payload[pos:pos + length]
                    pos += length
                    export_kind = payload[pos]
                    index, pos = bridge.read_uleb(payload, pos + 1)
                    if name in (b'revaro_mouse_scroll', b'revaro_key_release'):
                        self.assertEqual((export_kind, index), (0, 14591 if name == b'revaro_mouse_scroll' else 14688))
                    else:
                        entries.append(payload[start:pos])
                self.assertEqual(len(entries), count - 2)
                payload = bridge.uleb(len(entries)) + b''.join(entries)
            reconstructed.extend(bytes([kind]) + bridge.uleb(len(payload)) + payload)
        reconstructed = bridge.remap_enter(bytes(reconstructed), 0)
        self.assertEqual(hashlib.sha256(reconstructed).hexdigest(), bridge.ORIGINAL_SHA256)
        self.assertEqual(bridge.patch(bytes(reconstructed)), packaged)

    def test_glue_only_wraps_the_existing_scheduler(self):
        glue = (ROOT / 'crates/revaro-web/static/galgame/kirikiri2/index.js').read_bytes()
        hook = b'var promise=globalThis.RevaroPlayer?globalThis.RevaroPlayer.tick(()=>wrappedTick(arg)):wrappedTick(arg);'
        self.assertEqual(glue.count(hook), 1)
        original = glue.replace(hook, b'var promise=wrappedTick(arg);')
        self.assertEqual(hashlib.sha256(original).hexdigest(),
                         'cf58bad2be094ab94d29e5da8e3eb028480f02f369df404f0a9bf2080e4ddac5')

    def test_unknown_runtime_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'unknown ABI'):
            bridge.patch(b'not the pinned module')

if __name__ == '__main__':
    unittest.main()
