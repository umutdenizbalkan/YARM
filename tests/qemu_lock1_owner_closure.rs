// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-LOCK1 final checks — the two mailbox assumptions the LOCK1 grader's generation argument
//! relies on, pinned against the ACTUAL writes and call sites of the source tree (comments are
//! stripped first, so an explanatory sentence can never satisfy a check):
//!
//! 1. **Publication.** The per-(target, sender) generation `PUB_GEN` is written only by
//!    `lock1_witness::note_publication`, which is called only from the two enumerated mailbox
//!    publication owners (`ipi::send_reschedule`, `ipi::kick_self`) immediately before their single
//!    `fetch_or` of the sender's bit, with the owner's own sender argument. Every call of those owners
//!    reachable on RISC-V passes a sender identity that a call-graph walk traces, argument by
//!    argument through real call sites, back to the executing CPU: the trap bridge's
//!    `riscv_logical_cpu_for_trap_frame`, or the secondary hart's own claimed slot at bring-up.
//! 2. **Consumption.** The mailbox `PENDING` is private to `ipi.rs`; its only mutations are the two
//!    publishers' `fetch_or` and `take_arrival`'s `swap`; `take_arrival` is called only by the two
//!    RISC-V software-interrupt trap entries, with the trap's own CPU; and the witness's arrival hook
//!    is called only from `take_arrival`, with the sources that swap removed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every non-test source file under `src/`, comments stripped, test modules removed.
fn tree() -> BTreeMap<String, String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read_dir") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs")
                && p.file_name().is_some_and(|n| n != "tests.rs")
            {
                out.push(p);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&root, &mut files);
    files
        .into_iter()
        .map(|p| {
            let raw = std::fs::read_to_string(&p).expect("read");
            let raw = match raw.find("#[cfg(test)]\nmod tests") {
                Some(i) => raw[..i].to_string(),
                None => raw,
            };
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, strip_comments(&raw))
        })
        .collect()
}

/// Remove `//` comments (outside string literals) and `/* */` blocks.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' && !(i > 0 && b[i - 1] == b'\'') {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The comma-separated arguments of the call whose `(` ends just before `open`.
fn args_at(s: &str, open: usize) -> Vec<String> {
    let b = s.as_bytes();
    let (mut depth, mut j, mut cur, mut out) = (1i32, open, String::new(), Vec::new());
    while j < b.len() {
        let c = b[j];
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        if c == b',' && depth == 1 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c as char);
        }
        j += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

struct Site {
    file: String,
    at: usize,
    args: Vec<String>,
}

/// Every call whose text ends with `needle` (which ends in `(`), excluding definitions.
fn sites(t: &BTreeMap<String, String>, needle: &str) -> Vec<Site> {
    let mut out = Vec::new();
    for (f, s) in t {
        let mut from = 0;
        while let Some(k) = s[from..].find(needle) {
            let at = from + k;
            from = at + needle.len();
            let b = s.as_bytes();
            if at > 0 && is_ident(b[at - 1]) && is_ident(needle.as_bytes()[0]) {
                continue; // a longer identifier
            }
            if s[..at].trim_end().ends_with("fn") {
                continue; // the definition
            }
            out.push(Site {
                file: f.clone(),
                at,
                args: args_at(s, from),
            });
        }
    }
    out
}

/// (name, parameter names without `self`, body start, is_method) of the fn enclosing `at`.
fn enclosing(s: &str, at: usize) -> Option<(String, Vec<String>, usize, bool)> {
    let b = s.as_bytes();
    let mut best = None;
    let mut from = 0;
    while let Some(k) = s[from..at].find("fn ") {
        let p = from + k;
        from = p + 3;
        if p > 0 && is_ident(b[p - 1]) {
            continue;
        }
        let rest = &s[p + 3..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(paren) = rest.find('(') else {
            continue;
        };
        if rest[name.len()..paren]
            .trim_start()
            .starts_with(|c: char| c != '<')
        {
            continue;
        }
        best = Some((p, name, p + 3 + paren + 1));
    }
    let (p, name, open) = best?;
    let raw = args_at(s, open);
    let is_method = raw.first().is_some_and(|a| a.contains("self"));
    let params = raw
        .iter()
        .filter(|a| !a.contains("self"))
        .map(|a| {
            a.split(':')
                .next()
                .unwrap()
                .trim()
                .trim_start_matches("mut ")
                .to_string()
        })
        .collect();
    Some((name, params, p, is_method))
}

/// The CPU-carrying identifier an argument passes: `x`, `x.0`, `CpuId(x)`, `…::CpuId(x)`.
fn carried(arg: &str) -> Option<String> {
    let mut a = arg.trim();
    if let Some(i) = a.find("CpuId(") {
        a = a[i + "CpuId(".len()..].trim_end_matches(')');
    }
    let a = a.trim_end_matches(".0").trim();
    (!a.is_empty() && a.bytes().all(is_ident)).then(|| a.to_string())
}

/// The module path a call must name to reach a fn defined in `file` (e.g. `riscv64::smp3_witness`).
fn module_of(file: &str) -> String {
    file.trim_end_matches(".rs")
        .trim_end_matches("/mod")
        .trim_start_matches("arch/")
        .trim_start_matches("kernel/")
        .replace('/', "::")
}

const FOREIGN: [&str; 2] = ["arch/x86_64/", "arch/aarch64/"];

/// The executing-CPU roots a sender/consumer identity may originate from.
fn is_root(fn_name: &str, before: &str, tok: &str) -> bool {
    let bound = |rhs: &str| {
        before.contains(&format!("let {tok} = {rhs}"))
            || before.contains(&format!("let {tok} = crate::arch::riscv64::boot::{rhs}"))
    };
    bound("riscv_logical_cpu_for_trap_frame(")
        || bound("riscv_current_logical_cpu()")
        || (fn_name == "yarm_riscv64_secondary_boot"
            && before.contains(&format!(
                "let {tok} = crate::kernel::scheduler::CpuId(cpu_id as u8);"
            ))
            && before.contains("parked_cpu = Some(cpu_id);"))
}

/// The call sites that can reach a fn: method calls `.name(`, module-qualified `tail::name(`
/// anywhere, and bare `name(` within its own file.
fn callers(t: &BTreeMap<String, String>, home: &str, name: &str, method: bool) -> Vec<Site> {
    if method {
        return sites(t, &format!(".{name}("));
    }
    let m = module_of(home);
    let tail = m.rsplit("::").next().unwrap();
    let mut out = sites(t, &format!("{tail}::{name}("));
    // Bare calls ANYWHERE (a `use` import makes them legal in other files). Conservative: a
    // same-named fn elsewhere only adds paths that must also resolve; it can never hide one.
    let bare = sites(t, &format!("{name}("));
    out.extend(bare.into_iter().filter(|x| {
        let prev = t[&x.file].as_bytes()[x.at.saturating_sub(1)];
        prev != b':' && prev != b'.'
    }));
    let _ = home;
    out
}

/// Walk every RISC-V-reachable call site, following argument `idx` up through callers until it
/// reaches an executing-CPU root. Returns the unresolved paths (empty = closed); `log` is the trace.
fn trace(
    t: &BTreeMap<String, String>,
    all: Vec<Site>,
    what: &str,
    idx: usize,
    depth: usize,
    log: &mut Vec<String>,
) -> Vec<String> {
    let mut bad = Vec::new();
    let local: Vec<_> = all
        .iter()
        .filter(|x| !FOREIGN.iter().any(|f| x.file.starts_with(f)))
        .collect();
    if local.is_empty() {
        if all.is_empty() {
            bad.push(format!("{what}: no caller at all"));
        }
        return bad; // only foreign-architecture callers: not reachable on RISC-V
    }
    for site in local {
        let s = &t[&site.file];
        let Some((efn, params, start, method)) = enclosing(s, site.at) else {
            bad.push(format!("{}: call of {what} outside a fn", site.file));
            continue;
        };
        let arg = site.args.get(idx).cloned().unwrap_or_default();
        let line = s[..site.at].matches('\n').count() + 1;
        let Some(tok) = carried(&arg) else {
            bad.push(format!(
                "{}:{line} {efn} passes `{arg}` to {what}",
                site.file
            ));
            continue;
        };
        let before = &s[start..site.at];
        log.push(format!(
            "{}{}:{line} {efn}({arg}) -> {what}",
            "  ".repeat(depth),
            site.file
        ));
        if is_root(&efn, before, &tok) {
            log.push(format!("{}  ROOT", "  ".repeat(depth)));
            continue;
        }
        let Some(pi) = params.iter().position(|p| *p == tok) else {
            bad.push(format!(
                "{}:{line} {efn} passes `{arg}`, neither a parameter nor a root",
                site.file
            ));
            continue;
        };
        if before.contains(&format!("let {tok} =")) || before.contains(&format!("let mut {tok} ="))
        {
            bad.push(format!(
                "{}:{line} {efn} rebinds `{tok}` before the call",
                site.file
            ));
            continue;
        }
        if depth > 16 {
            bad.push(format!("{}:{line} {efn}: walk too deep", site.file));
            continue;
        }
        let up = callers(t, &site.file, &efn, method);
        bad.extend(trace(t, up, &efn, pi, depth + 1, log));
    }
    bad
}

fn fn_body<'a>(s: &'a str, head: &str) -> &'a str {
    let i = s.find(head).unwrap_or_else(|| panic!("{head} missing"));
    let open = i + s[i..].find('{').unwrap();
    let b = s.as_bytes();
    let (mut depth, mut j) = (0i32, open);
    loop {
        match b[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &s[open..=j];
                }
            }
            _ => {}
        }
        j += 1;
    }
}

const MUTATORS: [&str; 8] = [
    ".store(",
    ".swap(",
    ".fetch_or(",
    ".fetch_and(",
    ".fetch_xor(",
    ".fetch_add(",
    ".fetch_sub(",
    ".compare_exchange",
];

#[test]
fn publication_generations_are_written_only_by_the_enumerated_owners() {
    let t = tree();
    let w = &t["kernel/lock1_witness.rs"];
    // PUB_GEN: declared once, mutated only inside note_publication, read only by pub_gen.
    let uses: Vec<_> = w.match_indices("PUB_GEN").map(|(i, _)| i).collect();
    for i in &uses {
        let (efn, ..) = enclosing(w, *i).unwrap_or_default_fn();
        let line = &w[w[..*i].rfind('\n').unwrap_or(0)..*i + w[*i..].find('\n').unwrap_or(0)];
        if line.trim_start().starts_with("static PUB_GEN") {
            continue;
        }
        assert!(
            efn == "note_publication" || efn == "pub_gen",
            "PUB_GEN touched in {efn}: {line}"
        );
    }
    assert!(fn_body(w, "fn pub_gen(").matches(".load(").count() == 1);
    assert!(
        !MUTATORS
            .iter()
            .any(|m| fn_body(w, "fn pub_gen(").contains(m))
    );
    let np = fn_body(w, "pub fn note_publication(sender: u8, target: u8)");
    assert_eq!(
        np.matches(".fetch_add(").count(),
        1,
        "one generation advance"
    );
    for (f, s) in &t {
        if f != "kernel/lock1_witness.rs" {
            assert!(!s.contains("PUB_GEN"), "{f} names PUB_GEN");
        }
    }
    // note_publication: exactly the two mailbox publication owners, each with its own sender, each
    // immediately before its single fetch_or of that sender's bit.
    let calls = sites(&t, "lock1_witness::note_publication(");
    assert_eq!(calls.len(), 2, "exactly two publication hooks");
    let ipi = &t["arch/riscv64/ipi.rs"];
    for (owner, args) in [
        (
            "pub fn send_reschedule(sender: CpuId, target: CpuId)",
            ["sender.0", "target.0"],
        ),
        ("pub fn kick_self(cpu: CpuId)", ["cpu.0", "cpu.0"]),
    ] {
        let body = fn_body(ipi, owner);
        let hook = body.find("lock1_witness::note_publication(").expect(owner);
        let lo = body.as_ptr() as usize - ipi.as_ptr() as usize;
        let site = calls
            .iter()
            .find(|c| c.file == "arch/riscv64/ipi.rs" && (lo..lo + body.len()).contains(&c.at))
            .expect("hook site inside the owner");
        assert_eq!(
            site.args, args,
            "{owner}: the hook names the owner's own sender/target"
        );
        assert_eq!(
            body.matches(".fetch_or(").count(),
            1,
            "{owner}: one publication"
        );
        let fo = body.find("slot.fetch_or(bit").expect("publication");
        assert!(
            hook < fo,
            "{owner}: generation advanced before the bit is published"
        );
        let between = &body[hook..fo];
        assert!(
            !between.contains("return") && !between.contains('?'),
            "{owner}: no exit between"
        );
        assert!(
            body[..fo].contains("let bit = 1u64 << ("),
            "{owner}: bit derived from the sender"
        );
    }
    assert!(
        fn_body(ipi, "pub fn send_reschedule(")
            .contains("let bit = 1u64 << (sender.0 as u64 & 63);")
    );
    assert!(fn_body(ipi, "pub fn kick_self(").contains("let bit = 1u64 << (cpu.0 as u64 & 63);"));
}

trait OrDefaultFn {
    fn unwrap_or_default_fn(self) -> (String, Vec<String>, usize, bool);
}
impl OrDefaultFn for Option<(String, Vec<String>, usize, bool)> {
    fn unwrap_or_default_fn(self) -> (String, Vec<String>, usize, bool) {
        self.unwrap_or((String::new(), Vec::new(), 0, false))
    }
}

#[test]
fn every_sender_identity_traces_to_the_executing_cpu() {
    let t = tree();
    let mut log = Vec::new();
    let mut bad = trace(
        &t,
        sites(&t, "riscv64::ipi::send_reschedule("),
        "send_reschedule",
        0,
        0,
        &mut log,
    );
    bad.extend(trace(
        &t,
        sites(&t, "riscv64::ipi::kick_self("),
        "kick_self",
        0,
        0,
        &mut log,
    ));
    // The witness's own publication is a call of the production owner from the witness module.
    let w = &t["kernel/lock1_witness.rs"];
    assert!(fn_body(w, "fn publish_ipi_to_holder(").contains(
        "crate::arch::riscv64::ipi::send_reschedule(CpuId(waiter_cpu), CpuId(holder_cpu))"
    ));
    if std::env::var_os("LOCK1_TRACE").is_some() {
        eprintln!("{}", log.join("\n"));
    }
    let roots = log.iter().filter(|l| l.trim() == "ROOT").count();
    assert!(
        roots >= 4,
        "the walk reached the executing-CPU roots:\n{}",
        log.join("\n")
    );
    assert!(
        bad.is_empty(),
        "unresolved sender identities:\n{}\n--- trace ---\n{}",
        bad.join("\n"),
        log.join("\n")
    );
}

#[test]
fn the_mailbox_is_consumed_only_by_the_enumerated_consumer() {
    let t = tree();
    let ipi = &t["arch/riscv64/ipi.rs"];
    assert!(
        ipi.contains("\nstatic PENDING: [AtomicU64; MAX_CPUS]"),
        "PENDING stays module-private"
    );
    for (f, s) in &t {
        if f != "arch/riscv64/ipi.rs" {
            assert!(
                !s.contains("ipi::PENDING"),
                "{f} reaches the mailbox directly"
            );
        }
    }
    // Every PENDING use sits in one of four fns; the only mutations are the publishers' fetch_or
    // and take_arrival's swap.
    let allowed = ["pending", "send_reschedule", "kick_self", "take_arrival"];
    for (i, _) in ipi.match_indices("PENDING") {
        let line_start = ipi[..i].rfind('\n').unwrap_or(0);
        if ipi[line_start..i].contains("static ") {
            continue;
        }
        let (efn, ..) = enclosing(ipi, i).unwrap_or_default_fn();
        assert!(allowed.contains(&efn.as_str()), "PENDING used in {efn}");
    }
    let count = |head: &str, m: &str| fn_body(ipi, head).matches(m).count();
    for m in MUTATORS {
        let want = |f: &str| match (f, m) {
            ("send_reschedule", ".fetch_or(") | ("kick_self", ".fetch_or(") => 1,
            ("take_arrival", ".swap(") => 1,
            _ => 0,
        };
        assert_eq!(count("pub fn pending(", m), 0, "pending is an observer");
        for f in ["send_reschedule", "kick_self"] {
            let body = fn_body(ipi, &format!("pub fn {f}("));
            // SENT counters go through `bump`; only the mailbox slot is mutated in-body.
            assert_eq!(body.matches(m).count(), want(f), "{f}: {m}");
        }
        let ta = fn_body(ipi, "pub fn take_arrival(");
        let extra = if m == ".fetch_add(" { 1 } else { 0 }; // the TAKEN source counter
        assert_eq!(
            ta.matches(m).count(),
            want("take_arrival") + extra,
            "take_arrival: {m}"
        );
    }
    assert!(
        fn_body(ipi, "pub fn take_arrival(")
            .contains(".map_or(0, |p| p.swap(0, Ordering::AcqRel))")
    );
    assert!(fn_body(ipi, "pub fn take_arrival(").contains("row[3].fetch_add(sources.count_ones()"));

    // take_arrival: exactly the two RISC-V software-interrupt trap entries, with the trap's CPU.
    let consumers = sites(&t, "riscv64::ipi::take_arrival(");
    assert_eq!(consumers.len(), 2, "two consumption sites");
    let mut log = Vec::new();
    let bad = trace(
        &t,
        sites(&t, "riscv64::ipi::take_arrival("),
        "take_arrival",
        0,
        0,
        &mut log,
    );
    assert!(
        bad.is_empty(),
        "consumer CPU unresolved:\n{}\n{}",
        bad.join("\n"),
        log.join("\n")
    );
    let boot = &t["arch/riscv64/boot.rs"];
    for c in &consumers {
        assert_eq!(c.file, "arch/riscv64/boot.rs");
        let (efn, ..) = enclosing(boot, c.at).unwrap();
        let ctx = &boot[c.at.saturating_sub(400)..c.at];
        assert!(
            efn == "riscv_s_mode_software_trap"
                || ctx.contains("if crate::arch::riscv64::ipi::is_software_interrupt(scause) {"),
            "take_arrival in {efn} is not a software-interrupt trap entry"
        );
    }

    // The witness's arrival hook: only inside take_arrival, after the swap, with its sources.
    let hooks = sites(&t, "lock1_witness::note_arrival(");
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].file, "arch/riscv64/ipi.rs");
    assert_eq!(hooks[0].args, ["cpu.0", "sources"]);
    let (efn, ..) = enclosing(ipi, hooks[0].at).unwrap();
    assert_eq!(efn, "take_arrival");
    let ta = fn_body(ipi, "pub fn take_arrival(");
    assert!(ta.find(".swap(0").unwrap() < ta.find("lock1_witness::note_arrival(").unwrap());
}
