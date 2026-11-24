#!/usr/bin/env python3
# VFS-CORRECT1A negative-control mutations (applied to a worktree, restored with git checkout).
import sys, os
ROOT = os.getcwd()
MUT = {
    # The client returns the validated count but does not copy the bytes.
    "ncA_count_without_copy": ("crates/yarm-user-rt/src/vfs_client.rs",
        "            buf[..data.len()].copy_from_slice(data);\n            Ok(data.len())",
        "            let _ = &buf;\n            Ok(data.len())"),
    # The client ignores a failed NR 6 and receives anyway.
    "ncB_receive_after_failed_send": ("crates/yarm-user-rt/src/vfs_client.rs",
        "    t.call(msg).map_err(VfsClientError::Send)?;",
        "    let _ = t.call(msg);"),
    # The resident handler asks the backend for the whole request (cursor advances past what is delivered).
    "ncC_uncapped_backend_read": ("crates/yarm-fs-servers/src/fs/common/vfs_service.rs",
        "let cap = core::cmp::min(args.len, VFS_INLINE_READ_MAX as u64) as usize;",
        "let cap = core::cmp::min(args.len, 512) as usize;\n                let mut inline = [0u8; 512];"),
}
name = sys.argv[1]
path, old, new = MUT[name]
p = os.path.join(ROOT, path)
s = open(p).read()
assert s.count(old) == 1, (name, s.count(old))
s = s.replace(old, new)
if name == "ncC_uncapped_backend_read":
    # the original buffer declaration becomes unused; drop it
    s = s.replace("                let mut inline = [0u8; VFS_INLINE_READ_MAX];\n", "", 1)
open(p, "w").write(s)
print(f"applied {name} to {path}")
