#!/usr/bin/env python3
"""IRQ-AUTH1 negative controls. Usage: ia1_nc.py <worktree> <ncN>
Each control removes ONE protection from the candidate; the hosted suites must fail."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
IB = "src/kernel/boot/irq_binding.rs"
NL = "src/kernel/boot/notification_lifetime.rs"
IPC = "src/kernel/boot/ipc_state.rs"
RT = "src/runtime.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
C = {
 # NC1 no IRQ capability check: any capability names line 9
 "nc1": [(IB, "    let CapObject::Irq { line } = irq.object else {\n        return Err(KernelError::WrongObject);\n    };",
              "    let line = match irq.object {\n        CapObject::Irq { line } => line,\n        _ => 9,\n    };")],
 # NC2 the route is published on a different line than the capability names
 "nc2": [(IB, "    let slot = &mut ipc.irq_routes[usize::from(line)];",
              "    let slot = &mut ipc.irq_routes[usize::from(line ^ 1)];")],
 # NC3 an occupied line is overwritten
 "nc3": [(IB, "        Some(current) if Some(current) != stale => Ok(IrqBindOutcome::Occupied(current)),\n", "")],
 # NC4 no generation check in the binding decision
 "nc4": [(IB, "    if ipc.notification_generations[i] != route.notification.generation {\n        return Err(KernelError::StaleCapability);\n    }\n    let slot", "    let slot")],
 # NC5 validation and mutation separated: the incarnation is checked in its own rank-3
 #     acquisition BEFORE the window, and the decision no longer checks it
 "nc5": [(IB, "        irq_line_is_bindable(request.line)?;\n        #[cfg(test)]\n        irq_bind_phase_hook::run_before_publish(self);",
              "        irq_line_is_bindable(request.line)?;\n        let pre = request.route.notification;\n        self.with_ipc_state(|ipc| {\n            if ipc.notifications[pre.index].is_none() {\n                return Err(KernelError::WrongObject);\n            }\n            if ipc.notification_generations[pre.index] != pre.generation {\n                return Err(KernelError::StaleCapability);\n            }\n            Ok(())\n        })?;\n        #[cfg(test)]\n        irq_bind_phase_hook::run_before_publish(self);"),
         (IB, "    let i = route.notification.index;\n    if i >= MAX_NOTIFICATIONS || ipc.notifications[i].is_none() {\n        return Err(KernelError::WrongObject);\n    }\n    if ipc.notification_generations[i] != route.notification.generation {\n        return Err(KernelError::StaleCapability);\n    }\n    let slot",
              "    let _ = MAX_NOTIFICATIONS;\n    let slot")],
 # NC6 removals no longer report IRQ lines (no owner revocation handling)
 "nc6": [(NL, "            self.irq_lines |= super::irq_binding::irq_line_bit(line);\n", "            let _ = line;\n")],
 # NC7 retraction clears whatever route the line holds (stale teardown hits a replacement)
 "nc7": [(IB, "    if *slot != Some(route) {\n        return false;\n    }\n    *slot = None;", "    let _ = route;\n    if slot.is_none() {\n        return false;\n    }\n    *slot = None;")],
 # NC8 kernel-owned lines are bindable
 "nc8": [(IB, "    if line < FIRST_DEVICE_IRQ_LINE {\n        return Err(KernelError::MissingRight);\n    }\n", "    let _ = FIRST_DEVICE_IRQ_LINE;\n")],
 # NC9 no post-publication owner recheck
 "nc9": [(IB, "                if self.with_capability_state(|capability| {\n                    irq_owner_live_locked(capability, route.owner, line)\n                }) {\n                    crate::yarm_log!(\n                        \"IRQ_ROUTE_BOUND",
              "                if true {\n                    crate::yarm_log!(\n                        \"IRQ_ROUTE_BOUND")],
 # NC10 broad delivery ignores the route's generation
 "nc10": [(IB, "    if ipc.notification_generations.get(i).copied() != Some(route.notification.generation) {\n        return Err(KernelError::WrongObject);\n    }\n", "")],
 # NC11 split delivery ignores the route's generation
 "nc11": [(RT, "            if ipc.notification_generations.get(notification_idx).copied()\n                != Some(route.notification.generation)\n",
               "            if ipc.notification_generations.get(notification_idx).copied()\n                != Some(route.notification.generation)\n                && false\n")],
 # NC12 an occupied line is reclaimed whatever its owner's liveness
 "nc12": [(IB, "        if let IrqBindOutcome::Occupied(current) = outcome\n            && !self.with_capability_state(|capability| {\n                irq_owner_live_locked(capability, current.owner, line)\n            })",
               "        if let IrqBindOutcome::Occupied(current) = outcome")],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(which, "applied")
