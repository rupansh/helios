//! The Core-0116 native-fence KMD DDI surface.
//!
//! Normative: `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md`
//!
//! * §12.1 (lines 3130-3195) — the `HeliosNativeFencePddV1` (HNF1) 64-byte
//!   record and the exact create / share-open / GPU wait / GPU signal / CPU /
//!   close-destroy / multi-adapter sequence.
//! * §10.2 (lines 990-1004) — the conjunctive admission table. The rows this
//!   module owns are *KMD feature* (`DXGK_FEATURE_NATIVE_FENCE` enabled,
//!   `DXGK_VIDSCHCAPS::NativeGpuFence=1`, `No64BitAtomics=0`) and *native caps*
//!   (`DXGKQAITYPE_NATIVE_FENCE_CAPS` accepted with truthful stride / range /
//!   `MapToGpuSystemProcess`).
//! * §17.6 (lines 4328-4333) — "query caps/feature admission, create/open,
//!   close/destroy, current/monitored-value updates, and native-fence interrupt
//!   reporting. Driver global/local handles reference bounded native-fence
//!   objects directly; there is no hash table, scan, name, or process-global
//!   discovery registry."
//!
//! # Identity, and the structure that deliberately does not exist
//!
//! §12.1:3148-3152 makes KMD identity **exclusively** the OS-delivered
//! `hGlobalNativeFence` / `hLocalNativeFence`, and §10.1 invariant 10 forbids
//! any adapter- or process-global synchronization *discovery* structure. So
//! there is no table here: `DxgkDdiCreateNativeFence` allocates a
//! [`GlobalFenceObject`], hands its own address back as the driver's global
//! handle, and every later DDI reaches the object by dereferencing the handle
//! dxgkrnl returns. `DxgkDdiOpenNativeFence` does the same for
//! [`LocalFenceObject`].
//!
//! Two things are still bounded, because "bounded" is a requirement and a table
//! is not the only way to get it:
//!
//! * the **population**, through two counters checked against
//!   `helios_kmd_logic::native_fence_lifecycle::MAX_LIVE_{GLOBAL,LOCAL}`;
//! * the **validity**, through one adapter-wide epoch. Reset and removal bump
//!   [`invalidate_all`] once and every object minted earlier stops being usable
//!   — §12.1 item 6's "reset/removal invalidates every local mapping and
//!   generation" with no enumeration and nothing to scan.
//!
//! # The rules live in `kmd_logic`
//!
//! `kmd_render` is a `panic = "abort"` `no_std` cdylib and cannot run a test, so
//! every decision with a right and a wrong answer — PDD validation, population
//! bounds, the Live/Draining/Dead transitions, epoch validity, admission — is a
//! pure function in `helios_kmd_logic::native_fence_lifecycle` with host tests.
//! This file does the pointer work and the atomics.
//!
//! # Disabled until the surface flips
//!
//! Every advertisement here is gated on [`NATIVE_FENCE_ADVERTISED`], which is
//! false while `ddi::wddm_surface::SURFACE` is `Wddm2_1GpuMmu`. That flip is the
//! single atomic activation switch for the whole retirement and is its last edit
//! (`docs/retirement/OWNERSHIP.md` §3), so this code is complete and unreachable
//! rather than half-advertised. The DDI slots are registered regardless:
//! dxgkrnl does not invoke a WDDM 3.1/3.2 native-fence callback on a 2.1
//! adapter, and a registered-but-unreached slot is visible in the slot audit
//! while an unregistered one would have to be rediscovered later.
//!
//! # ⛔ The one unclosed window — `NF-UAF-1`
//!
//! **`DxgkDdiOpenNativeFence` racing `DxgkDdiDestroyNativeFence` on the same
//! global object is a use-after-free.** Found by the Phase-2 adversarial review
//! (2026-08-10), not yet fixed, and unreachable today because
//! [`NATIVE_FENCE_ADVERTISED`] is false — it becomes reachable at the single
//! atomic activation switch, which is precisely when a kernel UAF is most
//! expensive to discover by running.
//!
//! ```text
//!   A: open    model_of()             reads state = LIVE, refs = 0  -> Ok
//!   B: destroy model_of()             reads refs = 0                -> Ok
//!   B: destroy CAS(LIVE -> DEAD)      succeeds
//!   B: destroy free_global()          the object is FREED
//!   A: open    local_refs.fetch_add() writes freed memory, and hands dxgkrnl a
//!              LocalFenceObject whose `global` pointer dangles
//! ```
//!
//! The cause is structural rather than local: every decision here is taken from
//! a [`model_of`] snapshot and published by a *separate* atomic operation, so
//! any rule that reads both `state` and `local_refs` has a window between
//! deciding and acting. The sibling window on the teardown side — destroy
//! parking the object in `DRAINING` just after the last close already gave up
//! its own free attempt — was closed by [`finish_teardown_if_drained`], which
//! works only because both parties converge *after* `DRAINING` is published.
//! No such convergence point exists for open-versus-free.
//!
//! **The fix, which is deliberately not a re-read after the increment.** That
//! would narrow the window without closing it, and a narrowed race in kernel
//! code is a stopgap wearing a fix's clothes. `state` and `local_refs` must
//! become one `AtomicU64` (state in the high half, reference count in the low
//! half) so that deciding and transitioning are a single compare-exchange:
//! decode the word, hand the model to the `kmd_logic` rule that owns the
//! decision, then publish with a compare-exchange against the exact word the
//! rule saw, retrying if it moved. That keeps `kmd_logic` authoritative — the
//! rule is still the pure function — while removing the snapshot's TOCTOU.
//! It also needs `nf::destroy_global` to express "refused, and park in
//! `Draining`" as a transition rather than a bare `Err`, which is a signature
//! change in `kmd_logic` and its tests.
//!
//! # What is deliberately NOT here
//!
//! `DxgkDdiSetNativeFenceLogBuffer` / `DxgkDdiUpdateNativeFenceLogs` are
//! HWQueue-scoped (`DXGKARG_SETNATIVEFENCELOGBUFFER::hHwQueue`) and §17.6:4385
//! forbids advertising HWS/HWQueue in this generation, so both stay NULL and are
//! classified `Disabled` in `ddi/wddm32_slot_audit.rs`. Consistently,
//! `DXGK_VIDSCHCAPS::OptimizedNativeFenceSignaledInterrupt` stays 0 and
//! [`signal_native_fence_signaled`] reports an empty signalled-fence array,
//! which is the documented instruction to dxgkrnl to rescan every pending
//! waiter.

use core::ffi::c_void;
use core::mem::{align_of, offset_of, size_of};
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use alloc::boxed::Box;

use helios_kmd_logic::native_fence_lifecycle as nf;

use crate::adapter::AdapterContext;
use crate::ddi::wddm_surface::{WddmSurface, SURFACE};
use crate::dxgk::*;

/// Whether this build advertises the native-fence surface at all.
///
/// The native-fence DDIs are WDDM 3.1/3.2 slots, and §10.2:975-977 lets the KMD
/// report 3.2 only once the complete surface exists. Deriving the gate from
/// `SURFACE` rather than from a second constant means the advertisement cannot
/// disagree with the reported level: flipping `SURFACE` to `Wddm3_2GpuMmu` turns
/// this on, and nothing else does.
pub(crate) const NATIVE_FENCE_ADVERTISED: bool = matches!(SURFACE, WddmSurface::Wddm3_2GpuMmu);

/// `DXGK_VIDSCHCAPS::NativeGpuFence` — bit 11 of the `DXGK_DRIVERCAPS`
/// `SchedulingCaps` word.
///
/// Position derived field-by-field from WDK `shared/d3dkmddi.h`'s
/// `_DXGK_VIDSCHCAPS` bitfield at the WDDM 3.2 interface version:
/// 0 `MultiEngineAware`, 1 `VSyncPowerSaveAware`, 2 `PreemptionAware`,
/// 3 `NoDmaPatching`, 4 `CancelCommandAware`, 5 `No64BitAtomics`,
/// 6 `LowIrqlPreemptCommand`, 7-10 `HwQueuePacketCap` (4 bits),
/// **11 `NativeGpuFence`**, 12 `OptimizedNativeFenceSignaledInterrupt`.
/// Corroborated by the generated accessor
/// `DXGK_VIDSCHCAPS::OptimizedNativeFenceSignaledInterrupt`, which reads bit 12.
/// `query_adapter_info.rs` writes the word through `.Value`, the same
/// layout-independent convention its other cap bits use.
pub(crate) const VIDSCHCAPS_NATIVE_GPU_FENCE: u32 = 1 << 11;

/// `DXGK_VIDSCHCAPS::No64BitAtomics` — bit 5.
///
/// §10.2:994 requires it **zero**: the guest has 64-bit atomics, so claiming
/// otherwise would make dxgkrnl emulate every native-fence value update. Named
/// here so the requirement is a checked constant rather than the absence of a
/// line — see the assertion under [`vidschcaps_native_fence_bits`].
pub(crate) const VIDSCHCAPS_NO_64BIT_ATOMICS: u32 = 1 << 5;

/// `DXGK_VIDSCHCAPS::OptimizedNativeFenceSignaledInterrupt` — bit 12, kept 0.
///
/// Setting it would make dxgkrnl read `NativeFenceSignaled::hHWQueue` and the
/// per-HWQueue native-fence log buffer, neither of which exists in this
/// generation (§17.6:4385).
pub(crate) const VIDSCHCAPS_OPTIMIZED_NATIVE_FENCE_INTERRUPT: u32 = 1 << 12;

/// The `DXGK_VIDSCHCAPS` bits this module contributes, as one value.
///
/// Zero while the surface is not advertised, so the caps word cannot claim a
/// native-fence capability the reported WDDM level does not support.
pub(crate) const fn vidschcaps_native_fence_bits() -> u32 {
    if NATIVE_FENCE_ADVERTISED {
        VIDSCHCAPS_NATIVE_GPU_FENCE
    } else {
        0
    }
}

/// Compile-time proof of the two zero-valued halves of §10.2:994.
const _: () = assert!(
    vidschcaps_native_fence_bits()
        & (VIDSCHCAPS_NO_64BIT_ATOMICS | VIDSCHCAPS_OPTIMIZED_NATIVE_FENCE_INTERRUPT)
        == 0,
    "section 10.2 requires No64BitAtomics=0, and this generation advertises no HWQueue \
     native-fence log, so OptimizedNativeFenceSignaledInterrupt must stay 0"
);

// The HNF1 record must be exactly `D3DDDI_NATIVE_FENCE_PDD_SIZE` bytes. The WDK
// spells that size only as an array bound and `build.rs` does not allowlist the
// macro, so these assert the distance to the field that follows
// `pPrivateDriverData` in each DDI argument. Both are adjacent with no padding
// (a `BYTE[64]` at an 8-aligned offset followed by a 4-aligned member), which is
// exactly why the difference is the array length.
const _: () = assert!(
    offset_of!(DXGKARG_CREATENATIVEFENCE, Flags)
        - offset_of!(DXGKARG_CREATENATIVEFENCE, pPrivateDriverData)
        == nf::HNF1_SIZE,
    "DXGKARG_CREATENATIVEFENCE::pPrivateDriverData is not HNF1_SIZE bytes"
);
const _: () = assert!(
    offset_of!(DXGKARG_OPENNATIVEFENCE, Reserved)
        - offset_of!(DXGKARG_OPENNATIVEFENCE, pPrivateDriverData)
        == nf::HNF1_SIZE,
    "DXGKARG_OPENNATIVEFENCE::pPrivateDriverData is not HNF1_SIZE bytes"
);

// `DXGK_NATIVE_FENCE_CAPS`, asserted by the field names that are stable across
// WDK 26100 and 28000. The leading UINT is deliberately absent from this list:
// 26100 calls it `MonitoredValueStride` and 28000 renames it to
// `MonitoredValuePadding`. See `fill_native_fence_caps`.
const _: () = assert!(offset_of!(DXGK_NATIVE_FENCE_CAPS, MapToGpuSystemProcess) == 4);
const _: () = assert!(offset_of!(DXGK_NATIVE_FENCE_CAPS, MinimumAddress) == 8);
const _: () = assert!(offset_of!(DXGK_NATIVE_FENCE_CAPS, MaximumAddress) == 16);
const _: () = assert!(offset_of!(DXGK_NATIVE_FENCE_CAPS, Reserved) == 24);

// ── Named counters ───────────────────────────────────────────────────────────
//
// Every refusal below increments exactly one of these (CLAUDE.md rule 2). They
// are atomics rather than `diag::record` breadcrumbs because `record` is
// DiagLevel-gated and a refusal reported through it leaves no trace on a default
// boot. [`diag_dump_native_fence_atomics`] mirrors them into the service key.

/// Successful `DxgkDdiCreateNativeFence` calls.
pub static NF_CREATE_OK: AtomicU32 = AtomicU32::new(0);
/// `DxgkDdiCreateNativeFence` refusals, all causes.
pub static NF_CREATE_REJ: AtomicU32 = AtomicU32::new(0);
/// Successful `DxgkDdiOpenNativeFence` calls.
pub static NF_OPEN_OK: AtomicU32 = AtomicU32::new(0);
/// `DxgkDdiOpenNativeFence` refusals, all causes.
pub static NF_OPEN_REJ: AtomicU32 = AtomicU32::new(0);
/// Successful `DxgkDdiCloseNativeFence` calls.
pub static NF_CLOSE_OK: AtomicU32 = AtomicU32::new(0);
/// Successful `DxgkDdiDestroyNativeFence` calls.
pub static NF_DESTROY_OK: AtomicU32 = AtomicU32::new(0);
/// Close/destroy refusals — an OS ordering violation, or our own accounting
/// bug. **Must read 0.** A nonzero value means an object was deliberately
/// leaked rather than freed under a live reference.
pub static NF_TEARDOWN_REJ: AtomicU32 = AtomicU32::new(0);
/// The last [`nf::PddReject`] that refused a create or open, packed as
/// `(count << 8) | discriminant`, so one registry value names both how often and
/// which field. Discriminants are listed on [`pdd_reject_code`].
pub static NF_PDD_REJ: AtomicU32 = AtomicU32::new(0);
/// Operations refused because the object predates the current epoch (a reset
/// happened under it).
pub static NF_STALE_EPOCH: AtomicU32 = AtomicU32::new(0);
/// Operations refused because the surface is not admitted: the OS declined
/// `DXGK_FEATURE_NATIVE_FENCE`, the adapter LUID is unknown, or the reported
/// WDDM surface is below 3.2.
pub static NF_NOT_ADMITTED: AtomicU32 = AtomicU32::new(0);
/// `DXGKQAITYPE_NATIVE_FENCE_CAPS` answers.
pub static NF_CAPS_OK: AtomicU32 = AtomicU32::new(0);
/// `DXGKQAITYPE_NATIVE_FENCE_CAPS` refusals (not admitted, or buffer too small).
pub static NF_CAPS_REJ: AtomicU32 = AtomicU32::new(0);
/// Fence values published through `DxgkDdiUpdateMonitoredValues`.
pub static NF_MON_UPD: AtomicU32 = AtomicU32::new(0);
/// Fence values published through `DxgkDdiUpdateCurrentValuesFromCpu`.
pub static NF_CUR_UPD: AtomicU32 = AtomicU32::new(0);
/// Value updates refused (null array, undefined flag bit, unknown handle).
pub static NF_UPD_REJ: AtomicU32 = AtomicU32::new(0);
/// Value updates that moved a fence value BACKWARDS. Recorded, not refused —
/// see `nf::value_update_is_forward` for why refusing would wedge a fence.
pub static NF_UPD_BACKWARD: AtomicU32 = AtomicU32::new(0);
/// `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` notifications delivered.
pub static NF_INT_SIGNALED: AtomicU32 = AtomicU32::new(0);
/// `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` notifications dxgkrnl refused.
pub static NF_INT_FAILED: AtomicU32 = AtomicU32::new(0);
/// Adapter epoch bumps (reset / removal).
pub static NF_EPOCH_BUMPS: AtomicU32 = AtomicU32::new(0);

/// Live global (created) objects.
static LIVE_GLOBAL: AtomicU32 = AtomicU32::new(0);
/// Live local (opened) objects.
static LIVE_LOCAL: AtomicU32 = AtomicU32::new(0);
/// The adapter-wide validity epoch. Bumped by [`invalidate_all`].
static NATIVE_FENCE_EPOCH: AtomicU32 = AtomicU32::new(0);
/// Source of the KMD-assigned nonzero object generation (§12.1 line 3142).
static OBJECT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// `i64::MIN` is the "LUID not yet published" sentinel rather than 0, because 0
/// is a legal (if unlikely) LUID and treating it as unknown would leave a real
/// adapter permanently unadmitted.
const LUID_UNKNOWN: i64 = i64::MIN;
/// The exact creating adapter LUID, written into HNF1 offset 32.
static ADAPTER_LUID: AtomicI64 = AtomicI64::new(LUID_UNKNOWN);

/// The OS's answer to `DXGK_FEATURE_NATIVE_FENCE`, queried exactly once.
static FEATURE_STATE: AtomicU32 = AtomicU32::new(FEATURE_UNKNOWN);
/// Not asked yet.
const FEATURE_UNKNOWN: u32 = 0;
/// The OS returned `Enabled=TRUE`.
const FEATURE_ENABLED: u32 = 1;
/// The OS returned `Enabled=FALSE`, or there is no callback to ask.
const FEATURE_DECLINED: u32 = 2;

/// `DXGK_FEATURE_SUPPORT_STABLE` (`d3dkmdt.h:2159`, `((UINT)2)`), the driver
/// support level reported to `DxgkCbQueryFeatureSupport`.
///
/// Spelled as a literal because the WDK defines the level as an object-like
/// macro that `build.rs`'s `allowlist_var` set does not name, so bindgen does
/// not emit it. `ALWAYS_ON` would be a claim that the feature cannot be
/// disabled, which is false — the whole point of the query is that the OS may
/// decline.
const DXGK_FEATURE_SUPPORT_STABLE_VALUE: u32 = 2;

/// Publish the exact creating adapter LUID.
///
/// §12.1 line 3145 requires the KMD to write the exact creating adapter LUID
/// into HNF1, and the KMD's only source for it is `DXGK_START_INFO::AdapterLuid`
/// at `DxgkDdiStartDevice`. That function belongs to another unit of this lane,
/// so this is the one-line seam: until it calls here, [`native_fence_admitted`]
/// is false and every create/open refuses with `NfNotAdmit` rather than writing
/// a fabricated LUID.
///
/// CROSS-LANE: `ddi/lifecycle.rs::dxgkddi_start_device` must call
/// `crate::ddi::native_fence::publish_adapter_luid(<DXGK_START_INFO>.AdapterLuid)`.
#[allow(
    dead_code,
    reason = "cross-lane seam: lifecycle.rs (unit K10) supplies the LUID"
)]
pub(crate) fn publish_adapter_luid(luid: i64) {
    ADAPTER_LUID.store(luid, Ordering::Release);
}

/// The published adapter LUID, or `None` while StartDevice has not supplied one.
fn adapter_luid() -> Option<i64> {
    match ADAPTER_LUID.load(Ordering::Acquire) {
        LUID_UNKNOWN => None,
        luid => Some(luid),
    }
}

/// Invalidate every native-fence object on this adapter.
///
/// One atomic increment; no list is walked because none exists. Every object
/// carries the epoch it was minted under, so afterwards `nf::epoch_is_current`
/// refuses each of them individually the next time it is named. Close and
/// destroy deliberately keep working — see `nf::close_local`.
///
/// Callable at any IRQL. Intended for reset-from-timeout, stop-device and
/// remove-device.
///
/// CROSS-LANE: `ddi/submit_command.rs::dxgkddi_reset_from_timeout` and
/// `ddi/lifecycle.rs::{dxgkddi_stop_device,dxgkddi_remove_device}` should call
/// this.
#[allow(
    dead_code,
    reason = "cross-lane seam: the reset/stop/remove paths (units K9/K10) call this"
)]
pub(crate) fn invalidate_all() {
    NATIVE_FENCE_EPOCH.fetch_add(1, Ordering::AcqRel);
    NF_EPOCH_BUMPS.fetch_add(1, Ordering::Relaxed);
}

/// Ask the OS whether `DXGK_FEATURE_NATIVE_FENCE` may be enabled, once.
///
/// `d3dukmdt.h`: "For each feature in this enumeration, if the driver supports
/// it, it must invoke the OS QueryFeatureSupport callback to report the level of
/// support ... and only enable the feature if the OS returned Enabled=TRUE."
///
/// Runs from `DXGKQAITYPE_DRIVERCAPS` / `DXGKQAITYPE_NATIVE_FENCE_CAPS`, which
/// dxgkrnl issues after `DxgkDdiStartDevice` (so the callback table and
/// `DeviceHandle` exist) and before it consumes any scheduling cap.
///
/// # Safety
/// Caller must be at `PASSIVE_LEVEL` (`DXGKCB_QUERYFEATURESUPPORT` is annotated
/// `_IRQL_requires_(PASSIVE_LEVEL)`), and `adapter` must be a live adapter whose
/// `DxgkDdiStartDevice` has returned.
pub(crate) unsafe fn ensure_feature_admitted(adapter: &AdapterContext) -> bool {
    if !NATIVE_FENCE_ADVERTISED {
        return false;
    }
    match FEATURE_STATE.load(Ordering::Acquire) {
        FEATURE_ENABLED => return true,
        FEATURE_DECLINED => return false,
        _ => {}
    }

    // SAFETY: forwarded from this function's PASSIVE_LEVEL contract.
    let enabled = unsafe { query_feature_support(adapter) };
    // Two concurrent caps queries would store the same answer twice; the OS's
    // answer does not change within a device lifetime.
    FEATURE_STATE.store(
        if enabled {
            FEATURE_ENABLED
        } else {
            FEATURE_DECLINED
        },
        Ordering::Release,
    );
    enabled
}

/// The one `DxgkCbQueryFeatureSupport` round trip behind
/// [`ensure_feature_admitted`].
///
/// # Safety
/// PASSIVE_LEVEL only.
unsafe fn query_feature_support(adapter: &AdapterContext) -> bool {
    let Some(dxgkrnl) = adapter.dxgkrnl_opt() else {
        return false;
    };
    let Some(query) = dxgkrnl.DxgkCbQueryFeatureSupport else {
        // An OS without the callback cannot grant the feature, and §3 forbids a
        // fallback. Declining is the fail-closed answer.
        return false;
    };
    // SAFETY: an all-zero DXGKARGCB_QUERYFEATURESUPPORT is a valid input shape;
    // every field is written before the call.
    let mut args: DXGKARGCB_QUERYFEATURESUPPORT = unsafe { core::mem::zeroed() };
    args.DeviceHandle = dxgkrnl.DeviceHandle;
    args.FeatureId = _DXGK_FEATURE_ID::DXGK_FEATURE_NATIVE_FENCE;
    args.DriverSupportState = DXGK_FEATURE_SUPPORT_STABLE_VALUE;
    args.Enabled = 0;
    // SAFETY: PASSIVE_LEVEL per this function's contract; `args` is fully
    // initialized and lives across the synchronous call.
    let status = unsafe { query(&mut args) };
    status == STATUS_SUCCESS && args.Enabled != 0
}

/// Whether the whole §10.2 native-fence admission conjunction holds.
fn native_fence_admitted() -> bool {
    nf::surface_is_admitted(
        NATIVE_FENCE_ADVERTISED,
        FEATURE_STATE.load(Ordering::Acquire) == FEATURE_ENABLED,
        adapter_luid().is_some(),
        vidschcaps_native_fence_bits() & VIDSCHCAPS_NATIVE_GPU_FENCE != 0,
    )
}

/// The registry-visible code for a PDD refusal. Stable numbering: append only.
fn pdd_reject_code(reject: nf::PddReject) -> u32 {
    match reject {
        nf::PddReject::Magic => 1,
        nf::PddReject::AbiVersion => 2,
        nf::PddReject::StructSize => 3,
        nf::PddReject::PackageGeneration => 4,
        nf::PddReject::ObjectGenerationNotZero => 5,
        nf::PddReject::NativeType => 6,
        nf::PddReject::NativeTypeMismatch => 7,
        nf::PddReject::Flags => 8,
        nf::PddReject::AdapterLuid => 9,
        nf::PddReject::Reserved => 10,
    }
}

/// Record a PDD refusal: how many, and which field last.
fn note_pdd_reject(reject: nf::PddReject) {
    let prior = NF_PDD_REJ.load(Ordering::Relaxed) >> 8;
    let count = prior.saturating_add(1).min(0x00FF_FFFF);
    NF_PDD_REJ.store((count << 8) | pdd_reject_code(reject), Ordering::Relaxed);
}

// ── The objects ──────────────────────────────────────────────────────────────

/// Tag on a global object. Distinguishes it from a local one, so a
/// `hLocalNativeFence` handed to a global-only DDI is refused rather than
/// reinterpreted.
const GLOBAL_MAGIC: u32 = 0x3046_4e48; // "HNF0"
/// Tag on a local object.
const LOCAL_MAGIC: u32 = 0x4c46_4e48; // "HNFL"

/// Lifecycle state, as stored in [`GlobalFenceObject::state`].
const STATE_LIVE: u32 = 0;
/// Destroy ran while local references were still outstanding.
const STATE_DRAINING: u32 = 1;
/// Freed, or claimed for freeing by exactly one thread.
const STATE_DEAD: u32 = 2;

/// One created native-fence object. Its own address is the driver's
/// `hGlobalNativeFence`.
#[repr(C)]
struct GlobalFenceObject {
    /// [`GLOBAL_MAGIC`]. First field so a mistyped handle is caught by one read.
    magic: u32,
    /// The adapter epoch this object was minted under.
    epoch: u32,
    /// The KMD-assigned nonzero object generation written back into HNF1.
    object_generation: u64,
    /// The dxgkrnl handle the OS created the object with. Kept for diagnostics
    /// and never serialized into the PDD (§12.1 line 3152).
    os_handle: HANDLE,
    /// The documented `D3DDDI_NATIVEFENCE_TYPE` this object was created with.
    native_type: u32,
    /// HNF1 flags, validated to contain only `HNF1_FLAG_SHARED`.
    flags: u32,
    /// Opened local objects still pointing here.
    local_refs: AtomicU32,
    /// `STATE_LIVE` / `STATE_DRAINING` / `STATE_DEAD`. Atomic because close and
    /// destroy can run on different threads, and the Live/Draining -> Dead
    /// compare-exchange is what makes the free happen exactly once.
    state: AtomicU32,
    /// Last value published through `DxgkDdiUpdateCurrentValuesFromCpu`, for the
    /// forward-progress diagnostic only.
    last_current_value: AtomicU64,
    /// Last value published through `DxgkDdiUpdateMonitoredValues`, likewise.
    last_monitored_value: AtomicU64,
}

/// One opened native-fence object. Its own address is the driver's
/// `hLocalNativeFence`.
#[repr(C)]
struct LocalFenceObject {
    /// [`LOCAL_MAGIC`].
    magic: u32,
    /// The adapter epoch this object was minted under.
    epoch: u32,
    /// The global object this local view refers to — a direct strong reference
    /// (§17.6:4331), not a key into anything.
    global: *mut GlobalFenceObject,
    /// The dxgkrnl handle for this local object.
    os_handle: HANDLE,
}

/// Resolve a driver global handle.
///
/// # Safety
/// `handle` must be a value this driver returned from
/// `DxgkDdiCreateNativeFence` and has not yet freed. dxgkrnl only ever hands
/// back driver handles it received from us; the magic check turns a *mistyped*
/// (local-for-global) handle into a refusal rather than a misinterpretation.
unsafe fn global_from_handle<'a>(handle: HANDLE) -> Option<&'a GlobalFenceObject> {
    let p = handle as *const GlobalFenceObject;
    if p.is_null() || (p as usize) % align_of::<GlobalFenceObject>() != 0 {
        return None;
    }
    // SAFETY: per this function's contract the pointer is one we allocated and
    // have not yet freed.
    let obj = unsafe { &*p };
    (obj.magic == GLOBAL_MAGIC).then_some(obj)
}

/// Resolve a driver local handle. Same contract as [`global_from_handle`].
///
/// # Safety
/// `handle` must be a value this driver returned from
/// `DxgkDdiOpenNativeFence` and has not yet freed.
unsafe fn local_is_ours(handle: HANDLE) -> bool {
    let p = handle as *const LocalFenceObject;
    if p.is_null() || (p as usize) % align_of::<LocalFenceObject>() != 0 {
        return false;
    }
    // SAFETY: per this function's contract the pointer is one we allocated and
    // have not yet freed.
    unsafe { (*p).magic == LOCAL_MAGIC }
}

/// Snapshot a global object into the `kmd_logic` model so the pure rules can
/// judge it.
fn model_of(global: &GlobalFenceObject) -> nf::GlobalFence {
    nf::GlobalFence {
        state: match global.state.load(Ordering::Acquire) {
            STATE_LIVE => nf::FenceState::Live,
            STATE_DRAINING => nf::FenceState::Draining,
            _ => nf::FenceState::Dead,
        },
        local_refs: global.local_refs.load(Ordering::Acquire),
        epoch: global.epoch,
        object_generation: global.object_generation,
    }
}

/// Bounded increment of a population counter, refusing at its ceiling.
///
/// Compare-exchange rather than `fetch_add` so an exhausted pool never
/// transiently exceeds its bound.
fn admit(counter: &AtomicU32, ceiling: u32) -> bool {
    let mut live = counter.load(Ordering::Relaxed);
    loop {
        if live >= ceiling {
            return false;
        }
        match counter.compare_exchange_weak(live, live + 1, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => live = observed,
        }
    }
}

/// Bounded decrement, refusing an underflow rather than wrapping.
fn retire(counter: &AtomicU32) -> bool {
    let mut live = counter.load(Ordering::Relaxed);
    loop {
        if live == 0 {
            return false;
        }
        match counter.compare_exchange_weak(live, live - 1, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => live = observed,
        }
    }
}

// ── The DDIs ─────────────────────────────────────────────────────────────────

/// `DxgkDdiCreateNativeFence` — §12.1 item 1. PASSIVE_LEVEL.
///
/// Reads `pPrivateDriverData` as HNF1, validates it against the package
/// generation, the exact adapter LUID and the OS-supplied `Type`, allocates the
/// bounded object, writes its own address into `hGlobalNativeFence`, and writes
/// the KMD's answer back into the same buffer.
///
/// # Safety
/// Called by dxgkrnl with a live `hAdapter` and a writable
/// `DXGKARG_CREATENATIVEFENCE`.
pub unsafe extern "C" fn dxgkddi_create_native_fence(
    h_adapter: IN_CONST_HANDLE,
    p_create: *mut DXGKARG_CREATENATIVEFENCE,
) -> NTSTATUS {
    if h_adapter.is_null() || p_create.is_null() {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let Some(luid) = admitted_luid(&NF_CREATE_REJ) else {
        return STATUS_NOT_SUPPORTED;
    };

    // SAFETY: non-null per the check above; dxgkrnl owns a valid, writable
    // DXGKARG_CREATENATIVEFENCE for the duration of the call.
    let args = unsafe { &mut *p_create };

    // Copy the caller's bytes out before validating, so nothing downstream can
    // observe a value that changed between check and use.
    let pdd: [u8; nf::HNF1_SIZE] = args.pPrivateDriverData;
    let native_type = args.Type as u32;
    let parsed = match nf::validate_create(
        &pdd,
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        luid,
        native_type,
    ) {
        Ok(parsed) => parsed,
        Err(reject) => {
            NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
            note_pdd_reject(reject);
            return STATUS_INVALID_PARAMETER;
        }
    };
    // Every bit of `DXGKARG_CREATENATIVEFENCE::Flags` is reserved in this WDK
    // revision; a nonzero value is an OS revision we have not implemented
    // against, and guessing would be the "no version fallback" §3 forbids.
    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view.
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }

    if !admit(&LIVE_GLOBAL, nf::MAX_LIVE_GLOBAL) {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    let object_generation =
        nf::next_object_generation(OBJECT_GENERATION.fetch_add(1, Ordering::Relaxed));
    let epoch = NATIVE_FENCE_EPOCH.load(Ordering::Acquire);
    let object = Box::new(GlobalFenceObject {
        magic: GLOBAL_MAGIC,
        epoch,
        object_generation,
        os_handle: args.hGlobalNativeFence,
        native_type,
        flags: parsed.flags,
        local_refs: AtomicU32::new(0),
        state: AtomicU32::new(STATE_LIVE),
        last_current_value: AtomicU64::new(0),
        last_monitored_value: AtomicU64::new(0),
    });

    // The driver handle IS the object address: §12.1:3148-3150 makes identity
    // the OS-delivered handle pair, and the KMD half of that pair is this
    // pointer. Nothing else maps a handle to an object.
    args.hGlobalNativeFence = Box::into_raw(object) as HANDLE;
    args.pPrivateDriverData = nf::encode(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        object_generation,
        native_type,
        parsed.flags,
        luid,
    );
    NF_CREATE_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// The §10.2 admission conjunction plus the LUID, as one gate.
///
/// Increments `rejection_counter` and `NfNotAdmit` on refusal so the caller's
/// error return is the only thing left to choose.
fn admitted_luid(rejection_counter: &AtomicU32) -> Option<i64> {
    if !native_fence_admitted() {
        rejection_counter.fetch_add(1, Ordering::Relaxed);
        NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    match adapter_luid() {
        Some(luid) => Some(luid),
        None => {
            // Unreachable while `native_fence_admitted` includes the LUID gate;
            // kept because the two are separate facts and a future edit to one
            // must not silently fabricate the other.
            rejection_counter.fetch_add(1, Ordering::Relaxed);
            NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// `DxgkDdiOpenNativeFence` — §12.1 item 2. PASSIVE_LEVEL.
///
/// Takes a local reference on the global object and returns a second bounded
/// object as `hLocalNativeFence`.
///
/// # Safety
/// Called by dxgkrnl with a live `hAdapter` and a writable
/// `DXGKARG_OPENNATIVEFENCE` whose `hGlobalNativeFence` is a driver handle this
/// module returned from [`dxgkddi_create_native_fence`].
pub unsafe extern "C" fn dxgkddi_open_native_fence(
    h_adapter: IN_CONST_HANDLE,
    p_open: *mut DXGKARG_OPENNATIVEFENCE,
) -> NTSTATUS {
    if h_adapter.is_null() || p_open.is_null() {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let Some(luid) = admitted_luid(&NF_OPEN_REJ) else {
        return STATUS_NOT_SUPPORTED;
    };
    // SAFETY: non-null per the check above.
    let args = unsafe { &mut *p_open };

    // SAFETY: dxgkrnl returns only driver handles this module assigned and has
    // not freed.
    let Some(global) = (unsafe { global_from_handle(args.hGlobalNativeFence) }) else {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_HANDLE;
    };

    let pdd: [u8; nf::HNF1_SIZE] = args.pPrivateDriverData;
    if let Err(reject) = nf::validate_open(&pdd, helios_protocol::HELIOS_PACKAGE_GENERATION) {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        note_pdd_reject(reject);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view; every
    // bit is reserved in this revision.
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    // A non-shared object has no legal opener. §12.1 item 7 additionally
    // rejects LDA and cross-adapter, which is why HNF1 has no cross-adapter bit
    // to admit here.
    if global.flags & nf::HNF1_FLAG_SHARED == 0 {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_ACCESS_DENIED;
    }

    let epoch = NATIVE_FENCE_EPOCH.load(Ordering::Acquire);
    match nf::open_local(model_of(global), epoch) {
        Ok(_) => {}
        Err(nf::Refusal::StaleEpoch) => {
            NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
            NF_STALE_EPOCH.fetch_add(1, Ordering::Relaxed);
            return STATUS_DEVICE_REMOVED;
        }
        Err(_) => {
            NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
            return STATUS_INVALID_DEVICE_REQUEST;
        }
    }

    if !admit(&LIVE_LOCAL, nf::MAX_LIVE_LOCAL) {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // ⛔ KNOWN DEFECT — `NF-UAF-1`, open racing destroy. See the module header's
    // "The one unclosed window" section. Do not read the increment below as
    // safe: `nf::open_local` decided against the `model_of` snapshot taken
    // above, and a concurrent `DxgkDdiDestroyNativeFence` that sampled
    // `local_refs == 0` in its own snapshot frees this object between that
    // decision and this line. Closing it needs `state` and `local_refs` packed
    // into one atomic word so the rule and the transition are a single
    // compare-exchange; a re-read after the increment only narrows the window
    // and would be a stopgap. Unreachable today: `NATIVE_FENCE_ADVERTISED` is
    // false, so dxgkrnl never calls this DDI.
    global.local_refs.fetch_add(1, Ordering::AcqRel);

    let local = Box::new(LocalFenceObject {
        magic: LOCAL_MAGIC,
        epoch: global.epoch,
        global: global as *const GlobalFenceObject as *mut GlobalFenceObject,
        os_handle: args.hLocalNativeFence,
    });
    args.hLocalNativeFence = Box::into_raw(local) as HANDLE;
    // §12.1 item 2: the KMD writes the record back and the opening UMD is the
    // party that validates package/LUID/type against it.
    args.pPrivateDriverData = nf::encode(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        global.object_generation,
        global.native_type,
        global.flags,
        luid,
    );
    NF_OPEN_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// `DxgkDdiCloseNativeFence` — §12.1 item 6, the per-process half.
/// PASSIVE_LEVEL.
///
/// Frees the local object and drops its reference on the global one. A stale
/// epoch is deliberately NOT an error: teardown must complete across a reset or
/// the object leaks.
///
/// # Safety
/// Called by dxgkrnl with a `hLocalNativeFence` this module returned from
/// [`dxgkddi_open_native_fence`], exactly once per successful open.
pub unsafe extern "C" fn dxgkddi_close_native_fence(
    h_adapter: IN_CONST_HANDLE,
    p_close: *mut DXGKARG_CLOSENATIVEFENCE,
) -> NTSTATUS {
    let _ = h_adapter;
    if p_close.is_null() {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above.
    let args = unsafe { &mut *p_close };
    let handle = args.hLocalNativeFence;
    // SAFETY: dxgkrnl returns only driver handles this module assigned.
    if !unsafe { local_is_ours(handle) } {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_HANDLE;
    }
    // SAFETY: validated above as our own `LocalFenceObject`, produced by
    // `Box::into_raw` in `dxgkddi_open_native_fence` and reclaimed exactly once
    // here — dxgkrnl calls close once per successful open.
    let local = unsafe { Box::from_raw(handle as *mut LocalFenceObject) };
    args.hLocalNativeFence = core::ptr::null_mut();

    // SAFETY: a local object holds a reference on its global for its whole
    // lifetime, and the global is freed only after that count reaches zero, so
    // the pointer is live here.
    let global = unsafe { &*local.global };

    // The pure model owns the rule; the atomics below own exactly-once.
    if nf::close_local(model_of(global)).is_err() {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        // The local object is already unlinked from the OS handle, so freeing it
        // is still correct; only the global bookkeeping is refused.
        drop(local);
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if !retire(&global.local_refs) {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        drop(local);
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    let _ = retire(&LIVE_LOCAL);
    let global_ptr = local.global;
    drop(local);

    // The last close of an object whose destroy already ran owns the free.
    finish_teardown_if_drained(global, global_ptr);
    NF_CLOSE_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// `DxgkDdiDestroyNativeFence` — §12.1 item 6, the global half. PASSIVE_LEVEL.
///
/// Note the WDK signature: it takes **no** `hAdapter`.
///
/// If a local object still points at the global one, freeing here would dangle
/// it, so the object moves to `DRAINING`, `NfTeardnRej` moves, and the last
/// [`dxgkddi_close_native_fence`] frees it instead. Leaking a bounded object is
/// the fail-closed choice against a use-after-free inside a DDI.
///
/// # Safety
/// Called by dxgkrnl with a `hGlobalNativeFence` this module returned from
/// [`dxgkddi_create_native_fence`], exactly once per successful create.
pub unsafe extern "C" fn dxgkddi_destroy_native_fence(
    p_destroy: *mut DXGKARG_DESTROYNATIVEFENCE,
) -> NTSTATUS {
    if p_destroy.is_null() {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above.
    let args = unsafe { &mut *p_destroy };
    let handle = args.hGlobalNativeFence;
    // SAFETY: dxgkrnl returns only driver handles this module assigned.
    let Some(global) = (unsafe { global_from_handle(handle) }) else {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_HANDLE;
    };

    match nf::destroy_global(model_of(global)) {
        Ok(_) => {
            // Claim the free. Only the thread that wins LIVE -> DEAD frees.
            if global
                .state
                .compare_exchange(STATE_LIVE, STATE_DEAD, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // The rule said Ok, so references are drained — but the state
                // was not LIVE, which means an earlier destroy already parked
                // this object in DRAINING. Nobody else will come: the closes
                // are done and this is the destroy. Reclaim it here rather
                // than refusing and leaking the object plus its LIVE_GLOBAL
                // slot.
                finish_teardown_if_drained(global, handle as *mut GlobalFenceObject);
                NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
                return STATUS_INVALID_DEVICE_REQUEST;
            }
            args.hGlobalNativeFence = core::ptr::null_mut();
            free_global(handle as *mut GlobalFenceObject);
            NF_DESTROY_OK.fetch_add(1, Ordering::Relaxed);
            STATUS_SUCCESS
        }
        Err(nf::Refusal::LocalReferencesOutstanding) => {
            // Do NOT free here: locals hold a raw pointer to this object. Hand
            // the free to whichever close drops the last reference.
            let _ = global.state.compare_exchange(
                STATE_LIVE,
                STATE_DRAINING,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            // ⚠ The last close may have landed between `model_of` above and the
            // compare-exchange just now, in which case its own DRAINING -> DEAD
            // attempt failed against a state that was still LIVE and nobody
            // owns the free. Re-check now that DRAINING is published; see
            // `finish_teardown_if_drained` for the full interleaving.
            finish_teardown_if_drained(global, handle as *mut GlobalFenceObject);
            NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
            STATUS_INVALID_DEVICE_REQUEST
        }
        Err(_) => {
            NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
            STATUS_INVALID_DEVICE_REQUEST
        }
    }
}

/// The one place a `DRAINING` object is reclaimed, and the only exactly-once
/// claim on that transition.
///
/// # Why this must be called from three places and not one
///
/// A destroy that finds local references outstanding parks the object in
/// `DRAINING` and hands the free to "whichever close drops the last reference".
/// That is correct only if every party that can *observe* the reference count
/// reach zero also re-checks, because the count and the state are two separate
/// atomics and the decision to park is taken from a snapshot of both.
///
/// Concretely, the interleaving this closes (`local_refs == 1`, destroy on
/// thread A, the last close on thread B):
///
/// ```text
///   A: model_of()          reads local_refs = 1  -> LocalReferencesOutstanding
///   B: retire(local_refs)  1 -> 0
///   B: CAS(DRAINING->DEAD) FAILS, state is still LIVE  -> B does not free
///   A: CAS(LIVE->DRAINING) succeeds                    -> A does not free
///   =>  state = DRAINING, local_refs = 0, and no further close will ever
///       arrive, so the object and its LIVE_GLOBAL slot leak forever.
/// ```
///
/// Re-checking here, *after* `DRAINING` is published, makes the two orderings
/// converge: whichever party observes zero last performs the free, and the
/// compare-exchange makes sure only one of them does.
fn finish_teardown_if_drained(global: &GlobalFenceObject, p: *mut GlobalFenceObject) {
    if global.local_refs.load(Ordering::Acquire) == 0
        && global
            .state
            .compare_exchange(
                STATE_DRAINING,
                STATE_DEAD,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    {
        free_global(p);
    }
}

/// Free one global object and drop the population count.
///
/// The caller must already have won the compare-exchange into [`STATE_DEAD`],
/// which is what makes this exactly-once.
fn free_global(p: *mut GlobalFenceObject) {
    if p.is_null() {
        return;
    }
    // SAFETY: `p` was produced by `Box::into_raw` in
    // `dxgkddi_create_native_fence`; the caller won the single
    // `LIVE|DRAINING -> DEAD` transition and no local reference remains, so this
    // runs exactly once and no other reference to the object is live.
    drop(unsafe { Box::from_raw(p) });
    let _ = retire(&LIVE_GLOBAL);
    // Teardown is the bounded, naturally rare moment to publish. Publishing per
    // operation would turn a fence-heavy frame into a registry write storm.
    if LIVE_GLOBAL.load(Ordering::Acquire) == 0 {
        diag_dump_native_fence_atomics();
    }
}

/// `DxgkDdiUpdateMonitoredValues` — publish new monitored values into the
/// OS-owned storage.
///
/// `_IRQL_requires_max_(PROFILE_LEVEL - 1)`, so this path takes no lock,
/// allocates nothing, and calls nothing pageable.
///
/// # Safety
/// Called by dxgkrnl with a `DXGKARG_UPDATEMONITOREDVALUES` whose three arrays
/// each hold `NumFences` valid entries.
pub unsafe extern "C" fn dxgkddi_update_monitored_values(
    p_args: *const DXGKARG_UPDATEMONITOREDVALUES,
) -> NTSTATUS {
    if p_args.is_null() {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above; the struct is read-only for us.
    let args = unsafe { &*p_args };
    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view; every
    // bit is reserved in this revision.
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    // SAFETY: the three arrays are `_Field_size_(NumFences)`; `update_values`
    // reads exactly that many entries and refuses a null array outright.
    unsafe {
        update_values(
            args.NativeFenceArray,
            args.UpdatedValueArray,
            args.MonitoredValueKernelCpuVa,
            args.NumFences,
            /* write_storage */ true,
            /* monitored */ true,
        )
    }
}

/// `DxgkDdiUpdateCurrentValuesFromCpu` — publish new current values.
/// `_IRQL_requires_max_(DISPATCH_LEVEL)`.
///
/// `NotificationOnly` means the OS already wrote the storage and the driver only
/// has to make the GPU observe it. Helios has no context-management processor
/// and `DXGK_NATIVE_FENCE_CAPS::MapToGpuSystemProcess` is FALSE, so there is
/// nothing to notify and that arm is a counted no-op — stated here rather than
/// left as an unexplained early return.
///
/// # Safety
/// Called by dxgkrnl with a `DXGKARG_UPDATECURRENTVALUESFROMCPU` whose three
/// arrays each hold `NumFences` valid entries.
pub unsafe extern "C" fn dxgkddi_update_current_values_from_cpu(
    p_args: *const DXGKARG_UPDATECURRENTVALUESFROMCPU,
) -> NTSTATUS {
    if p_args.is_null() {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above.
    let args = unsafe { &*p_args };
    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view.
    let flags = unsafe { args.Flags.__bindgen_anon_1.Value };
    /// `DXGK_UPDATECURRENTVALUESFROMCPU_FLAGS::AlwaysSignaled`, bit 0.
    const ALWAYS_SIGNALED: u32 = 1 << 0;
    /// `DXGK_UPDATECURRENTVALUESFROMCPU_FLAGS::NotificationOnly`, bit 1.
    const NOTIFICATION_ONLY: u32 = 1 << 1;
    if flags & !(ALWAYS_SIGNALED | NOTIFICATION_ONLY) != 0 {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    // SAFETY: as for `dxgkddi_update_monitored_values`.
    unsafe {
        update_values(
            args.NativeFenceArray,
            args.UpdatedValueArray,
            args.CurrentValueKernelCpuVa,
            args.NumFences,
            /* write_storage */ flags & NOTIFICATION_ONLY == 0,
            /* monitored */ false,
        )
    }
}

/// The shared body of the two value-update DDIs.
///
/// One bad entry fails the whole call: a partial publication would leave the OS
/// believing values it never received.
///
/// # Safety
/// `handles`, `values` and `storage` must each be non-null arrays of `count`
/// valid entries, and every `storage[i]` must be a writable kernel CPU VA of at
/// least 8 bytes (the WDK's `_Field_size_` annotations).
unsafe fn update_values(
    handles: *mut HANDLE,
    values: *mut u64,
    storage: *mut *mut c_void,
    count: u32,
    write_storage: bool,
    monitored: bool,
) -> NTSTATUS {
    if count == 0 {
        return STATUS_SUCCESS;
    }
    if handles.is_null() || values.is_null() || storage.is_null() {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let epoch = NATIVE_FENCE_EPOCH.load(Ordering::Acquire);
    let mut i = 0usize;
    while i < count as usize {
        // SAFETY: `i < count` and all three arrays hold `count` entries.
        let (handle, value, slot) = unsafe {
            (
                handles.add(i).read(),
                values.add(i).read(),
                storage.add(i).read(),
            )
        };
        // SAFETY: dxgkrnl returns only driver handles this module assigned.
        let Some(global) = (unsafe { global_from_handle(handle) }) else {
            NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
            return STATUS_INVALID_HANDLE;
        };
        if !nf::epoch_is_current(global.epoch, epoch) {
            NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
            NF_STALE_EPOCH.fetch_add(1, Ordering::Relaxed);
            return STATUS_DEVICE_REMOVED;
        }

        let last = if monitored {
            &global.last_monitored_value
        } else {
            &global.last_current_value
        };
        let previous = last.swap(value, Ordering::AcqRel);
        if !nf::value_update_is_forward(previous, value) {
            // Recorded, never refused: the OS owns these values and refusing a
            // backwards update would wedge the runtime's fence.
            NF_UPD_BACKWARD.fetch_add(1, Ordering::Relaxed);
        }

        if write_storage {
            if slot.is_null() {
                NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            }
            // SAFETY: `slot` is the OS-supplied read/write kernel CPU VA of this
            // fence's 8-byte value storage, valid for the duration of the call.
            unsafe { slot.cast::<u64>().write_volatile(value) };
        }
        i += 1;
    }
    if monitored {
        NF_MON_UPD.fetch_add(count, Ordering::Relaxed);
    } else {
        NF_CUR_UPD.fetch_add(count, Ordering::Relaxed);
    }
    STATUS_SUCCESS
}

// ── DXGKQAITYPE_NATIVE_FENCE_CAPS ────────────────────────────────────────────

/// Fill `DXGK_NATIVE_FENCE_CAPS` for `DXGKQAITYPE_NATIVE_FENCE_CAPS` (= 37).
///
/// §10.2:995 requires the query to be "accepted with truthful stride/range/
/// `MapToGpuSystemProcess`":
///
/// * **stride** — the leading `UINT` is left **zero**. Note the WDK renamed it:
///   26100 calls it `MonitoredValueStride` ("stride for monitored values ...
///   packed in the same page") and 28000 calls it `MonitoredValuePadding`
///   ("additional reserved bytes ... below the standard 8 bytes"). Zero is the
///   truthful answer under *both* readings — Helios needs no packing stride of
///   its own and reserves no bytes below the value — so the field is left to the
///   zero-fill and deliberately not named, which is also what lets this module
///   build against either WDK.
/// * **`MapToGpuSystemProcess` = FALSE** — the same fact stated another way:
///   with no context-management processor there is nothing in a GPU
///   system-process address space to map the value into, and §12.1 items 3 and 4
///   route every wait and signal through
///   `pfnWaitForSynchronizationObjectFromGpuCb` /
///   `pfnSignalSynchronizationObjectFromGpuCb`, never a direct GPUVA write.
/// * **range** — the exact GpuMmu addressable range this driver advertises,
///   `[0, 2^VIRTUAL_ADDRESS_BIT_COUNT - 1]`, read from `ddi::gpummu` so it
///   cannot drift from `DXGK_GPUMMUCAPS`.
///
/// # Safety
/// `args.pOutputData` must be writable for `args.OutputDataSize` bytes.
pub(crate) unsafe fn fill_native_fence_caps(
    adapter: &AdapterContext,
    args: &DXGKARG_QUERYADAPTERINFO,
) -> NTSTATUS {
    // SAFETY: DxgkDdiQueryAdapterInfo is documented PASSIVE_LEVEL, which is what
    // DxgkCbQueryFeatureSupport requires.
    let admitted = unsafe { ensure_feature_admitted(adapter) } && native_fence_admitted();
    if !admitted {
        NF_CAPS_REJ.fetch_add(1, Ordering::Relaxed);
        NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    if (args.OutputDataSize as usize) < size_of::<DXGK_NATIVE_FENCE_CAPS>() {
        NF_CAPS_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_BUFFER_TOO_SMALL;
    }
    // Zero the whole declared buffer first, through the raw pointer, before any
    // other pointer into it exists — the same discipline `query_driver_caps`
    // uses. This is also what writes the leading stride/padding UINT.
    // SAFETY: dxgkrnl gives us `OutputDataSize` writable bytes at `pOutputData`.
    unsafe { core::ptr::write_bytes(args.pOutputData as *mut u8, 0, args.OutputDataSize as usize) };
    // SAFETY: the size gate above proves the buffer holds a whole
    // DXGK_NATIVE_FENCE_CAPS, and no other pointer into it is live.
    let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_NATIVE_FENCE_CAPS) };
    caps.MapToGpuSystemProcess = 0;
    caps.MinimumAddress = 0;
    // Inclusive maximum of the advertised GpuMmu virtual-address range.
    caps.MaximumAddress = (1u64 << crate::ddi::gpummu::VIRTUAL_ADDRESS_BIT_COUNT) - 1;
    NF_CAPS_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

// ── DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED ─────────────────────────────────────

/// Context handed across the `DxgkCbSynchronizeExecution` boundary.
struct NotifyCtx {
    /// The adapter's callback table.
    dxgkrnl: *const DXGKRNL_INTERFACE,
    /// The prepared interrupt packet.
    interrupt: *mut DXGKARGCB_NOTIFY_INTERRUPT_DATA,
}

/// Runs at the device's DIRQL, synchronized with the ISR — the only level at
/// which `DxgkCbNotifyInterrupt` may be called.
///
/// Duplicates `submit_command.rs::notify_dma_completed_routine`, which is
/// private to that module. CROSS-LANE: when unit K6/K9 lands, promote
/// `submit_command::notify_at_dirql` to `pub(crate)` and delete this pair.
unsafe extern "C" fn notify_routine(context: *mut c_void) -> BOOLEAN {
    if context.is_null() {
        return 0;
    }
    // SAFETY: `context` is the `NotifyCtx` passed to DxgkCbSynchronizeExecution,
    // valid for the duration of that synchronous call.
    let ctx = unsafe { &*(context as *const NotifyCtx) };
    // SAFETY: same lifetime as `ctx`.
    let dxgkrnl = unsafe { &*ctx.dxgkrnl };
    if let Some(notify) = dxgkrnl.DxgkCbNotifyInterrupt {
        // SAFETY: at DIRQL (raised by DxgkCbSynchronizeExecution); `interrupt`
        // is a fully-initialized packet live for this call.
        unsafe { notify(dxgkrnl.DeviceHandle, ctx.interrupt) };
    }
    if let Some(queue_dpc) = dxgkrnl.DxgkCbQueueDpc {
        // Same notify+DPC pairing the DMA-completed path uses, so dxgkrnl sees
        // one interrupt-completion event.
        // SAFETY: callable at DIRQL with a live DeviceHandle.
        unsafe { queue_dpc(dxgkrnl.DeviceHandle) };
    }
    1
}

/// Report `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` (= 19) for node 0 / engine 0.
///
/// `SignaledNativeFenceCount = 0` with a NULL array is the WDK's documented
/// instruction to dxgkrnl to **rescan every pending native-fence waiter**
/// instead of a named subset. That is the truthful report for this device: the
/// host signals through the Venus completion path, which tells us that fence
/// values advanced but not which OS objects were waiting on them. Naming a
/// subset we cannot compute would drop waiters silently.
///
/// `hHWQueue` stays NULL and is read only when
/// `DXGK_VIDSCHCAPS::OptimizedNativeFenceSignaledInterrupt` is TRUE, which this
/// generation keeps 0.
///
/// Callable at <= DIRQL.
///
/// # Safety
/// `dxgkrnl` must be the live callback table of the adapter being reported on.
pub(crate) unsafe fn signal_native_fence_signaled(dxgkrnl: &DXGKRNL_INTERFACE) -> NTSTATUS {
    // SAFETY: an all-zero DXGKARGCB_NOTIFY_INTERRUPT_DATA is a valid packet; the
    // type and its arm are written before it is handed over.
    let mut interrupt = unsafe { core::mem::zeroed::<DXGKARGCB_NOTIFY_INTERRUPT_DATA>() };
    interrupt.InterruptType = _DXGK_INTERRUPT_TYPE::DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED;
    // SAFETY: bindgen lowered the per-type union to __BindgenUnionField
    // accessors; NativeFenceSignaled is the arm for this interrupt type.
    let arm = unsafe { interrupt.__bindgen_anon_1.NativeFenceSignaled.as_mut() };
    arm.NodeOrdinal = 0;
    arm.EngineOrdinal = 0;
    arm.SignaledNativeFenceCount = 0;
    arm.pSignaledNativeFenceArray = core::ptr::null_mut();
    arm.hHWQueue = core::ptr::null_mut();

    let ctx = NotifyCtx {
        dxgkrnl: dxgkrnl as *const DXGKRNL_INTERFACE,
        interrupt: &mut interrupt as *mut DXGKARGCB_NOTIFY_INTERRUPT_DATA,
    };
    let Some(sync) = dxgkrnl.DxgkCbSynchronizeExecution else {
        NF_INT_FAILED.fetch_add(1, Ordering::Relaxed);
        return STATUS_DEVICE_NOT_READY;
    };
    let mut ret: BOOLEAN = 0;
    // SAFETY: live DeviceHandle; the routine and its context outlive the
    // synchronous call.
    let status = unsafe {
        sync(
            dxgkrnl.DeviceHandle,
            Some(notify_routine),
            &ctx as *const _ as *mut c_void,
            0,
            &mut ret,
        )
    };
    if status != STATUS_SUCCESS {
        NF_INT_FAILED.fetch_add(1, Ordering::Relaxed);
        return status;
    }
    if ret == 0 {
        NF_INT_FAILED.fetch_add(1, Ordering::Relaxed);
        return STATUS_DEVICE_NOT_READY;
    }
    NF_INT_SIGNALED.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// Whether any live native-fence object exists that a signalled-value
/// notification could unblock.
///
/// The DPC calls this before spending a `DxgkCbSynchronizeExecution` round trip.
/// While the surface is unadvertised this is always false, so the interrupt path
/// costs one relaxed load.
pub(crate) fn has_live_fences() -> bool {
    LIVE_GLOBAL.load(Ordering::Acquire) != 0
}

/// The counter names, as one list, so the collision proof and the writer cannot
/// drift apart.
const COUNTER_NAMES: [&[u8]; 21] = [
    b"NfCreateOk",
    b"NfCreateRej",
    b"NfOpenOk",
    b"NfOpenRej",
    b"NfCloseOk",
    b"NfDestroyOk",
    b"NfTeardnRej",
    b"NfPddRej",
    b"NfStaleEpoch",
    b"NfNotAdmit",
    b"NfCapsOk",
    b"NfCapsRej",
    b"NfMonUpd",
    b"NfCurUpd",
    b"NfUpdRej",
    b"NfUpdBack",
    b"NfIntSig",
    b"NfIntFail",
    b"NfEpochBump",
    b"NfLiveGlobal",
    b"NfLiveLocal",
];

/// Mirror the native-fence counters into the driver service key.
///
/// PASSIVE_LEVEL only — `diag::record_named_bytes` is a synchronous
/// `RtlWriteRegistryValue`.
///
/// CROSS-LANE: `device.rs::dxgkddi_destroy_device` already calls the sibling
/// `diag_dump_*_atomics` helpers and should call this one too. Until it does,
/// [`free_global`] publishes when the last fence on the adapter goes away, which
/// is bounded (one burst per population drain) rather than per operation.
pub fn diag_dump_native_fence_atomics() {
    let values: [u32; 21] = [
        NF_CREATE_OK.load(Ordering::Relaxed),
        NF_CREATE_REJ.load(Ordering::Relaxed),
        NF_OPEN_OK.load(Ordering::Relaxed),
        NF_OPEN_REJ.load(Ordering::Relaxed),
        NF_CLOSE_OK.load(Ordering::Relaxed),
        NF_DESTROY_OK.load(Ordering::Relaxed),
        NF_TEARDOWN_REJ.load(Ordering::Relaxed),
        NF_PDD_REJ.load(Ordering::Relaxed),
        NF_STALE_EPOCH.load(Ordering::Relaxed),
        NF_NOT_ADMITTED.load(Ordering::Relaxed),
        NF_CAPS_OK.load(Ordering::Relaxed),
        NF_CAPS_REJ.load(Ordering::Relaxed),
        NF_MON_UPD.load(Ordering::Relaxed),
        NF_CUR_UPD.load(Ordering::Relaxed),
        NF_UPD_REJ.load(Ordering::Relaxed),
        NF_UPD_BACKWARD.load(Ordering::Relaxed),
        NF_INT_SIGNALED.load(Ordering::Relaxed),
        NF_INT_FAILED.load(Ordering::Relaxed),
        NF_EPOCH_BUMPS.load(Ordering::Relaxed),
        LIVE_GLOBAL.load(Ordering::Relaxed),
        LIVE_LOCAL.load(Ordering::Relaxed),
    ];
    let mut i = 0;
    while i < COUNTER_NAMES.len() {
        crate::diag::record_named_bytes(COUNTER_NAMES[i], values[i]);
        i += 1;
    }
}

/// Compile-time proof that no counter name can be truncated into another's.
///
/// `diag::record_named_bytes` clamps silently at `MAX_CONFIG_NAME`, so two names
/// sharing a 14-byte prefix would MERGE into one registry value — a refusal
/// counter reading someone else's number. Same guard `diag::FaultCounter` uses.
const _: () = {
    let mut i = 0;
    while i < COUNTER_NAMES.len() {
        assert!(
            COUNTER_NAMES[i].len() <= crate::diag::MAX_CONFIG_NAME,
            "native-fence counter name exceeds MAX_CONFIG_NAME and would merge with another"
        );
        i += 1;
    }
};
