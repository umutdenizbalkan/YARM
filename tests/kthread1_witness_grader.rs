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
    /// The producer's execution stamp: the incarnation resident on the CPU (0: none) and the
    /// CPU's kernel-thread dispatch ordinal, read when the record was taken.
    res: u64,
    ord: u64,
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
            res: 0,
            ord: 0,
        };
        for tok in line.split_whitespace().skip(1) {
            let (k, v) = tok.split_once('=').expect("key=value");
            let hex = |v: &str| u64::from_str_radix(v.trim_start_matches("0x"), 16).expect("hex");
            match k {
                "seq" => r.seq = v.parse().expect("seq"),
                "kind" => r.kind = v.to_string(),
                "cpu" => r.cpu = v.parse().expect("cpu"),
                "f0" | "f1" | "f2" | "f3" | "f4" => r.f[(k.as_bytes()[1] - b'0') as usize] = hex(v),
                "res" => r.res = hex(v),
                "ord" => r.ord = hex(v),
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
        "KT1_REC seq={} kind={} cpu={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x} \
         res=0x{:x} ord=0x{:x}",
        r.seq, r.kind, r.cpu, r.f[0], r.f[1], r.f[2], r.f[3], r.f[4], r.res, r.ord
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
    // `KT1_GRADER` substitutes another copy of the grader (the undo controls run each case
    // against a grader with one repair removed).
    let grader = std::env::var("KT1_GRADER").unwrap_or_else(|_| GRADER.to_string());
    let out = Command::new("python3")
        .args(["-I", &grader, "--expect-cpus", &cpus.to_string()])
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
        "invalid lifecycle_run_outside_running_interval",
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
        // The replay refuses it first: the director's record, stamped with the director resident,
        // taken on a CPU where the director has no interval.
        "invalid lifecycle_director_resident_outside_its_interval",
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
        // The replay refuses it at the switch: residency was not cleared.
        "invalid lifecycle_switched_out_while_resident",
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
        // The replay refuses it first: the blip's record no longer names its own incarnation.
        "invalid lifecycle_blip_identity",
    );
}

#[test]
fn a_missing_kernel_origin_interrupt_population_is_detected() {
    expect_fail(
        "no_irq",
        &edited(|r| {
            r.retain(|x| x.kind != "irq");
            for (i, x) in r.iter_mut().enumerate() {
                x.seq = i as u64;
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
                res: r[at].res,
                ord: r[at].ord,
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
        // The replay refuses it first: the relabelled alternator was not running then.
        "invalid lifecycle_run_outside_running_interval",
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
                res: r[at].res,
                ord: r[at].ord,
            };
            insert_after(r, at, d);
        }),
        1,
        "hold_on_single_cpu",
    );
}

// ── KTHREAD1-ACCEPTANCE §4: the lifecycle replay ────────────────────────────────────────────────

fn renumber(recs: &mut [Rec]) {
    for (i, r) in recs.iter_mut().enumerate() {
        r.seq = i as u64;
    }
}

fn slot(kid: u64) -> u64 {
    kid & 0xFFFF
}

/// The three false passes the delivered grader awarded (KTHREAD1-ACCEPTANCE §4 reproduction):
/// every dispatch record deleted, all but one final deleted, an interrupt for a reclaimed thread.
#[test]
fn the_reproduced_false_passes_are_refused() {
    expect_fail(
        "no_dispatch",
        &edited(|r| {
            r.retain(|x| x.kind != "dispatch");
            renumber(r);
        }),
        2,
        "invalid lifecycle_",
    );
    expect_fail(
        "one_final_left",
        &edited(|r| {
            let first = r.iter().position(|x| x.kind == "final").unwrap();
            let mut i = 0;
            r.retain(|x| {
                i += 1;
                x.kind != "final" || i - 1 == first
            });
            renumber(r);
        }),
        2,
        "final_missing",
    );
    expect_fail(
        "irq_after_reclaim",
        &edited(|r| {
            let rec = r
                .iter()
                .rev()
                .find(|x| x.kind == "reclaim" && x.f[4] == 2)
                .expect("an alternator's reclaim")
                .clone();
            let cr = r
                .iter()
                .find(|x| x.kind == "create" && x.f[0] == rec.f[0])
                .unwrap()
                .clone();
            // Stamped as the thread itself would stamp it, in the CPU's latest interval.
            let ord = r
                .iter()
                .filter(|x| x.cpu == cr.f[2])
                .map(|x| x.ord)
                .max()
                .unwrap();
            r.push(Rec {
                seq: 0,
                kind: "irq".into(),
                cpu: cr.f[2],
                f: [rec.f[0], 1, 0xffff_ffff_8000_1234, cr.f[3] + 0x100, cr.f[1]],
                res: rec.f[0],
                ord,
            });
            renumber(r);
        }),
        2,
        "invalid lifecycle_irq_outside_running_interval",
    );
}

#[test]
fn impossible_lifecycle_events_are_refused() {
    // A second dispatch of a thread already running.
    expect_fail(
        "duplicate_dispatch",
        &edited(|r| {
            let at = r.iter().position(|x| x.kind == "dispatch").unwrap();
            let d = r[at].clone();
            r.insert(at + 1, d);
            renumber(r);
        }),
        2,
        "invalid lifecycle_dispatch_while_running",
    );
    // A dispatch recorded on a CPU that is not the thread's home.
    expect_fail(
        "dispatch_off_home",
        &edited(|r| {
            let x = first(r, |x| x.kind == "dispatch");
            x.cpu = 1 - x.cpu;
        }),
        2,
        "invalid lifecycle_dispatch_off_home",
    );
    // A run record naming an incarnation that was never created (a generation off by one).
    expect_fail(
        "wrong_incarnation",
        &edited(|r| first(r, |x| x.kind == "run").f[0] += 1 << 16),
        2,
        "invalid lifecycle_run_unknown_thread",
    );
    // A slot handed to a new incarnation before its previous holder was reclaimed.
    expect_fail(
        "reuse_before_reclaim",
        &edited(|r| {
            let rec = r
                .iter()
                .position(|x| {
                    x.kind == "reclaim"
                        && r.iter().any(|c| {
                            c.kind == "create" && c.seq > x.seq && slot(c.f[0]) == slot(x.f[0])
                        })
                })
                .expect("a reclaimed slot that is reused");
            let kid = r[rec].f[0];
            let reuse = r
                .iter()
                .position(|c| c.kind == "create" && c.seq > r[rec].seq && slot(c.f[0]) == slot(kid))
                .unwrap();
            let rc = r.remove(rec);
            r.insert(reuse, rc);
            renumber(r);
        }),
        2,
        "invalid lifecycle_slot_reused_before_reclaim",
    );
    // A final state naming one thread substituted for another's: one thread has two, the other none.
    expect_fail(
        "substituted_final",
        &edited(|r| {
            let finals: Vec<usize> = (0..r.len()).filter(|&i| r[i].kind == "final").collect();
            let (a, b) = (finals[0], finals[1]);
            r[b].f[0] = r[a].f[0];
            r[b].f[2] = r[a].f[2];
        }),
        2,
        "invalid lifecycle_final_duplicate",
    );
    // A final saying "gone" for a thread still running at that point.
    expect_fail(
        "final_before_exit",
        &edited(|r| {
            let last_switch = r
                .iter()
                .rposition(|x| x.kind == "switch" && x.f[1] == 3)
                .expect("an exit switch");
            let kid = r[last_switch].f[0];
            let fin = r
                .iter()
                .position(|x| x.kind == "final" && x.f[0] == kid)
                .unwrap();
            let f = r.remove(fin);
            let at = r
                .iter()
                .position(|x| x.kind == "switch" && x.f[1] == 3 && x.f[0] == kid)
                .unwrap();
            r.insert(at, f);
            renumber(r);
        }),
        2,
        "invalid lifecycle_final_gone_while_running",
    );
    // A create record whose stack is not its slot's.
    expect_fail(
        "foreign_stack",
        &edited(|r| first(r, |x| x.kind == "create").f[3] += 0x4000),
        2,
        "invalid lifecycle_create_stack",
    );
}

/// The obligations are bound to the creation records, not to whatever a role field says later:
/// relabelling a thread changes nothing about what must be shown for the threads that exist.
#[test]
fn a_changed_role_does_not_make_obligations_disappear() {
    // An alternator relabelled as a cycle thread in EVERY record that carries its role: the
    // replay sees a consistent thread, and the populations anchored in the creations still miss
    // an alternator.
    expect_fail(
        "relabelled_alternator",
        &edited(|r| {
            let kid = first(r, |x| x.kind == "create" && x.f[1] == 2).f[0];
            for x in r.iter_mut().filter(|x| x.f[0] == kid) {
                match x.kind.as_str() {
                    "create" | "dispatch" => x.f[1] = 5,
                    "switch" => x.f[3] = 5,
                    "reclaim" => x.f[4] = 5,
                    "final" => x.f[2] = 5,
                    _ => {}
                }
            }
        }),
        2,
        "creation_population",
    );
    expect_fail(
        "unknown_role",
        &edited(|r| first(r, |x| x.kind == "create" && x.f[1] == 3).f[1] = 9),
        2,
        "unknown_role",
    );
    // The director is excluded by its declared identity only; a second declaration is invalid.
    expect_fail(
        "second_director",
        &edited(|r| {
            let d = r.iter().find(|x| x.kind == "director").unwrap().clone();
            r.insert(5, d);
            renumber(r);
        }),
        2,
        "invalid lifecycle_director_not_declared",
    );
}

/// Legitimate orders the producer can emit pass: slot reuse with a new generation (present in the
/// fixture), and a final "gone" observed before its reclaim record (the slot is free before the
/// record is written). The reclaim moves only past other CPUs' records: a CPU's own records keep
/// their order (each is taken on that CPU, in its program order).
#[test]
fn legitimate_reuse_and_concurrent_orders_pass() {
    let recs = parse();
    let reused = recs.iter().any(|c| {
        c.kind == "create"
            && recs
                .iter()
                .any(|p| p.kind == "create" && p.seq < c.seq && slot(p.f[0]) == slot(c.f[0]))
    });
    assert!(reused, "the fixture reuses a slot");
    let moved = edited(|r| {
        // A thread whose slot is not handed out again after its reclaim, with a final after it.
        let rec = r
            .iter()
            .position(|x| {
                x.kind == "reclaim"
                    && !r.iter().any(|c| {
                        c.kind == "create" && c.seq > x.seq && slot(c.f[0]) == slot(x.f[0])
                    })
                    && r.iter().any(|f| {
                        f.kind == "final"
                            && f.f[0] == x.f[0]
                            && f.seq > x.seq
                            && !r
                                .iter()
                                .any(|y| y.cpu == x.cpu && y.seq > x.seq && y.seq < f.seq)
                    })
            })
            .expect("a last reclaim of its slot");
        let kid = r[rec].f[0];
        let rc = r.remove(rec);
        let fin = r
            .iter()
            .position(|x| x.kind == "final" && x.f[0] == kid)
            .unwrap();
        r.insert(fin + 1, rc);
        renumber(r);
    });
    let (code, out) = grade(&moved, 2, "final_before_reclaim");
    assert_eq!(code, 0, "{out}");
}

/// KTHREAD1-ACCEPTANCE §5 — the one-CPU coexistence phase: user rounds between kernel-thread
/// turns, and a real deadline delivered while kernel threads stayed runnable.
#[test]
fn single_cpu_coexistence_and_timeout_delivery_are_required() {
    assert!(
        SINGLE.contains("kind=uwork"),
        "the single-CPU fixture carries the S1 phase"
    );
    expect_fail(
        "wrong_work_value",
        &edited_from(SINGLE, |r| first(r, |x| x.kind == "uwork").f[2] ^= 1),
        1,
        "user_work_value_wrong",
    );
    expect_fail(
        "no_user_work",
        &edited_from(SINGLE, |r| {
            r.retain(|x| !matches!(x.kind.as_str(), "uwork" | "uarm" | "utimeout"));
            renumber(r);
        }),
        1,
        "user_work_missing",
    );
    expect_fail(
        "deadline_not_timed_out",
        &edited_from(SINGLE, |r| {
            for x in r.iter_mut().filter(|x| x.kind == "utimeout") {
                x.f[2] = 0;
            }
        }),
        1,
        "user_deadline_result",
    );
    // Every timeout preceded by an idle arrival: the CPU went idle to deliver it.
    expect_fail(
        "delivered_only_via_idle",
        &edited_from(SINGLE, |r| {
            for x in r.iter_mut().filter(|x| x.kind == "utimeout") {
                x.f[4] += 1;
            }
        }),
        1,
        "user_deadline_not_delivered_while_kernel_threads_ran",
    );
    // A user record from another task is not P's progress.
    expect_fail(
        "foreign_user_record",
        &edited_from(SINGLE, |r| first(r, |x| x.kind == "uwork").f[0] = 1),
        1,
        "invalid lifecycle_user_record_identity",
    );
}

// ── KTHREAD-CLOSURE1 §A: one execution owner per CPU ─────────────────────────────────────────────

/// Position of the next record of `cpu` after position `at`.
fn next_on(recs: &[Rec], cpu: u64, at: usize) -> Option<usize> {
    (at + 1..recs.len()).find(|&j| recs[j].cpu == cpu)
}

/// A switch-away on CPU 0 whose CPU's next record is another thread's dispatch.
fn switch_then_other_dispatch(r: &[Rec]) -> (usize, usize) {
    (0..r.len())
        .filter(|&i| r[i].kind == "switch" && r[i].cpu == 0)
        .find_map(|i| {
            let j = next_on(r, 0, i)?;
            (r[j].kind == "dispatch" && r[j].f[0] != r[i].f[0]).then_some((i, j))
        })
        .expect("a switch followed by another thread's dispatch on CPU 0")
}

/// The KTHREAD-CLOSURE1 §A1 reproduction (passed by the delivered grader at `a7da04f1`): thread
/// B's dispatch moved before thread A's switch-away on the same CPU, so both own CPU 0 at once.
/// With each record's stamp as the producer took it, the CPU's ordinals run backwards; restamped
/// so the ordinals are consistent, A's switch-away happens while B owns the CPU.
#[test]
fn overlapping_owners_on_one_cpu_are_refused() {
    expect_fail(
        "overlap_moved",
        &edited(|r| {
            let (i, j) = switch_then_other_dispatch(r);
            let d = r.remove(j);
            r.insert(i, d);
            renumber(r);
        }),
        2,
        "invalid lifecycle_ord_backwards",
    );
    expect_fail(
        "overlap_restamped",
        &edited(|r| {
            let (i, j) = switch_then_other_dispatch(r);
            let d = r.remove(j);
            r[i].ord = d.ord;
            r.insert(i, d);
            renumber(r);
        }),
        2,
        "invalid lifecycle_switch_by_non_owner",
    );
    // The switch-away names a thread that does not own the CPU (another thread homed there).
    expect_fail(
        "wrong_owner_switch",
        &edited(|r| {
            let (i, j) = switch_then_other_dispatch(r);
            r[i].f[0] = r[j].f[0];
        }),
        2,
        "invalid lifecycle_switch_by_non_owner",
    );
    // An interval that ends without its switch-away (the record deleted).
    expect_fail(
        "missing_switch",
        &edited(|r| {
            let (i, _) = switch_then_other_dispatch(r);
            r.remove(i);
            renumber(r);
        }),
        2,
        "invalid lifecycle_interval_ended_without_switch",
    );
    // A dispatch that opens no new interval (its ordinal is the previous interval's).
    expect_fail(
        "dispatch_same_interval",
        &edited(|r| {
            let (i, j) = switch_then_other_dispatch(r);
            r[j].ord = r[i].ord;
        }),
        2,
        "invalid lifecycle_dispatch_without_new_interval",
    );
    // A dispatch whose CPU did not have the dispatched thread resident.
    expect_fail(
        "dispatch_not_resident",
        &edited(|r| first(r, |x| x.kind == "dispatch").res = 0),
        2,
        "invalid lifecycle_dispatch_resident_mismatch",
    );
    // A CPU's ordinal running backwards.
    expect_fail(
        "ord_backwards",
        &edited(|r| {
            let at = (1..r.len())
                .find(|&i| r[i].kind == "run" && r[i].ord > 0)
                .unwrap();
            r[at].ord -= 1;
        }),
        2,
        "invalid lifecycle_ord_backwards",
    );
    // An in-thread record taken with nothing resident.
    expect_fail(
        "run_without_resident",
        &edited(|r| first(r, |x| x.kind == "run").res = 0),
        2,
        "invalid lifecycle_record_without_resident",
    );
    // An in-thread record whose resident is another thread than the CPU's owner.
    expect_fail(
        "run_other_resident",
        &edited(|r| {
            let (_, j) = switch_then_other_dispatch(r);
            let kid = r[j].f[0];
            let at = (j + 1..r.len())
                .find(|&k| r[k].cpu == 0 && r[k].res == kid && r[k].kind != "switch")
                .unwrap();
            r[at].res = kid + (1 << 16);
        }),
        2,
        "invalid lifecycle_resident_not_the_cpu_owner",
    );
    // A director record taken inside a recorded kernel-thread interval.
    expect_fail(
        "director_in_thread_interval",
        &edited(|r| {
            let at = r.iter().rposition(|x| x.kind == "create").unwrap();
            let ival = (0..at)
                .rev()
                .find(|&k| r[k].kind == "dispatch" && r[k].cpu == r[at].cpu)
                .unwrap();
            let kid = r[ival].f[0];
            r[at].res = kid;
        }),
        2,
        "invalid lifecycle_create_not_by_director",
    );
    // A reclaim recorded while a thread owns the CPU.
    expect_fail(
        "reclaim_while_owned",
        &edited(|r| {
            let x = first(r, |x| x.kind == "reclaim");
            x.res = x.f[0];
        }),
        2,
        "invalid lifecycle_reclaimed_while_cpu_owned",
    );
}

/// User execution cannot coexist with a kernel thread that still owns the CPU.
#[test]
fn user_execution_inside_a_kernel_thread_interval_is_refused() {
    // P's work stamped with a kernel thread resident.
    expect_fail(
        "user_with_resident",
        &edited_from(SINGLE, |r| {
            let kid = first(r, |x| x.kind == "dispatch").f[0];
            first(r, |x| x.kind == "uwork").res = kid;
        }),
        1,
        "invalid lifecycle_user_execution_while_kernel_thread_owns_cpu",
    );
    // P's arm moved into the recorded kernel-thread interval that follows it (stamped with that
    // interval's ordinal, nothing resident; the user sequence itself unchanged): the interval's
    // owner never switched away.
    expect_fail(
        "user_inside_interval",
        &edited_from(SINGLE, |r| {
            let a = r.iter().position(|x| x.kind == "uarm").unwrap();
            let d = (a + 1..r.len())
                .find(|&k| r[k].kind == "dispatch")
                .expect("a kernel-thread interval after the arm");
            let mut u = r.remove(a);
            u.ord = r[d - 1].ord;
            u.res = 0;
            r.insert(d, u);
            renumber(r);
        }),
        1,
        "invalid lifecycle_user_execution_while_kernel_thread_owns_cpu",
    );
}

// ── KTHREAD-CLOSURE1 §A: every user timeout sequence is valid ────────────────────────────────────

fn user_rec(r: &[Rec], kind: &str, round: u64) -> usize {
    r.iter()
        .position(|x| x.kind == kind && x.f[1] == round)
        .unwrap_or_else(|| panic!("{kind} round {round}"))
}

/// Move record `from` to just before position `to`, stamped as the record it lands next to.
fn move_restamped(r: &mut Vec<Rec>, from: usize, to: usize) {
    let mut x = r.remove(from);
    let to = if from < to { to - 1 } else { to };
    x.res = r[to].res;
    x.ord = r[to].ord;
    r.insert(to, x);
    renumber(r);
}

/// The KTHREAD-CLOSURE1 §A1 reproduction (passed by the delivered grader at `a7da04f1`): round
/// 2's result moved before round 2's arm. Every round is now validated before any coverage is
/// awarded, so the invalid round fails although other rounds still deliver their deadlines.
#[test]
fn every_user_timeout_sequence_is_validated() {
    // As reproduced, each record keeping its own stamp: the CPU's ordinals run backwards.
    expect_fail(
        "timeout_before_arm_moved",
        &edited_from(SINGLE, |r| {
            let (a, t) = (user_rec(r, "uarm", 2), user_rec(r, "utimeout", 2));
            let x = r.remove(t);
            r.insert(a, x);
            renumber(r);
        }),
        1,
        "invalid lifecycle_ord_backwards",
    );
    // Restamped so the replay accepts the CPU's order: the sequence rule refuses the round.
    let cases: [(&str, fn(&mut Vec<Rec>), &str); 9] = [
        (
            "timeout_before_arm",
            |r| {
                let (a, t) = (user_rec(r, "uarm", 2), user_rec(r, "utimeout", 2));
                move_restamped(r, t, a);
            },
            "user_sequence_invalid",
        ),
        (
            "timeout_missing",
            |r| {
                r.remove(user_rec(r, "utimeout", 3));
                renumber(r);
            },
            "user_sequence_invalid",
        ),
        (
            "arm_missing",
            |r| {
                r.remove(user_rec(r, "uarm", 3));
                renumber(r);
            },
            "user_sequence_invalid",
        ),
        (
            "arm_duplicated",
            |r| {
                let a = user_rec(r, "uarm", 3);
                let x = r[a].clone();
                insert_after(r, a, x);
            },
            "user_sequence_invalid",
        ),
        (
            "timeout_duplicated",
            |r| {
                let t = user_rec(r, "utimeout", 3);
                let x = r[t].clone();
                insert_after(r, t, x);
            },
            "user_sequence_invalid",
        ),
        (
            "timeout_round_substituted",
            |r| {
                let t = user_rec(r, "utimeout", 3);
                r[t].f[1] = 4;
            },
            "user_sequence_invalid",
        ),
        (
            "rounds_backwards",
            |r| {
                for x in r.iter_mut() {
                    if matches!(x.kind.as_str(), "uwork" | "uarm" | "utimeout") {
                        x.f[1] = match x.f[1] {
                            3 => 4,
                            4 => 3,
                            n => n,
                        };
                    }
                }
            },
            "user_sequence_invalid",
        ),
        (
            "deadline_not_elapsed",
            |r| {
                let (a, t) = (user_rec(r, "uarm", 3), user_rec(r, "utimeout", 3));
                r[t].f[3] = r[a].f[3];
            },
            "user_deadline_not_elapsed round=3",
        ),
        (
            "address_space_changed",
            |r| {
                let w = user_rec(r, "uwork", 3);
                r[w].f[3] ^= 1;
            },
            "user_round_address_space_changed",
        ),
    ];
    for (name, edit, reason) in cases {
        expect_fail(name, &edited_from(SINGLE, edit), 1, reason);
    }
}

/// Valid unfinished work at the census boundaries is not an impossible operation: recording
/// starts asynchronously to P (the first recorded round begins at its arm or result) and stops
/// after the director ends P's phase (the last round ends after its work or arm).
#[test]
fn valid_unfinished_rounds_at_the_census_boundaries_pass() {
    let recs = parse_from(SINGLE);
    let users: Vec<&Rec> = recs
        .iter()
        .filter(|x| matches!(x.kind.as_str(), "uwork" | "uarm" | "utimeout"))
        .collect();
    assert_ne!(
        users[0].kind, "uwork",
        "the fixture's first round is cut by recording's start"
    );
    assert_ne!(
        users.last().unwrap().kind,
        "utimeout",
        "the fixture's last round is cut by the census"
    );
    // Cut differently: the leading round from its arm (every user record before the first arm
    // removed), the last round right after its work.
    let cut = edited_from(SINGLE, |r| {
        let first_arm = r.iter().position(|x| x.kind == "uarm").unwrap();
        let mut i = 0;
        r.retain(|x| {
            i += 1;
            i - 1 >= first_arm || !matches!(x.kind.as_str(), "uwork" | "utimeout")
        });
        let last = r.iter().rposition(|x| x.kind == "uarm").unwrap();
        r.remove(last);
        renumber(r);
    });
    let (code, out) = grade(&cut, 1, "cut_rounds");
    assert_eq!(code, 0, "{out}");
    // But an unfinished round in the middle is not at a boundary.
    expect_fail(
        "unfinished_middle_round",
        &edited_from(SINGLE, |r| {
            r.retain(|x| !(x.f[1] == 3 && matches!(x.kind.as_str(), "uarm" | "utimeout")));
            renumber(r);
        }),
        1,
        "user_sequence_invalid",
    );
}

// ── KTHREAD-CLOSURE1 §B: a real plain lock across scheduling points ─────────────────────────────

fn plain_probe(r: &Rec) -> bool {
    r.kind == "probe" && r.f[1] == 5
}

/// The director holds a real plain `SpinLock` across a yield, a block, a nested acquisition, a
/// partial release and a timer interrupt; every attempt must be refused as `plain_lock_held`, the
/// counts must follow the guards, and the final release must restore scheduling.
#[test]
fn a_switch_under_a_plain_lock_is_detected() {
    let cases: [(&str, fn(&mut Vec<Rec>), &str); 7] = [
        (
            "plain_yield_switched",
            |r| first(r, plain_probe).f[3] |= 1,
            "switch_under_guard_not_refused probe=plain_lock",
        ),
        (
            "plain_block_not_refused",
            |r| first(r, plain_probe).f[2] &= !0xFF00,
            "switch_under_guard_not_refused probe=plain_lock",
        ),
        (
            "plain_refused_for_another_reason",
            |r| {
                let x = first(r, plain_probe);
                x.f[2] = (x.f[2] & !0xFF) | 1;
            },
            "switch_under_guard_not_refused probe=plain_lock",
        ),
        (
            "plain_release_not_restored",
            |r| first(r, plain_probe).f[3] &= !0x10,
            "plain_lock_release_did_not_restore_scheduling",
        ),
        (
            "plain_count_wrong",
            |r| first(r, plain_probe).f[4] ^= 3 << 8,
            "plain_lock_count_wrong",
        ),
        (
            "plain_without_interrupt",
            |r| first(r, plain_probe).f[4] &= !(1 << 32),
            "plain_lock_probe_without_interrupt",
        ),
        (
            "plain_probe_missing",
            |r| {
                r.retain(|x| !plain_probe(x));
                renumber(r);
            },
            "switch_under_guard_not_refused probe=plain_lock records=0",
        ),
    ];
    for (name, edit, reason) in cases {
        expect_fail(name, &edited(edit), 2, reason);
        expect_fail(name, &edited_from(SINGLE, edit), 1, reason);
    }
    // The probe is the director's: on a thread, it is refused by the replay.
    expect_fail(
        "plain_probe_not_by_director",
        &edited(|r| {
            let alt = first(r, |x| x.kind == "create" && x.f[1] == 2).f[0];
            first(r, plain_probe).f[0] = alt;
        }),
        2,
        "invalid lifecycle_probe_not_by_director",
    );
}

fn plain_other(r: &Rec) -> bool {
    r.kind == "probe" && r.f[1] == 6
}

/// KTHREAD-CLOSURE1 §B: with two CPUs, a holder on the second CPU takes a plain lock (its own yield
/// refused) while the director's count stays 0, the holder's is 1 and the director can yield.
/// Every misattribution, a missing probe, and a probe at one CPU are refused.
#[test]
fn another_threads_plain_lock_is_attributed_to_that_thread_only() {
    let cases: [(&str, fn(&mut Vec<Rec>), &str); 6] = [
        (
            "counted_on_the_director",
            |r| first(r, plain_other).f[2] |= 1,
            "plain_lock_misattributed",
        ),
        (
            "holder_not_counted",
            |r| first(r, plain_other).f[2] &= !0xFF00,
            "plain_lock_misattributed",
        ),
        (
            "holder_yield_not_refused",
            |r| first(r, plain_other).f[2] &= !0xFF_0000,
            "plain_lock_misattributed",
        ),
        (
            "director_refused",
            |r| first(r, plain_other).f[3] = 7,
            "plain_lock_misattributed",
        ),
        (
            "holder_never_held",
            |r| first(r, plain_other).f[4] = 0,
            "plain_lock_misattributed",
        ),
        (
            "attribution_probe_missing",
            |r| {
                r.retain(|x| !plain_other(x));
                renumber(r);
            },
            "plain_lock_attribution_probe records=0",
        ),
    ];
    for (name, edit, reason) in cases {
        expect_fail(name, &edited(edit), 2, reason);
    }
    // The one-CPU set has no attribution probe and passes; it must not carry one.
    assert!(
        parse_from(SINGLE).iter().all(|x| !plain_other(x)),
        "the one-CPU fixture has no attribution probe"
    );
}
