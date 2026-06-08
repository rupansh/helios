# Looking Glass + Helios Integration Goal

**Date:** 2026-06-08

The current display experiment is to make the Looking Glass IDD producer mirror completed desktop frames into
the Helios System-class Venus path, while keeping the normal Looking Glass ivshmem client stream intact.

## Current direction

- Keep the existing System-class Helios KMD and Mesa Venus ICD as the renderer/output path.
- Use the Looking Glass IDD swapchain path as the desktop-frame producer.
- Add an optional IDD-side sink (`HKLM\SOFTWARE\LookingGlass\IDD\HeliosEnable=1`) that:
  1. creates a persistent exportable Venus-backed Vulkan image through the Helios ICD;
  2. mirrors completed IDD/KVMFR BGRA frame bytes into the image's mapped memory;
  3. submits an empty Vulkan queue operation so the Helios ICD flushes cached coherent mappings;
  4. calls `IOCTL_HELIOS_PRESENT_BLOB` so the KMD issues `SET_SCANOUT_BLOB` + `RESOURCE_FLUSH`.
- Preserve the standard Looking Glass LGMP/ivshmem frame path for debugging and fallback.
- Keep the Helios sink off by default so the upstream IDD behavior is unchanged unless the registry switch is set.

The earlier Windows host-server sink (`helios.enable=yes`) remains useful as a reference, but the active fast path
now lives in `LookingGlass\idd\LGIdd\CHeliosSink.cpp` and is called after the D3D12 frame copy completes in
`CSwapChainProcessor::CompletionFunction`.

## Runtime switches

Registry path:

```text
HKLM\SOFTWARE\LookingGlass\IDD
```

Values:

```text
HeliosEnable  DWORD   1 to enable the IDD Vulkan mirror path; default is disabled.
HeliosGateFile REG_SZ Path used for the temporary Venus allocation-size -> resource-id handoff.
HeliosGpu      REG_SZ Substring used to select the Venus physical device, default "Intel".
```

For WUDFHost/LocalService, keep `HeliosGateFile` under a service-writable directory, not a user profile:

```text
HeliosGateFile = C:\ProgramData\HeliosVulkan\helios_lg_idd_resid.txt
```

The Mesa Helios renderer reads `HELIOS_GATE_RESID_FILE` with `GetEnvironmentVariableA`, not CRT `getenv()`, so the
handoff still works when the ICD is statically linked and loaded into a process using a different CRT.

Client-side SPICE display fallback is now opt-in with `spice:display=yes`. For Helios/IDD testing, keep the client
on the KVMFR stream while still allowing SPICE input:

```text
LookingGlass/client/build/looking-glass-client app:shmFile=/dev/kvmfr0 spice:input=yes spice:display=no
```

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

The IDD driver is a WDK/MSBuild project, not part of the `win_looking_glass` host-server CMake build. Build it with
the dedicated MCP helper:

```text
win_looking_glass_idd {}
```

`win_looking_glass_idd` mirrors the tree to local NTFS with robocopy, excludes `icd\mesa`, copies only
`icd\mesa\include\vulkan` for the IDD Vulkan headers, then runs MSBuild on
`C:\Users\Rupansh\helios-vgpu\LookingGlass\idd\LGIdd.sln`. This avoids both WDK build I/O on `Z:\` and Windows
reserved-name failures from the full Mesa include tree.

The current IDD build completes and emits:

```text
C:\Users\Rupansh\helios-vgpu\LookingGlass\idd\x64\Release\LGIdd\LGIdd.dll
C:\Users\Rupansh\helios-vgpu\LookingGlass\idd\x64\Release\LGIdd\LGIdd.inf
C:\Users\Rupansh\helios-vgpu\LookingGlass\idd\x64\Release\LGIdd\lgidd.cat
```

## Service-safe Vulkan ICD

The IDD runs inside WUDFHost as `NT AUTHORITY\LOCAL SERVICE`. Elevated/service Vulkan loader paths ignore
`VK_DRIVER_FILES`, so the Helios ICD must be registered in HKLM and loadable from a service-readable directory.

Current working deployment:

```text
C:\ProgramData\HeliosVulkan\vulkan_virtio.dll
C:\ProgramData\HeliosVulkan\virtio_devenv_icd.x86_64.json
HKLM\SOFTWARE\Khronos\Vulkan\Drivers
  C:\ProgramData\HeliosVulkan\virtio_devenv_icd.x86_64.json = DWORD 0
```

The JSON uses a relative library path:

```json
{
  "file_format_version": "1.0.1",
  "ICD": {
    "api_version": "1.4.352",
    "library_arch": "64",
    "library_path": ".\\vulkan_virtio.dll"
  }
}
```

`icd/win-build/mingw-native.ini` links the Mesa ICD with the MinGW runtime statically (`-static`,
`-static-libgcc`, `-static-libstdc++`). This removes the `libwinpthread-1.dll` runtime dependency that prevented
LocalService from loading `vulkan_virtio.dll` with loader error 126.

Validation command used through a LocalService scheduled task:

```text
C:\Windows\System32\vulkaninfo.exe --summary
```

Expected result:

```text
GPU0: Virtio-GPU Venus (Intel(R) Graphics (ARL)), driverName=venus
GPU1: Virtio-GPU Venus (llvmpipe ...), driverName=venus
```

## Current validation

Validated on 2026-06-08:

- `vulkaninfo --summary` works both interactively and as LocalService using the ProgramData ICD registration.
- Restarting the Looking Glass IDD initializes the Helios Vulkan sink inside WUDFHost.
- The IDD sink creates a persistent 1920x1080 BGRA Venus scanout image:

```text
Helios: IDD scanout image 1920x1080 res_id=4180 stride=7680 offset=0 memFlags=0xf
```

- A short git-built Looking Glass client run stays on the IDD/KVMFR stream:

```text
LookingGlass/client/build/looking-glass-client app:shmFile=/dev/kvmfr0 spice:input=yes spice:display=no win:disableWaitingMessage=yes
```

The IDD log showed no `PRESENT_BLOB` failure during that smoke run.

## Open items

- Decide whether `HELIOS_GATE_RESID_FILE` remains acceptable for the prototype or should be replaced by a real
  Helios Vulkan extension / private query API that returns the backing virtio resource id for a memory object.
- Replace the prototype CPU copy with a true fast path: copy or alias the completed IDD D3D12 frame into a
  Venus-exportable image without a CPU round trip, then present the same blob via `IOCTL_HELIOS_PRESENT_BLOB`.
- Investigate the transient `IddCxSwapChainSetDevice` keyed-mutex-abandoned log seen immediately after IDD
  restart; the second swapchain start recovered and produced frames.
