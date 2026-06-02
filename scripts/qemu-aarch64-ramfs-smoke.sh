#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

export RAMFS_SMOKE_EXPECTED=1
export LOGFILE=${LOGFILE:-qemu-aarch64-ramfs.log}

exec "${SCRIPT_DIR}/qemu-aarch64-core-smoke.sh" "$@"
