//! GENERATED — do not edit by hand.
//!
//! The WDDM 3.2 `DRIVER_INITIALIZATION_DATA` slot-and-cap audit required by
//! `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` section 17.6:4460-4462 and graded by
//! section 18.1:4653-4656.
//!
//! Regenerate with `python3 kmd_render/tools/gen_wddm32_slot_audit.py`; the
//! classification lives in `kmd_render/tools/wddm32_slot_classes.tsv` and the
//! human-readable rendering in `docs/retirement/d9-wddm32-slot-audit.md`.
//!
//! # What this file proves, and how
//!
//! 1. **Layout.** One `const _: () = assert!(offset_of!(..) == ..)` per slot,
//!    plus one on `size_of::<DRIVER_INITIALIZATION_DATA>()`. These are evaluated
//!    against the WDK the build actually bindgens, so a WDK whose table differs
//!    from the audited one is a build failure rather than a short struct handed
//!    to a longer-expecting dxgkrnl (`STATUS_REVISION_MISMATCH`).
//! 2. **Classification.** [`SLOTS`] carries every slot through WDDM 3.2 as
//!    terminal `Implemented` or `Disabled`. The generator retains the two
//!    pre-D9 transition classes, but the D9 activation gate requires zero rows
//!    of either class.
//! 3. **Agreement.** [`verify`] walks the table `build_ddi_table()` actually
//!    produced and checks each slot's pointer word against its class. A
//!    disagreement fails `DriverEntry` — a registered slot that the audit calls
//!    unreachable, or a NULL slot the audit calls implemented, is a driver bug
//!    and must not load.
//!
//! `kmd_render` cannot host a libtest harness (`panic = "abort"` `no_std`
//! cdylib) and `kmd_logic` has no `wdk-sys` edge, so 1-3 are the only three
//! mechanisms available; see the generator's module docs.

use core::mem::{offset_of, size_of};

use crate::dxgk::DRIVER_INITIALIZATION_DATA;

/// How a slot is expected to appear in the built table.
#[allow(
    dead_code,
    reason = "generator supports pre-D9 transition tables; the D9 gate requires zero rows"
)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotClass {
    /// Registered and backed by a real implementation. Must be non-NULL.
    Implemented,
    /// Deliberately unregistered, and unreachable because the capability that
    /// would reach it is reported as zero/absent. Must be NULL.
    Disabled,
    /// Pre-D9 transition: unregistered and verified like [`Self::Disabled`].
    Pending,
    /// Pre-D9 transition: registered and verified like [`Self::Implemented`].
    Retiring,
}

/// One audited slot of `DRIVER_INITIALIZATION_DATA`.
pub(crate) struct SlotAudit {
    /// The WDK field name.
    pub(crate) name: &'static str,
    /// Byte offset inside `DRIVER_INITIALIZATION_DATA`, asserted below.
    pub(crate) offset: usize,
    /// The `DXGKDDI_INTERFACE_VERSION_*` guard the WDK declares it under.
    pub(crate) min_version: &'static str,
    /// Expected presence in the built table.
    pub(crate) class: SlotClass,
    /// Why. For `Disabled`, the truthful zero capability; for `Pending` and
    /// `Retiring`, the owning lane and the normative line.
    pub(crate) reason: &'static str,
}

/// Why [`verify`] refused the built table.
pub(crate) struct SlotAuditFailure {
    /// Index into [`SLOTS`].
    pub(crate) index: usize,
    /// The offending slot.
    pub(crate) name: &'static str,
    /// `true` when the slot was registered but classified unreachable;
    /// `false` when it was classified implemented but is NULL.
    pub(crate) registered: bool,
}

/// Number of audited slots (every member except `Version`).
pub(crate) const SLOT_COUNT: usize = 192;

/// `Version` (ULONG + 4 bytes of padding) plus one pointer per slot.
const EXPECTED_STRUCT_SIZE: usize = 8 + SLOT_COUNT * 8;

/// The audited header: `kmd_render/tools/wdk-28000/km/dispmprt.h`.
#[allow(dead_code, reason = "provenance for the audit, read by humans")]
pub(crate) const AUDITED_HEADER: &str = "kmd_render/tools/wdk-28000/km/dispmprt.h";

/// The classification, in WDK declaration order.
pub(crate) const SLOTS: [SlotAudit; SLOT_COUNT] = [
    SlotAudit {
        name: "DxgkDdiAddDevice",
        offset: 8,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "PnP: adapter object creation.",
    },
    SlotAudit {
        name: "DxgkDdiStartDevice",
        offset: 16,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "PnP: transport bring-up, segment/placement init.",
    },
    SlotAudit {
        name: "DxgkDdiStopDevice",
        offset: 24,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "PnP: drain and tear down the transport.",
    },
    SlotAudit {
        name: "DxgkDdiRemoveDevice",
        offset: 32,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "PnP: free the adapter object.",
    },
    SlotAudit {
        name: "DxgkDdiDispatchIoRequest",
        offset: 40,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Legacy video-port IRP path; refuses every code and counts through StVrp.",
    },
    SlotAudit {
        name: "DxgkDdiInterruptRoutine",
        offset: 48,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "INTx ISR: read-to-clear virtio ISR status, queue the DPC.",
    },
    SlotAudit {
        name: "DxgkDdiDpcRoutine",
        offset: 56,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Drain the used ring and complete WDDM submissions.",
    },
    SlotAudit {
        name: "DxgkDdiQueryChildRelations",
        offset: 64,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "One video-output child when the display half is active.",
    },
    SlotAudit {
        name: "DxgkDdiQueryChildStatus",
        offset: 72,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Child connection status for that one target.",
    },
    SlotAudit {
        name: "DxgkDdiQueryDeviceDescriptor",
        offset: 80,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "EDID for the one monitor target.",
    },
    SlotAudit {
        name: "DxgkDdiSetPowerState",
        offset: 88,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Power transitions for the adapter and its one child.",
    },
    SlotAudit {
        name: "DxgkDdiNotifyAcpiEvent",
        offset: 96,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Platform ACPI events; services none and reports zero flags.",
    },
    SlotAudit {
        name: "DxgkDdiResetDevice",
        offset: 104,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Crash-dump quiesce; deliberately programs no hardware.",
    },
    SlotAudit {
        name: "DxgkDdiUnload",
        offset: 112,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Driver-wide unload; releases the cached BAR MMIO mappings.",
    },
    SlotAudit {
        name: "DxgkDdiQueryInterface",
        offset: 120,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Exports no driver interface; logs the requested GUID and refuses.",
    },
    SlotAudit {
        name: "DxgkDdiControlEtwLogging",
        offset: 128,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "C42 provider enable bit and level (section 12.3).",
    },
    SlotAudit {
        name: "DxgkDdiQueryAdapterInfo",
        offset: 136,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "The whole capability surface, including NATIVE_FENCE_CAPS.",
    },
    SlotAudit {
        name: "DxgkDdiCreateDevice",
        offset: 144,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Per-D3D-device state, exact hKmdProcess.",
    },
    SlotAudit {
        name: "DxgkDdiCreateAllocation",
        offset: 152,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "HWA2 create-time descriptor, HVM1 roles, HOC1 admission.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyAllocation",
        offset: 160,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Allocation teardown with generation-safe host unref.",
    },
    SlotAudit {
        name: "DxgkDdiDescribeAllocation",
        offset: 168,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Primary geometry for the display path.",
    },
    SlotAudit {
        name: "DxgkDdiGetStandardAllocationDriverData",
        offset: 176,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Standard (shared primary / cross-adapter) allocation shapes.",
    },
    SlotAudit {
        name: "DxgkDdiAcquireSwizzlingRange",
        offset: 184,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::NumberOfSwizzlingRanges=0; the swizzling-range family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiReleaseSwizzlingRange",
        offset: 192,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::NumberOfSwizzlingRanges=0; the swizzling-range family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiPatch",
        offset: 200,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Infallible idempotent physical-capability snapshotter (section 10.7:1856).",
    },
    SlotAudit {
        name: "DxgkDdiSubmitCommand",
        offset: 208,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Physical submit: epoch validation plus one fenced SUBMIT_3D.",
    },
    SlotAudit {
        name: "DxgkDdiPreemptCommand",
        offset: 216,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Preemption request; reports DMA_PREEMPTED.",
    },
    SlotAudit {
        name: "DxgkDdiBuildPagingBuffer",
        offset: 224,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "HPM1 paging DMA encoding (section 17.6:4421).",
    },
    SlotAudit {
        name: "DxgkDdiSetPalette",
        offset: 232,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "No palettized mode is enumerated; every VidPn source mode is 32bpp RGB.",
    },
    SlotAudit {
        name: "DxgkDdiSetPointerPosition",
        offset: 240,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Hardware pointer position.",
    },
    SlotAudit {
        name: "DxgkDdiSetPointerShape",
        offset: 248,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Hardware pointer shape.",
    },
    SlotAudit {
        name: "DxgkDdiResetFromTimeout",
        offset: 256,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "TDR reset.",
    },
    SlotAudit {
        name: "DxgkDdiRestartFromTimeout",
        offset: 264,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "TDR restart.",
    },
    SlotAudit {
        name: "DxgkDdiEscape",
        offset: 272,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "D9 leaves the WDDM 3.2 initialization slot NULL. No replacement Escape, IOCTL, registry, mapped-page, ticket, name, or discovery channel is advertised; wider K1 source demolition remains a later all-or-nothing retirement gate.",
    },
    SlotAudit {
        name: "DxgkDdiCollectDbgInfo",
        offset: 280,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "OS-requested bounded debug snapshot.",
    },
    SlotAudit {
        name: "DxgkDdiQueryCurrentFence",
        offset: 288,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Last completed submission fence for a node.",
    },
    SlotAudit {
        name: "DxgkDdiIsSupportedVidPn",
        offset: 296,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "VidPn validation.",
    },
    SlotAudit {
        name: "DxgkDdiRecommendFunctionalVidPn",
        offset: 304,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "VidPn recommendation.",
    },
    SlotAudit {
        name: "DxgkDdiEnumVidPnCofuncModality",
        offset: 312,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Cofunctional modality enumeration.",
    },
    SlotAudit {
        name: "DxgkDdiSetVidPnSourceAddress",
        offset: 320,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Classic scanout binding through validate_direct_scanout_binding.",
    },
    SlotAudit {
        name: "DxgkDdiSetVidPnSourceVisibility",
        offset: 328,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Source visibility.",
    },
    SlotAudit {
        name: "DxgkDdiCommitVidPn",
        offset: 336,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "VidPn commit.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateActiveVidPnPresentPath",
        offset: 344,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Active present-path update.",
    },
    SlotAudit {
        name: "DxgkDdiRecommendMonitorModes",
        offset: 352,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Monitor mode recommendation.",
    },
    SlotAudit {
        name: "DxgkDdiRecommendVidPnTopology",
        offset: 360,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "Optional. One source and one target, so the OS-built topology is accepted unchanged.",
    },
    SlotAudit {
        name: "DxgkDdiGetScanLine",
        offset: 368,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Scan-line query.",
    },
    SlotAudit {
        name: "DxgkDdiStopCapture",
        offset: 376,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "No capture/VPE surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiControlInterrupt",
        offset: 384,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "CRTC_VSYNC enable/disable; every other class is refused.",
    },
    SlotAudit {
        name: "DxgkDdiCreateOverlay",
        offset: 392,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyDevice",
        offset: 400,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Per-device teardown.",
    },
    SlotAudit {
        name: "DxgkDdiOpenAllocation",
        offset: 408,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Ordinary open; reads private data only, never restamps it.",
    },
    SlotAudit {
        name: "DxgkDdiCloseAllocation",
        offset: 416,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Per-device allocation close.",
    },
    SlotAudit {
        name: "DxgkDdiRender",
        offset: 424,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "HVC1/HNR2 fragment assembly, validation and output patch generation.",
    },
    SlotAudit {
        name: "DxgkDdiPresent",
        offset: 432,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Present DMA packet construction.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateOverlay",
        offset: 440,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiFlipOverlay",
        offset: 448,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyOverlay",
        offset: 456,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiCreateContext",
        offset: 464,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "HVC1 / HQA1 context admission (section 10.7, 10.4).",
    },
    SlotAudit {
        name: "DxgkDdiDestroyContext",
        offset: 472,
        min_version: "BASE",
        class: SlotClass::Implemented,
        reason: "Context teardown.",
    },
    SlotAudit {
        name: "DxgkDdiLinkDevice",
        offset: 480,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "Section 10.2 topology row: one physical node, LDA is not advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetDisplayPrivateDriverFormat",
        offset: 488,
        min_version: "BASE",
        class: SlotClass::Disabled,
        reason: "No private display format is negotiated; the host owns the scanout format.",
    },
    SlotAudit {
        name: "DxgkDdiDescribePageTable",
        offset: 496,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it.",
    },
    SlotAudit {
        name: "DxgkDdiUpdatePageTable",
        offset: 504,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it.",
    },
    SlotAudit {
        name: "DxgkDdiUpdatePageDirectory",
        offset: 512,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it.",
    },
    SlotAudit {
        name: "DxgkDdiMovePageDirectory",
        offset: 520,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it.",
    },
    SlotAudit {
        name: "DxgkDdiSubmitRender",
        offset: 528,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it.",
    },
    SlotAudit {
        name: "DxgkDdiCreateAllocation2",
        offset: 536,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it.",
    },
    SlotAudit {
        name: "DxgkDdiRenderKm",
        offset: 544,
        min_version: "WIN7",
        class: SlotClass::Implemented,
        reason: "Kernel-mode command buffer arm.",
    },
    SlotAudit {
        name: "Reserved",
        offset: 552,
        min_version: "WIN7",
        class: SlotClass::Disabled,
        reason: "Reserved by the WDK; must stay NULL.",
    },
    SlotAudit {
        name: "DxgkDdiQueryVidPnHWCapability",
        offset: 560,
        min_version: "WIN7",
        class: SlotClass::Implemented,
        reason: "Per-path hardware capability.",
    },
    SlotAudit {
        name: "DxgkDdiSetPowerComponentFState",
        offset: 568,
        min_version: "WIN8",
        class: SlotClass::Disabled,
        reason: "No power components are declared and SupportRuntimePowerManagement is 0, so no F-state transition is requested.",
    },
    SlotAudit {
        name: "DxgkDdiQueryDependentEngineGroup",
        offset: 576,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "One node, one engine.",
    },
    SlotAudit {
        name: "DxgkDdiQueryEngineStatus",
        offset: 584,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Per-engine progress for the TDR path.",
    },
    SlotAudit {
        name: "DxgkDdiResetEngine",
        offset: 592,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Per-engine reset (SupportPerEngineTDR=1).",
    },
    SlotAudit {
        name: "DxgkDdiStopDeviceAndReleasePostDisplayOwnership",
        offset: 600,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "PnP stop with post-display ownership release.",
    },
    SlotAudit {
        name: "DxgkDdiSystemDisplayEnable",
        offset: 608,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Bugcheck/system-display path.",
    },
    SlotAudit {
        name: "DxgkDdiSystemDisplayWrite",
        offset: 616,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Bugcheck/system-display path.",
    },
    SlotAudit {
        name: "DxgkDdiCancelCommand",
        offset: 624,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Cancel a queued packet.",
    },
    SlotAudit {
        name: "DxgkDdiGetChildContainerId",
        offset: 632,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Mandatory once a monitor target is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiPowerRuntimeControlRequest",
        offset: 640,
        min_version: "WIN8",
        class: SlotClass::Implemented,
        reason: "Runtime power control requests.",
    },
    SlotAudit {
        name: "DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay",
        offset: 648,
        min_version: "WIN8",
        class: SlotClass::Disabled,
        reason: "Superseded revision. Section 17.6:4465 selects only the MPO3 revision.",
    },
    SlotAudit {
        name: "DxgkDdiNotifySurpriseRemoval",
        offset: 656,
        min_version: "WIN8",
        class: SlotClass::Disabled,
        reason: "DXGK_DRIVERCAPS::SupportSurpriseRemoval=0; the OS never notifies surprise removal.",
    },
    SlotAudit {
        name: "DxgkDdiGetNodeMetadata",
        offset: 664,
        min_version: "WDDM1_3",
        class: SlotClass::Implemented,
        reason: "One node, GpuMmuSupported consistent with the reported surface.",
    },
    SlotAudit {
        name: "DxgkDdiSetPowerPState",
        offset: 672,
        min_version: "WDDM1_3",
        class: SlotClass::Disabled,
        reason: "No GPU P-states are declared; the device has no programmable clock domain.",
    },
    SlotAudit {
        name: "DxgkDdiControlInterrupt2",
        offset: 680,
        min_version: "WDDM1_3",
        class: SlotClass::Disabled,
        reason: "The driver services exactly one interrupt class (CRTC_VSYNC) and registers ControlInterrupt; revisions 2 and 3 only add classes it never raises.",
    },
    SlotAudit {
        name: "DxgkDdiCheckMultiPlaneOverlaySupport",
        offset: 688,
        min_version: "WDDM1_3",
        class: SlotClass::Disabled,
        reason: "Superseded revision. Section 17.6:4465 selects only the MPO3 revision; MaxPlanes=1 is advertised there.",
    },
    SlotAudit {
        name: "DxgkDdiCalibrateGpuClock",
        offset: 696,
        min_version: "WDDM1_3",
        class: SlotClass::Implemented,
        reason: "GPU clock calibration for the scheduler.",
    },
    SlotAudit {
        name: "DxgkDdiFormatHistoryBuffer",
        offset: 704,
        min_version: "WDDM1_3",
        class: SlotClass::Implemented,
        reason: "History buffer formatting.",
    },
    SlotAudit {
        name: "DxgkDdiRenderGdi",
        offset: 712,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "PENDING DELETE with the retired GDI path (K6; A.4 row 5770). Registered until the display lane confirms nothing lands here.",
    },
    SlotAudit {
        name: "DxgkDdiSubmitCommandVirtual",
        offset: 720,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "D3D12 virtual submit: HOS1 validation plus nonblocking GPUVA enqueue.",
    },
    SlotAudit {
        name: "DxgkDdiSetRootPageTable",
        offset: 728,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "C64 authoritative per-process root.",
    },
    SlotAudit {
        name: "DxgkDdiGetRootPageTableSize",
        offset: 736,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "GpuMmu root table sizing.",
    },
    SlotAudit {
        name: "DxgkDdiMapCpuHostAperture",
        offset: 744,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "COUPLED DELETE: goes with the HLM1 two-segment table (K1/K2); the current segment table still advertises SupportsCpuHostAperture, so removing the slot alone would break a live cap.",
    },
    SlotAudit {
        name: "DxgkDdiUnmapCpuHostAperture",
        offset: 752,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "COUPLED DELETE: pairs with MapCpuHostAperture; see that row.",
    },
    SlotAudit {
        name: "DxgkDdiCheckMultiPlaneOverlaySupport2",
        offset: 760,
        min_version: "WDDM2_0",
        class: SlotClass::Disabled,
        reason: "Superseded revision. Section 17.6:4465 selects only the MPO3 revision.",
    },
    SlotAudit {
        name: "DxgkDdiCreateProcess",
        offset: 768,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "One bounded ProcessContext HTS1 session list (section 17.6:4336).",
    },
    SlotAudit {
        name: "DxgkDdiDestroyProcess",
        offset: 776,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "Process teardown cancels all sessions.",
    },
    SlotAudit {
        name: "DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay2",
        offset: 784,
        min_version: "WDDM2_0",
        class: SlotClass::Disabled,
        reason: "Superseded revision. Section 17.6:4465 selects only the MPO3 revision.",
    },
    SlotAudit {
        name: "Reserved1",
        offset: 792,
        min_version: "WDDM2_0",
        class: SlotClass::Disabled,
        reason: "Reserved by the WDK; must stay NULL.",
    },
    SlotAudit {
        name: "Reserved2",
        offset: 800,
        min_version: "WDDM2_0",
        class: SlotClass::Disabled,
        reason: "Reserved by the WDK; must stay NULL.",
    },
    SlotAudit {
        name: "DxgkDdiPowerRuntimeSetDeviceHandle",
        offset: 808,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "Runtime power device handle.",
    },
    SlotAudit {
        name: "DxgkDdiSetStablePowerState",
        offset: 816,
        min_version: "WDDM2_0",
        class: SlotClass::Implemented,
        reason: "Stable power state for profiling tools.",
    },
    SlotAudit {
        name: "DxgkDdiSetVideoProtectedRegion",
        offset: 824,
        min_version: "WDDM2_0",
        class: SlotClass::Disabled,
        reason: "No protected-content surface is advertised (no OPM/PVP capability).",
    },
    SlotAudit {
        name: "DxgkDdiCheckMultiPlaneOverlaySupport3",
        offset: 832,
        min_version: "WDDM2_1",
        class: SlotClass::Implemented,
        reason: "Active D3 exact-allocation one-primary validator; unsupported shapes return a reserved-clean false result.",
    },
    SlotAudit {
        name: "DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3",
        offset: 840,
        min_version: "WDDM2_1",
        class: SlotClass::Implemented,
        reason: "Active D3 PASSIVE-retry binder over the D2 candidate and parking state; the D2 predicate derives solely from SURFACE.",
    },
    SlotAudit {
        name: "DxgkDdiPostMultiPlaneOverlayPresent",
        offset: 848,
        min_version: "WDDM2_1",
        class: SlotClass::Implemented,
        reason: "Bounded always-success D3 diagnostic callback; no output path requests PostPresentNeeded.",
    },
    SlotAudit {
        name: "DxgkDdiValidateUpdateAllocationProperty",
        offset: 856,
        min_version: "WDDM2_1",
        class: SlotClass::Implemented,
        reason: "Active D3 exact-allocation validation; all property mutations are rejected.",
    },
    SlotAudit {
        name: "DxgkDdiControlModeBehavior",
        offset: 864,
        min_version: "WDDM2_1",
        class: SlotClass::Implemented,
        reason: "D3 reports every requested unsupported mode behavior in NotSatisfied.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateMonitorLinkInfo",
        offset: 872,
        min_version: "WDDM2_1",
        class: SlotClass::Implemented,
        reason: "Mandatory once a monitor target is advertised; revalidated under the 3.2 table.",
    },
    SlotAudit {
        name: "DxgkDdiCreateHwContext",
        offset: 880,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyHwContext",
        offset: 888,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero.",
    },
    SlotAudit {
        name: "DxgkDdiCreateHwQueue",
        offset: 896,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyHwQueue",
        offset: 904,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero.",
    },
    SlotAudit {
        name: "DxgkDdiSubmitCommandToHwQueue",
        offset: 912,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero.",
    },
    SlotAudit {
        name: "DxgkDdiSwitchToHwContextList",
        offset: 920,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero.",
    },
    SlotAudit {
        name: "DxgkDdiResetHwEngine",
        offset: 928,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "HWS-only engine reset; per-engine TDR uses DxgkDdiResetEngine instead.",
    },
    SlotAudit {
        name: "DxgkDdiCreatePeriodicFrameNotification",
        offset: 936,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No periodic frame notification is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyPeriodicFrameNotification",
        offset: 944,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No periodic frame notification is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetTimingsFromVidPn",
        offset: 952,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "Display-Core timing programming is not advertised; the host owns modeset and the guest programs no CRTC.",
    },
    SlotAudit {
        name: "DxgkDdiSetTargetGamma",
        offset: 960,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No per-target gamma/LUT hardware is exposed.",
    },
    SlotAudit {
        name: "DxgkDdiSetTargetContentType",
        offset: 968,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No per-target content-type signalling is exposed.",
    },
    SlotAudit {
        name: "DxgkDdiSetTargetAnalogCopyProtection",
        offset: 976,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No analog output and no copy-protection hardware.",
    },
    SlotAudit {
        name: "DxgkDdiSetTargetAdjustedColorimetry",
        offset: 984,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No per-target colorimetry adjustment hardware; only SDR RGB is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiDisplayDetectControl",
        offset: 992,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "The single child is indicated statically from the virtio-gpu config-change event; there is no detection engine to gate.",
    },
    SlotAudit {
        name: "DxgkDdiQueryConnectionChange",
        offset: 1000,
        min_version: "WDDM2_2",
        class: SlotClass::Disabled,
        reason: "No DisplayPort topology/connection-change surface is exposed.",
    },
    SlotAudit {
        name: "DxgkDdiExchangePreStartInfo",
        offset: 1008,
        min_version: "WDDM2_2",
        class: SlotClass::Implemented,
        reason: "Pre-start info exchange.",
    },
    SlotAudit {
        name: "DxgkDdiGetMultiPlaneOverlayCaps",
        offset: 1016,
        min_version: "WDDM2_2",
        class: SlotClass::Implemented,
        reason: "Active D3 exact one-RGB-plane caps; all YUV, transform, scaling, and additional-plane authority remains zero.",
    },
    SlotAudit {
        name: "DxgkDdiGetPostCompositionCaps",
        offset: 1024,
        min_version: "WDDM2_2",
        class: SlotClass::Implemented,
        reason: "Active D3 unity-only post-composition caps; no post-composition transform or scaling authority is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateHwContextState",
        offset: 1032,
        min_version: "WDDM2_3",
        class: SlotClass::Disabled,
        reason: "HWS-only; the hardware-scheduling family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiCreateProtectedSession",
        offset: 1040,
        min_version: "WDDM2_3",
        class: SlotClass::Disabled,
        reason: "No protected-content session surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyProtectedSession",
        offset: 1048,
        min_version: "WDDM2_3",
        class: SlotClass::Disabled,
        reason: "No protected-content session surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetSchedulingLogBuffer",
        offset: 1056,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "Scheduling log buffers are HWS-scoped; HWS is not advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetupPriorityBands",
        offset: 1064,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "No priority bands are advertised.",
    },
    SlotAudit {
        name: "DxgkDdiNotifyFocusPresent",
        offset: 1072,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "Focus-present notification is not requested by this driver.",
    },
    SlotAudit {
        name: "DxgkDdiSetContextSchedulingProperties",
        offset: 1080,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "Context scheduling properties are not advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSuspendContext",
        offset: 1088,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "Context suspend/resume is HWS-scoped; HWS is not advertised.",
    },
    SlotAudit {
        name: "DxgkDdiResumeContext",
        offset: 1096,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "Context suspend/resume is HWS-scoped; HWS is not advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetVirtualMachineData",
        offset: 1104,
        min_version: "WDDM2_4",
        class: SlotClass::Implemented,
        reason: "VM data hand-off.",
    },
    SlotAudit {
        name: "DxgkDdiBeginExclusiveAccess",
        offset: 1112,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "No GPU-P exclusive-access surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiEndExclusiveAccess",
        offset: 1120,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "No GPU-P exclusive-access surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiQueryDiagnosticTypesSupport",
        offset: 1128,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "The WDK 28000 WDDM 2.4 contract covers only PSR notification and SyncLock progression types. Helios supports neither category and leaves this slot NULL; black-screen collection is the independent CollectDiagnosticInfo contract.",
    },
    SlotAudit {
        name: "DxgkDdiControlDiagnosticReporting",
        offset: 1136,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "The WDK 28000 WDDM 2.4 contract controls only the PSR and SyncLock diagnostic types that Helios truthfully reports unsupported, so this slot remains NULL.",
    },
    SlotAudit {
        name: "DxgkDdiResumeHwEngine",
        offset: 1144,
        min_version: "WDDM2_4",
        class: SlotClass::Disabled,
        reason: "HWS-only engine resume; the hardware-scheduling family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiSignalMonitoredFence",
        offset: 1152,
        min_version: "WDDM2_5",
        class: SlotClass::Disabled,
        reason: "HWQueue-scoped monitored-fence signalling; HWS/HWQueue is not advertised, and native fences use the Core-0116 slots instead.",
    },
    SlotAudit {
        name: "DxgkDdiPresentToHwQueue",
        offset: 1160,
        min_version: "WDDM2_5",
        class: SlotClass::Disabled,
        reason: "D9 unregisters the complete hardware-context and hardware-queue family; hardware flip queues and every HwQueuePacketCap bit remain zero.",
    },
    SlotAudit {
        name: "DxgkDdiValidateSubmitCommand",
        offset: 1168,
        min_version: "WDDM2_5",
        class: SlotClass::Disabled,
        reason: "Not advertised; SubmitCommand validates its own packet inline.",
    },
    SlotAudit {
        name: "DxgkDdiSetTargetAdjustedColorimetry2",
        offset: 1176,
        min_version: "WDDM2_5",
        class: SlotClass::Disabled,
        reason: "No per-target colorimetry adjustment hardware; only SDR RGB is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetTrackedWorkloadPowerLevel",
        offset: 1184,
        min_version: "WDDM2_5",
        class: SlotClass::Disabled,
        reason: "No tracked workloads are advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSaveMemoryForHotUpdate",
        offset: 1192,
        min_version: "WDDM2_6",
        class: SlotClass::Disabled,
        reason: "Driver hot-update is not supported by this package.",
    },
    SlotAudit {
        name: "DxgkDdiRestoreMemoryForHotUpdate",
        offset: 1200,
        min_version: "WDDM2_6",
        class: SlotClass::Disabled,
        reason: "Driver hot-update is not supported by this package.",
    },
    SlotAudit {
        name: "DxgkDdiCollectDiagnosticInfo",
        offset: 1208,
        min_version: "WDDM2_6",
        class: SlotClass::Implemented,
        reason: "C42 bounded PASSIVE snapshot callback with optional AddDevice adapter context and required WDDM 2.7 black-screen support; validates the WDK 28000 input before publishing strings, size, or bytes.",
    },
    SlotAudit {
        name: "Reserved3",
        offset: 1216,
        min_version: "WDDM2_6",
        class: SlotClass::Disabled,
        reason: "Reserved by the WDK; must stay NULL.",
    },
    SlotAudit {
        name: "DxgkDdiControlInterrupt3",
        offset: 1224,
        min_version: "WDDM2_7",
        class: SlotClass::Disabled,
        reason: "The driver services exactly one interrupt class (CRTC_VSYNC) and registers ControlInterrupt; revisions 2 and 3 only add classes it never raises.",
    },
    SlotAudit {
        name: "DxgkDdiSetFlipQueueLogBuffer",
        offset: 1232,
        min_version: "WDDM2_9",
        class: SlotClass::Disabled,
        reason: "Section 17.6:4497 forbids the hardware-flip-queue flags; no HW flip queue is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateFlipQueueLog",
        offset: 1240,
        min_version: "WDDM2_9",
        class: SlotClass::Disabled,
        reason: "Section 17.6:4497 forbids the hardware-flip-queue flags; no HW flip queue is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiCancelQueuedFlips",
        offset: 1248,
        min_version: "WDDM2_9",
        class: SlotClass::Disabled,
        reason: "Section 17.6:4497 forbids the hardware-flip-queue flags; no HW flip queue is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetInterruptTargetPresentId",
        offset: 1256,
        min_version: "WDDM2_9",
        class: SlotClass::Disabled,
        reason: "Requires the HW flip queue / target PresentId interrupt, neither of which is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetAllocationBackingStore",
        offset: 1264,
        min_version: "WDDM3_0",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_SHARE_BACKING_STORE_WITH_KMD is not enabled (section 9 rejects it as a Venus transport).",
    },
    SlotAudit {
        name: "DxgkDdiCreateCpuEvent",
        offset: 1272,
        min_version: "WDDM3_0",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_KMD_SIGNAL_CPU_EVENT is not enabled.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyCpuEvent",
        offset: 1280,
        min_version: "WDDM3_0",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_KMD_SIGNAL_CPU_EVENT is not enabled.",
    },
    SlotAudit {
        name: "DxgkDdiCancelFlips",
        offset: 1288,
        min_version: "WDDM3_0",
        class: SlotClass::Disabled,
        reason: "No HW flip queue is advertised, so there is no queued flip to cancel.",
    },
    SlotAudit {
        name: "DxgkDdiCreateNativeFence",
        offset: 1296,
        min_version: "WDDM3_1",
        class: SlotClass::Implemented,
        reason: "Core-0116 native fence create (section 12.1); returns the driver global handle and validated HNF1 PDD.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyNativeFence",
        offset: 1304,
        min_version: "WDDM3_1",
        class: SlotClass::Implemented,
        reason: "Native fence destroy after the final global reference.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateMonitoredValues",
        offset: 1312,
        min_version: "WDDM3_1",
        class: SlotClass::Implemented,
        reason: "OS-driven monitored-value publication into the OS-owned storage.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateCurrentValuesFromCpu",
        offset: 1320,
        min_version: "WDDM3_1",
        class: SlotClass::Implemented,
        reason: "OS-driven CPU-side current-value publication.",
    },
    SlotAudit {
        name: "DxgkDdiCreateDoorbell",
        offset: 1328,
        min_version: "WDDM3_1",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface.",
    },
    SlotAudit {
        name: "DxgkDdiConnectDoorbell",
        offset: 1336,
        min_version: "WDDM3_1",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface.",
    },
    SlotAudit {
        name: "DxgkDdiDisconnectDoorbell",
        offset: 1344,
        min_version: "WDDM3_1",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyDoorbell",
        offset: 1352,
        min_version: "WDDM3_1",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface.",
    },
    SlotAudit {
        name: "DxgkDdiNotifyWorkSubmission",
        offset: 1360,
        min_version: "WDDM3_1",
        class: SlotClass::Disabled,
        reason: "DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface.",
    },
    SlotAudit {
        name: "Reserved4",
        offset: 1368,
        min_version: "WDDM3_1",
        class: SlotClass::Disabled,
        reason: "Reserved by the WDK; must stay NULL.",
    },
    SlotAudit {
        name: "DxgkDdiCreateMemoryBasis",
        offset: 1376,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the memory-basis/dirty-tracking family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiDestroyMemoryBasis",
        offset: 1384,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the memory-basis/dirty-tracking family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiStartDirtyTracking",
        offset: 1392,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the dirty-tracking family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiStopDirtyTracking",
        offset: 1400,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the dirty-tracking family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiQueryDirtyBitData",
        offset: 1408,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the dirty-tracking family is unreachable.",
    },
    SlotAudit {
        name: "DxgkDdiPrepareLiveMigration",
        offset: 1416,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU-P live migration is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSaveImmutableMigrationData",
        offset: 1424,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU-P live migration is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSaveMutableMigrationData",
        offset: 1432,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU-P live migration is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiEndLiveMigration",
        offset: 1440,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU-P live migration is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiRestoreImmutableMigrationData",
        offset: 1448,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU-P live migration is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiRestoreMutableMigrationData",
        offset: 1456,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU-P live migration is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiWriteVirtualizedInterrupt",
        offset: 1464,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU partitioning (GPU-P/SR-IOV) surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetVirtualGpuResources2",
        offset: 1472,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU partitioning (GPU-P/SR-IOV) surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiSetVirtualFunctionPauseState",
        offset: 1480,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "No GPU partitioning (GPU-P/SR-IOV) surface is advertised.",
    },
    SlotAudit {
        name: "DxgkDdiOpenNativeFence",
        offset: 1488,
        min_version: "WDDM3_2",
        class: SlotClass::Implemented,
        reason: "Cross-process open; returns the driver local handle and validated HNF1 PDD.",
    },
    SlotAudit {
        name: "DxgkDdiCloseNativeFence",
        offset: 1496,
        min_version: "WDDM3_2",
        class: SlotClass::Implemented,
        reason: "Per-process local close.",
    },
    SlotAudit {
        name: "DxgkDdiSetNativeFenceLogBuffer",
        offset: 1504,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "Native-fence log buffers are HWQueue-scoped (DXGKARG_SETNATIVEFENCELOGBUFFER::hHwQueue) and DXGK_VIDSCHCAPS::OptimizedNativeFenceSignaledInterrupt=0, so dxgkrnl rescans waiters instead of reading a log.",
    },
    SlotAudit {
        name: "DxgkDdiUpdateNativeFenceLogs",
        offset: 1512,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "Native-fence log buffers are HWQueue-scoped and OptimizedNativeFenceSignaledInterrupt=0; see SetNativeFenceLogBuffer.",
    },
    SlotAudit {
        name: "DxgkDdiCollectDbgInfo2",
        offset: 1520,
        min_version: "WDDM3_2",
        class: SlotClass::Implemented,
        reason: "C42 WDDM 3.2 TDR callback validates reason, TDR enum, optional versioned payload, pointer-size coherence, alignment, and IRQL before publishing a bounded snapshot or extension output.",
    },
    SlotAudit {
        name: "DxgkDdiNotifyContextPriorityChange",
        offset: 1528,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "Context priority-change notification is not requested by this driver.",
    },
    SlotAudit {
        name: "DxgkDdiResetDisplayEngine",
        offset: 1536,
        min_version: "WDDM3_2",
        class: SlotClass::Disabled,
        reason: "There is no guest-side display engine to reset; scanout is a host SET_SCANOUT_BLOB. Display lane to revisit if the cold-DWM admission gate demands it.",
    },
];

// One offset assertion per slot. Written out rather than looped so the
// failing line names the slot that moved.
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiAddDevice) == 8);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiStartDevice) == 16);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiStopDevice) == 24);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRemoveDevice) == 32);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDispatchIoRequest) == 40);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiInterruptRoutine) == 48);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDpcRoutine) == 56);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryChildRelations) == 64);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryChildStatus) == 72);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryDeviceDescriptor) == 80);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetPowerState) == 88);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiNotifyAcpiEvent) == 96);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResetDevice) == 104);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUnload) == 112);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryInterface) == 120);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiControlEtwLogging) == 128);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryAdapterInfo) == 136);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateDevice) == 144);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateAllocation) == 152);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyAllocation) == 160);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDescribeAllocation) == 168);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetStandardAllocationDriverData) == 176);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiAcquireSwizzlingRange) == 184);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiReleaseSwizzlingRange) == 192);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPatch) == 200);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSubmitCommand) == 208);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPreemptCommand) == 216);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiBuildPagingBuffer) == 224);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetPalette) == 232);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetPointerPosition) == 240);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetPointerShape) == 248);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResetFromTimeout) == 256);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRestartFromTimeout) == 264);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiEscape) == 272);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCollectDbgInfo) == 280);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryCurrentFence) == 288);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiIsSupportedVidPn) == 296);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRecommendFunctionalVidPn) == 304);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiEnumVidPnCofuncModality) == 312);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVidPnSourceAddress) == 320);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVidPnSourceVisibility) == 328);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCommitVidPn) == 336);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateActiveVidPnPresentPath) == 344);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRecommendMonitorModes) == 352);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRecommendVidPnTopology) == 360);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetScanLine) == 368);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiStopCapture) == 376);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiControlInterrupt) == 384);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateOverlay) == 392);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyDevice) == 400);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiOpenAllocation) == 408);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCloseAllocation) == 416);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRender) == 424);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPresent) == 432);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateOverlay) == 440);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiFlipOverlay) == 448);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyOverlay) == 456);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateContext) == 464);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyContext) == 472);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiLinkDevice) == 480);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetDisplayPrivateDriverFormat) == 488);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDescribePageTable) == 496);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdatePageTable) == 504);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdatePageDirectory) == 512);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiMovePageDirectory) == 520);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSubmitRender) == 528);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateAllocation2) == 536);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRenderKm) == 544);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, Reserved) == 552);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryVidPnHWCapability) == 560);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetPowerComponentFState) == 568);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryDependentEngineGroup) == 576);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryEngineStatus) == 584);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResetEngine) == 592);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiStopDeviceAndReleasePostDisplayOwnership) == 600);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSystemDisplayEnable) == 608);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSystemDisplayWrite) == 616);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCancelCommand) == 624);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetChildContainerId) == 632);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPowerRuntimeControlRequest) == 640);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay) == 648);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiNotifySurpriseRemoval) == 656);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetNodeMetadata) == 664);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetPowerPState) == 672);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiControlInterrupt2) == 680);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCheckMultiPlaneOverlaySupport) == 688);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCalibrateGpuClock) == 696);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiFormatHistoryBuffer) == 704);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRenderGdi) == 712);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSubmitCommandVirtual) == 720);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetRootPageTable) == 728);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetRootPageTableSize) == 736);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiMapCpuHostAperture) == 744);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUnmapCpuHostAperture) == 752);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCheckMultiPlaneOverlaySupport2) == 760);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateProcess) == 768);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyProcess) == 776);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay2) == 784);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, Reserved1) == 792);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, Reserved2) == 800);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPowerRuntimeSetDeviceHandle) == 808);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetStablePowerState) == 816);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVideoProtectedRegion) == 824);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCheckMultiPlaneOverlaySupport3) == 832);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3) == 840);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPostMultiPlaneOverlayPresent) == 848);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiValidateUpdateAllocationProperty) == 856);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiControlModeBehavior) == 864);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateMonitorLinkInfo) == 872);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateHwContext) == 880);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyHwContext) == 888);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateHwQueue) == 896);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyHwQueue) == 904);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSubmitCommandToHwQueue) == 912);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSwitchToHwContextList) == 920);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResetHwEngine) == 928);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreatePeriodicFrameNotification) == 936);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyPeriodicFrameNotification) == 944);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTimingsFromVidPn) == 952);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTargetGamma) == 960);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTargetContentType) == 968);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTargetAnalogCopyProtection) == 976);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTargetAdjustedColorimetry) == 984);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDisplayDetectControl) == 992);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryConnectionChange) == 1000);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiExchangePreStartInfo) == 1008);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetMultiPlaneOverlayCaps) == 1016);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiGetPostCompositionCaps) == 1024);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateHwContextState) == 1032);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateProtectedSession) == 1040);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyProtectedSession) == 1048);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetSchedulingLogBuffer) == 1056);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetupPriorityBands) == 1064);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiNotifyFocusPresent) == 1072);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetContextSchedulingProperties) == 1080);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSuspendContext) == 1088);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResumeContext) == 1096);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVirtualMachineData) == 1104);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiBeginExclusiveAccess) == 1112);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiEndExclusiveAccess) == 1120);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryDiagnosticTypesSupport) == 1128);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiControlDiagnosticReporting) == 1136);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResumeHwEngine) == 1144);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSignalMonitoredFence) == 1152);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPresentToHwQueue) == 1160);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiValidateSubmitCommand) == 1168);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTargetAdjustedColorimetry2) == 1176);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetTrackedWorkloadPowerLevel) == 1184);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSaveMemoryForHotUpdate) == 1192);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRestoreMemoryForHotUpdate) == 1200);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCollectDiagnosticInfo) == 1208);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, Reserved3) == 1216);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiControlInterrupt3) == 1224);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetFlipQueueLogBuffer) == 1232);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateFlipQueueLog) == 1240);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCancelQueuedFlips) == 1248);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetInterruptTargetPresentId) == 1256);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetAllocationBackingStore) == 1264);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateCpuEvent) == 1272);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyCpuEvent) == 1280);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCancelFlips) == 1288);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateNativeFence) == 1296);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyNativeFence) == 1304);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateMonitoredValues) == 1312);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateCurrentValuesFromCpu) == 1320);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateDoorbell) == 1328);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiConnectDoorbell) == 1336);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDisconnectDoorbell) == 1344);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyDoorbell) == 1352);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiNotifyWorkSubmission) == 1360);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, Reserved4) == 1368);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCreateMemoryBasis) == 1376);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiDestroyMemoryBasis) == 1384);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiStartDirtyTracking) == 1392);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiStopDirtyTracking) == 1400);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiQueryDirtyBitData) == 1408);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiPrepareLiveMigration) == 1416);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSaveImmutableMigrationData) == 1424);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSaveMutableMigrationData) == 1432);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiEndLiveMigration) == 1440);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRestoreImmutableMigrationData) == 1448);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiRestoreMutableMigrationData) == 1456);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiWriteVirtualizedInterrupt) == 1464);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVirtualGpuResources2) == 1472);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetVirtualFunctionPauseState) == 1480);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiOpenNativeFence) == 1488);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCloseNativeFence) == 1496);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiSetNativeFenceLogBuffer) == 1504);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiUpdateNativeFenceLogs) == 1512);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiCollectDbgInfo2) == 1520);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiNotifyContextPriorityChange) == 1528);
const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, DxgkDdiResetDisplayEngine) == 1536);

/// Compile-time proof that the audited struct is the struct we bindgen.
///
/// `DRIVER_INITIALIZATION_DATA` is `Version: ULONG` followed by
/// [`SLOT_COUNT`] pointers, so the size is fully determined by the slot count.
/// A WDK that adds, removes or retypes a slot changes this number and fails the
/// build here rather than at `DxgkInitialize`.
const _: () = assert!(
    size_of::<DRIVER_INITIALIZATION_DATA>() == EXPECTED_STRUCT_SIZE,
    "DRIVER_INITIALIZATION_DATA size differs from the audited WDDM 3.2 layout; \
     regenerate kmd_render/src/ddi/wddm32_slot_audit.rs against the installed WDK"
);

/// Walk the table `build_ddi_table()` produced and check every slot against its
/// classification.
///
/// Reads the raw pointer word at each audited offset rather than the typed
/// field, so one loop covers all [`SLOT_COUNT`] slots including the WDK's
/// `PVOID`/`Reserved*` members. The offsets are the compile-time-asserted ones
/// above, so this cannot read outside the struct.
///
/// Returns the FIRST disagreement; `DriverEntry` reports it and refuses to load.
pub(crate) fn verify(data: &DRIVER_INITIALIZATION_DATA) -> Result<(), SlotAuditFailure> {
    let base = (data as *const DRIVER_INITIALIZATION_DATA).cast::<u8>();
    let mut i = 0;
    while i < SLOTS.len() {
        let slot = &SLOTS[i];
        // SAFETY: `slot.offset` is one of the offsets asserted at compile time
        // above to lie inside `DRIVER_INITIALIZATION_DATA`, and every member
        // after `Version` is a pointer-sized word. `read_unaligned` because the
        // caller's struct alignment is not this function's to assume.
        let word = unsafe { base.add(slot.offset).cast::<usize>().read_unaligned() };
        let registered = word != 0;
        // `Retiring` is verified like `Implemented` and `Pending` like
        // `Disabled`: the audit checks the table that is BUILT, and the two
        // transitional classes are the two directions it is still moving in.
        let expected = matches!(slot.class, SlotClass::Implemented | SlotClass::Retiring);
        if registered != expected {
            return Err(SlotAuditFailure {
                index: i,
                name: slot.name,
                registered,
            });
        }
        i += 1;
    }
    Ok(())
}
