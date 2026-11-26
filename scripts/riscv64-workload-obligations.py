#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""RISCV-STRICT1 — what the RISC-V core smoke owes the image it actually booted.

The RISC-V core smoke (scripts/qemu-riscv64-core-smoke.sh) grades every boot with one spawn census,
one required-marker list and one split-dispatch census. Those were written for the ORDINARY core
image. Two VFS witness images do more, or less, by design:

  vfs-fd-witness     init (init/vfs_fd_witness.rs) spawns a SECOND ramfs (one more NR 29), forks
                     once (NR 12) and runs a second thread; the thread and the fork child exit
                     (NR 16 each). Nothing else changes.
  vfs-mount-witness  init (init/vfs_mount_witness.rs) registers /ram ITSELF, through the authorized
                     stage + commit, instead of the ordinary registration, so the ordinary
                     `VFS_MOUNT_REGISTER_RAMFS_OK` is never printed. Nothing else changes.
  vfs-lifetime-witness
                     init (init/vfs_lifetime_witness.rs, VFS-LIFETIME1) launches ramfs_srv four more
                     times through PM (four more NR 29, `result=ok`), drives five failed grant
                     handoffs through PM's witness image 14, of which exactly ONE reaches NR 29 and is
                     refused there (`result=handled_err code=2`, InvalidArgs: the fixture is not an
                     ELF), and forks once (NR 12); the fork child exits (NR 16). Nothing else changes.

The workload is never chosen by a caller. It is derived from the boot's verified ARTIFACT_BINDING
line (scripts/artifact-provenance.py `verify`): the features the bound kernel and initramfs were
built with. Any other image, the terminal-fault cell included, is graded exactly as before.

Subcommands (each prints `[fail] …` lines and exits 1 on any failure, 0 otherwise):
  profile  --binding LINE   print `RISCV_WORKLOAD profile=<core|vfs-fd-witness|vfs-mount-witness>`
                            (refuses a failed binding, or a kernel and initramfs that disagree, or
                            both witness workloads in one image).
  fd LOG                    the fd witness's extra obligations, each tied to its own task:
      SPAWN  exactly six SPAWN_FROM_MO_OK, two of them image 11: one is the ordinary ramfs
             (INIT_RAMFS_SPAWN_OK child_tid, VC1C_BEGIN tid_a), the other is the witness's second
             ramfs (VC1C_SECOND_RAMFS tid_b mount_status=0), and VC1C_FD_WITNESS_SEAL names both.
  life LOG                  the lifetime witness's extra obligations, each tied to its own task:
      SPAWN  exactly nine SPAWN_FROM_MO_OK (the core five and the witness's four image-11 launches,
             each a VL1_CYCLE spawned=1 naming that tid); exactly nine NR 29 `result=ok` and ONE
             `result=handled_err code=2`, which follows the only VL1_GRANT_USE image_id=14 and
             precedes the next of PM's five VL1_PM_SPAWN_REFUSED_EXPECTED (one per failed
             handoff); init's VL1_WITNESS_DONE driver_ok=1.
      FORK / EXIT  as the fd witness's, for the one fork child (VL1_FORK_PARENT_WAKE child=C,
             which logged VL1_CHILD_EXIT): one NR 12 `result=ok` finalized once, one
             FORK_TXN_COMMITTED of C by init, one NR 16 for C with its exit records.
      FORK   exactly one NR 12 dispatch, `result=ok`, finalized once; exactly one
             FORK_TXN_COMMITTED, by init (VC1C_BEGIN's tid) of the witness's fork child
             (VC1C_FORK_PARENT child=C, which logged VC1C_CHILD_EXIT).
      EXIT   exactly two NR 16 dispatches, each `result=queue_advance_committed outgoing=T
             captured=0 committed=0` (an exiting task is never captured: U9-EXIT1 §3), one for the
             witness's second thread (VC1C_THREAD tid=T, which logged VC1C_THREAD_EXIT) and one for
             its fork child; for each T exactly one EXIT_TASK_SYSCALL_DISPATCHED nr=16 tid=T
             result=ok, one EXIT_TASK_LIFECYCLE_TRANSITION tid=T syscall_returns=0 result=ok, one
             EXIT_TASK_CLAIM_RETIRED tid=T code=0, and one QUEUE_ADVANCING_DISPATCH_DEFERRED
             reason=exit_current_task_switch_required tid=T on the dispatch's own CPU.
  mount LOG                 the mount witness's positive registration evidence, graded by the
                            UNMODIFIED mount grader (scripts/grade-vfs-mount-witness.py, loaded as
                            a module): its CORE rule (with the core smoke's own rc taken as 0 —
                            the smoke is what is running), IDENTITY (exactly one /ram publication,
                            the VFS-local capability VFS received, never the sender's number, every
                            /ram/ forward on it) and BYTES (the exact 192 bytes).
  --self-test               the rules on synthetic boots.
"""
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
FD, MOUNT, LIFE = "vfs-fd-witness", "vfs-mount-witness", "vfs-lifetime-witness"
CORE_IMAGE_SPAWNS = 5          # the ordinary census: images 7, 8, 9, 11, 12
MOUNT_RULES = ("CORE", "IDENTITY", "BYTES")
DISPATCH = "YARM_LOCK_SPLIT_DISPATCH arch=riscv64 "


def kv(text):
    return dict(re.findall(r"(\w+)=(\S+)", text))


def feats(v):
    return set() if v in (None, "", "none") else set(v.split(","))


def profile(binding):
    """(profile, failures) from one ARTIFACT_BINDING line."""
    if not binding or not binding.startswith("ARTIFACT_BINDING "):
        return None, ["no ARTIFACT_BINDING line to derive the workload from"]
    f = kv(binding)
    if f.get("arch") != "riscv64" or f.get("result") != "ok":
        return None, [f"the binding is not a verified riscv64 boot set ({f.get('arch')}, {f.get('result')})"]
    k, i = feats(f.get("kernel_features")), feats(f.get("initramfs_features"))
    for w in (FD, MOUNT, LIFE):
        if (w in k) != (w in i):
            return None, [f"the bound kernel and initramfs disagree on {w} "
                          f"(kernel={f.get('kernel_features')} initramfs={f.get('initramfs_features')})"]
    carried = [w for w in (FD, MOUNT, LIFE) if w in i]
    if len(carried) > 1:
        return None, [f"one image carries {' and '.join(carried)}; no one workload's obligations apply"]
    return (carried[0] if carried else "core"), []


def load(path):
    with open(path, "r", errors="replace") as fh:
        text = fh.read()
    # Count complete records only (a boot ended by its timeout can cut the last one).
    lines = text.replace("\r", "\n").split("\n")
    if text and not text.endswith(("\n", "\r")):
        lines = lines[:-1]
    return [x for x in lines if x]


def user(lines, tag):
    """[(tid, fields)] for `USER_LOG tid=T msg=TAG …` lines."""
    out = []
    for x in lines:
        m = re.search(r"USER_LOG tid=(\d+) msg=" + tag + r"(?: (.*))?$", x)
        if m:
            out.append((m.group(1), kv(m.group(2) or "")))
    return out


def kernel(lines, prefix):
    return [kv(x[x.index(prefix):]) for x in lines if prefix in x and "USER_LOG" not in x]


def one(seq, what, fails):
    if len(seq) != 1:
        fails.append(f"{what}: {len(seq)} records, expected exactly 1")
        return None
    return seq[0]


def fd(lines):
    fails, facts = [], {}
    begin = one(user(lines, "VC1C_BEGIN"), "VC1C_BEGIN", fails)
    second = one(user(lines, "VC1C_SECOND_RAMFS"), "VC1C_SECOND_RAMFS", fails)
    seal = one(user(lines, "VC1C_FD_WITNESS_SEAL"), "VC1C_FD_WITNESS_SEAL", fails)
    thread = one(user(lines, "VC1C_THREAD"), "VC1C_THREAD", fails)
    tex = one(user(lines, "VC1C_THREAD_EXIT"), "VC1C_THREAD_EXIT", fails)
    parent = one(user(lines, "VC1C_FORK_PARENT"), "VC1C_FORK_PARENT", fails)
    cex = one(user(lines, "VC1C_CHILD_EXIT"), "VC1C_CHILD_EXIT", fails)
    if None in (begin, second, seal, thread, tex, parent, cex):
        return [f"WITNESS: {x}" for x in fails], facts
    init = begin[0]
    tid_a, tid_b = begin[1].get("tid_a"), second[1].get("tid_b")
    t, c = thread[1].get("tid"), parent[1].get("child")
    facts.update(init=init, ramfs_a=tid_a, ramfs_b=tid_b, thread=t, fork_child=c)
    for who, rec in (("VC1C_SECOND_RAMFS", second), ("VC1C_THREAD", thread),
                     ("VC1C_FORK_PARENT", parent), ("VC1C_FD_WITNESS_SEAL", seal)):
        if rec[0] != init:
            fails.append(f"WITNESS: {who} logged by tid {rec[0]}, not init {init}")
    if second[1].get("mount_status") != "0":
        fails.append(f"WITNESS: the second ramfs mount status is {second[1].get('mount_status')}")
    if (seal[1].get("result"), seal[1].get("tid_a"), seal[1].get("tid_b")) != ("ok", tid_a, tid_b):
        fails.append(f"WITNESS: the seal {seal[1]} does not name ramfs {tid_a} and {tid_b} with result=ok")
    if tex[0] != t:
        fails.append(f"WITNESS: VC1C_THREAD_EXIT logged by tid {tex[0]}, not the thread {t}")
    if cex[0] != c:
        fails.append(f"WITNESS: VC1C_CHILD_EXIT logged by tid {cex[0]}, not the fork child {c}")
    if len({init, tid_a, tid_b, t, c}) != 5:
        fails.append(f"WITNESS: the witness tasks are not distinct ({init}, {tid_a}, {tid_b}, {t}, {c})")

    # SPAWN
    oks = kernel(lines, "SPAWN_FROM_MO_OK ")
    ram = sorted(o.get("spawned_tid") for o in oks if o.get("image_id") == "11")
    if len(oks) != CORE_IMAGE_SPAWNS + 1:
        fails.append(f"SPAWN: {len(oks)} successful NR 29 spawns, expected {CORE_IMAGE_SPAWNS + 1} "
                     f"(the core image's {CORE_IMAGE_SPAWNS} and the second ramfs)")
    if ram != sorted([tid_a, tid_b]):
        fails.append(f"SPAWN: image-11 spawns {ram} are not the ordinary ramfs {tid_a} and the second ramfs {tid_b}")
    init_ram = [f.get("child_tid") for _, f in user(lines, "INIT_RAMFS_SPAWN_OK")]
    if init_ram != [tid_a]:
        fails.append(f"SPAWN: INIT_RAMFS_SPAWN_OK {init_ram} is not the ordinary ramfs {tid_a}")

    # FORK
    d12 = [x for x in lines if x.startswith(DISPATCH + "nr=12 ")]
    if len(d12) != 1 or not re.fullmatch(DISPATCH + r"nr=12 cpu=\d+ result=ok", d12[0].strip() if d12 else ""):
        fails.append(f"FORK: NR 12 dispatches {d12}, expected exactly one `result=ok`")
    fin = [x for x in lines if x.strip() == "RISCV_SPLIT_FINALIZE_OK nr=12 result=ok"]
    if len(fin) != 1:
        fails.append(f"FORK: {len(fin)} NR 12 finalizations, expected 1")
    txn = kernel(lines, "FORK_TXN_COMMITTED ")
    if [(x.get("parent_tid"), x.get("child_tid")) for x in txn] != [(init, c)]:
        fails.append(f"FORK: committed forks {[(x.get('parent_tid'), x.get('child_tid')) for x in txn]}, "
                     f"expected exactly init {init} forking the witness child {c}")

    # EXIT
    d16 = [x for x in lines if x.startswith(DISPATCH + "nr=16 ")]
    outs = []
    for x in d16:
        m = re.fullmatch(DISPATCH + r"nr=16 cpu=(\d+) result=queue_advance_committed outgoing=(\d+) "
                         r"captured=0 committed=0 a0=\d+", x.strip())
        if not m:
            fails.append(f"EXIT: NR 16 dispatch with another disposition: {x.strip()}")
            continue
        outs.append((m.group(2), m.group(1)))
    if len(d16) != 2 or sorted(o for o, _ in outs) != sorted([t, c]):
        fails.append(f"EXIT: NR 16 dispatches for {[o for o, _ in outs]} ({len(d16)} lines), expected "
                     f"exactly the thread {t} and the fork child {c}")
    for tid, cpu in outs:
        if tid not in (t, c):
            continue
        sd = [x for x in kernel(lines, "EXIT_TASK_SYSCALL_DISPATCHED ") if x.get("tid") == tid]
        lt = [x for x in kernel(lines, "EXIT_TASK_LIFECYCLE_TRANSITION ") if x.get("tid") == tid]
        cr = [x for x in kernel(lines, "EXIT_TASK_CLAIM_RETIRED ") if x.get("tid") == tid]
        df = [x for x in kernel(lines, "QUEUE_ADVANCING_DISPATCH_DEFERRED ")
              if x.get("tid") == tid and x.get("reason") == "exit_current_task_switch_required"]
        if len(sd) != 1 or (sd[0].get("nr"), sd[0].get("result")) != ("16", "ok"):
            fails.append(f"EXIT: tid {tid}: {len(sd)} EXIT_TASK_SYSCALL_DISPATCHED nr=16 result=ok")
        if len(lt) != 1 or (lt[0].get("syscall_returns"), lt[0].get("result")) != ("0", "ok"):
            fails.append(f"EXIT: tid {tid}: lifecycle transition {lt}")
        if len(cr) != 1 or cr[0].get("code") != "0":
            fails.append(f"EXIT: tid {tid}: exit claim {cr}, expected one with code=0")
        if len(df) != 1 or df[0].get("cpu") != cpu:
            fails.append(f"EXIT: tid {tid}: the exit switch was deferred on {[x.get('cpu') for x in df]}, "
                         f"not once on the dispatch's CPU {cpu}")
    facts.update(nr12=len(d12), nr16=len(d16), nr29_ok=len(oks))
    return fails, facts


def fork_exit(lines, init, c, fails):
    """The one fork child C of init: its NR 12 and its NR 16, each with their own records."""
    d12 = [x for x in lines if x.startswith(DISPATCH + "nr=12 ")]
    if len(d12) != 1 or not re.fullmatch(DISPATCH + r"nr=12 cpu=\d+ result=ok", d12[0].strip() if d12 else ""):
        fails.append(f"FORK: NR 12 dispatches {d12}, expected exactly one `result=ok`")
    fin = [x for x in lines if x.strip() == "RISCV_SPLIT_FINALIZE_OK nr=12 result=ok"]
    if len(fin) != 1:
        fails.append(f"FORK: {len(fin)} NR 12 finalizations, expected 1")
    txn = kernel(lines, "FORK_TXN_COMMITTED ")
    if [(x.get("parent_tid"), x.get("child_tid")) for x in txn] != [(init, c)]:
        fails.append(f"FORK: committed forks {[(x.get('parent_tid'), x.get('child_tid')) for x in txn]}, "
                     f"expected exactly init {init} forking the witness child {c}")
    d16 = [x for x in lines if x.startswith(DISPATCH + "nr=16 ")]
    outs = []
    for x in d16:
        m = re.fullmatch(DISPATCH + r"nr=16 cpu=(\d+) result=queue_advance_committed outgoing=(\d+) "
                         r"captured=0 committed=0 a0=\d+", x.strip())
        if not m:
            fails.append(f"EXIT: NR 16 dispatch with another disposition: {x.strip()}")
            continue
        outs.append((m.group(2), m.group(1)))
    if len(d16) != 1 or [o for o, _ in outs] != [c]:
        fails.append(f"EXIT: NR 16 dispatches for {[o for o, _ in outs]} ({len(d16)} lines), expected "
                     f"exactly the fork child {c}")
    for tid, cpu in outs:
        if tid != c:
            continue
        sd = [x for x in kernel(lines, "EXIT_TASK_SYSCALL_DISPATCHED ") if x.get("tid") == tid]
        lt = [x for x in kernel(lines, "EXIT_TASK_LIFECYCLE_TRANSITION ") if x.get("tid") == tid]
        cr = [x for x in kernel(lines, "EXIT_TASK_CLAIM_RETIRED ") if x.get("tid") == tid]
        df = [x for x in kernel(lines, "QUEUE_ADVANCING_DISPATCH_DEFERRED ")
              if x.get("tid") == tid and x.get("reason") == "exit_current_task_switch_required"]
        if len(sd) != 1 or (sd[0].get("nr"), sd[0].get("result")) != ("16", "ok"):
            fails.append(f"EXIT: tid {tid}: {len(sd)} EXIT_TASK_SYSCALL_DISPATCHED nr=16 result=ok")
        if len(lt) != 1 or (lt[0].get("syscall_returns"), lt[0].get("result")) != ("0", "ok"):
            fails.append(f"EXIT: tid {tid}: lifecycle transition {lt}")
        if len(cr) != 1 or cr[0].get("code") != "0":
            fails.append(f"EXIT: tid {tid}: exit claim {cr}, expected one with code=0")
        if len(df) != 1 or df[0].get("cpu") != cpu:
            fails.append(f"EXIT: tid {tid}: the exit switch was deferred on {[x.get('cpu') for x in df]}, "
                         f"not once on the dispatch's CPU {cpu}")
    return len(d12), len(d16)


LIFE_LAUNCHES = 4          # success_1..3 and after: image 11 each
LIFE_FAILED = 5            # the failed handoffs: PM refuses image 14 once each
NR29_REFUSED = "2"         # InvalidArgs: the fixture is not an ELF


def life(lines):
    fails, facts = [], {}
    begin = one(user(lines, "VL1_BEGIN"), "VL1_BEGIN", fails)
    done = one(user(lines, "VL1_WITNESS_DONE"), "VL1_WITNESS_DONE", fails)
    wake = one(user(lines, "VL1_FORK_PARENT_WAKE"), "VL1_FORK_PARENT_WAKE", fails)
    cex = one(user(lines, "VL1_CHILD_EXIT"), "VL1_CHILD_EXIT", fails)
    if None in (begin, done, wake, cex):
        return [f"WITNESS: {x}" for x in fails], facts
    init, c = begin[0], wake[1].get("child")
    facts.update(init=init, fork_child=c)
    if done[0] != init or done[1].get("driver_ok") != "1":
        fails.append(f"WITNESS: VL1_WITNESS_DONE {done} is not init's driver_ok=1")
    if wake[0] != init or cex[0] != c:
        fails.append(f"WITNESS: the fork parent {wake[0]} / child exit {cex[0]} are not init {init} / child {c}")
    # SPAWN
    oks = kernel(lines, "SPAWN_FROM_MO_OK ")
    cycles = [f for t, f in user(lines, "VL1_CYCLE") if t == init and f.get("spawned") == "1"]
    launched = sorted(f.get("tid") for f in cycles)
    witness_ok = sorted(o.get("spawned_tid") for o in oks
                        if o.get("image_id") == "11" and o.get("spawned_tid") in launched)
    if len(oks) != CORE_IMAGE_SPAWNS + LIFE_LAUNCHES:
        fails.append(f"SPAWN: {len(oks)} successful NR 29 spawns, expected "
                     f"{CORE_IMAGE_SPAWNS + LIFE_LAUNCHES} (the core image's {CORE_IMAGE_SPAWNS} and the witness's "
                     f"{LIFE_LAUNCHES} launches)")
    if len(launched) != LIFE_LAUNCHES or witness_ok != launched:
        fails.append(f"SPAWN: the witness's launches {launched} are not each an image-11 NR 29 spawn ({witness_ok})")
    d29 = [x.strip() for x in lines if x.startswith(DISPATCH + "nr=29 ")]
    ok29 = [x for x in d29 if re.fullmatch(DISPATCH + r"nr=29 cpu=\d+ result=ok", x)]
    err29 = [i for i, x in enumerate(lines) if x.startswith(DISPATCH + "nr=29 ") and "result=handled_err" in x]
    if len(ok29) != CORE_IMAGE_SPAWNS + LIFE_LAUNCHES or len(err29) != 1 or len(d29) != len(ok29) + 1:
        fails.append(f"SPAWN: NR 29 dispatches ok={len(ok29)} handled_err={len(err29)} total={len(d29)}, expected "
                     f"{CORE_IMAGE_SPAWNS + LIFE_LAUNCHES} ok and exactly one refusal")
    else:
        j = err29[0]
        if not lines[j].strip().endswith(f"result=handled_err code={NR29_REFUSED}"):
            fails.append(f"SPAWN: the NR 29 refusal is not InvalidArgs: {lines[j].strip()}")
        use = [i for i, x in enumerate(lines) if "VL1_GRANT_USE " in x and "image_id=14 " in x]
        exp = [i for i, x in enumerate(lines) if "msg=VL1_PM_SPAWN_REFUSED_EXPECTED " in x]
        after = [i for i in exp if i > j]
        if len(exp) != LIFE_FAILED or len(use) != 1 or not after or not (use[0] < j < after[0]):
            fails.append(f"SPAWN: the NR 29 refusal (line {j}) is not PM's refused image-14 use "
                         f"(use {use}; PM's expected refusals {exp}, one per failed handoff "
                         f"expected: {LIFE_FAILED})")
    n12, n16 = fork_exit(lines, init, c, fails)
    facts.update(nr12=n12, nr16=n16, nr29_ok=len(oks), nr29_refused=len(err29))
    return fails, facts


def mount_grader():
    spec = importlib.util.spec_from_file_location(
        "grade_vfs_mount_witness", os.path.join(HERE, "grade-vfs-mount-witness.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def mount(lines):
    # `want_wide` only selects the witness grader's own SENDER / WIDTH rules, which stay that
    # grader's: the core smoke keeps CORE, IDENTITY and BYTES only.
    g = mount_grader()
    fails, facts = g.grade(lines, 0, False)
    kept = [f for f in fails if f.split(":", 1)[0] in MOUNT_RULES]
    if any(x for x in lines if "VFS_MOUNT_REGISTER_RAMFS_OK" in x):
        kept.append("REGISTRATION: the ordinary VFS_MOUNT_REGISTER_RAMFS_OK appears in a mount-witness "
                    "image, whose init registers /ram only through the authorized path")
    return kept, facts


def report(kind, fails, facts):
    for f in fails:
        print(f"[fail] RISC-V {kind} obligation: {f}")
    extra = " ".join(f"{k}={v}" for k, v in sorted(facts.items()) if not isinstance(v, dict))
    print(f"RISCV_WORKLOAD_OBLIGATIONS workload={kind} result={'ok' if not fails else 'fail'} "
          f"fails={len(fails)} {extra}".rstrip())
    return 0 if not fails else 1


# ---------------------------------------------------------------------------------------------
# Self-test

def _fd_synthetic():
    D = DISPATCH
    u = lambda tid, m: f"USER_LOG tid={tid} msg={m}"
    L = [f"SPAWN_FROM_MO_OK image_id={i} spawned_tid={t}" for i, t in
         ((7, 10003), (8, 10004), (9, 10005), (11, 10006))]
    L += [u(1, "INIT_RAMFS_SPAWN_OK child_tid=10006 send_cap=4295098384 prefix=/ram"),
          u(1, "VC1C_BEGIN tid_a=10006"),
          "SPAWN_FROM_MO_OK image_id=11 spawned_tid=10007",
          u(1, "VC1C_SECOND_RAMFS tid_b=10007 init_local_backend=4295032849 mount_status=0 reason=reply"),
          u(10008, "VC1C_THREAD_EXIT"),
          "EXIT_TASK_CLAIM_RETIRED tid=10008 asid=1 pid=1 code=0 token=1 caller_records=0 replier_records=0 links=0 joiners=0 server_death=0",
          "EXIT_TASK_SYSCALL_DISPATCHED nr=16 tid=10008 asid=1 target=self result=ok",
          "EXIT_TASK_LIFECYCLE_TRANSITION tid=10008 asid=1 syscall_returns=0 result=ok",
          "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required tid=10008 cpu=1",
          f"{D}nr=16 cpu=1 result=queue_advance_committed outgoing=10008 captured=0 committed=0 a0=0",
          u(1, "VC1C_THREAD tid=10008 fd=3 thread_read=1 main_read_after_exit=1 main_close=0"),
          "FORK_TXN_COMMITTED parent_tid=1 child_tid=10009 parent_asid=1 child_asid=12 cpu=1 inherited_caps=18 wp_runs=2",
          f"{D}nr=12 cpu=0 result=ok",
          "RISCV_SPLIT_FINALIZE_OK nr=12 result=ok",
          u(10009, "VC1C_CHILD_EXIT closed=0"),
          "EXIT_TASK_CLAIM_RETIRED tid=10009 asid=12 pid=10009 code=0 token=2 caller_records=0 replier_records=0 links=0 joiners=0 server_death=0",
          "EXIT_TASK_SYSCALL_DISPATCHED nr=16 tid=10009 asid=12 target=self result=ok",
          "EXIT_TASK_LIFECYCLE_TRANSITION tid=10009 asid=12 syscall_returns=0 result=ok",
          "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required tid=10009 cpu=1",
          f"{D}nr=16 cpu=1 result=queue_advance_committed outgoing=10009 captured=0 committed=0 a0=0",
          u(1, "VC1C_FORK_PARENT child=10009 open_b_before=1 open_b_after_child_exit=1 parent_read_after=1 parent_closed=3"),
          "SPAWN_FROM_MO_OK image_id=12 spawned_tid=10010",
          u(1, "VC1C_FD_WITNESS_SEAL result=ok fails=0 tid_a=10006 tid_b=10007")]
    return L


def _life_synthetic():
    D = DISPATCH
    u = lambda tid, m: f"USER_LOG tid={tid} msg={m}"
    L = []
    for i, t in ((7, 10003), (8, 10004), (9, 10005), (11, 10006), (12, 10007)):
        L += [f"SPAWN_FROM_MO_OK image_id={i} spawned_tid={t}", f"{D}nr=29 cpu=0 result=ok"]
    L.append(u(1, "VL1_BEGIN success_cycles=3 fixture_len=300"))
    for lab, t in (("success_1", 10010), ("success_2", 10011), ("success_3", 10012)):
        L += [f"SPAWN_FROM_MO_OK image_id=11 spawned_tid={t}", f"{D}nr=29 cpu=0 result=ok",
              u(1, f"VL1_CYCLE label={lab} image_id=11 spawned=1 tid={t}")]
    for lab in ("hop1_undelivered", "hop1_rejected", "hop2_undelivered", "hop2_rejected"):
        L += [u(3, "VL1_PM_SPAWN_REFUSED_EXPECTED image_id=14 err=Malformed"),
              u(1, f"VL1_CYCLE label={lab} image_id=14 spawned=0 tid=0")]
    L += ["VL1_GRANT_USE tid=3 image_id=14 cap=5 mo_id=40 cap_refcount=1 map_refcount=0",
          f"{D}nr=29 cpu=0 result=handled_err code=2",
          u(3, "VL1_PM_SPAWN_REFUSED_EXPECTED image_id=14 err=InvalidArgs"),
          u(1, "VL1_CYCLE label=nr29_refused image_id=14 spawned=0 tid=0"),
          "FORK_TXN_COMMITTED parent_tid=1 child_tid=10016 parent_asid=1 child_asid=14 cpu=1 inherited_caps=20 wp_runs=2",
          f"{D}nr=12 cpu=0 result=ok",
          "RISCV_SPLIT_FINALIZE_OK nr=12 result=ok",
          u(10016, "VL1_CHILD_EXIT holding=1"),
          "EXIT_TASK_SYSCALL_DISPATCHED nr=16 tid=10016 asid=14 target=self result=ok",
          "EXIT_TASK_CLAIM_RETIRED tid=10016 asid=14 pid=10016 code=0 token=1 caller_records=0 replier_records=0 links=0 joiners=0 server_death=0",
          "EXIT_TASK_LIFECYCLE_TRANSITION tid=10016 asid=14 syscall_returns=0 result=ok",
          "QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required tid=10016 cpu=1",
          f"{D}nr=16 cpu=1 result=queue_advance_committed outgoing=10016 captured=0 committed=0 a0=0",
          u(1, "VL1_FORK_PARENT_WAKE child=10016 slept=1"),
          "SPAWN_FROM_MO_OK image_id=11 spawned_tid=10017", f"{D}nr=29 cpu=0 result=ok",
          u(1, "VL1_CYCLE label=after image_id=11 spawned=1 tid=10017"),
          u(1, "VL1_WITNESS_DONE driver_ok=1")]
    return L


def self_test():
    n = 0

    def expect(ok, got, why):
        nonlocal n
        assert (not got[0]) == ok, (why, got[0])
        n += 1

    # profile derivation
    B = ("ARTIFACT_BINDING arch=riscv64 kernel=k kernel_sha256=x kernel_features={k} "
         "kernel_resolved_features={k} initramfs=i initramfs_sha256=y initramfs_features={i} "
         "initramfs_compile_env=none source=clean:z required=none result={r}")
    wf = "cap-gen-witness,vfs-correct-witness,vfs-fd-witness"
    wm = "cap-gen-witness,vfs-correct-witness,vfs-mount-witness"
    for k, i, r, want in (("none", "none", "ok", "core"), (wf, wf, "ok", FD), (wm, wm, "ok", MOUNT),
                          ("vfs-correct-witness", "vfs-correct-witness", "ok", "core"),
                          ("vfs-correct-witness,vfs-fd-witness", "vfs-correct-witness,vfs-fd-witness", "ok", FD),
                          ("none", wf, "ok", None), (wf, "none", "ok", None),
                          (wf + ",vfs-mount-witness", wf + ",vfs-mount-witness", "ok", None),
                          (wf, wf, "fail", None),
                          (LIFE, LIFE, "ok", LIFE), ("none", LIFE, "ok", None),
                          (LIFE + ",vfs-fd-witness", LIFE + ",vfs-fd-witness", "ok", None)):
        p, f = profile(B.format(k=k, i=i, r=r))
        assert p == want and bool(f) == (want is None), (k, i, r, p, f)
        n += 1
    assert profile("")[0] is None and profile(B.format(k=wf, i=wf, r="ok").replace("riscv64", "aarch64"))[0] is None
    n += 2

    good = _fd_synthetic()
    expect(True, fd(good), "the fd witness's own obligations")

    def mut(fn):
        x = list(good)
        fn(x)
        return x

    def drop(pat):
        return lambda x: x.remove(next(y for y in x if pat in y))

    def sub(pat, a, b):
        def f(x):
            j = next(i for i, y in enumerate(x) if pat in y)
            x[j] = x[j].replace(a, b)
        return f
    nr16 = f"{DISPATCH}nr=16 cpu=1 result=queue_advance_committed outgoing=10008"
    for why, fn in (
        ("missing second-ramfs spawn", drop("spawned_tid=10007")),
        ("extra spawn", lambda x: x.append("SPAWN_FROM_MO_OK image_id=11 spawned_tid=10011")),
        ("second ramfs is another task", sub("spawned_tid=10007", "10007", "10012")),
        ("unrelated syscall at the same total", sub(nr16, "nr=16", "nr=3")),
        ("fork replaced by another non-switching class", sub(DISPATCH + "nr=12", "nr=12", "nr=13")),
        ("exit of the wrong task", sub(nr16, "outgoing=10008", "outgoing=10005")),
        ("exit captured (would resume a corpse)", sub(nr16, "captured=0", "captured=1")),
        ("exit with another disposition", sub(nr16, "result=queue_advance_committed outgoing=10008 captured=0 committed=0 a0=0", "result=ok")),
        ("fork with a handled error", sub(DISPATCH + "nr=12", "result=ok", "result=handled_err code=3")),
        ("fork by another parent", sub("FORK_TXN_COMMITTED", "parent_tid=1", "parent_tid=10002")),
        ("fork of another child", sub("FORK_TXN_COMMITTED", "child_tid=10009", "child_tid=10011")),
        ("second fork", lambda x: x.append("FORK_TXN_COMMITTED parent_tid=1 child_tid=10011 parent_asid=1 child_asid=13 cpu=0 inherited_caps=18 wp_runs=2")),
        ("exit deferred on another CPU", sub("exit_current_task_switch_required tid=10009", "cpu=1", "cpu=0")),
        ("exit with a non-zero code", sub("EXIT_TASK_CLAIM_RETIRED tid=10008", "code=0", "code=1")),
        ("exit never reached its lifecycle transition", drop("EXIT_TASK_LIFECYCLE_TRANSITION tid=10009")),
        ("duplicate exit dispatch", lambda x: x.append(x[[i for i, y in enumerate(x) if nr16 in y][0]])),
        ("thread exit logged by another task", sub("msg=VC1C_THREAD_EXIT", "tid=10008", "tid=10009")),
        ("second ramfs mount refused", sub("VC1C_SECOND_RAMFS", "mount_status=0", "mount_status=4")),
        ("not an fd-witness boot", lambda x: x.__delitem__(slice(0, len(x)))),
    ):
        expect(False, fd(mut(fn)), why)

    g = mount_grader()
    syn = g.synthetic()
    expect(True, mount(syn), "the mount witness's registration evidence")
    expect(True, mount(g.synthetic(wide_ids=False)), "narrow")
    pub = [y for y in syn if "VFS_MOUNT_PUBLISHED" in y][0]
    for why, lines in (
        ("no /ram publication", [y for y in syn if "VFS_MOUNT_PUBLISHED" not in y]),
        ("doubled publication", syn + [pub]),
        ("the sender's number published", g.synthetic(nc="sender_slot")),
        ("a forward on another cap", [y.replace("send_cap=", "send_cap=9") if "VFS_FORWARD_CALL" in y else y for y in syn]),
        ("a short read", [y for y in syn if "VC1B_DATA k=1" not in y]),
        ("the wrong bytes", [y.replace("hex=0b", "hex=0c") if "VC1B_DATA k=0 off=0 " in y else y for y in syn]),
        ("the witness did not finish", [y for y in syn if "VC1B_END" not in y]),
        ("the ordinary registration in a mount image", syn + ["USER_LOG tid=1 msg=VFS_MOUNT_REGISTER_RAMFS_OK prefix=/ram"]),
        ("an fd-witness boot graded as a mount boot", good),
    ):
        expect(False, mount(lines), why)
    # The two workloads are exclusive: neither boot satisfies the other's obligations.
    expect(False, fd(syn), "a mount-witness boot graded as an fd boot")

    lg = _life_synthetic()
    expect(True, life(lg), "the lifetime witness's own obligations")

    def lmut(fn):
        x = list(lg)
        fn(x)
        return x
    for why, fn in (
        ("a launch that did not spawn", drop("spawned_tid=10012")),
        ("an extra spawn", lambda x: x.append("SPAWN_FROM_MO_OK image_id=11 spawned_tid=10020")),
        ("a second NR 29 refusal", lambda x: x.append(f"{DISPATCH}nr=29 cpu=0 result=handled_err code=2")),
        ("the refusal is another error", sub(DISPATCH + "nr=29 cpu=0 result=handled_err", "code=2", "code=9")),
        ("the refusal is not image 14's use", sub("VL1_GRANT_USE", "image_id=14", "image_id=11")),
        ("no fork", drop(DISPATCH + "nr=12")),
        ("child exit of another task", sub("msg=VL1_CHILD_EXIT", "tid=10016", "tid=10017")),
        ("driver failed", sub("VL1_WITNESS_DONE", "driver_ok=1", "driver_ok=0")),
        ("not a lifetime boot", lambda x: x.__delitem__(slice(0, len(x)))),
    ):
        expect(False, life(lmut(fn)), why)
    expect(False, life(good), "an fd-witness boot graded as a lifetime boot")
    expect(False, fd(lg), "a lifetime boot graded as an fd boot")
    print(f"RISCV_WORKLOAD_OBLIGATIONS_SELF_TEST result=ok cases={n}")


def main(argv):
    if argv[:1] == ["--self-test"]:
        self_test()
        return 0
    if argv[:1] == ["profile"] and len(argv) == 3 and argv[1] == "--binding":
        p, f = profile(argv[2])
        for x in f:
            print(f"[fail] RISC-V workload: {x}")
        if p is None:
            return 1
        print(f"RISCV_WORKLOAD profile={p}")
        return 0
    if argv[:1] == ["fd"] and len(argv) == 2:
        return report(FD, *fd(load(argv[1])))
    if argv[:1] == ["mount"] and len(argv) == 2:
        return report(MOUNT, *mount(load(argv[1])))
    if argv[:1] == ["life"] and len(argv) == 2:
        return report(LIFE, *life(load(argv[1])))
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
