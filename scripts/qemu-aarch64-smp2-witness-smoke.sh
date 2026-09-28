#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# QEMU-SMP2 — the AArch64 two-CPU interrupt / TLB / context witness, on QEMU virt / cortex-a72 /
# 1024M / -smp 2 with `yarm.ap_user_dispatch=1` and the existing GICv2.
#
# Usage: scripts/qemu-aarch64-smp2-witness-smoke.sh
#   LOGDIR=...     where the build, the artifact identity and the boot log land
#                  (default /tmp/qemu-aarch64-smp2-witness); every run should get its own
#   SKIP_BUILD=1   reuse the artifacts already in $LOGDIR/build
#   REGRADE=1      grade the existing $LOGDIR/boot.log again without booting (implies SKIP_BUILD)
#   TIMEOUT_SECS   boot budget (default 240)
#
# One boot built with `aarch64-smp2-witness`. The grader NEVER retries, and it does not take the
# kernel's own verdict on trust: it re-derives every graded edge from the raw sealed record
# (`SMP2_REC` lines, printed synchronously after both witness tasks finished), and additionally
# requires the kernel's verifier to agree.
#
# Graded, each separately:
#   * BRING-UP: each CPU derived its OWN GIC interface bit from its banked ITARGETSR0 (distinct,
#     one bit each); CPU 1 admitted to pinned dispatch in one transition; its two tasks placed and
#     one start-up kick taken.
#   * SGI POPULATION: every arrival is explained by an earlier send from its source to its CPU
#     (the send is recorded BEFORE the GICD_SGIR write), carries INTID 1 and the sender's
#     interface in its IAR source field, and is completed exactly once with its full token before
#     that CPU's next arrival; no send is left undelivered; every GICD_SGIR value encodes the
#     target's derived interface.
#   * PARKED TARGETS (8): C->S and S->C four times each: the call/reply step, the send, the
#     arrival, the idle advance resuming exactly the woken task, and that task's own
#     context-checked resume step on the target CPU. The SGI must drive the dispatch at the
#     target's idle boundary every time on CPU 1 (it has no timer) and at least twice on CPU 0,
#     where the periodic tick's idle advance may legitimately win the race — accepted only when
#     it resumed exactly the woken task and the SGI then arrived in that task.
#   * EL0 TARGETS (2): C->H1 while S spins on CPU 1, S->H0 while C spins on CPU 0: the arrival is
#     from EL0, in the resident task, with ELR inside its register-checked window, followed by that
#     window's own passing check.
#   * REMOTE INVALIDATION (4 serial rounds): target primed on CPU T; requester on the other CPU
#     arms the no-invalidation probe, then its NR 3 breaks the target's W and records one request
#     (asid, va, old PA, generation) whose completion carries the same identity; the displaced
#     frame is pinned, its shootdown answered, and it is settled only after that completion; the
#     target, still on CPU T with its probe unchanged, observes the new W after the completion.
#   * MUTUAL PROGRESS (4 rounds): both requests and both completions per round, both observations;
#     the rounds in which the two requests were in flight together are reported and must be >= 1.
#   * CONTEXT: every context-checked step is present (a failed check reports a FAIL step instead).
#   * NOTHING FATAL, no user failure step, no broad-entry or unrouted marker.
set -uo pipefail
cd "$(dirname "$0")/.."

LOGDIR=${LOGDIR:-/tmp/qemu-aarch64-smp2-witness}
mkdir -p "$LOGDIR"
BUILD_DIR="$LOGDIR/build"
BOOT_LOG="$LOGDIR/boot.log"
QEMU_BIN=${QEMU_BIN:-qemu-system-aarch64}

if [[ "${SKIP_BUILD:-0}" != "1" && "${REGRADE:-0}" != "1" ]]; then
  echo "[smp2-witness] building aarch64 artifacts with aarch64-smp2-witness into $BUILD_DIR"
  rm -rf "$BUILD_DIR"
  OUT_DIR="$BUILD_DIR" BOOTSTRAP_FEATURE_ARGS="--no-default-features --features aarch64-smp2-witness" \
    scripts/build-qemu-aarch64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
    || { echo "SMP2_WITNESS_SEAL result=fail reason=build"; exit 1; }
  {
    echo "tree=$(git rev-parse HEAD^{tree} 2>/dev/null) head=$(git rev-parse HEAD 2>/dev/null) dirty=$(git status --porcelain | wc -l)"
    sha256sum "$BUILD_DIR/yarm-aarch64.bin" "$BUILD_DIR/initramfs-core.cpio"
  } >"$LOGDIR/artifact-identity.txt"
fi

if [[ "${REGRADE:-0}" != "1" ]]; then
  echo "[smp2-witness] booting -smp 2 yarm.ap_user_dispatch=1"
  python3 - "$QEMU_BIN" "$BUILD_DIR" "$BOOT_LOG" "${TIMEOUT_SECS:-240}" <<'PY'
import subprocess, sys, time, os, select
qemu, build, log, budget = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
args = [qemu, "-machine", "virt", "-cpu", "cortex-a72", "-m", "1024M", "-smp", "2",
        "-nographic", "-monitor", "none", "-serial", "stdio", "-no-reboot", "-no-shutdown",
        "-kernel", os.path.join(build, "yarm-aarch64.bin"),
        "-initrd", os.path.join(build, "initramfs-core.cpio"),
        "-append", "yarm.ap_user_dispatch=1"]
p = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
start, seen, buf = time.time(), None, b""
with open(log, "wb") as out:
    while True:
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            out.write(chunk); out.flush(); buf = (buf + chunk)[-4096:]
            if seen is None and b"SMP2_VERDICT" in buf:
                seen = time.time()
        now = time.time()
        if (seen is not None and now - seen > 3) or now - start > budget:
            break
p.kill(); p.wait()
print("[smp2-witness] boot %s after %.1fs" % ("sealed" if seen else "TIMED OUT", time.time() - start))
PY
fi

python3 - "$BOOT_LOG" <<'PY'
import re, sys
text = open(sys.argv[1], "rb").read().decode("utf-8", "replace").replace("\r", "\n")
lines = text.split("\n")
fails = []
def fail(msg): fails.append(msg)

# ── BRING-UP ──
ready = {}
for l in lines:
    m = re.search(r"AARCH64_SMP2_SGI_READY cpu=(\d+) interface_mask=0x([0-9a-f]+) itargetsr0=0x([0-9a-f]+) intid=1 priority=0x40 pmr=0xff result=ok", l)
    if m:
        cpu, mask, it = int(m.group(1)), int(m.group(2), 16), int(m.group(3), 16)
        if (it & 0xff) != mask or bin(mask).count("1") != 1:
            fail("cpu%d published mask 0x%x not derived from ITARGETSR0 0x%08x" % (cpu, mask, it))
        ready[cpu] = mask
if sorted(ready) != [0, 1] or ready[0] == ready[1]:
    fail("both CPUs must publish distinct interface bits: %r" % ready)
iface_cpu = {m.bit_length() - 1: c for c, m in ready.items()}
for pat in ["AARCH64_SMP2_AP_ADMITTED cpu=1 wake_only=0 balance_excluded=1",
            "SMP2_WITNESS_AP_PLACED cpu=1 tids=9202,9200 kick=1",
            "SMP2_WITNESS_PROVISIONED"]:
    if sum(pat in l for l in lines) != 1:
        fail("expected exactly one '%s'" % pat)

# ── NOTHING FATAL ──
for bad in ["PANIC", "SMP2_USER_FAIL", "SMP2_WITNESS_PROVISION_FAIL", "AARCH64_SMP2_AP_UNADMITTED",
            "AARCH64_SMP2_AP_ADMISSION_REFUSED", "_BROAD_ENTRY", "_UNROUTED", "IRQ1_UNKNOWN_SETTLED",
            "AP_CPU_IDENTITY_VIOLATION", "SMP2_WITNESS_R_EXHAUSTED"]:
    n = sum(bad in l for l in lines)
    if n:
        fail("%d line(s) with %s" % (n, bad))

# ── the sealed record ──
# Every dump line is printed twice with an FNV-1a checksum of its text; a line is taken only from
# an intact copy (another CPU's raw UART markers can land mid-line), and a record whose copies
# are both damaged is missing.
def fnv1a(b):
    h = 0x811c9dc5
    for x in b:
        h = ((h ^ x) * 0x01000193) & 0xffffffff
    return h
dump = {}
damaged = 0
for l in lines:
    i, j = l.find("SMP2_"), l.rfind(" pass=")
    m = re.search(r" pass=([12]) crc=0x([0-9a-f]{8})$", l.rstrip())
    if i < 0 or j < i or not m or not re.match(r"SMP2_(REC|ROLES|CPU|COUNTS|VERDICT) ", l[i:]):
        if " pass=" in l and "crc=" in l: damaged += 1
        continue
    text = l[i:j]
    if fnv1a(text.encode()) != int(m.group(2), 16):
        damaged += 1
        continue
    key = text.split(" ", 1)[0]
    if key == "SMP2_REC":
        key = "REC " + re.search(r"seq=(\d+)", text).group(1)
    elif key in ("SMP2_COUNTS", "SMP2_CPU"):
        key = key + " " + text.split(" ", 2)[1]
    dump.setdefault(key, text)
roles = None
m = re.search(r"SMP2_ROLES s_tid=(\d+) s_asid=(\d+) s_cpu=(\d+) c_tid=(\d+) c_asid=(\d+) c_cpu=(\d+) h1_tid=(\d+) h0_tid=(\d+) w_va=0x([0-9a-f]+)", dump.get("SMP2_ROLES", ""))
if m:
    roles = dict(s=(int(m.group(1)), int(m.group(2)), int(m.group(3))),
                 c=(int(m.group(4)), int(m.group(5)), int(m.group(6))),
                 h1=int(m.group(7)), h0=int(m.group(8)), w=int(m.group(9), 16))
counts = " ".join(v for k, v in sorted(dump.items()) if k.startswith("SMP2_COUNTS"))
verdict = dump.get("SMP2_VERDICT")
nrec = re.search(r"records=(\d+)", counts)
recs = []
if nrec:
    for q in range(int(nrec.group(1))):
        text = dump.get("REC %d" % q)
        if text is None:
            fail("record seq %d damaged in both dump copies or missing" % q)
            continue
        m = re.match(r"SMP2_REC seq=(\d+) kind=(\w+) cpu=(\d+) f0=0x([0-9a-f]+) f1=0x([0-9a-f]+) f2=0x([0-9a-f]+) f3=0x([0-9a-f]+) f4=0x([0-9a-f]+)(?: step=(\w+))?$", text)
        recs.append(dict(seq=int(m.group(1)), kind=m.group(2), cpu=int(m.group(3)),
                         f=[int(m.group(i), 16) for i in range(4, 9)], step=m.group(9)))
if roles is None or not recs or verdict is None or not nrec:
    fail("record not dumped (roles=%s records=%d verdict=%s)" % (roles is not None, len(recs), verdict is not None))
    print("\n".join("[smp2-witness][fail] " + f for f in fails)); print("SMP2_WITNESS_SEAL result=fail"); sys.exit(1)
if [r["seq"] for r in recs] != list(range(len(recs))):
    fail("record sequence is not contiguous from 0")
S_TID, S_ASID, S_CPU = roles["s"]; C_TID, C_ASID, C_CPU = roles["c"]; W = roles["w"]
if (S_CPU, C_CPU) != (1, 0):
    fail("roles not on their home CPUs")

def find(start, pred):
    for i in range(start, len(recs)):
        if pred(recs[i]): return i
    return None
def user(start, tid, step, rnd=None):
    return find(start, lambda r: r["kind"] == "user" and r["f"][0] == tid and r["step"] == step and (rnd is None or r["f"][3] == rnd))

# ── SGI POPULATION ──
consumed = set()
for i, a in enumerate(recs):
    if a["kind"] != "sgi_arrived": continue
    tok, src = a["f"][0], a["f"][1]
    if tok & 0x3ff != 1:
        fail("seq %d: claimed INTID %d is not the reschedule SGI" % (a["seq"], tok & 0x3ff))
    if iface_cpu.get((tok >> 10) & 7) != src:
        fail("seq %d: IAR source field %d does not name cpu %d" % (a["seq"], (tok >> 10) & 7, src))
    sends = [j for j in range(i) if recs[j]["kind"] == "sgi_sent" and j not in consumed
             and recs[j]["cpu"] == src and recs[j]["f"][0] == a["cpu"]]
    if not sends:
        fail("seq %d: arrival on cpu %d from cpu %d has no earlier send" % (a["seq"], a["cpu"], src))
    consumed.update(sends)
    nxt = find(i + 1, lambda r: r["kind"] == "sgi_arrived" and r["cpu"] == a["cpu"])
    end = len(recs) if nxt is None else nxt
    done = [r for r in recs[i + 1:end] if r["kind"] == "sgi_completed" and r["cpu"] == a["cpu"]]
    if len(done) != 1 or done[0]["f"][0] != tok:
        fail("seq %d: %d completion(s) before the next arrival, token match=%s" % (a["seq"], len(done), bool(done) and done[0]["f"][0] == tok))
for j, s in enumerate(recs):
    if s["kind"] != "sgi_sent": continue
    if j not in consumed:
        fail("seq %d: send from cpu %d to cpu %d never taken" % (s["seq"], s["cpu"], s["f"][0]))
    want = ((2 << 24) | 1) if s["f"][2] == 1 else ((ready.get(s["f"][0], 0) << 16) | 1)
    if s["f"][1] != want:
        fail("seq %d: GICD_SGIR 0x%x does not encode target cpu %d (want 0x%x)" % (s["seq"], s["f"][1], s["f"][0], want))
sgi_arrivals = sum(r["kind"] == "sgi_arrived" for r in recs)

def chain(after, src_cpu, dst_cpu, origin, window):
    s = find(after, lambda r: r["kind"] == "sgi_sent" and r["cpu"] == src_cpu and r["f"][0] == dst_cpu and r["f"][2] == 0)
    if s is None: return None, "no send %d->%d" % (src_cpu, dst_cpu)
    a = find(s + 1, lambda r: r["kind"] == "sgi_arrived" and r["cpu"] == dst_cpu and r["f"][1] == src_cpu)
    if a is None: return None, "no arrival %d->%d" % (src_cpu, dst_cpu)
    if origin is not None and recs[a]["f"][2] & 0xff != origin: return None, "arrival seq %d origin %d, want %d" % (recs[a]["seq"], recs[a]["f"][2] & 0xff, origin)
    if window and recs[a]["f"][2] >> 8 != window: return None, "arrival seq %d ELR 0x%x outside window %d" % (recs[a]["seq"], recs[a]["f"][3], window)
    return a, None

def parked(after, src_cpu, dst_cpu, woken):
    """(dispatch index, 'sgi'|'timer') or (None, why). A timer-won race counts only when CPU dst's
    periodic idle advance resumed exactly the woken task and the SGI then arrived in that task."""
    a, why = chain(after, src_cpu, dst_cpu, None, 0)
    if why: return None, why
    if recs[a]["f"][2] & 0xff == 1:
        d = find(a + 1, lambda r: r["cpu"] == dst_cpu and r["kind"] in ("idle_dispatch", "sgi_arrived"))
        if d is None or recs[d]["kind"] != "idle_dispatch" or recs[d]["f"][1] != 1 or recs[d]["f"][0] != woken:
            return None, "idle-boundary arrival seq %d not followed by the SGI-driven dispatch of tid %d" % (recs[a]["seq"], woken)
        return (d, "sgi"), None
    ds = [i for i in range(after, a) if recs[i]["kind"] == "idle_dispatch" and recs[i]["cpu"] == dst_cpu
          and recs[i]["f"][1] == 0 and recs[i]["f"][0] == woken]
    if not ds: return None, "arrival seq %d not at the idle boundary and no timer idle dispatch of tid %d before it" % (recs[a]["seq"], woken)
    if recs[a]["f"][4] != woken: return None, "arrival seq %d in tid %d, not the resumed tid %d" % (recs[a]["seq"], recs[a]["f"][4], woken)
    return (ds[-1], "timer"), None

# ── PARKED TARGETS ──
parked_n, sgi_to_s, sgi_to_c, timer_first, at = 0, 0, 0, 0, 0
for k in (1, 2, 3, 4):
    c = user(at, C_TID, "C_P1_CALL", k)
    if c is None: fail("P1 round %d: C_P1_CALL missing" % k); break
    res, why = parked(c, 0, 1, S_TID)
    if why: fail("P1 round %d 0->1: %s" % (k, why)); break
    (d, route) = res
    sgi_to_s += route == "sgi"; timer_first += route == "timer"
    r = user(d, S_TID, "S_P1_RESUMED", k)
    if r is None or recs[r]["cpu"] != 1: fail("P1 round %d: S did not resume (context-checked) on cpu 1" % k); break
    parked_n += 1
    rp = user(r, S_TID, "S_P1_REPLY", k)
    if rp is None: fail("P1 round %d: S_P1_REPLY missing" % k); break
    res, why = parked(rp, 1, 0, C_TID)
    if why: fail("P1 round %d 1->0: %s" % (k, why)); break
    (d, route) = res
    sgi_to_c += route == "sgi"; timer_first += route == "timer"
    b = user(d, C_TID, "C_P1_RESUMED", k)
    if b is None or recs[b]["cpu"] != 0: fail("P1 round %d: C did not resume (context-checked) on cpu 0" % k); break
    parked_n += 1; at = b
if sgi_to_s != 4 or sgi_to_c < 2:
    fail("SGI-driven parked dispatches: %d/4 to CPU 1 (no timer there: all must be), %d/4 to CPU 0 (>= 2 required)" % (sgi_to_s, sgi_to_c))

# ── EL0 TARGETS ──
el0 = 0
c = user(at, C_TID, "C_P2A_CALL")
a, why = chain(c, 0, 1, 0, 1) if c is not None else (None, "C_P2A_CALL missing")
if why: fail("P2A: %s" % why)
elif recs[a]["f"][4] != S_TID or user(a, S_TID, "S_WIN_A_OK") is None: fail("P2A: arrival not in S, or S's window check missing after it")
else: el0 += 1
c = user(at, S_TID, "S_P2B_CALL")
a, why = chain(c, 1, 0, 0, 2) if c is not None else (None, "S_P2B_CALL missing")
if why: fail("P2B: %s" % why)
elif recs[a]["f"][4] != C_TID or user(a, C_TID, "C_WIN_B_OK") is None: fail("P2B: arrival not in C, or C's window check missing after it")
else: el0 += 1

# ── REMOTE INVALIDATION ──
def request(start, cpu, asid):
    b = find(start, lambda r: r["kind"] == "inval_begin" and r["cpu"] == cpu and r["f"][0] == asid and r["f"][1] == W)
    if b is None: return None, None, "no request"
    dones = [i for i in range(b + 1, len(recs)) if recs[i]["kind"] == "inval_done" and recs[i]["cpu"] == cpu
             and recs[i]["f"][:3] == recs[b]["f"][:3]]
    if not dones or recs[dones[0]]["f"] != recs[b]["f"]:
        return b, None, "first completion for this mapping is not this request's generation"
    return b, dones[0], None
def retired(b, d):
    old = recs[b]["f"][2]; asid = recs[b]["f"][0]
    disp = find(d + 1, lambda r: r["kind"] == "vm_displaced" and r["f"][0] == asid and r["f"][1] == W and r["f"][2] == old)
    if disp is None: return "displaced frame not recorded after the completion"
    sd = find(disp + 1, lambda r: r["kind"] == "vm_shootdown" and r["f"][0] == asid and r["f"][1] == W)
    if sd is None or recs[sd]["f"][2] != 1: return "shootdown owner did not acknowledge"
    st = [i for i, r in enumerate(recs) if r["kind"] == "vm_settled" and r["f"][0] == asid and r["f"][2] == old]
    if len(st) != 1 or st[0] < sd: return "displaced frame settled %d time(s), first at %s (shootdown at %d)" % (len(st), st[:1], sd)
    return None
tlb = 0
for rnd in (1, 2, 3, 4):
    (T, TA, TC, tp), (Q, QC, qp) = ((S_TID, S_ASID, 1, "S"), (C_TID, 0, "C")) if rnd % 2 else ((C_TID, C_ASID, 0, "C"), (S_TID, 1, "S"))
    p = user(0, T, tp + "_PRIMED", rnd)
    arm = user(p or 0, Q, qp + "_ARM_R", rnd) if p is not None else None
    if p is None or arm is None: fail("TLB round %d: primed/arm step missing" % rnd); continue
    if recs[p]["cpu"] != TC or recs[arm]["cpu"] != QC: fail("TLB round %d: target/requester not on their CPUs" % rnd); continue
    if find(arm, lambda r: r["kind"] == "r_repoint" and r["f"][0] == TA and r["f"][3] == rnd and r["f"][4] == 1) is None:
        fail("TLB round %d: probe not re-pointed" % rnd); continue
    b, d, why = request(arm, QC, TA)
    if why: fail("TLB round %d: %s" % (rnd, why)); continue
    why = retired(b, d)
    if why: fail("TLB round %d: %s" % (rnd, why)); continue
    o = user(p, T, tp + "_OBSERVED", rnd)
    if o is None or o < d or recs[o]["cpu"] != TC:
        fail("TLB round %d: target observation missing, before the completion, or on another CPU" % rnd); continue
    tlb += 1

# ── MUTUAL PROGRESS ──
mutual, overlapped = 0, 0
for m in (1, 2, 3, 4):
    sn, cn = user(0, S_TID, "S_MUT_NR3", m), user(0, C_TID, "C_MUT_NR3", m)
    if sn is None or cn is None: fail("mutual %d: request steps missing" % m); continue
    bs, ds, why1 = request(sn, 1, C_ASID)
    bc, dc, why2 = request(cn, 0, S_ASID)
    if why1 or why2: fail("mutual %d: %s / %s" % (m, why1, why2)); continue
    why = retired(bs, ds) or retired(bc, dc)
    if why: fail("mutual %d: %s" % (m, why)); continue
    if user(0, S_TID, "S_MUT_OK", m) is None or user(0, C_TID, "C_MUT_OK", m) is None:
        fail("mutual %d: observation missing" % m); continue
    mutual += 1
    overlapped += int(max(sn, cn) < min(ds, dc))
if overlapped < 1:
    fail("no mutual round had both requests in flight together")

# ── CONTEXT ──
ctx_steps = {"S_P1_RESUMED": 4, "C_P1_RESUMED": 4, "S_WIN_A_OK": 1, "C_WIN_B_OK": 1, "S_OBSERVED": 2, "C_OBSERVED": 2}
for step, want in ctx_steps.items():
    n = sum(r["kind"] == "user" and r["step"] == step for r in recs)
    if n != want: fail("context step %s seen %d time(s), want %d" % (step, n, want))

# ── the kernel verifier must agree ──
for want in ["p1_parked=8", "p2_el0=2", "tlb_rounds=4", "mutual_rounds=4", "settled_after_ack=12",
             "p1_sgi_to_s=%d" % sgi_to_s, "p1_sgi_to_c=%d" % sgi_to_c, "p1_timer_first=%d" % timer_first]:
    if want not in counts.split():
        fail("kernel counts disagree on %s: %s" % (want, counts))
if not verdict.startswith("SMP2_VERDICT result=ok "):
    fail("kernel verdict: %s" % verdict)

summary = "records=%d damaged_lines=%d sgi_arrivals=%d parked=%d sgi_to_s=%d sgi_to_c=%d timer_first=%d el0=%d tlb_rounds=%d mutual=%d mutual_overlapped=%d" % (
    len(recs), damaged, sgi_arrivals, parked_n, sgi_to_s, sgi_to_c, timer_first, el0, tlb, mutual, overlapped)
print("[smp2-witness] " + summary)
print("[smp2-witness] kernel: " + counts + " | " + verdict)
for f in fails: print("[smp2-witness][fail] " + f)
ok = not fails and parked_n == 8 and el0 == 2 and tlb == 4 and mutual == 4
print("SMP2_WITNESS_SEAL %s result=%s" % (summary, "ok" if ok else "fail"))
sys.exit(0 if ok else 1)
PY
