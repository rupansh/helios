#!/usr/bin/env python3
"""Present-rate metric for a flip app from the host QEMU log: set_scanout_blob
intervals inside a UTC window. Usage:
  tools/hostlog-present-rate.py <label> <t0 iso utc> <t1 iso utc> [...]"""
import re, sys, bisect
LOG = "/tmp/helios-qemu-stderr.log"
ts = []
for line in open(LOG, errors="replace"):
    if "virtio_gpu_cmd_set_scanout_blob" in line:
        m = re.match(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+)Z", line)
        if m:
            s = m.group(1)
            h, mi, sec = s[11:13], s[14:16], s[17:]
            ts.append((s[:10], int(h) * 3600 + int(mi) * 60 + float(sec)))
def key(iso):
    iso = iso.rstrip("Z")
    d, t = iso.split("T")
    h, mi, sec = t.split(":")
    return (d, int(h) * 3600 + int(mi) * 60 + float(sec[:9]))
args = sys.argv[1:]
while len(args) >= 3:
    label, a, b = args[:3]; args = args[3:]
    ka, kb = key(a), key(b)
    sel = [t for d, t in ts if (d, t) >= ka and (d, t) <= kb]
    if len(sel) < 3:
        print(f"{label}: n={len(sel)}"); continue
    # the app's steady phase: drop the first and last 2 s of the window
    core = [t for t in sel if sel[0] + 2 <= t <= sel[-1] - 2]
    d = sorted(b_ - a_ for a_, b_ in zip(core, core[1:]))
    d_ms = [x * 1000 for x in d]
    rate = len(core) / max(core[-1] - core[0], 1e-9)
    print(f"{label}: flips={len(sel)} core={len(core)} rate={rate:.1f}/s p10={d_ms[len(d_ms)//10]:.1f} p50={d_ms[len(d_ms)//2]:.1f} p90={d_ms[9*len(d_ms)//10]:.1f} ms")
