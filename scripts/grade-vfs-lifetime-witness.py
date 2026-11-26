#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""VFS-LIFETIME1 — grade one live file-grant lifetime witness boot (audit finding F08).

Input: the normalized serial log of a boot built with `vfs-lifetime-witness` (kernel: the grant
ledger `VL1_LEDGER` after every NR 22 / NR 28 and `VL1_GRANT_USE` at every NR 29; init:
init/vfs_lifetime_witness.rs; PM's refusal image 14; initramfs's and VFS's handoff injections).

Every holder of a FILE_GRANT_RO capability is identified from the log itself: initramfs is the
task that creates slice objects (NR 28), PM is TID 3 (the only NR 29 caller), VFS is the task that
logs VFS_GRANT_*; init is the task that logs VL1_CHECKPOINT. A CHECKPOINT is init's
`CapRelinquish(0)` probe, refused and mutating nothing; its ledger is the `VL1_LEDGER
event=nr22_refused tid=<init>` line printed just before init's `VL1_CHECKPOINT` line.

Rules (each failure names its rule):
  DRIVER     one VL1_BEGIN and one VL1_WITNESS_DONE driver_ok=1; every checkpoint (warm,
             success_1..3, the five failed handoffs, holder_exit, after) present once, in order,
             each probe refused_invalid; no BOOT_FATAL; no ledger truncated.
  BASELINE   the warm checkpoint (after the ordinary boot's own grant launches: driver_manager,
             ...) holds no live slice object, no slice capability reference and no slice slot in
             any CNode — the boot's grants were all reclaimed.
  RECLAIM    every later checkpoint equals warm in live slice objects, slice capability
             references and slice slots (in every CNode), and initramfs's and VFS's occupied slot
             counts equal warm's. (PM's own occupancy may grow by spawn outputs — capabilities of
             live children, not grant resources — and is reported, not checked.) Across the five
             failed handoffs the delegation-link count is also unchanged. The ONE allowed
             difference is the holder's: from holder_exit on, exactly one object, one reference
             and one slot, all in the exited holder's CNode (see HOLDER).
  SURVIVAL   every successful launch (boot launches and the witness's) used its grant at NR 29
             with cap_refcount=1: before the use, both intermediaries released THAT object
             (CAP_RELINQUISH_OK mo_id=M from initramfs and from VFS, each reparented=1 — a copy
             downstream survived each release), and the use came after both; then PM's own
             release reclaimed it (reclaimed=1). The success cycles' VL1_CYCLE spawned=1.
  FAILPATHS  each failed handoff refused the spawn (spawned=0) and showed its own cleanup:
               hop1_undelivered  INITRAMFS_GRANT_RELEASE why=undelivered inject=1, reclaimed
               hop1_rejected     initramfs held inject=2; VFS_GRANT_RELEASE why=rejected_*;
                                 initramfs released why=closed
               hop2_undelivered  VFS_GRANT_INJECT kind=stale_reply delivered=0;
                                 VFS_GRANT_RELEASE why=reply_refused
               hop2_rejected     PM_GRANT_RELEASE why=rejected_completion
               nr29_refused      VL1_GRANT_USE image_id=14 cap_refcount=1;
                                 PM_GRANT_RELEASE why=spawn_refused; VL1_PM_SPAWN_REFUSED_EXPECTED
  HOLDER     the fork child held the fixture's grant ALONE (a ledger inside the holder segment
             with one live slice object whose only slice slot is in a CNode that is none of
             initramfs, VFS, PM, init), logged VL1_CHILD_HOLDS len_ok=1 and VL1_CHILD_EXIT
             holding=1. Its exit (U9-EXIT1) does no general capability revoke: the holder's copy is
             released by the process teardown, PM's reap (NR 31), and nothing reaps an
             unsupervised fork child today. So the holder_exit and after checkpoints must show
             exactly that one retained grant in the exited child's CNode and nothing else (the
             release on reap is hosted: cap_relinquish_tests).
  NOREFUSAL  no CAP_RELINQUISH_REFUSED other than init's probes (cap=0, unresolvable), and no
             release left parked (the last VFS_GRANT_RELEASE parked=0).
  INJECTED   each deliberate failure line the smoke excuses from the AArch64 blocker scan appears
             exactly as often as the witness causes it, and no other: two pre-lock stale-reply
             refusals (reason=reply_cap err=InvalidCapability), one VFS_CLIENT_REPLY_REFUSED, five
             PM_VFS_SPAWN_FAIL all image 14 (four Malformed, one Unsupported), five matching
             VL1_PM_SPAWN_REFUSED_EXPECTED, and the five image-14 PM_ELF_ZC_FAIL lines the strict
             core smokes excuse (one grant_ro_malformed, three grant_ro_status, one
             spawn_from_mo_unsupported) with no other PM_ELF_ZC_FAIL.
Exit status 0 = sealed, 1 = not sealed. --self-test checks the rules on synthetic boots.
"""
import argparse
import re
import sys

KV = re.compile(r"(\w+)=(\S+)")
CHECKPOINTS = [
    "warm", "success_1", "success_2", "success_3",
    "hop1_undelivered", "hop1_rejected", "hop2_undelivered", "hop2_rejected", "nr29_refused",
    "holder_exit", "after",
]
FAILED = ["hop1_undelivered", "hop1_rejected", "hop2_undelivered", "hop2_rejected", "nr29_refused"]
PM_TID = "3"


def kv(line):
    return dict(KV.findall(line))


def user(line):
    """(tid, msg) of a USER_LOG line, else None."""
    m = re.search(r"USER_LOG tid=(\d+) msg=(.*)$", line)
    return (m.group(1), m.group(2)) if m else None


class Boot:
    def __init__(self, lines):
        self.lines = [l.rstrip("\n") for l in lines]
        self.ledgers = []  # (index, fields, {pid: (occupied, slice)})
        cur = None
        for i, l in enumerate(self.lines):
            if "VL1_LEDGER event=" in l:
                cur = (i, kv(l[l.index("VL1_LEDGER "):]), {})
                self.ledgers.append(cur)
            elif "VL1_LEDGER_CNODE " in l and cur is not None:
                f = kv(l[l.index("VL1_LEDGER_CNODE "):])
                cur[2][f.get("pid")] = (int(f.get("occupied", -1)), int(f.get("slice", -1)))
        self.initramfs = self.first_tid(r"CREATE_INITRAMFS_FILE_SLICE_MO_OK tid=(\d+)")
        self.vfs = self.first_user_tid("VFS_GRANT_")
        self.init = self.first_user_tid("VL1_CHECKPOINT")

    def first_tid(self, pat):
        for l in self.lines:
            m = re.search(pat, l)
            if m:
                return m.group(1)
        return None

    def first_user_tid(self, prefix):
        for l in self.lines:
            u = user(l)
            if u and u[1].startswith(prefix):
                return u[0]
        return None

    def checkpoints(self):
        """[(tag, probe, index, ledger)] in log order."""
        out = []
        for i, l in enumerate(self.lines):
            u = user(l)
            if not u or not u[1].startswith("VL1_CHECKPOINT "):
                continue
            f = kv(u[1])
            led = None
            for li, lf, cn in reversed(self.ledgers):
                if li < i and lf.get("event") == "nr22_refused" and lf.get("tid") == u[0]:
                    led = (li, lf, cn)
                    break
            out.append((f.get("tag"), f.get("probe"), i, led))
        return out

    def users(self, start, end, prefix):
        out = []
        for i in range(start, end):
            u = user(self.lines[i])
            if u and u[1].startswith(prefix):
                out.append((i, u[0], kv(u[1])))
        return out

    def kernel(self, start, end, prefix):
        out = []
        for i in range(start, end):
            l = self.lines[i]
            if l.startswith(prefix) or (" " + prefix) in l:
                out.append((i, kv(l[l.index(prefix):])))
        return out


def grade(lines):
    b = Boot(lines)
    fails = []
    facts = {}

    def fail(rule, why):
        fails.append(f"{rule}: {why}")

    # ── DRIVER ──
    begins = b.users(0, len(b.lines), "VL1_BEGIN")
    done = b.users(0, len(b.lines), "VL1_WITNESS_DONE")
    if len(begins) != 1 or len(done) != 1 or done[0][2].get("driver_ok") != "1":
        fail("DRIVER", f"begin={len(begins)} done={[d[2] for d in done]}")
    cps = b.checkpoints()
    tags = [c[0] for c in cps]
    if tags != CHECKPOINTS:
        fail("DRIVER", f"checkpoints {tags}")
    for tag, probe, _, led in cps:
        if probe != "refused_invalid" or led is None:
            fail("DRIVER", f"checkpoint {tag} probe={probe} ledger={'yes' if led else 'none'}")
    if any("BOOT_FATAL" in l for l in b.lines):
        fail("DRIVER", "BOOT_FATAL")
    if any(lf.get("truncated") != "0" for _, lf, _ in b.ledgers):
        fail("DRIVER", "a ledger was truncated")
    if not (b.initramfs and b.vfs and b.init):
        fail("DRIVER", f"holders not identified initramfs={b.initramfs} vfs={b.vfs} init={b.init}")
    cp = {c[0]: c for c in cps}
    facts["holders"] = f"initramfs={b.initramfs} vfs={b.vfs} pm={PM_TID} init={b.init}"

    def counts(led):
        _, lf, cn = led
        return (int(lf["live_slice_mo"]), int(lf["slice_cap_refs"]), int(lf["slice_slots"]),
                sorted((p, s) for p, (_, s) in cn.items() if s != 0))

    # ── BASELINE ──
    warm = cp.get("warm")
    if warm and warm[3]:
        w = counts(warm[3])
        facts["warm"] = (f"live_slice_mo={w[0]} slice_cap_refs={w[1]} slice_slots={w[2]} "
                         f"links={warm[3][1]['links']}")
        occ = {p: o for p, (o, _) in warm[3][2].items()}
        facts["warm_occupied"] = (f"initramfs={occ.get(b.initramfs)} vfs={occ.get(b.vfs)} "
                                  f"pm={occ.get(PM_TID)}")
        if w[:3] != (0, 0, 0) or w[3]:
            fail("BASELINE", f"warm holds grant resources {w}")
    else:
        fail("BASELINE", "no warm checkpoint ledger")

    # The holder's pid (for RECLAIM's one allowed difference): the task that logged VL1_CHILD_HOLDS.
    holds_all = b.users(0, len(b.lines), "VL1_CHILD_HOLDS")
    if holds_all:
        facts["holder_pid"] = holds_all[0][1]

    # ── RECLAIM ──
    if warm and warm[3]:
        wocc = {p: o for p, (o, _) in warm[3][2].items()}
        prev_links = None
        for tag in CHECKPOINTS[1:]:
            c = cp.get(tag)
            if not c or not c[3]:
                continue
            got = counts(c[3])
            occ = {p: o for p, (o, _) in c[3][2].items()}
            links = int(c[3][1]["links"])
            facts[f"cp_{tag}"] = (f"live_slice_mo={got[0]} slice_cap_refs={got[1]} "
                                  f"slice_slots={got[2]} links={links} pm_occupied={occ.get(PM_TID)}")
            zombie = facts.get("holder_pid")
            if tag in ("holder_exit", "after") and got == (1, 1, 1, [(zombie, 1)]):
                facts[f"retained_{tag}"] = f"one grant in exited holder pid={zombie} (released at reap)"
            elif got[:3] != (0, 0, 0) or got[3]:
                fail("RECLAIM", f"{tag} not at baseline: {got}")
            for who, pid in (("initramfs", b.initramfs), ("vfs", b.vfs)):
                if occ.get(pid) != wocc.get(pid):
                    fail("RECLAIM", f"{tag} {who} occupied {occ.get(pid)} != warm {wocc.get(pid)}")
            if tag in FAILED and prev_links is not None and links != prev_links:
                fail("RECLAIM", f"{tag} links {links} != previous {prev_links}")
            prev_links = links

    # ── SURVIVAL ──
    uses = b.kernel(0, len(b.lines), "VL1_GRANT_USE ")
    relinq = b.kernel(0, len(b.lines), "CAP_RELINQUISH_OK ")
    success_uses = 0
    for ui, uf in uses:
        mo = uf.get("mo_id")
        if uf.get("image_id") == "14":
            continue
        success_uses += 1
        before = [f for i, f in relinq if i < ui and f.get("mo_id") == mo]
        after = [f for i, f in relinq if i > ui and f.get("mo_id") == mo]
        src = [f for f in before if f.get("tid") == b.initramfs and f.get("reparented") == "1"]
        rel = [f for f in before if f.get("tid") == b.vfs and f.get("reparented") == "1"]
        pm = [f for f in after if f.get("tid") == PM_TID and f.get("reclaimed") == "1"]
        if uf.get("cap_refcount") != "1" or len(src) != 1 or len(rel) != 1 or len(pm) < 1:
            fail("SURVIVAL", f"image {uf.get('image_id')} mo {mo} refs={uf.get('cap_refcount')} "
                             f"initramfs_released={len(src)} vfs_released={len(rel)} pm_reclaimed={len(pm)}")
    facts["survival_uses"] = success_uses
    cycles = {f.get("label"): f for _, _, f in b.users(0, len(b.lines), "VL1_CYCLE")}
    for tag in ("success_1", "success_2", "success_3", "after"):
        if cycles.get(tag, {}).get("spawned") != "1":
            fail("SURVIVAL", f"{tag} did not spawn")
    if success_uses < 4 + 4:  # the boot's own grant launches plus the witness's four
        fail("SURVIVAL", f"only {success_uses} successful grant uses")

    # ── FAILPATHS ──
    def segment(tag):
        i = CHECKPOINTS.index(tag)
        a = cp.get(CHECKPOINTS[i - 1])
        z = cp.get(tag)
        return (a[2], z[2]) if a and z else None

    for tag in FAILED:
        seg = segment(tag)
        if not seg:
            fail("FAILPATHS", f"{tag} has no segment")
            continue
        s, e = seg
        if cycles.get(tag, {}).get("spawned") != "0":
            fail("FAILPATHS", f"{tag} spawned")
        ir = b.users(s, e, "INITRAMFS_GRANT_")
        vr = b.users(s, e, "VFS_GRANT_")
        pr = b.users(s, e, "PM_GRANT_RELEASE")
        ok = False
        if tag == "hop1_undelivered":
            ok = any(f.get("why") == "undelivered" and f.get("inject") == "1"
                     and "reclaimed: true" in b.lines[i] for i, _, f in ir)
        elif tag == "hop1_rejected":
            ok = (any("INITRAMFS_GRANT_HELD" in b.lines[i] and f.get("inject") == "2" for i, _, f in ir)
                  and any(f.get("why", "").startswith("rejected_") for _, _, f in vr)
                  and any(f.get("why") == "closed" for i, _, f in ir if "INITRAMFS_GRANT_RELEASE" in b.lines[i]))
        elif tag == "hop2_undelivered":
            ok = (any("kind=stale_reply" in b.lines[i] and f.get("delivered") == "0" for i, _, f in vr)
                  and any(f.get("why") == "reply_refused" for _, _, f in vr))
        elif tag == "hop2_rejected":
            ok = any(f.get("why") == "rejected_completion" for _, _, f in pr)
        elif tag == "nr29_refused":
            us = [f for i, f in uses if s < i < e and f.get("image_id") == "14"]
            ok = (len(us) == 1 and us[0].get("cap_refcount") == "1"
                  and any(f.get("why") == "spawn_refused" for _, _, f in pr)
                  and b.users(s, e, "VL1_PM_SPAWN_REFUSED_EXPECTED"))
        if not ok:
            fail("FAILPATHS", f"{tag}: its cleanup is not in the log")
    facts["failpaths_live"] = ",".join(FAILED)

    # ── HOLDER ──
    seg = segment("holder_exit")
    if seg:
        s, e = seg
        holds = b.users(s, e, "VL1_CHILD_HOLDS")
        exits = b.users(s, e, "VL1_CHILD_EXIT")
        alone = False
        for li, lf, cn in b.ledgers:
            if s < li < e and lf.get("live_slice_mo") == "1":
                owners = [p for p, (_, sl) in cn.items() if sl]
                if len(owners) == 1 and owners[0] not in (b.initramfs, b.vfs, PM_TID, b.init) \
                        and owners[0] == facts.get("holder_pid"):
                    alone = True
        if not (holds and holds[0][2].get("len_ok") == "1" and exits
                and exits[0][2].get("holding") == "1" and alone):
            fail("HOLDER", f"holds={len(holds)} exits={[x[2] for x in exits]} held_alone={alone}")
    else:
        fail("HOLDER", "no holder segment")

    # ── NOREFUSAL ──
    for l in b.lines:
        if "CAP_RELINQUISH_REFUSED " in l:
            f = kv(l[l.index("CAP_RELINQUISH_REFUSED "):])
            if not (f.get("cap") == "0" and f.get("reason") == "unresolvable" and f.get("tid") == b.init):
                fail("NOREFUSAL", l.strip()[:160])
    # ── INJECTED ──
    stale = [l for l in b.lines if "IPCREPLY_DIRECT_REFUSED_PRE_LOCK " in l
             and "reason=reply_cap err=InvalidCapability" in l]
    refused = [l for l in b.lines if re.search(r"msg=VFS_CLIENT_REPLY_REFUSED reply_cap=\d+ err=InvalidCapability$", l)]
    pmfail = [kv(u[1]) for u in (user(l) for l in b.lines) if u and u[1].startswith("PM_VFS_SPAWN_FAIL ")]
    pmexp = [kv(u[1]) for u in (user(l) for l in b.lines) if u and u[1].startswith("VL1_PM_SPAWN_REFUSED_EXPECTED ")]
    errs = sorted(f.get("err") for f in pmfail)
    if (len(stale), len(refused)) != (2, 1):
        fail("INJECTED", f"stale-reply refusals {len(stale)} (want 2), VFS refused replies {len(refused)} (want 1)")
    if any(f.get("image_id") != "14" for f in pmfail + pmexp) or errs != ["Malformed"] * 4 + ["Unsupported"] \
            or sorted(f.get("err") for f in pmexp) != errs:
        fail("INJECTED", f"PM refusals {[(f.get('image_id'), f.get('err')) for f in pmfail]} / expected "
                         f"{[(f.get('image_id'), f.get('err')) for f in pmexp]}")
    zc = [u[1] for u in (user(l) for l in b.lines) if u and u[1].startswith("PM_ELF_ZC_FAIL ")]
    zc_ok = re.compile(r"PM_ELF_ZC_FAIL image_id=14 reason=(grant_ro_malformed status=0 has_cap=1|"
                       r"grant_ro_status status=(5|10) has_cap=0|spawn_from_mo_unsupported)$")
    zc_reasons = sorted(kv(z).get("reason") for z in zc)
    if not all(zc_ok.match(z.strip()) for z in zc) or zc_reasons != (
            ["grant_ro_malformed"] + ["grant_ro_status"] * 3 + ["spawn_from_mo_unsupported"]):
        fail("INJECTED", f"PM_ELF_ZC_FAIL lines {[z.strip()[:90] for z in zc]} (want the five image-14 handoffs)")
    facts["injected"] = (f"stale_reply_refusals={len(stale)} vfs_refused={len(refused)} "
                         f"pm_image14={len(pmfail)} zc_fail_image14={len(zc)}")

    vrel = [f for _, _, f in b.users(0, len(b.lines), "VFS_GRANT_RELEASE ") if "parked" in f]
    if vrel and vrel[-1].get("parked") != "0":
        fail("NOREFUSAL", f"VFS ended with parked={vrel[-1].get('parked')}")
    return fails, facts


# ── self-test ────────────────────────────────────────────────────────────────────────────────

def synthetic(keep_relay=False, revoke=False):
    I, V, P, N = "10000", "10002", "3", "1"
    out = []
    mo = [10]
    links = [21]

    def ledger(event, tid, mid, live, refs, cn, links_now=None):
        ln = links[0] if links_now is None else links_now
        out.append(f"VL1_LEDGER event={event} tid={tid} mo_id={mid} live_slice_mo={live} "
                   f"slice_cap_refs={refs} slice_slots={sum(s for _, s in cn.values())} links={ln} "
                   f"cnodes={len(cn)} truncated=0")
        for p, (o, s) in cn.items():
            out.append(f"VL1_LEDGER_CNODE event={event} pid={p} occupied={o} slice={s}")

    base = {I: (3, 0), V: (6, 0), P: (21, 0), N: (14, 0)}
    leak = [0]
    zombie = [False]

    def cn(extra=None):
        d = dict(base)
        if leak[0]:
            d[V] = (6 + leak[0], leak[0])
        for k, v in (extra or {}).items():
            d[k] = v
        return d

    def u(tid, msg):
        out.append(f"USER_LOG tid={tid} msg={msg}")

    def grant_ok(image, label=None, keep=keep_relay):
        m = mo[0]
        mo[0] += 1
        out.append(f"CREATE_INITRAMFS_FILE_SLICE_MO_OK tid={I} name=x file_len=1 mo_id={m} cap=1")
        u(I, f"INITRAMFS_GRANT_HELD cap=1 handle=1 held=1 inject=0")
        if keep:
            u(V, "VFS_GRANT_KEEP_NC cap=2")
            leak[0] += 1
        else:
            u(V, "VFS_GRANT_HELD cap=2 instance=1 backend_handle=1 held=1")
        out.append(f"CAP_RELINQUISH_OK tid={I} pid={I} cap=1 mo_id={m} reparented=1 parent_pid=0 parent_cap=0 reclaimed=0")
        if not keep:
            out.append(f"CAP_RELINQUISH_OK tid={V} pid={V} cap=2 mo_id={m} reparented=1 parent_pid=0 parent_cap=0 reclaimed=0")
            u(V, "VFS_GRANT_RELEASE cap=2 why=closed step=Released parked=0")
        if revoke:
            out.append(f"VL1_NC_REVOKE tid={I} cap=1 result=Ok(())")
            u(P, f"PM_ELF_ZC_FAIL image_id={image} reason=spawn_from_mo_unsupported")
            out.append(f"BOOT_FATAL_ZC_ELF_LOAD_FAILED image_id={image}")
            return False
        out.append(f"VL1_GRANT_USE tid={P} image_id={image} cap=3 mo_id={m} "
                   f"cap_refcount={2 if keep else 1} map_refcount=0")
        out.append(f"CAP_RELINQUISH_OK tid={P} pid={P} cap=3 mo_id={m} reparented=0 parent_pid=0 "
                   f"parent_cap=0 reclaimed={0 if keep else 1}")
        u(P, f"PM_GRANT_RELEASE image_id={image} cap=3 why=spawned result=Ok")
        return True

    def checkpoint(tag):
        live = leak[0] + (1 if zombie[0] else 0)
        ledger("nr22_refused", N, 0, live, live, cn({"20000": (5, 1)} if zombie[0] else None))
        out.append(f"CAP_RELINQUISH_REFUSED tid={N} cap=0 reason=unresolvable")
        u(N, f"VL1_CHECKPOINT tag={tag} probe=refused_invalid")

    for img in (7, 8, 9, 11):
        grant_ok(img)
    u(N, "VL1_BEGIN success_cycles=3 fixture_len=300")
    checkpoint("warm")
    for t in ("success_1", "success_2", "success_3"):
        ok = grant_ok(11)
        u(N, f"VL1_CYCLE label={t} image_id=11 spawned={int(ok)} tid=10007")
        checkpoint(t)
    # failed handoffs
    out.append("IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=18446744073709551615 record_generation=18446744073709551615 replier_tid=10000 reason=reply_cap err=InvalidCapability reply_copies=0 caller_wakes=0")
    u(I, "INITRAMFS_GRANT_RELEASE cap=9 why=undelivered step=Released { reclaimed: true, reparented: 0 } inject=1")
    for _ in range(4):
        u(P, "PM_VFS_SPAWN_FAIL image_id=14 err=Malformed")
        u(P, "VL1_PM_SPAWN_REFUSED_EXPECTED image_id=14 err=Malformed")
    u(P, "PM_VFS_SPAWN_FAIL image_id=14 err=Unsupported")
    u(P, "PM_ELF_ZC_FAIL image_id=14 reason=grant_ro_status status=5 has_cap=0")
    u(N, "VL1_CYCLE label=hop1_undelivered image_id=14 spawned=0 tid=0")
    checkpoint("hop1_undelivered")
    u(I, "INITRAMFS_GRANT_HELD cap=9 handle=2 held=1 inject=2")
    u(V, "VFS_GRANT_RELEASE cap=8 why=rejected_malformed result=Ok(true)")
    u(I, "INITRAMFS_GRANT_RELEASE cap=9 why=closed handle=2 step=Released")
    u(P, "PM_ELF_ZC_FAIL image_id=14 reason=grant_ro_status status=10 has_cap=0")
    u(N, "VL1_CYCLE label=hop1_rejected image_id=14 spawned=0 tid=0")
    checkpoint("hop1_rejected")
    out.append("IPCREPLY_DIRECT_REFUSED_PRE_LOCK record_index=18446744073709551615 record_generation=18446744073709551615 replier_tid=10002 reason=reply_cap err=InvalidCapability reply_copies=0 caller_wakes=0")
    u(V, "VFS_CLIENT_REPLY_REFUSED reply_cap=5 err=InvalidCapability")
    u(V, "VFS_GRANT_INJECT kind=stale_reply delivered=0 then_backend_error=1")
    u(V, "VFS_GRANT_RELEASE cap=8 why=reply_refused step=Released parked=0")
    u(P, "PM_ELF_ZC_FAIL image_id=14 reason=grant_ro_status status=5 has_cap=0")
    u(N, "VL1_CYCLE label=hop2_undelivered image_id=14 spawned=0 tid=0")
    checkpoint("hop2_undelivered")
    u(P, "PM_ELF_ZC_FAIL image_id=14 reason=grant_ro_malformed status=0 has_cap=1")
    u(P, "PM_GRANT_RELEASE image_id=14 cap=7 why=rejected_completion result=Ok")
    u(N, "VL1_CYCLE label=hop2_rejected image_id=14 spawned=0 tid=0")
    checkpoint("hop2_rejected")
    out.append(f"VL1_GRANT_USE tid={P} image_id=14 cap=7 mo_id=99 cap_refcount=1 map_refcount=0")
    u(P, "PM_ELF_ZC_FAIL image_id=14 reason=spawn_from_mo_unsupported")
    u(P, "PM_GRANT_RELEASE image_id=14 cap=7 why=spawn_refused result=Ok")
    u(P, "VL1_PM_SPAWN_REFUSED_EXPECTED image_id=14 err=Unsupported")
    u(N, "VL1_CYCLE label=nr29_refused image_id=14 spawned=0 tid=0")
    checkpoint("nr29_refused")
    ledger("nr22_ok", V, 98, 1 + leak[0], 1 + leak[0], cn({"20000": (5, 1)}))
    u("20000", "VL1_CHILD_HOLDS cap=4 len=300 len_ok=1")
    u("20000", "VL1_CHILD_EXIT holding=1")
    zombie[0] = True
    checkpoint("holder_exit")
    ok = grant_ok(11)
    u(N, f"VL1_CYCLE label=after image_id=11 spawned={int(ok)} tid=10008")
    checkpoint("after")
    u(N, "VL1_WITNESS_DONE driver_ok=1")
    return out


def self_test():
    ok = True
    f, _ = grade(synthetic())
    if f:
        print("[self-test] clean boot failed:", f)
        ok = False
    f, _ = grade(synthetic(keep_relay=True))
    if not any(x.startswith("RECLAIM") for x in f):
        print("[self-test] NC keep-relay did not fail RECLAIM:", f)
        ok = False
    if any(x.startswith("SURVIVAL: image") and "refs=1" in x for x in f):
        print("[self-test] NC keep-relay blamed survival instead of reclaim:", f)
        ok = False
    f, _ = grade(synthetic() + ["USER_LOG tid=3 msg=PM_VFS_SPAWN_FAIL image_id=14 err=Malformed"])
    if not any(x.startswith("INJECTED") for x in f):
        print("[self-test] an extra excused line did not fail INJECTED:", f)
        ok = False
    f, _ = grade(synthetic() + ["USER_LOG tid=3 msg=PM_VFS_SPAWN_FAIL image_id=7 err=Malformed"])
    if not any(x.startswith("INJECTED") for x in f):
        print("[self-test] another image's refusal did not fail INJECTED:", f)
        ok = False
    f, _ = grade(synthetic() + ["USER_LOG tid=3 msg=PM_ELF_ZC_FAIL image_id=7 reason=grant_ro_status status=5 has_cap=0"])
    if not any(x.startswith("INJECTED") for x in f):
        print("[self-test] another image's loader failure did not fail INJECTED:", f)
        ok = False
    f, _ = grade(synthetic(revoke=True))
    if not any(x.startswith("SURVIVAL") for x in f):
        print("[self-test] NC revoke did not fail SURVIVAL:", f)
        ok = False
    print(f"[self-test] {'ok' if ok else 'FAIL'}")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("log", nargs="?")
    ap.add_argument("--arch", default="?")
    ap.add_argument("--smp", default="?")
    ap.add_argument("--core-rc", type=int, default=0)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    with open(a.log, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().replace("\r", "\n").split("\n")
    fails, facts = grade(lines)
    if a.core_rc != 0:
        fails.append(f"CORE: core smoke rc={a.core_rc}")
    for k, v in facts.items():
        print(f"[fact] {k}: {v}")
    for x in fails:
        print(f"[fail] {x}")
    print(f"VFS_LIFETIME_WITNESS_SEAL arch={a.arch} smp={a.smp} "
          f"result={'ok' if not fails else 'fail'} failures={len(fails)}")
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
