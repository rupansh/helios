#!/usr/bin/env python3
"""One D7 boot cycle: verified QMP system_reset, then watch the host log by
LINE OFFSET for the first dwm-composited primary (blob_size 4587520) and report
any D7 fault lines among the new lines.  Exit 0 = booted clean, 2 = booted
with a D7 fault, 3 = no completion line within the budget, 4 = QEMU exited
(the monitor closed) — its QMP events say whether the guest asked for it."""
import json, socket, subprocess, sys, time
LOG="/tmp/helios-qemu-stderr.log"; SOCK="/tmp/helios-tpm/mon.sock"
FAULT=("invalid res_id","fatal decoder","CS error","vn_dispatch_command failed")
def nlines(): return int(subprocess.check_output(["wc","-l",LOG]).split()[0])
class Qmp:
    """One QMP client kept open for the whole watch so every event QEMU emits
    after the reset (RESET guest=?, SHUTDOWN guest=?/reason, GUEST_PANICKED,
    STOP, WATCHDOG) is on the record; a silent QEMU exit shows as EOF."""
    def __init__(self):
        self.s=socket.socket(socket.AF_UNIX); self.s.settimeout(20); self.s.connect(SOCK)
        self.buf=b""; self.events=[]; self.eof=False
        self.read_until(lambda m:"QMP" in m)
        self.s.sendall(b'{"execute":"qmp_capabilities"}\n'); self.read_until(lambda m:"return" in m)
    def _lines(self):
        while True:
            nl=self.buf.find(b"\n")
            if nl<0: return
            line,self.buf=self.buf[:nl],self.buf[nl+1:]
            if line.strip(): yield json.loads(line)
    def read_until(self,pred):
        while True:
            for m in self._lines():
                if "event" in m: self.events.append((time.time(),m))
                if pred(m): return m
            c=self.s.recv(65536)
            if not c: self.eof=True; raise EOFError
            self.buf+=c
    def poll(self,seconds):
        """Drain events for up to `seconds`; returns False once QEMU hung up."""
        end=time.time()+seconds
        while time.time()<end and not self.eof:
            try:
                self.s.settimeout(max(0.05,end-time.time()))
                c=self.s.recv(65536)
            except socket.timeout:
                break
            except OSError:
                self.eof=True; break
            if not c: self.eof=True; break
            self.buf+=c
            for m in self._lines():
                if "event" in m: self.events.append((time.time(),m))
        return not self.eof
    def reset(self):
        self.s.sendall(b'{"execute":"system_reset"}\n')
        return self.read_until(lambda m:m.get("event")=="RESET")
    def dump(self):
        for t,m in self.events:
            print("  qmp %s %s %s"%(time.strftime("%H:%M:%S",time.localtime(t)),m.get("event"),json.dumps(m.get("data",{}))))
        if self.eof: print("  qmp EOF: QEMU closed the monitor (process exit)")
def new_lines(base):
    out=subprocess.check_output(["tail","-n","+%d"%(base+1),LOG]).decode(errors="replace").splitlines()
    return out
budget=float(sys.argv[1]) if len(sys.argv)>1 else 300
settle=float(sys.argv[2]) if len(sys.argv)>2 else 45
base=nlines(); t0=time.time()
q=Qmp(); ev=q.reset(); print("reset verified: %s guest=%s base=%d"%(ev.get("event"),ev.get("data",{}).get("guest"),base), flush=True)
done=None
while time.time()-t0<budget:
    if not q.poll(3):
        print("QEMU monitor closed %.0fs after the reset"%(time.time()-t0)); break
    lines=new_lines(base)
    for i,l in enumerate(lines):
        if "helios_scanout_blob_layout" in l and "blob_size 4587520" in l:
            done=(i,l); break
    if done: break
lines=new_lines(base)
faults=[l for l in lines if any(f in l for f in FAULT)]
# a settle window after the first composited frame: the two seen faults hit ~1-2 s after it
if done:
    q.poll(settle)
    lines=new_lines(base); faults=[l for l in lines if any(f in l for f in FAULT)]
print("elapsed=%.0fs newlines=%d completion=%s faults=%d qmp_events=%d"%(time.time()-t0,len(lines),"yes" if done else "NO",len(faults),len(q.events)))
q.dump()
for l in faults[:12]: print("  "+l[:160])
try: q.s.close()
except OSError: pass
sys.exit(4 if q.eof else (3 if not done else (2 if faults else 0)))
