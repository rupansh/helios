#!/usr/bin/env bash
# tools/launch-helios-gtk.sh — standalone QEMU for win11 (native Wayland). Pure
# virtio-gpu-gl-pci (no VGA/QXL) so the Helios DOD is the SOLE (primary) display.
# virtiofs shares the repo as Z:\ so builds work in-VM.
#
#   bash tools/launch-helios-gtk.sh        # run as YOUR user (NOT sudo); libvirt
#                                          # win11 must be shut off first.
#
# Runs as your normal desktop user (NOT sudo) so QEMU inherits the full Wayland/EGL
# session env; the privileged steps (tap on virbr0, chown the libvirt disk/nvram,
# copy the swtpm state) are sudo'd individually below (password prompt once).
#
# ⚠️ UNRESOLVED (next session): `gtk,gl=on` here floods `Gdk-WARNING eglMakeCurrent
# failed` (black window). Observations only, NOT a diagnosis: gl=on does work in a
# separate minimal qemu launch on this host with an ubuntu live boot iso in qemu)
# and running this script as the user
# with the full session env did not change the failure. Root cause is unknown —
# investigate. Until resolved, use a non-GL backend (default below).
#
# Display backend: $HELIOS_DISPLAY (default "gtk" = software display, NO EGL — shows
# the 2D DOD desktop scanout without the gl=on eglMakeCurrent issue). Set
# "gtk,gl=on,show-cursor=on" to reproduce/debug the eglMakeCurrent bug, or "spice".
set -uo pipefail
[ "$(id -u)" -ne 0 ] || { echo "run as your NORMAL user (not sudo/root) — the privileged steps are sudo'd internally"; exit 1; }
USER_NAME=$(id -un); USER_UID=$(id -u)
DISK=/var/lib/libvirt/images/win11.qcow2; NVRAM=/var/lib/libvirt/qemu/nvram/win11_VARS.fd
SWSRC=/var/lib/libvirt/swtpm/bfe8dc1f-8c5b-435c-8045-1ef3a5c19053/tpm2; TPMDIR=/tmp/helios-tpm; SHARE=/home/rupansh/helios-vgpu
cleanup() { sudo ip link del heltap0 2>/dev/null||true; sudo chown libvirt-qemu:libvirt-qemu "$DISK" "$NVRAM" 2>/dev/null||true; }
trap cleanup EXIT INT TERM
echo ">>> privileged setup (sudo: chown disk/nvram + swtpm state + tap on virbr0) <<<"
sudo chown "$USER_NAME" "$DISK" "$NVRAM"
sudo mkdir -p "$TPMDIR/state"; sudo cp -a "$SWSRC"/. "$TPMDIR/state/" 2>/dev/null || echo "WARN fresh TPM"
sudo chown -R "$USER_NAME" "$TPMDIR"
sudo ip link del heltap0 2>/dev/null||true; sudo ip tuntap add dev heltap0 mode tap user "$USER_NAME"
sudo ip link set heltap0 master virbr0 && sudo ip link set heltap0 up
# ---- services + QEMU, in the user's full session env ----
pkill -f 'virtiofsd.*helios-tpm' 2>/dev/null||true; pkill -f 'swtpm.*helios-tpm' 2>/dev/null||true
/usr/lib/virtiofsd --shared-dir "$SHARE" --socket-path "$TPMDIR/fs.sock" --tag helios-vgpu --sandbox none &
/usr/bin/swtpm socket --tpmstate dir="$TPMDIR/state" --ctrl type=unixio,path="$TPMDIR/swtpm-sock" --tpm2 --daemon
sleep 1
# Display backend (HELIOS_DISPLAY): default "gtk" = SOFTWARE display (no EGL), which
# shows the 2D DOD desktop scanout without the unsolved gl=on eglMakeCurrent issue.
# Use "gtk,gl=on,show-cursor=on" to reproduce/debug that bug. "spice" → -spice on
# :5930 (view `remote-viewer spice://127.0.0.1:5930`); HELIOS_SPICE_GL controls its
# GL (default off = software). Anything else is passed verbatim as -display.
DISPLAY_BACKEND="${HELIOS_DISPLAY:-gtk}"
if [ "$DISPLAY_BACKEND" = spice ]; then
  DISP=(-spice "port=5930,addr=127.0.0.1,disable-ticketing=on,gl=${HELIOS_SPICE_GL:-off}" -display none)
  echo '>>> QEMU (spice :5930, virtio-gpu SOLE display). View: remote-viewer spice://127.0.0.1:5930. SSH .120 <<<'
else
  DISP=(-display "$DISPLAY_BACKEND")
  echo ">>> QEMU (display=$DISPLAY_BACKEND, virtio-gpu SOLE display). Z:\\ = repo. SSH .120 <<<"
fi
# GPU device (HELIOS_GPU): "gl" (default) = virtio-gpu-gl-pci + venus/blob/hostmem —
# REQUIRES a GL display backend (spice gl=on or gtk,gl=on); QEMU refuses it on a
# software display. "plain" = virtio-gpu-pci (no venus/GL) — boots with the SOFTWARE
# gtk display, for the 2D DOD desktop / Code-43 bring-up test (the DOD's venus path
# is still a stub, so this loses nothing for now). Both are NON-VGA (what the DOD needs).
if [ "${HELIOS_GPU:-gl}" = plain ]; then
  GPU_DEV=(-device '{"driver":"virtio-gpu-pci","id":"ua-heliosgpu","max_outputs":1,"bus":"pci.8","addr":"0x0"}')
  echo ">>> GPU: virtio-gpu-pci (plain, non-VGA, NO venus/GL — works with the software display) <<<"
else
  GPU_DEV=(-device '{"driver":"virtio-gpu-gl-pci","id":"ua-heliosgpu","max_outputs":1,"bus":"pci.8","addr":"0x0","venus":true,"blob":true,"hostmem":4294967296,"max_hostmem":4294967296}')
fi
exec /usr/bin/qemu-system-x86_64 \
  -name \
  guest=win11,debug-threads=on \
  -blockdev \
  '{"driver":"file","filename":"/usr/share/edk2/x64/OVMF_CODE.4m.fd","node-name":"libvirt-pflash0-storage","auto-read-only":true,"discard":"unmap"}' \
  -blockdev \
  '{"node-name":"libvirt-pflash0-format","read-only":true,"driver":"raw","file":"libvirt-pflash0-storage"}' \
  -blockdev \
  '{"driver":"file","filename":"/var/lib/libvirt/qemu/nvram/win11_VARS.fd","node-name":"libvirt-pflash1-storage","read-only":false}' \
  -machine \
  pc-q35-11.0,usb=off,vmport=off,smm=on,dump-guest-core=off,memory-backend=pc.ram,pflash0=libvirt-pflash0-format,pflash1=libvirt-pflash1-storage,hpet=off,acpi=on \
  -accel \
  kvm \
  -cpu \
  host,migratable=on,hv-time=on,hv-relaxed=on,hv-vapic=on,hv-spinlocks=0x1fff,hv-vpindex=on,hv-runtime=on,hv-synic=on,hv-stimer=on,hv-frequencies=on,hv-tlbflush=on,hv-ipi=on,hv-evmcs=on,hv-avic=on \
  -m \
  size=33554432k \
  -object \
  '{"qom-type":"memory-backend-memfd","id":"pc.ram","share":true,"x-use-canonical-path-for-ramblock-id":false,"size":34359738368}' \
  -overcommit \
  mem-lock=off \
  -smp \
  16,sockets=16,cores=1,threads=1 \
  -uuid \
  bfe8dc1f-8c5b-435c-8045-1ef3a5c19053 \
  -no-user-config \
  -nodefaults \
  -chardev \
  socket,id=charmonitor,path=/tmp/helios-tpm/mon.sock,server=on,wait=off \
  -mon \
  chardev=charmonitor,id=monitor,mode=control \
  -rtc \
  base=localtime,driftfix=slew \
  -global \
  kvm-pit.lost_tick_policy=delay \
  -no-shutdown \
  -global \
  ICH9-LPC.disable_s3=1 \
  -global \
  ICH9-LPC.disable_s4=1 \
  -boot \
  strict=on \
  -device \
  '{"driver":"pcie-root-port","port":16,"chassis":1,"id":"pci.1","bus":"pcie.0","multifunction":true,"addr":"0x2"}' \
  -device \
  '{"driver":"pcie-root-port","port":17,"chassis":2,"id":"pci.2","bus":"pcie.0","addr":"0x2.0x1"}' \
  -device \
  '{"driver":"pcie-root-port","port":18,"chassis":3,"id":"pci.3","bus":"pcie.0","addr":"0x2.0x2"}' \
  -device \
  '{"driver":"pcie-root-port","port":19,"chassis":4,"id":"pci.4","bus":"pcie.0","addr":"0x2.0x3"}' \
  -device \
  '{"driver":"pcie-root-port","port":20,"chassis":5,"id":"pci.5","bus":"pcie.0","addr":"0x2.0x4"}' \
  -device \
  '{"driver":"pcie-root-port","port":21,"chassis":6,"id":"pci.6","bus":"pcie.0","addr":"0x2.0x5"}' \
  -device \
  '{"driver":"pcie-root-port","port":22,"chassis":7,"id":"pci.7","bus":"pcie.0","addr":"0x2.0x6"}' \
  -device \
  '{"driver":"pcie-root-port","port":23,"chassis":8,"id":"pci.8","bus":"pcie.0","addr":"0x2.0x7"}' \
  -device \
  '{"driver":"pcie-root-port","port":24,"chassis":9,"id":"pci.9","bus":"pcie.0","multifunction":true,"addr":"0x3"}' \
  -device \
  '{"driver":"pcie-root-port","port":25,"chassis":10,"id":"pci.10","bus":"pcie.0","addr":"0x3.0x1"}' \
  -device \
  '{"driver":"pcie-root-port","port":26,"chassis":11,"id":"pci.11","bus":"pcie.0","addr":"0x3.0x2"}' \
  -device \
  '{"driver":"pcie-root-port","port":27,"chassis":12,"id":"pci.12","bus":"pcie.0","addr":"0x3.0x3"}' \
  -device \
  '{"driver":"pcie-root-port","port":28,"chassis":13,"id":"pci.13","bus":"pcie.0","addr":"0x3.0x4"}' \
  -device \
  '{"driver":"pcie-root-port","port":29,"chassis":14,"id":"pci.14","bus":"pcie.0","addr":"0x3.0x5"}' \
  -device \
  '{"driver":"pcie-root-port","port":30,"chassis":15,"id":"pci.15","bus":"pcie.0","addr":"0x3.0x6"}' \
  -device \
  '{"driver":"pcie-pci-bridge","id":"pci.16","bus":"pci.5","addr":"0x0"}' \
  -device \
  '{"driver":"qemu-xhci","p2":15,"p3":15,"id":"usb","bus":"pci.2","addr":"0x0"}' \
  -device \
  '{"driver":"virtio-scsi-pci","id":"scsi0","bus":"pci.9","addr":"0x0"}' \
  -device \
  '{"driver":"virtio-serial-pci","id":"virtio-serial0","bus":"pci.3","addr":"0x0"}' \
  -blockdev \
  '{"driver":"file","filename":"/var/lib/libvirt/images/win11.qcow2","node-name":"libvirt-1-storage","auto-read-only":true,"discard":"unmap"}' \
  -blockdev \
  '{"node-name":"libvirt-1-format","read-only":false,"driver":"qcow2","file":"libvirt-1-storage"}' \
  -device \
  '{"driver":"ide-hd","bus":"ide.0","drive":"libvirt-1-format","id":"sata0-0-0","bootindex":1}' \
  -chardev \
  socket,id=chr-vu-fs0,path=/tmp/helios-tpm/fs.sock \
  -device \
  '{"driver":"vhost-user-fs-pci","id":"fs0","chardev":"chr-vu-fs0","tag":"helios-vgpu","bus":"pci.7","addr":"0x0"}' \
  -netdev \
  tap,id=hostnet0,ifname=heltap0,script=no,downscript=no \
  -device \
  '{"driver":"virtio-net-pci","netdev":"hostnet0","id":"net0","mac":"52:54:00:2e:4b:35","bus":"pci.1","addr":"0x0"}' \
  -chardev \
  pty,id=charserial0 \
  -device \
  '{"driver":"isa-serial","chardev":"charserial0","id":"serial0","index":0}' \
  -chardev \
  socket,id=chrtpm,path=/tmp/helios-tpm/swtpm-sock \
  -tpmdev \
  emulator,id=tpm-tpm0,chardev=chrtpm \
  -device \
  '{"driver":"tpm-crb","tpmdev":"tpm-tpm0","id":"tpm0"}' \
  -device \
  '{"driver":"usb-tablet","id":"input2","bus":"usb.0","port":"1"}' \
  "${GPU_DEV[@]}" \
  -global \
  ICH9-LPC.noreboot=off \
  -watchdog-action \
  reset \
  -device \
  '{"driver":"virtio-balloon-pci","id":"balloon0","bus":"pci.4","addr":"0x0"}' \
  -accel \
  kvm,honor-guest-pat=on \
  -d \
  guest_errors \
  -sandbox \
  off \
  -msg \
  timestamp=on \
  "${DISP[@]}"
