#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""VFS-CORRECT1B — grade one live mount-authority / capability-width witness boot.

Input: the normalized serial log of a boot whose init, VFS and ramfs are built with
`vfs-mount-witness` (init/vfs_mount_witness.rs; ramfs seeds /ram/vc1b-witness), optionally with a
kernel built with `cap-gen-witness` (--wide: every CNode slot starts at generation 0x1_0001, so
every capability identity exceeds 2^32). The byte formula is re-derived here, not taken from the
guest.

Rules (each failure names its rule):
  CORE      the core smoke passed (rc 0); exactly one VC1B_BEGIN and one VC1B_END, fails=0
            result=ok; admin_present=1.
  ADMIN     the kernel created the spawn's admin endpoint (KSPAWN_ADMIN_EP_CREATED) and delegated
            its receive side into VFS (KSPAWN_ADMIN_RECV_CAP_DELEGATED); VFS logged
            VFS_MOUNT_ADMIN_RECV_CAP present=1 with that cap; init logged INIT_VFS_ADMIN_CAP
            present=1 with the cap VC1B_BEGIN names.
  REFUSE    the ordinary-client cases: ordinary_register 5, ordinary_commit 6, unsupported_flags 8,
            duplicate 1, ordinary_register_after 5, each matched by VFS's own refusal line
            (VFS_MOUNT_ADMIN_REFUSED op=register status=5 reason=numeric_cap_is_not_authority,
            op=commit status=6 retained_cap=0, status=8 / status=1 retaining exactly the VFS-local
            cap of the stage they consumed).
  SENDER    stale_handle (and truncated_handle, which must be applicable under --wide) were
            refused by the kernel at the sender (reason=stage_refused) and nothing with those
            prefixes reached VFS.
  IDENTITY  exactly one VFS_MOUNT_PUBLISHED prefix=/ram; its vfs_local_cap equals the cap VFS
            received with the authorized stage; its sender_local_cap equals init's own backend
            cap (VC1B_BEGIN init_ramfs_cap, VC1B_IDENTITY); the two differ; and every
            VFS_FORWARD_CALL target=/ram/ (at least 8: open, 3 reads, close, open, read, close)
            uses the VFS-local cap and never the sender's number.
  BYTES     VC1B_READ k=0..2 sizes 200 200 64, counts 104 88 0, contiguous offsets, bytes_ok=1,
            tail_ok=1; the VC1B_DATA hex equals byte[i] = (i*37+11) mod 256 for i < 192;
            VC1B_CLOSE ok=1 total=192; VC1B_RECOVER ok=1.
  WIDTH     (--wide only) the published VFS-local cap, init's backend cap and init's admin cap
            each exceed 2^32 with generation (id >> 16) > 65535; PM's image-7 grant cap
            (PM_VFS_GRANT_RO_RECEIVED) exceeds 2^32 and is the exact mo_cap PM passed to NR 29
            (SPAWN_FROM_MO_ENTER image_id=7), and that spawn succeeded (SPAWN_FROM_MO_OK
            image_id=7 spawned_tid=T) and T ran (DRIVER_MANAGER_ENTRY by tid T).
            A truncated identity (low 32 bits) fails here or in IDENTITY.
  TRANSPORT no VFS_FORWARD_CALL_FAIL, PM_VFS_CALL_FAIL, VFS_BACKEND_COMPLETION_MALFORMED,
            INIT_MOUNT_REPLY_UNEXPECTED_CAP, VFS_MOUNT_ADMIN_RECV_CAP present=0 or BOOT_FATAL.
Exit status 0 = sealed, 1 = not sealed. --self-test checks the rules on synthetic boots.
"""
import argparse
import re
import sys

LEN = 192
READS = [(200, 104), (200, 88), (64, 0)]
CASES = {"ordinary_register": "5", "ordinary_commit": "6", "unsupported_flags": "8",
         "duplicate": "1", "authorized": "0", "ordinary_register_after": "5"}
FORBIDDEN = ("VFS_FORWARD_CALL_FAIL", "PM_VFS_CALL_FAIL", "VFS_BACKEND_COMPLETION_MALFORMED",
             "INIT_MOUNT_REPLY_UNEXPECTED_CAP", "VFS_MOUNT_ADMIN_RECV_CAP present=0", "BOOT_FATAL")
WIDE = 1 << 32


def byte(i):
    return (i * 37 + 11) & 0xFF


def fields(text):
    return dict(re.findall(r"(\w+)=(\S+)", text))


def tagged(log, tag):
    out = []
    for i, line in enumerate(log):
        m = re.search(r"(?:USER_LOG tid=(\d+) msg=)?\b(" + tag + r")\b(?: (.*))?$", line)
        if m:
            f = fields(m.group(3) or "")
            f["_tid"] = m.group(1)
            f["_at"] = i
            out.append(f)
    return out


def num(f, k):
    try:
        return int(f.get(k, ""))
    except ValueError:
        return None


def wide(c):
    return c is not None and c >= WIDE and (c >> 16) > 0xFFFF


def grade(log, core_rc, want_wide):
    fails = []
    facts = {}

    def fail(rule, msg):
        fails.append(f"{rule}: {msg}")

    # CORE
    if core_rc != 0:
        fail("CORE", f"core smoke rc={core_rc}")
    begins, ends = tagged(log, "VC1B_BEGIN"), tagged(log, "VC1B_END")
    if len(begins) != 1 or len(ends) != 1:
        fail("CORE", f"expected one VC1B_BEGIN and one VC1B_END, saw {len(begins)}/{len(ends)}")
        return fails, facts
    if ends[0].get("fails") != "0" or ends[0].get("result") != "ok":
        fail("CORE", f"VC1B_END fails={ends[0].get('fails')} result={ends[0].get('result')}")
    b = begins[0]
    if b.get("admin_present") != "1":
        fail("CORE", "init had no admin capability")
    init_admin, init_backend = num(b, "init_admin_cap"), num(b, "init_ramfs_cap")
    facts["init_backend"] = init_backend
    facts["init_admin"] = init_admin

    # ADMIN
    created = tagged(log, "KSPAWN_ADMIN_EP_CREATED")
    delegated = tagged(log, "KSPAWN_ADMIN_RECV_CAP_DELEGATED")
    vrecv = [x for x in tagged(log, "VFS_MOUNT_ADMIN_RECV_CAP") if x.get("present") == "1"]
    iadm = [x for x in tagged(log, "INIT_VFS_ADMIN_CAP") if x.get("present") == "1"]
    if not created or not delegated:
        fail("ADMIN", "kernel did not create and delegate the admin endpoint")
    if len(vrecv) != 1 or (delegated and num(vrecv[0], "cap") != num(delegated[0], "local_cap")):
        fail("ADMIN", f"VFS admin receive cap {vrecv} does not match the delegation {delegated}")
    if len(iadm) != 1 or num(iadm[0], "init_local") != init_admin:
        fail("ADMIN", f"init admin cap {iadm} does not match VC1B_BEGIN {init_admin}")
    if vrecv:
        facts["vfs_admin_recv"] = vrecv[0].get("cap")

    # Per-case results (init side).
    cases = {}
    for c in tagged(log, "VC1B_CASE"):
        cases.setdefault(c.get("name"), []).append(c)
    stages = tagged(log, "VFS_MOUNT_STAGE_RECEIVED")
    refusals = tagged(log, "VFS_MOUNT_ADMIN_REFUSED")
    published = [p for p in tagged(log, "VFS_MOUNT_PUBLISHED") if p.get("prefix") == "/ram"]

    # REFUSE
    for name, want in CASES.items():
        got = cases.get(name, [])
        if len(got) != 1 or got[0].get("status") != want or got[0].get("expect") != want:
            fail("REFUSE" if name != "authorized" else "IDENTITY",
                 f"{name}: expected status {want}, saw {[g.get('status') for g in got]}")
    reg = [r for r in refusals if r.get("op") == "register"]
    if len(reg) != 2 or any(r.get("status") != "5" or
                            r.get("reason") != "numeric_cap_is_not_authority" for r in reg):
        fail("REFUSE", f"VFS register refusals {len(reg)} (want 2, status 5)")
    com = [r for r in refusals if r.get("op") == "commit"]
    bystatus = {r.get("status"): r for r in com}
    if "6" not in bystatus or bystatus["6"].get("retained_cap") != "0":
        fail("REFUSE", "VFS did not refuse the unstaged commit with status 6 and nothing retained")
    for st, prefix, flags in (("8", "/ram", "8"), ("1", "/initramfs", "0")):
        r = bystatus.get(st)
        s = [x for x in stages if x.get("prefix") == prefix and x.get("flags") == flags]
        if r is None or not s:
            fail("REFUSE", f"no VFS stage/refusal pair for status {st}")
            continue
        if r.get("retained_cap") != s[0].get("vfs_local_cap") or r.get("retained_cap") in (None, "0"):
            fail("REFUSE", f"status {st} retained {r.get('retained_cap')}, "
                           f"staged {s[0].get('vfs_local_cap')}")

    # SENDER
    for name in ("stale_handle", "truncated_handle"):
        got = cases.get(name, [])
        if len(got) != 1:
            fail("SENDER", f"{name}: {len(got)} lines")
            continue
        applicable = got[0].get("applicable") == "1"
        if name == "truncated_handle" and want_wide and not applicable:
            fail("SENDER", "truncated_handle not applicable under --wide")
        if applicable and got[0].get("reason") != "stage_refused":
            fail("SENDER", f"{name}: reason={got[0].get('reason')}")
        facts[name] = f"{got[0].get('reason')}:{got[0].get('cap')}"
    if any(s.get("prefix", "").startswith("/vc1b-") for s in stages):
        fail("SENDER", "a refused-at-sender stage reached VFS")

    # IDENTITY
    vfs_local = None
    if len(published) != 1:
        fail("IDENTITY", f"{len(published)} VFS_MOUNT_PUBLISHED prefix=/ram lines")
    else:
        p = published[0]
        vfs_local, sender_local = num(p, "vfs_local_cap"), num(p, "sender_local_cap")
        auth = [s for s in stages if s.get("prefix") == "/ram" and s.get("flags") == "0"
                and s["_at"] < p["_at"]]
        if not auth or num(auth[-1], "vfs_local_cap") != vfs_local:
            fail("IDENTITY", "the published cap is not the one VFS received with the stage")
        ident = tagged(log, "VC1B_IDENTITY")
        if sender_local != init_backend or not ident or num(ident[0], "init_local_backend") != init_backend:
            fail("IDENTITY", f"sender-local {sender_local} is not init's backend cap {init_backend}")
        if vfs_local == sender_local:
            fail("IDENTITY", f"VFS published the sender's number ({sender_local}), not a received cap")
        fwd = [f for f in tagged(log, "VFS_FORWARD_CALL")
               if f.get("target") == "/ram/" and f["_at"] > p["_at"]]
        bad = [f.get("send_cap") for f in fwd if num(f, "send_cap") != vfs_local]
        if len(fwd) < 8:
            fail("IDENTITY", f"{len(fwd)} forwards to /ram/ (want >= 8)")
        if bad:
            fail("IDENTITY", f"{len(bad)} /ram/ forwards used another cap ({bad[0]})")
        facts["vfs_local_backend"] = vfs_local
        facts["ram_forwards"] = len(fwd)

    # BYTES
    reads = tagged(log, "VC1B_READ")
    data = {}
    for d in tagged(log, "VC1B_DATA"):
        data.setdefault(int(d.get("k", -1)), []).append(d)
    if len(reads) != len(READS):
        fail("BYTES", f"{len(reads)} VC1B_READ lines")
    off, got = 0, bytearray()
    for k, (r, (size, want)) in enumerate(zip(reads, READS)):
        if (num(r, "k"), num(r, "size"), num(r, "n"), num(r, "off")) != (k, size, want, off):
            fail("BYTES", f"read {k}: {r}")
        if r.get("bytes_ok") != "1" or r.get("tail_ok") != "1":
            fail("BYTES", f"read {k}: bytes_ok={r.get('bytes_ok')} tail_ok={r.get('tail_ok')}")
        chunk = bytearray()
        for d in data.get(k, []):
            if num(d, "off") != off + len(chunk):
                fail("BYTES", f"read {k}: data offset {d.get('off')}")
            chunk += bytes.fromhex(d.get("hex", ""))
        if len(chunk) != (num(r, "n") or 0):
            fail("BYTES", f"read {k}: {len(chunk)} data bytes for count {r.get('n')}")
        got += chunk
        off += num(r, "n") or 0
    if bytes(got) != bytes(byte(i) for i in range(LEN)):
        fail("BYTES", "wrong destination bytes: differ from the ramfs witness formula")
    cl = tagged(log, "VC1B_CLOSE")
    if len(cl) != 1 or cl[0].get("ok") != "1" or cl[0].get("total") != str(LEN):
        fail("BYTES", f"close {cl}")
    rc = tagged(log, "VC1B_RECOVER")
    if len(rc) != 1 or rc[0].get("ok") != "1":
        fail("BYTES", "route did not recover after the refusal")

    # WIDTH
    grants = [g for g in tagged(log, "PM_VFS_GRANT_RO_RECEIVED") if g.get("image_id") == "7"]
    enters = [e for e in tagged(log, "SPAWN_FROM_MO_ENTER") if e.get("image_id") == "7"]
    oks = [o for o in tagged(log, "SPAWN_FROM_MO_OK") if o.get("image_id") == "7"]
    gcap = num(grants[0], "cap") if grants else None
    facts["grant_cap"] = gcap
    if want_wide:
        for label, c in (("VFS-local backend", vfs_local), ("init backend", init_backend),
                         ("init admin", init_admin), ("PM grant", gcap)):
            if not wide(c):
                fail("WIDTH", f"{label} cap {c} is not a full-width identity above 2^32")
        if len(enters) != 1 or num(enters[0], "mo_cap") != gcap:
            fail("WIDTH", f"NR 29 mo_cap {[e.get('mo_cap') for e in enters]} is not PM's grant {gcap}")
        if len(oks) != 1:
            fail("WIDTH", "NR 29 for image 7 did not succeed")
        else:
            tid = oks[0].get("spawned_tid")
            if not [x for x in tagged(log, "DRIVER_MANAGER_ENTRY") if x["_tid"] == tid]:
                fail("WIDTH", f"driver_manager tid {tid} never ran")
        facts["generation"] = (vfs_local >> 16) if vfs_local else None

    # TRANSPORT
    for tag in FORBIDDEN:
        hits = [line for line in log if tag in line]
        if hits:
            fail("TRANSPORT", f"{len(hits)} {tag} line(s): {hits[0].strip()[:120]}")
    return fails, facts


def synthetic(wide_ids=True, nc=None):
    base = (0x1_0001 << 16) if wide_ids else (1 << 16)
    admin, backend, vadm = base + 13, base + 16, base + 2
    v = {"flags": base + 10, "dup": base + 11, "auth": base + 12}
    if nc == "sender_slot":
        v["auth"] = backend
    grant = base + 20
    if nc == "truncate":
        grant &= 0xFFFF_FFFF
        v["auth"] &= 0xFFFF_FFFF
    trunc = backend & 0xFFFF_FFFF
    tl = "1" if trunc != backend else "0"
    u = lambda tid, m: f"USER_LOG tid={tid} msg={m}"
    L = [f"[kernel] KSPAWN_ADMIN_EP_CREATED spawner_tid=3 send_cap={base + 30} recv_cap={base + 31}",
         f"[kernel] KSPAWN_ADMIN_RECV_CAP_DELEGATED tid=10002 local_cap={vadm}",
         u(3, f"PM_VFS_GRANT_RO_RECEIVED image_id=7 len=35032 cap={grant}"),
         u(3, f"SPAWN_FROM_MO_ENTER image_id=7 mo_cap={grant} parent_pid=0"),
         u(3, "SPAWN_FROM_MO_OK image_id=7 spawned_tid=10003"),
         u(10003, "DRIVER_MANAGER_ENTRY"),
         u(1, f"INIT_VFS_ADMIN_CAP present=1 init_local={admin}"),
         u(10002, f"VFS_MOUNT_ADMIN_RECV_CAP present=1 cap={vadm}"),
         u(1, f"VC1B_BEGIN admin_present=1 init_admin_cap={admin} init_ramfs_cap={backend}"),
         u(10002, "VFS_MOUNT_ADMIN_REFUSED op=register sender_tid=1 status=5 reason=numeric_cap_is_not_authority"),
         u(1, "VC1B_CASE name=ordinary_register expect=5 status=5 reason=reply"),
         u(10002, "VFS_MOUNT_ADMIN_REFUSED op=commit sender_tid=1 prefix=/ram status=6 retained_cap=0"),
         u(1, "VC1B_CASE name=ordinary_commit expect=6 status=6 reason=reply"),
         u(10002, f"VFS_MOUNT_STAGE_RECEIVED sender_tid=1 prefix=/ram flags=8 vfs_local_cap={v['flags']} sender_local_cap={backend}"),
         u(10002, f"VFS_MOUNT_ADMIN_REFUSED op=commit sender_tid=1 prefix=/ram status=8 retained_cap={v['flags']}"),
         u(1, "VC1B_CASE name=unsupported_flags expect=8 status=8 reason=reply"),
         u(1, f"VC1B_CASE name=stale_handle expect=stage_refused status=-1 reason=stage_refused cap={backend + 65536} applicable=1"),
         u(1, f"VC1B_CASE name=truncated_handle expect=stage_refused status=-1 reason={'stage_refused' if tl == '1' else 'not_applicable'} cap={trunc} applicable={tl}"),
         u(10002, f"VFS_MOUNT_STAGE_RECEIVED sender_tid=1 prefix=/initramfs flags=0 vfs_local_cap={v['dup']} sender_local_cap={backend}"),
         u(10002, f"VFS_MOUNT_ADMIN_REFUSED op=commit sender_tid=1 prefix=/initramfs status=1 retained_cap={v['dup']}"),
         u(1, "VC1B_CASE name=duplicate expect=1 status=1 reason=reply"),
         u(10002, f"VFS_MOUNT_STAGE_RECEIVED sender_tid=1 prefix=/ram flags=0 vfs_local_cap={v['auth']} sender_local_cap={backend}"),
         u(10002, f"VFS_MOUNT_PUBLISHED prefix=/ram vfs_local_cap={v['auth']} sender_local_cap={backend} entries=3"),
         u(1, "VC1B_CASE name=authorized expect=0 status=0 reason=reply"),
         u(1, f"VC1B_IDENTITY init_local_backend={backend}")]
    fwd = u(10002, f"VFS_FORWARD_CALL op=11 target=/ram/ send_cap={v['auth']} reply_cap={base + 1}")
    L += [fwd] * 5
    off = 0
    for k, (size, n) in enumerate(READS):
        L.append(u(1, f"VC1B_READ k={k} size={size} n={n} off={off} bytes_ok=1 tail_ok=1"))
        for s in range(0, n, 32):
            e = min(s + 32, n)
            L.append(u(1, f"VC1B_DATA k={k} off={off + s} hex={bytes(byte(off + i) for i in range(s, e)).hex()}"))
        off += n
    L += [u(1, f"VC1B_CLOSE ok=1 total={LEN}"),
          u(10002, "VFS_MOUNT_ADMIN_REFUSED op=register sender_tid=1 status=5 reason=numeric_cap_is_not_authority"),
          u(1, "VC1B_CASE name=ordinary_register_after expect=5 status=5 reason=reply")]
    L += [fwd] * 3
    L += [u(1, "VC1B_RECOVER ok=1"), u(1, "VC1B_END fails=0 result=ok")]
    return L


def self_test():
    good = synthetic()
    f, _ = grade(good, 0, True)
    assert not f, f
    narrow = synthetic(wide_ids=False)
    f, _ = grade(narrow, 0, False)
    assert not f, f
    n = 0

    def must_fail(lines, rule, needle="", rc=0, w=True):
        nonlocal n
        n += 1
        f, _ = grade(lines, rc, w)
        assert any(x.startswith(rule + ":") and needle in x for x in f), (rule, needle, f)

    # The two named negative controls.
    must_fail(synthetic(nc="truncate"), "WIDTH", "not a full-width identity")
    must_fail(synthetic(nc="sender_slot"), "IDENTITY", "sender's number")
    # A narrow boot graded as wide fails WIDTH; a narrow boot passes when not asked for width.
    must_fail(narrow, "WIDTH", w=True)
    must_fail(narrow, "SENDER", "not applicable", w=True)
    must_fail(good, "CORE", "rc=1", rc=1)
    must_fail([x.replace("VC1B_END fails=0 result=ok", "VC1B_END fails=1 result=fail") for x in good], "CORE")
    must_fail([x for x in good if "KSPAWN_ADMIN_RECV_CAP_DELEGATED" not in x], "ADMIN")
    must_fail([x.replace("name=ordinary_register expect=5 status=5", "name=ordinary_register expect=5 status=0")
               for x in good], "REFUSE", "ordinary_register")
    must_fail([x for x in good if "op=register" not in x], "REFUSE", "register refusals")
    must_fail([x.replace("status=6 retained_cap=0", "status=6 retained_cap=9") for x in good], "REFUSE")
    must_fail([x.replace("status=8 retained_cap=", "status=8 retained_cap=1") for x in good], "REFUSE", "status 8")
    must_fail([x.replace("name=stale_handle expect=stage_refused status=-1 reason=stage_refused",
                         "name=stale_handle expect=stage_refused status=0 reason=reply") for x in good],
              "SENDER", "stale_handle")
    must_fail(good + ["USER_LOG tid=10002 msg=VFS_MOUNT_STAGE_RECEIVED sender_tid=1 prefix=/vc1b-stale flags=0 vfs_local_cap=5 sender_local_cap=6"],
              "SENDER", "reached VFS")
    pub = [x for x in good if "VFS_MOUNT_PUBLISHED" in x][0]
    must_fail(good + [pub], "IDENTITY", "PUBLISHED")
    must_fail([x.replace("send_cap=", "send_cap=1", 1) if "VFS_FORWARD_CALL" in x else x for x in good],
              "IDENTITY", "another cap")
    must_fail([x for x in good if "VFS_FORWARD_CALL" not in x], "IDENTITY", "forwards")
    must_fail([x[:-2] + "00" if "VC1B_DATA k=1 off=136" in x else x for x in good], "BYTES", "wrong destination")
    must_fail([x.replace("n=88 off=104", "n=87 off=104") for x in good], "BYTES")
    must_fail([x.replace("tail_ok=1", "tail_ok=0", 1) for x in good], "BYTES")
    must_fail([x.replace("VC1B_RECOVER ok=1", "VC1B_RECOVER ok=0") for x in good], "BYTES", "recover")
    must_fail([x.replace("SPAWN_FROM_MO_ENTER image_id=7 mo_cap=", "SPAWN_FROM_MO_ENTER image_id=7 mo_cap=1")
               for x in good], "WIDTH", "NR 29")
    must_fail([x for x in good if "DRIVER_MANAGER_ENTRY" not in x], "WIDTH", "never ran")
    must_fail(good + ["USER_LOG tid=1 msg=INIT_MOUNT_REPLY_UNEXPECTED_CAP cap=5"], "TRANSPORT")
    must_fail(good + ["USER_LOG tid=10002 msg=VFS_FORWARD_CALL_FAIL op=12 err=WouldBlock"], "TRANSPORT")
    print(f"VFS_MOUNT_WITNESS_GRADER_SELF_TEST result=ok mutants={n}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--arch")
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--core-rc", type=int, default=0)
    ap.add_argument("--wide", action="store_true")
    ap.add_argument("log", nargs="?")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    with open(a.log, "r", errors="replace") as fh:
        log = fh.read().splitlines()
    fails, facts = grade(log, a.core_rc, a.wide)
    for f in fails:
        print(f"[vfs-mount-witness][fail] {f}")
    res = "ok" if not fails else "fail"
    extra = " ".join(f"{k}={v}" for k, v in sorted(facts.items()))
    print(f"VFS_MOUNT_WITNESS_SEAL arch={a.arch} smp={a.smp} wide={int(a.wide)} result={res} {extra}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
