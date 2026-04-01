#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

cargo test --lib selected_arch_irq_facade_is_callable
cargo test --lib hal_contract_is_isa_agnostic_for_x86_like_impl
cargo test --lib notification_irq_route_delivers_message_to_bound_endpoint
