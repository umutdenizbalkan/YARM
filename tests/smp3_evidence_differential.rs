// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! SMP3-EVIDENCE1 — the kernel verifier and the independent grader judge the same records alike.
//!
//! Every case of `tests/fixtures/smp3_evidence/cases.txt` is built from a real sealed record
//! (`base.rec`: the `SMP3_ROLES` and `SMP3_REC` lines of a passing two-hart boot) by named edits,
//! or from nothing. Each case is written to one file and graded twice: by the PRODUCTION Rust
//! verifier (`smp3_record::verify_attempts`, the code the kernel runs at its dump) and by the Python
//! grader's records-only CLI (`scripts/grade-riscv64-smp3-witness.py --records`). The two must
//! print the same verdict (acceptance, first violated obligation and where), the same counts and the
//! same graded attempts — and the verdict must be the one the case expects.
//!
//! The corpus covers both repairs: shootdown decisions checked against the production policy over
//! their recorded inputs (placement-only, residency-only and overlapping contributions, the
//! dispatch gap, another task of the address space, a justified exclusion after leaving and
//! flushing, missing or substituted targets and hart maps, failed / missing / duplicated /
//! mismatched completions, settlement before completion), and IPI attribution from the mailbox
//! protocol (both delayed-record orders, valid coalescing, a fresh publication after consumption,
//! lost publications, missing consumption, unexplained arrivals, duplicate attribution, stale
//! groups, wrong targets, firmware failure).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use yarm::kernel::boot::smp3_record::{self as rec, Kind, Rec, Role, Roles};

const GRADER: &str = "scripts/grade-riscv64-smp3-witness.py";
const BASE: &str = include_str!("fixtures/smp3_evidence/base.rec");
const CASES: &str = include_str!("fixtures/smp3_evidence/cases.txt");

fn repo() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("yarm-s3e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn kind_of(name: &str) -> Kind {
    (1..=u8::MAX)
        .filter_map(Kind::from_u8)
        .find(|k| k.name() == name)
        .unwrap_or_else(|| panic!("unknown record kind `{name}`"))
}

fn num(v: &str) -> u64 {
    match v.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16),
        None => v.parse(),
    }
    .unwrap_or_else(|_| panic!("bad number `{v}`"))
}

/// One record of a case, by its identity: the base record's seq, or the id a case gave it.
#[derive(Clone, Copy)]
struct Item {
    id: u64,
    kind: Kind,
    cpu: u8,
    f: [u64; 5],
}

fn field<'a>(l: &'a str, key: &str) -> &'a str {
    l.split_whitespace()
        .find_map(|w| w.strip_prefix(key))
        .unwrap_or_else(|| panic!("`{key}` missing in `{l}`"))
}

fn parse_rec(l: &str) -> Item {
    let mut f = [0u64; 5];
    for (i, v) in f.iter_mut().enumerate() {
        *v = num(field(l, &format!("f{i}=")));
    }
    Item {
        id: num(field(l, "seq=")),
        kind: kind_of(field(l, "kind=")),
        cpu: num(field(l, "cpu=")) as u8,
        f,
    }
}

fn roles_of(line: &str) -> Roles {
    let part = |key: &str| -> Vec<u64> { field(line, key).split(':').map(num).collect() };
    let (s, c, h1, h0) = (part("s="), part("c="), part("h1="), part("h0="));
    let role = |p: &[u64]| Role {
        tid: p[0],
        asid: p[1],
        cpu: p[2] as u8,
        hart: p[3],
    };
    let (s, c) = (role(&s), role(&c));
    Roles {
        s,
        c,
        h1: Role {
            tid: h1[0],
            asid: h1[1],
            ..s
        },
        h0: Role {
            tid: h0[0],
            asid: h0[1],
            ..c
        },
        w_va: num(field(line, "w_va=")),
    }
}

struct Case {
    name: String,
    expect: String,
    from_base: bool,
    edits: Vec<String>,
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    let mut cur: Option<Case> = None;
    for raw in CASES.lines() {
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(name) = l.strip_prefix("case ") {
            assert!(cur.is_none(), "case `{name}` opened inside another");
            cur = Some(Case {
                name: name.into(),
                expect: String::new(),
                from_base: true,
                edits: Vec::new(),
            });
            continue;
        }
        let c = cur
            .as_mut()
            .unwrap_or_else(|| panic!("`{l}` outside a case"));
        if let Some(e) = l.strip_prefix("expect ") {
            c.expect = e.into();
        } else if l == "empty" {
            c.from_base = false;
        } else if l == "end" {
            let done = cur.take().unwrap();
            assert!(
                !done.expect.is_empty(),
                "case `{}` expects nothing",
                done.name
            );
            out.push(done);
        } else {
            c.edits.push(l.into());
        }
    }
    assert!(cur.is_none(), "unterminated case");
    out
}

fn pos(items: &[Item], id: u64) -> usize {
    items
        .iter()
        .position(|i| i.id == id)
        .unwrap_or_else(|| panic!("no record with id {id}"))
}

/// Apply one case's edits. Ids name records stably across edits; the result is renumbered.
fn build(case: &Case, roles_line: &str, base: &[Item]) -> (String, Vec<Item>) {
    let mut roles = roles_line.to_string();
    let mut items: Vec<Item> = if case.from_base {
        base.to_vec()
    } else {
        Vec::new()
    };
    for e in &case.edits {
        let w: Vec<&str> = e.split_whitespace().collect();
        match w[0] {
            // harts <s_hart> <c_hart>: the same record as a boot whose harts were numbered so —
            // the roles and every hart a record names (publications, requests, fence masks).
            "harts" => {
                let r = roles_of(&roles);
                let map = [(r.s.hart, num(w[1])), (r.c.hart, num(w[2]))];
                let hart = |h: u64| map.iter().find(|(a, _)| *a == h).map_or(h, |(_, b)| *b);
                let mask = |m: u64| {
                    (0..64)
                        .filter(|b| m & (1u64 << b) != 0)
                        .fold(0u64, |acc, b| acc | (1u64 << hart(b)))
                };
                for it in &mut items {
                    match it.kind {
                        Kind::IpiPublished | Kind::IpiRequested => it.f[1] = hart(it.f[1]),
                        Kind::FenceRequest | Kind::FenceDone => it.f[2] = mask(it.f[2]),
                        _ => {}
                    }
                }
                let relabel = |line: &str, key: &str, new: u64| -> String {
                    let old = field(line, key);
                    let mut p: Vec<String> = old.split(':').map(String::from).collect();
                    p[3] = new.to_string();
                    line.replace(&format!("{key}{old}"), &format!("{key}{}", p.join(":")))
                };
                roles = relabel(&roles, "s=", num(w[1]));
                roles = relabel(&roles, "c=", num(w[2]));
            }
            // set <id> <key>=<value>...   (keys: cpu, f0..f4)
            "set" => {
                let i = pos(&items, num(w[1]));
                for kv in &w[2..] {
                    let (k, v) = kv.split_once('=').expect("key=value");
                    match k {
                        "cpu" => items[i].cpu = num(v) as u8,
                        _ => items[i].f[k[1..].parse::<usize>().expect("f<n>")] = num(v),
                    }
                }
            }
            "del" => {
                let i = pos(&items, num(w[1]));
                items.remove(i);
            }
            // move <id> after|before <id>
            "move" => {
                let it = items.remove(pos(&items, num(w[1])));
                let at = pos(&items, num(w[3])) + usize::from(w[2] == "after");
                items.insert(at, it);
            }
            // add <newid> after|before <id>|start <kind> <cpu> <f0> .. <f4>
            "add" => {
                let mut f = [0u64; 5];
                for (d, v) in f.iter_mut().zip(&w[6..11]) {
                    *d = num(v);
                }
                let it = Item {
                    id: num(w[1]),
                    kind: kind_of(w[4]),
                    cpu: num(w[5]) as u8,
                    f,
                };
                let at = if w[3] == "start" {
                    0
                } else {
                    pos(&items, num(w[3])) + usize::from(w[2] == "after")
                };
                items.insert(at, it);
            }
            // dup <newid> after <id> of <id>
            "dup" => {
                let mut it = items[pos(&items, num(w[5]))];
                it.id = num(w[1]);
                let at = pos(&items, num(w[3])) + 1;
                items.insert(at, it);
            }
            other => panic!("unknown edit `{other}` in case `{}`", case.name),
        }
    }
    (roles, items)
}

fn render(roles_line: &str, items: &[Item]) -> String {
    let mut out = String::from(roles_line);
    out.push('\n');
    for (seq, it) in items.iter().enumerate() {
        let step = match it.kind {
            Kind::User | Kind::Residency => format!(" step={}", rec::step_name(it.f[2])),
            _ => String::new(),
        };
        out.push_str(&format!(
            "SMP3_REC seq={} kind={} cpu={} f0=0x{:x} f1=0x{:x} f2=0x{:x} f3=0x{:x} f4=0x{:x}{}\n",
            seq,
            it.kind.name(),
            it.cpu,
            it.f[0],
            it.f[1],
            it.f[2],
            it.f[3],
            it.f[4],
            step
        ));
    }
    out
}

/// The Rust verdict in the CLI's own format, from the records as written to the file.
fn rust_lines(text: &str) -> Vec<String> {
    let roles = roles_of(text.lines().next().unwrap());
    let recs: Vec<Rec> = text
        .lines()
        .skip(1)
        .map(|l| {
            let it = parse_rec(l);
            Rec {
                seq: it.id as u32,
                kind: it.kind,
                cpu: it.cpu,
                f: it.f,
            }
        })
        .collect();
    let (v, att) = rec::verify_attempts(&recs, &roles, false);
    let counts: [(&str, u64); 37] = [
        ("records", v.records as u64),
        ("published", v.ipi_published as u64),
        ("fresh", v.ipi_fresh as u64),
        ("requests", v.ipi_requests as u64),
        ("arrivals", v.ipi_arrivals as u64),
        ("consumed", v.ipi_consumed as u64),
        ("empty", v.ipi_empty as u64),
        ("merged", v.ipi_merged as u64),
        ("user_fp_vs_off", v.user_fp_vs_off as u64),
        ("p1_parked", v.p1_parked as u64),
        ("p1_ipi_to_s", v.p1_ipi_to_s as u64),
        ("p1_ipi_to_c", v.p1_ipi_to_c as u64),
        ("p1_timer_first", v.p1_timer_first as u64),
        ("p1_busy", v.p1_busy as u64),
        ("p1_preceded", v.p1_preceded as u64),
        ("p1_merged", v.p1_merged as u64),
        ("p2_attempts", v.p2_attempts as u64),
        ("p2_credited_s", v.p2_credited_s as u64),
        ("p2_credited_c", v.p2_credited_c as u64),
        ("p2_uncredited", v.p2_uncredited as u64),
        ("p2_in_window", v.p2_in_window as u64),
        ("p2_outside", v.p2_outside as u64),
        ("p2_displaced", v.p2_displaced as u64),
        ("tlb_rounds", v.tlb_rounds as u64),
        ("credited_s", v.tlb_credited_s as u64),
        ("credited_c", v.tlb_credited_c as u64),
        ("interfered", v.tlb_interfered as u64),
        ("off_cpu", v.tlb_off_cpu as u64),
        ("not_ready", v.tlb_not_ready as u64),
        ("not_current", v.tlb_not_current as u64),
        ("mutual_rounds", v.mutual_rounds as u64),
        ("overlapped", v.mutual_overlapped as u64),
        ("contended", v.mutual_contended),
        ("mutual_off_cpu", v.mutual_off_cpu as u64),
        ("mutual_not_current", v.mutual_not_current as u64),
        (
            "settled_after_completion",
            v.settled_after_completion as u64,
        ),
        ("settled_local_only", v.settled_local_only as u64),
    ];
    let mut out = vec![
        format!(
            "SMP3_RECORDS_VERDICT result={} reason={} at={}",
            if v.ok() { "ok" } else { "fail" },
            v.failure.unwrap_or("none"),
            v.failure_at
        ),
        format!(
            "SMP3_RECORDS_COUNTS {}",
            counts
                .iter()
                .map(|(k, n)| format!("{k}={n}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ];
    // Keyed like the grader's: one attempt per (phase, n, target CPU), sorted.
    let mut attempts = BTreeMap::new();
    for a in att.iter() {
        attempts.insert(
            (a.phase, a.n, a.target_cpu),
            format!(
                "SMP3_RECORDS_ATTEMPT phase={} n={} target_cpu={} eligible={} reason={} outcome={}",
                a.phase,
                a.n,
                a.target_cpu,
                u8::from(a.eligible),
                a.reason,
                a.outcome.name()
            ),
        );
    }
    out.extend(attempts.into_values());
    out
}

fn python_lines(file: &std::path::Path) -> (i32, Vec<String>) {
    let out = Command::new("python3")
        .arg("-I")
        .arg(PathBuf::from(repo()).join(GRADER))
        .arg("--records")
        .arg(file)
        .output()
        .expect("python3 is required for the differential test");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines = text
        .lines()
        .filter(|l| !l.starts_with("SMP3_RECORDS_FAILURE "))
        .map(String::from)
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

fn base() -> (String, Vec<Item>) {
    let mut lines = BASE.lines().filter(|l| !l.trim().is_empty());
    let roles = lines.next().expect("roles line").trim().to_string();
    assert!(
        roles.starts_with("SMP3_ROLES "),
        "base.rec starts with its roles"
    );
    let items: Vec<Item> = lines.map(parse_rec).collect();
    assert!(
        items.iter().enumerate().all(|(i, it)| it.id == i as u64),
        "base.rec is contiguous"
    );
    (roles, items)
}

#[test]
fn the_two_verifiers_agree_on_every_case_and_each_gets_its_expected_verdict() {
    let s = Scratch::new("diff");
    let (roles, items) = base();
    let all = cases();
    assert!(all.len() >= 40, "corpus too small: {}", all.len());
    let mut bad = Vec::new();
    for (n, c) in all.iter().enumerate() {
        let (case_roles, case_items) = build(c, &roles, &items);
        let text = render(&case_roles, &case_items);
        let file = s.0.join(format!("case{n}.rec"));
        std::fs::write(&file, &text).unwrap();
        let rust = rust_lines(&text);
        let (code, py) = python_lines(&file);
        if rust != py {
            let diff: Vec<String> = rust
                .iter()
                .zip(py.iter().chain(std::iter::repeat(&String::new())))
                .filter(|(a, b)| a != b)
                .take(3)
                .map(|(a, b)| format!("    rust: {a}\n    py:   {b}"))
                .collect();
            bad.push(format!(
                "{}: the verifiers disagree ({} vs {} lines)\n{}",
                c.name,
                rust.len(),
                py.len(),
                diff.join("\n")
            ));
            continue;
        }
        let verdict = &rust[0];
        let want_ok = c.expect == "ok";
        if code != if want_ok { 0 } else { 1 } {
            bad.push(format!("{}: grader exit {code} for `{verdict}`", c.name));
        }
        let matches = if want_ok {
            verdict.starts_with("SMP3_RECORDS_VERDICT result=ok ")
        } else {
            let mut e = c.expect.split_whitespace();
            assert_eq!(e.next(), Some("fail"), "case `{}` expect", c.name);
            let reason = e.next().expect("a failure reason");
            let at = e.next();
            verdict.contains(&format!(" reason={reason} "))
                && at.is_none_or(|a| verdict.ends_with(&format!(" at={a}")))
        };
        if !matches {
            bad.push(format!(
                "{}: expected `{}`, got `{verdict}`",
                c.name, c.expect
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} case(s) wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The base itself is a passing record with every phase and both repaired paths exercised: its
/// own merged publications, and at least one decision whose loaded-root contribution differs from
/// its placement one is not required of a natural boot — the corpus edits supply those.
#[test]
fn the_base_record_passes_both_verifiers() {
    let (roles, items) = base();
    let text = render(&roles, &items);
    let rust = rust_lines(&text);
    assert!(
        rust[0].starts_with("SMP3_RECORDS_VERDICT result=ok "),
        "{}",
        rust[0]
    );
    let s = Scratch::new("base");
    let file = s.0.join("base.rec");
    std::fs::write(&file, &text).unwrap();
    let (code, py) = python_lines(&file);
    assert_eq!(code, 0);
    assert_eq!(rust, py);
}

/// Both verifiers number user steps identically.
#[test]
fn the_step_tables_agree() {
    let out = Command::new("python3")
        .args(["-I", GRADER, "--steps"])
        .current_dir(repo())
        .output()
        .expect("python3");
    let py: Vec<(String, u64)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let (a, b) = l.split_once(' ').unwrap();
            (a.to_string(), b.parse().unwrap())
        })
        .collect();
    let rust: Vec<(String, u64)> = rec::STEPS
        .iter()
        .map(|(a, b)| (a.to_string(), *b))
        .collect();
    assert_eq!(rust, py);
}

// ── the producer facts both verifiers rely on, pinned at their owners ──

const RUNTIME: &str = include_str!("../src/runtime.rs");
const WITNESS: &str = include_str!("../src/arch/riscv64/smp3_witness.rs");
const IPI: &str = include_str!("../src/arch/riscv64/ipi.rs");

fn body<'a>(src: &'a str, sig: &str) -> &'a str {
    let at = src.find(sig).unwrap_or_else(|| panic!("`{sig}` missing"));
    let rest = &src[at..];
    let open = rest.find('{').unwrap();
    let mut depth = 0usize;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced `{sig}`")
}

/// The decision's inputs are the values the production owner itself consumed — handed over from
/// inside the computation, never re-read — and its result is still placement ∪ loaded-root
/// residency over the same candidates.
#[test]
fn the_shootdown_inputs_are_the_owners_own_values() {
    let owner = body(
        RUNTIME,
        "pub(crate) fn live_cpu_bitmap_for_asid_split(&self, asid: crate::kernel::vm::Asid) -> u64",
    );
    for fact in [
        "let candidates = online & !wake_only;",
        "let loaded = crate::arch::hal::address_space_loaded_bitmap(asid) & candidates;",
        "[online, wake_only, bitmap, loaded],",
        "&current,",
    ] {
        assert!(owner.contains(fact), "owner: {fact}");
    }
    assert!(
        owner
            .trim_end_matches('}')
            .trim_end()
            .ends_with("bitmap | loaded")
    );
    // The witness stores what it was handed and records it with the result, inputs first.
    let snap = body(WITNESS, "pub fn note_live_snapshot(");
    assert!(snap.contains("for (slot, v) in inp.iter().zip(inputs)"));
    let targets = body(WITNESS, "pub fn note_shoot_targets(");
    let (i, t) = (
        targets.find("Kind::ShootInputs").unwrap(),
        targets.find("Kind::ShootTargets").unwrap(),
    );
    assert!(i < t, "the inputs are recorded just before the targets");
    assert!(!targets.contains("current_tid") && !targets.contains("address_space_loaded"));
}

/// Groups are numbered by the publisher from its own `fetch_or` result, consumptions by the
/// consumer from its own swap; the record is pushed after the operation it describes.
#[test]
fn groups_and_consumptions_are_numbered_by_their_own_operations() {
    let send = body(IPI, "pub fn send_reschedule(");
    let (f, n) = (
        send.find("slot.fetch_or(bit, Ordering::AcqRel)").unwrap(),
        send.find("note_ipi_published(sender, target, hart, old & bit != 0)")
            .unwrap(),
    );
    assert!(
        f < n,
        "the publication is recorded after its fetch_or, from its result"
    );
    let take = body(IPI, "pub fn take_arrival(");
    assert!(
        take.find("clear_ssip();").unwrap() < take.find("p.swap(0, Ordering::AcqRel)").unwrap()
    );
    let publ = body(WITNESS, "fn note_publication(");
    assert!(publ.contains("!merged => g.fetch_add(1, Ordering::AcqRel) + 1"));
    let arr = body(WITNESS, "pub fn note_arrival(");
    let (a, c) = (
        arr.find("Kind::IpiArrived").unwrap(),
        arr.find("Kind::IpiConsumed").unwrap(),
    );
    assert!(
        a < c,
        "each consumption is recorded right after its arrival"
    );
    // The labelled synchronizations never touch the mailbox or the fence.
    for f in ["fn await_dispatch_gap(", "fn delay_merged_record("] {
        let b = body(WITNESS, f);
        assert!(
            !b.contains("PENDING") && !b.contains("sbi::") && !b.contains(".lock()"),
            "{f}"
        );
    }
}
