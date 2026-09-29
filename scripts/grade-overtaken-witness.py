#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""QEMU-SMP3-ACCEPTANCE §2 — grade one overtaken-deferral witness boot (x86_64 or AArch64).

Usage: grade-overtaken-witness.py <boot.log> <arch>

The verdict comes from the kernel's sealed summary (printed twice, FNV-1a checked), cross-checked
against the provisioning markers; live lines are reported, never trusted over the seal, because two
CPUs write the console. Exit 0 only for `result=ok` with every required fact.
"""
import re
import sys

log, arch = sys.argv[1], sys.argv[2]
text = open(log, "rb").read().decode("utf-8", "replace").replace("\r", "\n")
lines = text.split("\n")
fails = []


def fail(msg):
    fails.append(msg)


def fnv1a(b):
    h = 0x811C9DC5
    for x in b:
        h = ((h ^ x) * 0x01000193) & 0xFFFFFFFF
    return h


FATAL = re.compile(
    r"KERNEL PANIC|RUST PANIC|panicked at|OVERTAKEN_DEFERRAL_UNAUTHENTICATED|"
    r"USER_FPU_HOME_UNAUTHENTICATED|POST_LOCK_DISPATCH_FATAL|OVT_WITNESS_[A-Z_]*FAIL"
)
for l in lines:
    if FATAL.search(l):
        fail("fatal marker: " + l.strip()[:200])
        break

armed = [re.search(r"OVT_WITNESS_ARMED w_tid=(\d+) w_asid=(\d+) k_tid=(\d+) k_asid=(\d+) rounds=(\d+)", l) for l in lines]
armed = [m for m in armed if m]
if len(armed) != 1:
    fail("expected exactly one OVT_WITNESS_ARMED, saw %d" % len(armed))
    w_tid = w_asid = k_tid = k_asid = rounds = None
else:
    w_tid, w_asid, k_tid, k_asid, rounds = (int(x) for x in armed[0].groups())

# K's placement on the other CPU. AArch64 prints it; on x86_64 K is the SMP oracle's AP task.
if arch == "aarch64":
    if not any(re.search(r"OVT_WITNESS_AP_PLACED cpu=1 tid=%s " % k_tid, l) for l in lines):
        fail("K was not placed on CPU 1")

passes = {1: {}, 2: {}}
total_parts = None
for l in lines:
    m = re.search(r"OVT_SUM part=(\d+)/(\d+) (.*?) pass=([12]) crc=0x([0-9a-f]{8})", l)
    if not m:
        continue
    idx, tot, part, ps, crc = int(m.group(1)), int(m.group(2)), m.group(3), int(m.group(4)), int(m.group(5), 16)
    if fnv1a(part.encode()) != crc:
        fail("summary part %d pass %d damaged (crc mismatch)" % (idx, ps))
        continue
    total_parts = tot
    passes[ps][idx] = part
bodies = []
for ps in (1, 2):
    if total_parts and len(passes[ps]) == total_parts:
        bodies.append(" ".join(passes[ps][i] for i in range(1, total_parts + 1)))
if not bodies:
    fail("no complete intact OVT_SUM pass")
    body = ""
else:
    body = bodies[0]
    if any(b != body for b in bodies):
        fail("the two summary passes differ")
    print("[overtaken-witness] summary (%d intact pass(es)): %s" % (len(bodies), body))

f = dict(re.findall(r"(\w+)=([^ ]+)", body))
if body:
    if f.get("arch") != arch:
        fail("summary arch %s != %s" % (f.get("arch"), arch))
    if f.get("hold_cpu") != "0":
        fail("the held drain ran on CPU %s, not CPU 0" % f.get("hold_cpu"))
    for key, want in (("w_tid", w_tid), ("w_asid", w_asid), ("k_tid", k_tid), ("k_asid", k_asid)):
        if want is not None and f.get(key) != str(want):
            fail("summary %s=%s != provisioned %s" % (key, f.get(key), want))
    if w_asid is not None and w_asid == k_asid:
        fail("W and K share an address space")
    n = int(f.get("rounds", "0"))
    if n < 1 or f.get("held") != str(n):
        fail("held %s of %s rounds" % (f.get("held"), n))
    per = []
    for r in range(1, n + 1):
        v = f.get("r%d" % r, "")
        parts = v.split("/")
        if len(parts) != 5:
            fail("round %d record malformed: %r" % (r, v))
            continue
        spins, obs, settle, inc, inc_asid = int(parts[0]), parts[1], parts[2], int(parts[3]), int(parts[4])
        per.append((spins, obs, settle, inc, inc_asid))
        if obs != "overtaken":
            fail("round %d: the drain was not overtaken (%s)" % (r, obs))
        if settle not in ("switch", "idle"):
            fail("round %d: settled as %s (only switch or idle are valid for an empty current)" % (r, settle))
        if settle == "switch" and inc == w_tid and inc_asid != w_asid:
            fail("round %d: resumed W under asid %d, not %s" % (r, inc_asid, w_asid))
    if f.get("result") != "ok" or f.get("reason") != "none":
        fail("summary result=%s reason=%s" % (f.get("result"), f.get("reason")))
    sync = sum(1 for p in per if p[0] > 0 and p[1] == "overtaken")
    nat = sum(1 for p in per if p[0] == 0 and p[1] == "overtaken")
    to_w = sum(1 for p in per if p[2] == "switch" and p[3] == w_tid)
    print("[overtaken-witness] rounds=%d synchronized=%d natural=%d switch_to_w=%d records=%s" % (
        n, sync, nat, to_w, " ".join("r%d=%s" % (i + 1, "/".join(str(x) for x in p)) for i, p in enumerate(per))))

# Live corroboration (reported; the seal is the verdict).
settled = [l for l in lines if "OVERTAKEN_DEFERRAL_SETTLED arch=%s" % arch in l and ("outgoing=%s " % w_tid) in l]
k_woke = sum(1 for l in lines if re.search(r"tid=%s .*OVT_K_WOKE_ONE" % k_tid, l))
w_ok = sum(1 for l in lines if re.search(r"tid=%s .*OVT_W_ROUND_OK" % w_tid, l))
print("[overtaken-witness] live: settled_lines=%d k_woke=%d w_round_ok=%d" % (len(settled), k_woke, w_ok))
natural_other = [l for l in lines if "OVERTAKEN_DEFERRAL_SETTLED" in l and ("outgoing=%s " % w_tid) not in l]
print("[overtaken-witness] naturally occurring settlements outside the witness: %d" % len(natural_other))

for m in fails:
    print("[overtaken-witness][fail] " + m)
print("OVT_WITNESS_SEAL arch=%s result=%s" % (arch, "fail" if fails else "ok"))
sys.exit(1 if fails else 0)
