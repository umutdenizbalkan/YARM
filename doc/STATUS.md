<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# YARM Current Status

> **Live state only.** This file does not narrate milestones. It says
> what is currently working on each architecture and per-service domain,
> and links the next-target details to the canonical owner doc. For
> closed-milestone history, see `doc/PROJECT_HISTORY.md`. For ownership
> and authoring rules, see `doc/DOCUMENTATION_MAP.md`.

---

## 0. Kernel-unlock frontier — current verified state

**Broad-lock census verified at commit `757993b6`, tree `1118b61b`. Live-cell evidence at
commit `f5669cb55325ac58aba6a15207a89c95ad8cad3d`, tree
`e2fd0b5c7a82dc6c8c422d5c6db242473533a9a6`.**
Full evidence: `doc/KERNEL_UNLOCK_AUDIT.md`. Canonical stage ladder and roadmap:
`doc/KERNEL_UNLOCKING.md` §0.

### Broad-lock position

| Metric | Value |
|--------|-------|
| Production `SharedKernel::with_cpu` callsites | **39** |
| Production broad `SharedKernel::with` callsites | **10** |
| **Total production broad-lock acquisition sites** | **49** |
| Ungated off-lock syscall classes | **5** on x86_64 (NR 15, 10, 8, 2-narrow, 14-narrow); **2** on AArch64 (NR 15, 10); **2** on RISC-V (NR 15, 10) |
| Proof-gated off-lock classes (default **OFF**) | NR 6 `IpcCall`, NR 7 `IpcReply` — all three architectures |
| Off-lock authoritative dispatch | **x86_64 (live) + AArch64 (structural, proof-gated)** via `offlock_authoritative_dispatch_enabled()`; `d6_genuine_enabled()` itself remains compile-time x86_64-only. RISC-V not admitted. |

### Hosted validation (re-executed, not inherited)

| Command | Result |
|---------|--------|
| `cargo test --lib -- --test-threads=1` | ✅ 3729 passed, 0 failed, 2 ignored |
| `cargo test --tests -- --test-threads=1` | ✅ 3881 passed (3729 lib + 152 integration), 0 failed |
| `cargo test --tests --features ipc-reply-timeout-oracle-core -- --test-threads=1` | ✅ 4045 passed, 0 failed |
| `cargo test --lib` (default parallel harness) | ⚠️ **completes, 0 aborts** — 58–71 logical shared-state assertion failures remain |
| all 13 repository gate scripts | ✅ **13 of 13 pass** |
| `cargo check` — x86_64 / AArch64 / RISC-V bare-metal `kernel_boot` | ✅ clean |
| `cargo check` — x86_64 / AArch64 / RISC-V freestanding `crash_test_srv` | ✅ clean |
| `cargo fmt --check`, `git diff --check` | ✅ clean |

The parallel memory corruption (three cross-test aliasing bugs) is **fixed**; see
`doc/KERNEL_TEST_RULES.md` Rule H1. What remains is process-global test contention in the
hosted corpus: **test-infrastructure debt, not canonical Stage 205C work.** It may precede
or support 205C (which is a long-running torture of the running kernel) but closes no part
of it.

### Canonical stage position

Stage definitions are owner-supplied and authoritative
(`doc/KERNEL_UNLOCKING.md` §0). **A historical stage carrying the same number does not
complete the canonical stage.**

| Phase | Complete | Partial foundation | Open |
|-------|----------|--------------------|------|
| 2 — IPC (199C–199G) | 0 of 5 | 199D, 199E | 199C, 199F, 199G |
| 3 — Capability (200A–200D) | 0 of 4 | 200A, 200C | 200B, 200D |
| 4 — VM (201A–201G) | 0 of 7 | 201B, 201F | 201A, 201C, 201D, 201E, 201G |
| 5 — Lifecycle (202A–202F) | 0 of 6 | 202D | 202A, 202B, 202C, 202E, 202F |
| 6 — Timer/IRQ/sched (203A–203D) | 0 of 4 | 203A, 203C, 203D | 203B |
| 7 — Monolith removal (204A–204E) | **1 of 5** (204A) | 204B | 204C, 204D, 204E |
| 8 — Seal (205A–205D) | 0 of 4 | 205A | 205B, 205C, 205D |
| **Total** | **1 of 35** | 12 | 22 |

**No canonical stage in Phases 2–6 or 8 is complete.** The one complete stage, 204A
(broad-lock callsite census), is documentation rather than lock retirement: 49 callsites
classified as 0 boot-only, 3 test-only, 2 obsolete, 44 runtime-required, 0 undocumented.

> **Arithmetic correction.** An earlier revision reported *1 of 34* with 11 partials. Phase 7
> was the only row written without an `N of M` denominator, and the totals silently counted it
> as four stages. **The dropped stage was `204B` (decompose `KernelState` ownership), the sole
> Phase-7 partial**, which is why both the total (34 → **35**) and the partial count
> (11 → **12**) were low by exactly one. All 35 stages were, and remain, individually
> documented and classified; only the summary arithmetic was wrong. `204B` is classified
> **partial foundation**: the eleven ranked domain locks and the `*_split_mut` / `*_split_read`
> seam set already exist, but `with_cpu` still forms a broad `&mut KernelState`, so the
> container still serializes the kernel.


The historical stages labelled 200A/200B/200C (terminal ownership, deadline token,
reply-timeout transaction) are IPC timeout work belonging to canonical **199E**. They
contribute nothing to canonical 200A–200C, which are the **capability** stages and have
essentially no production wiring — every capability seam is `M2_SEAM_HELPER_ONLY`.

### Live cells earned

| Programme | Cells | Seal / canonical stage served |
|-----------|-------|-------------------------------|
| Stage 198F combined retirement (first cohort 12 + supported `IpcSend` 18) | **30** | `STAGE_198F_COMPLETE_RETIREMENT_SEAL … total_live_cells=30 result=ok`; pre-199C groundwork, 199C delivery, 200C shared-region |
| Reply-timeout matrix | **6** | `STAGE_200_IPC_REPLY_TIMEOUT_MATRIX_SEAL`, commit `72a4ebf`; 199E (one quarter of the stage) |
| `ExitCurrentTask` NR 16 | **2 of 3** | x86_64 `0b5e98f`, AArch64; 202D (one sub-path; RISC-V unearned) |
| **Server death (`ServerDies`) — x86_64** | **1 of 3** | **`STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL`, commit `f5669cb5`; canonical 199D server-crash-cleanup increment** |
| *Pre-production subtotal* | *39* | *30 + 6 + 2 + 1 — the figure accepted before the x86_64 production default was flipped* |
| **Direct IPC NR 6 / NR 7 — HISTORICAL production-default-ON evidence (`0b5ec254`)** | **1** | **`IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok`, exact commit `0b5ec254`** — 53 NR6 + 41 NR7 ordinary syscalls off-lock with zero broad-lock entries on the **production** path; canonical 199D production-path increment |
| **Current production-path total** | **39** | 30 + 6 + 2 + 1 — the direct-IPC production cell moved out at Stage 199D-WA1-GATE |
| **Direct IPC NR 6 / NR 7 — moved to non-production at WA1-GATE** | **1** | **Originally earned UNDER the x86_64 production default** at `0b5ec254` (not under a proof knob). Retained as historical mechanism/production evidence. It is **no longer a claim about the current production predicate**, which `ipccall_direct_production_enabled()` now returns `false` for on every architecture while `WAITER_OWNERSHIP_EXCLUSIVE=no`. |
| Direct IPC NR 6 / NR 7 (x86_64, SMP=2) | 6, **knob-gated** | `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok`; proves the 199D **mechanism**, **not** the production path. Originally earned at `ccceb03d`; **re-earned at `7d5a22c9`** after three repairs — see §0.1. The re-earning restores historical evidence and **adds no new cell**. |
| **Non-production / mechanism evidence** | **7** | 6 knob-gated + the 1 moved at WA1-GATE |
| **Historical total** | **46** | 39 + 7 — unchanged; nothing is retracted and no new live cell is earned |

> **On the total.** There is no aggregate live-cell counter anywhere in the tree; the only
> in-tree aggregate is Stage 198F's `total_live_cells=30`. The figures above are computed from
> the seals listed, each of which is named with its exact commit.
>
> * **Current production-path = 39.** Stage 199D-WA1-GATE disabled the x86_64 direct production
>   default, so the one `0b5ec254` cell moved out of the current-production bucket and the
>   total returned to the pre-production subtotal (30 + 6 + 2 + 1). Exactly one cell moved: the
>   ledger records the x86 NR6+NR7 production-default increment as **one combined cell**, not
>   two.
> * **Non-production / mechanism = 7.** The six knob-gated x86 SMP cells below, plus the one
>   moved at WA1-GATE. The six x86 SMP direct-IPC cells frozen by
>   `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL` are historical **mechanism** evidence. They were
>   re-earned at `7d5a22c9` after the three repairs in §0.1; re-earning preserves evidence and
>   **adds no new cell**, so they are counted once and only here.
> * **Historical total = 46.** 39 + 7, stated explicitly so the two policies cannot be
>   conflated. The total is unchanged by the gate — WA1-GATE reclassifies evidence, it does not
>   retract it, and it earns no new live cell.
>
> A previously-quoted figure of **43** matches neither policy: it requires counting the six
> knob-gated Stage 199 cells *and* excluding the two `ExitCurrentTask` cells.
>
> **Complete chronology.** "39 / 45" predates the `0b5ec254` production-default seal and was
> superseded **historically** by **40 / 46**. Stage 199D-WA1-GATE then disabled the x86 direct
> production default and **reclassified the current state to 39 / 7 / 46** — the one `0b5ec254`
> cell moved from current-production to non-production/mechanism evidence. The **historical
> total remains 46** throughout: no cell was ever retracted and none was newly earned.

### x86_64 ServerDies cell — evidence

Exact commit `f5669cb55325ac58aba6a15207a89c95ad8cad3d`, tree
`e2fd0b5c7a82dc6c8c422d5c6db242473533a9a6`. One fresh boot, 14215 lines, **zero
`result=fail`**, all eighteen forbidden markers absent, one boot banner.

* Scoped vector `[1, 1, 1, 1, 1, 1, 1, 1, 1]`, `result_before_enqueue=1`.
* Quiescent system balance `created=54 closed=54 live_links=0` — the same 54 that was
  previously reported as a leak.
* `EXIT_TASK_OWNER_REVALIDATED … prepared=idle committed=replacement next_tid=1 advances=1`
  — `revalidate_idle_owner_after_drains` executed in QEMU for the first time — and
  `EXIT_TASK_COMMON_EPILOGUE_OWNER … owner=replacement frame_committed=1`.
* `TERMINAL_CLAIM terminal=PeerDeath result=won` → `USER_VALIDATED result=ServerDied code=10`;
  survivor and health attested.

Full detail: `doc/IPC.md` §8.5.

### 0.1 x86_64 SMP=2 direct-IPC seal — reproduction status

The four-run `STAGE_199_X86_DIRECT_IPC_FINAL_SEAL` was earned at `ccceb03d`. It does **not**
reproduce at HEAD. Three independent defects were found; **one is repaired**, two are open and
deliberately not folded into that repair.

| # | Symptom | First bad | Status |
|---|---------|-----------|--------|
| 1 | RUN_C: `X86_AP_RECV_V2_VALIDATE_FAIL`; request-OK / user-validated absent | `458bb3d4` | ✅ **repaired** (`db783142`) |
| 2 | RUN_C: `X86_AP_RESCHEDULE_IPI_SENT sender_cpu=0 receiver_cpu=1` fires **54×**, seal requires 1 | `fcfc55e3` | ✅ **repaired** (`6784a3ae`) |
| 3 | RUN_D: reverse NR7 never completes — `IPCREPLY_DIRECT_SMP_REPLY_OK=0`, `timeout_before_completion` | `458bb3d4`-era transfer-cap decline vs. a malformed oracle NR7 | ✅ **repaired** (`7d5a22c9`) |

**Defect 1 (repaired).** `ipc_call` (NR6) sends `opcode = OPCODE_INLINE` with `FLAG_REPLY_CAP`,
which by the frozen recv-v2 contract makes the raw payload a **framed** message: first two bytes
are the inline application opcode, the rest is application data. Every legacy path stripped that
prefix; `458bb3d4` correctly converged the direct NR6 path onto the one canonical
`project_recv_delivery`. The x86 SMP oracle's CPU-0 client had **never framed its request** — it
staged eight bare bytes `NR6-REQ!`. Pre-`458bb3d4` the unstripped delivery meant the CPU-1 server
saw exactly those eight bytes and validated; afterwards they were correctly reinterpreted as
`opcode = 0x524E` plus a six-byte payload `6-REQ!`, so the server's ring-3 comparison failed.
**The kernel was right; the oracle was asserting pre-conformance framing.** The repair stages a
genuine two-byte inline opcode ahead of the payload (wire length 8 → 10) in both client stubs; the
CPU-1 server stub is unchanged. Boundaries 1–13 of the causal chain now all pass.

**Defect 2 (repaired).** First bad commit **`fcfc55e3`**. The candidate range toggles
`ipccall_direct_production_enabled()` on and off repeatedly, so the signal is non-monotonic and
`git bisect` is the wrong tool; testing the toggle points directly gives an exact correlation:

| commit | `ipccall_direct_production_enabled()` | IPI sent |
|---|---|---|
| `da9d26e2` | `false` | 1 |
| **`fcfc55e3`** | `cfg!(target_arch = "x86_64")` | **54** |
| `340f7822` | `false` | 1 |
| `c94cd304` | `cfg!(target_arch = "x86_64")` | **54** |

**54 = 53 ordinary local direct-NR6 completions + the 1 genuine CPU0→CPU1 oracle delivery.** The
post-transaction wake decision read a global oracle selector — a question that selector cannot
answer — and aimed at a hardcoded CPU 1. The real authority was absent:
`sr_enqueue_committed_receiver_split` computed the target CPU and discarded it. While the
production default was off, the oracle's own request was the only traffic reaching the drain, so it
fired once and looked correct. The repair makes the enqueue **return** its committed target,
carries it in `IpcCallDirectSuccess`, and decides the wake by comparing it to the enqueueing CPU —
so a local enqueue sends nothing regardless of any selector, and a real remote enqueue is woken on
its authoritative home CPU.

**Defect 3 (repaired).** RUN_D's first missing boundary was **#4, the direct NR7 eligibility
verdict**: the AP's `nr=7` never split-dispatched. Instrumentation named it —
`verdict=TransferCapUnsupported transfer_cap=true arg5=0x0`.

`SYSCALL_NO_TRANSFER_CAP` (`u64::MAX`) is the ONE encoding meaning "no capability"; every other
value — **including a raw `0`** — NAMES one (pinned by
`transfer_cap_arg_zero_is_not_treated_as_none`). The AP oracle server left arg5 at 0, so its
reply was cap-bearing. At `4605ebc7` the NR7 gate had no transfer-cap fact at all, so the
malformed argument was ignored and the reply was delivered — which is why the bidirectional seal
passed there. Once the Stage 199D transfer-cap safety increment correctly declined cap-bearing
replies, the reply fell to legacy, where capability id 0 fails to resolve, and RUN_D timed out.
Both NR7 sites now declare `SYSCALL_NO_TRANSFER_CAP`; the four bytes were freed via
`push imm8; pop reg` rather than inserted, so the stub length and every jump displacement are
unchanged.

Repairing that exposed **defect 2's mirror on the reverse path** — the reply drain also read a
global oracle selector and aimed at a hardcoded CPU 0, so the process manager's ordinary NR7 fired
a spurious reverse IPI. Fixed identically to the forward path: the reply transaction reports its
committed wake target and the drain compares it to the enqueueing CPU.

### 0.2 The seal reproduces

`STAGE_199_X86_DIRECT_IPC_FINAL_SEAL … result=ok` at exact clean commit `7d5a22c9`, all four
fresh runs from one commit with a clean-tree re-check after each:

* **RUN_A** feature-off core smoke, marker-clean;
* **RUN_B** `request=1 reply=1 server_wakes=1 caller_wakes=1 duplicate_reply=rejected`;
* **RUN_C** `AP saved-dispatch=1 request_user_consumed=1 no ring-3 fault`;
* **RUN_D** `request/reply cross-CPU=1, user-consumed both dirs, IPIs 1/1, continuations 1/1,
  dup refused, no fuse`.

Seal counters: `cross_cpu_request_smp2=1 cross_cpu_reply_smp2=1 request_user_consumed=1
reply_user_consumed=1 trap_depth_errors=0 wrong_current_task=0 duplicate_replies=0
duplicate_wakes=0 overwrite_fuse_trips=0`.

This **preserves historical Stage 199 evidence and adds no live cell** — the six cells remain
knob-gated and prove the 199D mechanism, not the production path.

All three defects are repaired and the four-run seal reproduces — see §0.2. Standalone RUN_C
reports `sent=1 received=1 request_ok=1 continuation=1 user_validated=1`; standalone RUN_D
reports `cross_cpu_request=1 cross_cpu_reply=1 duplicate_replies=0 result=ok`.

Unaffected and re-verified live at `db783142`: x86 production core boot, ServerDies
(`STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL … result=ok`), and the x86 reply-timeout retirement smoke.
The reply-timeout **matrix** fails only at its first AArch64 cell because `qemu-system-aarch64` is
not installed here; both x86 cells pass (`timeout_wins=1 reply_wins=1 feature_off_clean=2`).

### Immediate blockers

1. **AArch64 and RISC-V ServerDies live cells are unearned** — 1 of 3. The x86_64 cell is
   earned (`STAGE_200D2B1C_X86_64_SERVER_DIES_SEAL`, `f5669cb5`), which also **cleared the
   two blockers that used to head this list**: the `IPC_SERVER_DEATH_LINK_LEAK` accounting
   failure is resolved, and `revalidate_idle_owner_after_drains` has now executed in QEMU
   (`EXIT_TASK_OWNER_REVALIDATED … committed=replacement`).
2. **NR 6 / NR 7 off-lock direct IPC IS the x86_64 production default** (was: cannot be made production-default yet — two remaining
   correctness defects in the transaction body, not the gates.** The acknowledgement-store
   prerequisite *is* met (the bounded endpoint-indexed multi-pair store,
   `src/kernel/direct_ack_store.rs`, Stage 199D), **delivery conformance is now met**
   (defect A: the NR6 delivery projects through the one canonical receiver-visible
   projection in `src/kernel/syscall/ipc_recv_core.rs`, byte-identical to the legacy
   blocked-waiter delivery and proved by `stage199d_delivery_projection_differential` plus a
   re-earned live round trip), and the two enablement gates are mechanically easy to remove.
   **error disposition is now met** (defect B: one pure exhaustive mapping per direction in
   `src/kernel/direct_disposition.rs` — `Completed` / `DeclinedBeforeMutation` /
   `Failed(SyscallError)`, no wildcard arm, neither drain's `Result` discarded, with
   fault-injection and empirical legacy error-code parity for both copy faults), and
   **return-lane parity is now met** (defect C: the successful direct NR6/NR7 frame writes
   `ret2 = SYSCALL_NO_TRANSFER_CAP` through the shared encoder, byte-for-byte equal to a
   successful legacy `IpcCall` frame, attested live — `ret2=18446744073709551615 ret2_ok=1`).
   **No correctness defect remains**, and **mode eligibility + production counters have
   landed**: `src/kernel/direct_eligibility.rs` (pure exhaustive contract — `SEND` rights,
   current endpoint incarnation, `Buffered` only, `Synchronous` declines before mutation to
   the legacy rendezvous path; NR7 needs no mode) and `src/kernel/direct_ipc_counters.rs`
   (per-direction terminal buckets, ack lifecycle, occupancy high-watermark and every
   fail-closed fuse, balance proved live). **Both gates are now removed** — admission,
   eligibility and both acknowledgement publication sites consult arch-split predicates over
   one compile-time constant, with structural guards pinning that no proof-gate or
   oracle-endpoint reference survives on the x86 path, and AArch64/RISC-V unchanged. **The
   flip itself is HELD OFF on four newly-found live blockers.** Flipping
   `ipccall_direct_production_enabled()` to `cfg!(target_arch = "x86_64")` and booting a
   normal feature-off x86_64 image regressed the service chain: (i) the direct NR7 path never
   reads `SYSCALL_ARG_TRANSFER_CAP`, so a cap-bearing reply **silently drops the capability**
   (`PM_VFS_REPLY_FULL transferred_cap=0` → `PM_ELF_ZC_FAIL reason=grant_ro_unsupported` →
   blkcache / virtio-blk / driver-manager never spawn → boot times out); (ii) the
   acknowledgement store has **no production release path**, so every legacy-satisfied recv
   orphans a `Committed` slot; (iii) orphans trip the overwrite fuse — 17 trips on one short
   boot; (iv) capacity 8 is structurally too small for the number of servers blocked at once.
   The quiescent attestation added for this increment is what caught it
   (`IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL … result=fail`, `commit=4 consume=1 live=3`), and
   it confirmed the gate removal worked (`not_admitted=0`, `completed=1` on ordinary
   feature-off traffic). No production-default seal is issued; the oracle regression still
   passes (`live_cells=2 result=ok`) and the feature-off boot is healthy with the flip held
   off. **Three of the four blockers are now closed.** Transfer-cap safety: NR7 eligibility
   carries `transfer_cap_present` and a cap-bearing reply declines before any mutation to the
   legacy path, asked through the one canonical `transfer_cap_arg_present` predicate the legacy
   decode is built on (direct capability transfer stays unimplemented). Ack lifecycle: the lease
   is now owned by the endpoint waiter lifecycle — `DirectAckStore::release` is a fourth
   `Released` state and the non-direct terminal edge, exact in
   `{endpoint_index, endpoint_generation, waiter_tid, waiter_asid}`, centralized in the three
   `IpcSubsystem` waiter-removal primitives every canonical closing edge funnels through and
   called nowhere else; direct consume and non-direct release are mutually exclusive terminals
   proved by two 200-run races. **Live, feature-off x86 boot with the flip temporarily on: the
   service chain is fully healthy** (all 6 service entries exactly once, `PM_ELF_ZC_FAIL
   count=0`), the overwrite fuse went **17 → 0**, 52 NR6 / 64 NR7 leases were retired by their
   waiters, and 10 cap-bearing replies declined. **Capacity is the only blocker left:** the
   genuine post-release high-watermark is **8 = full capacity** with one `CAPACITY_REFUSED` per
   store, and the 8 live leases are *not* orphans —
   `reserve == consume + release + cancel + live` is exact both ways (113 == 53+52+0+8,
   113 == 41+64+0+8) — they are ten-odd resident services legitimately parked in recv-v2.
   Resizing was out of scope. Also corrected: the quiescent trigger moved to
   `INIT_IDLE_PARK_BEGIN` (the earlier one sampled `high_watermark=2` before saturation), and
   `live == 0` is not a valid quiescence requirement for a running microkernel — the verdict now
   requires `no_orphaned_lease`. **Blocker 4 is now closed too and the flip is ON.**
   `DIRECT_ACK_STORE_CAPACITY` is derived at compile time from `ENDPOINT_WAITER_SLOTS`, the
   authoritative endpoint receive-waiter table, with one slot per endpoint index — which makes
   endpoint uniqueness and the absence of capacity exhaustion structural, reduces reservation to
   a single compare-exchange, and removes the store's last lock. An independent waiter census
   (`src/kernel/direct_ack_census.rs`), unbounded by the store's capacity and running on split
   seams only, proves an exact lease/waiter bijection. **First production-default live seal:**
   normal feature-off x86_64 boot with `YARM_BOOT_OK`, all 6 service entries exactly once,
   `PM_ELF_ZC_FAIL count=0`, **53 NR6 and 41 NR7 ordinary syscalls completed off-lock with zero
   broad-lock entries**, zero capacity refusals, zero overwrite-fuse trips, zero stale/foreign/
   duplicate/crossed terminals, exact bijection both directions
   (`IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok`), plus the
   oracle regression (`live_cells=2 result=ok`). **No seal is issued and the constant is restored
   to `false`:** the ServerDies regression fails, because
   `SharedKernel::register_server_reply_link_split` — the direct NR6 reverse-link installation —
   does not stamp `note_link_created` while its legacy twin does, so with the direct path as the
   default the system-wide leak accounting sees `created=0 closed=13`. The links are installed
   and closed correctly (an instrumentation gap in the split twin, not a link leak), but while it
   is open the attestation that would detect a *real* reverse-link leak is blind on the
   production path. AArch64 and RISC-V are untouched and remain proof-gated. Full evidence is in
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.6–§6.1.10; see also `doc/IPC.md` §8.6.5–§8.6.8.
   **Blockers 6 and 7 are now closed and the x86_64 production default is ON.**
   All four reverse-link closing paths delegate to the one `close_server_reply_link` decision,
   so `links_created == links_closed` is a meaningful invariant on the production path. And
   terminal-arbitrated NR7 replies are explicitly ineligible: `DirectReplyFacts::terminal_arbitrated`
   is read from the authoritative `reply_terminal_ownership` cell, exact in record index AND
   generation under one rank-3 acquisition, and such a reply declines before any mutation so the
   legacy terminal lease can make it provably beat a concurrent timeout. Porting that lease into
   the direct transaction is future canonical **199E** work.
   **FIRST x86_64 NR6/NR7 PRODUCTION-DEFAULT LIVE SEAL, exact commit `0b5ec254`:** core boot with
   `YARM_BOOT_OK`, all 6 service entries exactly once, `PM_ELF_ZC_FAIL count=0`, **53 NR6 + 41 NR7
   ordinary syscalls off-lock with zero broad-lock entries**,
   `IPC_DIRECT_PRODUCTION_QUIESCENT_SEAL nr6_ok=1 nr7_ok=1 census_ok=1 result=ok` and waiter/lease
   bijection `result=ok`; oracle regression `live_cells=2 result=ok`; ServerDies vector `[1;9]`,
   `created=54 closed=54 live_links=0`, one PeerDeath winner and one caller wake; and both x86
   reply-timeout matrix cells with zero `[fail]` lines — reply-wins `reserve=1 commit=1
   rollback=0 deferred=0 arbitrated=1`, timeout-wins unchanged with `late_reply=rejected`. Zero
   fail/leak/duplicate/stale/fatal markers. The AArch64 and RISC-V matrix cells could not run
   (`qemu-system-aarch64`/`riscv64` absent here); neither architecture was changed and both
   remain proof-gated. Canonical 199D remains open — this is an increment, not a stage seal.
   **AArch64 NR6/NR7 is audited and NOT ready.** The canonical contract stack is already
   architecture-neutral (zero `target_arch` references across eligibility, disposition, the ack
   store, the census, the counters and the projection; the transaction body has two, both
   selector-gated x86 SMP IPI sends) and takes no broad lock — so no AArch64 semantic copy is
   needed. Three blockers remain, all in the AArch64 arch bracketing: (i) the syscall-ABI import
   admits NR6/NR7 only under the proof gate, so flipping the production predicate alone would be
   a silent no-op; (ii) **decisive** — `finalize_split_handled_syscall` calls `with_cpu`, so
   every HANDLED AArch64 split syscall reacquires the broad lock to save the user context,
   restore arch thread state and export x0..x5 (x86_64's finalize is an empty no-op); (iii)
   `d6_genuine_enabled()` is x86_64-only, so an AArch64 wake's downstream dispatch runs under the
   broad lock. Nothing was staged live; production default unchanged (x86_64 only).
   `qemu-system-aarch64` is also absent here. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.11 and
   `doc/IPC.md` §8.6.10.
   **AArch64 blockers (i) and (ii) are now CLOSED; (iii) remains open.** (i) The syscall-ABI
   import and its return-path twin now admit NR6/NR7 through the canonical
   `ipccall_direct_admission_enabled()`; no `ipccall_direct_proof_enabled()` call survives in
   `src/arch/trap_entry.rs`, so AArch64 carries no architecture-specific admission rule (the
   predicate is still `production || proof` and production is x86_64-only, so AArch64 still
   resolves to the proof gate — a normal boot is byte-identical). (ii) The `with_cpu` wrapper
   around `split_finalize_handled_syscall` is gone: the finalize is driven by an exact entering
   identity `{tid, asid}` captured *before* the split dispatch, and splits into frame-only work
   outside every lock plus two bounded rank-2 task-domain transactions (exact-incarnation TLS
   take, exact-incarnation context commit). The pre-export save → restore → read-back round trip
   was **proved redundant and removed** — `apply_user_context(capture_user_context())` is an
   exact nine-field identity and the post-export save overwrites it before anything observes it.
   Byte-for-byte preserved: success and error lanes, ELR/SPSR/SP and all user GPRs, x18 TLS,
   stale-identity behaviour and every existing AArch64 split class. Census: `trap_entry.rs`
   12 → 11, tree total 51 → 50, `AUDITED_WITH_CPU_TOTAL` 41 → 40, `CLASS_RUNTIME_REQUIRED`
   46 → 45; no new broad-lock site. (iii) `d6_genuine_enabled()` is unchanged and explicitly
   open — the sole remaining gating item. **The AArch64 production default stays OFF**; this is
   structural preparation only, with no AArch64 flip and no QEMU seal (`qemu-system-aarch64`
   still absent). See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.12 and `doc/IPC.md` §8.6.11.
   **AArch64 blocker (iii) is now CLOSED STRUCTURALLY; live acceptance is pending.** The
   authoritative queue-advancing dispatch — the step that picks the next runnable task and
   actually resumes it — is no longer reachable only through the x86_64-only
   `d6_genuine_enabled()`. Classification: **NR6** publishes exactly one typed,
   generation-bearing work item
   (`DirectDispatchWork { outgoing_tid, outgoing_asid, blocked_generation, cpu, class }`) at the
   reply-blocked commit, i.e. only after the caller genuinely left `current` and committed
   `Blocked(EndpointReceive(reply_cap))`; **NR7** publishes nothing — the replier stays
   `current`, the caller is woken once inside the transaction, and the replier returns through
   the narrow handled-return finalizer (enforced twice: a reply never reaches the publishing
   commit, and `try_publish` refuses the `IpcReply` class). Publication is single-shot per CPU
   and the drain takes the item destructively, so one item drives at most one dispatch. The
   drain runs with the broad guard dropped: revalidate the exact incarnation and committed
   blocked state (rank 2) → one authoritative dequeue (rank 1) → mark Running (rank 2) +
   current-set agreement → ASID/TTBR0 activation → complete EL0 frame, x18 TLS and any parked
   blocked-syscall completion → existing eret model, or the existing `idle_no_eret_loop()`
   primitive. It **reuses** the FutexWait/Yield rank-1 dequeue, rank-2 mark-Running seam and
   idle loop — one scheduler policy, not two — and differs only in taking **no broad lock**:
   what those drains get from a brief `with_cpu`, this gets from bounded rank-2 seams, each
   released before the next. Existing AArch64 FutexWait/Yield behaviour is unchanged. To avoid a
   `KernelState` mutation in the activation step, the HAL's active-ASID record moved out of
   `SelectedIsaHal` into a lock-free cell that `active_asid()` now reads — one authority, not
   two. Races are exhaustive and fail closed (`DrainOutcome`, no wildcard arm); no broad-lock
   fallback exists after a direct transaction has committed. `d6_genuine_enabled()` itself is
   byte-identical and still x86_64-only; AArch64 is admitted by the canonical replacement
   `offlock_authoritative_dispatch_enabled()`, which resolves to the armed proof/oracle gate
   there, so **the AArch64 production default stays OFF** and an ordinary AArch64 boot publishes
   and drains nothing. Broad-lock census **unchanged at 50**, with a new guard pinning "50 or
   fewer". No live seal — `qemu-system-aarch64` remains absent. See
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.13 and `doc/IPC.md` §8.6.12.
   **That landing had four defects, now repaired.** (a) The publication protocol was a single
   `PENDING` boolean conflating *being written* / *readable* / *being read*, correct only under
   an unstated non-reentrancy assumption — replaced by an explicit per-CPU state machine
   `EMPTY → WRITING → READY → READING → EMPTY`, where a publisher claims only `EMPTY`, a taker
   only `READY`, and the slot recycles only after the payload is copied out. (b) **The serious
   one:** the drain treated its pre-mutation revalidation as a verdict, so a caller that a reply
   or timeout had made `Runnable` caused it to return "declined" — `eret`-ing through a parked
   task's frame with `current` still `None`. The `current`-clear is now modelled as a **debt**:
   the revalidation is diagnostics only, and every taken debt settles as either `Dispatched` or
   `Idle`. After the dequeue mutates scheduler state, a later failure rolls back exactly
   (status, `current`, queue) and takes an explicit fatal path that never returns to userspace;
   the only no-debt exit is a superseded lease. (c) `tcb.blocked_recv_generation` is never
   incremented anywhere in the tree — always 0 — so the "generation-bearing stale-cycle
   protection" claim was withdrawn and replaced by a real per-CPU **dispatch lease**, a
   monotonic epoch opened at exactly one site (the `current`-clear commit). (d) `ACTIVE_ASID`
   was one global cell although `TTBR0_EL1`/`CR3` are per-core registers; it is now a per-CPU
   table keyed by `CpuId`, `Hal::switch_address_space` takes the `CpuId` explicitly, and
   `active_asid_on(cpu)` replaces `active_asid()`. Census unchanged at 50 / 40 / 45. Because the
   HAL authority changed globally, the x86_64 live core-boot and ServerDies regressions were
   re-run. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.14 and `doc/IPC.md` §8.6.13.
   **Blocker 5 (link CREATION accounting) is now closed** — both installation seams delegate to
   the one `install_server_reply_link` decision, so the creation stamp cannot drift; live,
   `created` went 0 → 54. That exposed its mirror on the CLOSE edge: of four close sites only two
   stamp `note_link_closed`, and the direct NR7 close (`unregister_server_reply_link_split`) is
   one of the silent ones, so with the flip on the totals read `created=54 closed=13` and
   ServerDies fails. Everything else about the flip is proven healthy at commit `c94cd304`. The
   constant is restored to `false`; the fix is the exact mirror of the creation one.
   **CANONICAL 199D CLOSURE AUDIT — `CANONICAL_199D_CLOSABLE=no`.** An audit increment with no
   runtime or semantic change. The live-evidence ledger is reconciled: the pre-production
   subtotal of **39** plus the one production-path increment earned at `0b5ec254` gave
   **40** production-path cells at the time; the six knob-gated x86 SMP mechanism cells
   (re-earned at `7d5a22c9`, adding no new cell) give **46** in total. (Stage 199D-WA1-GATE
   later disabled the x86 production default and reclassified the current state to
   **39 / 7 / 46**; the historical total stays 46.) The superseded "39 / 45" pair and the
   never-coherent "43" are retired, and `PROJECT_HISTORY.md` gains the previously missing
   `0b5ec254` row. The **executable closure matrix** (`stage199d_closure_matrix`, 12 tests)
   classifies **23 in-scope coordinates — 19 COMPLETE, 2 `STRUCTURALLY_COMPLETE`, 1 PARTIAL,
   1 OPEN** — plus **1 `DEFERRED_TO_CANONICAL_199E`, excluded from the tally**, with the verdict
   *computed* from the in-scope matrix rather than asserted beside it.
   **Evidence is bound to the coordinate it proves.** Checking that a marker literal exists
   somewhere in the tree is not evidence: it let `IPC_DIRECT_TRANSFER_CAP`, a transfer-cap
   counter dump, stand as proof for the reply-vs-timeout terminal race. Each entry now names the
   file *and the emitting function* whose body must contain the literal, plus the exact
   observation. The 199D safety coordinate — a terminal-arbitrated NR7 declines **before
   mutation** so the legacy lease wins the causal race — is COMPLETE on the causal set
   `IPC_DIRECT_PRODUCTION_QUIESCENT … arbitrated=1`, `IPC_REPLY_WIN_RESERVE` count 1,
   `IPC_REPLY_BEATS_TIMEOUT_OK` count 1, `IPC_REPLY_WIN_ROLLBACK` count 0 and
   `IPC_REPLY_TIMEOUT_DEFERRED` count 0. Porting the terminal lease *into* the direct transaction
   is **199E**, so it is typed `DEFERRED_TO_CANONICAL_199E` and can neither close 199D nor block
   it. Four in-scope blockers remain, in dependency order: (1) AArch64 off-lock NR6/NR7 +
   authoritative dispatch and (2) the AArch64 broad-lock-free handled-syscall return, both
   `LIVE_EVIDENCE_PENDING_AND_CONDITIONAL_PRODUCTION_ENABLEMENT` — not merely a missing
   emulator, since live evidence needs *proof/oracle QEMU → enable the AArch64 production
   predicate → normal feature-off boot + direct oracle + ServerDies + timeout regressions on one
   exact commit*, none of which this audit-only increment performs; (3) the AArch64 and RISC-V
   ServerDies live cells, `LIVE_EVIDENCE_PENDING`; and (4) RISC-V off-lock NR6/NR7,
   `CODE_THEN_ENABLEMENT_THEN_EVIDENCE` — a **separate four-link chain**, not the AArch64 gap:
   kernel target-spec/toolchain repair → off-lock NR6/NR7 code → production enablement → live
   NR6/NR7 and ServerDies evidence. Nothing in the list is a defect in the landed x86_64
   production path. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.15.
   **RISC-V chain link 1 is now CLOSED — target-spec only.** The custom kernel target declared
   `"llvm-target": "riscv64gc-unknown-none-elf"`; `riscv64gc` is a Rust *target-name* component,
   not an LLVM architecture, so LLVM 22 failed with `could not create LLVM TargetMachine for
   triple` and the RISC-V kernel target could not be configured at all. The accepted triple was
   derived from the toolchain — rustc's own built-in `riscv64gc-unknown-none-elf` Rust target
   declares `llvm-target: "riscv64"` — and the repair is one token, to
   `riscv64-unknown-none-elf`, the triple the sibling user target has always used. **Nothing else
   changed**: `+m,+a,+f,+d,+c`, `lp64d`, little endian, 64-bit pointers, static relocation,
   medium code model, max atomic width 64, panic abort and the existing linker script are all
   byte-identical, and the linked ELF is `EXEC` at entry `0x80200000` = `_start` with no
   interpreter, no dynamic section, zero undefined symbols, zero relocations and flags `0x5`
   (RVC + double-float ABI). The build path was **not** repointed — it already used the built-in
   target, and linking both ways yields identical entry, ELF flags and `PT_LOAD` layout.
   `stage199d_riscv_target_spec_guards` (8 tests) pins the triple, the ISA feature set and the
   ABI as three independent propositions, each mutation-tested, so the triple can never be
   "fixed" by dropping `+c`/`+f`/`+d` or switching `lp64d`. **Links 2–4 are untouched and
   coordinate 23 stays OPEN**; the tally and the 39 / 7 / 46 ledger are unchanged. No QEMU seal
   is required for a target-spec-only repair. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.16.
   **RISC-V chain link 2 is AUDITED, not closed — `RISCV_199D_READINESS=case_c`.** An audit only;
   no runtime code, production predicate or target spec changed. The question was whether an
   eligible RISC-V NR6/NR7 transaction can complete end-to-end without entering or re-entering
   the broad `KernelState` lock. **It cannot, even at SMP=1.** The architecture-neutral contract
   stack is inherited clean — `ipccall_direct_txn.rs` takes no broad lock at all, eligibility and
   disposition carry zero `target_arch` references, the ecall import covers `a7` + `a0..a5`, the
   transfer-cap lane is `a5`, `sepc` advances exactly once, the return lanes are the YARM ABI, and
   `tp` is mirrored back — and the RISC-V trap **wrapper**'s Phase-1 split return correctly skips
   the broad-lock phase. But the trap **bridge** that wraps it brackets *every* trap with three
   unconditional `with_cpu` acquisitions (entering identity, resume identity, SATP asid), so a
   **handled** direct transaction enters the broad lock three times regardless. Three blockers:
   (1) admission asks `ipccall_direct_proof_enabled()` rather than the canonical
   `ipccall_direct_admission_enabled()`, so enabling production alone is a **silent no-op**;
   (2) **decisive** — the three bridge acquisitions above; (3) no cross-hart wake authority (both
   sends are x86_64-cfg-gated and SBI exposes no IPI extension), latent only because RISC-V is
   BSP-only. **Not a blocker:** post-lock authoritative dispatch — neither NR6 nor NR7 clears
   `current` (NR6 is request-send-only and the caller blocks on a *later* recv; NR7's replier
   stays current), and the `current`-clear that owes dispatch lives in the AArch64-only recv-block
   commit. Waking a task is not switching to it. **Smallest next increment:** swap the bridge's
   three lookups to the already-existing architecture-neutral `current_tid_split_read` and
   `task_asid_for_tid_split_read` seams — a call-site swap, no new mechanism, no RISC-V semantic
   copy. Blocker 1 must not land first: admitting NR6/NR7 while the bridge still brackets the trap
   would claim off-lock NR6/NR7 while taking the broad lock three times per syscall.
   **Coordinate 23 stays OPEN**; tally and the 39 / 7 / 46 ledger unchanged.
   `stage199d_riscv_production_readiness_audit` (18 tests) pins all of it. See
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.17.
   **RISC-V readiness blocker 2 is now CLOSED — the decisive one.** The trap bridge's four
   broad-lock lookups (entering identity, the typed-idle invariant read, resume identity, and the
   SATP asid) are replaced by the existing narrow seams: `current_tid_split_read(cpu)` and
   `task_asid_for_tid_split_read(resume_tid)`. A call-site swap — no new mechanism, no RISC-V
   semantic copy. **A handled Phase-1 NR6/NR7 direct transaction now returns to userspace with no
   broad-lock acquisition at all.** Two things made this non-trivial. First,
   `current_tid_split_read` is annotated TRAP_FORBIDDEN for the x86_64 trap seam; the equivalence
   holds here because `with_cpu(cpu, ..)` rebinds `current_cpu` *before* reading, so both resolve
   to `current_tid_on(cpu)`, and on a BSP-only architecture whose bridge always passes
   `BOOTSTRAP_CPU_ID` the rebind is idempotent — a guard pins that premise and fails if RISC-V
   ever boots a second hart. Second, `task_asid_for_tid_split_read` reports both "no such TID" and
   "no address space" as `0`, where the broad-lock read returned `None` meaning *leave SATP
   alone*; the bridge translates `0 → None` explicitly, so a stale identity declines instead of
   installing address space 0. Snapshots are taken at the same program boundaries, SATP is
   selected from the exact resume TID, and the activation + `sfence.vma` ordering is untouched.
   **Census: 50 / 40 / 45 → 49 / 39 / 44.** `stage199d_riscv_narrow_trap_snapshots` (16 tests)
   proves the narrow snapshots match the old authoritative results for same-current,
   switched-current, replacement and no-current, proves the fail-closed asid translation, and
   pins that no broad-lock acquisition remains in the bridge. See
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.18.
   **RISC-V readiness blocker 1 is now CLOSED, and readiness recomputes to
   `RISCV_199D_READINESS=case_b`.** The RISC-V Phase-1 whitelist asked
   `ipccall_direct_proof_enabled()` *directly*, which made the RISC-V production predicate
   un-flippable in practice: with the proof gate off, `nr` never reached
   `try_split_dispatch_into_frame`, so enabling production would have been a **silent no-op**. It
   now asks the canonical `ipccall_direct_admission_enabled()`. **Behaviour-preserving today, by
   construction:** admission is `production || proof` and RISC-V production is
   `cfg!(target_arch = "x86_64")` — a compile-time false — so on RISC-V the canonical predicate
   reduces to *exactly* the proof gate the site used to ask. Selector off still declines NR6/NR7
   to the unchanged broad-lock path; selector on admits the same population; no ordinary
   feature-off traffic is newly admitted; x86_64 and AArch64 are untouched. All three admission
   questions now flow through the one helper — the ABI import is unconditional on the RISC-V
   bridge, whitelist admission is canonical, and handler reachability already was — and **no
   `ipccall_direct_proof_enabled()` call survives anywhere in `src/arch/`**. Neither predicate's
   implementation changed and no production default moved.
   **Recomputed:** blockers 1 and 2 closed ⇒ the **SMP=1/local path is structurally complete**;
   what remains is **blocker 3**, the absent cross-hart wake (both sends are x86_64-cfg-gated; SBI
   has HSM but no IPI extension) — which is case B by definition. **Coordinate 23 remains OPEN**:
   structural completeness is not production readiness, the remote-wake requirement is unresolved,
   the RISC-V production predicate is still false, and no live evidence is earned.
   **RISC-V QEMU revalidation — TAKEN.** `qemu-system-riscv64` was installed for the purpose
   (Ubuntu 24.04 `qemu-system-misc`, QEMU 8.2.2; `qemu-system-aarch64` deliberately not installed)
   and both runs were executed from a clean `c9840e0f` tree after a fresh artifact build.
   **The proof-gated direct smoke PASSES:** `STAGE_199_IPCCALL_REPLY_DIRECT_LIVE_SEAL arch=riscv64
   classes=2 live_cells=2 duplicate_replies=0 duplicate_wakes=0 result=ok`, with genuine NR6
   request and NR7 reply delivery, request/reply userspace validation, the deliberate duplicate
   NR7 refused (`dup_rejected=1 err=Err(WrongObject)`), `ret2` return-lane parity, both direct
   classes retired off-lock (`GLOBAL_LOCK_RETIRE_CLASS_DONE … class=IpcCallDirectRequest` and
   `class=IpcReplyDirect`) with **no** in-lock dispatch or fallback marker, all fuses zero, and
   the lease balance holding at the structural `capacity=256`. Feature-off is marker-clean, so
   selector-off NR6/NR7 stay on the legacy path; selector-on admits the same single oracle round
   trip as before `c9840e0f`. *sepc-advanced-once, tp/TLS and SATP preservation are attested
   indirectly* — no marker prints them — by both sides completing their round trip
   (`client_continuations=1 server_continuations=1`) with zero fault markers.
   **The stale harness blocker is CLOSED and BOTH RISC-V SMP=1 smokes now pass.** The feature-off
   core smoke had failed on `\bcapacity\b` in `REJECT_PATTERNS` — added at Stage 181 (`2a30515d`)
   beside `Vm\(Full\)`/`\boom\b` as an exhaustion proxy, when nothing printed the word benignly —
   colliding with the `capacity=256` / `ack_capacity=256` / `capacity_refused=0` reporters that
   Stage 199D (`fcfc55e3`) made unconditional. Capacity checking is **narrowed, not removed**: the
   bare word is replaced by the exact exhaustion forms the current emitters produce
   (`capacity_refused=[1-9][0-9]*`, `reason=capacity_exhausted`, `reason=capacity\b`,
   `reason=cow_capacity`, `reason=page_table_capacity`, `reason=user_vm_capacity`,
   `reason=deferred_capacity`, `IPC_RECV_REPLY_CAP_MATERIALIZE_FAIL`), plus explicit
   `kernel_error=CapabilityFull` / `TaskTableFull` — which the retired word match never covered,
   since they contain no "capacity". `tests/riscv_core_smoke_capacity_rejection.rs` (11 tests) is
   behavioural: it parses the script's own `REJECT_PATTERNS` and evaluates fixtures with `rg`
   exactly as the script does. **Feature-off core smoke PASSES** (`[ok] qemu-riscv64-core-smoke
   passed`, `YARM_BOOT_OK`, service chain up, expected `RISCV_KERNEL_IDLE_WAITING_FOR_IO`
   terminal, every exhaustion/fault/broad-lock predicate 0, and the direct-oracle markers
   **absent** as feature-off requires). **Proof-gated direct smoke PASSES unchanged**
   (`live_cells=2`, request/reply userspace validation, duplicate reply refused, both direct
   classes retired off-lock with zero in-lock dispatch, fuses clean). **Neither run adds a live
   cell**; the ledger stays 39 / 7 / 46, `RISCV_199D_READINESS` remains `case_b`, and coordinate
   23 remains OPEN solely on cross-hart wake, production enablement and production live evidence.
   **RISC-V blocker 3 audited — `RISCV_REMOTE_WAKE=D_REMOTE_ENQUEUE_UNREACHABLE_UNDER_CURRENT_TOPOLOGY`.**
   Audit only; nothing implemented, flipped or re-homed. The intended chain (committed
   `wake_target_cpu` → local/remote comparison → arch wake seam → SBI IPI → supervisor software
   interrupt → target trap entry → pending-bit ack → cross-CPU work consumption → dispatch → user
   continuation) has **only two of ten links present**: the wake-target comparison and the
   cross-CPU consumer. It does **not** fail at the transport — it fails at the first link. Live
   `-smp 2` evidence: hart 1 *is* started through SBI HSM (`YARM_RISCV64_SMP_HART_START hart=1
   ret=0 ack=1 state=parked_not_online`) and the DTB scan sees both harts
   (`present_cpus=2 present_bitmap=0x3`), but `RISCV_SCHEDULER_BSP_ONLY online_cpus=1
   reason=riscv_smp_scheduler_not_enabled` — hart 1 is **present and started but not
   scheduler-online**, parked in a `wfi` loop with an `stvec` pointing at that park, `sstatus.SIE`
   cleared, and no `sscratch`, `satp` or per-CPU binding. `sie.SSIE` is never set on either hart;
   the only bit the tree enables is `STIE`. There is no SBI IPI transport, and cause 1 has no
   decoder arm (only causes 5 and 9 are recognised), so a software interrupt would fall to
   `TrapEvent::Unknown`. Independently, **no RISC-V task is ever pinned to CPU 1** — the sole
   `set_task_home_cpu(.., CpuId(1))` caller is the x86 AP workload builder — so the committed wake
   target is always the enqueueing CPU and the remote branch is dead code. Firmware is *not* the
   constraint: OpenSBI v1.3 advertises `Platform IPI Device : aclint-mswi`. **Minimum needed: (d)
   a larger RISC-V SMP foundation**, not transport alone. **Smallest next increment:** bring CPU 1
   online in the RISC-V scheduler and give hart 1 a real trap vector — nothing else — with
   hard-stops on `probe_extension(0x735049)`, `-smp 1` byte-identity, no user code on hart 1, and
   a healthy service chain at `online_cpus=2`. `stage199d_riscv_remote_wake_readiness` (13 tests)
   computes the classification from architecture-scoped seam probes. Ledger unchanged at
   39 / 7 / 46; coordinate 23 stays OPEN. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.20.
   **Blocker-3 link 7 is now structurally CLOSED — the trap-ready parked secondary.** Hart 1 owns
   a valid kernel execution/trap context and parks with every interrupt admission disabled: a
   validated, atomically-claimed logical `CpuId(1)`; the boot hart's live `satp` captured from its
   CSR and installed with the required `sfence.vma` (no ASID allocated, so `Asid(0)` cannot be
   materialised); `sscratch` set to a **private** per-hart trap stack per the existing
   `csrrw sp, sscratch, sp` frame ABI; and the real `yarm_riscv64_trap_vector` installed **last**,
   only after identity, address space and trap stack are valid. All six markers report values
   **read back from the CSRs**. `sie` is cleared outright and `sstatus.SIE` stays 0.
   **The §6.1.18 narrow-snapshot premise was replaced, not deleted:** the bridge now *derives* the
   trapping CpuId from the frame pointer (frames land on the trapping hart's own trap stack), and
   the equivalence argument is re-made per-hart — `with_cpu(cpu, ..)` and
   `current_tid_split_read(cpu)` both resolve to `current_tid_on(cpu)` for whatever cpu is
   derived, and only the boot hart can reach the bridge while secondaries park interrupts-disabled
   and never enter userspace. A live defect was found and fixed en route: the secondary acked
   *before* emitting its markers, so the boot hart resumed mid-sequence and interleaved the shared
   SBI console — the ack now lands after the sequence, making `ack=1` attest "trap-ready and
   parked". Live: `-smp 1` unchanged with **zero** secondary markers and the direct-IPC smoke
   still `live_cells=2 result=ok`; `-smp 2` passes with `present_cpus=2`, each marker exactly once
   in causal order, `cpu=1`, `stvec` equal to the real vector, `sscratch` equal to the private
   trap-stack top, `sie=0x0 sstatus_sie=0 ssie=0 stie=0 seie=0`, **`online_cpus` still 1**, no
   user/scheduler/timer work on hart 1, a healthy boot-hart service chain and no unexpected trap.
   **Link 2 remains absent**, so `RISCV_REMOTE_WAKE` stays **D**, `RISCV_199D_READINESS` stays
   `case_b`, coordinate 23 stays OPEN and the ledger stays 39 / 7 / 46. See §6.1.21.
   **Chain link 2 is now CLOSED — CPU 1 is scheduler-online, WAKE-ONLY.** The pre-audit found the
   tree already represents the required state through the **generic** mechanism x86_64 (183.5) and
   AArch64 (195D) use — no hard-stop, no RISC-V-private scheduler, no second bitmap. The decisive
   fact is that `least_loaded_online_cpu` **skips wake-only CPUs outright**, so onlining does not
   make CPU 1 eligible for ordinary placement, and `dispatching = online & !wake_only` keeps user
   dispatch BSP-only. Wake-only is marked *before* onlining (no placement window), the idle current
   (tid 0) is installed, and `RISCV_SCHEDULER_SMP_ONLINE` is published only after the scheduler
   state **reads back** `present=1 online=1 wake_only=1` — a mismatch rolls back and reports
   instead. Registration is gated on the hart having acknowledged `TRAP_READY_PARKED`, and the
   secondary never calls the scheduler.
   **A latent link-7 defect surfaced here:** OpenSBI chooses the boot hart *nondeterministically*
   (one `-smp 2` run entered on hart 1), while the bridge always names the boot hart `CpuId(0)`.
   The mapping had assumed `hart_id == logical CpuId`, so secondary hart 0 claimed the boot hart's
   own logical id — and the duplicate check could not catch it because the claim word was
   initialised to `0` despite its comment saying bit 0 was pre-claimed. Logical id 0 is now
   genuinely reserved and secondaries take the lowest free id ≥ 1; verified across three `-smp 2`
   runs that booted on hart 0 *and* hart 1, `cpu=1` and `online_cpus=2` every time.
   Live: `-smp 2` passes with `present_cpus=2 online_cpus=2`, `wake_only=1 dispatchable=0
   user_dispatch=0 timer=0 queue=0 irq=0`, all six link-7 markers once and in order with
   read-backs unchanged, and **no `cpu=1` dispatch, user-entry, dequeue, timer or task-switch
   marker at all** — hart 1's only lines are its trap-ready sequence. `-smp 1` unchanged with zero
   secondary markers and the direct-IPC seal still `live_cells=2 result=ok`. The core smoke gate
   was updated (it hard-required `online_cpus=1` at any `-smp`) to expect
   `online_cpus == present_cpus` plus per-CPU non-dispatch assertions and a `-smp 1` marker ban.
   **Links 2, 3, 7, 9 present; 1, 4, 5, 6, 8, 10 absent.** The earliest missing link is now 1 — no
   RISC-V task is pinned to a non-boot CPU — so `RISCV_REMOTE_WAKE` stays **D**,
   `RISCV_199D_READINESS` stays `case_b`, coordinate 23 stays OPEN and the ledger stays
   39 / 7 / 46. See §6.1.22.
   **Chain link 1 HARD-STOPPED — no code written, link 1 remains ABSENT.** The pre-audit found
   conditions 1–4 satisfiable (the existing oracle already spawns a disposable child server that
   runs on CPU 0 and parks in the exact NR6 waiter state; `set_task_home_cpu` is arch-neutral; and
   `sr_enqueue_committed_receiver_split` would genuinely commit to CPU 1 now that it is online),
   but **condition 5 — safe retirement — is not**. Once the transaction commits, the task sits in
   CPU 1's runqueue and CPU 1 never dispatches. `RingQueue::remove_tid` is **private to
   `scheduler.rs`** and reachable only through `on_preempt_prefer`, which also dispatches; there is
   no `Scheduler`- or `KernelState`-level "remove this TID from that CPU's runqueue". The only two
   routes are excluded by the condition itself: dispatching it on CPU 1 *schedules* the task there
   (destroying the wake-only idle-current invariant, which `install_ap_idle_current` refuses to
   restore while a current exists) and leaves a Runnable-but-unqueued window; or adding a generic
   removal seam, which is new production scheduler surface. A proof that observed the commit and
   left the task parked on CPU 1 forever would violate the required "no leaked oracle task"
   evidence and was **not** fabricated. **The contract that must be split first:** a generic
   non-dispatching `Scheduler::withdraw_queued_tid_on(cpu, tid)` that removes the TID from that
   CPU's queues without touching `current`, dispatching, or altering TCB status —
   `RingQueue::remove_tid` already has the mechanics and compaction; only the non-dispatching path
   to it is missing. Splitting into link 1A/1B does not help: retirement blocks NR6 and NR7
   identically. Chain unchanged — links 2, 3, 7, 9 present; 1, 4, 5, 6, 8, 10 absent;
   `RISCV_REMOTE_WAKE` recomputes to **D**; `RISCV_199D_READINESS` stays `case_b`; coordinate 23
   stays OPEN; ledger stays 39 / 7 / 46; `probe_extension(0x735049)` still uncalled.
   See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.23.

   **The generic non-dispatching runqueue-withdrawal foundation is CLOSED — link 1 is still
   ABSENT.** This closes the contract split §6.1.23 named as link 1's prerequisite, and nothing
   more. Three `pub(crate)` levels — `PriorityScheduler::withdraw_queued_tid(tid)`,
   `SmpScheduler::withdraw_queued_tid_on(cpu, tid)` and a narrow `KernelState` wrapper — remove
   **exactly one** queued incarnation of a TID from a **named** CPU's runqueue and do nothing else.
   §6.1.23 sketched the return as `bool`; `bool` is genuinely ambiguous here, so the smallest typed
   outcome is used instead — `Removed` / `NotQueued` / `RefusedCurrent` / `RefusedDuplicate` /
   `InvalidCpu` — because a bare `false` would conflate four facts with four different correct
   responses. The current task is refused **before** any mutation (which is also what protects the
   scheduler-owned tid-0 idle current on a wake-only CPU); a duplicate occurrence **fails closed**
   with zero mutation, counted across all three priority queues first; an out-of-range or offline
   CPU is refused rather than retargeted. Removal delegates to the existing
   `RingQueue::remove_tid` compaction and the exact-one count reuses the ring's own `index`
   mapping, so **no queue algorithm is duplicated**; the one thing withdrawal adds is the
   membership-mirror update, which `remove_tid`'s only pre-existing caller (`on_preempt_prefer`,
   which moves the task queue → `current`) must *not* do. 21 focused tests cover each priority
   queue, head/middle/tail, wrapped compaction, the empty queue, the wrong CPU, current refusal,
   idle-current preservation, duplicate fail-closed, invalid CPU, unrelated FIFO order,
   online/present/wake-only bitmaps and both current slots. Structural guards prove the seam
   contains no dispatch or context-switch token, no task-state-mutation token, no policy token and
   no architecture-specific reference, and that it stays `pub(crate)`; each was mutation-tested and
   each asserts its own extraction is non-degenerate. `KernelState::withdraw_queued_tid_on` images
   the TCB `status` field's **raw bytes** before and after, for `Runnable`, `Blocked(Poll)`,
   `Blocked(Join)` and `Exited`. **Nothing is wired**: a source-tree walk proves the seam has no
   caller outside the scheduler, its wrapper and tests. Chain unchanged — links 2, 3, 7, 9 present;
   1, 4, 5, 6, 8, 10 absent; `RISCV_REMOTE_WAKE` stays **D**; `RISCV_199D_READINESS` stays
   `case_b`; coordinate 23 stays OPEN; **no live cell and no QEMU seal**; ledger stays
   39 / 7 / 46. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.24.

   **RISC-V chain link 1 (NR6) HARD-STOPS on wake-only placement — link 1 remains ABSENT.** The
   requested proof needs CPU 1 to be **online**, **wake-only** and holding the server **queued
   exactly once**, all at the same moment. Those three are mutually exclusive: `wake_only` *means*
   explicit placement is denied, which is the very property that made §6.1.22's onlining safe.
   The full choreography was implemented and booted at `-smp 2` — a source-only hard-stop would
   not have been trustworthy, because steps 1–5 and 7–9 all work and only a live run separates
   "the target was committed" from "the target was requested". The live log is decisive: the pin
   landed (`RISCV_REMOTE_ENQUEUE_SERVER_PINNED … home_cpu=1 target_online=1 target_wake_only=1
   result=ok`), the off-lock transaction completed (`IPCCALL_DIRECT_REQUEST_OK arch=riscv64 …`)
   and reported `wake_target_cpu=1` — but `SCHED_ENQUEUE_DENIED_WAKE_ONLY cpu=1 tid=10008
   reason=no_ap_dispatcher_yet` fired, so `queued_exactly_once=0` and the withdrawal that followed
   found `NotQueued`. The mechanism was **reverted in full** once it produced that evidence; a
   source-tree walk proves nothing of it survives. A **second finding**: the seam that reports the
   committed target, `sr_enqueue_committed_receiver_split`, documents its return as "the CPU the
   receiver was **actually enqueued on**" and that the two "cannot disagree" — it discards the
   enqueue's `Err` and returns the *requested* CPU regardless, driven and demonstrated in
   `the_committed_wake_target_can_report_a_placement_that_never_happened`. That is what made
   §6.1.23 score condition 4 YES; **that scoring is corrected to NO**. The defect is latent, not
   live, on x86_64 (its AP is dispatching, so the denial never fires) and is not repaired here.
   **The contract that must be split first:** either split `wake_only` into "excluded from
   balanced placement/dispatch" vs "may receive an explicit remote enqueue" (the small route, and
   exactly what a remote-enqueue proof needs), or land the AP dispatcher (Stage 183.6, which needs
   links 4, 5, 6, 8 and 10 too). Both are production scheduler policy, which the increment's own
   hard-stop list forbids. The §6.1.24 withdrawal foundation is not implicated — `NotQueued` on a
   genuinely-unqueued TID is its correct fail-closed answer — and remains unwired. **NR7 remote
   reachability is NOT live-proved**; the NR6 blocker applies to it identically. Chain unchanged —
   links 2, 3, 7, 9 present; 1, 4, 5, 6, 8, 10 absent; `RISCV_REMOTE_WAKE` recomputes to **D**;
   `RISCV_199D_READINESS` stays `case_b`; coordinate 23 stays OPEN; **no new canonical live cell**;
   ledger stays 39 / 7 / 46. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.25.

   **The false-success enqueue contract is REPAIRED.** §6.1.25's second finding, closed — no
   production predicate, no wake-only change, no RISC-V status change.
   `sr_enqueue_committed_receiver_split` now returns `ReceiverEnqueue::{Enqueued{cpu},
   Rejected{cpu,error}}` instead of a bare `CpuId`, carrying `SchedulerError` verbatim so the five
   distinctions stay distinct: `InvalidCpu`, `CpuOffline`, `WakeOnly`, `QueueFull`,
   `AlreadyQueued`. `WakeOnly` is new — it used to fold into `CpuOffline`, and "the target is
   down" versus "the target is up but refuses work" are materially different answers for a wake.
   The load-bearing rule is **structural**: `enqueued_cpu()` is the only accessor and returns
   `None` for every rejection, and both transactions bind through an `Enqueued` let-else, so a
   `wake_target_cpu` cannot be written down unless the rank-1 enqueue returned `Ok`. No success
   object exists on failure, and the drain's IPI and retirement marker both sit inside
   `if let Ok(success)`. **Route B (complete rollback), not a bare `Err`:** preflight admission was
   rejected because `QueueFull` is genuinely racy against other CPUs, so the real enqueue stays the
   authority. On refusal NR6 undoes the whole publication in reverse order — reverse link, record,
   reply cap, `Runnable → Blocked` (new `sr_uncommit_blocked_receiver_split`), waiter restore — and
   the ack lease is restored, so it is genuinely retryable; the end-to-end test proves that by
   removing the refusal and re-running the *same* transaction to success. NR7 is terminal instead:
   its enqueue sits after the one-shot `consume_reply_record_split`, which must never be re-armed,
   so the record stays `Consumed` (the same terminal `CallerGone` uses) while the caller returns to
   `Blocked` with its waiter — leaving its completion to the existing reply timeout, which the old
   Runnable-but-unqueued state made impossible. The NR6 reverse-link-failure arm had the identical
   gap and now shares the same rollback. Twelve focused tests drive the real seam for every
   distinction plus both end-to-end rollbacks; the regression test reproduces `ca55400b` exactly.
   Three guards pinning the old contract were **updated, not deleted** —
   `a_stale_home_cpu_fails_closed` had asserted `assert_eq!(target, bogus)` beside "and nothing is
   queued there", the defect written down as if correct. RISC-V status recomputes unchanged: link 1
   ABSENT, links 2/3/7/9 present, 4/5/6/8/10 absent, `RISCV_REMOTE_WAKE` **D**,
   `RISCV_199D_READINESS` `case_b`, coordinate 23 OPEN, ledger 39 / 7 / 46, no new live cell, and
   the withdrawal foundation still unwired. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.26.

   **The two unsound enqueue-REJECTION contracts §6.1.26 shipped are REPAIRED.**
   **(A) `AlreadyQueued` is not "nothing is queued".** `Rejected` had documented itself as "the
   receiver is in no run queue" — true for `InvalidCpu`/`CpuOffline`/`WakeOnly`/`QueueFull`, which
   all fail before touching a queue, but false for `AlreadyQueued`, which reports *pre-existing*
   membership. And because `contains_tid` reads the membership mirror, which tracks the queues
   **plus the dispatched `current` task**, `AlreadyQueued` can mean the receiver is **executing**.
   The ordinary rollback would then have produced a `Blocked` task that is still queued or current.
   Fixed three ways: the reason now survives into the transaction error
   (`EnqueueRejected(SchedulerError)`, not one information-free variant); on `AlreadyQueued` the
   seam reconciles membership via `withdraw_queued_tid_on` **inside the same
   `with_scheduler_split_mut` closure** that detected it — one acquisition only, never through
   `self` — and carries the `WithdrawOutcome`; and only `Removed` (an atomically removed
   exactly-one queued entry, by construction not `current`) may enter the TCB rollback.
   `RefusedCurrent`/`RefusedDuplicate`/`NotQueued`/`InvalidCpu` fail closed via
   `EnqueueRejectedUnreconciled`, reclaiming the authority while making **no** restoration claim.
   The hard-stop window is *closed*, not argued: both transactions now run a pre-commit membership
   preflight (NR6 9c, NR7 5c) — a still-`Blocked` receiver with its waiter exclusively claimed
   cannot legitimately hold membership and nothing can wake it, so the check does not race — and
   decline before the first irreversible mutation.
   **(B) A direct-eligible NR7 has no timeout owner.** §6.1.26 justified leaving the caller
   `Blocked` with the record `Consumed` by appealing to "the existing reply timeout". That was
   false for exactly this population: `classify_direct_reply_eligibility` declines
   `terminal_arbitrated` replies **before any mutation**, and that flag *means* a reply timeout is
   armed — so every direct-eligible reply is untimed and the caller was stranded with no terminal
   owner. The claim is deleted and **route A** implemented:
   `restore_consumed_reply_record_split` returns the record `Consumed → Available` only at the
   exact generation, bound to the exact replier `{tid, asid}`, and only from `Consumed`; the
   reverse link the consume closed is re-registered; the ack lease is restored. Re-arming happens
   only when the receiver is provably unplaced. Proved end-to-end for each reachable reason:
   `Blocked` on the **exact original recv cap**, waiter restored once, neither queued nor current,
   no success/marker/IPI, no cap/record/link leak — NR6 by re-running the same transaction to
   success, NR7 by the restored authority retrying, succeeding exactly once, and a duplicate
   remaining rejected. Three mutations were run and all three now fail behaviourally (the
   reverse-link one was structural-only until a link-count assertion was added). RISC-V status
   recomputes unchanged: link 1 ABSENT, 2/3/7/9 present, 4/5/6/8/10 absent, `RISCV_REMOTE_WAKE`
   **D**, `case_b`, coordinate 23 OPEN, ledger 39 / 7 / 46, no new live cell. The withdrawal
   foundation is now consumed by exactly one caller — that reconciliation — and by no oracle,
   no RISC-V path and no link-1 work. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.27.

   **Three rejection-safety defects found in review of §6.1.27 are REPAIRED.**
   **(1) Membership detection moved before user-visible mutation.** The preflight had sat *after*
   NR6's record reservation, provisional reply-cap mint into the **server's own cnode**, and user
   copy — and after NR7's record reservation and caller copy. A receiver reported `RefusedCurrent`
   may already be executing and may already have read those bytes, so a check there cannot support
   retry or authority restoration. It now runs at NR6 (4a) and NR7 (2b), before any user copy, any
   provisional capability in the receiver's cnode, any record state exposed to another
   transaction, any waiter claim and any TCB mutation. A `Blocked` receiver with a committed waiter
   cannot acquire membership, so an early positive is an **invariant violation**: no mutation, the
   acknowledgement **discarded** (never re-armed), typed `ReceiverMembershipViolation`, and no
   claim that the task was restored or unplaced. The post-copy defence stays for genuine
   violations, classified by the same-acquisition `WithdrawOutcome`, and **no post-copy membership
   detection returns retryable authority** — NR6 settles the lease and NR7 restores the authority
   only when `reconciled.is_none()`. `direct_server_exact_still_blocked` /
   `direct_caller_exact_still_blocked` now also require the absence of scheduler membership:
   `Blocked` plus an intact waiter is not sufficient when the task is queued or current.
   **(2) The NR7 authority restore is all-or-nothing.** It had published `Consumed → Available` and
   only then attempted registration, permitting `Available`-without-link. It is now one composed
   transaction: task rank 2 taken first and held throughout, ipc rank 3 nested inside (ascending
   order); the link slot is validated **without writing**, then the record is validated and
   flipped, then the link is installed. Only two outcomes are observable — record `Available` with
   the exact link, or record `Consumed` with no new link. The revert is exercised by a
   `#[cfg(test)]`-only fault hook that forces the install to fail after the flip. Five failure
   cases each proved to leave outcome B: occupied slot, changed replier incarnation, recycled
   generation, already-`Available`, `Cancelled`.
   **(3) The hidden shared-region side effect is gone.** The reconciliation had lived in the seam
   the shared-region finalizer also calls, so that caller silently withdrew a pre-existing entry it
   had no rollback for. Option A: `sr_enqueue_committed_receiver_split` never reconciles;
   `sr_enqueue_committed_receiver_reconciled_split` is direct-IPC-only with exactly two call sites.
   Not a flag — the finalizer cannot select it because it calls the other function. Its rejection
   contract is repaired: no more `Some(true)` after a refusal; it restores its own receiver on the
   four never-touched-a-queue reasons and reports `None`; an unreconciled `AlreadyQueued` fails
   closed with **zero mutation**. Behavioural tests cover `WakeOnly`, `QueueFull` and
   `AlreadyQueued` as exactly-once/current/duplicate, each proving pre-existing membership is
   untouched. RISC-V status recomputes unchanged: link 1 ABSENT, 2/3/7/9 present, 4/5/6/8/10
   absent, `RISCV_REMOTE_WAKE` **D**, `case_b`, coordinate 23 OPEN, ledger 39 / 7 / 46, no new live
   cell. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.28.

   **`WAITER_OWNERSHIP_EXCLUSIVE=no` — the waiter-claim reorder is HARD-STOPPED; the
   `Removed`-is-recoverable repair is delivered.** Both the NR6/NR7 reorder and the shared-region
   publication lease rest on one premise: that owning the exact endpoint-waiter claim excludes
   every other wake owner between the claim and the commit. The required audit refutes it. Six
   owners were enumerated and classified from named seams; four arbitrate (endpoint send and the
   direct transactions via the waiter table; server-death and the token-bearing reply timeout via
   the terminal cell **and** the waiter). **Two do not.** The **ordinary IPC timeout scan** sets
   `Runnable` for any `Blocked(EndpointReceive)` with an expired deadline in Phase 1 (rank 2) and
   clears the waiter slots only in Phase 2 (rank 3) — the wake strictly precedes the invalidation,
   so an owned claim is never consulted. The **notification signal wake** takes a TID out of
   `notification_waiters` guarded only by `matches!(tcb.status, Blocked(_))` — true for our
   receiver right up to commit — and never reads `endpoint_waiters` at all. Today's direct-eligible
   populations are narrower than the mechanism (the only `ipc_timeout_deadline` arm site coincides
   with `terminal_arbitrated`, which NR7 declines pre-mutation, and an NR6 server is not a reply
   caller), but that is a cross-subsystem *argument*, not arbitration — exactly what may not be
   relied on. So no reorder was performed: one claim per transaction, still at NR6 (9) / NR7 (5),
   with §6.1.28's pre-mutation membership checks retained and correctly described as TOCTOU
   preflights. **Part C is hard-stopped identically** — building the publication lease would encode
   the same false exclusivity into a third subsystem. **The contract that must be split first:**
   either reorder the timeout scan to invalidate the waiter before waking and give the notification
   wake a waiter-claim check, or introduce a per-task wake-arbitration token every owner must claim.
   **Delivered: same-acquisition `Removed` is recoverable.** §6.1.28's "every `reconciled.is_some()`
   is terminal" was over-broad — `Removed` proves exactly one queued entry was withdrawn under the
   detecting acquisition and the task was not `current`, so the publication was never observed.
   Both directions now use the single predicate `receiver_is_unplaced()`; terminal stays
   `RefusedCurrent` / `RefusedDuplicate` / `NotQueued` / `InvalidCpu`, and a variant documented
   retryable is never returned after its lease or authority was discarded. Exercised end-to-end via
   a `#[cfg(test)]`-only post-copy membership hook: NR6 restores and retries once; NR7 restores the
   caller, the exact waiter, the record `Available` at the same generation and the exact replier
   reverse link, retries once and still rejects a duplicate, with **no timeout dependency**. The
   accepted §6.1.28 composed restore is preserved. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.29.

   **WA1-GATE: the x86 direct production default is OFF, and §6.1.29's reachability claim is
   RETRACTED.** That claim ("today's direct-eligible population is narrower than the ordinary
   timeout mechanism") rested on a grep for `ipc_timeout_deadline = Some(...)`, which found only
   the reply-receive timeout. A complete audit of `ipc_timeout_deadline\s*=` finds **seven**
   assignments, three of which arm an ordinary deadline by assigning a variable —
   `recv_block_phase_b_task` (`Blocked(EndpointReceive)`), its send-block twin
   (`Blocked(EndpointSend)`) and the queued-recv block path — **none** with a
   `reply_timeout_token`. So ordinary recv/send deadlines are independent of the reply-terminal
   arbitration that gates direct NR7 eligibility, `process_ipc_timeout_deadlines` can genuinely
   race an endpoint-blocked receiver mid-publication, and this is a **reachable production safety
   issue**, not a mechanism-level concern. `WAITER_OWNERSHIP_EXCLUSIVE` stays **no**.
   `ipccall_direct_production_enabled()` therefore returns `false` on every architecture; its
   body is exactly `false`, with no `target_arch`, `cfg!`, `||` or atomic load that could
   silently restore it. Admission is unchanged in form and is now exactly the proof gate: with
   the selector clear, ordinary NR6/NR7 reach neither the direct transaction nor the
   blocked-waiter acknowledgement and fall back to the legacy path; with it set, both are
   reachable. Every explicit proof/oracle selector survives verbatim.
   `AlreadyQueued + Removed` now fails closed on every freestanding runtime build including the
   proof kernels — `Removed` proves current queue state, not historical non-observation — while
   hosted `cfg(test)` builds keep exercising the rollback algebra. The `0b5ec254` seal is **not**
   re-emitted with changed semantics; a distinct `IPC_DIRECT_PRODUCTION_DISABLED_SEAL` is added,
   computed from the authoritative `REQUEST.completed` / `REPLY.completed` counters rather than
   inferred from absent user logs. Ledger reconciled to **39 / 7 / 46** (exactly one cell moved;
   no new live cell). Canonical 199D **OPEN**; waiter-claim-aware timeout arbitration and
   generation-bearing notification arbitration **not implemented**; RISC-V links/status
   unchanged. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.30.

   **WA2A: the generation-bearing waiter-ownership primitive exists, helper-only.** A mechanically
   gathered census of **15** production paths that install, replace, remove or clear an endpoint
   receive waiter, or move an endpoint-blocked task out of `Blocked`, confirms the two exclusivity
   breaks named above — `process_ipc_timeout_deadlines` (wakes at task rank *before* clearing
   waiters at ipc rank) and the notification signal wake (reads a different table entirely) — plus
   three index-only takers (`wake_waiter_for_endpoint`, `ipc_reply`, the shared-region finalize)
   that cannot tell a replacement waiter from the one they meant. `WaiterOwnershipTable` is the
   single bounded, allocation-free typed state machine those paths can later route through: its key
   is exact in four dimensions (endpoint index **and** generation, waiter tid **and** asid, plus the
   blocked-receive generation) where the waiter table is exact in two, so a recycled endpoint slot,
   a reused TID under a new ASID and a task that reblocked are three different keys. State is
   `Available → Claimed{owner, claim_generation} → Consumed | Cancelled`, never a bool; the six
   owners (`DirectRequest`, `DirectReply`, `OrdinaryTimeout`, `LegacyDelivery`, `Notification`,
   `Teardown`) are named but **none is wired**. The module acquires no lock at all — the caller
   supplies the rank-3 guard — so it structurally cannot nest task(2) or scheduler(1) beneath ipc(3),
   and the returned claim token is `Copy` and outlives the guard. Restoration validates the full key
   *and* the owner *and* the claim generation, so a stale token is rejected even when the same owner
   re-claims; that case was found by mutation M2 surviving, and is recorded rather than quietly
   repaired. **Nothing else moved:** diffing the defined-symbol sets of the freestanding
   `x86_64-yarm-none` `libyarm.rlib` before and after gives 0 symbols removed, 0 changed and exactly
   2 added — both never-called constructors inside the new module — which is why no QEMU run was
   required. `WAITER_OWNERSHIP_EXCLUSIVE` remains **no**, the x86 direct production default remains
   **OFF** on every architecture, NR6/NR7 keep their single late waiter claim, canonical 199D stays
   **OPEN**, the ledger stays **39 / 7 / 46**, RISC-V links/status are unchanged and **no new live
   cell** is earned. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.31.

   **WA2A-R1: the ownership foundation is repaired, and the exclusivity break is WIDER than
   reported.** Four defects in the WA2A primitive are fixed. (1) The associative 64-slot table
   leaked capacity across *lifetime*, not size: because a key carries the blocked-wait generation,
   64 sequential completed waits exhausted it with zero live claims. It is now endpoint-indexed —
   `WAITER_OWNERSHIP_SLOTS = ENDPOINT_WAITER_SLOTS`, derived and pinned by a compile-time
   assertion exactly as `DIRECT_ACK_STORE_CAPACITY` is — so a finished incarnation holds its slot
   only until the next incarnation of that endpoint claims it; three 10 001-cycle tests
   (claim/restore, claim/consume, claim/cancel) confirm it never exhausts, a live claim is never
   evicted, and an out-of-range index is a typed fail-closed error. (2) The table became a private field of
   `IpcSubsystem` (`waiter_ownership_stores = 1`) with every raw method module-private, so
   ownership cannot be *operated* through the task, scheduler, capability, VM or broad-state APIs
   at all. (R2 corrects the wording that called a boot-domain reference to it "inert": a boot
   sibling cannot call a method on the table but could still replace it wholesale, so the accurate
   description is rank-3 co-location plus source-guarded encapsulation.) (3)
   The claim token is opaque — private fields, no forgeable struct literal, no exposure of the
   live claim generation — and `wrapping_add` is replaced by `checked_add` with a typed
   `ClaimGenerationExhausted` that leaves both the slot and the counter untouched, so an ancient
   token can never be made valid again.

   (4) **The census is narrowed honestly.** The 15-row table is relabelled a *waiter-primitive
   callsite census*: it was collected by grepping the four waiter primitives, so by construction
   it could only find paths that touch a waiter — and the dangerous owners are the ones that do
   not. An independent pass starting from task status instead is mechanically complete as an
   *enumeration*: `status` is a plain TCB field with no aliasing writer (no `&mut …status`, no
   whole-TCB overwrite, no `mem::replace`/`swap`, no production TCB removal), so the 37
   status-assignment sites across eight files are the closure of "moves a task out of `Blocked`",
   and a guard pins the per-file counts. Twelve of them CAN act on `Blocked(EndpointReceive)` —
   **seven more than the callsite census knew about**, including the generic
   `wake_tid_to_runnable`, `wake_destroyed_notification_waiter`, `apply_cross_cpu_wake_task`,
   `sr_wake_receiver_split`, `exit_task`, `mark_task_dead` and `reap_faulted_task_noalloc_cleanup`,
   each reaching a `Blocked(_)` task by numeric TID (or unconditionally) while consulting no
   endpoint waiter. Four are provably out of reach from the source; twelve assign a status with no
   guard on the previous one, so their negative rests on a dynamic invariant the source does not
   establish, so at WA2A-R1 the verdict was recorded as incomplete rather than as an unsupported
   exhaustive claim. **WA2B-CENSUS resolves all twelve** — see below.

   Still helper-only: zero production call sites, neither late waiter claim moved, no timeout,
   notification, shared-region or teardown path converted. `WAITER_OWNERSHIP_EXCLUSIVE` remains
   **no**, the x86 direct production default remains **OFF** on every architecture, canonical 199D
   stays **OPEN**, the ledger stays **39 / 7 / 46**, RISC-V links/status are unchanged and **no new
   live cell** is earned. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.32.

   **WA2A-R2: the primitive rejected stale tokens but ACCEPTED stale claim requests.** R1's
   `claim` installed a key whenever it differed from the terminal one, reading "different key" as
   "a newer incarnation is taking over". A key states which incarnation, never when, so
   `claim A → consume A → claim B → consume B → delayed claim A` minted a fresh token for the
   older, already-finished incarnation A. Reproduced against `e3e5de91` before the repair.

   `claim` no longer installs a key at all. A slot is armed for exactly one current incarnation by
   `arm_current` — which the eventual authoritative waiter-publication path will call under the
   same ipc rank-3 acquisition that installs the receive-waiter — and released by
   `retire_current`; `claim` succeeds only from `Available` holding that exact key, `restore`
   returns to `Available` rather than `Vacant` (unarming there would reopen the same hole through
   the rollback path), a live claim can be neither armed over nor retired nor evicted, and a stale
   arm or retire can neither erase nor displace the current incarnation. The table stays bounded
   and leak-free with an **explicit obligation**: a terminal slot blocks its endpoint index until
   retired — fail-closed, but a liveness duty the wiring increment inherits. Three 10 001-cycle
   tests assert nothing is left occupied.

   The view is truthful now too: R1 reported `Vacant` both for an out-of-range endpoint index and
   for a key different from the one occupying the slot, each of which implied a claim would
   succeed. It distinguishes `EndpointIndexOutOfRange`, `Vacant`, `Available`, `Claimed{owner}`,
   `Consumed`, `Cancelled` and `ForeignIncarnation{holding}`, carries no claim generation, and a
   test walks every state asserting that whenever the view is not `Available`, `claim` fails.

   The encapsulation claim is corrected rather than restated: a boot sibling cannot call any
   method on the table, but the field and `vacant()` are visible within `crate::kernel::boot`, so
   it could replace the whole table by assignment, `mem::replace`/`swap` or a raw pointer write.
   Route 2 is taken — **rank-3 co-location plus source-guarded encapsulation, not complete
   type-system-enforced inertness** — and a guard rejects every one of those forms outside the
   ownership module, with a non-vacuity check and a positive control that only the declaration and
   the single initializer name the field.

   Sixteen mutations, all caught (the ten from R1 re-run, plus six for the lifecycle). Still
   helper-only with **zero production callers**; neither late direct waiter claim moved.
   `WAITER_OWNERSHIP_EXCLUSIVE=no` (census completeness was still open here; WA2B-CENSUS below
   raises it to `yes`), x86 direct production
   default **OFF** on every architecture, canonical 199D **OPEN**, ledger **39 / 7 / 46**, RISC-V
   links/status unchanged, **no new live cell**. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.33.

   **WA2B-CENSUS: the wake-owner census is complete, and the answer is worse than hoped.** All
   twelve rows WA2A-R1 left unproven are resolved, and **nine resolved against the comfortable
   answer**. `dispatch_next_task`, both halves of `yield_current` and `yield_current_to`,
   `d6_genuine_mark_running_via_task_seam` and `direct_dispatch_rollback_split` are all **CAN**,
   because `crates/yarm-kernel/src/scheduler.rs` contains *zero* occurrences of `TaskStatus` — the
   run queue carries bare TIDs with no status precondition, and no status is read between the
   dequeue and the `Running` write. `fault_current_task_with_fault` is CAN because it selects by
   `current`, never by status. `spawn_user_task_from_image` is CAN because `spec.tid` is
   caller-supplied at 24 sites and `register_task_with_class` is *idempotent*, so an existing —
   possibly endpoint-blocked — TID passes straight through to the `Runnable` write. Only three
   resolved to CANNOT, each closed locally: the AP client spawn sits inside
   `task_status(client_tid).is_none()`, and `spawn_user_thread` and `fork_complete_post_clone`
   bind their TID from `allocate_thread_id`, which returns a candidate only where
   `task_status(candidate).is_none()` and otherwise fails closed.

   The verdict is **computed, not asserted**: a guard extracts `(file, enclosing fn, count)`
   mechanically for all 37 sites and compares it against the classification table, so an
   unclassified writer cannot exist. **CAN 21 / CANNOT 7 / INTO_BLOCKED 7 / FRESH_CONSTRUCTOR 1 /
   NON_PRODUCTION 1 / UNPROVEN 0 = 37**, giving **`WAITER_OWNER_CENSUS_COMPLETE=yes`**. No runtime
   code was changed to reach that verdict. Every CANNOT pins both its guard and its caller
   closure, and both closure shapes are local — a value-level filter over every TCB, or a TID
   bound inside the function behind a fail-closed check — so there is no "helper trusted by its
   callers" anywhere in the set.

   The owner/origin matrix (design only) is repaired by WA2B-MATRIX-R1 below; its first version
   conflated writer sites with logical origins.

   Also repaired: the `WaiterOwnershipView` contract. R2 said `Available` meant a claim would
   succeed; with the generation counter saturated an `Available` slot rejects with
   `ClaimGenerationExhausted`. The implementation is unchanged and correct — only the contract was
   overstated. `Available` now means armed-and-unclaimed and is the **only slot state structurally
   eligible** for a claim, and the replacement test proves all three parts including the exhausted
   case, which stays `Available` and mutates nothing.

   **`WAITER_OWNERSHIP_EXCLUSIVE` remains `no`.** Completing the census says who the owners are; it
   does not make them arbitrate — not one of the 21 routes through the primitive. Still
   helper-only with zero production callers; neither late direct waiter claim moved. x86 direct
   production default **OFF** on every architecture, canonical 199D **OPEN**, ledger
   **39 / 7 / 46**, RISC-V links/status unchanged, **no new live cell**. See
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.34.

   **WA2B-MATRIX-R1: the owner matrix was wrong in five ways, and the census guard was too
   coarse.** Documentation and `cfg(test)` only — production executable code is byte-identical to
   `213bb4e4`, and the accepted verdict (CAN 21 / CANNOT 7 / INTO_BLOCKED 7 / FRESH_CONSTRUCTOR 1 /
   NON_PRODUCTION 1 / UNPROVEN 0, `WAITER_OWNER_CENSUS_COMPLETE=yes`,
   `WAITER_OWNERSHIP_EXCLUSIVE=no`) is unchanged.

   *Writers vs origins.* The first matrix put a "eight status-writer sites" heading over ten
   *path* rows and listed `ipc_reply` as a direct caller of `wake_tid_to_runnable` when the real
   chain is `ipc_reply` → `apply_scheduler_wake_plan` → `wake_tid_to_runnable`. The two layers are
   now separate: the 21 writer sites, and the exact direct production caller set of every helper
   writer — `wake_tid_to_runnable` 3, `apply_scheduler_wake_plan` 11,
   `apply_split_receiver_wake_plan` 5, `wake_waiter_for_endpoint` 3, `apply_cross_cpu_wake_task` 1,
   `rt_commit_receiver_runnable` 2 — each pinned by a guard so a new caller class forces the matrix
   to be re-derived.

   *Terminal origins.* `rt_commit_receiver_runnable` has two callers carrying **different**
   terminal claimants: `complete_reply_timeout_over` (`TimedOut`, `TerminalClaimant::Timeout`) and
   `complete_server_death_over` (`ServerDied`, `TerminalClaimant::PeerDeath`). Mapping ServerDied
   onto `OrdinaryTimeout` was wrong. Both callers reach the writer having **already won** the
   reply-terminal cell, so the open question is how an already-won terminal claimant translates
   into waiter ownership; `WaiterOwner` has no variant for it, recorded as a prerequisite. The enum
   is **not** changed here.

   *Notification is not an endpoint owner.* `signal_notification` and
   `wake_destroyed_notification_waiter` take a bare TID, guard only on `Blocked(_)`, and never read
   `endpoint_waiters`. The earlier matrix said they should claim and **cancel** the endpoint slot —
   which would let a stale notification destroy a live endpoint wait, turning a lost notification
   into a lost IPC reply. Retracted. Both move into the production-enforced refusal set, with the
   required repair recorded: generation-bearing notification waiter identity, a
   notification-specific blocked reason, stale → clear or ignore, and never consume, cancel or
   retire an unrelated endpoint waiter. Whether `WaiterOwner::Notification` becomes obsolete after
   that is recorded as open, and the variant is kept.

   *Three origins, three policies.* `wake_tid_to_runnable` is split: D2 receive-publication
   rollback (not delivery — must prove no slot is armed); genuine endpoint delivery via
   `wake_waiter_for_endpoint` (a valid claimant); and a generic `SchedulerWakePlan::Wake(tid)` whose
   11 origins span at least five causes, where a bare TID is insufficient and the plan must carry a
   typed cause or `apply_scheduler_wake_plan` must refuse in production. Cross-CPU wakes get the
   same treatment: five typed `WorkItem` forms, only the endpoint-delivery one may carry a token,
   and every form must carry `{tid, asid}` to reject stale TID reuse. There is currently **no
   production producer** of `WorkItem::WakeTask` — a guard asserts that, so the work stays
   prerequisite rather than remedial.

   *Group-3 preconditions.* `debug_assert` is explicitly rejected — it compiles out of release
   kernels, so the proof would not exist where it matters. Each of the five sites gets an exact
   expected transition (`Runnable → Running` only for dispatch; `Running → Runnable` only for
   yield; the exact transaction predecessor for rollback; current-and-running for fault; **absence**
   for spawn) with a fail-closed action. For `spawn_user_task_from_image` two near-misses are
   recorded explicitly: `register_task_with_class` idempotence is not a precondition because it
   *returns `Ok(())`* for an existing TID, and checking "not `Blocked(EndpointReceive)`" would still
   permit overwriting the entry point, stack, ASID and register context of a `Runnable` or `Running`
   victim.

   *The drift guard.* `(file, enclosing fn, count)` could not see a count-preserving substitution
   inside one function. Each of the 37 sites is now pinned by an exact-site fingerprint — file,
   enclosing function, normalized LHS, assigned status expression, and the exact preceding and
   following non-blank non-comment source lines. Four mutations confirm it: a one-line reorder,
   **removing one assignment and adding an identical one elsewhere in the same function**, swapping
   `Runnable`/`Running` within `yield_current`, and a brand-new assignment — all four caught, where
   the first two passed under the old fingerprint. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.34 E.

   **WA3A: eight of the nine Group-3 sites are now refused in production; CAN shrinks 21 → 13.**
   The first WA3 increment that changes production executable code. A new module,
   `src/kernel/task_transition.rs`, provides a typed **release-build** fail-closed transition
   barrier — `debug_assert` was rejected outright, since it compiles out of exactly the builds
   where the proof must hold. Six typed transitions (`DispatchIncoming`, `ContinueCurrent`,
   `PreemptOutgoing`, `PreemptOutgoingIdle`, `RollbackDispatchedIncoming`, `FaultRunningCurrent`),
   no set-status escape hatch, typed refusals that write no TCB field, and optional incarnation
   identity so a recycled numeric TID cannot authorize a transition on a replacement task.

   **No partial scheduler/TCB commit.** Both yields' outgoing transition and the fault path
   validate *before* either authoritative mutation — the fault precondition now runs ahead of
   `block_current_cpu`, which was previously an irreversible rank-1 commit with the status check
   after it. `dispatch_next_task`, both yields' incoming and the D6 seam use exact rollback via
   the pre-existing `preempt_reenqueue_only_on` / `preempt_reenqueue_current_cpu` — the inverse of
   `dispatch_next_on` — so **no new scheduler primitive was needed**. `direct_dispatch_rollback_
   split` became a typed transaction: if the task half is refused, the scheduler half is skipped,
   because re-enqueuing a task this transaction does not own could displace a live `current`. No
   new broad-lock acquisition and no task(2) → scheduler(1) inversion. The D6 seam gained a `cpu`
   parameter and a `bool` return; all eleven x86_64/AArch64/RISC-V trap-drain call sites now skip
   the resume on refusal.

   **Spawn: HARD-STOP.** The absence gate was implemented and then reverted. x86 boot (and its
   AArch64/RISC-V twins) calls `register_task_with_class(RING3_{SUPERVISOR,PM,INIT}_TID)` BEFORE
   the matching spawns, so the gate refused the kernel's own supervisor on an ordinary `-smp 1`
   boot: `SPAWN_REFUSED_TID_PRESENT tid=2` → `failed to bootstrap first user task: TaskTableFull`.
   That is live evidence, and it only surfaced after a **rebuild**: the first core-smoke run
   reported PASS against a stale prebuilt artifact, because the smoke script boots artifacts and
   does not rebuild them. No weaker predicate was substituted — "not `Blocked(EndpointReceive)`"
   would still permit overwriting a live task's context, stack, ASID and capabilities — so
   `spawn_user_task_from_image` stays **CAN**, and CAN shrinks 21 → **13**, not 12. The gap is
   pinned by a test asserting the current overwrite behaviour plus a guard that the boot sequence
   still pre-registers, so the hard-stop is falsifiable rather than narrated.

   **A real invariant break surfaced.** The idle task (TID 0) is `current` while `Runnable` — the
   rank-1 scheduler makes it current with no mark-running step. Rather than weaken
   `PreemptOutgoing`, that case gets its own `PreemptOutgoingIdle` transition which the primitive
   refuses for any TID but `IDLE_TID`; a test drives an ordinary task into the same state and
   confirms it is still refused.

   Census recomputed, not edited: **29 remaining raw writes + 8 barriered sites = 37**, giving
   **CAN 13 / CANNOT 15 / INTO_BLOCKED 7 / FRESH_CONSTRUCTOR 1 / NON_PRODUCTION 1 / UNPROVEN 0**.
   The 13 remaining CAN paths are the eight endpoint-delivery owners, four teardown paths, and
   spawn pending the C repair. Nine mutations, each removing one production check, all caught by
   a named behavioural test. Waiter ownership still has **zero production callers**,
   `WAITER_OWNERSHIP_EXCLUSIVE=no`, `WAITER_OWNER_CENSUS_COMPLETE=yes`, x86 direct production
   default **OFF**, NR6/NR7 late claims unchanged, canonical 199D **OPEN**, ledger **39 / 7 / 46**,
   RISC-V unchanged, **no new live cell**. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.35.

   **WA3A-R2-SEAL: the R1 repair had introduced a torn state of its own; WA3A is now sealed.**
   WA3A-R1 typed the dispatch provenance, and in doing so created this: a non-idle `Running`
   current with no ASID performed a legal `Running → Running`, `DispatchMarkToken::new` then
   refused for want of an exact incarnation, the common failure branch rolled the status back to
   `Runnable`, and `undo_dispatch_selection(ContinuedCurrent)` correctly mutated no scheduler
   state — leaving `current = T` with `status(T) = Runnable`. The "rollback" was the corruption.
   Identity resolution moved **inside** the same rank-2 acquisition as the transition and
   strictly before it, so a missing identity refuses with the TCB untouched and there is nothing
   to undo but the scheduler step.

   **Provenance reconstruction is gone from the whole Group-3 cohort.** R1 left the three in-lock
   sites computing `let dequeued = outgoing_tid != Some(tid);`, which is not merely inelegant but
   wrong: a lone task that yields is re-enqueued and then genuinely dequeued again, so outgoing
   == incoming while the queue really did advance — and a refusal would have skipped the
   re-enqueue and lost the only runnable task. `KernelState` gained the provenance-preserving
   `*_selection` seams and all three sites now commit through ONE shared
   `commit_dispatch_selection_in_lock`. The old try-`DispatchIncoming`-then-`ContinueCurrent`
   fallback went with it — inferring the transition from which one succeeds would launder a
   double-queued `Running` task through the dequeue path.

   **A second idle finding, handled the same way as the first.** Making the transition exact
   showed that the idle/bootstrap task's status is not governed by the ordinary contract at all:
   boot leaves TID 0 `Running` and re-dequeues it, while a queue-neutral step finds it
   `Runnable`. Rather than weaken either ordinary transition, each got an idle-only twin
   (`RedispatchIdleAlreadyRunning`, `ContinueCurrentIdle`), refused for every TID but `IDLE_TID`,
   joining `PreemptOutgoingIdle` from WA3A.

   **Three more seals.** Dequeue rollback now takes a sealed `DequeuedDispatchMarkToken` whose
   only constructor checks the provenance, so presenting a continuation is unrepresentable rather
   than refused late. The off-lock seams authenticate the requested CPU against the authoritative
   dispatch CPU **before any mutation** and return a CPU-bound `CpuDispatch`, so the mark seam
   takes no `cpu` argument and no caller can stamp an unverified CPU into rollback authority.
   `may_resume()` is removed: all eleven x86_64/AArch64/RISC-V consumers now match all five
   outcomes explicitly and route `RefusedTorn` to a divergent `dispatch_torn_fatal` — never a
   resume, a fallback dispatch, an idle halt or a return to userspace. The AArch64 post-mark
   resume drives ASID activation, context/TLS restore and the completion take off the token's
   exact `{tid, asid}`, so a replacement incarnation that reused the TID is refused instead of
   resumed.

   Census unchanged in every class — **CAN 13 / CANNOT 15 / INTO_BLOCKED 7 / FRESH_CONSTRUCTOR 1
   / NON_PRODUCTION 1 / UNPROVEN 0** — with three rows moved from `exec_state.rs` (10 → 7) to
   `scheduler_state.rs` (1 → 4) because the in-lock dispatch mark is now one shared commit.
   Waiter ownership still has **zero production callers**, `WAITER_OWNERSHIP_EXCLUSIVE=no`,
   `WAITER_OWNER_CENSUS_COMPLETE=yes`, direct production default **OFF**, NR6/NR7 late claims
   unchanged, spawn still **hard-stopped** and CAN, canonical 199D **OPEN**, ledger
   **39 / 7 / 46**, **no new live cell**. WA3A is sealed; the next increment is the one-shot
   `ReservedUnstarted → LiveSpawned` TCB protocol (CAN 13 → 12).
   See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.36.

   `stage199d_riscv_canonical_admission` (11 tests) pins the
   contract. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.19.
3. **`d6_genuine_enabled()` is compile-time x86_64-only** — 203C blocked; AArch64 and
   RISC-V cannot retire any queue-advancing class.
4. **Every capability seam is `M2_SEAM_HELPER_ONLY`** — all of Phase 3 has zero production
   wiring.
5. **`FutexWait` off-lock seams landed helper-only** and were never wired.
6. **Reply-timeout scan is off-lock on x86_64 only**; `IpcSend` and `IpcCall` timeouts are
   untouched; the broad fallback `run_reply_timeout_completion` survives (199E).
7. **RISC-V `ExitCurrentTask` live cell** — kernel chain proven correct, runner bound
   corrected at `5488d8e`, re-run never executed (202D).
8. **Parallel `cargo test --lib` produces 58–71 shared-state assertion failures** — keeps
   every hosted claim single-threaded-only. Test-infrastructure debt; a prerequisite for
   using the hosted suite as a 205C harness, not 205C completion work.
9. **AArch64 re-acquires the broad lock on its split return path**
   (`src/arch/trap_entry.rs:1432`) — 204B/204E must localize it. 205A reports the cell; it
   is not where it gets retired.

---

## 1. Per-architecture status

### 1.1 AArch64 (QEMU virt — primary)

| Item | Status |
|------|--------|
| Core service-chain spawns | ✅ initramfs_srv / devfs_srv / vfs_server / driver_manager / blkcache_srv / virtio_blk_srv (tids 10000–10005) |
| Strict core-smoke gate | ✅ ordered progression: `_start` → `prepare_arch_boot` → `vbar_el1_ready` → `mmu_enabled` → `run_with_prepared_kernel` → `YARM_BOOT_OK` → `YARM_INIT_START`/`_DONE` |
| Timer / scheduler tick | ✅ `YARM_TIMER_IRQ_DELIVERED` / `YARM_TIMER_EOI_DONE` / `YARM_SCHED_TICK` |
| Optional FS strict smoke | ✅ RAMFS + ext4 live (`RAMFS_MOUNT_READY`, `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_*_OK`); FAT skipped (`server_disabled`) |
| Steady-state | Expected quiescent idle: `init_server` blocks on `init_alert_recv_ep` after `INIT_ALERT_WAIT_BEGIN`; `process_manager` blocks for more requests |
| SMP / PSCI | Deferred (post-bring-up baseline) |

See `doc/ARCH_AARCH64.md` for the per-PR boot history, IPC contract, PM
exec-load policy, and capability-materialization rules.

### 1.2 x86_64 (PVH — primary; `-smp 1` baseline)

| Item | Status |
|------|--------|
| Core-smoke gate (`QEMU_SMP=1`) | ✅ all 6 service entries exactly once; boot markers detected |
| Optional FS strict | ✅ RAMFS + ext4 live; FAT skipped |
| AP Rust online (`yarm.x86_ap_rust=1`) | ✅ Stage 109 outcome A — AP enters Rust and parks |
| Production scheduler | BSP only; `online_cpu_count()` stays at 1; AP `started_secondary` reported separately |
| Off-lock authoritative dispatch (D6-genuine) | ✅ **production, no opt-out** — `d6_genuine_enabled()` is compile-time true on x86_64 unless a D6 switch diagnostic owns the switch path; the D2-send / D2-recv / FutexWait / Yield / D6 drains all run with the broad guard dropped |
| D6 switch proof harness | 🧪 default-off diagnostic only (`yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`); mutually exclusive with the production D6-genuine path. Historical bring-up detail (Stages 120–132) is in `doc/PROJECT_HISTORY.md` |
| AP scheduler participation | 🧪 **proof-gated live** — `X86_AP_GENERIC_RETURN_SEAL`, `X86_AP_SAVED_RETURN_SEAL`, `X86_AP_RECV_V2_BLOCK_SEAL` and the cross-CPU NR6/NR7 seals all earned live at SMP=2 under default-off knobs; the **production** scheduler is still BSP-only (`online_cpu_count()` == 1) |
| Timer interrupts on APs | ❌ not enabled on the production path |
| Restore-owner selection | ⚠️ resolved **in-lock** (identity-coherent) and revalidated after the drains by `revalidate_idle_owner_after_drains`; that revalidation is **hosted-proven only, never run in QEMU** |

See `doc/ARCH_X86_64.md` for the safety fences, AP marker sequence, BT2
LAPIC timer discipline, and the ordered next-target list before AP
scheduling can be enabled.

### 1.3 RISC-V64 (OpenSBI / QEMU virt)

| Item | Status |
|------|--------|
| OpenSBI handoff | ✅ a0 (hartid) + a1 (DTB) preserved; `mv a0, s1` fix applied |
| Secondary hart park (`--smp 2/3/4`) | ✅ live-verified; boot hart never parked; parked list is the topology bitmap minus the boot hart |
| SMP topology + nonzero boot hart | ✅ binary-FDT `/cpus` walk yields `present_cpus=N`, `present_bitmap=0x{1,3,7,f}`; nonzero OpenSBI boot hart correctly selected (commit 271ac73) |
| Monotonic cmdline capture | ✅ once-guarded; `RISCV_CMDLINE_CAPTURE_ONCE`; `RISCV_CMDLINE_PRESERVED reason=missing_dtb_after_valid` |
| DTB RAM / initrd staging | ✅ `crate::arch::fdt::memory_reg` + `chosen_initrd`; firmware / DTB / initrd reserved |
| Bootstrap | ✅ 16 MiB boot stack; `Bootstrap::init_static`; real RAM staged before allocator init |
| Early S-mode trap diagnostic | ✅ `RISCV_EARLY_TRAP` + `RISCV_BOOTSTRAP_TRAP_STEP` |
| Sv39 kernel-shared gigapage | ✅ root[2] over `[0x8000_0000, 0xC000_0000)` with `V \| R \| W \| X \| G \| A \| D`; idempotent installer |
| Page-table write-through + zero-on-alloc | ✅ MMU walks physical frames, intermediates with `U=0` (Sv39 spec compliance) |
| Real S-mode → U-mode `sret` | ✅ `RISCV_ENTER_USER_SRET tid=2`; first trap `from_u=1 spp=0` |
| Syscall round-trip | ✅ full `RiscvTrapFrame` save/restore; `+4` ecall PC advance via TCB snapshot; task-switch arg seeding; S-mode-fault fail-closed halt |
| Core service chain | ✅ initramfs / devfs / vfs / ramfs / ext4 reached; `RAMFS_MOUNT_READY`; `EXT4_SRV_READY`; `VFS_MOUNT_REGISTER_*_OK` |
| Terminal state | ✅ `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked` (event-driven idle, no timer/IRQ scope) |
| Regular smoke target (`--smp 1/2/3/4`) | ✅ `scripts/qemu-riscv64-core-smoke.sh` + `scripts/qemu-riscv64-smoke-matrix.sh` enforce the full per-N marker contract on QEMU virt + OpenSBI |
| Ready for global kernel-unlocking smoke matrix | ✅ **Ready: yes** — see `doc/ARCH_RISCV64.md` §13.5; the regular core smoke is RISC-V's per-arch gate, treated the same way as x86_64 / AArch64 core smokes |
| Timer audit scaffold | ✅ `RISCV_TIMER_AUDIT_BEGIN` + `RISCV_TIMER_AUDIT_DONE sbi_time=… boot_hart=… trap_bridge_reentrant=… feature=…`; canonical deferred reasons pinned by the smoke gate (`timer_irq_feature_disabled`, `trap_bridge_reentrancy_not_ready`, `sbi_time_ext_unavailable`, `stie_audit_pending`, `not_boot_hart`) |
| Timer interrupt (live) | ⏸ deferred — accepted as `RISCV_TIMER_DEFERRED reason=timer_irq_feature_disabled`; next pass enables S-mode timer (`stimecmp` + `sstatus.SIE=1` + `mideleg` STI) and flips the gate to live-required |
| PLIC threshold write under active satp | ✅ skipped + reported as `RISCV_PLIC_DEFERRED reason=plic_mmio_unmapped_under_active_satp` (PLIC MMIO is outside the kernel-shared gigapage; raw write would fault) |
| External IRQ enable | ⏸ deferred — `RISCV_EXTIRQ_DEFERRED reason=no_safe_source`; UART0 (sid=10) is the marked candidate, no source enabled in this pass |
| SMP scheduler | ⏸ off — `RISCV_SCHEDULER_BSP_ONLY online_cpus=1 reason=riscv_smp_scheduler_not_enabled`; `online_cpus` stays at 1 until RISC-V SMP scheduling lands |

See `doc/ARCH_RISCV64.md` for the full marker sequence, ABI mapping, and
SMP blocker list.

### 1.4 Raspberry Pi 5 (diagnostic only — not production)

| Stage | Status |
|-------|--------|
| Stage 1 UART / DTB / MMU / allocator / read-only timer + GIC | ✅ live diagnostic |
| Stage 2A–2D | ✅ live diagnostic; EL0 entry deferred at Stage 2D (`ttbr_split_not_ready`) |
| HH-2 (TTBR split, MMU on, branch to high alias) | ✅ live diagnostic; non-default `rpi5-highhalf` feature |
| HH-3 (high-linked Rust continuation) | ✅ live diagnostic |
| HH-4 (low-identity retirement) | ✅ live diagnostic |
| HH-5 (real userspace) | ❌ DEFERRED — `RPI5_HH5_DEFERRED reason=high_half_initrd_allocator_bridge_not_ready` |

Current next blocker: build the high-half initrd / allocator bridge so
HH-5 can consume the existing Stage 2C loader without violating HH-4's
no-low-VA contract.

See `doc/RPI5_BRINGUP.md` for the full Stage 1A → HH-5 sequence and the
hardware artifact-build commands.

---

## 2. Per-service status

### 2.1 Bootstrap chain (image IDs 1–3)

| tid | service | status |
|-----|---------|--------|
| 1 | `init_server` | ✅ live; reaches steady-state event-driven idle on every arch with U-mode |
| 2 | `supervisor` | ✅ live; handoff banner emitted; control / fault / control-send caps present |
| 3 | `process_manager` | ✅ live; SpawnV5 path proven; PM-private reply RECEIVE cap in startup slot 2 |

Slots 0..17 are documented in `doc/PROCESS_AND_SPAWN.md` (slot 12
is PM-private for PM↔VFS subcalls).

### 2.2 Bootstrap FS chain (image IDs 4–6)

| tid (typical) | service | status |
|---------------|---------|--------|
| 10000 | `initramfs_srv` | ✅ live; `INITRAMFS_BACKEND_SOURCE source=cpio` populated from boot CPIO bytes |
| 10001 | `devfs_srv` | ✅ live; console / null FDs registered; `DEVFS_SRV_RESIDENT_WAIT_BEGIN` |
| 10002 | `vfs_server` | ✅ live; `VFS_MOUNT_TABLE_READY`; routes initramfs + devfs sends |

### 2.3 Optional FS / storage (image IDs 7–12)

| Image ID | Service | Status |
|----------|---------|--------|
| 7 | `driver_manager` | ✅ live; spawned via VFS-backed `STATX → OPENAT → READ* → CLOSE` after init passes a `vfs_server` request SEND cap (SpawnV5 service caps slot 0) |
| 8 | `blkcache_srv` | ✅ live |
| 9 | `virtio_blk_srv` | ✅ live |
| 10 | `fat_srv` | Profile-ready; **disabled by default** (`INIT_FAT_SPAWN_SKIPPED reason=server_disabled`); see `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` (FAT server section) for activation blockers |
| 11 | `ramfs_srv` | ✅ live; fully writable; mounted at `/ram` |
| 12 | `ext4_srv` | ✅ live; read-only; mounted at `/ext4` (writes report `Unsupported`) |

The optional-FS strict smoke pins these markers per arch — see
`doc/KERNEL_UNLOCKING.md` §3 ("Optional-FS smoke markers"). Do not
rename or remove them without updating both smoke scripts.

### 2.4 Networking

Service domain crate exists (`crates/yarm-network-servers`) with
contracts consolidated into `doc/NETWORKING.md` (Pass 4). Not part
of the core boot smoke.

### 2.5 UI

Service domain crate exists (`crates/yarm-ui-servers`). Not part of the
core boot smoke. Current contracts live in `doc/PHASE_GATES.md`
(Phase 4 UI contract section; gated by `scripts/check-roadmap-readiness.sh`).

---

## 3. Current crate / domain boundary

Kernel and low-level runtime own:

- scheduling and dispatch mechanisms;
- IPC / notification mechanisms;
- capability enforcement / mechanisms;
- trap / IRQ routing mechanisms;
- VM / address-space and bootstrap mechanisms.

Userspace service domains own service policy (extracted workspace crates):

| Domain | Crate path |
|--------|------------|
| Control plane | `crates/yarm-control-plane-servers` |
| Drivers | `crates/yarm-driver-servers` |
| Filesystems | `crates/yarm-fs-servers` |
| Networking | `crates/yarm-network-servers` |
| UI | `crates/yarm-ui-servers` |
| Compatibility | `crates/yarm-compat-servers` |
| Shared service helper/runtime | `crates/yarm-srv-common` |

The root `yarm` crate is no longer the monolithic service owner.
Boundary checks enforce crate-graph and source-shape constraints:

```sh
scripts/check-crate-graph-boundary.py
scripts/phase5-boundary-gates.sh
scripts/phase5-boundary-gates.sh --fs-runtime-entrypoint
scripts/phase5-boundary-gates.sh --driver-runtime-entrypoint
scripts/phase5-boundary-gates.sh --network-runtime-entrypoint
scripts/phase5-boundary-gates.sh --ui-runtime-entrypoint
```

`yarm-server-runtime` is a narrow server-runtime boundary; see
`doc/AI_AGENT_RULES.md` §16 for the export-surface contract.

---

## 4. Documentation ownership status

| Topic | Canonical owner | Status |
|-------|-----------------|--------|
| Kernel unlocking | `doc/KERNEL_UNLOCKING.md` | ✅ Pass 1 (canonical) |
| Kernel locking | `doc/KERNEL_LOCKING.md` | ✅ (existing canonical) |
| Boot | `doc/BOOT.md` | ✅ Pass 2 (canonical) |
| Arch — AArch64 | `doc/ARCH_AARCH64.md` | ✅ Pass 2 (canonical) |
| Arch — x86_64 | `doc/ARCH_X86_64.md` | ✅ Pass 2 (canonical) |
| Arch — RISC-V64 | `doc/ARCH_RISCV64.md` | ✅ Pass 2 (canonical) |
| RPi5 | `doc/RPI5_BRINGUP.md` | ✅ Pass 2 (canonical) |
| Project history | `doc/PROJECT_HISTORY.md` | ✅ Pass 3 (this pass) |
| Current status | `doc/STATUS.md` | ✅ Pass 3 (this file) |
| IPC | `doc/IPC.md` | ✅ Pass 4 (canonical) |
| VFS | `doc/VFS.md` | ✅ Pass 4 (canonical) |
| Filesystem / storage | `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md` | ✅ Pass 4 (canonical) |
| Networking | `doc/NETWORKING.md` | ✅ Pass 4 (canonical) |
| Capabilities | `doc/CAPABILITY_MODEL.md` | ✅ Pass 4 (canonical) |
| Process / spawn | `doc/PROCESS_AND_SPAWN.md` | ✅ Pass 4 (canonical) |
| Phase gates (Phase 2/3/4 contracts, roadmap, kernel-status milestones) | `doc/PHASE_GATES.md` | ✅ Pass 4 (canonical) |
| Service manifest | `doc/SERVICE_MANIFEST.md` | ✅ (existing canonical) |
| Kernel-unlock audit (census / matrix / stage table / roadmap) | `doc/KERNEL_UNLOCK_AUDIT.md` | ✅ Pass 6 (canonical) |
| Roadmap (current direction) | `doc/KERNEL_UNLOCKING.md` §0 | ✅ Pass 6 — the former `doc/ROADMAP.md` never existed in this tree; the kernel-unlock roadmap is canonical |
| Agent rules (capability/spawn/zero-copy/smoke + source-licensing header §15 + server-runtime boundary §16) | `doc/AI_AGENT_RULES.md` | ✅ Pass 5 (canonical; absorbed `AGENTS.md` body 2026-06-16) |
| libc / Linux / musl POSIX compatibility | `doc/LIBC_AND_LINUX_COMPAT.md` | ✅ Pass 5 (canonical; merged `LIBC_ABI_X86_64_NONE.md` + `LINUX_COMPAT.md` + `MUSL_POSIX_IPC_MAPPING.md` 2026-06-16) |
| Global unlocking readiness audit | `doc/KERNEL_UNLOCKING.md` §7.1 | ✅ Pass 5 (single source of truth) |
| Kernel test rules | `doc/KERNEL_TEST_RULES.md` | ✅ (existing canonical) |
| Agent-facing entry point (external-tool convention `AGENTS.md`) | `doc/AGENTS.md` | ✅ Pass 5 (short pointer to `doc/AI_AGENT_RULES.md`) |

---

## 5. Current top next steps

The four highest-impact items, in order of unlock value:

1. **RISC-V S-mode timer interrupt (live path) + smoke-gate tightening.**
   The regular RISC-V core smoke now passes live across `--smp 1/2/3/4`
   on the deferred branch (timer / PLIC / external IRQ all reported with
   explicit `reason=` markers). Next, enable `stimecmp` via the SBI Timer
   extension, set `sstatus.SIE=1`, delegate `STI` in `mideleg`, then
   flip the smoke gate's `RISCV_TIMER_SMOKE_OK|RISCV_TIMER_DEFERRED`
   accept-regex from "either" to "live required". PLIC + external-IRQ
   follow the same flip; once both land, queue RISC-V into the global
   kernel-unlocking smoke policy and unblock RISC-V SMP scheduling so
   `online_cpus` can climb past 1. See `doc/ARCH_RISCV64.md` §10–11.

2. **Kernel unlocking — canonical Stage 199D.**
   The broad `SpinLock<KernelState>` still has **49** production acquisition sites (§0).
   The ServerDies reverse-link accounting failure that used to head this list is
   **resolved** (`doc/IPC.md` §8.5): the transition counters now describe exactly one armed
   ServerDies transaction and the leak invariant moved to system-wide link totals, so there
   is no hard `result=fail` left in the tree. Two follow-ons, of different kinds: the
   **x86_64 ServerDies live cell** (a runner act — one clean boot earns the first cell and
   finally exercises `revalidate_idle_owner_after_drains`, which has never run in QEMU), and
   the **smallest production change**, flipping `ipccall_direct_proof_enabled()` to the
   production default on x86_64 so the landed off-lock NR 6 / NR 7 transaction is actually
   taken by a normal boot. Neither completes 199D. See `doc/KERNEL_UNLOCKING.md` §0 and
   `doc/KERNEL_UNLOCK_AUDIT.md` §6.

3. **RPi5 HH-5 — high-half initrd / allocator bridge.** Build the bridge
   so HH-5 can consume the existing Stage 2C loader without violating
   HH-4's no-low-VA contract; then enter EL0 via the real ERET path. See
   `doc/RPI5_BRINGUP.md` §12–13.

4. **Documentation consolidation Pass 4 — completed 2026-06-15.** Six
   ABI-sensitive clusters (IPC, VFS, FS/storage, networking, capabilities,
   process/spawn) and the six CI-gated phase docs were consolidated into
   seven canonical owners (`doc/IPC.md`, `doc/VFS.md`,
   `doc/FILESYSTEM_AND_STORAGE_CONTRACTS.md`, `doc/NETWORKING.md`,
   `doc/CAPABILITY_MODEL.md`, `doc/PROCESS_AND_SPAWN.md`,
   `doc/PHASE_GATES.md`). CI gate scripts were updated atomically. See
   `doc/DOCUMENTATION_MAP.md`.

---

## 6. Frozen boundaries (one-line reminders)

The full invariant list lives in `doc/KERNEL_UNLOCKING.md` §3. Headlines:

- SpawnV5 ABI (16-byte reply, argument layout) — frozen.
- Image IDs 7–12 — frozen.
- `SYSCALL_COUNT = 32` (dispatch-table size); public ABI surface is `0..=16`
  after `ExitCurrentTask` (NR 16) landed — see `doc/SYSCALL_ABI.md`.
- `STARTUP_SLOT_COUNT = 18`.
- `RecvSharedV3` ABI offsets — frozen.
- Optional-FS smoke markers (`INIT_RAMFS_SPAWN_OK`, `RAMFS_MOUNT_READY`,
  `VFS_MOUNT_REGISTER_RAMFS_OK`, `INIT_EXT4_SPAWN_OK`, `EXT4_SRV_ENTRY`,
  `EXT4_SRV_READY`, `VFS_MOUNT_REGISTER_EXT4_OK`,
  `INIT_FAT_SPAWN_SKIPPED reason=server_disabled`) — do not rename or
  remove.
- No `ipc_recv_with_deadline(_, 0)` in required-reply paths.
- `VFS_SUPERVISOR_TASK_EXIT_NOTIFICATION_ENABLED = false`.
- VM / TLB two-phase ordering (PTE removal → TLB shootdown → reclaim).
- Boundary gates (`phase5-boundary-gates`) remain green.
- No service-policy logic in the kernel; no reintroduction of
  `src/services/*`.

---

## 7. Authoring rule

Do **not** turn this file into a milestone diary. Append a row to
`doc/PROJECT_HISTORY.md` for a closed milestone; update the rows above
to reflect the new live state; link the next-target details to the
canonical owner doc. New status / next-context / audit / PR-plan
fragment files are forbidden — see `doc/DOCUMENTATION_MAP.md`.
