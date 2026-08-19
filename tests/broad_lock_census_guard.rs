// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Broad-lock census guard — keeps canonical Stage 204A from silently going stale.
//!
//! Stage 204A ("broad-lock callsite census") is the one canonical stage recorded COMPLETE.
//! Its deliverable is an authoritative, classified list of every runtime acquisition of the
//! broad `SpinLock<KernelState>`, in `doc/KERNEL_UNLOCK_AUDIT.md` §1. A census is only
//! complete while it matches the tree, so the moment someone adds or deletes a production
//! `SharedKernel::with` / `with_cpu` callsite without re-classifying it, 204A quietly stops
//! being true and every downstream count (§1.2, §1.4a, `doc/KERNEL_LOCKING.md` §0.2,
//! `doc/STATUS.md` §0) becomes wrong.
//!
//! This guard recomputes the census from source with the same method the audit used and
//! fails if it drifts from the pinned expectation.
//!
//! **Adding a broad-lock callsite is not forbidden** — the retirement work legitimately
//! moves them around. What is forbidden is doing so without updating the census. When this
//! test fails, update [`EXPECTED_WITH_CPU`] / [`EXPECTED_WITH_BROAD`] /
//! [`EXPECTED_STATE_LOCK`] here **and** the classification and totals in the documents
//! named above, in the same change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-file count of production `SharedKernel::with_cpu` callsites.
const EXPECTED_WITH_CPU: &[(&str, usize)] = &[
    // U3 (203C): 8 -> 7 -> 6 -> 3. Two read-only post-lock drains moved to the rank-1
    // scheduler seam (`current_tid_split_read`), then the three homologous switch/restore
    // drains (queue-switch foundation, FutexWait switch-success, Yield switch-success) moved
    // to the exact-token rank-2 transaction: `direct_dispatch_activate_asid_split` (which
    // performs the real RISC-V map/root/`write_satp`+`sfence.vma` on this target) plus
    // `direct_dispatch_restore_context_split` and `direct_dispatch_take_completion_split`.
    // U3 (203C): 3 -> 2. The post-lock `CurrentTaskExited` validation snapshot moved to
    // `post_lock_exit_validation_split`, one coherent rank-1 (scheduler) transaction with the
    // rank-2 (task) acquisition nested inside it. The two remaining acquisitions are the
    // canonical broad trap phase and the deferred terminal-idle predicate.
    // U3 (203C): 2 -> 1. The post-lock blocked-syscall TERMINAL-IDLE predicate moved to
    // `terminal_idle_on_cpu_split`, ONE coherent rank-1 scheduler snapshot: validate + bind
    // `current_cpu` (as `with_cpu` did), then `current_tid_on(cpu)` and `runnable_count_on(cpu)`
    // under that single guard, so the predicate can no longer tear between two separately
    // locked reads. **This file is now fully drained of reacquisitions**: its ONE remaining
    // production `with_cpu` is the canonical broad Phase-2 trap handler itself.
    ("src/arch/riscv64/trap.rs", 1),
    // Stage 199D: 12 -> 11. The AArch64 handled-split return path no longer reacquires the
    // broad lock to finalize a syscall; it uses two bounded rank-2 task-domain transactions
    // (exact-incarnation TLS take, exact-incarnation context commit) instead.
    // U3 (203C): 11 -> 9. The AArch64 FutexWait and Yield switch-success restores now run the
    // neutral exact-token resume core (`direct_dispatch_resume_incoming_core`). The FutexWait
    // no-incoming idle check was briefly retired too and then RESTORED: its only live gate (the
    // Stage 195F idle oracle) is unreachable behind the pre-existing SpawnV5 stall, so that one
    // substitution could not be live-proven and was not shipped.
    // U3 (203C): 9 -> 7. The two homologous x86_64 D2 switch-success restores (blocking send
    // and blocking receive) were byte-identical broad re-acquires; both now run one neutral
    // exact-token transaction, `x86_post_lock_resume_marked_incoming`.
    // U3 (203C): 7 -> 5. The x86_64 FutexWait and Yield switch-success restores were the same
    // body again; both now reuse that transaction unchanged. The five that remain are the
    // canonical broad trap phase, the AArch64 FutexWait no-incoming idle read, the D6
    // controlled-proof restore, and the two AArch64 ExitCurrentTask acquisitions.
    // U3 (203C): 5 -> 4. The AArch64 `CurrentTaskExited` VALIDATION reacquisition became
    // `SharedKernel::post_lock_exit_validation_split`, the same rank-1 -> rank-2 snapshot the
    // RISC-V exit consumer already used.
    // U3 (203C): 4 -> 3. The AArch64 `CurrentTaskExited` REPLACEMENT RESTORE reacquisition became
    // `SharedKernel::post_exit_replacement_restore_split` (one coherent rank-1 -> rank-2
    // transaction taking the replacement's ASID, saved context, TLS request and parked
    // blocked-syscall completion) plus `aarch64::trap::post_exit_restore_replacement`, which does
    // the TTBR0/frame work with every domain lock released through the SAME single frame writer
    // the in-lock restore uses. The three that remain are the canonical broad trap phase, the
    // AArch64 FutexWait no-incoming idle read, and the D6 controlled-proof restore — whose
    // acquisition is DELIBERATELY retained: its body also runs the D6 proof cleanup
    // (`d6_ensure_post_cleanup_task_stacks_mapped` maps kernel stack pages into the active root
    // and every live task root), which is D3-fenced by AI_AGENT_RULES §14.4 and has no split form,
    // so splitting it would leave a broad drain behind at census delta 0.
    ("src/arch/trap_entry.rs", 3),
    // U3 (203C): 4 -> 3. The AP saved-resume placement's `with_cpu(cpu, |k| { enqueue; dispatch })`
    // became one authoritative rank-1 -> rank-2 transaction,
    // `SharedKernel::enqueue_then_dispatch_on_cpu_split`: rank 1 acquired once, CPU validated with
    // the same predicate `set_current_cpu` uses, `current_cpu` bound, then rank 2 nested (ascending)
    // to resolve the whole enqueue policy — reservation refusal, existence, class -> priority — in
    // ONE acquisition instead of the two `with_tcbs` reads `enqueue_on_cpu` takes before touching
    // the queue. The homologous next-task placement in `ap_sched_next_or_idle` is NOT converted: no
    // existing workload reaches its success body, so it keeps its acquisition byte-for-byte. The
    // three that remain are that one, the AP return-to-idle `block_current_on_cpu`, and the BSP
    // `on_preempt_prefer_on_cpu`.
    // U3 (203C): 3 -> 2. The AP return-to-idle `with_cpu(cpu, |k| k.block_current_on_cpu(cpu))`
    // became `SharedKernel::block_current_on_cpu_split`, one rank-1 scheduler transaction: the
    // whole acquisition existed to do a single scheduler-domain thing — take this CPU's `current`
    // and drop its membership entry — so rank 1 is acquired once, the CPU is validated with the
    // same predicate `set_current_cpu` uses, `current_cpu` is bound, and the existing
    // `block_current_on` primitive runs inside that guard. Rank 2 is never taken and no task
    // status changes. The two that remain are the BSP `on_preempt_prefer_on_cpu` and the
    // unreached ED-2 next-task placement, neither of which is live-reached.
    ("src/arch/x86_64/smp.rs", 2),
    // U3 (203C): 1 -> 0. `src/kernel/boot/thread_state.rs` is now FULLY DRAINED. The x86_64 D6
    // first-resume trampoline's `with_cpu(ctx.cpu_id, |kernel| …)` existed to call
    // `post_switch_restore_arch_thread_state(kernel, cpu, None)` — and with `frame == None` the
    // x86_64 `restore_arch_thread_state` returns `Ok(())` on its first statement, before any
    // current-TID read, TCB access, context/TLS restore, ASID activation, CR3 check or domain
    // lock. The acquisition's only KernelState effect was therefore the CPU validate-and-bind
    // `with_cpu` performs on entry, which is exactly what `SharedKernel::bind_current_cpu_split`
    // now does under one rank-1 acquisition. The REAL D6 restore/cleanup acquisition in
    // `trap_entry.rs` passes a live frame and is untouched.
    // U1: 13 -> 12. The obsolete `SharedKernel::handle_trap_with_cpu` wrapper had no
    // in-tree caller at all and was deleted.
    // U3 (203C): 12 -> 11. `current_tid_authoritative` became the authoritative rank-1
    // scheduler transaction. It still VALIDATES the CPU and BINDS `current_cpu` before
    // reading — only the broad lock went away — which is what distinguishes it from the
    // reverted Stage 4T+6 substitution onto the non-binding `current_tid_split_read`.
    // U3 (203C): 11 -> 8. The three homologous blocked-waiter Phase-C completions
    // (`execute_dispatch_post_work`, `execute_blocked_waiter_reply_cap_delivery`,
    // `execute_blocked_waiter_ordinary_cap_delivery`) each re-entered the broad lock with a
    // byte-identical body: clear the waiter's return registers, clear the endpoint waiter, wake.
    // All three now call ONE shared, class-neutral, rank-ordered transaction,
    // `complete_blocked_waiter_delivery_split` — rank 1 (validate + bind), rank 2 (return-register
    // clear, wake-TID ASID read), rank 3 (identity-keyed `clear_endpoint_waiter_if_identity`),
    // then the rank-2/rank-1 wake — so no seam is entered while a rank >= its own is held.
    // U3 (203C): 8 -> 7. `revalidate_idle_owner_after_drains` became a CPU-local rank-1/rank-2
    // transaction: rank 1 (`validate_online_cpu` + bind `current_cpu` + the same single
    // `dispatch_next_on`) is fully released before the rank-2 snapshot (user context, the
    // pending TLS-restore request, and the task's OPTIONAL asid in ONE acquisition, no
    // `TaskStatus` write), which is fully released before the frame / FS-base / CR3 work.
    // Rollback re-takes rank 1 alone. The proven-infallible legacy restore is why the
    // `restorable` requeue arm stays unreachable there, exactly as it was under the broad lock.
    // U3 (203C): 7 -> 5. The two homologous recv copy-fault completions in
    // `complete_recv_boundary_user_copy` — the plain `CopyFault` arm and the recv-v2
    // `PayloadCopyFault` arm — each re-entered the broad lock with the byte-identical body
    // `recv_boundary_record_user_fault(k, frame, user_ptr)`. Both now call ONE class-neutral
    // transaction, `record_recv_boundary_user_fault_split`: rank 1 (the existing
    // `bind_current_cpu_split`, promoted to architecture-neutral) then rank 8 (the existing
    // `record_fault_split_mut`), then the frame error with no lock held. No new seam. The
    // capability-rollback re-entry in the same method is dependency-blocked and untouched.
    // U3 (203C): 5 -> 4. The ordinary-cap DEFERRED SENDER WAKE in
    // `complete_recv_boundary_ordinary_cap` re-entered the broad lock to run a two-line
    // composition: the `IPC_RECV_SPLIT_REFILL_WAKE_APPLY` marker, then
    // `apply_scheduler_wake_plan(Wake(tid))` — which is `wake_tid_to_runnable`. It now calls
    // `apply_split_sender_wake_plan_split`: rank 1 (`bind_current_cpu_split`, the same
    // validate-and-bind `with_cpu` performed on entry) released, the marker, then the SHARED
    // `wake_tid_to_runnable_split` (rank 2 then rank 1). No wake logic is duplicated — that
    // body is the one the blocked-waiter completion already used, renamed from
    // `wake_blocked_waiter_split` now that it has a second production caller.
    ("src/runtime.rs", 4),
];

/// Per-file count of production broad `SharedKernel::with(|state| …)` callsites.
///
/// `src/kernel/boot/orchestrator_state.rs` is listed because it matches the same textual
/// pattern, but its single hit is `LOCK_ORDER_LAST_RANK.with(|last| …)` — a `thread_local!`
/// accessor, **not** a broad-lock acquisition. It is counted here so the guard stays purely
/// mechanical, and subtracted by [`THREAD_LOCAL_FALSE_POSITIVES`] to reach the audited
/// figure.
const EXPECTED_WITH_BROAD: &[(&str, usize)] = &[
    // U3 (203C): 2 -> 1. The AP saved-frame resume's broad read became one authoritative rank-2
    // snapshot transaction (`SharedKernel::ap_saved_resume_context_split`): a single task-domain
    // acquisition copies ASID + status + full `UserRegisterContext` + TLS by value, and the
    // ASID -> CR3 resolution runs only after that guard is released. The BSP counterpart is
    // deliberately NOT converted: its path is not live-reached at this base (the cross-CPU reply
    // oracle never emits `X86_BSP_SAVED_DISPATCH_OK`), and an unreached site is never retired
    // merely because it is homologous to a reached one.
    ("src/arch/x86_64/smp.rs", 1),
    ("src/kernel/boot/orchestrator_state.rs", 1),
    // U1: 8 -> 7. The obsolete `SharedKernel::run_reply_timeout_completion` wrapper had no
    // production caller and was deleted; the single completion body
    // (`KernelState::run_reply_timeout_completion_locked`) is unchanged.
    // U2: 7 -> 4. The two test-only helpers were relocated into test-only modules:
    // `ipc_recv_with_deadline_split_bridge` (2 acquisitions) and the
    // `SharedKernel::control_plane_set_process_cnode_slots_via_syscall` wrapper (1).
    // U3 (203C): 4 -> 0. The last four broad acquisitions in this file moved onto existing
    // rank-domain seams: the two home-CPU wrappers onto the rank-2 task seam, and the reply-win
    // deadline disarm onto rank 2 (handle read) followed by rank 3 (exact token disarm), taken
    // sequentially and never nested. `src/runtime.rs` now has NO production broad acquisition.
];

/// Per-file count of raw `self.state.lock()` sites. All three are the bodies of
/// `SharedKernel::lock` / `with` / `with_cpu` themselves.
const EXPECTED_STATE_LOCK: &[(&str, usize)] = &[("src/runtime.rs", 3)];

/// Textual matches in [`EXPECTED_WITH_BROAD`] that are not broad-lock acquisitions.
const THREAD_LOCAL_FALSE_POSITIVES: usize = 1;

/// The audited totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.2.
///
/// The acquisition total is `with_cpu + with_broad` only. The three raw `self.state.lock()`
/// sites are the **bodies** of `SharedKernel::lock` / `with` / `with_cpu` — the
/// implementations that every callsite goes through, not callsites themselves. Adding them
/// would double-count the lock.
const AUDITED_WITH_CPU_TOTAL: usize = 10; // U3: 38 -> 33 -> 31 -> 30 -> 28 -> 26 -> 23 -> 22 -> 21 -> 20 -> 17 -> 16 -> 14 -> 13 -> 12 -> 11 -> 10 (the AArch64 CurrentTaskExited validation, then its replacement restore)
const AUDITED_WITH_BROAD_TOTAL: usize = 1; // U3: 6 -> 2 -> 1 (runtime.rs fully drained; the reached x86 AP SMP read retired)
const AUDITED_STATE_LOCK_TOTAL: usize = 3;
const AUDITED_ACQUISITION_TOTAL: usize = AUDITED_WITH_CPU_TOTAL + AUDITED_WITH_BROAD_TOTAL;

/// Stage 204A classification totals, as published in `doc/KERNEL_UNLOCK_AUDIT.md` §1.4a.
const CLASS_BOOT_ONLY: usize = 0;
const CLASS_TEST_ONLY: usize = 0; // U2: 3 -> 0 (test-only helpers left the production census)
const CLASS_OBSOLETE: usize = 0; // U1: 2 -> 0 (both obsolete acquisitions deleted)
const CLASS_RUNTIME_REQUIRED: usize = 11; // U3: 44 -> 39 -> 37 -> 36 -> 34 -> 32 -> 28 -> 25 -> 24 -> 23 -> 22 -> 21 -> 18 -> 17 -> 15 -> 14 -> 13 -> 12 -> 11 (thirty-three retired onto their seams)

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every production `.rs` file: everything under `src/` and `crates/`, minus whole-file
/// test modules (`tests.rs`).
fn production_rs_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                out.push(path);
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("src"), &mut out);
    walk(&root.join("crates"), &mut out);
    out.sort();
    out
}

/// Index of the `mod tests {` line that follows a `#[cfg(test)]` attribute, i.e. where
/// production code ends in a file that carries an inline test module.
fn test_module_cutoff(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if (trimmed.starts_with("mod tests") || trimmed.starts_with("pub mod tests"))
            && trimmed.ends_with('{')
            && i > 0
            && lines[i - 1].contains("#[cfg(test)]")
        {
            return i;
        }
    }
    lines.len()
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with('*') || t.starts_with("/*")
}

/// Count production occurrences of `needle_pred` per file, relative to the repo root.
fn census(needle_pred: fn(&str) -> bool) -> BTreeMap<String, usize> {
    let root = repo_root();
    let mut counts = BTreeMap::new();
    for path in production_rs_files() {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let cutoff = test_module_cutoff(&lines);
        let mut n = 0usize;
        for line in lines.iter().take(cutoff) {
            if !is_comment(line) && needle_pred(line) {
                n += 1;
            }
        }
        if n > 0 {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            counts.insert(rel, n);
        }
    }
    counts
}

fn with_cpu_line(line: &str) -> bool {
    line.contains(".with_cpu(")
}

/// `.with(` immediately followed by a closure — the broad `&mut KernelState` form.
fn with_broad_line(line: &str) -> bool {
    let Some(idx) = line.find(".with(") else {
        return false;
    };
    line[idx + ".with(".len()..].trim_start().starts_with('|')
}

fn state_lock_line(line: &str) -> bool {
    line.contains(".state.lock()")
}

fn expected_map(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs.iter().map(|(f, n)| ((*f).to_string(), *n)).collect()
}

fn diff_report(
    kind: &str,
    actual: &BTreeMap<String, usize>,
    expected: &BTreeMap<String, usize>,
) -> String {
    let mut out = format!("\n{kind} census drift:\n");
    for (file, n) in actual {
        match expected.get(file) {
            Some(e) if e == n => {}
            Some(e) => out.push_str(&format!("  CHANGED {file}: expected {e}, found {n}\n")),
            None => out.push_str(&format!(
                "  NEW     {file}: found {n} (not in the census)\n"
            )),
        }
    }
    for (file, e) in expected {
        if !actual.contains_key(file) {
            out.push_str(&format!("  GONE    {file}: expected {e}, found 0\n"));
        }
    }
    out.push_str(
        "\nA broad-lock callsite was added or removed without updating the Stage 204A census.\n\
         Adding one is allowed; leaving the census stale is not — Stage 204A is recorded\n\
         COMPLETE and stops being true the moment this drifts.\n\n\
         Update, in the same change:\n\
           * the pinned tables in tests/broad_lock_census_guard.rs\n\
           * doc/KERNEL_UNLOCK_AUDIT.md §1.2, §1.3, §1.4 and the §1.4a classification\n\
           * doc/KERNEL_LOCKING.md §0.2\n\
           * doc/STATUS.md §0 (broad-lock position table)\n",
    );
    out
}

#[test]
fn with_cpu_callsites_match_the_census() {
    let actual = census(with_cpu_line);
    let expected = expected_map(EXPECTED_WITH_CPU);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("SharedKernel::with_cpu", &actual, &expected)
    );
}

#[test]
fn broad_with_callsites_match_the_census() {
    let actual = census(with_broad_line);
    let expected = expected_map(EXPECTED_WITH_BROAD);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("broad SharedKernel::with", &actual, &expected)
    );
}

#[test]
fn raw_state_lock_sites_match_the_census() {
    let actual = census(state_lock_line);
    let expected = expected_map(EXPECTED_STATE_LOCK);
    assert_eq!(
        actual,
        expected,
        "{}",
        diff_report("raw state.lock()", &actual, &expected)
    );
}

#[test]
fn census_totals_are_internally_consistent() {
    let with_cpu: usize = census(with_cpu_line).values().sum();
    let with_broad_raw: usize = census(with_broad_line).values().sum();
    let state_lock: usize = census(state_lock_line).values().sum();
    let with_broad = with_broad_raw - THREAD_LOCAL_FALSE_POSITIVES;

    assert_eq!(with_cpu, AUDITED_WITH_CPU_TOTAL, "with_cpu total drifted");
    assert_eq!(
        with_broad, AUDITED_WITH_BROAD_TOTAL,
        "broad `with` total drifted (after subtracting {THREAD_LOCAL_FALSE_POSITIVES} \
         thread_local false positive)"
    );
    assert_eq!(
        state_lock, AUDITED_STATE_LOCK_TOTAL,
        "raw state.lock() total drifted — these are the three `SharedKernel` method bodies; \
         a fourth means the broad lock grew a new entry point"
    );
    assert_eq!(
        with_cpu + with_broad,
        AUDITED_ACQUISITION_TOTAL,
        "total broad-lock acquisition sites drifted (state.lock() bodies are deliberately \
         excluded — counting them would double-count the lock)"
    );
}

#[test]
fn stage_204a_classification_covers_every_runtime_required_site() {
    assert_eq!(
        CLASS_BOOT_ONLY + CLASS_TEST_ONLY + CLASS_OBSOLETE + CLASS_RUNTIME_REQUIRED,
        AUDITED_ACQUISITION_TOTAL,
        "the Stage 204A classification must account for every acquisition site; \
         boot-only + test-only + obsolete + runtime-required must equal the census total"
    );
}

#[test]
fn documents_publish_the_same_census_totals() {
    let doc = |name: &str| {
        fs::read_to_string(repo_root().join("doc").join(name))
            .unwrap_or_else(|_| panic!("doc/{name} must exist"))
    };
    let audit = doc("KERNEL_UNLOCK_AUDIT.md");
    let locking = doc("KERNEL_LOCKING.md");
    let status = doc("STATUS.md");

    let total = AUDITED_ACQUISITION_TOTAL.to_string();
    for (name, text) in [
        ("KERNEL_UNLOCK_AUDIT.md", &audit),
        ("KERNEL_LOCKING.md", &locking),
        ("STATUS.md", &status),
    ] {
        assert!(
            text.contains(&format!("**{total}**")),
            "doc/{name} must publish the broad-lock acquisition total ({total}); \
             the census and the documents must not drift apart"
        );
    }

    // The classification counts are published only by the audit and the locking doc.
    for (name, text) in [
        ("KERNEL_UNLOCK_AUDIT.md", &audit),
        ("KERNEL_LOCKING.md", &locking),
    ] {
        assert!(
            text.contains(&format!("**{CLASS_RUNTIME_REQUIRED}**")),
            "doc/{name} must publish the runtime-required count ({CLASS_RUNTIME_REQUIRED})"
        );
    }
}

/// Stage 199D (AArch64 blocker 3): the post-lock direct dispatch drain must not have added a
/// broad-lock acquisition site. The requirement is that the census stays at 50 or DECREASES —
/// a drain that reached for `with_cpu` to do its ASID activation or frame restore, as the
/// FutexWait and Yield drains legitimately do, would push it to 51 and fail here.
#[test]
fn stage199d_blocker3_added_no_broad_lock_acquisition_site() {
    assert!(
        AUDITED_ACQUISITION_TOTAL <= 50,
        "the broad-lock census must stay at 50 or decrease; it is {AUDITED_ACQUISITION_TOTAL}"
    );
    // The drain and the arch resume primitive it calls contain no acquisition of their own.
    let trap_entry = std::fs::read_to_string("src/arch/trap_entry.rs").expect("trap_entry.rs");
    let drain = trap_entry
        .split("if crate::kernel::direct_dispatch::is_pending(cpu_idx) {")
        .nth(1)
        .and_then(|s| s.split("\n    // Stage 192A").next())
        .expect("the direct dispatch drain");
    assert!(
        !drain.contains("with_cpu(") && !drain.contains("shared.with("),
        "the post-lock direct dispatch drain must acquire no broad lock"
    );
}
