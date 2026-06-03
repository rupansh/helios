//! `DxgkDdiBuildPagingBuffer` — back allocations with guest memory.
// STUB: Phase 3. Will implement DXGK_BUILDPAGINGBUFFER_OPERATION_TRANSFER
// (map/unmap backing pages into the aperture segment). See KMD.md Phase 3.

use core::ffi::c_void;

use crate::dxgk::*;

pub unsafe extern "C" fn dxgkddi_build_paging_buffer(
    _h_adapter: *mut c_void,
    _build_paging_buffer: *mut DXGKARG_BUILDPAGINGBUFFER,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

// ── GPU-VA root page-table DDIs. Required by the GPU MMU model we advertise in
//    query_adapter_info (MemoryManagementCaps.GpuMmuSupported); registered so
//    DxgkInitialize accepts the table. Real page tables land in Phase 3. ──────

/// `DxgkDdiSetRootPageTable` — point a context at its GPU-VA root page table.
// STUB: Phase 3. Returns void per the DDI contract; no-op until GPU VA is live.
pub unsafe extern "C" fn dxgkddi_set_root_page_table(
    _h_adapter: IN_CONST_HANDLE,
    _set_root_page_table: IN_CONST_PDXGKARG_SETROOTPAGETABLE,
) {
}

/// `DxgkDdiGetRootPageTableSize` — report the root page-table size in bytes.
// STUB: Phase 3. Returns 0 (no GPU-VA address space carved out yet).
pub unsafe extern "C" fn dxgkddi_get_root_page_table_size(
    _h_adapter: IN_CONST_HANDLE,
    _get_root_page_table_size: INOUT_PDXGKARG_GETROOTPAGETABLESIZE,
) -> SIZE_T {
    0
}
