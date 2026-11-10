#!/usr/bin/env python3
"""Add the SEND-TIMEOUT-ARB1 test-only phase hooks to a 414afb86 tree at the BASELINE positions
(drain: between its rank-2 claim and its rank-3 removal). Usage: sta1_hooks_baseline.py <wt> <scratch>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); sc = pathlib.Path(sys.argv[2])
def patch(rel, old, new):
    p = wt / rel; s = p.read_text(); assert s.count(old) == 1, (rel, old[:60], s.count(old)); p.write_text(s.replace(old, new))
p = wt / "src/kernel/boot/sender_waiter.rs"; p.write_text(p.read_text() + (sc / "sta1_hookmod.rs").read_text())
patch("src/kernel/boot/ipc_state.rs", '''            crate::yarm_log!("D2_SEND_GENUINE_PHASE_TASK_BLOCK tid={}", blocked_tid);
        }
''', '''            crate::yarm_log!("D2_SEND_GENUINE_PHASE_TASK_BLOCK tid={}", blocked_tid);
        }
        #[cfg(test)]
        super::sender_waiter::send_arb_phase_hook::run_between_block_and_publish(self);
''')
patch("src/runtime.rs", '''            // ── (2) rank 3: remove the waiter and resolve what it still owned ───────────────
''', '''            #[cfg(test)]
            crate::kernel::boot::sender_waiter::send_arb_phase_hook::run_between_drain_phases();
            // ── (2) rank 3: remove the waiter and resolve what it still owned ───────────────
''')
print("baseline hooks added")
