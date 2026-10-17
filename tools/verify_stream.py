#!/usr/bin/env python3
"""Measure one exclusive DevBridge video session. Close Studio before running.

Uses the system liblz4 to validate complete frames without installing Python packages.
No input is injected. --check-controls verifies mode acknowledgements without pressing keys.
"""
import argparse
import collections
import ctypes
import ctypes.util
import hashlib
import json
import socket
import struct
import time


def percentile(values, fraction):
    return sorted(values)[min(len(values) - 1, int(len(values) * fraction))] if values else None


def verify(host, seconds, check_controls=False):
    lib = ctypes.CDLL(ctypes.util.find_library('lz4') or 'liblz4.so.1')
    lib.LZ4_decompress_safe.argtypes = [ctypes.c_char_p, ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
    lib.LZ4_decompress_safe.restype = ctypes.c_int
    with socket.create_connection((host, 7365), 2) as control:
        control.settimeout(3)
        reader = control.makefile('rb')

        def command(name, payload=None):
            data = {'type': name}
            if payload is not None:
                data['payload'] = payload
            control.sendall(json.dumps(data).encode() + b'\n')
            return json.loads(reader.readline())

        handshake = command('Handshake', {'client_version': '0.2.9', 'client_id': 'stream-verification'})
        if check_controls:
            state = command('SetGamepadMonitor', {'enabled': True})['payload']
            assert state['listening'] and not state['input_enabled'], state
            state = command('SetGamepadInput', {'enabled': True})['payload']
            assert state['input_enabled'] and not state['listening'], state
            state = command('SetGamepadMonitor', {'enabled': True})['payload']
            assert state['listening'] and not state['input_enabled'], state
        command('StartStream', {'target_fps': 30})
        counts = collections.Counter()
        formats = collections.Counter()
        sizes, intervals, pings = [], [], []
        hashes = set()
        first = None
        errors = []
        start = time.monotonic()
        previous = None
        last_ping = start
        try:
            with socket.create_connection((host, 7366), 2) as video:
                video.settimeout(2)

                def exact(size):
                    result = bytearray()
                    while len(result) < size:
                        data = video.recv(size - len(result))
                        if not data:
                            raise EOFError('video disconnected')
                        result.extend(data)
                    return bytes(result)

                while time.monotonic() - start < seconds:
                    magic, index, width, height, fmt, flag, size = struct.unpack('<4sIHHBBI2x', exact(20))
                    expected = width * height * (2 if fmt == 1 else 4)
                    if magic != b'VID0' or not 0 < expected <= 16 * 1024 * 1024 or size > expected + 65536:
                        raise ValueError('invalid header')
                    payload = exact(size)
                    now = time.monotonic()
                    counts[flag] += 1
                    formats[fmt] += 1
                    if previous is not None:
                        intervals.append((now - previous) * 1000)
                    previous = now
                    if size:
                        decoded = ctypes.create_string_buffer(expected)
                        n = lib.LZ4_decompress_safe(payload, decoded, size, expected)
                        if n != expected:
                            raise ValueError('invalid LZ4 length: ' + str(n))
                        sizes.append(size)
                        hashes.add(hashlib.sha256(decoded.raw).hexdigest())
                        if first is None:
                            first = {'width': width, 'height': height, 'format': fmt, 'bytes': size}
                    if now - last_ping >= 1:
                        ping = time.monotonic()
                        assert command('Ping')['type'] == 'Pong'
                        pings.append((time.monotonic() - ping) * 1000)
                        last_ping = now
        except (OSError, ValueError, EOFError) as error:
            errors.append(str(error))
        finally:
            elapsed = time.monotonic() - start
            command('StopStream')
        report = {
            'handshake': handshake, 'seconds': elapsed, 'flags': dict(counts), 'formats': dict(formats),
            'frames_per_second': sum(counts.values()) / elapsed, 'changed_frames': len(sizes),
            'unique_frames': len(hashes), 'payload_mean': sum(sizes) / max(1, len(sizes)),
            'payload_mib_s': sum(sizes) / elapsed / 1024 / 1024,
            'frame_interval_p95_ms': percentile(intervals, .95), 'ping_p95_ms': percentile(pings, .95),
            'first': first, 'errors': errors,
        }
        return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host', default='192.168.7.2')
    parser.add_argument('--seconds', type=float, default=10)
    parser.add_argument('--check-controls', action='store_true')
    args = parser.parse_args()
    if args.seconds <= 0:
        parser.error('--seconds must be positive')
    result = verify(args.host, args.seconds, args.check_controls)
    print(json.dumps(result, indent=2))
    raise SystemExit(bool(result['errors']) or not result['changed_frames'])
