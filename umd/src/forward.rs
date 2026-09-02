//! d3d10umddi device-funcs → D3D11 COM forwarders (pure Rust via the windows
//! crate). Each func reads its bindgen DDI arg struct, translates to a
//! windows-crate COM call on the DXVK `ID3D11Device`/`ID3D11DeviceContext`, and
//! stores the returned COM interface in the runtime-allocated DDI handle.
//!
//! DDI Usage/BindFlags/MiscFlags mirror the D3D11 API bit values (passthrough).
//! Resource/view handles store the raw COM pointer (8 bytes) in pDrvPrivate;
//! CalcPrivate*Size returns 8. Errors on VOID-returning Create* are dropped for
//! now (TODO: report via the device error callback) — a failed create leaves a
//! null handle.

// T8/R1107 commit 0: the child modules below need this file's import surface,
// and a `use` statement is not an item -- `use super::*;` in a child cannot see
// one. Re-exporting them is what lets every child declare exactly
// `use super::*;` instead of carrying its own copy of a 50-line import block
// that would then drift.
mod alloc;
mod handles;

mod bindings;
mod deferred;
mod format_caps;
mod layout;
mod pipeline;
mod present;
mod queries;
mod resource;
mod shaders;
mod state;
mod state_objects;
mod tables;
mod tiles;
mod transfer;
mod views;
mod wddm2;

pub(super) use alloc::{Hwa2CreateInput, Hwa2InputRefusal};
pub(crate) use bindings::*;
pub(crate) use deferred::*;
pub(crate) use format_caps::*;
pub(super) use handles::{Boxed, Com, ComHandle, DdiHandle, Slot};
pub(crate) use layout::*;
pub(crate) use pipeline::*;
pub(crate) use present::*;
pub(crate) use queries::*;
pub(crate) use resource::*;
pub(crate) use shaders::*;
pub(crate) use state::*;
pub(crate) use state_objects::*;
pub(crate) use tables::*;
pub(crate) use tiles::*;
pub(crate) use transfer::*;
pub(crate) use views::*;
pub(crate) use wddm2::*;
// NOT re-exported: `boxed_slot` is `pub(super)` in `handles` and its
// `BoxedHandle` bound names types (`ResourceState`, `RtvState`, `LayoutData`)
// that are private to this subtree, so a `pub(super)` re-export would leak
// them (E0446). The child modules that need it say
// `use super::handles::boxed_slot;` -- one line, and the bound stays sealed.
use handles::boxed_slot;

/// Microseconds since this process first asked. Orders the D2 present trace
/// (Flush / sync token / PresentMPO / outer batch) across dwm's threads.
pub(crate) fn trace_us() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros() as u64
}

pub(super) use core::ffi::c_void;
pub(super) use core::mem::ManuallyDrop;
pub(super) use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) use windows::core::{IUnknown, Interface, PCSTR};
pub(super) use windows::Win32::Foundation::{BOOL, RECT};
pub(super) use windows::Win32::Graphics::Direct3D::{
    D3D11_SRV_DIMENSION_BUFFER, D3D11_SRV_DIMENSION_BUFFEREX, D3D11_SRV_DIMENSION_TEXTURE1D,
    D3D11_SRV_DIMENSION_TEXTURE1DARRAY, D3D11_SRV_DIMENSION_TEXTURE2D,
    D3D11_SRV_DIMENSION_TEXTURE2DARRAY, D3D11_SRV_DIMENSION_TEXTURE2DMS,
    D3D11_SRV_DIMENSION_TEXTURE2DMSARRAY, D3D11_SRV_DIMENSION_TEXTURE3D,
    D3D11_SRV_DIMENSION_TEXTURECUBE, D3D11_SRV_DIMENSION_TEXTURECUBEARRAY,
};
pub(super) use windows::Win32::Graphics::Direct3D11::*;
pub(super) use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

// ⛔ K4 / HPS2 retirement: the create-time `HeliosWddmAllocPrivate` +
// `HeliosWddmAllocMeta` pair, the open-time `HeliosWddmOpenIdentity` restamp,
// the `HELIOS_WDDM_ALLOC_KIND_*` / `HELIOS_WDDM_ALLOC_MISC_*` vocabularies, the
// `GLOBAL_VIDMM_TRACKER` blob-flag overload and the `VIRTIO_GPU_BLOB_*` create
// words are ALL retired from this driver's allocation seam. One record crosses
// it now — `HeliosWddmAllocationDescV2` (HWA2, 168 B) — and it is built in
// `forward/alloc.rs` and read in `forward/state.rs`. Nothing here may re-import
// a `wddm_legacy` allocation symbol: `docs/retirement/K4-CONTRACT.md` §1
// (the two-stage contract) and §6 (the VidMm tracker has no successor). The
// interim §5 `resource_id` gap is closed only by the landed HRA1/A7 path: an
// opener assigns its own package token to the exact runtime allocation and the
// KMD resolves it from the allocation list. No UMD-visible resid replaces it.
pub(super) use helios_protocol::{
    HeliosAllocDescRejection, HeliosResourceAssociationV1, HeliosWddmAllocationDescV2,
    HELIOS_HWA2_BYTES, HELIOS_PACKAGE_GENERATION, HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION,
    HELIOS_RESOURCE_ASSOCIATION_BYTES, HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE,
};

pub(super) use crate::ddi;
pub(super) use crate::device_funcs::HeliosDevice;
pub(super) use crate::log_error;
pub(super) use crate::trace_line;

pub(super) type Hdevice = ddi::D3D10DDI_HDEVICE;

/// One rate-limited log site's occurrence counter.
///
/// Replaces the hand-rolled `AtomicUsize` + threshold expression that was
/// re-derived at every reference, in eleven different shapes (`n < 16`, `< 32`,
/// `< 64`, `< 128`, `< 256`, `< 512`, `< 1024`, `< 2048`, `n % 512 == 0`,
/// `n % 1024 == 0`, `(n + 1) % 512 == 0`, `(n + 1) % 2048 == 0`).
///
/// DEVIATION from the review, and the reason: it asks for the budget to live in
/// the static, "instantiated per site with the site's current numbers so no
/// site's cadence changes". That is not implementable — eleven of these statics
/// are SHARED by sites with different budgets (`SHADER_BIND_LOG_COUNT` is used
/// with both `< 128` and `< 256`, `MPO_LOG_COUNT` with `< 16`, `< 64` and
/// `< 128`, `VIEW_LOG_COUNT` with `< 128` and `< 256`, `DRAW_LOG_COUNT` with
/// `< 2048` and a `% 1024` shape). Giving each site its own counter would change
/// the cadence of every one of them, which is precisely what must not happen.
/// So the counter is shared exactly as today and the budget is a call argument.
// `LogThrottle` moved to `helios_umd_common::throttle` (`DECISIONS.md` D3b,
// stage S1) — the budget stays a call argument, exactly as described above.
// Re-exported here so all 44 call sites in this module and its submodules
// resolve unchanged.
pub(super) use helios_umd_common::throttle::LogThrottle;

// The refusal-counter MECHANISM (`DECISIONS.md` D3b, stage S2). ⛔ Only the
// mechanism is shared: the counters below are this driver's, and
// `umd12` declares its own set. `CONFORMANCE.md`'s charter reads
// `DDI refusals:` per driver.
use helios_umd_common::refusals::{self, RefusalCounter};

pub(super) static CREATE_RESOURCE_IDENTITY_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static SYNC_TOKEN_IDENTITY_LOG: LogThrottle = LogThrottle::new();
pub(super) static VIEW_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static WDDM_ALLOC_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static D3D11_1_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static COPY_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static COPY_REGION_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static MAP_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static SHADER_BIND_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static SHADER_SET_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static SRV_CREATE_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static SRV_BIND_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static DRAW_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static OM_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static UPDATE_LOG_COUNT: LogThrottle = LogThrottle::new();
/// UpdateSubresource lines the rate cap dropped. Without this the cap would
/// turn "no lines" into "nothing happened".
pub(super) static UPDATE_SUPPRESSED: AtomicUsize = AtomicUsize::new(0);
pub(super) static DISPATCH_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static HANDLE_MISS_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static UAV_BIND_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static CLEAR_RTV_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static VIEWPORT_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static SCISSOR_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static RASTER_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static IA_BIND_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static PRESENT_READBACK_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static PRESENT_FORCE_OPAQUE_LOG_COUNT: LogThrottle = LogThrottle::new();
pub(super) static PRESENT_CB_LOG_COUNT: LogThrottle = LogThrottle::new();

pub(super) use crate::format;

/// The lossy DXGI -> legacy D3DDDIFORMAT downgrade the KMD's
/// `DxgkDdiDescribeAllocation` consumes. Counted, not refused: only two DXGI
/// formats have a spelling here, the EXACT format travels beside it in
/// `HeliosWddmAllocationDescV2::dxgi_format` (HWA2 offset 48, K4 -- it was the
/// retired `HeliosWddmAllocMeta::dxgi_format`), and every consumer that needs
/// bpp/layout reads that one. It was the last silent answer in the format
/// readers.
fn dxgi_to_d3dddi_format(fmt: u32) -> u32 {
    let d3dddi = format::to_d3dddi(fmt);
    if d3dddi == format::D3DDDIFMT_UNKNOWN {
        note_ddi_refusal(&DDI_REFUSALS.alloc_meta_format_unknown);
    }
    d3dddi
}

// ⛔ K4 deleted `d3dddi_to_dxgi_format`, the reverse (D3DDDIFORMAT -> DXGI)
// translation. Its ONLY caller was `open_resource`'s fallback for a creator
// that had recorded no DXGI format — a legacy-trailer / KMD-standard-allocation
// case that assumed BGRA. HWA2 carries the EXACT `DXGI_FORMAT` at offset 48 and
// hard-fails an image kind that leaves it `UNKNOWN`, so there is no longer a
// descriptor from which the format has to be guessed. Reintroducing the
// fallback would reintroduce the collapse-everything-to-BGRA bug that made an
// A8 mask rebuild at 4 bpp.

/// Bytes per pixel of an (uncompressed) `DXGI_FORMAT`, for computing the WDDM
/// surface pitch.
fn dxgi_bytes_per_pixel(fmt: u32) -> u32 {
    format::bytes_per_pixel(fmt)
}

/// Bits per sample for the uncompressed DXGI formats that can participate in
/// D3D11 output/MSAA validation.
fn dxgi_bits_per_sample(fmt: u32) -> Option<u32> {
    format::bits_per_sample(fmt)
}

fn dxgi_output_family_bits(fmt: u32) -> Option<u32> {
    format::output_family_bits(fmt)
}

fn dxgi_output_bits_per_sample(fmt: u32, caps: u32) -> Option<u32> {
    const D3D11_FORMAT_SUPPORT_RENDER_TARGET: u32 = 0x0000_4000;
    const D3D11_FORMAT_SUPPORT_DEPTH_STENCIL: u32 = 0x0001_0000;

    if caps & (D3D11_FORMAT_SUPPORT_RENDER_TARGET | D3D11_FORMAT_SUPPORT_DEPTH_STENCIL) != 0 {
        dxgi_bits_per_sample(fmt)
    } else {
        dxgi_output_family_bits(fmt)
    }
}

fn dxgi_msaa_bits_per_sample(fmt: u32, caps: u32) -> Option<u32> {
    if format::msaa_ineligible(fmt) {
        None
    } else {
        dxgi_output_bits_per_sample(fmt, caps)
    }
}

fn dxgi_resolve_required(fmt: u32) -> bool {
    format::resolve_required(fmt)
}

fn dxgi_color_typeless_parent(fmt: u32) -> bool {
    format::color_typeless_parent(fmt)
}

fn dxgi_integer_typed_format(fmt: u32) -> bool {
    format::integer_typed(fmt)
}

pub(super) use crate::hr::{
    DXGI_ERROR_UNSUPPORTED, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY,
};
/// Lock a mutex, ignoring poison. With `panic = "abort"` in both profiles no
/// unwind can ever poison a mutex, and a DDI must never panic on lock — so the
/// poisoned arm hands back the inner guard instead of propagating.
pub(crate) fn lock_ignore_poison<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The DDI paths that refuse or silently downgrade runtime-requested work.
///
/// Each field is a legitimate runtime decision about runtime-supplied data, so
/// a *type* encoding would be cosmetic — what they lacked was any record at
/// all. Every one of them returned, dropped or downgraded without incrementing
/// anything, which violates the loud-failure invariant. R911.
///
/// The refusals themselves are unchanged: this makes them countable, not
/// different.
struct DdiRefusals {
    /// `pfnResourceReadAfterWriteHazard` for an SRV — empty body.
    srv_raw_hazard: RefusalCounter,
    /// `pfnResourceReadAfterWriteHazard` for a resource — empty body.
    resource_raw_hazard: RefusalCounter,
    /// `pfnSetTextFilterSize` — empty body.
    text_filter_size_ignored: RefusalCounter,
    /// `pfnResourceIsStagingBusy` returning 0. NOT a no-op: that is the
    /// semantic claim "never busy", which the runtime acts on.
    staging_busy_assumed_free: RefusalCounter,
    /// `pfnDiscard` with `num_rects != 0` — the partial discard is dropped.
    /// Well reasoned (upstream DXVK does the same, and forwarding partial
    /// discards as full-view discards wiped the undamaged 99% of DWM's flip
    /// backbuffer), but it was uncounted AND unlogged once the 64-line budget
    /// was spent.
    discard_partial: RefusalCounter,
    /// `pfnClearView` for a non-RTV view type — the clear is dropped. This one
    /// already logs its refusal, so it was uncounted but never silent; no
    /// second log line is added.
    clear_view_unsupported: RefusalCounter,
    /// `pfnCreateGeometryShaderWithStreamOutput` — the SO declaration is
    /// discarded and a plain GS is created. The most consequential of the nine:
    /// `SOSetTargets` then binds buffers that are never written and `DrawAuto`
    /// reads zero vertices, so the app renders nothing with nothing recording
    /// that SO capture was dropped.
    gs_so_declaration_dropped: RefusalCounter,
    /// Hull/domain shader creates taking the signature-less fallback.
    /// Expected to MOVE under 3DMark; the UB against SINT inputs noted at the
    /// fallback is NOT fixed here — it is made countable, which is the
    /// precondition for fixing it against a real workload.
    tess_sig_fallback: RefusalCounter,
    /// `create_resource` with a resource dimension outside the four we handle.
    unhandled_resource_dimension: RefusalCounter,
    /// A DXGI format with no legacy D3DDDIFORMAT spelling, stamped into the
    /// allocation descriptor as `D3DDDIFMT_UNKNOWN` (0).
    ///
    /// The tenth, added with R1010. `format::to_d3dddi` knows exactly two
    /// formats -- R8G8B8A8_UNORM and B8G8R8A8_UNORM -- and answered 0 for
    /// everything else with no log and no counter; that 0 goes straight into
    /// the descriptor's `D3DDDIFORMAT` field, which
    /// `DxgkDdiDescribeAllocation` consumes. It is a legitimate downgrade (the EXACT format travels
    /// separately in `dxgi_format`, which is what every consumer that needs
    /// bpp/layout reads), so this counts rather than refuses -- but it was the
    /// one silent path the format table's readers still had.
    ///
    /// ⚠ K4 renamed the destination, not the counter: the lossy value now lands
    /// in `HeliosWddmAllocationDescV2::d3d_ddi_format` (HWA2 offset 52) beside
    /// the exact `dxgi_format` at offset 48. The counter's NAME is the evidence
    /// contract (`DDI refusals:` lines are diffed across builds), so it keeps
    /// its spelling.
    alloc_meta_format_unknown: RefusalCounter,
    /// `maybe_log_present_readback` refusing to sample a mapped surface whose
    /// `dxgi_bytes_per_pixel` stride would leave the mapped row.
    ///
    /// The eleventh, added with R1010. Env-gated
    /// (`HELIOS_PRESENT_READBACK`) and capped at 8 invocations, so it is a
    /// debugging path rather than a live one -- but it was reading out of
    /// bounds for a genuinely 16-bpp or block-compressed surface, and a
    /// refusal has to be countable like every other.
    readback_stride_unsafe: RefusalCounter,

    // ── K4 / HPS2 retirement: the HWA2 allocation seam ──────────────────────
    //
    // ⛔ APPEND-ONLY, and appended deliberately at the end: `DDI_REFUSAL_SET`'s
    // order is the evidence contract, and inserting these next to the older
    // allocation counter would re-order every line a build-to-build diff reads.
    /// This driver's own create-input descriptor failed
    /// `HeliosWddmAllocationDescV2::validate_create_input` BEFORE
    /// `pfnAllocateCb`. A producer bug in `Hwa2CreateInput::build`, never a
    /// runtime-data condition — the create is refused rather than sent, because
    /// the KMD would refuse it anyway and a self-check that only logs is not a
    /// check.
    hwa2_input_invalid: RefusalCounter,
    /// `pfnAllocateCb` returned success but the descriptor that came back
    /// failed `validate_create_output`: a missing/zero `allocation_generation`,
    /// a mutated field, or a package-generation mismatch.
    ///
    /// ⚠ Before K4 NOTHING checked the write-back — the old code diffed six
    /// fields into a log line and trusted whatever it found. The allocation is
    /// rolled back and the create fails, because a descriptor that disagrees
    /// with the resource we believe we made is exactly the state §10.3 exists
    /// to make impossible.
    hwa2_output_invalid: RefusalCounter,
    /// No open-time private buffer (per-allocation or resource-level) was
    /// exactly `HELIOS_HWA2_BYTES`. `from_private_data` requires an exact
    /// length — §10.3 forbids ever selecting a legacy parser, so a 96-byte
    /// pre-retirement buffer or an oversized resource-level buffer is refused
    /// by name instead of being read as a prefix.
    hwa2_open_private_size: RefusalCounter,
    /// An open-time buffer WAS 168 bytes but failed
    /// `HeliosWddmAllocationDescV2::validate`.
    hwa2_open_desc_invalid: RefusalCounter,
    /// Historical K4 refusal column. It remains in the append-only evidence
    /// order, but no live path increments it after A3/A7: `OpenResource` now
    /// constructs through HRA1 and never asks HWA2 for a host resource id.
    hwa2_open_needs_mesa_a3: RefusalCounter,
    /// A D3D11 DDI bind bit with no HWA2 counterpart was dropped from the
    /// descriptor (`D3D11DDI_BIND_CAPTURE`, or anything the runtime adds
    /// later). A counted downgrade, in the `alloc_meta_format_unknown` class:
    /// HWA2's bind word is descriptive metadata for an opener and no consumer
    /// in this stack reads the capture bind.
    hwa2_bind_bits_dropped: RefusalCounter,
    /// D3D11 DDI misc bits with no HWA2 counterpart were dropped
    /// (`AUTO_GEN_MIP_MAP`, `DRAWINDIRECT_ARGS`, `BUFFER_ALLOW_RAW_VIEWS`,
    /// `BUFFER_STRUCTURED`, `TILED`, `TILE_POOL`). The retired trailer carried
    /// `MiscFlags` RAW, so these used to reach an opener; HWA2 offset 76 is a
    /// four-bit protocol vocabulary and they do not. Counted, not silent.
    hwa2_misc_bits_dropped: RefusalCounter,
    /// The create's plane-0 record could not be expressed inside `byte_size`:
    /// a zero row pitch, a `row_pitch * height` that overflows, or an
    /// `offset + slice_pitch` past the extent. Refused — an unbounded plane is
    /// the shape the oversize/undersize import guards exist to catch.
    hwa2_plane_unrepresentable: RefusalCounter,
    /// An image-kind create whose `DXGI_FORMAT` is `UNKNOWN`. HWA2 hard-fails
    /// it and there is nothing to substitute: the exact format is what an
    /// opener rebuilds the image from (the A8-mask-as-BGRA regression).
    hwa2_image_format_unknown: RefusalCounter,
    /// A create reached `allocate_wddm_resource` with a
    /// `D3D10DDIRESOURCE_TYPE` outside the four dimensions HWA2's kind enum can
    /// express. Unreachable through `create_resource`, which refuses an unknown
    /// dimension first (`unhandled_resource_dimension`); counted separately so
    /// a future caller that skips that gate cannot allocate with a guessed kind.
    hwa2_unknown_dimension: RefusalCounter,
    /// A valid HWA2 open cannot be represented by the generation-4 D3D11
    /// single-allocation image path: a disjoint allocation array, non-image
    /// kind, or lower image whose exact requirement exceeds the immutable HWA2
    /// extent. No allocation is guessed or partially associated.
    hwa2_open_unsupported_shape: RefusalCounter,
    /// `pfnAcquireResource`/`pfnReleaseResource` whose RESOURCE-side identity
    /// could not be verified, and which were forwarded anyway.
    ///
    /// ⛔ MEASURED 2026-08-24 (KMD 22.22.352.0): refusing here is fatal. The
    /// first `AcquireResource` DWM makes lands on a resource whose slot this
    /// device never stored; the old code answered `set_runtime_error(
    /// E_INVALIDARG)`, and DWM tore both devices down with `PresentBoundary
    /// present=0` — it never presented a single frame all boot, which is the
    /// black desktop. `D3DDDICB_SYNCTOKEN` carries only the token and the
    /// broadcast context, neither of which comes from the resource, so the
    /// check was validation the DDI does not require.
    sync_token_identity_unverified: RefusalCounter,
    /// `pfnEvictCb` skipped because a successful `pfnDeallocateCb` on the same
    /// handle had already dropped the residency reference.
    ///
    /// The paired evict is redundant AND raced: dxgkrnl answers a
    /// `D3DKMTEvict` whose allocation a submitted-but-unretired DMA packet
    /// still references with `VidSchErrorEvictingWhileInUse`, which sets the
    /// device execution state to 7 and ends in the session freeze (ROADMAP,
    /// 2026-08-28). `DestroyAllocation` has no such race -- dxgkrnl waits for
    /// the packet. Knob `UmdEvictOnDeallocate=1` restores the paired evict.
    residency_evict_suppressed: RefusalCounter,
    flush_sync_failed: RefusalCounter,
    /// `dxvk_outer_submit_begin` found a scope already active on the context
    /// (a teardown or submit colliding with an in-flight lower submit) and had
    /// to wait for it (3b, 2026-09-02). Until the serialization fix this was a
    /// silent null that DXVK mapped to `VK_ERROR_DEVICE_LOST`.
    outer_scope_busy: RefusalCounter,
    /// The wait above ran out (`UmdScopeWaitMs`) and the begin fell back to
    /// returning no scope — the old failure, now only for a wedged opener.
    outer_scope_wait_timeout: RefusalCounter,
    /// A `wait_hqc1` released because the device's adapter LUID no longer
    /// opens (PnP restart): the FromGpu signal it waited for can never execute.
    hqc1_wait_adapter_gone: RefusalCounter,
}

/// ⚠ Each counter now carries its own NAME (`RefusalCounter`, stage S2), so
/// the summary below is built by iterating a slice instead of by a
/// hand-written format string with one `{}` per field. That shape made adding
/// a twelfth counter a three-place edit whose third place -- the format
/// string -- was silently optional.
static DDI_REFUSALS: DdiRefusals = DdiRefusals {
    srv_raw_hazard: RefusalCounter::new("srv_raw_hazard"),
    resource_raw_hazard: RefusalCounter::new("resource_raw_hazard"),
    text_filter_size_ignored: RefusalCounter::new("text_filter_size_ignored"),
    staging_busy_assumed_free: RefusalCounter::new("staging_busy_assumed_free"),
    discard_partial: RefusalCounter::new("discard_partial"),
    clear_view_unsupported: RefusalCounter::new("clear_view_unsupported"),
    gs_so_declaration_dropped: RefusalCounter::new("gs_so_declaration_dropped"),
    tess_sig_fallback: RefusalCounter::new("tess_sig_fallback"),
    unhandled_resource_dimension: RefusalCounter::new("unhandled_resource_dimension"),
    alloc_meta_format_unknown: RefusalCounter::new("alloc_meta_format_unknown"),
    readback_stride_unsafe: RefusalCounter::new("readback_stride_unsafe"),
    hwa2_input_invalid: RefusalCounter::new("hwa2_input_invalid"),
    hwa2_output_invalid: RefusalCounter::new("hwa2_output_invalid"),
    hwa2_open_private_size: RefusalCounter::new("hwa2_open_private_size"),
    hwa2_open_desc_invalid: RefusalCounter::new("hwa2_open_desc_invalid"),
    hwa2_open_needs_mesa_a3: RefusalCounter::new("hwa2_open_needs_mesa_a3"),
    hwa2_bind_bits_dropped: RefusalCounter::new("hwa2_bind_bits_dropped"),
    hwa2_misc_bits_dropped: RefusalCounter::new("hwa2_misc_bits_dropped"),
    hwa2_plane_unrepresentable: RefusalCounter::new("hwa2_plane_unrepresentable"),
    hwa2_image_format_unknown: RefusalCounter::new("hwa2_image_format_unknown"),
    hwa2_unknown_dimension: RefusalCounter::new("hwa2_unknown_dimension"),
    hwa2_open_unsupported_shape: RefusalCounter::new("hwa2_open_unsupported_shape"),
    sync_token_identity_unverified: RefusalCounter::new("sync_token_identity_unverified"),
    residency_evict_suppressed: RefusalCounter::new("residency_evict_suppressed"),
    flush_sync_failed: RefusalCounter::new("flush_sync_failed"),
    outer_scope_busy: RefusalCounter::new("outer_scope_busy"),
    outer_scope_wait_timeout: RefusalCounter::new("outer_scope_wait_timeout"),
    hqc1_wait_adapter_gone: RefusalCounter::new("hqc1_wait_adapter_gone"),
};

/// The set, in the order the summary prints them. ⛔ This order is the
/// evidence contract: `DDI refusals:` lines from different builds are diffed.
/// The K4 counters are APPENDED so every pre-existing column keeps its place.
static DDI_REFUSAL_SET: [&RefusalCounter; 28] = [
    &DDI_REFUSALS.srv_raw_hazard,
    &DDI_REFUSALS.resource_raw_hazard,
    &DDI_REFUSALS.text_filter_size_ignored,
    &DDI_REFUSALS.staging_busy_assumed_free,
    &DDI_REFUSALS.discard_partial,
    &DDI_REFUSALS.clear_view_unsupported,
    &DDI_REFUSALS.gs_so_declaration_dropped,
    &DDI_REFUSALS.tess_sig_fallback,
    &DDI_REFUSALS.unhandled_resource_dimension,
    &DDI_REFUSALS.alloc_meta_format_unknown,
    &DDI_REFUSALS.readback_stride_unsafe,
    &DDI_REFUSALS.hwa2_input_invalid,
    &DDI_REFUSALS.hwa2_output_invalid,
    &DDI_REFUSALS.hwa2_open_private_size,
    &DDI_REFUSALS.hwa2_open_desc_invalid,
    &DDI_REFUSALS.hwa2_open_needs_mesa_a3,
    &DDI_REFUSALS.hwa2_bind_bits_dropped,
    &DDI_REFUSALS.hwa2_misc_bits_dropped,
    &DDI_REFUSALS.hwa2_plane_unrepresentable,
    &DDI_REFUSALS.hwa2_image_format_unknown,
    &DDI_REFUSALS.hwa2_unknown_dimension,
    &DDI_REFUSALS.hwa2_open_unsupported_shape,
    &DDI_REFUSALS.sync_token_identity_unverified,
    &DDI_REFUSALS.residency_evict_suppressed,
    &DDI_REFUSALS.flush_sync_failed,
    &DDI_REFUSALS.outer_scope_busy,
    &DDI_REFUSALS.outer_scope_wait_timeout,
    &DDI_REFUSALS.hqc1_wait_adapter_gone,
];

/// One bounded log line carrying every counter.
///
/// The UMD's evidence channel is the log — it has no registry counter surface —
/// and T5 proved the failure mode this avoids: three of the four R806/R809
/// scan-out counters were process-global atomics that NOTHING ever loaded, so
/// ROADMAP's own instruction to read them after a gate run was not executable.
/// **An instrument nothing can read is not an instrument.** This extends the
/// `scanout_counter_summary()` pattern that fixed it, rather than inventing a
/// second mechanism, and it is a `log_line` summary rather than an escape,
/// which the recommendation is explicit about.
///
/// ⚠ NOT on a per-present path: the UMD hot-path logger cost is exactly what T2
/// measured and reduced. Emitted at `DestroyDevice`, and on the FIRST hit of
/// each counter (so a refusal that fires once in a session that never tears a
/// device down is still visible).
pub(crate) fn ddi_refusal_summary() -> String {
    refusals::summary("DDI refusals:", &DDI_REFUSAL_SET)
}

/// Bump one refusal counter and emit the summary on its FIRST hit.
///
/// Taking `&RefusalCounter` rather than a field name keeps the call sites one
/// line and makes "increment without a readout" — the defect this whole item
/// exists to close — impossible to write by accident. `note()` is
/// `#[must_use]` in `umd_common`, so dropping the first-hit signal does not
/// compile.
fn note_ddi_refusal(counter: &RefusalCounter) {
    if counter.note() {
        log_error!("{}", ddi_refusal_summary());
    }
}

/// See `DdiRefusals::outer_scope_busy`.
pub(crate) fn note_outer_scope_busy() {
    note_ddi_refusal(&DDI_REFUSALS.outer_scope_busy);
}

/// See `DdiRefusals::outer_scope_wait_timeout`.
pub(crate) fn note_outer_scope_wait_timeout() {
    note_ddi_refusal(&DDI_REFUSALS.outer_scope_wait_timeout);
}

/// See `DdiRefusals::hqc1_wait_adapter_gone`.
pub(crate) fn note_hqc1_wait_adapter_gone() {
    note_ddi_refusal(&DDI_REFUSALS.hqc1_wait_adapter_gone);
}

/// `state::release_residency` skipped a `pfnEvictCb` the deallocate had
/// already made redundant. Its own function because `DDI_REFUSALS` is private to
/// this module.
pub(crate) fn note_residency_evict_suppressed() {
    note_ddi_refusal(&DDI_REFUSALS.residency_evict_suppressed);
}

/// Why a present returned without minting a swapchain token. All three shapes
/// used to share one log line and return S_OK to DXGI, so the failing stage was
/// lost and the runtime never learned the present had not happened.
#[derive(Copy, Clone, PartialEq, Eq)]
enum PresentSkip {
    NoDxgiCallbacks,
    NoContext,
    NoSourceAllocation,
}

/// The three preconditions of the present-callback block, resolved once. The
/// callback code is unreachable with any of them unmet, so "skipped" is
/// distinguishable from "succeeded" at the type level even though the returned
/// HRESULT is unchanged.
struct PresentReady {
    h_context: core::ptr::NonNull<c_void>,
    src_alloc: core::num::NonZeroU32,
}

/// `dxgi_callbacks` was null: no DXGI base callback table on the device.
static PRESENT_SKIP_NO_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
/// `h_context` was null: pfnCreateContextCb failed at CreateDevice. R404 closes
/// the creation half (such a device is now refused); this counts the presents
/// that reach here on a device that predates it or fails another way.
static PRESENT_SKIP_NO_CONTEXT: AtomicUsize = AtomicUsize::new(0);
/// The presented source resource carries no WDDM allocation.
static PRESENT_SKIP_NO_SRC_ALLOC: AtomicUsize = AtomicUsize::new(0);
/// Rate cap for the skip log line (declared diagnostic-volume change: a device
/// that permanently lacks a context used to write one formatted line per
/// present, at frame rate, through the unconditional writer).
static PRESENT_SKIP_LOG_COUNT: LogThrottle = LogThrottle::new();

/// Resolve the present-callback preconditions, counting exactly which one
/// failed. Deliberately no fourth "NoDevice" variant: `helios_device` returns
/// None only for a null `pDrvPrivate`, which dxgkrnl does not pass.
unsafe fn present_prerequisites(
    dev: &crate::device_funcs::HeliosDevice,
    src_alloc: u32,
) -> Result<PresentReady, PresentSkip> {
    if dev.dxgi_callbacks.is_null() {
        PRESENT_SKIP_NO_CALLBACKS.fetch_add(1, Ordering::Relaxed);
        return Err(PresentSkip::NoDxgiCallbacks);
    }
    let Some(h_context) = dev.outer.context.as_ref().map(|c| c.handle) else {
        PRESENT_SKIP_NO_CONTEXT.fetch_add(1, Ordering::Relaxed);
        return Err(PresentSkip::NoContext);
    };
    let Some(src_alloc) = core::num::NonZeroU32::new(src_alloc) else {
        PRESENT_SKIP_NO_SRC_ALLOC.fetch_add(1, Ordering::Relaxed);
        return Err(PresentSkip::NoSourceAllocation);
    };
    Ok(PresentReady {
        h_context,
        src_alloc,
    })
}
