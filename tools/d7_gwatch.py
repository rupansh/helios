#!/usr/bin/env python3
"""Watch the host log by LINE OFFSET for the guest to finish a GRACEFUL reboot.
No QMP: the guest reboots itself (shutdown /r), so QEMU is never touched and the
unclean-shutdown recovery path a hard `system_reset` intermittently triggers is
avoided. Completion = a new `helios_scanout_blob_layout … blob_size 4587520`
line (dwm composited a real primary). Exit 0 = booted, 3 = no completion in the
budget."""
import subprocess, sys, time
LOG="/tmp/helios-qemu-stderr.log"
def nlines(): return int(subprocess.check_output(["wc","-l",LOG]).split()[0])
def new_lines(base):
    return subprocess.check_output(["tail","-n","+%d"%(base+1),LOG]).decode(errors="replace").splitlines()
base=int(sys.argv[1]) if len(sys.argv)>1 else nlines()
budget=float(sys.argv[2]) if len(sys.argv)>2 else 240
settle=float(sys.argv[3]) if len(sys.argv)>3 else 45
FAULT=("invalid res_id","fatal decoder","CS error","vn_dispatch_command failed")
print("gwatch base=%d budget=%.0f"%(base,budget), flush=True)
t0=time.time(); done=None
while time.time()-t0<budget:
    time.sleep(3)
    for l in new_lines(base):
        if "helios_scanout_blob_layout" in l and "blob_size 4587520" in l:
            done=l; break
    if done: break
if done:
    time.sleep(settle)
lines=new_lines(base); faults=[l for l in lines if any(f in l for f in FAULT)]
print("elapsed=%.0fs newlines=%d completion=%s faults=%d"%(time.time()-t0,len(lines),"yes" if done else "NO",len(faults)))
for l in faults[:12]: print("  "+l[:160])
sys.exit(3 if not done else (2 if faults else 0))
