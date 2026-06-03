//! `DxgkDdiQueryAdapterInfo` — report adapter capabilities.
//!
//! Render-only WDDM 2.0 adapter: GPU virtual addressing, one CPU-visible
//! aperture segment (backed later by the virtio-gpu hostmem blob), no scanout.
//! Render-only-ness is conveyed by reporting zero video present sources in
//! StartDevice; this WDK's DXGK_PRESENTATIONCAPS has no `NoScanout` bit.
//!
//! Reference: https://learn.microsoft.com/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_queryadapterinfo

use core::ffi::c_void;
use core::mem::size_of;

use crate::dxgk::_DXGK_QUERYADAPTERINFOTYPE::{
    DXGKQAITYPE_DRIVERCAPS, DXGKQAITYPE_GPUMMUCAPS, DXGKQAITYPE_QUERYSEGMENT4,
};
use crate::dxgk::*;

pub unsafe extern "C" fn dxgkddi_query_adapter_info(
    miniport_device_context: *mut c_void,
    query_adapter_info: *const DXGKARG_QUERYADAPTERINFO,
) -> NTSTATUS {
    if miniport_device_context.is_null() || query_adapter_info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract; we only read the args struct.
    let args = unsafe { &*query_adapter_info };

    match args.Type {
        DXGKQAITYPE_DRIVERCAPS => unsafe { query_driver_caps(args) },
        DXGKQAITYPE_QUERYSEGMENT4 => unsafe { query_segments(args) },
        DXGKQAITYPE_GPUMMUCAPS => unsafe { query_gpummu_caps(args) },
        // Everything else: let Dxgkrnl apply its defaults.
        _ => STATUS_NOT_SUPPORTED,
    }
}

unsafe fn query_driver_caps(args: &DXGKARG_QUERYADAPTERINFO) -> NTSTATUS {
    if (args.OutputDataSize as usize) < size_of::<DXGK_DRIVERCAPS>() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: pOutputData points to a DXGK_DRIVERCAPS of sufficient size.
    let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_DRIVERCAPS) };
    unsafe { core::ptr::write_bytes(caps as *mut _ as *mut u8, 0, size_of::<DXGK_DRIVERCAPS>()) };

    // 64-bit addressable.
    caps.HighestAcceptableAddress.QuadPart = -1;
    caps.MaxAllocationListSlotId = 0xFFFF;
    // Cap on bytes committed in the aperture segment (512 MB initially).
    caps.ApertureSegmentCommitLimit = 512 * 1024 * 1024;
    // Not a legacy VGA device.
    caps.SupportNonVGA = 1;

    // Advertise the WDDM 2.0 GPU-VA model. Without VirtualAddressingSupported +
    // GpuMmuSupported the runtime never enables GPU virtual addressing, and
    // DxgkInitialize rejects the GPU-VA DDI table (one half of the Code-37 fix;
    // the matching SetRootPageTable/GetRootPageTableSize/Render DDIs are
    // registered in lib.rs). MemoryManagementCaps is a union of a bitfield over
    // `Value`; the flags live two anon levels down (same shape as the segment
    // descriptor flags in query_segments).
    // SAFETY: caps (incl. MemoryManagementCaps) was zeroed above; we write the
    // bitfield via its generated setters.
    unsafe {
        let vidmm = &mut caps.MemoryManagementCaps.__bindgen_anon_1.__bindgen_anon_1;
        vidmm.set_VirtualAddressingSupported(1);
        vidmm.set_GpuMmuSupported(1);
    }

    // WDDMVersion stays 0 (zeroed above). The WDDM level is conveyed by
    // DRIVER_INITIALIZATION_DATA.Version (= DXGKDDI_INTERFACE_VERSION_WDDM2_0);
    // a non-zero DXGK_WDDMVERSION here is wrong for a 2.0 driver and was part of
    // the Code-37 rejection.

    STATUS_SUCCESS
}

unsafe fn query_gpummu_caps(args: &DXGKARG_QUERYADAPTERINFO) -> NTSTATUS {
    if (args.OutputDataSize as usize) < size_of::<DXGK_GPUMMUCAPS>() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: pOutputData points to a DXGK_GPUMMUCAPS of sufficient size.
    let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_GPUMMUCAPS) };
    unsafe { core::ptr::write_bytes(caps as *mut _ as *mut u8, 0, size_of::<DXGK_GPUMMUCAPS>()) };

    // 48-bit GPU VA. Use a 2-level table: with PageTableLevelCount == 2 the root
    // page table is dynamically resizable and its size comes from
    // DxgkDdiGetRootPageTableSize. A level count > 2 would instead obligate us to
    // answer DXGKQAITYPE_PAGETABLELEVELDESC with a per-level DXGK_PAGE_TABLE_LEVEL_DESC
    // (which we don't yet populate), so 2 is the consistent choice for bring-up.
    caps.VirtualAddressBitCount = 48;
    caps.PageTableLevelCount = 2;
    // STUB (Phase 3): PageTableUpdateMode stays 0 (CPU_VIRTUAL) and the page-table
    // machinery (DxgkDdiGetRootPageTableSize real size + BuildPagingBuffer
    // UpdatePageTable/FlushTlb) is not yet meaningful — fine until a GPU-VA
    // context is actually created.

    STATUS_SUCCESS
}

unsafe fn query_segments(args: &DXGKARG_QUERYADAPTERINFO) -> NTSTATUS {
    if (args.OutputDataSize as usize) < size_of::<DXGK_QUERYSEGMENTOUT4>() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: pOutputData points to a DXGK_QUERYSEGMENTOUT4.
    let out = unsafe { &mut *(args.pOutputData as *mut DXGK_QUERYSEGMENTOUT4) };

    // Report one CPU-visible aperture segment (the virtio-gpu hostmem blob).
    out.NbSegment = 1;
    out.SegmentDescriptorStride = size_of::<DXGK_SEGMENTDESCRIPTOR4>() as u64;
    out.PagingBufferSegmentId = 1;
    out.PagingBufferSize = 64 * 1024;
    out.PagingBufferPrivateDataSize = 0;

    // Second call: Dxgkrnl provides the (byte-addressed) descriptor array.
    if !out.pSegmentDescriptor.is_null() {
        // SAFETY: pSegmentDescriptor points to >= NbSegment descriptors.
        let seg = unsafe { &mut *(out.pSegmentDescriptor as *mut DXGK_SEGMENTDESCRIPTOR4) };
        unsafe {
            core::ptr::write_bytes(
                seg as *mut _ as *mut u8,
                0,
                size_of::<DXGK_SEGMENTDESCRIPTOR4>(),
            );
            // CPU-visible aperture flags live in a nested bindgen union/bitfield.
            seg.Flags.__bindgen_anon_1.__bindgen_anon_1.set_CpuVisible(1);
            seg.Flags.__bindgen_anon_1.__bindgen_anon_1.set_Aperture(1);
        }
        // BaseAddress filled from the virtio-gpu BAR once PCI init lands (Phase 2/3).
        seg.BaseAddress.QuadPart = 0;
        seg.Size = 512 * 1024 * 1024;
        seg.CommitLimit = 512 * 1024 * 1024;
    }

    STATUS_SUCCESS
}

/// `DxgkDdiGetNodeMetadata` — describe GPU engine node `node_ordinal`.
///
/// Unlike the other Phase-1.5 stubs this has a real body: Dxgkrnl enumerates
/// engine nodes during adapter bring-up starting at ordinal 0, and MSDN requires
/// every call for a *valid* ordinal to succeed — `STATUS_NOT_IMPLEMENTED` is not
/// an allowed return and would leave the device in an error state. We expose a
/// single symmetric 3D engine node (ordinal 0); the node count is implicit (1).
pub unsafe extern "C" fn dxgkddi_get_node_metadata(
    _h_adapter: IN_CONST_HANDLE,
    node_ordinal: UINT,
    get_node_metadata: OUT_PDXGKARG_GETNODEMETADATA,
) -> NTSTATUS {
    // Only node 0 exists; any other ordinal is out of range.
    if get_node_metadata.is_null() || node_ordinal != 0 {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above; Dxgkrnl provides a writable
    // DXGK_NODEMETADATA (DXGKARG_GETNODEMETADATA is an alias for it).
    let node = unsafe { &mut *get_node_metadata };
    unsafe {
        core::ptr::write_bytes(node as *mut _ as *mut u8, 0, size_of::<DXGK_NODEMETADATA>());
    }
    node.EngineType = DXGK_ENGINE_TYPE::DXGK_ENGINE_TYPE_3D;
    // Mirror the adapter-level GpuMmu opt-in (DRIVERCAPS.MemoryManagementCaps).
    node.GpuMmuSupported = 1;
    // FriendlyName, Flags, IoMmuSupported stay zeroed.
    STATUS_SUCCESS
}
