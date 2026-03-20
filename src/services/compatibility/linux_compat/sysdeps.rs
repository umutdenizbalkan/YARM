use super::{LINUX_NR_BRK, LINUX_NR_MMAP, LINUX_NR_MPROTECT, LINUX_NR_MUNMAP, LinuxErrno};
use crate::kernel::bootstrap::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::proc_proto::{PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID};
use crate::kernel::process_manager::ProcessService;
use crate::kernel::vfs::{
    CloseRequest, OpenAtRequest, ReadWriteRequest, VfsBackend, close_message, openat_message,
    read_message, write_message,
};
use crate::kernel::vm::PAGE_SIZE;
use crate::services::common::service::FsService;
use crate::services::network::socket::service::SocketAdapterService;

/// Minimal sysdeps status used while porting musl to x86_64-unknown-none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysdepsBootstrapStatus {
    pub startup_hook_ready: bool,
    pub memory_hooks_ready: bool,
    pub clock_hooks_ready: bool,
    pub thread_hooks_ready: bool,
    pub futex_hooks_ready: bool,
    pub io_hooks_ready: bool,
}

impl SysdepsBootstrapStatus {
    pub const fn in_progress() -> Self {
        Self {
            startup_hook_ready: true,
            memory_hooks_ready: true,
            clock_hooks_ready: true,
            thread_hooks_ready: true,
            futex_hooks_ready: true,
            io_hooks_ready: true,
        }
    }
}

/// Stable syscall numbers expected by the shim for memory bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySyscallNumbers {
    pub brk: usize,
    pub mmap: usize,
    pub munmap: usize,
    pub mprotect: usize,
}

pub const fn memory_syscall_numbers() -> MemorySyscallNumbers {
    MemorySyscallNumbers {
        brk: LINUX_NR_BRK,
        mmap: LINUX_NR_MMAP,
        munmap: LINUX_NR_MUNMAP,
        mprotect: LINUX_NR_MPROTECT,
    }
}

const MAX_STARTUP_ARGS: usize = 16;
const MAX_STARTUP_ENVP: usize = 16;
const MAX_STARTUP_AUXV: usize = 16;
pub const AUXV_AT_NULL: usize = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupBootstrapInfo {
    pub stack_top: usize,
    pub argc: usize,
    pub argv_ptr: usize,
    pub envp_ptr: usize,
    pub auxv_ptr: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxVectorEntry {
    pub key: usize,
    pub value: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuslStartupFrame {
    pub info: StartupBootstrapInfo,
    pub argv: [usize; MAX_STARTUP_ARGS],
    pub envp: [usize; MAX_STARTUP_ENVP],
    pub auxv: [AuxVectorEntry; MAX_STARTUP_AUXV],
    pub envc: usize,
    pub auxc: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuslStartupOutcome {
    pub info: StartupBootstrapInfo,
    pub envc: usize,
    pub auxc: usize,
    pub main_return: i32,
}

fn word_size() -> usize {
    core::mem::size_of::<usize>()
}

fn stack_base(stack_top: usize, words_len: usize) -> Result<usize, LinuxErrno> {
    stack_top
        .checked_sub(
            words_len
                .checked_mul(word_size())
                .ok_or(LinuxErrno::Inval)?,
        )
        .ok_or(LinuxErrno::Inval)
}

pub fn parse_musl_initial_stack(
    stack_top: usize,
    words: &[usize],
) -> Result<MuslStartupFrame, LinuxErrno> {
    if stack_top == 0 || words.is_empty() {
        return Err(LinuxErrno::Inval);
    }
    let argc = words[0];
    if argc > MAX_STARTUP_ARGS {
        return Err(LinuxErrno::Inval);
    }
    let argv_end = 1usize.checked_add(argc).ok_or(LinuxErrno::Inval)?;
    if argv_end >= words.len() || words[argv_end] != 0 {
        return Err(LinuxErrno::Inval);
    }

    let mut argv = [0usize; MAX_STARTUP_ARGS];
    let mut idx = 0usize;
    while idx < argc {
        argv[idx] = words[1 + idx];
        idx += 1;
    }

    let env_start = argv_end + 1;
    let mut env_end = env_start;
    while env_end < words.len() && words[env_end] != 0 {
        env_end += 1;
    }
    if env_end >= words.len() {
        return Err(LinuxErrno::Inval);
    }
    let envc = env_end - env_start;
    if envc > MAX_STARTUP_ENVP {
        return Err(LinuxErrno::Inval);
    }
    let mut envp = [0usize; MAX_STARTUP_ENVP];
    idx = 0;
    while idx < envc {
        envp[idx] = words[env_start + idx];
        idx += 1;
    }

    let aux_start = env_end + 1;
    let mut auxv = [AuxVectorEntry { key: 0, value: 0 }; MAX_STARTUP_AUXV];
    let mut auxc = 0usize;
    let mut cursor = aux_start;
    loop {
        if cursor + 1 >= words.len() {
            return Err(LinuxErrno::Inval);
        }
        let key = words[cursor];
        let value = words[cursor + 1];
        if key == AUXV_AT_NULL {
            break;
        }
        if auxc >= MAX_STARTUP_AUXV {
            return Err(LinuxErrno::Inval);
        }
        auxv[auxc] = AuxVectorEntry { key, value };
        auxc += 1;
        cursor += 2;
    }

    let base = stack_base(stack_top, words.len())?;
    let info = StartupBootstrapInfo {
        stack_top,
        argc,
        argv_ptr: base + word_size(),
        envp_ptr: base + env_start * word_size(),
        auxv_ptr: base + aux_start * word_size(),
    };
    Ok(MuslStartupFrame {
        info,
        argv,
        envp,
        auxv,
        envc,
        auxc,
    })
}

pub fn startup_hook(info: StartupBootstrapInfo) -> Result<StartupBootstrapInfo, LinuxErrno> {
    if info.stack_top == 0 || info.argv_ptr == 0 || info.envp_ptr == 0 || info.auxv_ptr == 0 {
        return Err(LinuxErrno::Inval);
    }
    if info.argc > MAX_STARTUP_ARGS
        || info.argv_ptr >= info.stack_top
        || info.envp_ptr >= info.stack_top
        || info.auxv_ptr >= info.stack_top
    {
        return Err(LinuxErrno::Inval);
    }
    if !(info.argv_ptr <= info.envp_ptr && info.envp_ptr <= info.auxv_ptr) {
        return Err(LinuxErrno::Inval);
    }
    Ok(info)
}

pub fn prepare_musl_startup(
    stack_top: usize,
    words: &[usize],
) -> Result<MuslStartupFrame, LinuxErrno> {
    let frame = parse_musl_initial_stack(stack_top, words)?;
    startup_hook(frame.info)?;
    Ok(frame)
}

pub fn run_musl_startup(
    stack_top: usize,
    words: &[usize],
    main: fn(usize, usize, usize, usize) -> i32,
) -> Result<MuslStartupOutcome, LinuxErrno> {
    let frame = prepare_musl_startup(stack_top, words)?;
    let main_return = main(
        frame.info.argc,
        frame.info.argv_ptr,
        frame.info.envp_ptr,
        frame.info.auxv_ptr,
    );
    Ok(MuslStartupOutcome {
        info: frame.info,
        envc: frame.envc,
        auxc: frame.auxc,
        main_return,
    })
}

pub struct LinuxSysdepsContext<'a, B: VfsBackend> {
    pub kernel: &'a mut KernelState,
    pub proc_service: &'a mut ProcessService,
    pub vfs_service: &'a mut FsService<B>,
    pub socket_service: &'a mut SocketAdapterService,
}

impl<'a, B: VfsBackend> LinuxSysdepsContext<'a, B> {
    pub fn new(
        kernel: &'a mut KernelState,
        proc_service: &'a mut ProcessService,
        vfs_service: &'a mut FsService<B>,
        socket_service: &'a mut SocketAdapterService,
    ) -> Self {
        Self {
            kernel,
            proc_service,
            vfs_service,
            socket_service,
        }
    }

    fn decode_u64(reply: Message) -> Result<usize, LinuxErrno> {
        if reply.as_slice().len() < 8 {
            return Err(LinuxErrno::Inval);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&reply.as_slice()[..8]);
        usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| LinuxErrno::Inval)
    }

    pub fn clock_gettime_hook(&mut self) -> Result<u64, LinuxErrno> {
        Ok(self
            .kernel
            .timer
            .current_ticks()
            .0
            .saturating_mul(1_000_000))
    }

    pub fn nanosleep_hook(&mut self, nanos: u64) -> Result<(), LinuxErrno> {
        if nanos == 0 {
            return Ok(());
        }
        let ticks = nanos.saturating_add(999_999) / 1_000_000;
        for _ in 0..ticks {
            self.kernel.timer.tick();
        }
        Ok(())
    }

    pub fn getpid_hook(&mut self) -> Result<u64, LinuxErrno> {
        let tid = self
            .kernel
            .scheduler
            .current_tid()
            .ok_or(LinuxErrno::NoSys)?;
        let reply = self
            .proc_service
            .handle(
                Message::with_header(0, PROC_OP_GETPID, 0, None, &tid.to_le_bytes())
                    .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(Self::decode_u64(reply)? as u64)
    }

    pub fn getppid_hook(&mut self) -> Result<u64, LinuxErrno> {
        let tid = self
            .kernel
            .scheduler
            .current_tid()
            .ok_or(LinuxErrno::NoSys)?;
        let reply = self
            .proc_service
            .handle(
                Message::with_header(0, PROC_OP_GETPPID, 0, None, &tid.to_le_bytes())
                    .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(Self::decode_u64(reply)? as u64)
    }

    pub fn exit_hook(&mut self, code: u64) -> Result<(), LinuxErrno> {
        self.proc_service
            .handle(
                Message::with_header(0, PROC_OP_EXIT, 0, None, &code.to_le_bytes())
                    .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(())
    }

    pub fn openat_hook(
        &mut self,
        path_ptr: usize,
        flags: u32,
        mode: u32,
    ) -> Result<i32, LinuxErrno> {
        let reply = self
            .vfs_service
            .handle(
                openat_message(OpenAtRequest {
                    dirfd: 0,
                    path_ptr: path_ptr as u64,
                    flags: flags as u64,
                    mode: mode as u64,
                })
                .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        i32::try_from(Self::decode_u64(reply)?).map_err(|_| LinuxErrno::Inval)
    }

    pub fn socket_hook(
        &mut self,
        domain: i32,
        sock_type: i32,
        protocol: i32,
    ) -> Result<i32, LinuxErrno> {
        self.socket_service
            .open(domain, sock_type, protocol)
            .map_err(|_| LinuxErrno::Inval)
    }

    pub fn read_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        buf_len: usize,
    ) -> Result<usize, LinuxErrno> {
        if self.socket_service.is_socket_fd(fd) {
            return self
                .socket_service
                .read(fd, buf_len)
                .map_err(|_| LinuxErrno::Inval);
        }
        let reply = self
            .vfs_service
            .handle(
                read_message(ReadWriteRequest {
                    fd: fd as u64,
                    buf_ptr: buf_ptr as u64,
                    len: buf_len as u64,
                })
                .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Self::decode_u64(reply)
    }

    pub fn write_hook(
        &mut self,
        fd: i32,
        buf_ptr: usize,
        buf_len: usize,
    ) -> Result<usize, LinuxErrno> {
        if self.socket_service.is_socket_fd(fd) {
            return self
                .socket_service
                .write(fd, buf_len)
                .map_err(|_| LinuxErrno::Inval);
        }
        let reply = self
            .vfs_service
            .handle(
                write_message(ReadWriteRequest {
                    fd: fd as u64,
                    buf_ptr: buf_ptr as u64,
                    len: buf_len as u64,
                })
                .map_err(|_| LinuxErrno::Inval)?,
            )
            .map_err(|_| LinuxErrno::Inval)?;
        Self::decode_u64(reply)
    }

    pub fn close_hook(&mut self, fd: i32) -> Result<(), LinuxErrno> {
        if self.socket_service.is_socket_fd(fd) {
            return self.socket_service.close(fd).map_err(|_| LinuxErrno::Inval);
        }
        self.vfs_service
            .handle(close_message(CloseRequest { fd: fd as u64 }).map_err(|_| LinuxErrno::Inval)?)
            .map_err(|_| LinuxErrno::Inval)?;
        Ok(())
    }
}

pub fn mmap_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<usize, LinuxErrno> {
    kernel
        .linux_mmap_region(aspace_cap, addr, len, prot)
        .map_err(Into::into)
}

pub fn munmap_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
) -> Result<(), LinuxErrno> {
    kernel
        .linux_munmap_region(aspace_cap, addr, len)
        .map_err(Into::into)
}

pub fn mprotect_hook(
    kernel: &mut KernelState,
    aspace_cap: CapId,
    addr: usize,
    len: usize,
    prot: usize,
) -> Result<(), LinuxErrno> {
    kernel
        .linux_mprotect_region(aspace_cap, addr, len, prot)
        .map_err(Into::into)
}

pub fn brk_hook(
    kernel: &mut KernelState,
    tid: u64,
    aspace_cap: CapId,
    requested: usize,
    prot: usize,
) -> Result<usize, LinuxErrno> {
    kernel
        .linux_brk(tid, aspace_cap, requested, prot)
        .map_err(Into::into)
}

pub fn clone_thread_hook(
    kernel: &mut KernelState,
    parent_tid: u64,
    tls_base: usize,
    user_stack_top: usize,
    user_entry: usize,
) -> Result<u64, LinuxErrno> {
    kernel
        .spawn_user_thread(parent_tid, tls_base, user_stack_top, user_entry)
        .map_err(Into::into)
}

pub fn set_tls_hook(kernel: &mut KernelState, tid: u64, tls_base: usize) -> Result<(), LinuxErrno> {
    kernel
        .set_thread_tls_base(tid, tls_base)
        .map_err(Into::into)
}

pub fn get_tls_hook(kernel: &KernelState, tid: u64) -> Result<Option<usize>, LinuxErrno> {
    if tid == 0 {
        return Err(LinuxErrno::Inval);
    }
    Ok(kernel.thread_tls_base(tid))
}

pub fn futex_wait_hook(
    kernel: &mut KernelState,
    addr: usize,
    expected: u32,
    observed: u32,
) -> Result<bool, LinuxErrno> {
    kernel
        .futex_wait_current(addr, expected, observed)
        .map_err(LinuxErrno::from)
}

pub fn futex_wake_hook(
    kernel: &mut KernelState,
    addr: usize,
    max_wake: u32,
) -> Result<u32, LinuxErrno> {
    kernel.futex_wake(addr, max_wake).map_err(LinuxErrno::from)
}

pub const fn default_mmap_len() -> usize {
    PAGE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;
    use crate::kernel::task::TaskClass;
    use crate::kernel::vfs::InMemoryBackend;
    use crate::services::common::service::FsService;

    #[test]
    fn memory_syscall_numbers_match_linux_compat_contract() {
        let nums = memory_syscall_numbers();
        assert_eq!(nums.brk, LINUX_NR_BRK);
        assert_eq!(nums.mmap, LINUX_NR_MMAP);
        assert_eq!(nums.munmap, LINUX_NR_MUNMAP);
        assert_eq!(nums.mprotect, LINUX_NR_MPROTECT);
    }

    #[test]
    fn startup_hook_validates_nonzero_stack_top() {
        let ok = startup_hook(StartupBootstrapInfo {
            stack_top: 0x1000,
            argc: 1,
            argv_ptr: 1,
            envp_ptr: 2,
            auxv_ptr: 3,
        })
        .expect("startup");
        assert_eq!(ok.stack_top, 0x1000);
        assert_eq!(ok.argc, 1);
        assert_eq!(
            startup_hook(StartupBootstrapInfo {
                stack_top: 0,
                argc: 1,
                argv_ptr: 1,
                envp_ptr: 2,
                auxv_ptr: 3
            }),
            Err(LinuxErrno::Inval)
        );
    }

    #[test]
    fn prepare_musl_startup_parses_initial_stack_layout() {
        let words = [2, 0x1110, 0x2220, 0, 0x3330, 0, 6, 0x4440, AUXV_AT_NULL, 0];
        let stack_top = 0x9000;
        let frame = prepare_musl_startup(stack_top, &words).expect("startup frame");
        let base = stack_top - words.len() * core::mem::size_of::<usize>();
        assert_eq!(frame.info.argc, 2);
        assert_eq!(frame.info.argv_ptr, base + core::mem::size_of::<usize>());
        assert_eq!(
            frame.info.envp_ptr,
            base + 4 * core::mem::size_of::<usize>()
        );
        assert_eq!(
            frame.info.auxv_ptr,
            base + 6 * core::mem::size_of::<usize>()
        );
        assert_eq!(&frame.argv[..2], &[0x1110, 0x2220]);
        assert_eq!(&frame.envp[..1], &[0x3330]);
        assert_eq!(
            frame.auxv[0],
            AuxVectorEntry {
                key: 6,
                value: 0x4440
            }
        );
        assert_eq!(frame.envc, 1);
        assert_eq!(frame.auxc, 1);
    }

    #[test]
    fn run_musl_startup_invokes_main_with_prepared_vectors() {
        fn fake_main(argc: usize, argv_ptr: usize, envp_ptr: usize, auxv_ptr: usize) -> i32 {
            argc as i32 + ((argv_ptr < envp_ptr && envp_ptr <= auxv_ptr) as i32)
        }

        let words = [1, 0x1110, 0, 0x3330, 0, AUXV_AT_NULL, 0];
        let outcome = run_musl_startup(0x8000, &words, fake_main).expect("startup run");
        assert_eq!(outcome.info.argc, 1);
        assert_eq!(outcome.envc, 1);
        assert_eq!(outcome.auxc, 0);
        assert_eq!(outcome.main_return, 2);
    }

    #[test]
    fn prepare_musl_startup_rejects_missing_argv_terminator() {
        let words = [1, 0x1110, 0x2220, 0x3330];
        assert_eq!(prepare_musl_startup(0x7000, &words), Err(LinuxErrno::Inval));
    }

    #[test]
    fn service_backed_clock_hooks_use_kernel_timer() {
        let mut kernel = Bootstrap::init().expect("init");
        let mut proc = ProcessService::new();
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let mut socket = SocketAdapterService::new();
        let mut ctx = LinuxSysdepsContext::new(&mut kernel, &mut proc, &mut vfs, &mut socket);
        assert_eq!(ctx.clock_gettime_hook().expect("before"), 0);
        ctx.nanosleep_hook(2_500_000).expect("sleep");
        assert_eq!(ctx.clock_gettime_hook().expect("after"), 3_000_000);
    }

    #[test]
    fn thread_tls_and_futex_hooks_have_stable_semantics() {
        let mut kernel = crate::kernel::bootstrap::Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = kernel.create_user_address_space().expect("asid");
        kernel
            .spawn_user_task_from_image(crate::kernel::bootstrap::UserImageSpec {
                tid: 7,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("parent");
        let tid =
            clone_thread_hook(&mut kernel, 7, 0xDEAD_BEEF, 0x8000_0000, 0x4010).expect("clone");
        assert!(tid >= 10_000);
        set_tls_hook(&mut kernel, tid, 0xFEED_CAFE).expect("set tls");
        assert_eq!(
            get_tls_hook(&kernel, tid).expect("get tls"),
            Some(0xFEED_CAFE)
        );
        assert!(!futex_wait_hook(&mut kernel, 0x1000, 3, 4).expect("mismatch"));
        assert!(futex_wait_hook(&mut kernel, 0x1000, 3, 3).expect("wait"));
        assert_eq!(futex_wake_hook(&mut kernel, 0x1000, 1).expect("wake"), 1);
        assert_eq!(
            futex_wake_hook(&mut kernel, 0x1000, 1).expect("wake empty"),
            0
        );
    }

    #[test]
    fn memory_hooks_route_into_kernel_vm_helpers() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");
        let addr = mmap_hook(
            &mut state,
            aspace_cap,
            0x8000,
            default_mmap_len(),
            super::super::PROT_READ | super::super::PROT_WRITE,
        )
        .expect("mmap");
        assert_eq!(addr, 0x8000);
        mprotect_hook(
            &mut state,
            aspace_cap,
            0x8000,
            default_mmap_len(),
            super::super::PROT_READ,
        )
        .expect("mprotect");
        munmap_hook(&mut state, aspace_cap, 0x8000, default_mmap_len()).expect("munmap");
    }

    #[test]
    fn brk_hook_routes_into_kernel_brk_helper() {
        let mut state = Bootstrap::init().expect("init");
        let (_asid, aspace_cap) = state.create_user_address_space().expect("aspace");
        let grown = brk_hook(
            &mut state,
            0,
            aspace_cap,
            0x4000_0000 + PAGE_SIZE,
            super::super::PROT_READ | super::super::PROT_WRITE,
        )
        .expect("grow");
        assert_eq!(grown, 0x4000_0000 + PAGE_SIZE);
    }

    #[test]
    fn status_tracks_bootstrap_progress() {
        let status = SysdepsBootstrapStatus::in_progress();
        assert!(status.startup_hook_ready);
        assert!(status.memory_hooks_ready);
        assert!(status.clock_hooks_ready);
        assert!(status.thread_hooks_ready);
        assert!(status.futex_hooks_ready);
        assert!(status.io_hooks_ready);
    }

    #[test]
    fn service_backed_proc_and_vfs_hooks_roundtrip_real_services() {
        let mut kernel = Bootstrap::init().expect("init");
        kernel.register_task(41).expect("task");
        kernel.scheduler.enqueue(41).expect("enqueue");
        kernel.dispatch_next_task().expect("dispatch");

        let mut proc = ProcessService::new();
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let mut socket = SocketAdapterService::new();
        let mut ctx = LinuxSysdepsContext::new(&mut kernel, &mut proc, &mut vfs, &mut socket);

        assert_eq!(ctx.getpid_hook().expect("getpid"), 41);
        assert_eq!(ctx.getppid_hook().expect("getppid"), 40);
        ctx.exit_hook(0).expect("exit");

        let fd = ctx.openat_hook(0x1000, 0, 0).expect("open");
        assert!(fd >= 3);
        assert_eq!(ctx.read_hook(fd, 0x2000, 128).expect("read"), 128);
        assert_eq!(ctx.write_hook(fd, 0x2000, 11).expect("write"), 11);
        ctx.close_hook(fd).expect("close");
        assert_eq!(ctx.read_hook(fd, 0x2000, 1), Err(LinuxErrno::Inval));
    }

    #[test]
    fn socket_hooks_route_through_socket_service() {
        let mut kernel = Bootstrap::init().expect("init");
        let mut proc = ProcessService::new();
        let mut vfs = FsService::with_backend(InMemoryBackend::new());
        let mut socket = SocketAdapterService::new();
        let mut ctx = LinuxSysdepsContext::new(&mut kernel, &mut proc, &mut vfs, &mut socket);
        let fd = ctx.socket_hook(2, 1, 0).expect("socket");
        assert!(fd >= 1000);
        assert_eq!(ctx.read_hook(fd, 0, 128).expect("read"), 64);
        assert_eq!(ctx.write_hook(fd, 0, 32).expect("write"), 32);
        ctx.close_hook(fd).expect("close");
    }
}
