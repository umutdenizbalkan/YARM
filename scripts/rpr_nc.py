#!/usr/bin/env python3
"""RISCV-PARK-RET1 negative controls. Usage: rpr_nc.py <worktree> <ncN>
Each control reintroduces ONE defect into the candidate; the permanent tests must catch it by
behaviour (a compile failure does not count). NC10 is the one structural control (see its note)."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
TF = "src/kernel/trapframe.rs"; BM = "src/kernel/boot/mod.rs"; TK = "src/kernel/task.rs"

def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:100], s.count(old))
    p.write_text(s.replace(old, new))

COMMIT = """        self.user_gprs[10] = a0;
        self.user_gprs[11] = a1;
        self.user_gprs[12] = a2;
        self.args[0] = a0;
        self.args[1] = a1;
        self.args[2] = a2;
    }"""

def nc1():
    # The resumed a0 is the original futex address: a0's lanes keep the syscall's argument.
    patch(TF, COMMIT, COMMIT.replace("self.user_gprs[10] = a0;", "self.user_gprs[10] = self.args[0];")
          .replace("self.args[0] = a0;", "let _ = a0;"))

def nc2():
    # The final syscall result is never committed (the bridge's step is a no-op).
    patch(BM, """    if final_result {
        frame.commit_riscv_parked_syscall_returns();
    }
    final_result""", """    let _ = &frame;
    final_result""")

def nc3():
    # The result is committed where a later step overwrites it: into the register file only, so
    # the switched write-back's argument-lane install puts the syscall's arguments back over it.
    patch(TF, COMMIT, """        self.user_gprs[10] = a0;
        self.user_gprs[11] = a1;
        self.user_gprs[12] = a2;
    }""")

def nc4():
    # The result is published for the wrong incarnation: an EXITING task, which is never resumed.
    patch(BM, """    if exit_queue_advance_pending(cpu_idx).is_some_and(|(exiting, _)| exiting == outgoing) {
        return false;
    }
""", "")

def nc5():
    # A stale blocked generation is accepted: a send completion of an older cycle matches.
    patch(TK, "            BlockedSyscallClass::IpcSend => tcb.blocked_send_generation,",
          "            BlockedSyscallClass::IpcSend => self.blocked_generation,")

def nc6():
    # A syscall result is applied to an asynchronously preempted task: the snapshot installs the
    # saved syscall lane's a0 over the live register file.
    patch(TK, """    tcb.user_context = captured;
    tcb.user_context.arg0 = preserved_syscall_lane.0;""", """    tcb.user_context = captured;
    tcb.user_context.user_gprs[10] = preserved_syscall_lane.0;
    tcb.user_context.arg0 = preserved_syscall_lane.0;""")

def nc7():
    # The commit overwrites the whole argument mirror (a0..a5), lanes the resume installs as the
    # caller's own — for a fresh task, its startup arguments.
    patch(TF, COMMIT, COMMIT.replace("        self.args[2] = a2;\n    }", """        self.args[2] = a2;
        for i in 3..6 {
            self.args[i] = 0;
            self.user_gprs[10 + i] = 0;
        }
    }"""))

def nc8():
    # sepc is advanced a second time (the bridge already advanced it past the ecall at entry).
    patch(TF, COMMIT, COMMIT.replace("        self.args[2] = a2;\n    }", """        self.args[2] = a2;
        self.saved_pc += 4;
    }"""))

def nc9():
    # A completion already delivered is consumed again: the take leaves it parked.
    patch(TK, """    // Take it either way — a stale entry must not linger to be seen by a later block.
    tcb.pending_syscall_completion = None;
""", "")

def nc10():
    # STRUCTURAL: the return is settled through a global lock. Production has no broad lock left
    # (`SharedKernel::with` is test-only, so reintroducing it does not compile there); any other
    # lock the commit took is found by `t22`, and a `with`/`with_cpu` callsite by the census guard.
    patch(BM, """    let futex = futex_wait_dispatch_outgoing(cpu_idx);
    let recv = d2_recv_dispatch_outgoing(cpu_idx);
    let yielded = yield_dispatch_outgoing(cpu_idx);
    // The chain the bridge's capture reads, in its order.""", """    static SETTLE: crate::kernel::lock::SpinLock<()> = crate::kernel::lock::SpinLock::new(());
    let _global = SETTLE.lock();
    let futex = futex_wait_dispatch_outgoing(cpu_idx);
    let recv = d2_recv_dispatch_outgoing(cpu_idx);
    let yielded = yield_dispatch_outgoing(cpu_idx);
    // The chain the bridge's capture reads, in its order.""")

C = {f"nc{i}": globals()[f"nc{i}"] for i in range(1, 11)}
C[which]()
print(f"{which} applied")
