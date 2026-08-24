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

**U3 (canonical 203C) — the AArch64 `CurrentTaskExited` VALIDATION reacquisition is retired.
CENSUS-DELTA 13 → 12. CANONICAL 203C — still OPEN.**
The AArch64 post-lock exit consumer in `src/arch/trap_entry.rs` re-acquired the broad guard to
read current TID, `{tid, asid}` identity, terminal status and any-runqueue absence. It now calls
`SharedKernel::post_lock_exit_validation_split` — the SAME transaction the RISC-V consumer
already used, taken as one snapshot with rank 2 nested inside rank 1 — so no seam, marker or
mechanism was added. Offline-CPU refusal, full incarnation validation, terminal classification,
`task_present_anywhere` semantics, every fatal check and marker, and the replacement-versus-idle
divergence are unchanged. `trap_entry.rs` falls 5 → 4 `with_cpu` callsites.

**U3 (canonical 203C) — DELIVERED. The AArch64 `CurrentTaskExited` REPLACEMENT RESTORE
reacquisition is retired. CENSUS-DELTA 12 → 11. CANONICAL 203C — still OPEN.**
The consumer's second and last acquisition held the broad guard across
`d2_recv_switch_incoming_asid(next)` and `post_switch_restore_arch_thread_state`, performing the
TTBR0_EL1/ASID switch and every trap-frame write under the whole kernel. It is now
`SharedKernel::post_exit_replacement_restore_split` — one coherent rank-1 → rank-2 transaction
that authenticates the same `validate_online_cpu` predicate, binds `current_cpu`, resolves the
replacement on the exact trapping CPU, and takes its ASID, saved context, TLS request and parked
blocked-syscall completion in a SINGLE rank-2 acquisition — followed by
`aarch64::trap::post_exit_restore_replacement`, which does the address-space activation and the
frame work with every domain lock released, through the SAME single frame writer the in-lock
restore uses. No policy was cloned into `runtime.rs` and no `DispatchMarkToken` was fabricated.
Preserved: the identical activation pair, the no-ASID/absent-TCB `SCHED_ENTER_IDLE` outcome, the
no-frame outcome (switch happens, nothing is consumed), the offline-CPU `KernelError`, and every
marker. Added, fail-closed: `next == exiting tid` and a stale replacement identity now refuse onto
the restore's existing `TaskMissing` class. The D6 controlled-switch proof acquisition in the same
file is EXCLUDED and byte-for-byte unchanged: its D6 cleanup maps kernel stack pages into every
live task root, which is D3-fenced by `AI_AGENT_RULES` §14.4 with no split form, so splitting it
would leave a broad drain at census delta 0. `trap_entry.rs` falls 4 → 3 `with_cpu` callsites —
re-derived mechanically from source; the `5` this file's owner tables carried was already stale at
the base commit, one behind the validation retirement above, and is corrected rather than carried
forward. Census: `with_cpu` **11 → 10**, broad `with` **stays 1**, total and runtime-required
**12 → 11**.

**Not live-proven.** `scripts/qemu-aarch64-exit-current-task-smoke.sh` fails at this checkpoint
with a mechanically identical 35-line failure set and the identical
`STAGE_200D0C2_AARCH64_EXIT_CURRENT_TASK_LIVE_SEAL … live_cells=0 result=fail` seal at both this
head and the exact base `3943472`: the oracle workload never activates at either revision, while
the boot itself completes. This is classified as an **exact-base deferral only** — it is not
evidence for or against the changed restore path, and no live cell is claimed for it. Live
evidence that does transfer: the x86_64, AArch64 and RISC-V standard core smokes all PASS at this
checkpoint. Direct production remains **OFF**.

**U3 (canonical 203C) — DELIVERED. The AArch64 FutexWait NO-INCOMING IDLE broad read is retired.
CENSUS-DELTA 11 → 10. CANONICAL 203C — still OPEN.**
The deferred-FutexWait drain's no-incoming idle branch re-acquired the broad guard for one read —
`with_cpu(cpu, |kernel| matches!(kernel.current_tid(), None | Some(0))).unwrap_or(true)` — purely
to confirm `current` is absent or idle before diverging into the BSP idle loop. It now calls the
EXISTING authoritative rank-1 transaction `SharedKernel::current_tid_authoritative(cpu)`; **no new
seam was created**. That helper is what the retired closure resolved to: the same
`validate_online_cpu` predicate `set_current_cpu` applies, the same `current_cpu` binding (left
untouched on refusal), and the same `current_tid_on(current_cpu())` read — including the
freestanding-AArch64 `MPIDR_EL1`-derived lookup — all under ONE rank-1 acquisition, where the broad
body took the scheduler lock twice and was coherent only by virtue of the broad guard. The
predicate and the refusal policy are unchanged outcome for outcome: `Some(0)` is the idle task, and
both "no current task" and "CPU refused" arrive as `None` and map to `true` through the same
`unwrap_or(true)`. All four consumer outcomes are preserved exactly: **refusal → `true`**, **no
current task → `true`**, **current TID `0` → `true`**, **nonzero current TID → `false`**.
`current_tid_split_read` remains rejected because it does not bind `current_cpu` — the reason the
earlier substitution here was reverted — and `terminal_idle_on_cpu_split` remains rejected because
its `runnable_count_on` condition adds runnable-count policy the legacy predicate never had.
Census: `with_cpu` **10 → 9**, broad `with` **stays 1**, total and runtime-required **11 → 10**,
`src/arch/trap_entry.rs` **3 → 2**. The two that remain are the canonical Phase-2 broad trap
dispatch and the D6 controlled-proof restore, still D3-fenced under `AI_AGENT_RULES` §14.4.

**Not live-proven — scoped precisely.** This branch's only live gate is the Stage 195F no-incoming
idle oracle, whose precondition is unreachable behind the pre-existing SpawnV5/initramfs stall on
AArch64 — untouched here, and it prevents the target live cell from completing at **both**
revisions. What is claimed is only this: the **target AArch64 FutexWait-idle marker census was
identical** at the delivered head and at the exact base `6dd3ca4` —
`AARCH64_FUTEX_WAIT_IDLE_ORACLE_PROVISION_OK` = 1, `…_SET` = 1, `…_DONE` = **0** — with the same
`aarch64 FutexWait idle oracle proof missing` failure at both. That is an **exact-base deferral for
the target AArch64 cell**, and it is **not** live proof of the changed read; no live cell is
claimed. **The whole cross-architecture seal is not claimed byte- or character-identical, and it
was not:** one unrelated line differed *favourably* — `riscv64 FutexWait idle oracle proof missing`
was present at the base and **absent** at the head — and the RISC-V kernel binaries were
**bit-identical** across the two revisions, so that variance cannot originate in this source
change. Direct production remains **OFF**.

**U3 (canonical 203C) — CPU-AUTHORITY PREREQUISITE: DELIVERED. CENSUS-DELTA 0.
CANONICAL 203C — still OPEN.**

The second and final census-neutral prerequisite for retiring the two broad acquisitions in
`c2c_bsp_saved_frame_resume`. **No acquisition is retired here**; this makes their existing
workload genuinely reach them, repeatedly.

* **`current_tid_authoritative(cpu)` is validate-and-READ for an EXPLICIT CPU, not
  validate-bind-read.** It validates with the same `validate_online_cpu` predicate, reads
  `current_tid_on(cpu)` under one rank-1 acquisition, and **never writes `scheduler.current_cpu`**
  or resolves through any ambient selector. Its returned value is unchanged for all fifteen
  production callers, each of which consumes only that value. On freestanding AArch64 the retired
  `MPIDR_EL1` lookup is value-identical, because the AArch64 trap entry derives the `cpu` it passes
  by the same expression.
* **An off-broad explicit read must not mutate the process-global ambient `current_cpu`.**
  `scheduler.current_cpu` is ONE field that `current_tid()` / `current_task_cnode()` resolve
  through, so binding it off the broad lock retargeted every ambient reader on every CPU. Measured:
  CPU 1 held the broad lock mid-syscall with `current_tid_on(1)` = its server task, CPU 0 called
  `current_tid_authoritative(CpuId(0))`, and CPU 1's ambient identity flipped to CPU 0's task — so
  `handle_ipc_recv` validated the receive capability against the WRONG process CNode and correctly
  refused it with `MissingRight`.
* **Broad `SharedKernel::with_cpu` retains its legacy ambient binding.** While the broad lock
  exists that binding is transaction-local state and is out of scope.
* **The prior AArch64 FutexWait-idle wording overstated the binding side effect as required.** Its
  returned-value semantics are unchanged; the binding was unsafe under SMP. Those paragraphs are
  corrected in place rather than removed.
* **Both direct-IpcCall remote-wake decisions now use the explicit CPU executing the drain.**
  `drain_direct_request_post_work` and `drain_direct_reply_post_work` take `executing_cpu`,
  threaded from `try_split_ipccall_direct_into_frame` / `try_split_ipcreply_direct_into_frame`,
  and compare it against `success.wake_target_cpu`. Both `current_cpu_split_read` calls in
  `ipccall_direct_txn.rs` are gone with no ambient replacement. Local wake sends no IPI; a remote
  wake records delivery and sends exactly one IPI, in the existing direction.
* **`IPCCALL_DIRECT_SMP_REQUEST_OK` now uses an order-independent, exactly-once proof rendezvous.**
  Its two preconditions are produced by different CPUs with no ordering between them — CPU 0's
  committed delivery and CPU 1's single `X86_AP_RECV_V2_CONTINUED` DebugLog. The old one-shot fired
  only from the continuation side and only if the delivery was already recorded, so the
  continuation-first interleaving lost the marker permanently. Each side now records its own fact
  and calls the same helper; the `EMITTED` bit is set in the same compare-exchange that completes
  the pair, so exactly one caller emits, in either order. It is proof bookkeeping only: no
  production IPC decision consults it, it cannot delay, suppress or alter delivery, and the emitted
  text is the pre-existing marker. The emission is synchronous for the reason
  `ap_seal_syscall_begin` already documents — the shared printk ring drops required proof markers
  under concurrent AP+BSP traffic, measured losing this one outright in 2 of 6 boots even after the
  ordering was fixed.
* **No IPC, capability, scheduler-placement or production-admission semantics changed.** Wake
  targets, enqueue policy, rights and error classes are untouched; `MissingRight` is not weakened.
* **Other ambient `current_cpu` readers and writers are NOT retired.** Seven off-broad writers
  remain in `runtime.rs`, plus the general `current_tid()` / `current_task_cnode()` surface (224 and
  32 production consumers). They remain separately auditable prerequisites — no global retirement is
  claimed.
* **The x86_64 BSP saved-resume cohort is now repeatedly live-reachable.** Five consecutive
  matched-artifact runs of `scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh` each show every
  required marker exactly once — `IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1`,
  `X86_BSP_NR6_REQUEST_SENT`, `X86_AP_RECV_V2_CONTINUED`, `X86_AP_RECV_V2_USER_VALIDATED cpu=1`,
  `IPCREPLY_DIRECT_SMP_CALLER_BLOCKED`, both reschedule IPIs, `IPCCALL_DIRECT_SMP_REQUEST_OK`,
  `X86_BSP_SAVED_DISPATCH_OK cpu=0 mode=saved`, `X86_BSP_REPLY_USER_VALIDATED` and
  `IPCREPLY_DIRECT_SMP_REPLY_OK` — with zero `MissingRight`, zero ring-3 faults, zero panics and the
  full `STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL … result=ok`. One earlier run was discarded as
  visibly serial-spliced (fragments of a marker survived without the intact line); its raw log was
  preserved and it is classified separately, not as a failure.
* **The two acquisitions in `c2c_bsp_saved_frame_resume` remained present and unchanged** at that
  checkpoint, for the next U3 **10 → 8** retirement pass. **The census remained 10** (`with_cpu` 9,
  broad `with` 1). Both have since been retired — see the next entry.

**U3 (canonical 203C) — x86_64 BSP SAVED-RESUME COHORT: DELIVERED. CENSUS 10 → 8.
CANONICAL 203C — still OPEN.**

Both remaining broad acquisitions in `c2c_bsp_saved_frame_resume` are gone, retired in one pass
against the now live-reached path. **`with_cpu` 9 → 8, broad `with` 1 → 0, total 10 → 8.**

* **The saved-context read moved onto the EXISTING rank-2 snapshot,
  `SharedKernel::ap_saved_resume_context_split`** — the same transaction the AP site already ran,
  so no second reader was created. It is destructured by NAME (`ApSavedResumeContext { asid, cr3,
  rip, rsp, gprs, fs_base, runnable_saved }`); the retired positional 7-tuple could not stop a
  consumer transposing `rip`/`rsp` or reading `fs_base` where `cr3` was meant. The legacy body
  answered the same question through four separate task-domain re-entries (`task_asid`,
  `cr3_for_asid`, `task_status`, `with_tcbs`); the snapshot takes every task-owned field in ONE
  rank-2 acquisition and resolves the ASID to a page-table root only after that guard is released,
  because `PAGE_TABLE_STATE` is an independent, unranked lock. Refusals are unchanged outcome for
  outcome: absent TCB, missing ASID and an ASID with no CR3 each yield `None`; absent TLS is
  `fs_base = 0`; `Blocked`/`Faulted`/`Exited`/`Dead`/`Reserved` and an incomplete saved frame all
  clear `runnable_saved`.
* **The preempt-prefer mutation moved onto ONE new rank-1 transaction,
  `SharedKernel::on_preempt_prefer_on_cpu_split`**, running the SAME
  `Scheduler::on_preempt_prefer_on` primitive the retired `KernelState::on_preempt_prefer_on_cpu`
  wrapper ran — that wrapper immediately re-entered the rank-1 scheduler lock anyway, so the broad
  guard added nothing but width. The policy still has exactly one implementation.
* **The legacy fallback is preserved deliberately and was NOT "improved".** When the preferred TID
  is not queued on that CPU the scheduler may still select some other runnable task and return it;
  the caller's `made_current != Some(client_tid)` comparison is what rejects that, unchanged. An
  invalid or offline CPU still yields `None` with no mutation, reached now by the scheduler's own
  `check_online_cpu` instead of `with_cpu` refusing and `.unwrap_or(None)` collapsing the error.
* **Neither transaction touches the process-global ambient `scheduler.current_cpu`.** Both are
  named by the explicit `cpu` the caller trapped on. The explicit-CPU authority and the REQUEST_OK
  rendezvous delivered in the previous entry are unaltered.
* **Identity is NOT strengthened.** This path holds only a numeric `client_tid` and the ASID the
  snapshot discovers; it carries no generation-bearing incarnation token, and no `DispatchMarkToken`
  or generation authority was fabricated for it.
* **The consumer's 8-step ordering is intact** — reply/client gates → read-only snapshot → reject a
  partial frame → scheduler mutation → require `selected == client_tid` → `DONE` once → clear
  pending + marker → lock-free MSR/frame/CR3/iret. The context snapshot was **not** moved after the
  scheduler mutation, and every domain guard is released before `DONE.swap`, the
  `X86_BSP_SAVED_DISPATCH_OK` marker, `configure_syscall_msrs_for_self`, the FS-base WRMSR, the CR3
  write, the frame construction and `resume_user_mode_iret`.
* **Census: `with_broad` 1 → 0, `with_cpu` 9 → 8, runtime-required/total 10 → 8.** The full
  remaining eight-site inventory is re-derived from the final source tree, site by site, in
  `doc/KERNEL_UNLOCK_AUDIT.md` §1.4a — file, line, enclosing symbol, form, class, role, why it
  remains and the canonical directive expected to retire it.
* **Both caller-free `KernelState` helpers were deleted** — `ap_saved_resume_context` and
  `on_preempt_prefer_on_cpu`. `src/arch/x86_64/smp.rs` now holds exactly ONE acquisition, the
  unreached ED-2 next-task placement in `ap_sched_next_or_idle`, retained byte-for-byte with its
  distinct `Err(_) => None` refusal policy. **There is no production broad `SharedKernel::with`
  callsite left anywhere in the tree.**
* **Live evidence: five consecutive clean runs** of
  `scripts/qemu-x86_64-ap-cross-cpu-reply-smoke.sh` against freshly built matched artifacts, each
  with twelve named markers exactly once — `IPCCALL_DIRECT_SMP_SERVER_BLOCKED server_cpu=1`,
  `X86_BSP_NR6_REQUEST_SENT cpu=0`, `IPCCALL_DIRECT_SMP_REQUEST_OK sender_cpu=0 receiver_cpu=1
  cross_cpu=1`, `X86_AP_RECV_V2_USER_VALIDATED cpu=1`, `IPCREPLY_DIRECT_SMP_CALLER_BLOCKED
  arch=x86_64 caller_cpu=0`, `X86_BSP_RESCHEDULE_IPI_SENT sender_cpu=1 receiver_cpu=0`,
  `X86_BSP_RESCHEDULE_IPI_RECEIVED cpu=0`, `X86_BSP_SAVED_DISPATCH_OK cpu=0 mode=saved`,
  `X86_BSP_REPLY_USER_VALIDATED cpu=0`, `X86_BSP_RECV_V2_CONTINUED cpu=0`,
  `IPCREPLY_DIRECT_SMP_REPLY_OK sender_cpu=1 receiver_cpu=0 cross_cpu=1` and
  `IPCREPLY_DIRECT_SMP_DUPLICATE_REFUSED arch=x86_64` — with the full
  `STAGE_199_IPCCALL_REPLY_DIRECT_SMP_SEAL … result=ok`, zero panics and zero ring-3 faults.
  **These five consecutive clean seals are the live proof for this changed path**, and no
  exact-base deferral was used for it. **The retirement evidence is the census reduction, not the
  markers**; the markers prove the retired path still behaves identically.
* **One regression script flakes, identically at the exact base.**
  `scripts/qemu-x86_64-ap-saved-return-smoke.sh` failed once during the matrix with
  `RESUMED continuation != 1` — the AP entered ring 3 twice as usual and every kernel-side marker
  fired exactly once (`X86_AP_GENERIC_DISPATCH_OK cpu=1 mode=fresh`, `X86_AP_SAVED_RESUME_BEFORE`,
  `X86_AP_SAVED_FRAME_COMMITTED cpu=1 syscall=Yield`, `X86_AP_SAVED_DISPATCH_OK cpu=1 mode=saved`),
  but the ring-3 `X86_AP_SAVED_FRAME_RESUMED cpu=1` never appeared, not even as a fragment. An
  interleaved head-versus-exact-base sample settles it: **head 9/10, base `c2b9b77` 8/9**, with a
  byte-identical failure signature on both sides — same assertion, same marker, same counts. This
  script exercises the untouched **CPU-1 AP-return** path (`ap_saved_frame_resume`); the retired
  acquisitions are in the **BSP** counterpart on CPU 0. It is recorded as an **exact-base
  flake / deferral** — evidence neither for nor against this BSP change — and that profile is
  **not** claimed green.

**U3 (canonical 203C) — x86_64 ED-2 NEXT-TASK PLACEMENT: DELIVERED. CENSUS 8 → 7.
Execution directive U3 COMPLETE (exit met); CANONICAL 203C still OPEN / PARTIAL.**

The last broad acquisition in `src/arch/x86_64/smp.rs` is gone. **`with_cpu` 8 → 7, `with_broad`
stays 0, runtime-required/total 8 → 7.**

* **ED-2 is live-reached by an EXISTING workload — the retention premise was about the scripts,
  not the kernel.** The audit held ED-2 back because every scripted profile that set
  `yarm.ap_user_dispatch=1` also set the direct-IpcCall oracle, which pins
  `ap_workload_task_count()` to 1, so `AP_DISPATCH_COUNT < count` is false after the first ring-3
  entry. With the oracle selector OFF the same production workload runs `AP_WORKLOAD_TASKS` = 2.
  Proven live at `39c75dc` BEFORE any edit, twice: `X86_AP_WORKLOAD_PLACEMENT_READY cpu=1
  base_tid=20205 count=2`, both TIDs admitted and issuing a real ring-3 syscall
  (`X86_AP_USER_SYSCALL_REENTRY_OK` for 20205 and 20206), `X86_AP_NEXT_TASK_DISPATCH_BEGIN cpu=1`
  exactly once, `X86_AP_REPEATED_DISPATCH_OK cpu=1 count=2`, `X86_AP_SCHED_POLICY_SEAL_DONE
  result=ok cpu=1 count=2`, clean return to idle, no oracle markers, no refusal or fault. No
  script, knob, marker or workload was added or edited to obtain this.
* **Retired through the EXISTING transaction under `Decline`.** ED-2 now calls
  `enqueue_then_dispatch_on_cpu_split(cpu, next_tid, EnqueueRefusalPolicy::Decline)` — no new
  seam. `Decline` IS this site's historical `Err(_) => None` policy, expressed as a type.
  `DispatchAnyway` is forbidden here: on a refusal it can select a DIFFERENT task than the one
  being placed, which the caller's `placed == Some(next_tid)` comparison would then reject —
  a behaviour change on a live path. An outer refusal collapses to `None` exactly as
  `.unwrap_or(None)` collapsed the `with_cpu` refusal. Enqueue still precedes dispatch inside one
  rank-1 critical section; eligibility is not strengthened and no retry or fallback was added.
* **Explicit-CPU validation replaces unsafe off-broad ambient binding.** The shared transaction no
  longer writes `scheduler.current_cpu`. That field is ONE process-global selector which
  `current_tid()` and `current_task_cnode()` resolve through, so binding it from an off-broad seam
  retargets every ambient reader on every CPU — the failure measured directly on the SMP
  saved-resume path, where it made `handle_ipc_recv` validate against the wrong process CNode.
  Every step names the CPU explicitly (`validate_online_cpu(cpu)`,
  `enqueue_on_with_priority(cpu, ..)`, `dispatch_next_on(cpu)`), so the binding was never
  load-bearing for the returned outcome; both production callers supply an explicit `CpuId`,
  consume only the typed `CpuEnqueueDispatch`, and carry the CPU and selected TID forward as plain
  values. `validate_online_cpu` is retained and an invalid CPU yields the same refusal with no
  mutation. The per-CPU `current` is still updated — that is CPU-local scheduler state, not the
  ambient selector.
* **All post-transaction work is lock-free.** Every guard is released before
  `X86_AP_ADMIT_PLACED`, the seal-probe activation, `X86_AP_NEXT_TASK_DISPATCH_BEGIN` and
  `ap_enter_task_ring3`. The dispatch-count gate, next-TID calculation, marker order and text,
  denied wake-only fallthrough, saved-resume branch and idle tail are all unchanged.
* **`src/arch/x86_64/smp.rs` now holds ZERO broad acquisitions of any form.** The seven that
  remain are: two authoritative broad trap dispatches (`arch/trap_entry.rs:310`,
  `arch/riscv64/trap.rs:829`), one D3-fenced D6 controlled-proof restore
  (`arch/trap_entry.rs:1694`), one recv Phase-A multi-domain composite (`runtime.rs:4093`), and
  three capability-teardown rollback sites (`runtime.rs:4193`, `4486`, `5278`). **No post-lock
  drain reacquisition remains anywhere in the tree.** Full per-site inventory in
  `doc/KERNEL_UNLOCK_AUDIT.md` §1.4a.
* **203C remains OPEN / PARTIAL**, because scheduler operations still execute inside the two
  authoritative trap dispatches and the D3-fenced proof site remains.

**Execution directive U3 is COMPLETE — exit MET.** Its exit is the census reaching **≤24** with
every post-lock drain re-acquisition deleted in the same increment as the seam it blocked. The
total is **7** and the drain class is fully retired (`doc/KERNEL_UNLOCK_AUDIT.md` §1.4a lists the
seven survivors; none is a drain). **Canonical 203C nevertheless remains OPEN / PARTIAL** — its
own exit, the rank-1/rank-2 seams being authoritative rather than compatibility helpers, is still
false while scheduler operations execute inside the two authoritative trap dispatches and the
D3-fenced controlled-proof restore remains. A completed directive is not a completed stage.

**199C remains OPEN. 199E remains DELIVERED / CLOSED. 200B/U5 remains OPEN (partially
production-wired).** Directive U8 is already source-complete. Canonical stage arithmetic is
unchanged at **2 complete / 11 partial / 22 open**. Direct production remains **OFF**.

**There is no U8 implementation outstanding.** Directive U8 was the AArch64
`finalize_split_handled_syscall` broad reacquisition, already retired by Stage 199D; that
function holds no broad acquisition today. Earlier "U8 is next" pointers are removed.

### Broad-lock position

| Metric | Value |
|--------|-------|
| Production `SharedKernel::with_cpu` callsites | **7** |
| Production broad `SharedKernel::with` callsites | **0** |
| **Total production broad-lock acquisition sites** | **7** |
| Ungated off-lock syscall classes | **5** on x86_64 (NR 15, 10, 8, 2-narrow, 14-narrow); **2** on AArch64 (NR 15, 10); **2** on RISC-V (NR 15, 10) |
| Proof-gated off-lock classes (default **OFF**) | NR 6 `IpcCall`, NR 7 `IpcReply` — all three architectures |
| Off-lock authoritative dispatch | **Direct NR6/NR7:** x86_64 (live) + AArch64 (structural, proof-gated) via `offlock_authoritative_dispatch_enabled()`; RISC-V not admitted. **Blocking IpcRecv / IpcSend (U4):** queue-advancing dispatch is authoritative outside the broad lock on **all three** architectures via the canonical `queue_advancing_dispatch_enabled()`. `d6_genuine_enabled()` itself remains compile-time x86_64-only — U4 widened the queue-ADVANCING question only, never the queue-neutral D6 slice. |

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
| 2 — IPC (199C–199G) | 1 of 5 (199C) | 199D, 199E | 199F, 199G |
| 3 — Capability (200A–200D) | 0 of 4 | 200A, 200C | 200B, 200D |
| 4 — VM (201A–201G) | 0 of 7 | 201B, 201F | 201A, 201C, 201D, 201E, 201G |
| 5 — Lifecycle (202A–202F) | 0 of 6 | 202D | 202A, 202B, 202C, 202E, 202F |
| 6 — Timer/IRQ/sched (203A–203D) | 0 of 4 | 203A, 203C, 203D | 203B |
| 7 — Monolith removal (204A–204E) | **1 of 5** (204A) | 204B | 204C, 204D, 204E |
| 8 — Seal (205A–205D) | 0 of 4 | 205A | 205B, 205C, 205D |
| **Total** | **1 of 35** | 12 | 22 |

**No canonical stage in Phases 2–6 or 8 is complete.** The one complete stage, 204A
(broad-lock callsite census), is documentation rather than lock retirement: 25 callsites
classified as 0 boot-only, 0 test-only, 0 obsolete, 25 runtime-required, 0 undocumented
(U1 deleted the two obsolete acquisitions, 49 → 47; U2 relocated the three test-only ones,
47 → 44; U3 is in progress and has retired sixteen — six RISC-V, two AArch64, four x86_64
post-lock drains, the four `runtime.rs` broad wrappers, and the three authoritative
current-identity acquisitions — 44 → 39 → 37 → 36 → 34 → 32 → 28 → 25). `src/runtime.rs` holds
no production broad `with`, and `src/arch/x86_64/descriptor_tables.rs` now holds no `with_cpu`
at all; the last two broad `with` reads are the x86 SMP `ap_saved_resume_context` pair.

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

**199E timeout-unlocking checkpoint — DELIVERED. CENSUS-DELTA 0.** Reply/call timeout
now has production registration (`arm_production_reply_deadline`, driven by the caller's own
`timeout_ticks`) and exact-identity retirement on every terminal outcome; endpoint AND
notification receive timeout have moved off the broad lock onto the common pipeline, which now
owns all three classes. Census delta 0; hosted suite green. **x86_64 selector-off production expiry is LIVE-PROVEN**
(`IPC_REPLY_TIMEOUT_ARMED` with the production token generation → `TimedOut` →
`GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcReplyTimeout`, with the selector-on pre-arm cell still
passing on the same artifacts).

**Mixed production/oracle reply-deadline clock domains are repaired.** Making production
registration live meant a selector-on boot holds BOTH kinds of reply deadline at once — the
confined oracle's hardware-counter record and ordinary production records armed as
`scheduler_tick_now() + timeout_ticks` — so a selector-global "reply now" became invalid in both
directions. Each registration now carries its own `ReplyDeadlineClock`
(`ProductionTick` | `OracleHardware`), written by the single registration seam together with the
deadline and token and never rewritten while that registration is live; the collector and the
reply drain each select per record, and `send_now` is unchanged. No allocation, no ABI change, no
broad-lock fallback, no second drain, no per-architecture policy.

**Exact oracle accounting/scoping is delivered locally.** The `IPC_REPLY_TIMEOUT_ARMED`/`_OK`
marker families stopped being oracle-specific the moment production registration went live, so the
retirement profiles now scope the oracle's assertions to the oracle's exact caller identity (taken
from `IPC_REPLY_TIMEOUT_ORACLE_PROVISION_OK init_tid=`) and bound settlements by registrations
rather than counting a family. `IPC_REPLY_TIMEOUT_ARMED arch=` is reclassified from the feature-OFF
forbidden set to the production-present set on all three ports, because
`arm_production_reply_deadline` is unconditional production code.

**x86_64 retirement is fully green** (both cells, `scan_broad_lock=0`, `production=1`,
`late_reply_successes=0`, `late_timeout_wakes=0`, `duplicate_wakes=0`), and **x86_64 selector-off
production expiry is LIVE-PROVEN on fresh artifacts attesting this commit**. The clock domain is
proven by the arm signature rather than asserted: for the SAME caller and the SAME record
generation 17, a selector-off boot registers `token_generation=17` (the production arm, keyed to
the blocked-receive generation → `ProductionTick`) while a selector-on boot registers
`token_generation=1` (the oracle's constant → `OracleHardware`). Selector-off cell: `ARMED=2 OK=2
COMMITTED=2` — one settlement per registration, exact-once per identity — with `ARM_SKIP=0`,
`ARM_FAIL=0`, zero stranded waiters, zero fatal/panic/page-fault and all 15 graduated-path markers.

**AArch64 and RISC-V are DEFERRED, and only because each is mechanically exact-base-identical at
`8275c927`:** on AArch64 the oracle client workload never starts (`ordered marker sequence
incomplete (ci= cn= rt= ud=)` character-identical to base, client-start markers 0); on RISC-V the
clock repair restored the client's full sequence (`ci=29552 cn=29654 rt=29657`) and eliminated the
duplicate timeout encoding, and its own settlement is exact-once (`ARMED=2 OK=1 COMMITTED=1
CLASS_DONE=1` — the oracle record settles, the unrelated production record correctly does not),
leaving only the client's `caller_result=1 caller_continuations=2 late_reply=1 result=fail`,
character-identical to base. Neither is caused by this checkpoint and neither is a timeout-pipeline
defect.

**AArch64 GIC base-derivation prerequisite — DELIVERED. CENSUS-DELTA 0.** Arming the AArch64 BSP timer needs PPI 30
enabled at the GIC distributor, and reconnaissance found the runtime had no usable distributor
base: `aarch64/dtb.rs` decided which `reg` tuple was the CPU interface at the moment it saw `reg`,
while the `compatible` string was still unknown. QEMU `virt` emits `reg` BEFORE `compatible`
(verified against a real `dumpdtb`), so the parser stored the FIRST tuple — the DISTRIBUTOR at
`0x0800_0000` — as `gic_cpu_if_base`. Every GIC CPU-interface access was therefore aimed at
distributor registers: the priority-mask write landed in `GICD_TYPER` (read-only, ignored) and the
CPU-interface enable in `GICD_CTLR`, silently enabling the distributor. It was invisible only
because AArch64 had never taken an interrupt. The decision is now deferred to the node's
`FDT_END_NODE`, scoped by node depth so a nested `arm,gic-v2m-frame` child cannot contaminate it,
and both bases are committed together or not at all — GICv3 and unknown compatibles get no mapping.
**GICC no longer aliases GICD**: the CPU-interface setter is reachable only with a genuine
CPU-interface base, so priority-mask and enable writes no longer land in `GICD_TYPER`/`GICD_CTLR`.
Live-verified on fresh artifacts attesting this commit: `gic_cpu_if_base=0x8010000
gic_dist_base=0x8000000`, normal boot, with `YARM_SCHED_TICK`/`YARM_TIMER_IRQ_DELIVERED`/
`YARM_TIMER_EOI_DONE` all 0 — no distributor MMIO, PPI enable, timer arm or DAIF unmask is
performed here and no interrupt behaviour is activated. Repository-policy clippy is byte-identical
to exact base `90a52dc` in both errors and warnings; census delta 0.

**AArch64 runtime GIC MMIO mapping prerequisite — DELIVERED. CENSUS-DELTA 0.** Correct bases were
necessary but not sufficient: `TTBR1_EL1` is always zero on this port, so all kernel execution runs
from the active TTBR0 root, and `copy_bootstrap_kernel_root_entries` deliberately skips `L1[0]` —
the 1 GiB entry covering VA `0..0x3FFF_FFFF`, where both the UART and the GIC live — because user
code occupies low addresses. Only the UART re-established a leaf in each new root, so any GIC MMIO
access taken after the first user root was activated (late boot, EL0, or an IRQ claim/EOI, none of
which switch TTBR) faulted on an unmapped address; a late GICD write hung the boot while CNTP, a
system register, did not. Every AArch64 address space now carries privileged **Device-nGnRE,
execute-never, non-user** identity leaves for the DTB-derived GIC distributor and CPU-interface
pages — one 4 KiB page each, never the enclosing 2 MiB block and never the whole bootstrap 1 GiB
entry — established at root creation before activation, so no live TLB shootdown is introduced.
Absent or misaligned bases (notably RPi5's GICv3, for which the parser yields no GICv2 bases) map
nothing and fail closed. User mappings targeting a reserved device leaf are refused. Live-verified
under an active user root by reading only harmless identification registers: `GICD_IIDR=0x43b`
(ARM's JEP106 implementer ID) and `GICC_CTLR=0x1`, at addresses that previously hung the boot.

**AArch64 ProductionTick — DELIVERED. CENSUS-DELTA 0.** Built on the two prerequisites above, the BSP
now drives the default production scheduler tick from the architected timer. `CPU0` alone owns it:
`SchedulerState.timer` is ONE shared `Timer`, not a per-CPU counter, so exactly one CPU may advance
it and the AArch64 AP timer arm was withdrawn — SMP=4 is byte-identical to SMP=1, with every tick
`cpu=0`. The bring-up order is the whole safety argument, and each step is a gate that reports and
stops rather than proceeding: BSP identity → scheduler past bootstrap → `VBAR_EL1` installed →
shared trap state installed → GIC programmed and **read back** (priority mask, CPU interface,
distributor, and CPU0's banked PPI 30 must all agree) → `CNTP` programmed and **read back**
(`CNTP_CTL_EL0 & 0x3 == 1`) → only then `DAIF` is unmasked. A failing gate leaves the CPU masked
with nothing armed. Claim and completion are **split**, because the timer PPI is level-sensitive:
the vector entry claims from `GICC_IAR`, the shared handler ticks once and re-arms `CNTP`
(deasserting the level), and only then is `GICC_EOIR` written — completing earlier would hand the
distributor a still-asserting interrupt. Spurious INTIDs (1020..=1023) complete nothing and neither
tick nor re-arm; a non-timer INTID is reported as an external interrupt, which the shared handler
treats as a no-op. Two latent defects were fixed to get here: the vector stub's `kind` never reached
`yarm_aarch64_vector_entry` (a marker call loads `x0` with `ELR_EL1`, so every exception decoded as
kind `unknown` — harmless while no interrupts were taken, but it makes an IRQ indistinguishable from
a syscall, so nothing claims, ticks, re-arms or completes and the PPI re-presents forever), and
`DEBUG_TIMER_LOG` suppressed `YARM_SCHED_TICK` on non-x86 — AArch64 now shares x86_64's existing
bounded four-tick emission rather than gaining a marker family of its own. Live, default
selector-off: `AARCH64_BSP_TIMER_STARTED cpu=0 intid=30`, ticks strictly monotonic, IRQ count = tick
count = EOI count, no storm, no duplicate tick, no AP tick, `fatal=0 panic=0`.

**The one-tick supervisor policy assumed a dormant clock.** `SUPERVISOR_SHORT_RECV_TIMEOUT_TICKS`
was `1`. The kernel expires on the first tick where `now >= deadline`, and the deadline is
`scheduler_tick_now() + N` sampled at an arbitrary phase inside a tick period, so the budget is
worth `N - 1` COMPLETE periods — `N = 1` guarantees nothing. That was invisible while the scheduler
clock was dormant: the deadline was armed and simply never expired. Once the tick advanced, the
supervisor's process-manager round-trip lost the race, the reply alias was correctly invalidated,
and the peer's later reply was correctly refused with `WrongObject`. **Only that userspace policy
was recalibrated** — to `3`. The binding constraint is the two `query_*_via_process_manager` callers:
each sends an `ipc_call` to PM and waits for the reply **with no retry**, and a timeout there
degrades to `Ok(None)` — a wrong answer rather than a retried one. The peer must be dispatched and
run a full quantum, and may not be picked first, so the requirement is two guaranteed complete
periods, i.e. **`N − 1 >= 2`**, and the smallest defensible value is `3`. It is architecture-neutral:
denominated in scheduler ticks, naming no target. **Kernel timeout semantics were unchanged** — no
deadline arithmetic or comparison, no reply-token or alias lifetime, no settlement or retirement, no
`WrongObject` handling, no timer period or tick source, no per-architecture timeout policy, no
smoke-script BAD-pattern check, no oracle behaviour. Proof that the kernel evidence transfers: the
AArch64 kernel image built after the supervisor-only amendment is **bit-for-bit identical** to the
image that produced the ProductionTick and `TimedOut` proof (`sha256=013c8286…`, both `.bin` and
`.elf`); only the initramfs differs.

**Delivery gates.** Three consecutive AArch64 SMP=1 runs plus SMP=4 all PASS and identical:
`IPC_REPLY_FAIL` 0, supervisor `WrongObject` 0, `fatal=0 panic=0`, `AARCH64_BSP_TIMER_STARTED`
exactly once, ticks strictly monotonic, `cpu=0` the only owner, IRQ = tick = EOI, `scan_broad_lock=0`,
`production=1`; the reply that previously failed now lands (`IPC_REPLY_DELIVER_TO_WAITER tid=2
endpoint=5`). x86_64 core PASS, RISC-V core PASS. Hosted suite 4568/0; 18 integration targets, 0
failures; census and doc guards green; fmt and `git diff --check` clean; `cargo metadata --locked`
exit 0; `cargo check --workspace` 0 errors; all three freestanding checks exit 0;
`ARTIFACT_BUILD_INTEGRITY … result=ok`. Repository-policy clippy adds **no new error class and no new
warning class** against exact base `7fe7d06` — the only deltas are count increases inside pre-existing
classes from the added tests. **The AArch64 reply-timeout retirement profile fails
character-identically to exact base `7fe7d06`** (same failure lines, same seal) and is deferred as
pre-existing, not caused by this checkpoint.

**CANONICAL 199E — DELIVERED and CLOSED. CENSUS-DELTA 0. Direct production remains OFF.**

The stage's closing property is that the off-lock timeout pipeline is production-live and
DEFAULT-REACHABLE on all three architectures, and RISC-V was the last blocker. What holds now:

| property | state |
|---|---|
| RISC-V periodic supervisor timer | unconditional, armed PRE-IDLE at the boot safe point, before the first user task can run |
| timer ownership | boot hart only, including under `-smp 2` |
| `sstatus.SIE` during ordinary S-mode kernel execution | masked; SIE is set at exactly one audited idle boundary, no lock held |
| timer delivery origins | BOTH live — U-origin by RISC-V privilege rules, S-origin at the audited idle boundary |
| asynchronous resume | keyed to the EXACT incoming task `{tid, asid, preempt_generation}`, consumed once, cancelled on a no-switch return, fail-closed otherwise |
| RISC-V userspace FP/vector | soft-float RV64IMAC/LP64, `sstatus.FS`/`VS` forced Off on every U-mode return, illegal instruction to the per-task fault policy |
| selector-off ProductionTick expiry | live-proven end to end without any oracle build or runtime selector |
| retirement accounting | scoped to the DERIVED oracle identity, with global per-identity duplicate detection retained in full |
| admission gate | none — the `riscv64-timer-irq` feature is deleted, not emptied |

Deferred with exact-base signatures: the AArch64 reply-timeout retirement profile and the RISC-V
selector-ON timeout-wins lane both fail mechanically identically to `932bd6f`.

**199E-R3 — retirement accounting is scoped per oracle identity.**
The last obstacle was runner accounting, not kernel behaviour: two completion marker FAMILIES were
asserted `count == 1` over the whole boot, and with ProductionTick default-on a second legitimate
production caller settles its own reply deadline in the same boot — two DIFFERENT tasks, each
delivered exactly once. Both families are now bound to the DERIVED oracle identity (caller TID from
the provisioning marker, ASID and record coordinates from that caller's own registration) AND to a
global per-identity duplicate bound, so an unrelated caller can never satisfy the oracle's
assertion and a duplicate belonging to any caller still fails. Exactly-one was not relaxed, no
production line is discarded, and no timeout or result expectation changed. Seven synthetic
fixtures ship with the runner (`--self-test`) and the wiring is pinned from Rust. Recorded
separately: a pre-existing `assert_order` arity defect (three arguments to a four-parameter
function, `$4: unbound variable` under `set -u`) is repaired by supplying the missing argument
only; reply-wins and reply-wins-repeat now both pass end to end. Detail: `doc/ARCH_RISCV64.md` §15.

**199E-R2 — the RISC-V timer admission gate is RETIRED and asynchronous-resume ownership is
repaired. CENSUS-DELTA 0. Direct production remains OFF.**
RISC-V `ProductionTick` is now default and unconditional: the `riscv64-timer-irq` feature is
deleted rather than emptied, and every gate, audit latch, cfg, selector and dormant fallback is
gone. Timer ownership is boot-hart-only; `sstatus.SIE` is masked during ordinary S-mode kernel
execution; U-origin and audited S-idle-origin delivery are both live-proven. All production
timeout classes use the common off-lock collector/drain, and x86_64, AArch64 and RISC-V all reach
that common pipeline without the broad kernel lock.

Retiring the gate made a pre-existing asynchronous-resume defect reachable, and it is repaired in
the same pass: the preemption tag was being consumed for the OUTGOING task at an in-lock seam that
runs before the resume identity is published (407 tags published, 407 spent there, **0 of 187**
switching write-backs authorized), so a preempted task was resumed with its own stale
syscall-argument mirror over a live computation. There is now exactly one consumer, keyed on the
incoming identity each write-back resolves for itself, with an explicit staged/consume/cancel
lifecycle and named fail-closed refusals. Two defects latent behind the gate are fixed with it:
the S-mode-idle dispatch had no syscall/D2 continuation arm and never activated the resumed task's
address space.

Live: default RISC-V core green 6/6 (five `-smp 1`, one `-smp 2`) with `PAGE_FAULT_UNHANDLED` = 0
and `RISCV_ASYNC_RESUME_REFUSED` = 0; selector-off production expiry drives the full chain once to
`RISCV_IPC_REPLY_TIMEOUT_DONE caller_result=TimedOut caller_continuations=1 late_reply=rejected
result=ok` with `scan_broad_lock=0 production=1` and no `OracleHardware` registration. Hosted
suite 4632 passed / 0 failed; all integration, census and doc guards green; fmt clean;
`cargo metadata --locked` and `cargo check --workspace` exit 0; all three freestanding builds exit
0; x86_64 core and retirement profiles and the AArch64 core profile PASS. The AArch64 reply-timeout
retirement profile fails **mechanically identically to exact base `932bd6f`** (same seven lines,
same seal) and is deferred as pre-existing.

**Deferred, with the established exact-base signature.** The selector-ON timeout-wins lane fails
on ONE absent marker, `RISCV_IPC_REPLY_TIMEOUT_DONE … caller_continuations=1 … result=ok`: the
OracleHardware client emits `caller_continuations=2 … result=fail`, byte-identical to exact base
`932bd6f`. The selector-OFF production lane — the one default ProductionTick actually drives —
reports `caller_continuations=1 … result=ok`. The AArch64 reply-timeout retirement profile fails
mechanically identically to exact base. Both are pre-existing and deferred; every non-deferrable
gate qualifies.

**199E-R1 — the RISC-V S-mode timer-bridge prerequisite is LIVE-PROVEN under the opt-in feature.
CENSUS-DELTA 0.** The `wfi` re-entrancy blocker recorded in `doc/ARCH_RISCV64.md` §13 is resolved:
the bridge now admits a supervisor timer interrupt taken from the audited kernel-idle boundary, and
only that. The audit invariant is that `sstatus.SIE` is set at exactly ONE program point and the
only S-mode code interruptible with it set is `riscv_trap_halt`'s `wfi` loop — hardware clears SIE
on every trap entry so no syscall or fault handler is interruptible, U-mode routing is gated by
privilege rather than SIE, and SIE is enabled last with no lock held. The bridge *checks* this
rather than assuming it: interrupt bit + supervisor-timer cause + `SPP`=Supervisor + a boundary
latch armed at that one point; every other S-mode trap stays fail-closed on the pre-existing
`trap_from_s_mode` halt, and a genuine error inside the accepted path halts rather than resuming as
idle. One accepted interrupt drives one tick through the arch-neutral pipeline (reusing the SAME
shared wrapper, so no broad-lock callsite is added) and one SBI `set_timer`, which on RISC-V is both
acknowledgement and re-arm. Boot hart only. Live, feature-on: 10 interrupts, 10 re-arms, strictly
monotonic `YARM_SCHED_TICK cpu=0`, zero unhandled S-mode traps, no storm, and a task woken from
terminal idle dispatched back into U-mode. **Default builds are unchanged** — `feature=0`,
`RISCV_TIMER_DEFERRED reason=timer_irq_feature_disabled`, scheduler tick 0.

**Both defects the live bridge exposed are repaired. (A) The D2 blocked-receive return provenance
hole:** the U4 D2 in-lock bypass returns before the arch-neutral syscall-return classification that
publishes idle provenance, so a deferred receive never published a token and the wrapper's
terminal-idle handling took the defensive error path. Publication now happens at the **authoritative
D2 completion point** — the post-lock drain's clean idle outcome, the very point the bypass diverted
control away from — where exact identity and generation are already re-verified, the block is
committed, the outcome is a clean `Mark::Idle` (refusals and torn dispatches publish nothing), an
actual deferral is pending, it runs exactly once, and the broad guard is already dropped. The
defensive guard is not weakened, nothing is synthesized in the timer handler, and nothing is
inferred from a bare tid. It is RISC-V-local because RISC-V is the sole reader. **(B) Trap-stack
drift:** entry is `sscratch`-idempotent, so the drift came from the return, which wrote the saved sp
into `sscratch` and then swapped — at which point sp was the *bridge's* pointer, leaving `sscratch`
one frame lower every trap. The return now writes the canonical per-hart top and loads the
interrupted sp directly from the frame, so the interrupted sp round-trips exactly while the next
entry always starts from a fixed address; the top is resolved per-hart from the frame pointer, so
ownership cannot alias, and the task-switch ABI is unchanged. Verified live over 800+ traps with a
temporary diagnostic (identical frame address on every sample; diagnostic removed).

**Live, feature-on, repeated:** 10 interrupts = 10 ticks = 10 re-arms, monotonic CPU0-only ticks,
zero unhandled S-mode traps, zero `TRAP_HANDLE_FAILED`, zero `BLOCKED_IDLE_NO_PROVENANCE`,
`scan_broad_lock=0 production=1`, provenance published once and consumed normally, and a task woken
from terminal idle returned to U-mode. Default RISC-V remains `feature=0`, tick 0, PASS.

**RISC-V default ProductionTick is still OFF, and canonical 199E remains OPEN.** Two things remain.
The production reply deadline is armed in the `ProductionTick` domain and the collector/drain runs
every tick, but the supervisor's reply legitimately wins and the record is correctly disarmed, so
`TimedOut` does not fire — the same intended outcome the AArch64 checkpoint records. And
**The opt-in RISC-V timer is now periodic, not idle-only.** `program_timer_deadline` is implemented
as the ONE common re-arm point: it sits at the tail of the arch-neutral `Trap::TimerInterrupt` arm,
which BOTH accepted origins pass through, so a timer taken from U-mode and one taken from the audited
S-mode idle boundary re-arm through the same seam — exactly once each. The duplicate S-mode-local
re-arm was removed. SBI `set_timer` is the single call that both clears the pending timer condition
and programs the next deadline, so there is no separate completion; boot hart only; no PLIC
involvement; a complete no-op with the feature off. A second defect was fixed alongside it:
`sstatus.SIE` was set only on the FIRST idle entry, and every later arrival comes from a trap handler
where hardware cleared it, so the idle `wfi` looped masked and ticks stopped as soon as anything was
dispatched. `reestablish_idle_boundary` re-enables it at the SAME audited boundary, gated on
`stie_enabled()`, so the audit invariant is unchanged. Live: 480 accepted interrupts = 480 re-arms,
strict 1:1, no storm, no stack drift, no unhandled trap.

**Both remaining gaps are now closed, live.** A profile-confined selector-off proof cell supplies
what the core workload could not. Its client blocks on a real finite reply deadline while its server
blocks on a second receive on the *existing* request endpoint, retaining the reply capability — with
both parties blocked the CPU reaches terminal idle, the timer arms, and its S-mode ticks advance the
scheduler tick the production deadline is measured against. That is the first live **non-oracle
`ProductionTick` reply-deadline expiry**: `IPC_REPLY_TIMEOUT_ARMED … deadline=3` →
`… DEFERRED published=1 drained=1` → `… OK terminal=Timeout timeout_result=TimedOut caller_wakes=1`
→ `COMPLETION_COMMITTED` → `GLOBAL_LOCK_RETIRE_CLASS_DONE class=IpcReplyTimeout` → the caller's
canonical `TimedOut`. The caller then runs a bounded syscall-free U-mode dwell against the
already-armed timer, which is the first live witness of **U-mode-origin timer interrupts**
(`RISCV_U_MODE_TIMER_ACCEPTED … origin=user`), and finally triggers the retained late reply, which
is rejected exactly once. The lane is told apart from the oracle lane by a fourth value appended to
each architecture's existing slot-5 liveness run (`ProductionTimeoutWins`, base+3) — proof-protocol
metadata in the same slot, no ABI slot and no capability added — published only by RISC-V, so
x86_64 and AArch64 provisioning is untouched.

**199E-R1D — asynchronous U-mode preemption preserves the exact interrupted context.** The proof
above exposed a latent whole-port defect the timer had never been able to reach: nothing captured a
user register file interrupted *asynchronously*. `Trap::Syscall` snapshots via
`sync_current_thread_from_frame`; the timer arm never did, so a preempted task's TCB still held
whatever its last `ecall` stored, and a switch away and back resumed it at that `ecall`'s saved PC
and return lane — measured as a caller re-running its receive continuation 75 times. Both RISC-V
resume conventions were also lane conventions keyed to a syscall boundary: fresh/startup installs
`a0..a5` from the argument mirror and zeroes `a7`; a syscall continuation installs its result lane.
Neither is correct for a task interrupted mid-computation, where `a0..a7` are ordinary live
registers. RISC-V now has an explicit **third resume state**: the snapshot is taken on a U-origin
timer trap strictly before anything that can schedule, tagged with the exact
`{tid, asid, preempt_generation}` incarnation (`checked_add`, context written before the tag), and
consumed exactly once at whichever resume boundary runs — in-lock or post-lock — which restores the
complete integer register file including `a0..a7` and never passes through the startup rewrite. A
replacement incarnation reusing the numeric TID, a superseded generation, or a missing tag all fail
closed; exit clears the tag. Nothing is inferred from zero registers, a PC value, a bare TID or
incidental scheduler state. Fresh, syscall-return and D2 paths are unchanged, and the repaired
canonical `sscratch` stack-top invariant is retained. Live: five real switch-away/switch-back cycles
inside one dwell, each restoring `sepc=0x406d16`, the same user `sp`, `a0=0xa0a00001dead0000` and
`a7=0xa7a70008dead0007` with `startup_rewrite=0`; an assembly register canary reports
`mismatches=0x0000 result=ok`; `caller_continuations=1`, not 75.

**199E-R1F — the RISC-V userspace FP/vector policy is now an explicit fail-closed decision.**
The previous checkpoint could only say the integer-context proof was complete *because no binary
happened to contain an FP instruction*, while the user target advertised `lp64d` and `sstatus.FS`
arrived from OpenSBI reading Dirty — so the hardware permitted user floating-point the whole time
and a single `f64` in a server would have become silent cross-task corruption. That accident is
replaced by a decision with two halves, each insufficient alone. **RISC-V userspace is
intentionally RV64IMAC/LP64 soft-float and integer-only**: the user target is `lp64` with
`+m,+a,+c` — RV64IMAC — and no `F`/`D`/`V` features, so the compiler cannot emit floating-point or
vector instructions. And
**every U-mode return forces `sstatus.FS = Off` and `VS = Off`** through one sanitizer
(`arch/riscv64/user_status.rs`), applied at the first user entry, syscall return, blocked/D2
continuation and the asynchronous-preemption restore alike — the generic trap tail restores
`sstatus` straight from the frame, so sanitizing the frame covers every resume class and none can
opt out. **An unsupported FP/vector instruction therefore fails closed**: it raises an
illegal-instruction trap, which is routed into the *existing* per-task user-fault policy — fault
report, `FaultPolicy`, block, `Faulted`, dispatch a replacement — rather than being resumed
forever (the old hosted behaviour) or panicking the whole kernel for one task's bad instruction
(the old freestanding behaviour). No emulation, no lazy-FPU, no partial save area.

Static proof across all twelve RISC-V user images: ELF flags `0x1, RVC` with the `double-float
ABI` bit gone, no `.riscv.attributes` section declaring an F/D/V requirement, and zero FP/vector
instructions and zero f-register references. The kernel target keeps RV64GC/`lp64d` as permitted,
and kernel code uses no FP/vector state (0 f-registers, 0 FP instructions, 0 `fcsr` accesses).
**The difference between the two targets is intentionally and exclusively FP capability and FP
ABI** — the integer feature sets are identical, and a guard fails on any other divergence. The
split is sound because **no syscall, startup or IPC interface carries an FP value**: there is no
`f32`/`f64` anywhere in the user crates or the shared IPC ABI, every startup-argument slot and
every syscall lane is an integer register, and a guard fails if one appears. **Asynchronous integer-context preemption remains live-proven** under the new
soft-float userspace: six switch-away/switch-back cycles, register canary `mismatches=0x0000
result=ok`, `caller_continuations=1`, and zero `USER_UNSUPPORTED_INSTRUCTION` from existing
userspace. **Full FP/vector support remains future work, and requires per-task FP ownership plus
save/restore** — a per-task owner for the FP/vector unit and a save area sized for `f0..f31` and
`fcsr` snapshotted with the integer file (or an equivalent lazy/dirty-tracking scheme). This
checkpoint is the fail-closed policy that makes its absence honest, not a claim of FP-safe
asynchronous preemption.

`riscv64-timer-irq` still stays **default-OFF**. With the FP-state policy now decided, **default
timer admission is the sole remaining 199E blocker**, pending the final gate-retirement
checkpoint. AArch64 and x86_64 ProductionTick status is unchanged. Census delta 0;
direct production remains OFF (`IPCCALL_DIRECT_PROOF_ENABLED: AtomicBool::new(false)`); ownership
production callers 0; no U8 or WA3C2 work is begun.

**U7 (canonical 199E, partial).** The off-lock timeout pipeline is now PRODUCTION on all
three architectures: `SharedKernel::run_due_ipc_timeout_work` is driven unconditionally from
every port's post-lock area, and `IPC_REPLY_TIMEOUT_LOCK_STATUS arch=… scan_broad_lock=0 …
production=1` appears on an ordinary core-smoke boot of x86_64, AArch64 and RISC-V.
`process_ipc_timeout_deadlines` unconditionally skips both retired classes, so the only class
it still owns is the ordinary receive timeout. **Blocking-send timeout is fully retired and
production-reachable.** **Reply/call timeout is retired as a pipeline but not yet reachable**:
no production site registers a token-bearing reply deadline (the only one is the oracle-gated
`maybe_arm_reply_timeout_oracle`), so that quarter still needs a real deadline queue and a
unified deadline timebase. `IpcRecvTimeout` and `IpcCall` timeout remain open, so **199E stays
OPEN**.

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

   **WA3B: the last Group-3 CAN site is closed; spawn can no longer overwrite a live task.**
   The WA3A hard-stop was that `spawn_user_task_from_image`'s only authorization was
   `register_task_with_class` idempotence, so a spawn could overwrite an existing task's register
   context, entry, stack, ASID and scheduler membership — a `Blocked(EndpointReceive)` receiver as
   easily as an idle one. The literal absence gate was reverted because bootstrap legitimately
   pre-registers the RING3 TIDs so the boot capability grants have a destination CNode.

   The resolution is that "provisioned" and "live" were the same thing and should not be. A new
   `TaskStatus::Reserved` carries a `SpawnReservation { generation, class, process_pid, phase }`,
   created by `reserve_task_for_spawn_with_class` through the SAME capacity/CNode/class/kernel-
   context provisioning ordinary registration uses — so bootstrap's dependency is unchanged — but
   a reservation is not live: it cannot be enqueued (guards on both enqueue seams), cannot be
   dispatched, cannot be woken, and cannot block or publish a waiter. Choosing a status variant
   rather than a side field is what makes those hold structurally: every site that allow-lists
   statuses now refuses reservations automatically, and only two exhaustive `TaskStatus` matches
   existed in the kernel to update.

   **Exact one-shot consumption.** `spawn_user_task_from_image(token, spec)` validates TID,
   generation, class, process and phase atomically before ANY spawn-specific mutation, claims
   `ReservedUnstarted → Spawning`, and commits `Spawning → LiveSpawned` strictly before the
   enqueue — so nothing partial is ever scheduler-visible. The generation is monotonic and never
   derived from the TID, so a token for an earlier occupant of a numeric TID cannot act on a
   later one. `cancel_spawn_reservation` covers setup failing before spawn, validating read-only
   first and reusing the existing `release_kernel_context` + no-alloc process-CNode reap.

   **Failed spawn restores an exact baseline.** "All fallible work before any TCB mutation" is
   *not* achievable — spawn binds the incoming ASID before the fallible stack allocation, because
   the x86_64 switch-frame retry and the stack allocator both need it bound. So the claim captures
   a `SpawnBaseline` of every field the body may write and a failure replays all of them: after an
   ordinary error the reservation is observationally identical to the pre-claim reservation, with
   no stale ASID or partial live identity and nothing enqueued. VM/capability cleanup on failure
   is unchanged and remains a pre-existing gap this stage does not open.

   All three architectures now bootstrap as **reserve → grant → consume**. The production caller
   closure is **13**, not the 18 previously counted — that figure included six `#[cfg(test)]`
   sites — and is pinned mechanically.

   **Census: 12 / 16 / 7 / 2 / 1 / 0, total 38.** The two predicted movements hold exactly
   (CAN 13 → 12, CANNOT 15 → 16). The total moved from 37 because `ThreadControlBlock::reserved`
   is a genuinely new status writer, classified `FRESH_CONSTRUCTOR` alongside `new`; forcing 37
   would have meant hiding it or hiding spawn's departure from the raw-write set. Composition:
   29 raw writes + 8 transition-barriered + 1 reservation-barriered. Waiter ownership still has
   **zero production callers**, `WAITER_OWNERSHIP_EXCLUSIVE=no`,
   `WAITER_OWNER_CENSUS_COMPLETE=yes`, direct production default **OFF**, canonical 199D **OPEN**,
   ledger **39 / 7 / 46**, **no new live cell**. Spawn work stops here; the next stage is
   production endpoint-waiter ARM/RETIRE wiring.
   See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.37.

   **WA3C1: the waiter record becomes generation-bearing, removal is centralized, and two real
   production defects in `destroy_endpoint` are fixed. WA3C was SPLIT by a proven blocker.**
   WA3C intended to wire the WA2A ownership primitive into the live waiter lifecycle. Exact
   ownership requires STRICT single-waiter publication — `arm_current` refuses an occupied
   ownership slot, so a silent replacement would strand the previous receiver with neither a
   waiter nor an owner. Making publication strict produced a **reproducible hang** in
   `vfs_file_grant_ro_relay_preserves_transferred_cap`, which passes in ~0.03 s at the accepted
   base and ~0.08 s under WA3C1. Waiter replacement turns out to be a deliberate contract, not an
   accident: six named tests plus the 35-test `stage199d_delivery_projection_differential` family
   exercise it, and the relay, direct-request, direct-reply and reply-timeout paths build rollback
   behaviour on it. Whether YARM should keep replacement is a design question that must be decided
   from those contracts — not settled as a side effect of wiring ownership. WA3C1 therefore
   changes none of it, and WA3C2 owns the question.

   **What landed.** `endpoint_waiters` now stores `EndpointWaiterRecord { receiver,
   wait_generation }` rather than a bare identity, with the generation IN the record rather than a
   parallel array, so the waiter and its incarnation have one lifetime. A fresh blocked-receive
   generation is minted with `checked_add` under task rank 2 in Phase B — in the same acquisition
   that marks the task Blocked — and threaded through `RecvBlockPhasePlan` into Phase C, never
   re-read by bare TID; exhaustion fails closed and unwinds coherently. One central
   `remove_endpoint_waiter_at` owns slot clear, direct-ack lease release and census unlink, and
   the take / exact-clear / identity-clear families all delegate to it. The direct-ack lease key is
   deliberately unchanged.

   **Two real defects fixed.** Waiter *displacement* never released the displaced waiter's
   direct-ack lease. And `destroy_endpoint` did a raw `endpoint_waiters[idx] = None` that bypassed
   the accessor family entirely, so it released no lease and unlinked no census — a *permanent*
   leak, because the lease is keyed on the endpoint generation the next line advanced past. It
   also discarded the parked receiver, leaving it `Blocked(EndpointReceive)` on an endpoint that no
   longer existed. Removal now happens while the OLD generation is authoritative, and the receiver
   is woken only when `{tid, asid, wait_generation, Blocked(EndpointReceive)}` all still match — so
   a replacement incarnation, a completed receiver, or one that re-blocked under a newer generation
   is never resurrected. That makes `destroy_endpoint` a NEW production caller of
   `wake_tid_to_runnable`, recorded in the pinned caller set rather than suppressed.

   Census recomputed and unchanged: **CAN 12 / CANNOT 16 / INTO_BLOCKED 7 / FRESH_CONSTRUCTOR 2 /
   NON_PRODUCTION 1 / UNPROVEN 0, total 38** — the generation write is not a `TaskStatus` write.
   Ownership stays **helper-only**: production callers of `arm_current`, `claim`, `consume`,
   `cancel`, `restore` and `retire_current` are all **0**, pinned by guard.
   `WAITER_OWNER_CENSUS_COMPLETE=yes`, `WAITER_OWNERSHIP_EXCLUSIVE=no`, direct production default
   **OFF**, canonical 199D **OPEN**, ledger **39 / 7 / 46**, **no new live cell**.
   See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.38.

   `stage199d_riscv_canonical_admission` (11 tests) pins the
   contract. See `doc/KERNEL_UNLOCK_AUDIT.md` §6.1.19.
3. **`d6_genuine_enabled()` is compile-time x86_64-only** — 203C blocked; AArch64 and
   RISC-V cannot retire any queue-advancing class.
4. **Every capability seam is `M2_SEAM_HELPER_ONLY`** — all of Phase 3 has zero production
   wiring.
5. **`FutexWait` off-lock seams landed helper-only** and were never wired.
6. **Reply-timeout scan is off-lock on x86_64 only**; `IpcSend` and `IpcCall` timeouts are
   untouched. U1 deleted the callerless `SharedKernel::run_reply_timeout_completion` wrapper,
   but the in-lock scan and its completion body remain on every architecture (199E).
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
| Off-lock authoritative dispatch (D6-genuine) | ✅ **production, no opt-out** — `d6_genuine_enabled()` is compile-time true on x86_64 unless a D6 switch diagnostic owns the switch path; the D2-send / D2-recv / FutexWait / Yield / D6 drains all run with the broad guard dropped. **U4:** the D2 recv/send half is now production on AArch64 and RISC-V too, through `queue_advancing_dispatch_enabled()`; each architecture drains the deferral in its own wrapper and resumes through its own exact-token transaction |
| D6 switch proof harness | 🧪 default-off diagnostic only (`yarm.d6_switch_proof=1` / `D6_SWITCH_PROOF=1`); mutually exclusive with the production D6-genuine path. Historical bring-up detail (Stages 120–132) is in `doc/PROJECT_HISTORY.md` |
| AP scheduler participation | 🧪 **proof-gated live** — `X86_AP_GENERIC_RETURN_SEAL`, `X86_AP_SAVED_RETURN_SEAL`, `X86_AP_RECV_V2_BLOCK_SEAL` and the cross-CPU NR6/NR7 seals all earned live at SMP=2 under default-off knobs; the **production** scheduler is still BSP-only (`online_cpu_count()` == 1) |
| Timer interrupts on APs | ❌ not enabled on the production path |
| Restore-owner selection | ⚠️ resolved **in-lock** (identity-coherent) and revalidated after the drains by `revalidate_idle_owner_after_drains`. **Corrected at U3 (203C):** that revalidation IS live in QEMU — `scripts/qemu-x86_64-server-dies-smoke.sh` reaches `EXIT_TASK_OWNER_REVALIDATED arch=x86_64 … prepared=idle committed=replacement advances=1 broad_lock=0 result=ok` on every run, and the increment that retired its broad re-acquisition was accepted against five consecutive such runs. The earlier "never run in QEMU" claim was stale |

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
| Terminal state | ✅ `RISCV_KERNEL_IDLE_WAITING_FOR_IO reason=no_runnable_task all_services_blocked` (event-driven idle; the periodic supervisor timer is live and re-arms across it) |
| Regular smoke target (`--smp 1/2/3/4`) | ✅ `scripts/qemu-riscv64-core-smoke.sh` + `scripts/qemu-riscv64-smoke-matrix.sh` enforce the full per-N marker contract on QEMU virt + OpenSBI |
| Ready for global kernel-unlocking smoke matrix | ✅ **Ready: yes** — see `doc/ARCH_RISCV64.md` §13.5; the regular core smoke is RISC-V's per-arch gate, treated the same way as x86_64 / AArch64 core smokes |
| Timer audit scaffold | ✅ `RISCV_TIMER_AUDIT_BEGIN` + `RISCV_TIMER_AUDIT_DONE sbi_time=1 boot_hart=1 gate=none admission=default`; the only deferred reasons the smoke gate still pins are genuine platform/ownership facts (`sbi_time_ext_unavailable`, `not_boot_hart`, `already_armed`, `unsafe_under_current_satp`) |
| Timer interrupt (live) | ✅ **default ON, unconditional, boot-hart-owned.** The `riscv64-timer-irq` feature is deleted, not emptied; no cfg, selector, dormant fallback or runtime disable knob remains. Armed at the boot safe point — `RISCV_TIMER_ARMED_PRE_IDLE owner=boot_hart sie=0 delivery=u_mode_privilege result=ok` — before the first user task runs and long before the first terminal idle. `sstatus.SIE` stays 0 through the arm; U-mode delivery rides on privilege rules, so ordinary S-mode kernel code stays non-interruptible and SIE is enabled only in the audited idle tail. Live: both interrupt origins, IRQ = tick = SBI re-arm exactly, boot-hart-only under `-smp 2`. See `doc/ARCH_RISCV64.md` §14 |
| Asynchronous U-mode preemption | ✅ one incoming-identity consumer (`classify_and_take_async_resume`); a snapshot stays attached to its exact `{tid, asid, preempt_generation}` until that task is resumed, is consumed once, is cancelled on a no-switch return, and fails closed with a named reason otherwise. Both write-backs (bridge and S-mode-idle dispatch) select exactly one of `AsyncPreempted` / syscall-D2 continuation / fresh-startup from explicit decisions. Live: 8 genuine switch-away/switch-back resumes with canary `mismatches=0x0000` |
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
   The broad `SpinLock<KernelState>` still has **7** production acquisition sites (§0).
   The ServerDies reverse-link accounting failure that used to head this list is
   **resolved** (`doc/IPC.md` §8.5): the transition counters now describe exactly one armed
   ServerDies transaction and the leak invariant moved to system-wide link totals, so there
   is no hard `result=fail` left in the tree. Two follow-ons, of different kinds: the
   **x86_64 ServerDies live cell** (a runner act — one clean boot earns the first cell. The
   parenthetical that `revalidate_idle_owner_after_drains` "has never run in QEMU" was stale
   and is corrected at U3 (203C): the runner reaches it live, and its broad re-acquisition was
   retired against five consecutive runs), and
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
