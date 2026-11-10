#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""IRQ-DELIVERY1 — grade one saturation-window boot of a device-interrupt witness.

Usage: grade-irq-saturation-witness.py --arch riscv64|aarch64|x86_64 <normalized boot log>

Graded, each separately:
  * HOST: ten saturation bytes injected, each paced on the previous claim's DRAIN marker.
  * ORDINARY: the witness's own eight items still pass (`IRQ1_UART_WITNESS items=8 ... result=ok`).
  * SATURATION: init's window summary — ten interrupts, ten bytes drained by the kernel, nine
    events received (eight queued + the one coalesced pending line), every label the bound line.
  * DELIVERY: the kernel's per-delivery outcomes — sixteen `delivered`, exactly two `coalesced`,
    no `failed`, nothing else.
  * COMPLETION: the controller completions the fixture counted equal its claims.
  * CONTINUITY: no fatal trap path on any port, no kernel-thread refusal, no panic; the boot
    reached the witness summary and the post-disable silence and isolation phases ran.
Never retries; prints IRQSAT_GRADE ... result=ok|fail.
"""
import re
import sys


def main() -> int:
    args = sys.argv[1:]
    if len(args) != 3 or args[0] != "--arch":
        print("usage: grade-irq-saturation-witness.py --arch ARCH LOG")
        return 2
    arch, path = args[1], args[2]
    text = open(path, "rb").read().decode("utf-8", "replace")
    fails = []

    def need(cond, what):
        if not cond:
            fails.append(what)

    sat = re.findall(r"#HOST_SATURATION injected=(\d+) of=(\d+)", text)
    need(sat == [("10", "10")], f"host saturation injections {sat}")
    paced = len(re.findall(r"#HOST_INJECT byte=0x6[0-9a-f] .* sat=\d+/10 ack=(sat_ready|drain claim=\d+|drain\+idle)", text))
    need(paced == 10, f"paced saturation injections {paced}/10")

    ordinary = re.findall(r"IRQ1_UART_WITNESS items=8 received=8 exact_once=8 label_ok=8 data_ok=8 .* result=(\w+)", text)
    need(ordinary == ["ok"], f"ordinary witness summary {ordinary}")

    m = re.findall(r"IRQ1_UART_SATURATION interrupts=10 bytes=(\d+) claims=(\d+) received=(\d+) label_ok=(\d+) expected=9 errors=(\d+) rounds=\d+ result=(\w+)", text)
    need(len(m) == 1, f"one saturation summary ({len(m)})")
    if m:
        bytes_, claims, received, label_ok, errors, result = m[0]
        need(bytes_ == "10" and int(claims) >= 10, f"saturation bytes={bytes_} claims={claims}")
        need(received == "9" and label_ok == "9", f"received={received} label_ok={label_ok}, expected 9 (8 queued + 1 pending)")
        need(errors == "0" and result == "ok", f"saturation errors={errors} result={result}")

    outcomes = re.findall(r"IRQ1_SPLIT_DELIVERY cpu=\d+ line=\d+ outcome=([a-z_]+)", text)
    counts = {o: outcomes.count(o) for o in set(outcomes)}
    need(counts.get("delivered", 0) == 16, f"delivered={counts.get('delivered', 0)}, expected 16 (8 items + 8 queued)")
    need(counts.get("coalesced", 0) == 2, f"coalesced={counts.get('coalesced', 0)}, expected 2")
    need(set(counts) <= {"delivered", "coalesced"}, f"unexpected outcomes {counts}")

    c = re.findall(r"IRQ1_UART_WITNESS_COUNTS claims=(\d+) completions=(\d+)", text)
    need(len(c) == 1 and c[0][0] == c[0][1], f"claims/completions {c}")

    for fatal in ["x86 shared trap dispatch failed", "YARM_AARCH64_TRAP_HANDLE failed",
                  "YARM_AARCH64_TRAP_HANDLE halting", "RISCV_TRAP_HANDLE_FAILED",
                  "KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED", "panicked at", "IRQ1_UNEXPECTED_DISPOSITION"]:
        need(fatal not in text, f"fatal marker present: {fatal}")
    need("IRQ1_UART_POSTDISABLE_READY disabled=1" in text, "post-disable phase not reached")
    need(re.search(r"IRQ1_UART_ISOLATION .* result=ok", text) is not None, "isolation phase")

    origins = re.findall(r"IRQ1_UART_WITNESS_COUNTS claims=\d+ completions=\d+ empty_drains=\d+ idle_origin=(\d+) user_origin=(\d+)", text)
    summary = (f"IRQSAT_GRADE arch={arch} origins(idle,user)={origins[0] if origins else 'none'} "
               f"delivered={counts.get('delivered', 0)} "
               f"coalesced={counts.get('coalesced', 0)} saturation={m[0][2] if m else 'none'} "
               f"claims_completions={c[0] if c else 'none'}")
    if fails:
        for f in fails:
            print(f"[irqsat-grade][fail] {f}")
        print(summary + " result=fail")
        return 1
    print(summary + " result=ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
