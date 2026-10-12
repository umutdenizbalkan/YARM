#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""SHARED-REGION-OFFSET1 — where the shared-region oracle object's frames were installed.

Usage: shared-region-frames.py BOOT_LOG GRANT_OFFSET ORACLE_VA

Reads the kernel's `SHARED_REGION_ORACLE_PROVISION_PHYS obj_id=.. base=0x.. len=..` record and
every `USER_MAP_PA_CHECK asid=.. va=0x.. pa=0x..` install. An install is the oracle object's when
its physical address lies inside `[base, base + len)`; it is MISPLACED unless its virtual address
is `ORACLE_VA + (pa - base - GRANT_OFFSET)` — i.e. unless the frame mapped at each page of the
receive window is the one the grant's object offset names. Prints
`installs=<n> misplaced=<m>`, or `nobase` when the object was never provisioned.
"""
import re
import sys

PHYS = re.compile(r"SHARED_REGION_ORACLE_PROVISION_PHYS obj_id=\d+ base=0x([0-9a-f]+) len=(\d+)")
INSTALL = re.compile(r"USER_MAP_PA_CHECK asid=\d+ va=0x([0-9a-f]+) pa=0x([0-9a-f]+)")


def main() -> None:
    log, offset, oracle_va = sys.argv[1], int(sys.argv[2]), int(sys.argv[3], 16)
    base = size = None
    installs = []
    with open(log, errors="replace") as f:
        for line in f:
            m = PHYS.search(line)
            if m:
                base, size = int(m.group(1), 16), int(m.group(2))
            m = INSTALL.search(line)
            if m:
                installs.append((int(m.group(1), 16), int(m.group(2), 16)))
    if base is None:
        print("nobase")
        return
    ours = [(va, pa) for va, pa in installs if base <= pa < base + size]
    bad = [(va, pa) for va, pa in ours if va - oracle_va != pa - base - offset]
    print(f"installs={len(ours)} misplaced={len(bad)}")


if __name__ == "__main__":
    main()
