#!/usr/bin/env python3
"""FUTEX-TIME1 Addendum F: count host `#HOST_INJECT` annotations written into a boot log right
after a PARTIAL guest line (no trailing CR, not empty, not a host line). Usage: <boot.log>..."""
import sys
for path in sys.argv[1:]:
    raw = open(path, 'rb').read().split(b'\n'); bad = []
    for i, l in enumerate(raw):
        if l.startswith(b'#HOST_INJECT') and i > 0:
            p = raw[i - 1]
            if p and not p.endswith(b'\r') and not p.startswith(b'#'):
                bad.append((i + 1, p[-40:].decode('latin-1')))
    print(path, 'host_lines_after_partial_guest_line=%d' % len(bad), bad)
