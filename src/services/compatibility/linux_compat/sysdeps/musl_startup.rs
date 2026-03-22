use super::{
    LINUX_NR_BRK, LINUX_NR_MMAP, LINUX_NR_MPROTECT, LINUX_NR_MUNMAP, LinuxErrno, clone_thread_hook,
};
use crate::kernel::bootstrap::KernelState;
use crate::kernel::task::ThreadGroupId;

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

pub const MUSL_TLS_ALIGN: usize = 16;
pub const MUSL_STACK_ALIGN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuslThreadSpec {
    pub tls_base: usize,
    pub stack_top: usize,
    pub entry: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuslThreadState {
    pub tid: u64,
    pub thread_pointer: usize,
    pub stack_top: usize,
    pub entry: usize,
    pub thread_group_id: ThreadGroupId,
    pub tls_restore_pending: bool,
}

pub fn validate_musl_thread_spec(spec: MuslThreadSpec) -> Result<MuslThreadSpec, LinuxErrno> {
    if spec.tls_base == 0 || spec.stack_top == 0 || spec.entry == 0 {
        return Err(LinuxErrno::Inval);
    }
    if !spec.tls_base.is_multiple_of(MUSL_TLS_ALIGN)
        || !spec.stack_top.is_multiple_of(MUSL_STACK_ALIGN)
    {
        return Err(LinuxErrno::Inval);
    }
    Ok(spec)
}

pub fn validate_musl_thread_state(
    kernel: &KernelState,
    parent_tid: u64,
    tid: u64,
    spec: MuslThreadSpec,
) -> Result<MuslThreadState, LinuxErrno> {
    let spec = validate_musl_thread_spec(spec)?;
    let thread_group_id = kernel.thread_group_id(tid).ok_or(LinuxErrno::Inval)?;
    let parent_group_id = kernel
        .thread_group_id(parent_tid)
        .ok_or(LinuxErrno::Inval)?;
    if thread_group_id != parent_group_id {
        return Err(LinuxErrno::Inval);
    }
    if kernel.thread_tls_base(tid) != Some(spec.tls_base) {
        return Err(LinuxErrno::Inval);
    }
    let context = kernel.thread_user_context(tid).ok_or(LinuxErrno::Inval)?;
    if context.instruction_ptr != spec.entry || context.stack_ptr != spec.stack_top {
        return Err(LinuxErrno::Inval);
    }
    let tls_restore_pending = kernel.tls_restore_pending(tid).ok_or(LinuxErrno::Inval)?;
    if !tls_restore_pending {
        return Err(LinuxErrno::Inval);
    }
    Ok(MuslThreadState {
        tid,
        thread_pointer: spec.tls_base,
        stack_top: spec.stack_top,
        entry: spec.entry,
        thread_group_id,
        tls_restore_pending,
    })
}

pub fn spawn_musl_thread(
    kernel: &mut KernelState,
    parent_tid: u64,
    spec: MuslThreadSpec,
) -> Result<MuslThreadState, LinuxErrno> {
    let spec = validate_musl_thread_spec(spec)?;
    let tid = clone_thread_hook(
        kernel,
        parent_tid,
        spec.tls_base,
        spec.stack_top,
        spec.entry,
    )?;
    validate_musl_thread_state(kernel, parent_tid, tid, spec)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::bootstrap::Bootstrap;
    use crate::kernel::task::TaskClass;

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
        crate::services::compatibility::linux_compat::sysdeps::set_tls_hook(
            &mut kernel,
            state.tid,
            0x3000_0100,
        )
        .expect("set tls");

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
}
