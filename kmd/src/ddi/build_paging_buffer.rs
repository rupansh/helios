//! `DxgkDdiBuildPagingBuffer` and the GPU-VA root-page-table DDIs.
//!
//! GpuMmu is a *formality* for Helios: we advertise `GpuMmuSupported` (the WDDM
//! 2.0 GPU-VA invariant) so DxgkInitialize accepts the render table, but the GPU
//! address space is owned by the host — virglrenderer replays the Venus stream
//! against its own VkDeviceMemory, so nothing in our path ever dereferences a
//! guest GPU virtual address through a guest-built page table. We therefore build
//! *no* real paging DMA and keep the page tables empty; the DDIs below exist only
//! to satisfy dxgkrnl's post-StartDevice GPU-VA bring-up (clearing Code 43).

use core::ffi::c_void;

use crate::dxgk::*;

/// `DxgkDdiBuildPagingBuffer` — translate a memory-management operation into GPU
/// DMA commands written into `pDmaBuffer`.
///
/// dxgkrnl drives every GPU-VA memory operation through here: UPDATE_PAGE_TABLE,
/// FLUSH_TLB, MAP/UNMAP_APERTURE_SEGMENT, TRANSFER, FILL, etc. Because the host
/// owns the address space (see the module note), each is a no-op for us: we write
/// nothing into the DMA buffer (leaving `pDmaBuffer` unadvanced = zero bytes) and
/// report success. A zero-length buffer then submits as a no-op through
/// `DxgkDdiSubmitCommand`.
///
/// MUST NOT return an error: like the submit DDIs, a non-success return from the
/// paging path bug-checks the OS (0x119) on the first paging operation, which
/// dxgkrnl issues during adapter bring-up — returning anything but SUCCESS here
/// is one of the live triggers for the post-start failure we are clearing.
pub unsafe extern "C" fn dxgkddi_build_paging_buffer(
    _h_adapter: *mut c_void,
    _build_paging_buffer: *mut DXGKARG_BUILDPAGINGBUFFER,
) -> NTSTATUS {
    STATUS_SUCCESS
}

// ── GPU-VA root page-table DDIs. Required by the GPU MMU model we advertise in
//    query_adapter_info (MemoryManagementCaps.GpuMmuSupported); registered so
//    DxgkInitialize accepts the table. Page tables stay empty (host-owned VA). ──

/// `DxgkDdiSetRootPageTable` — point a context at its GPU-VA root page table.
/// No-op: we keep no real page tables (host-owned address space). Returns void.
pub unsafe extern "C" fn dxgkddi_set_root_page_table(
    _h_adapter: IN_CONST_HANDLE,
    _set_root_page_table: IN_CONST_PDXGKARG_SETROOTPAGETABLE,
) {
}

/// `DxgkDdiGetRootPageTableSize` — report the root page-table size in bytes.
///
/// MUST be non-zero. With `query_gpummu_caps` advertising `PageTableLevelCount = 2`,
/// dxgkrnl treats the root table as dynamically resizable and takes its size from
/// this DDI during GPU-VA bring-up; a zero here makes the root table degenerate
/// and fails post-StartDevice (Code 43). One page is sufficient — we never
/// populate the table (BuildPagingBuffer is a no-op), so the value only has to be
/// a plausible page-aligned size dxgkrnl can allocate.
pub unsafe extern "C" fn dxgkddi_get_root_page_table_size(
    _h_adapter: IN_CONST_HANDLE,
    _get_root_page_table_size: INOUT_PDXGKARG_GETROOTPAGETABLESIZE,
) -> SIZE_T {
    // DIAG: confirm dxgkrnl reaches the GPU-VA setup during AddAdapter.
    crate::diag::record(0x0500_0000);
    4096
}
