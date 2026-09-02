#!/usr/bin/env python3
"""Read a tracerpt CSV (optionally gzipped) of a Microsoft-Windows-DxgKrnl slice
taken around tmp/vblank_short.ps1 and answer ROADMAP 3c: how many vsync DPCs
dxgkrnl logs per second, how often the KMD's notify is seen, and how the vblank
waiter's wakes relate to them. dxgkrnl names almost no tasks, so function
Start/Stop (ids 105/106) are keyed by the function name in their user data and
everything else by numeric id. Usage:
  tools/etw-vsync-report.py tmp/vs_a.csv.gz [probe_pid]"""
import gzip, sys, collections

FUNC_START, FUNC_STOP = "105", "106"
VSYNC_IDS = {"17": "VSyncDPC(17)", "181": "id181", "273": "id273", "319": "id319"}
FUNCS = ("DpiDpcForIsr", "DdiNotifyDpc", "VidSchDdiNotifyInterrupt", "DpiFdoMessageInterruptRoutine",
         "DxgkWaitForVerticalBlankEvent", "DxgkCbNotifyInterrupt", "VidSchiProcessIsrCompletedPacket",
         "DdiControlInterrupt", "DxgkDdiControlInterrupt")

def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    rows = []
    with opener(path, "rt", errors="replace") as f:
        header = f.readline()
        for line in f:
            p = line.split(",")
            if len(p) < 18:
                continue
            try:
                clock = int(p[16].strip())
            except ValueError:
                continue
            ud = ",".join(p[19:]).strip()
            rows.append((clock, p[2].strip(), p[1].strip(), p[9].strip(), p[10].strip(), ud))
    return header, rows

def fname(ud):
    return ud.split('"')[1].strip() if ud.startswith('"') else None

def stats(ts, label):
    if len(ts) < 3:
        print(f"{label}: n={len(ts)}")
        return
    d = sorted((b - a) / 1e4 for a, b in zip(ts, ts[1:]))
    rate = len(ts) / max((ts[-1] - ts[0]) / 1e7, 1e-9)
    h = collections.Counter(int(x // 2) * 2 for x in d)
    print(f"{label}: n={len(ts)} rate={rate:.1f}/s p10={d[len(d)//10]:.2f} p50={d[len(d)//2]:.2f} p90={d[9*len(d)//10]:.2f} max={d[-1]:.1f} ms")
    print("   hist(2ms): " + " ".join(f"{k}:{h[k]}" for k in sorted(h)))

def main():
    path = sys.argv[1]
    probe_pid = sys.argv[2] if len(sys.argv) > 2 else None
    header, rows = load(path)
    if not rows:
        sys.exit("no rows")
    span = (rows[-1][0] - rows[0][0]) / 1e7
    print(f"{len(rows)} events over {span:.2f} s")
    census = collections.Counter()
    for clock, eid, typ, pid, tid, ud in rows:
        key = (eid, typ, fname(ud) or "") if eid in (FUNC_START, FUNC_STOP) else (eid, typ, "")
        census[key] += 1
    print("\n== census top 50 (id, type, function) ==")
    for k, n in census.most_common(50):
        print(f"{n:8d}  id={k[0]:<4s} {k[1]:<12s} {k[2]}")
    print("\n== cadence ==")
    for eid, label in VSYNC_IDS.items():
        stats([r[0] for r in rows if r[1] == eid], label)
        ex = [r[5] for r in rows if r[1] == eid][:2]
        for e in ex:
            print(f"   sample: {e[:160]}")
    for fn in FUNCS:
        stats([r[0] for r in rows if r[1] == FUNC_START and fname(r[5]) == fn], f"{fn} Start")
    if probe_pid:
        want = int(probe_pid, 0)
        def is_probe(pid):
            try:
                return int(pid, 0) == want
            except ValueError:
                return False
        print(f"\n== probe pid {probe_pid}: DxgkWaitForVerticalBlankEvent ==")
        st = [r[0] for r in rows if is_probe(r[3]) and r[1] == FUNC_START and fname(r[5]) == "DxgkWaitForVerticalBlankEvent"]
        sp = [r[0] for r in rows if is_probe(r[3]) and r[1] == FUNC_STOP and fname(r[5]) == "DxgkWaitForVerticalBlankEvent"]
        stats(sp, "wait Stop (waiter wakes)")
        stats(st, "wait Start")
        # Align every wait with the DPC ticks (DpiDpcForIsr Start) it spans.
        import bisect
        dpc = sorted(r[0] for r in rows if r[1] == FUNC_START and fname(r[5]) == "DpiDpcForIsr")
        inside = collections.Counter(); wake_lat = []; arm_lat = []; rowsout = []
        for a, b in zip(st, sp):
            i0 = bisect.bisect_left(dpc, a); i1 = bisect.bisect_right(dpc, b)
            n = i1 - i0
            inside[n] += 1
            if n:
                wake_lat.append((b - dpc[i1 - 1]) / 1e4)   # Stop minus last tick inside the wait
                arm_lat.append((dpc[i0] - a) / 1e4)        # first tick after Start minus Start
            prev = (a - dpc[i0 - 1]) / 1e4 if i0 else float('nan')  # Start minus the tick just before it
            rowsout.append((n, (b - a) / 1e4, prev, (b - dpc[i1 - 1]) / 1e4 if n else float('nan')))
        print("   DPC ticks per wait (ticks:waits): " + " ".join(f"{k}:{v}" for k, v in sorted(inside.items())))
        if wake_lat:
            wl = sorted(wake_lat); al = sorted(arm_lat)
            print(f"   Stop - last tick: p10={wl[len(wl)//10]:.2f} p50={wl[len(wl)//2]:.2f} p90={wl[9*len(wl)//10]:.2f} max={wl[-1]:.2f} ms")
            print(f"   first tick - Start: p10={al[len(al)//10]:.2f} p50={al[len(al)//2]:.2f} p90={al[9*len(al)//10]:.2f} ms")
        print("   first 36 waits (ticks, dur_ms, Start-prevTick_ms, Stop-lastTick_ms):")
        print("   " + " | ".join(f"{n} {d:.1f} {pv:.1f} {wk:.1f}" for n, d, pv, wk in rowsout[:36]))
        # split: waits whose Start came within X ms after a tick vs later
        by_prev = collections.defaultdict(list)
        for n, d, pv, wk in rowsout:
            if pv == pv:
                by_prev[min(int(pv // 2) * 2, 16)].append(n)
        print("   ticks-per-wait by (Start - prevTick) 2ms bucket: " + " ".join(f"{k}ms:{collections.Counter(v).most_common()}" for k, v in sorted(by_prev.items())))
        other = collections.Counter(fname(r[5]) or r[1] for r in rows if is_probe(r[3]) and r[1] == FUNC_START)
        print("   probe's other dxgkrnl calls: " + " ".join(f"{k}={v}" for k, v in other.most_common(8)))
    print("\nheader: " + header.strip()[:200])

if __name__ == "__main__":
    main()
