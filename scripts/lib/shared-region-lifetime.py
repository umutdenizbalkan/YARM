#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""SHARED-REGION-LIFETIME1 — the oracle object's counters and frames, read back from the kernel.

Usage: shared-region-lifetime.py BOOT_LOG

Reads, in log order:

* `SHARED_REGION_ORACLE_PROVISION_PHYS obj_id=.. base=0x.. len=..` — the object's extent;
* every `SHARED_REGION_ORACLE_REFS op=.. obj_id=.. off=0x.. cap=.. map=.. pin=..` — the kernel's
  own accounting, reported after each change by the operation that made it;
* every `ASPACE_FRAME_RELEASED phys=0x..` — a frame handed back to the allocator;
* the witness's coordination markers.

Prints one line:

  refs=<seq> frees_in_extent=<n> order=<ok|bad:..>

`seq` is each REFS record as `<op initial><page index>:<map>/<pin>`, comma-joined, so a smoke can
compare the whole transition sequence against the one the contract requires. `order` checks that
the clone's two references precede the fork's commit, that the parent's release (both settlements)
precedes the parent's RELEASED marker, and that the child's check follows it.
"""
import re
import sys

PHYS = re.compile(r"SHARED_REGION_ORACLE_PROVISION_PHYS obj_id=\d+ base=0x([0-9a-f]+) len=(\d+)")
REFS = re.compile(
    r"SHARED_REGION_ORACLE_REFS op=(\w+) obj_id=\d+ off=0x([0-9a-f]+) cap=\d+ map=(\d+) pin=(\d+)"
)
FREED = re.compile(r"ASPACE_FRAME_RELEASED phys=0x([0-9a-f]+)")


def main() -> None:
    base = size = None
    refs, freed = [], []
    marks = {}
    with open(sys.argv[1], errors="replace") as f:
        for i, line in enumerate(f):
            m = PHYS.search(line)
            if m:
                base, size = int(m.group(1), 16), int(m.group(2))
            m = REFS.search(line)
            if m:
                refs.append((i, m.group(1)[0], int(m.group(2), 16) >> 12, m.group(3), m.group(4)))
            m = FREED.search(line)
            if m:
                freed.append(int(m.group(1), 16))
            for key in (
                "FORK_TXN_COMMITTED",
                "SHARED_REGION_LIFETIME_PARENT_RELEASED",
                "SHARED_REGION_LIFETIME_CHILD ",
            ):
                if key in line and key not in marks:
                    marks[key] = i
    if base is None:
        print("nobase")
        return
    seq = ",".join(f"{op}{page}:{mp}/{pin}" for _, op, page, mp, pin in refs)
    frees = sum(1 for p in freed if base <= p < base + size)
    order = "ok"
    fork = marks.get("FORK_TXN_COMMITTED")
    released = marks.get("SHARED_REGION_LIFETIME_PARENT_RELEASED")
    child = marks.get("SHARED_REGION_LIFETIME_CHILD ")
    if len(refs) < 8 or None in (fork, released, child):
        order = "bad:missing"
    elif not refs[3][0] < fork < refs[4][0]:
        order = "bad:clone_not_in_fork"
    elif not refs[7][0] < released < child:
        order = "bad:child_before_release"
    print(f"refs={seq} frees_in_extent={frees} order={order}")


if __name__ == "__main__":
    main()
