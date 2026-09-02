#!/usr/bin/env python3
"""Per-process present flow from a Microsoft-Windows-DxgKrnl CSV (tracerpt -of CSV -y, optionally
gzipped): for the given pids, counts of dxgkrnl function Starts by name and a per-second timeline of
Present/Flip/QueuePacket/DmaPacket/PresentHistory activity. Usage: etw-present-report.py <csv.gz> <pid> [pid...]
(pids in decimal; the CSV carries them as 0x… hex)."""
import gzip, sys, collections
FUNC_START, FUNC_STOP = "105", "106"
def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    rows = []
    with opener(path, "rt", errors="replace") as f:
        f.readline()
        for line in f:
            p = line.split(",")
            if len(p) < 18: continue
            try: clock = int(p[16].strip())
            except ValueError: continue
            rows.append((clock, p[2].strip(), p[9].strip(), p[10].strip(), ",".join(p[19:]).strip()))
    return rows
def fname(ud): return ud.split('"')[1].strip() if ud.startswith('"') else None
def main():
    rows = load(sys.argv[1]); pids = {"0x%08X" % int(x) for x in sys.argv[2:]}
    t0 = min(r[0] for r in rows); ticks = 10_000_000.0
    per = collections.defaultdict(collections.Counter); ids = collections.defaultdict(collections.Counter)
    timeline = collections.defaultdict(lambda: collections.defaultdict(collections.Counter))
    keys = ("Present", "Flip", "PresentHistory", "QueuePacket", "SubmitCommand", "Render", "DmaPacket")
    for clock, eid, pid, tid, ud in rows:
        if pid not in pids: continue
        sec = int((clock - t0) / ticks)
        ids[pid][eid] += 1
        if eid == FUNC_START:
            n = fname(ud) or "?"; per[pid][n] += 1
            for k in keys:
                if k in n: timeline[pid][sec][n] += 1
        elif eid in ("178",):
            timeline[pid][sec]["QueuePacket(178)"] += 1
    for pid in sorted(per):
        print("== pid", int(pid, 16), "=="); 
        for n, c in per[pid].most_common(30): print("  %6d  %s" % (c, n))
        print("  event ids:", " ".join("%s:%d" % kv for kv in ids[pid].most_common(12)))
        for sec in sorted(timeline[pid]):
            print("  t=%2ds " % sec + " ".join("%s=%d" % kv for kv in sorted(timeline[pid][sec].items())))
if __name__ == "__main__": main()
