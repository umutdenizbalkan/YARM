"""SCHED-LOCK-FAIR1 negative control: mutate one fact in a real witness log (every copy of the
record), recompute its CRC, write the result. Modes:
  reacq — 1L[0]'s rapid re-acquisition drew the queued peer's ticket (it overtook the peer);
  gap   — 1L[1]'s FAIR_STAT reports one service gap."""
import re
import sys
import zlib

src, dst, mode = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(src, encoding="utf-8", errors="replace").read()
REC = re.compile(r"^(SL1W_R seq=\d+ kind=(\w+) cpu=(\d+) exec=\d+ f0=(\d+) f1=(\d+) f2=(\d+) f3=(\d+) f4=(\d+) f5=(\d+)) crc=[0-9a-f]{8}$")


def seal(body):
    return "%s crc=%08x" % (body, zlib.crc32(body.encode()) & 0xFFFFFFFF)


lines = text.split("\n")
peer_ty = {}
for l in lines:
    m = REC.match(l)
    if m and m.group(2) == "FAIR_PEER" and m.group(3) == "0":
        peer_ty[m.group(4)] = int(m.group(5))
changed = 0
out = []
for l in lines:
    m = REC.match(l)
    if m:
        kind, cpu, f = m.group(2), m.group(3), [int(m.group(i)) for i in range(4, 10)]
        body = m.group(1)
        if mode == "reacq" and kind == "FAIR_REACQ" and cpu == "0" and str(f[0]) in peer_ty and f[2] == 1:
            ty = peer_ty[str(f[0])]
            body = re.sub(r" f1=\d+ ", " f1=%d " % ty, body)
            body = re.sub(r" f3=\d+ ", " f3=%d " % ty, body)
            l = seal(body)
            changed += 1
        elif mode == "gap" and kind == "FAIR_STAT" and cpu == "1":
            body = re.sub(r" f3=\d+ ", " f3=1 ", body)
            l = seal(body)
            changed += 1
    out.append(l)
if not changed:
    sys.exit("no record to mutate for mode %s" % mode)
open(dst, "w").write("\n".join(out))
print("mutated %d record copies (%s)" % (changed, mode))
