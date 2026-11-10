#!/usr/bin/env python3
"""IRQ-DELIVERY1 negative controls. Usage: id1_nc.py <worktree> <ncN>
Each control reintroduces ONE defect into the candidate; the hosted suites must fail."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
D = "src/kernel/boot/defs.rs"
F = "src/kernel/boot/fault_state.rs"
R = "src/runtime.rs"
SS = "src/kernel/syscall_split.rs"
TE = "src/arch/trap_entry.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
C = {
 # NC1 fatal queue-full propagation: a full queue refuses with EndpointQueueFull again
 "nc1": [(D, "        let bit = 1u64 << irq_line;\n        let newly_pending",
              "        if self.len >= self.max_depth {\n            return Err(KernelError::EndpointQueueFull);\n        }\n        let bit = 1u64 << irq_line;\n        let newly_pending")],
 # NC2 silent overflow loss: a full queue reports Coalesced but records nothing
 "nc2": [(D, "        self.overflow_pending |= bit;\n        Ok(IrqSignal::Coalesced { newly_pending })",
              "        let _ = bit;\n        Ok(IrqSignal::Coalesced { newly_pending })")],
 # NC3 missing pending-event publication: a receive never reports a pending line
 "nc3": [(D, "        } else if self.overflow_pending != 0 {\n            let line",
              "        } else if self.overflow_pending != 0 && false {\n            let line")],
 # NC4 a consumption clears every pending line, erasing the others
 "nc4": [(D, "            self.overflow_pending &= !(1u64 << line);", "            self.overflow_pending = 0;")],
 # NC5 stale-generation delivery (split): the route's generation is not checked
 "nc5": [(R, "            if ipc.notification_generations.get(notification_idx).copied()\n                != Some(route.notification.generation)\n",
              "            if ipc.notification_generations.get(notification_idx).copied()\n                != Some(route.notification.generation)\n                && false\n")],
 # NC6 missing controller completion for a settled single-step ending
 "nc6": [(SS, "    if settled.is_some() || completion.is_in_flight() {", "    if completion.is_in_flight() {")],
 # NC7 duplicate controller completion
 "nc7": [(SS, "        completion.complete(cpu);\n", "        completion.complete(cpu);\n        completion.complete(cpu);\n")],
 # NC8 the interrupted frame is written for a settled interrupt
 "nc8": [(TE, "            if let Some(result) = routed {\n                irq_handled = true;\n                irq_result = Some(result);\n            }",
              "            if let Some(result) = routed {\n                irq_handled = true;\n                irq_result = Some(result);\n                if let Some(f) = frame.as_deref_mut() {\n                    f.set_err(7);\n                }\n            }")],
 # NC9 saturation classified as an error at the outcome level
 "nc9": [(F, "            Self::Failed { error, .. } => Some(error),\n", "            Self::Failed { error, .. } => Some(error),\n            Self::Coalesced { .. } => Some(crate::kernel::boot::KernelError::EndpointQueueFull),\n")],
 # NC10 no coalescing mode: the queue is preferred again while lines are pending (starvation)
 "nc10": [(D, "        if self.overflow_pending == 0 && self.len < self.max_depth {", "        if self.len < self.max_depth {")],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(which, "applied")
