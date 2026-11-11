
/// ENDPOINT-ROLLBACK1 — provisional endpoint teardown never consults another object class.
///
/// `remove_unpublished_endpoint_locked` gives back the endpoint a failed spawn provisioned. An
/// Endpoint and a Notification have independent slot indexes and generations, and an IRQ route
/// names a Notification incarnation (IRQ-AUTH1), so a route can never refer to an Endpoint. Every
/// test here uses real objects: real Notifications bound to real lines through line authority,
/// real deliveries, a real blocked receiver, a real parked capability-bearing sender, and the real
/// spawn transaction.
mod endpoint_rollback1 {
    use super::*;
    use crate::kernel::boot::irq_binding::IrqRoute;
    use crate::kernel::boot::spawn_ipc_cap_txn::{EndpointRemoval, ServiceEndpointGrant};
    use crate::kernel::capabilities::{CapId, CapObject};
    use crate::kernel::syscall::spawn_image_txn::{
        SpawnImageRequest, SpawnImageSource, run_image_spawn_transaction,
    };
    use crate::kernel::syscall::spawn_txn::{
        BroadSpawnOwners, LinkClosure, ResolvedLinkPids, SharedSpawnOwners, SpawnTxnOwners,
        release_service_endpoint_grant,
    };
    // The owner-trait vocabulary, for the failure-injecting owner set below.
    use crate::kernel::boot::cow_clone::{CowCloneRollback, CowCloneToken};
    use crate::kernel::boot::exec_state::SpawnedImagePublication;
    use crate::kernel::boot::process_cnode_txn::{ProcessCNodeGrant, ProcessCNodeRequest};
    use crate::kernel::boot::provisional_cap::{ProvisionalCap, ProvisionalCapRelease};
    use crate::kernel::boot::spawn_image_provision::{ImageProvisionRequest, ProvisionToken};
    use crate::kernel::boot::spawn_ipc_cap_txn::{DelegationGrant, ServiceEndpointRequest};
    use crate::kernel::boot::RuntimeCapacityConfig;
    use crate::kernel::capabilities::{CNodeId, CapRights, Capability};
    use crate::kernel::spawn_reservation::{ReservationRefusal, SpawnBaseline, SpawnReservationToken};
    use crate::kernel::vm::{Asid, VirtAddr};
    use crate::kernel::task::{TaskClass, TaskStatus, WaitReason};
    use crate::runtime::SharedKernel;

    const LINE_A: u16 = 9;
    const LINE_B: u16 = 10;
    const LINE_C: u16 = 11;

    fn fixture() -> SharedKernel {
        let k = SharedKernel::new(Bootstrap::init().expect("init"));
        k.with(|s| {
            assert!(
                s.current_task_cnode().is_some(),
                "the spawner must own a cspace, or the grant is not minted for real"
            );
        });
        k
    }

    /// One service-endpoint grant, exactly as the spawn transaction requests it.
    fn grant(k: &SharedKernel) -> ServiceEndpointGrant {
        k.with(|s| {
            let cnode = s.current_task_cnode().expect("spawner cspace");
            s.provision_service_endpoint(&super::u9spawn1_sp3_spawn_ledger::endpoint_request(
                s, cnode,
            ))
            .expect("a grant")
        })
    }

    fn next_free_endpoint(s: &KernelState) -> usize {
        let limit = s.runtime_capacity_config().max_endpoints;
        s.with_ipc_state(|ipc| {
            ipc.endpoints[..limit]
                .iter()
                .position(Option::is_none)
                .expect("a free endpoint slot")
        })
    }

    /// A live Notification whose SLOT INDEX equals the index the next endpoint allocation takes.
    /// The two namespaces are independent, so this is arranged, never assumed: unrelated
    /// Notifications or filler endpoints move whichever cursor is behind.
    fn notification_at_next_endpoint_index(s: &mut KernelState) -> (usize, CapId, CapId) {
        for _ in 0..64 {
            let target = next_free_endpoint(s);
            let (idx, sig, recv) = s.create_notification(4).expect("a notification");
            if idx < target {
                continue;
            }
            while next_free_endpoint(s) < idx {
                s.create_endpoint(1).expect("a filler endpoint");
            }
            assert_eq!(next_free_endpoint(s), idx, "the indexes must coincide");
            return (idx, sig, recv);
        }
        panic!("could not line the two namespaces up");
    }

    /// Everything IRQ and Notification state a rollback must leave alone: the routes, every
    /// Notification incarnation and reference count, and every queued or pending event.
    #[allow(clippy::type_complexity)]
    fn irq_view(
        s: &KernelState,
    ) -> (
        alloc::vec::Vec<Option<IrqRoute>>,
        alloc::vec::Vec<u64>,
        alloc::vec::Vec<u32>,
        alloc::vec::Vec<Option<(usize, usize, u64, alloc::vec::Vec<u16>)>>,
    ) {
        s.with_ipc_state(|ipc| {
            (
                ipc.irq_routes.to_vec(),
                ipc.notification_generations.to_vec(),
                ipc.notification_refs.to_vec(),
                ipc.notifications
                    .iter()
                    .map(|n| {
                        n.as_ref()
                            .map(|n| (n.head, n.len, n.overflow_pending, n.irq_queue.to_vec()))
                    })
                    .collect(),
            )
        })
    }

    fn installed(s: &KernelState, g: &ServiceEndpointGrant) -> bool {
        s.with_ipc_state(|ipc| {
            ipc.endpoints[g.endpoint_index].is_some()
                && ipc.endpoint_generations[g.endpoint_index] == g.endpoint_generation
        })
    }

    fn release_broad(k: &SharedKernel, g: &ServiceEndpointGrant) -> EndpointRemoval {
        k.with(|s| release_service_endpoint_grant(&mut BroadSpawnOwners { kernel: s }, g))
    }

    fn release_split(k: &SharedKernel, g: &ServiceEndpointGrant) -> EndpointRemoval {
        let cpu = k.current_cpu_split_read();
        let tid = k.current_tid_split_read(cpu);
        let mut owners = SharedSpawnOwners {
            shared: k,
            spawner_tid: tid,
            spawner_cnode: tid.and_then(|t| k.task_cnode_split(t)),
            cpu,
        };
        release_service_endpoint_grant(&mut owners, g)
    }

    fn deliver(k: &SharedKernel, line: u16) {
        let outcome = k.deliver_external_irq_split(CpuId(0), line);
        assert_eq!(outcome.marker(), "delivered", "line {line}: {outcome:?}");
    }

    fn drain(k: &SharedKernel, recv: CapId) -> alloc::vec::Vec<u16> {
        let mut seen = alloc::vec::Vec::new();
        k.with(|s| {
            while let Some(msg) = s.try_ipc_recv(recv).expect("receive") {
                seen.push(msg.opcode);
                assert!(seen.len() <= 64, "bounded");
            }
        });
        seen
    }

    /// The shared body of E1: a route to a Notification at the endpoint's own index, with a
    /// pending event, then the release through `release`.
    fn same_index_collision(release: fn(&SharedKernel, &ServiceEndpointGrant) -> EndpointRemoval) {
        let k = fixture();
        let (nidx, sig, recv) = k.with(|s| {
            let n = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_A, n.1).expect("bind through line authority");
            n
        });
        let _ = sig;
        deliver(&k, LINE_A);
        let g = grant(&k);
        assert_eq!(
            g.endpoint_index, nidx,
            "the collision must be real, or this proves nothing"
        );
        let irq_before = k.with(|s| irq_view(s));
        let removal = release(&k, &g);
        assert_eq!(
            removal,
            EndpointRemoval::Removed,
            "a route to a Notification is not a reference to an Endpoint"
        );
        k.with(|s| {
            assert!(!installed(s, &g), "the provisional endpoint is gone");
            assert!(
                s.with_ipc_state(|ipc| ipc.endpoints[g.endpoint_index].is_none()
                    && ipc.endpoint_generations[g.endpoint_index] > g.endpoint_generation),
                "its slot is free and its incarnation advanced"
            );
            assert_eq!(
                irq_view(s),
                irq_before,
                "the Notification, its generation, references, route and events are untouched"
            );
        });
        assert_eq!(
            drain(&k, recv),
            alloc::vec![LINE_A],
            "the pending event is still delivered"
        );
    }

    /// E1 — the confirmed collision, through the broad adapter.
    #[test]
    fn e1_a_same_index_notification_route_does_not_pin_an_unpublished_endpoint() {
        same_index_collision(release_broad);
    }

    /// E1 — the same collision through the split adapter: both reach the same rank-3 body.
    #[test]
    fn e1_split_a_same_index_notification_route_does_not_pin_an_unpublished_endpoint() {
        same_index_collision(release_split);
    }

    /// E2 — a route to a Notification at a DIFFERENT index (control).
    #[test]
    fn e2_a_route_at_another_index_never_mattered() {
        let k = fixture();
        let g = grant(&k);
        k.with(|s| {
            let (idx, sig, _recv) = loop {
                let n = s.create_notification(4).expect("a notification");
                if n.0 != g.endpoint_index {
                    break n;
                }
            };
            assert_ne!(idx, g.endpoint_index);
            s.bind_irq_line_for_test(LINE_B, sig).expect("bind");
        });
        let irq_before = k.with(|s| irq_view(s));
        assert_eq!(release_broad(&k, &g), EndpointRemoval::Removed);
        k.with(|s| {
            assert!(!installed(s, &g));
            assert_eq!(irq_view(s), irq_before);
        });
    }

    /// E3 — a legitimately blocked receiver still pins the endpoint, with or without a
    /// colliding route, and the refusal leaves the waiter exactly where it was.
    #[test]
    fn e3_a_blocked_receiver_still_refuses_the_removal() {
        let k = fixture();
        k.with(|s| {
            let n = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_A, n.1).expect("bind");
        });
        let g = grant(&k);
        let receiver = k.with(|s| s.current_tid().expect("current"));
        k.with(|s| {
            let got = s.ipc_recv(g.recv_cap);
            assert!(
                matches!(got, Ok(None) | Err(KernelError::WouldBlock)),
                "an empty endpoint parks the receiver: {got:?}"
            );
        });
        let (status, waiter) = k.with(|s| {
            (
                s.task_status(receiver),
                s.with_ipc_state(|ipc| ipc.endpoint_waiters[g.endpoint_index]),
            )
        });
        assert!(
            matches!(status, Some(TaskStatus::Blocked(WaitReason::EndpointReceive(_)))),
            "the receiver is blocked for real: {status:?}"
        );
        let waiter = waiter.expect("its waiter is published");
        let irq_before = k.with(|s| irq_view(s));
        let removal = k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                crate::kernel::boot::spawn_ipc_cap_txn::remove_unpublished_endpoint_locked(
                    ipc,
                    g.endpoint_index,
                    g.endpoint_generation,
                )
            })
        });
        assert_eq!(removal, EndpointRemoval::Published);
        k.with(|s| {
            assert!(installed(s, &g), "the endpoint is still installed");
            assert_eq!(
                s.with_ipc_state(|ipc| ipc.endpoint_waiters[g.endpoint_index]),
                Some(waiter),
                "the waiter is untouched"
            );
            assert!(matches!(
                s.task_status(receiver),
                Some(TaskStatus::Blocked(WaitReason::EndpointReceive(_)))
            ));
            assert_eq!(irq_view(s), irq_before);
        });
    }

    /// E4 — a legitimately parked sender, carrying a transferred capability, still pins the
    /// endpoint; its message and its live transfer envelope are untouched by the refusal.
    #[test]
    fn e4_a_parked_capability_sender_still_refuses_the_removal() {
        let k = fixture();
        let g = grant(&k);
        let (sender, handle) = k.with(|s| {
            s.set_timer_for_test(Timer::new(1));
            let tid = s.current_tid().expect("current");
            let (_c, carried, _cr) = s.create_endpoint(2).expect("a carried capability");
            // Fill the buffered queue, then park one more send that carries a capability,
            // through the real IpcSend syscall.
            let queued = |s: &KernelState| {
                s.with_ipc_state(|ipc| {
                    ipc.endpoints[g.endpoint_index].as_ref().map(|e| {
                        let e = crate::kernel::boot::kernel_ref(e);
                        (e.queued(), e.capacity())
                    })
                })
            };
            while let Some((n, cap)) = queued(s) {
                if n >= cap {
                    break;
                }
                s.ipc_send(g.send_cap, Message::new(tid, b"fill").expect("m"))
                    .expect("fill");
            }
            assert!(matches!(queued(s), Some((n, cap)) if n == cap), "the queue is full");
            let mut frame = TrapFrame::new(
                crate::kernel::syscall::Syscall::IpcSend as usize,
                [g.send_cap.0 as usize, 0, 0, 0, 100, carried.0 as usize],
            );
            s.handle_trap(Trap::Syscall, Some(&mut frame)).expect("park");
            assert!(
                matches!(
                    s.task_status(tid),
                    Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))
                ),
                "the capability send is parked for real"
            );
            let handle = s
                .with_ipc_state(|ipc| ipc.endpoint_sender_waiters[g.endpoint_index][0])
                .and_then(|w| w.msg.transferred_cap())
                .expect("the parked waiter carries the transfer");
            (tid, handle)
        });
        let envelope_live = |s: &KernelState| {
            let idx = (handle.0 & 0xFFFF) as usize;
            s.with_ipc_state(|ipc| {
                ipc.transfer_envelopes[idx].is_some()
                    && ipc.transfer_envelope_generations[idx] == (handle.0 >> 16)
            })
        };
        let waiters_before =
            k.with(|s| s.with_ipc_state(|ipc| ipc.endpoint_sender_waiters[g.endpoint_index]));
        assert!(k.with(|s| envelope_live(s)), "the envelope is live while parked");
        let removal = k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                crate::kernel::boot::spawn_ipc_cap_txn::remove_unpublished_endpoint_locked(
                    ipc,
                    g.endpoint_index,
                    g.endpoint_generation,
                )
            })
        });
        assert_eq!(removal, EndpointRemoval::Published);
        k.with(|s| {
            assert!(installed(s, &g), "the endpoint is still installed");
            assert_eq!(
                s.with_ipc_state(|ipc| ipc.endpoint_sender_waiters[g.endpoint_index]),
                waiters_before,
                "the parked sender and its message are untouched"
            );
            assert!(envelope_live(s), "and its transfer envelope is still live");
            assert!(matches!(
                s.task_status(sender),
                Some(TaskStatus::Blocked(WaitReason::EndpointSend(_)))
            ));
        });
    }

    /// E5 — a stale token whose slot now holds a replacement endpoint is refused as `Stale`, not
    /// `Published`, even with a route at that index, and the replacement is untouched.
    #[test]
    fn e5_a_stale_token_never_destroys_the_replacement() {
        let k = fixture();
        k.with(|s| {
            let n = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_A, n.1).expect("bind");
        });
        let first = grant(&k);
        assert_eq!(release_broad(&k, &first), EndpointRemoval::Removed);
        let second = grant(&k);
        assert_eq!(second.endpoint_index, first.endpoint_index, "the slot is recycled");
        assert_ne!(second.endpoint_generation, first.endpoint_generation);
        let removal = k.with(|s| {
            s.with_ipc_state_mut(|ipc| {
                crate::kernel::boot::spawn_ipc_cap_txn::remove_unpublished_endpoint_locked(
                    ipc,
                    first.endpoint_index,
                    first.endpoint_generation,
                )
            })
        });
        assert_eq!(removal, EndpointRemoval::Stale);
        k.with(|s| assert!(installed(s, &second), "the replacement is untouched"));
        assert_eq!(release_broad(&k, &second), EndpointRemoval::Removed);
    }

    /// E6 — repeated provision/rollback cycles on the colliding slot: each releases the slot, the
    /// next reuses it under a new generation, and the old capabilities resolve to nothing.
    #[test]
    fn e6_repeated_cycles_release_and_reuse_the_slot() {
        let k = fixture();
        let (_nidx, _sig, recv) = k.with(|s| {
            let n = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_A, n.1).expect("bind");
            n
        });
        let live_before =
            k.with(|s| s.with_ipc_state(|ipc| ipc.endpoints.iter().flatten().count()));
        deliver(&k, LINE_A);
        let mut previous: Option<ServiceEndpointGrant> = None;
        for round in 0..6 {
            let g = grant(&k);
            if let Some(p) = previous {
                assert_eq!(g.endpoint_index, p.endpoint_index, "round {round}: slot reused");
                assert!(
                    g.endpoint_generation > p.endpoint_generation,
                    "round {round}: a new incarnation"
                );
            }
            assert_eq!(release_broad(&k, &g), EndpointRemoval::Removed, "round {round}");
            k.with(|s| {
                assert!(
                    s.resolve_endpoint_index(CapObject::Endpoint {
                        index: g.endpoint_index,
                        generation: g.endpoint_generation,
                    })
                    .is_err(),
                    "round {round}: the removed incarnation resolves to nothing"
                );
            });
            assert_eq!(
                k.with(|s| s.with_ipc_state(|ipc| ipc.endpoints.iter().flatten().count())),
                live_before,
                "round {round}: no endpoint capacity is retained"
            );
            previous = Some(g);
        }
        assert_eq!(drain(&k, recv), alloc::vec![LINE_A], "the event survived every cycle");
    }

    /// E7 — several authorized routes to live Notifications, one of them at the endpoint's own
    /// index: none pins the unrelated endpoint, and none changes.
    #[test]
    fn e7_multiple_routes_pin_nothing() {
        let k = fixture();
        k.with(|s| {
            let n = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_A, n.1).expect("bind A");
            let (_i2, sig2, _r2) = s.create_notification(4).expect("n2");
            s.bind_irq_line_for_test(LINE_B, sig2).expect("bind B");
            s.bind_irq_line_for_test(LINE_C, n.1).expect("bind C to the colliding one too");
        });
        deliver(&k, LINE_B);
        let g = grant(&k);
        let routes = k.with(|s| s.with_ipc_state(|ipc| ipc.irq_routes.iter().flatten().count()));
        assert!(routes >= 3, "three routes are live");
        let irq_before = k.with(|s| irq_view(s));
        assert_eq!(release_broad(&k, &g), EndpointRemoval::Removed);
        k.with(|s| {
            assert!(!installed(s, &g));
            assert_eq!(irq_view(s), irq_before);
        });
    }


    /// A test-only owner set: the real broad owners for every step, except that one commit-phase
    /// step (`bind_spawned_task_asid`) fails, as an owner is allowed to. The production
    /// transaction runs unmodified and unwinds through its own ledger.
    struct FailingCommitOwners<'a> {
        inner: BroadSpawnOwners<'a>,
    }

    impl SpawnTxnOwners for FailingCommitOwners<'_> {
        fn current_cpu(&self) -> CpuId {
            self.inner.current_cpu()
        }
        fn current_tid(&self) -> Option<u64> {
            self.inner.current_tid()
        }
        fn current_task_cnode(&self) -> Option<CNodeId> {
            self.inner.current_task_cnode()
        }
        fn capacity_limits(&self) -> RuntimeCapacityConfig {
            self.inner.capacity_limits()
        }
        fn task_status(&self, tid: u64) -> Option<TaskStatus> {
            self.inner.task_status(tid)
        }
        fn live_task_count(&self) -> usize {
            self.inner.live_task_count()
        }
        fn allocate_thread_id(&mut self) -> Result<u64, KernelError> {
            self.inner.allocate_thread_id()
        }
        fn stamp_spawn_generation(&mut self) -> u64 {
            self.inner.stamp_spawn_generation()
        }
        fn insert_reservation( &mut self, tid: u64, reservation: crate::kernel::task::SpawnReservation) -> Option<usize> {
            self.inner.insert_reservation(tid, reservation)
        }
        fn clear_reservation_at(&mut self, index: usize) {
            self.inner.clear_reservation_at(index)
        }
        fn provision_default_kernel_context(&mut self, tid: u64) -> Result<(), KernelError> {
            self.inner.provision_default_kernel_context(tid)
        }
        fn release_kernel_context(&mut self, tid: u64) {
            self.inner.release_kernel_context(tid)
        }
        fn validate_cancellable( &mut self, token: &SpawnReservationToken) -> Result<(usize, u64), ReservationRefusal> {
            self.inner.validate_cancellable(token)
        }
        fn claim_reservation( &mut self, token: &SpawnReservationToken) -> Result<SpawnBaseline, ReservationRefusal> {
            self.inner.claim_reservation(token)
        }
        fn restore_after_failed_spawn( &mut self, token: &SpawnReservationToken, baseline: SpawnBaseline) -> bool {
            self.inner.restore_after_failed_spawn(token, baseline)
        }
        fn bind_spawned_task_asid(&mut self, tid: u64, asid: Asid) -> Result<(), KernelError> {
            let _ = (tid, asid);
            // The injected failure: a commit-phase step after both service endpoints exist.
            Err(KernelError::WrongObject)
        }
        fn publish_spawned_image( &mut self, reservation: &SpawnReservationToken, publication: &SpawnedImagePublication) -> Result<(), ReservationRefusal> {
            self.inner.publish_spawned_image(reservation, publication)
        }
        fn live_tcb_count_for(&self, tid: u64) -> usize {
            self.inner.live_tcb_count_for(tid)
        }
        fn is_zombie(&self, tid: u64) -> bool {
            self.inner.is_zombie(tid)
        }
        fn thread_kernel_stack_top(&self, tid: u64) -> u64 {
            self.inner.thread_kernel_stack_top(tid)
        }
        fn thread_kernel_context_initialized(&self, tid: u64) -> bool {
            self.inner.thread_kernel_context_initialized(tid)
        }
        fn initialize_thread_kernel_switch_frame( &mut self, tid: u64, entry: usize) -> Result<(), KernelError> {
            self.inner.initialize_thread_kernel_switch_frame(tid, entry)
        }
        fn enqueue_on_cpu(&mut self, cpu: CpuId, tid: u64) -> Result<CpuId, KernelError> {
            self.inner.enqueue_on_cpu(cpu, tid)
        }
        fn enqueue_balanced(&mut self, tid: u64) -> Result<CpuId, KernelError> {
            self.inner.enqueue_balanced(tid)
        }
        fn provision_process_cnode( &mut self, request: &ProcessCNodeRequest) -> Result<ProcessCNodeGrant, KernelError> {
            self.inner.provision_process_cnode(request)
        }
        fn release_process_cnode_grant( &mut self, request: &ProcessCNodeRequest, grant: &ProcessCNodeGrant) {
            self.inner.release_process_cnode_grant(request, grant)
        }
        fn reap_process_cnode_if_unused(&mut self, pid: u64) -> bool {
            self.inner.reap_process_cnode_if_unused(pid)
        }
        fn task_cnode(&self, tid: u64) -> Option<CNodeId> {
            self.inner.task_cnode(tid)
        }
        fn set_process_cnode_for_pid(&mut self, pid: u64, cnode: CNodeId) -> Result<(), KernelError> {
            self.inner.set_process_cnode_for_pid(pid, cnode)
        }
        fn mint_capability_in_cnode( &mut self, cnode: CNodeId, capability: Capability) -> Result<CapId, KernelError> {
            self.inner.mint_capability_in_cnode(cnode, capability)
        }
        fn provisional_cap_token( &mut self, cnode: CNodeId, cap: CapId) -> Result<ProvisionalCap, KernelError> {
            self.inner.provisional_cap_token(cnode, cap)
        }
        fn collect_cap_link_closure(&mut self, cap: CapId) -> Option<(LinkClosure, usize)> {
            self.inner.collect_cap_link_closure(cap)
        }
        fn resolve_link_pids(&self, links: &LinkClosure) -> ResolvedLinkPids {
            self.inner.resolve_link_pids(links)
        }
        fn release_provisional_cap( &mut self, token: &ProvisionalCap, links: &LinkClosure, resolved: &ResolvedLinkPids) -> ProvisionalCapRelease {
            self.inner.release_provisional_cap(token, links, resolved)
        }
        fn delegate_capability( &mut self, source_tid: u64, source_cap: CapId, dest_tid: u64, rights: CapRights) -> Result<DelegationGrant, KernelError> {
            self.inner.delegate_capability(source_tid, source_cap, dest_tid, rights)
        }
        fn release_delegation(&mut self, grant: &DelegationGrant) -> bool {
            self.inner.release_delegation(grant)
        }
        fn provision_service_endpoint( &mut self, request: &ServiceEndpointRequest) -> Result<ServiceEndpointGrant, KernelError> {
            self.inner.provision_service_endpoint(request)
        }
        fn remove_unpublished_endpoint(&mut self, index: usize, generation: u64) -> EndpointRemoval {
            self.inner.remove_unpublished_endpoint(index, generation)
        }
        fn provision_image( &mut self, request: &ImageProvisionRequest<'_>) -> Result<ProvisionToken, KernelError> {
            self.inner.provision_image(request)
        }
        fn rollback_provision(&mut self, asid: Asid, phase: &'static str, err: KernelError) {
            self.inner.rollback_provision(asid, phase, err)
        }
        fn address_space_exists(&self, asid: Asid) -> bool {
            self.inner.address_space_exists(asid)
        }
        fn destroy_unresident_address_space(&mut self, asid: Asid) -> bool {
            self.inner.destroy_unresident_address_space(asid)
        }
        fn destroy_live_address_space(&mut self, asid: Asid) -> bool {
            self.inner.destroy_live_address_space(asid)
        }
        fn asid_carrier_tid(&self, asid: Asid) -> Option<u64> {
            self.inner.asid_carrier_tid(asid)
        }
        fn allocate_user_stack_with_guard( &mut self, tid: u64, pages: usize) -> Result<VirtAddr, KernelError> {
            self.inner.allocate_user_stack_with_guard(tid, pages)
        }
        fn copy_to_user(&mut self, asid: Asid, va: VirtAddr, bytes: &[u8]) -> Result<(), KernelError> {
            self.inner.copy_to_user(asid, va, bytes)
        }
        fn fork_parent_snapshot(&mut self, tid: u64) -> Option<crate::kernel::syscall::fork_txn::ForkParentFacts> {
            self.inner.fork_parent_snapshot(tid)
        }
        fn snapshot_inheritable_caps( &self, tid: u64) -> Result<crate::kernel::syscall::fork_txn::InheritableCaps, KernelError> {
            self.inner.snapshot_inheritable_caps(tid)
        }
        fn clone_address_space_cow( &mut self, parent_asid: Asid, generation: u64) -> Result<CowCloneToken, KernelError> {
            self.inner.clone_address_space_cow(parent_asid, generation)
        }
        fn complete_cow_shootdown( &mut self, plan: &crate::kernel::boot::cow_clone::CowShootdownPlan) -> bool {
            self.inner.complete_cow_shootdown(plan)
        }
        fn rollback_cow_clone(&mut self, token: &CowCloneToken) -> CowCloneRollback {
            self.inner.rollback_cow_clone(token)
        }
        fn publish_forked_child( &mut self, reservation: &SpawnReservationToken, publication: &crate::kernel::syscall::fork_txn::ForkChildPublication) -> Result<(), crate::kernel::spawn_reservation::ReservationRefusal> {
            self.inner.publish_forked_child(reservation, publication)
        }
        fn reserved_cnode_slot_total(&self) -> usize {
            self.inner.reserved_cnode_slot_total()
        }
        fn cnode_capacity_breakdown(&self) -> alloc::vec::Vec<(u64, usize, usize)> {
            self.inner.cnode_capacity_breakdown()
        }
        fn remove_published_fork_child(&mut self, tid: u64) -> bool {
            self.inner.remove_published_fork_child(tid)
        }
    }

    /// The real spawn transaction, failing at its commit — after both service endpoints and
    /// their capabilities are provisioned — through the failure-injecting owner set.
    fn failing_spawn(k: &SharedKernel, elf: &[u8]) {
        let err = k.with(|s| {
            run_image_spawn_transaction(
                &mut FailingCommitOwners {
                    inner: BroadSpawnOwners { kernel: s },
                },
                SpawnImageRequest {
                    image_id: 0,
                    image_path: "init",
                    source: SpawnImageSource::PtLoadSegments {
                        elf,
                        entry: 0x40_0000,
                    },
                    class: TaskClass::SystemServer,
                    parent_pid: 0,
                    startup_args: [0u64; 18],
                    extra_send_caps: [0u64; 4],
                    map_initrd_window: false,
                    lifecycle_markers: false,
                },
            )
            .map(|c| c.tid)
        });
        assert!(err.is_err(), "the commit must fail: {err:?}");
    }

    /// What a failed spawn must give back.
    fn spawn_view(s: &KernelState) -> (usize, alloc::vec::Vec<usize>, usize, usize) {
        let cnode = s.current_task_cnode();
        (
            s.with_tcbs(|tcbs| tcbs.iter().flatten().count()),
            s.with_ipc_state(|ipc| {
                (0..ipc.endpoints.len())
                    .filter(|i| ipc.endpoints[*i].is_some())
                    .collect()
            }),
            match cnode {
                Some(cnode) => s.with_capability_state(|cap| {
                    cap.cnode_spaces
                        .iter()
                        .flatten()
                        .filter(|space| space.id == cnode)
                        .map(|space| crate::kernel::boot::kernel_ref(&space.cspace).occupied_slots())
                        .sum::<usize>()
                }),
                None => 0,
            },
            s.with_memory_state(|m| m.frame_allocator.free_frames()),
        )
    }

    /// E8 — the real spawn rollback, with routes to Notifications at the indexes both service
    /// endpoints take. Every provisional resource comes back, repeatedly, so no endpoint capacity
    /// is retained; the capabilities still go back before the endpoint; no IRQ state changes.
    #[test]
    fn e8_a_failed_spawn_gives_back_its_endpoints_despite_colliding_routes() {
        let k = fixture();
        let elf = super::u9spawn1_sp3_spawn_ledger::tiny_elf();
        let (first_idx, second_idx) = k.with(|s| {
            let a = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_A, a.1).expect("bind A");
            // Reserve `a.0` as an endpoint just long enough to line the second Notification up
            // with the slot after it, then give it back.
            let (filler, _fs, _fr) = s.create_endpoint(1).expect("filler");
            assert_eq!(filler, a.0);
            let b = notification_at_next_endpoint_index(s);
            s.bind_irq_line_for_test(LINE_B, b.1).expect("bind B");
            s.destroy_endpoint(filler).expect("give the filler back");
            assert_eq!(next_free_endpoint(s), a.0);
            (a.0, b.0)
        });
        let before = k.with(|s| spawn_view(s));
        let irq_before = k.with(|s| irq_view(s));
        for round in 0..4 {
            failing_spawn(&k, &elf);
            let after = k.with(|s| spawn_view(s));
            assert_eq!(
                after.1, before.1,
                "round {round}: the provisional endpoints at {first_idx} and {second_idx} were retained"
            );
            assert_eq!(
                after, before,
                "round {round}: every provisional resource comes back"
            );
            k.with(|s| assert_eq!(irq_view(s), irq_before, "round {round}: IRQ state moved"));
        }
        // The capabilities are still released before the endpoint (the policy's order).
        let policy = include_str!("../syscall/spawn_txn.rs");
        let body = policy
            .split("pub(crate) fn release_service_endpoint_grant<O: SpawnTxnOwners>(")
            .nth(1)
            .expect("the release");
        let send = body.find("grant.send_cap").expect("send release");
        let recv = body.find("grant.recv_cap").expect("recv release");
        let ep = body.find("remove_unpublished_endpoint(").expect("endpoint removal");
        assert!(send < ep && recv < ep, "capabilities first, then the endpoint");
    }
}
