# TOOLCHAIN.md — Build Environment Setup

> **⚠️ SUPERSEDED (2026-07-05) — install/verify steps are for the abandoned
> System-class driver.** The active driver is the **WDDM render+display miniport**
> (crate `kmd_render`, service/INF `helios_kmd_render`, `helios_kmd_render.cat`),
> a display-class adapter that carries `UserModeDriverName` and owns one VidPn
> source — NOT a System-class device, and it DOES appear
> under Display adapters. The ICD builds as the C **Mesa-Venus** port
> (`vulkan_virtio.dll` via `win_meson`), not a Rust `icd` crate. Use the
> platform build rules and `win_*` MCP tooling in **CLAUDE.md** and the deploy
> steps in **HELIOS_DRIVER_DEPLOYMENT.md**; those are authoritative.

> **DIRECTION RESET (2026-06-07):** active KMD work is System-class KMDF + DeviceIoControl + Mesa Venus.
> DOD/dxgk build artifacts may remain as archived reference, but the active build should follow `ARCH.md` and
> `SYSTEM_CLASS_REFOCUS_2026_06_07.md`.

## Overview

You need two environments:
1. **Windows 11 Dev VM** — builds and runs the KMD (kernel-mode driver) and ICD
2. **Linux Host** — builds and runs QEMU + virglrenderer (Venus)

The Windows dev VM can be a separate VM from the target VM, or the same one if you're careful. Using separate VMs is strongly recommended.

---

## 1. Linux Host Setup

### 1.1 System Requirements

- Linux kernel **6.13+** (required for KVM page-fault fixes with blob resources)
- Vulkan 1.3-capable GPU with a compliant driver (RADV for AMD, ANV for Intel)
- The pinned **`qemu-helios`** submodule (based on QEMU 11.0.1)
- virglrenderer built from source with Venus enabled

Check your kernel:
```bash
uname -r   # must be ≥ 6.13
```

Check Vulkan support:
```bash
vulkaninfo --summary | grep -E "apiVersion|driverVersion"
```

### 1.2 Build virglrenderer with Venus

```bash
# Dependencies (Ubuntu/Debian)
sudo apt install -y \
  meson ninja-build pkg-config \
  libepoxy-dev libgbm-dev \
  libdrm-dev libvulkan-dev \
  libpng-dev cmake

git clone https://gitlab.freedesktop.org/virgl/virglrenderer.git
cd virglrenderer
# Use a recent stable commit or tag — and PIN it. The Venus protocol/capset is
# version-coupled between the guest Venus encoder (Mesa / the Helios ICD) and
# host virglrenderer; record the exact virglrenderer commit + matching Mesa-Venus
# version and bump them together. (mvisor-win-vgpu-driver pins exact Mesa +
# virglrenderer commits for the same reason — see TRANSPORT.md §7.)
meson setup build \
  -Dvenus=true \
  -Dvenus-validate=false \
  -Ddrm-renderers=auto \
  -Dprefix=/usr/local
ninja -C build
sudo ninja -C build install
sudo ldconfig
```

Verify:
```bash
virgl_test_server --help 2>&1 | grep venus
# Should show: --venus   Enable Venus (VirtIO-GPU Vulkan)
```

### 1.3 Build the pinned QEMU fork

```bash
# Stock QEMU is useful for renderer A/B tests, but it is not the active Windows
# display binary: it cannot reconstruct the modifier-less OPTIMAL DWM primary.
sudo apt install -y \
  libglib2.0-dev libpixman-1-dev libssl-dev \
  libslirp-dev libcap-ng-dev libattr1-dev \
  python3-pip python3-setuptools ninja-build

git submodule update --init --recursive qemu-helios
cd qemu-helios
mkdir -p build-helios && cd build-helios
../configure \
  --target-list=x86_64-softmmu \
  --enable-kvm \
  --enable-opengl \
  --enable-virglrenderer \
  --enable-gtk \
  --enable-sdl \
  --enable-vnc \
  --enable-modules \
  --disable-docs
ninja
```

### 1.4 QEMU Launch Command for Development

**Operational rule:** if you change the standalone VM launch command, launcher script, QEMU display/debug
transport, or any environment variable needed by `tools/launch-helios-gtk.sh`, do not try to start the VM
yourself unless the user explicitly asks in that same turn. Tell the user exactly what changed and ask them to
run the VM. This matters because the launcher often needs the user's desktop session, sudo credentials, GPU
environment, and active display state.

```bash
# egl-headless + VNC (visually verified)
HELIOS_QEMU_RENDER_GPU=nvidia HELIOS_DISPLAY=egl-vnc \
  bash tools/launch-helios-gtk.sh

# Native Wayland SDL is the verified accelerated local-window path. Interactive
# UI EGL follows the compositor's vendor; the launcher pins only Venus/readback
# Vulkan to NVIDIA.
SDL_VIDEODRIVER=wayland \
  HELIOS_QEMU_RENDER_GPU=nvidia HELIOS_DISPLAY=sdl \
  bash tools/launch-helios-gtk.sh

# GTK uses the same fallback code but is not currently operational for a full
# Windows run: GDK later reports repeated eglMakeCurrent failures.
HELIOS_QEMU_RENDER_GPU=nvidia HELIOS_DISPLAY=gtk \
  bash tools/launch-helios-gtk.sh
```

**Key flags explained:**
- `blob=on` — enables blob resource support (zero-copy memory between guest and virglrenderer)
- `hostmem=8G` — dedicates 8 GB of host memory as the blob/hostmem region
- `venus=on` — enables Venus capset (Vulkan over virtio-gpu)
- `-display sdl,gl=on` — verified native-Wayland OpenGL window
- `-display gtk,gl=on` — compiled fallback frontend; currently blocked by the
  GTK/GDK `eglMakeCurrent` failure above

### 1.5 Verify Venus Is Working (Linux Guest First)

Before tackling the Windows driver, verify the stack works end-to-end with a Linux guest:

```bash
# In the Linux guest VM:
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/virtio_icd.x86_64.json
vulkaninfo --summary
vkcube   # should render a spinning cube
```

If this works, your host stack (QEMU + virglrenderer + Venus) is correct.

---

## 2. Windows 11 Dev VM Setup

This is where you compile and test the KMD and ICD.

### 2.0 Dev VM (`win11`)

A Windows 11 dev VM named `win11` is reachable via `ssh win` (preconfigured). It was **not** fully provisioned out of the box — only Rust (stable) was present; everything else below had to be installed. The actually-required, verified toolchain:

- **VS 2022 Build Tools** — "Desktop development with C++" (MSVC v143 + Spectre-mitigated x64 libs).
- **WDK** — kits **10.0.26100.0** *and* **10.0.28000.0**, both complete. ⭐ Since 2026-08-10 the VM has 28000 installed and **that is the one `wdk-build` selects**, because it picks the **highest** installed kit with **no override**. Both must be *complete* kits (SDK **and** WDK at the same version): an incomplete higher kit (e.g. a winget WDK with no matching SDK → missing `specstrings.h`) breaks the build. **Keep only complete kits** — that rule is why 28000 was installed from four NuGet packages rather than headers alone; see §2.1. Verified after the switch: `kmd_render` checks at the same 22-warning baseline.
- **LLVM 22.1.8** at `C:\Program Files\LLVM\bin`; `LIBCLANG_PATH` points there for bindgen and it is also the `clang-cl` that builds vkd3d, the DXVK bridge and the umd12 bridge. ⚠ **One LLVM for both** — bindgen parsing headers with one clang while `clang-cl` compiles them with another is the drift class this tree keeps getting bitten by.
  - ⭐ **Upgraded 17.0.6 → 22.1.8 on 2026-08-10, and it is a floor, not a preference.** VS 18 landed MSVC 14.51, whose `<yvals_core.h>` hard-asserts *"Unexpected compiler version, expected Clang 20 or newer"*; VS 2022's 14.44 asserts Clang 19+. Clang 17 satisfies neither, and no `-D` can suppress it — 14.51's `__msvc_doom_core.hpp` also assumes `defined(__clang__)` implies `__builtin_verbose_trap`, a Clang 19 builtin, so the build fails on a missing builtin rather than on the assert.
  - ⇒ **If a future MSVC raises the bar again, RAISE CLANG.** Do not reintroduce `_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH`; it was deleted from all **four** sites that carried it, each of which recorded it as "a runtime-risk acknowledgement, not a fix": `umd/build.rs`, `umd12/build.rs`, `tools/win-mcp/src/main.rs` (`win_vkd3d`'s meson setup) and `ci/windows/Build-Driver.ps1` (the DXVK meson setup). ⚠ *This bullet said "three" until 2026-08-10, and the fourth was the one that mattered* — the CI copy survived the first pass because the round that deleted the other three touched no file under `ci/` or `.github/`. A `grep -r` still finds a **fifth** mention, `icd/win-build/clang-cl-native.ini`, and that one is prose, not a define: it is the comment header of a deliberately non-preferred alternative toolchain for the Mesa ICD (mingw is the preferred one, and CI's `mesa` job uses msys2/mingw), and it already names its own exit — *"retire it … by moving to LLVM>=19"*, which is what happened. Nothing builds with it; it is stale text in a file this section does not own.
  - ⛔ **The floor is expressed at TWO sites, and this is the second-site rule the KMD version already lives under.** The argument is here; the value CI builds with is `LLVM_VERSION` in `.github/workflows/windows-stack.yml`. The tree keeps the KMD version at exactly one site (`kmd_render/driver-version.env`) because an INF and a FILEVERSION that disagree produce `FAILED_ADD 0xc0000182`; a clang floor and a CI pin that disagree produce `error STL1000` in the DXVK bridge compile, at a step that used to pass. **Change this section and that variable in the same edit.** (Nothing enforces it mechanically — that would need a gate that parses the workflow, which does not exist.)
    - ⚠ **CI cannot ask the action for 22.1.8 by version.** `KyleMayes/install-llvm-action` resolves versions from a table baked into its bundle; verified 2026-08-10 at the pinned commit `ebc04262` **and** at the action's `master` that the newest `win32`/`x64` entry in both is `21.1.8`, so an unlisted version throws `Unsupported version for platform`. The workflow therefore passes the action's own `force-url` input, deriving the URL from `LLVM_VERSION` so the version still has one site. Verified the same day: the `llvmorg-22.1.8` release exists and ships `LLVM-22.1.8-win64.exe`, the exact asset name the action's own URL template produces. **Not verified: that the job runs green** — the workflow triggers only on push/PR to `wddm` and the retirement work is on `wddm-dx12`, so it has never fired against this changeset, and the `driver` job has an independent break documented in §2.1's WDK 28000 note.
- **Rust nightly + `rust-src`** (for `no_std` build-std), target `x86_64-pc-windows-msvc`.
- **cargo-make** — `cargo install --locked cargo-make`.
- **coreutils** are installed (Unix tools like `ls`/`cp`/`grep` work in `win_exec`).
- **SSH:** `ssh win` does not auto-`cd`; the source tree is at `Z:\`. Prefer the `win` MCP server over raw ssh.
- **Shared folder:** the current Linux project folder (`helios-vgpu/`) is shared into the VM and mounted as the **`Z:\`** drive. `ssh win` does **not** auto-`cd` into it — you must **explicitly `cd /d Z:\`** to reach the shared project folder before running any build commands. Commands run there operate on the same source tree you edit on Linux.

This means a typical Windows-side build must first `cd` into `Z:\`:

```bash
ssh win "cd /d Z:\ && cargo make"   # cd into the shared folder, then build on win11
```

The remaining subsections (§2.1–§2.5) document the full from-scratch setup for reference, but most of it is already done on `win11`.

> **IMPORTANT — building on `win11` (updated):**
> - **Rust IO fails on the `Z:\` share.** `cargo`/`cargo make` hit `OS error 87 (The parameter is incorrect)` on artifact copies and warn `could not canonicalize path Z:\`. So the **Cargo target dir must be on local disk**, not the share. Edit source on `Z:\`; build with `CARGO_TARGET_DIR=C:\Users\Rupansh\helios-target\<crate>`.
> - Set it via the **`CARGO_TARGET_DIR` env var** per invocation. Do **NOT** put `target-dir` in a committed `.cargo/config.toml` — Linux reads that file too (it builds shared crates like `protocol/`), and a `C:` path would break it. On Linux use `CARGO_TARGET_DIR=target/linux`.
> - **coreutils are installed** on `win11`: standard Unix tools (`ls`, `cp`, `mv`, `rm`, `cat`, …) work alongside PowerShell.
> - Prefer the **`win` MCP server** (`win_exec` / `win_cargo`) over raw `ssh win "cd /d Z:\ && …"` — it sidesteps cmd.exe quoting and stale-ControlMaster env, and `win_cargo` sets the local target dir + `LIBCLANG_PATH`.

### 2.1 Required Software

Install in this order:

#### Visual Studio 2022
Download from https://visualstudio.microsoft.com/  
Required workloads:
- "Desktop development with C++" (for MSVC toolchain, linker, headers)
- Individual component: "MSVC v143 Spectre-mitigated libs (x64)"

#### Windows Driver Kit (WDK) 22H2
```
https://learn.microsoft.com/en-us/windows-hardware/drivers/download-the-wdk
```
Install the WDK matching your VS 2022. The WDK installs as a VS extension.

Verify: Open VS → Extensions → should show "Windows Driver Kit".

#### ⭐ Windows Kit **10.0.28000.0** — INSTALLED on the VM (owner decision, 2026-08-10)

**Helios targets kit 28000, not 26100.** The kit is installed on `win11` as a
complete kit beside 26100, so nothing in the build depends on a hand-staged tree
in the repository any more.

> **Why this changed.** Round 3 of the Phase-2 review found that
> `umd12/build.rs` `require_path`'d a `.gitignore`d directory
> (`tmp/wdk-28000/`) and `require_core_0116()` **panicked** without it, so
> `helios_umd12.dll` could be built on exactly one computer; the same untracked
> tree also silently decided whether `tools/retirement-gates.sh` ran its
> slot-audit gate or skipped it, which is why that suite reported 8 PASS on one
> checkout and 7 PASS + 1 SKIP on another **at the same commit**. Owner
> decision: install it properly instead.

**What is installed**, and it is a *complete* kit — which matters, because the
rule two sections up ("keep only complete kits") exists precisely because
`wdk-build` picks the **highest** installed kit with no override, and a partial
higher kit breaks the build:

```
C:\Program Files (x86)\Windows Kits\10\
  Include\10.0.28000.0\{um, shared, km, ucrt, winrt, cppwinrt}
  Lib\10.0.28000.0\{um\x64, km\x64, ucrt\x64, ucrt_enclave\x64}
  bin\10.0.28000.0\{x64, x86, ...}   build\  CrossCertificates\  tools\
```

Measured after the install: `Include` and `Lib` have **exactly the same
subdirectory shape** as the 26100 kit beside them, and `um=1541 shared=255
km=224` headers (the old hand-staged tree had `um=27 shared=11`).

**Installing it** — four NuGet packages, all at **10.0.28000.2526**, merged into
the kit root. Run on the VM (it has `tar.exe` and needs admin):

```powershell
$ver = '10.0.28000.2526'
foreach ($p in 'Microsoft.Windows.WDK.x86','Microsoft.Windows.WDK.x64',
               'Microsoft.Windows.SDK.CPP','Microsoft.Windows.SDK.CPP.x64') {
  Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/$($p.ToLower())/$ver/$($p.ToLower()).$ver.nupkg" `
    -OutFile "$stage\$p.$ver.nupkg" -UseBasicParsing
}
# version-scoped paths ONLY -- never Include\wdf, Lib\wdf, Catalogs\ or the
# legacy bin\10.0.1xxxx dirs: those are SHARED with the 26100 kit.
#   from WDK.x86 / WDK.x64 / SDK.CPP:  c/{Include,Lib,bin,build,CrossCertificates,tools}/10.0.28000.0
#   from SDK.CPP.x64 (different layout): c/{ucrt,ucrt_enclave,um}  ->  Lib\10.0.28000.0\{...}
```

SHA-256, verified at install:

| package | sha256 |
|---|---|
| `Microsoft.Windows.WDK.x86` | `3432999540db204315247f8f904feebfd4a217af5529e3beea59884478f0daef` |
| `Microsoft.Windows.WDK.x64` | `63c939fb5a79295bf40e941db592681272219b04edff095fe2f3d123e5579a90` |
| `Microsoft.Windows.SDK.CPP` | `be1b419491607eae6f7c57844ebab39face9643c51e2af1d9176a3ba0d0b23fc` |
| `Microsoft.Windows.SDK.CPP.x64` | `a9cae2a8c5da7f5dc5838ae6a76d06d0d2e2fdc3d8cfc69ca6c184e4b9193a00` |

⭐ The first hash is the one `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` §1 already
pinned for the hand-staged tree, so the installed kit is **provably the same
package** the earlier measurements were taken with. Independently confirmed:
`km\dispmprt.h`, `um\d3d12umddi.h` and `shared\d3dkmddi.h` hash **identical**
between the installed kit and `tmp/wdk-28000/`.

**What the install changed in the build**

| Before | After |
|---|---|
| `umd12/build.rs` `WDK_DDI_INCLUDE_DEFAULT` → `tmp\wdk-28000\...` | → the installed kit |
| `SDK_PLATFORM_INCLUDE_DEFAULT` → the **26100** kit, because the staged package was a WDK with 11 `shared/` headers | → the **same 28000** kit; the split is retired |
| 28000 `shared/` kept OFF the include path (measured clang errors: `D3DDDI_CREATEHWQUEUEFORUSERMODESUBMISSION_FLAGS`, `D3DDDI_UMS_PDD_SIZE` undeclared) | on the path; both identifiers are declared by `shared\d3dukmdt.h` |
| `kmd_render/build.rs` hardcoded the staged `dispmprt.h` and never passed it to the generator | resolves `HELIOS_WDK_KM_INCLUDE` → installed kit → staged tree, and **passes `--header`** |

⚠ **The bindings changed, and the change was inspected rather than assumed.**
Moving `shared/` from 26100 to 28000 grew `umd12/bindgen/cached/d3d12umddi.rs`
from 6,485,166 to 6,573,255 bytes: **65 top-level items added, 3 removed**. The
additions are 28000-era kernel types (`D3DKMT_CREATEHWQUEUEFORUSERMODESUBMISSION`,
`D3DDDI_SEGMENTPREFERENCE2`, `D3DDDI_NATIVEFENCELOGDETAIL`, `D3DDDI_DOORBELLMAPPING`,
12 new `DXGK_FEATURE_ID` values); the 3 removals are anonymous `__bindgen_ty_N`
renumbering inside `_D3DKMT_VIDMM_ESCAPE`, whose union grew. All five
`require_core_0116` symbols are present in the new generation.

**Verified green after the install** (2026-08-10): `kmd_render` `cargo check`
exit 0 at **22 warnings — the pre-change baseline count** (with `wdk-sys`
force-cleaned first, 151.6 MiB removed, so the regeneration was real and not a
cached green); `umd12` release build exit 0; `umd` release build exit 0; all 8
Linux gates PASS.

**Why 28000 and not 26100** — `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` §1:
26100's `d3d12umddi.h` ends at Core build `0110`, so it contains no
`D3D12DDI_DEVICE_FUNCS_CORE_0116`, no `PFND3D12DDI_CREATEFENCE_0116`, no
`pfnOpenNativeFenceCb` and no `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT`.
⚠ This is a **header** requirement only — F1 measured Core 0116 negotiating on
the installed *runtime* build 26100.8875, so 28000 is **not** a runtime or
package minimum and must not be written into one.

#### ⭐ `km/dispmprt.h` is VENDORED — the Linux gate has tracked ground truth

The install above is on the **VM**. `tools/retirement-gates.sh` runs on the
**Linux host**, where no Windows kit exists, and its slot-audit staleness gate
used to read `tmp/wdk-28000/…/km/dispmprt.h` — a `.gitignore`d path, so the gate
**skipped in every fresh clone, worktree and CI runner** while the suite printed
"ALL … PASS".

That is closed, not merely reported: the generator's single input is checked in
at **`kmd_render/tools/wdk-28000/km/dispmprt.h`** (167 KB; package, both
SHA-256s and the re-extraction command are in that directory's `README.md`), the
generator's `DEFAULT_HEADER` points at it, and the gate's `if [ -f … ]` wrapper
is **deleted**. The generator resolves no `#include`, which is why one file
suffices.

Verified the way that matters — the whole `tmp/` directory moved aside, i.e. a
fresh clone: **all 8 gates PASS, exit 0, with the slot-audit gate RUNNING**
(`WDDM 3.2 slot audit is not stale vs its generator — up to date`) rather than
skipping. Regenerating against the vendored copy changed exactly the recorded
`AUDITED_HEADER` path — **zero** `SlotClass` changes, still 192 slots and 1544
bytes — which was checked before the regeneration was accepted, because
`FINDINGS.md` F6 records what happens when it is not.

⚠ Still outstanding, and deliberately deferred: **CI installs only 26100** —
`ci/windows/Install-WindowsDriverKit.ps1` installs
`Microsoft.WindowsSDK.10.0.26100` + `Microsoft.WindowsWDK.10.0.26100`, and
`Build-Driver.ps1` derives `HELIOS_WDK_INCLUDE` from `Find-WindowsKitInclude`,
which returns the highest **installed** kit. The fix is to install kit 28000 on
the runner exactly as §2.1 installs it on the VM. Per the owner's sequencing, CI
is updated at the **end** of the retirement, not per-change.

#### The staged `tmp/wdk-28000/` tree — now optional

Nothing requires it any more: the VM builds from the installed kit and the Linux
gate reads the vendored header. It remains a convenient way to get the other
28000 headers on a Linux host (e.g. to read `d3d12umddi.h` without the VM), and
the recipe below still works.

**Staging it on Linux** (headers only — this is the gate's input, not a build
input):

**Staging it** (run on the Linux host). ⚠ This used to be load-bearing for the
VM build too, because `tmp/` is inside `win_cargo`'s and `win_build_kmd`'s
robocopy mirror and so arrived at `C:\Users\Rupansh\helios-vgpu\tmp\wdk-28000`
for free. **That is no longer how the VM resolves its headers** — it uses the
installed kit — so this recipe now serves the Linux gate alone:

```bash
# Package: Microsoft.Windows.WDK.x86 10.0.28000.2526
#   https://www.nuget.org/packages/Microsoft.Windows.WDK.x86/10.0.28000.2526
curl -sSL -o /tmp/wdk28000.nupkg \
  https://www.nuget.org/api/v2/package/Microsoft.Windows.WDK.x86/10.0.28000.2526
sha256sum /tmp/wdk28000.nupkg
# MUST be 3432999540db204315247f8f904feebfd4a217af5529e3beea59884478f0daef
# (the hash HELIOS_PRESENT_SYNC_RETIREMENT.md §1 records; re-verified 2026-08-10
#  by downloading the package and hashing it, and the two staged headers below
#  are byte-identical to the ones inside it)
mkdir -p tmp/wdk-28000
bsdtar -xf /tmp/wdk28000.nupkg -C tmp/wdk-28000 --strip-components=1 'c/Include/*'
```
The package's `c/Include/` becomes `tmp/wdk-28000/Include/`. Correct result:
`Include/10.0.28000.0/{um,shared,km}` with **44 / 11 / 247** headers plus
`Include/wdf`, ≈40 MB — the 44 and 11 are the counts `umd12/build.rs` cites when
it explains why `HELIOS_SDK_INCLUDE` is a *separate* root (the package is a WDK,
not an SDK, so `windows.h`, `d3dkmdt.h`, `d3dukmdt.h`, `d3dkmthk.h`, `dxmini.h`,
`dxgiddi.h` and `winapifamily.h` resolve from the installed
`Windows Kits\10\Include\10.0.26100.0` instead — that split is
`HELIOS_SDK_INCLUDE`, and the 28000 `um` must come **first** on the include line
or `d3d12umddi.h` silently resolves to the 26100 copy of the same file name).
⭐ This recipe was run end-to-end on 2026-08-10 into a scratch directory and
`diff -rq`'d against the tree already staged here: no differences. Two spot
checks worth more than the file counts:

```bash
grep -c D3D12DDI_DEVICE_FUNCS_CORE_0116 \
  tmp/wdk-28000/Include/10.0.28000.0/um/d3d12umddi.h      # 2, not 0
ls tmp/wdk-28000/Include/10.0.28000.0/km/dispmprt.h       # the slot audit's input
```

**Why the Linux copy stays `.gitignore`d and untracked, by decision.**
`.gitignore` excludes `/tmp/`; `git ls-files tmp/` is empty. ⚠ *No prior
document argues this* — `FINDINGS.md` F1 records the fact ("gitignored") and
stops — so what follows is the argument, written here so it can be attacked
rather than inherited: the package is 40 MB of Microsoft-redistributed headers
under Microsoft's licence, and a pinned SHA-256 makes a local copy verifiable
without vendoring it, the same trade the DXVK/vkd3d/Mesa engines take as
submodules rather than copies. Vendoring would put a licence question and a
40 MB blob into every clone to save one `curl`. If that trade ever stops paying,
reversing it is one commit — and the cheaper reversal is now checking in the
single ~167 KB `dispmprt.h` the gate actually reads, or a derived manifest.

⭐ **What this decision no longer costs.** It used to be argued here that its
price was *"CI cannot build `helios_umd12.dll` today"*. That is now the wrong
attribution: since the VM has kit 28000 **installed**, the untracked tree is not
a build input at all — it is a Linux-gate input. What CI still cannot do it
cannot do for its own reasons, which are unchanged and outstanding:
`ci/windows/Install-WindowsDriverKit.ps1` installs
`Microsoft.WindowsSDK.10.0.26100` + `Microsoft.WindowsWDK.10.0.26100` and never
28000, and `Build-Driver.ps1` derives `HELIOS_WDK_INCLUDE` from
`Find-WindowsKitInclude`, which returns the highest **installed** kit — so on a
runner that would still be 26100 and `require_core_0116` would panic. (Nor is
that the job's only gap: `Build-Driver.ps1` also never sets `HELIOS_VKD3D_BUILD`,
which `umd12/build.rs` `require_path`s.) ⇒ The CI fix is to install kit 28000 on
the runner exactly as §2.1 installs it on the VM. Per the owner's sequencing,
that lands at the **end** of the retirement, not per-change.

**What silently degrades without it.** `tools/retirement-gates.sh` guards its
slot-audit gate on the presence of `.../km/dispmprt.h`; absent the tree it prints
a `SKIP` line and keeps going, so the same commit reports a different gate result
on a machine with the tree than in a fresh worktree. ⚠ That script owns the
wording of its own SKIP and what the run's summary says about it — read it there,
do not infer it from here (a verbatim quote in this file would be a copy that can
go stale on somebody else's edit).

#### LLVM 22.1.8 (minimum 20 — MSVC 14.51's STL asserts it)
The silent NSIS installer upgrades the existing `C:\Program Files\LLVM` in place,
which is what keeps `LIBCLANG_PATH` and `HELIOS_CLANG_CL` valid with no config
change:
```powershell
$exe = "$env:USERPROFILE\Downloads\LLVM-22.1.8-win64.exe"
Invoke-WebRequest -UseBasicParsing -OutFile $exe `
  https://github.com/llvm/llvm-project/releases/download/llvmorg-22.1.8/LLVM-22.1.8-win64.exe
Start-Process -FilePath $exe -ArgumentList '/S' -Wait   # /S = silent, default dir
```

Verify — and check the STL pairing, not just the version, because the version
alone is what went stale here:
```powershell
& 'C:\Program Files\LLVM\bin\clang-cl.exe' --version   # clang version 22.1.8
(Get-Item 'C:\Program Files\LLVM\bin\libclang.dll').VersionInfo.FileVersion  # 22.1.8
# The real check: a bare <memory> TU against the newest MSVC STL.
'#include <memory>
int main(){ return 0; }' | Set-Content -Encoding ascii $env:TEMP\stlprobe.cpp
& 'C:\Program Files\LLVM\bin\clang-cl.exe' /c /EHsc /std:c++20 /MD `
  $env:TEMP\stlprobe.cpp "/Fo$env:TEMP\stlprobe.obj"   # must exit 0, no -D flags
```

⚠ **bindgen must keep up with libclang.** 0.70 *and* 0.71 both mis-generate
against libclang 22 — they bind the forward declaration instead of the
definition for structs declared before they are defined, yielding
`pub _address: u8` and size 1. `umd`/`umd12` are on **0.72**; `kmd_render` stays
on **0.71** because wdk-build 0.5.1's `BuilderExt` extends that exact
`bindgen::Builder` type, and 0.71 is verified working against libclang 22 for
the WDK's C headers.

#### Rust (nightly channel — required for no_std kernel mode)
```powershell
# Install rustup from https://rustup.rs/
rustup toolchain install nightly
rustup default nightly
rustup component add rust-src
rustup target add x86_64-pc-windows-msvc
```

#### cargo-make
```powershell
cargo install --locked cargo-make --no-default-features --features tls-native
```

#### (Optional) cargo-wdk — driver project scaffolding
```powershell
cargo install cargo-wdk
```

### 2.2 Enable Test Signing

The development KMD needs test signing. On the target VM (where the driver runs):

```powershell
# Run as Administrator in the TARGET VM (not necessarily the dev VM)
bcdedit /set testsigning on
bcdedit /set nointegritychecks on
# Reboot
```

On the dev VM, generate a test certificate:
```powershell
# This is done automatically by cargo-make / wdk-build
# The cert goes to: target/<profile>/package/WDRLocalTestCert.cer
# Install it in the target VM's Trusted Root + Trusted Publishers stores
```

### 2.3 Workspace Setup

```powershell
# Create project
mkdir helios-vgpu
cd helios-vgpu

# KMD — kernel-mode driver (KMDF System-class, no_std)
cargo new kmd --lib
cd kmd
```

**`kmd/Cargo.toml`:**
```toml
[package]
name = "helios_kmd"
version = "0.1.0"
edition = "2021"
build = "build.rs"

[lib]
crate-type = ["cdylib"]

[package.metadata.wdk.driver-model]
driver-type = "KMDF"
kmdf-version-major = 1
target-kmdf-version-minor = 33
# driver-type = "KMDF" flips the generated INF Class from Display to System
# {4d36e97d-e325-11ce-bfc1-08002be10318}.

[dependencies]
wdk = "0.4.0"
wdk-sys = "0.5.0"
wdk-alloc = "0.4.0"
wdk-panic = "0.4.0"

[build-dependencies]
wdk-build = "0.4.0"

[profile.dev]
panic = "abort"
lto = "thin"
opt-level = 1

[profile.release]
panic = "abort"
lto = true
opt-level = 3
codegen-units = 1

[features]
default = []
nightly = ["wdk/nightly", "wdk-sys/nightly"]
```

**`kmd/build.rs`:**
```rust
fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::Config::from_env_auto()?.configure_binary_build();
    Ok(())
}
```

**`kmd/Cargo.make.toml`:**
```toml
extend = "target/rust-driver-makefile.toml"
[config]
load_script = '''
#!@rust
//! ```cargo
//! [dependencies]
//! wdk-build = "0.4.0"
//! ```
#![allow(unused_doc_comments)]
wdk_build::cargo_make::load_rust_driver_makefile()?
'''
```

**`.cargo/config.toml`:**
```toml
[build]
rustflags = ["-C", "target-feature=+crt-static"]

[target.x86_64-pc-windows-msvc]
rustflags = [
    "-C", "target-feature=+crt-static",
    "-Z", "sanitizer=address",   # remove for release
]
```

Build the skeleton:
```powershell
# From the kmd/ directory, in a VS 2022 Developer Command Prompt
cargo make
# Should produce: target/debug/package/helios_kmd.inf + helios_kmd.sys
```

### 2.4 ICD Setup

```powershell
cd ../
cargo new icd --lib
```

**`icd/Cargo.toml`:**
```toml
[package]
name = "helios_icd"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]   # produces helios_icd.dll

[dependencies]
# Vulkan bindings
ash = "0.38"              # Vulkan types/enums
# Windows user-mode APIs  
windows = { version = "0.58", features = [
    "Win32_Graphics_Direct3D",
    "Win32_System_Memory",
]}
# Serialization for Venus
bytemuck = { version = "1", features = ["derive"] }
```

### 2.5 Deploying to the Target VM

Use a network share or WinRM to copy files to the target VM. Then:

```powershell
# On target VM (as Administrator):
pnputil /add-driver helios_kmd.inf /install
# Or using devcon:
devcon install helios_kmd.inf "PCI\VEN_1AF4&DEV_1050"
```

Check Device Manager → the device should appear under "System devices" (System
class {4d36e97d-e325-11ce-bfc1-08002be10318}), NOT under "Display adapters". There
is no display adapter and no Code 43 — this is a System-class KMDF function driver,
not a WDDM miniport.

Verify the device interface is reachable from user mode (this is how the ICD finds
the KMD): `SetupDiGetClassDevs(&GUID_DEVINTERFACE_HELIOS, ...)` →
`SetupDiEnumDeviceInterfaces` → `SetupDiGetDeviceInterfaceDetail` → `CreateFile` on
the returned device path should succeed.

Check for errors:
```powershell
Get-WinEvent -LogName System | Where-Object {$_.ProviderName -eq "helios_kmd"} | Select-Object -First 20
```

---

## 3. Debugging Setup

### WinDbg Kernel Debugging (Host ↔ Target VM)

The same launch-command rule applies to kernel debugging. If adding or changing a KD transport requires a QEMU
argument or `tools/launch-helios-gtk.sh` environment change, document the exact command and
ask the user to run/restart the VM. Configure guest BCD and build tools from automation, but leave VM launch to
the user after launch-command changes.

On the target VM:
```powershell
bcdedit /debug on
bcdedit /dbgsettings net hostip:192.168.x.x port:50001 key:1.1.1.1
```

On the dev machine, open WinDbg and connect:
```
File → Attach to Kernel → Net → Port: 50001, Key: 1.1.1.1
```

Useful WinDbg commands for KMDF driver debugging:
```
!wdfkd                       # load the WDF debugger extension
!wdfkd.wdfldr                # show loaded WDF drivers / framework versions
!wdfkd.wdfdevice             # inspect our WDFDEVICE (context, queues, interrupts)
lm m helios*                 # check driver is loaded
!devnode 0 1 "PCI\VEN_1AF4"  # find our device node
.reload /f helios_kmd.sys    # load symbols
```

### DbgPrint Viewing (simpler — no kernel debugger needed)

Use [DebugView](https://learn.microsoft.com/en-us/sysinternals/downloads/debugview) from SysInternals in the target VM. It captures `DbgPrint` / `KdPrint` output.

In Rust (via wdk-sys):
```rust
use wdk_sys::ntddk::KdPrint;
// KdPrint is a macro that calls DbgPrint in debug builds
// Usage:
unsafe { KdPrint!("Helios: adapter started\n\0"); }
```

### virglrenderer Logging (Host Side)

> ⚠️ **`VIRGL_DEBUG` does NOT reliably produce readable logs** with the venus render-server:
> venus runs in the `virgl_render_server` child whose stderr may not be captured. See HOST.md
> §5.1. Use QEMU `-d guest_errors` for `RESP_ERR_*`; for venus traces, capture the
> render-server child's stderr directly or build virglrenderer with logging.

---

## 4. Version Compatibility Matrix

| Component | Minimum | Recommended | Notes |
|-----------|---------|-------------|-------|
| Linux kernel | 6.13 | Latest stable | Blob resource KVM fixes |
| QEMU | 9.2.0 | Latest | Venus upstreamed in 9.2 |
| virglrenderer | 1.1.0 | Latest | Build from source with -Dvenus=true |
| Mesa (Linux guest test) | 24.2 | Latest | Venus ICD |
| WDK/SDK kit 26100 (installed) | 10.0.26100.0 | 10.0.26100.0 | Kept for KMDF/WDF (KMDF 1.33). No longer the DDI or platform include root. |
| ⭐ WDK/SDK kit **28000** (installed) | 10.0.28000.2526 | 10.0.28000.2526 | **The kit Helios targets**, and the one `wdk-build` selects. Required for `helios_umd12.dll`: 26100's `d3d12umddi.h` ends at Core 0110. Installed from four NuGet packages as a *complete* kit; see §2.1 for the recipe and the four SHA-256s. ⚠ Headers only in the sense that matters: NOT a runtime or package minimum (FINDINGS F1). |
| ⭐ `kmd_render/tools/wdk-28000/km/dispmprt.h` (**vendored**) | 10.0.28000.2526 | 10.0.28000.2526 | The slot-audit generator's only input, checked in so `tools/retirement-gates.sh` has tracked ground truth on the Linux host. See that directory's README. |
| `tmp/wdk-28000/` (staged, untracked) | 10.0.28000.2526 | 10.0.28000.2526 | **Optional now.** Convenience copy of the other 28000 headers on Linux; nothing builds or gates on it. |
| VS | 2022 | 2022 | Earlier versions may work |
| LLVM | 22.1.8 | 22.1.8 | **Minimum 20** — MSVC 14.51's STL asserts it. One LLVM for bindgen AND clang-cl. ⚠ Second site: `LLVM_VERSION` in `.github/workflows/windows-stack.yml` — change both together (§2.0). |
| Rust | nightly-2024-11+ | Latest nightly | 2024 edition |
| windows-drivers-rs | 0.4.x / 0.5.x | Latest | wdk = 0.4, wdk-sys = 0.5 |

---

## 5. Common Build Failures

### "cannot find -lntoskrnl"
The WDK is not on PATH or VS Developer Command Prompt was not used.  
Fix: Build inside "x64 Native Tools Command Prompt for VS 2022".

### bindgen emits empty structs (`pub _address: u8`, size 1) and E0609 on every field
The bindgen version predates the installed libclang. It binds a struct's
FORWARD DECLARATION instead of its definition, so layout assertions underflow
(`1_usize - 144_usize`) and every field access fails. Seen with bindgen 0.70 and
0.71 against libclang 22; fixed by 0.72.

Rule out a header cause first, because the symptom looks like one — it is not:
```powershell
clang -target x86_64-pc-windows-msvc -fsyntax-only <includes> wrapper.h   # expect 0 errors
clang -target x86_64-pc-windows-msvc -E        <includes> wrapper.h > pp.txt
# then confirm the struct body is COMPLETE in pp.txt before touching bindgen
```

### C++ fails with `use of undeclared identifier '__builtin_verbose_trap'`
The MSVC STL is newer than clang. Look one error further up for the real gate,
`error STL1000: Unexpected compiler version, expected Clang N or newer` — the
builtin is a Clang 19 addition that 14.51's `__msvc_doom_core.hpp` reaches for
whenever `__clang__` is defined. **Raise clang to N; do not define the builtin
and do not add `_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH`** (it cannot suppress
a missing builtin anyway). If this fires **in CI and not locally**, the clang
floor has gone stale at its second site: `LLVM_VERSION` in
`.github/workflows/windows-stack.yml` (§2.0).

### `helios_umd12: the generated d3d12umddi bindings do not expose Core 0116`
`umd12/build.rs`'s `require_core_0116` panicked: the DDI headers came from the
installed WDK 26100, whose `d3d12umddi.h` ends at Core build 0110. Stage the WDK
28000 headers per §2.1 — and note the panic is *deliberately* louder than the
alternative, because a 26100 generation succeeds in bindgen and would otherwise
surface only as a runtime version mismatch on the guest.

### vkd3d C fails with `incompatible pointer types passing 'LONG *' ... 'uint32_t *'`
Clang 19+ promotes this to an error in C, and upstream vkd3d relies on the
implicit conversion (same width on Windows). The build demotes it with
`-Dc_args=-Wno-error=incompatible-pointer-types` in `win_vkd3d`'s canonical
meson setup — keep the fix in the BUILD, not in the fork.

### KMD loads but crashes on start
Check IRQL. A common mistake is calling pageable functions at DISPATCH_LEVEL during virtqueue init. Use `KeGetCurrentIrql()` assertions in debug.
