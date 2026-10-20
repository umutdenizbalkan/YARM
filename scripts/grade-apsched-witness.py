#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
#
# AP-SCHED1/AP-SCHED2 — grade the RISC-V production SMP-scheduling witness, for every expected CPU.
#
# Usage: grade-apsched-witness.py [--arch riscv64|aarch64|x86_64] [--topo T] --expect-cpus N BOOT_LOG
#        grade-apsched-witness.py [--arch riscv64|aarch64] --topology-only N BOOT_LOG
#        grade-apsched-witness.py [--arch riscv64|aarch64|x86_64] --self-test
#
# AP-SCHED4: the x86_64 port. Its CPUs come from the firmware's MADT and are bound by APIC ID, so
# the run is graded against an INDEPENDENT derivation of the APIC IDs QEMU gives `-smp N[,T]` (T =
# the `--topo` topology, e.g. `sockets=2,cores=3,threads=1`, which makes them sparse) and of the
# plan's own rule (the boot CPU is logical 0; the rest ascending by APIC ID; at most 8 logical CPUs,
# the others named `beyond_production_cpu_ceiling` and never started) — on every run, not only
# beyond the ceiling. Each admission record carries the APIC ID the CPU read from its own LAPIC,
# graded separately from the logical id. And every expected CPU must show at least one idle wake
# delivered BY ITS IPI: x86_64 has no record of an IPI that lost to the CPU's timer, so no wake is
# ever credited to a timer race there.
#
# AP-SCHED3: one grader for both ports' witnesses (default riscv64). The record format, the
# obligations and the thresholds are the same; a port contributes only its console marker names,
# its register-index names and — RISC-V only — the beyond-the-ceiling topology section (QEMU virt
# with GICv2 has at most 8 AArch64 CPUs, so the AArch64 run is never larger than the ceiling).
#
# N is the boot's hart count (`-smp N`). Production admits at most PRODUCTION_CPU_CEILING (8)
# logical CPUs, the boot CPU included, so the expected logical CPUs are 0..min(N, 8)-1. Beyond 8
# harts, the excluded harts are graded SEPARATELY (the topology section, `APSW_TOPOLOGY_SEAL`):
# re-derived here from the boot hart the kernel reports, by the policy's own rule, and each must be
# named with its reason and never started. `--topology-only` grades that section alone (a short
# boot that stops once P has seen every expected CPU admitted).
#
# The witness keeps its records in the kernel and dumps them at P's last step, every line twice
# with a CRC-32 of its body. Only lines whose CRC verifies are evidence (the harts' console output
# interleaves, and a torn line is not a record); both copies of one stamp must agree. The grade is
# by causal stamp, never by console position.
#
# The EXPECTED secondaries are 1..N-1, from the boot's own `-smp N` — never from the kernel's
# report. The kernel's release record must name exactly that set, and every obligation below must
# hold on EVERY expected secondary: a CPU that announced readiness but ran nothing fails.
#
# Obligations (each failure names the one it broke):
#   admission   the boot hart released exactly the expected set, each expected CPU took the
#               PRODUCTION admission (its record), and the diagnostic pinned path never ran;
#   placement   P ran on CPU 0; spinners started on CPU 0 and on every expected secondary; one
#               deadline thread, one sleeper and one IPC thread ran on each expected secondary;
#               the churn children, over all rounds, ran on every expected secondary;
#   preemption  every spinner on a secondary was preempted there by that CPU's own timer, each
#               secondary took at least PRE_MIN such preemptions, and every spinner finished on the
#               CPU it started on (no migration) with all 28 patterned registers intact;
#   time        each deadline receive timed out (9) on the CPU it armed on, after at least its
#               timeout in scheduler ticks; the tick advanced by exactly CPU 0's timer interrupts
#               over its window; and its own CPU took interrupts of its own — no CPU sped time up;
#   idle wake   every (sleeper, round): its CPU idle (an idle arrival, not left since) when P stored
#               its word; then an IPI taken at that CPU's idle boundary; then that CPU leaving idle
#               for the sleeper BY that IPI; then the acknowledgement there; one waiter woken;
#   ipc         every (thread, round): the ping arrived intact on CPU 0 from that thread, and the
#               pong intact on the thread's CPU, in order;
#   churn       every child: ran on a secondary; that CPU then left THAT child's root before the
#               reap; and only then reaped, with its address space destroyed and none quarantined;
#   progress    P reached its end with no failure step; the dump is complete and did not overflow.
import re
import sys
import zlib

PRE0_MIN = 3
PRE_MIN = 4
# The AArch64 program's spinner slots (its top-up's ceiling).
SPIN_SLOTS = 24


# Idle wakes whose IPI arrived but lost the race to the CPU's own timer (filled by `grade`).
RACED = []


def mask_of(cpus):
    m = 0
    for c in cpus:
        m |= 1 << c
    return m


WAKE_ROUNDS = 3
IPC_ROUNDS = 4
FORK_ROUNDS = 4
WITNESS_CPUS = 8
PRODUCTION_CPU_CEILING = 8
HANDOFF_SLOTS = 16
TIMED_OUT = 9
FAIL_STEPS = {
    "P_NO_AP", "P_PRE_TIMEOUT", "P_A_TIMEOUT", "P_B_TIMEOUT", "P_NO_IDLE", "P_NO_ACK",
    "P_PING_RAW", "P_C_TIMEOUT", "P_REAP_TIMEOUT", "P_SPAWN_FAIL", "P_FORK_FAIL", "EXIT_REFUSED",
    "P_W_TIMEOUT", "?",
}
PORTS = {
    "riscv64": {
        "forbidden": [
            "panicked at", "KERNEL PANIC", "RISCV_SMP3_SECONDARY_ADMITTED", "RISCV_AP_SCHED_ADMISSION_REFUSED",
            "RISCV_SMP3_SECONDARY_UNADMITTED", "TIMER_SPLIT_CURRENT_NOT_RUNNING",
            "VM_ASPACE_TEARDOWN_QUARANTINED", "RISCV_AP_QUIESCENT", "RISCV_HART_EXCLUDED",
            "RISCV_LOGICAL_CPU_MISMATCH", "RISCV_SECONDARY_START_FAILED", "RISCV_SECONDARY_NOT_STARTABLE",
        ],
        "state": "RISCV_AP_STATE",
        "admitted_line": "RISCV_AP_SCHED_ADMITTED hart=1 cpu=1 mode=production",
        "pinned": "RISCV_SMP3_SECONDARY_ADMITTED hart=1 cpu=1",
        "quiescent": "RISCV_AP_QUIESCENT cpu=2 reason=x",
        "max_harts": None,
        "stack": False,
        "idle_timer_parked": False,
    },
    "aarch64": {
        "forbidden": [
            "panicked at", "KERNEL PANIC", "AARCH64_SMP2_AP_ADMITTED", "AARCH64_AP_SCHED_ADMISSION_REFUSED",
            "AARCH64_AP_UNADMITTED", "AARCH64_AP_IDENTITY_MISMATCH", "TIMER_SPLIT_CURRENT_NOT_RUNNING",
            "VM_ASPACE_TEARDOWN_QUARANTINED", "AARCH64_CPU_EXCLUDED", "reason=psci_cpu_on_failed",
            "_DISPATCH_FATAL",
        ],
        "state": "AARCH64_AP_STATE",
        "admitted_line": "AARCH64_AP_SCHED_ADMITTED cpu=1 mpidr=0x80000001 mode=production",
        "pinned": "AARCH64_SMP2_AP_ADMITTED cpu=1 wake_only=0 balance_excluded=1",
        "quiescent": "AARCH64_CPU_EXCLUDED affinity=0x2 reason=x dispatch=0",
        # QEMU virt with GICv2: at most 8 CPU interfaces, so never more CPUs than the ceiling.
        "max_harts": 8,
        # Each secondary's dispatch stack is painted before it starts; the dump reports its depth.
        "stack": True,
        # An idle secondary parks its own timer behind a 100-period backstop (`smp`), so a
        # deadline window spent idle can legitimately see none of its CPU's ticks.
        "idle_timer_parked": True,
    },
}
PORTS["x86_64"] = {
    "forbidden": [
        "panicked at", "KERNEL PANIC", "PANIC src/", "X86_AP_UNADMITTED", "X86_AP_SCHED_ADMISSION_REFUSED",
        "X86_EXECUTING_CPU_UNMAPPED", "X86_APIC_CPU_BIND_REFUSED", "YARM_SMP_AP_UNPLANNED",
        "TIMER_SPLIT_CURRENT_NOT_RUNNING", "VM_ASPACE_TEARDOWN_QUARANTINED", "unknown trap event",
        "X86_AP_WORKLOAD_PLACEMENT_READY", "X86_AP_SAVED_DISPATCH_OK", "X86_TLB_SHOOTDOWN_FAIL",
        "DOUBLE FAULT", "YARM_SMP_AP_STACK_OVERLAPS_IMAGE",
    ],
    "state": "X86_AP_STATE",
    "admitted_line": "X86_AP_SCHED_ADMITTED cpu=1 apic_id=1 mode=production",
    "pinned": "X86_AP_WORKLOAD_PLACEMENT_READY cpu=1 base_tid=20000 count=4",
    "quiescent": "X86_AP_UNADMITTED cpu=2 reason=x admitted=0 dispatching=0 result=fail",
    "max_harts": None,
    "stack": True,
    "idle_timer_parked": False,
    # Every expected CPU must show an idle wake delivered by its IPI (never a timer-won one).
    "ipi_wake_per_cpu": True,
    # The admission record carries the APIC ID (v0) under v1 = 1.
    "hw_identity": True,
}
for _p in ("riscv64", "aarch64"):
    PORTS[_p]["ipi_wake_per_cpu"] = False
    PORTS[_p]["hw_identity"] = False
ARCH = "riscv64"
# x86_64: the `-smp` topology beside the count (`--topo`), e.g. "sockets=2,cores=3,threads=1".
TOPO = ""


def P():
    return PORTS[ARCH]


def reg_name(idx):
    """The register a spinner's mismatch index names, per port."""
    if ARCH == "x86_64":
        gprs = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi",
                "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"]
        named = {31: "rsp", 32: "rflags", 33: "fcw", 34: "mxcsr", 35: "tls(fs:0)"}
        if idx in named:
            return named[idx]
        if idx < 16:
            return gprs[idx]
        if 40 <= idx < 48:
            return f"st{idx - 40}"
        if 64 <= idx < 80:
            return f"xmm{idx - 64}.lo"
        if 96 <= idx < 112:
            return f"xmm{idx - 96}.hi"
        return f"index{idx}"
    if ARCH == "aarch64":
        named = {31: "sp", 32: "nzcv", 33: "fpcr", 34: "fpsr", 35: "tpidr_el0"}
        if idx in named:
            return named[idx]
        if 64 <= idx < 96:
            return f"q{idx - 64}.lo"
        if 96 <= idx < 128:
            return f"q{idx - 96}.hi"
    return f"x{idx}"

LINE = re.compile(r"(APSW_(?:R|DUMP_END|STACK) [^\n]*?) crc=([0-9a-f]{8})\s*$")


def parse_stack(text):
    """The verified per-CPU dispatch-stack depths (AArch64), or None: (reserved, [used per CPU])."""
    for raw in text.split("\n"):
        m = LINE.search(raw)
        if not m or not m.group(1).startswith("APSW_STACK"):
            continue
        body = m.group(1)
        if zlib.crc32(body.encode()) != int(m.group(2), 16):
            continue
        f = dict(kv.split("=", 1) for kv in body.split()[1:])
        return int(f["reserved"], 16), [int(u, 16) for u in f["used"].split(",")]
    return None


def parse(text):
    """The verified records by stamp, and the verified dump trailer (or None)."""
    recs, end, conflicts = {}, None, []
    for raw in text.split("\n"):
        m = LINE.search(raw)
        if not m:
            continue
        body, crc = m.group(1), int(m.group(2), 16)
        if not body.startswith("APSW_") or zlib.crc32(body.encode()) != crc:
            continue
        fields = dict(kv.split("=", 1) for kv in body.split()[1:] if "=" in kv)
        if body.startswith("APSW_DUMP_END"):
            end = fields
            continue
        if body.startswith("APSW_STACK"):
            continue
        seq = int(fields["seq"])
        if seq in recs and recs[seq] != fields:
            conflicts.append(seq)
        recs[seq] = fields
    return recs, end, conflicts


def policy_plan(nharts, boot_hart):
    """The RISC-V admission policy (`src/arch/riscv_hart_admission.rs`), re-derived: hart -> logical
    CPU, or the exclusion marker. Ascending hart order; a hart beyond the handoff table is never
    started; the ceiling counts logical CPUs, the boot CPU included."""
    plan, nxt = {boot_hart: 0}, 1
    for h in range(nharts):
        if h == boot_hart:
            continue
        if h >= HANDOFF_SLOTS:
            plan[h] = "beyond_hsm_handoff_table"
        elif nxt >= PRODUCTION_CPU_CEILING:
            plan[h] = "beyond_production_cpu_ceiling"
        else:
            plan[h], nxt = nxt, nxt + 1
    return plan


def grade_topology(text, nharts):
    """Admission and exclusion at the boundary, graded apart from the work: exactly the policy's
    logical CPUs admitted and dispatching, every other hart named with its reason, never started."""
    fails = []
    m = re.search(r"RISCV_LOGICAL_CPUS present=0x([0-9a-f]+) count=(\d+) excluded_harts=0x([0-9a-f]+) "
                  r"hart_bitmap=0x([0-9a-f]+) boot_hart=(\d+)", text)
    if not m:
        return ["topology: no RISCV_LOGICAL_CPUS record"], None
    present, count, excl, harts, boot = (int(m.group(1), 16), int(m.group(2)), int(m.group(3), 16),
                                          int(m.group(4), 16), int(m.group(5)))
    plan = policy_plan(nharts, boot)
    want_cpus = [c for c in plan.values() if isinstance(c, int)]
    want_present = sum(1 << c for c in want_cpus)
    want_excl = {h: r for h, r in plan.items() if isinstance(r, str)}
    if harts != (1 << nharts) - 1:
        fails.append(f"topology: hart bitmap 0x{harts:x}, the boot has {nharts} harts")
    if present != want_present or count != len(want_cpus):
        fails.append(f"topology: logical CPUs 0x{present:x} ({count}), the policy gives 0x{want_present:x}")
    if len(want_cpus) > PRODUCTION_CPU_CEILING:
        fails.append(f"topology: {len(want_cpus)} logical CPUs exceed the ceiling")
    if excl != sum(1 << h for h in want_excl):
        fails.append(f"topology: excluded harts 0x{excl:x}, the policy excludes 0x{sum(1 << h for h in want_excl):x}")
    for h, why in sorted(want_excl.items()):
        named = re.findall(rf"RISCV_HART_EXCLUDED hart={h} reason=(\w+) dispatch=0", text)
        if named != [why]:
            fails.append(f"topology: hart {h} must be excluded once as {why}, named {named}")
        if why == "beyond_production_cpu_ceiling" and not re.search(
                rf"RISCV_SECONDARY_CPU_ID_REJECTED hart={h} slot={h} reason={why} started=0", text):
            fails.append(f"topology: hart {h}'s claim was not refused at the ceiling")
        if re.search(rf"RISCV_SECONDARY_HART_PARK hart={h}\b", text):
            fails.append(f"topology: excluded hart {h} was started")
    for h, c in sorted(plan.items()):
        if isinstance(c, int) and c and not re.search(
                rf"RISCV_AP_SCHED_ADMITTED hart={h} cpu={c} mode=production", text):
            fails.append(f"topology: hart {h} was not admitted as logical CPU {c}")
    rel = re.search(r"RISCV_AP_RELEASE_PLAN trap_ready=0x[0-9a-f]+ dispatch_capable=0x[0-9a-f]+ released=0x([0-9a-f]+)", text)
    want_rel = want_present & ~1
    if not rel or int(rel.group(1), 16) != want_rel:
        fails.append(f"topology: released {rel.group(1) if rel else None}, the policy releases 0x{want_rel:x}")
    for c in range(1, 64):
        if want_present >> c & 1 and not re.search(
                rf"RISCV_AP_STATE cpu={c} discovered=1 initialized=1 irq_ready=1 admitted=1 dispatching=1 reason=none", text):
            fails.append(f"topology: CPU {c} never reported dispatching")
    summary = (f"harts={nharts} boot_hart={boot} logical=0x{present:x} count={count} "
               f"excluded={','.join(f'{h}:{r}' for h, r in sorted(want_excl.items())) or 'none'}")
    return fails, summary


def _clog2(n):
    b = 0
    while (1 << b) < n:
        b += 1
    return b


def qemu_x86_apic_ids(n, topo):
    """The APIC IDs QEMU gives `-smp n[,topo]`, by CPU index (index 0 is the boot CPU): each level's
    field is the power-of-two width of its count. Without a topology every CPU is its own core of
    one socket (8.x machine types prefer cores), which numbers them 0..n-1 either way."""
    t = dict(kv.split("=", 1) for kv in topo.split(",") if "=" in kv) if topo else {}
    threads, cores, dies = int(t.get("threads", 1)), int(t.get("cores", 0) or 0), int(t.get("dies", 1))
    sockets = int(t.get("sockets", 0) or 0)
    if not cores:
        cores = max(1, n // (max(sockets, 1) * dies * threads))
    tb, cb, db = _clog2(threads), _clog2(cores), _clog2(dies)
    ids = []
    for i in range(n):
        thread, core = i % threads, (i // threads) % cores
        die, socket = (i // (threads * cores)) % dies, i // (threads * cores * dies)
        ids.append((socket << (db + cb + tb)) | (die << (cb + tb)) | (core << tb) | thread)
    return ids


def x86_plan(n, topo):
    """The x86 admission rule (`src/arch/x86_cpu_admission.rs`), re-derived: APIC ID -> logical CPU,
    or the exclusion marker. The boot CPU is logical 0; the rest ascending by APIC ID; the ceiling
    counts logical CPUs, the boot CPU included."""
    ids = qemu_x86_apic_ids(n, topo)
    plan, nxt = {ids[0]: 0}, 1
    for a in sorted(ids[1:]):
        if nxt >= PRODUCTION_CPU_CEILING:
            plan[a] = "beyond_production_cpu_ceiling"
        else:
            plan[a], nxt = nxt, nxt + 1
    return plan, ids[0]


def grade_topology_x86(text, n, topo):
    """Discovery, identity, admission and exclusion, against the independent derivation: exactly
    the plan's logical CPUs bound, admitted under their own APIC ID and dispatching; every other
    listed processor named with its reason and never started."""
    fails = []
    plan, boot = x86_plan(n, topo)
    want = {a: c for a, c in plan.items() if isinstance(c, int)}
    excl = {a: r for a, r in plan.items() if isinstance(r, str)}
    want_present = sum(1 << c for c in want.values())
    m = re.search(r"X86_CPU_DISCOVERY source=(\w+) listed=(\d+) beyond_table=(\d+) boot_apic_id=(\d+) boot_listed=(\d)", text)
    if not m:
        return ["topology: no X86_CPU_DISCOVERY record"], None
    if m.group(1) != "madt" or int(m.group(2)) != n or int(m.group(4)) != boot or m.group(5) != "1":
        fails.append(f"topology: discovery {m.group(0)!r}, the boot has {n} processors, boot APIC {boot}")
    planned = {(int(a), int(c)) for a, c in re.findall(r"X86_CPU_PLANNED apic_id=(\d+) cpu=(\d+)", text)}
    if planned != set(want.items()):
        fails.append(f"topology: planned {sorted(planned)}, the rule plans {sorted(want.items())}")
    lc = re.search(r"X86_LOGICAL_CPUS count=(\d+) bitmap=0x([0-9a-f]+)", text)
    if not lc or int(lc.group(2), 16) != want_present or int(lc.group(1)) != len(want):
        fails.append(f"topology: logical CPUs {lc.group(0) if lc else None}, the rule gives 0x{want_present:x}")
    for a, why in sorted(excl.items()):
        named = re.findall(rf"X86_CPU_EXCLUDED apic_id={a} reason=(\w+) started=0", text)
        if named != [why]:
            fails.append(f"topology: APIC {a} must be excluded once as {why}, named {named}")
        if re.search(rf"X86_APIC_CPU_BOUND apic_id={a} ", text) or re.search(rf"X86_AP_LAPIC_OK cpu=\d+ apic_id={a}\b", text):
            fails.append(f"topology: excluded APIC {a} was bound or started")
    for a, c in sorted(want.items()):
        if not c:
            continue
        if not re.search(rf"X86_APIC_CPU_BOUND apic_id={a} cpu={c} new=1 result=ok", text):
            fails.append(f"topology: APIC {a} was not bound to logical CPU {c}")
        if not re.search(rf"X86_AP_SCHED_ADMITTED cpu={c} apic_id={a} mode=production", text):
            fails.append(f"topology: logical CPU {c} was not admitted at APIC {a}")
        if not re.search(rf"X86_AP_STATE cpu={c} discovered=1 initialized=1 irq_ready=1 admitted=1 dispatching=1 reason=none", text):
            fails.append(f"topology: CPU {c} never reported dispatching")
    rel = re.search(r"X86_AP_RELEASE_PLAN planned=0x([0-9a-f]+) released=0x([0-9a-f]+) knob=(\d)", text)
    want_rel = want_present & ~1
    if not rel or int(rel.group(1), 16) != want_rel or int(rel.group(2), 16) != want_rel or rel.group(3) != "0":
        fails.append(f"topology: release plan {rel.group(0) if rel else None}, the rule releases 0x{want_rel:x} with no knob")
    summary = (f"cpus={n} topo={topo or 'default'} boot_apic={boot} "
               f"apic_to_cpu={','.join(f'{a}:{c}' for a, c in sorted(want.items()))} "
               f"excluded={','.join(f'{a}:{r}' for a, r in sorted(excl.items())) or 'none'}")
    return fails, summary


def grade(text, ncpus):
    fails = []
    nharts = ncpus
    if P()["max_harts"] is not None and nharts > P()["max_harts"]:
        return [f"admission: this port's supported configuration has at most {P()['max_harts']} CPUs"]
    if ARCH == "x86_64":
        # Every run: the firmware plan and the identities, against the independent derivation.
        tfails, _ = grade_topology_x86(text, nharts, TOPO)
        fails += tfails
        ncpus = min(nharts, PRODUCTION_CPU_CEILING)
    elif nharts > PRODUCTION_CPU_CEILING:
        # Beyond the ceiling the work is owed by the 8 admitted CPUs; the exclusions are graded
        # in the topology section, not as console failures here.
        tfails, _ = grade_topology(text, nharts)
        fails += tfails
        ncpus = PRODUCTION_CPU_CEILING
        text = re.sub(r"RISCV_HART_EXCLUDED hart=\d+ reason=\w+ dispatch=0", "", text)
    expected = set(range(1, ncpus))
    if not expected:
        return ["admission: the witness needs at least one secondary (-smp >= 2)"]
    if ncpus > WITNESS_CPUS:
        return [f"admission: the witness's mailbox and worker-stack layout covers at most {WITNESS_CPUS} CPUs"]
    recs, end, conflicts = parse(text)
    if conflicts:
        fails.append(f"progress: two verified copies disagree for stamps {conflicts[:5]}")
    if P()["stack"] and end is not None:
        st = parse_stack(text)
        if st is None:
            fails.append("stack: no verified APSW_STACK record")
        else:
            reserved, used = st
            for c in sorted(expected):
                u = used[c] if c < len(used) else 0
                if u == 0:
                    fails.append(f"stack: CPU {c}'s dispatch stack was never used (not painted, or never run)")
                elif u >= reserved:
                    fails.append(f"stack: CPU {c} used all {reserved:#x} bytes of its dispatch stack")
    if end is None:
        # Without the record, the console still names any CPU that was refused admission.
        named = [f"admission: CPU {m.group(1)} refused ({m.group(2)})"
                 for m in re.finditer(P()["state"] + r" cpu=(\d+) [^\n]*admitted=0 [^\n]*reason=(\w+)", text)]
        return [f"progress: no verified dump trailer (P never reached its end)"] + named + fails
    count = int(end["count"])
    if end.get("overflow") != "0":
        fails.append("progress: the record overflowed")
    missing = [s for s in range(1, count + 1) if s not in recs]
    if missing:
        fails.append(f"progress: no verified copy of stamps {missing[:8]}")
    R = [dict(r, seq=int(r["seq"]), cpu=int(r["cpu"]), tid=int(r["tid"]),
              v=[int(r[f"v{i}"]) for i in range(5)]) for _, r in sorted(recs.items())]

    def user(step):
        return [r for r in R if r["kind"] == "USER" and r["step"] == step]

    def kind(k, cpu=None):
        return [r for r in R if r["kind"] == k and (cpu is None or r["cpu"] == cpu)]

    for bad in P()["forbidden"]:
        if bad in text:
            fails.append(f"forbidden console marker: {bad}")
    # The kernel publishes into the mailbox frame it captured at provisioning: a copy-on-write
    # split of P's mailbox page would leave every later observation in a frame P no longer maps.
    prov = re.search(r"APSW_PROVISIONED p_tid=\d+ asid=(\d+)", text)
    if prov and re.search(rf"VM_COW_SPLIT_COMMITTED [^\n]*asid={prov.group(1)} va=0x20070000\b", text):
        fails.append("mailbox: P's mailbox page was split copy-on-write")

    # admission — the expected set is the boot's, and the kernel must agree with it
    rel = kind("RELEASE")
    want_mask = sum(1 << c for c in expected)
    if len(rel) != 1 or rel[0]["v"][0] != want_mask:
        got = [hex(r["v"][0]) for r in rel]
        fails.append(f"admission: released {got}, expected {hex(want_mask)} for -smp {ncpus}")
    admitted = {r["cpu"] for r in kind("AP_ADMITTED")}
    for c in sorted(expected - admitted):
        fails.append(f"admission: CPU {c} never took the production admission")
    # The hardware identity each CPU was admitted under, apart from its logical id.
    if P()["hw_identity"]:
        plan, _ = x86_plan(nharts, TOPO)
        apic_of = {c: a for a, c in plan.items() if isinstance(c, int)}
        for r in kind("AP_ADMITTED"):
            if r["v"][1] != 1 or r["v"][0] != apic_of.get(r["cpu"]):
                fails.append(f"identity: CPU {r['cpu']} admitted at APIC {r['v'][0]} (recorded={r['v'][1]}), "
                             f"the plan gives APIC {apic_of.get(r['cpu'])}")

    # progress
    if not user("P_END"):
        fails.append("progress: P never reached its end")
    for r in R:
        if r["kind"] == "USER" and r["step"] in FAIL_STEPS:
            fails.append(f"progress: failure step {r['step']} (stamp {r['seq']})")
    # Phase A's clock deadline (P's waits are bounded by CPU 0's clock, which no control breaks):
    # the record names the CPUs that fell short, so the failure is per CPU, not a stall.
    for r in user("P_W_TIMEOUT"):
        fails.append(f"placement: phase A's deadline passed with {r['v'][0]} of {r['v'][1]} spinners running")
    for r in user("P_NO_IDLE"):
        for c in range(64):
            if r["v"][0] >> c & 1:
                fails.append(f"idle: CPU {c} never reached its idle wait when P needed every secondary idle")
    for r in user("P_PRE_TIMEOUT"):
        short, never = r["v"][0], r["v"][1]
        for c in range(64):
            if never >> c & 1:
                fails.append(f"preemption: phase A's deadline passed and CPU {c} had taken no timer preemption at all")
            elif short >> c & 1:
                fails.append(f"preemption: phase A's deadline passed with CPU {c} below its minimum of timer preemptions")
    p_start = user("P_START")
    if not p_start or p_start[0]["cpu"] != 0:
        fails.append("placement: P did not start on CPU 0")

    # placement + preemption + registers
    starts = user("W_START")
    want_spinners = 2 * ncpus
    # AArch64 P tops its spinners up, through the same placement, until one has started on CPU 0
    # and on every expected secondary (P_W_COVER: v0 = spinners placed, v1 = CPUs covered).
    cover = user("P_W_COVER")
    if len(cover) > 1:
        fails.append(f"placement: {len(cover)} P_W_COVER records")
    if cover:
        placed, covered = cover[0]["v"][0], cover[0]["v"][1]
        if not want_spinners <= placed <= SPIN_SLOTS:
            fails.append(f"placement: P placed {placed} spinners, outside {want_spinners}..{SPIN_SLOTS}")
        if placed > want_spinners and placed < SPIN_SLOTS and covered & mask_of(expected | {0}) != mask_of(expected | {0}):
            fails.append(f"placement: P stopped its top-up at {placed} spinners with CPUs {hex(covered)} covered")
        want_spinners = placed
    if len(starts) != want_spinners:
        fails.append(f"placement: {len(starts)} spinners started, expected {want_spinners}")
    cpus = {r["cpu"] for r in starts}
    for c in sorted((expected | {0}) - cpus):
        fails.append(f"placement: no spinner ran on CPU {c}")
    spin_tids = {r["tid"] for r in starts}
    pre = [r for r in kind("PREEMPT") if r["tid"] in spin_tids]
    for c in sorted(expected):
        n = len([r for r in pre if r["cpu"] == c])
        if n < PRE_MIN:
            fails.append(f"preemption: {n} timer preemptions of spinners on CPU {c}, need {PRE_MIN}")
    for s in starts:
        if s["cpu"] != 0 and not [r for r in pre if r["tid"] == s["tid"] and r["cpu"] == s["cpu"]]:
            fails.append(f"preemption: spinner {s['tid']} on CPU {s['cpu']} was never preempted by its timer")
        done = [r for r in user("W_DONE") if r["tid"] == s["tid"]]
        if len(done) != 1:
            fails.append(f"preemption: spinner {s['tid']} reported {len(done)} completions")
            continue
        d = done[0]
        if d["v"][0] != 0:
            fails.append(f"registers: spinner {s['tid']} found {d['v'][0]} corrupted registers (last {reg_name(d['v'][1])})")
        if d["cpu"] != s["cpu"]:
            fails.append(f"placement: spinner {s['tid']} started on CPU {s['cpu']} and finished on CPU {d['cpu']}")
        if d["seq"] < s["seq"]:
            fails.append(f"preemption: spinner {s['tid']} finished before it started")

    # time — one deadline thread per expected secondary
    dl = kind("DEADLINE")
    before = {r["tid"]: r for r in user("D_BEFORE")}
    dl_cpus = sorted(r["cpu"] for r in dl)
    if dl_cpus != sorted(expected):
        fails.append(f"time: deadline records on CPUs {dl_cpus}, expected one on each of {sorted(expected)}")
    for r in dl:
        timeout, result, dtick, dirq0, dirq_own = r["v"]
        tag = f"time: deadline on CPU {r['cpu']} (tid {r['tid']})"
        if result != TIMED_OUT:
            fails.append(f"{tag} returned {result}, not TimedOut")
        if dtick < timeout:
            fails.append(f"{tag} woke after {dtick} ticks, before its {timeout}-tick deadline")
        if dtick != dirq0:
            fails.append(f"{tag}: the tick advanced {dtick} over a window of {dirq0} CPU 0 interrupts")
        # Non-vacuity of the no-multiplication check above: the deadline's own CPU ticked in the
        # window and those ticks did not advance the clock. Where an idle secondary parks its
        # timer the window may be spent idle, so there the run as a whole must show it (below);
        # per-CPU timer liveness is the preemption obligation's.
        if dirq_own < 1 and not P()["idle_timer_parked"]:
            fails.append(f"{tag}: its CPU took no timer interrupt in the window")
        b = before.get(r["tid"])
        if b is None or b["cpu"] != r["cpu"]:
            fails.append(f"{tag}: armed on another CPU (no migration)")

    if P()["idle_timer_parked"] and dl and not any(r["v"][4] >= 1 for r in dl):
        fails.append("time: no deadline window saw its CPU's own timer, so the clock's independence "
                     "from secondary ticks went unexercised")

    # idle wake — one sleeper per expected secondary
    RACED.clear()
    ipi_woken = set()
    sleepers = {r["v"][0]: r for r in user("S_START")}
    s_cpus = sorted(r["cpu"] for r in sleepers.values())
    if s_cpus != sorted(expected):
        fails.append(f"placement: sleepers ran on CPUs {s_cpus}, expected one on each of {sorted(expected)}")
    prev = {k: r["seq"] for k, r in sleepers.items()}
    for rnd in range(1, WAKE_ROUNDS + 1):
        for k in range(1, ncpus):
            tag = f"idle wake (sleeper {k}, round {rnd})"
            code = (k << 8) | rnd
            s = sleepers.get(k)
            wake = [r for r in user("P_WAKE") if r["v"][0] == code]
            woke = [r for r in user("S_WOKE") if r["v"][0] == code]
            acked = [r for r in user("P_ACKED") if r["v"][0] == code]
            if s is None or len(wake) != 1 or len(woke) != 1 or len(acked) != 1:
                fails.append(f"{tag}: start/wake/woke/acked records {int(s is not None)}/{len(wake)}/{len(woke)}/{len(acked)}")
                continue
            c, tid = s["cpu"], s["tid"]
            w, kk = wake[0]["seq"], woke[0]["seq"]
            idles = [r for r in kind("IDLE", c) if prev[k] < r["seq"] < w]
            if not idles:
                fails.append(f"{tag}: CPU {c} never went idle before the wake")
            elif [r for r in kind("IDLE_DISPATCH", c) if idles[-1]["seq"] < r["seq"] < w]:
                fails.append(f"{tag}: CPU {c} had left idle before the wake")
            ipi = [r for r in kind("IDLE_IPI", c) if w < r["seq"] < kk and r["v"][0] & 1]
            disp = [r for r in kind("IDLE_DISPATCH", c) if r["tid"] == tid and w < r["seq"] < kk]
            # The one benign race: the idle CPU's own (backstop) timer fired between the enqueue
            # and the IPI's arrival, and dispatched the sleeper first; the IPI then arrived outside
            # the idle boundary. Accepted only when that IPI is on record after the wake (before
            # the next wake): a wake whose IPI never came still fails.
            nxt = min([r["seq"] for r in user("P_WAKE") if r["seq"] > w] + [10 ** 9])
            late = [r for r in kind("SGI", c) if w < r["seq"] < nxt]
            raced = (not ipi and len(disp) == 1 and disp[0]["v"][0] != 1 and bool(late))
            if raced:
                RACED.append((k, rnd, c))
            elif not ipi:
                fails.append(f"{tag}: no IPI taken at CPU {c}'s idle boundary after the wake")
            if len(disp) != 1:
                fails.append(f"{tag}: CPU {c} left idle for the sleeper {len(disp)} times")
            elif disp[0]["v"][0] != 1 and not raced:
                fails.append(f"{tag}: CPU {c} left idle for the sleeper by its timer, not the IPI")
            elif ipi and not ipi[0]["seq"] < disp[0]["seq"]:
                fails.append(f"{tag}: the dispatch preceded the IPI")
            if woke[0]["cpu"] != c:
                fails.append(f"{tag}: the sleeper woke on CPU {woke[0]['cpu']}, not its CPU {c}")
            if acked[0]["v"][1] != 1:
                fails.append(f"{tag}: the wake woke {acked[0]['v'][1]} waiters")
            prev[k] = kk
            if ipi and len(disp) == 1 and disp[0]["v"][0] == 1:
                ipi_woken.add(c)
    if P()["ipi_wake_per_cpu"]:
        for c in sorted(expected - ipi_woken):
            fails.append(f"idle wake: CPU {c} never left idle by its IPI")

    # ipc — one IPC thread per expected secondary, in turn
    threads = {r["v"][0]: r for r in user("T_START")}
    t_cpus = sorted(r["cpu"] for r in threads.values())
    if t_cpus != sorted(expected):
        fails.append(f"placement: IPC threads ran on CPUs {t_cpus}, expected one on each of {sorted(expected)}")
    last = 0
    for k in range(1, ncpus):
        t = threads.get(k)
        for rnd in range(1, IPC_ROUNDS + 1):
            code = (k << 8) | rnd
            ping = [r for r in user("P_PING") if r["v"][0] == code]
            pong = [r for r in user("T_PONG") if r["v"][0] == code]
            tag = f"ipc (thread {k}, round {rnd})"
            if t is None or len(ping) != 1 or len(pong) != 1:
                fails.append(f"{tag}: ping/pong records {len(ping)}/{len(pong)}")
                continue
            if ping[0]["v"][1] != 1 or ping[0]["cpu"] != 0:
                fails.append(f"{tag}: the ping did not arrive intact on CPU 0")
            if pong[0]["v"][1] != 1 or pong[0]["cpu"] != t["cpu"]:
                got = pong[0]["v"][1]
                why = (f" (receive error {got >> 8}, metadata {'written' if got & 0x80 else 'untouched'})"
                       if ARCH == "x86_64" and got != 1 else "")
                fails.append(f"{tag}: the pong did not arrive intact on the thread's CPU {t['cpu']}{why}")
            if not last < ping[0]["seq"] < pong[0]["seq"]:
                fails.append(f"{tag}: out of order")
            last = pong[0]["seq"]

    # churn
    child_cpus = set()
    child_on = {}
    for rnd in range(1, FORK_ROUNDS + 1):
        for k in range(1, ncpus):
            code = (rnd << 8) | k
            tag = f"churn (round {rnd}, child {k})"
            forked = [r for r in user("P_FORKED") if r["v"][1] == code]
            if len(forked) != 1:
                fails.append(f"{tag}: {len(forked)} fork records")
                continue
            child = forked[0]["v"][0]
            run = [r for r in user("C_RUN") if r["tid"] == child]
            reap = [r for r in kind("REAP") if r["tid"] == child]
            if len(run) != 1 or len(reap) != 1:
                fails.append(f"{tag}: child {child} run/reap records {len(run)}/{len(reap)}")
                continue
            c = run[0]["cpu"]
            child_cpus.add(c)
            child_on.setdefault(c, set()).add(run[0]["tid"])
            if c == 0:
                fails.append(f"{tag}: child {child} ran on CPU 0")
            if not run[0]["seq"] < reap[0]["seq"]:
                fails.append(f"{tag}: reaped before it ran")
            child_asid = run[0]["v"][2]
            left = [r for r in kind("ROOT_LEFT", c) if r["v"][0] == child_asid
                    and run[0]["seq"] < r["seq"] < reap[0]["seq"]]
            if not left:
                fails.append(f"{tag}: CPU {c} never left child {child}'s root (asid {child_asid}) before its reap")
            _, reaped, destroyed, quarantined, _ = reap[0]["v"]
            if reaped != 1 or destroyed != 1 or quarantined != 0:
                fails.append(f"{tag}: reap process_reaped={reaped} destroyed={destroyed} quarantined={quarantined}")
    for c in sorted(expected - child_cpus):
        fails.append(f"churn: no child ever ran on CPU {c}")
    # Faults: each child's first write to its copy-on-write stack is a user fault, handled on the
    # CPU it runs on (the kernel's own settlement line names the CPU and the task).
    cow = {(int(m.group(1)), int(m.group(2))) for m in
           re.finditer(r"PAGE_FAULT_HANDLED_COW cpu=(\d+) tid=(\d+) ", text)}
    for c in sorted(expected & child_cpus):
        if not any((c, t) in cow for t in child_on.get(c, ())):
            fails.append(f"faults: no child's copy-on-write fault was handled on CPU {c}")
    return fails


# ─────────────────────────────── self-test ───────────────────────────────

def _line(body):
    return f"{body} crc={zlib.crc32(body.encode()):08x}"


def _fixture(ncpus=3):
    """A passing record, in stamp order: (kind, cpu, tid, step, v...)."""
    sec = list(range(1, ncpus))
    mask = sum(1 << c for c in sec)
    ev = [("RELEASE", 0, 0, "-", mask, mask)]
    if P()["hw_identity"]:
        apic_of = {c: a for a, c in x86_plan(ncpus, TOPO)[0].items() if isinstance(c, int)}
        ev += [("AP_ADMITTED", c, 0, "-", apic_of[c], 1) for c in sec]
    else:
        ev += [("AP_ADMITTED", c, 0, "-", 0) for c in sec]
    ev += [("USER", 0, 9600, "P_START", 0), ("USER", 0, 9600, "P_AP_READY", mask, len(sec))]
    tid = 10000
    dls = []
    for c in sec:
        tid += 1
        dls.append((tid, c))
        ev.append(("USER", c, tid, "D_BEFORE", 0))
    spins = []
    for c in [0] + sec + [0] + sec:
        tid += 1
        spins.append((tid, c))
        ev.append(("USER", c, tid, "W_START", 0))
    for n in range(PRE_MIN):
        for t, c in spins:
            ev.append(("PREEMPT", c, t, "-", n + 1))
    for t, c in dls:
        ev.append(("DEADLINE", c, t, "-", 40, 9, 55, 55, 56))
    for t, c in spins:
        ev.append(("USER", c, t, "W_DONE", 0, 0))
    ev.append(("USER", 0, 9600, "P_A_DONE", len(spins), 0))
    sl = {}
    for k, c in enumerate(sec, 1):
        tid += 1
        sl[k] = (tid, c)
        ev += [("IDLE_IPI", c, 0, "-", 1), ("IDLE_DISPATCH", c, tid, "-", 1), ("USER", c, tid, "S_START", k)]
    for r in range(1, WAKE_ROUNDS + 1):
        for k, (t, c) in sl.items():
            code = (k << 8) | r
            ev += [("IDLE", c, 0, "-", r + 1), ("USER", 0, 9600, "P_WAKE", code), ("IDLE_IPI", c, 0, "-", 1),
                   ("IDLE_DISPATCH", c, t, "-", 1), ("USER", c, t, "S_WOKE", code, r),
                   ("USER", 0, 9600, "P_ACKED", code, 1)]
    th = {}
    for k, c in enumerate(sec, 1):
        tid += 1
        th[k] = (tid, c)
        ev.append(("USER", c, tid, "T_START", k))
    for k, (t, c) in th.items():
        for r in range(1, IPC_ROUNDS + 1):
            code = (k << 8) | r
            ev += [("USER", 0, 9600, "P_PING", code, 1), ("USER", c, t, "T_PONG", code, 1)]
    asid = 40
    for r in range(1, FORK_ROUNDS + 1):
        for k, c in enumerate(sec, 1):
            tid += 1
            asid += 1
            code = (r << 8) | k
            ev += [("USER", 0, 9600, "P_FORKED", tid, code), ("USER", c, tid, "C_RUN", code, code, asid),
                   ("ROOT_LEFT", c, 0, "-", asid), ("IDLE", c, 0, "-", 20 + r), ("REAP", 0, tid, "-", r, 1, 1, 0)]
    ev.append(("USER", 0, 9600, "P_END", 0))
    return ev


STACK_OK = "APSW_STACK reserved=0x200000 used=0,0x6000,0x6000,0x6000,0x6000,0x6000,0x6000,0x6000"


def x86_console(n, topo=""):
    """The x86 boot console a passing run prints for the plan of `-smp n[,topo]`."""
    plan, boot = x86_plan(n, topo)
    want = {a: c for a, c in plan.items() if isinstance(c, int)}
    present = sum(1 << c for c in want.values())
    con = [f"X86_CPU_DISCOVERY source=madt listed={n} beyond_table=0 boot_apic_id={boot} boot_listed=1"]
    con += [f"X86_CPU_PLANNED apic_id={a} cpu={c}" for a, c in sorted(want.items(), key=lambda x: x[1])]
    con += [f"X86_CPU_EXCLUDED apic_id={a} reason={r} started=0" for a, r in sorted(plan.items()) if isinstance(r, str)]
    con += [f"X86_LOGICAL_CPUS count={len(want)} bitmap=0x{present:x} ceiling=8"]
    con += [f"X86_APIC_CPU_BOUND apic_id={a} cpu={c} new=1 result=ok" for a, c in sorted(want.items()) if c]
    con += [f"X86_AP_LAPIC_OK cpu={c} apic_id={a}" for a, c in sorted(want.items()) if c]
    con += [f"X86_AP_RELEASE_PLAN planned=0x{present & ~1:x} released=0x{present & ~1:x} knob=0"]
    for a, c in sorted(want.items()):
        if c:
            con += [f"X86_AP_SCHED_ADMITTED cpu={c} apic_id={a} mode=production wake_only=0 result=ok",
                    f"X86_AP_STATE cpu={c} discovered=1 initialized=1 irq_ready=1 admitted=1 dispatching=1 reason=none"]
    return con


def _render(ev, *, tear=(), drop_both=(), extra="", stack=STACK_OK, cow=True, x86n=None):
    out = [P()["admitted_line"]]
    if ARCH == "x86_64":
        n = x86n or (max([e[1] for e in ev if e[0] == "AP_ADMITTED"] + [0]) + 1)
        out = x86_console(n, TOPO)
    if cow:
        out += [f"PAGE_FAULT_HANDLED_COW cpu={e[1]} tid={e[2]} asid=9 reason=recovered" for e in ev
                if e[0] == "USER" and e[3] == "C_RUN"]
    for i, e in enumerate(ev, 1):
        kind, cpu, tid, step, *v = e
        v = (list(v) + [0] * 5)[:5]
        body = f"APSW_R seq={i} kind={kind} cpu={cpu} tid={tid} step={step} " + " ".join(f"v{j}={v[j]}" for j in range(5))
        copies = [_line(body), _line(body)]
        if i in tear:
            copies[0] = copies[0][:20] + "XRISCV" + copies[0][20:]
        if i in drop_both:
            copies = [c[:25] + "Z" + c[26:] for c in copies]
        out += copies
    end = f"APSW_DUMP_END count={len(ev)} overflow=0 irq=1,1,1,1,0,0,0,0 pre=4,4,4,4,0,0,0,0 admitted=0x6 reap_retries=0"
    if P()["stack"] and stack is not None:
        out += [_line(stack), _line(stack)]
    out += [_line(end), _line(end), extra]
    return "\n".join(out)


def stack_summary(text, ncpus):
    st = parse_stack(text) if P()["stack"] else None
    if st is None:
        return ""
    reserved, used = st
    deepest = max(used[1:ncpus])
    return (f" stack_reserved={reserved:#x} stack_used={','.join(hex(u) for u in used[1:ncpus])}"
            f" stack_headroom_min={reserved - deepest:#x}")


def self_test():
    cases_run = 0
    for n in (2, 3, 4):
        base = _fixture(n)
        ok = grade(_render(base), n)
        assert ok == [], (n, ok)
        cases_run += 1
    base = _fixture(3)
    assert grade(_render(base, tear={7, 30}), 3) == [], "a torn copy with a good twin must pass"
    cases_run += 1

    def idx(pred, after=0):
        return next(i for i, e in enumerate(base) if i >= after and pred(e))

    def mutate(i, new):
        ev = list(base)
        ev[i] = new
        return ev

    def drop(i):
        ev = list(base)
        del ev[i]
        return ev

    def without(pred):
        return [e for e in base if not pred(e)]

    wake1 = idx(lambda e: e[3] == "P_WAKE")
    w2 = idx(lambda e: e[3] == "W_DONE" and e[1] == 2)
    cases = {
        "both copies torn": (_render(base, drop_both={12}), 3),
        "pinned diagnostic admission": (_render(base, extra=P()["pinned"]), 3),
        "an expected CPU never admitted": (_render(without(lambda e: e[0] == "AP_ADMITTED" and e[1] == 2)), 3),
        "a released CPU missing from the plan": (_render(mutate(0, ("RELEASE", 0, 0, "-", 0b10, 0b110))), 3),
        "a CPU quiescent on the console": (_render(base, extra=P()["quiescent"]), 3),
        "the boot had more CPUs than the run covered": (_render(base), 4),
        "an expected CPU ran no spinner": (_render([e if not (e[3] == "W_START" and e[1] == 2) else
                                                   ("USER", 1, e[2], "W_START", 0) for e in base]), 3),
        "no copy-on-write fault on any child's CPU": (_render(base, cow=False), 3),
        "quarantined teardown on console": (_render(base, extra="VM_ASPACE_TEARDOWN_QUARANTINED pid=1 asid=5"), 3),
        "corrupted register": (_render(mutate(w2, ("USER", 2, base[w2][2], "W_DONE", 1, 17))), 3),
        "spinner migrated": (_render(mutate(w2, ("USER", 1, base[w2][2], "W_DONE", 0, 0))), 3),
        "no secondary timer preemption on CPU 2": (_render(without(lambda e: e[0] == "PREEMPT" and e[1] == 2)), 3),
        "clock multiplied": (_render(mutate(idx(lambda e: e[0] == "DEADLINE"),
                                            ("DEADLINE", 1, base[idx(lambda e: e[0] == "DEADLINE")][2], "-", 40, 9, 111, 55, 56))), 3),
        "deadline not timed out": (_render(mutate(idx(lambda e: e[0] == "DEADLINE"),
                                                  ("DEADLINE", 1, base[idx(lambda e: e[0] == "DEADLINE")][2], "-", 40, 0, 55, 55, 56))), 3),
        "no deadline on CPU 2": (_render(without(lambda e: e[0] == "DEADLINE" and e[1] == 2)), 3),
        "wake by timer": (_render(mutate(idx(lambda e: e[0] == "IDLE_DISPATCH", wake1),
                                         base[idx(lambda e: e[0] == "IDLE_DISPATCH", wake1)][:4] + (0,))), 3),
        "wake without IPI": (_render(drop(idx(lambda e: e[0] == "IDLE_IPI", wake1))), 3),
        "target not idle at wake": (_render(drop(idx(lambda e: e[0] == "IDLE", wake1 - 2))), 3),
        "pong corrupted": (_render(mutate(idx(lambda e: e[3] == "T_PONG"),
                                          base[idx(lambda e: e[3] == "T_PONG")][:5] + (0,))), 3),
        "child on CPU 0": (_render(mutate(idx(lambda e: e[3] == "C_RUN"),
                                          ("USER", 0) + base[idx(lambda e: e[3] == "C_RUN")][2:])), 3),
        "reap quarantined": (_render(mutate(idx(lambda e: e[0] == "REAP"),
                                            base[idx(lambda e: e[0] == "REAP")][:5] + (1, 0, 1))), 3),
        "no root release before reap": (_render(drop(idx(lambda e: e[0] == "ROOT_LEFT"))), 3),
        "failure step": (_render(base[:-1] + [("USER", 0, 9600, "P_NO_ACK", 3), base[-1]]), 3),
        "no end": (_render(base[:-1]), 3),
        "a one-CPU boot": (_render(base), 1),
        "a nine-hart boot with no topology record": (_render(base), 9),
        "an unadmitted CPU and no trailer": (_render(base[:5]).split("APSW_DUMP_END")[0]
                                              + "\n" + P()["state"] + " cpu=2 discovered=1 initialized=1 irq_ready=0 admitted=0 dispatching=0 reason=no_irq_ready", 3),
        "the mailbox split copy-on-write": (_render(base, extra="APSW_PROVISIONED p_tid=9600 asid=4 image=1\n"
                                                     "VM_COW_SPLIT_COMMITTED cpu=0 tid=9600 asid=4 va=0x20070000 old_pa=0x1 new_pa=0x2 acked=1"), 3),
    }
    # Phase A's clock deadline: P's record names the CPU, and the grade says which.
    a_done = idx(lambda e: e[3] == "P_A_DONE")
    timed = without(lambda e: e[0] == "PREEMPT" and e[1] == 2)
    timed = timed[:a_done] + [("USER", 0, 9600, "P_PRE_TIMEOUT", 1 << 2, 1 << 2)] + timed[a_done:]
    got = grade(_render(timed), 3)
    assert any("CPU 2 had taken no timer preemption at all" in f for f in got), got
    assert not any("CPU 1 had" in f or "CPU 1 below" in f for f in got), got
    cases_run += 1
    got = grade(_render(base[:a_done] + [("USER", 0, 9600, "P_NO_IDLE", 1 << 2, 0)] + base[a_done:]), 3)
    assert any("CPU 2 never reached its idle wait" in f for f in got), got
    assert not any("CPU 1 never reached" in f for f in got), got
    cases_run += 1
    got = grade(_render(base[:a_done] + [("USER", 0, 9600, "P_W_TIMEOUT", 5, 6)] + base[a_done:]), 3)
    assert any("5 of 6 spinners running" in f for f in got), got
    cases_run += 1
    # A deadline window with none of its own CPU's ticks: a failure where idle timers run, and
    # allowed where they park, as long as some window in the run shows them.
    d1 = idx(lambda e: e[0] == "DEADLINE")
    quiet = mutate(d1, base[d1][:8] + (0,))
    got = grade(_render(quiet), 3)
    if P()["idle_timer_parked"]:
        assert got == [], got
        all_quiet = [e[:8] + (0,) if e[0] == "DEADLINE" else e for e in base]
        got = grade(_render(all_quiet), 3)
        assert any("went unexercised" in f for f in got), got
        cases_run += 2
    else:
        assert any("took no timer interrupt in the window" in f for f in got), got
        cases_run += 1
    if ARCH == "x86_64":
        global TOPO
        assert [reg_name(i) for i in (0, 3, 15, 31, 32, 33, 34, 35, 47, 64, 111)] == \
            ["rax", "rbx", "r15", "rsp", "rflags", "fcw", "mxcsr", "tls(fs:0)", "st7", "xmm0.lo", "xmm15.hi"]
        got = grade(_render(mutate(w2, ("USER", 2, base[w2][2], "W_DONE", 1, 96 + 7))), 3)
        assert any("last xmm7.hi" in f for f in got), got
        cases_run += 1
        # QEMU's sparse numbering: sockets=2,cores=3 -> APIC IDs 0,1,2,4,5,6.
        assert qemu_x86_apic_ids(6, "sockets=2,cores=3,threads=1") == [0, 1, 2, 4, 5, 6]
        assert qemu_x86_apic_ids(4, "") == [0, 1, 2, 3]
        assert qemu_x86_apic_ids(8, "sockets=2,cores=2,threads=2") == [0, 1, 2, 3, 4, 5, 6, 7]
        assert qemu_x86_apic_ids(6, "sockets=3,cores=1,threads=2") == [0, 1, 2, 3, 4, 5]
        assert qemu_x86_apic_ids(6, "sockets=2,cores=1,threads=3") == [0, 1, 2, 4, 5, 6]
        TOPO = "sockets=2,cores=3,threads=1"
        sparse = _fixture(6)
        assert grade(_render(sparse, x86n=6), 6) == [], grade(_render(sparse, x86n=6), 6)
        cases_run += 1
        # The admission record names another APIC than the plan gives (logical 3 is APIC 4 here).
        ai = next(i for i, e in enumerate(sparse) if e[0] == "AP_ADMITTED" and e[1] == 3)
        wrong = list(sparse)
        wrong[ai] = ("AP_ADMITTED", 3, 0, "-", 3, 1)
        assert any("identity: CPU 3" in f for f in grade(_render(wrong, x86n=6), 6))
        # The console plan numbering the sparse IDs as if contiguous.
        bad = _render(sparse, x86n=6).replace("X86_CPU_PLANNED apic_id=4 cpu=3", "X86_CPU_PLANNED apic_id=3 cpu=3")
        assert any("topology: planned" in f for f in grade(bad, 6))
        cases_run += 2
        TOPO = ""
        # Ten processors: eight logical CPUs, APIC 8 and 9 excluded at the ceiling, never started.
        ten = _fixture(8)
        good = _render(ten, x86n=10)
        assert grade(good, 10) == [], grade(good, 10)
        assert any("excluded APIC 9 was bound or started" in f
                   for f in grade(good + "\nX86_AP_LAPIC_OK cpu=8 apic_id=9", 10))
        assert any("APIC 8 must be excluded" in f
                   for f in grade(good.replace("X86_CPU_EXCLUDED apic_id=8 reason=beyond_production_cpu_ceiling started=0", ""), 10))
        cases_run += 3
        # A released set that leaves a planned CPU out, or a release under the diagnostic knob.
        assert any("release plan" in f for f in grade(_render(base).replace("released=0x6 knob=0", "released=0x2 knob=0"), 3))
        assert any("release plan" in f for f in grade(_render(base).replace("released=0x6 knob=0", "released=0x6 knob=1"), 3))
        cases_run += 2
        # No raced wake is accepted, and a CPU whose wakes all came by timer fails per CPU.
        timered = [e[:4] + (0,) if e[0] == "IDLE_DISPATCH" and e[1] == 2 else e for e in base]
        got = grade(_render(timered), 3)
        assert any("CPU 2 never left idle by its IPI" in f for f in got), got
        cases_run += 1
        return _finish_self_test(cases, cases_run)
    # The register names a mismatch is reported under, per port.
    if ARCH == "aarch64":
        assert [reg_name(i) for i in (20, 31, 32, 33, 34, 35, 64, 127)] == \
            ["x20", "sp", "nzcv", "fpcr", "fpsr", "tpidr_el0", "q0.lo", "q31.hi"]
        got = grade(_render(mutate(w2, ("USER", 2, base[w2][2], "W_DONE", 1, 96 + 7))), 3)
        assert any("last q7.hi" in f for f in got), got
        cases_run += 1
        # The spinner top-up: one more spinner, placed and graded like the rest, counted from
        # P_W_COVER; a count the starts do not match fails, as does a top-up that stopped short.
        w1 = idx(lambda e: e[3] == "W_START" and e[1] == 1)
        tid = base[w1][2]
        extra = [(e[0], e[1], tid + 500) + tuple(e[3:]) for e in base
                 if e[2] == tid and e[0] in ("USER", "PREEMPT") and e[3] in ("W_START", "W_DONE", "-")]
        topped = base[:a_done] + extra + [("USER", 0, 9600, "P_W_COVER", 7, 0b111)] + base[a_done:]
        assert grade(_render(topped), 3) == [], grade(_render(topped), 3)
        cases_run += 1
        miscount = base[:a_done] + extra + [("USER", 0, 9600, "P_W_COVER", 8, 0b111)] + base[a_done:]
        assert any("spinners started, expected 8" in f for f in grade(_render(miscount), 3))
        cases_run += 1
        short = base[:a_done] + extra + [("USER", 0, 9600, "P_W_COVER", 7, 0b011)] + base[a_done:]
        assert any("stopped its top-up" in f for f in grade(_render(short), 3))
        cases_run += 1
        # The backstop-timer race: accepted only with the IPI's late arrival on record.
        ii = idx(lambda e: e[0] == "IDLE_IPI", wake1)
        di = idx(lambda e: e[0] == "IDLE_DISPATCH", wake1)
        cpu = base[ii][1]
        raced = list(base)
        raced[di] = base[di][:4] + (0,)
        raced = raced[:di + 1] + [("SGI", cpu, 0, "-", 1)] + raced[di + 1:]
        del raced[ii]
        assert grade(_render(raced), 3) == [], grade(_render(raced), 3)
        lost = [e for e in raced if e[0] != "SGI"]
        got = grade(_render(lost), 3)
        assert any("by its timer, not the IPI" in f for f in got), got
        cases_run += 2
        got = grade(_render(base, stack=None), 3)
        assert any("no verified APSW_STACK" in f for f in got), got
        got = grade(_render(base, stack="APSW_STACK reserved=0x200000 used=0,0x6000,0,0,0,0,0,0"), 3)
        assert any("CPU 2's dispatch stack was never used" in f for f in got), got
        got = grade(_render(base, stack="APSW_STACK reserved=0x200000 used=0,0x200000,0x6000,0,0,0,0,0"), 3)
        assert any("CPU 1 used all" in f for f in got), got
        cases_run += 3
        return _finish_self_test(cases, cases_run)
    # The topology section: -smp 10, the boot hart inside and outside the table.
    for boot, rejected in ((0, 8), (9, 7), (8, 7), (3, 8)):
        plan = policy_plan(10, boot)
        cpus = sorted(c for c in plan.values() if isinstance(c, int))
        excl = {h: r for h, r in plan.items() if isinstance(r, str)}
        assert len(cpus) == 8 and (rejected is None or excl.get(rejected) == "beyond_production_cpu_ceiling")
        con = [f"RISCV_LOGICAL_CPUS present=0xff count=8 excluded_harts=0x{sum(1 << h for h in excl):x} "
               f"hart_bitmap=0x3ff boot_hart={boot}"]
        con += [f"RISCV_HART_EXCLUDED hart={h} reason={r} dispatch=0" for h, r in sorted(excl.items())]
        con += [f"RISCV_SECONDARY_CPU_ID_REJECTED hart={h} slot={h} reason={r} started=0"
                for h, r in sorted(excl.items()) if h < HANDOFF_SLOTS]
        con += [f"RISCV_AP_SCHED_ADMITTED hart={h} cpu={c} mode=production" for h, c in plan.items()
                if isinstance(c, int) and c]
        con += ["RISCV_AP_RELEASE_PLAN trap_ready=0xff dispatch_capable=0xff released=0xfe knob=0"]
        con += [f"RISCV_AP_STATE cpu={c} discovered=1 initialized=1 irq_ready=1 admitted=1 dispatching=1 reason=none"
                for c in range(1, 8)]
        good = "\n".join(con)
        assert grade_topology(good, 10)[0] == [], (boot, grade_topology(good, 10))
        cases_run += 1
        if rejected is not None:
            bad = good + f"\nRISCV_SECONDARY_HART_PARK hart={rejected}"
            assert grade_topology(bad, 10)[0], "an excluded hart that was started must fail"
            bad = good.replace(f"RISCV_HART_EXCLUDED hart={rejected} reason=beyond_production_cpu_ceiling", "")
            assert grade_topology(bad, 10)[0], "an unnamed exclusion must fail"
            nine = good.replace("present=0xff count=8", "present=0x1ff count=9")
            assert grade_topology(nine, 10)[0], "nine logical CPUs must fail"
            cases_run += 3
    full = _fixture(8)
    con = ("RISCV_LOGICAL_CPUS present=0xff count=8 excluded_harts=0x300 hart_bitmap=0x3ff boot_hart=0\n"
           "RISCV_HART_EXCLUDED hart=8 reason=beyond_production_cpu_ceiling dispatch=0\n"
           "RISCV_HART_EXCLUDED hart=9 reason=beyond_production_cpu_ceiling dispatch=0\n"
           "RISCV_SECONDARY_CPU_ID_REJECTED hart=8 slot=8 reason=beyond_production_cpu_ceiling started=0\n"
           "RISCV_SECONDARY_CPU_ID_REJECTED hart=9 slot=9 reason=beyond_production_cpu_ceiling started=0\n"
           + "\n".join(f"RISCV_AP_SCHED_ADMITTED hart={c} cpu={c} mode=production" for c in range(1, 8)) + "\n"
           "RISCV_AP_RELEASE_PLAN trap_ready=0xff dispatch_capable=0xff released=0xfe knob=0\n"
           + "\n".join(f"RISCV_AP_STATE cpu={c} discovered=1 initialized=1 irq_ready=1 admitted=1 dispatching=1 reason=none"
                       for c in range(1, 8)))
    assert grade(_render(full, extra=con), 10) == [], grade(_render(full, extra=con), 10)
    cases_run += 1
    return _finish_self_test(cases, cases_run)


def _finish_self_test(cases, cases_run):
    for name, (text, n) in cases.items():
        got = grade(text, n)
        assert got, f"self-test: the '{name}' control passed"
    print(f"APSW_GRADER_SELF_TEST arch={ARCH} cases={len(cases) + cases_run} result=ok")


def main():
    global ARCH
    args = sys.argv[1:]
    if len(args) >= 2 and args[0] == "--arch":
        if args[1] not in PORTS:
            print(f"unknown --arch {args[1]}")
            return 2
        ARCH = args[1]
        args = args[2:]
    global TOPO
    if len(args) >= 2 and args[0] == "--topo":
        TOPO = args[1]
        args = args[2:]
    if args == ["--self-test"]:
        self_test()
        return 0
    if len(args) == 3 and args[0] == "--topology-only":
        if ARCH != "riscv64":
            print("usage: --topology-only is the RISC-V beyond-the-ceiling section")
            return 2
        nharts = int(args[1])
        tfails, summary = grade_topology(open(args[2], errors="replace").read(), nharts)
        for f in tfails:
            print(f"[apsched-witness][fail] {f}")
        print(f"APSW_TOPOLOGY_SEAL arch={ARCH} {summary or ''} result={'fail' if tfails else 'ok'}")
        return 1 if tfails else 0
    if len(args) != 3 or args[0] != "--expect-cpus":
        print("usage: grade-apsched-witness.py [--arch riscv64|aarch64|x86_64] [--topo T] --expect-cpus N BOOT_LOG")
        return 2
    ncpus = int(args[1])
    text = open(args[2], errors="replace").read()
    if ARCH == "riscv64" and ncpus > PRODUCTION_CPU_CEILING:
        tfails, summary = grade_topology(text, ncpus)
        print(f"APSW_TOPOLOGY_SEAL arch={ARCH} {summary or ''} result={'fail' if tfails else 'ok'}")
    if ARCH == "x86_64":
        tfails, summary = grade_topology_x86(text, ncpus, TOPO)
        print(f"APSW_TOPOLOGY_SEAL arch={ARCH} {summary or ''} result={'fail' if tfails else 'ok'}")
    fails = grade(text, ncpus)
    ncpus = min(ncpus, PRODUCTION_CPU_CEILING)
    for f in fails:
        print(f"[apsched-witness][fail] {f}")
    if fails:
        print(f"APSW_WITNESS_SEAL arch={ARCH} cpus={ncpus} result=fail")
        return 1
    recs, end, _ = parse(text)
    R = sorted(recs.values(), key=lambda r: int(r["seq"]))
    starts = {r["tid"] for r in R if r["kind"] == "USER" and r["step"] == "W_START"}
    pre = [sum(1 for r in R if r["kind"] == "PREEMPT" and r["tid"] in starts and int(r["cpu"]) == c)
           for c in range(ncpus)]
    dl = [r for r in R if r["kind"] == "DEADLINE"]
    dtick = ",".join(f"{r['cpu']}:{r['v2']}={r['v3']}/{r['v4']}" for r in sorted(dl, key=lambda r: int(r["cpu"])))
    print(f"APSW_WITNESS_SEAL arch={ARCH} cpus={ncpus} admission=production per_cpu_work=threads,forks "
          f"preempt_recorded={','.join(map(str, pre))} preempt_counted={end.get('pre', '?')} registers=intact deadlines=timed_out dtick=dirq0/own:{dtick} "
          f"idle_wakes={WAKE_ROUNDS * (ncpus - 1)}_ipi{'(' + str(len(RACED)) + '_timer_won_race)' if RACED else ''} ipc={IPC_ROUNDS * (ncpus - 1)} "
          f"churn={FORK_ROUNDS * (ncpus - 1)}_reaped_quarantined=0 records={end['count']}{stack_summary(text, ncpus)} result=ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
