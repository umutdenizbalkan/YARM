#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

import json
import pathlib
import re
import subprocess
import sys

SERVER_CRATES = {
    "yarm-control-plane-servers",
    "yarm-fs-servers",
    "yarm-network-servers",
    "yarm-driver-servers",
    "yarm-ui-servers",
    "yarm-compat-servers",
}

RUNTIME_BRIDGE = "yarm-server-runtime"
ROOT_CRATE = "yarm"
KERNEL_CRATE = "yarm-kernel"


def main() -> int:
    proc = subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    )
    meta = json.loads(proc.stdout)

    packages = {pkg["name"]: pkg for pkg in meta["packages"]}

    missing = [name for name in SERVER_CRATES | {RUNTIME_BRIDGE, ROOT_CRATE} if name not in packages]
    if missing:
        print(f"[fail] expected workspace packages missing from cargo metadata: {', '.join(sorted(missing))}")
        return 1

    bad = False

    for crate_name in sorted(SERVER_CRATES):
        deps = {dep["name"] for dep in packages[crate_name]["dependencies"]}
        if RUNTIME_BRIDGE not in deps:
            print(f"[fail] {crate_name} must directly depend on {RUNTIME_BRIDGE}")
            bad = True
        if ROOT_CRATE in deps:
            print(f"[fail] {crate_name} must not directly depend on {ROOT_CRATE}")
            bad = True
        if KERNEL_CRATE in deps:
            print(f"[fail] {crate_name} must not directly depend on {KERNEL_CRATE}")
            bad = True

    bridge_deps = {dep["name"] for dep in packages[RUNTIME_BRIDGE]["dependencies"]}
    if ROOT_CRATE in bridge_deps:
        print(f"[fail] {RUNTIME_BRIDGE} must not depend on {ROOT_CRATE}")
        bad = True

    runtime_lib_path = pathlib.Path("crates") / "yarm-server-runtime" / "src" / "lib.rs"
    runtime_lib_src = runtime_lib_path.read_text(encoding="utf-8")
    if re.search(r"^\s*pub\s+use\s+yarm\s*::\s*\*\s*;", runtime_lib_src, flags=re.MULTILINE):
        print(f"[fail] {runtime_lib_path} must not glob re-export root {ROOT_CRATE}")
        bad = True

    root_deps = {dep["name"] for dep in packages[ROOT_CRATE]["dependencies"]}
    illegal_root_server_edges = sorted(root_deps & SERVER_CRATES)
    if illegal_root_server_edges:
        print(
            f"[fail] {ROOT_CRATE} must not depend on server crates directly: {', '.join(illegal_root_server_edges)}"
        )
        bad = True

    if bad:
        return 1

    print("[ok] crate-graph boundary checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
