# Looking Glass + Helios Integration Goal

**Date:** 2026-06-08

The next display experiment is to make the Looking Glass Windows host server mirror captured IDD/DXGI frames into
the Helios System-class Venus path, while keeping the normal Looking Glass ivshmem client stream intact.

## Current direction

- Keep the existing System-class Helios KMD and Mesa Venus ICD as the renderer/output path.
- Use Looking Glass host capture as the desktop-frame producer.
- Add an optional Looking Glass host sink (`helios.enable=yes`) that:
  1. creates a persistent exportable Venus-backed Vulkan image through the Helios ICD;
  2. copies captured BGRA frame bytes into the image's mapped memory;
  3. submits an empty Vulkan queue operation so the Helios ICD flushes cached coherent mappings;
  4. calls `IOCTL_HELIOS_PRESENT_BLOB` so the KMD issues `SET_SCANOUT_BLOB` + `RESOURCE_FLUSH`.
- Preserve the standard Looking Glass LGMP/ivshmem frame path for debugging and fallback.

## Build tooling

`tools/win-mcp` now has a dedicated `win_looking_glass` tool. It mirrors the Linux source tree to the local Windows
build mirror with `robocopy`, configures `LookingGlass\host` with CMake/Ninja, and builds the Windows host from
local disk. The current MCP process must be restarted before the new tool appears in the available tool schema.

Default behavior:

```text
source_root: Z:\
mirror:      C:\Users\Rupansh\helios-vgpu
build dir:   C:\Users\Rupansh\helios-lookingglass-host-build
configure:   cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo -DUSE_NVFBC=OFF
```

If the VM source share is not mounted as `Z:\`, call `win_looking_glass` with `source_root` set to the active share
path.

The Windows VM currently sees the repository at `Z:\` again after updating the host virtiofsd package. The
dedicated MCP build path has also been verified manually with the same commands used by `win_looking_glass`.
The tool normalizes `source_root` before forming the absolute `icd\mesa` exclude path; without that normalization,
`Z:\` became `Z:\\icd\mesa`, robocopy did not exclude Mesa, and the sync failed on Linux-only Mesa filenames.

```text
robocopy Z:\ C:\Users\Rupansh\helios-vgpu /MIR /XD target .git icd\mesa Z:\icd\mesa ...
cmake -S C:\Users\Rupansh\helios-vgpu\LookingGlass\host \
      -B C:\Users\Rupansh\helios-lookingglass-host-build \
      -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo -DUSE_NVFBC=OFF
cmake --build C:\Users\Rupansh\helios-lookingglass-host-build
```

Result:

```text
[15/15] Linking C executable looking-glass-host.exe
```

One upstream Looking Glass source file needed a MinGW `-Werror=format-truncation` fix in the DXGI RGB24
postprocessor before the Windows host would compile.

## Open items

- Validate the Vulkan sink against the current Helios ICD JSON.
- Decide whether `HELIOS_GATE_RESID_FILE` remains acceptable for the prototype or should be replaced by a real
  Helios Vulkan extension / private query API that returns the backing virtio resource id for a memory object.
- If CPU-copying into the mapped scanout image is too slow, move the sink from CPU copy to GPU copy from the
  capture backend's D3D resource into an interop/export path.
