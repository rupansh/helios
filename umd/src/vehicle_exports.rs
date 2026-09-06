use crate::forward;

/// Dcomp present vehicle (road 4 unit 2), in-process export for the ICD's
/// WSI: hand over the frame to present — venus resid + WS1 #4 fence value +
/// geometry + the creator's exact allocation identity — immediately before
/// calling Present() on the vehicle swapchain ON THE SAME THREAD. The next
/// dxgi_present on this thread consumes the slot (see
/// forward::set_present_source / vehicle_present_prepare).
/// Returns 0 = stored, 1 = stored but overwrote a pending source (counted
/// contract violation), -1 = refused (zero resid/geometry).
#[no_mangle]
pub extern "system" fn helios_umd_set_present_source_v2(
    resid: u32,
    fence_value: u64,
    width: u32,
    height: u32,
    dxgi_format: u32,
    alloc_size: u64,
    memory_type_index: u32,
    semaphore_handle: usize,
) -> i32 {
    forward::set_present_source(
        resid,
        fence_value,
        width,
        height,
        dxgi_format,
        alloc_size,
        memory_type_index,
        semaphore_handle,
    )
}

/// Companion export: bounded wait (µs) until the last vehicle present on
/// THIS thread — the frame copy included — completed on the GPU. The ICD's
/// present worker calls this after Present() returns and only then recycles
/// the frame image, closing the copy-vs-rerender race. Returns 0 =
/// complete, 1 = timeout (counted caller-side), -1 = no vehicle present
/// recorded on this thread.
#[no_mangle]
pub extern "system" fn helios_umd_wait_last_present(timeout_us: u32) -> i32 {
    forward::wait_last_present(timeout_us)
}

/// Fixed-copy completion protocol. The helper device stays retained by WSI
/// throughout the same-thread set/Present/clear/wait sequence. 0 = complete,
/// 1 = pending (safe to retry the SAME captured submission), negative = error.
/// This separate export prevents new ICDs retrying the older ambiguous wait.
#[no_mangle]
pub extern "system" fn helios_umd_wait_present_copy_v2(timeout_us: u32) -> i32 {
    forward::wait_last_present(timeout_us)
}

/// Ends the same-thread borrowed handle scope. See forward::clear_present_source.
#[no_mangle]
pub extern "system" fn helios_umd_clear_present_source_v2() -> i32 {
    forward::clear_present_source()
}
