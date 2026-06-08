# icd/win-build — Windows build support for the Mesa venus ICD

Out-of-tree scaffolding for compiling the vendored Mesa `venus` driver
(`icd/mesa`, Phase 5) on Windows **without editing the Mesa submodule** — venus
stays byte-identical to upstream so it can be re-synced. Full recipe + the
ranked toolchain decision are in [`../PHASE5_HANDOVER.md`](../PHASE5_HANDOVER.md) §6.

- **`helios_win_compat.h`** — the single forced-include (`-include` / `/FI`) that
  makes venus compile on Windows: `pid_t`, the clang-cl interlocked-intrinsic
  aliases, and the `sync_wait`/`sync_valid_fd` libsync stubs. Each block self-gates
  to the toolchain that needs it. ⚠️ the sync stubs are PLACEHOLDERS — real fence
  waits route through the KMD `WAIT_FENCE` IOCTL once `vn_renderer_helios.c` exists.
- **`mingw-native.ini`** — meson native file for **mingw-w64 gcc** (RECOMMENDED:
  cleanest, builds straight from `Z:\`, gcc-native GNU extensions; validated to
  compile 100% of venus with zero Mesa edits). Paths are VM-specific.
- **`clang-cl-native.ini`** — meson native file for **clang-cl** (also validated to
  compile 100% of venus, but needs a local C: source mirror + an STL-mismatch
  define + the SDK `rc.exe`; the alternative, not the default).

Both toolchains reach the final link step; the remaining undefined symbols
(`vn_renderer_create_vtest`, the SPIR-V→NIR `vtn_*`) are the unwritten
`vn_renderer_helios.c` backend + libvtn wiring — real Phase 5 work, not portability.

Quick build (mingw, from the win-mcp `win_meson` tool):
```
win_meson(["setup","C:\\Users\\Rupansh\\helios-mesa-build","Z:\\icd\\mesa",
  "--native-file","Z:\\icd\\win-build\\mingw-native.ini",
  "-Dc_args=-includeZ:\\icd\\win-build\\helios_win_compat.h",
  "-Dvulkan-drivers=virtio","-Dgallium-drivers=","-Dplatforms=windows","-Dvideo-codecs=",
  "-Dvulkan-layers=","-Degl=disabled","-Dgbm=disabled","-Dglx=disabled","-Dopengl=false",
  "-Dgles1=disabled","-Dgles2=disabled","-Dllvm=disabled","-Dshader-cache=disabled",
  "-Dbuild-tests=false","-Dperfetto=false","--buildtype=debugoptimized"])
win_meson([])   # => compile
```

Install the built ICD into the canonical loader location from an elevated
PowerShell in the guest:

```
Z:\tools\install-helios-icd.ps1
```

The installer copies `vulkan_virtio.dll` from
`C:\Users\Rupansh\helios-mesa-build\src\virtio\vulkan\` to a content-hashed
ProgramData filename such as `vulkan_virtio-ed15edb62e6d.dll`, writes
`C:\ProgramData\HeliosVulkan\virtio_devenv_icd.x86_64.json` with
`library_path` pointing at that versioned DLL, removes stale Helios/Virtio Vulkan registry values, and registers the manifest under
`HKLM\SOFTWARE\Khronos\Vulkan\Drivers`. Use that ProgramData JSON for
`VK_DRIVER_FILES`; do not point tests at Mesa build-tree JSONs or old
`helios-mesa-mingw` paths. The versioned DLL avoids failed installs when the
previous ICD DLL is still mapped by a running process.
