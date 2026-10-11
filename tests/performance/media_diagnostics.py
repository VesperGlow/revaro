#!/usr/bin/env python3
"""Read-only media diagnostics; optional offline MP4 faststart writes a new file.

Requires host ffprobe (and ffmpeg only for --faststart-output). It is not an
online/transcoding dependency. Packet sampling is capped at 20,000 packets and
30 seconds; rates below are one-second packet buckets, not decoder benchmarks.
"""
import argparse
import collections
import json
from pathlib import Path
import shutil
import struct
import subprocess
import time


def layout(path):
    size = path.stat().st_size
    with path.open('rb') as source:
        if source.read(4) == b'fLaC':
            blocks = []
            while len(blocks) < 1024:
                offset = source.tell()
                header = source.read(4)
                if len(header) != 4:
                    raise ValueError('truncated FLAC metadata')
                length = int.from_bytes(header[1:], 'big')
                if source.tell() + length > size:
                    raise ValueError('FLAC metadata exceeds file size')
                blocks.append({'type': header[0] & 127, 'offset': offset, 'bytes': length})
                source.seek(length, 1)
                if header[0] & 128:
                    return {'flac_seektable': any(b['type'] == 3 for b in blocks), 'metadata': blocks}
            raise ValueError('too many FLAC metadata blocks')
        source.seek(0)
        boxes = []
        while source.tell() + 8 <= size and len(boxes) < 1024:
            offset = source.tell()
            length, name = struct.unpack('>I4s', source.read(8))
            header_bytes = 8
            if length == 1:
                extended = source.read(8)
                if len(extended) != 8:
                    break
                length = struct.unpack('>Q', extended)[0]
                header_bytes = 16
            if length == 0:
                length = size - offset
            if length < header_bytes or offset + length > size:
                break
            boxes.append({'type': name.decode('ascii', errors='replace'), 'offset': offset, 'bytes': length})
            source.seek(offset + length)
        if boxes and boxes[0]['type'] == 'ftyp':
            moov = next((b['offset'] for b in boxes if b['type'] == 'moov'), None)
            mdat = next((b['offset'] for b in boxes if b['type'] == 'mdat'), None)
            return {'mp4_boxes': boxes, 'moov_before_media': moov is not None and mdat is not None and moov < mdat}
        return {}


def diagnose(path):
    start = time.perf_counter()
    executable = shutil.which('ffprobe')
    if not executable:
        raise RuntimeError('host ffprobe is required for offline diagnostics')
    probe = subprocess.run([executable, '-v', 'error', '-show_format', '-show_streams',
        '-show_entries', 'packet=stream_index,pts_time,dts_time,size,flags',
        '-read_intervals', '%+#20000', '-of', 'json', str(path)],
        check=True, capture_output=True, timeout=30)
    result = json.loads(probe.stdout)
    packets = result.pop('packets', [])
    rates = collections.defaultdict(int)
    keys = collections.defaultdict(list)
    for packet in packets:
        timestamp = packet.get('dts_time', packet.get('pts_time'))
        if timestamp is None:
            continue
        timestamp = float(timestamp)
        rates[int(timestamp)] += int(packet.get('size', 0)) * 8
        if 'K' in packet.get('flags', ''):
            keys[packet['stream_index']].append(timestamp)
    result.update(path=str(path), bytes=path.stat().st_size, layout=layout(path),
        sampled_packets=len(packets), packet_limit=20000,
        max_sampled_one_second_packet_bps=max(rates.values(), default=0),
        max_sampled_keyframe_interval_seconds={str(k): max((b-a for a,b in zip(v,v[1:])), default=0) for k,v in keys.items()},
        diagnostic_seconds=time.perf_counter()-start)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=Path)
    parser.add_argument('--faststart-output', type=Path, help='optional new MP4 output; never overwrite an existing file')
    args = parser.parse_args()
    source = args.input.resolve(strict=True)
    result = diagnose(source)
    if args.faststart_output:
        output = args.faststart_output.resolve()
        if output.exists() or output == source:
            parser.error('faststart output must be a new file distinct from the source')
        if 'mp4_boxes' not in result['layout']:
            parser.error('offline faststart is supported only for MP4 input')
        executable = shutil.which('ffmpeg')
        if not executable:
            parser.error('host ffmpeg is required for offline faststart')
        subprocess.run([executable, '-v', 'error', '-nostdin', '-n', '-i', str(source),
            '-map', '0', '-c', 'copy', '-movflags', '+faststart', str(output)], check=True)
        result['derived'] = diagnose(output)
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
