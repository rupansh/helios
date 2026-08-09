/* bindgen wrapper for the D3D12 user-mode display driver DDI.
 *
 * Pulls in the SDK's d3d12umddi.h -- the DDI the OS D3D12 runtime
 * (d3d12.dll + D3D12Core.dll) lowers app calls into -- so bindgen can generate
 * Rust types for the DDI tables, the OPENADAPTER/CREATEDEVICE arg structs, the
 * caps enums and the runtime callback tables.
 *
 * ⛔ THE LAYOUT ASSERTIONS ARE THE DELIVERABLE. `ARCHITECTURE.md` §12 rule 1:
 * never hand-transcribe a DDI ABI struct. R908 (`e315d03`) deleted five
 * hand-written `D3d12Ddi*` structs and seven hand-transcribed
 * `D3D12DDICAPS_TYPE_*` values for exactly this reason, and §12 rule 2 records
 * what a wrong table size costs: "a 376..392 byte out-of-bounds write into the
 * runtime's heap".
 *
 * ⚠ THIS HEADER DOES NOT COMPILE IN USER MODE OUT OF THE BOX, which is the one
 * thing worth knowing before touching it. d3d12umddi.h pulls d3dkmddi.h, which
 * uses NTSTATUS -- a type the um `windows.h` does not define. The fix is the
 * same incantation `umd/bindgen/d3d10umddi_wrapper.h:14-18` already uses for
 * the D3D11 side; it is repeated rather than shared because the two wrappers
 * include different DDI headers and a shared wrapper would be a header that
 * pulls BOTH DDIs into BOTH drivers.
 *
 * ⛔ BUILT AGAINST **WDK 10.0.28000.2526** um+shared+km under clang, with the
 * platform headers (windows.h, the CRT, d3dkmthk.h, d3dkmdt.h, d3dukmdt.h,
 * dxmini.h, dxgiddi.h) still coming from the installed SDK 10.0.26100. The
 * staged 28000 package is a WDK, not an SDK — 44 `um` and 11 `shared` headers —
 * so the mix is not a shortcut, it is the only shape that compiles.
 * `build.rs`'s include order is what makes it deterministic: the 28000
 * directories are listed first, so `d3d12umddi.h`, `d3d10umddi.h`,
 * `d3dumddi.h` and `d3dkmddi.h` come from there and everything else falls
 * through.
 *
 * The retarget is the HPS2 retirement's U0: §10.2 requires a negotiated
 * `D3D12DDI_SUPPORTED_0116`, and 26100's `d3d12umddi.h` ends at Core build
 * 0110 — no `D3D12DDI_DEVICE_FUNCS_CORE_0116`, no `PFND3D12DDI_CREATEFENCE_0116`,
 * no `pfnOpenNativeFenceCb`, no `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT`.
 * `build.rs::require_core_0116` fails the build if a generation cannot name
 * them, because a silent fallback to 26100 is otherwise indistinguishable from
 * success.
 *
 * ⚠ Line citations: `d3d12umddi.h:NNNN` references in docs/dx12/ predate this
 * retarget and are written against the 26100 copy. Retirement-era citations are
 * against `tmp/wdk-28000/Include/10.0.28000.0/um/d3d12umddi.h`. */
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>

/* d3dkmddi.h (pulled by d3d12umddi.h) uses NTSTATUS, which the um windows.h
 * does not define. Provide it the same way the D3D11 wrapper does. */
#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif

#include <d3d12umddi.h>
