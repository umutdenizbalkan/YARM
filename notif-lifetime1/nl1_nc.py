#!/usr/bin/env python3
"""NOTIF-LIFETIME1 negative controls. Usage: nl1_nc.py <worktree> <ncN>

Each control removes ONE protection from the candidate; the hosted lifetime suites must fail.
"""
import pathlib
import sys

wt = pathlib.Path(sys.argv[1])
which = sys.argv[2]
LT = "src/kernel/boot/notification_lifetime.rs"
LC = "src/kernel/boot/capability_lifecycle_state.rs"


def patch(rel, old, new):
    p = wt / rel
    s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))


CONTROLS = {
    # NC1 — the destruction owner ignores the generation (index-only, the IRQ-GENERATION1 defect).
    "nc1": (LT, "        || ipc.notification_generations[i] != incarnation.generation\n        || ipc.notification_refs[i] == 0\n    {\n        return NotificationReleaseOutcome::Ignored;",
            "        || ipc.notification_refs[i] == 0\n    {\n        return NotificationReleaseOutcome::Ignored;"),
    # NC2 — destroy-on-any-revoke: any release takes the count straight to zero.
    "nc2": (LT, "    ipc.notification_refs[i] -= count;\n", "    let _ = count;\n    ipc.notification_refs[i] = 0;\n"),
    # NC3 — reservation ignores the generation: a stale publication counts against the successor.
    "nc3": (LT, "    if ipc.notification_generations[i] != incarnation.generation || ipc.notification_refs[i] == 0 {",
            "    if ipc.notification_refs[i] == 0 {"),
    # NC4 — the broad mint publishes without reserving.
    "nc4": (LC, "        self.reserve_notification_ref(capability.object)?;\n        if let Err(e) = self.ensure_cnode_space(cnode) {",
            "        if let Err(e) = self.ensure_cnode_space(cnode) {"),
    # NC5 — a failed broad mint keeps its reservation (leak).
    "nc5": (LC, "                self.release_notification_ref(capability.object);\n                return Err(e);\n            }\n        };\n        self.adjust_memory_object_cap_refcount(capability.object, 1);",
            "                return Err(e);\n            }\n        };\n        self.adjust_memory_object_cap_refcount(capability.object, 1);"),
    # NC6 — the whole-cspace reap drops capabilities without reporting their references.
    "nc6": None,
    # NC7 — the cspace revoke stops reporting what it removed.
    "nc7": ("src/kernel/capabilities.rs", "                on_removed(entry.capability);\n", "                let _ = (&mut on_removed, entry);\n"),
    # NC8 — broad delegation (and so fork and spawn) publishes without reserving.
    "nc8": ("src/kernel/boot/capability_state.rs", "        self.reserve_notification_ref(capability.object)?;\n        let grant = match", "        let grant = match"),
    # NC9 — the split mint seam publishes without reserving.
    "nc9": ("src/kernel/boot/cap_memory_mint_split.rs", "        self.reserve_notification_ref_split(capability.object)?;\n", ""),
    # NC10 — the split rollback gives the reference back even when it did not remove the slot.
    "nc10": ("src/kernel/boot/cap_memory_mint_split.rs", "        if cleared {\n            self.release_notification_ref_split(object);\n        }",
             "        let _ = cleared;\n        self.release_notification_ref_split(object);"),
    # NC11 — the ordering: the broad mint reserves only AFTER publishing.
    "nc11": (LC, "        self.reserve_notification_ref(capability.object)?;\n        if let Err(e) = self.ensure_cnode_space(cnode) {",
             "        if let Err(e) = self.ensure_cnode_space(cnode) {"),
}

if which not in CONTROLS:
    raise SystemExit("unknown control " + which)
if which == "nc6":
    p = wt / "src/kernel/boot/cnode_state.rs"
    src = p.read_text()
    at = src.index("pub(crate) fn reap_process_cspace_locked(")
    head, tail = src[:at], src[at:]
    assert tail.count("notification_refs.note(&held);") == 1
    p.write_text(head + tail.replace("notification_refs.note(&held);", "let _ = held;", 1))
else:
    rel, old, new = CONTROLS[which]
    patch(rel, old, new)
if which == "nc11":
    # Re-add the reservation after the slot is installed (the unsafe order).
    patch(LC, "        self.adjust_memory_object_cap_refcount(capability.object, 1);\n        Ok(minted)",
          "        self.reserve_notification_ref(capability.object)?;\n        self.adjust_memory_object_cap_refcount(capability.object, 1);\n        Ok(minted)")
print("applied", which)
