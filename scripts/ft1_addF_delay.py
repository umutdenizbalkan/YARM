#!/usr/bin/env python3
"""FUTEX-TIME1 Addendum F scratch patch (never pushed): widen the window between a schedlock1
witness section's record reservation and its open_hold() owner-commit check."""
import pathlib, sys
p = pathlib.Path(sys.argv[1]) / "src/kernel/schedlock1_witness.rs"; s = p.read_text()
old = "    // An OWNER COMMIT by the observer on its own CPU, during an open hold.\n"
assert s.count(old) == 1
new = ("    // ADDENDUM-F SCRATCH DELAY (never pushed): widen the reserve -> open_hold() window.\n"
       "    if exec == idx && after_cur > 1 && after_cur != before_cur && open_hold().is_none() {\n"
       "        for _ in 0..1_000_000u32 { core::hint::spin_loop(); }\n"
       "    }\n") + old
p.write_text(s.replace(old, new)); print("patched", p)
