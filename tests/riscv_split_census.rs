// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-SMP2-ACCEPTANCE §3 — behavioural controls for the RISC-V core smoke's retired-set census.
//!
//! The census omitted NR 0 (`Yield`), a default-on retired class since Stage 196G, so any strict
//! core boot of a `riscv64-ipc-reply-timeout-oracle` kernel in which init's oracle lanes actually
//! yielded failed with "serviced a syscall outside the retired set". The correction widens the sum by exactly that one term and pins it:
//! every NR 0 line must be the committed queue-advance disposition with its continuation captured,
//! paired one-to-one on its CPU with the `YIELD_SPLIT_COMMITTED` of the same tid.
//!
//! These tests are behavioural: they extract `riscv64_split_census` from the smoke script and run
//! THAT code with `bash` over fixture logs — the same function the smoke runs on a boot's log.

use std::process::Command;

const SMOKE: &str = include_str!("../scripts/qemu-riscv64-core-smoke.sh");

fn census_fn() -> &'static str {
    let start = SMOKE
        .find("riscv64_split_census() {")
        .expect("the smoke defines riscv64_split_census");
    let end = SMOKE[start..]
        .find("\n# END riscv64_split_census")
        .expect("the census function is closed by its END marker");
    &SMOKE[start..start + end]
}

/// Run the census over `log` under the smoke's own shell options (`set -euo pipefail`, which a
/// command in the census must survive on every log, including one with no matches at all);
/// returns (failures, output).
fn census(log: &str) -> (u32, String) {
    let dir = std::env::temp_dir().join(format!(
        "yarm-riscv-census-{}-{}",
        std::process::id(),
        log.len() ^ log.lines().count() << 16 ^ fnv(log)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("boot.log");
    std::fs::write(&path, log).expect("fixture");
    let script = format!(
        "set -euo pipefail\n{}\nfailures=0\nTERMINAL_FAULT_ORACLE=0\nriscv64_split_census \"$1\"\necho \"FAILURES=$failures\"\n",
        census_fn()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .arg("census")
        .arg(&path)
        .output()
        .expect("bash and rg must be available — the smoke itself depends on them");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let n = text
        .lines()
        .find_map(|l| l.strip_prefix("FAILURES="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("census did not report: {text}"));
    (n, text)
}

fn fnv(s: &str) -> usize {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    }) as usize
}

const D: &str = "YARM_LOCK_SPLIT_DISPATCH arch=riscv64";

/// Every retired class of a healthy default boot, in its pinned disposition.
fn healthy() -> String {
    let mut v = vec![
        format!("{D} nr=15 cpu=0 result=ok"),
        format!("{D} nr=10 cpu=0 result=ok"),
        format!("{D} nr=9 cpu=0 result=queue_advance_committed outgoing=3 captured=1"),
        format!("{D} nr=5 cpu=0 result=queue_advance_committed outgoing=3 captured=1"),
        format!("{D} nr=2 cpu=0 result=queue_advance_committed outgoing=4 captured=1"),
        format!("{D} nr=1 cpu=0 result=post_work_committed finalized=1"),
    ];
    v.extend((0..3).map(|_| format!("{D} nr=23 cpu=0 result=ok")));
    v.extend((0..5).map(|_| format!("{D} nr=29 cpu=0 result=ok")));
    v.join("\n") + "\n"
}

/// One oracle-lane yield (timeout-wins: the client waiting for the server's rejected late NR 7),
/// exactly as the bridge prints it.
fn yield_of(tid: u64) -> String {
    format!(
        "USER_LOG tid={tid} msg=IPC_REPLY_TIMEOUT_ORACLE_CLIENT_TIMED_OUT\n\
         RISCV_YIELD_DISPATCH_DEFER_BEGIN cpu=0 outgoing={tid}\n\
         YIELD_SPLIT_COMMITTED cpu=0 tid={tid}\n\
         {D} nr=0 cpu=0 result=queue_advance_committed outgoing={tid} captured=1\n\
         QUEUE_ADVANCE_BROAD_DISPATCH_SKIPPED cpu=0 reason=publication_committed\n"
    )
}

#[test]
fn legitimate_nr0_traffic_passes() {
    let (n, out) = census(&(healthy() + &yield_of(1)));
    assert_eq!(n, 0, "{out}");
    assert!(
        out.contains("RISC-V NR 0 (Yield) split dispatches: 1,"),
        "{out}"
    );
    // Two yields in one boot (measured live) are two paired dispatches.
    let (n, out) = census(&(healthy() + &yield_of(1) + &yield_of(1)));
    assert_eq!(n, 0, "{out}");
    // A boot that never yields is still a healthy census (a regression smoke, not a witness).
    let (n, out) = census(&healthy());
    assert_eq!(n, 0, "{out}");
    assert!(out.contains("split dispatches: 0,"), "{out}");
}

#[test]
fn an_unexpected_nr_still_fails() {
    let (n, out) = census(&(healthy() + &yield_of(1) + &format!("{D} nr=3 cpu=0 result=ok\n")));
    assert_eq!(n, 1, "{out}");
    assert!(
        out.contains("serviced a syscall outside the retired set"),
        "{out}"
    );
    assert!(out.contains("nr0=1"), "{out}");
}

#[test]
fn missing_or_inconsistent_nr0_accounting_still_fails() {
    let wrong = |mutate: &dyn Fn(&str) -> String| {
        let (n, out) = census(&(healthy() + &mutate(&yield_of(1))));
        assert!(n >= 1, "accepted:\n{out}");
        assert!(
            out.contains("RISC-V Yield split dispatch is not reconciled"),
            "{out}"
        );
    };
    // A dispatch with no committed yield behind it.
    wrong(&|y| y.replace("YIELD_SPLIT_COMMITTED cpu=0 tid=1\n", ""));
    // A committed yield whose dispatch is missing from the accounting.
    wrong(&|y| {
        y.lines()
            .filter(|l| !l.contains(" nr=0 "))
            .map(|l| format!("{l}\n"))
            .collect()
    });
    // An excess dispatch: two NR 0 lines for one commit.
    wrong(&|y| format!("{y}{D} nr=0 cpu=0 result=queue_advance_committed outgoing=1 captured=1\n"));
    // Another disposition, a lost continuation, another task's dispatch, another CPU's commit.
    wrong(&|y| {
        y.replace(
            "result=queue_advance_committed outgoing=1",
            "result=ok outgoing=1",
        )
    });
    wrong(&|y| y.replace("outgoing=1 captured=1", "outgoing=1 captured=0"));
    wrong(&|y| {
        y.replace(
            "nr=0 cpu=0 result=queue_advance_committed outgoing=1",
            "nr=0 cpu=0 result=queue_advance_committed outgoing=2",
        )
    });
    wrong(&|y| {
        y.replace(
            "YIELD_SPLIT_COMMITTED cpu=0 tid=1",
            "YIELD_SPLIT_COMMITTED cpu=1 tid=1",
        )
    });
}

#[test]
fn the_sum_is_widened_by_exactly_nr0() {
    let f = census_fn();
    // RISCV-STRICT1 adds NR 12 and NR 16, which are 0 in every image but the fd witness (below).
    assert!(f.contains(
        "riscv_split_nr2 + riscv_split_nr1 + riscv_split_nr23 + riscv_split_nr29 + riscv_split_nr0 + riscv_split_nr12 + riscv_split_nr16 )); then"
    ));
    assert_eq!(f.matches("if (( riscv_split_total != ").count(), 1);
    // The smoke runs the census once, on its own boot log.
    assert_eq!(
        SMOKE.matches("\nriscv64_split_census \"$LOGFILE\"").count(),
        1
    );
}

/// AP-SCHED4 §B: a log cut mid-record by the boot's timeout. The cut final record is not counted;
/// every complete record still is, so a complete non-committed record before the cut still fails.
#[test]
fn a_record_cut_by_the_timeout_is_not_counted() {
    let cut = format!("{D} nr=5 cpu=0 result=queue_advance_committ");
    let (n, out) = census(&(healthy() + &yield_of(1) + &cut));
    assert_eq!(n, 0, "{out}");
    // The same record, complete, is counted (and passes as the committed disposition).
    let whole = format!("{D} nr=5 cpu=0 result=queue_advance_committed outgoing=2 captured=1\n");
    let (n, out) = census(&(healthy() + &whole));
    assert_eq!(n, 0, "{out}");
    // A complete early-returned NR 5 before the cut still fails, cut or not.
    let bad = format!("{D} nr=5 cpu=0 result=ok\n");
    let (n, out) = census(&(healthy() + &bad + &cut));
    assert_eq!(n, 1, "{out}");
    assert!(out.contains("IpcRecvTimeout split did not commit"), "{out}");
    // A cut line that is not even a complete class name is not an unexpected class either.
    let (n, out) = census(&(healthy() + &format!("{D} nr=")));
    assert_eq!(n, 0, "{out}");
}

// ------------------------------------------------------------------------------------------------
// RISCV-STRICT1 — the census and registration obligation of the image the smoke actually bound.
//
// The workload is derived from the verified ARTIFACT_BINDING line by
// scripts/riscv64-workload-obligations.py; here it is set directly, as the smoke sets it after the
// derivation, and the derivation itself is exercised on binding lines. The fixtures are verbatim
// extracts of the three strict SMP2 RISC-V boots IPC-D2-RULE1 left open (and of its default core
// boot): scripts/fixtures/riscv-workload/, provenance in each header.

const FIX_CORE: &str = include_str!("../scripts/fixtures/riscv-workload/core.txt");
const FIX_FD: &str = include_str!("../scripts/fixtures/riscv-workload/fd-witness.txt");
const FIX_MOUNT: &str = include_str!("../scripts/fixtures/riscv-workload/mount-witness.txt");
const FIX_LIFE: &str = include_str!("../scripts/fixtures/riscv-workload/lifetime-witness.txt");
const HELPER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/scripts/riscv64-workload-obligations.py"
);

fn extract(start: &str, end: &str) -> &'static str {
    let s = SMOKE
        .find(start)
        .unwrap_or_else(|| panic!("the smoke defines {start}"));
    let e = SMOKE[s..]
        .find(end)
        .unwrap_or_else(|| panic!("{start} is closed by {end}"));
    &SMOKE[s..s + e]
}

/// Run `body` (the census or the registration obligation) over `log` as the smoke would for an
/// image of `workload`; returns (failures, output).
fn graded(log: &str, workload: &str, body: &str, call: &str, chain_owed: bool) -> (u32, String) {
    let dir = std::env::temp_dir().join(format!(
        "yarm-riscv-workload-{}-{}-{}",
        std::process::id(),
        workload,
        fnv(log) ^ fnv(call) << 1
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("boot.log");
    std::fs::write(&path, log).expect("fixture");
    let script = format!(
        "set -euo pipefail\n{body}\nfailures=0\nTERMINAL_FAULT_ORACLE=0\nRISCV_WORKLOAD={workload}\n\
         RV_SCRIPT_DIR={dir}\nriscv_service_chain_owed={owed}\n{call} \"$1\"\necho \"FAILURES=$failures\"\n",
        dir = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts"),
        owed = u8::from(chain_owed),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .arg("graded")
        .arg(&path)
        .output()
        .expect("bash, rg and python3 must be available");
    let _ = std::fs::remove_dir_all(&dir);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let n = text
        .lines()
        .find_map(|l| l.strip_prefix("FAILURES="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("no report: {text}"));
    (n, text)
}

fn census_of(log: &str, workload: &str) -> (u32, String) {
    graded(log, workload, census_fn(), "riscv64_split_census", true)
}

fn registration_of(log: &str, workload: &str) -> (u32, String) {
    let f = extract(
        "riscv64_registration_obligation() {",
        "\n# END riscv64_registration_obligation",
    );
    graded(log, workload, f, "riscv64_registration_obligation", true)
}

/// Both of the smoke's workload-dependent checks.
fn smoke_of(log: &str, workload: &str) -> (u32, String) {
    let (a, x) = census_of(log, workload);
    let (b, y) = registration_of(log, workload);
    (a + b, x + &y)
}

fn edit(log: &str, f: impl Fn(&mut Vec<String>)) -> String {
    let mut v: Vec<String> = log.lines().map(str::to_owned).collect();
    f(&mut v);
    v.join("\n") + "\n"
}

fn at(v: &[String], pat: &str) -> usize {
    v.iter()
        .position(|l| l.contains(pat))
        .unwrap_or_else(|| panic!("fixture line {pat}"))
}

const NR16_THREAD: &str = "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 cpu=1 result=queue_advance_committed outgoing=10008";
const NR12: &str = "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=12 cpu=0 result=ok";

#[test]
fn each_image_passes_its_own_obligations() {
    for (log, w) in [
        (FIX_CORE, "core"),
        (FIX_FD, "vfs-fd-witness"),
        (FIX_MOUNT, "vfs-mount-witness"),
    ] {
        let (n, out) = smoke_of(log, w);
        assert_eq!(n, 0, "{w}:\n{out}");
        assert!(
            out.contains(&format!("RISCV-STRICT1: workload={w} ")),
            "{out}"
        );
    }
    let (_, out) = census_of(FIX_FD, "vfs-fd-witness");
    assert!(out.contains("nr29_owed=6 nr12=1 nr16=2"), "{out}");
    assert!(
        out.contains("ramfs_b=10007")
            && out.contains("thread=10008")
            && out.contains("fork_child=10009"),
        "{out}"
    );
    let (_, out) = registration_of(FIX_MOUNT, "vfs-mount-witness");
    assert!(
        out.contains("ram_forwards=8") && out.contains("vfs_local_backend="),
        "{out}"
    );
}

/// The ordinary image's checks are exactly what they were: graded as the ordinary core image, the
/// archived witness boots fail exactly as IPC-D2-RULE1 recorded them.
#[test]
fn the_ordinary_core_checks_are_unchanged_and_still_reject_the_witness_images() {
    let (n, out) = smoke_of(FIX_FD, "core");
    assert_eq!(n, 2, "{out}");
    assert!(
        out.contains("SpawnFromMemoryObject split count is 6, expected 5"),
        "{out}"
    );
    assert!(
        out.contains("serviced a syscall outside the retired set"),
        "{out}"
    );
    let (n, out) = smoke_of(FIX_MOUNT, "core");
    assert_eq!(n, 1, "{out}");
    assert!(
        out.contains("required marker missing: VFS_MOUNT_REGISTER_RAMFS_OK"),
        "{out}"
    );
    // An NR 12 or NR 16 in an ordinary image is still outside the retired set.
    let (n, out) = census_of(&(healthy() + &format!("{NR12}\n")), "core");
    assert_eq!(n, 1, "{out}");
    assert!(out.contains("outside the retired set"), "{out}");
    // The ordinary image still owes the registration marker.
    let (n, out) = registration_of(
        &edit(FIX_CORE, |v| {
            v.retain(|l| !l.contains("VFS_MOUNT_REGISTER_RAMFS_OK"))
        }),
        "core",
    );
    assert_eq!(n, 1, "{out}");
    // The fd witness uses the ordinary registration too, so it still owes the marker.
    let (n, out) = registration_of(
        &edit(FIX_FD, |v| {
            v.retain(|l| !l.contains("VFS_MOUNT_REGISTER_RAMFS_OK"))
        }),
        "vfs-fd-witness",
    );
    assert_eq!(n, 1, "{out}");
}

#[test]
fn a_witness_profile_on_the_wrong_image_fails() {
    for (log, w) in [
        (FIX_CORE, "vfs-fd-witness"),
        (FIX_MOUNT, "vfs-fd-witness"),
        (FIX_CORE, "vfs-mount-witness"),
        (FIX_FD, "vfs-mount-witness"),
    ] {
        let (n, out) = smoke_of(log, w);
        assert!(n >= 1, "{w} accepted another image:\n{out}");
    }
    let (n, out) = census_of(FIX_CORE, "vfs-fd-witness");
    assert!(
        n >= 2 && out.contains("split count is 5, expected 6"),
        "{out}"
    );
    let (n, out) = census_of(FIX_CORE, "no-such-workload");
    assert!(n >= 1 && out.contains("unknown workload"), "{out}");
}

#[test]
fn the_fd_witness_spawn_census_is_exact() {
    // The second ramfs's spawn missing (its NR 29 dispatch and its SPAWN_FROM_MO_OK).
    let missing = edit(FIX_FD, |v| {
        let s = at(v, "SPAWN_FROM_MO_OK image_id=11 spawned_tid=10007");
        v.remove(s);
        let d = v
            .iter()
            .rposition(|l| l.starts_with("YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=29 "))
            .unwrap();
        v.remove(d);
    });
    let (n, out) = census_of(&missing, "vfs-fd-witness");
    assert!(
        n >= 2 && out.contains("split count is 5, expected 6") && out.contains("SPAWN:"),
        "{out}"
    );
    // One spawn too many.
    let extra = FIX_FD.to_owned()
        + "SPAWN_FROM_MO_OK image_id=11 spawned_tid=10011\n\
           YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=29 cpu=0 result=ok\n";
    let (n, out) = census_of(&extra, "vfs-fd-witness");
    assert!(
        n >= 2 && out.contains("split count is 7, expected 6"),
        "{out}"
    );
    // The sixth spawn is not the second ramfs: the right count, another task.
    let other = edit(FIX_FD, |v| {
        let s = at(v, "SPAWN_FROM_MO_OK image_id=11 spawned_tid=10007");
        v[s] = v[s].replace("spawned_tid=10007", "spawned_tid=10012");
    });
    let (n, out) = census_of(&other, "vfs-fd-witness");
    assert_eq!(n, 1, "{out}");
    assert!(
        out.contains("not the ordinary ramfs 10006 and the second ramfs 10007"),
        "{out}"
    );
}

#[test]
fn an_unrelated_dispatch_fails_even_at_the_right_total() {
    // NR 16 replaced by NR 3, same disposition, same total.
    let sub = edit(FIX_FD, |v| {
        let i = at(v, NR16_THREAD);
        v[i] = v[i].replace(" nr=16 ", " nr=3 ");
    });
    let (n, out) = census_of(&sub, "vfs-fd-witness");
    assert!(n >= 2, "{out}");
    assert!(
        out.contains("outside the retired set")
            && out.contains("fd-witness workload is not reconciled"),
        "{out}"
    );
    // NR 12 replaced by another non-switching class, same total.
    let sub = edit(FIX_FD, |v| {
        let i = at(v, NR12);
        v[i] = v[i].replace(" nr=12 ", " nr=13 ");
    });
    let (n, out) = census_of(&sub, "vfs-fd-witness");
    assert!(n >= 2 && out.contains("FORK:"), "{out}");
    // An NR 12 swapped for an NR 16 of the thread: both counts wrong, total right.
    let swap = edit(FIX_FD, |v| {
        let i = at(v, NR12);
        let j = at(v, NR16_THREAD);
        v[i] = v[j].clone();
    });
    let (n, out) = census_of(&swap, "vfs-fd-witness");
    assert!(
        n >= 1 && out.contains("FORK:") && out.contains("EXIT:"),
        "{out}"
    );
}

#[test]
fn a_wrong_task_or_disposition_fails() {
    let cases: [(&str, &str, &str, &str); 7] = [
        (
            "exit of another task",
            NR16_THREAD,
            "outgoing=10008",
            "outgoing=10005",
        ),
        (
            "an exiting frame captured",
            NR16_THREAD,
            "captured=0",
            "captured=1",
        ),
        (
            "exit as an early return",
            NR16_THREAD,
            "result=queue_advance_committed outgoing=10008 captured=0 committed=0 a0=0",
            "result=ok",
        ),
        (
            "fork with a handled error",
            NR12,
            "result=ok",
            "result=handled_err code=3",
        ),
        (
            "fork by another parent",
            "FORK_TXN_COMMITTED parent_tid=1 ",
            "parent_tid=1 ",
            "parent_tid=10002 ",
        ),
        (
            "exit deferred on another CPU",
            "exit_current_task_switch_required tid=10009",
            "cpu=1",
            "cpu=0",
        ),
        (
            "exit with a non-zero code",
            "EXIT_TASK_CLAIM_RETIRED tid=10008",
            "code=0",
            "code=9",
        ),
    ];
    for (why, pat, a, b) in cases {
        let log = edit(FIX_FD, |v| {
            let i = at(v, pat);
            v[i] = v[i].replacen(a, b, 1);
        });
        let (n, out) = census_of(&log, "vfs-fd-witness");
        assert!(n >= 1, "{why} accepted:\n{out}");
        assert!(
            out.contains("fd-witness workload is not reconciled"),
            "{why}:\n{out}"
        );
    }
}

#[test]
fn the_mount_witness_owes_its_publication_capability_and_bytes() {
    let cases: [(&str, Box<dyn Fn(&mut Vec<String>)>); 5] = [
        (
            "no /ram publication",
            Box::new(|v| v.retain(|l| !l.contains("VFS_MOUNT_PUBLISHED prefix=/ram "))),
        ),
        (
            "the sender's number published",
            Box::new(|v| {
                let i = at(v, "VFS_MOUNT_PUBLISHED prefix=/ram ");
                let s = v[i]
                    .split("sender_local_cap=")
                    .nth(1)
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned();
                let l = v[i]
                    .split("vfs_local_cap=")
                    .nth(1)
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned();
                v[i] = v[i].replace(&format!("vfs_local_cap={l}"), &format!("vfs_local_cap={s}"));
            }),
        ),
        (
            "a /ram/ forward on another capability",
            Box::new(|v| {
                let i = at(v, "VFS_FORWARD_CALL op=11 target=/ram/");
                v[i] = v[i].replace("send_cap=", "send_cap=7");
            }),
        ),
        (
            "a short read",
            Box::new(|v| v.retain(|l| !l.contains("msg=VC1B_DATA k=1 "))),
        ),
        (
            "the ordinary registration in a mount image",
            Box::new(|v| {
                v.push("USER_LOG tid=1 msg=VFS_MOUNT_REGISTER_RAMFS_OK prefix=/ram".to_owned());
            }),
        ),
    ];
    for (why, f) in cases {
        let (n, out) = registration_of(&edit(FIX_MOUNT, f), "vfs-mount-witness");
        assert_eq!(n, 1, "{why}:\n{out}");
        assert!(
            out.contains("mount-witness registration evidence is incomplete"),
            "{why}:\n{out}"
        );
    }
    // Its census is the ordinary one.
    let (n, out) = census_of(FIX_MOUNT, "vfs-mount-witness");
    assert_eq!(n, 0, "{out}");
}

/// The workload comes from the bound artifacts, never from a caller.
#[test]
fn the_workload_is_derived_from_the_artifact_binding() {
    let bind = |k: &str, i: &str, r: &str| {
        format!(
            "ARTIFACT_BINDING arch=riscv64 kernel=build-riscv64/yarm-riscv64.bin kernel_sha256=x \
             kernel_features={k} kernel_resolved_features={k} kernel_nofp=pass kernel_cargo=c:fresh=0 \
             initramfs=build-riscv64/initramfs-core.cpio initramfs_sha256=y initramfs_features={i} \
             initramfs_compile_env=none source=clean:z required=none result={r}"
        )
    };
    let fd = "cap-gen-witness,vfs-correct-witness,vfs-fd-witness";
    let mount = "cap-gen-witness,vfs-correct-witness,vfs-mount-witness";
    for (k, i, r, want) in [
        ("none", "none", "ok", Some("core")),
        (fd, fd, "ok", Some("vfs-fd-witness")),
        (
            "vfs-correct-witness,vfs-fd-witness",
            "vfs-correct-witness,vfs-fd-witness",
            "ok",
            Some("vfs-fd-witness"),
        ),
        (mount, mount, "ok", Some("vfs-mount-witness")),
        (
            "vfs-correct-witness",
            "vfs-correct-witness",
            "ok",
            Some("core"),
        ),
        ("none", fd, "ok", None),
        (mount, "none", "ok", None),
        (
            "vfs-fd-witness,vfs-mount-witness",
            "vfs-fd-witness,vfs-mount-witness",
            "ok",
            None,
        ),
        (fd, fd, "fail", None),
    ] {
        let out = Command::new("python3")
            .args(["-I", HELPER, "profile", "--binding", &bind(k, i, r)])
            .output()
            .expect("python3");
        let text = String::from_utf8_lossy(&out.stdout);
        match want {
            Some(w) => {
                assert!(out.status.success(), "{k} / {i}: {text}");
                assert_eq!(text.trim(), format!("RISCV_WORKLOAD profile={w}"));
            }
            None => assert!(
                !out.status.success() && text.contains("[fail]"),
                "{k} / {i} / {r}: {text}"
            ),
        }
    }
    // The smoke derives it from its own binding output, after the binding check, before it boots,
    // and refuses a witness image in the terminal-fault cell.
    for needle in [
        "python3 -I \"$RV_SCRIPT_DIR/riscv64-workload-obligations.py\" profile \\",
        "--binding \"$(grep -m1 '^ARTIFACT_BINDING ' <<<\"$rv_binding_out\" || true)\")\" || {",
        "RISCV_WORKLOAD=\"${rv_workload_out##*profile=}\"",
        "the terminal-fault cell cannot run the $RISCV_WORKLOAD image",
    ] {
        assert_eq!(SMOKE.matches(needle).count(), 1, "{needle}");
    }
    let derive = SMOKE
        .find("RISCV_WORKLOAD=\"${rv_workload_out##*profile=}\"")
        .unwrap();
    assert!(SMOKE.find("(( rv_binding_rc == 0 )) || exit 1").unwrap() < derive);
    assert!(derive < SMOKE.find("-kernel \"$KERNEL_IMAGE\"").unwrap());
    assert_eq!(
        SMOKE.matches("RISCV_WORKLOAD=").count(),
        1,
        "the workload is assigned once, by the derivation"
    );
}

/// The terminal-fault cell's expectations are untouched: no spawn chain is owed there, whatever
/// the image, and the census's switching-disposition checks still run.
#[test]
fn the_terminal_fault_cell_still_owes_no_spawn_chain() {
    let no_chain: String = healthy()
        .lines()
        .filter(|l| !l.contains(" nr=23 ") && !l.contains(" nr=29 "))
        .map(|l| format!("{l}\n"))
        .collect();
    let (n, out) = graded(
        &no_chain,
        "core",
        census_fn(),
        "riscv64_split_census",
        false,
    );
    assert_eq!(n, 0, "{out}");
    let (n, out) = graded(
        &healthy(),
        "core",
        census_fn(),
        "riscv64_split_census",
        false,
    );
    assert_eq!(
        n, 2,
        "a chain in the terminal-fault cell still fails:\n{out}"
    );
}

#[test]
fn the_obligations_helper_self_test_passes() {
    let out = Command::new("python3")
        .args(["-I", HELPER, "--self-test"])
        .output()
        .expect("python3");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("RISCV_WORKLOAD_OBLIGATIONS_SELF_TEST result=ok")
    );
}

// ── VFS-LIFETIME1: the lifetime witness's census ─────────────────────────────────────────────

const NR29_REFUSED: &str = "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=29 cpu=0 result=handled_err code=2";

#[test]
fn vl1_the_lifetime_image_passes_its_own_obligations() {
    let (n, out) = smoke_of(FIX_LIFE, "vfs-lifetime-witness");
    assert_eq!(n, 0, "{out}");
    assert!(
        out.contains("RISCV-STRICT1: workload=vfs-lifetime-witness nr29_owed=10 nr12=1 nr16=1"),
        "{out}"
    );
    assert!(out.contains("nr29_refused=1") && out.contains("fork_child="), "{out}");
}

#[test]
fn vl1_the_lifetime_image_fails_as_any_other_image() {
    for w in ["core", "vfs-fd-witness", "vfs-mount-witness"] {
        let (n, out) = census_of(FIX_LIFE, w);
        assert!(n > 0, "the lifetime boot passed as {w}:\n{out}");
    }
    for (log, w) in [(FIX_CORE, "core"), (FIX_FD, "vfs-fd-witness")] {
        let (n, out) = census_of(log, "vfs-lifetime-witness");
        assert!(n > 0, "the {w} boot passed as the lifetime image:\n{out}");
    }
}

#[test]
fn vl1_the_one_refused_spawn_is_owed_exactly_and_only_as_the_fixture_use() {
    // A second refusal, a refusal with another error, and a refused spawn that is not PM's
    // image-14 use each fail, even where the totals still add up.
    let twice = edit(FIX_LIFE, |v| {
        let i = at(v, NR29_REFUSED);
        let line = v[i].clone();
        v.insert(i, line);
    });
    assert!(census_of(&twice, "vfs-lifetime-witness").0 > 0);
    let other = edit(FIX_LIFE, |v| {
        let i = at(v, NR29_REFUSED);
        v[i] = v[i].replace("code=2", "code=9");
    });
    assert!(census_of(&other, "vfs-lifetime-witness").0 > 0);
    let unattributed = edit(FIX_LIFE, |v| {
        let i = at(v, "VL1_GRANT_USE tid=3 image_id=14 ");
        v.remove(i);
    });
    assert!(census_of(&unattributed, "vfs-lifetime-witness").0 > 0);
    let lost_launch = edit(FIX_LIFE, |v| {
        let i = at(v, "msg=VL1_CYCLE label=success_2 ");
        v.remove(i);
    });
    assert!(census_of(&lost_launch, "vfs-lifetime-witness").0 > 0);
    let stray_exit = edit(FIX_LIFE, |v| {
        let i = at(v, "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=16 ");
        v[i] = v[i].replace("outgoing=", "outgoing=9");
    });
    assert!(census_of(&stray_exit, "vfs-lifetime-witness").0 > 0);
}
