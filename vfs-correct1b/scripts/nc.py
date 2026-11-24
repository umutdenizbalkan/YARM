#!/usr/bin/env python3
# VFS-CORRECT1B negative-control mutations. Applied only inside a fresh worktree checked out at the
# committed candidate (the runner creates one per control); the candidate's own worktree is never
# touched. Each mutation must match exactly once.
import os
import sys

ROOT = os.getcwd()
MUT = {
    # F07: the user runtime cuts a received capability ID to 32 bits (the baseline's `as u32`).
    "ncT_recv_truncates": (
        "crates/yarm-user-rt/src/lib.rs",
        "        let returned_cap = if meta.cap_id == SYSCALL_NO_TRANSFER_CAP {\n"
        "            None\n"
        "        } else {\n"
        "            Some(meta.cap_id)\n"
        "        };\n"
        "        let preview_len",
        "        let returned_cap = if meta.cap_id == SYSCALL_NO_TRANSFER_CAP {\n"
        "            None\n"
        "        } else {\n"
        "            Some(meta.cap_id & 0xFFFF_FFFF)\n"
        "        };\n"
        "        let preview_len",
    ),
    # F07 at the routing record: VFS keeps only the low 32 bits of the backend it received.
    "ncT_vfs_truncates": (
        "crates/yarm-fs-servers/src/fs/common/mount_admin.rs",
        "            sender_local_cap: args.sender_local_cap,\n            backend_cap,\n",
        "            sender_local_cap: args.sender_local_cap,\n"
        "            backend_cap: backend_cap & 0xFFFF_FFFF,\n",
    ),
    # F04: VFS routes through the number the sender named (its own slot) instead of the received cap.
    "ncS_sender_slot": (
        "crates/yarm-fs-servers/src/fs/common/mount_admin.rs",
        "            sender_local_cap: args.sender_local_cap,\n            backend_cap,\n",
        "            sender_local_cap: args.sender_local_cap,\n"
        "            backend_cap: args.sender_local_cap,\n",
    ),
    # F04 provisioning: the kernel creates the admin endpoint but never hands its receive side over.
    "ncK_no_slot17": (
        "src/kernel/syscall/spawn_txn.rs",
        "                spec.startup_args[17] = grant.dest_cap.0;\n",
        "                let _ = grant.dest_cap.0;\n",
    ),
}
name = sys.argv[1]
path, old, new = MUT[name]
p = os.path.join(ROOT, path)
s = open(p).read()
assert s.count(old) == 1, (name, s.count(old))
open(p, "w").write(s.replace(old, new))
print(f"applied {name} to {path}")
