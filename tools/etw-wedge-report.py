#!/usr/bin/env python3
"""Name the first dxgkrnl error in a HeliosBoot ETW capture and the call that
never returned.

The session-wedge investigation cost most of 2026-08-27 because the interesting
events are 12 lines out of 340k. This reduces a boot capture to the three
things that decide the diagnosis:

  1. every dxgkrnl function Start with no Stop  -> the blocked thread(s)
  2. every VidSchError* event                   -> the FIRST error, with reason
  3. the surrounding microsecond timeline       -> what caused it

Input is the CSV that `tracerpt <etl> -o hb.csv -of CSV -y` writes, optionally
gzipped. Produce it on the guest and gzip it across the share:

    tracerpt C:\\heliosboot.etl -o C:\\hb.csv -of CSV -y
    # then GZipStream C:\\hb.csv -> Z:\\tmp\\hb.csv.gz   (140 MB -> 8 MB)

    tools/etw-wedge-report.py tmp/hb.csv.gz

dxgkrnl's manifest names almost no tasks (`wevtutil gp Microsoft-Windows-DxgKrnl
/ge /gm` returns 5 of them), so events are identified by numeric id here. The
decoded strings inside the user data -- VidSchError*, DXGK_BLOCK_THREAD_*,
allocation flags -- come from the templates' value maps and ARE reliable.
"""
import gzip, sys, collections

# Event ids that carry no diagnosis and swamp everything else.
PERIODIC_NAMES = {
    "DpiDpcForIsr", "DdiNotifyDpc", "DpiFdoMessageInterruptRoutine",
    "VidSchDdiNotifyInterrupt", "DdiCalibrateGpuClock",
}
FUNC_START, FUNC_STOP = "105", "106"
VIDSCH_ERROR = "467"        # (device, isFatal, "VidSchError<Reason>")
BLOCK_THREAD = "103"        # DXGK_BLOCK_THREAD_<resource>


def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    rows = []
    with opener(path, "rt", errors="replace") as f:
        f.readline()                                   # header
        for line in f:
            p = line.split(",")
            if len(p) < 18:
                continue
            try:
                clock = int(p[16].strip())
            except ValueError:
                continue
            rows.append((clock, p[2].strip(), p[1].strip(), p[7].strip(),
                         p[9].strip(), p[10].strip(), ",".join(p[19:]).strip()))
    return rows


def fname(userdata):
    return userdata.split('"')[1].strip() if userdata.startswith('"') else None


def main(path):
    rows = load(path)
    if not rows:
        sys.exit(f"{path}: no parseable rows")
    t0 = rows[0][0]
    rel = lambda c: (c - t0) / 1e7
    print(f"{len(rows)} events over {rel(rows[-1][0]):.1f} s\n")

    print("== dxgkrnl calls that never returned ==")
    open_calls = {}
    for clock, eid, _typ, _task, pid, tid, ud in rows:
        name = fname(ud)
        if not name:
            continue
        if eid == FUNC_START:
            open_calls.setdefault((pid, tid, name), []).append(clock)
        elif eid == FUNC_STOP and open_calls.get((pid, tid, name)):
            open_calls[(pid, tid, name)].pop()
    stuck = sorted(((c, k) for k, v in open_calls.items() for c in v),
                   key=lambda x: x[0])
    for clock, (pid, tid, name) in stuck:
        print(f"  t={rel(clock):8.4f}s  pid={pid} tid={tid}  {name}")
    if not stuck:
        print("  (none -- this capture does not contain a wedge)")

    print("\n== VidSchError* (the first one is the first cause) ==")
    for clock, eid, _typ, _task, pid, tid, ud in rows:
        if eid == VIDSCH_ERROR:
            print(f"  t={rel(clock):8.4f}s  pid={pid} tid={tid}  {ud[:120]}")

    print("\n== DxgkRender that queued no packet (eid 178 absent in scope) ==")
    # 3113 of 3114 renders queue a packet; the one that does not is the render
    # whose allocation then deadlocks in DestroyAllocation (2026-08-27).
    scope, unqueued = {}, []
    for clock, eid, _typ, _task, pid, tid, ud in rows:
        key = (pid, tid)
        name = fname(ud)
        if eid == FUNC_START and name == "DxgkRender":
            scope[key] = [clock, False]
        elif eid == FUNC_STOP and name == "DxgkRender" and key in scope:
            start, queued = scope.pop(key)
            if not queued:
                unqueued.append((rel(start), pid, tid))
        elif eid == "178" and key in scope:
            scope[key][1] = True
    for t, pid, tid in unqueued:
        print(f"  t={t:8.4f}s  pid={pid} tid={tid}")
    if not unqueued:
        print("  (none)")

    print("\n== DDI durations ==")
    for name in ("DdiSubmitCommand", "DdiRender", "DdiSetVidPnSourceVisibility",
                 "DdiSetVidPnSourceAddressWithMultiPlaneOverlay3",
                 "DdiBuildPagingBuffer", "DdiCreateAllocation",
                 "DdiCloseAllocation", "DdiDestroyAllocation"):
        start, spans = {}, []
        for clock, eid, _typ, _task, pid, tid, ud in rows:
            if fname(ud) != name:
                continue
            if eid == FUNC_START:
                start[(pid, tid)] = clock
            elif eid == FUNC_STOP and (pid, tid) in start:
                spans.append(((clock - start.pop((pid, tid))) / 1e4, rel(clock)))
        if not spans:
            continue
        ms = sorted(s[0] for s in spans)
        worst = max(spans)
        print(f"  {name:<48} n={len(spans):<5} p50={ms[len(ms)//2]:7.3f}ms "
              f"max={worst[0]:9.3f}ms @t={worst[1]:.4f}s")


def window(path, lo, hi, pid=None, tid=None):
    rows = load(path)
    t0 = rows[0][0]
    for clock, eid, typ, task, p, t, ud in rows:
        rt = (clock - t0) / 1e7
        if not lo <= rt <= hi:
            continue
        if (pid and p != pid) or (tid and t != tid):
            continue
        name = fname(ud)
        if name in PERIODIC_NAMES:
            continue
        print(f"{rt:11.7f} eid={eid:>5} {typ:<7} task={task:>4} "
              f"pid={p} tid={t} {name or ud[:170]}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if len(sys.argv) >= 4:                 # csv lo hi [pid [tid]]
        window(sys.argv[1], float(sys.argv[2]), float(sys.argv[3]),
               *sys.argv[4:6])
    else:
        main(sys.argv[1])
