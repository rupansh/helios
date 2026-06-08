#!/usr/bin/env bash
# tools/launch-helios-gtk.sh — standalone QEMU for win11.
#
# Default mode keeps the original GTK GL console:
#   sudo bash tools/launch-helios-gtk.sh
#
# Looking Glass mode adds KVMFR ivshmem + SPICE input and starts the git-built
# client on the host:
#   sudo HELIOS_DISPLAY=looking-glass bash tools/launch-helios-gtk.sh
set -uo pipefail
USER_NAME=${SUDO_USER:-rupansh}; USER_UID=$(id -u "$USER_NAME")
DISK=/var/lib/libvirt/images/win11.qcow2; NVRAM=/var/lib/libvirt/qemu/nvram/win11_VARS.fd
SWSRC=/var/lib/libvirt/swtpm/bfe8dc1f-8c5b-435c-8045-1ef3a5c19053/tpm2; TPMDIR=/tmp/helios-tpm; SHARE=/home/rupansh/helios-vgpu
DISPLAY_MODE=${HELIOS_DISPLAY:-gtk}
KVMFR_DEV=${HELIOS_KVMFR_DEV:-/dev/kvmfr0}
KVMFR_SIZE=${HELIOS_KVMFR_SIZE:-134217728}
LG_CLIENT=${HELIOS_LG_CLIENT:-$SHARE/LookingGlass/client/build/looking-glass-client}
LG_RENDER_GPU=${HELIOS_LG_RENDER_GPU:-intel}
LG_ALLOW_DMA=${HELIOS_LG_ALLOW_DMA:-no}
LG_START_CLIENT=${HELIOS_LG_START_CLIENT:-yes}
LG_CLIENT_DELAY=${HELIOS_LG_CLIENT_DELAY:-12}
if [ "${HELIOS_PHASE:-}" != "user" ]; then
  [ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }
  cleanup() { ip link del heltap0 2>/dev/null||true; chown libvirt-qemu:libvirt-qemu "$DISK" "$NVRAM" 2>/dev/null||true; }
  trap cleanup EXIT INT TERM
  chown "$USER_NAME" "$DISK" "$NVRAM"
  if [ "$DISPLAY_MODE" = "looking-glass" ]; then
    [ -e "$KVMFR_DEV" ] || { echo "missing $KVMFR_DEV (load kvmfr first)"; exit 1; }
    chown "$USER_NAME" "$KVMFR_DEV" 2>/dev/null || true
  fi
  mkdir -p "$TPMDIR/state"; cp -a "$SWSRC"/. "$TPMDIR/state/" 2>/dev/null || echo "WARN fresh TPM"
  chown -R "$USER_NAME" "$TPMDIR"
  ip link del heltap0 2>/dev/null||true; ip tuntap add dev heltap0 mode tap user "$USER_NAME"
  ip link set heltap0 master virbr0 && ip link set heltap0 up
  sudo -u "$USER_NAME" env HELIOS_PHASE=user XDG_RUNTIME_DIR=/run/user/$USER_UID \
    WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-wayland-1} GDK_BACKEND=wayland \
    HELIOS_DISPLAY="$DISPLAY_MODE" HELIOS_KVMFR_DEV="$KVMFR_DEV" \
    HELIOS_KVMFR_SIZE="$KVMFR_SIZE" HELIOS_LG_CLIENT="$LG_CLIENT" \
    HELIOS_LG_RENDER_GPU="$LG_RENDER_GPU" HELIOS_LG_ALLOW_DMA="$LG_ALLOW_DMA" \
    HELIOS_LG_START_CLIENT="$LG_START_CLIENT" HELIOS_LG_CLIENT_DELAY="$LG_CLIENT_DELAY" \
    bash "$0"
  exit $?
fi
# ---- user phase (desktop user, native Wayland) ----
pkill -f 'virtiofsd.*helios-tpm' 2>/dev/null||true; pkill -f 'swtpm.*helios-tpm' 2>/dev/null||true
/usr/lib/virtiofsd --shared-dir "$SHARE" --socket-path "$TPMDIR/fs.sock" --tag helios-vgpu --sandbox none &
/usr/bin/swtpm socket --tpmstate dir="$TPMDIR/state" --ctrl type=unixio,path="$TPMDIR/swtpm-sock" --tpm2 --daemon
sleep 1

qemu_display_args=(-display gtk,gl=on)
qemu_lg_args=()
lg_client_args=()
if [ "$DISPLAY_MODE" = "looking-glass" ]; then
  qemu_display_args=(-display egl-headless)
  qemu_lg_args=(
    -spice port=5900,addr=127.0.0.1,disable-ticketing=on,image-compression=off
    -chardev spicevmc,id=charchannel0,name=vdagent
    -device '{"driver":"virtserialport","bus":"virtio-serial0.0","nr":1,"chardev":"charchannel0","id":"channel0","name":"com.redhat.spice.0"}'
    -device '{"driver":"ivshmem-plain","id":"shmem0","memdev":"looking-glass"}'
    -object "{\"qom-type\":\"memory-backend-file\",\"id\":\"looking-glass\",\"mem-path\":\"$KVMFR_DEV\",\"size\":$KVMFR_SIZE,\"share\":true}"
  )

  lg_client_args=(
    app:shmFile="$KVMFR_DEV"
    app:allowDMA="$LG_ALLOW_DMA"
    spice:input=yes
    spice:display=no
    win:disableWaitingMessage=yes
  )

  if [ "$LG_RENDER_GPU" = "intel" ]; then
    lg_client_env=(
      __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json
      DRI_PRIME=pci-0000_00_02_0
      MESA_LOADER_DRIVER_OVERRIDE=iris
    )
  else
    lg_client_env=()
  fi

  if [ "$LG_START_CLIENT" = "yes" ]; then
    (
      sleep "$LG_CLIENT_DELAY"
      echo '>>> Starting Looking Glass client <<<'
      exec env "${lg_client_env[@]}" "$LG_CLIENT" "${lg_client_args[@]}"
    ) &
  fi
fi

echo ">>> QEMU ($DISPLAY_MODE, virtio-gpu-gl-pci, native Wayland). Z:\\ = repo. SSH 192.168.122.120 <<<"
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
  "${qemu_lg_args[@]}" \
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
  -device \
  '{"driver":"virtio-gpu-gl-pci","id":"ua-heliosgpu","max_outputs":1,"bus":"pci.8","addr":"0x0","venus":true,"blob":true,"hostmem":4294967296,"max_hostmem":4294967296}' \
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
  "${qemu_display_args[@]}"
