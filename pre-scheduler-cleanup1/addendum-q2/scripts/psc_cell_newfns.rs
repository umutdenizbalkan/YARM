    /// SCHED-SAT-HANDOFF1 — one pass over the parked spares: wake each in turn; a wake places the
    /// spare on its home CPU (filling a slot there) or is refused because that home is full (the
    /// spare stays parked). Returns how many were placed.
    fn spare_pass() -> u32 {
        let mut placed = 0;
        for k in 0..SPARES {
            if SPARE_STATE[k]
                .compare_exchange(0, 2, SeqCst, SeqCst)
                .is_err()
            {
                continue;
            }
            SPARE_GO[k].store(1, SeqCst);
            match futex_wake(SPARE_GO[k].as_ptr(), 1, P) {
                Ok(n) if n >= 1 => {
                    SPARE_STATE[k].store(1, SeqCst);
                    SPARES_PLACED.fetch_add(1, SeqCst);
                    placed += 1;
                }
                _ => SPARE_STATE[k].store(0, SeqCst),
            }
        }
        placed
    }

    /// The hold's spare pass, at most every `TOPUP_EVERY_NS` (every `SPARE_IDLE_EVERY_NS` after a
    /// pass that placed nothing), and the handoff line once both waiters have armed.
    fn maybe_spare_pass(now: u64) {
        let last = LAST_SPARE_PASS.load(SeqCst);
        let every = if SPARE_PASS_IDLE.load(SeqCst) != 0 {
            SPARE_IDLE_EVERY_NS
        } else {
            TOPUP_EVERY_NS
        };
        let (wa, ra) = (W_ARM_AT.load(SeqCst), R_ARM_AT.load(SeqCst));
        let armed = wa != 0 && ra != 0;
        // Right after both waiters armed, pass at once: their own slots have just opened.
        let fresh = armed && last < wa.max(ra);
        if (!fresh && now.saturating_sub(last) < every)
            || LAST_SPARE_PASS
                .compare_exchange(last, now, SeqCst, SeqCst)
                .is_err()
        {
            return;
        }
        let placed = spare_pass();
        SPARE_PASS_IDLE.store(u32::from(placed == 0), SeqCst);
        if armed
            && now >= wa.max(ra) + HANDOFF_SETTLE_NS
            && HANDOFF_LOGGED.swap(1, SeqCst) == 0
        {
            let (wd, re) = (Q_DEADLINE.load(SeqCst), R_DEADLINE.load(SeqCst));
            let at = mono();
            yarm_user_rt::user_log!(
                "STW_HANDOFF at={} w_deadline={} r_earliest={} spares_placed={} early={}",
                at,
                wd,
                re,
                SPARES_PLACED.load(SeqCst),
                u32::from(at < wd.min(re))
            );
        }
    }

    /// Spawn the spares while the queues are still empty, and wait (bounded) for each to park.
    fn spawn_spares() {
        let entry = FILLER_ENTRY.load(SeqCst);
        let stacks = core::ptr::addr_of_mut!(FILLER_STACKS) as usize;
        let tls = core::ptr::addr_of_mut!(FILLER_TLS) as usize;
        SPARE_MODE.store(1, SeqCst);
        let mut spawned = 0u32;
        for k in 0..SPARES {
            let i = SPAWN_LIMIT + k;
            let top = (stacks + (i + 1) * FILLER_STACK_BYTES) & !0xF;
            // SAFETY: a valid entry; the static stack and TLS outlive the thread.
            if let Ok(t) = unsafe { yarm_user_rt::syscall::spawn_thread(tls + i * 64, top, entry) }
                && t >= 10_000
            {
                spawned += 1;
            }
        }
        let mut n = 0u32;
        while SPARES_PARKING.load(SeqCst) < spawned && n < BOUND {
            let _ = yield_now();
            n += 1;
        }
        // Let the last of them complete its park before anything can be woken.
        for _ in 0..4 {
            let _ = yield_now();
        }
        SPARE_MODE.store(0, SeqCst);
        yarm_user_rt::user_log!(
            "STW_SPARES spawned={} parking={}",
            spawned,
            SPARES_PARKING.load(SeqCst)
        );
        if spawned as usize != SPARES || SPARES_PARKING.load(SeqCst) != spawned {
            fail("spares");
        }
    }

