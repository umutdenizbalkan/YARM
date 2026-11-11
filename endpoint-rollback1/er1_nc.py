#!/usr/bin/env python3
"""ENDPOINT-ROLLBACK1 negative controls. Usage: er1_nc.py <worktree> <ncN>
Each control reintroduces ONE defect into the candidate's removal body."""
import pathlib, sys
wt = pathlib.Path(sys.argv[1]); which = sys.argv[2]
T = "src/kernel/boot/spawn_ipc_cap_txn.rs"
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
GUARD = '''    if ipc.endpoint_waiters[index].is_some()
        || ipc.endpoint_sender_waiters[index]
            .iter()
            .any(Option::is_some)
    {'''
C = {
 # NC1 the index-space comparison restored
 "nc1": [(T, GUARD, '''    if ipc.endpoint_waiters[index].is_some()
        || ipc.endpoint_sender_waiters[index]
            .iter()
            .any(Option::is_some)
        || ipc
            .irq_routes
            .iter()
            .flatten()
            .any(|route| route.notification.index == index)
    {''')],
 # NC2 the endpoint generation check removed (an occupied slot is enough)
 "nc2": [(T, '''    if ipc.endpoints.get(index).map(Option::is_some) != Some(true)
        || ipc.endpoint_generations.get(index).copied() != Some(generation)
    {''', '''    if ipc.endpoints.get(index).map(Option::is_some) != Some(true) {''')],
 # NC3 the receiver-waiter guard removed
 "nc3": [(T, GUARD, '''    if ipc.endpoint_sender_waiters[index]
        .iter()
        .any(Option::is_some)
    {''')],
 # NC4 the sender-waiter guard removed
 "nc4": [(T, GUARD, '''    if ipc.endpoint_waiters[index].is_some() {''')],
 # NC5 the rollback disturbs the IRQ route at the same index (an over-reaching "fix")
 "nc5": [(T, '''    ipc.endpoints[index] = None;
    let mut next = generation.wrapping_add(1);''', '''    for route in ipc.irq_routes.iter_mut() {
        if route.is_some_and(|r| r.notification.index == index) {
            *route = None;
        }
    }
    ipc.endpoints[index] = None;
    let mut next = generation.wrapping_add(1);''')],
 # NC6 the generation is not advanced on removal
 "nc6": [(T, '''    ipc.endpoint_generations[index] = next;
    EndpointRemoval::Removed''', '''    let _ = next;
    EndpointRemoval::Removed''')],
}
for rel, old, new in C[which]:
    patch(rel, old, new)
print(f"{which} applied")
