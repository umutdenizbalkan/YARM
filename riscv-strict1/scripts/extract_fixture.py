#!/usr/bin/env python3
"""Extract a RISCV-STRICT1 workload fixture: the lines of one archived boot that the RISC-V core
smoke's split census, registration obligation and scripts/riscv64-workload-obligations.py read."""
import hashlib, re, sys
src, label, out, prov = sys.argv[1:5]
raw = open(src, "rb").read()
sha = hashlib.sha256(raw).hexdigest()
lines = raw.decode("utf-8", "replace").replace("\r", "\n").split("\n")
KEEP = re.compile(
    r"YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=|RISCV_SPLIT_FINALIZE_OK nr=12 |YIELD_SPLIT_COMMITTED |"
    r"SPAWN_FROM_MO_(OK|ENTER) |INIT_RAMFS_SPAWN_OK |msg=VC1[BC]_|FORK_TXN_COMMITTED |"
    r"EXIT_TASK_(SYSCALL_DISPATCHED|LIFECYCLE_TRANSITION|CLAIM_RETIRED) |"
    r"QUEUE_ADVANCING_DISPATCH_DEFERRED reason=exit_current_task_switch_required |"
    r"KSPAWN_ADMIN_(EP_CREATED|RECV_CAP_DELEGATED) |VFS_MOUNT_|INIT_VFS_ADMIN_CAP |"
    r"VFS_FORWARD_CALL|PM_VFS_GRANT_RO_RECEIVED |DRIVER_MANAGER_ENTRY|PM_VFS_CALL_FAIL|"
    r"VFS_BACKEND_COMPLETION_MALFORMED|INIT_MOUNT_REPLY_UNEXPECTED_CAP|BOOT_FATAL")
SAMPLE = {"5": 40, "2": 40}      # the two high-volume receive classes: the first N, verbatim
seen, kept, omitted = {}, [], {}
for x in lines:
    if not KEEP.search(x):
        continue
    m = re.match(r"YARM_LOCK_SPLIT_DISPATCH arch=riscv64 nr=(\d+) ", x)
    if m and m.group(1) in SAMPLE:
        n = seen[m.group(1)] = seen.get(m.group(1), 0) + 1
        if n > SAMPLE[m.group(1)]:
            omitted[m.group(1)] = omitted.get(m.group(1), 0) + 1
            continue
    kept.append(x)
with open(out, "w") as fh:
    fh.write(f"# RISCV-STRICT1 workload fixture: {label}\n")
    fh.write(f"# source: {prov}\n# source sha256: {sha}\n")
    fh.write("# selection: rv1-tools/extract_fixture.py (evidence branch); every kept line is verbatim, CR removed.\n")
    fh.write("# omitted: " + " ".join(f"nr={k} dispatch lines after the first {SAMPLE[k]}: {v}" for k, v in sorted(omitted.items())) + "\n")
    for x in kept:
        fh.write(x + "\n")
print(label, len(kept), omitted, sha)
