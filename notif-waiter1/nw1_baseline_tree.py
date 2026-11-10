#!/usr/bin/env python3
"""Build the NOTIF-WAITER1 baseline-reproducer tree: <base worktree at dc5c8fd> + the test-only
phase hook + the (production-unused) owner module + F's new tests. No production behaviour of the
base changes. Usage: nw1_baseline_tree.py <base_wt> <candidate_wt>"""
import pathlib, sys
b = pathlib.Path(sys.argv[1]); f = pathlib.Path(sys.argv[2])
def rd(p): return p.read_text()
ipc = b / "src/kernel/boot/ipc_state.rs"; s = rd(ipc)
anchor = "            // Publish waiter under ipc_state_lock after TCB is Blocked.\n"
assert s.count(anchor) == 1
s = s.replace(anchor, "            #[cfg(test)]\n            notif_recv_phase_hook::run_before_publish(self);\n" + anchor)
fs = rd(f / "src/kernel/boot/ipc_state.rs")
hook = fs[fs.index("\n#[cfg(test)]\npub(crate) mod notif_recv_phase_hook {"):]
s = s.rstrip("\n") + "\n" + hook
ipc.write_text(s)
(b / "src/kernel/boot/notification_waiter.rs").write_text(rd(f / "src/kernel/boot/notification_waiter.rs"))
m = b / "src/kernel/boot/mod.rs"; ms = rd(m)
a = "pub(crate) mod notification_lifetime;\n"; assert ms.count(a) == 1
m.write_text(ms.replace(a, a + "#[allow(dead_code)]\npub(crate) mod notification_waiter;\n"))
t = b / "src/kernel/boot/tests.rs"; ts = rd(t); ft = rd(f / "src/kernel/boot/tests.rs")
start = ft.index("/// NOTIF-WAITER1 — ") if "/// NOTIF-WAITER1 — " in ft else None
mod_at = ft.index("\nmod notif_waiter1 {")
# include the doc comment block immediately preceding the module
doc_at = ft.rfind("\n\n", 0, mod_at)
t.write_text(ts.rstrip("\n") + "\n" + ft[doc_at:])
# the exit companion test (other-asid record must survive exit)
comp_at = ft.index("/// NOTIF-WAITER1 companion: exit removes")
comp_end = ft.index("\n}\n", comp_at) + 3
t.write_text(rd(t) + "\n" + ft[comp_at:comp_end])
print("baseline tree built")
