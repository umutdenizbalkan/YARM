#!/usr/bin/env python3
"""RISCV-ECALL-A7-RET1 negative controls. Usage: ea7_nc.py <worktree> <ncN>
Each control reintroduces ONE defect into the candidate; the permanent tests must catch it by
behaviour (a compile failure does not count)."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
RB = "src/arch/riscv64/boot.rs"; TF = "src/kernel/trapframe.rs"; BM = "src/kernel/boot/mod.rs"
TK = "src/kernel/task.rs"; RT = "crates/yarm-user-rt/src/arch/riscv64.rs"

def patch(rel, old, new, count=1):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == count, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))

def nc1():
    # The switched ordinary arm forces a7 = 0 again (bridge and S-mode idle).
    patch(RB, """            frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
        } else {""", """            frame.regs[RiscvTrapFrame::A7] = 0;
        } else {""")
    patch(RB, """                frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
            }
            #[cfg(feature = "riscv-ecall-witness")]""", """                frame.regs[RiscvTrapFrame::A7] = 0;
            }
            #[cfg(feature = "riscv-ecall-witness")]""")

def nc2():
    # The wrapper declares a2 an input: the compiler is told a result lane survives the ecall.
    patch(RT, '            inlateout("a2") a2,\n', '            in("a2") a2,\n')

def nc3():
    # a7 is overwritten after the (bridge) continuation arm restored it.
    patch(RB, """            frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
            crate::yarm_log!(
                "RISCV_SYSCALL_CONTINUATION_RESUME tid={} origin=bridge""", """            frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
            frame.regs[RiscvTrapFrame::A7] = 0;
            crate::yarm_log!(
                "RISCV_SYSCALL_CONTINUATION_RESUME tid={} origin=bridge""")

def nc4():
    # A syscall result is applied to an asynchronously preempted task (the bridge's async arm).
    patch(RB, """            frame.regs[RiscvTrapFrame::A0] = tframe.user_gpr(10) as u64;
            frame.regs[RiscvTrapFrame::A1] = tframe.user_gpr(11) as u64;
            frame.regs[RiscvTrapFrame::A2] = tframe.user_gpr(12) as u64;
            frame.regs[RiscvTrapFrame::A3] = tframe.user_gpr(13) as u64;
            frame.regs[RiscvTrapFrame::A4] = tframe.user_gpr(14) as u64;
            frame.regs[RiscvTrapFrame::A5] = tframe.user_gpr(15) as u64;
            frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
            crate::yarm_log!(
                "RISCV_ASYNC_PREEMPT_RESUME tid={} entering={}""", """            frame.regs[RiscvTrapFrame::A0] = tframe.ret0() as u64;
            frame.regs[RiscvTrapFrame::A1] = tframe.user_gpr(11) as u64;
            frame.regs[RiscvTrapFrame::A2] = tframe.user_gpr(12) as u64;
            frame.regs[RiscvTrapFrame::A3] = tframe.user_gpr(13) as u64;
            frame.regs[RiscvTrapFrame::A4] = tframe.user_gpr(14) as u64;
            frame.regs[RiscvTrapFrame::A5] = tframe.user_gpr(15) as u64;
            frame.regs[RiscvTrapFrame::A7] = tframe.user_gpr(17) as u64;
            crate::yarm_log!(
                "RISCV_ASYNC_PREEMPT_RESUME tid={} entering={}""")

def nc5():
    # Startup arguments over a live continuation (the bridge's continuation arm success path).
    patch(RB, """            } else {
                frame.regs[RiscvTrapFrame::A0] = tframe.ret0() as u64;
                frame.regs[RiscvTrapFrame::A1] = tframe.ret1() as u64;
                frame.regs[RiscvTrapFrame::A2] = tframe.ret2() as u64;
            }
            // RISCV-ECALL-A7-RET1: `a3..a5` and `a7` are INPUTS""", """            } else {
                frame.regs[RiscvTrapFrame::A0] = tframe.arg(0) as u64;
                frame.regs[RiscvTrapFrame::A1] = tframe.ret1() as u64;
                frame.regs[RiscvTrapFrame::A2] = tframe.ret2() as u64;
            }
            // RISCV-ECALL-A7-RET1: `a3..a5` and `a7` are INPUTS""")

def nc6():
    # A completion of another blocked generation is accepted (a send completion of an old cycle).
    patch(TK, "            BlockedSyscallClass::IpcSend => tcb.blocked_send_generation,",
          "            BlockedSyscallClass::IpcSend => self.blocked_generation,")

def nc7():
    # The parked FutexWait `a0 = address` defect: FutexWait is left out of the park commit.
    patch(BM, """    let final_result = (nr == crate::kernel::syscall::SYSCALL_FUTEX_WAIT_NR
        && futex == Some(outgoing))""", """    let final_result = (nr == usize::MAX && futex == Some(outgoing))""")

def nc8():
    # The parked Yield argument-return defect: Yield is left out of the park commit.
    patch(BM, """        || (nr == crate::kernel::syscall::SYSCALL_YIELD_NR
            && futex.is_none()""", """        || (nr == usize::MAX
            && futex.is_none()""")

def nc9():
    # sepc is advanced a second time (the park commit adds 4 to the already-advanced pc).
    patch(TF, """        self.args[2] = a2;
    }""", """        self.args[2] = a2;
        self.saved_pc += 4;
    }""")

def nc10():
    # A delivered completion is consumed again: the take leaves it parked.
    patch(TK, """    // Take it either way — a stale entry must not linger to be seen by a later block.
    tcb.pending_syscall_completion = None;
""", "")

def nc11():
    # The same-task arm writes a3 = 0 again (the old error lane).
    patch(RB, """            // there is one, in `a0`). The asm save preserved them and the mirror loop skipped them.
            frame.regs[RiscvTrapFrame::A3] = tframe.user_gpr(13) as u64;""", """            // there is one, in `a0`). The asm save preserved them and the mirror loop skipped them.
            frame.regs[RiscvTrapFrame::A3] = 0;""")

C = {f"nc{i}": globals()[f"nc{i}"] for i in range(1, 12)}
C[which]()
print(f"{which} applied")
