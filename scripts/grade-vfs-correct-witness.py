#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""VFS-CORRECT1A — grade one live read-path witness boot.

Input: the normalized serial log of a boot whose init is built with `vfs-correct-witness` (the
cell in init/vfs_correct_witness.rs), the committed fixture file (--fixture) and the build's
rootfs copy of init (--init-rootfs). The cell's own checks are re-done here from the bytes it
logged, against files the guest never saw.

Rules (each failure names its rule):
  CORE      the core smoke passed (rc 0); exactly one VC1A_BEGIN and one VC1A_END, END fails=0
            result=ok.
  READS     six VC1A_READ lines k=0..5 with sizes 13 200 1 104 64 512, counts 13 104 1 104 64 14,
            offsets contiguous from 0, tail_ok=1.
  BYTES     the VC1A_DATA hex of each read covers exactly its count at its offset, and the
            concatenation equals the fixture file byte for byte; bytes_ok=1 on every read.
            A read that returned the right count without copying fails here, as "wrong
            destination bytes".
  EOF       VC1A_EOF k=0..2 (sizes 50, 0, 32): n=0, untouched=1. STATX value = fixture length.
  CLOSE     VC1A_CLOSE ok=1.
  ERRORS    VC1A_ERR read_after_close and bad_fd -> status 4 (bad fd), missing_path -> status 2,
            no_mount -> status 3, each kind=status; VC1A_ERR_UNTOUCHED ok=1; VC1A_RECOVER ok=1.
  INIT      VC1A_INIT_HEAD n=64, and its VC1A_INIT_DATA equals the first 64 bytes of --init-rootfs.
  GRANT     PM obtained the read-only grant for image 7 (PM_VFS_GRANT_RO_RECEIVED image_id=7),
            spawned it through NR 29 (PM_SPAWN_FROM_MO_DONE image_id=7 tid=T), and T ran
            (DRIVER_MANAGER_ENTRY logged by tid T after the spawn).
  TRANSPORT no VFS_FORWARD_CALL_FAIL, PM_VFS_CALL_FAIL, VFS_BACKEND_COMPLETION_MALFORMED,
            PM_ELF_ZC_FAIL or BOOT_FATAL line anywhere in the boot.
Exit status 0 = sealed, 1 = not sealed. --self-test checks the rules on synthetic boots.
"""
import argparse
import re
import sys

SIZES = [13, 200, 1, 104, 64, 512]
COUNTS = [13, 104, 1, 104, 64, 14]
EOF_SIZES = [50, 0, 32]
ERR_EXPECT = {"read_after_close": 4, "missing_path": 2, "no_mount": 3, "bad_fd": 4}
FORBIDDEN = ("VFS_FORWARD_CALL_FAIL", "PM_VFS_CALL_FAIL", "VFS_BACKEND_COMPLETION_MALFORMED",
             "PM_ELF_ZC_FAIL", "BOOT_FATAL")


def fields(line):
    return dict(re.findall(r"(\w+)=(\S+)", line))


def user_lines(log, tag):
    out = []
    for i, line in enumerate(log):
        m = re.search(r"USER_LOG tid=(\d+) msg=(" + tag + r")(?: (.*))?$", line)
        if m:
            f = fields(m.group(3) or "")
            f["_tid"] = m.group(1)
            f["_at"] = i
            out.append(f)
    return out


def grade(log, fixture, init_head, core_rc):
    fails = []
    facts = {}

    def fail(rule, msg):
        fails.append(f"{rule}: {msg}")

    # CORE
    if core_rc != 0:
        fail("CORE", f"core smoke rc={core_rc}")
    begins = user_lines(log, "VC1A_BEGIN")
    ends = user_lines(log, "VC1A_END")
    if len(begins) != 1 or len(ends) != 1:
        fail("CORE", f"expected one VC1A_BEGIN and one VC1A_END, saw {len(begins)}/{len(ends)}")
    elif ends[0].get("fails") != "0" or ends[0].get("result") != "ok":
        fail("CORE", f"VC1A_END {ends[0].get('fails')} {ends[0].get('result')}")

    # READS / BYTES
    reads = user_lines(log, "VC1A_READ")
    data = user_lines(log, "VC1A_DATA")
    if len(reads) != len(SIZES):
        fail("READS", f"expected {len(SIZES)} reads, saw {len(reads)}")
    assembled = bytearray()
    off = 0
    bytes_ok_all = True
    for k, (size, want) in enumerate(zip(SIZES, COUNTS)):
        r = [x for x in reads if x.get("k") == str(k)]
        if len(r) != 1:
            fail("READS", f"read {k}: {len(r)} lines")
            continue
        r = r[0]
        if "ok" in r and r["ok"] == "0":
            fail("READS", f"read {k} failed: kind={r.get('kind')} code={r.get('code')}")
            continue
        n = int(r.get("n", "-1"))
        if int(r.get("size", "-1")) != size or n != want:
            fail("READS", f"read {k}: size={r.get('size')} n={n}, expected size={size} n={want}")
        if int(r.get("off", "-1")) != off:
            fail("READS", f"read {k}: off={r.get('off')}, expected {off}")
        if r.get("tail_ok") != "1":
            fail("READS", f"read {k}: destination tail was modified")
        if r.get("bytes_ok") != "1":
            bytes_ok_all = False
        chunk = bytearray()
        for d in [x for x in data if x.get("k") == str(k)]:
            if int(d.get("off", "-1")) != off + len(chunk):
                fail("BYTES", f"read {k}: data line at off={d.get('off')}, expected {off + len(chunk)}")
            chunk += bytes.fromhex(d.get("hex", ""))
        if len(chunk) != n:
            fail("BYTES", f"read {k}: {len(chunk)} data bytes logged for count {n}")
        assembled += chunk
        off += max(n, 0)
    facts["bytes"] = len(assembled)
    if bytes(assembled) != fixture:
        first = next((i for i, (a, b) in enumerate(zip(assembled, fixture)) if a != b),
                     min(len(assembled), len(fixture)))
        fail("BYTES", f"wrong destination bytes: differ from the fixture at offset {first} "
                      f"({len(assembled)} read, fixture {len(fixture)})")
    elif not bytes_ok_all:
        fail("BYTES", "wrong destination bytes: the cell's own check failed on a read")

    # EOF / STATX
    eofs = user_lines(log, "VC1A_EOF")
    for k, size in enumerate(EOF_SIZES):
        e = [x for x in eofs if x.get("k") == str(k)]
        if len(e) != 1 or e[0].get("n") != "0" or e[0].get("untouched") != "1" \
                or e[0].get("size") != str(size):
            fail("EOF", f"eof {k} (size {size}): {e}")
    st = user_lines(log, "VC1A_STATX")
    if len(st) != 1 or st[0].get("value") != str(len(fixture)):
        fail("EOF", f"STATX: {st}")

    # CLOSE
    cl = user_lines(log, "VC1A_CLOSE")
    if len(cl) != 1 or cl[0].get("ok") != "1":
        fail("CLOSE", f"{cl}")

    # ERRORS
    errs = user_lines(log, "VC1A_ERR")
    for case, want in ERR_EXPECT.items():
        e = [x for x in errs if x.get("case") == case]
        if len(e) != 1 or e[0].get("kind") != "status" or e[0].get("code") != str(want):
            fail("ERRORS", f"{case}: expected status {want}, got {e}")
    un = user_lines(log, "VC1A_ERR_UNTOUCHED")
    if len(un) != 1 or un[0].get("ok") != "1":
        fail("ERRORS", "an error left the destination modified")
    rc = user_lines(log, "VC1A_RECOVER")
    if len(rc) != 1 or rc[0].get("ok") != "1":
        fail("ERRORS", "a valid request did not work after the errors")

    # INIT
    ih = user_lines(log, "VC1A_INIT_HEAD")
    idata = bytearray()
    for d in user_lines(log, "VC1A_INIT_DATA"):
        idata += bytes.fromhex(d.get("hex", ""))
    if len(ih) != 1 or ih[0].get("n") != "64":
        fail("INIT", f"{ih}")
    elif bytes(idata) != init_head:
        fail("INIT", "wrong destination bytes: /initramfs/init head differs from rootfs/init")

    # GRANT
    got = [i for i, l in enumerate(log) if "msg=PM_VFS_GRANT_RO_RECEIVED image_id=7 " in l]
    spawn = user_lines(log, "PM_SPAWN_FROM_MO_DONE")
    spawn7 = [s for s in spawn if s.get("image_id") == "7"]
    if not got:
        fail("GRANT", "no PM_VFS_GRANT_RO_RECEIVED for image 7")
    if len(spawn7) != 1:
        fail("GRANT", f"PM_SPAWN_FROM_MO_DONE image_id=7: {len(spawn7)}")
    else:
        tid = spawn7[0].get("tid")
        runs = [x for x in user_lines(log, "DRIVER_MANAGER_ENTRY")
                if x["_tid"] == tid and x["_at"] > spawn7[0]["_at"]]
        if not runs:
            fail("GRANT", f"driver_manager (tid {tid}) never logged DRIVER_MANAGER_ENTRY")
        facts["late_service_tid"] = tid
    facts["grants"] = sum(1 for l in log if "msg=PM_VFS_GRANT_RO_RECEIVED " in l)
    facts["nr29_spawns"] = len(spawn)

    # TRANSPORT
    for tag in FORBIDDEN:
        hits = [l for l in log if tag in l]
        if hits:
            fail("TRANSPORT", f"{len(hits)} {tag} line(s): {hits[0].strip()[:120]}")
    return fails, facts


def fixture_bytes():
    return bytes(((i * 131 + 43) & 0xFF) for i in range(300))


def synthetic(fix, init_head, nc_no_copy=False):
    L = ["USER_LOG tid=1 msg=VC1A_BEGIN fixture_len=300 inline_max=104",
         "USER_LOG tid=1 msg=VC1A_OPEN ok=1 fd=14",
         "USER_LOG tid=1 msg=VC1A_STATX ok=1 value=300"]
    off = 0
    for k, (size, n) in enumerate(zip(SIZES, COUNTS)):
        dest = bytes([0xA5] * n) if nc_no_copy else fix[off:off + n]
        ok = "0" if nc_no_copy else "1"
        L.append(f"USER_LOG tid=1 msg=VC1A_READ k={k} size={size} n={n} off={off} bytes_ok={ok} tail_ok=1")
        for s in range(0, n, 32):
            L.append(f"USER_LOG tid=1 msg=VC1A_DATA k={k} off={off + s} hex={dest[s:s + 32].hex()}")
        off += n
    for k, size in enumerate(EOF_SIZES):
        L.append(f"USER_LOG tid=1 msg=VC1A_EOF k={k} size={size} n=0 untouched=1")
    L.append("USER_LOG tid=1 msg=VC1A_CLOSE ok=1 value=0")
    for case, want in ERR_EXPECT.items():
        L.append(f"USER_LOG tid=1 msg=VC1A_ERR case={case} expect={want} kind=status code={want}")
    L += ["USER_LOG tid=1 msg=VC1A_ERR_UNTOUCHED ok=1", "USER_LOG tid=1 msg=VC1A_RECOVER ok=1",
          "USER_LOG tid=1 msg=VC1A_INIT_HEAD n=64 sentinel_bytes=0"]
    for s in range(0, 64, 32):
        L.append(f"USER_LOG tid=1 msg=VC1A_INIT_DATA k=0 off={s} hex={init_head[s:s + 32].hex()}")
    L.append(f"USER_LOG tid=1 msg=VC1A_END fails={1 if nc_no_copy else 0} result={'fail' if nc_no_copy else 'ok'}")
    pm = ["USER_LOG tid=3 msg=PM_VFS_GRANT_RO_RECEIVED image_id=7 len=32960 cap=65554",
          "USER_LOG tid=3 msg=PM_SPAWN_FROM_MO_DONE image_id=7 tid=10003 caller_cap=1 spawner_cap=0",
          "USER_LOG tid=10003 msg=DRIVER_MANAGER_ENTRY"]
    return pm + L


def self_test():
    fix = fixture_bytes()
    head = bytes(range(64))
    good = synthetic(fix, head)
    f, _ = grade(good, fix, head, 0)
    assert not f, f

    def must_fail(lines, rule, needle="", rc=0):
        f, _ = grade(lines, fix, head, rc)
        assert any(x.startswith(rule + ":") and needle in x for x in f), (rule, needle, f)

    # The negative control's shape: right counts, no copy.
    nc = synthetic(fix, head, nc_no_copy=True)
    must_fail(nc, "BYTES", "wrong destination bytes")
    f, _ = grade(nc, fix, head, 0)
    assert not any(x.startswith("READS:") for x in f), f
    must_fail(good, "CORE", "rc=1", rc=1)
    must_fail([l.replace("n=104 off=13", "n=103 off=13") for l in good], "READS")
    must_fail([l.replace("tail_ok=1", "tail_ok=0", 1) for l in good], "READS", "tail")
    must_fail([l if "VC1A_DATA k=3 off=118" not in l else l[:-2] + "00" for l in good], "BYTES")
    must_fail([l for l in good if "VC1A_DATA k=5" not in l], "BYTES")
    must_fail([l.replace("VC1A_EOF k=2 size=32 n=0", "VC1A_EOF k=2 size=32 n=5") for l in good], "EOF")
    must_fail([l.replace("value=300", "value=299") for l in good], "EOF")
    must_fail([l.replace("VC1A_CLOSE ok=1", "VC1A_CLOSE ok=0") for l in good], "CLOSE")
    must_fail([l.replace("case=no_mount expect=3 kind=status code=3",
                         "case=no_mount expect=3 kind=malformed code=0") for l in good], "ERRORS", "no_mount")
    must_fail([l.replace("VC1A_RECOVER ok=1", "VC1A_RECOVER ok=0") for l in good], "ERRORS")
    must_fail(synthetic(fix, bytes(64)), "INIT")
    must_fail([l for l in good if "PM_VFS_GRANT_RO_RECEIVED" not in l], "GRANT")
    must_fail([l for l in good if "DRIVER_MANAGER_ENTRY" not in l], "GRANT", "never logged")
    must_fail(good + ["USER_LOG tid=10002 msg=VFS_FORWARD_CALL_FAIL op=12 err=WouldBlock"], "TRANSPORT")
    must_fail(good + ["USER_LOG tid=10002 msg=VFS_BACKEND_COMPLETION_MALFORMED op=12 fd=14"], "TRANSPORT")
    must_fail(good + ["USER_LOG tid=1 msg=VC1A_END fails=0 result=ok"], "CORE")
    print("VFS_CORRECT_WITNESS_GRADER_SELF_TEST result=ok mutants=17")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--arch")
    ap.add_argument("--smp", type=int, default=2)
    ap.add_argument("--core-rc", type=int, default=0)
    ap.add_argument("--fixture")
    ap.add_argument("--init-rootfs")
    ap.add_argument("log", nargs="?")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    with open(a.log, "r", errors="replace") as fh:
        log = fh.read().splitlines()
    with open(a.fixture, "rb") as fh:
        fixture = fh.read()
    with open(a.init_rootfs, "rb") as fh:
        init_head = fh.read(64)
    fails, facts = grade(log, fixture, init_head, a.core_rc)
    for f in fails:
        print(f"[vfs-correct-witness][fail] {f}")
    res = "ok" if not fails else "fail"
    extra = " ".join(f"{k}={v}" for k, v in sorted(facts.items()))
    print(f"VFS_CORRECT_WITNESS_SEAL arch={a.arch} smp={a.smp} result={res} {extra}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
