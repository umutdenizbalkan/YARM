// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

extern crate yarm;

use std::time::Instant;
use yarm::kernel::ipc::{Message, SharedMemoryRegion};

fn bench_inline_construct(payload_len: usize, iterations: usize) -> f64 {
    let payload = vec![0xABu8; payload_len];
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let msg = Message::with_header(7, 0x42, 0, None, &payload).expect("inline msg");
        checksum ^= msg.len as usize;
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(checksum);
    elapsed.as_secs_f64() / iterations as f64
}

fn bench_shared_descriptor_construct(iterations: usize) -> f64 {
    let descriptor = SharedMemoryRegion {
        offset: 0x4000,
        len: 1024,
    }
    .encode();
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let msg = Message::with_header(
            7,
            1,
            Message::FLAG_CAP_TRANSFER,
            Some(1),
            &descriptor,
        )
        .expect("shared descriptor msg");
        checksum ^= msg.len as usize;
    }
    let elapsed = start.elapsed();
    let _ = std::hint::black_box(checksum);
    elapsed.as_secs_f64() / iterations as f64
}

#[test]
fn phase1_payload_policy_benchmark_snapshot() {
    let iterations = 200_000;
    let ns64 = bench_inline_construct(64, iterations) * 1e9;
    let ns128 = bench_inline_construct(128, iterations) * 1e9;
    let ns_shared = bench_shared_descriptor_construct(iterations) * 1e9;

    // Simulated 256B request via two 128B inline fragments.
    let ns256_two_frag = ns128 * 2.0;

    println!(
        "phase1-bench ns/op inline64={:.2} inline128={:.2} shared_desc={:.2} simulated_2x128={:.2}",
        ns64, ns128, ns_shared, ns256_two_frag
    );

    // Guardrail sanity checks only (avoid brittle perf asserts).
    assert!(ns64.is_finite() && ns64 > 0.0);
    assert!(ns128.is_finite() && ns128 > 0.0);
    assert!(ns_shared.is_finite() && ns_shared > 0.0);
}
