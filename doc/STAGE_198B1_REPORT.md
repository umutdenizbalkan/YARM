<!-- SPDX-License-Identifier: Apache-2.0 -->
# Stage 198B1 — Build Integrity, Ordinary-Cap Negative Seal, and Crash-Restart Baseline

Base commit: `b645c9b` (Stage 198B — second-cohort ordinary-cap IpcSend cross-arch parity).
Branch: `claude/yarm-188g-post-merge-audit-cndn9m`.

This stage hardens the *evidence and regression infrastructure* for the already-completed
Stage 198B ordinary-cap work. It adds NO new retirement class, NO reply-cap / shared-region /
D2 work, NO new syscall, and does not change the ordinary-cap ABI. `SYSCALL_COUNT=32`,
`VARIANT_COUNT=22`, NR27 (`InitramfsReadChunk`) remains removed.

## 22-item final report

1. **Part A — fail-closed builds (DONE, proven).** All three artifact build scripts
   (`build-qemu-{x86_64,aarch64,riscv64}-artifacts.sh`) now use `set -euo pipefail`, record a
   build-start epoch, `rm -f` stale outputs up front, publish atomically (stage `*.staging.$$`
   → `mv`), gate the published binary on mtime > build-start (freshness), verify required
   retirement markers are present in the *new* binary and forbidden obsolete markers are absent,
   and record commit/sha256/size/mtime into a manifest. `common_exit_if_strict_mode` fails closed
   (`exit 1` unless `ARTIFACTS_SOFT_FAIL=1`). A dedicated negative test injects a `compile_error!`
   into the kernel, confirms the build exits non-zero, confirms **no** stale kernel artifact is
   left at the output path, and confirms the ordinary-cap seal then refuses (missing_artifacts).
   Evidence: `ARTIFACT_BUILD_INTEGRITY_SEAL arches=3 stale_artifact_acceptance=0
   failed_build_rejected=1 result=ok`.

2. **Part A — per-arch integrity line (DONE).** Each fresh build emits
   `ARTIFACT_BUILD_INTEGRITY arch=<arch> stale_artifact_acceptance=0 failed_build_rejected=1
   result=ok`. Confirmed on the fresh 3-arch rebuild used for this stage (§18).

3. **Part B — isolation model (Model 1: serialized master) (DONE).**
   `scripts/qemu-combined-retirement-seal.sh` runs the first-cohort (12), plain (6) and
   ordinary-cap (6) seals **strictly sequentially** — only one QEMU at a time — each with a
   unique per-run `LOGDIR` and `ORACLE_RUN_ID`, so there is no CPU/memory starvation and no
   shared log/artifact/socket contention. Shared resources audited: target dirs (per-arch, fixed),
   QEMU serial (per-boot stdio, no sockets), oracle scratch logs (now per-invocation
   `ORACLE_SCRATCH_DIR`), timeout state (per inner script), manifests (per-arch).

4. **Part B — repeated-run isolation proof.** `scripts/qemu-retirement-seal-isolation.sh` runs the
   combined seal 3× consecutively and asserts each cohort cell appears exactly once per run, no
   run times out (inner exit 124), and every run's `COMBINED_RETIREMENT_SEAL` is `result=ok`,
   emitting `RETIREMENT_SEAL_ISOLATION serialized=1 repeated_runs=3 successful_runs=3
   contaminated_runs=0 timeout_runs=0 result=ok`. Run 1 was observed fully green
   (`COMBINED_RETIREMENT_SEAL first=12 plain=6 ordinary_cap=6 result=ok`) against the fresh 3-arch
   artifacts; the container was suspended/resumed mid-run-2 (which invalidates the wall-clock QEMU
   timeouts for that partial run), so the authoritative 3/3 repetition is delivered under the
   Stage 198C serialized-runner acceptance, which re-runs this same combined seal 3× with the added
   reply-cap-direct cohort. <!-- ISOLATION_RESULT -->

5. **Part C — capability-rights attestation semantics.** Ordinary-cap transfer is
   **COPY/DELEGATION, not move**: the source cap is recorded only as a delegation-tree parent edge
   and is never revoked; the destination rights equal the source rights (no attenuation). The
   delivery layer resolves the FULL freshly-minted capability
   (`resolved_capability_split(receiver_cnode, cap)`) and compares the full `CapObject` for
   identity, then attests destination rights against the canonical transfer result.

6. **Part C — direct + queued attestation markers (DONE, live-verified on all 3 arches).** For both
   direct (`class=IpcSendOrdinaryCap`) and queued (`class=IpcSendOrdinaryCapEnqueue`) delivery the
   boot emits `IPC_ORDINARY_CAP_RIGHTS receiver_tid=<t> dst_rights=Some(<r>) expected_rights=<r>
   rights_ok=1 object_endpoint=1 reply_object=0 generation=<g>` and
   `IPCSEND_ORDINARY_CAP_RIGHTS_OK arch=x86_64 class=<class> source_semantics=copy
   destination_rights_ok=1 source_still_valid=1 reply_metadata=0`. `source_still_valid=1` is proven
   by re-exercising the sender's source cap with a plain probe send *after* the transfer (copy
   semantics); `reply_metadata=0`/`reply_object=0` proves no reply-cap misclassification; a fresh
   receiver-local CapId (≠ sender handle, ≠ source cap) is minted (`fresh_cap=1`).

7. **Part C — wired into the ordinary-cap seal (DONE, 6/6).** `qemu-second-cohort-ordinary-cap-seal.sh`
   now requires the per-cell `IPCSEND_ORDINARY_CAP_RIGHTS_OK … destination_rights_ok=1
   source_still_valid=1 reply_metadata=0` marker in addition to the retirement + oracle-done markers
   before a cell counts as live (`proof=live rights=attested`). The oracle smoke echoes the rights
   marker on every arch (the raw serial otherwise reaches only the per-run analysis log, not the
   stdout the seal greps) and additionally asserts the kernel `IPC_ORDINARY_CAP_RIGHTS … rights_ok=1
   reply_object=0`. Confirmed against the fresh 3-arch artifacts: `SECOND_COHORT_ORDINARY_CAP_MATRIX
   arches=3 classes=2 live_cells=6 result=ok` / `SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2
   live_cells=6 result=ok`, every cell `rights=attested`.

8. **Part D — ordinary-cap negative + rollback seal (DONE, hosted).**
   `scripts/qemu-ordinary-cap-negative-seal.sh` drives the ACTUAL production transaction /
   finalization helpers (`produce_blocked_waiter_ordinary_cap_delivery` +
   `SharedKernel::drain_dispatch_post_work` for the direct path; `complete_recv_boundary_ordinary_cap`
   for the enqueue path) through 9 direct + 3 enqueue negative/rollback cases and emits
   `SECOND_COHORT_ORDINARY_CAP_NEGATIVE_SEAL direct_cases=9 enqueue_cases=3 leaked_caps=0
   leaked_envelopes=0 duplicate_wakes=0 result=ok`.

9. **Part D — cases covered.** Direct: cap materialization failure → mint rolled back, source
   `cap_refcount` restored; invalid receiver payload destination → Phase-A rejection, no stash,
   envelope retained (retryable); invalid receiver metadata destination → rejection after envelope
   consume, nothing minted; reply-cap message → not produced (no ordinary/reply misclassification);
   plain / shared-region → not produced; no trap drainer → not produced; missing/consumed source
   envelope → synchronous error, no stash; executor seam+rollback structure (copy-fault rolls back
   the mint). Enqueue: phony/dead source cap on the boundary transfer → materialization fails;
   queued mem-object and endpoint cap transfers routed through the real boundary seam.

10. **Part D — invariants.** No receiver wake on failure and no partial payload/metadata ever
    visible (proven by "no post-work stashed" ⇒ no delivery/wake); no receiver-cap leak and no
    object-reference leak (source `cap_refcount` unchanged across every failure path); no
    transfer-envelope leak (envelope retained/retryable on Phase-A reject); no duplicate waiter /
    duplicate queue entry (stash discipline, one-shot envelope consume); no CNode/queue capacity
    increase.

11. **Part E — crash-restart baseline root cause (RESOLVED).** The failure was a **wall-clock
    budget artifact, not a kernel lifecycle bug and not a stale artifact/marker**. The
    `crash_test_srv` restart chain is deterministic and COMPLETES all four instances
    (`tid 10008→10009→10010→10011`) plus the terminal `RESTART_LIMIT_EXCEEDED` /
    `SERVICE_DEGRADED_FINAL` / `DEGRADED_TERMINAL_APPLY_OK`. In a fresh uncontended boot the full
    sequence spans ~414k log lines (first fault at ~line 117k; the instance previously believed to
    "never enter", tid 10010, actually enters at line 311314 and faults at 313854; tid 10011 enters
    at 408506; degraded-final at 411174), needing ~150s. The historic 90s `TIMEOUT_SECS` truncated
    the log mid-chain (typically after two instances) and — compounded by background QEMU
    contention in earlier observation — was misread as a restart stall.

12. **Part E — fix (bounded, no marker exclusion, no blind timeout bump).** `TIMEOUT_SECS` default
    raised 90→240 with a comment documenting that this is a wall-clock budget, not a masked missing
    transition: every transition is line-proven present in a long-enough run, so no failing
    process/marker was excluded, no terminal-idle success was declared before crash/restart
    completed, and the timeout was not raised to paper over a missing transition. The `count_marker`
    oracle is **not** buggy — on the fresh run it counted `CRASH_TEST_SRV_ENTRY=4`,
    `CRASH_TEST_SRV_FAULT_NOW=4`, `PM_RESTART_REPLY_ACCEPTED=3`,
    `SUPERVISOR_PM_RESTART_STATE_UPDATED=3`, `SUPERVISOR_RESTART_LIMIT_EXCEEDED=1`,
    `SUPERVISOR_SERVICE_DEGRADED_FINAL=1` exactly.

13. **Part E — baseline marker (DONE, proven).** The smoke's success path emits
    `SUPERVISOR_CRASH_RESTART_BASELINE fault_observed=1 supervisor_notified=1 restart_observed=1
    stale_reply_objects=0 result=ok`, each field derived from the oracle's own counts; the crash-test
    binary is `sbin/crash_test_srv` sha256 `f655225c348e15e2fd85364bc53c89fb2ffae6c4bd83d5da40a04797fb25e296`
    (deterministic null-deref fault after 128 yields). `stale_reply_objects=0` excludes the 5 benign
    pre-crash startup control-recv `WrongObject` probes (they do not match the restart-token-query
    fatal pattern).

14. **Part F — DebugLog widening stays bounded (DONE, hosted).** All three kernel copy limits plus
    the userspace mirror are exactly `192`: userspace `MAX_LOG_LEN = 192`
    (`crates/yarm-user-rt/src/lib.rs`), the global handler `DEBUG_LOG_MAX_BYTES = 192`
    (`syscall/debug.rs`), the split handler (`syscall_split.rs`) and its copy helper
    `copy_from_user_asid_split_read` (`runtime.rs`) both route through the shared const. Over-length
    policy is **truncate** (`raw_len.min(DEBUG_LOG_MAX_BYTES)`) identically at both handlers, with a
    defensive `len > MAX` reject in the copy helper; global and split paths therefore return
    identical results. The strengthened `debuglog_192_bound_is_capped_identical_and_trapframe_safe`
    test pins one canonical definition each, rejects any wider redefinition on the three copy paths,
    and asserts the 192-byte buffer is a handler local **never** embedded in `TrapFrame` (no risky
    trap-frame stack expansion) and that the cap const is not arch-gated (identical on all 3 arches).

15. **Part G — functional seals preserved.** `FIRST_COHORT_CROSS_ARCH_SEAL arches=3 classes=4
    result=ok` (12/12), `SECOND_COHORT_PLAIN_SEAL arches=3 classes=2 live_cells=6 result=ok` (6/6),
    and `SECOND_COHORT_ORDINARY_CAP_SEAL arches=3 classes=2 live_cells=6 result=ok` (6/6) are
    re-proven by the serialized combined runner (`COMBINED_RETIREMENT_SEAL first=12 plain=6
    ordinary_cap=6 result=ok`). All three cohorts have been observed green against the fresh 3-arch
    artifacts: first-cohort 12/12 and plain 6/6 (serialized combined run) and ordinary-cap 6/6
    (standalone re-run after the Part C rights-marker capture fix). **Status: the aggregate
    `COMBINED_RETIREMENT_SEAL` / `RETIREMENT_SEAL_ISOLATION` lines are being re-confirmed by the
    in-progress 3× isolation run (§4); this line will carry the final aggregate result.**
    <!-- SEAL_RESULT -->

16. **Part G — exclusions honored.** No `IpcSendReplyCap` / `ReplyCapEnqueue` / shared-region / D2
    retirement is exercised or claimed (the ordinary-cap seal's `FORBIDDEN` pattern actively rejects
    `class=IpcSendReplyCap` and `class=IpcSendSharedRegion`). NR27 references = 0 live (only
    removal-documenting comments remain). No full global-lock-retirement claim is made.

17. **Validation — formatting/whitespace.** `cargo fmt --check` clean; `git diff --check` clean.

18. **Validation — fresh 3-arch build (mandatory).** Rebuilt from `06bc431` via the fail-closed
    scripts; all three emitted `ARTIFACT_BUILD_INTEGRITY … result=ok`. Artifacts:
    `build-x86_64/kernel_boot.elf` `823c7daaec152b087ea4c9132b6c79afcc53cc5cf04b6001130fb86b5772c36d`
    (2026-07-14 09:43:17); `build-aarch64/yarm-aarch64.bin`
    `4b8d687f43e9873a5264710551d2caebd580cd4ce577f2ec74f1cf6ba77b5795` (09:43:47);
    `build-riscv64/yarm-riscv64.bin` `491941cb28b00b75aeacf5fd1d8b33ba7d53e8ea3ed95f7b343662e1bfc0a3ca`
    (09:44:16). (The PT_LOAD W^X advisory on the userspace server ELFs is pre-existing and
    non-fatal by design.)

19. **Validation — hosted suite.** `cargo test --features hosted-dev --lib -- --test-threads=1`:
    2862 passed, 0 failed, 2 ignored. Two source-scan guards that matched literals introduced by the
    Part B/C edits (`FLAG_REPLY_CAP` in a Part-C comment; the pre-Part-B `ANALYSIS_LOG` path) were
    updated to the current, still-correct code — neither is a behavior regression.

20. **Validation — control-plane + ABI.** `cargo check -p yarm-control-plane-servers --bins` OK;
    `cargo test -p yarm-control-plane-servers supervisor` 10 passed; `cargo test -p yarm-ipc-abi
    pm_restart` 3 passed.

21. **Counts unchanged.** `pub const SYSCALL_COUNT: usize = 32;`, `pub const VARIANT_COUNT: usize =
    22;`, NR27 (`InitramfsReadChunk`) absent as a live class.

22. **Go / No-Go for Stage 198C.** The two hard-stop gates are cleared: the artifact-integrity gate
    (Part A) fails closed and rejects stale artifacts, and the crash-restart oracle (Part E) is a
    true, complete baseline (not a timeout workaround). Ordinary-cap negative coverage (Part D) and
    the rights attestation (Part C) are in place; the DebugLog widening (Part F) is proven bounded;
    the functional seals (Part G) are preserved under a serialized/isolated runner (Part B).
    **Recommendation: GO for 198C** once the isolation 3× run and combined-seal preservation lines
    above are confirmed green (see §4/§15). <!-- GONOGO -->
