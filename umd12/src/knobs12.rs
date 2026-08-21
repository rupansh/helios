//! Active D3D12 UMD registry configuration.
//!
//! The reader is shared with the D3D11 UMD, but this table is crate-local so a
//! setting for one driver cannot silently alter the other. Retired delay,
//! legacy-Render submission, and present-identity probes are deliberately absent.

use helios_umd_common::knobs::{BoolKnob, DwordKnob};

/// `pfnCheckFormatSupport` encoding selection. The measured default is the
/// D3D12 DDI encoding; value 1 retains the API-bit passthrough A/B arm.
pub(crate) static UMD12_FORMAT_CAPS: DwordKnob = DwordKnob::new(c"Umd12FormatCaps", 0);

pub(crate) fn umd12_format_caps() -> u32 {
    UMD12_FORMAT_CAPS.get()
}

/// Per-operation trace logging. Absent = off.
pub(crate) static UMD12_TRACE: BoolKnob = BoolKnob::new(c"Umd12Trace", false);

/// D3D12 driver admission switch. Absent = off until separately admitted on a
/// target; source/build retirement does not change that authority boundary.
pub(crate) static UMD_D3D12: BoolKnob = BoolKnob::new(c"UmdD3D12", false);

pub(crate) fn umd12_trace() -> bool {
    UMD12_TRACE.get()
}

pub(crate) fn umd_d3d12() -> bool {
    UMD_D3D12.get()
}

/// Negotiated Core-DDI build. The landed table aliases implement 0116; the
/// closed selection in `adapter12` still rejects unknown builds.
pub(crate) const UMD12_CORE_DDI_DEFAULT: u32 = 116;

pub(crate) static UMD12_CORE_DDI: DwordKnob =
    DwordKnob::new(c"Umd12CoreDdi", UMD12_CORE_DDI_DEFAULT);

pub(crate) fn umd12_core_ddi() -> u32 {
    UMD12_CORE_DDI.get()
}

/// Emit the active, fully resolved configuration once at driver initialization.
pub(crate) fn log_knob_inventory() {
    helios_umd_common::log::log_knob_inventory(&resolved_inventory());
}

/// Active knobs in stable emission order.
pub(crate) fn resolved_inventory() -> [(&'static str, u32); 4] {
    [
        ("Umd12Trace", UMD12_TRACE.get() as u32),
        ("UmdD3D12", UMD_D3D12.get() as u32),
        ("Umd12FormatCaps", UMD12_FORMAT_CAPS.get()),
        ("Umd12CoreDdi", crate::adapter12::selected_core_ddi_build()),
    ]
}
