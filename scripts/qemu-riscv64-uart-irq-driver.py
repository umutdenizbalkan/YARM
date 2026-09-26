#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Umut Deniz Balkan
"""QEMU-IRQ1 host driver: the PRODUCER for the RISC-V UART external-interrupt witness, and since
QEMU-IRQ2 (`--arch aarch64`) for the AArch64 PL011 witness. The receiver protocol is the same on
both ports; only the QEMU machine line and the kernel's idle acknowledgement differ.

Runs one QEMU `virt` boot whose only serial port (UART0, the ns16550a at 0x1000_0000) is a
dedicated UNIX-socket chardev. There is no monitor (`-monitor none`) and no console multiplexing:
the socket carries the guest's console output one way and the injected receive bytes the other,
and nothing else shares it. QEMU is started with `wait=on`, so the guest does not run until this
driver is connected and reading.

Synchronisation is by the guest's own acknowledgements, never by sleeping:

* `IRQ1_UART_READY seq=N mode=user` -> inject byte `0x40+N` at once: the receiver is spinning in
  U-mode, so the interrupt lands on user code.
* `IRQ1_UART_READY seq=N mode=idle` -> wait until the kernel reports the hart idle AFTER that line
  (`RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io` or `RISCV_S_MODE_TIMER_RESUME_IDLE`), then
  inject `0x40+N`, so the interrupt lands on the idle `wfi`.
* `IRQ1_UART_POSTDISABLE_READY` (after `IRQ1_UART_SOURCE_DISABLED`) -> inject one more byte, `Z`,
  which a disabled source must NOT deliver.

One item is outstanding at a time; the next READY is only printed after the receiver has consumed
the previous item. Every injection is recorded in the log as a `#HOST_INJECT` line at the point in
the byte stream where it happened.

Exit status: 0 when the witness's final line was seen, 2 on timeout, 3 if QEMU exited first. The
driver does not grade; the smoke script does.

AArch64 (`--arch aarch64`): QEMU `virt`, `cortex-a72`, 1024M, `-smp 1` — the core smoke's machine
at one CPU — whose only serial port is the PL011 at 0x0900_0000. The idle acknowledgement is
`SCHED_ENTER_IDLE_HLT`, which the kernel prints on its way into the parked `wfi` loop.

Usage: qemu-riscv64-uart-irq-driver.py [--arch riscv64|aarch64] --kernel K --initrd I --log L
       [--timeout S] [--no-inject]
"""

import argparse
import os
import re
import select
import socket
import subprocess
import sys
import tempfile
import time

READY_RE = re.compile(rb"IRQ1_UART_READY seq=(\d+) mode=(idle|user) expect=0x([0-9a-f]{2})")
IDLE_RE = re.compile(rb"RISCV_TRAP_HALTED reason=kernel_idle_awaiting_io|RISCV_S_MODE_TIMER_RESUME_IDLE")
IDLE_RE_AARCH64 = re.compile(rb"SCHED_ENTER_IDLE_HLT")
DISABLED_RE_AARCH64 = re.compile(rb"IRQ2_PL011_SOURCE_DISABLED ")
DONE_RE = re.compile(rb"IRQ1_UART_WITNESS items=")
DISABLED_RE = re.compile(rb"IRQ1_UART_SOURCE_DISABLED ")
POST_RE = re.compile(rb"IRQ1_UART_POSTDISABLE_READY")
BYTE_BASE = 0x40
POST_BYTE = ord("Z")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--arch", choices=["riscv64", "aarch64"], default="riscv64")
    ap.add_argument("--kernel", required=True)
    ap.add_argument("--initrd", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--timeout", type=float, default=240.0)
    ap.add_argument("--tail", type=float, default=15.0,
                    help="seconds to keep capturing after the witness's final line")
    ap.add_argument("--cmdline", default=None)
    ap.add_argument("--qemu", default=None)
    ap.add_argument("--no-inject", action="store_true",
                    help="negative control: answer no READY (the producer is withheld)")
    args = ap.parse_args()

    sockdir = tempfile.mkdtemp(prefix="yarm-irq1-")
    sock_path = os.path.join(sockdir, "uart0.sock")
    serial = [
        "-display", "none", "-monitor", "none", "-no-reboot",
        "-chardev", f"socket,id=uart0,path={sock_path},server=on,wait=on",
        "-serial", "chardev:uart0",
    ]
    if args.arch == "aarch64":
        idle_re, disabled_re = IDLE_RE_AARCH64, DISABLED_RE_AARCH64
        cmd = [args.qemu or "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a72",
               "-m", "1024M", "-smp", "1", *serial,
               "-kernel", args.kernel, "-initrd", args.initrd]
        if args.cmdline:
            cmd += ["-append", args.cmdline]
    else:
        idle_re, disabled_re = IDLE_RE, DISABLED_RE
        cmd = [args.qemu or "qemu-system-riscv64", "-machine", "virt", "-cpu", "rv64",
               "-m", "512M", "-smp", "1", *serial,
               "-bios", "default", "-kernel", args.kernel, "-initrd", args.initrd,
               "-append", args.cmdline or "console=ttyS0 rdinit=/init"]
    log = open(args.log, "wb")
    log.write(("#HOST_QEMU " + " ".join(cmd) + "\n").encode())
    qemu = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)

    sock = None
    deadline = time.monotonic() + 20.0
    while sock is None:
        if qemu.poll() is not None:
            log.write(b"#HOST_ERROR qemu exited before the serial socket appeared\n")
            log.write(qemu.stdout.read() or b"")
            return 3
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(sock_path)
            sock = s
        except OSError:
            if time.monotonic() > deadline:
                log.write(b"#HOST_ERROR could not connect to the serial socket\n")
                qemu.kill()
                return 3
            time.sleep(0.05)  # waiting for QEMU to create its listening socket, not for the guest
    sock.setblocking(False)
    t0 = time.monotonic()

    def inject(byte: int, why: str) -> None:
        sock.sendall(bytes([byte]))
        log.write(f"\n#HOST_INJECT byte=0x{byte:02x} t={time.monotonic() - t0:.3f} {why}\n".encode())
        log.flush()

    buf = b""
    pending_idle = None  # (seq, byte) waiting for the idle acknowledgement
    injected = {}
    disabled_seen = False
    post_injected = False
    done_at = None
    status = 2
    end = t0 + args.timeout
    while True:
        now = time.monotonic()
        if done_at is not None and now - done_at >= args.tail:
            status = 0
            break
        if now > end:
            log.write(b"\n#HOST_TIMEOUT\n")
            status = 2 if done_at is None else 0
            break
        if qemu.poll() is not None:
            log.write(b"\n#HOST_QEMU_EXITED\n")
            status = 3 if done_at is None else 0
            break
        r, _, _ = select.select([sock], [], [], 0.2)
        if not r:
            continue
        try:
            chunk = sock.recv(65536)
        except BlockingIOError:
            continue
        if not chunk:
            log.write(b"\n#HOST_SOCKET_CLOSED\n")
            status = 3 if done_at is None else 0
            break
        log.write(chunk)
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            m = READY_RE.search(line)
            if m and not args.no_inject:
                seq = int(m.group(1))
                mode = m.group(2)
                expect = int(m.group(3), 16)
                byte = BYTE_BASE + seq
                if expect != byte:
                    log.write(f"\n#HOST_ERROR seq={seq} guest_expect=0x{expect:02x} host=0x{byte:02x}\n".encode())
                if seq in injected:
                    log.write(f"\n#HOST_ERROR duplicate READY seq={seq}\n".encode())
                    continue
                injected[seq] = byte
                if mode == b"user":
                    inject(byte, f"seq={seq} mode=user ack=ready")
                else:
                    pending_idle = (seq, byte)
                continue
            if pending_idle is not None and idle_re.search(line):
                seq, byte = pending_idle
                pending_idle = None
                inject(byte, f"seq={seq} mode=idle ack=ready+idle")
                continue
            if disabled_re.search(line):
                disabled_seen = True
            if POST_RE.search(line) and not post_injected and not args.no_inject:
                if disabled_seen:
                    inject(POST_BYTE, "post_disable=1 ack=postdisable_ready+source_disabled")
                    post_injected = True
                else:
                    log.write(b"\n#HOST_ERROR postdisable ready before the source was disabled\n")
            if DONE_RE.search(line) and done_at is None:
                done_at = time.monotonic()
    log.write(f"\n#HOST_SUMMARY injected={len(injected)} post_injected={int(post_injected)} "
              f"status={status}\n".encode())
    log.close()
    try:
        sock.close()
    except OSError:
        pass
    qemu.kill()
    qemu.wait()
    return status


if __name__ == "__main__":
    sys.exit(main())
