<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Umut Deniz Balkan -->

# Kernel-unlocking deferred defects

Running list of real defects found while executing kernel-unlocking increments that were
**deliberately not fixed** in the increment that found them, because they were out of its
scope and not caused by it.

Recording a defect here is not a plan, a diagnosis, or a numbered directive. It exists so a
known failure cannot quietly become invisible between increments.

Format: `date | defect | where | found during`

- 2026-08-11 | AArch64 strict optional-FS smoke stalls at steady-state idle before RAMFS/EXT4 markers; reproduced identically at U1 parent 03f6e5b and U1 26364834, therefore not a U1 regression | scripts/qemu-aarch64-optional-fs-smoke.sh | found during U1
