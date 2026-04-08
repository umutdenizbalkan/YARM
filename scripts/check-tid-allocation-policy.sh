#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan

set -euo pipefail

cargo test -q -p yarm kernel::boot::tests::allocate_thread_id_enforces_dynamic_tid_gap_floor
cargo test -q -p yarm kernel::boot::tests::allocate_thread_id_wraps_to_dynamic_floor_after_u64_max
cargo test -q -p yarm kernel::boot::tests::tid_allocation_telemetry_tracks_repairs_allocations_and_wraps

echo "[ok] TID allocation policy gates passed"
