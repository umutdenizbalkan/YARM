#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path

PACKER_PATH = Path(__file__).with_name("pack-initramfs-aligned.py")
SPEC = importlib.util.spec_from_file_location("pack_initramfs_aligned", PACKER_PATH)
PACKER = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(PACKER)


def cpio_data_offsets(archive: bytes):
    offsets = {}
    cursor = 0
    while cursor + 110 <= len(archive):
        header = archive[cursor : cursor + 110]
        if header[:6] != b"070701":
            raise AssertionError(f"bad newc magic at {cursor}")
        filesize = int(header[54:62], 16)
        namesize = int(header[94:102], 16)
        name_start = cursor + 110
        name_end = name_start + namesize
        name = archive[name_start : name_end - 1].decode()
        data_offset = PACKER.round_up(name_end, 4)
        if name == "TRAILER!!!":
            return offsets
        offsets[name.removeprefix("./")] = data_offset
        cursor = PACKER.round_up(data_offset + filesize, 4)
    raise AssertionError("missing CPIO trailer")


class AlignedInitramfsTests(unittest.TestCase):
    def test_every_elf_is_page_aligned_and_emits_proof(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "root"
            (root / "sbin").mkdir(parents=True)
            (root / "init").write_bytes(b"\x7fELFinit")
            (root / "sbin" / "early").write_bytes(b"\x7fELFearly")
            (root / "sbin" / "late").write_bytes(b"\x7fELFlate")
            (root / "sbin" / "config").write_text("not an ELF")
            output = Path(temp) / "initramfs.cpio"

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                results = PACKER.pack(root, output, set())

            offsets = cpio_data_offsets(output.read_bytes())
            for path in ("init", "sbin/early", "sbin/late"):
                self.assertEqual(offsets[path] % PACKER.PAGE_ALIGN, 0)
                self.assertEqual(results[path], offsets[path])
                self.assertIn(
                    f"ALIGN_PROOF path=/{path} data_offset={offsets[path]} "
                    "alignment_mod=0 aligned=true",
                    stderr.getvalue(),
                )
            self.assertNotIn("sbin/config", results)
            self.assertNotIn("aligned=false", stderr.getvalue())

    def test_packing_fails_if_an_elf_payload_would_be_unaligned(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "root"
            root.mkdir()
            (root / "init").write_bytes(b"\x7fELFunpadded")
            output = Path(temp) / "initramfs.cpio"
            original = PACKER.insert_alignment_pad
            PACKER.insert_alignment_pad = lambda *_args: 0
            try:
                with contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "unaligned payload"):
                        PACKER.pack(root, output, set())
            finally:
                PACKER.insert_alignment_pad = original

    def test_explicit_non_elf_alignment_remains_available(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "root"
            root.mkdir()
            (root / "manifest").write_text("metadata")
            output = Path(temp) / "initramfs.cpio"

            results = PACKER.pack(root, output, {"manifest"})

            self.assertEqual(results["manifest"] % PACKER.PAGE_ALIGN, 0)


if __name__ == "__main__":
    unittest.main()
