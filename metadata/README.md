# Helios vGPU metadata

Helios vGPU is the product; WinBoat is its publisher and developer. Labels have
no trailing periods. This inventory describes source/build output, not an already
updated Windows installation.

## Single sources and edit workflow

- `metadata/helios.env`: product, publisher, component roles, virtual-monitor model year.
- `kmd_render/driver-version.env`: Windows release version, retained at its existing
  path for cargo-make and the win-mcp bumper. A version bump needs no synchronization.
- `metadata/version.rc.in`: shared Windows VERSIONINFO template for the KMD, both
  UMDs and ADL shim. Rust and PowerShell readers substitute the same source values.
- `metadata/helios_branding.h`: generated C header consumed by the Windows Mesa
  forced-include compatibility header. Do not edit it directly.

After changing branding, run `python3 tools/sync-metadata.py`. It synchronizes the
active INX, three component descriptions, author fields in all seven active Rust
packages, and the C header. `--check` rejects drift without writing. Driver and
Mesa CI run that check. A normal KMD build also rejects stale INF branding.
The KMD/UMD build scripts track shared metadata/template changes as rebuild inputs.

The package manifest carries the publisher into installation state, and verification
compares the installed provider with that value. Old bundles/state without the
field retain the historical Helios Project expectation. This is intentional upgrade
compatibility, not a second current publisher setting.

Printable ASCII is used across the formats; quotes/backslashes are rejected. The
monitor descriptors allow 12 text bytes plus a newline, so product and publisher
labels are checked against that limit. A future longer publisher/product needs a
separate short EDID text label. WinBoat and Helios vGPU both fit.

## Final field table

| Field | Final value or policy |
|---|---|
| Product / Windows adapter name | Helios vGPU |
| INF provider and manufacturer label | WinBoat |
| CompanyName in KMD, D3D11 UMD, D3D12 UMD, ADL DLL | WinBoat |
| Cargo authors, all seven active Rust packages | WinBoat |
| HardwareInformation.AdapterString | Same INF DeviceDesc string: Helios vGPU |
| Installation media label | Helios vGPU Driver Package |
| KMD file and Cargo description | Helios vGPU WDDM Render and Display Driver |
| D3D11 UMD file and Cargo description | Helios vGPU Direct3D 11 User-Mode Driver |
| D3D12 UMD file and Cargo description | Helios vGPU Direct3D 12 User-Mode Driver |
| ADL file description | Helios vGPU ADL Compatibility Adapter |
| FileVersion / ProductVersion, four Helios binaries | Shared Windows release version; 22.22.271.0 for this change |
| KMD InternalName / OriginalFilename | helios_kmd_render / helios_kmd_render.sys |
| D3D11 InternalName / OriginalFilename | helios_umd / helios_umd.dll |
| D3D12 InternalName / OriginalFilename | helios_umd12 / helios_umd12.dll |
| ADL InternalName / OriginalFilename | atiadlxx / atiadlxx.dll |
| Resource language / encoding | English US / Unicode |
| Resource file type | KMD: display driver; UMDs and ADL: DLL |
| Resource flags | 0, unchanged policy |
| ADL driver-version queries, both structures | Shared version + Helios vGPU ADL Compatibility Adapter |
| ADL Catalyst / Crimson labels | Helios vGPU |
| Package manifest productName / publisher | Helios vGPU / WinBoat |
| Package version | Shared version; assembly rejects requested/version-resource mismatches |
| Local development signer subject | CN=WinBoat Helios vGPU Development Test Signing |
| Local package public certificate filename | helios-dev-test.cer |
| CI signer subject | CN=WinBoat Helios vGPU GitHub CI Test Signing &lt;commit&gt; |
| CI public certificate filename | helios-ci-test.cer |
| Monitor EDID product name | Helios vGPU |
| Monitor EDID publisher text | WinBoat |
| Monitor model year | 2026; replaces fabricated week 1 of 2024 |
| Monitor physical size | No invented centimeters; aspect ratio derives from current mode |
| Monitor digital interface | Digital, 8 bits/channel; no physical connector standard claimed |
| Monitor color data | Standard sRGB primaries/D65 white point, gamma 2.2 |
| Monitor frequency ranges | No invented range descriptor; explicit preferred timing only |
| Monitor preferred timing | Mode-derived, approximately 60 Hz |
| Monitor manufacturer/product code | Stable legacy HLS / 0x0001 virtual-display identity |
| Monitor serial | 0: no serial assigned |
| Monitor container GUID | {48454C49-4F53-4D4E-5452-000000000001}, stable |
| Monitor EDID version | 1.4, no extension blocks |
| Standard GPU engine | 3D; FriendlyName empty as specified by WDK |
| Windows Helios/Venus OpenGL GL_VENDOR | WinBoat |
| Other Zink backend GL_VENDOR | Mesa, unchanged |
| Backing-GPU vendor text | AMD, NVIDIA or Intel for known IDs; numeric unknown fallback otherwise |
| Vulkan driverInfo in the Windows Helios build | WinBoat Helios vGPU / Mesa &lt;version&gt;&lt;Git suffix&gt; |
| Vulkan driverName / driverID | venus / VK_DRIVER_ID_MESA_VENUS |
| Vulkan deviceName | Virtio-GPU Venus (&lt;backing GPU&gt;), preserving DXVK selection |
| OpenGL renderer | Existing Zink/Vulkan description, including underlying GPU |
| Hardware/API IDs, API versions, UUIDs | Actual interface/implementation values, not replaced with brand strings |
| WDDM surface | 2.1 GPU MMU, unchanged; stale 3.2 description corrected |
| Cargo package versions | 0.1.0, distinct from Windows release versions |

The active Rust packages are kmd_render, umd, umd12, umd_common, protocol,
kmd_logic and tools/win-mcp. Their upstream dependencies retain their own authors.
The Helios-owned Venus backend copyright attribution now says WinBoat; third-party
license/copyright notices remain intact. No blanket copyright replacement was made.

## Corrections and compatibility decisions

- KMD and both UMDs now share complete version resources; the ADL shim gained file
  properties as well. Its stale 22.22.256.0 query string is gone.
- Local cargo-make certificate generation, copying and signing override the WDK
  sample's WDRLocalTestCert settings. The local installer and CI also use WinBoat
  subjects. Building generates/reuses a suitable development certificate; no
  certificates or trust stores were changed during this source edit. Old trusted
  certificates are not removed. An obsolete WDR certificate file is removed from
  a reused package output directory when the new certificate is copied there.
- Package discovery still recognizes older adapter descriptions. Provider
  verification no longer assumes every publisher starts with Helios.
- EDID white-point data was wrong; it now matches the sRGB declaration. Gamma 2.2
  is the standard SDR presentation metadata, not an invented hardware measurement.
- A 128-byte EDID cannot represent every host extent. The previous encoder silently
  truncated widths of 4096 or more and clamped overflowing pixel clocks. Those modes
  now increment EdidModeRejectCount and use the existing 1920x1080 fallback with a
  matching EDID. Supporting larger modes requires an extension/DisplayID; it is not
  represented as working here. Failure to build even the fallback is a named error.
- Zero serial is valid for a virtual display without a manufactured serial number.
  HLS remains the virtual monitor's stable encoded identifier; no registration or
  invented replacement manufacturer code is required for this branding change.
- The empty 3D engine FriendlyName is correct. WDK reserves custom names for OTHER
  engines; changing the engine type merely to display a name would be wrong.
- GPU PCI IDs/vendor names describe the backing hardware, not WinBoat. ADL's
  synthetic AMD ID is retained for its documented Resolve compatibility purpose.
- Mesa, Zink, Venus, DXVK, vkd3d, CLVK and the Khronos loaders keep their implementation
  identities, versions and third-party notices. WinBoat identifies the distributed
  Helios product. Vulkan conformance fields are not a claim of Helios certification.
- Binary filenames, exports, service/registry paths, class GUID, INF feature score,
  hardware matches and UMD registration slots remain unchanged. Archived drivers
  and frozen historical documents are not rewritten.

## Validation

Run the available host checks with:

```sh
python3 tools/sync-metadata.py --check
rustc --edition 2021 --test metadata/windows_resource.rs -o /tmp/helios-metadata-tests
/tmp/helios-metadata-tests
CARGO_TARGET_DIR=target/linux cargo test --manifest-path kmd_logic/Cargo.toml
```

Verified during this edit: all 215 host logic tests; resource parser tests; resource
compilation for KMD, both UMDs and ADL with WinBoat/product/version inspection;
PowerShell parsing for all ten affected scripts, legacy/new publisher selection
and ADL resource generation; C vendor-selection tests; metadata synchronization;
EDID conformity via edid-decode for 1920x1080, 3840x2160 and 1080x1920. Signing-task
overrides were checked against the inherited WDK task definitions.

These checks do not replace full Windows driver/Mesa builds, actual certificate
signing, installation and visible rendering validation. Those remain pending;
nothing was deployed or rebooted, and the release version was not bumped.

References: [VESA E-EDID A2](https://glenwing.github.io/docs/VESA-EEDID-A2.pdf),
[WDK node metadata](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmdt/ns-d3dkmdt-_dxgk_nodemetadata),
[Windows version resources](https://learn.microsoft.com/en-us/windows/win32/menurc/versioninfo-resource).
