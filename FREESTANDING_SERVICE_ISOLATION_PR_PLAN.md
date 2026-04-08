<!-- SPDX-License-Identifier: Apache-2.0 -->

# Freestanding Service Isolation PR Plan

This plan starts the implementation track for **real freestanding/user-mode service isolation**:
non-hosted ELF launch pipeline + initramfs-driven service launch + separate user address spaces.

## Phases

1. **Phase 1 — Non-hosted boot payload discovery + launch-manifest seed (this PR)**
   - Parse PVH module metadata in non-hosted x86_64 boot.
   - Emit deterministic boot telemetry showing discovered initramfs payload window.
   - Keep behavior non-disruptive (observation + data plumbing only).

2. **Phase 2 — Initramfs executable manifest contract**
   - Define stable initramfs paths/metadata for core services (`init.srv`, `process_manager.srv`, `vfs.srv`, `supervisor.srv`).
   - Add a typed loader-manifest format and parser.
   - Add contract tests for missing/corrupt manifest entries.

3. **Phase 3 — ELF image validation + mapping contract integration**
   - Bridge parsed initramfs images into `ElfImageInfo` validation path.
   - Add segment-level mapping plan output (text/data/bss/stack guard intent) before full pager wiring.
   - Enforce reject-on-invalid ABI/ELF layout before spawn.

4. **Phase 4 — init.srv launch via initramfs (replace fixed entry constants)**
   - Remove hardcoded entry addresses from core launch plan.
   - Resolve entry points from validated ELF metadata loaded from initramfs.
   - Keep scheduler handoff stable with deterministic fault reporting.

5. **Phase 5 — Separate address-space hardening + lifecycle policies**
   - Enforce per-service ASID/address-space ownership and switch auditing.
   - Add restart/revoke tests proving no cross-service mapping leakage.
   - Extend core-profile smoke and fault-injection scenarios for isolation invariants.

## PR List

- **PR-A (Phase 1):** PVH payload/module discovery telemetry + tests.
- **PR-B (Phase 2):** initramfs executable manifest contract + parser/tests.
- **PR-C (Phase 3):** ELF mapping-plan adapter and strict validation gates.
- **PR-D (Phase 4):** init launch path migration to initramfs-derived entries.
- **PR-E (Phase 5):** address-space isolation hardening + scenario/CI gates.
