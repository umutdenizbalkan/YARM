#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""KTHREAD1-ACCEPTANCE §2 — grade one device-interrupt witness boot built with
`kthread-irq-witness` (scripts/qemu-kthread-irq-witness-smoke.sh).

usage: grade-kthread-irq-witness.py --arch A --kernel-elf K <normalized boot log>

The port witness exchanges twelve items: 1..=8 as before (odd idle, even user) and 9..=12 whose
interrupt must land on a running KERNEL THREAD. Every check below reads independent evidence:
the port fixture's entry record (written on the interrupt path, with the interrupted PC and the
thread's incarnation id), the witness thread's own record (registers, arrival, resumption), the
receiver's records (what userspace received), the host driver's injection lines and the kernel
ELF's symbol table.

Graded, each separately:
  * RECEIVER: twelve items received exactly once with the bound label and the exact data sequence,
    4 idle + 4 user + 4 kernel-thread items, registers intact, the post-disable quiet phase (ten
    timed-out parks: timer service and userspace progress after the last item), isolation.
  * KERNEL-THREAD ARRIVALS: exactly four device-interrupt entries with origin `kthread`, the n-th for
    item 8+n, each naming the witness thread's incarnation and the gate opened for that item by
    that thread, each with its interrupted PC inside the witness thread's register-patterned spin
    (`kirq_witness::spin_unmasked`, from the ELF symbol table, and equal to the address the thread
    reported for it).
  * INTERRUPTED CONTEXT: per item the thread reports every patterned register intact, the spin
    ended on its own device interrupt (one per item), and the thread resumed after its scheduling
    point.
  * ORDER: injection -> kernel-thread entry -> the thread's record -> the receiver's record, per item.
  * COMPLETION: the port's own accounting at the disable: twelve claims, twelve completions, twelve
    data claims, twelve bytes (and on AArch64 twelve deactivations); idle and user origins still 4
    and 4 and nothing classified anywhere else.
  * CONTINUATION: after the last item the thread kept running and the CPU's timer interrupted kernel
    threads at least three more times (`KIRQ_WINDOW_DONE ... result=ok`).
  * NOTHING FATAL: no kernel-thread fault, unsupported or unauthenticated interrupt, refused delivery,
    panic, unhandled trap or host error.

Exit 0 only when every check holds. Each failure is printed as `[fail] <reason> ...`.
"""
import argparse
import re
import subprocess
import sys

ITEMS, BASE = 12, 8
KT_ITEMS = range(BASE + 1, ITEMS + 1)

PORT = {
    "riscv64": dict(
        # sepc is the interrupted PC; the entry line carries the item and the gate.
        kentry=re.compile(r"IRQ1_UART_IRQ_ENTRY origin=kthread n=(\d+) sepc=0x([0-9a-f]+) tid=0 "
                          r"kthread=0x([0-9a-f]+) item=(\d+) gate=(\w+)"),
        kentry_fields=lambda m: dict(pc=int(m.group(2), 16), kid=int(m.group(3), 16),
                                     item=int(m.group(4)), gate_ok=m.group(5) == "kthread",
                                     gate_kid=int(m.group(3), 16)),
        entries=re.compile(r"IRQ1_UART_IRQ_ENTRY origin=(\w+)"),
        disabled=re.compile(r"IRQ1_UART_SOURCE_DISABLED source=\d+ claims=(\d+) data_claims=(\d+) "
                            r"completions=(\d+) bytes=(\d+) "),
        totals=re.compile(r"IRQ1_UART_TOTALS empty_drains=(\d+) idle_origin=(\d+) user_origin=(\d+) "
                          r"foreign_claims=(\d+) gate_opens=(\d+) ungated_claims=(\d+)"),
        totals_ok=lambda g: g == ("0", "4", "4", "0", "12", "0"),
        ktotals=re.compile(r"IRQ1_UART_KTHREAD_TOTALS kthread_origin=(\d+)"),
        fatal=["RISCV_TRAP_UNHANDLED", "RISCV_TRAP_HANDLE_FAILED",
               "RISCV_S_MODE_EXTIRQ_UNEXPECTED_RESUME", "IRQFINAL_RISCV_COMPLETION_UNTRANSLATABLE"],
    ),
    "aarch64": dict(
        kentry=re.compile(r"IRQ2_PL011_KTHREAD_ENTRY intid=\d+ elr=0x([0-9a-f]+) kthread=0x([0-9a-f]+) "
                          r"gate_item=(\d+) gate_kthread=0x([0-9a-f]+)"),
        kentry_fields=lambda m: dict(pc=int(m.group(1), 16), kid=int(m.group(2), 16),
                                     item=int(m.group(3)), gate_ok=True,
                                     gate_kid=int(m.group(4), 16)),
        entries=re.compile(r"IRQ2_PL011_IRQ_ENTRY origin=(\w+)"),
        disabled=re.compile(r"IRQ2_PL011_SOURCE_DISABLED intid=\d+ claims=(\d+) data_claims=(\d+) "
                            r"completions=(\d+) deactivated=(\d+) bytes=(\d+) "),
        totals=re.compile(r"IRQ2_PL011_TOTALS empty_drains=(\d+) idle_origin=(\d+) user_origin=(\d+) "
                          r"other_origin=(\d+) foreign_claims=(\d+) special_claims=(\d+)"),
        totals_ok=lambda g: g[:5] == ("0", "4", "4", "0", "0"),
        ktotals=re.compile(r"IRQ2_PL011_KTHREAD_TOTALS kthread_origin=(\d+) gate_opens=(\d+)"),
        fatal=["KTHREAD_IRQ_UNSUPPORTED"],
    ),
    "x86_64": dict(
        kentry=re.compile(r"IRQ3_UART_KTHREAD_ENTRY vector=0x[0-9a-f]+ rip=0x([0-9a-f]+) "
                          r"kthread=0x([0-9a-f]+) gate_item=(\d+) gate_kthread=0x([0-9a-f]+)"),
        kentry_fields=lambda m: dict(pc=int(m.group(1), 16), kid=int(m.group(2), 16),
                                     item=int(m.group(3)), gate_ok=True,
                                     gate_kid=int(m.group(4), 16)),
        entries=re.compile(r"IRQ3_UART_IRQ_ENTRY origin=(\w+)"),
        disabled=re.compile(r"IRQ3_UART_SOURCE_DISABLED .*?claims=(\d+) data_claims=(\d+) "
                            r"completions=(\d+) .*?bytes=(\d+)"),
        totals=re.compile(r"IRQ3_UART_TOTALS empty_drains=(\d+) idle_origin=(\d+) user_origin=(\d+) "
                          r"other_origin=(\d+) together_held=(\d+)"),
        totals_ok=lambda g: g[:4] == ("0", "4", "4", "0"),
        ktotals=re.compile(r"IRQ3_UART_KTHREAD_TOTALS kthread_origin=(\d+) gate_opens=(\d+)"),
        fatal=["X86_USER_FPU_HOME_UNAUTHENTICATED"],
    ),
}

COMMON_FATAL = ["KTHREAD_FAULT", "KTHREAD_IRQ_UNAUTHENTICATED", "KTHREAD_IRQ_UNSUPPORTED",
                "KTHREAD_DEVICE_IRQ_DELIVERY_REFUSED", "panicked at", "KERNEL PANIC", "#HOST_ERROR"]


def sym_range(elf, needle):
    try:
        out = subprocess.run(["nm", "-S", "-C", elf], capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    for line in out.splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4 and needle in parts[3]:
            return int(parts[0], 16), int(parts[0], 16) + int(parts[1], 16)
    return None


def grade(text, arch, elf):
    p = PORT[arch]
    fails = []
    lines = text.splitlines()

    def first(pat):
        for i, l in enumerate(lines):
            m = re.search(pat, l)
            if m:
                return i, m
        return None, None

    def all_of(pat):
        return [(i, m) for i, l in enumerate(lines) for m in [re.search(pat, l)] if m]

    # ── RECEIVER ──
    summ = all_of(r"IRQ1_UART_WITNESS items=(\d+) .*result=(\w+)")
    want = ("items=12 received=12 exact_once=12 label_ok=12 data_ok=12 idle_items=4 user_items=4 "
            "regs_bad=0 dup=0 errors=0 isolated=1 result=ok")
    if len(summ) != 1 or want not in lines[summ[0][0]]:
        fails.append("receiver_summary %s" % (lines[summ[0][0]][-200:] if summ else "missing"))
    _, m = first(r"IRQ1_UART_KTHREAD_ITEMS items=4 kthread_items=(\d+) kthread_origin=(\d+) result=(\w+)")
    if not m or m.groups() != ("4", "4", "ok"):
        fails.append("receiver_kthread_items %s" % (m.group(0) if m else "missing"))
    _, m = first(r"IRQ1_UART_WITNESS_COUNTS claims=(\d+) completions=(\d+) .*post_timeouts=(\d+) "
                 r"post_notifications=(\d+) post_quiet=(\d+)")
    if not m or m.groups() != ("12", "12", "10", "0", "1"):
        fails.append("receiver_counts %s" % (m.group(0) if m else "missing"))
    recv_at = {}
    for s in range(1, ITEMS + 1):
        mode = "kthread" if s > BASE else ("idle" if s % 2 else "user")
        hits = all_of(r"IRQ1_UART_RECV seq=%d mode=%s label=\d+ payload_len=\d+ once=1 byte=0x%02x "
                      r"bytes=%d data_ok=1 " % (s, mode, 0x40 + s, s))
        if len(hits) != 1:
            fails.append("item_not_received_once seq=%d mode=%s" % (s, mode))
        else:
            recv_at[s] = hits[0][0]

    # ── the witness thread ──
    starts = all_of(r"KIRQ_WINDOW_START kthread=0x([0-9a-f]+) cpu=0")
    if len(starts) != 1:
        fails.append("window_thread_start count=%d" % len(starts))
        return fails
    kid = int(starts[0][1].group(1), 16)
    rng = sym_range(elf, "kirq_witness::spin_unmasked")
    if rng is None:
        fails.append("spin_symbol_missing")
    win_at, spin_fn = {}, set()
    for s in KT_ITEMS:
        hits = all_of(r"KIRQ_WINDOW item=%d kthread=0x([0-9a-f]+) cpu=0 regs_ok=(\d) arrived=(\d) "
                      r"device_irqs=(\d+) resched_pending=(\d) switched=(\d) resumed=1 spin_fn=0x([0-9a-f]+)" % s)
        if len(hits) != 1:
            fails.append("window_record_missing item=%d" % s)
            continue
        i, m = hits[0]
        win_at[s] = i
        spin_fn.add(int(m.group(7), 16))
        if int(m.group(1), 16) != kid:
            fails.append("window_record_other_thread item=%d" % s)
        if m.group(2) != "1":
            fails.append("registers_clobbered_by_device_interrupt item=%d" % s)
        if m.group(3) != "1" or m.group(4) != "1":
            fails.append("device_interrupt_not_taken_once_in_spin item=%d arrived=%s device_irqs=%s"
                         % (s, m.group(3), m.group(4)))
        if m.group(5) != "1":
            fails.append("reschedule_not_left_pending item=%d" % s)
    if rng and spin_fn and spin_fn != {rng[0]}:
        fails.append("spin_fn_mismatch reported=%s elf=0x%x" % (sorted(hex(x) for x in spin_fn), rng[0]))

    # ── KERNEL-THREAD ARRIVALS ──
    kents = all_of(p["kentry"])
    if len(kents) != len(KT_ITEMS):
        fails.append("kthread_entries count=%d expected=%d" % (len(kents), len(KT_ITEMS)))
    ent_at = {}
    for n, (i, m) in enumerate(kents[:len(KT_ITEMS)]):
        s = BASE + 1 + n
        f = p["kentry_fields"](m)
        if f["kid"] != kid or f["gate_kid"] != kid:
            fails.append("kthread_entry_identity item=%d kid=0x%x gate_kid=0x%x window=0x%x"
                         % (s, f["kid"], f["gate_kid"], kid))
        if f["item"] != s or not f["gate_ok"]:
            fails.append("kthread_entry_gate item=%d gate_item=%d" % (s, f["item"]))
        if rng and not (rng[0] <= f["pc"] < rng[1]):
            fails.append("kthread_entry_pc_outside_spin item=%d pc=0x%x spin=[0x%x,0x%x)"
                         % (s, f["pc"], rng[0], rng[1]))
        ent_at[s] = i
    origins = {}
    for _, m in all_of(p["entries"]):
        origins[m.group(1)] = origins.get(m.group(1), 0) + 1
    if origins != {"idle": 4, "user": 4, "kthread": 4}:
        fails.append("entry_origins %s" % sorted(origins.items()))

    # ── ORDER ──
    for s in KT_ITEMS:
        inj = all_of(r"^#HOST_INJECT .* seq=%d mode=kthread " % s)
        pts = [inj[0][0] if inj else None, ent_at.get(s), win_at.get(s), recv_at.get(s)]
        if None in pts or not (pts[0] < pts[1] < pts[2] < pts[3]):
            fails.append("order item=%d inject/entry/window/recv=%s" % (s, pts))

    # ── COMPLETION ──
    _, m = first(p["disabled"])
    if not m:
        fails.append("source_not_disabled")
    elif any(int(x) != ITEMS for x in m.groups()):
        fails.append("completion_accounting %s" % m.group(0))
    if p["totals"] is not None:
        _, m = first(p["totals"])
        if not m or not p["totals_ok"](m.groups()):
            fails.append("port_totals %s" % (m.group(0) if m else "missing"))
    _, m = first(p["ktotals"])
    if not m or m.group(1) != "4" or (len(m.groups()) > 1 and m.group(2) != "4"):
        fails.append("port_kthread_totals %s" % (m.group(0) if m else "missing"))

    # ── CONTINUATION ──
    done = all_of(r"KIRQ_WINDOW_DONE kthread=0x([0-9a-f]+) cpu=0 rounds_after=(\d+) "
                  r"timer_irqs_after=(\d+) device_irqs_after=(\d+) result=(\w+)")
    if len(done) != 1 or done[0][1].group(5) != "ok" or int(done[0][1].group(3)) < 3 \
            or int(done[0][1].group(1), 16) != kid or int(done[0][1].group(2)) == 0:
        fails.append("no_kernel_thread_progress_or_timer_after %s"
                     % (done[0][1].group(0) if done else "missing"))
    elif win_at and done[0][0] < max(win_at.values()):
        fails.append("done_before_last_item")

    # ── NOTHING FATAL ──
    for bad in COMMON_FATAL + p["fatal"]:
        n = sum(1 for l in lines if bad in l)
        if n:
            fails.append("fatal_marker %s count=%d" % (bad, n))
    return fails


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--arch", required=True, choices=sorted(PORT))
    ap.add_argument("--kernel-elf", required=True)
    ap.add_argument("log")
    a = ap.parse_args()
    with open(a.log, "rb") as f:
        text = f.read().decode("utf-8", "replace")
    fails = grade(text, a.arch, a.kernel_elf)
    for f in fails:
        print("[fail] " + f)
    print("KIRQ_VERDICT arch=%s failures=%d result=%s" % (a.arch, len(fails), "fail" if fails else "pass"))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
