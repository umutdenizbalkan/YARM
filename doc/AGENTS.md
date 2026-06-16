<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM agent-facing entry point

**Canonical owner:** [`doc/AI_AGENT_RULES.md`](./AI_AGENT_RULES.md).

This file exists for AI agents and tooling that look for `AGENTS.md`
by convention. The canonical, authoritative rules for AI agents
working on YARM live in `doc/AI_AGENT_RULES.md` — capabilities, spawn,
zero-copy, fallback / live-path policy, smoke discipline, and (since
the global-unlocking readiness audit) source-file licensing header
(§15) and server-runtime boundary rules (§16).

Always read `doc/AI_AGENT_RULES.md` end-to-end before making any
kernel, IPC, server, or build-script changes; the per-section rules
encode invariants proven through Phase 2A–3B and the kernel-unlocking
milestones (Stage 101+).

Related canonical references:

- `doc/KERNEL_UNLOCKING.md` — kernel unlocking workstream + readiness
  audit (RISC-V64 included as a regular smoke target as of
  stabilization pass 2).
- `doc/KERNEL_LOCKING.md` — lock-rank design.
- `doc/KERNEL_TEST_RULES.md` — per-rule unit-test guard rails.
- `doc/DOCUMENTATION_MAP.md` — repo-wide documentation ownership map.
- `doc/STATUS.md` — live per-arch and per-service status snapshot.
