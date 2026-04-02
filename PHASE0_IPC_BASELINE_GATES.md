<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 0 IPC Baseline Gates

Use these commands as conformance gates before refactoring IPC internals:

```bash
cargo test -q capability_checked_ipc_round_trip
cargo test -q notification_irq_route_delivers_message_to_bound_endpoint
cargo test -q syscall_send_large_payload_uses_shared_region_descriptor_with_cap_transfer
cargo test -q syscall_recv_shared_mem_can_auto_map_into_receiver_when_requested
cargo test -q syscall_transfer_release_unmaps_receiver_range_and_revokes_transfer_cap
```

These checks pin:
- endpoint round-trip semantics,
- IRQ notification routing semantics,
- shared-memory transfer descriptor path,
- receiver auto-map contract,
- transfer release unmap+revoke lifecycle.
