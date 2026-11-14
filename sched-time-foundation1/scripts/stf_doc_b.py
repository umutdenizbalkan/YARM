import re,sys
p='doc/SYSCALL_ABI.md'; s=open(p).read()
def rep(a,b,count=1):
    global s
    n=s.count(a)
    if n<count or (count==1 and n!=1): sys.exit(f"anchor x{n}: {a[:70]!r}")
    s=s.replace(a,b)
rep('# YARM Syscall ABI v11 (Frozen Contract)','# YARM Syscall ABI v12 (Frozen Contract)')
rep('- ABI Version: `11`\n- Public syscall count: `18` (`0..=17`)','- ABI Version: `12`\n- Public syscall count: `20` (`0..=19`)')
rep('## Public ABI v11 syscall numbers','## Public ABI v12 syscall numbers')
rep('count. The current public ABI v11 surface is the contiguous user-callable range\n`0..=17` (**18 slots**).','count. The current public ABI v12 surface is the contiguous user-callable range\n`0..=19` (**20 slots**).')
row17_end='`17` is the lowest number of the former reserved gap `17..=22`. |\n'
rep(row17_end, row17_end+
 '| `18` | `ClockGetTime` | public (`arg0=clock_id`: `0` `CLOCK_REALTIME`, `1` `CLOCK_MONOTONIC`; `arg1=ts_ptr`, 8-aligned, 16 writable bytes) — writes one `Timespec`; SCHED-TIME-FOUNDATION1, ABI v12 |\n'
 '| `19` | `ClockSetRealtime` | public control-plane ABI (`arg0=ts_ptr`, 8-aligned `Timespec`, UTC since the Unix epoch); policy-gated: only a `SystemServer` caller may initialize `CLOCK_REALTIME`, others receive `MissingRight` — SCHED-TIME-FOUNDATION1, ABI v12 |\n')
rep('- **public v11**: part of the stable user-callable syscall ABI for version `10`.','- **public v12**: part of the stable user-callable syscall ABI for version `12`.')
s=s.replace('| public v11 |','| public v12 |')
rep('range; intended for PM/SystemServer/bootstrap','range; intended for PM/SystemServer/bootstrap')
s=s.replace('outside the public v11\n  range','outside the public v12\n  range')
rep('- **reserved gap**: not assigned and not dispatched in v11.','- **reserved gap**: not assigned and not dispatched in v12.')
rep('**No public v11 syscall is currently\n  deprecated.**','**No public v12 syscall is currently\n  deprecated.**')
m=re.search(r'\| `18\.\.=22` \| — \| reserved gap \|[^\n]*\n',s); assert m
old=m.group(0)
s=s.replace(old,
 '| `18` | `ClockGetTime` | public v12 | any user task | Coherent clock read; split from birth on every port. |\n'
 '| `19` | `ClockSetRealtime` | public v12 | `SystemServer` only | Realtime epoch initialization; others receive `MissingRight`; split from birth. |\n'
 + old.replace('`18..=22`','`20..=22`',1))
s=s.replace('not part of public v11 count','not part of public v12 count')
# versioning policy
rep('The v11 contract is frozen for the public ABI slots listed above. Freezing means\ncall numbers and incompatible user-visible semantics do not change inside v11;',
    'The v12 contract is frozen for the public ABI slots listed above. Freezing means\ncall numbers and incompatible user-visible semantics do not change inside v12;')
rep('**v10 → v11 (FUTEX-REQUEUE1).**',
 '**v11 → v12 (SCHED-TIME-FOUNDATION1).** NR `18` `ClockGetTime` and NR `19` `ClockSetRealtime`\n'
 'are assigned from the reserved gap, and error code `11` `Unavailable` is added (a clock that has\n'
 'no trustworthy value: `CLOCK_REALTIME` before an authorized initialization, or no monotonic\n'
 'source). `SYSCALL_ABI_VERSION` is `12`, and the `abi_version` field NR 30 writes reports it\n'
 '(`RECV_V3_ABI_VERSION`). Every other number, layout and contract is v11\'s. See\n'
 '"Timekeeping (SCHED-TIME-FOUNDATION1)" below.\n\n'
 '**v10 → v11 (FUTEX-REQUEUE1).**')
rep('Changes that require an ABI v12 bump include:','Changes that require an ABI v13 bump include:')
rep('- assigning a reserved public gap (`18..=22` or `25`) to a user-visible public','- assigning a reserved public gap (`20..=22` or `25`) to a user-visible public')
rep('- removing or making invalid a public v11 syscall that is still documented as','- removing or making invalid a public v12 syscall that is still documented as')
rep('Changes allowed within v11 include:','Changes allowed within v12 include:')
rep('- adding or refining privileged extension slots outside the public v11 range,','- adding or refining privileged extension slots outside the public v12 range,')
rep('The public tail slots `13..=15` are assigned and dispatched in v11:','The public tail slots `13..=15` are assigned and dispatched in v12:')
rep('They are not reserved gaps and are covered by the public ABI v11 matrix above.','They are not reserved gaps and are covered by the public ABI v12 matrix above.')
# broad-lock matrix
r17='| `17` | `FutexCmpRequeue` | off-lock | off-lock | off-lock | ungated — split from birth (FUTEX-REQUEUE1) |\n'
rep(r17, r17+
 '| `18` | `ClockGetTime` | off-lock | off-lock | off-lock | ungated — split from birth (SCHED-TIME-FOUNDATION1) |\n'
 '| `19` | `ClockSetRealtime` | off-lock | off-lock | off-lock | ungated — split from birth (SCHED-TIME-FOUNDATION1) |\n')
rep('purpose-built bridge (`src/arch/riscv64/trap.rs:417`) whose gate admits only NR 15,\nNR 10, and gated NR 6 / NR 7.',
    'purpose-built bridge (`src/arch/riscv64/trap.rs:417`) whose gate admits only NR 15,\nNR 10, NR 17, NR 18 / NR 19, NR 9, NR 5, NR 2, and gated NR 6 / NR 7.')
rep('- `18..=22`: reserved/unassigned in ABI v11 (`17` became `FutexCmpRequeue` in v11).',
    '- `20..=22`: reserved/unassigned in ABI v12 (`17` became `FutexCmpRequeue` in v11; `18` `ClockGetTime` and `19` `ClockSetRealtime` in v12).')
rep('- `25`: reserved/unassigned in ABI v11.','- `25`: reserved/unassigned in ABI v12.')
rep('`version=3`, `record_len=80`, `abi_version=11`,','`version=3`, `record_len=80`, `abi_version=12`,')
rep('There is no user-visible clock in ABI v11, and the timeout needs none:',
    'The timeout is independent of the ABI v12 clocks (NR 18) and needs none:')
open(p,'w').write(s)
