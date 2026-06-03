//! `DxgkDdiEscape` — out-of-band ICD → KMD channel.
// STUB: Phase 4. Will validate the HeliosEscapeHeader (helios_protocol::escape)
// and dispatch SUBMIT_VENUS / CTX_CREATE / ALLOC_BLOB / MAP_BLOB / WAIT_FENCE.
// See TRANSPORT.md §3.

use core::ffi::c_void;

use crate::dxgk::*;

pub unsafe extern "C" fn dxgkddi_escape(
    _h_adapter: *mut c_void,
    _escape: *const DXGKARG_ESCAPE,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}
