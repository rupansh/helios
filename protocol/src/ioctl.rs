//! Helios IOCTL channel definitions (ARCH.md §2, §3).
//!
//! In the System-class KMDF model the user-mode Vulkan ICD reaches the KMD by
//! `DeviceIoControl` on a device interface (`GUID_DEVINTERFACE_HELIOS`),
//! discovered with SetupDiGetClassDevs + CreateFile. The IOCTL **control code is
//! the verb** — it replaces the old `HeliosEscapeHeader.cmd_type` dispatch — and
//! WDF validates the in/out buffer lengths the I/O manager reports. The payload
//! structs themselves live in [`crate::escape`] with their wire layout
//! unchanged; only the carrier (IOCTL buffers instead of `D3DKMTEscape`) differs.
//!
//! This module is the single source of truth for the IOCTL codes and the device
//! interface GUID. The KMD builds a `wdk_sys::GUID` from the field constants
//! below; the (C, Mesa-venus) ICD must use the same value — see
//! [`GUID_DEVINTERFACE_HELIOS_STRING`].

/// `CTL_CODE(DeviceType, Function, Method, Access)` per `winioctl.h`:
/// `(DeviceType << 16) | (Access << 14) | (Function << 2) | Method`.
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

// ── CTL_CODE field values ───────────────────────────────────────────────────
/// `FILE_DEVICE_UNKNOWN` — Helios is not a standard device class.
pub const FILE_DEVICE_UNKNOWN: u32 = 0x0000_0022;
/// `FILE_ANY_ACCESS` — no per-IOCTL access requirement.
pub const FILE_ANY_ACCESS: u32 = 0;
/// `FILE_READ_ACCESS` (== FILE_READ_DATA).
pub const FILE_READ_ACCESS: u32 = 0x0001;
/// `FILE_WRITE_ACCESS` (== FILE_WRITE_DATA).
pub const FILE_WRITE_ACCESS: u32 = 0x0002;
/// Helios IOCTL RequiredAccess: every op transfers data both ways and is a
/// privileged GPU operation, so require read+write access (the I/O manager then
/// refuses the IOCTL on a handle lacking it). Per the WDK "Security Issues for
/// I/O Control Codes" guidance, always specify a real RequiredAccess (not
/// FILE_ANY_ACCESS). The ICD opens the device with GENERIC_READ|GENERIC_WRITE.
pub const HELIOS_IOCTL_ACCESS: u32 = FILE_READ_ACCESS | FILE_WRITE_ACCESS;

/// `METHOD_BUFFERED` — I/O manager double-buffers a fixed-size verb.
pub const METHOD_BUFFERED: u32 = 0;
/// `METHOD_IN_DIRECT` — small buffered header + a locked input MDL for bulk data.
pub const METHOD_IN_DIRECT: u32 = 1;
/// `METHOD_OUT_DIRECT` — small buffered input + a locked output MDL.
pub const METHOD_OUT_DIRECT: u32 = 2;
/// `METHOD_NEITHER` — raw user pointers (unused by Helios).
pub const METHOD_NEITHER: u32 = 3;

/// Vendor function-code base (>= 0x800 is the customer-reserved range).
pub const HELIOS_FN_BASE: u32 = 0x900;

// ── IOCTL control codes ─────────────────────────────────────────────────────
// RequiredAccess = read+write (HELIOS_IOCTL_ACCESS), not FILE_ANY_ACCESS — this
// shifts the Access bits (14-15) of every code vs the original ARCH.md §3 table.

/// Create a Venus virtio-gpu context. In/out: [`crate::HeliosEscapeCtxCreate`].
pub const IOCTL_HELIOS_CTX_CREATE: u32 =
    ctl_code(FILE_DEVICE_UNKNOWN, HELIOS_FN_BASE, METHOD_BUFFERED, HELIOS_IOCTL_ACCESS);
/// Destroy a context. In: [`crate::HeliosEscapeCtxDestroy`].
pub const IOCTL_HELIOS_CTX_DESTROY: u32 =
    ctl_code(FILE_DEVICE_UNKNOWN, HELIOS_FN_BASE + 1, METHOD_BUFFERED, HELIOS_IOCTL_ACCESS);
/// Submit an opaque Venus command stream. Buffered header
/// [`crate::HeliosEscapeSubmitVenus`] + Venus blob via the input MDL.
pub const IOCTL_HELIOS_SUBMIT_VENUS: u32 =
    ctl_code(FILE_DEVICE_UNKNOWN, HELIOS_FN_BASE + 2, METHOD_IN_DIRECT, HELIOS_IOCTL_ACCESS);
/// Allocate a virtio-gpu blob resource. In/out: [`crate::HeliosEscapeAllocBlob`].
pub const IOCTL_HELIOS_ALLOC_BLOB: u32 =
    ctl_code(FILE_DEVICE_UNKNOWN, HELIOS_FN_BASE + 3, METHOD_BUFFERED, HELIOS_IOCTL_ACCESS);
/// Map a blob into the calling process; returns a user VA.
/// In/out: [`crate::HeliosEscapeMapBlob`].
///
/// METHOD_BUFFERED (not OUT_DIRECT): the request only carries an 8-byte user VA
/// out + a resource id in — there is no bulk buffer to transfer. The actual page
/// mapping is a side effect (`MmMapLockedPagesSpecifyCache(UserMode)` in the KMD),
/// not data copied into an output buffer, so a locked output MDL would be pure
/// overhead. The small fixed verb fits the double-buffered system buffer.
pub const IOCTL_HELIOS_MAP_BLOB: u32 =
    ctl_code(FILE_DEVICE_UNKNOWN, HELIOS_FN_BASE + 4, METHOD_BUFFERED, HELIOS_IOCTL_ACCESS);
/// Wait on a fence id. In: [`crate::HeliosEscapeWaitFence`].
pub const IOCTL_HELIOS_WAIT_FENCE: u32 =
    ctl_code(FILE_DEVICE_UNKNOWN, HELIOS_FN_BASE + 5, METHOD_BUFFERED, HELIOS_IOCTL_ACCESS);

// Lock the wire values (with Access bits 14-15 = 0b11 = read+write).
const _: () = {
    assert!(IOCTL_HELIOS_CTX_CREATE == 0x0022_E400);
    assert!(IOCTL_HELIOS_CTX_DESTROY == 0x0022_E404);
    assert!(IOCTL_HELIOS_SUBMIT_VENUS == 0x0022_E409);
    assert!(IOCTL_HELIOS_ALLOC_BLOB == 0x0022_E40C);
    assert!(IOCTL_HELIOS_MAP_BLOB == 0x0022_E410);
    assert!(IOCTL_HELIOS_WAIT_FENCE == 0x0022_E414);
};

// ── Device interface GUID ───────────────────────────────────────────────────
//
// Freshly minted v4 GUID, defined once here so the KMD and ICD agree. The KMD
// constructs a `wdk_sys::GUID` from these fields; the C ICD uses the string form.

/// `GUID.Data1` of `GUID_DEVINTERFACE_HELIOS`.
pub const GUID_DEVINTERFACE_HELIOS_DATA1: u32 = 0xC8F8_4237;
/// `GUID.Data2`.
pub const GUID_DEVINTERFACE_HELIOS_DATA2: u16 = 0xCD89;
/// `GUID.Data3`.
pub const GUID_DEVINTERFACE_HELIOS_DATA3: u16 = 0x48F5;
/// `GUID.Data4`.
pub const GUID_DEVINTERFACE_HELIOS_DATA4: [u8; 8] =
    [0xAF, 0xC5, 0x32, 0x94, 0x45, 0x24, 0x62, 0x5C];

/// Canonical string form for the ICD / SetupDi / registry.
pub const GUID_DEVINTERFACE_HELIOS_STRING: &str = "{C8F84237-CD89-48F5-AFC5-32944524625C}";
