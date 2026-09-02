#!/usr/bin/env python3
"""One D7 boot cycle: verified QMP system_reset, then watch the host log by
LINE OFFSET for the first dwm-composited primary (blob_size 4587520) and report
any D7 fault lines among the new lines.  Exit 0 = booted clean, 2 = booted
with a D7 fault, 3 = no completion line within the budget."""
import json, socket, subprocess, sys, time
LOG="/tmp/helios-qemu-stderr.log"; SOCK="/tmp/helios-tpm/mon.sock"
FAULT=("invalid res_id","fatal decoder","CS error","vn_dispatch_command failed")
def nlines(): return int(subprocess.check_output(["wc","-l",LOG]).split()[0])
def reset():
    s=socket.socket(socket.AF_UNIX); s.settimeout(20); s.connect(SOCK)
    buf=b""
    def read_until(pred):
        nonlocal buf
        while True:
            nl=buf.find(b"\n")
            if nl>=0:
                line,buf=buf[:nl],buf[nl+1:]
                if line.strip():
                    m=json.loads(line)
                    if pred(m): return m
                continue
            c=s.recv(65536)
            if not c: raise EOFError
            buf+=c
    read_until(lambda m:"QMP" in m)
    s.sendall(b'{"execute":"qmp_capabilities"}\n'); read_until(lambda m:"return" in m)
    s.sendall(b'{"execute":"system_reset"}\n')
    ev=read_until(lambda m:m.get("event")=="RESET")
    s.close(); return ev
def new_lines(base):
    out=subprocess.check_output(["tail","-n","+%d"%(base+1),LOG]).decode(errors="replace").splitlines()
    return out
budget=float(sys.argv[1]) if len(sys.argv)>1 else 300
settle=float(sys.argv[2]) if len(sys.argv)>2 else 45
base=nlines(); t0=time.time()
ev=reset(); print("reset verified: %s base=%d"%(ev.get("event"),base), flush=True)
done=None
while time.time()-t0<budget:
    time.sleep(3)
    lines=new_lines(base)
    for i,l in enumerate(lines):
        if "helios_scanout_blob_layout" in l and "blob_size 4587520" in l:
            done=(i,l); break
    if done: break
lines=new_lines(base)
faults=[l for l in lines if any(f in l for f in FAULT)]
# a settle window after the first composited frame: the two seen faults hit ~1-2 s after it
if done:
    time.sleep(settle)
    lines=new_lines(base); faults=[l for l in lines if any(f in l for f in FAULT)]
print("elapsed=%.0fs newlines=%d completion=%s faults=%d"%(time.time()-t0,len(lines),"yes" if done else "NO",len(faults)))
for l in faults[:12]: print("  "+l[:160])
sys.exit(3 if not done else (2 if faults else 0))
