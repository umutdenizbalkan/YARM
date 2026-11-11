#!/usr/bin/env python3
"""ENDPOINT-ROLLBACK1: re-derive the two existing tests that encoded the index-space confusion.
Usage: er1_testfix.py <worktree>"""
import pathlib, sys
wt = pathlib.Path(sys.argv[1])
def patch(rel, old, new):
    p = wt / rel; s = p.read_text()
    assert s.count(old) == 1, (rel, old[:70], s.count(old))
    p.write_text(s.replace(old, new))
T = "src/kernel/boot/tests.rs"

# 1. The source guard listed `irq_routes` as a publication surface the removal must consult.
patch(T, '''    /// §2: the unpublished-endpoint removal checks all three publication surfaces and the
    /// incarnation, and the LIVE teardown keeps the wake/settle work it owes.
    #[test]
    fn the_unpublished_removal_refuses_rather_than_guesses() {
        let removal = body(TXN, "pub(crate) fn remove_unpublished_endpoint_locked(");
        for surface in [
            "endpoint_generations",
            "endpoint_waiters",
            "endpoint_sender_waiters",
            "irq_routes",
        ] {
            assert!(
                removal.contains(surface),
                "the removal must consult {surface} before clearing a slot"
            );
        }''', '''    /// §2: the unpublished-endpoint removal checks the endpoint's own publication surfaces and
    /// the incarnation, and the LIVE teardown keeps the wake/settle work it owes.
    ///
    /// ENDPOINT-ROLLBACK1 re-derived this: `irq_routes` used to be listed as a surface, but a
    /// route names a Notification, so consulting it compared two namespaces. The behaviour is
    /// proved by `endpoint_rollback1`; this guard only pins the surfaces.
    #[test]
    fn the_unpublished_removal_refuses_rather_than_guesses() {
        let removal = body(TXN, "pub(crate) fn remove_unpublished_endpoint_locked(");
        for surface in [
            "endpoint_generations",
            "endpoint_waiters",
            "endpoint_sender_waiters",
        ] {
            assert!(
                removal.contains(surface),
                "the removal must consult {surface} before clearing a slot"
            );
        }
        for foreign in ["irq_routes", "notification"] {
            assert!(
                !removal.contains(foreign),
                "the endpoint removal must not consult another object class ({foreign})"
            );
        }''')

# 2. The behavioural test "published" the endpoint by forcing an IRQ route whose NOTIFICATION
#    index equals the endpoint index — exactly the confused case. It now publishes it for real,
#    with a blocked receiver.
patch(T, '''        // Publish it the way an IRQ binding would: route a line at this endpoint.
        k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                ipc.irq_routes[0] = Some(crate::kernel::boot::irq_binding::forced_route_for_test(
                    grant.endpoint_index,
                    0,
                ));
            })
        });
        let removal = k.with(|s| {''', '''        // Publish it for real: the capability holder blocks receiving on it, so a waiter names it.
        // (ENDPOINT-ROLLBACK1: this used to force an IRQ route whose NOTIFICATION index equalled
        // the endpoint index, which publishes nothing — a route names a Notification.)
        k.with(|s| {
            let _ = s.ipc_recv(grant.recv_cap);
            assert!(
                s.with_ipc_state(|ipc| ipc.endpoint_waiters[grant.endpoint_index].is_some()),
                "the receiver's waiter is published"
            );
        });
        let removal = k.with(|s| {''')
patch(T, '''        assert!(
            k.with(|s| s.with_ipc_state(|ipc| ipc.endpoints[grant.endpoint_index].is_some())),
            "and it must still be installed"
        );
        k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                ipc.irq_routes[0] = None;
            })
        });
    }''', '''        assert!(
            k.with(|s| s.with_ipc_state(|ipc| ipc.endpoints[grant.endpoint_index].is_some())),
            "and it must still be installed"
        );
        assert!(
            k.with(|s| s.with_ipc_state(|ipc| ipc.endpoint_waiters[grant.endpoint_index].is_some())),
            "with its waiter untouched"
        );
    }''')
print("testfix applied")
