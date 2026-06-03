//! Command-submission and TDR DDIs.
// STUB: Phase 3. SubmitCommandVirtual is the hot path — it will package the
// DMA buffer's Venus stream into VIRTIO_GPU_CMD_SUBMIT_3D and kick the control
// virtqueue. See KMD.md Phase 3.

use core::ffi::c_void;

use crate::dxgk::*;

pub unsafe extern "C" fn dxgkddi_submit_command_virtual(
    _h_adapter: *mut c_void,
    _submit_command: *const DXGKARG_SUBMITCOMMANDVIRTUAL,
) -> NTSTATUS {
    // No-op success. The submit DDIs are the one place STATUS_NOT_IMPLEMENTED is
    // unsafe: MSDN says returning any error here bugchecks the OS (0x119) on the
    // first real submission. The Venus submit path lands in Phase 4.
    STATUS_SUCCESS
}

/// `DxgkDdiSubmitCommand` — submit a DMA buffer to the GPU. Critically, this is
/// also how Dxgkrnl queues *paging* buffers (built by DxgkDdiBuildPagingBuffer,
/// with `hDevice == NULL`); since we register paging, this slot must be present.
// STUB: Phase 3/4. Runs at DISPATCH_LEVEL. MUST return STATUS_SUCCESS — an error
// return triggers Bug Check 0x119. The real body kicks the virtio-gpu control
// queue with the buffer's command stream.
pub unsafe extern "C" fn dxgkddi_submit_command(
    _h_adapter: IN_CONST_HANDLE,
    _submit_command: IN_CONST_PDXGKARG_SUBMITCOMMAND,
) -> NTSTATUS {
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_preempt_command(
    _h_adapter: *mut c_void,
    _preempt_command: *const DXGKARG_PREEMPTCOMMAND,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiResetFromTimeout` — TDR recovery (no engines to reset yet).
pub unsafe extern "C" fn dxgkddi_reset_from_timeout(_h_adapter: *mut c_void) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiRestartFromTimeout` — resume after TDR.
pub unsafe extern "C" fn dxgkddi_restart_from_timeout(_h_adapter: *mut c_void) -> NTSTATUS {
    STATUS_SUCCESS
}

// ── Mandatory render-path DDIs (registered so DxgkInitialize accepts the WDDM
//    2.0 render table; real bodies land with the Venus submit path). ──────────

/// `DxgkDdiRender` — record/patch a DMA buffer from a command buffer.
// STUB: Phase 4 (Venus). Will translate the command buffer into a Venus stream.
pub unsafe extern "C" fn dxgkddi_render(
    _h_context: IN_CONST_HANDLE,
    _render: INOUT_PDXGKARG_RENDER,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiRenderKm` — kernel-mode (GDI) render path.
// STUB: Phase 4. Same shape as Render; unused by the Vulkan path.
pub unsafe extern "C" fn dxgkddi_render_km(
    _h_context: IN_CONST_HANDLE,
    _render: INOUT_PDXGKARG_RENDER,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiPatch` — patch allocation references in a DMA buffer.
// STUB: Phase 3. Will resolve allocation handles to GPU VAs in the buffer.
pub unsafe extern "C" fn dxgkddi_patch(
    _h_adapter: IN_CONST_HANDLE,
    _patch: IN_CONST_PDXGKARG_PATCH,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiQueryCurrentFence` — report the last fence the GPU completed.
// STUB: Phase 4. Will read the last completed virtio-gpu fence id. Tied to the
// WDDM 2.0 monitored-fence sync primitive.
pub unsafe extern "C" fn dxgkddi_query_current_fence(
    _h_adapter: IN_CONST_HANDLE,
    _query_current_fence: INOUT_PDXGKARG_QUERYCURRENTFENCE,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiCollectDbgInfo` — dump driver debug state on a TDR/bugcheck.
// STUB: Phase 3. Companion to the TDR DDIs above; nothing to collect yet.
pub unsafe extern "C" fn dxgkddi_collect_dbg_info(
    _h_adapter: IN_CONST_HANDLE,
    _collect_dbg_info: IN_CONST_PDXGKARG_COLLECTDBGINFO,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}
