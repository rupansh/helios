#!/usr/bin/env bash
# Select this executable through HELIOS_QEMU_BIN when the owner restarts QEMU.
# The ordinary launcher crosses sudo boundaries, which strip LD_LIBRARY_PATH.
# Set the library path here, after those transitions, for QEMU and its server.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
prefix="$repo/target/linux/virglrenderer-install"
qemu="$repo/qemu-helios/build-helios/qemu-system-x86_64"
for artifact in "$qemu" "$prefix/lib/libvirglrenderer.so.1" "$prefix/libexec/virgl_render_server"; do
    [[ -r "$artifact" ]] || { echo "Missing paired renderer artifact: $artifact" >&2; exit 1; }
done
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RENDER_SERVER_EXEC_PATH="$prefix/libexec/virgl_render_server"
export QEMU_MODULE_DIR="${QEMU_MODULE_DIR:-$repo/qemu-helios/build-helios}"
exec "$qemu" "$@"
