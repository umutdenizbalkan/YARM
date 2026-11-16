#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""ARTIFACT-CARGO-BIND1 — what ONE `cargo build` invocation produced.

The QEMU artifact scripts used to copy binaries from a fixed `./target/<triple>/<profile>/` path.
With `CARGO_TARGET_DIR` (or a config `build.target-dir`) cargo writes elsewhere, the fixed path
keeps whatever older binary sits there, and that stale binary was published and booted. This tool
reads the build's own JSON messages (`--message-format=json-render-diagnostics`) instead: the
`compiler-artifact` message of the requested binary names the executable this invocation produced
or confirmed fresh, wherever its target directory is.

  executable --bin NAME --messages FILE
      Print `PATH FRESH SHA256` of the one executable cargo reported for bin NAME. Fails (exit 1,
      nothing printed) if the build did not finish successfully, or reported no such executable,
      or more than one, or a path that does not exist.
  ledger-check --ledger FILE --path P
      Exit 0 iff P (resolved) is an executable recorded in the ledger with its current sha256.
"""
import argparse
import hashlib
import json
import os
import sys


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def executable(bin_name, messages):
    found, finished = [], None
    with open(messages, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                m = json.loads(line)
            except json.JSONDecodeError:
                continue
            reason = m.get("reason")
            if reason == "build-finished":
                finished = bool(m.get("success"))
            elif reason == "compiler-artifact":
                t = m.get("target") or {}
                if "bin" in (t.get("kind") or []) and t.get("name") == bin_name and m.get("executable"):
                    found.append((m["executable"], 1 if m.get("fresh") else 0))
    if finished is not True:
        return None, f"the build did not finish successfully (build-finished={finished})"
    paths = sorted(set(found))
    if len(paths) != 1:
        return None, f"the build reported {len(paths)} executables for bin {bin_name}"
    path, fresh = paths[0]
    if not os.path.isfile(path):
        return None, f"the reported executable {path} does not exist"
    return (os.path.realpath(path), fresh, sha256(path)), None


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("executable")
    e.add_argument("--bin", required=True)
    e.add_argument("--messages", required=True)
    c = sub.add_parser("ledger-check")
    c.add_argument("--ledger", required=True)
    c.add_argument("--path", required=True)
    a = ap.parse_args()
    if a.cmd == "executable":
        got, why = executable(a.bin, a.messages)
        if got is None:
            print(f"[error] ARTIFACT-CARGO: {why}", file=sys.stderr)
            return 1
        print(f"{got[0]} {got[1]} {got[2]}")
        return 0
    if a.cmd == "ledger-check":
        want = os.path.realpath(a.path)
        if not os.path.isfile(want):
            return 1
        cur = sha256(want)
        try:
            with open(a.ledger) as f:
                for line in f:
                    parts = line.split()
                    if len(parts) == 4 and parts[1] == want and parts[3] == cur:
                        print(line.strip())
                        return 0
        except OSError:
            pass
        return 1
    return 2


if __name__ == "__main__":
    sys.exit(main())
