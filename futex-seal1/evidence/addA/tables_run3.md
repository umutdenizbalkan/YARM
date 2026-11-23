| port | profile | arm | clean rounds | direct wakes | requeued | NR 17 calls | mutex re-parks (actually parked) | mutex `ValueChanged` | worker unlock wake calls / woke | broadcaster unlock calls / woke | elapsed median (min–max) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| x86_64 | gp | rq | 4/4 | 1 1 1 1 | 9 9 9 9 | 1 each | 0 0 0 0 | 0 0 0 0 | 10/8 10/8 10/8 10/8 | 1/1 1/1 1/1 1/1 | 480 ms (362–591) |
| x86_64 | gp | wa | 4/4 | 10 10 10 10 | 0 0 0 0 | 0 | 0 0 0 0 | 0 0 0 0 | 10/0 10/0 10/0 10/0 | 0/0 0/0 0/0 0/0 | 508 ms (383–556) |
| x86_64 | rtb | rq | 4/4 | 1 1 1 1 | 9 9 9 9 | 1 each | 0 0 0 0 | 0 0 0 0 | 10/8 10/8 10/8 10/8 | 1/1 1/1 1/1 1/1 | 477 ms (463–626) |
| x86_64 | rtb | wa | 4/4 | 10 10 10 10 | 0 0 0 0 | 0 | 0 0 0 0 | 0 0 0 0 | 10/0 10/0 10/0 10/0 | 0/0 0/0 0/0 0/0 | 468 ms (390–557) |
| aarch64 | gp | rq | 4/4 | 1 1 1 1 | 9 9 9 9 | 1 each | 0 0 0 0 | 0 0 0 0 | 10/8 10/8 10/8 10/8 | 1/1 1/1 1/1 1/1 | 96 ms (87–108) |
| aarch64 | gp | wa | 4/4 | 10 10 10 10 | 0 0 0 0 | 0 | 0 0 0 0 | 0 0 0 0 | 10/0 10/0 10/0 10/0 | 0/0 0/0 0/0 0/0 | 96 ms (91–104) |
| aarch64 | rtb | rq | 4/4 | 1 1 1 1 | 9 9 9 9 | 1 each | 0 0 0 0 | 0 0 0 0 | 10/8 10/8 10/8 10/8 | 1/1 1/1 1/1 1/1 | 98 ms (84–120) |
| aarch64 | rtb | wa | 4/4 | 10 10 10 10 | 0 0 0 0 | 0 | 0 0 0 0 | 0 0 0 0 | 10/0 10/0 10/0 10/0 | 0/0 0/0 0/0 0/0 | 101 ms (84–115) |
| riscv64 | gp | rq | 4/4 | 1 1 1 1 | 9 9 9 9 | 1 each | 1 0 1 0 | 0 0 0 0 | 10/9 10/8 10/9 10/8 | 1/1 1/1 1/1 1/1 | 1870 ms (1633–2347) |
| riscv64 | gp | wa | 4/4 | 10 10 10 10 | 0 0 0 0 | 0 | 0 0 0 0 | 0 0 0 0 | 10/0 10/0 10/0 10/0 | 0/0 0/0 0/0 0/0 | 1615 ms (1396–2840) |
| riscv64 | rtb | rq | 4/4 | 1 1 1 1 | 9 9 9 9 | 1 each | 0 0 0 0 | 0 0 0 0 | 10/8 10/8 10/8 10/8 | 1/1 1/1 1/1 1/1 | 1830 ms (1652–2160) |
| riscv64 | rtb | wa | 4/4 | 10 10 10 10 | 0 0 0 0 | 0 | 0 0 0 0 | 0 0 0 0 | 10/0 10/0 10/0 10/0 | 0/0 0/0 0/0 0/0 | 1930 ms (1867–2655) |

| port | induced drains | park tick (CPU) | release instant | rejoin tick | rejoin lateness (ticks) | deadline → settled (CLOCK_MONOTONIC) |
|---|---|---|---|---|---|---|
| x86_64 | 1 | 43 (CPU 1) | 105 | 105 | 0 | 0.6 ms after |
| aarch64 | 1 | 559 (CPU 1) | 594 | 628 | 34 | 0.6 ms after |
| riscv64 | 1 | 3040 (CPU 1) | 3046 | 3065 | 19 | 10.3 ms after |
<!-- gp rb x86_64: {"induced_exhaustion": 0} -->
<!-- gp rb aarch64: {"induced_exhaustion": 0} -->
<!-- gp rb riscv64: {"induced_exhaustion": 0} -->
<!-- lh x86_64 gp: {"early": 0, "late": 10, "timeout": 0, "timeout_acquired": 0} -->
<!-- lh x86_64 rtb: {"early": 0, "late": 10, "timeout": 0, "timeout_acquired": 0} -->
<!-- lh aarch64 gp: {"early": 0, "late": 7, "timeout": 1, "timeout_acquired": 2} -->
<!-- lh aarch64 rtb: {"early": 0, "late": 10, "timeout": 0, "timeout_acquired": 0} -->
<!-- lh riscv64 gp: {"early": 0, "late": 4, "timeout": 4, "timeout_acquired": 2} -->
<!-- lh riscv64 rtb: {"early": 0, "late": 5, "timeout": 0, "timeout_acquired": 5} -->
