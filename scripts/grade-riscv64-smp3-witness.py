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


KEYS = r"SMP3_(REC|ROLES|BRINGUP|CPU_IPI|CPU_FENCE|COUNTS_IPI|COUNTS_WAKE|COUNTS_TLB|SYNC|VERDICT) "
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
    if window and recs[a]["f"][1] >> 8 != window:
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
    if recs[a]["f"][3] != woken:
        return None, "arrival seq %d in tid %d, not the resumed tid %d" % (recs[a]["seq"], recs[a]["f"][3], woken)
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

# ── USER TARGETS: P2_ROUNDS attempts per direction ──
# An arrival from U-mode IN the target must be inside its window (credited). An arrival that found
# another task resident, or the hart idle with the target displaced, is counted apart. Every
# attempt's window check must follow on the target's own CPU; each direction needs >= 1 credit.
P2_ROUNDS = 4
user_n, displaced = 0, 0
per_dir = [0, 0]
for k in range(1, P2_ROUNDS + 1):
    for d, (waker, src, dst, call, tgt, ok, win) in enumerate([
            (C, 0, 1, "C_P2A_CALL", S, "S_WIN_A_OK", 1),
            (S, 1, 0, "S_P2B_CALL", C, "C_WIN_B_OK", 2)]):
        c = user(at, waker["tid"], call, k)
        if c is None:
            fail("P2 attempt %d: %s missing" % (k, call))
            continue
        a, why = chain(c, src, dst, None, 0)
        if why:
            fail("P2 attempt %d %d->%d: %s" % (k, src, dst, why))
            continue
        r = recs[a]
        if r["f"][1] & 0xFF == 0 and r["f"][3] == tgt["tid"]:
            if r["f"][1] >> 8 != win:
                fail("P2 attempt %d: arrival seq %d in the target but sepc 0x%x outside window %d" % (k, r["seq"], r["f"][2], win))
                continue
            user_n += 1
            per_dir[d] += 1
        else:
            displaced += 1
        w = user(a, tgt["tid"], ok, k)
        if w is None or recs[w]["cpu"] != dst:
            fail("P2 attempt %d: %s missing after the arrival or on another CPU" % (k, ok))
            continue
        if d == 1:
            at = w
if per_dir[0] < 1 or per_dir[1] < 1:
    fail("P2: arrivals inside a resident target's window: %d to S, %d to C (>= 1 each required)" % tuple(per_dir))


# ── one production replacement, end to end ──
def replacement(start, req, target):
    b = find(start, lambda r: r["kind"] == "vm_op_begin" and r["cpu"] == req["cpu"] and r["f"][0] == target["asid"]
             and r["f"][1] == W and r["f"][4] == req["tid"])
    if b is None:
        return None, "no production operation"
    e = None
    for i in range(b + 1, len(recs)):
        r = recs[i]
        if r["cpu"] != req["cpu"]:
            continue
        if r["kind"] == "vm_op_begin" and e is None:
            return None, "operation seq %d never completed" % recs[b]["seq"]
        if r["kind"] == "vm_op_end" and r["f"][:3] == [target["asid"], W, recs[b]["f"][3]]:
            if e is not None:
                return None, "operation seq %d completed twice" % recs[b]["seq"]
            e = i
    if e is None:
        return None, "operation seq %d never completed" % recs[b]["seq"]
    if recs[e]["f"][3] != 0 or recs[e]["f"][4] != W:
        return None, "operation seq %d returned outcome %d addr 0x%x" % (recs[b]["seq"], recs[e]["f"][3], recs[e]["f"][4])

    def inside(kind, after, extra=lambda r: True):
        i = find(after, lambda r: r["kind"] == kind and r["cpu"] == req["cpu"] and r["f"][0] == target["asid"]
                 and r["f"][1] == W and extra(r))
        return i if i is not None and i < e else None
    disp = inside("vm_displaced", b)
    if disp is None:
        return None, "no displacement inside the operation"
    fr = inside("fence_request", disp, lambda r: r["f"][2] & (1 << target["hart"]) and r["f"][4] & (1 << target["cpu"]))
    if fr is None:
        return None, "no fence request naming the target's hart %d inside the operation" % target["hart"]
    if recs[fr]["f"][2] & (1 << req["hart"]):
        return None, "the requester fenced itself through the firmware"
    dones = [i for i in range(fr + 1, len(recs)) if recs[i]["kind"] == "fence_done" and recs[i]["cpu"] == req["cpu"]
             and recs[i]["f"][:3] == recs[fr]["f"][:3]]
    if not dones or recs[dones[0]]["f"][3] != recs[fr]["f"][3]:
        return None, "the first completion for this fence is not this request's generation"
    if len([i for i in dones if recs[i]["f"][3] == recs[fr]["f"][3]]) != 1:
        return None, "the fence completed more than once"
    d = dones[0]
    if recs[d]["f"][4] != 0:
        return None, "the firmware fence failed (0x%x)" % recs[d]["f"][4]
    if d > e:
        return None, "the fence completed outside the operation"
    sd = inside("vm_shootdown", d)
    if sd is None or recs[sd]["f"][2] != 1:
        return None, "the shootdown owner did not acknowledge after the fence completed"
    old = recs[disp]["f"][2]
    st = [i for i, r in enumerate(recs) if r["kind"] == "vm_settled" and r["f"][0] == target["asid"] and r["f"][2] == old]
    if len(st) != 1 or not (sd < st[0] < e):
        return None, "displaced frame settled %d time(s), not once between the acknowledgement and the return" % len(st)
    return (b, e, d), None


def residency(tid, step, rnd):
    i = find(0, lambda r: r["kind"] == "residency" and r["f"][0] == tid and r["step"] == step and r["f"][3] == rnd)
    return None if i is None else recs[i]["f"][1]


# ── REMOTE FENCE (serial) ──
tlb, credited_s, credited_c, interfered = 0, 0, 0, 0
for rnd in range(1, 9):
    T, Q, tp, qp = (S, C, "S", "C") if rnd % 2 else (C, S, "C", "S")
    pre = user(0, T["tid"], tp + "_PRE", rnd)
    req = user(pre or 0, Q["tid"], qp + "_REQ", rnd) if pre is not None else None
    if pre is None or req is None:
        fail("fence round %d: PRE/REQ step missing" % rnd)
        continue
    if recs[pre]["cpu"] != T["cpu"] or recs[req]["cpu"] != Q["cpu"]:
        fail("fence round %d: target/requester not on their harts" % rnd)
        continue
    res, why = replacement(req, Q, T)
    if why:
        fail("fence round %d: %s" % (rnd, why))
        continue
    b, e, d = res
    o = user(pre, T["tid"], tp + "_OBSERVED", rnd)
    if o is None or o < e or recs[o]["cpu"] != T["cpu"]:
        fail("fence round %d: target observation missing, before the operation returned, or on another hart" % rnd)
        continue
    c0, c1 = residency(T["tid"], tp + "_PRE", rnd), residency(T["tid"], tp + "_OBSERVED", rnd)
    if c0 is None or c1 is None or c1 <= c0:
        fail("fence round %d: residency counts missing or not monotone (%s, %s)" % (rnd, c0, c1))
        continue
    tlb += 1
    if c1 == c0 + 1:
        credited_s += T is S
        credited_c += T is C
    else:
        interfered += 1
        print("[smp3-witness] fence round %d: target %s took %d other supervisor entr(ies) in its window — not credited" % (rnd, tp, c1 - c0 - 1))
if credited_s < 2 or credited_c < 2:
    fail("credited fence rounds: %d with S resident on CPU 1, %d with C resident on CPU 0 (>= 2 each)" % (credited_s, credited_c))

# ── MUTUAL PROGRESS ──
mutual, overlapped, contended = 0, 0, 0
for mr in (1, 2, 3, 4):
    sn, cn = user(0, S["tid"], "S_MUT_NR3", mr), user(0, C["tid"], "C_MUT_NR3", mr)
    if sn is None or cn is None:
        fail("mutual %d: announcement steps missing" % mr)
        continue
    rs, ws = replacement(sn, S, C)
    rc, wc = replacement(cn, C, S)
    if ws or wc:
        fail("mutual %d: %s" % (mr, " / ".join(w for w in (ws and "S: " + ws, wc and "C: " + wc) if w)))
        continue
    so, co = user(0, S["tid"], "S_MUT_OK", mr), user(0, C["tid"], "C_MUT_OK", mr)
    if so is None or co is None or so < rc[1] or co < rs[1]:
        fail("mutual %d: an observation is missing or precedes the other operation's completion" % mr)
        continue
    mutual += 1
    ov = max(rs[0], rc[0]) < min(rs[1], rc[1])
    overlapped += int(ov)
    first, last = min(rs[0], rc[0]), max(rs[1], rc[1])
    c_in = find(first, lambda r: r["kind"] == "contention" and r["f"][1] == 0)
    c_out = find(last, lambda r: r["kind"] == "contention" and r["f"][1] == 1)
    cn_ = (recs[c_out]["f"][0] - recs[c_in]["f"][0]) if c_in is not None and c_out is not None else 0
    contended += cn_
    print("[smp3-witness] mutual %d: S op seq %d..%d (cpu 1), C op seq %d..%d (cpu 0): %s, contended acquisitions %d" % (
        mr, recs[rs[0]]["seq"], recs[rs[1]]["seq"], recs[rc[0]]["seq"], recs[rc[1]]["seq"],
        "OVERLAPPED" if ov else "SERIALIZED", cn_))
    if not ov:
        fail("mutual %d: the two production operations did not overlap" % mr)

# ── premature release, globally ──
settled_ok = 0
for i, r in enumerate(recs):
    if r["kind"] != "vm_displaced" or r["f"][1] != W:
        continue
    st = [j for j in range(i + 1, len(recs)) if recs[j]["kind"] == "vm_settled" and recs[j]["f"][:3] == r["f"][:3]]
    if len(st) != 1:
        fail("seq %d: displaced page settled %d time(s)" % (r["seq"], len(st)))
        continue
    if not any(x["kind"] == "fence_done" and x["cpu"] == r["cpu"] and x["f"][:2] == r["f"][:2] and x["f"][4] == 0
               for x in recs[i:st[0]]):
        fail("seq %d: displaced page settled before any completed firmware fence" % r["seq"])
        continue
    settled_ok += 1

# ── CONTEXT ──
for step, want in {"S_P1_RESUMED": P1_ROUNDS, "C_P1_RESUMED": P1_ROUNDS, "S_WIN_A_OK": P2_ROUNDS, "C_WIN_B_OK": P2_ROUNDS,
                   "S_OBSERVED": 4, "C_OBSERVED": 4, "S_MUT_OK": 4, "C_MUT_OK": 4, "S_DONE": 1, "C_DONE": 1}.items():
    n = sum(r["kind"] == "user" and r["step"] == step for r in recs)
    if n != want:
        fail("step %s seen %d time(s), want %d" % (step, n, want))

# ── the kernel verifier must agree ──
for want in ["arrivals=%d" % arrivals, "consumed=%d" % consumed, "empty=%d" % empty, "merged=%d" % merged,
             "user_fp_vs_off=%d" % fpvs, "p1_parked=%d" % (2 * P1_ROUNDS), "p1_ipi_to_s=%d" % ipi_to_s, "p1_ipi_to_c=%d" % ipi_to_c,
             "p1_timer_first=%d" % timer_first, "p1_busy=%d" % busy, "p1_preceded=%d" % preceded, "p2_user=%d" % user_n, "p2_displaced=%d" % displaced, "tlb_rounds=%d" % tlb,
             "credited_s=%d" % credited_s, "credited_c=%d" % credited_c, "interfered=%d" % interfered,
             "mutual_rounds=%d" % mutual, "overlapped=%d" % overlapped, "contended=%d" % contended,
             "settled_after_completion=%d" % settled_ok]:
    if want not in counts.split():
        fail("kernel counts disagree on %s: %s" % (want, counts))
if not verdict.startswith("SMP3_VERDICT result=ok "):
    fail("kernel verdict: %s" % verdict)

summary = ("records=%d damaged_lines=%d arrivals=%d consumed=%d empty=%d merged=%d p1=%d ipi_to_s=%d ipi_to_c=%d "
           "timer_first=%d busy=%d preceded=%d user=%d displaced=%d fence_rounds=%d credited_s=%d credited_c=%d interfered=%d mutual=%d "
           "overlapped=%d contended=%d settled=%d") % (
    len(recs), damaged, arrivals, consumed, empty, merged, parked_n, ipi_to_s, ipi_to_c, timer_first, busy,
    preceded, user_n, displaced, tlb, credited_s, credited_c, interfered, mutual, overlapped, contended, settled_ok)
print("[smp3-witness] " + summary)
print("[smp3-witness] kernel: " + counts + " | " + verdict)
print("[smp3-witness] " + dump.get("SMP3_SYNC", "SMP3_SYNC missing"))
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
# The phase totals, each a named failure rather than a silent seal condition.
for name, got, want in [("parked-target resumes", parked_n, 2 * P1_ROUNDS), ("fence rounds", tlb, 8),
                        ("mutual rounds", mutual, 4), ("overlapped mutual rounds", overlapped, 4)]:
    if got != want:
        fail("%s: %d, want %d" % (name, got, want))
if user_n < 2:
    fail("in-window user-target arrivals: %d, want >= 2 (one per direction)" % user_n)
for f in fails:
    print("[smp3-witness][fail] " + f)
ok = not fails
print("SMP3_WITNESS_SEAL %s result=%s" % (summary, "ok" if ok else "fail"))
sys.exit(0 if ok else 1)
