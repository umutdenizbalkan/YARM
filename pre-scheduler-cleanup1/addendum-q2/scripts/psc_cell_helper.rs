    fn helper_saturate() {
        spawn_spares();
        spawn_round(true);
        let first = NEXT_FILLER.load(SeqCst);
        // W, R and H stay runnable: a second round and a spare pass fill whatever the balanced
        // spawns left open, so every home queue is full before either waiter arms.
        spawn_round(true);
        let placed0 = spare_pass();
        let full_at = mono();
        let refusals = SPAWN_REFUSALS.load(SeqCst);
        yarm_user_rt::user_log!(
            "STW_SAT_READY full_at={} fillers={} refusals={} spares_placed={}",
            full_at,
            NEXT_FILLER.load(SeqCst),
            refusals,
            placed0
        );
        if refusals < 2 {
            fail("saturate");
        }
        // From here every runnable cell thread keeps the queues full (spare pass, then spawn
        // top-up): W's and R's waits free their slots on their homes, and H — which gets the CPU
        // only once per rotation — could not refill them in time. The kernel's SATW_REFUSED lines
        // are the proof.
        LAST_TOP.store(mono(), SeqCst);
        LAST_SPARE_PASS.store(mono(), SeqCst);
        TOPUP_ON.store(1, SeqCst);
        SAT_READY.store(1, SeqCst);
        // W and R arm only now: wait (bounded, staying in the rotation) for both deadlines.
        let mut nr = 0u32;
        while (R_DEADLINE.load(SeqCst) == 0 || Q_DEADLINE.load(SeqCst) == 0) && nr < BOUND {
            maybe_topup();
            let _ = yield_now();
            nr += 1;
        }
        let deadline = Q_DEADLINE.load(SeqCst);
        yarm_user_rt::user_log!(
            "STW_SATURATED fillers={} first={} refusals={} full_at={} w_deadline={} early={}",
            NEXT_FILLER.load(SeqCst),
            first,
            refusals,
            full_at,
            deadline,
            u32::from(deadline != 0 && full_at < deadline)
        );
        if deadline == 0 || full_at >= deadline {
            fail("saturate");
        }
        // R's receive must still be pending: its deadline lies after saturation is complete.
        let r_deadline = R_DEADLINE.load(SeqCst);
        yarm_user_rt::user_log!(
            "STW_SAT_RECV r_deadline={} full_at={} early={}",
            r_deadline,
            full_at,
            u32::from(r_deadline != 0 && full_at < r_deadline)
        );
        if r_deadline == 0 || full_at >= r_deadline {
            fail("saturate_recv");
        }
