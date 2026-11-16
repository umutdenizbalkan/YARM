#!/usr/bin/env python3
"""AArch64 default-profile boot completion (BOOT-COMPLETE1).

The first `SCHED_ENTER_IDLE_HLT` of an AArch64 boot appears before the supervisor's first userspace
log: that CPU merely has nothing to run yet. It is no evidence that the boot finished, and stopping
QEMU two seconds after it (the earlier stop condition) can cut a boot off in the middle of RAMFS's
ELF load while the verdict still passes. A default-profile boot is complete only when, IN THIS
ORDER:

  1. the core services have started (supervisor, process manager, init, initramfs, devfs, VFS);
  2. RAMFS is spawned, reports its `/ram` mount ready, and VFS registers `/ram`;
  3. EXT4 is spawned, becomes ready, and VFS registers `/ext4`;
  4. init reaches its terminal park (INIT_IDLE_PARK_BEGIN, INIT_IDLE_PARK_FUTEX_BEGIN, and the
     kernel publishing init's futex block);
  5. the scheduler then enters idle (`SCHED_ENTER_IDLE_HLT`), with init still parked.

Each stage is matched at its first occurrence AFTER the previous stage, so an idle marker (or any
other marker) from before boot completion can never satisfy a later stage. The core services must
each appear before RAMFS is spawned; their relative order is not fixed.

  check LOG [--quiet]   exit 0 iff the sequence is complete; prints one line per stage
  self-test             synthetic positive and negative cases
  replay FAIL_LOG PASS_LOG
                        preserved logs: FAIL_LOG must be incomplete, PASS_LOG complete, and
                        PASS_LOG cut off before its post-park idle must be incomplete although its
                        early idle marker is still present
"""
import re
import sys

CORE = [
    ("supervisor", r"\bmsg=SUP_RUN_ENTER\b"),
    ("process_manager", r"\bmsg=PM_RECV_LOOP_START\b"),
    ("init", r"\bmsg=INIT_BIN_ENTRY_START\b"),
    ("initramfs_srv", r"\bmsg=INITRAMFS_SRV_BLOCKING_RECV_LOOP\b"),
    ("devfs_srv", r"\bmsg=DEVFS_SRV_BLOCKING_RECV_LOOP\b"),
    ("vfs_srv", r"\bmsg=VFS_SRV_BLOCKING_RECV_LOOP\b"),
]

ORDERED = [
    ("ramfs_spawn", r"\bmsg=INIT_RAMFS_SPAWN_BEGIN\b"),
    ("ramfs_mount_ready", r"\bmsg=RAMFS_MOUNT_READY prefix=/ram\b"),
    ("ramfs_spawn_ok", r"\bmsg=INIT_RAMFS_SPAWN_OK\b.*\bprefix=/ram\b"),
    ("ram_registered", r"\bmsg=VFS_MOUNT_REGISTER_RAMFS_OK prefix=/ram\b"),
    ("ext4_spawn", r"\bmsg=INIT_EXT4_SPAWN_BEGIN\b"),
    ("ext4_ready", r"\bmsg=EXT4_SRV_READY\b"),
    ("ext4_spawn_ok", r"\bmsg=INIT_EXT4_SPAWN_OK\b"),
    ("ext4_registered", r"\bmsg=VFS_MOUNT_REGISTER_EXT4_OK prefix=/ext4\b"),
    ("init_park", r"\bmsg=INIT_IDLE_PARK_BEGIN\b"),
    ("init_park_futex", r"\bmsg=INIT_IDLE_PARK_FUTEX_BEGIN\b"),
    ("init_parked", r"\bFUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK tid=1 "),
    ("idle_after_park", r"^SCHED_ENTER_IDLE_HLT\b"),
]

# Init leaving its park between the block and the idle would make that idle someone else's.
UNPARK = re.compile(r"\bmsg=INIT_IDLE_PARK_(FUTEX_RETURN|FALLBACK_YIELD)\b")
IDLE_PATH = re.compile(r"\b(AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED|TIMER_SPLIT_IDLE_ADVANCE_COMMITTED)\b")


def evaluate(lines):
    """Returns (complete, records, reason). records: [(stage, lineno, text)]."""
    records = []
    ramfs_pat = re.compile(ORDERED[0][1])
    ramfs_at = next((i for i, l in enumerate(lines) if ramfs_pat.search(l)), None)
    for name, pat in CORE:
        rx = re.compile(pat)
        at = next((i for i, l in enumerate(lines) if rx.search(l)), None)
        if at is None:
            return False, records, f"core service never started: {name}"
        if ramfs_at is not None and at > ramfs_at:
            return False, records, f"core service {name} started only after RAMFS was spawned"
        records.append((name, at + 1, lines[at]))
    pos = -1
    for name, pat in ORDERED:
        rx = re.compile(pat)
        at = next((i for i in range(pos + 1, len(lines)) if rx.search(lines[i])), None)
        if at is None:
            return False, records, f"missing {name} after line {pos + 1}"
        if name == "idle_after_park":
            between = [i for i in range(pos + 1, at) if UNPARK.search(lines[i])]
            if between:
                return False, records, f"init left its park at line {between[0] + 1} before the idle"
            path = next((i for i in range(at - 1, pos, -1) if IDLE_PATH.search(lines[i])), None)
            if path is not None:
                records.append(("idle_path", path + 1, lines[path]))
        records.append((name, at + 1, lines[at]))
        pos = at
    return True, records, "complete"


def read_lines(path):
    with open(path, "rb") as f:
        data = f.read().decode("utf-8", "replace")
    return [l.strip() for l in data.replace("\r", "\n").split("\n")]


def check(path, quiet):
    try:
        lines = read_lines(path)
    except OSError as e:
        print(f"BOOT_COMPLETION result=incomplete reason=unreadable:{e}")
        return 1
    ok, records, reason = evaluate(lines)
    if not quiet:
        for name, n, text in records:
            print(f"BOOT_COMPLETION stage={name} line={n} text={text[:160]}")
        print(f"BOOT_COMPLETION result={'complete' if ok else 'incomplete'} reason={reason}")
    return 0 if ok else 1


def synthetic(drop=(), early_idle=True, swap=None, unpark=False):
    out = ["YARM_BOOT_OK present_cpus=2"]
    if early_idle:
        out.append("SCHED_ENTER_IDLE_HLT")
    for name, _ in CORE:
        out.append({
            "supervisor": "USER_LOG tid=2 msg=SUP_RUN_ENTER",
            "process_manager": "USER_LOG tid=3 msg=PM_RECV_LOOP_START",
            "init": "USER_LOG tid=1 msg=INIT_BIN_ENTRY_START",
            "initramfs_srv": "USER_LOG tid=10000 msg=INITRAMFS_SRV_BLOCKING_RECV_LOOP",
            "devfs_srv": "USER_LOG tid=10001 msg=DEVFS_SRV_BLOCKING_RECV_LOOP",
            "vfs_srv": "USER_LOG tid=10002 msg=VFS_SRV_BLOCKING_RECV_LOOP",
        }[name])
    seq = [
        ("ramfs_spawn", "USER_LOG tid=1 msg=INIT_RAMFS_SPAWN_BEGIN"),
        ("ramfs_mount_ready", "USER_LOG tid=10006 msg=RAMFS_MOUNT_READY prefix=/ram fd=101"),
        ("ramfs_spawn_ok", "USER_LOG tid=1 msg=INIT_RAMFS_SPAWN_OK child_tid=10006 send_cap=1 prefix=/ram"),
        ("ram_registered", "USER_LOG tid=1 msg=VFS_MOUNT_REGISTER_RAMFS_OK prefix=/ram"),
        ("ext4_spawn", "USER_LOG tid=1 msg=INIT_EXT4_SPAWN_BEGIN"),
        ("ext4_ready", "USER_LOG tid=10007 msg=EXT4_SRV_READY"),
        ("ext4_spawn_ok", "USER_LOG tid=1 msg=INIT_EXT4_SPAWN_OK child_tid=10007 send_cap=2"),
        ("ext4_registered", "USER_LOG tid=1 msg=VFS_MOUNT_REGISTER_EXT4_OK prefix=/ext4"),
        ("init_park", "USER_LOG tid=1 msg=INIT_IDLE_PARK_BEGIN"),
        ("init_park_futex", "USER_LOG tid=1 msg=INIT_IDLE_PARK_FUTEX_BEGIN"),
        ("init_parked", "FUTEX_WAIT_SPLIT_BLOCK_PUBLISH_OK tid=1 addr=4296704 priority=exact"),
    ]
    if swap:
        a, b = [i for i, (n, _) in enumerate(seq) if n in swap]
        seq[a], seq[b] = seq[b], seq[a]
    out += [t for n, t in seq if n not in drop]
    if unpark:
        out.append("USER_LOG tid=1 msg=INIT_IDLE_PARK_FUTEX_RETURN ret=0")
    out.append("AARCH64_FUTEX_WAIT_POST_LOCK_IDLE_ENTERED cpu=0")
    if "idle_after_park" not in drop:
        out.append("SCHED_ENTER_IDLE_HLT")
    return out


def self_test():
    cases = [
        ("complete boot", synthetic(), True),
        ("complete boot without the early idle", synthetic(early_idle=False), True),
        ("early idle only (no idle after the park)", synthetic(drop=("idle_after_park",)), False),
        ("stopped in RAMFS load (D2s shape)",
         ["YARM_BOOT_OK", "SCHED_ENTER_IDLE_HLT"] + synthetic(early_idle=False)[1:8] + ["SCHED_ENTER_IDLE_HLT"], False),
        ("no /ram registration", synthetic(drop=("ram_registered",)), False),
        ("no EXT4 ready", synthetic(drop=("ext4_ready",)), False),
        ("no /ext4 registration", synthetic(drop=("ext4_registered",)), False),
        ("/ext4 registered before /ram", synthetic(swap=("ram_registered", "ext4_registered")), False),
        ("init's block published before its park began", synthetic(swap=("init_park_futex", "init_parked")), False),
        ("no kernel block for init's park", synthetic(drop=("init_parked",)), False),
        ("init left its park before the idle", synthetic(unpark=True), False),
    ]
    core_missing = [l for l in synthetic() if "VFS_SRV_BLOCKING_RECV_LOOP" not in l]
    cases.append(("a core service never started", core_missing, False))
    late_core = synthetic()
    i = next(k for k, l in enumerate(late_core) if "DEVFS_SRV_BLOCKING_RECV_LOOP" in l)
    late_core.append(late_core.pop(i))
    cases.append(("a core service started after RAMFS", late_core, False))
    bad = 0
    for name, lines, want in cases:
        got, _, reason = evaluate(lines)
        mark = "ok" if got == want else "FAIL"
        bad += got != want
        print(f"[{mark}] {name}: {'complete' if got else 'incomplete'} ({reason})")
    print(f"BOOT_COMPLETION self-test {'passed' if not bad else 'FAILED'} ({len(cases)} cases)")
    return 1 if bad else 0


def replay(fail_log, pass_log):
    bad = 0
    f_ok, _, f_reason = evaluate(read_lines(fail_log))
    print(f"[{'ok' if not f_ok else 'FAIL'}] {fail_log}: must be incomplete: {f_reason}")
    bad += f_ok
    lines = read_lines(pass_log)
    p_ok, records, p_reason = evaluate(lines)
    print(f"[{'ok' if p_ok else 'FAIL'}] {pass_log}: must be complete: {p_reason}")
    bad += not p_ok
    if p_ok:
        idle_line = dict((n, ln) for n, ln, _ in records)["idle_after_park"]
        cut = lines[: idle_line - 1]
        early = [i + 1 for i, l in enumerate(cut) if re.match(r"^SCHED_ENTER_IDLE_HLT\b", l)]
        c_ok, _, c_reason = evaluate(cut)
        good = bool(early) and not c_ok
        print(f"[{'ok' if good else 'FAIL'}] {pass_log} cut before line {idle_line} "
              f"(early idle markers kept at lines {early}): must be incomplete: {c_reason}")
        bad += not good
    print(f"BOOT_COMPLETION replay {'passed' if not bad else 'FAILED'}")
    return 1 if bad else 0


def main(argv):
    if len(argv) >= 2 and argv[1] == "self-test":
        return self_test()
    if len(argv) >= 4 and argv[1] == "replay":
        return replay(argv[2], argv[3])
    if len(argv) >= 3 and argv[1] == "check":
        return check(argv[2], "--quiet" in argv[3:])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
