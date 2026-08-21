#!/usr/bin/env python3
"""Source and in-memory mutation gate for broad HPS2 carrier demolition."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))

SHIPPED_ROOTS = (
    "umd/src",
    "umd/bridge",
    "umd_common/src",
    "umd_common/bridge",
    "umd12/src",
    "kmd_render/src",
    "protocol/src",
    "protocol/include",
    "dxvk-helios/src",
    "vkd3d-proton-helios/libs",
    "icd/mesa/src/virtio/vulkan",
    "icd/mesa/src/vulkan/wsi",
)
IMPLEMENTATION_SUFFIXES = (
    ".rs",
    ".c",
    ".cc",
    ".cpp",
    ".cxx",
    ".h",
    ".hpp",
    ".inl",
    ".m",
    ".mm",
    ".def",
)
BUILD_FILENAMES = ("meson.build", "CMakeLists.txt")
SHIPPED_BUILD_PATHS = (
    "umd/build.rs",
    "umd12/build.rs",
    "kmd_render/build.rs",
)
RELEASED_WDK_DECLARATION = "icd/win-build/wdk-include/d3dkmthk.h"

DELETED_PATHS = (
    "dxvk-helios/src/dxvk/dxvk_helios_present_sync.cpp",
    "dxvk-helios/src/dxvk/dxvk_helios_present_sync.h",
    "dxvk-helios/src/dxvk/dxvk_helios_scanout_acquire.cpp",
    "dxvk-helios/src/dxvk/dxvk_helios_scanout_acquire.h",
    "dxvk-helios/src/util/util_shared_res.cpp",
    "dxvk-helios/src/util/util_shared_res.h",
    "umd/src/scanout_acquire.rs",
    "umd/src/vehicle_exports.rs",
    "umd/src/forward/snapshot.rs",
    "umd/src/forward/vehicle.rs",
    "kmd_render/src/ddi/escape.rs",
    "kmd_render/src/ddi/blob_map.rs",
    "kmd_render/src/ddi/cpu_host_aperture.rs",
    "kmd_render/src/ddi/scanout_timeline.rs",
    "kmd_render/src/adapter/scanout.rs",
    "kmd_render/src/adapter/read_ledger.rs",
    "kmd_render/src/mapping.rs",
    "kmd_render/src/seh_shim.c",
    "protocol/src/escape.rs",
    "protocol/src/ioctl.rs",
    "protocol/src/wddm_legacy.rs",
    "tools/blob_capacity_probe.c",
    "tools/blob_map_size_probe.c",
    "tools/d3d11_shared_blob_truth_probe.cpp",
    "tools/d3dkmt_alloc_probe.c",
    "tools/d3dkmt_sync_probe.cpp",
    "tools/escape_owner_probe.c",
    "tools/read_ledger_dump.c",
    "tools/scanout_timeline_dump.c",
    "tools/vehicle_flipwait_probe.c",
    "tools/vidmm_tracking_probe.c",
)

RETIRED_CODE = re.compile(
    r"(?:"
    r"\bHPS2\b|HELIOS_PRESENT_SYNC|helios_present_sync_v2|dxvk_helios_present_sync|"
    r"present_sync_(?:publish|fence|lookup|reclaim)|PresentSyncPublish|"
    r"VehicleKernelFlipWait|present_vehicle_copy|present_flip_wait|"
    r"scanout_acquire|present_stream|read_ledger|raw_resid|present_fence_id|"
    r"HELIOS_ESCAPE_|helios_umd_vehicle_"
    r")",
    re.I,
)


def strip_comments(source: str) -> str:
    """Remove C/Rust comments while preserving executable strings and calls."""
    source = re.sub(r"/\*.*?\*/", " ", source, flags=re.S)
    source = re.sub(r"//[^\n]*", " ", source)
    return source


def load_sources(repo: str) -> dict[str, str]:
    sources: dict[str, str] = {}
    for relative_root in SHIPPED_ROOTS:
        root = os.path.join(repo, relative_root)
        if not os.path.isdir(root):
            continue
        for base, dirs, names in os.walk(root):
            dirs[:] = [name for name in dirs if name not in ("target", ".git", "build")]
            for name in names:
                if not name.endswith(IMPLEMENTATION_SUFFIXES) and name not in BUILD_FILENAMES:
                    continue
                path = os.path.join(base, name)
                relative = os.path.relpath(path, repo).replace(os.sep, "/")
                with open(path, encoding="utf-8", errors="replace") as stream:
                    sources[relative] = stream.read()
    for relative in SHIPPED_BUILD_PATHS + (
        "kmd_render/src/lib.rs",
        "kmd_render/src/render_user_copy.c",
        "packaging/windows/Install-Helios.ps1",
        "umd/src/forward/present.rs",
        RELEASED_WDK_DECLARATION,
        "icd/mesa/src/virtio/vulkan/vn_renderer_helios_hvm.c",
    ):
        path = os.path.join(repo, relative)
        if os.path.isfile(path):
            with open(path, encoding="utf-8", errors="replace") as stream:
                sources[relative] = stream.read()
    return sources


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []

    for path in DELETED_PATHS:
        if path in sources:
            errors.append(f"{path}: retired carrier exists")

    for path, source in sources.items():
        if path == RELEASED_WDK_DECLARATION:
            continue
        if not path.endswith(IMPLEMENTATION_SUFFIXES) and os.path.basename(path) not in BUILD_FILENAMES:
            continue
        live = strip_comments(source)
        if re.search(r"\bD3DKMTEscape\s*\(", live):
            errors.append(f"{path}: active outbound D3DKMTEscape call")
        if re.search(r"SharedGpuResource|IOCTL_SHARED_GPU_RESOURCE", live, re.I):
            errors.append(f"{path}: active SharedGpuResource private transport")
        match = RETIRED_CODE.search(live)
        if match:
            errors.append(f"{path}: retired runtime spelling remains active: {match.group(0)}")
        if re.search(
            r"(?:hps2|present_sync|scanout_acquire).{0,48}(?:poll|watchdog|thread_local|pid|process_name|global_lookup)",
            live,
            re.I | re.S,
        ):
            errors.append(f"{path}: retired carrier gained polling or heuristic lookup")

    lib = strip_comments(sources.get("kmd_render/src/lib.rs", ""))
    if not re.search(
        r"let\s+mut\s+data\s*:\s*DRIVER_INITIALIZATION_DATA\s*=\s*unsafe\s*\{\s*core::mem::zeroed\(\)\s*\}\s*;",
        lib,
    ):
        errors.append("kmd_render/src/lib.rs: DDI table is no longer zero-initialized")
    for slot in ("DxgkDdiEscape", "DxgkDdiMapCpuHostAperture", "DxgkDdiUnmapCpuHostAperture"):
        assignments = re.findall(rf"\bdata\.{slot}\s*=\s*([^;]+);", lib)
        if len(assignments) > 1 or any(value.strip() != "None" for value in assignments):
            errors.append(
                f"kmd_render/src/lib.rs: {slot} must remain zero-initialized or be assigned None exactly once"
            )
        if re.search(rf"\bdata\.{slot}\s*=\s*Some", lib):
            errors.append(f"kmd_render/src/lib.rs: {slot} was re-registered")

    build = strip_comments(sources.get("kmd_render/build.rs", ""))
    for retired in ("seh_shim.c", "escape.rs", "blob_map.rs", "cpu_host_aperture.rs"):
        if retired in build:
            errors.append(f"kmd_render/build.rs: deleted input remains reachable: {retired}")
    helper = strip_comments(sources.get("kmd_render/src/render_user_copy.c", ""))
    for required in (
        "helios_render_copy_user_seh",
        "helios_mm_probe_and_lock_pages_seh",
        "helios_mm_get_mdl_pfn_array",
    ):
        if helper.count(required) != 1:
            errors.append(f"kmd_render/src/render_user_copy.c: bounded surviving helper drifted: {required}")
    for retired in ("helios_mm_map_locked_pages_user_seh", "MmMapLockedPagesSpecifyCache"):
        if retired in helper:
            errors.append(f"kmd_render/src/render_user_copy.c: broad user mapper returned: {retired}")

    installer = sources.get("packaging/windows/Install-Helios.ps1", "")
    for retired in ("helios_present_sync_v2.bin", "presentSyncPath", "PresentSyncPublish"):
        if re.search(re.escape(retired), installer, re.I):
            errors.append(f"packaging/windows/Install-Helios.ps1: HPS2 installer carrier remains: {retired}")

    present = sources.get("umd/src/forward/present.rs", "")
    for required in ("fn dxgi_present(", "fn dxgi_present_mpo(", "pfnPresentCb"):
        if required not in present:
            errors.append(f"umd/src/forward/present.rs: ordinary DXGI Present boundary lost: {required}")
    sharing = sources.get("icd/mesa/src/virtio/vulkan/vn_renderer_helios_hvm.c", "")
    for required in ("D3DKMTShareObjects", "D3DKMTOpenResourceFromNtHandle"):
        if required not in sharing:
            errors.append(f"Mesa HVM1: ordinary documented KMT sharing lost: {required}")
    released = sources.get(RELEASED_WDK_DECLARATION, "")
    if "D3DKMTEscape" not in released:
        errors.append("released WDK D3DKMTEscape declaration was deleted")

    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    text: str
    append: bool = True


def run_mutations(sources: dict[str, str]) -> None:
    cases = (
        Mutation("restore DXVK HPS2 file", DELETED_PATHS[0], "void hps2(void) {}"),
        Mutation("restore Escape call", "umd/src/lib.rs", "\nvoid bad(void){D3DKMTEscape(0);}\n"),
        Mutation("restore SharedGpuResource transport", "umd/src/lib.rs", '\nconst char *bad="\\\\.\\SharedGpuResource";\n'),
        Mutation("restore present stream", "kmd_render/src/adapter/mod.rs", "\nfn present_stream() {}\n"),
        Mutation("restore Escape slot", "kmd_render/src/lib.rs", "\ndata.DxgkDdiEscape = Some(ddi::dxgkddi_escape);\n"),
        Mutation("restore HAP slot", "kmd_render/src/lib.rs", "\ndata.DxgkDdiMapCpuHostAperture = Some(ddi::map);\n"),
        Mutation("restore deleted build input", "kmd_render/build.rs", '\nprintln!("seh_shim.c");\n'),
        Mutation("restore HPS2 installer file", "packaging/windows/Install-Helios.ps1", '\n$p="helios_present_sync_v2.bin"\n'),
        Mutation("restore polling watchdog", "umd/src/lib.rs", "\nfn present_sync_watchdog_poll() {}\n"),
        Mutation("restore retired header declaration", "umd/bridge/dxvk_bridge.h", "\nvoid scanout_acquire();\n"),
        Mutation("restore retired build entry", "dxvk-helios/src/dxvk/meson.build", "\n'dxvk_helios_present_sync.cpp',\n"),
    )
    for case in cases:
        mutated = dict(sources)
        mutated[case.path] = mutated.get(case.path, "") + case.text
        if not check_sources(mutated):
            raise SystemExit(f"HPS2 demolition mutation accepted: {case.name}")
    print(f"OK: {len(cases)} in-memory HPS2 demolition mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else REPO_DEFAULT)
    sources = load_sources(repo)
    # Include deleted-path truth in the same dictionary the mutation suite uses.
    for path in DELETED_PATHS:
        full = os.path.join(repo, path)
        if os.path.exists(full) and path not in sources:
            with open(full, encoding="utf-8", errors="replace") as stream:
                sources[path] = stream.read()
    errors = check_sources(sources)
    if errors:
        raise SystemExit("broad HPS2 demolition gate violated:\n" + "\n".join(errors))
    run_mutations(sources)
    print("OK: zero active HPS2/Escape/private-IOCTL mapper, reader, writer, probe, or installer carrier")


if __name__ == "__main__":
    main()
