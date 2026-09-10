# Windows CI package

The `Windows graphics and compute bundle` GitHub Actions workflow builds one
x64 Windows archive that turns a clean Helios Windows 11 guest into a
system-wide graphics/compute installation. It includes x86 Vulkan/OpenGL
components for WoW64 applications alongside the native x64 stack.

The 2026-09-09 native-DGC source requires the paired renderer/protocol fork
described in [NATIVE_DGC.md](docs/dx12/NATIVE_DGC.md) for its state-changing
indirect path. This Windows bundle does not install the Linux renderer. Mesa's
generated driver headers must match that protocol; regenerate them with
`tools/build-native-renderer.sh` before packaging source changes. Existing
LLVM/libclang22.1.8, VulkanSDK1.4.350.0 and bindgen0.72 pins remain. The new local
builds are not hosted-CI or native guest acceptance evidence.

## What the workflow builds

The jobs are independent so an error points at the actual component:

1. `driver` builds the DXVK and vkd3d-proton static cores, embeds them in
   `helios_umd.dll` (D3D11) and `helios_umd12.dll` (D3D12), and builds/packages
   the Rust WDDM kernel driver. Both UMDs are required package inputs.
2. `mesa` and `mesa_x86` build the pinned Mesa submodule for x64 and x86 with
   both the Venus Vulkan ICD and the Zink WGL OpenGL ICD enabled.
3. `opencl` builds pinned CLVK with the clspv online compiler embedded. End-user
   machines therefore do not need `clspv.exe` or `CLVK_CLSPV_PATH`.
4. `loaders` builds the official x64 Vulkan/OpenCL loaders, the x86 Vulkan
   loader, and architecture-matched smoke probes.
5. `compatibility` builds and validates the app-local DaVinci Resolve ADL shim.
6. `package` test-signs the final driver package and compatibility shim, hashes
   every distributed binary,
   and creates `helios-windows-x64-<version>-<commit>.zip`.

The workflow runs for pull requests and pushes to `master`, and can be started
manually. A tag beginning with `v` also publishes the zip and its SHA-256 file
as a GitHub Release.

## Reproducibility and source pins

The Helios, Mesa, DXVK, and vkd3d-proton revisions come from the checked-out commit and its
gitlinks. The Windows OpenCL build uses the `winboat-org/clvk-helios` fork for
guest DXGI/OpenCL device association. Its repository and commit, along with the
Vulkan-Loader, Vulkan-Headers, and OpenCL-ICD-Loader commits, are pinned in
`.github/workflows/windows-stack.yml`. Toolchain versions are pinned there as
well. Every resulting source revision is written to `manifest.json`.

When updating an external pin, first build and run the packaged probes in
the VM. In particular, CLVK and Zink are consumers of the Venus ICD and can
expose synchronization/protocol mismatches that a successful compile cannot.

## Signing model

CI creates a unique, non-exportable test-signing key for each bundle. It signs
the SYS and both UMDs before creating the catalog, signs the final catalog, exports
only the public certificate, then destroys the CI private key. The installer
adds that public certificate to `Root` and `TrustedPublisher`.

This is intentionally a development distribution. Windows must boot with test
signing enabled, which requires Secure Boot to be disabled. The installer can
enable test signing, but never changes Secure Boot and never silently weakens
code-integrity settings. Production releases need Microsoft attestation/WHQL
signing (or another project-approved production certificate flow) in place of
the ephemeral certificate.

## Installation behavior

`Install-Helios.ps1` verifies the payload manifest before making changes, then:

- installs the Visual C++ x64 runtime and the prebuilt PnP driver package;
- installs Mesa and CLVK in a versioned directory below `Program Files`;
- installs official x64 and x86 `vulkan-1.dll` loaders and the x64 `OpenCL.dll`
  only when the matching system loader is absent;
- registers Venus and CLVK through the Khronos machine ICD registries; and
- registers the x64 and x86 `libgallium_wgl.dll` files as the Microsoft OpenGL
  ICDs on the Helios display adapter key. It does not replace Windows'
  `opengl32.dll`.

Original OpenGL registry values and every created path/hash are saved in
`C:\ProgramData\Helios\install-state.json`. The package refuses to overwrite an
installation managed by another bundle; uninstall it first so rollback state
cannot be lost.

`Verify-Helios.ps1 -RunSmokeTests` checks hashes and registrations, then creates
a Vulkan instance, creates D3D11 and D3D12 devices on Helios, creates a WGL context, and
compiles/runs an OpenCL kernel. The OpenCL probe validates every output value. Run graphics probes in the
logged-in desktop session or an interactive scheduled task; session 0 is refused.
The D3D12 smoke checks native runtime device creation, not rendering or conformance.

## Application compatibility files

The archive includes the separately deployed DaVinci Resolve ADL shim at
`compatibility\DaVinci Resolve\atiadlxx.dll`. It is not installed system-wide or
copied by `Install-Helios.ps1`. The adjacent installer safely backs up and
places the DLL beside `Resolve.exe`; no special launcher is required.

D3D12 is enabled when `HKLM\SOFTWARE\Helios!UmdD3D12` is absent. Explicit
DWORD `0` disables it, and the installer preserves that override. The D3D12
smoke then expects device creation to fail. Deleting the value restores the
enabled default. Resource ownership and failure-path limits remain documented
in [EXECUTION_SYNC.md](docs/dx12/EXECUTION_SYNC.md) and
[HPS2_REFACTOR.md](docs/HPS2_REFACTOR.md); the default change does not close them.

## Hosted runner requirements

The driver and package jobs require Visual Studio 2022 and the Windows 11 SDK
and WDK. The setup script uses an already installed WDK when available and
otherwise installs the official 10.0.26100 SDK/WDK packages with winget. A
self-hosted runner should preinstall those tools if winget is unavailable.

The bundle supports WoW64 Vulkan and OpenGL using independently built x86 Mesa
and Vulkan-loader binaries. WoW64 Direct3D and OpenCL still require separately
built x86 WDDM UMD/DXVK and CLVK/OpenCL-loader components; copying x64 DLLs into
`SysWOW64` is not a valid substitute.

The driver job installs native `widl` through MSYS2's
`mingw-w64-ucrt-x86_64-tools` package and initializes vkd3d's recursive submodules.
It builds only `helios_d3d12_static`; no app-local `d3d12.dll`, `d3d12core.dll`,
or `helios_vkd3d.dll` is shipped. The build verifies `OpenAdapter12` and rejects
DXGI/D3D12 runtime imports in `helios_umd12.dll`. DXVK uses `/MT`; vkd3d and
UMD12 keep their existing `/MD` contract and the bundle includes the VC runtime.
Engine licenses, optional UMD PDBs, vkd3d source provenance, and the actual driver
build tool versions (`payload/driver/toolchain.json`) travel with the package.

The VM comparison on 2026-09-07 found LLVM/clang-cl/libclang **22.1.8** in both
active engine builds and Vulkan SDK **1.4.350.0** (glslang **16.2.0**). CI now
pins those versions instead of LLVM 17.0.6 / SDK 1.4.309.0. VM Meson is 1.11.1,
Python 3.12.10, widl 11.5, and cargo-make 0.37.24; CI retains Meson 1.11.2,
Python 3.12, and cargo-make 0.37.24, and records the resolved widl version.
The VM has both VS 2022 and VS 18 and several SDKs; CI uses its Windows 2022
runner's installed MSVC/WDK. `toolchain.json` records their selected versions.
The VM's nightly is dated 2026-06-03 and its default Rust is 1.96.0; CI retains
its explicit nightly-2026-07-14 pin and applies it to cargo-make and both UMDs.
