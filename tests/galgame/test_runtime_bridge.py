"""Audit the binary adaptation without executing any supplied game."""
import hashlib
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('bridge', ROOT / 'scripts/kirikiri/export-input-bridge.py')
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
text_spec = importlib.util.spec_from_file_location('text_decoder', ROOT / 'scripts/kirikiri/fix-text-decoder.py')
text_decoder = importlib.util.module_from_spec(text_spec)
text_spec.loader.exec_module(text_decoder)

def sections(data):
    pos = 8
    while pos < len(data):
        kind = data[pos]
        size, start = bridge.read_uleb(data, pos + 1)
        yield kind, data[start:start + size]
        pos = start + size

class RuntimeBridge(unittest.TestCase):
    def test_packaged_runtime_only_adds_export_and_repairs_enter_map(self):
        packaged = text_decoder.patch(
            (ROOT / 'crates/revaro-web/static/galgame/kirikiri2/index.wasm').read_bytes(), reverse=True)
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

    def test_text_decoder_changes_only_the_two_auto_detection_stores(self):
        packaged = (ROOT / 'crates/revaro-web/static/galgame/kirikiri2/index.wasm').read_bytes()
        previous = text_decoder.patch(packaged, reverse=True)
        self.assertEqual(text_decoder.patch(previous), packaged)
        expected = set()
        for offset, store in text_decoder.STORES:
            self.assertEqual(previous[offset:offset + len(store)], store)
            self.assertEqual(packaged[offset:offset + len(store)], b'\x01' * len(store))
            expected.update(range(offset, offset + len(store)))
        self.assertEqual({i for i, (a, b) in enumerate(zip(previous, packaged)) if a != b}, expected)
        with self.assertRaisesRegex(ValueError, 'Unknown runtime'):
            text_decoder.patch(b'unknown WASM')

if __name__ == '__main__':
    unittest.main()
