#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP3 — the independent grader for `scripts/qemu-riscv64-smp3-witness-smoke.sh`.
#
# Usage: grade-riscv64-smp3-witness.py <boot.log> <sequence-check exit status>
#
# Re-derives every graded edge from the raw sealed record (`SMP3_REC`), written independently of
# the kernel's verifier (`kernel::boot::smp3_record::verify`), and then also requires the kernel's
# counts and verdict to agree. Never retries; exits non-zero on any failure.
import re
import sys

text = open(sys.argv[1], "rb").read().decode("utf-8", "replace").replace("\r", "\n")
lines = text.split("\n")
fails = []


def fail(msg):
    fails.append(msg)


if sys.argv[2] != "0":
    fail("the built image's fence / publication / clear ordering is incomplete (sequence.txt)")

# ── FIRMWARE AND BRING-UP ──
sbi = next((l for l in lines if "Runtime SBI Version" in l), None)
osbi = next((l for l in lines if re.search(r"OpenSBI v\d", l)), None)
if not sbi or not osbi:
    fail("the OpenSBI banner (version, runtime SBI version) is missing")

# QEMU-SMP3-ACCEPTANCE §3 — THE QUALIFIED FIRMWARE. The remote-fence completion claim holds for one
# pinned implementation, so every boot must prove it ran exactly that one: the runner's recorded
# artifact, the banner, and the firmware's own Base-extension answer. Any missing piece fails.
pin_path = sys.argv[3] if len(sys.argv) > 3 else None
ident_path = sys.argv[4] if len(sys.argv) > 4 else None
if not pin_path or not ident_path:
    fail("no firmware pin / artifact identity was given: the firmware is unqualified")
else:
    pin = dict(l.split("=", 1) for l in open(pin_path).read().splitlines() if "=" in l and not l.startswith("#"))
    try:
        ident = open(ident_path).read()
    except OSError:
        ident = ""
    m = re.search(r"^firmware_sha256=([0-9a-f]{64})$", ident, re.M)
    if not m:
        fail("the run recorded no firmware identity")
    elif m.group(1) != pin["sha256"]:
        fail("the booted firmware %s is not the pinned %s" % (m.group(1), pin["sha256"]))
    if osbi and pin["banner"] not in osbi:
        fail("the banner names %r, not the pinned %r" % (osbi.strip(), pin["banner"]))
    if sbi and not re.search(r"Runtime SBI Version\s*:\s*%s\b" % re.escape(pin["runtime_sbi"]), sbi):
        fail("the runtime SBI version is not the pinned %s" % pin["runtime_sbi"])
    ids = [re.search(r"SMP3_SBI_IDENTITY spec=0x([0-9a-f]+) impl_id=(\d+) impl_version=0x([0-9a-f]+)", l) for l in lines]
    ids = [x for x in ids if x]
    if len(ids) != 1:
        fail("expected exactly one firmware self-identification, saw %d" % len(ids))
    else:
        spec, impl_id, impl_ver = int(ids[0].group(1), 16), int(ids[0].group(2)), int(ids[0].group(3), 16)
        if (spec, impl_id, impl_ver) != (int(pin["spec"], 16), int(pin["impl_id"]), int(pin["impl_version"], 16)):
            fail("the firmware identifies as spec=0x%x impl_id=%d impl_version=0x%x, not the pinned implementation" % (spec, impl_id, impl_ver))
        else:
            print("[smp3-witness] firmware pinned: sha256=%s impl_id=%d impl_version=0x%x spec=0x%x" % (pin["sha256"][:16], impl_id, impl_ver, spec))
# Printed while only one hart writes the console (the secondary is parked, or not yet released),
# so each must be present exactly once and intact.
for pat, want in [
    ("RISCV_SECONDARY_PARK_WAKE_SOURCE hart=", 1),
    ("RISCV_SMP3_RELEASE boot_cpu=0 ", 1),
    ("SMP3_WITNESS_PROVISIONED ", 1),
]:
    n = sum(pat in l for l in lines)
    if n != want:
        fail("expected %d x '%s', saw %d" % (want, pat, n))
# Printed by the released secondary while the boot hart is also writing. The two harts share the
# console without a line lock, so these can be interleaved byte-wise: an intact copy is checked,
# at most one may exist, and the graded facts come from the sealed `SMP3_BRINGUP` line.
live_damaged = []
for pat in ["RISCV_SMP3_SECONDARY_RELEASED ", "RISCV_SMP3_SECONDARY_ADMITTED ",
            "SMP3_WITNESS_SECONDARY_PLACED "]:
    n = sum(pat in l for l in lines)
    if n > 1:
        fail("expected at most 1 x '%s', saw %d" % (pat, n))
    elif n == 0:
        live_damaged.append(pat.strip())
wake = next((l for l in lines if "RISCV_SECONDARY_PARK_WAKE_SOURCE" in l), "")
if "ssie=1 sstatus_sie=0 delivery=wfi_wake_only" not in wake:
    fail("the park's wake source is not SSIE-only with SIE clear: %r" % wake)
rel = next((l for l in lines if "RISCV_SMP3_RELEASE boot_cpu=0" in l), "")
if "ssie=1" not in rel or "released_harts=1 refused=0 knob=1" not in rel:
    fail("the boot hart's release is not one successful knob-gated release: %r" % rel)
adm = next((l for l in lines if "RISCV_SMP3_SECONDARY_ADMITTED" in l), None)
if adm is not None and ("cpu=1 wake_only=0 balance_excluded=1 idle_placeholder=cleared ipi_ready=1" not in adm or "result=ok" not in adm):
    fail("the secondary's admission is not the one scheduler transition: %r" % adm)
if any(p in l for l in lines for p in ("RISCV_SMP3_SECONDARY_UNADMITTED", "RISCV_SMP3_SECONDARY_ADMISSION_REFUSED")):
    fail("the secondary was not admitted")
m = re.search(r"RISCV_BOOT_HART_SELECTED hart=(\d+)", text)
boot_hart = int(m.group(1)) if m else None

# ── NOTHING FATAL ──
for bad in ["PANIC", "panicked", "SMP3_USER_FAIL", "SMP3_WITNESS_PROVISION_FAIL",
            "RISCV_SMP3_SECONDARY_UNADMITTED", "RISCV_SMP3_SECONDARY_ADMISSION_REFUSED",
            "SMP3_WITNESS_SECONDARY_PLACEMENT_FAIL", "_BROAD_ENTRY", "_UNROUTED",
            "RISCV_TRAP_UNHANDLED", "RISCV_TRAP_HANDLE_FAILED", "IRQ1_UNKNOWN_SETTLED",
            "trap_from_s_mode"]:
    n = sum(bad in l for l in lines)
    if n:
        fail("%d line(s) with %s" % (n, bad))

# ── the sealed record: each line taken only from a copy whose checksum verifies ──


def fnv1a(b):
    h = 0x811C9DC5
    for x in b:
        h = ((h ^ x) * 0x01000193) & 0xFFFFFFFF
    return h


KEYS = r"SMP3_(REC|ATTEMPT|ROLES|BRINGUP|CPU_IPI|CPU_FENCE|COUNTS_IPI|COUNTS_WAKE|COUNTS_P2|COUNTS_TLB|COUNTS_MUT|SYNC|SEAL_SYNC|VERDICT) "
dump, damaged = {}, 0
for l in lines:
    i, j = l.find("SMP3_"), l.rfind(" pass=")
    m = re.search(r" pass=([12]) crc=0x([0-9a-f]{8})$", l.rstrip())
    if i < 0 or j < i or not m or not re.match(KEYS, l[i:]):
        if " pass=" in l and "crc=" in l:
            damaged += 1
        continue
    body = l[i:j]
    if fnv1a(body.encode()) != int(m.group(2), 16):
        damaged += 1
        continue
    key = body.split(" ", 1)[0]
    if key == "SMP3_REC":
        key = "REC " + re.search(r"seq=(\d+)", body).group(1)
    elif key == "SMP3_ATTEMPT":
        key = "ATTEMPT " + " ".join(re.search(r"phase=(\d+) n=(\d+) target_cpu=(\d+)", body).groups())
    elif key.startswith("SMP3_CPU_"):
        key = key + " " + body.split(" ", 2)[1]
    dump.setdefault(key, body)

m = re.search(r"SMP3_ROLES s=(\d+):(\d+):(\d+):(\d+) c=(\d+):(\d+):(\d+):(\d+) h1=(\d+):(\d+) h0=(\d+):(\d+) w_va=0x([0-9a-f]+)", dump.get("SMP3_ROLES", ""))
roles = None
if m:
    g = [int(x) for x in m.groups()[:12]]
    roles = dict(s=dict(tid=g[0], asid=g[1], cpu=g[2], hart=g[3]),
                 c=dict(tid=g[4], asid=g[5], cpu=g[6], hart=g[7]),
                 h1=g[8], h0=g[10], w=int(m.group(13), 16))
counts = " ".join(v for k, v in sorted(dump.items()) if k.startswith("SMP3_COUNTS"))
# ── the secondary's admitted state and placement, from the sealed line ──
# The secondary's hart as the park itself announced it (printed before the release, so intact) —
# independent of the kernel's CPU->hart table that the sealed lines are written from.
m = re.search(r"RISCV_SECONDARY_PARK_WAKE_SOURCE hart=(\d+) cpu=1 ", text)
sec_hart = int(m.group(1)) if m else None
m = re.search(r"SMP3_BRINGUP cpu=1 hart=(\d+) sie=0x([0-9a-f]+) sstatus_sie=(\d) sum=(\d) placed=(\w+) tids=9302,9300 kick=(\d)$", dump.get("SMP3_BRINGUP", ""))
if not m:
    fail("the sealed bring-up line is missing or malformed: %r" % dump.get("SMP3_BRINGUP"))
else:
    if sec_hart is None or int(m.group(1)) != sec_hart:
        fail("the sealed bring-up hart %s is not the parked secondary's hart %s" % (m.group(1), sec_hart))
    sie = int(m.group(2), 16)
    # Admitted with SSIE as the only enabled source, SIE clear outside the idle wait, SUM set.
    if sie != 0x2 or m.group(3) != "0" or m.group(4) != "1":
        fail("the secondary's admitted state is wrong: sie=0x%x sstatus_sie=%s sum=%s" % (sie, m.group(3), m.group(4)))
    if m.group(5) != "ok" or m.group(6) != "1":
        fail("the secondary's two tasks were not placed and kicked: placed=%s kick=%s" % (m.group(5), m.group(6)))
verdict = dump.get("SMP3_VERDICT")
nrec = re.search(r"records=(\d+)", counts)
recs = []
if nrec:
    for q in range(int(nrec.group(1))):
        body = dump.get("REC %d" % q)
        if body is None:
            fail("record seq %d damaged in both dump copies or missing" % q)
            continue
        m = re.match(r"SMP3_REC seq=(\d+) kind=(\w+) cpu=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+)(?: step=(\w+))?$", body)
        recs.append(dict(seq=int(m.group(1)), kind=m.group(2), cpu=int(m.group(3)),
                         f=[int(m.group(i), 16) for i in range(4, 9)], step=m.group(9)))
if roles is None or not recs or verdict is None or not nrec:
    fail("record not dumped (roles=%s records=%d verdict=%s)" % (roles is not None, len(recs), verdict is not None))
    for f in fails:
        print("[smp3-witness][fail] " + f)
    print("SMP3_WITNESS_SEAL result=fail")
    sys.exit(1)
if [r["seq"] for r in recs] != list(range(len(recs))):
    fail("record sequence is not contiguous from 0")
S, C, W = roles["s"], roles["c"], roles["w"]
if (S["cpu"], C["cpu"]) != (1, 0):
    fail("roles not on their home CPUs")
# Hart identity: the roles' harts are the harts the boot actually ran, never CPU indices assumed.
if sec_hart is None or boot_hart is None or S["hart"] != sec_hart or C["hart"] != boot_hart or sec_hart == boot_hart:
    fail("hart identity: s_hart=%s (secondary %s), c_hart=%s (boot %s)" % (S["hart"], sec_hart, C["hart"], boot_hart))


def find(start, pred):
    for i in range(max(start, 0), len(recs)):
        if pred(recs[i]):
            return i
    return None


def user(start, tid, step, rnd=None):
    return find(start, lambda r: r["kind"] == "user" and r["f"][0] == tid and r["step"] == step
                and (rnd is None or r["f"][3] == rnd))


# ── IPI POPULATION ──
pending, requests, taken = {}, {}, {}
consumed = empty = merged = arrivals = 0
for i, r in enumerate(recs):
    k = r["kind"]
    if k == "ipi_published" and r["f"][2] in (0, 2):
        key = (r["cpu"], r["f"][0])
        if r["f"][3] == 0:
            if key in pending:
                fail("seq %d: fresh publication %d->%d while one was pending" % (r["seq"], key[0], key[1]))
            pending[key] = i
        else:
            if key not in pending:
                fail("seq %d: merged publication %d->%d with none pending" % (r["seq"], key[0], key[1]))
            merged += 1
        # The target named by hart must be the target's hart.
        want_hart = S["hart"] if r["f"][0] == 1 else C["hart"]
        if r["f"][1] != want_hart:
            fail("seq %d: publication to cpu %d names hart %d, not %d" % (r["seq"], r["f"][0], r["f"][1], want_hart))
    elif k == "ipi_requested":
        if r["f"][3] != 0:
            fail("seq %d: firmware refused the IPI request (0x%x)" % (r["seq"], r["f"][3]))
        else:
            requests[r["f"][0]] = requests.get(r["f"][0], 0) + 1
    elif k in ("ipi_arrived", "park_released"):
        taken[r["cpu"]] = taken.get(r["cpu"], 0) + 1
        if k == "park_released":
            continue
        arrivals += 1
        if r["f"][0] == 0:
            empty += 1
        for src in range(64):
            if r["f"][0] & (1 << src):
                if (src, r["cpu"]) not in pending:
                    fail("seq %d: consumed source %d on cpu %d with no pending publication" % (r["seq"], src, r["cpu"]))
                else:
                    del pending[(src, r["cpu"])]
                    consumed += 1
for (src, dst), i in pending.items():
    fail("seq %d: publication %d->%d never consumed" % (recs[i]["seq"], src, dst))
for cpu, n in taken.items():
    if n > requests.get(cpu, 0):
        fail("cpu %d consumed %d supervisor software interrupts but the firmware raised at most %d" % (cpu, n, requests.get(cpu, 0)))
fpvs = 0
for r in recs:
    if r["kind"] == "ipi_arrived" and r["f"][1] & 0xFF == 0:
        if r["f"][4] & ((3 << 13) | (3 << 9)):
            fail("seq %d: FP or vector on in an interrupted user frame (sstatus 0x%x)" % (r["seq"], r["f"][4]))
        else:
            fpvs += 1


def chain(after, src_cpu, dst_cpu, origin, window):
    p = find(after, lambda r: r["kind"] == "ipi_published" and r["cpu"] == src_cpu and r["f"][0] == dst_cpu and r["f"][2] == 0)
    if p is None:
        return None, "no publication %d->%d" % (src_cpu, dst_cpu)
    a = find(p + 1, lambda r: r["kind"] == "ipi_arrived" and r["cpu"] == dst_cpu and r["f"][0] & (1 << src_cpu))
    if a is None:
        return None, "publication seq %d %d->%d never consumed" % (recs[p]["seq"], src_cpu, dst_cpu)
    if origin is not None and recs[a]["f"][1] & 0xFF != origin:
        return None, "arrival seq %d origin %d, want %d" % (recs[a]["seq"], recs[a]["f"][1] & 0xFF, origin)
    if window and (recs[a]["f"][1] >> 8) & 0xFF != window:
        return None, "arrival seq %d sepc 0x%x outside window %d" % (recs[a]["seq"], recs[a]["f"][2], window)
    return a, None


def parked(after, src_cpu, dst_cpu, woken):
    a, why = chain(after, src_cpu, dst_cpu, None, 0)
    if why:
        return None, why
    if recs[a]["f"][1] & 0xFF == 1:
        d = find(a + 1, lambda r: r["cpu"] == dst_cpu and r["kind"] in ("idle_dispatch", "ipi_arrived"))
        if d is None or recs[d]["kind"] != "idle_dispatch" or recs[d]["f"][1] != 1 or recs[d]["f"][0] == 0:
            return None, "idle-boundary arrival seq %d not followed by an IPI-driven idle dispatch" % recs[a]["seq"]
        # The IPI's idle advance selected another runnable task first: counted apart, never
        # credited; the woken task's own context-checked resume on that hart is still required.
        if recs[d]["f"][0] != woken:
            return (d, "preceded"), None
        return (d, "ipi"), None
    ds = [i for i in range(after, a) if recs[i]["kind"] == "idle_dispatch" and recs[i]["cpu"] == dst_cpu
          and recs[i]["f"][1] == 0 and recs[i]["f"][0] == woken]
    if not ds:
        return (after, "busy"), None
    if recs[a]["f"][3] & 0xFFFFFFFF != woken:
        return None, "arrival seq %d in tid %d, not the resumed tid %d" % (recs[a]["seq"], recs[a]["f"][3] & 0xFFFFFFFF, woken)
    return (ds[-1], "timer"), None


# ── PARKED TARGETS ──
parked_n, ipi_to_s, ipi_to_c, timer_first, busy, preceded, at = 0, 0, 0, 0, 0, 0, 0
P1_ROUNDS = 8
for k in range(1, P1_ROUNDS + 1):
    c = user(at, C["tid"], "C_P1_CALL", k)
    if c is None:
        fail("P1 round %d: C_P1_CALL missing" % k)
        break
    res, why = parked(c, 0, 1, S["tid"])
    if why:
        fail("P1 round %d 0->1: %s" % (k, why))
        break
    d, route = res
    ipi_to_s += route == "ipi"
    timer_first += route == "timer"
    busy += route == "busy"
    preceded += route == "preceded"
    r = user(d, S["tid"], "S_P1_RESUMED", k)
    if r is None or recs[r]["cpu"] != 1:
        fail("P1 round %d: S did not resume (context-checked) on cpu 1" % k)
        break
    parked_n += 1
    rp = user(r, S["tid"], "S_P1_REPLY", k)
    if rp is None:
        fail("P1 round %d: S_P1_REPLY missing" % k)
        break
    res, why = parked(rp, 1, 0, C["tid"])
    if why:
        fail("P1 round %d 1->0: %s" % (k, why))
        break
    d, route = res
    ipi_to_c += route == "ipi"
    timer_first += route == "timer"
    busy += route == "busy"
    preceded += route == "preceded"
    b = user(d, C["tid"], "C_P1_RESUMED", k)
    if b is None or recs[b]["cpu"] != 0:
        fail("P1 round %d: C did not resume (context-checked) on cpu 0" % k)
        break
    parked_n += 1
    at = b
if ipi_to_s != P1_ROUNDS or ipi_to_c < 2:
    fail("IPI-driven parked dispatches: %d/%d to CPU 1 (no timer there: all must be), %d/%d to CPU 0 (>= 2 required)" % (ipi_to_s, P1_ROUNDS, ipi_to_c, P1_ROUNDS))

# ── QEMU-SMP3-SEAL: the attempt contract, re-derived independently of the kernel's verifier ──
# Every attempt is graded for its OBLIGATIONS first (any failure fails the boot, never retried);
# then classified eligible or not from independently recorded evidence only — the readiness
# record, the shootdown's own target computation, the hart's supervisor-entry history and its
# activation history — never from a missing fence or a failed check. Only eligible attempts
# earn coverage. The kernel's attempt lines and counts must then agree with this grader's.
P2_ATTEMPTS, TLB_ROUNDS, MUT_ROUNDS = 6, 24, 4
attempts = {}   # (phase, n, target_cpu) -> "eligible reason outcome"


def arr_origin(r): return r["f"][1] & 0xFF
def arr_window(r): return (r["f"][1] >> 8) & 0xFF
def arr_entries(r): return r["f"][1] >> 16
def arr_tid(r): return r["f"][3] & 0xFFFFFFFF
def arr_asid(r): return r["f"][3] >> 32


def ready(start, waker_cpu, target_cpu, phase, rnd):
    return find(start, lambda r: r["kind"] == "ready" and r["cpu"] == waker_cpu and r["f"][0] & 0xFF == target_cpu
                and (r["f"][0] >> 8) & 0xFF == phase and r["f"][3] == rnd)


def reactivated(target, disp, before):
    i = find(disp + 1, lambda r: r["kind"] == "activation" and r["cpu"] == target["cpu"] and r["f"][0] == target["asid"])
    return i is not None and i < before


# ── USER TARGETS (P2) ──
p2 = dict(attempts=0, credited_s=0, credited_c=0, uncredited=0, in_window=0, outside=0, displaced=0)
for k in range(1, P2_ATTEMPTS + 1):
    for waker, call, sent, tgt, ok, win in [
            (C, "C_P2A_CALL", "C_P2A_SENT", S, "S_WIN_A_OK", 1),
            (S, "S_P2B_CALL", "S_P2B_SENT", C, "C_WIN_B_OK", 2)]:
        p2["attempts"] += 1
        key = (2, k, tgt["cpu"])

        def attempt():
            c = user(at, waker["tid"], call, k)
            if c is None:
                return None, "p2_call_missing"
            rd = ready(c, waker["cpu"], tgt["cpu"], 2, k)
            if rd is None:
                return None, "p2_ready_missing"
            sn = user(rd, waker["tid"], sent, k)
            if sn is None:
                return None, "p2_sent_missing"
            # The helper on the target's hart was not parked in receive when readiness was
            # recorded: the send queued the call and owed no wake — ineligible from that record,
            # its window check still owed, no arrival attributed.
            if not (recs[rd]["f"][0] >> 17) & 1:
                w = user(rd, tgt["tid"], ok, k)
                if w is None:
                    return None, "p2_window_check_missing"
                if recs[w]["cpu"] != tgt["cpu"]:
                    return None, "p2_window_checked_elsewhere"
                return (w, "helper_not_parked", None), None
            pb = find(rd, lambda r: r["kind"] == "ipi_published" and r["cpu"] == waker["cpu"]
                      and r["f"][0] == tgt["cpu"] and r["f"][2] == 0)
            if pb is None or pb >= sn:
                return None, "ipi_not_published"
            a, why = chain(pb, waker["cpu"], tgt["cpu"], None, 0)
            if why:
                return None, "ipi_not_published" if why.startswith("no publication") else "ipi_not_consumed"
            w = user(a, tgt["tid"], ok, k)
            if w is None:
                return None, "p2_window_check_missing"
            if recs[w]["cpu"] != tgt["cpu"]:
                return None, "p2_window_checked_elsewhere"
            ar, r = recs[a], recs[rd]
            if arr_entries(ar) <= r["f"][2]:
                return None, "p2_entry_history_inconsistent"
            in_target = arr_origin(ar) == 0 and arr_tid(ar) == tgt["tid"] and arr_asid(ar) == tgt["asid"]
            cls = "in_window" if in_target and arr_window(ar) == win else ("outside" if in_target else "displaced")
            switched = any(x["kind"] == "activation" and x["cpu"] == tgt["cpu"] and x["f"][0] != tgt["asid"] for x in recs[c + 1:a])
            # Readiness is BOTH recorded facts: the bounded wait reported it met, and it saw the
            # target current. Either alone is not readiness.
            if not (r["f"][0] >> 16) & 1 or r["f"][1] != tgt["tid"]:
                reason = "not_ready"
            elif arr_entries(ar) != r["f"][2] + 1:
                reason = "intervening_entry"
            elif switched:
                reason = "switched"
            else:
                reason = "none"
            if reason == "none" and cls != "in_window":
                return None, "ipi_sepc_outside_window" if in_target else "ipi_eligible_arrival_not_in_target"
            return (w, reason, cls), None
        res, why = attempt()
        if why:
            fail("P2 attempt %d -> cpu %d: %s" % (k, tgt["cpu"], why))
            attempts[key] = "0 %s failed" % why
            continue
        w, reason, cls = res
        if cls is not None:
            p2[cls] += 1
        if reason == "none":
            p2["credited_s" if tgt is S else "credited_c"] += 1
            attempts[key] = "1 none credited"
        else:
            p2["uncredited"] += 1
            attempts[key] = "0 %s uncredited" % reason
        if tgt is C:
            at = w
if p2["credited_s"] < 1 or p2["credited_c"] < 1:
    fail("P2: credited attempts %d to S, %d to C (>= 1 each required of %d each)" % (p2["credited_s"], p2["credited_c"], P2_ATTEMPTS))


# ── one production replacement, end to end ──
def replacement(start, req, target):
    b = find(start, lambda r: r["kind"] == "vm_op_begin" and r["cpu"] == req["cpu"] and r["f"][0] == target["asid"]
             and r["f"][1] == W and r["f"][4] == req["tid"])
    if b is None:
        return None, "operation_missing"
    e = None
    for i in range(b + 1, len(recs)):
        r = recs[i]
        if r["cpu"] != req["cpu"]:
            continue
        if r["kind"] == "vm_op_begin" and e is None:
            return None, "operation_incomplete"
        if r["kind"] == "vm_op_end" and r["f"][:3] == [target["asid"], W, recs[b]["f"][3]]:
            if e is not None:
                return None, "operation_duplicate_completion"
            e = i
    if e is None:
        return None, "operation_incomplete"
    if recs[e]["f"][3] != 0 or recs[e]["f"][4] != W:
        return None, "operation_failed"

    def inside(kind, after, extra=lambda r: True):
        i = find(after, lambda r: r["kind"] == kind and r["cpu"] == req["cpu"] and r["f"][0] == target["asid"]
                 and r["f"][1] == W and extra(r))
        return i if i is not None and i < e else None
    disp = inside("vm_displaced", b)
    if disp is None:
        return None, "displacement_outside_operation"
    sh = inside("shoot_targets", disp)
    if sh is None:
        return None, "shoot_targets_missing"
    mask = recs[sh]["f"][2]
    if mask & (1 << req["cpu"]):
        return None, "shoot_targets_named_requester"
    if target["cpu"] not in (0, 1):
        return None, "shoot_targets_cpu_unrecorded"
    resident = recs[sh]["f"][3 + target["cpu"]] == target["tid"]
    if bool(mask & (1 << target["cpu"])) != resident:
        return None, "shoot_targets_inconsistent"
    d = None
    if resident:
        fr = inside("fence_request", sh, lambda r: r["f"][2] & (1 << target["hart"]) and r["f"][4] & (1 << target["cpu"]))
        if fr is None:
            return None, "fence_not_requested_for_the_resident_target"
        if recs[fr]["f"][2] & (1 << req["hart"]):
            return None, "requester_fenced_itself"
        dones = [i for i in range(fr + 1, len(recs)) if recs[i]["kind"] == "fence_done" and recs[i]["cpu"] == req["cpu"]
                 and recs[i]["f"][:3] == recs[fr]["f"][:3]]
        if not dones:
            return None, "fence_completion_missing"
        if recs[dones[0]]["f"][3] != recs[fr]["f"][3]:
            return None, "fence_stale_completion"
        if len([i for i in dones if recs[i]["f"][3] == recs[fr]["f"][3]]) != 1:
            return None, "fence_duplicate_completion"
        d = dones[0]
        if recs[d]["f"][4] != 0:
            return None, "fence_failed"
        if d > e:
            return None, "fence_completion_outside_operation"
    sd = inside("vm_shootdown", d if d is not None else sh)
    if sd is None:
        return None, "shootdown_outside_operation"
    if recs[sd]["f"][2] != 1:
        return None, "shootdown_not_acknowledged"
    old = recs[disp]["f"][2]
    if inside("vm_settled", sd, lambda r: r["f"][2] == old) is None:
        return None, "settlement_outside_operation"
    return dict(b=b, e=e, disp=disp, d=d, resident=resident), None


def residency(tid, step, rnd):
    i = find(0, lambda r: r["kind"] == "residency" and r["f"][0] == tid and r["step"] == step and r["f"][3] == rnd)
    return None if i is None else recs[i]["f"][1]


# ── REMOTE FENCE (P3, serial) ──
tlb = dict(rounds=0, credited_s=0, credited_c=0, interfered=0, off_cpu=0, not_ready=0)
for rnd in range(1, TLB_ROUNDS + 1):
    T, Q, tp, qp = (S, C, "S", "C") if rnd % 2 else (C, S, "C", "S")
    key = (3, rnd, T["cpu"])

    def attempt():
        pre = user(0, T["tid"], tp + "_PRE", rnd)
        if pre is None:
            return None, "tlb_pre_missing"
        req = user(pre, Q["tid"], qp + "_REQ", rnd)
        if req is None:
            return None, "tlb_request_step_missing"
        rd = ready(req, Q["cpu"], T["cpu"], 3, rnd)
        if rd is None:
            return None, "tlb_ready_missing"
        rep, why = replacement(rd, Q, T)
        if why:
            return None, why
        o = user(pre, T["tid"], tp + "_OBSERVED", rnd)
        if o is None:
            return None, "tlb_observation_missing"
        if o < rep["e"] or (rep["d"] is not None and o < rep["d"]):
            return None, "tlb_observed_before_completion"
        if recs[o]["cpu"] != T["cpu"] or recs[pre]["cpu"] != T["cpu"] or recs[req]["cpu"] != Q["cpu"]:
            return None, "tlb_roles_off_their_harts"
        c0, c1 = residency(T["tid"], tp + "_PRE", rnd), residency(T["tid"], tp + "_OBSERVED", rnd)
        if c0 is None or c1 is None:
            return None, "tlb_residency_missing"
        if c1 <= c0:
            return None, "tlb_residency_count_not_monotone"
        if not rep["resident"] and not reactivated(T, rep["disp"], o):
            return None, "off_cpu_target_not_reactivated"
        if not (recs[rd]["f"][0] >> 16) & 1 or recs[rd]["f"][1] != T["tid"]:
            return "not_ready", None
        if not rep["resident"]:
            return "off_cpu", None
        if c1 != c0 + 1:
            return "interfered", None
        return "none", None
    reason, why = attempt()
    if why:
        fail("fence attempt %d (target %s): %s" % (rnd, tp, why))
        attempts[key] = "0 %s failed" % why
        continue
    tlb["rounds"] += 1
    if reason == "none":
        tlb["credited_s" if T is S else "credited_c"] += 1
        attempts[key] = "1 none credited"
    else:
        tlb[reason] += 1
        attempts[key] = "0 %s uncredited" % reason
if tlb["credited_s"] < 2 or tlb["credited_c"] < 2:
    fail("credited fence attempts: %d with S resident on CPU 1, %d with C resident on CPU 0 (>= 2 each of %d each)" % (
        tlb["credited_s"], tlb["credited_c"], TLB_ROUNDS // 2))

# ── MUTUAL PROGRESS (P4): obligations, all of them ──
mutual, overlapped, contended, mutual_off_cpu = 0, 0, 0, 0
for mr in range(1, MUT_ROUNDS + 1):
    sn, cn = user(0, S["tid"], "S_MUT_NR3", mr), user(0, C["tid"], "C_MUT_NR3", mr)
    if sn is None or cn is None:
        fail("mutual %d: announcement steps missing" % mr)
        attempts[(4, mr, 0)] = "0 mut_request_missing failed"
        continue
    rs, ws = replacement(sn, S, C)
    rc, wc = replacement(cn, C, S)
    if ws or wc:
        fail("mutual %d: %s" % (mr, " / ".join(w for w in (ws and "S: " + ws, wc and "C: " + wc) if w)))
        attempts[(4, mr, 0)] = "0 %s failed" % ("mut_s_operation_invalid" if ws else "mut_c_operation_invalid")
        continue
    so, co = user(0, S["tid"], "S_MUT_OK", mr), user(0, C["tid"], "C_MUT_OK", mr)
    if so is None or co is None:
        fail("mutual %d: an observation is missing" % mr)
        attempts[(4, mr, 0)] = "0 mut_observation_missing failed"
        continue
    if so < rc["e"] or co < rs["e"]:
        fail("mutual %d: an observation precedes the other operation's completion" % mr)
        attempts[(4, mr, 0)] = "0 mut_observed_before_operation_completed failed"
        continue
    if (not rs["resident"] and not reactivated(C, rs["disp"], co)) or (not rc["resident"] and not reactivated(S, rc["disp"], so)):
        fail("mutual %d: an off-CPU target was not re-activated before its observation" % mr)
        attempts[(4, mr, 0)] = "0 off_cpu_target_not_reactivated failed"
        continue
    mutual += 1
    for op, tgt in ((rs, C), (rc, S)):
        mutual_off_cpu += not op["resident"]
        attempts[(4, mr, tgt["cpu"])] = "1 %s verified" % ("resident" if op["resident"] else "off_cpu")
    ov = max(rs["b"], rc["b"]) < min(rs["e"], rc["e"])
    overlapped += int(ov)
    first, last = min(rs["b"], rc["b"]), max(rs["e"], rc["e"])
    c_in = find(first, lambda r: r["kind"] == "contention" and r["f"][1] == 0)
    c_out = find(last, lambda r: r["kind"] == "contention" and r["f"][1] == 1)
    cn_ = (recs[c_out]["f"][0] - recs[c_in]["f"][0]) if c_in is not None and c_out is not None else 0
    contended += cn_
    print("[smp3-witness] mutual %d: S op seq %d..%d (cpu 1, C %s), C op seq %d..%d (cpu 0, S %s): %s, contended acquisitions %d" % (
        mr, recs[rs["b"]]["seq"], recs[rs["e"]]["seq"], "resident" if rs["resident"] else "off-CPU",
        recs[rc["b"]]["seq"], recs[rc["e"]]["seq"], "resident" if rc["resident"] else "off-CPU",
        "OVERLAPPED" if ov else "SERIALIZED", cn_))
    if not ov:
        fail("mutual %d: the two production operations did not overlap" % mr)

# ── premature release, globally: the owed shootdown completed, then exactly one settlement ──
settled_ok, settled_local = 0, 0
hart_of = {S["cpu"]: S["hart"], C["cpu"]: C["hart"]}
for i, r in enumerate(recs):
    if r["kind"] != "vm_displaced" or r["f"][1] != W:
        continue
    st = [j for j in range(i + 1, len(recs)) if recs[j]["kind"] == "vm_settled" and recs[j]["f"][:3] == r["f"][:3]]
    if len(st) != 1:
        fail("seq %d: displaced page settled %d time(s)" % (r["seq"], len(st)))
        continue

    def same(x, kind):
        return x["kind"] == kind and x["cpu"] == r["cpu"] and x["f"][:2] == r["f"][:2]
    sh = next((j for j in range(i + 1, st[0]) if same(recs[j], "shoot_targets")), None)
    if sh is None:
        fail("seq %d: displaced page settled with no recorded shootdown target computation" % r["seq"])
        continue
    ack = next((j for j in range(sh + 1, st[0]) if same(recs[j], "vm_shootdown") and recs[j]["f"][2] == 1), None)
    if ack is None:
        fail("seq %d: displaced page settled before its shootdown was acknowledged" % r["seq"])
        continue
    mask = recs[sh]["f"][2]
    if mask == 0:
        settled_local += 1
        continue
    need = 0
    for cpu in range(64):
        if mask & (1 << cpu):
            need |= (1 << hart_of[cpu]) if cpu in hart_of else (1 << 63)
    if not any(same(recs[j], "fence_done") and recs[j]["f"][4] == 0 and recs[j]["f"][2] & need == need for j in range(sh + 1, ack)):
        fail("seq %d: displaced page settled before any completed firmware fence" % r["seq"])
        continue
    settled_ok += 1

# ── CONTEXT ──
for step, want in {"S_P1_RESUMED": P1_ROUNDS, "C_P1_RESUMED": P1_ROUNDS, "S_WIN_A_OK": P2_ATTEMPTS, "C_WIN_B_OK": P2_ATTEMPTS,
                   "S_OBSERVED": TLB_ROUNDS // 2, "C_OBSERVED": TLB_ROUNDS // 2, "S_MUT_OK": MUT_ROUNDS, "C_MUT_OK": MUT_ROUNDS,
                   "S_DONE": 1, "C_DONE": 1}.items():
    n = sum(r["kind"] == "user" and r["step"] == step for r in recs)
    if n != want:
        fail("step %s seen %d time(s), want %d" % (step, n, want))

# ── the kernel verifier must agree: every attempt line, then every count ──
kernel_attempts = {}
for k, body in dump.items():
    if not k.startswith("ATTEMPT "):
        continue
    m = re.match(r"SMP3_ATTEMPT phase=(\d+) n=(\d+) target_cpu=(\d+) gen=0x[0-9a-f]+ eligible=([01]) reason=(\w+) outcome=(\w+)$", body)
    if not m:
        fail("malformed kernel attempt line: %r" % body)
        continue
    kernel_attempts[(int(m.group(1)), int(m.group(2)), int(m.group(3)))] = "%s %s %s" % m.group(4, 5, 6)
for key in sorted(set(attempts) | set(kernel_attempts)):
    if attempts.get(key) != kernel_attempts.get(key):
        fail("attempt phase=%d n=%d target_cpu=%d: grader %s, kernel %s" % (key + (attempts.get(key), kernel_attempts.get(key))))
for want in ["arrivals=%d" % arrivals, "consumed=%d" % consumed, "empty=%d" % empty, "merged=%d" % merged,
             "user_fp_vs_off=%d" % fpvs, "p1_parked=%d" % (2 * P1_ROUNDS), "p1_ipi_to_s=%d" % ipi_to_s, "p1_ipi_to_c=%d" % ipi_to_c,
             "p1_timer_first=%d" % timer_first, "p1_busy=%d" % busy, "p1_preceded=%d" % preceded,
             "p2_attempts=%d" % p2["attempts"], "p2_credited_s=%d" % p2["credited_s"], "p2_credited_c=%d" % p2["credited_c"],
             "p2_uncredited=%d" % p2["uncredited"], "p2_in_window=%d" % p2["in_window"], "p2_outside=%d" % p2["outside"],
             "p2_displaced=%d" % p2["displaced"], "tlb_rounds=%d" % tlb["rounds"], "credited_s=%d" % tlb["credited_s"],
             "credited_c=%d" % tlb["credited_c"], "interfered=%d" % tlb["interfered"], "off_cpu=%d" % tlb["off_cpu"],
             "not_ready=%d" % tlb["not_ready"], "mutual_rounds=%d" % mutual, "overlapped=%d" % overlapped,
             "contended=%d" % contended, "mutual_off_cpu=%d" % mutual_off_cpu,
             "settled_after_completion=%d" % settled_ok, "settled_local_only=%d" % settled_local]:
    if want not in counts.split():
        fail("kernel counts disagree on %s: %s" % (want, counts))
if not verdict.startswith("SMP3_VERDICT result=ok "):
    fail("kernel verdict: %s" % verdict)

summary = ("records=%d damaged_lines=%d arrivals=%d consumed=%d empty=%d merged=%d p1=%d ipi_to_s=%d ipi_to_c=%d "
           "timer_first=%d busy=%d preceded=%d p2_attempts=%d p2_credited_s=%d p2_credited_c=%d p2_uncredited=%d "
           "p2_outside=%d p2_displaced=%d fence_rounds=%d credited_s=%d credited_c=%d interfered=%d off_cpu=%d not_ready=%d "
           "mutual=%d overlapped=%d mutual_off_cpu=%d contended=%d settled=%d settled_local=%d") % (
    len(recs), damaged, arrivals, consumed, empty, merged, parked_n, ipi_to_s, ipi_to_c, timer_first, busy,
    preceded, p2["attempts"], p2["credited_s"], p2["credited_c"], p2["uncredited"], p2["outside"], p2["displaced"],
    tlb["rounds"], tlb["credited_s"], tlb["credited_c"], tlb["interfered"], tlb["off_cpu"], tlb["not_ready"],
    mutual, overlapped, mutual_off_cpu, contended, settled_ok, settled_local)
print("[smp3-witness] " + summary)
print("[smp3-witness] kernel: " + counts + " | " + verdict)
print("[smp3-witness] " + dump.get("SMP3_SYNC", "SMP3_SYNC missing"))
print("[smp3-witness] " + dump.get("SMP3_SEAL_SYNC", "SMP3_SEAL_SYNC missing"))
for key in sorted(attempts):
    if attempts[key].split()[2] != "credited":
        print("[smp3-witness] attempt phase=%d n=%d target_cpu=%d: %s" % (key + (attempts[key],)))
for k in sorted(dump):
    if k.startswith("SMP3_CPU_"):
        print("[smp3-witness] " + dump[k])
print("[smp3-witness] firmware: %s | %s" % ((osbi or "").strip(), (sbi or "").strip()))
print("[smp3-witness] " + dump.get("SMP3_BRINGUP", "SMP3_BRINGUP missing"))
if live_damaged:
    print("[smp3-witness] live secondary markers interleaved on the shared console (graded from the sealed line): " + ", ".join(live_damaged))
# Reported, not graded: present at the same rate in the base boots (it scales with boot length).
print("[smp3-witness] pre-existing RISCV_ASYNC_RESUME_REFUSED lines: %d" % sum("RISCV_ASYNC_RESUME_REFUSED" in l for l in lines))
print("[smp3-witness] overtaken-deferral settlements: %d" % sum("RISCV_OVERTAKEN_DEFERRAL_SETTLED" in l for l in lines))
print("[smp3-witness] replies retried while the caller was still blocking: %d" % sum("IPCREPLY_DIRECT_CALLER_NOT_YET_BLOCKED" in l for l in lines))
# The phase totals, each a named failure rather than a silent seal condition.
for name, got, want in [("parked-target resumes", parked_n, 2 * P1_ROUNDS), ("fence attempts", tlb["rounds"], TLB_ROUNDS),
                        ("P2 attempts", p2["attempts"], 2 * P2_ATTEMPTS),
                        ("mutual rounds", mutual, MUT_ROUNDS), ("overlapped mutual rounds", overlapped, MUT_ROUNDS)]:
    if got != want:
        fail("%s: %d, want %d" % (name, got, want))
for f in fails:
    print("[smp3-witness][fail] " + f)
ok = not fails
print("SMP3_WITNESS_SEAL %s result=%s" % (summary, "ok" if ok else "fail"))
sys.exit(0 if ok else 1)
