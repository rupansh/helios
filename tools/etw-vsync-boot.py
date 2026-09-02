#!/usr/bin/env python3
"""Boot-trace view of ROADMAP 3c: when did dxgkrnl start servicing vblank waiters
from its software emulation worker (SignalVSyncEvent) instead of the KMD's vsync
DPC, and what did it ask the driver (DdiControlInterrupt) around that moment.
Input: the HeliosBoot autologger's tracerpt CSV (optionally gzipped).
Usage: tools/etw-vsync-boot.py tmp/hb3.csv.gz"""
import gzip, sys, collections, bisect

def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    rows = []
    with opener(path, "rt", errors="replace") as f:
        f.readline()
        for line in f:
            p = line.split(",")
            if len(p) < 20:
                continue
            try:
                clock = int(p[16].strip())
            except ValueError:
                continue
            ud = ",".join(p[19:]).strip()
            fn = ud.split('"')[1].strip() if ud.startswith('"') else ""
            rows.append((clock, p[2].strip(), p[1].strip(), p[9].strip(), p[10].strip(), fn, ud))
    rows.sort()
    return rows

def main():
    rows = load(sys.argv[1])
    t0 = rows[0][0]
    rel = lambda c: (c - t0) / 1e7
    print(f"{len(rows)} events over {rel(rows[-1][0]):.1f} s")
    dpc = sorted(r[0] for r in rows if r[1] == "105" and r[5] == "DpiDpcForIsr")
    print(f"DPC ticks: {len(dpc)} first at {rel(dpc[0]):.2f} s" if dpc else "no DPC ticks")
    print("\n== DdiControlInterrupt / DdiControlInterrupt2 / interrupt-ish DDI calls ==")
    for r in rows:
        if r[1] in ("105", "106") and ("ControlInterrupt" in r[5] or "SetVidPnSourceVisibility" in r[5] or "CommitVidPn" in r[5]):
            print(f"  {rel(r[0]):9.3f} {r[2]:5s} {r[5]} pid={r[3]}")
    sig = [r for r in rows if "SignalVSyncEvent" in r[6]]
    print(f"\n== SignalVSyncEvent: {len(sig)} events; first at {rel(sig[0][0]):.3f} s" if sig else "\n== SignalVSyncEvent: none")
    for r in sig[:3]:
        print(f"  {rel(r[0]):9.3f} id={r[1]} pid={r[3]} tid={r[4]} {r[6][:140]}")
    for eid in ("1067", "1141", "1142", "1146", "1121", "1122", "1123"):
        ev = [r for r in rows if r[1] == eid]
        if ev:
            print(f"  id {eid}: n={len(ev)} first {rel(ev[0][0]):.3f} s  ud={ev[0][6][:120]}")
    print("\n== vblank waiters over time (10 s buckets): released by DPC (<0.5 ms after a tick) vs late ==")
    waits = collections.defaultdict(lambda: [0, 0, collections.Counter()])
    starts = {}
    for r in rows:
        if r[5] != "DxgkWaitForVerticalBlankEvent":
            continue
        key = (r[3], r[4])
        if r[1] == "105":
            starts[key] = r[0]
        elif r[1] == "106" and key in starts:
            a, b = starts.pop(key), r[0]
            i1 = bisect.bisect_right(dpc, b)
            late = (b - dpc[i1 - 1]) / 1e4 if i1 else 99.0
            bucket = int(rel(b) // 10) * 10
            w = waits[bucket]
            w[0 if late < 0.5 else 1] += 1
            w[2][r[3]] += 1
    for bucket in sorted(waits):
        d, l, pids = waits[bucket]
        print(f"  t={bucket:4d}s: dpc-released={d:4d} late={l:4d} pids={pids.most_common(3)}")
    print("\n== strings mentioning vsync/emul in user data (first 12 distinct, excluding per-tick ids) ==")
    seen = collections.OrderedDict()
    for r in rows:
        if r[1] in ("17", "181", "273", "319", "105", "106", "1067", "1141", "1142", "1146"):
            continue
        u = r[6].lower()
        if "vsync" in u or "emul" in u or "vblank" in u:
            k = (r[1], r[6][:100])
            if k not in seen:
                seen[k] = rel(r[0])
    for (eid, u), t in list(seen.items())[:12]:
        print(f"  {t:9.3f} id={eid} {u}")

if __name__ == "__main__":
    main()
