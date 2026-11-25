#!/usr/bin/env python3
# VFS-CORRECT1C negative-control mutations. Applied only inside a fresh worktree checked out at the
# committed candidate (the runner creates one per control); the candidate's own worktree is never
# touched. Each mutation must match exactly once.
import os
import sys

R = "crates/yarm-control-plane-servers/src/control_plane/vfs/router.rs"
MUT = {
    # F01: OPENAT returns the backend's handle as the client fd (the baseline's behavior).
    "ncH_handle_as_fd": [(
        R,
        "        let reply = completion(VfsCompletion::ok(VFS_OP_OPENAT, fd, 0), &[], None);",
        "        let reply = completion(VfsCompletion::ok(VFS_OP_OPENAT, handle, 0), &[], None);",
    )],
    # F14: the mount is chosen from the canonical path but the original spelling is forwarded.
    "ncP_original_spelling": [(
        R,
        "            mask_or_buf: args.mask_or_buf,\n            path: norm.as_bytes(),",
        "            mask_or_buf: args.mask_or_buf,\n            path: args.path,",
    ), (
        R,
        "            mode: args.mode,\n            path: norm.as_bytes(),",
        "            mode: args.mode,\n            path: args.path,",
    )],
    # F02: a failed client reply after a successful backend open does not close the backend handle.
    "ncR_no_rollback_close": [(
        R,
        "        let closed = match self.fds.close(session, fd) {\n            Ok(Some(c)) => {",
        "        let closed = match self.fds.close(session, fd) {\n            Ok(Some(c)) if false => {",
    )],
    # F03: the session is the calling THREAD (its TID as the group), not the process.
    "ncS_thread_session": [(
        "crates/yarm-control-plane-servers/src/control_plane/vfs/service.rs",
        "                yarm_fs_servers::common::fd_space::SessionId {\n"
        "                    thread_group_id,\n"
        "                    incarnation,\n"
        "                },",
        "                yarm_fs_servers::common::fd_space::SessionId {\n"
        "                    thread_group_id: tid + 0 * thread_group_id,\n"
        "                    incarnation,\n"
        "                },",
    )],
    # F03, kernel: NR 21's split route (the one every production call takes) names the caller's
    # TID as its thread group.
    "ncK_split_names_tid": [(
        "src/kernel/syscall/session.rs",
        "            frame.set_ok(tgid as usize, incarnation as usize, caller.tid.0 as usize);",
        "            let _ = tgid;\n"
        "            frame.set_ok(caller.tid.0 as usize, incarnation as usize, caller.tid.0 as usize);",
    )],
}


def main():
    name = sys.argv[1]
    for path, old, new in MUT[name]:
        p = os.path.join(os.getcwd(), path)
        s = open(p).read()
        n = s.count(old)
        if n != 1:
            print(f"nc {name}: {path}: pattern found {n} times", file=sys.stderr)
            return 2
        open(p, "w").write(s.replace(old, new))
    print(f"nc {name}: applied")
    return 0


if __name__ == "__main__":
    sys.exit(main())
