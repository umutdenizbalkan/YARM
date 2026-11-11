#!/usr/bin/env python3
"""FUTEX-FOUND1 patch, part D: the user ABI and every in-tree caller (user runtime, init_server,
compat hooks, the witness programs and their mailboxes). Usage: ff1_patch_d.py <worktree>"""
import pathlib, re, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:80], s.count(old))
    p.write_text(s.replace(old, new))

# ── user runtime ────────────────────────────────────────────────────────────────────────────
UR = "crates/yarm-user-rt/src/lib.rs"
patch(UR, '''    #[inline]
    pub fn futex_wait(
        addr: *const u32,
        expected: u32,
        observed: u32,
    ) -> core::result::Result<bool, SyscallError> {
        // SAFETY: Uses architecture syscall ABI to enter kernel. The kernel validates
        // that `addr` names a readable current-user futex word before blocking.
        let ret = unsafe {
            crate::arch::raw_syscall(
                SYSCALL_FUTEX_WAIT_NR,
                [addr as usize, expected as usize, observed as usize, 0, 0, 0],
            )
        };''', '''    /// FUTEX-FOUND1 — which namespace a futex word is keyed in (NR 9 / NR 10, argument slot 2).
    ///
    /// `Private`: the caller's own address space and the word's VA — for threads of one process.
    /// `Shared`: the backing MemoryObject and the word's offset in it — for processes that map one
    /// object (at any VA). A shared wait on memory no allocator-owned object backs is refused
    /// with `WrongObject`; it is never silently made private.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FutexScope {
        Private,
        Shared,
    }

    impl FutexScope {
        /// The flag word this scope encodes (`FUTEX_FLAG_SHARED` = bit 0).
        pub const fn flags(self) -> usize {
            match self {
                Self::Private => 0,
                Self::Shared => 1,
            }
        }
    }

    /// Wait while the 32-bit word at `addr` (4-byte aligned) holds `expected` (NR 9).
    ///
    /// The KERNEL reads the word and compares it with `expected`; `Ok(false)` means it already
    /// differed and nothing parked, `Ok(true)` that the caller parked and was woken. Spurious
    /// returns are possible only as wakes of the same key, so callers re-check their condition.
    #[inline]
    pub fn futex_wait(
        addr: *const u32,
        expected: u32,
        scope: FutexScope,
    ) -> core::result::Result<bool, SyscallError> {
        // SAFETY: Uses architecture syscall ABI to enter kernel. The kernel validates
        // that `addr` names a readable current-user futex word before blocking.
        let ret = unsafe {
            crate::arch::raw_syscall(
                SYSCALL_FUTEX_WAIT_NR,
                [addr as usize, expected as usize, scope.flags(), 0, 0, 0],
            )
        };''')
patch(UR, '''    /// Wake up to `max_wake` waiters blocked on the futex word at `addr` (NR 10).
    /// Returns the number of waiters actually woken. On AArch64 (Stage 195C) this flows
    /// through the split-dispatch seam (`try_split_futex_wake_into_frame`); the count is
    /// in x0 (`ret0`), byte-identical to the legacy global-lock return.
    #[inline]
    pub fn futex_wake(addr: *const u32, max_wake: u32) -> core::result::Result<u32, SyscallError> {
        // SAFETY: Uses architecture syscall ABI. The kernel validates that `addr` names a
        // readable current-user futex word before scanning waiters.
        let ret = unsafe {
            crate::arch::raw_syscall(
                SYSCALL_FUTEX_WAKE_NR,
                [addr as usize, max_wake as usize, 0, 0, 0, 0],
            )
        };''', '''    /// Wake up to `count` waiters of the futex word at `addr` in `scope`, oldest first (NR 10).
    /// Returns the number actually woken. The count is in `ret0` on every architecture.
    #[inline]
    pub fn futex_wake(
        addr: *const u32,
        count: u32,
        scope: FutexScope,
    ) -> core::result::Result<u32, SyscallError> {
        // SAFETY: Uses architecture syscall ABI. The kernel validates that `addr` names a
        // readable current-user futex word before claiming waiters.
        let ret = unsafe {
            crate::arch::raw_syscall(
                SYSCALL_FUTEX_WAKE_NR,
                [addr as usize, count as usize, scope.flags(), 0, 0, 0],
            )
        };''')

# ── every user-rt caller: rewrite the third argument / add the scope ────────────────────────
def edit_calls(text, fname, transform):
    out = []; i = 0; needle = fname + "("
    while True:
        j = text.find(needle, i)
        if j < 0:
            out.append(text[i:]); break
        k = j + len(needle); depth = 1; m = k
        while depth:
            c = text[m]
            if c in "([": depth += 1
            elif c in ")]": depth -= 1
            m += 1
        args = []; cur = ""; d = 0
        for c in text[k:m-1]:
            if c in "([": d += 1
            if c in ")]": d -= 1
            if c == "," and d == 0:
                args.append(cur.strip()); cur = ""
            else:
                cur += c
        if cur.strip(): args.append(cur.strip())
        out.append(text[i:k]); out.append(transform(args)); out.append(")")
        i = m
    return "".join(out)
SCOPE = "yarm_user_rt::syscall::FutexScope::Private"
for rel, scope in [("crates/yarm-control-plane-servers/src/control_plane/init/service.rs", SCOPE),
                   ("crates/yarm-control-plane-servers/src/bin/init_server.rs",
                    "yarm::user_rt::syscall::FutexScope::Private")]:
    p = wt / rel; s = p.read_text()
    for prefix in ["yarm_user_rt::syscall::", "yarm::user_rt::syscall::"]:
        s = edit_calls(s, prefix + "futex_wait", lambda a: f"{a[0]}, {a[1]}, {scope}")
        s = edit_calls(s, prefix + "futex_wake", lambda a: f"{a[0]}, {a[1]}, {scope}")
    p.write_text(s)

# ── compat hooks (they call the kernel owner directly) ──────────────────────────────────────
CH = "crates/yarm-compat-servers/src/posix_compat/sysdeps/kernel_hooks.rs"
patch(CH, '''pub fn futex_wait_hook(
    kernel: &mut KernelState,
    addr: usize,
    expected: u32,
    observed: u32,
) -> Result<bool, PosixErrno> {
    kernel
        .futex_wait_current(addr, expected, observed)
        .map_err(PosixErrno::from)
}''', '''pub fn futex_wait_hook(
    kernel: &mut KernelState,
    addr: usize,
    expected: u32,
) -> Result<bool, PosixErrno> {
    kernel
        .futex_wait_current(
            addr,
            expected,
            crate::kernel::boot::futex::FutexKind::Private,
        )
        .map_err(PosixErrno::from)
}''')
patch(CH, '''pub fn futex_wait_hook(_addr: usize, _expected: u32, _observed: u32) -> Result<bool, PosixErrno> {''',
      '''pub fn futex_wait_hook(_addr: usize, _expected: u32) -> Result<bool, PosixErrno> {''')
patch(CH, '''    kernel.futex_wake(addr, max_wake).map_err(PosixErrno::from)''',
      '''    kernel
        .futex_wake(
            addr,
            max_wake,
            crate::kernel::boot::futex::FutexKind::Private,
        )
        .map_err(PosixErrno::from)''')
patch(CH, '''        assert!(!futex_wait_hook(&mut kernel, 0x1000, 3, 4).expect("mismatch"));
        assert!(futex_wait_hook(&mut kernel, 0x1000, 3, 3).expect("wait"));''',
      '''        // The kernel compares `expected` with the word it reads (3).
        assert!(!futex_wait_hook(&mut kernel, 0x1000, 4).expect("mismatch"));
        assert!(futex_wait_hook(&mut kernel, 0x1000, 3).expect("wait"));''')

# ── witness programs ────────────────────────────────────────────────────────────────────────
# apsched: private waits/wakes among threads of one process; the flags word is set explicitly.
patch("src/arch/x86_64/apsched_witness.S", '''    lea rsi, [r15 - 1]
    mov rdx, rax
    SYS NR_FUTEX_WAIT''', '''    lea rsi, [r15 - 1]
    xor edx, edx            /* FUTEX-FOUND1: flags = private */
    SYS NR_FUTEX_WAIT''')
patch("src/arch/x86_64/apsched_witness.S", '''    lea rdi, [MBX + M_F + r14 * 4]
    mov esi, 1
    SYS NR_FUTEX_WAKE''', '''    lea rdi, [MBX + M_F + r14 * 4]
    mov esi, 1
    xor edx, edx            /* FUTEX-FOUND1: flags = private */
    SYS NR_FUTEX_WAKE''')
patch("src/arch/aarch64/apsched_witness.S", '''    sub x1, x20, #1
    mov x2, x10
    SVC NR_FUTEX_WAIT''', '''    sub x1, x20, #1
    mov x2, #0              /* FUTEX-FOUND1: flags = private */
    SVC NR_FUTEX_WAIT''')
patch("src/arch/aarch64/apsched_witness.S", '''    mov x0, x25
    mov x1, #1
    SVC NR_FUTEX_WAKE''', '''    mov x0, x25
    mov x1, #1
    mov x2, #0              /* FUTEX-FOUND1: flags = private */
    SVC NR_FUTEX_WAKE''')
patch("src/arch/riscv64/apsched_witness.S", '''    addi a1, s1, -1
    mv a2, t1
    li a7, NR_FUTEX_WAIT''', '''    addi a1, s1, -1
    li a2, 0                /* FUTEX-FOUND1: flags = private */
    li a7, NR_FUTEX_WAIT''')
patch("src/arch/riscv64/apsched_witness.S", '''    mv a0, s6
    li a1, 1
    li a7, NR_FUTEX_WAKE''', '''    mv a0, s6
    li a1, 1
    li a2, 0                /* FUTEX-FOUND1: flags = private */
    li a7, NR_FUTEX_WAKE''')
# overtaken: W and K are two PROCESSES sharing one mailbox object, so their futex is SHARED.
patch("src/arch/x86_64/overtaken_witness.S", '''     * FutexWait(addr, expected = observed = r15). BL6e: since BL3b the kernel parks only while the
     * 32-bit word still holds `expected`, so the program stores it first. Nothing else writes these
     * words; only K's FutexWake (on WAIT) ends a park.
     */
    .macro OVT_WAIT_ON addr
    mov eax, 9
    mov edi, \\addr
    mov dword ptr [rdi], r15d
    mov rsi, r15
    mov rdx, r15
    syscall
    .endm''', '''     * FutexWait(addr, expected = r15, flags = SHARED). The kernel parks only while the 32-bit word
     * still holds `expected`, so the program stores it first. W and K are two processes mapping one
     * mailbox MemoryObject, so the futex is SHARED (FUTEX-FOUND1). Nothing else writes these words;
     * only K's FutexWake (on WAIT) ends a park.
     */
    .macro OVT_WAIT_ON addr
    mov eax, 9
    mov edi, \\addr
    mov dword ptr [rdi], r15d
    mov rsi, r15
    mov edx, 1
    syscall
    .endm''')
patch("src/arch/x86_64/overtaken_witness.S", '''    mov eax, 10
    mov edi, OVT_WAIT
    mov esi, 1
    syscall''', '''    mov eax, 10
    mov edi, OVT_WAIT
    mov esi, 1
    mov edx, 1              /* FUTEX-FOUND1: flags = shared (W is another process) */
    syscall''')
patch("src/arch/aarch64/overtaken_witness.S", '''     * FutexWait(addr, expected = observed = x20). BL6e: since BL3b the kernel parks only while the
     * 32-bit word still holds `expected`, so the program stores it first. Nothing else writes these
     * words; only K's FutexWake (on WAIT) ends a park.
     */
    .macro OWAIT addr
    OLI32 x0, \\addr
    str w20, [x0]
    mov x1, x20
    mov x2, x20''', '''     * FutexWait(addr, expected = x20, flags = SHARED). The kernel parks only while the 32-bit word
     * still holds `expected`, so the program stores it first. W and K are two processes mapping one
     * mailbox MemoryObject, so the futex is SHARED (FUTEX-FOUND1). Nothing else writes these words;
     * only K's FutexWake (on WAIT) ends a park.
     */
    .macro OWAIT addr
    OLI32 x0, \\addr
    str w20, [x0]
    mov x1, x20
    mov x2, #1''')
patch("src/arch/aarch64/overtaken_witness.S", '''    OLI32 x0, WAIT_VA
    mov x1, #1
    mov x8, #NR_FUTEX_WAKE''', '''    OLI32 x0, WAIT_VA
    mov x1, #1
    mov x2, #1              /* FUTEX-FOUND1: flags = shared (W is another process) */
    mov x8, #NR_FUTEX_WAKE''')
# The mailbox: an allocator-owned MemoryObject (so a shared key resolves), each mapping accounted.
patch("src/kernel/boot/exec_state.rs", '''        let mbx = self.alloc_user_data_frame()?;
        for asid in [server_asid, client_asid] {
            self.map_user_page_in_asid_raw(
                asid,
                VirtAddr(ow::MBX_VA),
                Mapping {
                    phys: PhysAddr(mbx),
                    flags: PageFlags::USER_RW,
                },
            )?;
        }''', '''        // FUTEX-FOUND1: the mailbox is ONE allocator-owned MemoryObject mapped by both processes,
        // so their futex words resolve to one SHARED key; each mapping takes its reference.
        let (_mbx_object, mbx_phys) = self.alloc_anonymous_object_without_cap()?;
        let mbx = mbx_phys.0;
        for asid in [server_asid, client_asid] {
            self.map_user_page_in_asid_raw(
                asid,
                VirtAddr(ow::MBX_VA),
                Mapping {
                    phys: PhysAddr(mbx),
                    flags: PageFlags::USER_RW,
                },
            )?;
            self.note_mapping_inserted(PhysAddr(mbx));
        }''')
patch("src/arch/aarch64/overtaken_witness.rs", '''    kernel.map_user_page_in_asid_raw(
        asid,
        VirtAddr(ow::MBX_VA),
        Mapping {
            phys: PhysAddr(mbx),
            flags: PageFlags::USER_RW,
        },
    )?;''', '''    kernel.map_user_page_in_asid_raw(
        asid,
        VirtAddr(ow::MBX_VA),
        Mapping {
            phys: PhysAddr(mbx),
            flags: PageFlags::USER_RW,
        },
    )?;
    // FUTEX-FOUND1: the mailbox object's reference for this mapping.
    kernel.note_mapping_inserted(PhysAddr(mbx));''')
patch("src/arch/aarch64/overtaken_witness.rs", '''    let mbx = kernel.alloc_user_data_frame()?;''',
      '''    // FUTEX-FOUND1: ONE allocator-owned MemoryObject, so W's and K's words resolve to one
    // SHARED futex key.
    let (_mbx_object, mbx_phys) = kernel.alloc_anonymous_object_without_cap()?;
    let mbx = mbx_phys.0;''')
print("patch D applied")
