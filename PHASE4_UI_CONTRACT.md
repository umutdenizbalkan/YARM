<!-- SPDX-License-Identifier: Apache-2.0 -->

# Phase 4 UI Contract (YARM)

This contract defines minimal invariants for UI-facing services (`display`, `compositor`, `shell`, and phase-linked `input`).

## Boot marker contract

- `display.srv` must emit a stable boot marker string: `[ui] boot-to-shell marker`.
- QEMU smoke validation must treat this marker as an accepted boot-success signal.

## Display contract

- Mode-set and frame-present counters are deterministic for a fixed event sequence.
- Frame-present checks are required in CI for Phase-4 readiness.

## Compositor replay contract

- Composition replay count is deterministic across repeated runs.
- Compositor behavior should remain independent from FS/network internals.

## Shell/session contract

- Shell session startup counter is deterministic and monotonic.
- Session-manager startup is validated by deterministic unit coverage.

## IPC transfer-cap ABI prerequisite

- Kernel IPC syscall ABI is frozen at v3.
- Transfer-cap send requires a known waiting receiver (`WouldBlock` otherwise).
- Transfer metadata is an envelope handle (not a raw source capability id).
- Reference: `LIBC_ABI_X86_64_NONE.md`.

## CI gate mapping

- `kernel::syscall::tests::transfer_send_without_waiter_returns_would_block`
- `services::ui::display::service::tests::boot_marker_is_stable`
- `services::ui::display::service::tests::display_tracks_modeset_and_present`
- `services::ui::compositor::service::tests::compositor_replay_is_deterministic`
- `services::ui::shell::service::tests::shell_session_counter_increments`
- `phase4-ui-smoke-marker` job in `.github/workflows/compat-gates.yml`
