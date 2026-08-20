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
//! bounds, handle-refusal transitions, epoch validity, and admission — is a
//! pure function in `helios_kmd_logic::native_fence_lifecycle` with host tests.
//! This file does the pointer work and the atomics.
//!
//! # Activated only by the WDDM surface
//!
//! Every advertisement here is gated on [`NATIVE_FENCE_ADVERTISED`], derived
//! solely from `ddi::wddm_surface::SURFACE`. The local WDDM 3.2 package therefore
//! advertises the six registered callbacks atomically with D2 ownership; there
//! is no independent native-fence activation switch. This source activation is
//! not deployment, cold-DWM admission, or runtime-correctness evidence.
//!
//! # Handle lifetime (`NF-UAF-1`)
//!
//! Microsoft's native-fence contract says dxgkrnl retains the global handle
//! while any local reference exists and calls Close for the final local before
//! Destroy. An in-progress open locates that same kernel object before this DDI,
//! so the OS-owned reference keeps this handle allocation live. See
//! <https://learn.microsoft.com/windows-hardware/drivers/display/native-gpu-fence-objects>.
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
use alloc::sync::Arc;

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

/// Exact GPUVA range in which this GpuMmu implementation can address the two
/// 64-bit native-fence values.
pub(crate) const NATIVE_FENCE_MINIMUM_ADDRESS: u64 = 0;
pub(crate) const NATIVE_FENCE_MAXIMUM_ADDRESS: u64 =
    (1u64 << crate::ddi::gpummu::VIRTUAL_ADDRESS_BIT_COUNT) - 1;

fn fence_gpuva_is_supported(address: u64) -> bool {
    address != 0
        && address % align_of::<u64>() as u64 == 0
        && address
            .checked_add(size_of::<u64>() as u64 - 1)
            .is_some_and(|last| last <= NATIVE_FENCE_MAXIMUM_ADDRESS)
}

/// The `DXGK_VIDSCHCAPS` bits this module contributes, as one value.
///
/// Zero while the surface is not advertised, so the caps word cannot claim a
/// native-fence capability the reported WDDM level does not support.
/// Compile-time proof that the only contributed bit cannot imply either
/// unsupported scheduler capability.
const _: () = assert!(
    VIDSCHCAPS_NATIVE_GPU_FENCE
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
const _: () = assert!(
    nf::NATIVE_FENCE_TYPE_DEFAULT
        == crate::dxgk::_D3DDDI_NATIVEFENCE_TYPE::D3DDDI_NATIVEFENCE_TYPE_DEFAULT as u32
);
const _: () = assert!(
    nf::NATIVE_FENCE_TYPE_INTRA_GPU
        == crate::dxgk::_D3DDDI_NATIVEFENCE_TYPE::D3DDDI_NATIVEFENCE_TYPE_INTRA_GPU as u32
);

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
pub static NF_FEATURE_REJ: AtomicU32 = AtomicU32::new(0);
pub static NF_BUFFER_REJ: AtomicU32 = AtomicU32::new(0);
pub static NF_CAPS_SIZE_REJ: AtomicU32 = AtomicU32::new(0);
pub static NF_MISSING_LUID: AtomicU32 = AtomicU32::new(0);
pub static NF_FOREIGN_ADAPTER: AtomicU32 = AtomicU32::new(0);
pub static NF_BAD_HANDLE: AtomicU32 = AtomicU32::new(0);
pub static NF_STALE_GENERATION: AtomicU32 = AtomicU32::new(0);
pub static NF_FLAGS_REJ: AtomicU32 = AtomicU32::new(0);
pub static NF_COUNT_OVERFLOW: AtomicU32 = AtomicU32::new(0);
pub static NF_PREFLIGHT_REJ: AtomicU32 = AtomicU32::new(0);
pub static NF_LIFECYCLE_REJ: AtomicU32 = AtomicU32::new(0);
pub static NF_EPOCH_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
pub static NF_OBJECT_GENERATION_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
pub static NF_INT_NO_EDGE: AtomicU32 = AtomicU32::new(0);

const FEATURE_BITS: u32 = 3;
const FEATURE_STATE_MASK: u64 = (1 << FEATURE_BITS) - 1;
const FEATURE_STOPPED: u64 = 0;
const FEATURE_UNKNOWN: u64 = 1;
const FEATURE_QUERYING: u64 = 2;
const FEATURE_ENABLED: u64 = 3;
const FEATURE_DECLINED: u64 = 4;
const FEATURE_POISONED: u64 = 5;
const MAX_ADAPTER_GENERATION: u64 = u64::MAX >> FEATURE_BITS;

const LIFECYCLE_STOPPED: u32 = 0;
const LIFECYCLE_ACTIVE: u32 = 1;
const LIFECYCLE_RESETTING: u32 = 2;
const LIFECYCLE_POISONED: u32 = 3;

const fn feature_word(generation: u64, state: u64) -> u64 {
    (generation << FEATURE_BITS) | state
}

const fn feature_generation(word: u64) -> u64 {
    word >> FEATURE_BITS
}

const fn feature_state(word: u64) -> u64 {
    word & FEATURE_STATE_MASK
}

/// Stable native-fence authority owned by one `AdapterContext`.
pub(crate) struct NativeFenceAdapterState {
    feature: AtomicU64,
    luid: AtomicI64,
    lifecycle: AtomicU32,
    epoch: AtomicU64,
    object_generation: AtomicU64,
    live_global: AtomicU32,
    live_local: AtomicU32,
    active_monitored: AtomicU32,
}

impl NativeFenceAdapterState {
    pub(crate) const fn new() -> Self {
        Self {
            feature: AtomicU64::new(feature_word(0, FEATURE_STOPPED)),
            luid: AtomicI64::new(0),
            lifecycle: AtomicU32::new(LIFECYCLE_STOPPED),
            epoch: AtomicU64::new(1),
            object_generation: AtomicU64::new(0),
            live_global: AtomicU32::new(0),
            live_local: AtomicU32::new(0),
            active_monitored: AtomicU32::new(0),
        }
    }

    fn current_generation(&self) -> u64 {
        feature_generation(self.feature.load(Ordering::Acquire))
    }

    fn feature_enabled(&self) -> bool {
        feature_state(self.feature.load(Ordering::Acquire)) == FEATURE_ENABLED
    }

    fn lifecycle_active(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == LIFECYCLE_ACTIVE
    }

    fn reserve_object_generation(&self) -> Option<u64> {
        self.object_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
                nf::next_object_generation(previous)
            })
            .ok()
            .and_then(nf::next_object_generation)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum NativeFenceInvalidation {
    Reset,
    StopOrRemove,
}

#[derive(Clone, Copy)]
struct AdapterIdentity {
    generation: u64,
    epoch: u64,
    luid: i64,
}

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
/// `ddi/lifecycle.rs::dxgkddi_start_device` is the sole publisher.
pub(crate) fn publish_adapter_luid(adapter: &AdapterContext, luid: LUID) {
    let state = adapter.native_fence.as_ref();
    state.lifecycle.store(LIFECYCLE_STOPPED, Ordering::Release);
    let generation = feature_generation(state.feature.load(Ordering::Acquire));
    let Some(next) = nf::next_epoch(generation, MAX_ADAPTER_GENERATION) else {
        state.feature.store(
            feature_word(generation, FEATURE_POISONED),
            Ordering::Release,
        );
        state.lifecycle.store(LIFECYCLE_POISONED, Ordering::Release);
        NF_EPOCH_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let exact = ((luid.HighPart as i64) << 32) | luid.LowPart as i64;
    state.luid.store(exact, Ordering::Release);
    state
        .feature
        .store(feature_word(next, FEATURE_UNKNOWN), Ordering::Release);
    state.lifecycle.store(LIFECYCLE_ACTIVE, Ordering::Release);
}

fn admitted_identity(state: &NativeFenceAdapterState) -> Option<AdapterIdentity> {
    if !state.lifecycle_active() {
        NF_LIFECYCLE_REJ.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let before = state.feature.load(Ordering::Acquire);
    let generation = feature_generation(before);
    if generation == 0 {
        NF_MISSING_LUID.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if feature_state(before) != FEATURE_ENABLED {
        return None;
    }
    let identity = AdapterIdentity {
        generation,
        epoch: state.epoch.load(Ordering::Acquire),
        luid: state.luid.load(Ordering::Acquire),
    };
    if state.feature.load(Ordering::Acquire) != before || !state.lifecycle_active() {
        NF_STALE_GENERATION.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some(identity)
}

/// Return the exact StartDevice identity for the current adapter lifetime.
///
/// Unlike [`admitted_identity`], this does not require native-fence feature
/// admission.  The UMD-private bootstrap query runs before either UMD creates
/// its direct translator, and its identity is an adapter-lifecycle fact rather
/// than a native-fence capability.  The before/after snapshot makes a
/// concurrent stop or new StartDevice fail closed.
pub(crate) fn lifecycle_identity(adapter: &AdapterContext) -> Option<(u64, i64)> {
    let state = adapter.native_fence.as_ref();
    if !state.lifecycle_active() {
        return None;
    }
    let before = state.feature.load(Ordering::Acquire);
    let generation = feature_generation(before);
    let luid = state.luid.load(Ordering::Acquire);
    if generation == 0 || luid == 0 {
        return None;
    }
    if state.feature.load(Ordering::Acquire) != before || !state.lifecycle_active() {
        return None;
    }
    Some((generation, luid))
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
/// Reset, stop, remove, and skipped-stop teardown all call this before the
/// paired allocation invalidation and before device-lost/transport wakeup.
pub(crate) fn invalidate_all(adapter: &AdapterContext, boundary: NativeFenceInvalidation) {
    let state = adapter.native_fence.as_ref();
    state.lifecycle.store(
        match boundary {
            NativeFenceInvalidation::Reset => LIFECYCLE_RESETTING,
            NativeFenceInvalidation::StopOrRemove => LIFECYCLE_STOPPED,
        },
        Ordering::Release,
    );
    let advanced = state
        .epoch
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
            nf::next_epoch(previous, u64::MAX)
        })
        .is_ok();
    if advanced {
        state.active_monitored.store(0, Ordering::Release);
        NF_EPOCH_BUMPS.fetch_add(1, Ordering::Relaxed);
    } else {
        state.lifecycle.store(LIFECYCLE_POISONED, Ordering::Release);
        NF_EPOCH_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn resume_after_reset(adapter: &AdapterContext) {
    let _ = adapter.native_fence.lifecycle.compare_exchange(
        LIFECYCLE_RESETTING,
        LIFECYCLE_ACTIVE,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
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
    if !NATIVE_FENCE_ADVERTISED
        || !crate::virtio::KMD_D2_OWNER_ENABLED
        || !adapter.native_fence.lifecycle_active()
    {
        return false;
    }
    let state = adapter.native_fence.as_ref();
    let observed = state.feature.load(Ordering::Acquire);
    match feature_state(observed) {
        FEATURE_ENABLED => return true,
        FEATURE_DECLINED | FEATURE_STOPPED | FEATURE_POISONED => return false,
        FEATURE_QUERYING => {
            let _ = state.feature.compare_exchange(
                observed,
                feature_word(feature_generation(observed), FEATURE_DECLINED),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            NF_FEATURE_REJ.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        _ => {}
    }
    let querying = feature_word(feature_generation(observed), FEATURE_QUERYING);
    if state
        .feature
        .compare_exchange(observed, querying, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        NF_FEATURE_REJ.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let enabled = unsafe { query_feature_support(adapter) };
    let final_state = if enabled {
        FEATURE_ENABLED
    } else {
        FEATURE_DECLINED
    };
    let published = state
        .feature
        .compare_exchange(
            querying,
            feature_word(feature_generation(observed), final_state),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    if !enabled || !published {
        NF_FEATURE_REJ.fetch_add(1, Ordering::Relaxed);
    }
    enabled && published
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
    status == STATUS_SUCCESS
        && args.DeviceHandle == dxgkrnl.DeviceHandle
        && args.FeatureId as u32 == _DXGK_FEATURE_ID::DXGK_FEATURE_NATIVE_FENCE as u32
        && args.DriverSupportState == DXGK_FEATURE_SUPPORT_STABLE_VALUE
        && args.Enabled == 1
}

/// Whether the whole §10.2 native-fence admission conjunction holds.
fn native_fence_admitted(state: &NativeFenceAdapterState) -> bool {
    nf::surface_is_admitted(
        NATIVE_FENCE_ADVERTISED,
        crate::virtio::KMD_D2_OWNER_ENABLED,
        state.feature_enabled(),
        state.current_generation() != 0,
        state.lifecycle_active(),
    )
}

pub(crate) unsafe fn vidschcaps_native_fence_bits(adapter: &AdapterContext) -> u32 {
    if unsafe { ensure_feature_admitted(adapter) }
        && native_fence_admitted(adapter.native_fence.as_ref())
    {
        VIDSCHCAPS_NATIVE_GPU_FENCE
    } else {
        0
    }
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
/// Freed, or claimed for freeing by exactly one thread.
const STATE_DEAD: u32 = 2;

/// One created native-fence object. Its own address is the driver's
/// `hGlobalNativeFence`.
#[repr(C)]
struct GlobalFenceObject {
    /// [`GLOBAL_MAGIC`]. First field so a mistyped handle is caught by one read.
    magic: u32,
    /// Strong reference to the exact adapter authority captured at create.
    /// It can outlive `AdapterContext` after RemoveDevice, so late OS-owned
    /// handle teardown never dereferences freed adapter storage.
    authority: Arc<NativeFenceAdapterState>,
    adapter_generation: u64,
    adapter_luid: i64,
    /// The adapter epoch this object was minted under.
    epoch: u64,
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
    /// `STATE_LIVE` / `STATE_DEAD`. The compare-exchange into Dead is the
    /// exactly-once claim on freeing the object.
    state: AtomicU32,
    /// Last value published through `DxgkDdiUpdateCurrentValuesFromCpu`, for the
    /// forward-progress diagnostic only.
    last_current_value: AtomicU64,
    /// Last value published through `DxgkDdiUpdateMonitoredValues`, likewise.
    last_monitored_value: AtomicU64,
    /// Whether this object currently contributes to `active_monitored`.
    monitored_active: AtomicU32,
}

/// One opened native-fence object. Its own address is the driver's
/// `hLocalNativeFence`.
#[repr(C)]
struct LocalFenceObject {
    /// [`LOCAL_MAGIC`].
    magic: u32,
    adapter_generation: u64,
    object_generation: u64,
    epoch: u64,
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
unsafe fn local_from_handle<'a>(handle: HANDLE) -> Option<&'a LocalFenceObject> {
    let p = handle as *const LocalFenceObject;
    if p.is_null() || (p as usize) % align_of::<LocalFenceObject>() != 0 {
        return None;
    }
    // SAFETY: per this function's contract the pointer is one we allocated and
    // have not yet freed.
    let local = unsafe { &*p };
    (local.magic == LOCAL_MAGIC).then_some(local)
}

/// Snapshot a global object into the `kmd_logic` model so the pure rules can
/// judge it.
fn model_of(global: &GlobalFenceObject) -> nf::GlobalFence {
    nf::GlobalFence {
        state: match global.state.load(Ordering::Acquire) {
            STATE_LIVE => nf::FenceState::Live,
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

fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

fn note_bad_handle() {
    NF_BAD_HANDLE.fetch_add(1, Ordering::Relaxed);
}

fn validate_global_for_use(
    global: &GlobalFenceObject,
    expected_authority: Option<&NativeFenceAdapterState>,
) -> Result<AdapterIdentity, NTSTATUS> {
    let authority = global.authority.as_ref();
    if let Some(expected) = expected_authority {
        if !core::ptr::eq(authority, expected) {
            NF_FOREIGN_ADAPTER.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_HANDLE);
        }
    }
    if !native_fence_admitted(authority) {
        NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_DEVICE_NOT_READY);
    }
    let Some(identity) = admitted_identity(authority) else {
        return Err(STATUS_DEVICE_NOT_READY);
    };
    if global.adapter_generation != identity.generation || global.adapter_luid != identity.luid {
        NF_STALE_GENERATION.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_DEVICE_REMOVED);
    }
    if !nf::epoch_is_current(global.epoch, identity.epoch) {
        NF_STALE_EPOCH.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_DEVICE_REMOVED);
    }
    if global.state.load(Ordering::Acquire) != STATE_LIVE {
        note_bad_handle();
        return Err(STATUS_INVALID_HANDLE);
    }
    Ok(identity)
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
    if h_adapter.is_null()
        || !(h_adapter as *const AdapterContext).is_aligned()
        || p_create.is_null()
        || !p_create.is_aligned()
    {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let Some(identity) = admitted_identity_for_ddi(adapter, &NF_CREATE_REJ) else {
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
        identity.luid,
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
        NF_FLAGS_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    if args.hGlobalNativeFence.is_null()
        || !all_zero(&args.Reserved)
        || args.CurrentValueSystemProcessGpuVa != 0
        || args.MonitoredValueSystemProcessGpuVa != 0
    {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }

    let Some(object_generation) = adapter.native_fence.reserve_object_generation() else {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        NF_OBJECT_GENERATION_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
        return STATUS_INTEGER_OVERFLOW;
    };
    if !admit(&adapter.native_fence.live_global, nf::MAX_LIVE_GLOBAL) {
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    let object = Box::new(GlobalFenceObject {
        magic: GLOBAL_MAGIC,
        authority: Arc::clone(&adapter.native_fence),
        adapter_generation: identity.generation,
        adapter_luid: identity.luid,
        epoch: identity.epoch,
        object_generation,
        os_handle: args.hGlobalNativeFence,
        native_type,
        flags: parsed.flags,
        local_refs: AtomicU32::new(0),
        state: AtomicU32::new(STATE_LIVE),
        last_current_value: AtomicU64::new(0),
        last_monitored_value: AtomicU64::new(u64::MAX),
        monitored_active: AtomicU32::new(0),
    });

    if let Err(status) = validate_global_for_use(&object, Some(adapter.native_fence.as_ref())) {
        let _ = retire(&adapter.native_fence.live_global);
        NF_CREATE_REJ.fetch_add(1, Ordering::Relaxed);
        return status;
    }

    // The driver handle IS the object address: §12.1:3148-3150 makes identity
    // the OS-delivered handle pair, and the KMD half of that pair is this
    // pointer. Nothing else maps a handle to an object.
    args.hGlobalNativeFence = Box::into_raw(object) as HANDLE;
    args.pPrivateDriverData = nf::encode(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        object_generation,
        native_type,
        parsed.flags,
        identity.luid,
    );
    NF_CREATE_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// The §10.2 admission conjunction plus the LUID, as one gate.
///
/// Increments `rejection_counter` and `NfNotAdmit` on refusal so the caller's
/// error return is the only thing left to choose.
fn admitted_identity_for_ddi(
    adapter: &AdapterContext,
    rejection_counter: &AtomicU32,
) -> Option<AdapterIdentity> {
    let state = adapter.native_fence.as_ref();
    if !native_fence_admitted(state) {
        rejection_counter.fetch_add(1, Ordering::Relaxed);
        NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let identity = admitted_identity(state);
    if identity.is_none() {
        rejection_counter.fetch_add(1, Ordering::Relaxed);
        NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
    }
    identity
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
    if h_adapter.is_null()
        || !(h_adapter as *const AdapterContext).is_aligned()
        || p_open.is_null()
        || !p_open.is_aligned()
    {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    if admitted_identity_for_ddi(adapter, &NF_OPEN_REJ).is_none() {
        return STATUS_NOT_SUPPORTED;
    }
    // SAFETY: non-null per the check above.
    let args = unsafe { &mut *p_open };

    // SAFETY: dxgkrnl returns only driver handles this module assigned and has
    // not freed.
    let Some(global) = (unsafe { global_from_handle(args.hGlobalNativeFence) }) else {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        note_bad_handle();
        return STATUS_INVALID_HANDLE;
    };

    let identity = match validate_global_for_use(global, Some(adapter.native_fence.as_ref())) {
        Ok(identity) => identity,
        Err(status) => {
            NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
            return status;
        }
    };

    let pdd: [u8; nf::HNF1_SIZE] = args.pPrivateDriverData;
    if let Err(reject) = nf::validate_open(
        &pdd,
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        global.adapter_luid,
        global.native_type,
        global.flags,
    ) {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        note_pdd_reject(reject);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view; every
    // bit is reserved in this revision.
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FLAGS_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    let device_adapter = unsafe { crate::device::DeviceHandleRef::from_raw(args.hDevice) }
        .and_then(|device| device.adapter());
    if device_adapter.is_none_or(|device_adapter| !core::ptr::eq(device_adapter, adapter)) {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FOREIGN_ADAPTER.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_HANDLE;
    }
    if args.hLocalNativeFence.is_null() || !all_zero(&args.Reserved) {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    if !fence_gpuva_is_supported(args.CurrentValueGpuVa)
        || !fence_gpuva_is_supported(args.MonitoredValueGpuVa)
        || args.CurrentValueGpuVa == args.MonitoredValueGpuVa
    {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // A non-shared object has no legal opener. §12.1 item 7 additionally
    // rejects LDA and cross-adapter, which is why HNF1 has no cross-adapter bit
    // to admit here.
    if global.flags & nf::HNF1_FLAG_SHARED == 0 {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_ACCESS_DENIED;
    }

    match nf::open_local(model_of(global), identity.epoch) {
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

    if !admit(&adapter.native_fence.live_local, nf::MAX_LIVE_LOCAL) {
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    let local = Box::new(LocalFenceObject {
        magic: LOCAL_MAGIC,
        adapter_generation: global.adapter_generation,
        object_generation: global.object_generation,
        epoch: global.epoch,
        global: global as *const GlobalFenceObject as *mut GlobalFenceObject,
        os_handle: args.hLocalNativeFence,
    });
    if let Err(status) = validate_global_for_use(global, Some(adapter.native_fence.as_ref())) {
        let _ = retire(&adapter.native_fence.live_local);
        NF_OPEN_REJ.fetch_add(1, Ordering::Relaxed);
        return status;
    }
    // Dxgkrnl keeps the global object referenced across this callback and calls
    // Destroy only after the final local Close (Microsoft native-fence contract).
    global.local_refs.fetch_add(1, Ordering::AcqRel);
    args.hLocalNativeFence = Box::into_raw(local) as HANDLE;
    // §12.1 item 2: the KMD writes the record back and the opening UMD is the
    // party that validates package/LUID/type against it.
    args.pPrivateDriverData = nf::encode(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        global.object_generation,
        global.native_type,
        global.flags,
        identity.luid,
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
    if h_adapter.is_null()
        || !(h_adapter as *const AdapterContext).is_aligned()
        || p_close.is_null()
        || !p_close.is_aligned()
    {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    // SAFETY: non-null per the check above.
    let args = unsafe { &mut *p_close };
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 || !all_zero(&args.Reserved) {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FLAGS_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let handle = args.hLocalNativeFence;
    // SAFETY: dxgkrnl returns only driver handles this module assigned.
    let Some(local_ref) = (unsafe { local_from_handle(handle) }) else {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        note_bad_handle();
        return STATUS_INVALID_HANDLE;
    };
    let Some(global_ref) = (unsafe { global_from_handle(local_ref.global.cast()) }) else {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        note_bad_handle();
        return STATUS_INVALID_HANDLE;
    };
    if !core::ptr::eq(global_ref.authority.as_ref(), adapter.native_fence.as_ref()) {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FOREIGN_ADAPTER.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_HANDLE;
    }
    if global_ref.adapter_generation != local_ref.adapter_generation
        || global_ref.object_generation != local_ref.object_generation
        || global_ref.epoch != local_ref.epoch
    {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_STALE_GENERATION.fetch_add(1, Ordering::Relaxed);
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
    let _ = retire(&adapter.native_fence.live_local);
    drop(local);
    NF_CLOSE_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// `DxgkDdiDestroyNativeFence` — §12.1 item 6, the global half. PASSIVE_LEVEL.
///
/// Note the WDK signature: it takes **no** `hAdapter`.
///
/// Dxgkrnl's documented native-fence reference contract calls this only after
/// the final local Close. If that invariant is violated, the driver refuses and
/// deliberately leaks the bounded object rather than free storage a local may
/// still name.
///
/// # Safety
/// Called by dxgkrnl with a `hGlobalNativeFence` this module returned from
/// [`dxgkddi_create_native_fence`], exactly once per successful create.
pub unsafe extern "C" fn dxgkddi_destroy_native_fence(
    p_destroy: *mut DXGKARG_DESTROYNATIVEFENCE,
) -> NTSTATUS {
    if p_destroy.is_null() || !p_destroy.is_aligned() {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above.
    let args = unsafe { &mut *p_destroy };
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 || !all_zero(&args.Reserved) {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FLAGS_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let handle = args.hGlobalNativeFence;
    // SAFETY: dxgkrnl returns only driver handles this module assigned.
    let Some(global) = (unsafe { global_from_handle(handle) }) else {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        note_bad_handle();
        return STATUS_INVALID_HANDLE;
    };
    if global.adapter_generation == 0 || global.object_generation == 0 {
        NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
        NF_STALE_GENERATION.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_HANDLE;
    }

    match nf::destroy_global(model_of(global)) {
        Ok(_) => {
            // Claim the free. Only the thread that wins LIVE -> DEAD frees.
            if global
                .state
                .compare_exchange(STATE_LIVE, STATE_DEAD, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
                return STATUS_INVALID_DEVICE_REQUEST;
            }
            args.hGlobalNativeFence = core::ptr::null_mut();
            free_global(handle as *mut GlobalFenceObject);
            NF_DESTROY_OK.fetch_add(1, Ordering::Relaxed);
            STATUS_SUCCESS
        }
        Err(nf::Refusal::LocalReferencesOutstanding) => {
            // This contradicts dxgkrnl's ordering. Leave the global live and
            // leak it if necessary; freeing or parking it would make a still-
            // owned local handle unsafe.
            NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
            STATUS_INVALID_DEVICE_REQUEST
        }
        Err(_) => {
            NF_TEARDOWN_REJ.fetch_add(1, Ordering::Relaxed);
            STATUS_INVALID_DEVICE_REQUEST
        }
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
    // `LIVE -> DEAD` transition and no local reference remains, so this
    // runs exactly once and no other reference to the object is live.
    let object = unsafe { Box::from_raw(p) };
    let authority = Arc::clone(&object.authority);
    if object.monitored_active.swap(0, Ordering::AcqRel) != 0
        && nf::epoch_is_current(object.epoch, authority.epoch.load(Ordering::Acquire))
    {
        let _ = retire(&authority.active_monitored);
    }
    drop(object);
    let _ = retire(&authority.live_global);
    // Teardown is the bounded, naturally rare moment to publish. Publishing per
    // operation would turn a fence-heavy frame into a registry write storm.
    if authority.live_global.load(Ordering::Acquire) == 0 {
        diag_dump_native_fence_atomics(authority.as_ref());
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
    if p_args.is_null() || !p_args.is_aligned() {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: non-null per the check above; the struct is read-only for us.
    let args = unsafe { &*p_args };
    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view; every
    // bit is reserved in this revision.
    if unsafe { args.Flags.__bindgen_anon_1.Value } != 0 || !all_zero(&args.Reserved) {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FLAGS_REJ.fetch_add(1, Ordering::Relaxed);
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
    if p_args.is_null() || !p_args.is_aligned() {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
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
    if flags & !(ALWAYS_SIGNALED | NOTIFICATION_ONLY) != 0 || !all_zero(&args.Reserved) {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        NF_FLAGS_REJ.fetch_add(1, Ordering::Relaxed);
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
    if !nf::update_count_is_bounded(count) {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        NF_COUNT_OVERFLOW.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    if count == 0 {
        return STATUS_SUCCESS;
    }
    let count = count as usize;
    let extents_fit = count
        .checked_mul(size_of::<HANDLE>())
        .is_some_and(|bytes| bytes <= isize::MAX as usize)
        && count
            .checked_mul(size_of::<u64>())
            .is_some_and(|bytes| bytes <= isize::MAX as usize)
        && count
            .checked_mul(size_of::<*mut c_void>())
            .is_some_and(|bytes| bytes <= isize::MAX as usize);
    if !extents_fit
        || handles.is_null()
        || !handles.is_aligned()
        || values.is_null()
        || !values.is_aligned()
        || storage.is_null()
        || !storage.is_aligned()
    {
        NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }

    // Pass one validates the complete batch; pass two is the only mutation pass.
    let mut i = 0usize;
    while i < count {
        // SAFETY: `i < count` and all three arrays hold `count` entries.
        let (handle, _, slot) = unsafe {
            (
                handles.add(i).read(),
                values.add(i).read(),
                storage.add(i).read(),
            )
        };
        // SAFETY: dxgkrnl returns only driver handles this module assigned.
        let Some(global) = (unsafe { global_from_handle(handle) }) else {
            NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
            NF_PREFLIGHT_REJ.fetch_add(1, Ordering::Relaxed);
            note_bad_handle();
            return STATUS_INVALID_HANDLE;
        };
        if let Err(status) = validate_global_for_use(global, None) {
            NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
            NF_PREFLIGHT_REJ.fetch_add(1, Ordering::Relaxed);
            return status;
        }
        if global.object_generation == 0
            || !nf::native_type_is_documented(global.native_type)
            || global.flags & nf::HNF1_FLAGS_RESERVED_MASK != 0
            || slot.is_null()
            || !slot.cast::<u64>().is_aligned()
        {
            NF_UPD_REJ.fetch_add(1, Ordering::Relaxed);
            NF_PREFLIGHT_REJ.fetch_add(1, Ordering::Relaxed);
            NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        }
        i += 1;
    }

    i = 0;
    while i < count {
        let (handle, value, slot) = unsafe {
            (
                handles.add(i).read(),
                values.add(i).read(),
                storage.add(i).read(),
            )
        };
        let global = unsafe { &*(handle as *const GlobalFenceObject) };
        let last = if monitored {
            &global.last_monitored_value
        } else {
            &global.last_current_value
        };
        // The OS-owned mapping is the authoritative fence value. Publish it
        // before the diagnostic/population mirrors can make this update visible
        // to the completion path.
        if write_storage {
            unsafe { slot.cast::<u64>().write_volatile(value) };
        }
        let previous = last.swap(value, Ordering::AcqRel);
        if !nf::value_update_is_forward(previous, value) {
            NF_UPD_BACKWARD.fetch_add(1, Ordering::Relaxed);
        }
        if monitored {
            let active = u32::from(value != u64::MAX);
            let was_active = global.monitored_active.swap(active, Ordering::AcqRel);
            if was_active == 0 && active != 0 {
                let _ = admit(&global.authority.active_monitored, nf::MAX_LIVE_GLOBAL);
            } else if was_active != 0 && active == 0 {
                let _ = retire(&global.authority.active_monitored);
            }
        }
        i += 1;
    }
    if monitored {
        NF_MON_UPD.fetch_add(count as u32, Ordering::Relaxed);
    } else {
        NF_CUR_UPD.fetch_add(count as u32, Ordering::Relaxed);
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
    if args.pOutputData.is_null() || !(args.pOutputData as *mut DXGK_NATIVE_FENCE_CAPS).is_aligned()
    {
        NF_CAPS_REJ.fetch_add(1, Ordering::Relaxed);
        NF_BUFFER_REJ.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    if args.OutputDataSize as usize != size_of::<DXGK_NATIVE_FENCE_CAPS>() {
        NF_CAPS_REJ.fetch_add(1, Ordering::Relaxed);
        NF_CAPS_SIZE_REJ.fetch_add(1, Ordering::Relaxed);
        return if (args.OutputDataSize as usize) < size_of::<DXGK_NATIVE_FENCE_CAPS>() {
            STATUS_BUFFER_TOO_SMALL
        } else {
            STATUS_INVALID_PARAMETER
        };
    }
    // SAFETY: DxgkDdiQueryAdapterInfo is documented PASSIVE_LEVEL, which is what
    // DxgkCbQueryFeatureSupport requires.
    let admitted = unsafe { ensure_feature_admitted(adapter) }
        && native_fence_admitted(adapter.native_fence.as_ref());
    if !admitted {
        NF_CAPS_REJ.fetch_add(1, Ordering::Relaxed);
        NF_NOT_ADMITTED.fetch_add(1, Ordering::Relaxed);
        return STATUS_NOT_SUPPORTED;
    }
    let mut caps = unsafe { core::mem::zeroed::<DXGK_NATIVE_FENCE_CAPS>() };
    caps.MonitoredValuePadding = 0;
    caps.MapToGpuSystemProcess = 0;
    caps.MinimumAddress = NATIVE_FENCE_MINIMUM_ADDRESS;
    caps.MaximumAddress = NATIVE_FENCE_MAXIMUM_ADDRESS;
    unsafe { (args.pOutputData as *mut DXGK_NATIVE_FENCE_CAPS).write(caps) };
    NF_CAPS_OK.fetch_add(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

// ── DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED ─────────────────────────────────────

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
pub(crate) unsafe fn signal_native_fence_signaled(
    adapter: &AdapterContext,
    dxgkrnl: &DXGKRNL_INTERFACE,
) -> NTSTATUS {
    if !has_possible_progress_edge(adapter) {
        NF_INT_NO_EDGE.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_DEVICE_REQUEST;
    }
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

    let status = unsafe { super::submit_command::notify_at_dirql(dxgkrnl, &mut interrupt, false) };
    if status != STATUS_SUCCESS {
        NF_INT_FAILED.fetch_add(1, Ordering::Relaxed);
        return status;
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
pub(crate) fn has_possible_progress_edge(adapter: &AdapterContext) -> bool {
    let state = adapter.native_fence.as_ref();
    native_fence_admitted(state)
        && state.live_global.load(Ordering::Acquire) != 0
        && state.active_monitored.load(Ordering::Acquire) != 0
}

/// The counter names, as one list, so the collision proof and the writer cannot
/// drift apart.
const COUNTER_NAMES: [&[u8]; 35] = [
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
    b"NfFeatRej",
    b"NfBufRej",
    b"NfCapsSize",
    b"NfNoLuid",
    b"NfForeign",
    b"NfBadHandle",
    b"NfStaleGen",
    b"NfFlagRej",
    b"NfCountOvf",
    b"NfPreflight",
    b"NfLifecycle",
    b"NfEpochExh",
    b"NfObjGenExh",
    b"NfIntNoEdge",
    b"NfLiveGlobal",
    b"NfLiveLocal",
];

/// Mirror the native-fence counters into the driver service key.
///
/// PASSIVE_LEVEL only — `diag::record_named_bytes` is a synchronous
/// `RtlWriteRegistryValue`.
///
/// Two publishers, and the FIRST one is the load-bearing one:
///
/// 1. `device.rs::dxgkddi_destroy_device` calls this unconditionally, beside the
///    sibling `diag_dump_{gpummu,engine,present}_atomics` helpers. This is the
///    only publisher that can run while the surface is `Wddm2_1GpuMmu`, because
///    dxgkrnl invokes no native-fence callback on a 2.1 adapter, so no fence is
///    ever created and (2) never fires. That call is what establishes the
///    all-zero pre-flip baseline the post-flip numbers are read against.
/// 2. [`free_global`] publishes when the last fence on the adapter goes away.
///    Bounded (one burst per population drain) rather than per operation, and
///    reachable only once the surface actually carries native fences.
pub fn diag_dump_native_fence_atomics(state: &NativeFenceAdapterState) {
    let values: [u32; 35] = [
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
        NF_FEATURE_REJ.load(Ordering::Relaxed),
        NF_BUFFER_REJ.load(Ordering::Relaxed),
        NF_CAPS_SIZE_REJ.load(Ordering::Relaxed),
        NF_MISSING_LUID.load(Ordering::Relaxed),
        NF_FOREIGN_ADAPTER.load(Ordering::Relaxed),
        NF_BAD_HANDLE.load(Ordering::Relaxed),
        NF_STALE_GENERATION.load(Ordering::Relaxed),
        NF_FLAGS_REJ.load(Ordering::Relaxed),
        NF_COUNT_OVERFLOW.load(Ordering::Relaxed),
        NF_PREFLIGHT_REJ.load(Ordering::Relaxed),
        NF_LIFECYCLE_REJ.load(Ordering::Relaxed),
        NF_EPOCH_EXHAUSTED.load(Ordering::Relaxed),
        NF_OBJECT_GENERATION_EXHAUSTED.load(Ordering::Relaxed),
        NF_INT_NO_EDGE.load(Ordering::Relaxed),
        state.live_global.load(Ordering::Relaxed),
        state.live_local.load(Ordering::Relaxed),
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
