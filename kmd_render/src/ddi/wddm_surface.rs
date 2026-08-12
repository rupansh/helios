//! The adapter's WDDM surface level, as one value.
//!
//! Dxgkrnl rejects an internally inconsistent version/capability surface during
//! AddAdapter, and the surface is assembled from FIVE coupled sites in two files:
//! `DriverEntry`'s `DRIVER_INITIALIZATION_DATA.Version`, `DRIVERCAPS.WDDMVersion`,
//! the `VirtualAddressingSupported|GpuMmuSupported` `MemoryManagementCaps` bits,
//! `WDDMDEVICECAPS.WDDMVersion`, and `GetNodeMetadata.GpuMmuSupported`. A PARTIAL
//! raise is the one thing that must never happen, and it was previously spelled as
//! two independent `const bool`s giving four states — one of them (`RAISE = false,
//! USE_2_1 = true`) meaningless — with the same nested `if` retyped three times.
//!
//! Here the level is a single value and every site reads it through an exhaustive
//! `const fn`, so a mixed surface is unconstructible and adding or renaming a level
//! fails the build until every accessor has been updated.
//!
//! History of the levels themselves, all of it load-bearing:
//!
//! - **1.3** — the bring-up surface, viogpu3d-style render/display miniport. Kept
//!   as a named level because it is the documented recovery shape, but on Windows
//!   11 24H2 it commits the enumerated monitor as a zero-path/powered-off VidPn,
//!   so it is not deployable with the display half active.
//! - **2.1 + GpuMmu** — the historical production and current recovery level.
//!   It provides the GPU-virtual-addressing / GpuMmu memory model needed by
//!   `D3DDDI_MONITORED_FENCE`, but stays below the MPO3 requirement boundary.
//! - **3.2 + GpuMmu** — the D9 source package. It is selected only with the
//!   complete D2-D5 display table, truthful MPO capability, terminal slot audit,
//!   and native-fence table. Runtime cold-DWM admission remains a separate gate.
//!
//! ⚠ The 2.1 level is why `RAISE_WDDM_3_2_GPUMMU` was a misleading name: with both
//! old bools `true` the adapter advertised `DXGKDDI_WDDMv2_1`, not 3.2. The
//! binding and generated audit now use the exact WDK 28000 table shape.

use crate::dxgk::_DXGK_WDDMVERSION::{DXGKDDI_WDDMv1_3, DXGKDDI_WDDMv2_1, DXGKDDI_WDDMv3_2};
use crate::dxgk::{
    DXGKDDI_INTERFACE_VERSION_WDDM1_3, DXGKDDI_INTERFACE_VERSION_WDDM2_1,
    DXGKDDI_INTERFACE_VERSION_WDDM3_2, DXGK_WDDMVERSION,
};

/// The WDDM level this adapter reports. One value, three coupled facts.
///
/// `dead_code` is allowed deliberately: only one variant is constructed at a
/// time, and that is the point — the unselected levels must stay nameable so
/// changing [`SURFACE`] is a one-line edit that the exhaustive accessors below
/// then force to be complete. Deleting them to silence the warning would put the
/// recovery shape back into prose.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WddmSurface {
    /// Bring-up / recovery: WDDM 1.3, no explicit memory model.
    Wddm1_3,
    /// Historical production / recovery: WDDM 2.1 with the GpuMmu memory model.
    Wddm2_1GpuMmu,
    /// WDDM 3.2 + GpuMmu. Crosses the MPO3 boundary — see the module docs.
    Wddm3_2GpuMmu,
}

/// **The** surface level. Changing this line changes all five sites at once, which
/// is the entire point of the type.
///
/// D9 selects 3.2 only together with the complete D2/D3/D4/D5 table, native
/// fences, terminal slot audit, truthful MPO capability, and the cold-DWM
/// admission harness. This source package is deliberately not a 2.1 fallback;
/// runtime admission remains a separate deployment gate.
pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm3_2GpuMmu;

impl WddmSurface {
    /// `DRIVER_INITIALIZATION_DATA.Version` — the PRIMARY lever. The OS infers the
    /// WDDM level from this plus the advertised caps.
    ///
    /// WDDM 2.0 is deliberately absent: a 2.0 adapter was rejected as too old for
    /// the 24H2 user-mode driver (`STATUS_REVISION_MISMATCH`).
    pub(crate) const fn ddi_interface_version(self) -> u32 {
        match self {
            Self::Wddm1_3 => DXGKDDI_INTERFACE_VERSION_WDDM1_3,
            Self::Wddm2_1GpuMmu => DXGKDDI_INTERFACE_VERSION_WDDM2_1,
            Self::Wddm3_2GpuMmu => DXGKDDI_INTERFACE_VERSION_WDDM3_2,
        }
    }

    /// `DXGK_DRIVERCAPS.WDDMVersion` and `DXGK_WDDMDEVICECAPS.WDDMVersion` — the
    /// same value is required in both, which is why there is one accessor.
    pub(crate) const fn driver_caps_version(self) -> DXGK_WDDMVERSION {
        match self {
            Self::Wddm1_3 => DXGKDDI_WDDMv1_3,
            Self::Wddm2_1GpuMmu => DXGKDDI_WDDMv2_1,
            Self::Wddm3_2GpuMmu => DXGKDDI_WDDMv3_2,
        }
    }

    /// Whether the GpuMmu (GPU virtual addressing) memory model is declared. Gates
    /// both the `VirtualAddressingSupported|GpuMmuSupported` `DXGK_VIDMMCAPS` bits
    /// and `DXGK_NODEMETADATA.GpuMmuSupported`, which must agree.
    ///
    /// RISK, recorded because it cost a bring-up cycle: declaring GPU VA re-engages
    /// the VidMm paging path that historically bugchecked (InitDmaPools Code-43,
    /// `0x10E:0x49`). Dropping to [`WddmSurface::Wddm1_3`] is the revert.
    pub(crate) const fn gpu_mmu(self) -> bool {
        match self {
            Self::Wddm1_3 => false,
            Self::Wddm2_1GpuMmu | Self::Wddm3_2GpuMmu => true,
        }
    }
}
