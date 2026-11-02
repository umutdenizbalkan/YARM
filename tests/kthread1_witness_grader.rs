// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 — the kernel-thread witness grader, exercised through its CLI.
//!
//! `tests/fixtures/kthread1/base.rec` holds the `KT1_REC` / `KT1_SEAL` lines (both dump passes) of a
//! passing two-CPU RISC-V witness boot. Every case below derives one record set from it by a named edit, re-seals
//! it (each record's checksum and the seal recomputed, so only the edit itself is under test,
//! except where the case tampers with the integrity data on purpose), writes it to a file, runs
//! `scripts/grade-kthread1-witness.py --expect-cpus N <file>` and requires the exit status and the
//! named failure the case expects. The base must pass; every adversarial case must fail for its own
//! reason. `tests/fixtures/kthread1/single.rec` is the same for a one-CPU boot, graded by the
//! single-CPU rules (local wake, no hold).

use std::path::PathBuf;
use std::process::Command;

const GRADER: &str = "scripts/grade-kthread1-witness.py";
const BASE: &str = include_str!("fixtures/kthread1/base.rec");
const SINGLE: &str = include_str!("fixtures/kthread1/single.rec");

#[derive(Clone, Debug)]
struct Rec {
    seq: u64,
    kind: String,
    cpu: u64,
    f: [u64; 5],
}

fn fnv(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for b in data {
        h = (h ^ u32::from(*b)).wrapping_mul(0x0100_0193);
    }
    h
}

fn parse() -> Vec<Rec> {
    parse_from(BASE)
}

fn parse_from(text: &str) -> Vec<Rec> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find("KT1_REC ") else {
            continue;
        };
        let line = &line[at..];
        let mut r = Rec {
            seq: 0,
            kind: String::new(),
            cpu: 0,
            f: [0; 5],
        };
        for tok in line.split_whitespace().skip(1) {
            let (k, v) = tok.split_once('=').expect("key=value");
            let hex = |v: &str| u64::from_str_radix(v.trim_start_matches("0x"), 16).expect("hex");
            match k {
                "seq" => r.seq = v.parse().expect("seq"),
                "kind" => r.kind = v.to_string(),
                "cpu" => r.cpu = v.parse().expect("cpu"),
                "f0" | "f1" | "f2" | "f3" | "f4" => r.f[(k.as_bytes()[1] - b'0') as usize] = hex(v),
                _ => {}
            }
        }
        out.push(r);
    }
    // The dump has two passes; each record is taken once.
    out.sort_by_key(|r| r.seq);
    out.dedup_by_key(|r| r.seq);
    assert!(out.len() > 100, "the fixture holds a full record set");
    out
}

fn body(r: &Rec) -> String {
    format!(
        "KT1_REC seq={} kind={} cpu={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}",
        r.seq, r.kind, r.cpu, r.f[0], r.f[1], r.f[2], r.f[3], r.f[4]
    )
}

/// One pass of a dump: per-record checksums, then a seal over them with its own checksum.
fn pass(recs: &[Rec]) -> String {
    let mut s = String::new();
    let mut h: u32 = 0x811C_9DC5;
    for r in recs {
        let b = body(r);
        let crc = fnv(b.as_bytes());
        h = (h ^ crc).wrapping_mul(0x0100_0193);
        s.push_str(&format!("{b} crc=0x{crc:08x}\n"));
    }
    let seal = format!(
        "KT1_SEAL records={} taken={} overflow=0 seal=0x{h:08x}",
        recs.len(),
        recs.len()
    );
    s.push_str(&format!("{seal} crc=0x{:08x}\n", fnv(seal.as_bytes())));
    s
}

/// The record set as a single-pass dump.
fn seal(recs: &[Rec]) -> String {
    pass(recs) + "KT1_DUMP_END\n"
}

/// The record set as the witness dumps it: two passes.
fn dumped(recs: &[Rec]) -> String {
    pass(recs) + &pass(recs) + "KT1_DUMP_END\n"
}

/// The `n`th `KT1_REC` line of `seq` damaged as a stray console byte damages it (the byte seen
/// live: `seq=423 3kind=decide`).
fn damage(text: &str, seq: u64, n: usize) -> String {
    let key = format!("KT1_REC seq={seq} kind=");
    let at = text.match_indices(&key).nth(n).expect("that copy").0;
    let mut out = text.to_string();
    out.insert(at + key.len() - "kind=".len(), '3');
    out
}

fn grade(text: &str, cpus: u32, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("yarm-kt1g-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("boot.log");
    std::fs::write(&file, text).unwrap();
    let out = Command::new("python3")
        .args(["-I", GRADER, "--expect-cpus", &cpus.to_string()])
        .arg(&file)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run the grader");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn expect_fail(name: &str, text: &str, cpus: u32, reason: &str) {
    let (code, out) = grade(text, cpus, name);
    assert_eq!(code, 1, "{name}: must fail\n{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("[fail] ") && l.contains(reason)),
        "{name}: expected `{reason}`\n{out}"
    );
}

fn first<'a>(recs: &'a mut [Rec], pred: impl Fn(&Rec) -> bool) -> &'a mut Rec {
    recs.iter_mut()
        .find(|r| pred(r))
        .expect("a matching record")
}

fn edited(edit: impl Fn(&mut Vec<Rec>)) -> String {
    edited_from(BASE, edit)
}

fn edited_from(text: &str, edit: impl Fn(&mut Vec<Rec>)) -> String {
    let mut recs = parse_from(text);
    edit(&mut recs);
    seal(&recs)
}

/// Insert a record after position `at` and renumber, as the producer would have numbered it.
fn insert_after(recs: &mut Vec<Rec>, at: usize, rec: Rec) {
    recs.insert(at + 1, rec);
    for (i, r) in recs.iter_mut().enumerate() {
        r.seq = i as u64;
    }
}

#[test]
fn the_real_record_set_passes() {
    let (code, out) = grade(&seal(&parse()), 2, "base");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("result=pass"), "{out}");
    // And the base fixture as dumped (its own checksums and seal) passes unchanged.
    let (code, out) = grade(BASE, 2, "raw");
    assert_eq!(code, 0, "{out}");
}

#[test]
fn integrity_failures_award_nothing() {
    // A field edited without its checksum.
    let tampered = seal(&parse()).replacen(" kind=run cpu=", " kind=run cpu=", 1);
    let line = tampered
        .lines()
        .find(|l| l.contains(" kind=run "))
        .unwrap()
        .to_string();
    let bad = line.replacen("f1=0x1 ", "f1=0x2 ", 1);
    assert_ne!(bad, line);
    // The edited line fails its checksum: it counts for nothing, so its record is missing.
    expect_fail(
        "checksum",
        &tampered.replacen(&line, &bad, 1),
        2,
        "invalid sequence_gap",
    );
    // A record removed: the sequence has a gap (and the seal no longer matches).
    expect_fail(
        "gap",
        &edited(|r| {
            r.remove(10);
        }),
        2,
        "invalid sequence_gap",
    );
    // The seal's count disagrees with the records (re-sealed, so the seal's own checksum holds).
    let s = seal(&parse());
    let n = parse().len();
    let reseal = |old: &str, new: &str| {
        let line = s.lines().find(|l| l.starts_with("KT1_SEAL ")).unwrap();
        let text = line.split(" crc=").next().unwrap().replace(old, new);
        s.replace(line, &format!("{text} crc=0x{:08x}", fnv(text.as_bytes())))
    };
    expect_fail(
        "seal_count",
        &reseal(&format!("records={n} "), &format!("records={} ", n + 1)),
        2,
        "invalid seal_count",
    );
    // Ring overflow declared.
    expect_fail(
        "overflow",
        &reseal("overflow=0", "overflow=3"),
        2,
        "invalid ring_overflow",
    );
    // A seal edited without its checksum is no seal.
    expect_fail(
        "seal_checksum",
        &s.replace("overflow=0", "overflow=3"),
        2,
        "invalid seal_missing",
    );
}

#[test]
fn a_record_needs_one_intact_copy() {
    let recs = parse();
    // Both passes intact: pass, nothing damaged.
    let (code, out) = grade(&dumped(&recs), 2, "two_pass");
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("damaged_lines"), "{out}");
    // One copy damaged: the other copy carries the record; the damage is reported.
    let (code, out) = grade(&damage(&dumped(&recs), 40, 0), 2, "one_damaged");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[info] damaged_lines=1 "), "{out}");
    let (code, out) = grade(&damage(&dumped(&recs), 40, 1), 2, "other_damaged");
    assert_eq!(code, 0, "{out}");
    // Both copies damaged: the record is missing.
    expect_fail(
        "both_damaged",
        // (the first damage leaves one intact copy, now the first match)
        &damage(&damage(&dumped(&recs), 40, 0), 40, 0),
        2,
        "invalid sequence_gap",
    );
    // Two intact copies that differ: refused, whichever came first.
    let mut other = recs.clone();
    other[40].f[0] ^= 1;
    expect_fail(
        "conflict",
        &(pass(&recs) + &pass(&other) + "KT1_DUMP_END\n"),
        2,
        "invalid record_conflict seq=40",
    );
    // A seal per pass; one damaged seal is recovered from the other, two intact seals must agree.
    let one_seal = dumped(&recs).replacen("KT1_SEAL records=", "KT1_SEAL 4records=", 1);
    let (code, out) = grade(&one_seal, 2, "one_seal_damaged");
    assert_eq!(code, 0, "{out}");
    let mut fewer = recs.clone();
    fewer.pop();
    let pass2 = pass(&recs);
    let seal_fewer = pass(&fewer)
        .lines()
        .find(|l| l.starts_with("KT1_SEAL "))
        .unwrap()
        .to_string();
    let seal_all = pass2.lines().find(|l| l.starts_with("KT1_SEAL ")).unwrap();
    expect_fail(
        "seals_disagree",
        &(pass(&recs) + &pass2.replace(seal_all, &seal_fewer) + "KT1_DUMP_END\n"),
        2,
        "invalid seal_conflicting",
    );
}

#[test]
fn population_must_be_the_expected_cpus() {
    expect_fail("population", &seal(&parse()), 4, "population_mismatch");
}

#[test]
fn corrupted_continuations_are_detected() {
    expect_fail(
        "callee_saved",
        &edited(|r| first(r, |x| x.kind == "yield").f[2] = 0),
        2,
        "callee_saved_clobbered_by_switch",
    );
    expect_fail(
        "async_regs",
        &edited(|r| first(r, |x| x.kind == "run").f[4] &= !2),
        2,
        "registers_clobbered_by_interrupt",
    );
    expect_fail(
        "private_stack",
        &edited(|r| first(r, |x| x.kind == "run").f[4] &= !1),
        2,
        "private_stack_corrupted",
    );
    expect_fail(
        "off_home",
        &edited(|r| {
            let x = first(r, |x| x.kind == "run");
            x.cpu = 1 - x.cpu;
        }),
        2,
        "run_off_home",
    );
}

#[test]
fn a_stale_incarnation_accepted_is_detected() {
    expect_fail(
        "stale",
        &edited(|r| first(r, |x| x.kind == "probe" && x.f[1] == 3).f[2] = 1),
        2,
        "stale_wake_accepted",
    );
}

#[test]
fn a_suppressed_remote_wake_is_detected() {
    // Every reschedule arrival on CPU 1 becomes a timer arrival: the IPI never came.
    expect_fail(
        "no_arrival",
        &edited(|r| {
            for x in r
                .iter_mut()
                .filter(|x| x.kind == "irq" && x.cpu == 1 && x.f[1] == 2)
            {
                x.f[1] = 1;
            }
        }),
        2,
        "remote_wake_without_reschedule_arrival",
    );
    // The wake itself recorded from the target CPU: not remote.
    expect_fail(
        "not_remote",
        &edited(|r| {
            let x = first(r, |x| x.kind == "wake" && x.f[4] == 1 && x.f[1] == 1);
            x.cpu = 1;
        }),
        2,
        "remote_wake_not_remote_or_not_enqueued",
    );
    // The holder never honoured the pending reschedule.
    expect_fail(
        "not_honoured",
        &edited(|r| first(r, |x| x.kind == "resched" && x.cpu == 1).f[1] = 0),
        2,
        "reschedule_not_honoured",
    );
}

#[test]
fn a_lost_wakeup_is_detected() {
    expect_fail(
        "lost",
        &edited(|r| first(r, |x| x.kind == "probe" && x.f[1] == 4).f[2] = 1),
        2,
        "lost_wakeup",
    );
    expect_fail(
        "left_behind",
        &edited(|r| first(r, |x| x.kind == "final").f[1] = 4),
        2,
        "thread_left_behind",
    );
}

#[test]
fn a_switch_under_a_prohibited_guard_is_detected() {
    expect_fail(
        "masked",
        &edited(|r| {
            let x = first(r, |x| x.kind == "probe" && x.f[1] == 1);
            x.f[2] = 0;
            x.f[3] = 1;
        }),
        2,
        "switch_under_guard_not_refused probe=masked_section",
    );
    expect_fail(
        "irq_guard",
        &edited(|r| first(r, |x| x.kind == "probe" && x.f[1] == 2).f[3] = 1),
        2,
        "switch_under_guard_not_refused probe=irq_guard",
    );
}

#[test]
fn premature_stack_reuse_is_detected() {
    // A blip's reclaim recorded before the dispatcher switched away from it.
    expect_fail(
        "early_reclaim",
        &edited(|r| {
            let rec = r
                .iter()
                .position(|x| x.kind == "reclaim" && x.f[4] == 5)
                .unwrap();
            let kid = r[rec].f[0];
            let sw = r
                .iter()
                .position(|x| x.kind == "switch" && x.f[0] == kid && x.f[1] == 3)
                .unwrap();
            let (a, b) = (r[sw].clone(), r[rec].clone());
            r[sw] = Rec { seq: a.seq, ..b };
            r[rec] = Rec { seq: b.seq, ..a };
        }),
        2,
        "reclaimed_before_switch_away",
    );
    // A reclaim while the residency record still named the thread.
    expect_fail(
        "resident",
        &edited(|r| {
            let kid = first(r, |x| x.kind == "reclaim" && x.f[4] == 5).f[0];
            first(r, |x| x.kind == "switch" && x.f[0] == kid && x.f[1] == 3).f[2] = kid;
        }),
        2,
        "reclaimed_while_resident",
    );
    // A reused slot whose generation did not advance.
    expect_fail(
        "generation",
        &edited(|r| {
            let blips: Vec<usize> = (0..r.len()).filter(|&i| r[i].kind == "blip").collect();
            let g = r[blips[0]].f[2];
            r[blips[1]].f[2] = g;
        }),
        2,
        "reuse_generation_not_newer",
    );
}

#[test]
fn a_missing_kernel_origin_interrupt_population_is_detected() {
    expect_fail(
        "no_irq",
        &edited(|r| {
            for x in r.iter_mut().filter(|x| x.kind == "irq") {
                x.kind = "probe".to_string();
                x.f = [0; 5];
            }
        }),
        2,
        "no_timer_interrupt_on_kernel_thread",
    );
    expect_fail(
        "irq_off_stack",
        &edited(|r| first(r, |x| x.kind == "irq").f[3] = 0x1000),
        2,
        "irq_frame_off_stack",
    );
}

#[test]
fn user_progress_and_pool_recovery_are_required() {
    expect_fail(
        "no_user",
        &edited(|r| first(r, |x| x.kind == "user").f[1] = 0),
        2,
        "no_user_progress",
    );
    expect_fail(
        "exhaustion",
        &edited(|r| first(r, |x| x.kind == "exhaust").f[3] -= 1),
        2,
        "exhaustion_or_recovery",
    );
}

#[test]
fn the_holder_s_honoured_point_is_its_turn() {
    // The holder yields again after its scheduling point switched away (the defect that gave the
    // other alternator two turns in a row): an extra yield record for that round.
    expect_fail(
        "extra_yield",
        &edited(|r| {
            let at = r
                .iter()
                .position(|x| x.kind == "resched" && x.f[1] == 1)
                .expect("resched");
            let (kid, cpu) = (r[at].f[0], r[at].cpu);
            let round = r
                .iter()
                .find(|x| x.kind == "decide" && x.f[0] == kid)
                .expect("decide")
                .f[4];
            let y = Rec {
                seq: 0,
                kind: "yield".into(),
                cpu,
                f: [kid, round, 1, 0, 0],
            };
            insert_after(r, at, y);
        }),
        2,
        "yields_incomplete",
    );
    // Two runs of one alternator in a row after the hold.
    expect_fail(
        "double_turn",
        &edited(|r| {
            let d = r
                .iter()
                .position(|x| x.kind == "decide" && x.cpu == 1)
                .expect("decide");
            let late: Vec<usize> = (d..r.len())
                .filter(|&i| r[i].kind == "run" && r[i].cpu == 1)
                .collect();
            // Relabel the alternator of the fourth late run as the third's, in its yield too.
            let (a, b) = (late[2], late[3]);
            let (from, to) = (r[b].f[0], r[a].f[0]);
            assert_ne!(from, to);
            for x in r
                .iter_mut()
                .skip(b)
                .filter(|x| x.f[0] == from && x.cpu == 1)
            {
                if x.kind == "run" || x.kind == "yield" {
                    x.f[0] = to;
                    break;
                }
            }
        }),
        2,
        "alternation_broken",
    );
}

#[test]
fn a_single_cpu_record_set_is_graded_by_the_single_cpu_rules() {
    let (code, out) = grade(SINGLE, 1, "single");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("result=pass"), "{out}");
    expect_fail("single_as_two", SINGLE, 2, "population");
    expect_fail(
        "single_not_enqueued",
        &edited_from(SINGLE, |r| {
            first(r, |x| x.kind == "wake" && x.f[4] == 1).f[2] = 0
        }),
        1,
        "local_wake_not_enqueued",
    );
    expect_fail(
        "single_wrong_token",
        &edited_from(SINGLE, |r| first(r, |x| x.kind == "woken").f[1] = 0),
        1,
        "woken_thread_did_not_resume_after_wake",
    );
    expect_fail(
        "single_hold",
        &edited_from(SINGLE, |r| {
            let at = r.iter().position(|x| x.kind == "run").expect("run");
            let (kid, cpu) = (r[at].f[0], r[at].cpu);
            let d = Rec {
                seq: 0,
                kind: "decide".into(),
                cpu,
                f: [kid, 0, 0, 0, 1],
            };
            insert_after(r, at, d);
        }),
        1,
        "hold_on_single_cpu",
    );
}
