#!/usr/bin/env python3
"""Safely patch the R36S ArkOS DTB for USB Ethernet gadget mode."""
import argparse
import pathlib
import struct
import sys

MAGIC = 0xD00DFEED
OTG_PHY = "/syscon@ff2c0000/usb2-phy@100/otg-port"
USB_CTRL = "/usb@ff300000"


def read_tree(blob):
    if len(blob) < 40:
        raise ValueError("DTB is too short")
    h = list(struct.unpack_from(">10I", blob, 0))
    if h[0] != MAGIC or h[1] > len(blob):
        raise ValueError("Invalid FDT header")
    total, off_struct, off_strings, off_rsv, size_strings, size_struct = (
        h[1], h[2], h[3], h[4], h[8], h[9]
    )
    if not (40 <= off_rsv <= off_struct <= total):
        raise ValueError("Invalid FDT offsets")
    if off_struct + size_struct > total or off_strings + size_strings > total:
        raise ValueError("FDT block extends beyond totalsize")
    strings = blob[off_strings:off_strings + size_strings]
    start, end = off_struct, off_struct + size_struct
    pos = start
    stack = []
    props = {}
    tokens = []

    while pos < end:
        token_start = pos
        token = struct.unpack_from(">I", blob, pos)[0]
        pos += 4
        if token == 1:  # FDT_BEGIN_NODE
            zero = blob.index(b"\0", pos, end)
            raw_name = blob[pos:zero + 1]
            name = raw_name[:-1].decode("utf-8", "strict")
            pos = (zero + 4) & ~3
            if pos > end:
                raise ValueError("Truncated node name")
            stack.append(name)
            path = "/" + "/".join(part for part in stack if part)
            tokens.append((1, raw_name, path))
        elif token == 2:  # FDT_END_NODE
            if not stack:
                raise ValueError("Unbalanced FDT_END_NODE")
            stack.pop()
            tokens.append((2,))
        elif token == 3:  # FDT_PROP
            if pos + 8 > end:
                raise ValueError("Truncated property header")
            length, nameoff = struct.unpack_from(">2I", blob, pos)
            pos += 8
            if nameoff >= len(strings):
                raise ValueError("Invalid property name offset")
            zname = strings.find(b"\0", nameoff)
            if zname < 0:
                raise ValueError("Unterminated property name")
            prop_name = strings[nameoff:zname].decode("ascii", "strict")
            value = blob[pos:pos + length]
            if len(value) != length:
                raise ValueError("Truncated property value")
            pos = (pos + length + 3) & ~3
            path = "/" + "/".join(part for part in stack if part)
            props.setdefault(path, {})[prop_name] = value
            tokens.append((3, nameoff, value, path, prop_name))
        elif token == 4:  # FDT_NOP
            tokens.append((4,))
        elif token == 9:  # FDT_END
            tokens.append((9,))
            if pos != end:
                # Structure blocks may have trailing alignment bytes only.
                if any(blob[pos:end]):
                    raise ValueError("Unexpected bytes after FDT_END")
                pos = end
            break
        else:
            raise ValueError("Unknown FDT token 0x%08x at %d" % (token, token_start))

    if pos != end:
        raise ValueError("FDT structure block length mismatch")
    return h, strings, blob[off_rsv:off_struct], tokens, props


def state(blob):
    _h, _strings, _rsv, _tokens, props = read_tree(blob)
    try:
        status = props[OTG_PHY]["status"].split(b"\0", 1)[0].decode("ascii")
        mode = props[USB_CTRL]["dr_mode"].split(b"\0", 1)[0].decode("ascii")
    except (KeyError, UnicodeDecodeError) as exc:
        raise ValueError("Expected R36S USB nodes/properties not found") from exc
    return status, mode


def patch(blob):
    h, strings, reserve_map, tokens, props = read_tree(blob)
    current = state(blob)
    if current == ("okay", "peripheral"):
        return blob, False
    if current not in (("disabled", "otg"), ("okay", "otg")):
        raise ValueError("Unexpected USB DTB state: otg-port=%s, dr_mode=%s" % current)
    if OTG_PHY not in props or USB_CTRL not in props:
        raise ValueError("Expected R36S USB nodes not found")

    out_struct = bytearray()
    stack = []
    changed_phy = changed_ctrl = False
    for item in tokens:
        kind = item[0]
        if kind == 1:
            raw_name, path = item[1], item[2]
            out_struct += struct.pack(">I", 1) + raw_name
            out_struct += b"\0" * ((-len(out_struct)) & 3)
            stack.append(path)
        elif kind == 2:
            out_struct += struct.pack(">I", 2)
            if stack:
                stack.pop()
        elif kind == 3:
            _kind, nameoff, value, path, prop_name = item
            if path == OTG_PHY and prop_name == "status":
                value = b"okay\0"
                changed_phy = True
            elif path == USB_CTRL and prop_name == "dr_mode":
                value = b"peripheral\0"
                changed_ctrl = True
            out_struct += struct.pack(">3I", 3, len(value), nameoff) + value
            out_struct += b"\0" * ((-len(value)) & 3)
        elif kind in (4, 9):
            out_struct += struct.pack(">I", kind)

    if not (changed_phy and changed_ctrl):
        raise ValueError("Did not find both target USB properties")

    off_struct = h[2]
    off_strings = (off_struct + len(out_struct) + 3) & ~3
    # Keep the header and memory reservation map in their original positions.
    result = bytearray(blob[:off_struct])
    result += out_struct
    result += b"\0" * (off_strings - len(result))
    result += strings
    h[1] = len(result)
    h[3] = off_strings
    h[9] = len(out_struct)
    result[:40] = struct.pack(">10I", *h)
    patched = bytes(result)
    if state(patched) != ("okay", "peripheral"):
        raise ValueError("Post-patch validation failed")
    return patched, True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("patch", "check"))
    parser.add_argument("input", type=pathlib.Path)
    parser.add_argument("output", nargs="?", type=pathlib.Path)
    args = parser.parse_args()
    raw = args.input.read_bytes()
    if args.mode == "check":
        print("otg-port=%s dr_mode=%s" % state(raw))
        return
    if args.output is None:
        parser.error("patch mode requires an output path")
    patched, changed = patch(raw)
    args.output.write_bytes(patched)
    print("patched" if changed else "already configured")
    print("otg-port=%s dr_mode=%s" % state(patched))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, struct.error) as exc:
        print("DTB patch failed: %s" % exc, file=sys.stderr)
        sys.exit(1)
