// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD-CLOSURE1 §B — the plain-lock rule at scheduling points, held mechanically.
//!
//! A plain `SpinLock` does not mask interrupts, so a kernel thread holding one is invisible to the
//! `irqs_enabled` check. Every plain guard is counted against the kernel-thread stack it was taken
//! on, released against that same stack, and every scheduling point refuses while the count is
//! non-zero (or poisoned). These guards pin that structure, the audited set of hand-rolled locks
//! (each new one fails here until it is audited), the absence of any forget primitive outside the
//! audited sites, and the lock path's independence from locks and logging.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const LOCK: &str = include_str!("../src/kernel/lock.rs");
const KTHREAD: &str = include_str!("../src/kernel/kthread.rs");

fn body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("missing `{sig}`"));
    let open = start + src[start..].find('{').expect("body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated `{sig}`")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Files under `src/` and `crates/` with how many lines match `pred`.
fn census(pred: impl Fn(&str) -> bool) -> BTreeMap<String, usize> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("crates"), &mut files);
    let mut out = BTreeMap::new();
    for f in files {
        let text = std::fs::read_to_string(&f).expect("read");
        let n = text.lines().filter(|l| pred(l)).count();
        if n > 0 {
            let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            out.insert(rel, n);
        }
    }
    out
}

#[test]
fn every_plain_guard_is_counted_against_its_stack_and_released_there() {
    let lock = body(LOCK, "pub fn lock(&self) -> SpinLockGuard<'_, T>");
    let try_lock = body(
        LOCK,
        "pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>>",
    );
    for (name, b) in [("lock", lock), ("try_lock", try_lock)] {
        assert!(
            b.contains("on_kthread_stack: crate::kernel::kthread::plain_guard_taken(),"),
            "{name} counts its guard"
        );
        // A plain lock stays plain: no interrupt masking is added to make the check pass.
        assert!(!b.contains("irq_save"), "{name} does not mask");
    }
    let drop = body(LOCK, "impl<T> Drop for SpinLockGuard<'_, T>");
    let release = drop
        .find("crate::kernel::kthread::plain_guard_released(self.on_kthread_stack);")
        .expect("the release names the counter the acquisition returned");
    let store = drop.find("self.lock.held.0.store(false").expect("unlock");
    assert!(release < store, "counted down while still held");
    assert_eq!(
        LOCK.matches("SpinLockGuard {").count(),
        2,
        "two constructions"
    );
}

#[test]
fn every_scheduling_point_refuses_a_live_or_poisoned_plain_count() {
    let check = body(KTHREAD, "pub fn switch_check(&self, op: &'static str)");
    assert!(check.contains("self.pool.plain_guards_held(self.id) == PLAIN_POISON"));
    assert!(check.contains("Some(KSwitchRefusal::PlainLockAccounting)"));
    assert!(check.contains("self.pool.plain_guards_held(self.id) != 0"));
    assert!(check.contains("Some(KSwitchRefusal::PlainLockHeld)"));
    // Interrupt masking, the guard count and residency stay separate checks, masking first.
    let irq = check.find("!arch::irqs_enabled()").unwrap();
    let plain = check.find("plain_guards_held").unwrap();
    let resident = check.find(".running").unwrap();
    assert!(irq < plain && plain < resident);
    for sig in [
        "pub fn yield_now(&self)",
        "pub fn sched_point(&self)",
        "fn park(&self)",
        "pub fn exit(&self) -> !",
    ] {
        let b = body(KTHREAD, sig);
        let first_line = b
            .lines()
            .skip(1)
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("//"))
            .unwrap();
        assert!(
            first_line.contains("self.switch_check("),
            "{sig} checks before anything else: {first_line}"
        );
    }
}

#[test]
fn the_lock_path_takes_no_lock_and_does_not_log() {
    for sig in [
        "pub(crate) fn plain_guard_taken()",
        "pub(crate) fn plain_guard_released(",
        "fn plain_counter_at(sp: usize)",
        "fn plain_poison(c: &AtomicU32)",
    ] {
        let b = body(KTHREAD, sig);
        for banned in [
            ".lock()",
            "yarm_log!",
            "pr_info!",
            "println!",
            "panic!",
            "with_cpu",
        ] {
            assert!(!b.contains(banned), "{sig} must not use `{banned}`");
        }
    }
    // Registration happens under the pool lock before any thread of the pool can run.
    let create = body(KTHREAD, "pub fn create(");
    let reg = create.find("plain_register(").unwrap();
    let pop = create.find("free.pop()").unwrap();
    let enqueue = create.find("enqueue_with_priority").unwrap();
    assert!(reg < pop && pop < enqueue);
    assert!(create.contains("KTHREAD_PLAIN_LOCK_INVARIANT"));
}

#[test]
fn logging_takes_no_plain_lock() {
    // A refusal is reported by the scheduling point through the log, so the log path itself must
    // never wait on a plain lock a refused thread could be holding.
    for (name, src) in [
        ("printk", include_str!("../src/kernel/printk.rs")),
        (
            "aarch64 console",
            include_str!("../src/arch/aarch64/console.rs"),
        ),
        (
            "riscv64 console",
            include_str!("../src/arch/riscv64/console.rs"),
        ),
        (
            "x86_64 console",
            include_str!("../src/arch/x86_64/console.rs"),
        ),
    ] {
        let plain = src
            .lines()
            .filter(|l| {
                let l = l.replace("SpinLockIrq", "");
                l.contains("SpinLock<") || l.contains("SpinLock::new")
            })
            .count();
        assert_eq!(plain, 0, "{name} uses no plain SpinLock");
    }
}

#[test]
fn every_hand_rolled_lock_is_in_the_audited_set() {
    // Each site is audited in doc/KERNEL_UNLOCKING.md (KTHREAD-CLOSURE1 Stage B): the plain and
    // masking lock types themselves, interrupt-masked console exclusion, boot-only locks, one-shot
    // claims and flags, a syscall-only staging claim, the userspace allocator, and test code.
    let found = census(|l| {
        l.contains("compare_exchange(false, true")
            || l.contains("compare_exchange_weak(false, true")
    });
    let audited: BTreeMap<String, usize> = [
        ("crates/yarm-freestanding-alloc/src/lib.rs", 2),
        ("src/arch/aarch64/boot.rs", 1),
        ("src/arch/boot_entry.rs", 2),
        ("src/arch/riscv64/console.rs", 1),
        ("src/arch/x86_64/descriptor_tables.rs", 2),
        ("src/kernel/boot/mod.rs", 2),
        ("src/kernel/boot/orchestrator_state.rs", 1),
        ("src/kernel/boot/tests.rs", 2),
        ("src/kernel/lock.rs", 3),
        ("src/kernel/syscall/process.rs", 1),
    ]
    .into_iter()
    .map(|(f, n)| (f.to_string(), n))
    .collect();
    assert_eq!(
        found, audited,
        "a new hand-rolled lock must be audited first"
    );
}

#[test]
fn no_guard_escapes_through_a_forget_primitive_outside_the_audited_sites() {
    let found = census(|l| l.contains("mem::forget(") || l.contains("ManuallyDrop"));
    let files: Vec<&str> = found.keys().map(String::as_str).collect();
    assert_eq!(
        files,
        [
            // U9-EXIT3's documentation of why it has none.
            "src/kernel/boot/cleared_current.rs",
            // Its guard test, which proves the absence.
            "src/kernel/boot/tests.rs",
            // The deliberate leak a handler can cause, shown to keep its thread ineligible.
            "src/kernel/kthread_tests.rs",
        ]
    );
}
