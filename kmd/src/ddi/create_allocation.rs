//! Allocation management DDIs.
// STUB: Phase 3 (WDDM memory management). CreateAllocation will assign
// virtio-gpu resource ids and create blob resources; DestroyAllocation will
// unref them. See KMD.md Phase 3 and TRANSPORT.md §6.

use core::ffi::c_void;

use crate::dxgk::*;

pub unsafe extern "C" fn dxgkddi_create_allocation(
    _h_adapter: *mut c_void,
    _create_allocation: *mut DXGKARG_CREATEALLOCATION,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

pub unsafe extern "C" fn dxgkddi_destroy_allocation(
    _h_adapter: *mut c_void,
    _destroy_allocation: *const DXGKARG_DESTROYALLOCATION,
) -> NTSTATUS {
    // Nothing allocated yet → succeed so teardown never blocks.
    STATUS_SUCCESS
}

// ── Allocation lifetime DDIs (registered so DxgkInitialize accepts the WDDM 2.0
//    render table; bodies land with CreateAllocation in Phase 3). ─────────────

/// `DxgkDdiOpenAllocation` — bind a device to allocations created elsewhere.
// STUB: Phase 3.
pub unsafe extern "C" fn dxgkddi_open_allocation(
    _h_device: IN_CONST_HANDLE,
    _open_allocation: IN_CONST_PDXGKARG_OPENALLOCATION,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiCloseAllocation` — release device-local allocation references.
// STUB: Phase 3. Succeeds so device teardown never blocks (nothing opened yet).
pub unsafe extern "C" fn dxgkddi_close_allocation(
    _h_device: IN_CONST_HANDLE,
    _close_allocation: IN_CONST_PDXGKARG_CLOSEALLOCATION,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiDescribeAllocation` — report an allocation's dimensions/format.
// STUB: Phase 3.
pub unsafe extern "C" fn dxgkddi_describe_allocation(
    _h_adapter: IN_CONST_HANDLE,
    _describe_allocation: INOUT_PDXGKARG_DESCRIBEALLOCATION,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiGetStandardAllocationDriverData` — describe a runtime "standard"
/// allocation (shared primary, staging surface, ...).
// STUB: Phase 3.
pub unsafe extern "C" fn dxgkddi_get_standard_allocation_driver_data(
    _h_adapter: IN_CONST_HANDLE,
    _standard_allocation: INOUT_PDXGKARG_GETSTANDARDALLOCATIONDRIVERDATA,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}
