#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""VFS-CORRECT1C — grade one live client fd-namespace witness boot.

Input: the normalized serial log of a boot whose init, VFS and ramfs are built with
`vfs-fd-witness` (init/vfs_fd_witness.rs: a second ramfs mounted at /ram/b; each ramfs seeds
/ram/vc1c-file, /ram/b/vc1c-file and /ram/vc1c-failreply with bytes of its own TID), optionally
with a kernel built with `cap-gen-witness` (--wide). Canonical paths and mount routing are
re-derived here, not taken from the guest.

Rules (each failure names its rule):
  CORE       the core smoke passed (rc 0); one VC1C_BEGIN, one VC1C_FD_WITNESS_SEAL result=ok
             fails=0, every VC1C_CHECK ok=1 and each expected check present (canonical x8,
             canonical_initramfs x3, the rest once).
  COLLISION  the two opens right before VC1C_COLLISION are VFS_FD_OPEN target=/ram/ and
             target=/ram/b/ with the SAME backend_handle on two different instances, both
             ramfs instances logged that handle (RAMFS_FD_COUNT op=10), the two client fds are
             distinct and neither is the backend handle; each read its own bytes, one closed,
             the other read on, the closed fd answers BAD_FD (4).
  CANON      each VC1C_CANON spelling opened and read the bytes of the backend its canonical
             form names, and the VFS_ROUTE_LOOKUP that served it names that canonical path and
             mount (canonical=0: the spelling differed); the three initramfs spellings (refused
             on the baseline) succeed.
  BOUNDARY   /ram/bx routes to /ram/ (not /ram/b/) and fails there; /ram/b is the /ram/b/ root;
             /rbx is NO_MOUNT (3) with no forward.
  FULL       16 opens, then NO_FD (9) twice; exactly 16 OPENAT forwards in that step and two
             VFS_FD_NO_FREE reason=session_full; the backend's open count rose by 16, did not
             move on refusal and came back.
  FAILREPLY  status 5; the backend's next handle advanced by one and its open count did not
             move; VFS_OPEN_ROLLBACK injected=1 close=Some(Closed); the backend logged the open
             and the close of that handle; exactly one VFS_CLIENT_REPLY_REFUSED and one
             IPCREPLY_DIRECT_REFUSED_PRE_LOCK ... reason=reply_cap in the whole boot.
  THREAD     the fd a second thread (tid T != init) opened (VFS_FD_OPEN tid=T) is read and
             closed by init's main thread (VFS_FD_CLOSE tid=init, same session) after
             VC1C_THREAD_EXIT; both reads were the right bytes.
  FORK       the fork child (another session) opened two files and got the SAME fd numbers init
             holds, read its own backend's bytes, got BAD_FD for init's third fd, saw the second
             backend's open count up by two, and exited (VC1C_CHILD_EXIT) before init woke; VFS
             then reclaimed it (VFS_SESSION_RECLAIM tgid=child descriptors=2 backend_closed=2)
             and the second backend's open count came back; init's own fds still read its bytes.
  FINAL      both backends end with the open counts they started with; the last VFS_FD_STATE
             has pending_retry=0 quarantined=0 unknown_backend_opens=0.
  WIDTH      (--wide only) every VFS_FORWARD_CALL send_cap and reply_cap exceeds 2^32 with a
             generation (id >> 16) above 65535.
  RACE       the one D2 publish-race checker (scripts/check-d2-publish-race.py, IPC-D2-RULE1,
             doc/AI_AGENT_RULES.md §14.3) gives the boot verdict ok: no broad-route race, no
             fail-closed receive, no prohibited displacement, and every split queue-publication race
             attributed one-to-one to its wait, its queued call, its exact restored unwind, one
             delivery and one committed reply. Facts: publish_race_unwinds, races_recovered.
  TRANSPORT  none of VFS_FORWARD_CALL_FAIL, VFS_BACKEND_COMPLETION_MALFORMED,
             VFS_CALLER_SESSION_FAIL, VFS_SESSION_UNKNOWN, VFS_SESSION_ALIVE_FAIL,
             VFS_OPEN_BACKEND_STATE_UNKNOWN, PM_VFS_CALL_FAIL, BOOT_FATAL.
Exit status 0 = sealed, 1 = not sealed. --self-test checks the rules on synthetic boots.
"""
import argparse
import importlib.util
import os
import re
import sys


def _d2_checker():
    """scripts/check-d2-publish-race.py, loaded by path (this grader runs under `python3 -I`)."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "check-d2-publish-race.py")
    spec = importlib.util.spec_from_file_location("check_d2_publish_race", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

LEN = 96
WIDE = 1 << 32
MOUNTS = ["/initramfs/", "/dev/", "/ram/", "/ram/b/"]
# Boot mounts carry their server's name as the label; dynamic mounts their stored prefix.
LABELS = {"/initramfs/": "initramfs", "/dev/": "devfs"}
FORBIDDEN = ("VFS_FORWARD_CALL_FAIL", "VFS_BACKEND_COMPLETION_MALFORMED", "VFS_CALLER_SESSION_FAIL",
             "VFS_SESSION_UNKNOWN", "VFS_SESSION_ALIVE_FAIL", "VFS_OPEN_BACKEND_STATE_UNKNOWN",
             "PM_VFS_CALL_FAIL", "BOOT_FATAL")
CHECKS = {"collision_aligned": 1, "collision_distinct_fds": 1, "collision_bytes_a": 1,
          "collision_bytes_b": 1, "collision_close_one_keep_other": 1,
          "collision_backends_released": 1, "canonical": 8, "canonical_initramfs": 3,
          "boundary": 1, "full_no_fd": 1, "full_nothing_opened_at_backend": 1,
          "failreply_rolled_back": 1, "thread_shares_session": 1, "fork_session_own_files": 1,
          "fork_session_reclaimed_on_exit": 1, "final_backend_counts": 1}


def fields(text):
    return dict(re.findall(r"(\w+)=(\S+)", text))


def tagged(log, tag, lo=0, hi=None):
    out = []
    hi = len(log) if hi is None else hi
    for i in range(lo, hi):
        m = re.search(r"(?:USER_LOG tid=(\d+) msg=)?\b(" + tag + r")\b(?: (.*))?$", log[i])
        if m:
            f = fields(m.group(3) or "")
            f["_tid"] = m.group(1)
            f["_at"] = i
            out.append(f)
    return out


def num(f, k):
    try:
        return int(f.get(k, ""))
    except (ValueError, TypeError):
        return None


def normalize(path):
    out = []
    for c in path.split("/"):
        if c in ("", "."):
            continue
        if c == "..":
            if out:
                out.pop()
            continue
        out.append(c)
    return "/" + "/".join(out)


def route(path):
    best = None
    for m in MOUNTS:
        base = m.rstrip("/")
        if (path == base or path.startswith(m)) and (best is None or len(m) > len(best)):
            best = m
    return LABELS.get(best, best)


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
    begins, seals = tagged(log, "VC1C_BEGIN"), tagged(log, "VC1C_FD_WITNESS_SEAL")
    if len(begins) != 1 or len(seals) != 1:
        fail("CORE", f"expected one VC1C_BEGIN and one seal, saw {len(begins)}/{len(seals)}")
        return fails, facts
    seal = seals[0]
    if seal.get("result") != "ok" or seal.get("fails") != "0":
        fail("CORE", f"seal result={seal.get('result')} fails={seal.get('fails')}")
    init_tid = begins[0]["_tid"]
    tid_a, tid_b = seal.get("tid_a"), seal.get("tid_b")
    facts.update(init_tid=init_tid, ramfs_a=tid_a, ramfs_b=tid_b)
    seen = {}
    for c in tagged(log, "VC1C_CHECK"):
        seen[c.get("name")] = seen.get(c.get("name"), 0) + 1
        if c.get("ok") != "1":
            fail("CORE", f"check {c.get('name')} ok={c.get('ok')}")
    for name, n in CHECKS.items():
        if seen.get(name, 0) != n:
            fail("CORE", f"check {name} seen {seen.get(name, 0)} times (want {n})")
    vfs_tid = None
    ready = tagged(log, "VFS_FD_SPACE_READY")
    if ready:
        vfs_tid = ready[0]["_tid"]

    # COLLISION
    col = tagged(log, "VC1C_COLLISION")
    align = tagged(log, "VC1C_ALIGN")
    res = tagged(log, "VC1C_COLLISION_RESULT")
    if len(col) != 1 or not align or len(res) != 1:
        fail("COLLISION", "missing VC1C_ALIGN / VC1C_COLLISION / VC1C_COLLISION_RESULT")
    else:
        c, lo = col[0], align[-1]["_at"]
        opens = [o for o in tagged(log, "VFS_FD_OPEN", lo, c["_at"]) if o.get("tgid") == init_tid]
        fd_a, fd_b = num(c, "client_fd_a"), num(c, "client_fd_b")
        oa = [o for o in opens if o.get("target") == "/ram/"]
        ob = [o for o in opens if o.get("target") == "/ram/b/"]
        if not oa or not ob:
            fail("COLLISION", f"no VFS_FD_OPEN on both backends before VC1C_COLLISION ({len(oa)}/{len(ob)})")
        else:
            oa, ob = oa[-1], ob[-1]
            ha, hb = num(oa, "backend_handle"), num(ob, "backend_handle")
            facts.update(backend_handle_a=ha, backend_handle_b=hb, client_fd_a=fd_a, client_fd_b=fd_b,
                         instance_a=oa.get("instance"), instance_b=ob.get("instance"))
            if ha is None or ha != hb:
                fail("COLLISION", f"backend handles differ ({ha}/{hb}): no collision was exercised")
            if oa.get("instance") == ob.get("instance"):
                fail("COLLISION", "both opens on one instance")
            if num(oa, "client_fd") != fd_a or num(ob, "client_fd") != fd_b:
                fail("COLLISION", "VFS's client fds are not the ones init received")
            if fd_a == fd_b:
                fail("COLLISION", f"one client fd ({fd_a}) for two open files")
            if ha in (fd_a, fd_b):
                fail("COLLISION", f"a client fd is the backend handle {ha}")
            ra = [r for r in tagged(log, "RAMFS_FD_COUNT", lo, c["_at"])
                  if r["_tid"] == tid_a and r.get("op") == "10" and num(r, "handle") == ha]
            rb = [r for r in tagged(log, "RAMFS_FD_COUNT", lo, c["_at"])
                  if r["_tid"] == tid_b and r.get("op") == "10" and num(r, "handle") == hb]
            if not ra or not rb:
                fail("COLLISION", "the two ramfs instances did not both log the shared handle")
        r = res[0]
        want = {"read_a": "1", "read_b": "1", "close_a": "0", "read_b_after": "1",
                "read_a_after_close": "4", "close_b": "0"}
        bad = {k: r.get(k) for k, v in want.items() if r.get(k) != v}
        if bad:
            fail("COLLISION", f"result {bad}")

    # CANON
    canons = tagged(log, "VC1C_CANON")
    ram = [x for x in canons if x.get("backend") in ("0", "1")]
    ini = [x for x in canons if x.get("backend") == "initramfs"]
    if len(ram) != 8 or len(ini) != 3:
        fail("CANON", f"{len(ram)} ramfs and {len(ini)} initramfs spellings (want 8 and 3)")
    prev = col[0]["_at"] if col else 0
    for x in canons:
        spelling = x.get("path", "")
        canon = normalize(spelling)
        mount = route(canon)
        lookups = [lk for lk in tagged(log, "VFS_ROUTE_LOOKUP", prev, x["_at"])
                   if lk.get("canonical") == "0"]
        prev = x["_at"]
        if not lookups:
            fail("CANON", f"{spelling}: no non-canonical route lookup")
            continue
        if any(lk.get("path") != canon or lk.get("target") != mount for lk in lookups):
            fail("CANON", f"{spelling}: VFS routed {[(lk.get('path'), lk.get('target')) for lk in lookups]},"
                          f" want {canon} on {mount}")
        if x.get("backend") in ("0", "1"):
            if {"0": "/ram/", "1": "/ram/b/"}[x["backend"]] != mount:
                fail("CANON", f"{spelling}: the cell's backend {x['backend']} is not {mount}")
            if x.get("open") != "0" or x.get("bytes") != "1":
                fail("CANON", f"{spelling}: open={x.get('open')} bytes={x.get('bytes')}")
            if (num(x, "statx") or 0) <= 0 or ((num(x, "statx") >> 16) & 0xFFFFFFFF) != LEN:
                fail("CANON", f"{spelling}: statx={x.get('statx')}")
        elif x.get("statx") != "0" or x.get("open") != "0":
            fail("CANON", f"{spelling}: statx={x.get('statx')} open={x.get('open')}")

    # BOUNDARY
    bnd = tagged(log, "VC1C_BOUNDARY")
    if len(bnd) != 1:
        fail("BOUNDARY", "no VC1C_BOUNDARY")
    else:
        b = bnd[0]
        if b.get("ram_bx") in ("0", "3") or b.get("ram_b") != "0" or b.get("rbx") != "3":
            fail("BOUNDARY", f"ram_bx={b.get('ram_bx')} ram_b={b.get('ram_b')} rbx={b.get('rbx')}")
        lk = tagged(log, "VFS_ROUTE_LOOKUP", prev, b["_at"])
        if not any(x.get("path") == "/ram/bx" and x.get("target") == "/ram/" for x in lk):
            fail("BOUNDARY", "/ram/bx was not routed to /ram/")
        if not any(x.get("path") == "/ram/b" and x.get("target") == "/ram/b/" for x in lk):
            fail("BOUNDARY", "/ram/b was not routed to /ram/b/")
        if any(x.get("path") == "/rbx" for x in lk) or not tagged(log, "VFS_ROUTE_NO_MOUNT", prev, b["_at"]):
            fail("BOUNDARY", "/rbx was routed")
        prev = b["_at"]

    # FULL
    full = tagged(log, "VC1C_FULL")
    if len(full) != 1:
        fail("FULL", "no VC1C_FULL")
    else:
        f = full[0]
        n0, n1, n2, n3 = (num(f, k) for k in ("backend_open_before", "after_fill", "after_refusal", "after_close"))
        if f.get("opened") != "16" or f.get("refused_status") != "9" or f.get("again_status") != "9":
            fail("FULL", f"opened={f.get('opened')} refused={f.get('refused_status')} again={f.get('again_status')}")
        if None in (n0, n1, n2, n3) or n1 != n0 + 16 or n2 != n1 or n3 != n0 or f.get("closed") != "16":
            fail("FULL", f"backend open counts {n0} {n1} {n2} {n3} closed={f.get('closed')}")
        fwd = [x for x in tagged(log, "VFS_FORWARD_CALL", prev, f["_at"]) if x.get("op") == "10"]
        nofree = [x for x in tagged(log, "VFS_FD_NO_FREE", prev, f["_at"]) if x.get("reason") == "session_full"]
        if len(fwd) != 16 or len(nofree) != 2:
            fail("FULL", f"{len(fwd)} OPENAT forwards and {len(nofree)} NO_FREE (want 16 and 2)")
        prev = f["_at"]

    # FAILREPLY
    fr = tagged(log, "VC1C_FAILREPLY")
    if len(fr) != 1:
        fail("FAILREPLY", "no VC1C_FAILREPLY")
    else:
        f = fr[0]
        nb = [num(f, k) for k in ("backend_next_before", "backend_open_before")]
        after = re.findall(r"after=(\d+)", log[f["_at"]])
        if f.get("status") != "5" or len(after) != 2 or None in nb \
                or int(after[0]) != nb[0] + 1 or int(after[1]) != nb[1]:
            fail("FAILREPLY", f"status={f.get('status')} next {nb[0]}->{after[:1]} open {nb[1]}->{after[1:]}")
        rb = tagged(log, "VFS_OPEN_ROLLBACK", prev, f["_at"])
        if len(rb) != 1 or rb[0].get("injected") != "1" or rb[0].get("close") != "Some(Closed)":
            fail("FAILREPLY", f"rollback lines {rb}")
        else:
            h = rb[0].get("backend_handle")
            ops = [(x.get("op"), x.get("handle")) for x in tagged(log, "RAMFS_FD_COUNT", prev, f["_at"])
                   if x["_tid"] == tid_a and x.get("handle") == h]
            if ("10", h) not in ops or ("11", h) not in ops:
                fail("FAILREPLY", f"backend did not log the open and close of handle {h}: {ops}")
        prev = f["_at"]
    refused = tagged(log, "VFS_CLIENT_REPLY_REFUSED")
    kref = [x for x in tagged(log, "IPCREPLY_DIRECT_REFUSED_PRE_LOCK") if x.get("reason") == "reply_cap"]
    if len(refused) != 1 or len(kref) != 1:
        fail("FAILREPLY", f"{len(refused)} client-reply refusals and {len(kref)} kernel refusals (want 1 and 1)")

    # THREAD
    th = tagged(log, "VC1C_THREAD")
    tx = tagged(log, "VC1C_THREAD_EXIT")
    if len(th) != 1 or len(tx) != 1:
        fail("THREAD", "no VC1C_THREAD / VC1C_THREAD_EXIT")
    else:
        t = th[0]
        ttid, fd = t.get("tid"), t.get("fd")
        if ttid in (None, "0", init_tid):
            fail("THREAD", f"thread tid {ttid}")
        if t.get("thread_read") != "1" or t.get("main_read_after_exit") != "1" or t.get("main_close") != "0":
            fail("THREAD", f"thread_read={t.get('thread_read')} main_read={t.get('main_read_after_exit')} close={t.get('main_close')}")
        op = [x for x in tagged(log, "VFS_FD_OPEN", prev, t["_at"]) if x.get("tid") == ttid]
        cl = [x for x in tagged(log, "VFS_FD_CLOSE", tx[0]["_at"], t["_at"]) if x.get("client_fd") == fd]
        if len(op) != 1 or op[0].get("client_fd") != fd or op[0].get("tgid") != init_tid:
            fail("THREAD", f"the thread's open {op}")
        if len(cl) != 1 or cl[0].get("tid") != init_tid or cl[0].get("tgid") != init_tid:
            fail("THREAD", f"the main thread's close after the thread exited {cl}")
        prev = t["_at"]

    # FORK
    holds = tagged(log, "VC1C_FORK_PARENT_HOLDS")
    child = tagged(log, "VC1C_CHILD")
    cexit = tagged(log, "VC1C_CHILD_EXIT")
    wake = tagged(log, "VC1C_FORK_PARENT_WAKE")
    parent = tagged(log, "VC1C_FORK_PARENT")
    capmap = tagged(log, "VC1C_FORK_CAPMAP")
    ccaps = tagged(log, "VC1C_CHILD_CAPS")
    if not (len(holds) == len(child) == len(cexit) == len(wake) == len(parent) == len(capmap) == len(ccaps) == 1):
        fail("FORK", "missing fork lines")
    else:
        pfds = holds[0].get("fds", "").split(",")
        c = child[0]
        ctid = c["_tid"]
        facts["fork_child"] = ctid
        if ctid == init_tid or parent[0].get("child") != ctid:
            fail("FORK", f"child tid {ctid}")
        # The child's copies may or may not get the parent's numbers (a fresh CNode at the same
        # rank and initial generation gives the same ID); what matters is that it found them.
        if ccaps[0].get("vfs") in (None, "0") or ccaps[0].get("reply") in (None, "0"):
            fail("FORK", f"child caps {ccaps[0]} (parent {capmap[0]})")
        if [c.get("fd1"), c.get("fd2")] != pfds[:2]:
            fail("FORK", f"child fds {c.get('fd1')},{c.get('fd2')} are not init's numbers {pfds[:2]}")
        if c.get("read_own") != "1" or c.get("read_parents_fd") != "4" or c.get("parents_fd") != pfds[2]:
            fail("FORK", f"child read_own={c.get('read_own')} parents_fd={c.get('read_parents_fd')}")
        if num(c, "open_b") != (num(c, "open_b_before") or 0) + 2:
            fail("FORK", f"child saw open_b={c.get('open_b')} (before {c.get('open_b_before')})")
        if not cexit[0]["_at"] < wake[0]["_at"]:
            fail("FORK", "the child had not exited when init woke")
        copens = [x for x in tagged(log, "VFS_FD_OPEN", holds[0]["_at"], cexit[0]["_at"]) if x.get("tgid") == ctid]
        if sorted(x.get("client_fd") for x in copens) != sorted(pfds[:2]):
            fail("FORK", f"VFS opens for the child {copens}")
        rec = [x for x in tagged(log, "VFS_SESSION_RECLAIM", wake[0]["_at"], parent[0]["_at"]) if x.get("tgid") == ctid]
        if len(rec) != 1 or rec[0].get("descriptors") != "2" or rec[0].get("backend_closed") != "2":
            fail("FORK", f"reclaim of the child {rec}")
        p = parent[0]
        if p.get("open_b_after_child_exit") != p.get("open_b_before") or p.get("parent_read_after") != "1" \
                or p.get("parent_closed") != "3":
            fail("FORK", f"parent {p}")

    # FINAL
    fin = tagged(log, "VC1C_FINAL")
    if len(fin) != 1 or fin[0].get("open_a") != fin[0].get("open_a_start") \
            or fin[0].get("open_b") != fin[0].get("open_b_start"):
        fail("FINAL", f"final counts {fin}")
    states = [x for x in tagged(log, "VFS_FD_STATE") if x["_at"] < seal["_at"]]
    if not states or any(states[-1].get(k) != "0" for k in ("pending_retry", "quarantined", "unknown_backend_opens")):
        fail("FINAL", f"last VFS_FD_STATE {states[-1] if states else None}")
    if states:
        facts["vfs_state_end"] = {k: states[-1].get(k) for k in ("descriptors", "ofds", "sessions")}

    # WIDTH
    if want_wide:
        fwd = tagged(log, "VFS_FORWARD_CALL")
        narrow = [x for x in fwd if not (wide(num(x, "send_cap")) and wide(num(x, "reply_cap")))]
        if not fwd or narrow:
            fail("WIDTH", f"{len(narrow)} of {len(fwd)} forwards use a narrow identity")

    # RACE — the one checker every gate uses
    rep = _d2_checker().check_lines(log)
    facts["publish_race_unwinds"] = len(rep.events)
    facts["races_recovered"] = sum(1 for e in rep.events if e.get("recovered"))
    if rep.verdict() != "ok":
        fail("RACE", f"D2 publish-race verdict={rep.verdict()}: "
             + "; ".join((rep.defects + rep.unverified)[:4]))

    # TRANSPORT
    for bad in FORBIDDEN:
        if any(bad in line for line in log):
            fail("TRANSPORT", bad)
    return fails, facts


def synthetic(nc=None):
    I, V, A, B, T, C = "1", "10002", "10006", "10007", "10008", "10009"
    H = 105
    u = lambda tid, msg: f"USER_LOG tid={tid} msg={msg}"
    cap = lambda k: (0x1_0001 << 16) + k
    fwd = lambda op, tgt: u(V, f"VFS_FORWARD_CALL op={op} target={tgt} send_cap={cap(12)} reply_cap={cap(1)} instance=3")
    L = [u(V, "VFS_FD_SPACE_READY max_descriptors=32 max_per_session=16 first_fd=3 fail_reply_injection=1"),
         u(I, f"VC1C_BEGIN tid_a={A}"),
         u(I, f"VC1C_ALIGN next_a={H} next_b={H} open_a=1 open_b=1 steps=0")]
    fa, fb = (H, H) if nc == "handle_as_fd" else (3, 4)
    L += [fwd(10, "/ram/"), u(V, f"VFS_FD_OPEN tgid={I} incarnation=4 tid={I} client_fd={fa} instance=3 target=/ram/ backend_handle={H}"),
          u(A, f"RAMFS_FD_COUNT op=10 handle={H} next_handle={H + 1} open_handles=2"),
          fwd(10, "/ram/b/"), u(V, f"VFS_FD_OPEN tgid={I} incarnation=4 tid={I} client_fd={fb} instance=4 target=/ram/b/ backend_handle={H}"),
          u(B, f"RAMFS_FD_COUNT op=10 handle={H} next_handle={H + 1} open_handles=2"),
          u(I, f"VC1C_COLLISION client_fd_a={fa} client_fd_b={fb} backend_handle_a={H} backend_handle_b={H}"),
          u(I, "VC1C_COLLISION_RESULT read_a=1 read_b=1 close_a=0 read_b_after=1 read_a_after_close=4 close_b=0")]
    for name in ("collision_aligned", "collision_distinct_fds", "collision_bytes_a", "collision_bytes_b",
                 "collision_close_one_keep_other", "collision_backends_released"):
        L.append(u(I, f"VC1C_CHECK name={name} ok=1"))
    statx = (1 << 60) | (LEN << 16) | 0x180
    for sp, be in (("/ram//vc1c-file", 0), ("/ram/./vc1c-file", 0), ("/ram/b/../vc1c-file", 0),
                   ("/ram/b/x/../../vc1c-file", 0), ("//ram/b//vc1c-file/", 1), ("/ram/b/./vc1c-file", 1),
                   ("/initramfs/../ram/b/vc1c-file", 1), ("/ram/./b/x/../vc1c-file", 1)):
        canon = normalize(sp)
        if nc == "misroute" and be == 1:
            L.append(u(V, f"VFS_ROUTE_LOOKUP path={canon} target=/ram/ instance=3 canonical=0"))
        else:
            L.append(u(V, f"VFS_ROUTE_LOOKUP path={canon} target={route(canon)} instance=3 canonical=0"))
        L += [u(I, f"VC1C_CANON path={sp} backend={be} statx={statx} open=0 bytes=1"), u(I, "VC1C_CHECK name=canonical ok=1")]
    for sp in ("/initramfs//etc/vfs-fixture", "/initramfs/etc/./vfs-fixture", "/ram/../initramfs/etc/vfs-fixture"):
        st = "2" if nc == "spelling" else "0"
        L += [u(V, f"VFS_ROUTE_LOOKUP path={normalize(sp)} target=initramfs instance=1 canonical=0"),
              u(I, f"VC1C_CANON path={sp} backend=initramfs statx={st} open={st}"),
              u(I, f"VC1C_CHECK name=canonical_initramfs ok={'0' if st != '0' else '1'}")]
    L += [u(V, "VFS_ROUTE_LOOKUP path=/ram/bx target=/ram/ instance=3 canonical=1"),
          u(V, "VFS_ROUTE_LOOKUP path=/ram/b target=/ram/b/ instance=4 canonical=1"),
          u(V, "VFS_ROUTE_NO_MOUNT path=/rbx"),
          u(I, "VC1C_BOUNDARY ram_bx=2 ram_b=0 rbx=3"), u(I, "VC1C_CHECK name=boundary ok=1")]
    nfwd = 18 if nc == "full_forwards" else 16
    L += [fwd(10, "/ram/") for _ in range(nfwd)]
    L += [u(V, "VFS_FD_NO_FREE tgid=1 incarnation=4 reason=session_full status=9")] * 2
    L += [u(I, "VC1C_FULL opened=16 refused_status=9 again_status=9 backend_open_before=1 after_fill=17 after_refusal=17 after_close=1 closed=16"),
          u(I, "VC1C_CHECK name=full_no_fd ok=1"), u(I, "VC1C_CHECK name=full_nothing_opened_at_backend ok=1")]
    close = "None" if nc == "no_rollback" else "Some(Closed)"
    L += [u(A, "RAMFS_FD_COUNT op=10 handle=126 next_handle=127 open_handles=2"),
          u(V, f"VFS_CLIENT_REPLY_REFUSED reply_cap={cap(40) + (1 << 16)} err=InvalidCapability"),
          f"IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=18446744073709551615 record_generation=18446744073709551615 replier_tid={V} reason=reply_cap err=InvalidCapability reply_copies=0 caller_wakes=0"]
    if nc != "no_rollback":
        L.append(u(A, "RAMFS_FD_COUNT op=11 handle=126 next_handle=127 open_handles=1"))
    L += [u(V, f"VFS_OPEN_ROLLBACK tgid=1 client_fd=3 instance=3 backend_handle=126 injected=1 close={close}"),
          u(I, f"VC1C_FAILREPLY status=5 backend_next_before=126 after=127 backend_open_before=1 after={2 if nc == 'no_rollback' else 1}"),
          u(I, "VC1C_CHECK name=failreply_rolled_back ok=1")]
    otid = T if nc == "sender_session" else I
    L += [u(V, f"VFS_FD_OPEN tgid={otid} incarnation=4 tid={T} client_fd=3 instance=3 target=/ram/ backend_handle=127"),
          u(T, "VC1C_THREAD_OPENED fd=3 read_ok=1"), u(T, "VC1C_THREAD_EXIT")]
    if nc == "sender_session":
        L += [u(V, "VFS_FD_BAD op=12 fd=3 tgid=1 reason=unknown")]
    else:
        L += [u(V, f"VFS_FD_CLOSE tgid={I} tid={I} client_fd=3 instance=3 backend_handle=127 outcome=Closed")]
    L += [u(I, f"VC1C_THREAD tid={T} fd=3 thread_read=1 main_read_after_exit={0 if nc == 'sender_session' else 1} main_close=0"),
          u(I, "VC1C_CHECK name=thread_shares_session ok=1"),
          u(I, "VC1C_FORK_PARENT_HOLDS fds=3,4,5 read=1 open_b=1"),
          u(I, f"VC1C_FORK_CAPMAP parent_vfs={cap(12)} rank_vfs=9 parent_reply={cap(1)} rank_reply=1 g0={0x1_0001}"),
          u(C, f"VC1C_CHILD_CAPS vfs={cap(9)} reply={cap(1)}"),
          u(V, f"VFS_FD_OPEN tgid={C} incarnation=9 tid={C} client_fd=3 instance=4 target=/ram/b/ backend_handle=130"),
          u(V, f"VFS_FD_OPEN tgid={C} incarnation=9 tid={C} client_fd=4 instance=4 target=/ram/b/ backend_handle=131"),
          u(C, "VC1C_CHILD fd1=3 fd2=4 read_own=1 read_parents_fd=4 parents_fd=5 open_b=3 open_b_before=1"),
          u(C, "VC1C_CHILD_EXIT closed=0"),
          u(I, f"VC1C_FORK_PARENT_WAKE child={C} slept=1")]
    if nc != "no_reclaim":
        L.append(u(V, f"VFS_SESSION_RECLAIM tgid={C} incarnation=9 descriptors=2 backend_closed=2"))
    L += [u(I, f"VC1C_FORK_PARENT child={C} open_b_before=1 open_b_after_child_exit={3 if nc == 'no_reclaim' else 1} parent_read_after=1 parent_closed=3"),
          u(I, "VC1C_CHECK name=fork_session_own_files ok=1"), u(I, "VC1C_CHECK name=fork_session_reclaimed_on_exit ok=1"),
          u(I, "VC1C_FINAL open_a=1 open_a_start=1 open_b=1 open_b_start=1"),
          u(I, "VC1C_CHECK name=final_backend_counts ok=1"),
          u(V, "VFS_FD_STATE why=close descriptors=0 ofds=0 sessions=0 pending_retry=0 quarantined=0 pending_overflow=0 unknown_backend_opens=0"),
          u(I, f"VC1C_FD_WITNESS_SEAL result=ok fails=0 tid_a={A} tid_b={B}")]
    return L


def self_test():
    good = synthetic()
    f, _ = grade(good, 0, True)
    assert not f, f

    def must_fail(lines, rule, needle="", rc=0, w=True):
        f, _ = grade(lines, rc, w)
        hits = [x for x in f if x.startswith(rule + ":") and needle in x]
        assert hits, (rule, needle, f)

    must_fail(synthetic(nc="handle_as_fd"), "COLLISION", "one client fd")
    must_fail(synthetic(nc="handle_as_fd"), "COLLISION", "is the backend handle")
    must_fail(synthetic(nc="spelling"), "CANON", "statx=2")
    must_fail(synthetic(nc="misroute"), "CANON", "VFS routed")
    must_fail(synthetic(nc="full_forwards"), "FULL", "OPENAT forwards")
    must_fail(synthetic(nc="no_rollback"), "FAILREPLY")
    must_fail(synthetic(nc="sender_session"), "THREAD")
    must_fail(synthetic(nc="no_reclaim"), "FORK", "reclaim")
    must_fail(good, "CORE", "rc=1", rc=1)
    must_fail([x.replace("result=ok fails=0", "result=fail fails=1") for x in good], "CORE")
    must_fail([x for x in good if "name=boundary" not in x], "CORE", "boundary")
    must_fail([x.replace("backend_handle=105", "backend_handle=106") if "target=/ram/b/" in x else x
               for x in good], "COLLISION", "differ")
    must_fail([x for x in good if not ("RAMFS_FD_COUNT op=10 handle=105" in x and "10007" in x)], "COLLISION", "both log")
    must_fail([x.replace("read_a_after_close=4", "read_a_after_close=0") for x in good], "COLLISION", "result")
    must_fail([x.replace("rbx=3", "rbx=2") for x in good], "BOUNDARY")
    must_fail([x.replace("path=/ram/bx target=/ram/", "path=/ram/bx target=/ram/b/") for x in good], "BOUNDARY")
    must_fail([x.replace("after_refusal=17", "after_refusal=18") for x in good], "FULL", "counts")
    must_fail([x for x in good if "VFS_CLIENT_REPLY_REFUSED" not in x], "FAILREPLY", "refusals")
    must_fail(good + [good[[i for i, x in enumerate(good) if "VFS_CLIENT_REPLY_REFUSED" in x][0]]], "FAILREPLY", "refusals")
    must_fail([x.replace("VC1C_THREAD_EXIT", "VC1C_THREAD_EXIT_X") for x in good], "THREAD")
    must_fail([x.replace("fd1=3 fd2=4", "fd1=6 fd2=7") for x in good], "FORK", "init's numbers")
    must_fail([x.replace("read_parents_fd=4", "read_parents_fd=0") for x in good], "FORK", "parents_fd")
    cx = [i for i, x in enumerate(good) if "VC1C_CHILD_EXIT" in x][0]
    wk = [i for i, x in enumerate(good) if "VC1C_FORK_PARENT_WAKE" in x][0]
    swapped = list(good)
    swapped[cx], swapped[wk] = swapped[wk], swapped[cx]
    must_fail(swapped, "FORK", "had not exited")
    must_fail([x.replace("quarantined=0", "quarantined=1") for x in good], "FINAL")
    must_fail([x.replace(f"send_cap={(0x1_0001 << 16) + 12}", "send_cap=12") for x in good], "WIDTH")
    f, _ = grade([x.replace(f"send_cap={(0x1_0001 << 16) + 12}", "send_cap=12") for x in good], 0, False)
    assert not f, f
    for bad in FORBIDDEN:
        must_fail(good + [f"USER_LOG tid=10002 msg={bad} op=10"], "TRANSPORT", bad)
    race = ["D2_PUBLISH_RACE_UNWIND endpoint=split tid=10002", "D2_RECV_WAITER_RACE_UNWIND tid=10002"]
    must_fail(good + race, "RACE", "verdict=defect")
    must_fail(good + race[:1], "RACE")
    must_fail(good + race[1:], "RACE")
    must_fail(good + ["IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED tid=10002"], "RACE")
    must_fail(good + ["D2_PUBLISH_RACE_UNWIND endpoint=10 tid=10002", "D2_RECV_WAITER_RACE_UNWIND tid=10002"], "RACE")
    legal = _d2_checker()._synthetic()
    f, facts = grade(good + legal, 0, True)
    assert not f and facts["publish_race_unwinds"] == 2 and facts["races_recovered"] == 2, (f, facts)
    must_fail(good + [x for x in legal if "resolution=commit" not in x or "record_index=5 " not in x], "RACE")
    print("VFS_FD_WITNESS_GRADER_SELF_TEST result=ok")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log", nargs="?")
    ap.add_argument("--arch", default="?")
    ap.add_argument("--smp", default="?")
    ap.add_argument("--core-rc", type=int, default=0)
    ap.add_argument("--wide", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test()
        return 0
    with open(a.log, encoding="utf-8", errors="replace") as fh:
        log = fh.read().splitlines()
    fails, facts = grade(log, a.core_rc, a.wide)
    for k, v in facts.items():
        print(f"fact {k}={v}")
    for f in fails:
        print(f"FAIL {f}")
    print(f"VFS_FD_WITNESS_SEAL arch={a.arch} smp={a.smp} wide={int(a.wide)} "
          f"result={'ok' if not fails else 'fail'} fails={len(fails)}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
