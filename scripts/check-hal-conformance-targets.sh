#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# HAL conformance target gate: riscv64 + x86_64 + aarch64 baseline checks.

cargo test -q hal_contract_is_isa_agnostic_for_riscv_like_impl
cargo test -q hal_contract_is_isa_agnostic_for_x86_like_impl
cargo test -q hal_contract_is_isa_agnostic_for_aarch64_like_impl

cargo test -q discovers_plic_description_from_alias_keys
cargo test -q discovers_lapic_description_from_alias_key
cargo test -q discovers_gic_description_from_alias_key

echo "[ok] HAL conformance targets gate passed (riscv64/x86_64/aarch64)"
