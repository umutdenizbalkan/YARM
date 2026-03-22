use super::{LINUX_NR_BRK, LINUX_NR_MMAP, LINUX_NR_MPROTECT, LINUX_NR_MUNMAP, LinuxErrno};
use crate::kernel::bootstrap::KernelState;
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::proc_proto::{PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID};
use crate::kernel::process_manager::ProcessService;
use crate::kernel::task::ThreadGroupId;
use crate::kernel::vfs::{
    CloseRequest, OpenAtRequest, ReadWriteRequest, VfsBackend, close_message, openat_message,
    read_message, write_message,
};
use crate::kernel::vm::PAGE_SIZE;
use crate::services::common::service::FsService;
use crate::services::network::socket::service::SocketAdapterService;

mod musl_startup;
pub use musl_startup::*;

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
            let _ = self.kernel.timer.tick_and_check();
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
    fn musl_thread_spawn_validates_kernel_state_against_musl_expectations() {
        let mut kernel = Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = kernel.create_user_address_space().expect("asid");
        kernel
            .spawn_user_task_from_image(crate::kernel::bootstrap::UserImageSpec {
                tid: 7,
                entry: 0x4000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("leader");

        let state = spawn_musl_thread(
            &mut kernel,
            7,
            MuslThreadSpec {
                tls_base: 0x2000_0000,
                stack_top: 0x8000_0010,
                entry: 0x4010,
            },
        )
        .expect("musl thread");

        assert_eq!(state.thread_pointer, 0x2000_0000);
        assert_eq!(state.stack_top, 0x8000_0010);
        assert_eq!(state.entry, 0x4010);
        assert_eq!(state.thread_group_id, ThreadGroupId(7));
        assert!(state.tls_restore_pending);
    }

    #[test]
    fn musl_thread_validation_rejects_unaligned_tls_or_stack() {
        assert_eq!(
            validate_musl_thread_spec(MuslThreadSpec {
                tls_base: 3,
                stack_top: 0x8000_0010,
                entry: 0x4010,
            }),
            Err(LinuxErrno::Inval)
        );
        assert_eq!(
            validate_musl_thread_spec(MuslThreadSpec {
                tls_base: 0x2000_0000,
                stack_top: 0x8000_0008,
                entry: 0x4010,
            }),
            Err(LinuxErrno::Inval)
        );
    }

    #[test]
    fn musl_thread_validation_accepts_tls_updates_that_require_restore() {
        let mut kernel = Bootstrap::init().expect("init");
        let (asid, _aspace_cap) = kernel.create_user_address_space().expect("asid");
        kernel
            .spawn_user_task_from_image(crate::kernel::bootstrap::UserImageSpec {
                tid: 9,
                entry: 0x5000,
                asid: Some(asid),
                class: TaskClass::App,
            })
            .expect("leader");
        let state = spawn_musl_thread(
            &mut kernel,
            9,
            MuslThreadSpec {
                tls_base: 0x3000_0000,
                stack_top: 0x9000_0010,
                entry: 0x5010,
            },
        )
        .expect("thread");
        set_tls_hook(&mut kernel, state.tid, 0x3000_0100).expect("set tls");

        let refreshed = validate_musl_thread_state(
            &kernel,
            9,
            state.tid,
            MuslThreadSpec {
                tls_base: 0x3000_0100,
                stack_top: 0x9000_0010,
                entry: 0x5010,
            },
        )
        .expect("validate");
        assert_eq!(refreshed.thread_pointer, 0x3000_0100);
        assert!(refreshed.tls_restore_pending);
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
