#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# pack-initramfs-aligned.py — CPIO newc packer that aligns specified file
# data payloads to PAGE_ALIGN (4096-byte) boundaries within the archive.
#
# Usage:
#   pack-initramfs-aligned.py <rootfs_dir> <output_cpio> \
#       [--align <archive_path>] ...
#
# Entries are packed in sorted order (equivalent to `find . | sort`).
# For each --align target, a zero-data padding entry is inserted immediately
# before it so that the target's file data starts at a multiple of 4096 bytes
# from the start of the archive.  The padding entry uses a name of the form
# `._padNNNN\0` (namesize=10) which the initramfs_srv ignores (unknown name).
#
# The archive is terminated by the standard TRAILER!!! entry.
#
# Proof of alignment is printed to stderr so build scripts can verify:
#   ALIGN_PROOF path=<p> data_offset=<N> aligned=<true|false>

import os
import sys
import struct
import stat

PAGE_ALIGN = 4096
PAD_NAMESIZE = 10  # `._padNNNN\0` = 9 chars + null = 10 bytes

# With namesize=10, the header+name block is:
#   (110 + 10 + 3) & ~3 = 120 bytes  (pad1 = 0 since 120 % 4 == 0)
PAD_HEADER_OVERHEAD = ((110 + PAD_NAMESIZE + 3) & ~3)  # 120


def round_up(n, align):
    return (n + align - 1) & ~(align - 1)


def cpio_newc_header(ino, mode, uid, gid, nlink, mtime, filesize,
                     devmajor, devminor, rdevmajor, rdevminor, namesize):
    """Return a 110-byte CPIO newc header as bytes."""
    h = (
        b"070701"
        + f"{ino:08X}".encode()
        + f"{mode:08X}".encode()
        + f"{uid:08X}".encode()
        + f"{gid:08X}".encode()
        + f"{nlink:08X}".encode()
        + f"{mtime:08X}".encode()
        + f"{filesize:08X}".encode()
        + f"{devmajor:08X}".encode()
        + f"{devminor:08X}".encode()
        + f"{rdevmajor:08X}".encode()
        + f"{rdevminor:08X}".encode()
        + f"{namesize:08X}".encode()
        + b"00000000"  # check field (zero for newc)
    )
    assert len(h) == 110, f"header length {len(h)}"
    return h


def entry_header_name_size(namesize):
    """Return the number of bytes consumed by header + name + pad1."""
    return round_up(110 + namesize, 4)


def write_entry(out, name_bytes, data, ino, mode, mtime=0):
    """Write one CPIO entry to `out` (a bytearray).  Returns bytes written."""
    namesize = len(name_bytes)
    filesize = len(data)
    hdr = cpio_newc_header(
        ino=ino, mode=mode, uid=0, gid=0, nlink=(2 if stat.S_ISDIR(mode) else 1),
        mtime=mtime, filesize=filesize,
        devmajor=0, devminor=0, rdevmajor=0, rdevminor=0,
        namesize=namesize,
    )
    before = len(out)
    out.extend(hdr)
    out.extend(name_bytes)
    while len(out) % 4 != 0:
        out.append(0)
    data_offset = len(out)
    out.extend(data)
    while len(out) % 4 != 0:
        out.append(0)
    return len(out) - before, data_offset


def insert_alignment_pad(out, next_namesize, pad_counter):
    """Insert a zero-data padding entry so the NEXT file's data is PAGE_ALIGN-aligned.

    The CPIO entry for the next file has a header+name block of size H bytes
    before the actual file data.  We insert a pad entry of size P bytes such
    that (current_pos + P + H) is a multiple of PAGE_ALIGN, meaning the file
    data lands exactly on a page boundary.

    Returns the number of bytes written (0 if no padding was needed).
    """
    H = entry_header_name_size(next_namesize)
    current_pos = len(out)
    next_data_pos = current_pos + H
    if next_data_pos % PAGE_ALIGN == 0:
        return 0  # already aligned

    # We want: (current_pos + P + H) % PAGE_ALIGN == 0
    # => (current_pos + P) % PAGE_ALIGN == (-H) % PAGE_ALIGN
    # => target_pos_after_pad = round_up(current_pos + H, PAGE_ALIGN) - H
    target_data_pos = round_up(next_data_pos, PAGE_ALIGN)
    target_pos_after_pad = target_data_pos - H  # position where pad entry ends
    needed = target_pos_after_pad - current_pos  # total bytes the pad entry must occupy

    # A pad entry with PAD_NAMESIZE has overhead = PAD_HEADER_OVERHEAD bytes.
    # The data payload (D bytes, always a multiple of 4) fills the rest.
    if needed < PAD_HEADER_OVERHEAD:
        # target_pos_after_pad < current_pos + PAD_HEADER_OVERHEAD:
        # advance to the next PAGE_ALIGN boundary instead.
        target_data_pos += PAGE_ALIGN
        target_pos_after_pad = target_data_pos - H
        needed = target_pos_after_pad - current_pos

    data_size = needed - PAD_HEADER_OVERHEAD
    assert data_size >= 0, f"data_size={data_size}"
    assert data_size % 4 == 0, f"data_size={data_size} not multiple of 4"

    pad_name = f"._pad{pad_counter:04d}\x00".encode("ascii")
    assert len(pad_name) == PAD_NAMESIZE, f"pad name size={len(pad_name)}"

    written, _ = write_entry(
        out,
        pad_name,
        b"\x00" * data_size,
        ino=0,
        mode=0o100444,
    )
    assert written == needed, f"pad entry wrote {written}, wanted {needed}"
    return written


def collect_entries(rootfs_dir):
    """Walk rootfs_dir and return a sorted list of (archive_name, fs_path, is_dir)."""
    entries = []
    rootfs_dir = os.path.realpath(rootfs_dir)
    for root, dirs, files in os.walk(rootfs_dir, topdown=True):
        dirs.sort()
        rel_root = os.path.relpath(root, rootfs_dir)
        # Normalize to use forward slashes and strip leading ./
        if rel_root == ".":
            arc_dir = "."
        else:
            arc_dir = rel_root.replace(os.sep, "/")
        entries.append((arc_dir, root, True))
        for fname in sorted(files):
            arc_name = fname if arc_dir == "." else f"{arc_dir}/{fname}"
            entries.append((arc_name, os.path.join(root, fname), False))
    return entries


def pack(rootfs_dir, output_path, align_set):
    """Create the CPIO archive at output_path."""
    entries = collect_entries(rootfs_dir)
    out = bytearray()
    ino = 1
    pad_counter = 0
    alignment_results = {}

    for arc_name, fs_path, is_dir in entries:
        name_bytes = arc_name.encode("utf-8") + b"\x00"
        namesize = len(name_bytes)

        needs_align = (arc_name in align_set)
        if needs_align:
            pad_counter += 1
            insert_alignment_pad(out, namesize, pad_counter)

        if is_dir:
            mode = 0o040755
            written, data_off = write_entry(out, name_bytes, b"", ino, mode)
        else:
            try:
                st = os.stat(fs_path)
                mode_bits = st.st_mode & 0o777
                mode = 0o100000 | mode_bits
                with open(fs_path, "rb") as f:
                    data = f.read()
            except OSError as e:
                print(f"[warn] skipping {arc_name}: {e}", file=sys.stderr)
                continue
            written, data_off = write_entry(out, name_bytes, data, ino, mode)

        if needs_align:
            aligned = (data_off % PAGE_ALIGN == 0)
            alignment_results[arc_name] = data_off
            status = "true" if aligned else "false"
            print(
                f"ALIGN_PROOF path={arc_name} data_offset={data_off} aligned={status}",
                file=sys.stderr,
            )

        ino += 1

    # TRAILER!!! entry
    trailer = b"TRAILER!!!\x00"
    write_entry(out, trailer, b"", ino=0, mode=0, mtime=0)

    with open(output_path, "wb") as f:
        f.write(out)

    print(f"[ok] packed {len(entries)} entries, archive size={len(out)} bytes",
          file=sys.stderr)
    return alignment_results


def main():
    import argparse
    parser = argparse.ArgumentParser(
        description="CPIO newc packer with 4096-byte file-data alignment support"
    )
    parser.add_argument("rootfs_dir", help="Root filesystem directory to pack")
    parser.add_argument("output_cpio", help="Output CPIO archive path")
    parser.add_argument(
        "--align", action="append", default=[], metavar="ARCHIVE_PATH",
        help="Archive path to align to 4096-byte boundary (can repeat)",
    )
    args = parser.parse_args()

    align_set = set(args.align)
    results = pack(args.rootfs_dir, args.output_cpio, align_set)

    # Exit non-zero if any requested alignment was not achieved.
    failed = [p for p, off in results.items() if off % PAGE_ALIGN != 0]
    if failed:
        print(f"[error] alignment failed for: {', '.join(failed)}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
