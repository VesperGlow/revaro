import struct
import tempfile
import unittest
from pathlib import Path
from media_diagnostics import layout


class LayoutTests(unittest.TestCase):
    def inspect(self, data):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'media'
            path.write_bytes(data)
            result = layout(path)
            self.assertEqual(path.read_bytes(), data)
            return result

    def test_mp4_extended_size_and_front_or_tail_index(self):
        ftyp = struct.pack('>I4s', 8, b'ftyp')
        moov = struct.pack('>I4s', 8, b'moov')
        mdat = struct.pack('>I4sQ', 1, b'mdat', 20) + b'data'
        self.assertTrue(self.inspect(ftyp + moov + mdat)['moov_before_media'])
        self.assertFalse(self.inspect(ftyp + mdat + moov)['moov_before_media'])
        self.assertEqual(self.inspect(ftyp + mdat + moov)['mp4_boxes'][2]['offset'], 28)

    def test_flac_seektable_and_truncated_metadata(self):
        self.assertTrue(self.inspect(b'fLaC' + bytes([3, 0, 0, 0, 128, 0, 0, 0]))['flac_seektable'])
        self.assertFalse(self.inspect(b'fLaC' + bytes([128, 0, 0, 0]))['flac_seektable'])
        with self.assertRaises(ValueError):
            self.inspect(b'fLaC' + bytes([128, 0, 0, 9]))

    def test_truncated_box_is_not_followed_outside_the_file(self):
        result = self.inspect(struct.pack('>I4s', 8, b'ftyp') + struct.pack('>I4s', 100, b'moov'))
        self.assertFalse(result['moov_before_media'])
        self.assertEqual(len(result['mp4_boxes']), 1)


if __name__ == '__main__':
    unittest.main()
