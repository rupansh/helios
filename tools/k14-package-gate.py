#!/usr/bin/env python3
"""Source and in-memory mutation gate for K14 package activation metadata."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
VERSION = "kmd_render/driver-version.env"
PROTOCOL = "protocol/src/lib.rs"
HEADERS = (
    "protocol/include/helios_diagnostics.h",
    "protocol/include/helios_native_fence.h",
    "protocol/include/helios_native_render.h",
    "protocol/include/helios_translation_session.h",
    "protocol/include/helios_wddm.h",
)
MESA_BUILD = "ci/windows/build-mesa.sh"
DRIVER_BUILD = "ci/windows/Build-Driver.ps1"
ASSEMBLE = "ci/windows/Assemble-Package.ps1"
COMMON = "packaging/windows/Helios-PackageCommon.ps1"
INSTALL = "packaging/windows/Install-Helios.ps1"
VERIFY = "packaging/windows/Verify-Helios.ps1"
UNINSTALL = "packaging/windows/Uninstall-Helios.ps1"
RETIREMENT = "tools/retirement-gates.sh"
INF = "kmd_render/helios_kmd_render.inx"
CARGO_MAKE = "kmd_render/Cargo.make.toml"
LAYER_HEADER = "icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.h"
LAYER_DEF = "icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.def"
LOWER_DEF = "icd/mesa/src/virtio/vulkan/vn_helios_exports.def"
DIRECT = "icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.c"

PACKAGE_GENERATION = "0x48454C4900000004"
RUST_GENERATION = "0x4845_4C49_0000_0004"
REQUIRED_PAYLOADS = (
    "payload/driver/helios_kmd_render.inf",
    "payload/driver/helios_kmd_render.sys",
    "payload/driver/helios_umd.dll",
    "payload/driver/helios_umd12.dll",
    "payload/mesa/vulkan_virtio.dll",
    "payload/mesa/VkLayer_HELIOS_present.dll",
    "payload/mesa/VkLayer_HELIOS_present.json",
    "payload/mesa/libgallium_wgl.dll",
)
PATHS = (
    VERSION,
    PROTOCOL,
    *HEADERS,
    MESA_BUILD,
    DRIVER_BUILD,
    ASSEMBLE,
    COMMON,
    INSTALL,
    VERIFY,
    UNINSTALL,
    RETIREMENT,
    INF,
    CARGO_MAKE,
    LAYER_HEADER,
    LAYER_DEF,
    LOWER_DEF,
    DIRECT,
)


def require(errors: list[str], path: str, source: str, fragments: tuple[str, ...]) -> None:
    for fragment in fragments:
        if fragment not in source:
            errors.append(f"{path}: required K14 fragment missing: {fragment}")


def require_order(errors: list[str], path: str, source: str, fragments: tuple[str, ...]) -> None:
    cursor = -1
    for fragment in fragments:
        found = source.find(fragment, cursor + 1)
        if found < 0:
            errors.append(f"{path}: K14 ordering lost at: {fragment}")
            return
        cursor = found


def exports(source: str) -> list[str]:
    lines = [line.strip() for line in source.splitlines()]
    start = lines.index("EXPORTS") if "EXPORTS" in lines else -1
    return [line for line in lines[start + 1 :] if line and not line.startswith(";")]


def check(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    version = sources[VERSION]
    if version.count("HELIOS_KMD_VERSION=") != 1 or "HELIOS_KMD_VERSION=22.22.297.0" not in version:
        errors.append(f"{VERSION}: KMD version must advance exactly once to 22.22.297.0")
    if "22.22.296.0" in version:
        errors.append(f"{VERSION}: prior K11 version remains active")

    protocol = sources[PROTOCOL]
    require(
        errors,
        PROTOCOL,
        protocol,
        (
            "pub const HELIOS_PACKAGE_GENERATION_ORDINAL: u32 = 4;",
            RUST_GENERATION,
        ),
    )
    for path in HEADERS:
        require(errors, path, sources[path], ("HELIOS_PACKAGE_GENERATION_ORDINAL 4u",))
        if path != "protocol/include/helios_native_fence.h":
            require(errors, path, sources[path], (PACKAGE_GENERATION,))

    mesa = sources[MESA_BUILD]
    require(
        errors,
        MESA_BUILD,
        mesa,
        (
            "-Dvulkan-layers=helios-present",
            'VkLayer_HELIOS_present.dll" "${output_dir}/"',
            'VkLayer_HELIOS_present.json" "${output_dir}/"',
            '"${output_dir}/VkLayer_HELIOS_present.dll"',
        ),
    )
    driver = sources[DRIVER_BUILD]
    require(
        errors,
        DRIVER_BUILD,
        driver,
        (
            '@("helios_kmd_render.inf", "helios_kmd_render.sys", "helios_umd.dll", "helios_umd12.dll")',
            '$umd12Dll = Join-Path $package "helios_umd12.dll"',
            "(?:dxgi|d3d12|vulkan-1)\\.dll",
        ),
    )

    assemble = sources[ASSEMBLE]
    require(
        errors,
        ASSEMBLE,
        assemble,
        (
            '$packageGeneration = "0x48454C4900000004"',
            "schemaVersion = 2",
            "packageGeneration = $packageGeneration",
            '@("helios_kmd_render.inf", "helios_kmd_render.sys", "helios_umd.dll", "helios_umd12.dll")',
            '@("vulkan_virtio.dll", "VkLayer_HELIOS_present.dll", "VkLayer_HELIOS_present.json", "libgallium_wgl.dll")',
            'Invoke-SignTool $signTool $certificate.Thumbprint (Join-Path $driverOut "helios_umd12.dll")',
            "Package version $Version does not match kmd_render/driver-version.env",
        ),
    )

    common = sources[COMMON]
    require(
        errors,
        COMMON,
        common,
        (
            '$script:HeliosPackageGeneration = "0x48454C4900000004"',
            "$manifest.schemaVersion -ne 2",
            "[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)",
            "if (-not $seen.Add($relative))",
            "Duplicate path in package manifest",
            "Required generation payload is missing from the manifest",
            "The signing certificate is missing from the package manifest",
        ) + REQUIRED_PAYLOADS,
    )

    install = sources[INSTALL]
    if re.search(r"helios_present_sync_v2|PresentSyncPublish|\bHPS2\b", install, re.I):
        errors.append(f"{INSTALL}: retired HPS2 creation, ACL, state, or deletion returned")
    require(
        errors,
        INSTALL,
        install,
        (
            "schemaVersion = 2",
            "packageGeneration = [string]$manifest.packageGeneration",
            '"HKLM:\\SOFTWARE\\Khronos\\Vulkan\\ImplicitLayers"',
            '"mesa\\VkLayer_HELIOS_present.json"',
            '"mesa\\VkLayer_HELIOS_present.dll"',
            '[string]$presentLayerJson.layer.name -cne "VK_LAYER_HELIOS_present"',
            "$presentLayerJson.layer.library_path = ($presentLayerDllPath -replace \"\\\\\", \"/\")",
            "New-ItemProperty -LiteralPath $vulkanImplicitLayerRegistry -Name $presentLayerManifestPath -Value 0",
        ),
    )
    require_order(
        errors,
        INSTALL,
        install,
        (
            'Copy-Item -Path (Join-Path $payloadRoot "mesa")',
            "$presentLayerJson = Get-Content",
            "Write-HeliosJson $presentLayerJson",
            "Write-HeliosJson $vulkanJson",
            "foreach ($file in Get-ChildItem -LiteralPath $runtimeRoot -File -Recurse)",
            "New-ItemProperty -LiteralPath $vulkanImplicitLayerRegistry",
        ),
    )

    require(
        errors,
        VERIFY,
        sources[VERIFY],
        (
            "$state.schemaVersion -ne 2",
            "$script:HeliosPackageGeneration",
            '"HKLM:\\SOFTWARE\\Khronos\\Vulkan\\ImplicitLayers"',
            '"VK_LAYER_HELIOS_present"',
            '"runtime\\mesa\\VkLayer_HELIOS_present.dll"',
        ),
    )
    require(
        errors,
        UNINSTALL,
        sources[UNINSTALL],
        (
            '"HKLM:\\SOFTWARE\\Khronos\\Vulkan\\ImplicitLayers"',
            "Remove-ItemProperty -LiteralPath $vulkanImplicitLayerRegistry -Name ([string]$state.presentLayerManifest)",
        ),
    )

    require(
        errors,
        INF,
        sources[INF],
        (
            "helios_umd12.dll = 1,,",
            "%13%\\helios_umd.dll,%13%\\helios_umd.dll,%13%\\helios_umd.dll,%13%\\helios_umd12.dll",
        ),
    )
    require(errors, CARGO_MAKE, sources[CARGO_MAKE], ('("umd12", "helios_umd12.dll"',))
    require(
        errors,
        LAYER_HEADER,
        sources[LAYER_HEADER],
        (
            "translators never enter here",
            "DXVK and vkd3d reach the ICD",
            "private direct-dispatch entry point and never see this layer",
        ),
    )
    if "helios_icd_create_translator_v1(" not in sources[DIRECT]:
        errors.append(f"{DIRECT}: A5 direct translator entry point lost")
    if exports(sources[LOWER_DEF]) != [
        "vk_icdNegotiateLoaderICDInterfaceVersion",
        "vk_icdGetInstanceProcAddr",
        "vk_icdGetPhysicalDeviceProcAddr",
        "helios_icd_create_translator_v1",
    ]:
        errors.append(f"{LOWER_DEF}: lower ICD four-export boundary drifted")
    if exports(sources[LAYER_DEF]) != [
        "vkNegotiateLoaderLayerInterfaceVersion",
        "vkGetInstanceProcAddr",
        "vkGetDeviceProcAddr",
        "vk_layerGetPhysicalDeviceProcAddr",
        "vkEnumerateInstanceLayerProperties",
        "vkEnumerateInstanceExtensionProperties",
        "vkEnumerateDeviceLayerProperties",
        "vkEnumerateDeviceExtensionProperties",
    ]:
        errors.append(f"{LAYER_DEF}: present-layer eight-export boundary drifted")

    suite = sources[RETIREMENT]
    for gate in (
        "hps2-demolition-gate.py",
        "k8-k10-lifecycle-gate.py",
        "k14-package-gate.py",
    ):
        if len(re.findall(rf"(?m)^\s*python3\s+\"\$REPO/tools/{re.escape(gate)}\"\s+\"\$REPO\"\s*$", suite)) != 1:
            errors.append(f"{RETIREMENT}: {gate} must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def run_mutations(sources: dict[str, str]) -> None:
    cases = (
        Mutation("roll generation back", PROTOCOL, "HELIOS_PACKAGE_GENERATION_ORDINAL: u32 = 4;", "HELIOS_PACKAGE_GENERATION_ORDINAL: u32 = 3;"),
        Mutation("reuse K11 driver version", VERSION, "22.22.297.0", "22.22.296.0"),
        Mutation("omit packaged UMD12", ASSEMBLE, '"helios_umd.dll", "helios_umd12.dll"', '"helios_umd.dll"'),
        Mutation("omit present-layer manifest", ASSEMBLE, '"VkLayer_HELIOS_present.dll", "VkLayer_HELIOS_present.json"', '"VkLayer_HELIOS_present.dll"'),
        Mutation("accept old manifest schema", COMMON, "$manifest.schemaVersion -ne 2", "$manifest.schemaVersion -ne 1"),
        Mutation("drop duplicate manifest rejection", COMMON, "if (-not $seen.Add($relative))", "if ($false)"),
        Mutation("restore HPS2 installer file", INSTALL, "Assert-HeliosAdministrator", '$hps = "helios_present_sync_v2.bin"\nAssert-HeliosAdministrator'),
        Mutation("drop implicit layer registration", INSTALL, "New-ItemProperty -LiteralPath $vulkanImplicitLayerRegistry -Name $presentLayerManifestPath -Value 0", "Write-Host $presentLayerManifestPath"),
        Mutation("point layer at lower ICD", INSTALL, "$presentLayerJson.layer.library_path = ($presentLayerDllPath", "$presentLayerJson.layer.library_path = ($vulkanDll"),
        Mutation("let translators enter the layer", LAYER_HEADER, "private direct-dispatch entry point and never see this layer", "Vulkan loader and may see this layer"),
        Mutation("drop UMD12 import boundary", DRIVER_BUILD, "$umd12Dll = Join-Path $package", "$unusedUmd12Dll = Join-Path $package"),
        Mutation("delete an unproven mapped legacy file", INSTALL, "Assert-HeliosAdministrator", 'Remove-Item "C:\\ProgramData\\Helios\\helios_present_sync_v2.bin"\nAssert-HeliosAdministrator'),
    )
    for case in cases:
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"K14 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check(mutated):
            raise SystemExit(f"K14 mutation accepted: {case.name}")
    print(f"OK: {len(cases)} in-memory K14 package mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else REPO_DEFAULT)
    sources: dict[str, str] = {}
    for path in PATHS:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as stream:
            sources[path] = stream.read()
    errors = check(sources)
    if errors:
        raise SystemExit("K14 package gate violated:\n" + "\n".join(errors))
    run_mutations(sources)
    print("OK: generation 4 / KMD 22.22.297.0 is one complete lower/layer package source")


if __name__ == "__main__":
    main()
