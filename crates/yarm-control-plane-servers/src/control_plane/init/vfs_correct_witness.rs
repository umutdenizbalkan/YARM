// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! VFS-CORRECT1A — the live read-path witness (init, `vfs-correct-witness`).
//!
//! Init is an ordinary userspace client here: every request goes through the production
//! `yarm_user_rt::vfs_client` wrappers to the resident VFS router, which forwards it to
//! `initramfs_srv`. The cell reads the fixture `/initramfs/etc/vfs-fixture` (300 bytes,
//! `byte[i] = (i * 131 + 43) mod 256`, longer than one inline completion) into successive,
//! differently sized, sentinel-filled destinations, checking every count, every byte against the
//! formula, and that each destination's tail is untouched; then end of file twice (and a
//! zero-length read between), close, a read after close, a missing path, a path with no mount and
//! an invalid fd — each followed by a valid request that must still work. It also repeats the
//! baseline capture (64 bytes of `/initramfs/init` into a sentinel buffer); the grader compares
//! those bytes with the build's `rootfs/init`.
//!
//! Lines (all `VC1A_*`): BEGIN, OPEN, STATX, READ (k size n off bytes_ok tail_ok), DATA (k off hex,
//! at most 32 bytes per line), EOF, CLOSE, ERR (case expect got), RECOVER (case ok), INIT_HEAD,
//! INIT_DATA, END (fails result).

use yarm_ipc_abi::vfs_abi::{
    VFS_INLINE_READ_MAX, VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_INVALID_PATH,
    VFS_STATUS_ERR_NO_MOUNT,
};
use yarm_user_rt::vfs_client::{VfsClientError, vfs_close, vfs_openat, vfs_read, vfs_statx};

const FIXTURE: &[u8] = b"/initramfs/etc/vfs-fixture";
const FIXTURE_LEN: usize = 300;
const SENTINEL: u8 = 0xA5;
/// Destinations, in order; 200 and 512 exceed one completion (104 bytes).
const SIZES: [usize; 6] = [13, 200, 1, 104, 64, 512];
/// What each read must return: the fixture is 300 bytes.
const EXPECT: [usize; 6] = [13, 104, 1, 104, 64, 14];
const REGION: usize = 600;

fn fixture_byte(i: usize) -> u8 {
    ((i * 131 + 43) & 0xFF) as u8
}

fn err_code(e: VfsClientError) -> (&'static str, u64) {
    match e {
        VfsClientError::Status(s) => ("status", u64::from(s)),
        VfsClientError::Malformed(_) => ("malformed", 0),
        VfsClientError::Send(e) => ("send", e as u64),
        VfsClientError::Recv(e) => ("recv", e as u64),
        VfsClientError::NoReply => ("no_reply", 0),
        VfsClientError::PathTooLong => ("path", 0),
        VfsClientError::MessageFailed => ("message", 0),
    }
}

/// Log `bytes` as hex, at most 32 bytes per line (the log line limit).
fn log_hex(tag: &str, k: usize, off: usize, bytes: &[u8]) {
    let mut start = 0;
    while start < bytes.len() {
        let end = core::cmp::min(start + 32, bytes.len());
        let mut hex = [0u8; 64];
        for (i, b) in bytes[start..end].iter().enumerate() {
            const H: &[u8; 16] = b"0123456789abcdef";
            hex[2 * i] = H[(b >> 4) as usize];
            hex[2 * i + 1] = H[(b & 0xF) as usize];
        }
        let hex = core::str::from_utf8(&hex[..2 * (end - start)]).unwrap_or("?");
        yarm_user_rt::user_log!("{} k={} off={} hex={}", tag, k, off + start, hex);
        start = end;
    }
}

/// Run the witness. `vfs_send` is init's send cap to the VFS router, `reply` its reply endpoint.
pub(super) fn run(vfs_send: u64, reply: u64) {
    let mut fails = 0u32;
    yarm_user_rt::user_log!(
        "VC1A_BEGIN fixture_len={} inline_max={}",
        FIXTURE_LEN,
        VFS_INLINE_READ_MAX
    );
    // SAFETY (all calls below): `vfs_send` and `reply` are init's kernel-provided startup caps.
    let fd = match unsafe { vfs_openat(vfs_send, reply, FIXTURE, 0) } {
        Ok(fd) => {
            yarm_user_rt::user_log!("VC1A_OPEN ok=1 fd={}", fd);
            fd
        }
        Err(e) => {
            let (kind, code) = err_code(e);
            yarm_user_rt::user_log!("VC1A_OPEN ok=0 kind={} code={}", kind, code);
            yarm_user_rt::user_log!("VC1A_END fails=1 result=fail");
            return;
        }
    };
    match unsafe { vfs_statx(vfs_send, reply, FIXTURE) } {
        Ok(v) => {
            yarm_user_rt::user_log!("VC1A_STATX ok=1 value={}", v);
            if v != FIXTURE_LEN as u64 {
                fails += 1;
            }
        }
        Err(e) => {
            let (kind, code) = err_code(e);
            yarm_user_rt::user_log!("VC1A_STATX ok=0 kind={} code={}", kind, code);
            fails += 1;
        }
    }

    let mut off = 0usize;
    for (k, (&size, &want)) in SIZES.iter().zip(EXPECT.iter()).enumerate() {
        let mut region = [SENTINEL; REGION];
        match unsafe { vfs_read(vfs_send, reply, fd, &mut region[..size]) } {
            Ok(n) => {
                let bytes_ok = region[..n]
                    .iter()
                    .enumerate()
                    .all(|(i, &b)| b == fixture_byte(off + i));
                let tail_ok = region[n..].iter().all(|&b| b == SENTINEL);
                yarm_user_rt::user_log!(
                    "VC1A_READ k={} size={} n={} off={} bytes_ok={} tail_ok={}",
                    k,
                    size,
                    n,
                    off,
                    u32::from(bytes_ok),
                    u32::from(tail_ok)
                );
                log_hex("VC1A_DATA", k, off, &region[..n]);
                if n != want || !bytes_ok || !tail_ok {
                    fails += 1;
                }
                off += n;
            }
            Err(e) => {
                let (kind, code) = err_code(e);
                yarm_user_rt::user_log!(
                    "VC1A_READ k={} size={} ok=0 kind={} code={}",
                    k,
                    size,
                    kind,
                    code
                );
                fails += 1;
            }
        }
    }

    // End of file twice, a zero-length request between.
    for (k, size) in [(0usize, 50usize), (1, 0), (2, 32)] {
        let mut region = [SENTINEL; 64];
        match unsafe { vfs_read(vfs_send, reply, fd, &mut region[..size]) } {
            Ok(n) => {
                let untouched = region.iter().all(|&b| b == SENTINEL);
                yarm_user_rt::user_log!(
                    "VC1A_EOF k={} size={} n={} untouched={}",
                    k,
                    size,
                    n,
                    u32::from(untouched)
                );
                if n != 0 || !untouched {
                    fails += 1;
                }
            }
            Err(e) => {
                let (kind, code) = err_code(e);
                yarm_user_rt::user_log!(
                    "VC1A_EOF k={} size={} ok=0 kind={} code={}",
                    k,
                    size,
                    kind,
                    code
                );
                fails += 1;
            }
        }
    }

    match unsafe { vfs_close(vfs_send, reply, fd) } {
        Ok(v) => yarm_user_rt::user_log!("VC1A_CLOSE ok=1 value={}", v),
        Err(e) => {
            let (kind, code) = err_code(e);
            yarm_user_rt::user_log!("VC1A_CLOSE ok=0 kind={} code={}", kind, code);
            fails += 1;
        }
    }

    // Errors through the same transport, each followed by a request that must still work.
    let mut expect_status = |case: &str, got: Result<u64, VfsClientError>, want: u32| {
        let (kind, code) = match got {
            Ok(v) => ("ok", v),
            Err(e) => err_code(e),
        };
        yarm_user_rt::user_log!(
            "VC1A_ERR case={} expect={} kind={} code={}",
            case,
            want,
            kind,
            code
        );
        if kind != "status" || code != u64::from(want) {
            fails += 1;
        }
    };
    let mut after = [SENTINEL; 8];
    let r = unsafe { vfs_read(vfs_send, reply, fd, &mut after) }.map(|n| n as u64);
    expect_status("read_after_close", r, VFS_STATUS_ERR_BAD_FD);
    let r = unsafe { vfs_openat(vfs_send, reply, b"/initramfs/etc/vfs-missing", 0) };
    expect_status("missing_path", r, VFS_STATUS_ERR_INVALID_PATH);
    let r = unsafe { vfs_openat(vfs_send, reply, b"/nomount/vfs-fixture", 0) };
    expect_status("no_mount", r, VFS_STATUS_ERR_NO_MOUNT);
    let mut bad = [SENTINEL; 8];
    let r = unsafe { vfs_read(vfs_send, reply, 9999, &mut bad) }.map(|n| n as u64);
    expect_status("bad_fd", r, VFS_STATUS_ERR_BAD_FD);
    let untouched = after.iter().chain(bad.iter()).all(|&b| b == SENTINEL);
    yarm_user_rt::user_log!("VC1A_ERR_UNTOUCHED ok={}", u32::from(untouched));
    if !untouched {
        fails += 1;
    }

    // A valid request still works: reopen, read the first 4 bytes, close.
    let recovered = match unsafe { vfs_openat(vfs_send, reply, FIXTURE, 0) } {
        Ok(fd2) => {
            let mut head = [SENTINEL; 4];
            let n = unsafe { vfs_read(vfs_send, reply, fd2, &mut head) };
            let closed = unsafe { vfs_close(vfs_send, reply, fd2) }.is_ok();
            matches!(n, Ok(4)) && (0..4).all(|i| head[i] == fixture_byte(i)) && closed
        }
        Err(_) => false,
    };
    yarm_user_rt::user_log!("VC1A_RECOVER ok={}", u32::from(recovered));
    if !recovered {
        fails += 1;
    }

    // The baseline capture repeated: 64 bytes of /initramfs/init into a sentinel buffer.
    match unsafe { vfs_openat(vfs_send, reply, b"/initramfs/init", 0) } {
        Ok(fd3) => {
            let mut head = [SENTINEL; 64];
            match unsafe { vfs_read(vfs_send, reply, fd3, &mut head) } {
                Ok(n) => {
                    let untouched = head.iter().filter(|&&b| b == SENTINEL).count();
                    yarm_user_rt::user_log!("VC1A_INIT_HEAD n={} sentinel_bytes={}", n, untouched);
                    log_hex("VC1A_INIT_DATA", 0, 0, &head[..n]);
                    if n != 64 {
                        fails += 1;
                    }
                }
                Err(e) => {
                    let (kind, code) = err_code(e);
                    yarm_user_rt::user_log!("VC1A_INIT_HEAD ok=0 kind={} code={}", kind, code);
                    fails += 1;
                }
            }
            let _ = unsafe { vfs_close(vfs_send, reply, fd3) };
        }
        Err(_) => {
            yarm_user_rt::user_log!("VC1A_INIT_HEAD ok=0 kind=open code=0");
            fails += 1;
        }
    }

    yarm_user_rt::user_log!(
        "VC1A_END fails={} result={}",
        fails,
        if fails == 0 { "ok" } else { "fail" }
    );
}
