//! TEMPORARY registry-breadcrumb tracer for DOD bring-up.
//!
//! No kernel debugger is available on this host, so each instrumented DDI writes
//! a step code to `HKLM\SYSTEM\CurrentControlSet\Services\helios_kmd\HeliosStep`
//! (REG_DWORD). Read it after install to see the furthest DDI reached / the
//! failing step — dxgkrnl stops calling DDIs after one returns an error, so the
//! last code written is the suspect. The high byte identifies the DDI; the low
//! bits carry a sub-step or argument. Remove once the DOD loads cleanly (Code 0).

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use wdk_sys::ntddk::RtlWriteRegistryValue;

/// Rolling log of the last 8 DDI high-nibbles (each `record` shifts one in).
/// Teardown DDIs use [`record_step_only`] which does NOT update this, so it
/// preserves the exact DDI sequence dxgkrnl drove right BEFORE it tore the device
/// down (e.g. ...present(D) → commit(9)? → IsSupportedVidPn(7) → stop). Read as
/// `HeliosSeq` (oldest nibble = most-significant).
static SEQ: AtomicU32 = AtomicU32::new(0);

// `RelativeTo` = RTL_REGISTRY_SERVICES → path relative to ...\CurrentControlSet\
// Services. REG_DWORD = 4. (Stable Windows values; declared locally to avoid a
// dependency on whether wdk-sys exports them.)
const RTL_REGISTRY_SERVICES: u32 = 1;
const REG_DWORD_TYPE: u32 = 4;

// L"helios_kmd\0" and L"HeliosStep\0" as NUL-terminated UTF-16.
static SVC_PATH: [u16; 11] = [
    b'h' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'_' as u16,
    b'k' as u16, b'm' as u16, b'd' as u16, 0,
];
static VAL_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'S' as u16,
    b't' as u16, b'e' as u16, b'p' as u16, 0,
];
// "HeliosEnum\0" — durable EnumVidPnCofuncModality detail (paths + per-step flags).
static ENUM_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'E' as u16,
    b'n' as u16, b'u' as u16, b'm' as u16, 0,
];
// "HeliosCommit\0" — sticky: set when CommitVidPn fires (with committed dims).
static COMMIT_NAME: [u16; 13] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'C' as u16,
    b'o' as u16, b'm' as u16, b'm' as u16, b'i' as u16, b't' as u16, 0,
];
// "HeliosPresent\0" — sticky: set when PresentDisplayOnly fires.
static PRESENT_NAME: [u16; 14] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'P' as u16,
    b'r' as u16, b'e' as u16, b's' as u16, b'e' as u16, b'n' as u16, b't' as u16, 0,
];
// "HeliosEnumP\0" — last PIVOT (source/target) enum call result (HeliosEnum keeps
// the no-pivot call so both survive).
static ENUMP_NAME: [u16; 12] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'E' as u16,
    b'n' as u16, b'u' as u16, b'm' as u16, b'P' as u16, 0,
];
// "HeliosErr\0" — last failing NTSTATUS from a source/target create/add/assign.
static ERR_NAME: [u16; 10] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'E' as u16,
    b'r' as u16, b'r' as u16, 0,
];
// "HeliosAErr\0" — last pfnAssignTargetModeSet NTSTATUS (the root, pre-cascade).
static AERR_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'A' as u16,
    b'E' as u16, b'r' as u16, b'r' as u16, 0,
];
// "HeliosSrcRes\0" — pinned source-mode resolution (w<<16 | h), 0 if none.
static SRES_NAME: [u16; 13] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'S' as u16,
    b'r' as u16, b'c' as u16, b'R' as u16, b'e' as u16, b's' as u16, 0,
];
// "HeliosTAdd\0" — last pfnAddMode(target) NTSTATUS (0=success → modeset non-empty).
static TADD_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'T' as u16,
    b'A' as u16, b'd' as u16, b'd' as u16, 0,
];
// "HeliosCmd\0" — the virtio-gpu command that stalled/failed a present, recorded at
// PASSIVE after the spinlock drops: (phase<<24) | (wedged<<20) | (cmd & 0xFFFF).
// phase: 0x01 set_desktop_mode (commit), 0x02 present_desktop. cmd is the virtio
// hdr type_ (0x0101 CREATE_2D, 0x0102 UNREF, 0x0103 SET_SCANOUT, 0x0104 FLUSH,
// 0x0105 TRANSFER_TO_HOST_2D, 0x0106 ATTACH_BACKING). wedged=1 → the host stopped
// completing the control queue (timeout); wedged=0 → the host returned an error.
static CMD_NAME: [u16; 10] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'C' as u16,
    b'm' as u16, b'd' as u16, 0,
];
// "HeliosVis\0" — sticky SetVidPnSourceVisibility marker: 0x0C00_0000 | Visible.
// Visible=1 (0x0C000001) = healthy bring-up; Visible=0 (0x0C000000) = dxgkrnl
// blanking/teardown (the post-commit reject happened upstream).
static VIS_NAME: [u16; 10] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'V' as u16,
    b'i' as u16, b's' as u16, 0,
];
// "HeliosQai\0" — sticky: set (0x0300_0010) when dxgkrnl queries
// DXGKQAITYPE_DISPLAY_DRIVERCAPS_EXTENSION (type 16) — confirms the OS asks for it
// and that we now answer (vs the old NOT_SUPPORTED). Fires at start time (before
// Commit/Present), so its ordering also proves it is/isn't the post-present trigger.
static QAI_NAME: [u16; 10] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'Q' as u16,
    b'a' as u16, b'i' as u16, 0,
];
// "HeliosTAnp\0" — last pfnAddMode(target) NTSTATUS during a NO-PIVOT enum.
static TANP_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'T' as u16,
    b'A' as u16, b'n' as u16, b'p' as u16, 0,
];
// "HeliosPost\0" — every instrumented DDI EXCEPT the teardown DDIs (StopDevice /
// StopDeviceAndReleasePostDisplayOwnership / RemoveDevice) writes its step code here.
// So HeliosPost = the LAST DDI dxgkrnl called *before* it decided to stop the device.
// On the post-present Code-43 teardown, this names what dxgkrnl invoked right before
// the StopDevice (e.g. 0x0D present = nothing ran after present; some other code =
// that DDI is the post-present trigger).
static POST_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'P' as u16,
    b'o' as u16, b's' as u16, b't' as u16, 0,
];
// "HeliosSeq\0" — rolling last-8 DDI high-nibbles (see SEQ).
static SEQ_NAME: [u16; 10] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'S' as u16,
    b'e' as u16, b'q' as u16, 0,
];
// "HeliosPiv\0" — rolling last-8 EnumCofuncModality pivot-nibbles (see PIV).
static PIV_NAME: [u16; 10] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'P' as u16,
    b'i' as u16, b'v' as u16, 0,
];
// "HeliosSupp\0" — IsSupportedVidPn decision detail.
static SUPP_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'S' as u16,
    b'u' as u16, b'p' as u16, b'p' as u16, 0,
];
// "HeliosSurf\0" — sampled source/fb bytes from PresentDisplayOnly.
static SURF_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'S' as u16,
    b'u' as u16, b'r' as u16, b'f' as u16, 0,
];

/// Write `value` to REG_DWORD `name` under the service key. Best-effort.
/// PASSIVE_LEVEL only (RtlWriteRegistryValue is pageable). Single registry write,
/// no atomics/globals — the load-safe pattern (avoids whatever broke 5ec3ef6's
/// `SEEN` atomic / second write).
fn write_dword(name: &[u16], value: u32) {
    let mut v = value;
    // SAFETY: SVC_PATH/`name` are NUL-terminated UTF-16 valid for the program's
    // lifetime; `v` is a 4-byte DWORD. RtlWriteRegistryValue copies its inputs.
    unsafe {
        let _ = RtlWriteRegistryValue(
            RTL_REGISTRY_SERVICES,
            SVC_PATH.as_ptr() as *mut u16,
            name.as_ptr() as *mut u16,
            REG_DWORD_TYPE,
            &mut v as *mut u32 as *mut c_void,
            4,
        );
    }
}

/// Write `code` to the `HeliosStep` breadcrumb (last-DDI tracer) AND to `HeliosPost`
/// (last NON-teardown DDI — teardown DDIs use [`record_step_only`]).
pub fn record(code: u32) {
    write_dword(&VAL_NAME, code);
    write_dword(&POST_NAME, code);
    // Shift this DDI's high-nibble into the rolling sequence log.
    let seq = (SEQ.load(Ordering::Relaxed) << 4) | ((code >> 24) & 0xF);
    SEQ.store(seq, Ordering::Relaxed);
    write_dword(&SEQ_NAME, seq);
}

/// Like [`record`] but writes ONLY `HeliosStep`, not `HeliosPost`. Used by the
/// teardown DDIs (StopDevice etc.) so `HeliosPost` preserves the last DDI dxgkrnl
/// called before it tore the device down.
pub fn record_step_only(code: u32) {
    write_dword(&VAL_NAME, code);
}

/// EnumVidPnCofuncModality detail → `HeliosEnum` (survives later DDIs).
pub fn record_enum(code: u32) {
    write_dword(&ENUM_NAME, code);
}

/// Sticky CommitVidPn marker → `HeliosCommit`.
pub fn record_commit(code: u32) {
    write_dword(&COMMIT_NAME, code);
}

/// Sticky PresentDisplayOnly marker → `HeliosPresent`.
pub fn record_present(code: u32) {
    write_dword(&PRESENT_NAME, code);
}

/// Last PIVOT enum call result → `HeliosEnumP`.
pub fn record_enum_pivot(code: u32) {
    write_dword(&ENUMP_NAME, code);
}

/// Last failing source/target create/add/assign NTSTATUS → `HeliosErr`.
pub fn record_err(status: i32) {
    write_dword(&ERR_NAME, status as u32);
}

/// Last target pfnAssignTargetModeSet NTSTATUS → `HeliosAErr`.
pub fn record_aerr(status: i32) {
    write_dword(&AERR_NAME, status as u32);
}

/// Pinned source-mode resolution seen during enum (w<<16 | h) → `HeliosSrcRes`.
pub fn record_sres(code: u32) {
    write_dword(&SRES_NAME, code);
}

/// Last pfnAddMode(target) NTSTATUS → `HeliosTAdd`.
pub fn record_tadd(status: i32) {
    write_dword(&TADD_NAME, status as u32);
}

/// The virtio-gpu command that stalled/failed a present → `HeliosCmd` (see the
/// `CMD_NAME` note for the layout). PASSIVE_LEVEL only.
pub fn record_cmd(code: u32) {
    write_dword(&CMD_NAME, code);
}

/// Sticky SetVidPnSourceVisibility marker (with the Visible flag) → `HeliosVis`.
pub fn record_vis(code: u32) {
    write_dword(&VIS_NAME, code);
}

/// Sticky DXGKQAITYPE_DISPLAY_DRIVERCAPS_EXTENSION query marker → `HeliosQai`.
pub fn record_qai(code: u32) {
    write_dword(&QAI_NAME, code);
}

/// IsSupportedVidPn detail → `HeliosSupp`.
pub fn record_supp(code: u32) {
    write_dword(&SUPP_NAME, code);
}

/// Present source/framebuffer sample → `HeliosSurf`.
pub fn record_surf(code: u32) {
    write_dword(&SURF_NAME, code);
}

/// Rolling log of per-EnumCofuncModality `nibble`s → `HeliosPiv`. `nibble` packs the
/// EnumPivotType (low 3 bits: 0=uninit 1=src 2=tgt 3=scaling 4=rot 5=nopivot) and
/// bit3 = "did this enum modify any path (called UpdatePathSupportInfo)". Reveals
/// what dxgkrnl cycles through in the post-present EnumCofuncModality↔IsSupportedVidPn
/// loop (HeliosSeq=0x8787...) and whether we keep dirtying the VidPN every round.
static PIV: AtomicU32 = AtomicU32::new(0);
pub fn record_piv(nibble: u32) {
    let p = (PIV.load(Ordering::Relaxed) << 4) | (nibble & 0xF);
    PIV.store(p, Ordering::Relaxed);
    write_dword(&PIV_NAME, p);
}

/// Last pfnAddMode(target) NTSTATUS seen during a NO-PIVOT (or uninitialized) enum
/// → `HeliosTAnp`. (HeliosTAdd keeps the last value across ALL enum types; this one
/// isolates the no-pivot enum so we can tell whether the initial cofunctional
/// enumeration's target add succeeds independently of the source-pivot enum.)
pub fn record_tadd_np(status: i32) {
    write_dword(&TANP_NAME, status as u32);
}
