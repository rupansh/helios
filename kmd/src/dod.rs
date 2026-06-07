//! WDDM Display-Only Driver (DOD) DDIs.
//!
//! `lib.rs` wires these into the `KMDDOD_INITIALIZATION_DATA` table and registers
//! it with dxgkrnl via `DxgkInitializeDisplayOnlyDriver`. dxgkrnl then drives the
//! device lifecycle (AddDevice → StartDevice → the VidPN/present DDIs) through
//! these callbacks. Reference: the inbox `VioGpuDod` + `qxldod` (DISPLAY.md §3,
//! §5; the per-DDI blueprint is in the Phase-7.1 research handover).
//!
//! Phase split:
//!  - 7.1a (this cut): the device loads as a Display adapter (Code 0) — full
//!    lifecycle (AddDevice/StartDevice/Stop/Remove), QueryAdapterInfo(DRIVERCAPS),
//!    a single monitor child, the 2D desktop scanout brought up at StartDevice,
//!    and `DxgkDdiPresentDisplayOnly` painting the primary. The VidPN
//!    mode-negotiation DDIs are conservative stubs (marked `// STUB (7.1b)`).
//!  - 7.1b: flesh out EnumVidPnCofuncModality / CommitVidPn / RecommendMonitorModes
//!    against the generated VidPN interface bindings so the desktop mode commits.
//!  - 7.2: `DxgkDdiEscape` carries the venus ops (body = today's ioctl.rs).

use core::ffi::c_void;
use core::mem::size_of;
use core::sync::atomic::Ordering;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::adapter::AdapterContext;
use crate::dxgk::*;
use crate::virtio::{DxgkConfigAccess, VirtioGpu};

/// Single-output DOD: one video-present source, one monitor child.
pub(crate) const MAX_VIEWS: u32 = 1;
pub(crate) const MAX_CHILDREN: u32 = 1;
/// The monitor child id == the VidPN target id == 0 (must match everywhere).
const CHILD_UID: u32 = 0;
/// Default desktop mode when no POST framebuffer geometry is available.
const DEFAULT_WIDTH: u32 = 1024;
const DEFAULT_HEIGHT: u32 = 768;
const BYTES_PER_PIXEL: u32 = 4;

/// Recover the `&AdapterContext` from the opaque miniport device context dxgkrnl
/// hands back on every DDI.
///
/// # Safety
/// `ctx` must be the pointer AddDevice returned (a `Box<AdapterContext>` leaked
/// to a raw pointer), still live (freed only in RemoveDevice).
unsafe fn adapter<'a>(ctx: *mut c_void) -> Option<&'a AdapterContext> {
    if ctx.is_null() {
        None
    } else {
        Some(unsafe { &*(ctx as *const AdapterContext) })
    }
}

// ── Lifecycle ───────────────────────────────────────────────────────────────

/// `DxgkDdiAddDevice` — allocate the adapter context for a discovered device.
pub unsafe extern "C" fn add_device(
    physical_device_object: PDEVICE_OBJECT,
    miniport_device_context: *mut *mut c_void,
) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: AddDevice\n");
    crate::diag::record(0x0100_0000);
    if miniport_device_context.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let ctx = match AdapterContext::new(physical_device_object) {
        Ok(c) => c,
        Err(e) => return e.into_ntstatus(),
    };
    // Leak to a raw pointer; dxgkrnl hands it back on every DDI, reclaimed in
    // RemoveDevice.
    let raw = Box::into_raw(Box::new(ctx)) as *mut c_void;
    // SAFETY: valid out-pointer per the DDI contract.
    unsafe { *miniport_device_context = raw };
    STATUS_SUCCESS
}

/// `DxgkDdiStartDevice` — save the dxgkrnl interface, bring up the virtio-gpu
/// transport, report one source + one child, and install the default desktop
/// scanout so the host shows the (black) primary immediately.
pub unsafe extern "C" fn start_device(
    miniport_device_context: *mut c_void,
    _dxgk_start_info: *mut DXGK_START_INFO,
    dxgkrnl_interface: *mut DXGKRNL_INTERFACE,
    number_of_video_present_sources: *mut u32,
    number_of_children: *mut u32,
) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: StartDevice\n");
    crate::diag::record(0x0200_0000);
    if miniport_device_context.is_null()
        || dxgkrnl_interface.is_null()
        || number_of_video_present_sources.is_null()
        || number_of_children.is_null()
    {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl passes our adapter context + valid out-pointers. StartDevice
    // is serialized w.r.t. other DDIs, so the &mut to write `dxgkrnl` is sound.
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    // Save the callback interface for the driver's lifetime (Copy struct).
    adapter.dxgkrnl = Some(unsafe { *dxgkrnl_interface });
    adapter.reset_current_mode();

    // Bring up the virtio transport (PCI cap scan + BAR map + virtqueue) over the
    // Dxgkrnl config-space callbacks. Hard-fail StartDevice on error so a bring-up
    // failure surfaces distinctly in the device's problem code.
    adapter.set_virtio(None);
    let access = DxgkConfigAccess::new(unsafe { &*dxgkrnl_interface });
    match VirtioGpu::init(&access) {
        Ok(gpu) => {
            crate::kmsg(c"Helios DOD: virtio-gpu transport up\n");
            // Publish the ISR status register VA BEFORE any command that could make
            // the device assert an interrupt, so DxgkDdiInterruptRoutine can ack it
            // (read de-asserts INTx) rather than leaving it asserted → storm/hang.
            let isr_va = gpu.isr_status_va();
            adapter.set_virtio(Some(gpu));
            adapter.isr_status_va.store(isr_va, Ordering::Release);
        }
        Err(e) => {
            crate::kmsg(c"Helios DOD: virtio-gpu init FAILED\n");
            // NOTE: do NOT record() here — it would overwrite the last init milestone
            // in HeliosStep, which is exactly the localization we want (init RETURNS
            // Err, confirmed; the surviving milestone shows WHERE: 0x2F feature-reject,
            // 0x23 entering VirtQueue::new, etc.).
            return e.into();
        }
    }
    crate::diag::record(0x0200_0001);

    // Single monitor on one source/target.
    // SAFETY: out-pointers validated non-null above.
    unsafe {
        *number_of_video_present_sources = MAX_VIEWS;
        *number_of_children = MAX_CHILDREN;
    }

    // Install the default desktop scanout (best-effort): proves the 2D scanout
    // path end-to-end (the host shows a black primary) even before the VidPN
    // mode-commit flow runs. A failure here does NOT fail StartDevice — the mode
    // is (re)set later by CommitVidPn.
    let _ = set_desktop_mode(adapter, DEFAULT_WIDTH, DEFAULT_HEIGHT, false);
    crate::diag::record(0x0200_00FF);

    STATUS_SUCCESS
}

/// `DxgkDdiStopDevice` — quiesce: tear down the virtio transport (drops the
/// scanout fb + resets the device).
pub unsafe extern "C" fn stop_device(miniport_device_context: *mut c_void) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: StopDevice\n");
    crate::diag::record_step_only(0x1100_0000); // TEMP: StopDevice entered (preserve HeliosPost)
    if let Some(adapter) = unsafe { adapter(miniport_device_context) } {
        // Tear the transport down first (VirtioGpu::drop resets the device so it
        // stops asserting), THEN stop the ISR from acking (its register is going away
        // conceptually; the mapping itself survives in the WdkHal cache).
        adapter.set_virtio(None);
        crate::diag::record_step_only(0x1100_0001); // TEMP: VirtioGpu::drop completed
        adapter.isr_status_va.store(0, Ordering::Release);
    }
    crate::diag::record_step_only(0x1100_00FF); // TEMP: StopDevice returning SUCCESS
    STATUS_SUCCESS
}

/// `DxgkDdiRemoveDevice` — free the adapter context allocated in AddDevice.
pub unsafe extern "C" fn remove_device(miniport_device_context: *mut c_void) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: RemoveDevice\n");
    if !miniport_device_context.is_null() {
        // SAFETY: came from Box::into_raw in AddDevice; freed exactly once.
        drop(unsafe { Box::from_raw(miniport_device_context as *mut AdapterContext) });
    }
    STATUS_SUCCESS
}

/// `DxgkDdiStopDeviceAndReleasePostDisplayOwnership` — hand the framebuffer back
/// to the OS for the post-display path. We have no cached POST geometry yet
/// (7.1b), so report the current mode and stop.
pub unsafe extern "C" fn stop_device_and_release_post_display_ownership(
    miniport_device_context: *mut c_void,
    _target_id: u32,
    display_info: *mut DXGK_DISPLAY_INFORMATION,
) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: StopDeviceAndReleasePostDisplayOwnership\n");
    if !display_info.is_null() {
        // SAFETY: dxgkrnl provides a valid out struct; zero it (no POST handoff).
        unsafe { core::ptr::write_bytes(display_info as *mut u8, 0, size_of::<DXGK_DISPLAY_INFORMATION>()) };
    }
    if let Some(adapter) = unsafe { adapter(miniport_device_context) } {
        adapter.set_virtio(None);
        adapter.isr_status_va.store(0, Ordering::Release);
    }
    STATUS_SUCCESS
}

/// `DxgkDdiUnload` — driver-wide unload; release the cached BAR MMIO mappings.
pub unsafe extern "C" fn unload() {
    crate::kmsg(c"Helios DOD: Unload\n");
    crate::virtio::hal::WdkHal::unmap_all();
}

// ── Adapter info / children ─────────────────────────────────────────────────

/// `DxgkDdiQueryAdapterInfo` — a DOD answers only `DXGKQAITYPE_DRIVERCAPS`, and
/// sets a minimal cap set (NO segment / GPU-engine / flip caps — leaving those
/// zero is what keeps dxgkrnl treating us as display-only, not a render adapter).
pub unsafe extern "C" fn query_adapter_info(
    _h_adapter: *mut c_void,
    p_query_adapter_info: *const DXGKARG_QUERYADAPTERINFO,
) -> NTSTATUS {
    if p_query_adapter_info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract; we only read the args.
    let args = unsafe { &*p_query_adapter_info };
    crate::diag::record(0x0300_0000 | (args.Type as u32 & 0xFFFF));
    match args.Type {
        _DXGK_QUERYADAPTERINFOTYPE::DXGKQAITYPE_DRIVERCAPS => {
            if (args.OutputDataSize as usize) < size_of::<DXGK_DRIVERCAPS>() {
                return STATUS_BUFFER_TOO_SMALL;
            }
            // SAFETY: pOutputData points to a DXGK_DRIVERCAPS of sufficient size.
            let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_DRIVERCAPS) };
            unsafe {
                core::ptr::write_bytes(caps as *mut _ as *mut u8, 0, size_of::<DXGK_DRIVERCAPS>())
            };
            // 64-bit addressable; not a legacy VGA device. No HW cursor advertised
            // (PointerCaps stays zero) → dxgkrnl composites a software cursor into
            // the present source, so we need no virtio cursor queue yet.
            caps.HighestAcceptableAddress.QuadPart = -1;
            caps.SupportNonVGA = 1;
            // Match VioGpuDod's rotation contract: DMM may pin Identity or Rotate90,
            // and PresentDisplayOnly rotates the blit when dxgkrnl sets Flags.Rotate.
            caps.SupportSmoothRotation = 1;
            // A DOD reports its WDDM version in DRIVERCAPS. The Microsoft KMDOD sample
            // sets DXGKDDI_WDDMv1_2 here (even though it compiles at the native interface
            // version), so mirror that exactly.
            caps.WDDMVersion = _DXGK_WDDMVERSION::DXGKDDI_WDDMv1_2;
            STATUS_SUCCESS
        }
        // EXPERIMENT (un-bundling): the OS queries this at start time on WDDM 2.0+
        // (build 26100); KMDOD answers it with VirtualModeSupport=1 (bdd.cxx).
        // Setting it made dxgkrnl drive a SOURCE-PIVOT cofunc enum whose target
        // AddMode our fixed timing failed (0xC01E030A) → no cofunctional VidPN → no
        // present. Reverted to NOT_SUPPORTED to confirm that correlation while
        // keeping the HeliosQai breadcrumb (proves the query still arrives). The
        // Code-43 fix is the DDI-table trim (lib.rs), not this cap.
        _DXGK_QUERYADAPTERINFOTYPE::DXGKQAITYPE_DISPLAY_DRIVERCAPS_EXTENSION => {
            crate::diag::record_qai(0x0300_0010); // sticky: extension query arrived
            STATUS_NOT_SUPPORTED
        }
        _ => STATUS_NOT_SUPPORTED,
    }
}

/// `DxgkDdiQueryChildRelations` — report the single monitor child (video output
/// on target 0). The array dxgkrnl passes has room for `size/sizeof - 1` entries
/// plus a NULL terminator; we fill index 0.
pub unsafe extern "C" fn query_child_relations(
    _miniport_device_context: *mut c_void,
    child_relations: *mut DXGK_CHILD_DESCRIPTOR,
    child_relations_size: u32,
) -> NTSTATUS {
    crate::diag::record(0x0400_0000);
    if child_relations.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let count = child_relations_size as usize / size_of::<DXGK_CHILD_DESCRIPTOR>();
    if count == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl provides a zeroed array of `count` descriptors.
    let desc = unsafe { &mut *child_relations };
    desc.ChildDeviceType = _DXGK_CHILD_DEVICE_TYPE::TypeVideoOutput;
    desc.ChildCapabilities.HpdAwareness = _DXGK_CHILD_DEVICE_HPD_AWARENESS::HpdAwarenessInterruptible;
    // VOT_HD15. NOTE: VOT_INTERNAL was tried (KMDOD uses VOT_INTERNAL/VOT_OTHER) and
    // REGRESSES bring-up — dxgkrnl then never even enumerates the VidPN for the
    // internal panel (it expects EDID/descriptor data we don't supply), so no commit/
    // present happens at all. HD15 is what lets the VidPN flow run.
    desc.ChildCapabilities
        .Type
        .VideoOutput
        .InterfaceTechnology = _D3DKMDT_VIDEO_OUTPUT_TECHNOLOGY::D3DKMDT_VOT_HD15;
    desc.ChildCapabilities
        .Type
        .VideoOutput
        .MonitorOrientationAwareness = _D3DKMDT_MONITOR_ORIENTATION_AWARENESS::D3DKMDT_MOA_NONE;
    desc.ChildCapabilities.Type.VideoOutput.SupportsSdtvModes = 0;
    desc.AcpiUid = 0;
    desc.ChildUid = CHILD_UID;
    STATUS_SUCCESS
}

/// `DxgkDdiQueryChildStatus` — the monitor is always connected while we are
/// started.
pub unsafe extern "C" fn query_child_status(
    miniport_device_context: *mut c_void,
    child_status: *mut DXGK_CHILD_STATUS,
    _non_destructive_only: BOOLEAN,
) -> NTSTATUS {
    if child_status.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract.
    let status = unsafe { &mut *child_status };
    crate::diag::record(0x0500_0000 | (status.Type as u32 & 0xFFFF));
    match status.Type {
        _DXGK_CHILD_STATUS_TYPE::StatusConnection => {
            // Always connected (virtual monitor). NOTE: coupling this to the virtio
            // transport being bound REGRESSES bring-up — dxgkrnl queries child status
            // before StartDevice binds virtio, so reporting Connected=0 there makes it
            // mark the monitor permanently disconnected and never enumerate the VidPN.
            // The HotPlug arm of the union is valid for StatusConnection.
            let _ = miniport_device_context;
            status.__bindgen_anon_1.HotPlug.Connected = 1;
            STATUS_SUCCESS
        }
        _ => STATUS_NOT_SUPPORTED,
    }
}

/// `DxgkDdiQueryDeviceDescriptor` — no EDID; dxgkrnl uses our RecommendMonitorModes
/// instead.
pub unsafe extern "C" fn query_device_descriptor(
    _miniport_device_context: *mut c_void,
    _child_uid: u32,
    _device_descriptor: *mut DXGK_DEVICE_DESCRIPTOR,
) -> NTSTATUS {
    crate::diag::record(0x0600_0000);
    STATUS_MONITOR_NO_MORE_DESCRIPTOR_DATA
}

// ── Power / IO / interrupts (minimal) ───────────────────────────────────────

/// `DxgkDdiSetPowerState` — accept all transitions (no device-specific work yet).
pub unsafe extern "C" fn set_power_state(
    _miniport_device_context: *mut c_void,
    _device_uid: u32,
    _device_power_state: DEVICE_POWER_STATE,
    _action_type: POWER_ACTION::Type,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiDispatchIoRequest` — DODs receive no legacy VRP IO.
pub unsafe extern "C" fn dispatch_io_request(
    _miniport_device_context: *mut c_void,
    _vidpn_source_id: u32,
    _video_request_packet: PVIDEO_REQUEST_PACKET,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiInterruptRoutine` — the control path polls the used ring, but the device
/// still asserts INTx for events we don't suppress (notably config-change/display
/// events, which `set_dev_notify(false)` does NOT mask). The ONLY way to de-assert a
/// virtio INTx line is to read its ISR status register; if we never do, an asserted
/// interrupt re-fires forever → interrupt storm that hard-hangs the guest. So read it
/// here (the read both acks and de-asserts) and claim the interrupt when it was ours.
pub unsafe extern "C" fn interrupt_routine(
    miniport_device_context: *mut c_void,
    _message_number: u32,
) -> BOOLEAN {
    let Some(adapter) = (unsafe { adapter(miniport_device_context) }) else {
        return 0;
    };
    let va = adapter.isr_status_va.load(Ordering::Acquire);
    if va == 0 {
        return 0; // no mapped ISR register (or transport down) — not ours
    }
    // SAFETY: `va` is the device's mapped 1-byte VIRTIO_PCI_ISR status register, valid
    // for the driver's lifetime (WdkHal cache, released only at DxgkDdiUnload). A single
    // volatile read ACKs + de-asserts the virtio interrupt (virtio 1.x §4.1.4.5);
    // it is a plain MMIO read, valid at DIRQL.
    let isr = unsafe { core::ptr::read_volatile(va as *const u8) };
    // Bit 0 = used-ring, bit 1 = config-change. Non-zero ⇒ the interrupt was ours and
    // is now acked → claim it; zero ⇒ a shared-line interrupt for another device.
    if isr & 0x3 != 0 {
        1
    } else {
        0
    }
}

/// `DxgkDdiDpcRoutine` — nothing to drain while polling.
pub unsafe extern "C" fn dpc_routine(_miniport_device_context: *mut c_void) {}

/// `DxgkDdiQueryInterface` — we export no driver-defined interface.
pub unsafe extern "C" fn query_interface(
    _miniport_device_context: *mut c_void,
    _query_interface: *mut _QUERY_INTERFACE,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

// ── VidPN (mode negotiation) ────────────────────────────────────────────────
// 7.1a: conservative stubs that let the adapter load. The full cofunctional
// modality enumeration + mode commit (what actually makes the desktop appear)
// lands in 7.1b against the generated VidPN interface bindings.

/// `DxgkDdiIsSupportedVidPn` — a VidPN is rejected by setting the out bool FALSE,
/// never by returning a failure status. We accept all (single source/target).
pub unsafe extern "C" fn is_supported_vidpn(
    h_adapter: *mut c_void,
    p_is_supported_vidpn: *mut DXGKARG_ISSUPPORTEDVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x0700_0000);
    let Some(adapter) = (unsafe { adapter(h_adapter) }) else {
        return STATUS_INVALID_PARAMETER;
    };
    unsafe { crate::vidpn::is_supported_vidpn(adapter, p_is_supported_vidpn) }
}

/// `DxgkDdiRecommendFunctionalVidPn` — we recommend none (dxgkrnl builds one).
pub unsafe extern "C" fn recommend_functional_vidpn(
    _h_adapter: *mut c_void,
    _p_recommend_functional_vidpn: *const _DXGKARG_RECOMMENDFUNCTIONALVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x0F00_0000);
    STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN
}

/// `DxgkDdiEnumVidPnCofuncModality`.
// STUB (7.1b): walk the topology and pin/assign the source+target mode sets.
pub unsafe extern "C" fn enum_vidpn_cofunc_modality(
    h_adapter: *mut c_void,
    p_enum_cofunc_modality: *const _DXGKARG_ENUMVIDPNCOFUNCMODALITY,
) -> NTSTATUS {
    crate::diag::record(0x0800_0000);
    let Some(adapter) = (unsafe { adapter(h_adapter) }) else {
        return STATUS_INVALID_PARAMETER;
    };
    unsafe { crate::vidpn::enum_vidpn_cofunc_modality(adapter, p_enum_cofunc_modality) }
}

/// `DxgkDdiSetVidPnSourceVisibility` — track visibility; blackout handled at the
/// scanout when hidden (7.1b).
pub unsafe extern "C" fn set_vidpn_source_visibility(
    h_adapter: *mut c_void,
    p_set_vidpn_source_visibility: *const _DXGKARG_SETVIDPNSOURCEVISIBILITY,
) -> NTSTATUS {
    // Record the Visible flag (sticky `HeliosVis`): a healthy post-commit bring-up
    // calls this with Visible=TRUE just before PresentDisplayOnly; a Visible=FALSE
    // here means dxgkrnl is blanking/tearing the source down (the reject already
    // happened upstream). Low bit = Visible.
    let (source_id, visible) = if p_set_vidpn_source_visibility.is_null() {
        (0xFF, 0)
    } else {
        // SAFETY: non-null per the check; Visible is a plain BOOLEAN field.
        let arg = unsafe { &*p_set_vidpn_source_visibility };
        (arg.VidPnSourceId & 0xFF, (arg.Visible as u32) & 1)
    };
    let vis_code = 0x0C00_0000 | (source_id << 8) | visible;
    crate::diag::record(vis_code);
    crate::diag::record_vis(vis_code);
    if let Some(adapter) = unsafe { adapter(h_adapter) } {
        adapter.set_source_visible(visible != 0);
    }
    STATUS_SUCCESS
}

/// `DxgkDdiCommitVidPn`.
// STUB (7.1b): read the pinned source mode and (re)program the scanout via
// set_desktop_mode at the committed resolution.
pub unsafe extern "C" fn commit_vidpn(
    h_adapter: *mut c_void,
    p_commit_vidpn: *const _DXGKARG_COMMITVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x0900_0000);
    crate::diag::record_commit(0x0900_0000); // sticky: CommitVidPn fired
    let Some(adapter) = (unsafe { adapter(h_adapter) }) else {
        return STATUS_INVALID_PARAMETER;
    };
    unsafe { crate::vidpn::commit_vidpn(adapter, p_commit_vidpn) }
}

/// `DxgkDdiUpdateActiveVidPnPresentPath`.
pub unsafe extern "C" fn update_active_vidpn_present_path(
    h_adapter: *mut c_void,
    p_update_active_vidpn_present_path: *const _DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH,
) -> NTSTATUS {
    crate::diag::record(0x0E00_0000);
    if p_update_active_vidpn_present_path.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*p_update_active_vidpn_present_path };
    let reason = crate::vidpn::active_present_path_reason(&arg.VidPnPresentPathInfo);
    if reason != 0 {
        crate::diag::record(0x0E00_0000 | reason);
        return 0xC01E_0306u32 as NTSTATUS; // STATUS_GRAPHICS_VIDPN_MODALITY_NOT_SUPPORTED
    }
    if let Some(adapter) = unsafe { adapter(h_adapter) } {
        adapter.mark_fullscreen_present();
        adapter.set_rotation(arg.VidPnPresentPathInfo.ContentTransformation.Rotation);
    }
    STATUS_SUCCESS
}

/// `DxgkDdiRecommendMonitorModes`.
// STUB (7.1b): add the supported monitor modes (current = preferred).
pub unsafe extern "C" fn recommend_monitor_modes(
    _h_adapter: *mut c_void,
    p_recommend_monitor_modes: *const _DXGKARG_RECOMMENDMONITORMODES,
) -> NTSTATUS {
    crate::diag::record(0x0A00_0000);
    unsafe { crate::vidpn::recommend_monitor_modes(p_recommend_monitor_modes) }
}

/// `DxgkDdiQueryVidPnHWCapability` — no HW transform offloads; zeroed caps
/// (everything driver-handled by dxgkrnl) is correct for a DOD.
pub unsafe extern "C" fn query_vidpn_hw_capability(
    _h_adapter: *mut c_void,
    io_p_vidpn_hw_caps: *mut _DXGKARG_QUERYVIDPNHWCAPABILITY,
) -> NTSTATUS {
    crate::diag::record(0x0B00_0000);
    if !io_p_vidpn_hw_caps.is_null() {
        // Match VioGpuDod: the DOD blit path handles the active rotation.
        // SAFETY: dxgkrnl provides a valid in/out struct.
        let caps = unsafe { &mut (*io_p_vidpn_hw_caps).VidPnHWCaps };
        caps.set_DriverRotation(1);
        caps.set_DriverColorConvert(1);
    }
    STATUS_SUCCESS
}

// ── Present ─────────────────────────────────────────────────────────────────

/// `DxgkDdiPresentDisplayOnly` — paint the desktop primary onto the scanout.
/// Blts the system-memory source into the desktop framebuffer (full frame for
/// first light) then pushes it to the host (`TRANSFER_TO_HOST_2D` +
/// `RESOURCE_FLUSH`). Runs at PASSIVE/APC; the source is valid for this call only.
pub unsafe extern "C" fn present_display_only(
    h_adapter: *mut c_void,
    p_present_display_only: *const DXGKARG_PRESENT_DISPLAYONLY,
) -> NTSTATUS {
    if p_present_display_only.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract; read-only.
    let arg = unsafe { &*p_present_display_only };
    let pitch = arg.Pitch;
    let rotate_present = arg.Flags.__bindgen_anon_1.__bindgen_anon_1.Rotate() != 0;
    let present_flags = ((arg.BytesPerPixel as u32) & 0xFF) << 8
        | if pitch < 0 {
            0x2
        } else if pitch == 0 {
            0x1
        } else {
            0
        }
        | ((rotate_present as u32) << 16);
    crate::diag::record(0x0D00_0000 | present_flags);
    crate::diag::record_present(0x0D00_0000 | present_flags); // sticky: PresentDisplayOnly fired
    if arg.BytesPerPixel < 4 || arg.pSource.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(adapter) = (unsafe { adapter(h_adapter) }) else {
        return STATUS_INVALID_PARAMETER;
    };
    if adapter.source_not_visible() {
        return STATUS_SUCCESS;
    }
    if !adapter.framebuffer_active() {
        return STATUS_UNSUCCESSFUL;
    }
    if pitch == 0 {
        return STATUS_SUCCESS;
    }
    let rotation = if rotate_present {
        adapter.rotation()
    } else {
        _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_IDENTITY
    };
    // The committed scanout geometry (CommitVidPn → set_desktop_mode); (0,0) if
    // no mode is committed yet → drop the frame. `desktop_dims` height bounds the
    // source read: CommitVidPn pinned the source mode to this height, so the
    // present source surface is `Pitch * height` bytes — the slice cannot
    // over-read.
    let (_w, h) = adapter.with_virtio(|v| v.desktop_dims()).unwrap_or((0, 0));
    if h == 0 {
        return STATUS_SUCCESS;
    }

    let mut retired: Vec<crate::virtio::gpu::InFlight> = Vec::new();
    // Capture diag_last_cmd/is_wedged in the same lock acquisition so a failed
    // present can be attributed to the exact stalling command at PASSIVE below.
    let r = adapter.with_virtio(|v| {
        let res = unsafe {
            v.present_desktop(
                arg.pSource as *const u8,
                pitch as isize,
                rotation,
                &mut retired,
            )
        };
        (res, v.diag_last_cmd(), v.is_wedged())
    });
    drop(retired);
    match r {
        Ok((Ok(sample), _, _)) => {
            crate::diag::record_surf(sample);
            STATUS_SUCCESS
        }
        Ok((Err(_), last_cmd, wedged)) => {
            // Breadcrumb: phase 0x02 (present_desktop), which command, wedged vs error.
            crate::diag::record_cmd(0x0200_0000 | ((wedged as u32) << 20) | (last_cmd & 0xFFFF));
            STATUS_SUCCESS // soft-fail a present; never bugcheck
        }
        Err(_) => STATUS_SUCCESS, // virtio not bound; soft-fail
    }
}

// ── Pointer (software cursor — accept-and-ignore) ───────────────────────────
// We advertise no HW cursor (QueryAdapterInfo PointerCaps == 0), so dxgkrnl
// composites the cursor into the present source and these are not load-bearing.

/// `DxgkDdiSetPointerPosition`.
pub unsafe extern "C" fn set_pointer_position(
    _h_adapter: *mut c_void,
    _p_set_pointer_position: *const _DXGKARG_SETPOINTERPOSITION,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiSetPointerShape` — accept-and-ignore (we advertise no HW pointer caps, so
/// dxgkrnl composites the cursor in software; NOT_IMPLEMENTED here can read as a
/// failed present-pipeline DDI, so return SUCCESS like SetPointerPosition).
pub unsafe extern "C" fn set_pointer_shape(
    _h_adapter: *mut c_void,
    _p_set_pointer_shape: *const _DXGKARG_SETPOINTERSHAPE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

// ── System display (bugcheck screen) — not supported (virtio is not VGA) ─────

/// `DxgkDdiSystemDisplayEnable`.
pub unsafe extern "C" fn system_display_enable(
    _miniport_device_context: *mut c_void,
    _target_id: u32,
    _flags: *mut _DXGKARG_SYSTEM_DISPLAY_ENABLE_FLAGS,
    _width: *mut u32,
    _height: *mut u32,
    _color_format: *mut D3DDDIFORMAT,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiSystemDisplayWrite` — never called once Enable returns failure.
pub unsafe extern "C" fn system_display_write(
    _miniport_device_context: *mut c_void,
    _source: *mut c_void,
    _source_width: u32,
    _source_height: u32,
    _source_stride: u32,
    _position_x: u32,
    _position_y: u32,
) {
}

// ── Mandatory base + misc DDIs (trivial) ───────────────────────────────────
// dxgkrnl's DOD init path rejects the table (STATUS_FAILED_DRIVER_ENTRY → device
// Code 37) when the base-block DDIs ResetDevice / NotifyAcpiEvent /
// ControlEtwLogging are NULL — even though they do nothing useful here (the
// render-miniport bring-up established this; same gate for a DOD). We register
// the full set as trivial stubs. (Best-guess signatures; the compiler reports
// the exact bindgen typedefs to match.)

/// `DxgkDdiResetDevice` — reset to a known state (e.g. pre-bugcheck). No-op.
pub unsafe extern "C" fn reset_device(_miniport_device_context: *mut c_void) {}

/// `DxgkDdiNotifyAcpiEvent` — we service no ACPI events. Accept-and-ignore: a
/// REGISTERED DDI must not return NOT_IMPLEMENTED (dxgkrnl treats that as a contract
/// violation, which can tear the adapter down post-start / Code 43).
pub unsafe extern "C" fn notify_acpi_event(
    _miniport_device_context: *mut c_void,
    _event_type: DXGK_EVENT_TYPE,
    _event: u32,
    _argument: *mut c_void,
    _acpi_flags: *mut u32,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiControlEtwLogging` — we emit no ETW; no-op.
pub unsafe extern "C" fn control_etw_logging(_enable: BOOLEAN, _flags: u32, _level: u8) {}

/// `DxgkDdiSetPalette` — no palette (32bpp direct). Trivial success.
pub unsafe extern "C" fn set_palette(
    _h_adapter: *mut c_void,
    _p_set_palette: *const DXGKARG_SETPALETTE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiCollectDbgInfo` — nothing to collect.
pub unsafe extern "C" fn collect_dbg_info(
    _h_adapter: *mut c_void,
    _p_collect_dbg_info: *const DXGKARG_COLLECTDBGINFO,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiGetScanLine` — no real scanline counter. Report "in vertical blank" so
/// dxgkrnl's present scheduler treats any time as safe to present. Accept-and-ignore
/// (registered DDI must not return NOT_IMPLEMENTED — see `notify_acpi_event`).
pub unsafe extern "C" fn get_scan_line(
    _h_adapter: *mut c_void,
    p_get_scan_line: *mut DXGKARG_GETSCANLINE,
) -> NTSTATUS {
    if !p_get_scan_line.is_null() {
        // SAFETY: dxgkrnl provides a valid out struct.
        let arg = unsafe { &mut *p_get_scan_line };
        arg.InVerticalBlank = 1;
        arg.ScanLine = 0;
    }
    STATUS_SUCCESS
}

/// `DxgkDdiControlInterrupt` — the transport polls; we service no interrupt classes,
/// but accept the enable/disable request (registered DDI must not return
/// NOT_IMPLEMENTED — see `notify_acpi_event`).
pub unsafe extern "C" fn control_interrupt(
    _h_adapter: *mut c_void,
    _interrupt_type: DXGK_INTERRUPT_TYPE,
    _enable_interrupt: BOOLEAN,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiGetChildContainerId` — no container id; accept-and-ignore (registered DDI
/// must not return NOT_IMPLEMENTED — see `notify_acpi_event`).
pub unsafe extern "C" fn get_child_container_id(
    _miniport_device_context: *mut c_void,
    _child_uid: u32,
    _container_id: *mut DXGK_CHILD_CONTAINER_ID,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiNotifySurpriseRemoval` — accept.
pub unsafe extern "C" fn notify_surprise_removal(
    _miniport_device_context: *mut c_void,
    _removal_type: DXGK_SURPRISE_REMOVAL_TYPE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiEscape` — the venus carrier. STUB (7.2): port today's ioctl.rs body.
pub unsafe extern "C" fn escape(
    _h_adapter: *mut c_void,
    _p_escape: *const DXGKARG_ESCAPE,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// (Re)program the desktop scanout to `width`×`height`. Allocates the persistent
/// contiguous framebuffer at PASSIVE_LEVEL (outside the virtio spinlock), installs
/// it under the lock via [`VirtioGpu::set_desktop_mode`], and drops the old
/// framebuffer (if any) back at PASSIVE. Returns the command result.
pub(crate) fn set_desktop_mode(adapter: &AdapterContext, width: u32, height: u32, force: bool) -> Result<(), crate::error::DriverError> {
    // Idempotent fast-path (only when NOT forced): skip the teardown+recreate churn
    // when the scanout is already programmed to this geometry. StartDevice uses this
    // (force=false). CommitVidPn uses force=TRUE: it MUST re-realize the scanout for
    // the committed VidPN on every commit — KMDOD/VioGpuDod program the scanout on
    // every CommitVidPn (SetSourceModeAndPath / CreateFrameBufferObj), and dxgkrnl
    // validates that "realize" after the first probe present; an idempotent no-op
    // commit makes dxgkrnl conclude the source was never brought online → it
    // StopDevices + Code 43 after one present, with no restart.
    if !force {
        if let Ok((true, (cw, ch))) = adapter.with_virtio(|v| (v.desktop_programmed(), v.desktop_dims())) {
            if cw == width && ch == height {
                return Ok(());
            }
        }
    }
    let bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(BYTES_PER_PIXEL as usize);
    let fb = match crate::virtio::hal::DmaBuffer::new(bytes) {
        Some(fb) => fb,
        None => return Err(crate::error::DriverError::InsufficientResources),
    };
    let mut retired: Vec<crate::virtio::gpu::InFlight> = Vec::new();
    // SAFETY/IRQL: with_virtio runs the closure at DISPATCH; the closure performs
    // no allocation/free (the fb came in pre-allocated, the old fb comes back out
    // to be dropped here at PASSIVE). diag_last_cmd/is_wedged are read inside the
    // same lock acquisition and recorded to the registry below at PASSIVE.
    let (old_fb, result, last_cmd, wedged) = adapter.with_virtio(|v| {
        let (old, result) = v.set_desktop_mode(fb, width, height, &mut retired);
        (old, result, v.diag_last_cmd(), v.is_wedged())
    })?;
    drop(old_fb); // PASSIVE_LEVEL — frees the prior contiguous framebuffer
    drop(retired);
    if result.is_err() {
        // Breadcrumb: phase 0x01 (set_desktop_mode), which command, wedged vs error.
        crate::diag::record_cmd(0x0100_0000 | ((wedged as u32) << 20) | (last_cmd & 0xFFFF));
    }
    result.map_err(|_| crate::error::DriverError::IoError)
}
