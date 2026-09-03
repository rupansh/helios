//! helios_kmd_logic — host-testable leaf logic for the Helios WDDM miniport.
//!
//! `kmd_render` cannot host a libtest harness at all: `kmd_render/build.rs` runs
//! bindgen over `ntddk.h` / `dispmprt.h` / `d3dkmddi.h` and shells out to
//! `rc.exe`, and the crate is a `panic = "abort"` `cdylib`. So any KMD logic that
//! deserves an automated oracle has to be moved *out* of the crate rather than
//! tested in place. This crate is where it goes.
//!
//! The contract that makes that worth doing is the absent dependency edge: there
//! is no `wdk-sys`, no `wdk-build`, no generated `dxgk`/`ntddk` binding, and no
//! `kmd_render` type reachable from here, so nothing in this crate can read a
//! `DXGKARG_*` field, a `HANDLE`, or an atomic out of `AdapterContext`. Every
//! rule below is a function of its arguments and nothing else, which is exactly
//! the property that makes it testable on the host.
//!
//! ⚠ The one admitted edge is `helios_protocol` (K4, 2026-08-10). It is
//! `no_std`, it is already a `kmd_render` dependency, and its records carry no
//! handle, pointer or kernel state by construction, so it does not weaken the
//! rule above — see the argument in `kmd_logic/Cargo.toml`.
//!
//! Run the tests with `cargo test` inside `kmd_logic/`, the same way `protocol/`
//! is tested. Nothing else runs them.

#![no_std]

pub mod allocation_placement;
pub mod batch_replay;
pub mod system_page_join;
pub mod committed_mode;
pub mod committed_mode_lifecycle;
pub mod context_attachment;
pub mod context_lifecycle;
pub mod control_owner_slots;
pub mod control_owner_table;
pub mod control_owner_tickets;
pub mod control_ownership;
pub mod direct_scanout_admission;
pub mod direct_scanout_lifetime;
pub mod display_backing_lifetime;
pub mod ordered_engine;
pub mod present_copy;
pub mod present_dma_header;
pub mod outer_execution;
pub mod umd_private_query;
pub mod venus_executor;
pub mod wire_fence;

/// Fixed-phase scheduling for the synthetic 60 Hz CRTC heartbeat.
///
/// `DXGK_VIDPN_SOURCE_MODE` advertises 60/1 Hz, so a 16 ms recurring timer is
/// not an approximation we may expose as the same mode: it runs at 62.5 Hz.
/// The KMD uses a one-shot timer and advances this absolute interrupt-time
/// deadline instead. Advancing from the preceding deadline (rather than from
/// `now`) prevents ordinary DPC latency from accumulating as phase drift; a
/// genuinely late DPC skips directly to the first future deadline instead of
/// generating a catch-up burst.
pub mod vsync_deadline {
    /// 16.6667 ms in the kernel timer's 100 ns units. This is 60 Hz to within
    /// 0.0002%, versus the old 16 ms period's 4.17% error.
    pub const PERIOD_100NS: u64 = 166_667;

    /// Return the first fixed-phase deadline strictly after `now`.
    ///
    /// `previous` is the deadline which just fired (or `now` when arming the
    /// first tick). `None` is terminal: the interrupt-time representation is
    /// exhausted, so the KMD must leave the one-shot timer unarmed rather than
    /// turn a saturated deadline into an immediate rearm storm.
    pub const fn next(previous: u64, now: u64) -> Option<u64> {
        let Some(candidate) = previous.checked_add(PERIOD_100NS) else {
            return None;
        };
        if candidate > now {
            return Some(candidate);
        }

        let late = now.saturating_sub(candidate);
        let intervals = late / PERIOD_100NS + 1;
        let Some(advance) = intervals.checked_mul(PERIOD_100NS) else {
            return None;
        };
        candidate.checked_add(advance)
    }

    /// Convert a future interrupt-time deadline to a negative relative
    /// `LARGE_INTEGER` due time. A raced/equal deadline is rearmed one 100 ns
    /// unit ahead, never as zero (which has absolute-time semantics).
    pub const fn relative_due(deadline: u64, now: u64) -> i64 {
        let delta = deadline.saturating_sub(now);
        let nonzero = if delta == 0 { 1 } else { delta };
        let bounded = if nonzero > i64::MAX as u64 {
            i64::MAX as u64
        } else {
            nonzero
        };
        -(bounded as i64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn initial_arm_is_one_sixtieth_second() {
            assert_eq!(next(10_000_000, 10_000_000), Some(10_166_667));
        }

        #[test]
        fn ordinary_lateness_does_not_accumulate_phase_drift() {
            let fired = 1_166_667;
            assert_eq!(next(fired, fired + 4_000), Some(1_333_334));
        }

        #[test]
        fn missed_ticks_skip_to_one_future_deadline_without_burst() {
            let fired = 1_166_667;
            let now = fired + PERIOD_100NS * 4 + 7;
            let Some(deadline) = next(fired, now) else {
                panic!("a representable deadline must remain armable");
            };
            assert!(deadline > now);
            assert_eq!(deadline, fired + PERIOD_100NS * 5);
        }

        #[test]
        fn relative_due_is_always_negative_and_exact() {
            assert_eq!(relative_due(166_667, 0), -166_667);
            assert_eq!(relative_due(42, 42), -1);
            assert_eq!(relative_due(41, 42), -1);
        }

        #[test]
        fn deadline_overflow_is_terminal_not_an_immediate_rearm() {
            assert_eq!(next(u64::MAX - PERIOD_100NS + 1, 0), None);
            assert_eq!(next(u64::MAX - PERIOD_100NS, u64::MAX), None);
        }
    }
}

/// Page granularity for blob window offsets/sizes and for WDDM allocation sizes.
///
/// Moved verbatim from `kmd_render/src/ddi/create_allocation.rs` (`const PAGE`)
/// and `kmd_render/src/virtio/gpu.rs` (`const BLOB_PAGE`), which held two
/// byte-identical copies of the constant and of [`round_up_page`].
pub const PAGE_BYTES: u64 = 4096;

/// Cross-adapter row-major textures require a 256-byte row-pitch alignment
/// (`D3D12_TEXTURE_DATA_PITCH_ALIGNMENT`). The IddCx composition surface is a
/// cross-adapter resource (created as a standard allocation, opened on the Helios
/// render side — `rendering-on-a-discrete-gpu-using-cross-adapter-resources.md`),
/// so its backing must be linear with this pitch for the IndirectKMD adapter to
/// open the same surface. PATH-A (2026-06-22).
pub const CROSS_ADAPTER_PITCH_ALIGN: u32 = 256;

/// Round `n` up to the next [`PAGE_BYTES`] multiple (saturating).
///
/// Saturating on purpose: `u64::MAX` rounds to `0xFFFF_FFFF_FFFF_F000`, never to
/// zero. The third, non-saturating copy that used to live in
/// `kmd_render/src/virtio/venus/bringup.rs` is **gone** — K4 made one of its
/// callers reachable with a guest-supplied size, which turned the wrap into an
/// overflow panic inside a DDI. There is now exactly one implementation.
pub const fn round_up_page(n: u64) -> u64 {
    n.saturating_add(PAGE_BYTES - 1) & !(PAGE_BYTES - 1)
}

/// 32-bpp linear row pitch aligned to the cross-adapter requirement.
///
/// This is the stride the KMD hands the host in `SET_SCANOUT_BLOB`: a 1896-wide
/// primary must produce 7680, not `width * 4` = 7584. Reading each scanout row
/// short is invisible guest-side, which is why the vectors below are pinned.
pub const fn cross_adapter_pitch(width: u32) -> u32 {
    let raw = width.saturating_mul(4);
    raw.saturating_add(CROSS_ADAPTER_PITCH_ALIGN - 1) & !(CROSS_ADAPTER_PITCH_ALIGN - 1)
}

/// Checked sub-range of a mapped byte window: is `offset .. offset + bytes`
/// wholly inside a window of `len` bytes?
///
/// Returns the byte offset on success, `None` on any overflow or overrun. Both
/// operands of every `copy_nonoverlapping` in the paging engine have to answer
/// this question, and one of them (the MDL side of a classic TRANSFER) used to
/// answer it with a comment instead — `MdlOffset`/`TransferSize` were applied
/// raw to a pointer that carries no length, so an over-long eviction wrote
/// kernel memory past the mapped buffer (k-paging-03).
pub const fn window_range(len: u64, offset: u64, bytes: u64) -> Option<u64> {
    match offset.checked_add(bytes) {
        Some(end) if end <= len => Some(offset),
        _ => None,
    }
}

/// A physical page frame number, as supplied by `DXGK_PTE::PageAddress` or
/// derived from `MmGetPhysicalAddress`.
///
/// Exists so the PFN-to-address conversion is written once, checked. The
/// paging engine guarded it with `checked_shl(12)`, which is not an overflow
/// guard at all: `u64::checked_shl` returns `None` only when the SHIFT COUNT is
/// >= 64, so with a constant 12 it can never fail and the `BAR_ERR_VIRTUAL`
/// counter documented for that failure was unreachable (k-paging-10).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pfn(pub u64);

impl Pfn {
    /// Byte address of this page frame, or `None` if it cannot be represented.
    ///
    /// Two rules, both load-bearing for the caller: the multiply must not
    /// overflow, and the result must fit a POSITIVE `i64`, because it is
    /// assigned to `PHYSICAL_ADDRESS.QuadPart` (an `i64`) and handed to
    /// `MmMapIoSpace` — an address with bit 63 set would arrive negative.
    pub const fn physical_address(self) -> Option<u64> {
        match self.0.checked_mul(PAGE_BYTES) {
            Some(address) if address <= i64::MAX as u64 => Some(address),
            _ => None,
        }
    }
}

// ⛔ TOMBSTONE — `MetaLayout` (the `Legacy24` / `Full48` trailer classifier, its
// `LEGACY_BYTES` / `FULL_BYTES` counts, `from_trailer_len`, `copy_bytes` and the
// `TryFrom<usize>` impl) was DELETED by K4, 2026-08-10.
//
// It classified the length of the retired `HeliosWddmAllocMeta` trailer, and its
// own doc said out loud what made it a defect: "the byte counts are duplicated
// from `helios_protocol` because this crate deliberately has no dependency edge
// to it". K4 admitted that edge (see the `helios_protocol` argument in
// `kmd_logic/Cargo.toml`), so the duplication no longer buys anything — and a
// second declaration of a RETIRED wire record's layout, sitting in the very
// crate whose new dependency argument cites exactly that as the thing to avoid,
// is worse than a duplicate: it is a duplicate of something that no longer
// exists. Its last consumer (`create_allocation.rs`'s trailer parse, which
// pinned the two together with `const _: () = assert!(size_of::<…>() ==
// MetaLayout::FULL_BYTES)`) died with the trailer; `rg MetaLayout kmd_render/`
// is empty.
//
// ⭐ The ARGUMENT it carried is not deleted, because it is still live — it is
// the "validate per-arm, never max-union" rule (k-alloc-03). A max-union bound
// ("at least 24, copy up to 48") accepted any length in 25..=47 and copied it
// into the MIDDLE of a field, zero-extending the remainder: a 30-byte trailer
// yielded `venus_alloc_size = real & 0x0000_FFFF_FFFF_FFFF` and
// `plane_offset = 0`, a plausible-looking but wrong exact import size, which is
// the undersize-import class that produced host Xid 31 FAULT_PTE. That rule now
// lives where the record does: HWA2/HVM1/HOC1 are ONE fixed size each and are
// parsed only through `helios_protocol`'s `from_private_data`, which refuses any
// length that is not exact rather than reading a prefix — see
// [`allocation_identity`], whose tests are its oracle.

/// Verdict of one seqlock read attempt over a published descriptor.
///
/// The primary-scanout descriptor is published field by field and was read the
/// same way, with the resource id loaded FIRST and everything else `Relaxed`:
/// the publisher's store order defends a first publish, but a REPUBLISH landing
/// between the reader's loads yields resource_id from generation N combined with
/// pitch/format/plane_offset from N+1, and the consumer cannot detect it
/// (k-capsescape-11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeqRead {
    /// The snapshot is coherent.
    Stable,
    /// A publish was in flight (odd sequence) or landed mid-read — read again.
    Retry,
}

/// Classify a seqlock read from the sequence value before and after the fields.
///
/// Odd `before` means a writer held the descriptor when the read started; a
/// changed value means one landed during it. Both are retries; nothing else is.
pub const fn seq_read(before: u32, after: u32) -> SeqRead {
    if before % 2 != 0 || before != after {
        SeqRead::Retry
    } else {
        SeqRead::Stable
    }
}

/// Bound on seqlock read attempts before the reader gives up and reports "no
/// coherent value" instead of spinning.
///
/// The reader is a PASSIVE escape but the publishers can run at raised IRQL on
/// the VidPn path, so an unbounded spin here would be a new wedge class: the
/// escape thread could hold the CPU while the publisher is descheduled.
pub const SEQ_READ_ATTEMPTS: u32 = 8;

/// Smallest scan-out extent Helios will adopt from the host's `GET_DISPLAY_INFO`.
///
/// One named constant for what used to be two bare literals re-evaluated on
/// every `display_mode()` call.
pub const MIN_DISPLAY_WIDTH: u32 = 320;
pub const MIN_DISPLAY_HEIGHT: u32 = 240;

/// A scan-out extent that has passed the minimum-size check exactly once.
///
/// The mode used to be two unvalidated `u32`s whose validation re-ran on every
/// read through bare literals. Making it a constructed value means there is no
/// way to observe an unvalidated `(0, 0)` mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayMode {
    width: core::num::NonZeroU32,
    height: core::num::NonZeroU32,
}

impl DisplayMode {
    /// Adopt the host-reported extent, or `None` if it is unusable.
    pub const fn from_host(width: u32, height: u32) -> Option<Self> {
        if width < MIN_DISPLAY_WIDTH || height < MIN_DISPLAY_HEIGHT {
            return None;
        }
        match (
            core::num::NonZeroU32::new(width),
            core::num::NonZeroU32::new(height),
        ) {
            (Some(width), Some(height)) => Some(Self { width, height }),
            // Unreachable: both are >= the minimums above. Written as a match
            // rather than `unwrap` because a panic in a DDI is a silent graphics
            // deadlock.
            _ => None,
        }
    }

    pub const fn width(self) -> u32 {
        self.width.get()
    }

    pub const fn height(self) -> u32 {
        self.height.get()
    }

    /// The `(w << 16) | h` form the `DspMd` breadcrumb reports.
    pub const fn packed(self) -> u32 {
        (self.width.get() << 16) | (self.height.get() & 0xFFFF)
    }

    /// The extent used when the host reports nothing usable.
    ///
    /// Written as a total `match` rather than an `unwrap` or a const `panic!`:
    /// a panicking expression has no place in a crate the kernel driver links,
    /// even one that could only fire at compile time. The `None` arms are
    /// unreachable for the nonzero constants below, and
    /// `display_mode_fallback_is_the_documented_extent` asserts that — so an
    /// edit that broke it fails a host test instead of silently degrading the
    /// fallback to the minimum extent.
    pub const FALLBACK: Self = Self {
        width: match core::num::NonZeroU32::new(FALLBACK_DISPLAY_WIDTH) {
            Some(w) => w,
            None => core::num::NonZeroU32::MIN,
        },
        height: match core::num::NonZeroU32::new(FALLBACK_DISPLAY_HEIGHT) {
            Some(h) => h,
            None => core::num::NonZeroU32::MIN,
        },
    };
}

/// The mode Helios advertises when the host's `GET_DISPLAY_INFO` reported no
/// usable scanout-0 size. Mirrored by `ddi::vidpn::DEFAULT_MODE_*`.
pub const FALLBACK_DISPLAY_WIDTH: u32 = 1920;
pub const FALLBACK_DISPLAY_HEIGHT: u32 = 1080;

impl From<DisplayMode> for (u32, u32) {
    fn from(mode: DisplayMode) -> Self {
        (mode.width(), mode.height())
    }
}

/// Pack the VidPn programming gate: generation in the high 32 bits, the active
/// flag in the low 32.
///
/// The gate used to be a bare `AtomicU32` flag, so nothing identified WHICH
/// programming interval a completion belonged to. Because the DIRQL half of
/// `SetVidPnSourceAddress` takes no lock, a second call can raise the gate for
/// interval N+1 while copy N is still outstanding; copy N's completion then
/// cleared the gate belonging to N+1. Pairing the flag with its generation in
/// ONE word makes raise and "clear only my interval" single atomic operations.
///
/// The low half keeps the exact 0/1 meaning the flag had, so every existing
/// breadcrumb that reports the gate still reports 0 or 1.
pub const fn gate_pack(seq: u32, active: bool) -> u64 {
    ((seq as u64) << 32) | (active as u64)
}

/// The generation half of a packed gate word.
pub const fn gate_seq(word: u64) -> u32 {
    (word >> 32) as u32
}

/// The active half of a packed gate word — what the VSync DPC tests.
pub const fn gate_active(word: u64) -> bool {
    (word & 0xFFFF_FFFF) != 0
}

/// The scan-out surface formats Helios can put on virtio-gpu scanout 0.
///
/// This is the one place the DXGI-to-wire mapping is written down. It used to be
/// spelled four times with bare integers — `display.rs`'s `virtio_scanout_format`
/// match, a function-local `const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87` used
/// twice, the `matches!(dxgi_format, 28 | 87 | 88)` direct-scan-out allowlist,
/// and three more local consts plus a three-way `!=` chain gating the copy path
/// in `create_allocation.rs`. A format added to three of those four surfaces as
/// an opaque `ScSet=0xE3` or `ScFmt` trace.
///
/// The virtio values are literals rather than a `helios_protocol` import because
/// this crate's whole contract is that it has no dependency edge (see the
/// Cargo.toml comment); `kmd_render` pins them to `helios_protocol` with a
/// compile-time assertion, so a drift is a build failure, not a runtime bug.
///
/// NOT merged in, on purpose, because they answer different questions:
/// `create_allocation::resolved_dxgi_format` (D3DDDIFORMAT -> DXGI),
/// `venus::PresentPixelFormat::from_dxgi` (the wider render-side set), and
/// `scanout_diag::scanout_format` (keyed on the *diag mode*, returning a virtio
/// constant directly — it is not a fourth DXGI mapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanoutFormat {
    /// DXGI 88 `B8G8R8X8_UNORM` -> virtio 2.
    Bgrx8,
    /// DXGI 87 `B8G8R8A8_UNORM` -> virtio 1.
    Bgra8,
    /// DXGI 28 `R8G8B8A8_UNORM` -> virtio 67.
    Rgba8,
}

impl ScanoutFormat {
    /// The DXGI formats a scan-out surface may *declare*.
    ///
    /// This is the strict set — exactly the three the direct-scan-out validator
    /// and the copy-path gate accept today. DXGI 0 is deliberately NOT here; see
    /// [`Self::from_dxgi_or_legacy_zero`].
    pub const fn from_dxgi(dxgi: u32) -> Option<Self> {
        match dxgi {
            28 => Some(Self::Rgba8),
            87 => Some(Self::Bgra8),
            88 => Some(Self::Bgrx8),
            _ => None,
        }
    }

    /// [`Self::from_dxgi`] plus the legacy `0 -> Bgrx8` arm.
    ///
    /// The wire-format converter has always accepted DXGI 0 while both
    /// validators reject it, so collapsing all four sites onto one acceptance
    /// set would have *changed which formats are accepted*. That divergence is
    /// preserved verbatim here and named, so it is greppable instead of latent:
    /// only the converter calls this, and its `0` arm has no reachable producer
    /// today (a direct-scan-out primary always carries the UMD's exact DXGI
    /// format from the same private-data record, and the LINEAR arm's format is
    /// [`Self::Bgra8`] by construction). Deleting the arm is a separate,
    /// counter-backed commit.
    pub const fn from_dxgi_or_legacy_zero(dxgi: u32) -> Option<Self> {
        if dxgi == 0 {
            Some(Self::Bgrx8)
        } else {
            Self::from_dxgi(dxgi)
        }
    }

    /// The `VIRTIO_GPU_FORMAT_*` value for `SET_SCANOUT_BLOB`.
    pub const fn virtio(self) -> u32 {
        match self {
            Self::Bgra8 => 1,  // VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM
            Self::Bgrx8 => 2,  // VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM
            Self::Rgba8 => 67, // VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM
        }
    }

    /// The canonical DXGI format value.
    ///
    /// Note this is not a round trip for the legacy zero arm:
    /// `from_dxgi_or_legacy_zero(0).unwrap().dxgi() == 88`. That is the existing
    /// behaviour — the converter maps 0 and 88 onto the same wire format.
    pub const fn dxgi(self) -> u32 {
        match self {
            Self::Bgrx8 => 88,
            Self::Bgra8 => 87,
            Self::Rgba8 => 28,
        }
    }
}

/// Maximum venus command stream built for any single direct/ring command.
///
/// The largest encoder in the KMD is `vkCreateDevice` at its `EXT_FULL` tier,
/// which is **332 bytes**: 144 bytes of struct plus a 188-byte extension block
/// (five 8-byte string lengths plus 24+28+28+32+36 bytes of NUL-terminated,
/// 4-byte-padded names). That leaves 180 bytes of slack — enough for one more
/// extension string, not for four, and not for any encoder that grows by a
/// `pNext` chain, a multi-region `vkCmdCopyImage` or a queue-family index array.
///
/// The number is asserted by `writer_ext_full_create_device_is_332_bytes` below
/// rather than by a comment: the previous comment claimed "the largest is
/// `vkCreateDevice` (~120 bytes)" and was wrong by 212 bytes, which mattered
/// because until [`Writer`] gained a checked API, overrunning this buffer was a
/// slice-index panic — i.e. a `KeBugCheck` inside a DDI.
pub const MAX_CMD_BYTES: usize = 512;

/// Fixed-capacity little-endian writer for venus command streams.
///
/// All venus scalars are 4-byte aligned in the stream; `size_t` / `VkDeviceSize`
/// / handle / array_size are 8 bytes, and `u32` / `VkResult` / `VkStructureType`
/// / `VkFlags` / `VkCommandTypeEXT` are 4 bytes.
///
/// Overflow is **sticky and non-panicking**: a write that would exceed the
/// capacity `N` is dropped, the writer is poisoned, and
/// [`StreamWriter::finished`] returns `None` forever after. The caller turns
/// that into a refusal with a named counter. Every write method is infallible
/// so the ~40 encoder bodies stay linear; the single fallible point is where the
/// bytes are handed out.
///
/// `N` is a const parameter because the present copy stream (D5b) carries up to
/// [`present_copy::MAX_REGIONS`] `VkImageCopy` records and does not fit the
/// 512-byte control-command size every other encoder needs; [`Writer`] keeps
/// that default so no existing encoder changes.
pub struct StreamWriter<const N: usize> {
    buf: [u8; N],
    len: usize,
    overflow: bool,
}

/// The control-command writer: [`StreamWriter`] at [`MAX_CMD_BYTES`].
pub type Writer = StreamWriter<MAX_CMD_BYTES>;

impl<const N: usize> Default for StreamWriter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> StreamWriter<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
            overflow: false,
        }
    }

    /// Reserve `n` bytes, or poison the writer and report that there is no room.
    fn reserve(&mut self, n: usize) -> bool {
        if self.overflow || self.len + n > N {
            self.overflow = true;
            return false;
        }
        true
    }

    pub fn u32(&mut self, v: u32) {
        if !self.reserve(4) {
            return;
        }
        self.buf[self.len..self.len + 4].copy_from_slice(&v.to_le_bytes());
        self.len += 4;
    }

    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    pub fn u64(&mut self, v: u64) {
        if !self.reserve(8) {
            return;
        }
        self.buf[self.len..self.len + 8].copy_from_slice(&v.to_le_bytes());
        self.len += 8;
    }

    /// A f32 priority value (encoded as its IEEE-754 bits).
    pub fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }

    /// `vn_encode_simple_pointer` / `vn_encode_array_size`: a u64 count (1
    /// present, 0 absent / empty array).
    pub fn count(&mut self, present: bool) {
        self.u64(if present { 1 } else { 0 });
    }

    /// Copy `bytes` and zero-fill to the next 4-byte boundary.
    pub fn bytes_padded(&mut self, bytes: &[u8]) {
        let padded = (bytes.len() + 3) & !3;
        if !self.reserve(padded) {
            return;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        for i in bytes.len()..padded {
            self.buf[self.len + i] = 0;
        }
        self.len += padded;
    }

    /// A Vulkan object handle. Identical bytes to [`StreamWriter::u64`]; the
    /// separate name exists so the KMD's per-class handle newtypes can be
    /// written without spelling out a conversion at every encoder, and so a
    /// reader can see at a glance which 8-byte words in a stream are handles.
    pub fn handle<H: Into<u64>>(&mut self, h: H) {
        self.u64(h.into());
    }

    /// The command header: `VkCommandTypeEXT | VkCommandFlagsEXT`.
    pub fn header(&mut self, cmd_type: u32, flags: u32) {
        self.u32(cmd_type);
        self.u32(flags);
    }

    /// Bytes written so far. Meaningless once [`StreamWriter::overflowed`] is set.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True once any write has been refused for want of room.
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    /// The `VkCommandTypeEXT` this stream opened with — stream word 0, read back
    /// so an overflow refusal can name the command that caused it. Reads 0 if the
    /// header was never written.
    pub fn cmd_type(&self) -> u32 {
        if self.len < 4 {
            return 0;
        }
        u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]])
    }

    /// The encoded stream, or `None` if any write overflowed.
    ///
    /// This is the only way to get the bytes out, so a stream that did not fit
    /// cannot be submitted: the `Option` replaces what used to be a slice-index
    /// panic on the first over-long write.
    pub fn finished(&self) -> Option<&[u8]> {
        (!self.overflow).then(|| &self.buf[..self.len])
    }
}

// ── K11 finite per-session host transport ───────────────────────────────────

/// Pure, fixed-capacity pieces of K11's one allowed host operation.  The WDK
/// half owns context/resource lifetimes and the actual control roundtrip; this
/// module pins the byte stream, reply range, and raw host evidence on Linux.
pub mod session_transport {
    use super::{scanout_lease::fence_is_forward, Writer, CMD_FLAG_GENERATE_REPLY};

    pub const CMD_CREATE_INSTANCE: u32 = 0;
    pub const CMD_DESTROY_INSTANCE: u32 = 1;
    pub const CMD_SET_REPLY_COMMAND_STREAM_MESA: u32 = 178;
    pub const ST_APPLICATION_INFO: i32 = 0;
    pub const ST_INSTANCE_CREATE_INFO: i32 = 1;
    pub const HOST_CREATE_INSTANCE_REPLY_BYTES: u64 = 24;
    /// `VK_MAKE_API_VERSION(0, 1, 4, 0)`. A NULL `pApplicationInfo` is patched
    /// by vkr_instance.c to apiVersion 1.1, which MIN2s into every device proc
    /// table and leaves all core-1.2+/1.3+ procs NULL when their KHR alias ext
    /// is unenabled — vkr dispatch then jumps to 0 on the first
    /// vkQueueSubmit2/vkCmdBeginRendering (2,835 host SIGSEGVs, 2026-08-22/23).
    pub const API_VERSION_1_4: u32 = (1 << 22) | (4 << 12);

    /// Admission state for the one synchronous ring-zero control operation a
    /// live K11 session may own.  The WDK half supplies the exact wait/event
    /// and mints the fence from the transport-global wire namespace at enqueue;
    /// this state makes the single-owner transition independently testable.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RingZeroControlState {
        occupied: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RingZeroControlRefusal {
        Occupied,
    }

    impl RingZeroControlState {
        pub const fn new() -> Self {
            Self { occupied: false }
        }

        pub fn try_acquire(&mut self) -> Result<(), RingZeroControlRefusal> {
            if self.occupied {
                return Err(RingZeroControlRefusal::Occupied);
            }
            self.occupied = true;
            Ok(())
        }

        pub fn release(&mut self) -> bool {
            if !self.occupied {
                return false;
            }
            self.occupied = false;
            true
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ReplyRange {
        pub payload_offset: u64,
        pub final_end: u64,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ReplyRangeRefusal {
        ZeroCapacity,
        SlotMisaligned,
        CapacityExceedsSlot,
        ArithmeticOverflow,
        RangeExceedsPool,
        FinalReplyExceedsRange,
    }

    /// Admit the one physical K2a slot range used for the fixed HVR1 header and
    /// final HTS1 payload.  The raw renderer reply belongs to K11's private SHM
    /// resource and is deliberately absent from this user-visible range model.
    pub fn admit_reply_range(
        pool_bytes: u64,
        slot_bytes: u64,
        reply_offset: u64,
        reply_capacity: u64,
        hvr1_header_bytes: u64,
        final_payload_bytes: u64,
    ) -> Result<ReplyRange, ReplyRangeRefusal> {
        if reply_capacity == 0 {
            return Err(ReplyRangeRefusal::ZeroCapacity);
        }
        if slot_bytes == 0 || reply_offset % slot_bytes != 0 {
            return Err(ReplyRangeRefusal::SlotMisaligned);
        }
        if reply_capacity > slot_bytes {
            return Err(ReplyRangeRefusal::CapacityExceedsSlot);
        }
        let reply_end = reply_offset
            .checked_add(reply_capacity)
            .ok_or(ReplyRangeRefusal::ArithmeticOverflow)?;
        if reply_end > pool_bytes {
            return Err(ReplyRangeRefusal::RangeExceedsPool);
        }
        let payload_offset = reply_offset
            .checked_add(hvr1_header_bytes)
            .ok_or(ReplyRangeRefusal::ArithmeticOverflow)?;
        let final_end = payload_offset
            .checked_add(final_payload_bytes)
            .ok_or(ReplyRangeRefusal::ArithmeticOverflow)?;
        if final_end > reply_end {
            return Err(ReplyRangeRefusal::FinalReplyExceedsRange);
        }
        Ok(ReplyRange {
            payload_offset,
            final_end,
        })
    }

    /// Select the one fixed KMD-private SHM reply target.  This is a standalone
    /// direct submit, matching Mesa's canonical Venus setup sequence: the
    /// renderer must finish installing the target before a reply-generating
    /// command is decoded.
    pub fn encode_set_reply_command_stream(
        reply_resource_id: u32,
        reply_offset: u64,
        reply_bytes: u64,
    ) -> Writer {
        let mut stream = Writer::new();
        stream.header(CMD_SET_REPLY_COMMAND_STREAM_MESA, 0);
        stream.count(true);
        stream.u32(reply_resource_id);
        stream.u64(reply_offset);
        stream.u64(reply_bytes);
        stream
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ReplyOperandRefusal {
        ResourceIdZero,
        OffsetOutsidePayload,
        OperandNotPlaceholder,
    }

    /// Patch only the generated SetReply resource operand in the KMD-owned
    /// payload copy. The renderer-private id must never be substituted by the
    /// user-visible K2a reply-pool id: K2a receives only the validated final
    /// reply after the context-local private target reaches its host terminal.
    pub fn patch_private_reply_resource(
        payload: &mut [u8],
        operand_offset: u32,
        private_resource_id: u32,
    ) -> Result<(), ReplyOperandRefusal> {
        if private_resource_id == 0 {
            return Err(ReplyOperandRefusal::ResourceIdZero);
        }
        let start = operand_offset as usize;
        let end = start
            .checked_add(core::mem::size_of::<u32>())
            .ok_or(ReplyOperandRefusal::OffsetOutsidePayload)?;
        let dst = payload
            .get_mut(start..end)
            .ok_or(ReplyOperandRefusal::OffsetOutsidePayload)?;
        if dst != 0u32.to_le_bytes() {
            return Err(ReplyOperandRefusal::OperandNotPlaceholder);
        }
        dst.copy_from_slice(&private_resource_id.to_le_bytes());
        Ok(())
    }

    /// Encode K11's one allowlisted reply-generating host operation.  Reply
    /// target setup is intentionally not repeated or combined with this stream.
    pub fn encode_create_instance(instance_handle: u64) -> Writer {
        let mut stream = Writer::new();
        stream.header(CMD_CREATE_INSTANCE, CMD_FLAG_GENERATE_REPLY);
        stream.count(true);
        stream.i32(ST_INSTANCE_CREATE_INFO);
        stream.u64(0); // pNext
        stream.u32(0); // flags
        stream.count(true); // pApplicationInfo — see API_VERSION_1_4
        stream.i32(ST_APPLICATION_INFO); // sType
        stream.u64(0); // pNext
        stream.count(false); // pApplicationName: array_size 0
        stream.u32(0); // applicationVersion
        stream.count(false); // pEngineName: array_size 0
        stream.u32(0); // engineVersion
        stream.u32(API_VERSION_1_4); // apiVersion
        stream.u32(0); // enabledLayerCount
        stream.count(false); // ppEnabledLayerNames
        stream.u32(0); // enabledExtensionCount
        stream.count(false); // ppEnabledExtensionNames
        stream.count(false); // pAllocator
        stream.count(true); // pInstance
        stream.u64(instance_handle);
        stream
    }

    pub fn encode_destroy_instance(instance_handle: u64) -> Writer {
        let mut stream = Writer::new();
        stream.header(CMD_DESTROY_INSTANCE, 0);
        stream.u64(instance_handle);
        stream.count(false); // pAllocator
        stream
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HostInitEvidence {
        pub opcode: u32,
        pub status: i32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HostReplyRefusal {
        Opcode { found: u32 },
        Status { found: i32 },
        PointerCount { found: u64 },
        Instance { found: u64 },
    }

    /// Fixed per-HVC1-context record of the exact WDDM submissions whose K11
    /// host operation had already reached a terminal reply before
    /// `DxgkDdiSubmitCommand` arrived.  This is one scalar state machine, never
    /// an adapter queue or a reusable lookup token.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HostSubmissionState {
        observed: bool,
        last_fence: u32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HostSubmissionRefusal {
        Duplicate { fence: u32 },
        WentBackward { last: u32, found: u32 },
    }

    impl HostSubmissionState {
        pub const fn new() -> Self {
            Self {
                observed: false,
                last_fence: 0,
            }
        }

        pub const fn last_fence(&self) -> Option<u32> {
            if self.observed {
                Some(self.last_fence)
            } else {
                None
            }
        }

        /// Admit only a first, forward, or exact scheduler-marked resubmission.
        /// The first value may legitimately be zero after WDDM's u32 wrap, so
        /// presence is represented separately rather than by a sentinel.
        pub fn admit_host_completion(
            &mut self,
            fence: u32,
            resubmission: bool,
        ) -> Result<(), HostSubmissionRefusal> {
            if !self.observed {
                self.observed = true;
                self.last_fence = fence;
                return Ok(());
            }
            if fence == self.last_fence {
                return if resubmission {
                    Ok(())
                } else {
                    Err(HostSubmissionRefusal::Duplicate { fence })
                };
            }
            if !fence_is_forward(self.last_fence, fence) {
                return Err(HostSubmissionRefusal::WentBackward {
                    last: self.last_fence,
                    found: fence,
                });
            }
            self.last_fence = fence;
            Ok(())
        }
    }

    pub fn validate_create_instance_reply(
        raw: &[u8; HOST_CREATE_INSTANCE_REPLY_BYTES as usize],
        expected_instance: u64,
    ) -> Result<HostInitEvidence, HostReplyRefusal> {
        let opcode = u32::from_le_bytes(raw[0..4].try_into().unwrap());
        if opcode != CMD_CREATE_INSTANCE {
            return Err(HostReplyRefusal::Opcode { found: opcode });
        }
        let status = i32::from_le_bytes(raw[4..8].try_into().unwrap());
        if status != 0 {
            return Err(HostReplyRefusal::Status { found: status });
        }
        let pointer_count = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        if pointer_count != 1 {
            return Err(HostReplyRefusal::PointerCount {
                found: pointer_count,
            });
        }
        let instance = u64::from_le_bytes(raw[16..24].try_into().unwrap());
        if instance != expected_instance {
            return Err(HostReplyRefusal::Instance { found: instance });
        }
        Ok(HostInitEvidence { opcode, status })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const POOL: u64 = 4 * 1024 * 1024;
        const SLOT: u64 = 1024 * 1024;
        const HVR1: u64 = 80;

        #[test]
        fn reply_target_and_create_are_two_bounded_direct_streams() {
            let target = encode_set_reply_command_stream(
                0x1122_3344,
                0x0102_0304_0506_0708,
                HOST_CREATE_INSTANCE_REPLY_BYTES,
            );
            let bytes = target.finished().unwrap();
            assert_eq!(bytes.len(), 36);
            assert_eq!(&bytes[0..8], &[178, 0, 0, 0, 0, 0, 0, 0]);
            assert_eq!(&bytes[8..16], &1u64.to_le_bytes());
            assert_eq!(&bytes[16..20], &0x1122_3344u32.to_le_bytes());
            assert_eq!(&bytes[20..28], &0x0102_0304_0506_0708u64.to_le_bytes());
            assert_eq!(&bytes[28..36], &24u64.to_le_bytes());

            let stream = encode_create_instance(0x8877_6655_4433_2211);
            let bytes = stream.finished().unwrap();
            assert_eq!(bytes.len(), 128);
            assert_eq!(&bytes[0..8], &[0, 0, 0, 0, 1, 0, 0, 0]);
            // VkApplicationInfo.apiVersion sits after sType(4)+pNext(8)+
            // appName size(8)+appVer(4)+engName size(8)+engVer(4) inside the
            // app-info body that starts at offset 40.
            assert_eq!(&bytes[76..80], &API_VERSION_1_4.to_le_bytes());
            assert_eq!(&bytes[120..128], &0x8877_6655_4433_2211u64.to_le_bytes());
        }

        #[test]
        fn ring_zero_control_admits_one_owner_at_a_time() {
            let mut state = RingZeroControlState::new();
            assert_eq!(state.try_acquire(), Ok(()));
            assert_eq!(state.try_acquire(), Err(RingZeroControlRefusal::Occupied));
            assert!(state.release());
            assert!(!state.release());
            assert_eq!(state.try_acquire(), Ok(()));
        }

        #[test]
        fn generated_reply_uses_only_the_private_context_resource() {
            let mut payload = [0xAA; 24];
            payload[12..16].fill(0);
            patch_private_reply_resource(&mut payload, 12, 0x1122_3344).expect("private patch");
            assert_eq!(&payload[12..16], &0x1122_3344u32.to_le_bytes());

            assert_eq!(
                patch_private_reply_resource(&mut payload, 12, 0x5566_7788),
                Err(ReplyOperandRefusal::OperandNotPlaceholder)
            );
            assert_eq!(
                patch_private_reply_resource(&mut payload, 24, 1),
                Err(ReplyOperandRefusal::OffsetOutsidePayload)
            );
            assert_eq!(
                patch_private_reply_resource(&mut [0; 4], 0, 0),
                Err(ReplyOperandRefusal::ResourceIdZero)
            );
        }

        #[test]
        fn destroy_stream_is_exact_and_replyless() {
            let stream = encode_destroy_instance(0x8877_6655_4433_2211);
            let bytes = stream.finished().unwrap();
            assert_eq!(bytes.len(), 24);
            assert_eq!(&bytes[0..8], &[1, 0, 0, 0, 0, 0, 0, 0]);
            assert_eq!(&bytes[8..16], &0x8877_6655_4433_2211u64.to_le_bytes());
            assert_eq!(&bytes[16..24], &0u64.to_le_bytes());
        }

        #[test]
        fn all_four_physical_slots_admit_the_exact_finite_reply() {
            for slot in 0..4u64 {
                let offset = slot * SLOT;
                let range = admit_reply_range(POOL, SLOT, offset, HVR1 + 4096, HVR1, 56)
                    .expect("one exact slot");
                assert_eq!(range.payload_offset, offset + HVR1);
                assert_eq!(range.final_end, offset + HVR1 + 56);
            }
        }

        #[test]
        fn range_refuses_zero_unaligned_oversize_overflow_and_short_capacity() {
            assert_eq!(
                admit_reply_range(POOL, SLOT, 0, 0, HVR1, 56),
                Err(ReplyRangeRefusal::ZeroCapacity)
            );
            assert_eq!(
                admit_reply_range(POOL, SLOT, 1, HVR1 + 56, HVR1, 56),
                Err(ReplyRangeRefusal::SlotMisaligned)
            );
            assert_eq!(
                admit_reply_range(POOL, SLOT, 0, SLOT + 1, HVR1, 56),
                Err(ReplyRangeRefusal::CapacityExceedsSlot)
            );
            assert_eq!(
                admit_reply_range(POOL, SLOT, u64::MAX - SLOT + 1, SLOT, HVR1, 56),
                Err(ReplyRangeRefusal::ArithmeticOverflow)
            );
            assert_eq!(
                admit_reply_range(POOL, SLOT, POOL, HVR1 + 56, HVR1, 56),
                Err(ReplyRangeRefusal::RangeExceedsPool)
            );
            assert_eq!(
                admit_reply_range(POOL, SLOT, 0, HVR1 + 24, HVR1, 56),
                Err(ReplyRangeRefusal::FinalReplyExceedsRange)
            );
        }

        fn reply(opcode: u32, status: i32, pointer_count: u64, instance: u64) -> [u8; 24] {
            let mut raw = [0u8; 24];
            raw[0..4].copy_from_slice(&opcode.to_le_bytes());
            raw[4..8].copy_from_slice(&status.to_le_bytes());
            raw[8..16].copy_from_slice(&pointer_count.to_le_bytes());
            raw[16..24].copy_from_slice(&instance.to_le_bytes());
            raw
        }

        #[test]
        fn only_the_exact_host_create_reply_is_evidence() {
            let expected = 0x51;
            assert_eq!(
                validate_create_instance_reply(&reply(0, 0, 1, expected), expected),
                Ok(HostInitEvidence {
                    opcode: 0,
                    status: 0
                })
            );
            assert!(matches!(
                validate_create_instance_reply(&reply(2, 0, 1, expected), expected),
                Err(HostReplyRefusal::Opcode { .. })
            ));
            assert!(matches!(
                validate_create_instance_reply(&reply(0, -4, 1, expected), expected),
                Err(HostReplyRefusal::Status { .. })
            ));
            assert!(matches!(
                validate_create_instance_reply(&reply(0, 0, 0, expected), expected),
                Err(HostReplyRefusal::PointerCount { .. })
            ));
            assert!(matches!(
                validate_create_instance_reply(&reply(0, 0, 1, expected + 1), expected),
                Err(HostReplyRefusal::Instance { .. })
            ));
        }

        #[test]
        fn host_submission_state_is_one_context_local_forward_watermark() {
            let mut state = HostSubmissionState::new();
            assert_eq!(state.last_fence(), None);
            assert_eq!(state.admit_host_completion(0, false), Ok(()));
            assert_eq!(state.last_fence(), Some(0));
            assert_eq!(
                state.admit_host_completion(0, false),
                Err(HostSubmissionRefusal::Duplicate { fence: 0 })
            );
            assert_eq!(state.admit_host_completion(0, true), Ok(()));
            assert_eq!(state.admit_host_completion(7, false), Ok(()));
            assert_eq!(
                state.admit_host_completion(6, true),
                Err(HostSubmissionRefusal::WentBackward { last: 7, found: 6 })
            );
        }

        #[test]
        fn host_submission_state_accepts_wrap_without_a_zero_sentinel() {
            let mut state = HostSubmissionState::new();
            assert_eq!(state.admit_host_completion(u32::MAX, false), Ok(()));
            assert_eq!(state.admit_host_completion(0, false), Ok(()));
            assert_eq!(state.last_fence(), Some(0));
        }
    }
}

// ── Venus VkImageCreateInfo / VkMemoryAllocateInfo encoding ───────────────────
//
// R1002. Three image encoders and five memory encoders each re-emitted the whole
// struct body inline — about 25 `Writer` calls for an image — differing only in
// the pNext chain and a handful of scalars. Two of the three image bodies wrote
// `w.u32(0); // VK_IMAGE_TILING_OPTIMAL` as a BARE LITERAL while the third used
// the named `IMAGE_TILING_LINEAR`: the exact shape of the 39th-session defect (a
// tiling scalar hand-written in one of several copies of one struct), only
// inverted. That session's root cause was `IMAGE_TILING_LINEAR` being defined as
// 0, i.e. OPTIMAL, and it painted black.
//
// The pNext encoding is order-sensitive in a way no reader can infer from a call
// site: for the export-plus-dedicated allocation, the DEDICATED struct's fields
// are written inline first and the EXPORT struct's own `handleTypes` field comes
// AFTER them, because export's pNext points at dedicated. Getting that backwards
// produces a stream the host decodes as a different allocation.
//
// These live in `kmd_logic`, not in `venus.rs`, for one reason: they can then be
// pinned by golden-byte tests on the Linux host. The literals in those tests were
// produced by compiling the PRE-CHANGE inline sequences and printing their
// output, so they are an equivalence proof against the old code rather than a
// restatement of the new.

/// `VkCommandTypeEXT` for `vkAllocateMemory`.
pub const CMD_ALLOCATE_MEMORY: u32 = 21;
/// `VkCommandTypeEXT` for `vkCreateImage`.
pub const CMD_CREATE_IMAGE: u32 = 54;
/// `VK_COMMAND_GENERATE_REPLY_BIT_EXT`.
pub const CMD_FLAG_GENERATE_REPLY: u32 = 0x1;

pub const ST_MEMORY_ALLOCATE_INFO: i32 = 5;
pub const ST_IMAGE_CREATE_INFO: i32 = 14;
pub const ST_EXTERNAL_MEMORY_IMAGE_CREATE_INFO: i32 = 1000072001;
pub const ST_EXPORT_MEMORY_ALLOCATE_INFO: i32 = 1000072002;
pub const ST_MEMORY_DEDICATED_ALLOCATE_INFO: i32 = 1000127001;
/// `VK_STRUCTURE_TYPE_IMPORT_MEMORY_RESOURCE_INFO_MESA`.
///
/// Venus-private — it is not in `vulkan_core.h`. The value is venus-protocol's
/// own `vn_protocol_driver_defines.h:20`, which is the same header the ICD
/// encodes against, so guest and host agree by construction rather than by a
/// number copied out of a doc.
pub const ST_IMPORT_MEMORY_RESOURCE_INFO_MESA: i32 = 1000384002;

pub const IMAGE_TYPE_2D: u32 = 1;
pub const SAMPLE_COUNT_1: u32 = 0x0000_0001;
pub const SHARING_MODE_EXCLUSIVE: u32 = 0;

/// `VK_IMAGE_TILING_OPTIMAL`.
///
/// ⚠ Introducing this is half the point of R1002. It was a bare `w.u32(0)` with
/// a trailing comment at two of the three image encoders, while its sibling
/// `IMAGE_TILING_LINEAR` was a named constant — the 39th session's defect shape
/// inverted. There are now no bare tiling literals anywhere.
pub const IMAGE_TILING_OPTIMAL: u32 = 0;
/// `VK_IMAGE_TILING_LINEAR`.
///
/// ⚠ THE 39TH SESSION'S ROOT CAUSE. This was defined as 0 — which is OPTIMAL —
/// and the host built a tiled image the display importer read as linear: a black
/// screen with no error anywhere. It is 1. Do not "simplify" it.
pub const IMAGE_TILING_LINEAR: u32 = 1;

/// Everything the two live external image creates differ by. The rest of
/// `VkImageCreateInfo` — 2D, depth 1, one mip, one layer, 1 sample, exclusive
/// sharing, no queue families — is fixed by [`encode_image_create`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageCreateSpec {
    /// Exact `VkExternalMemoryHandleTypeFlags` for the mandatory external-memory
    /// create chain.
    pub external_handle_type: u32,
    pub flags: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub tiling: u32,
    pub usage: u32,
    pub initial_layout: u32,
    /// `VkImageFormatListCreateInfo` (`view_format_count == 0`: none). DXVK
    /// attaches the DXGI format family to every MUTABLE_FORMAT color texture,
    /// and the D5b alias must carry the identical list or the host may pick a
    /// different (compressed) layout for the same memory.
    pub view_format_count: u32,
    pub view_formats: [u32; 2],
}

pub const ST_IMAGE_FORMAT_LIST_CREATE_INFO: i32 = 1000147000;

/// Encode one `vkCreateImage` command stream. The pNext chain follows DXVK's
/// order: external-memory info first (when present), then the format list.
pub fn encode_image_create(device_id: u64, image_id: u64, spec: &ImageCreateSpec) -> Writer {
    let mut w = Writer::new();
    w.header(CMD_CREATE_IMAGE, CMD_FLAG_GENERATE_REPLY);
    w.u64(device_id);
    w.count(true);
    w.i32(ST_IMAGE_CREATE_INFO);
    let format_list = spec.view_format_count.min(2);
    if spec.external_handle_type != 0 {
        w.count(true);
        w.i32(ST_EXTERNAL_MEMORY_IMAGE_CREATE_INFO);
        // The rest of the chain is encoded before this struct's own fields.
        encode_format_list_pnext(&mut w, spec, format_list);
        w.u32(spec.external_handle_type);
    } else {
        // No external declaration — the D5b alias mirrors the ICD's `ext=0x0`
        // image exactly rather than declaring an empty handle set.
        encode_format_list_pnext(&mut w, spec, format_list);
    }
    w.u32(spec.flags);
    w.u32(IMAGE_TYPE_2D);
    w.u32(spec.format);
    w.u32(spec.width);
    w.u32(spec.height);
    w.u32(1); // depth
    w.u32(1); // mipLevels
    w.u32(1); // arrayLayers
    w.u32(SAMPLE_COUNT_1);
    w.u32(spec.tiling);
    w.u32(spec.usage);
    w.u32(SHARING_MODE_EXCLUSIVE);
    w.u32(0); // queueFamilyIndexCount
    w.count(false); // pQueueFamilyIndices
    w.u32(spec.initial_layout);
    w.count(false); // pAllocator
    w.count(true);
    w.u64(image_id);
    w
}

/// `VkImageFormatListCreateInfo` as a pNext link (or a NULL link).
fn encode_format_list_pnext(w: &mut Writer, spec: &ImageCreateSpec, count: u32) {
    if count == 0 {
        w.count(false);
        return;
    }
    w.count(true);
    w.i32(ST_IMAGE_FORMAT_LIST_CREATE_INFO);
    w.count(false); // its pNext
    w.u32(count);
    w.u64(u64::from(count)); // array_size(pViewFormats)
    for format in &spec.view_formats[..count as usize] {
        w.u32(*format); // vn_encode_VkFormat_array: packed i32s
    }
}

/// Which pNext chain a `VkMemoryAllocateInfo` carries.
///
/// ⚠ `ExportDedicated` is the order-sensitive one: the dedicated struct is
/// nested INSIDE the export struct's pNext, so its `image`/`buffer` fields are
/// written before the export struct's own `handleTypes`. That ordering is why
/// this is an enum and not four hand-written sequences.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MemoryPNext {
    /// Plain host-visible allocation.
    None,
    /// `VkExportMemoryAllocateInfo`.
    Export { handle_type: u32 },
    /// `VkExportMemoryAllocateInfo` -> `VkMemoryDedicatedAllocateInfo`.
    ExportDedicated { handle_type: u32, image: u64 },
    /// `VkImportMemoryResourceInfoMESA` — the memory's storage IS the named
    /// virtio resource, not a fresh host allocation. This is how a guest-backed
    /// blob becomes a `VkDeviceMemory`: QEMU turns the guest pages into a
    /// udmabuf and virglrenderer imports it
    /// (`virtio-gpu-virgl.c:951-964`), so the host GPU and the guest CPU
    /// address the same physical memory.
    ImportResource { resource_id: u32 },
    /// `VkImportMemoryResourceInfoMESA` -> `VkMemoryDedicatedAllocateInfo`:
    /// the D5b alias import. The admitted host's dma-buf image/buffer import is
    /// DEDICATED_ONLY — a bare import of the same blob returned
    /// `VK_ERROR_INVALID_EXTERNAL_HANDLE` (`PcVkImp`, 2026-09-02); DXVK's
    /// `forceDedicated` is what makes the ICD's own import of it succeed.
    /// Exactly one of `image`/`buffer` is nonzero.
    ImportResourceDedicated {
        resource_id: u32,
        image: u64,
        buffer: u64,
    },
}

/// The allocation chain for the KMD-owned OPTIMAL GDI image on the admitted
/// target.
///
/// The target's exact external-image query reports dedicated allocation as
/// preferred, not required.  Keeping the image out of `vkAllocateMemory` is
/// therefore valid and removes an unnecessary object-table dependency from the
/// command that creates the exportable memory.  The image is still the exact
/// object passed to the subsequent `vkBindImageMemory`.
pub const fn optimal_gdi_memory_pnext(handle_type: u32) -> MemoryPNext {
    MemoryPNext::Export { handle_type }
}

/// Everything the three live memory allocations differ by.
#[derive(Clone, Copy)]
pub struct MemoryAllocateSpec {
    pub pnext: MemoryPNext,
    pub size: u64,
    pub memory_type_index: u32,
}

/// Encode one `vkAllocateMemory` command stream.
pub fn encode_memory_allocate(device_id: u64, memory_id: u64, spec: &MemoryAllocateSpec) -> Writer {
    let mut w = Writer::new();
    w.header(CMD_ALLOCATE_MEMORY, CMD_FLAG_GENERATE_REPLY);
    w.u64(device_id);
    w.count(true);
    w.i32(ST_MEMORY_ALLOCATE_INFO);
    match spec.pnext {
        MemoryPNext::None => w.count(false),
        MemoryPNext::Export { handle_type } => {
            w.count(true);
            w.i32(ST_EXPORT_MEMORY_ALLOCATE_INFO);
            w.count(false);
            w.u32(handle_type);
        }
        MemoryPNext::ImportResource { resource_id } => {
            w.count(true);
            w.i32(ST_IMPORT_MEMORY_RESOURCE_INFO_MESA);
            w.count(false);
            w.u32(resource_id);
        }
        MemoryPNext::ImportResourceDedicated {
            resource_id,
            image,
            buffer,
        } => {
            w.count(true);
            w.i32(ST_IMPORT_MEMORY_RESOURCE_INFO_MESA);
            w.count(true);
            w.i32(ST_MEMORY_DEDICATED_ALLOCATE_INFO);
            w.count(false);
            w.u64(image);
            w.u64(buffer);
            // The IMPORT struct's own field, after the nested dedicated one.
            w.u32(resource_id);
        }
        MemoryPNext::ExportDedicated { handle_type, image } => {
            w.count(true);
            w.i32(ST_EXPORT_MEMORY_ALLOCATE_INFO);
            w.count(true);
            w.i32(ST_MEMORY_DEDICATED_ALLOCATE_INFO);
            w.count(false);
            w.u64(image);
            w.u64(0); // buffer
                      // The EXPORT struct's own field, after the nested dedicated one.
            w.u32(handle_type);
        }
    }
    w.u64(spec.size);
    w.u32(spec.memory_type_index);
    w.count(false); // pAllocator
    w.count(true);
    w.u64(memory_id);
    w
}

// ── Vulkan memory-type selection ──────────────────────────────────────────────

/// `VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT`.
pub const MEMORY_PROPERTY_DEVICE_LOCAL: u32 = 0x1;
/// `VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT`.
pub const MEMORY_PROPERTY_HOST_VISIBLE: u32 = 0x2;
/// `VK_MEMORY_PROPERTY_HOST_COHERENT_BIT`.
pub const MEMORY_PROPERTY_HOST_COHERENT: u32 = 0x4;
/// `VK_MAX_MEMORY_TYPES` — the fixed array length the host encodes in
/// `vkGetPhysicalDeviceMemoryProperties`.
pub const VK_MAX_MEMORY_TYPES: u32 = 32;

/// Which memory type a selector settled on, and whether it is the one that was
/// actually asked for.
///
/// Both selectors fall back rather than fail, which is the right policy — a
/// downgraded allocation usually still works. What was wrong is that they
/// returned a bare `Option<u32>`, so every caller read `Some(i)` as "the
/// requested property was satisfied" and a downgrade left no trace anywhere.
/// That is the ScanoutDiag=16 / SdgErr=2 defect class: a memory-type choice that
/// looked fine and was not.
///
/// `#[must_use]` plus two arms means a caller has to say what it does about the
/// downgrade. It does not prevent choosing a downgraded type — it prevents doing
/// so silently.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTypeChoice {
    /// Every requested property is present on this type.
    Exact(u32),
    /// The type is allowed by `memory_type_bits` but is missing at least one
    /// requested property.
    Downgraded(u32),
}

impl MemoryTypeChoice {
    /// The chosen `memoryTypeIndex`, whichever arm it came from.
    pub fn index(self) -> u32 {
        match self {
            Self::Exact(i) | Self::Downgraded(i) => i,
        }
    }
}

/// Pick the memory type an IMPORTED guest-page resource can actually be
/// allocated into: the one with the FEWEST property flags.
///
/// Not a preference — a measurement. `tools/udmabuf_import_probe.c`, run on the
/// host GPU outside the whole stack: NVIDIA's `vkGetMemoryFdProperties` mask
/// for a udmabuf is `0x9`, and only type 0 (`propertyFlags = 0x00`) imports;
/// the host-visible type returns `VK_ERROR_OUT_OF_DEVICE_MEMORY`. The guest
/// never needed a host-visible type here: the host does not map this memory,
/// the GUEST holds the CPU view of the same pages.
///
/// `None` when `memory_type_bits` allows nothing, which is a refusal, not a
/// fallback — importing into a type the host rejects fails the allocate and
/// would otherwise look like a transport error.
pub fn choose_importable_memory_type(
    memory_type_flags: &[u32],
    memory_type_count: u32,
    memory_type_bits: u32,
) -> Option<u32> {
    let count = memory_type_count.min(VK_MAX_MEMORY_TYPES) as usize;
    let mut best: Option<(u32, u32)> = None;
    for index in 0..count.min(memory_type_flags.len()) {
        if memory_type_bits & (1u32 << index) == 0 {
            continue;
        }
        let flags = memory_type_flags[index];
        let weight = flags.count_ones();
        if best.is_none_or(|(_, w)| weight < w) {
            best = Some((index as u32, weight));
            if weight == 0 {
                break;
            }
        }
    }
    best.map(|(index, _)| index)
}

/// Pick a HOST_VISIBLE memory type, preferring one that is also HOST_COHERENT.
///
/// `Downgraded` means HOST_VISIBLE but NOT HOST_COHERENT, which for a MAPPABLE
/// scanout blob means the guest's writes need explicit flushes that nothing
/// issues.
pub fn choose_host_visible_memory_type(
    memory_type_flags: &[u32],
    memory_type_count: u32,
    memory_type_bits: u32,
) -> Option<MemoryTypeChoice> {
    let mut fallback = None;
    let mut i = 0;
    while i < memory_type_count && i < VK_MAX_MEMORY_TYPES && (i as usize) < memory_type_flags.len()
    {
        if (memory_type_bits & (1u32 << i)) != 0 {
            let flags = memory_type_flags[i as usize];
            if (flags & MEMORY_PROPERTY_HOST_VISIBLE) != 0 {
                if fallback.is_none() {
                    fallback = Some(i);
                }
                if (flags & MEMORY_PROPERTY_HOST_COHERENT) != 0 {
                    return Some(MemoryTypeChoice::Exact(i));
                }
            }
        }
        i += 1;
    }
    fallback.map(MemoryTypeChoice::Downgraded)
}

/// Pick a DEVICE_LOCAL memory type, in three tiers.
///
/// The tier order is load-bearing and must not be reordered:
/// 1. device-local and NOT host-visible — the real VRAM type;
/// 2. device-local (host-visible too, i.e. a BAR/ReBAR type);
/// 3. the first allowed type at all, device-local or not.
///
/// Tiers 1 and 2 are both `Exact`: the requested property is DEVICE_LOCAL and
/// both have it, so tier 1 is a preference inside the same answer, not a
/// downgrade. Tier 3 is the downgrade, and it is just the lowest set bit of
/// `memory_type_bits`: on a host whose `memoryTypeBits` for an OPTIMAL GDI image
/// contains only a host-visible type, the old signature reported success and the
/// "device-local dedicated memory" contract in the caller's own doc comment was
/// silently false.
pub fn choose_device_local_memory_type(
    memory_type_flags: &[u32],
    memory_type_count: u32,
    memory_type_bits: u32,
) -> Option<MemoryTypeChoice> {
    let limit = |i: u32| {
        i < memory_type_count && i < VK_MAX_MEMORY_TYPES && (i as usize) < memory_type_flags.len()
    };
    let mut fallback = None;
    let mut i = 0;
    while limit(i) {
        if (memory_type_bits & (1u32 << i)) != 0 {
            let flags = memory_type_flags[i as usize];
            if fallback.is_none() {
                fallback = Some(i);
            }
            if (flags & MEMORY_PROPERTY_DEVICE_LOCAL) != 0
                && (flags & MEMORY_PROPERTY_HOST_VISIBLE) == 0
            {
                return Some(MemoryTypeChoice::Exact(i));
            }
        }
        i += 1;
    }
    i = 0;
    while limit(i) {
        if (memory_type_bits & (1u32 << i)) != 0 {
            let flags = memory_type_flags[i as usize];
            if (flags & MEMORY_PROPERTY_DEVICE_LOCAL) != 0 {
                return Some(MemoryTypeChoice::Exact(i));
            }
        }
        i += 1;
    }
    fallback.map(MemoryTypeChoice::Downgraded)
}

/// Pick a strictly non-host-visible DEVICE_LOCAL memory type.
///
/// HVM1 role 4 promises that no CPU mapping exists.  The ordinary device-local
/// selector above deliberately accepts a BAR/ReBAR type as an exact answer and
/// eventually accepts an arbitrary allowed type as a named downgrade.  Neither
/// answer is legal for role 4: a HOST_VISIBLE type would make the allocation
/// Lock-capable in fact even though its fixed HVM1 contract says otherwise, and
/// a non-device-local fallback would simply be a different memory class.
///
/// There is therefore no tier or downgrade here.  `None` is the only truthful
/// result when the host does not expose an allowed pure-VRAM type.
pub fn choose_strict_device_local_memory_type(
    memory_type_flags: &[u32],
    memory_type_count: u32,
    memory_type_bits: u32,
) -> Option<MemoryTypeChoice> {
    let limit = |i: u32| {
        i < memory_type_count && i < VK_MAX_MEMORY_TYPES && (i as usize) < memory_type_flags.len()
    };
    let mut i = 0;
    while limit(i) {
        if (memory_type_bits & (1u32 << i)) != 0 {
            let flags = memory_type_flags[i as usize];
            if (flags & MEMORY_PROPERTY_DEVICE_LOCAL) != 0
                && (flags & MEMORY_PROPERTY_HOST_VISIBLE) == 0
            {
                return Some(MemoryTypeChoice::Exact(i));
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {

    // ── R1002 golden bytes ────────────────────────────────────────────────
    //
    // Produced by compiling the PRE-CHANGE inline encoder sequences against
    // this same `Writer` and printing their output, so these literals are an
    // equivalence proof against the old code rather than a restatement of the
    // new. Regenerating them from the current encoder would make the tests
    // circular and worthless.
    //
    // Fixed handles and geometry so the arrays are stable: device
    // 0x1111222233334444, image 0x5555666677778888, memory 0x9999AAAABBBBCCCC,
    // 1896x1030 (the live DWM primary), size 0x800000, memoryTypeIndex 7.
    const GOLD_DEVICE: u64 = 0x1111_2222_3333_4444;
    const GOLD_IMAGE: u64 = 0x5555_6666_7777_8888;
    const GOLD_MEMORY: u64 = 0x9999_AAAA_BBBB_CCCC;
    const GOLD_SIZE: u64 = 0x0080_0000;
    const GOLD_MTI: u32 = 7;

    // GOLDEN_LINEAR_SCANOUT_IMAGE (140 bytes)
    const GOLDEN_LINEAR_SCANOUT_IMAGE: &[u8] = &[
        0x36, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0xe3, 0x9b, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x2c, 0x00, 0x00, 0x00, 0x68, 0x07, 0x00, 0x00, 0x06, 0x04, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x88, 0x77,
        0x77, 0x66, 0x66, 0x55, 0x55,
    ];
    // GOLDEN_OPTIMAL_GDI_IMAGE (140 bytes)
    const GOLDEN_OPTIMAL_GDI_IMAGE: &[u8] = &[
        0x36, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0xe3, 0x9b, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x2c, 0x00, 0x00, 0x00, 0x68, 0x07, 0x00, 0x00, 0x06, 0x04, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x17, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x88, 0x77,
        0x77, 0x66, 0x66, 0x55, 0x55,
    ];
    // GOLDEN_MEMORY_PLAIN (72 bytes)
    const GOLDEN_MEMORY_PLAIN: &[u8] = &[
        0x15, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0x99, 0x99,
    ];
    // GOLDEN_MEMORY_EXPORT (88 bytes)
    const GOLDEN_MEMORY_EXPORT: &[u8] = &[
        0x15, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x42, 0xe3, 0x9b, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0x99, 0x99,
    ];
    // GOLDEN_MEMORY_EXPORT_DEDICATED (116 bytes)
    const GOLDEN_MEMORY_EXPORT_DEDICATED: &[u8] = &[
        0x15, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x42, 0xe3, 0x9b, 0x3b, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x19, 0xba, 0x9c, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x88, 0x88, 0x77, 0x77, 0x66, 0x66, 0x55, 0x55, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0x99, 0x99,
    ];
    /// The production LINEAR scan-out image. The frozen direct-primary path:
    /// wrong bytes here are a black desktop, which is how the 39th session
    /// started.
    #[test]
    fn linear_scanout_image_bytes_are_unchanged() {
        let w = encode_image_create(
            GOLD_DEVICE,
            GOLD_IMAGE,
            &ImageCreateSpec {
                external_handle_type: 0x0000_0200,
                flags: 0,
                format: 44, // VK_FORMAT_B8G8R8A8_UNORM
                width: 1896,
                height: 1030,
                tiling: IMAGE_TILING_LINEAR,
                usage: 0x1 | 0x2,
                initial_layout: 8, // PREINITIALIZED,
                view_format_count: 0,
                view_formats: [0; 2],
},
        );
        assert_eq!(w.finished(), Some(GOLDEN_LINEAR_SCANOUT_IMAGE));
    }

    #[test]
    fn optimal_gdi_image_bytes_are_unchanged() {
        let w = encode_image_create(
            GOLD_DEVICE,
            GOLD_IMAGE,
            &ImageCreateSpec {
                external_handle_type: 0x0000_0200,
                flags: 0x8, // MUTABLE_FORMAT
                format: 44,
                width: 1896,
                height: 1030,
                tiling: IMAGE_TILING_OPTIMAL,
                usage: 0x1 | 0x2 | 0x4 | 0x10,
                initial_layout: 0, // UNDEFINED,
                view_format_count: 0,
                view_formats: [0; 2],
},
        );
        assert_eq!(w.finished(), Some(GOLDEN_OPTIMAL_GDI_IMAGE));
    }

    #[test]
    fn plain_memory_allocate_bytes_are_unchanged() {
        let w = encode_memory_allocate(
            GOLD_DEVICE,
            GOLD_MEMORY,
            &MemoryAllocateSpec {
                pnext: MemoryPNext::None,
                size: GOLD_SIZE,
                memory_type_index: GOLD_MTI,
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_MEMORY_PLAIN));
    }

    #[test]
    fn export_memory_allocate_bytes_are_unchanged() {
        let w = encode_memory_allocate(
            GOLD_DEVICE,
            GOLD_MEMORY,
            &MemoryAllocateSpec {
                pnext: MemoryPNext::Export {
                    handle_type: 0x0000_0200,
                },
                size: GOLD_SIZE,
                memory_type_index: GOLD_MTI,
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_MEMORY_EXPORT));
    }

    #[test]
    fn optimal_gdi_memory_export_has_no_dedicated_image_dependency() {
        assert!(matches!(
            optimal_gdi_memory_pnext(0x0000_0200),
            MemoryPNext::Export {
                handle_type: 0x0000_0200
            }
        ));
    }

    /// Not a golden-byte tautology: `VkImportMemoryResourceInfoMESA` and
    /// `VkExportMemoryAllocateInfo` have the SAME wire shape as a pNext element
    /// — non-null pointer, sType, null pNext, one u32 — so the two streams must
    /// differ in exactly two words, the sType and that u32. Anything else means
    /// the import arm emitted a different chain shape than the encoder venus
    /// decodes with.
    #[test]
    fn import_resource_differs_from_export_only_in_stype_and_payload() {
        let mk = |pnext| {
            encode_memory_allocate(
                GOLD_DEVICE,
                GOLD_MEMORY,
                &MemoryAllocateSpec {
                    pnext,
                    size: GOLD_SIZE,
                    memory_type_index: GOLD_MTI,
                },
            )
        };
        let export_w = mk(MemoryPNext::Export {
            handle_type: 0x0000_0200,
        });
        let import_w = mk(MemoryPNext::ImportResource {
            resource_id: 0x0000_0201,
        });
        let export = export_w.finished().expect("encoder must not overflow");
        let import = import_w.finished().expect("encoder must not overflow");
        assert_eq!(export.len(), import.len());
        assert_eq!(export.len() % 4, 0);
        let word = |b: &[u8], i: usize| {
            u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]])
        };
        let mut differing = 0;
        for i in 0..export.len() / 4 {
            if word(export, i) == word(import, i) {
                continue;
            }
            differing += 1;
            let v = word(import, i);
            assert!(
                v == ST_IMPORT_MEMORY_RESOURCE_INFO_MESA as u32 || v == 0x0000_0201,
                "word {i} differs but is neither the sType nor the resource id: {v:#x}"
            );
        }
        assert_eq!(
            differing, 2,
            "expected exactly the sType word and the payload word to differ"
        );
    }

    #[test]
    fn importable_memory_type_prefers_the_fewest_property_flags() {
        // Type 0 has no flags at all; 3 is HOST_VISIBLE|HOST_COHERENT and is
        // the one the host refused for a udmabuf.
        let flags = [0x0, 0x1, 0x5, 0x7];
        assert_eq!(choose_importable_memory_type(&flags, 4, 0b1111), Some(0));
        // With type 0 masked out, the next-fewest wins rather than the first.
        assert_eq!(choose_importable_memory_type(&flags, 4, 0b1110), Some(1));
        assert_eq!(choose_importable_memory_type(&flags, 4, 0b1100), Some(2));
        // Nothing allowed is a refusal, not type 0.
        assert_eq!(choose_importable_memory_type(&flags, 4, 0), None);
        // Never returns an index at or above the host's reported count.
        assert_eq!(choose_importable_memory_type(&flags, 2, 0b1100), None);
    }

    #[test]
    fn import_resource_stype_is_the_venus_protocol_value() {
        // `vn_protocol_driver_defines.h:20`. A wrong sType is silently ignored
        // by the host decoder's `default:` arm, which would leave the memory a
        // plain host allocation with no import and no error anywhere.
        assert_eq!(ST_IMPORT_MEMORY_RESOURCE_INFO_MESA, 1000384002);
    }

    /// The order-sensitive one: the dedicated struct's image/buffer fields come
    /// BEFORE the export struct's own handleTypes, because export's pNext points
    /// at dedicated. Swapping them still compiles and still type-checks.
    #[test]
    fn export_dedicated_memory_allocate_keeps_its_nesting_order() {
        let w = encode_memory_allocate(
            GOLD_DEVICE,
            GOLD_MEMORY,
            &MemoryAllocateSpec {
                pnext: MemoryPNext::ExportDedicated {
                    handle_type: 0x0000_0200,
                    image: GOLD_IMAGE,
                },
                size: GOLD_SIZE,
                memory_type_index: GOLD_MTI,
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_MEMORY_EXPORT_DEDICATED));
    }

    /// The 39th session, as an assertion. `IMAGE_TILING_LINEAR` was 0 — which
    /// is OPTIMAL — and the desktop painted black with no error anywhere.
    #[test]
    fn linear_and_optimal_tiling_are_not_the_same_value() {
        assert_eq!(IMAGE_TILING_OPTIMAL, 0);
        assert_eq!(IMAGE_TILING_LINEAR, 1);
    }
    use super::*;

    /// Vectors taken from the production comments so they double as
    /// documentation. `1896 -> 7680` is the case `ddi/display.rs` states by name:
    /// it is the shipped `.117`-era pitch defect, where the host read every
    /// scanout row short.
    #[test]
    fn cross_adapter_pitch_vectors() {
        assert_eq!(cross_adapter_pitch(0), 0);
        assert_eq!(cross_adapter_pitch(1), 256);
        assert_eq!(cross_adapter_pitch(64), 256);
        assert_eq!(cross_adapter_pitch(65), 512);
        assert_eq!(cross_adapter_pitch(1896), 7680);
        assert_eq!(cross_adapter_pitch(1920), 7680);
        assert_eq!(cross_adapter_pitch(1952), 7936);
        assert_eq!(cross_adapter_pitch(u32::MAX), 0xFFFF_FF00);
    }

    /// A 256-byte-aligned width is already a multiple of the alignment, so the
    /// function must be idempotent rather than adding another 256 bytes.
    #[test]
    fn cross_adapter_pitch_is_idempotent_on_aligned_widths() {
        for width in [64u32, 128, 320, 1920, 3840] {
            let pitch = cross_adapter_pitch(width);
            assert_eq!(pitch % CROSS_ADAPTER_PITCH_ALIGN, 0, "width {width}");
            assert!(pitch >= width * 4, "width {width} pitch {pitch}");
            assert!(
                pitch - width * 4 < CROSS_ADAPTER_PITCH_ALIGN,
                "width {width}"
            );
        }
    }

    #[test]
    fn round_up_page_vectors() {
        assert_eq!(round_up_page(0), 0);
        assert_eq!(round_up_page(1), 4096);
        assert_eq!(round_up_page(4096), 4096);
        assert_eq!(round_up_page(4097), 8192);
        assert_eq!(round_up_page(u64::MAX), 0xFFFF_FFFF_FFFF_F000);
    }

    /// The saturating add is the whole point of this copy: the near-`u64::MAX`
    /// input must clamp downward, never wrap to 0 and hand a zero-byte mapping
    /// to `MmMapLockedPagesSpecifyCache`.
    #[test]
    fn round_up_page_saturates_instead_of_wrapping() {
        for n in [u64::MAX, u64::MAX - 1, u64::MAX - 4094, u64::MAX - 4095] {
            assert_eq!(round_up_page(n), 0xFFFF_FFFF_FFFF_F000, "n {n:#x}");
        }
        // One page below the clamp still rounds normally.
        assert_eq!(round_up_page(u64::MAX - 4096), 0xFFFF_FFFF_FFFF_F000);
        assert_eq!(round_up_page(0xFFFF_FFFF_FFFF_EFFF), 0xFFFF_FFFF_FFFF_F000);
    }

    /// The exact boundary the MDL bound rests on: a transfer that ends on the
    /// last mapped byte is legal, one byte more is not.
    #[test]
    fn window_range_accepts_exact_fit_and_rejects_one_past() {
        assert_eq!(window_range(4096, 0, 4096), Some(0));
        assert_eq!(window_range(4096, 4095, 1), Some(4095));
        assert_eq!(window_range(4096, 0, 4097), None);
        assert_eq!(window_range(4096, 4096, 1), None);
        assert_eq!(window_range(4096, 1, 4096), None);
    }

    /// A zero-byte op inside the window is fine; a zero-byte op at the far end
    /// is still bounded by `len`.
    #[test]
    fn window_range_handles_empty_ranges() {
        assert_eq!(window_range(4096, 0, 0), Some(0));
        assert_eq!(window_range(4096, 4096, 0), Some(4096));
        assert_eq!(window_range(4096, 4097, 0), None);
        assert_eq!(window_range(0, 0, 0), Some(0));
        assert_eq!(window_range(0, 0, 1), None);
    }

    /// The overflow case is the whole reason this is checked arithmetic:
    /// `MdlOffset << 12` is a guest/VidMm-supplied quantity, so `offset + bytes`
    /// must not be allowed to wrap and land back inside the window.
    #[test]
    fn window_range_rejects_wrapping_offsets() {
        assert_eq!(window_range(4096, u64::MAX, 1), None);
        assert_eq!(window_range(4096, u64::MAX - 4095, 4096), None);
        assert_eq!(window_range(u64::MAX, u64::MAX, 1), None);
    }

    #[test]
    fn pfn_physical_address_vectors() {
        assert_eq!(Pfn(0).physical_address(), Some(0));
        assert_eq!(Pfn(1).physical_address(), Some(4096));
        assert_eq!(Pfn(0x1234).physical_address(), Some(0x1234 * 4096));
        // Largest PFN whose address still has bit 63 clear.
        assert_eq!(
            Pfn((1 << 51) - 1).physical_address(),
            Some(0x7FFF_FFFF_FFFF_F000)
        );
    }

    /// The multiply overflows at 2^52 pages (2^52 * 4096 == 2^64), which is the
    /// case `checked_shl(12)` could never catch.
    #[test]
    fn pfn_physical_address_rejects_overflow() {
        assert_eq!(Pfn(1 << 52).physical_address(), None);
        assert_eq!(Pfn(u64::MAX).physical_address(), None);
    }

    /// Deliberate deviation from the review's wording, which asked for both
    /// "2^52 - 1 returns the right address" AND "the sign bit is rejected":
    /// (2^52 - 1) * 4096 == 0xFFFF_FFFF_FFFF_F000, whose bit 63 IS set, so the
    /// two rules cannot both hold for that input. The sign rule wins, because
    /// the value's only consumer is an i64 QuadPart handed to MmMapIoSpace.
    #[test]
    fn pfn_physical_address_rejects_the_sign_bit() {
        assert_eq!(Pfn((1 << 52) - 1).physical_address(), None);
        assert_eq!(Pfn(1 << 51).physical_address(), None);
    }

    // ⛔ The three `meta_layout_*` tests were DELETED with the type they tested
    // (K4, 2026-08-10 — see the tombstone above `SeqRead`). They are NOT
    // re-homed: a test whose subject is gone is not coverage, it is the
    // appearance of coverage. The rule they defended — refuse a length that is
    // not exactly one real layout, never read a prefix — is now tested against
    // the record that actually crosses the seam, in
    // `allocation_identity::tests` — `hwa2_private_data_is_an_exact_length_gate`
    // and its HVM1/HOC1 siblings.

    #[test]
    fn seq_read_accepts_only_an_even_unchanged_sequence() {
        assert_eq!(seq_read(0, 0), SeqRead::Stable);
        assert_eq!(seq_read(2, 2), SeqRead::Stable);
        assert_eq!(seq_read(u32::MAX - 1, u32::MAX - 1), SeqRead::Stable);
    }

    #[test]
    fn seq_read_retries_on_an_in_flight_or_landed_publish() {
        // Writer held the descriptor when the read started.
        assert_eq!(seq_read(1, 1), SeqRead::Retry);
        assert_eq!(seq_read(3, 4), SeqRead::Retry);
        // A publish landed during the read (the republish tear this fixes).
        assert_eq!(seq_read(2, 4), SeqRead::Retry);
        assert_eq!(seq_read(2, 3), SeqRead::Retry);
        // Wrap is still a change.
        assert_eq!(seq_read(u32::MAX - 1, 0), SeqRead::Retry);
    }

    /// A simulated writer that never stops must exhaust the bound rather than
    /// spin: the reader is PASSIVE, the publishers can be at raised IRQL.
    #[test]
    fn seq_read_bound_terminates_against_a_live_writer() {
        let mut attempts = 0;
        let mut settled = false;
        while attempts < SEQ_READ_ATTEMPTS {
            attempts += 1;
            // Always odd => always Retry.
            if seq_read(2 * attempts + 1, 2 * attempts + 1) == SeqRead::Stable {
                settled = true;
                break;
            }
        }
        assert!(!settled);
        assert_eq!(attempts, SEQ_READ_ATTEMPTS);
    }

    /// Both moved functions are `const fn`, so a future edit that reaches for
    /// runtime state stops compiling here rather than in `kmd_render`.
    #[test]
    fn both_helpers_are_const_evaluable() {
        const PITCH: u32 = cross_adapter_pitch(1896);
        const PAGES: u64 = round_up_page(4097);
        assert_eq!(PITCH, 7680);
        assert_eq!(PAGES, 8192);
    }

    /// The four sites `ScanoutFormat` replaces, reproduced by hand from the
    /// pre-R503 code so this test fails if the enum ever changes which formats
    /// are accepted or what they encode to.
    ///
    /// Site 1, `display.rs::virtio_scanout_format` (the wire-format converter):
    ///     0 | 88 => B8G8R8X8 (2), 28 => R8G8B8A8 (67), 87 => B8G8R8A8 (1),
    ///     _ => None
    /// Site 2, `display.rs`'s two uses of a function-local
    ///     `const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87`.
    /// Site 3, `display.rs`'s direct-scan-out allowlist:
    ///     `matches!(dxgi_format, 28 | 87 | 88)` — note 0 is REJECTED here.
    /// Site 4, `create_allocation.rs`'s copy gate: the same {28, 87, 88} set.
    #[test]
    fn scanout_format_reproduces_all_four_pre_r503_sites() {
        fn site1_converter(dxgi: u32) -> Option<u32> {
            match dxgi {
                0 | 88 => Some(2),
                28 => Some(67),
                87 => Some(1),
                _ => None,
            }
        }
        fn site3_and_4_allowlist(dxgi: u32) -> bool {
            matches!(dxgi, 28 | 87 | 88)
        }

        // Every value the review names, plus the neighbours most likely to be
        // added by mistake.
        for dxgi in [0u32, 28, 87, 88, 10, 24, 91, 93, 1, 2, 67, 134, u32::MAX] {
            assert_eq!(
                ScanoutFormat::from_dxgi_or_legacy_zero(dxgi).map(ScanoutFormat::virtio),
                site1_converter(dxgi),
                "converter disagrees for DXGI {dxgi}"
            );
            assert_eq!(
                ScanoutFormat::from_dxgi(dxgi).is_some(),
                site3_and_4_allowlist(dxgi),
                "validator acceptance set disagrees for DXGI {dxgi}"
            );
        }

        // Site 2: the LINEAR fallback's hard-coded 87.
        assert_eq!(ScanoutFormat::Bgra8.dxgi(), 87);

        // The strict set is a subset of the converter set, so a format that
        // passes a validator always has a virtio encoding — that is the
        // "unrepresentable" half of the guarantee.
        for dxgi in 0..=256u32 {
            if ScanoutFormat::from_dxgi(dxgi).is_some() {
                assert!(ScanoutFormat::from_dxgi_or_legacy_zero(dxgi).is_some());
            }
        }

        // The legacy zero arm is deliberately not a round trip.
        assert_eq!(
            ScanoutFormat::from_dxgi_or_legacy_zero(0),
            Some(ScanoutFormat::Bgrx8)
        );
        assert_eq!(ScanoutFormat::from_dxgi(0), None);
        assert_eq!(ScanoutFormat::Bgrx8.dxgi(), 88);
    }

    /// The vectors the review names, plus the shipped mode.
    #[test]
    fn display_mode_from_host_validates_once() {
        assert_eq!(DisplayMode::from_host(0, 0), None);
        assert_eq!(DisplayMode::from_host(319, 240), None);
        assert_eq!(DisplayMode::from_host(320, 239), None);
        // The 320x240 boundary is INCLUSIVE, matching the pre-R512
        // `>= 320 && >= 240` test exactly.
        let min = DisplayMode::from_host(320, 240).expect("320x240 is the minimum");
        assert_eq!((min.width(), min.height()), (320, 240));
        let shipped = DisplayMode::from_host(1896, 1066).expect("the shipped primary");
        assert_eq!((shipped.width(), shipped.height()), (1896, 1066));
        let fallback = DisplayMode::from_host(1920, 1080).expect("the fallback mode");
        assert_eq!(<(u32, u32)>::from(fallback), (1920, 1080));
        // DspMd's packed form must not change.
        assert_eq!(shipped.packed(), (1896 << 16) | 1066);
        assert_eq!(fallback.packed(), (1920 << 16) | 1080);
        // A height whose low 16 bits would alias must still round-trip its
        // width, since DspMd masks only the height.
        let wide = DisplayMode::from_host(4096, 2160).expect("4K");
        assert_eq!(wide.packed() >> 16, 4096);
    }

    /// `DisplayMode::FALLBACK`'s `None` arms are unreachable — this is what says
    /// so, instead of a const `panic!` inside a crate the kernel driver links.
    #[test]
    fn display_mode_fallback_is_the_documented_extent() {
        assert_eq!(
            <(u32, u32)>::from(DisplayMode::FALLBACK),
            (FALLBACK_DISPLAY_WIDTH, FALLBACK_DISPLAY_HEIGHT)
        );
        assert_eq!(<(u32, u32)>::from(DisplayMode::FALLBACK), (1920, 1080));
        // If either constant were edited below the floor the const `match` would
        // silently degrade to the minimum extent; this catches that.
        assert_eq!(
            DisplayMode::from_host(FALLBACK_DISPLAY_WIDTH, FALLBACK_DISPLAY_HEIGHT),
            Some(DisplayMode::FALLBACK)
        );
    }

    #[test]
    fn gate_pack_round_trips_and_keeps_the_flag_in_the_low_half() {
        for seq in [0u32, 1, 2, 0x7FFF_FFFF, 0x8000_0000, u32::MAX] {
            for active in [false, true] {
                let word = gate_pack(seq, active);
                assert_eq!(gate_seq(word), seq, "seq lost for {seq}/{active}");
                assert_eq!(gate_active(word), active, "flag lost for {seq}/{active}");
            }
        }
        // A fresh adapter (word 0) is generation 0, inactive — the same "no
        // programming outstanding" answer the bare AtomicU32 flag gave.
        assert_eq!(gate_pack(0, false), 0);
        assert!(!gate_active(0));
        // The VSync DPC tests only the low half, so a raised gate at ANY
        // generation reads active. This is the check that would fail if a reader
        // were left comparing the packed word against 0/1.
        assert!(gate_active(gate_pack(12345, true)));
        // ...and a CLEARED gate at a nonzero generation must read inactive even
        // though the word itself is nonzero. A missed reader here would suppress
        // VSync forever.
        assert!(!gate_active(gate_pack(12345, false)));
        assert_ne!(gate_pack(12345, false), 0);
    }

    /// The three transitions the gate's correctness rests on, as the
    /// compare-exchange operands the driver actually uses.
    #[test]
    fn gate_transitions_reject_a_stale_clear() {
        // Raise N -> N+1, active.
        let empty = gate_pack(0, false);
        let n1 = gate_pack(gate_seq(empty).wrapping_add(1), true);
        assert_eq!(n1, gate_pack(1, true));

        // The owner of interval 1 clears it: CAS (1,true) -> (1,false) matches.
        assert_eq!(n1, gate_pack(1, true));

        // A SECOND raise lands first, making the gate interval 2.
        let n2 = gate_pack(gate_seq(n1).wrapping_add(1), true);
        assert_eq!(n2, gate_pack(2, true));

        // Interval 1's completion now tries its clear. Its expected operand no
        // longer matches the live word, so the CAS fails and the gate stays
        // raised for interval 2 — which is the whole point.
        assert_ne!(n2, gate_pack(1, true));

        // Generation wrap must not alias a live interval with a stale one.
        let wrapped = gate_pack(u32::MAX, true);
        assert_eq!(gate_seq(wrapped).wrapping_add(1), 0);
        assert_ne!(gate_pack(0, true), wrapped);
    }

    // ── Writer ────────────────────────────────────────────────────────────────

    /// The KMD's `EXT_FULL` extension tier, verbatim from
    /// `kmd_render/src/virtio/venus.rs`. These strings decide the size of the
    /// largest stream the driver ever encodes, so the test carries its own copy:
    /// if the driver's list grows, this test still asserts the OLD number and the
    /// [`MAX_CMD_BYTES`] headroom must be recomputed deliberately.
    const EXT_FULL: [&[u8]; 5] = [
        b"VK_KHR_external_memory\0",
        b"VK_KHR_external_memory_fd\0",
        b"VK_KHR_image_format_list\0",
        b"VK_EXT_external_memory_dma_buf\0",
        b"VK_EXT_image_drm_format_modifier\0",
    ];

    /// Re-encode `vkCreateDevice` exactly as `create_venus_device` does, for the
    /// given extension list.
    fn encode_create_device(exts: &[&[u8]]) -> Writer {
        const CMD_CREATE_DEVICE: u32 = 11;
        const CMD_FLAG_GENERATE_REPLY: u32 = 1;
        const ST_DEVICE_CREATE_INFO: i32 = 3;
        const ST_DEVICE_QUEUE_CREATE_INFO: i32 = 2;

        let mut w = Writer::new();
        w.header(CMD_CREATE_DEVICE, CMD_FLAG_GENERATE_REPLY);
        w.u64(0xDEAD_BEEF); // VkPhysicalDevice
        w.count(true); // simple_pointer(pCreateInfo)
        w.i32(ST_DEVICE_CREATE_INFO);
        w.u64(0); // pNext
        w.u32(0); // flags
        w.u32(1); // queueCreateInfoCount
        w.count(true); // array_size(1)
        w.i32(ST_DEVICE_QUEUE_CREATE_INFO);
        w.u64(0); // pNext
        w.u32(0); // flags
        w.u32(0); // queueFamilyIndex
        w.u32(1); // queueCount
        w.count(true); // array_size(1)
        w.f32(1.0); // priority
        w.u32(0); // enabledLayerCount
        w.count(false);
        if exts.is_empty() {
            w.u32(0);
            w.count(false);
        } else {
            w.u32(exts.len() as u32);
            w.u64(exts.len() as u64);
            for ext in exts {
                w.u64(ext.len() as u64);
                w.bytes_padded(ext);
            }
        }
        w.count(false); // pEnabledFeatures
        w.count(false); // pAllocator
        w.count(true); // simple_pointer(pDevice)
        w.u64(0x1234); // VkDevice handle
        w
    }

    /// The number that sizes [`MAX_CMD_BYTES`]. The comment this replaces said
    /// "the largest is vkCreateDevice (~120 bytes)" and was wrong by 212 bytes,
    /// so the buffer's whole safety margin was arithmetic performed in prose.
    #[test]
    fn writer_ext_full_create_device_is_332_bytes() {
        let w = encode_create_device(&EXT_FULL);
        assert!(!w.overflowed());
        assert_eq!(w.len(), 332);
        // 144 bytes of struct plus a 188-byte extension block. The zero-extension
        // arm encodes the same two words for count and array size, so the
        // difference is exactly the five lengths plus the five padded names.
        let bare = encode_create_device(&[]);
        assert_eq!(bare.len(), 144);
        assert_eq!(w.len() - bare.len(), 188);
    }

    /// 512 - 332 = 180 bytes of slack. A 32-character extension name costs 44
    /// bytes (an 8-byte length plus 33 bytes padded to 36), so FOUR more still
    /// fit at 508 bytes and a fifth does not.
    ///
    /// Both the original finding ("a sixth extension bugchecks the guest") and
    /// the review that corrected it ("four more EXT_FULL strings" overflow) are
    /// wrong in the same direction. This is the measured boundary.
    #[test]
    fn writer_headroom_is_four_more_extension_names() {
        const BIG: &[u8] = b"VK_EXT_image_drm_format_modifier\0";
        let plus_four: [&[u8]; 9] = [
            EXT_FULL[0],
            EXT_FULL[1],
            EXT_FULL[2],
            EXT_FULL[3],
            EXT_FULL[4],
            BIG,
            BIG,
            BIG,
            BIG,
        ];
        let w = encode_create_device(&plus_four);
        assert!(!w.overflowed(), "four more names must still fit");
        assert_eq!(w.len(), 508);

        let plus_five: [&[u8]; 10] = [
            EXT_FULL[0],
            EXT_FULL[1],
            EXT_FULL[2],
            EXT_FULL[3],
            EXT_FULL[4],
            BIG,
            BIG,
            BIG,
            BIG,
            BIG,
        ];
        let w = encode_create_device(&plus_five);
        assert!(w.overflowed(), "a fifth extra name must be refused");
        assert!(w.finished().is_none());
    }

    /// Overflow must be sticky: a refused write must not leave a shorter, valid
    /// looking stream that a later small write could complete.
    #[test]
    fn writer_overflow_is_sticky_and_withholds_the_bytes() {
        let mut w = Writer::new();
        w.header(7, 0);
        assert_eq!(w.cmd_type(), 7);
        for _ in 0..MAX_CMD_BYTES {
            w.u64(0);
        }
        assert!(w.overflowed());
        assert!(w.finished().is_none());
        // A write that WOULD fit must not un-poison it.
        w.u32(1);
        assert!(w.finished().is_none());
        // The command type stays readable so the refusal can name the command.
        assert_eq!(w.cmd_type(), 7);
    }

    /// The exact boundary: a stream that fills the buffer to the last byte is
    /// valid; one byte more is not.
    #[test]
    fn writer_accepts_exactly_max_cmd_bytes() {
        let mut w = Writer::new();
        for _ in 0..MAX_CMD_BYTES / 8 {
            w.u64(0);
        }
        assert!(!w.overflowed());
        assert_eq!(w.len(), MAX_CMD_BYTES);
        assert_eq!(w.finished().map(|s| s.len()), Some(MAX_CMD_BYTES));

        w.u32(0);
        assert!(w.overflowed());
    }

    /// `bytes_padded` zero-fills to the next 4-byte boundary, and the padding is
    /// counted against the budget — a 33-byte name costs 36.
    #[test]
    fn writer_bytes_padded_pads_with_zeroes() {
        let mut w = Writer::new();
        w.bytes_padded(b"abcde");
        assert_eq!(w.len(), 8);
        assert_eq!(w.finished(), Some(&b"abcde\0\0\0"[..]));

        let mut w = Writer::new();
        w.bytes_padded(b"VK_EXT_image_drm_format_modifier\0");
        assert_eq!(w.len(), 36);
    }

    // ── Memory-type selection ────────────────────────────────────────────────

    /// The shape this box actually reports: a pure DEVICE_LOCAL type, a
    /// HOST_VISIBLE|HOST_COHERENT type, and a device-local BAR type.
    const NVIDIA_SHAPED: [u32; 3] = [
        MEMORY_PROPERTY_DEVICE_LOCAL,
        MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT,
        MEMORY_PROPERTY_DEVICE_LOCAL | MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT,
    ];

    /// Today's host takes the exact arm on both selectors — which is why the
    /// guest gate for R605 is "VnMtDown is ABSENT", not "VnMtDown is 0".
    #[test]
    fn memory_type_exact_on_the_shape_this_box_reports() {
        assert_eq!(
            choose_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b111),
            Some(MemoryTypeChoice::Exact(0))
        );
        assert_eq!(
            choose_host_visible_memory_type(&NVIDIA_SHAPED, 3, 0b111),
            Some(MemoryTypeChoice::Exact(1))
        );
    }

    /// The defect case: `memoryTypeBits` allows only a host-visible,
    /// non-device-local type. The old signature returned `Some(i)` and the
    /// caller's "device-local dedicated memory" contract was silently false.
    #[test]
    fn memory_type_downgrades_when_only_host_visible_is_allowed() {
        assert_eq!(
            choose_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b010),
            Some(MemoryTypeChoice::Downgraded(1))
        );
    }

    /// A device-local BAR type still satisfies DEVICE_LOCAL, so tier 2 is Exact.
    /// Tier 1 is a preference inside the same answer, not a downgrade.
    #[test]
    fn memory_type_device_local_bar_type_is_exact() {
        assert_eq!(
            choose_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b100),
            Some(MemoryTypeChoice::Exact(2))
        );
        // ...and tier 1 still wins when both are allowed.
        assert_eq!(
            choose_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b101),
            Some(MemoryTypeChoice::Exact(0))
        );
    }

    /// Role 4 has no BAR tier and no property downgrade.  The first pure-VRAM
    /// type wins; a host-visible DEVICE_LOCAL type is not an alternate answer.
    #[test]
    fn strict_device_local_never_selects_host_visible_or_fallback_memory() {
        assert_eq!(
            choose_strict_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b111),
            Some(MemoryTypeChoice::Exact(0))
        );
        assert_eq!(
            choose_strict_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b110),
            None
        );
        assert_eq!(
            choose_strict_device_local_memory_type(&NVIDIA_SHAPED, 3, 0b010),
            None
        );
        assert_eq!(
            choose_strict_device_local_memory_type(&NVIDIA_SHAPED, 3, 0),
            None
        );
    }

    /// HOST_VISIBLE without HOST_COHERENT is the silent half of the host-visible
    /// selector: a MAPPABLE scanout blob whose writes need flushes nobody issues.
    #[test]
    fn memory_type_host_visible_without_coherent_is_a_downgrade() {
        let flags = [MEMORY_PROPERTY_HOST_VISIBLE, MEMORY_PROPERTY_DEVICE_LOCAL];
        assert_eq!(
            choose_host_visible_memory_type(&flags, 2, 0b11),
            Some(MemoryTypeChoice::Downgraded(0))
        );
    }

    /// No allowed type has the property at all.
    #[test]
    fn memory_type_none_when_nothing_qualifies() {
        let flags = [MEMORY_PROPERTY_DEVICE_LOCAL, MEMORY_PROPERTY_DEVICE_LOCAL];
        assert_eq!(choose_host_visible_memory_type(&flags, 2, 0b11), None);
        assert_eq!(choose_device_local_memory_type(&flags, 2, 0), None);
    }

    /// `memory_type_count` and the array length both bound the scan; a host that
    /// reports a count larger than the array must not index past it.
    #[test]
    fn memory_type_scan_is_bounded_by_both_count_and_array() {
        let flags = [MEMORY_PROPERTY_DEVICE_LOCAL];
        assert_eq!(
            choose_device_local_memory_type(&flags, 32, u32::MAX),
            Some(MemoryTypeChoice::Exact(0))
        );
        // A count of 0 means the host reported nothing usable.
        assert_eq!(choose_device_local_memory_type(&flags, 0, u32::MAX), None);
    }

    /// Little-endian, and the header is two 4-byte words in encode order.
    #[test]
    fn writer_encodes_little_endian() {
        let mut w = Writer::new();
        w.header(0x1122_3344, 0x5566_7788);
        w.u64(0x0102_0304_0506_0708);
        w.f32(1.0);
        assert_eq!(
            w.finished(),
            Some(
                &[
                    0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55, 0x08, 0x07, 0x06, 0x05, 0x04,
                    0x03, 0x02, 0x01, 0x00, 0x00, 0x80, 0x3F,
                ][..]
            )
        );
    }
}

// ── Sorted-by-key splice (R712) ─────────────────────────────────────────────

/// Where an ascending block of keys belongs in an already-sorted slice, and how
/// many existing entries it displaces.
///
/// # Why this is here rather than in `kmd_render`
///
/// `PagingPteShadow::update_leaf` used to `retain` and then
/// `sort_unstable_by_key` a table bounded at 65,536 entries with a spinlock held
/// — and `KeAcquireSpinLockRaiseToDpc` raises to DISPATCH_LEVEL regardless of the
/// caller's IRQL. That is O(n log n) of unbounded work in a DISPATCH path, which
/// the project rule forbids, and its cost was invisible because only the
/// resulting LENGTH was recorded (`PgVp`).
///
/// The sort was unnecessary: the table is already sorted before the update, the
/// appended block is itself ascending, and it is confined to the exact VA range
/// `retain` just cleared. So one splice at the right index reproduces the sorted
/// result. That argument is easy to get subtly wrong for a partially-overlapping
/// range — and getting it wrong makes `resolve`'s `binary_search_by_key` return
/// `None`, which breaks eviction CONTENT. Hence: pure logic, moved out, with a
/// randomized oracle test against a reference `sort`.
///
/// Returns `(start, removed)`: the entries in `[start, start + removed)` are the
/// ones whose keys fall inside `[first_key, end_key)`, and the new block belongs
/// at `start`.
///
/// `keys` must be sorted ascending.
pub fn sorted_splice_range(keys: &[u64], first_key: u64, end_key: u64) -> (usize, usize) {
    let start = partition_point(keys, |k| k < first_key);
    let end = partition_point(keys, |k| k < end_key);
    (start, end - start)
}

/// `slice::partition_point` over a key slice, spelled out so this crate stays
/// free of any dependency on slice-method stabilisation in `no_std`.
fn partition_point<F: Fn(u64) -> bool>(keys: &[u64], pred: F) -> usize {
    let mut lo = 0usize;
    let mut hi = keys.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(keys[mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod sorted_splice_tests {
    extern crate alloc;
    use super::sorted_splice_range;
    use alloc::vec::Vec;

    /// Reference implementation: retain-then-sort, exactly what the KMD did
    /// before the splice replaced it.
    fn reference(
        existing: &[(u64, u64)],
        first: u64,
        end: u64,
        block: &[(u64, u64)],
    ) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = existing
            .iter()
            .copied()
            .filter(|(k, _)| *k < first || *k >= end)
            .collect();
        out.extend_from_slice(block);
        out.sort_by_key(|(k, _)| *k);
        out
    }

    /// Splice implementation, driven by `sorted_splice_range`.
    fn spliced(
        existing: &[(u64, u64)],
        first: u64,
        end: u64,
        block: &[(u64, u64)],
    ) -> Vec<(u64, u64)> {
        let keys: Vec<u64> = existing.iter().map(|(k, _)| *k).collect();
        let (start, removed) = sorted_splice_range(&keys, first, end);
        let mut out: Vec<(u64, u64)> = Vec::new();
        out.extend_from_slice(&existing[..start]);
        out.extend_from_slice(block);
        out.extend_from_slice(&existing[start + removed..]);
        out
    }

    /// Deterministic xorshift — `Math::random` is not available and a fixed seed
    /// makes a failure reproducible.
    fn next(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn splice_matches_retain_then_sort_over_random_ranges() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for case in 0..2000u64 {
            // A sorted existing table with gaps, so ranges can partially overlap.
            let n = (next(&mut seed) % 40) as usize;
            let mut existing: Vec<(u64, u64)> = Vec::new();
            let mut key = next(&mut seed) % 8;
            for i in 0..n {
                existing.push((key, key * 1000 + i as u64));
                key += 1 + next(&mut seed) % 4;
            }

            let first = next(&mut seed) % 80;
            let len = next(&mut seed) % 12;
            let end = first + len;
            // The appended block is ascending and confined to [first, end).
            let block: Vec<(u64, u64)> = (first..end).map(|k| (k, k * 7 + case)).collect();

            let want = reference(&existing, first, end, &block);
            let got = spliced(&existing, first, end, &block);
            assert_eq!(want, got, "case {case}: first={first} end={end}");
            // The result must be sorted — that is what `resolve`'s binary search
            // depends on.
            assert!(
                got.windows(2).all(|w| w[0].0 <= w[1].0),
                "case {case} unsorted"
            );
        }
    }

    #[test]
    fn empty_block_is_pure_removal() {
        let existing = [(1u64, 10u64), (2, 20), (5, 50), (9, 90)];
        let keys: Vec<u64> = existing.iter().map(|(k, _)| *k).collect();
        let (start, removed) = sorted_splice_range(&keys, 2, 6);
        assert_eq!((start, removed), (1, 2));
    }

    #[test]
    fn range_beyond_the_end_removes_nothing() {
        let existing = [(1u64, 10u64), (2, 20)];
        let keys: Vec<u64> = existing.iter().map(|(k, _)| *k).collect();
        assert_eq!(sorted_splice_range(&keys, 100, 200), (2, 0));
    }
}

/// Scan-out presentation-epoch ownership — the display-consumer half of
/// "when may Windows have this allocation back?" (ROADMAP defect 0ab-B).
///
/// # The invariant
///
/// A Helios scan-out is **not** a continuous scan-out. The host reads the bound
/// DMA-BUF exactly once per `RESOURCE_FLUSH` and at no other time, so a
/// presented buffer must be immutable from the moment it is published to the
/// host until that read has finished. Windows knows nothing about that read: it
/// retires a flip on the driver's own completion notifications and then hands
/// the buffer back to DXGI, which hands it to the app, which clears it to
/// opaque black for its next frame — while the host has still not read it. That
/// is the entirely-black published frame.
///
/// So a presentation may be released only when BOTH halves hold:
///
/// ```text
/// reuse_safe(epoch) = producer_venus_work_retired(epoch)
///                     AND host_reader_lease_ended(epoch)
/// ```
///
/// The producer half already exists (`VirtioGpu::note_wddm_submission`'s wire-
/// fence watermark). This module is the second half.
///
/// # The state machine
///
/// * Every presentation of a buffer to the host mints a **monotonically
///   increasing epoch**. Re-presenting a still-bound buffer mints a NEW epoch:
///   the app wrote it again, so it needs to be read again. A bind generation
///   would collapse those and is therefore not enough.
/// * `bound_epoch` is the epoch whose buffer the host is bound to right now. It
///   advances only when the display worker has actually published the binding
///   (a completed `SET_SCANOUT_BLOB`, or an already-bound re-present).
/// * A `RESOURCE_FLUSH` issued while `bound_epoch == E` proves, when its
///   response returns, that the host has read the buffer of epoch `E` — and, by
///   monotonicity, of every epoch below it. That is [`LeaseTracker::issue_flush`]
///   / [`LeaseTracker::complete_flush`], and the snapshot `E` is the typed token
///   the transport carries on the in-flight entry.
/// * `read_epoch` is the watermark: every epoch `<= read_epoch` has ended its
///   lease. It only ever moves forward ([`merge_read_epoch`]), so a completion
///   that arrives out of order is inert rather than corrupting.
/// * A successful bind of a DIFFERENT resource ends every older epoch's lease
///   without a read. The virtio control queue is strictly FIFO host-side, so a
///   returned `SET_SCANOUT_BLOB` proves every earlier-enqueued flush already
///   completed, and a flush enqueued later cannot read a resource that is no
///   longer the scan-out. **A completed bind is not a completed read** — it is a
///   proof that no read remains, which is a different (and weaker) statement,
///   and it is why this is the escape hatch and not the primary terminator.
/// * Failure, cancellation and teardown end leases explicitly and loudly. There
///   is deliberately NO timeout: a lease is never released on the theory that
///   the read has probably finished by now.
///
/// # Why the shipped driver mirrors this with atomics
///
/// The three edges run under three different locks: the mint is on the
/// `DxgkDdiSubmitCommand` DISPATCH path, the bind and the flush issue are on the
/// PASSIVE display worker under `scanout_mutex`, and the flush completion is in
/// the used-ring drain under `virtio_lock` — which may not take
/// `wddm_notify_lock`, because the established order is the reverse and
/// inverting it is a DIRQL deadlock. Every transition here is monotone, so the
/// KMD implements them as `fetch_add` / `fetch_max` on `AdapterContext` atomics
/// and calls the SAME predicates below.
///
/// # ⚠ WHAT 22.22.217.0 RETIRED, and what it kept
///
/// The **withholding** half above — holding `DXGK_INTERRUPT_DMA_COMPLETED` and
/// the CRTC_VSYNC primary address until the presentation's read finished — was
/// built, shipped and MEASURED INERT against the defect: a 2×2 lease ×
/// `BindFlushMode` factorial over 46 681 frames moved whole-flush black by
/// nothing in any cell (14.5–16.6 % everywhere). The reason is structural and is
/// now proven: the app's clear of a reclaimed buffer never travels in a WDDM DMA
/// buffer, so no completion-notification policy can order it. The driver
/// therefore gates a WDDM submission on its Venus watermark alone again.
///
/// The **epochs** are kept and are load-bearing for the replacement fix: the
/// ownership gate on the flush executor ([`surplus_republish`]), which refuses to
/// re-read a binding generation that has already been published while a newer
/// presentation is outstanding. So [`next_epoch`], [`merge_read_epoch`] and
/// [`lease_satisfied`] are shipped predicates; [`LeaseTracker`]'s FIFO/pending-
/// primary machinery below is the specification of the RETIRED withholding, kept
/// as the record of what was measured — it is no longer a model of shipped code.
/// Every item that models only the retired half says so on its own doc line.
pub mod scanout_retire {
    /// Mint the next nonzero direct-scanout bind sequence without wrapping.
    pub const fn next_bind_sequence(high_water: u64) -> Option<u64> {
        high_water.checked_add(1)
    }
}

#[cfg(test)]
mod scanout_retire_tests {
    use super::scanout_retire::next_bind_sequence;

    #[test]
    fn bind_sequence_exhaustion_never_wraps() {
        assert_eq!(next_bind_sequence(0), Some(1));
        assert_eq!(next_bind_sequence(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(next_bind_sequence(u64::MAX), None);
    }
}

/// Shared WDDM submission-fence ordering arithmetic used by K9 and K11.
pub mod scanout_lease {
    /// Whether `fence` is forward of `last` in the wrapping u32 sequence.
    pub const fn fence_is_forward(last: u32, fence: u32) -> bool {
        fence != last && fence.wrapping_sub(last) < 0x8000_0000
    }
}

#[cfg(test)]
mod scanout_lease_tests {
    use super::scanout_lease::fence_is_forward;

    #[test]
    fn fence_forwardness_survives_the_u32_wrap() {
        assert!(fence_is_forward(0, 1));
        assert!(!fence_is_forward(1, 1));
        assert!(!fence_is_forward(2, 1));
        assert!(fence_is_forward(u32::MAX, 0));
        assert!(fence_is_forward(u32::MAX - 1, 3));
        assert!(!fence_is_forward(0, 0x8000_0000));
        assert!(fence_is_forward(0, 0x7fff_ffff));
    }
}
/// Core-0116 native-fence object lifecycle, and the HNF1 private-driver-data
/// record it travels with.
///
/// Normative: `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` section 12.1 (lines
/// 3130-3195, the `HeliosNativeFencePddV1` byte table and the
/// create/open/wait/signal/CPU/close/multi-adapter sequence), section 10.2's
/// admission table (lines 990-1004), and section 17.6:4328-4333 ("Driver
/// global/local handles reference bounded native-fence objects directly; there
/// is no hash table, scan, name, or process-global discovery registry").
///
/// # Why the rules live here and not in `kmd_render`
///
/// `kmd_render` is a `panic = "abort"` `no_std` cdylib and cannot run a test, so
/// a `#[cfg(test)]` there is assurance that is not real (CLAUDE.md). Everything
/// in this module is a function of its arguments: the DDI bodies in
/// `kmd_render/src/ddi/native_fence.rs` do the pointer work and call in here for
/// every decision that has a right and a wrong answer.
///
/// # Bounded, with no discovery structure
///
/// Section 10.1 invariant 10 forbids any adapter- or process-global resource or
/// synchronization *discovery* structure. So this model has no table: identity
/// is the OS-delivered `hGlobalNativeFence` / `hLocalNativeFence`, which the KMD
/// sets to the object's own address. What is bounded is the *population*
/// ([`admit_create`] / [`admit_open`]) and the *validity* of each object, which
/// is a single adapter-wide epoch ([`epoch_is_current`]) rather than an
/// enumeration — reset invalidates every object with one store and no scan.
pub mod native_fence_lifecycle {
    use helios_protocol::HeliosNativeFencePddV1;

    /// HNF1 magic at offset 0 — `0x31464e48`, i.e. the bytes `H N F 1`
    /// little-endian (section 12.1 line 3138).
    pub const HNF1_MAGIC: u32 = helios_protocol::HELIOS_HNF1_MAGIC;
    /// HNF1 ABI version at offset 4 (section 12.1 line 3139).
    pub const HNF1_ABI_VERSION: u16 = helios_protocol::HELIOS_HNF1_ABI_VERSION;
    /// HNF1 structure size at offset 6, and the whole record's length. Equal to
    /// the WDK's `D3DDDI_NATIVE_FENCE_PDD_SIZE`; `kmd_render` asserts that
    /// equality against the generated bindings.
    pub const HNF1_SIZE: usize = helios_protocol::HELIOS_HNF1_SIZE;

    /// Byte offsets of the section-12.1 table. Named so the parser and the
    /// encoder cannot drift from each other.
    pub const OFF_MAGIC: usize = core::mem::offset_of!(HeliosNativeFencePddV1, magic);
    /// Offset of the 2-byte ABI version.
    pub const OFF_ABI_VERSION: usize = core::mem::offset_of!(HeliosNativeFencePddV1, abi_version);
    /// Offset of the 2-byte structure size.
    pub const OFF_STRUCT_SIZE: usize = core::mem::offset_of!(HeliosNativeFencePddV1, struct_size);
    /// Offset of the 8-byte atomic package generation.
    pub const OFF_PACKAGE_GENERATION: usize =
        core::mem::offset_of!(HeliosNativeFencePddV1, package_generation);
    /// Offset of the 8-byte KMD-assigned object generation.
    pub const OFF_OBJECT_GENERATION: usize =
        core::mem::offset_of!(HeliosNativeFencePddV1, object_generation);
    /// Offset of the 4-byte `D3DDDI_NATIVEFENCE_TYPE`.
    pub const OFF_NATIVE_TYPE: usize = core::mem::offset_of!(HeliosNativeFencePddV1, native_type);
    /// Offset of the 4-byte flag word.
    pub const OFF_FLAGS: usize = core::mem::offset_of!(HeliosNativeFencePddV1, flags);
    /// Offset of the 8-byte creating adapter LUID.
    pub const OFF_ADAPTER_LUID: usize = core::mem::offset_of!(HeliosNativeFencePddV1, adapter_luid);
    /// Offset of the 24 reserved bytes, which must be zero.
    pub const OFF_RESERVED: usize = core::mem::offset_of!(HeliosNativeFencePddV1, reserved);
    /// Length of the reserved tail.
    pub const RESERVED_LEN: usize = core::mem::size_of::<[u8; 24]>();

    /// HNF1 flags bit 0 — the object is shareable (section 12.1 line 3144).
    pub const HNF1_FLAG_SHARED: u32 = helios_protocol::HELIOS_HNF1_FLAG_SHARED;
    /// Every other flag bit, including cross-adapter, is zero in this
    /// generation (section 12.1 line 3144, section 12.1 item 7).
    pub const HNF1_FLAGS_RESERVED_MASK: u32 = helios_protocol::HELIOS_HNF1_FLAGS_RESERVED_MASK;

    /// `D3DDDI_NATIVEFENCE_TYPE_DEFAULT`.
    pub const NATIVE_FENCE_TYPE_DEFAULT: u32 = helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT;
    /// `D3DDDI_NATIVEFENCE_TYPE_INTRA_GPU`.
    pub const NATIVE_FENCE_TYPE_INTRA_GPU: u32 =
        helios_protocol::HELIOS_NATIVE_FENCE_TYPE_INTRA_GPU;

    /// Live global (created) native-fence objects admitted per adapter.
    ///
    /// "Bounded" is a requirement, not a tuning parameter (section 17.6:4331).
    /// Each object is one small non-paged allocation; 4096 is far above any
    /// observed D3D12 fence population and still a hard ceiling that turns a
    /// runaway creator into a counted refusal instead of pool exhaustion.
    pub const MAX_LIVE_GLOBAL: u32 = 4096;
    /// Live local (opened) native-fence objects admitted per adapter. A shared
    /// fence may be opened once per process/device, so the local ceiling is
    /// deliberately larger than the global one.
    pub const MAX_LIVE_LOCAL: u32 = 16384;

    /// The single protocol-owned rejection taxonomy used by UMD and KMD.
    pub use helios_protocol::HeliosNativeFencePddReject as PddReject;
    /// The parsed HNF1 payload is the protocol record itself; there is no local
    /// wire mirror or second offset table.
    pub type Hnf1 = HeliosNativeFencePddV1;

    /// Decode the record without judging it. Used by both validators and by the
    /// diagnostics path; every caller that acts on the contents goes through
    /// [`validate_create`] or [`validate_open`] first.
    pub fn parse(bytes: &[u8; HNF1_SIZE]) -> Hnf1 {
        HeliosNativeFencePddV1::from_bytes(bytes)
    }

    /// Validate a `DxgkDdiCreateNativeFence` PDD.
    ///
    /// `ddi_native_type` is `DXGKARG_CREATENATIVEFENCE::Type`, which the record
    /// must agree with — the OS's type and the UMD's claim about it are two
    /// separate inputs, and a disagreement is a refusal rather than a silent
    /// preference for one of them.
    ///
    /// `adapter_luid` is the exact creating adapter's LUID. Section 12.1 line
    /// 3145 makes the KMD the writer of that field, so a caller may leave it
    /// zero; any *other* value is a claim about a different adapter and is
    /// refused (`AdapterLuid`). See this module's tests.
    pub fn validate_create(
        bytes: &[u8; HNF1_SIZE],
        package_generation: u64,
        adapter_luid: i64,
        ddi_native_type: u32,
    ) -> Result<Hnf1, PddReject> {
        let parsed = parse(bytes);
        parsed.validate_create_input(package_generation, adapter_luid, ddi_native_type)?;
        Ok(parsed)
    }

    /// Validate an open request against the exact global object selected by
    /// dxgkrnl. The UMD input generation remains zero; the KMD writes the
    /// object's nonzero generation only on successful return.
    pub fn validate_open(
        bytes: &[u8; HNF1_SIZE],
        package_generation: u64,
        adapter_luid: i64,
        native_type: u32,
        flags: u32,
    ) -> Result<Hnf1, PddReject> {
        let parsed = parse(bytes);
        parsed.validate_create_input(package_generation, adapter_luid, native_type)?;
        if parsed.flags != flags {
            return Err(PddReject::Flags);
        }
        if parsed.adapter_luid != adapter_luid {
            return Err(PddReject::AdapterLuid);
        }
        Ok(parsed)
    }

    /// Whether `native_type` is the one type this package implements.
    ///
    /// `INTRA_GPU` requires the separate native-fence storage-allocation path,
    /// which this traditional-queue package does not advertise. Merely being a
    /// documented enum value is not enough to accept it.
    pub fn native_type_is_documented(native_type: u32) -> bool {
        helios_protocol::native_type_is_admitted(native_type)
    }

    /// Render the KMD's answer: the same header, the assigned nonzero object
    /// generation, the exact creating adapter LUID, and a zero reserved tail.
    ///
    /// Always builds the full record from scratch rather than editing the
    /// caller's bytes in place, so no unvalidated input byte can survive into
    /// the reply.
    pub fn encode(
        package_generation: u64,
        object_generation: u64,
        native_type: u32,
        flags: u32,
        adapter_luid: i64,
    ) -> [u8; HNF1_SIZE] {
        let mut pdd = HeliosNativeFencePddV1::create_input(package_generation, native_type, flags);
        pdd.object_generation = object_generation;
        pdd.adapter_luid = adapter_luid;
        pdd.to_bytes()
    }

    /// Why a native-fence lifecycle operation was refused.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Refusal {
        /// The bounded object population is full ([`MAX_LIVE_GLOBAL`] /
        /// [`MAX_LIVE_LOCAL`]).
        PoolExhausted,
        /// The object was minted before the current adapter epoch. Reset and
        /// removal invalidate every object with one epoch bump (section 12.1
        /// item 6: "Reset/removal invalidates every local mapping and
        /// generation").
        StaleEpoch,
        /// The native-fence surface is not admitted: either the OS declined
        /// `DXGK_FEATURE_NATIVE_FENCE`, or the adapter LUID is not known yet,
        /// or the reported WDDM surface is below 3.2.
        NotAdmitted,
        /// A global object still has opened local objects referencing it, so
        /// freeing it now would dangle them.
        LocalReferencesOutstanding,
        /// A close was issued against a global object with no outstanding local
        /// reference — an OS ordering violation, or our own accounting bug.
        NoLocalReference,
        /// The object is already in a non-live defensive state.
        NotLive,
    }

    /// The observable state of one global native-fence object.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum FenceState {
        /// Created and usable.
        Live,
        /// Defensive model state for an early destroy. The renderer leaves the
        /// object live and refuses instead, relying on dxgkrnl's documented
        /// final-Close-before-Destroy lifetime order.
        Draining,
        /// Freed. No further operation is legal.
        Dead,
    }

    /// The bounded population accounting for one adapter. Two counters and one
    /// epoch; deliberately not a table (section 10.1 invariant 10).
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Population {
        /// Live global (created) objects.
        pub live_global: u32,
        /// Live local (opened) objects.
        pub live_local: u32,
    }

    /// Admit one `DxgkDdiCreateNativeFence`.
    pub fn admit_create(pop: Population) -> Result<Population, Refusal> {
        if pop.live_global >= MAX_LIVE_GLOBAL {
            return Err(Refusal::PoolExhausted);
        }
        Ok(Population {
            live_global: pop.live_global + 1,
            ..pop
        })
    }

    /// Admit one `DxgkDdiOpenNativeFence`.
    pub fn admit_open(pop: Population) -> Result<Population, Refusal> {
        if pop.live_local >= MAX_LIVE_LOCAL {
            return Err(Refusal::PoolExhausted);
        }
        Ok(Population {
            live_local: pop.live_local + 1,
            ..pop
        })
    }

    /// Retire one local object. Underflow is a refusal, never a wrap.
    pub fn retire_local(pop: Population) -> Result<Population, Refusal> {
        if pop.live_local == 0 {
            return Err(Refusal::NoLocalReference);
        }
        Ok(Population {
            live_local: pop.live_local - 1,
            ..pop
        })
    }

    /// Retire one global object. Underflow is a refusal, never a wrap.
    pub fn retire_global(pop: Population) -> Result<Population, Refusal> {
        if pop.live_global == 0 {
            return Err(Refusal::NotLive);
        }
        Ok(Population {
            live_global: pop.live_global - 1,
            ..pop
        })
    }

    /// One global object's reference state.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct GlobalFence {
        /// Lifecycle state.
        pub state: FenceState,
        /// Opened local objects that still point at this global object.
        pub local_refs: u32,
        /// The adapter epoch this object was minted under.
        pub epoch: u64,
        /// The KMD-assigned nonzero diagnostic/stale-validation generation.
        pub object_generation: u64,
    }

    impl GlobalFence {
        /// A freshly created object.
        pub fn new(epoch: u64, object_generation: u64) -> Self {
            Self {
                state: FenceState::Live,
                local_refs: 0,
                epoch,
                object_generation,
            }
        }
    }

    /// Whether an object minted under `object_epoch` is still valid.
    ///
    /// A single adapter-wide epoch is what lets reset invalidate every object
    /// at once with no enumeration and no discovery table.
    pub fn epoch_is_current(object_epoch: u64, adapter_epoch: u64) -> bool {
        object_epoch == adapter_epoch
    }

    /// Take a local reference on a global object (`DxgkDdiOpenNativeFence`).
    pub fn open_local(fence: GlobalFence, adapter_epoch: u64) -> Result<GlobalFence, Refusal> {
        if fence.state != FenceState::Live {
            return Err(Refusal::NotLive);
        }
        if !epoch_is_current(fence.epoch, adapter_epoch) {
            return Err(Refusal::StaleEpoch);
        }
        if fence.local_refs == u32::MAX {
            return Err(Refusal::PoolExhausted);
        }
        Ok(GlobalFence {
            local_refs: fence.local_refs + 1,
            ..fence
        })
    }

    /// Drop a local reference (`DxgkDdiCloseNativeFence`).
    ///
    /// Close must succeed across a reset — the OS is tearing state down and a
    /// refusal would leak — so a stale epoch is *not* an error here. It is an
    /// error to close a reference that was never taken.
    pub fn close_local(fence: GlobalFence) -> Result<GlobalFence, Refusal> {
        if fence.state == FenceState::Dead {
            return Err(Refusal::NotLive);
        }
        if fence.local_refs == 0 {
            return Err(Refusal::NoLocalReference);
        }
        let local_refs = fence.local_refs - 1;
        let state = if fence.state == FenceState::Draining && local_refs == 0 {
            FenceState::Dead
        } else {
            fence.state
        };
        Ok(GlobalFence {
            local_refs,
            state,
            ..fence
        })
    }

    /// Destroy the global object (`DxgkDdiDestroyNativeFence`).
    ///
    /// Freeing while a local object still points here would dangle it, so an
    /// early destroy is refused with [`Refusal::LocalReferencesOutstanding`].
    /// The renderer keeps that bounded object live rather than publishing a
    /// partial teardown state or risking a use-after-free.
    pub fn destroy_global(fence: GlobalFence) -> Result<GlobalFence, Refusal> {
        match fence.state {
            FenceState::Dead => Err(Refusal::NotLive),
            _ if fence.local_refs != 0 => Err(Refusal::LocalReferencesOutstanding),
            _ => Ok(GlobalFence {
                state: FenceState::Dead,
                ..fence
            }),
        }
    }

    /// The next KMD-assigned object generation.
    ///
    /// Section 12.1 line 3142 requires it nonzero on return, and line 3148
    /// forbids treating it as an object key — it exists for stale-validation and
    /// diagnostics. Exhaustion returns `None`: reaching `u64::MAX` must never
    /// wrap to 0, which is the "UMD supplied it" sentinel.
    pub fn next_object_generation(previous: u64) -> Option<u64> {
        previous.checked_add(1).filter(|next| *next != 0)
    }

    /// Reserve a strictly newer epoch without wrapping or reusing the terminal
    /// value. `maximum` lets the renderer share the epoch with packed metadata.
    pub fn next_epoch(previous: u64, maximum: u64) -> Option<u64> {
        previous.checked_add(1).filter(|next| *next <= maximum)
    }

    /// Maximum number of entries accepted by either native-fence update DDI.
    pub const MAX_UPDATE_FENCES: u32 = MAX_LIVE_GLOBAL;

    /// Reject the count before any array offset is formed.
    pub fn update_count_is_bounded(count: u32) -> bool {
        count <= MAX_UPDATE_FENCES
    }

    /// Whether a monitored/current value update moves forward.
    ///
    /// Recorded, never enforced: the OS owns these values, and `UINT64_MAX` is
    /// a legal always-signaled terminal. A driver that *refused* a backwards
    /// update would deadlock the runtime's fence, so `kmd_render` counts a
    /// `false` here instead of failing the DDI.
    pub fn value_update_is_forward(previous: u64, next: u64) -> bool {
        next >= previous
    }

    /// Whether every native-fence admission gate in section 10.2's table is
    /// satisfied. All five are conjunctive; a single false makes the whole
    /// surface refuse rather than partially advertise.
    pub fn surface_is_admitted(
        wddm_3_2_reported: bool,
        display_owner_enabled: bool,
        os_enabled_feature: bool,
        adapter_luid_known: bool,
        lifecycle_active: bool,
    ) -> bool {
        wddm_3_2_reported
            && display_owner_enabled
            && os_enabled_feature
            && adapter_luid_known
            && lifecycle_active
    }
}

#[cfg(test)]
mod native_fence_lifecycle_tests {
    use super::native_fence_lifecycle::*;

    const PKG: u64 = 0x0102_0304_0506_0708;
    const LUID: i64 = 0x0000_1234_5678_9abc_u64 as i64;

    fn good_pdd() -> [u8; HNF1_SIZE] {
        encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, HNF1_FLAG_SHARED, 0)
    }

    #[test]
    fn magic_is_the_ascii_tag_hnf1_little_endian() {
        assert_eq!(HNF1_MAGIC.to_le_bytes(), *b"HNF1");
    }

    #[test]
    fn the_byte_table_fields_tile_the_record_exactly() {
        // Section 12.1 lines 3136-3146: 4 + 2 + 2 + 8 + 8 + 4 + 4 + 8 + 24 = 64,
        // with no gap and no overlap. Checked as a running cursor so a future
        // offset edit cannot quietly open a hole.
        let spans = [
            (OFF_MAGIC, 4),
            (OFF_ABI_VERSION, 2),
            (OFF_STRUCT_SIZE, 2),
            (OFF_PACKAGE_GENERATION, 8),
            (OFF_OBJECT_GENERATION, 8),
            (OFF_NATIVE_TYPE, 4),
            (OFF_FLAGS, 4),
            (OFF_ADAPTER_LUID, 8),
            (OFF_RESERVED, RESERVED_LEN),
        ];
        let mut cursor = 0usize;
        for (off, len) in spans {
            assert_eq!(off, cursor, "field at {off} does not abut the previous one");
            cursor += len;
        }
        assert_eq!(cursor, HNF1_SIZE);
    }

    #[test]
    fn encode_round_trips_through_parse() {
        let bytes = encode(PKG, 7, NATIVE_FENCE_TYPE_INTRA_GPU, HNF1_FLAG_SHARED, LUID);
        let parsed = parse(&bytes);
        assert_eq!(parsed.package_generation, PKG);
        assert_eq!(parsed.object_generation, 7);
        assert_eq!(parsed.native_type, NATIVE_FENCE_TYPE_INTRA_GPU);
        assert_eq!(parsed.flags, HNF1_FLAG_SHARED);
        assert_eq!(parsed.adapter_luid, LUID);
        // The reserved tail is zero by construction, never copied from input.
        assert!(bytes[OFF_RESERVED..].iter().all(|b| *b == 0));
    }

    #[test]
    fn a_well_formed_create_pdd_is_admitted() {
        let ok = validate_create(&good_pdd(), PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT);
        assert_eq!(ok.map(|h| h.flags), Ok(HNF1_FLAG_SHARED));
    }

    #[test]
    fn every_header_field_has_its_own_refusal() {
        let mut b = good_pdd();
        b[OFF_MAGIC] ^= 1;
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::Magic)
        );

        let mut b = good_pdd();
        b[OFF_ABI_VERSION] = 2;
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::AbiVersion)
        );

        let mut b = good_pdd();
        b[OFF_STRUCT_SIZE] = 63;
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::StructSize)
        );

        let b = good_pdd();
        assert_eq!(
            validate_create(&b, PKG ^ 1, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::PackageGeneration)
        );

        // A caller-supplied object generation is a forged identity attempt.
        let b = encode(PKG, 1, NATIVE_FENCE_TYPE_DEFAULT, 0, 0);
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::ObjectGenerationNotZero)
        );

        // Cross-adapter and every other bit are zero in this generation.
        let b = encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, HNF1_FLAG_SHARED | 2, 0);
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::Flags)
        );

        let mut b = good_pdd();
        b[OFF_RESERVED + RESERVED_LEN - 1] = 1;
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::Reserved)
        );
    }

    #[test]
    fn an_undocumented_native_type_is_refused_before_the_ddi_comparison() {
        let b = encode(PKG, 0, 2, 0, 0);
        assert_eq!(
            validate_create(&b, PKG, LUID, 2),
            Err(PddReject::NativeType),
            "an undocumented type must not be admitted just because the OS echoed it"
        );

        let b = encode(PKG, 0, NATIVE_FENCE_TYPE_INTRA_GPU, 0, 0);
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_INTRA_GPU),
            Err(PddReject::NativeType),
            "INTRA_GPU requires a fence-storage allocation path this package does not expose"
        );
    }

    #[test]
    fn the_pdd_type_must_agree_with_the_ddi_type() {
        let b = encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, 0, 0);
        assert_eq!(
            validate_create(&b, PKG, LUID, NATIVE_FENCE_TYPE_INTRA_GPU),
            Err(PddReject::NativeTypeMismatch)
        );
    }

    #[test]
    fn create_accepts_a_zero_or_exact_luid_and_nothing_else() {
        // Section 12.1 line 3145 makes the KMD the writer, so zero is the
        // ordinary input...
        let zero = encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, 0, 0);
        assert!(validate_create(&zero, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT).is_ok());
        // ...and a UMD that already knows the adapter may restate it...
        let exact = encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, 0, LUID);
        assert!(validate_create(&exact, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT).is_ok());
        // ...but a claim about a different adapter is refused, never rewritten.
        let other = encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, 0, LUID ^ 1);
        assert_eq!(
            validate_create(&other, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT),
            Err(PddReject::AdapterLuid)
        );
    }

    #[test]
    fn open_requires_the_exact_global_identity() {
        let b = encode(PKG, 0, NATIVE_FENCE_TYPE_DEFAULT, HNF1_FLAG_SHARED, LUID);
        assert!(validate_open(&b, PKG, LUID, NATIVE_FENCE_TYPE_DEFAULT, HNF1_FLAG_SHARED,).is_ok());
        assert_eq!(
            validate_open(
                &b,
                PKG,
                LUID ^ 1,
                NATIVE_FENCE_TYPE_DEFAULT,
                HNF1_FLAG_SHARED,
            ),
            Err(PddReject::AdapterLuid)
        );
        let nonzero_generation = encode(PKG, 7, NATIVE_FENCE_TYPE_DEFAULT, HNF1_FLAG_SHARED, LUID);
        assert_eq!(
            validate_open(
                &nonzero_generation,
                PKG,
                LUID,
                NATIVE_FENCE_TYPE_DEFAULT,
                HNF1_FLAG_SHARED,
            ),
            Err(PddReject::ObjectGenerationNotZero)
        );
    }

    #[test]
    fn the_population_is_bounded_in_both_directions() {
        let full = Population {
            live_global: MAX_LIVE_GLOBAL,
            live_local: 0,
        };
        assert_eq!(admit_create(full), Err(Refusal::PoolExhausted));
        let full_local = Population {
            live_global: 0,
            live_local: MAX_LIVE_LOCAL,
        };
        assert_eq!(admit_open(full_local), Err(Refusal::PoolExhausted));

        let empty = Population::default();
        assert_eq!(retire_local(empty), Err(Refusal::NoLocalReference));
        assert_eq!(retire_global(empty), Err(Refusal::NotLive));

        let one = admit_create(empty).unwrap();
        assert_eq!(one.live_global, 1);
        assert_eq!(retire_global(one).unwrap(), empty);
    }

    #[test]
    fn a_stale_epoch_blocks_open_but_never_blocks_close() {
        let f = GlobalFence::new(4, 1);
        assert_eq!(open_local(f, 5), Err(Refusal::StaleEpoch));

        // Take the reference before the reset, then close it after: teardown
        // must still work or the object leaks.
        let f = open_local(f, 4).unwrap();
        assert_eq!(f.local_refs, 1);
        let f = close_local(f).expect("close must survive a reset");
        assert_eq!(f.local_refs, 0);
    }

    #[test]
    fn destroy_with_live_locals_refuses_rather_than_dangling_them() {
        let f = GlobalFence::new(0, 1);
        let f = open_local(f, 0).unwrap();
        assert_eq!(
            destroy_global(f),
            Err(Refusal::LocalReferencesOutstanding),
            "freeing the global object here is a use-after-free in every local"
        );
        let f = close_local(f).unwrap();
        let f = destroy_global(f).unwrap();
        assert_eq!(f.state, FenceState::Dead);
        assert_eq!(destroy_global(f), Err(Refusal::NotLive));
        assert_eq!(close_local(f), Err(Refusal::NotLive));
    }

    #[test]
    fn draining_becomes_dead_when_the_last_local_closes() {
        let mut f = GlobalFence::new(0, 1);
        f = open_local(f, 0).unwrap();
        f = open_local(f, 0).unwrap();
        f.state = FenceState::Draining;
        f = close_local(f).unwrap();
        assert_eq!(
            f.state,
            FenceState::Draining,
            "one reference still holds it"
        );
        f = close_local(f).unwrap();
        assert_eq!(f.state, FenceState::Dead);
    }

    #[test]
    fn a_dead_object_cannot_be_reopened() {
        let f = GlobalFence {
            state: FenceState::Dead,
            ..GlobalFence::new(0, 1)
        };
        assert_eq!(open_local(f, 0), Err(Refusal::NotLive));
    }

    #[test]
    fn the_object_generation_is_never_zero_and_never_wraps_to_zero() {
        assert_eq!(next_object_generation(0), Some(1));
        assert_eq!(next_object_generation(1), Some(2));
        assert_eq!(next_object_generation(u64::MAX), None);
        assert_eq!(next_epoch(6, 7), Some(7));
        assert_eq!(next_epoch(7, 7), None);
    }

    #[test]
    fn update_count_is_rejected_before_the_bound_is_exceeded() {
        assert!(update_count_is_bounded(0));
        assert!(update_count_is_bounded(MAX_UPDATE_FENCES));
        assert!(!update_count_is_bounded(MAX_UPDATE_FENCES + 1));
    }

    #[test]
    fn value_updates_are_reported_not_enforced() {
        assert!(value_update_is_forward(4, 4));
        assert!(value_update_is_forward(4, 5));
        assert!(value_update_is_forward(4, u64::MAX));
        assert!(!value_update_is_forward(5, 4));
    }

    #[test]
    fn admission_is_conjunctive() {
        assert!(surface_is_admitted(true, true, true, true, true));
        for i in 0..5 {
            let g = |n: usize| n != i;
            assert!(
                !surface_is_admitted(g(0), g(1), g(2), g(3), g(4)),
                "gate {i} alone must be able to refuse the whole surface"
            );
        }
    }
}

/// The allocation-identity subsystem: the create/open state machine for the
/// three per-allocation private-data records, and the generation lifecycle that
/// ties them to the KMD allocation object.
///
/// # Why this module exists at all
///
/// `dxgkddi_create_allocation` receives HWA2 (168 bytes), HVM1 (64) or HOC1
/// (64) on the same `(pPrivateDriverData, PrivateDriverDataSize)` pair, parses
/// it, validates it, stamps the fields only the kernel can know, and writes the
/// record back through the runtime's own buffer. `protocol/` validates **one
/// record at a time** — it is a pure function of the bytes in front of it. It
/// cannot express the parts of the contract that only exist *across* calls:
///
/// * that the create-input a UMD sends and the create-output the KMD returns
///   differ in exactly the KMD-owned fields and in nothing else (§10.3's
///   "every opener treats it as const" is worthless if create silently
///   rewrites a field the UMD believes it chose);
/// * that a generation is minted once, per allocation, at create, is never
///   zero, is never restarted by an adapter reset, and is never resolved *from*
///   (`wddm.rs:383-388`: "⛔ Never an identity lookup key");
/// * that the C65 pool's live-extent ledger, which `validate_pool_extent` takes
///   as an *argument*, actually advances and retires.
///
/// Those are this module's subject. It is the acceptance evidence for
/// `docs/retirement/K4-CONTRACT.md` §8 obligations 1-4 and the only part of K4
/// that runs on Linux at all — `kmd_render` cannot be built here, let alone
/// tested.
///
/// # What it deliberately does not do
///
/// It does not re-declare a wire record, a field offset, a flag value or a
/// validation rule that `protocol/` already owns. Every admission below routes
/// through `protocol`'s own validator and wraps `protocol`'s own named
/// rejection, so a rule can never be enforced here and not there. The one
/// apparent exception is [`encode_hwa2`] and its two siblings, which write the
/// documented byte layout by hand: they are an *independent oracle* for the
/// offset table (the tests assert `from_private_data(encode(x)) == x`, which
/// fails if either side drifts), not a second declaration — they construct no
/// type and define no rule.
pub mod allocation_identity {
    use helios_protocol::{
        validate_pool_extent, HeliosAllocDescRejection, HeliosExtentRejection,
        HeliosExtentRetirementV1, HeliosExtentSealState, HeliosOuterCommandAllocRejection,
        HeliosOuterCommandAllocationV1, HeliosRetirementRejection, HeliosSealTransitionRejection,
        HeliosVenusMemoryAllocationV1, HeliosWddmAllocationDescV2, Hvm1Placement, Hvm1Reject,
        Hvm1Role, Hvm1Stage, HELIOS_HOC1_ABI_VERSION, HELIOS_HOC1_BYTES, HELIOS_HOC1_MAGIC,
        HELIOS_HVM1_ABI_VERSION, HELIOS_HVM1_MAGIC, HELIOS_HVM1_SEGMENT_PAGE_SHIFT,
        HELIOS_HVM1_SIZE, HELIOS_HWA2_ABI_VERSION, HELIOS_HWA2_BYTES,
        HELIOS_HWA2_FLAG_KMD_OWNED_MASK, HELIOS_HWA2_MAGIC, HELIOS_SEGMENT_ID_APERTURE,
    };

    // ── The exact HWA2 flag bits the KMD owns, per `K4-CONTRACT.md` §1.1.
    //
    // Both are `== 0` on create-input and are the only two bits create may add.
    // `DIRECT_FLIP_COMPATIBLE` is a claim about the *display backend* ("only
    // when the exact allocation is a non-protected, non-cross-adapter managed
    // primary in a swizzle/layout class the selected display backend
    // implements") and `D3D12_RUNTIME_PRIMARY` is the C44 bit whose sentinel is
    // cross-validated — neither is inferrable by a UMD, and §10.3 forbids an
    // opener inferring either one. Every other bit in the word is the UMD's and
    // is echoed verbatim.
    //
    // ⛔ That reasoning is kept; the VALUE is not. This module used to declare
    // its own `HWA2_KMD_OWNED_FLAGS = DIRECT_FLIP_COMPATIBLE | D3D12_RUNTIME_PRIMARY`,
    // which is a second declaration of a rule `protocol` already owns as
    // [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] (`wddm.rs:245`) — the same rule was
    // ALSO re-derived by hand in `umd/src/forward/resource.rs` and
    // `umd12/src/forward12/resource12.rs`, so one rule had four declarations and
    // nothing compared them. The failure mode is not a typo today, it is the day
    // a third KMD-owned bit is added: `protocol`'s validator refuses the bit on
    // input, and every stale copy keeps clearing only two bits in its echo
    // check, so the echo silently starts comparing a field the KMD legitimately
    // stamped. Every consumer now reads the one constant, and the C mirror is
    // pinned to it by `helios_wddm.h`'s own static assert.

    /// Why an allocation-identity operation was refused.
    ///
    /// Every variant is a distinct counted refusal in `kmd_render`; none is
    /// ever silently repaired, and the four wrapping variants carry
    /// `protocol`'s own named rejection so the KMD counter and the ETW field
    /// can use one vocabulary rather than two.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum IdentityRefusal {
        /// An HWA2 record was refused by `protocol`.
        Hwa2(HeliosAllocDescRejection),
        /// An HVM1 record was refused by `protocol`.
        Hvm1(Hvm1Reject),
        /// An HOC1 record was refused by `protocol`.
        Hoc1(HeliosOuterCommandAllocRejection),
        /// A C65 pool extent was refused by `protocol`.
        Extent(HeliosExtentRejection),
        /// A C65 seal-state transition was refused by `protocol`.
        SealTransition(HeliosSealTransitionRejection),
        /// A C65 retirement tuple was refused by `protocol`.
        Retirement(HeliosRetirementRejection),
        /// The generation counter was read out of a zeroed structure, i.e. one
        /// that never went through [`GenerationCounter::new`]. Minting from it
        /// would hand out generation 0, which every validator in `protocol`
        /// reads as "the UMD supplied it" and refuses — but only *after* the
        /// KMD has already created the object. Refuse at the source instead.
        GenerationCounterUninitialised,
        /// The 64-bit generation space is exhausted.
        ///
        /// Deliberately **not** saturating, and this is the one place this
        /// module diverges from [`super::native_fence_lifecycle`]'s
        /// `next_object_generation`. A saturating counter hands `u64::MAX` to
        /// every subsequent allocation, so two live allocations share one
        /// generation and every `expected_allocation_generation` stale check in
        /// HOB1 (`wddm.rs:1430`) and HNR2 (`native_render.rs:615`) starts
        /// silently accepting a batch aimed at the wrong object. At one
        /// allocation per nanosecond the arm is 584 years away; it is here so
        /// the failure is a counted refusal instead of a correctness hole.
        GenerationSpaceExhausted,
        /// The adapter-epoch space is exhausted, so a reset could no longer
        /// invalidate the previous epoch's objects by bumping it.
        AdapterEpochSpaceExhausted,
        /// The object was minted before the current adapter epoch. §14 (adapter
        /// reset) requires that "all generations change" together; a stale
        /// object is refused rather than revalidated.
        StaleAdapterEpoch {
            /// The epoch the object was minted under.
            object_epoch: u32,
            /// The adapter's current epoch.
            adapter_epoch: u32,
        },
        /// One allocation accumulated `u32::MAX` opens. Refused rather than
        /// wrapped: a wrapped open count would let the last close free backing
        /// that other openers still hold.
        OpenCountOverflow,
        /// The KMD tried to stamp a zero `allocation_generation` /
        /// `object_generation` into a create-output record.
        GenerationZeroAtStamp,
        /// The KMD tried to set an HWA2 flag bit outside
        /// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] during the create write-back. The bits
        /// outside that mask belong to the UMD and are echoed, never authored.
        KmdFlagOutsidePartition {
            /// The offending bits.
            bits: u32,
        },
        /// A `stale`-validation comparison was handed a zero expectation.
        /// `protocol` refuses a zero `expected_allocation_generation` on the
        /// wire (`wddm.rs:1652`, `native_render.rs:1517`); accepting it here
        /// would turn "I did not fill this in" into "matches anything".
        ExpectedGenerationZero,
        /// HVM1 shared backing requires the WDDM aperture segment, and that
        /// segment is not exposed. Refused and counted per role.
        ApertureSegmentNotExposed {
            /// `HELIOS_HVM1_ROLE_*` of the refused create.
            role: u32,
        },
        /// A role-4 (device-local) allocation was offered to `Lock2` /
        /// `MapCpuHostAperture`. §10.7: role 4 "may never be passed to Lock2".
        /// There is no CPU VA to hand back, so the only honest answer is a
        /// counted refusal.
        Role4NeverLockable,
        /// The `DXGK_ALLOCATIONINFOFLAGS`/`Flags2` word the KMD is about to
        /// write disagrees with [`Hvm1Placement`] for the role.
        PlacementMismatch {
            /// Which flag disagreed.
            field: PlacementField,
        },
        /// The live-extent counter would overflow. Unreachable while
        /// `validate_pool_extent` refuses at 256, and present because an
        /// unchecked `+ 1` on a DDI path is never worth the byte saved.
        LiveExtentCounterOverflow,
        /// A retirement was accepted by the tuple validator but its
        /// `hqc1_value` has not been reached by the context's completed value
        /// yet, so the extent is still owned by in-flight GPU work.
        ExtentNotRetiredYet {
            /// The extent's recorded bottom-of-pipe value.
            recorded: u64,
            /// The context's completed value.
            completed: u64,
        },
        /// A retirement arrived with no live extent to retire — an accounting
        /// bug or a double retire.
        NoLiveExtent,
    }

    // ── the allocation generation ───────────────────────────────────────────

    /// The KMD's allocation-generation source: one monotone counter plus the
    /// adapter epoch, per adapter.
    ///
    /// Deliberately **not** `Default` and deliberately not zero-initialisable,
    /// for the same reason `HeliosWddmAllocationDescV2` is not `Default`: a
    /// zeroed counter would mint generation 0, and 0 is the "user mode supplied
    /// it" sentinel every validator in `protocol` refuses.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GenerationCounter {
        /// The next generation to hand out. Never 0 after [`Self::new`].
        pub next: u64,
        /// The adapter epoch. §14: an adapter reset "first changes
        /// adapter/package-visible generation" and then "invalidates queue,
        /// endpoint/ring, allocation/HPM1, host-batch, native-fence mapping,
        /// WSI, and flip generations together" — one bump invalidates every
        /// object with no enumeration and no discovery table.
        pub epoch: u32,
    }

    impl GenerationCounter {
        /// A fresh counter. Epoch starts at 1 so that a zeroed structure is not
        /// a *valid* epoch either, and generation at 1 so the first allocation
        /// is already nonzero.
        ///
        /// ⛔ No `Default`, and the lint is silenced on purpose: a derived
        /// `Default` is the all-zero counter, which mints generation 0 — the
        /// exact "user mode supplied it" sentinel every validator in
        /// `protocol` refuses. The zero value of this type is invalid by
        /// construction, so offering it under the name `default()` would be a
        /// trap, not a convenience.
        #[allow(clippy::new_without_default)]
        pub const fn new() -> Self {
            Self { next: 1, epoch: 1 }
        }

        /// Mint the next allocation generation.
        ///
        /// This is the *only* place a generation is created, and it is called
        /// exactly once per allocation, from create. Open never calls it — see
        /// [`AllocationIdentity::open`], whose whole point is that the
        /// generation it returns is the one it was given.
        pub fn mint(self) -> Result<(u64, Self), IdentityRefusal> {
            if self.next == 0 {
                return Err(IdentityRefusal::GenerationCounterUninitialised);
            }
            let value = self.next;
            let next = match value.checked_add(1) {
                Some(next) => next,
                None => return Err(IdentityRefusal::GenerationSpaceExhausted),
            };
            Ok((value, Self { next, ..self }))
        }

        /// Bump the adapter epoch on reset.
        ///
        /// ⚠ `next` is **preserved**, not restarted. Restarting it would make a
        /// post-reset allocation reuse a pre-reset generation, and a stale HOB1
        /// use record naming that value would then pass its
        /// `expected_allocation_generation` check against a different object.
        /// The epoch is what invalidates; the counter is what disambiguates.
        pub fn on_adapter_reset(self) -> Result<Self, IdentityRefusal> {
            let epoch = match self.epoch.checked_add(1) {
                Some(epoch) => epoch,
                None => return Err(IdentityRefusal::AdapterEpochSpaceExhausted),
            };
            Ok(Self { epoch, ..self })
        }
    }

    /// The KMD-side identity of one allocation object.
    ///
    /// ⚠ This is *not* the allocation object. `AllocationContext` in
    /// `kmd_render` carries 21 mutable display fields beside it and none of
    /// them belong here (`K4-CONTRACT.md` §3.1). This is only the immutable
    /// identity triple the const descriptor is stamped from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AllocationIdentity {
        /// The nonzero KMD-assigned generation written into HWA2 offset 16 /
        /// HVM1 offset 16 / HOC1 offset 16.
        pub generation: u64,
        /// The adapter epoch this allocation was minted under.
        pub epoch: u32,
        /// How many `DxgkDdiOpenAllocation` references are outstanding.
        pub opens: u32,
    }

    impl AllocationIdentity {
        /// Mint one identity at create. Returns the advanced counter, so a
        /// caller cannot mint twice from the same value by accident.
        pub fn create(
            counter: GenerationCounter,
        ) -> Result<(Self, GenerationCounter), IdentityRefusal> {
            let (generation, counter) = counter.mint()?;
            Ok((
                Self {
                    generation,
                    epoch: counter.epoch,
                    opens: 0,
                },
                counter,
            ))
        }

        /// Take one open reference.
        ///
        /// The returned identity has the **same** generation: `DxgkDdiOpen-
        /// Allocation` neither mints nor restamps. The retired
        /// `HeliosWddmOpenIdentity` restamped the first 48 bytes at open time,
        /// so two openers of one allocation could disagree about what they had
        /// (`wddm.rs:349-356`); this signature is what makes that
        /// unrepresentable rather than merely discouraged.
        pub fn open(self, adapter_epoch: u32) -> Result<Self, IdentityRefusal> {
            if self.epoch != adapter_epoch {
                return Err(IdentityRefusal::StaleAdapterEpoch {
                    object_epoch: self.epoch,
                    adapter_epoch,
                });
            }
            let opens = match self.opens.checked_add(1) {
                Some(opens) => opens,
                None => return Err(IdentityRefusal::OpenCountOverflow),
            };
            Ok(Self { opens, ..self })
        }

        /// Does a batch's `expected_allocation_generation` still describe this
        /// object?
        ///
        /// ⛔ This is the *only* direction the generation is ever used in. There
        /// is deliberately no `fn find(generation) -> AllocationIdentity` in
        /// this module and there must never be one: §10.3 says the generation
        /// is "never an identity lookup key", and the caller here already holds
        /// the object — dxgkrnl resolved the allocation handle for it. A zero
        /// expectation is refused rather than treated as a wildcard.
        pub fn matches_expected(
            &self,
            expected_allocation_generation: u64,
            adapter_epoch: u32,
        ) -> Result<bool, IdentityRefusal> {
            if expected_allocation_generation == 0 {
                return Err(IdentityRefusal::ExpectedGenerationZero);
            }
            if self.epoch != adapter_epoch {
                return Err(IdentityRefusal::StaleAdapterEpoch {
                    object_epoch: self.epoch,
                    adapter_epoch,
                });
            }
            Ok(self.generation == expected_allocation_generation)
        }
    }

    // ── HWA2: the create-input / create-output / open state machine ──────────

    /// `DxgkDdiCreateAllocation`, HWA2 arm, step 1: parse the runtime's buffer
    /// through the bounded reader and admit it as a create *request*.
    ///
    /// The length gate is the whole reason `from_private_data` exists: a KMD
    /// that read 168 bytes out of a shorter buffer would take an out-of-bounds
    /// kernel read no validator in `protocol` could catch.
    pub fn hwa2_admit_create_input(
        bytes: &[u8],
        package_generation: u64,
    ) -> Result<HeliosWddmAllocationDescV2, IdentityRefusal> {
        let desc =
            HeliosWddmAllocationDescV2::from_private_data(bytes).map_err(IdentityRefusal::Hwa2)?;
        desc.validate_create_input(package_generation)
            .map_err(IdentityRefusal::Hwa2)?;
        Ok(desc)
    }

    /// `DxgkDdiCreateAllocation`, HWA2 arm, step 2: the KMD write-back.
    ///
    /// Every field except `allocation_generation` and the bits in
    /// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] is echoed from the admitted input untouched.
    /// `K4-CONTRACT.md` §1.1: "The KMD refuses the create rather than
    /// correcting a field. Silent correction would make the descriptor
    /// disagree with the resource the UMD believes it made." The output is
    /// re-validated before it is handed back, so a stamp that produces an
    /// inadmissible pairing (a Direct Flip bit on a non-primary, a C44 primary
    /// bit against a concrete VidPn source) fails the create instead of
    /// shipping a descriptor no opener will accept.
    pub fn hwa2_stamp_create_output(
        input: &HeliosWddmAllocationDescV2,
        allocation_generation: u64,
        kmd_flags: u32,
        package_generation: u64,
    ) -> Result<HeliosWddmAllocationDescV2, IdentityRefusal> {
        if allocation_generation == 0 {
            return Err(IdentityRefusal::GenerationZeroAtStamp);
        }
        let stray = kmd_flags & !HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        if stray != 0 {
            return Err(IdentityRefusal::KmdFlagOutsidePartition { bits: stray });
        }
        let mut output = *input;
        output.allocation_generation = allocation_generation;
        output.flags |= kmd_flags;
        output
            .validate_create_output(package_generation)
            .map_err(IdentityRefusal::Hwa2)?;
        Ok(output)
    }

    /// `DxgkDdiOpenAllocation`, HWA2 arm: parse, validate, **read only**.
    ///
    /// The signature is the contract: `&[u8]` in, an owned copy out, no `&mut`
    /// anywhere. `describe_allocation`'s answers come from the KMD allocation
    /// object, not from re-parsing, and no byte of the private buffer is
    /// written on this path (obligation 5).
    pub fn hwa2_admit_open(
        bytes: &[u8],
        package_generation: u64,
    ) -> Result<HeliosWddmAllocationDescV2, IdentityRefusal> {
        let desc =
            HeliosWddmAllocationDescV2::from_private_data(bytes).map_err(IdentityRefusal::Hwa2)?;
        desc.validate(package_generation)
            .map_err(IdentityRefusal::Hwa2)?;
        Ok(desc)
    }

    /// Do two descriptors agree on every field the KMD does **not** own?
    ///
    /// Field-wise rather than byte-wise, which is the same thing here: HWA2 is
    /// `#[repr(C)]`, its fields tile offsets 0..168 with no gap, and it
    /// derives `PartialEq`. The tests additionally compare the encoded byte
    /// images with the same two regions masked, so "byte-identical" is proven
    /// on bytes and not only on fields.
    pub fn hwa2_echoed_fields_equal(
        a: &HeliosWddmAllocationDescV2,
        b: &HeliosWddmAllocationDescV2,
    ) -> bool {
        let mut a = *a;
        let mut b = *b;
        a.allocation_generation = 0;
        b.allocation_generation = 0;
        a.flags &= !HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        b.flags &= !HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        a == b
    }

    // ── HVM1: role admission and placement ──────────────────────────────────

    /// `DxgkDdiCreateAllocation`, HVM1 arm, step 1. Returns the decoded role,
    /// because `validate` hands back the role and the admission in one call and
    /// re-deriving it from the `u32` would be a second decode that could
    /// disagree.
    pub fn hvm1_admit_create_input(
        bytes: &[u8],
        package_generation: u64,
    ) -> Result<(HeliosVenusMemoryAllocationV1, Hvm1Role), IdentityRefusal> {
        let record = HeliosVenusMemoryAllocationV1::from_private_data(bytes)
            .map_err(IdentityRefusal::Hvm1)?;
        let role = record
            .validate(package_generation, Hvm1Stage::CreateInput)
            .map_err(IdentityRefusal::Hvm1)?;
        Ok((record, role))
    }

    /// `DxgkDdiCreateAllocation`, HVM1 arm, step 2: fill the three write-back
    /// fields and self-check against the output contract.
    ///
    /// `segment_page_shift` is not a parameter: §10.7 fixes it at
    /// [`HELIOS_HVM1_SEGMENT_PAGE_SHIFT`] for this generation, and letting a
    /// call site choose it is how the two halves of a boundary drift.
    pub fn hvm1_stamp_create_output(
        input: &HeliosVenusMemoryAllocationV1,
        object_generation: u64,
        allocation_alignment: u64,
        package_generation: u64,
    ) -> Result<HeliosVenusMemoryAllocationV1, IdentityRefusal> {
        if object_generation == 0 {
            return Err(IdentityRefusal::GenerationZeroAtStamp);
        }
        let mut output = *input;
        output.object_generation = object_generation;
        output.segment_page_shift = HELIOS_HVM1_SEGMENT_PAGE_SHIFT;
        output.allocation_alignment = allocation_alignment;
        output
            .validate(package_generation, Hvm1Stage::CreateOutput)
            .map_err(IdentityRefusal::Hvm1)?;
        Ok(output)
    }

    /// The role's exact shared-backing placement, or a counted refusal while
    /// the WDDM aperture segment is unavailable.
    pub fn hvm1_admit_placement(
        role: Hvm1Role,
        aperture_segment_exposed: bool,
    ) -> Result<Hvm1Placement, IdentityRefusal> {
        let placement = role.placement();
        if placement.preferred_segment == HELIOS_SEGMENT_ID_APERTURE && !aperture_segment_exposed {
            return Err(IdentityRefusal::ApertureSegmentNotExposed {
                role: role.to_u32(),
            });
        }
        Ok(placement)
    }

    /// May this role ever reach `D3DKMTLock2` / `MapCpuHostAperture`?
    ///
    /// Role 4 has `CpuVisible=0` and no CPU VA exists for it, so the refusal is
    /// not a policy choice — there is nothing to return.
    pub fn hvm1_lock_admissible(role: Hvm1Role) -> Result<(), IdentityRefusal> {
        if role.placement().lockable {
            Ok(())
        } else {
            Err(IdentityRefusal::Role4NeverLockable)
        }
    }

    /// Which placement flag disagreed with the role table.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PlacementField {
        /// `DXGK_ALLOCATIONINFOFLAGS::CpuVisible`.
        CpuVisible,
        /// `DXGK_ALLOCATIONINFOFLAGS::Cached`.
        Cached,
        /// `DXGK_ALLOCATIONINFOFLAGS::AccessedPhysically`.
        AccessedPhysically,
        /// `DXGK_ALLOCATIONINFOFLAGS::ExplicitResidencyNotification`.
        ExplicitResidencyNotification,
        /// `DXGK_ALLOCATIONINFO::Flags2::DisablePartialResidency`.
        DisablePartialResidency,
        /// `DXGK_ALLOCATIONINFO::Flags2::RestrictedToSingleSegment`.
        RestrictedToSingleSegment,
        /// The preferred read/write segment id.
        PreferredSegment,
        /// The declared HVM1 cache policy.
        CachePolicy,
    }

    /// The flag word the KMD is about to write into `DXGK_ALLOCATIONINFO`,
    /// mirrored as plain booleans.
    ///
    /// ⚠ This structure is a **model of** the WDK union, not the union. Nothing
    /// in `kmd_logic` can see `DXGK_ALLOCATIONINFOFLAGS`, so the bridge from
    /// these booleans to the real bitfield lives in `kmd_render` and is the one
    /// place the table can still drift. `K4-CONTRACT.md` §8 obligation 8 flags
    /// the `Flags2` half as "first-time union writes with zero field evidence".
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WrittenAllocationFlags {
        /// `CpuVisible`.
        pub cpu_visible: bool,
        /// `Cached`.
        pub cached: bool,
        /// `AccessedPhysically`.
        pub accessed_physically: bool,
        /// `ExplicitResidencyNotification`.
        pub explicit_residency_notification: bool,
        /// `Flags2::DisablePartialResidency`.
        pub disable_partial_residency: bool,
        /// `Flags2::RestrictedToSingleSegment`.
        pub restricted_to_single_segment: bool,
        /// Preferred read/write segment id.
        pub preferred_segment: u32,
        /// The HVM1 `cache_policy` the KMD writes back.
        pub cache_policy: u32,
    }

    /// Compare what the KMD is about to write against the role table.
    ///
    /// `Hvm1Placement` exists so "the KMD cannot express it differently per
    /// call site" (`native_render.rs:1659-1662`) — but it is a plain data
    /// struct and `protocol` ships no comparison, so today a KMD can call
    /// `placement()` and then write different bits with nothing failing. This
    /// is that missing comparison, KMD-side, one field at a time so the counter
    /// names which bit drifted.
    pub fn hvm1_written_flags_match(
        placement: &Hvm1Placement,
        written: &WrittenAllocationFlags,
    ) -> Result<(), IdentityRefusal> {
        let mismatch = |field| Err(IdentityRefusal::PlacementMismatch { field });
        if written.cpu_visible != placement.cpu_visible {
            return mismatch(PlacementField::CpuVisible);
        }
        if written.cached != placement.cached {
            return mismatch(PlacementField::Cached);
        }
        if written.accessed_physically != placement.accessed_physically {
            return mismatch(PlacementField::AccessedPhysically);
        }
        if written.explicit_residency_notification != placement.explicit_residency_notification {
            return mismatch(PlacementField::ExplicitResidencyNotification);
        }
        if written.disable_partial_residency != placement.disable_partial_residency {
            return mismatch(PlacementField::DisablePartialResidency);
        }
        if written.restricted_to_single_segment != placement.restricted_to_single_segment {
            return mismatch(PlacementField::RestrictedToSingleSegment);
        }
        if written.preferred_segment != placement.preferred_segment {
            return mismatch(PlacementField::PreferredSegment);
        }
        if written.cache_policy != placement.cache_policy {
            return mismatch(PlacementField::CachePolicy);
        }
        Ok(())
    }

    /// Field-wise equality for HVM1.
    ///
    /// `HeliosVenusMemoryAllocationV1` derives `Pod`/`Zeroable` but **not**
    /// `PartialEq` (`native_render.rs:1797`), so `assert_eq!` on two records is
    /// not available and the echo check needs this instead.
    pub fn hvm1_fields_equal(
        a: &HeliosVenusMemoryAllocationV1,
        b: &HeliosVenusMemoryAllocationV1,
    ) -> bool {
        a.magic == b.magic
            && a.abi_version == b.abi_version
            && a.struct_size == b.struct_size
            && a.package_generation == b.package_generation
            && a.object_generation == b.object_generation
            && a.byte_size == b.byte_size
            && a.role == b.role
            && a.access == b.access
            && a.cache_policy == b.cache_policy
            && a.segment_page_shift == b.segment_page_shift
            && a.allocation_alignment == b.allocation_alignment
            && a.reserved == b.reserved
    }

    // ── HOC1: the C65 command pool ──────────────────────────────────────────

    /// `DxgkDdiCreateAllocation`, HOC1 arm, step 1.
    pub fn hoc1_admit_create_input(
        bytes: &[u8],
        package_generation: u64,
    ) -> Result<HeliosOuterCommandAllocationV1, IdentityRefusal> {
        let record = HeliosOuterCommandAllocationV1::from_private_data(bytes)
            .map_err(IdentityRefusal::Hoc1)?;
        record
            .validate_create_input(package_generation)
            .map_err(IdentityRefusal::Hoc1)?;
        Ok(record)
    }

    /// `DxgkDdiCreateAllocation`, HOC1 arm, step 2: the one create-time
    /// write-back in this ABI.
    pub fn hoc1_stamp_create_output(
        input: &HeliosOuterCommandAllocationV1,
        allocation_generation: u64,
        package_generation: u64,
    ) -> Result<HeliosOuterCommandAllocationV1, IdentityRefusal> {
        if allocation_generation == 0 {
            return Err(IdentityRefusal::GenerationZeroAtStamp);
        }
        let mut output = *input;
        output.allocation_generation = allocation_generation;
        output
            .validate_create_output(package_generation)
            .map_err(IdentityRefusal::Hoc1)?;
        Ok(output)
    }

    /// The live-extent ledger for one C65 pool.
    ///
    /// `validate_pool_extent` takes the live count as an **argument** and
    /// `HeliosExtentRetirementV1::validate` takes the previous HQC1 value as an
    /// argument, which means `protocol` can check one reservation and one
    /// retirement in isolation but cannot check that the count ever advances or
    /// that the values ever increase across calls. That is what this is.
    ///
    /// ⚠ Overlap between live extents is deliberately **not** modelled here.
    /// `validate_pool_extent` does not check it either — the allocator owns it,
    /// and on this pool the allocator is the D3D12 UMD's, not the KMD's. The
    /// KMD admits the pool; it does not carve it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Hoc1Pool {
        /// Extents currently reserved-or-later and not yet retired.
        pub live_extents: u32,
        /// The last retirement value accepted on this context. Retirements must
        /// strictly increase, so this is the floor for the next one.
        pub last_retired_hqc1_value: u64,
    }

    impl Hoc1Pool {
        /// A freshly created, empty pool. Unlike [`GenerationCounter`], the
        /// all-zero value of this type *is* the valid initial state — no
        /// extents live, no retirement recorded — so `Default` is offered and
        /// delegates here rather than being suppressed.
        pub const fn new() -> Self {
            Self {
                live_extents: 0,
                last_retired_hqc1_value: 0,
            }
        }

        /// Reserve one extent, returning its initial seal state.
        ///
        /// The seal state is produced by walking `Free -> Reserved` through
        /// `protocol`'s own transition table rather than by naming `Reserved`
        /// directly, so this ledger and the C65 lifecycle can never disagree
        /// about where an extent starts.
        pub fn reserve(
            self,
            offset: u64,
            bytes: u64,
        ) -> Result<(HeliosExtentSealState, Self), IdentityRefusal> {
            validate_pool_extent(offset, bytes, self.live_extents)
                .map_err(IdentityRefusal::Extent)?;
            let state = HeliosExtentSealState::Free
                .advance(HeliosExtentSealState::Reserved)
                .map_err(IdentityRefusal::SealTransition)?;
            let live_extents = match self.live_extents.checked_add(1) {
                Some(live) => live,
                None => return Err(IdentityRefusal::LiveExtentCounterOverflow),
            };
            Ok((
                state,
                Self {
                    live_extents,
                    ..self
                },
            ))
        }

        /// Retire one extent against its owning context's completed HQC1 value.
        ///
        /// Three separate gates, in order: the tuple must be complete and
        /// strictly increasing (`protocol`), the GPU must actually have reached
        /// it, and there must be a live extent to retire. Retiring an extent
        /// the GPU still owns is exactly the use-after-free the C65 lifecycle
        /// exists to prevent, so it is a refusal and not a warning.
        pub fn retire(
            self,
            retirement: &HeliosExtentRetirementV1,
            completed_hqc1_value: u64,
        ) -> Result<Self, IdentityRefusal> {
            retirement
                .validate(self.last_retired_hqc1_value)
                .map_err(IdentityRefusal::Retirement)?;
            if !retirement.is_retired_at(completed_hqc1_value) {
                return Err(IdentityRefusal::ExtentNotRetiredYet {
                    recorded: retirement.hqc1_value,
                    completed: completed_hqc1_value,
                });
            }
            if self.live_extents == 0 {
                return Err(IdentityRefusal::NoLiveExtent);
            }
            Ok(Self {
                live_extents: self.live_extents - 1,
                last_retired_hqc1_value: retirement.hqc1_value,
            })
        }
    }

    impl Default for Hoc1Pool {
        fn default() -> Self {
            Self::new()
        }
    }

    // ── the independent offset oracle ───────────────────────────────────────
    //
    // These three encoders write the byte layout the doc's §10.3/§10.6/§10.7
    // tables state, by hand, from the offsets alone. They exist so the tests
    // can feed `from_private_data` a real byte buffer at a real (possibly odd)
    // address without borrowing `protocol`'s own view of the layout: if either
    // side's offsets drift, the round-trip test fails. They construct no type
    // and define no rule, so they are not a second declaration of a wire
    // record — see this module's header.

    fn wr_u16(out: &mut [u8], off: usize, value: u16) {
        out[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn wr_u32(out: &mut [u8], off: usize, value: u32) {
        out[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn wr_u64(out: &mut [u8], off: usize, value: u64) {
        out[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    /// Encode one HWA2 record into its 168 documented bytes.
    pub fn encode_hwa2(desc: &HeliosWddmAllocationDescV2) -> [u8; HELIOS_HWA2_BYTES as usize] {
        let mut out = [0u8; HELIOS_HWA2_BYTES as usize];
        wr_u32(&mut out, 0, desc.magic);
        wr_u16(&mut out, 4, desc.abi_version);
        wr_u16(&mut out, 6, desc.struct_size);
        wr_u64(&mut out, 8, desc.package_generation);
        wr_u64(&mut out, 16, desc.allocation_generation);
        wr_u64(&mut out, 24, desc.byte_size);
        wr_u32(&mut out, 32, desc.width);
        wr_u32(&mut out, 36, desc.height);
        wr_u32(&mut out, 40, desc.depth_or_array_size);
        wr_u32(&mut out, 44, desc.mip_levels);
        wr_u32(&mut out, 48, desc.dxgi_format);
        wr_u32(&mut out, 52, desc.d3d_ddi_format);
        wr_u32(&mut out, 56, desc.sample_count);
        wr_u32(&mut out, 60, desc.sample_quality);
        wr_u32(&mut out, 64, desc.allocation_kind);
        wr_u32(&mut out, 68, desc.flags);
        wr_u32(&mut out, 72, desc.bind_flags);
        wr_u32(&mut out, 76, desc.misc_flags);
        wr_u32(&mut out, 80, desc.vidpn_source);
        wr_u32(&mut out, 84, desc.standard_allocation_type);
        wr_u32(&mut out, 88, desc.swizzle_class);
        wr_u32(&mut out, 92, desc.memory_class);
        wr_u32(&mut out, 96, desc.plane_count);
        wr_u32(&mut out, 100, desc.reserved);
        let mut i = 0usize;
        while i < 4 {
            let base = 104 + i * 16;
            wr_u64(&mut out, base, desc.planes[i].offset);
            wr_u32(&mut out, base + 8, desc.planes[i].row_pitch);
            wr_u32(&mut out, base + 12, desc.planes[i].slice_pitch);
            i += 1;
        }
        out
    }

    /// Encode one HVM1 record into its 64 documented bytes.
    pub fn encode_hvm1(record: &HeliosVenusMemoryAllocationV1) -> [u8; HELIOS_HVM1_SIZE as usize] {
        let mut out = [0u8; HELIOS_HVM1_SIZE as usize];
        wr_u32(&mut out, 0, record.magic);
        wr_u16(&mut out, 4, record.abi_version);
        wr_u16(&mut out, 6, record.struct_size);
        wr_u64(&mut out, 8, record.package_generation);
        wr_u64(&mut out, 16, record.object_generation);
        wr_u64(&mut out, 24, record.byte_size);
        wr_u32(&mut out, 32, record.role);
        wr_u32(&mut out, 36, record.access);
        wr_u32(&mut out, 40, record.cache_policy);
        wr_u32(&mut out, 44, record.segment_page_shift);
        wr_u64(&mut out, 48, record.allocation_alignment);
        wr_u64(&mut out, 56, record.reserved);
        out
    }

    /// Encode one HOC1 record into its 64 documented bytes.
    pub fn encode_hoc1(
        record: &HeliosOuterCommandAllocationV1,
    ) -> [u8; HELIOS_HOC1_BYTES as usize] {
        let mut out = [0u8; HELIOS_HOC1_BYTES as usize];
        wr_u32(&mut out, 0, record.magic);
        wr_u16(&mut out, 4, record.abi_version);
        wr_u16(&mut out, 6, record.struct_size);
        wr_u64(&mut out, 8, record.package_generation);
        wr_u64(&mut out, 16, record.allocation_generation);
        wr_u64(&mut out, 24, record.byte_size);
        wr_u32(&mut out, 32, record.extent_alignment);
        wr_u32(&mut out, 36, record.access);
        wr_u32(&mut out, 40, record.cache_policy);
        wr_u32(&mut out, 44, record.physical_adapter_mask);
        out[48..64].copy_from_slice(&record.reserved);
        out
    }

    /// The three records' identity headers, so a discriminator can peek at a
    /// `(pPrivateDriverData, size)` pair without a second copy of the magic
    /// table. HVM1 and HOC1 are the **same length**, so length alone cannot
    /// discriminate them and the magic is load-bearing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AllocPrivateKind {
        /// 168 bytes, magic `'HWA2'`.
        Hwa2,
        /// 64 bytes, magic `'HVM1'`.
        Hvm1,
        /// 64 bytes, magic `'HOC1'`.
        Hoc1,
    }

    /// Classify a create-time private-data buffer by its length and magic.
    ///
    /// `None` is a refusal, not a default: §10.3 says a malformed, unknown or
    /// truncated descriptor "makes create/open fail; it never selects a legacy
    /// parser". The caller must count the `None` and fail the DDI, never fall
    /// back to `HeliosWddmAllocPrivate`.
    ///
    /// ⚠ This is a *peek*. It checks the length and the four magic bytes and
    /// nothing else; the record's own validator still owns admission. It is
    /// here rather than in `protocol` because `protocol` does not ship a
    /// discriminator yet (recon R5) — if one lands there, this must be deleted
    /// rather than kept as a second table.
    pub fn classify_alloc_private_data(bytes: &[u8]) -> Option<AllocPrivateKind> {
        if bytes.len() < 4 {
            return None;
        }
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        match (bytes.len(), magic) {
            (len, HELIOS_HWA2_MAGIC) if len == HELIOS_HWA2_BYTES as usize => {
                Some(AllocPrivateKind::Hwa2)
            }
            (len, HELIOS_HVM1_MAGIC) if len == HELIOS_HVM1_SIZE as usize => {
                Some(AllocPrivateKind::Hvm1)
            }
            (len, HELIOS_HOC1_MAGIC) if len == HELIOS_HOC1_BYTES as usize => {
                Some(AllocPrivateKind::Hoc1)
            }
            _ => None,
        }
    }

    /// The three ABI versions, re-exported as a single tuple so a reviewer can
    /// see at a glance that none of them is negotiable. A mismatch is a hard
    /// reject on every one and never selects a legacy parser.
    pub const ADMITTED_ABI_VERSIONS: (u16, u16, u16) = (
        HELIOS_HWA2_ABI_VERSION,
        HELIOS_HVM1_ABI_VERSION,
        HELIOS_HOC1_ABI_VERSION,
    );
}

#[cfg(test)]
mod allocation_identity_tests {
    use super::allocation_identity::*;
    use helios_protocol::*;

    const PKG: u64 = HELIOS_PACKAGE_GENERATION;

    // ── fixtures ────────────────────────────────────────────────────────────

    /// The create-**input** form of a D3D11 shared primary: everything the UMD
    /// legitimately knows, and nothing the KMD owns. Note the two differences
    /// from `protocol`'s own `primary_desc()` fixture: `allocation_generation`
    /// is 0 and `DIRECT_FLIP_COMPATIBLE` is clear, which is precisely the
    /// partition `K4-CONTRACT.md` §1.1 pins.
    fn primary_input() -> HeliosWddmAllocationDescV2 {
        let mut d = HeliosWddmAllocationDescV2::header(PKG, 0);
        d.byte_size = 1920 * 4 * 1080;
        d.width = 1920;
        d.height = 1080;
        d.depth_or_array_size = 1;
        d.mip_levels = 1;
        d.dxgi_format = DXGI_FORMAT_B8G8R8A8_UNORM;
        d.d3d_ddi_format = D3DDDIFMT_A8R8G8B8;
        d.sample_count = 1;
        d.sample_quality = 0;
        d.allocation_kind = HELIOS_HWA2_KIND_STANDARD_PRIMARY;
        d.flags = HELIOS_HWA2_FLAG_PRIMARY
            | HELIOS_HWA2_FLAG_DISPLAYABLE
            | HELIOS_HWA2_FLAG_SHARED
            | HELIOS_HWA2_FLAG_STANDARD;
        d.bind_flags = HELIOS_HWA2_BIND_RENDER_TARGET | HELIOS_HWA2_BIND_SHADER_RESOURCE;
        d.vidpn_source = 0;
        d.standard_allocation_type = 1; // D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE
        d.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        d.memory_class = HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
        d.plane_count = 1;
        d.planes[0] = HeliosWddmPlaneRecordV2 {
            offset: 0,
            row_pitch: 1920 * 4,
            slice_pitch: 1920 * 4 * 1080,
        };
        d
    }

    /// A plain constant buffer: the non-image arm, so the geometry rules are
    /// exercised from the other side too.
    fn buffer_input() -> HeliosWddmAllocationDescV2 {
        let mut d = HeliosWddmAllocationDescV2::header(PKG, 0);
        d.byte_size = 65536;
        d.allocation_kind = HELIOS_HWA2_KIND_BUFFER;
        d.bind_flags = HELIOS_HWA2_BIND_CONSTANT_BUFFER;
        d.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        d.memory_class = HELIOS_HWA2_MEMORY_CPU_VISIBLE;
        d.flags = HELIOS_HWA2_FLAG_CPU_VISIBLE;
        d
    }

    fn host_visible_input() -> HeliosVenusMemoryAllocationV1 {
        HeliosVenusMemoryAllocationV1::new(
            PKG,
            Hvm1Role::VulkanHostVisible,
            65536,
            HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_CPU_WRITE,
        )
    }

    fn device_local_input() -> HeliosVenusMemoryAllocationV1 {
        HeliosVenusMemoryAllocationV1::new(
            PKG,
            Hvm1Role::VulkanDeviceLocal,
            4096,
            HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE,
        )
    }

    fn retirement(hqc1_value: u64) -> HeliosExtentRetirementV1 {
        HeliosExtentRetirementV1 {
            queue_context: 0xDEAD_BEEF,
            context_generation: 0x2222_2222_2222_2222,
            batch_id: 7,
            hqc1_value,
        }
    }

    // ── (b) the allocation generation lifecycle ─────────────────────────────

    /// Nonzero, minted once per allocation, and — the part `protocol` cannot
    /// see — not restarted by an adapter reset. §14 requires that a reset
    /// change every generation; if the counter restarted, a post-reset
    /// allocation would reuse a pre-reset value and a stale HOB1 use record
    /// naming it would pass its `expected_allocation_generation` check against
    /// the wrong object.
    #[test]
    fn generations_are_nonzero_unique_and_survive_an_adapter_reset() {
        let counter = GenerationCounter::new();
        let (first, counter) = AllocationIdentity::create(counter).expect("first create");
        let (second, counter) = AllocationIdentity::create(counter).expect("second create");
        assert_ne!(first.generation, 0);
        assert_ne!(second.generation, 0);
        assert_ne!(first.generation, second.generation);

        let counter = counter.on_adapter_reset().expect("reset bumps the epoch");
        let (third, _) = AllocationIdentity::create(counter).expect("post-reset create");
        assert_ne!(third.generation, first.generation);
        assert_ne!(third.generation, second.generation);
        assert!(third.generation > second.generation);
        assert_ne!(third.epoch, first.epoch);

        // And the pre-reset objects are refused, not revalidated.
        assert_eq!(
            first.open(third.epoch),
            Err(IdentityRefusal::StaleAdapterEpoch {
                object_epoch: first.epoch,
                adapter_epoch: third.epoch,
            })
        );
    }

    /// The counter refuses at exhaustion instead of wrapping to 0 or saturating
    /// at `u64::MAX`. Both alternatives hand one generation to two live
    /// allocations; 0 additionally collides with the "user mode supplied it"
    /// sentinel.
    #[test]
    fn the_generation_counter_refuses_exhaustion_rather_than_repeating() {
        let counter = GenerationCounter {
            next: u64::MAX - 1,
            epoch: 1,
        };
        let (value, counter) = counter.mint().expect("the penultimate value is legal");
        assert_eq!(value, u64::MAX - 1);
        assert_eq!(counter.next, u64::MAX);
        assert_eq!(
            counter.mint().err(),
            Some(IdentityRefusal::GenerationSpaceExhausted)
        );

        // A zeroed structure never mints; it is not a valid counter.
        assert_eq!(
            GenerationCounter { next: 0, epoch: 0 }.mint().err(),
            Some(IdentityRefusal::GenerationCounterUninitialised)
        );
        assert_eq!(
            GenerationCounter {
                next: 1,
                epoch: u32::MAX,
            }
            .on_adapter_reset()
            .err(),
            Some(IdentityRefusal::AdapterEpochSpaceExhausted)
        );
    }

    /// Open takes a reference and returns the same generation. This is the
    /// executable form of "`DxgkDdiOpenAllocation` never writes it": the retired
    /// `HeliosWddmOpenIdentity` restamped the first 48 bytes at open, so two
    /// openers could disagree about what they had.
    #[test]
    fn open_never_restamps_the_generation() {
        let (identity, _) = AllocationIdentity::create(GenerationCounter::new()).expect("create");
        let mut current = identity;
        for expected_opens in 1..=8u32 {
            current = current.open(identity.epoch).expect("open");
            assert_eq!(current.generation, identity.generation);
            assert_eq!(current.epoch, identity.epoch);
            assert_eq!(current.opens, expected_opens);
        }

        let saturated = AllocationIdentity {
            opens: u32::MAX,
            ..identity
        };
        assert_eq!(
            saturated.open(identity.epoch),
            Err(IdentityRefusal::OpenCountOverflow)
        );
    }

    /// The generation is a stale check, never a lookup key. The only exposed
    /// direction takes an object the caller already holds; a zero expectation
    /// is a refusal, not a wildcard.
    #[test]
    fn the_generation_is_a_stale_check_and_zero_is_never_a_wildcard() {
        let (identity, counter) =
            AllocationIdentity::create(GenerationCounter::new()).expect("create");
        let (other, _) = AllocationIdentity::create(counter).expect("second create");

        assert_eq!(
            identity.matches_expected(identity.generation, identity.epoch),
            Ok(true)
        );
        assert_eq!(
            identity.matches_expected(other.generation, identity.epoch),
            Ok(false)
        );
        assert_eq!(
            identity.matches_expected(0, identity.epoch),
            Err(IdentityRefusal::ExpectedGenerationZero)
        );
        assert_eq!(
            identity.matches_expected(identity.generation, identity.epoch + 1),
            Err(IdentityRefusal::StaleAdapterEpoch {
                object_epoch: identity.epoch,
                adapter_epoch: identity.epoch + 1,
            })
        );
    }

    // ── (a) the HWA2 create-input / create-output state machine ─────────────

    /// A UMD may not prefill any KMD-owned field. All three refusals are
    /// distinct and named, so the KMD counter says which one a UMD got wrong.
    #[test]
    fn hwa2_create_input_refuses_every_kmd_owned_field() {
        let good = encode_hwa2(&primary_input());
        assert!(hwa2_admit_create_input(&good, PKG).is_ok());

        let mut with_generation = primary_input();
        with_generation.allocation_generation = 0x51;
        assert_eq!(
            hwa2_admit_create_input(&encode_hwa2(&with_generation), PKG),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::AllocationGenerationNonZeroOnInput { found: 0x51 }
            ))
        );

        for bit in [
            HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE,
            HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY,
        ] {
            let mut d = primary_input();
            // C44 also requires the sentinel; set it so the *only* thing wrong
            // with this record is that the UMD claimed a KMD-owned bit.
            if bit == HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY {
                d.vidpn_source = D3DDDI_ID_UNINITIALIZED;
            }
            d.flags |= bit;
            assert_eq!(
                hwa2_admit_create_input(&encode_hwa2(&d), PKG),
                Err(IdentityRefusal::Hwa2(
                    HeliosAllocDescRejection::KmdOwnedFlagSetOnInput { bits: bit }
                )),
                "flag {bit:#x} is KMD-owned and must be refused on input"
            );
        }
    }

    /// The write-back produces a record that passes create-output and that no
    /// longer passes create-input — the two stages partition the record, they
    /// do not overlap.
    #[test]
    fn hwa2_write_back_passes_create_output_and_then_fails_create_input() {
        let input = hwa2_admit_create_input(&encode_hwa2(&primary_input()), PKG).expect("admit");
        let output =
            hwa2_stamp_create_output(&input, 0x51, HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE, PKG)
                .expect("stamp");

        assert_eq!(output.validate_create_output(PKG), Ok(()));
        assert_eq!(output.validate(PKG), Ok(()));
        assert_eq!(
            output.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationNonZeroOnInput { found: 0x51 })
        );
    }

    /// A zero generation is refused at the stamp, before it can reach the
    /// record — the KMD's own bug is caught by the KMD's own counter rather
    /// than by the record validator after the object already exists.
    #[test]
    fn hwa2_create_output_with_a_zero_generation_is_refused() {
        let input = hwa2_admit_create_input(&encode_hwa2(&buffer_input()), PKG).expect("admit");
        assert_eq!(
            hwa2_stamp_create_output(&input, 0, 0, PKG),
            Err(IdentityRefusal::GenerationZeroAtStamp)
        );

        // And a record that reaches an opener with a zero generation is refused
        // there too, so neither end can be the only guard.
        let mut unstamped = input;
        unstamped.allocation_generation = 0;
        assert_eq!(
            hwa2_admit_open(&encode_hwa2(&unstamped), PKG),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::AllocationGenerationZero
            ))
        );
    }

    /// Every non-KMD field is byte-identical between input and output.
    ///
    /// Checked on the encoded byte images, not only on the fields: the two
    /// buffers must differ in exactly bytes 16..24 and in exactly the
    /// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] bits of the flag word at offset 68, and be
    /// equal everywhere else. This is the check that would catch a KMD that
    /// "helpfully" recomputed a pitch or normalised a format.
    #[test]
    fn hwa2_echoes_every_non_kmd_byte_verbatim() {
        for input in [primary_input(), buffer_input()] {
            let kmd_flags = if input.has_flag(HELIOS_HWA2_FLAG_PRIMARY) {
                HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE
            } else {
                0
            };
            let admitted = hwa2_admit_create_input(&encode_hwa2(&input), PKG).expect("admit");
            let output =
                hwa2_stamp_create_output(&admitted, 0xABCD, kmd_flags, PKG).expect("stamp");

            assert!(hwa2_echoed_fields_equal(&admitted, &output));

            let before = encode_hwa2(&admitted);
            let after = encode_hwa2(&output);
            for offset in 0..before.len() {
                if (16..24).contains(&offset) {
                    continue; // allocation_generation, KMD-owned
                }
                if (68..72).contains(&offset) {
                    continue; // the flag word, masked separately below
                }
                assert_eq!(
                    before[offset], after[offset],
                    "byte {offset} is not KMD-owned and must be echoed verbatim"
                );
            }
            let flags_before = u32::from_le_bytes([before[68], before[69], before[70], before[71]]);
            let flags_after = u32::from_le_bytes([after[68], after[69], after[70], after[71]]);
            assert_eq!(
                flags_before & !HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
                flags_after & !HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
                "only the two KMD-owned bits may change in the flag word"
            );
            assert_eq!(flags_after & HELIOS_HWA2_FLAG_KMD_OWNED_MASK, kmd_flags);
        }
    }

    /// The stamp is not a rubber stamp: it re-validates the pairing it just
    /// created, so a Direct Flip claim on a non-primary or a C44 primary bit
    /// against a concrete VidPn source fails the create rather than shipping a
    /// descriptor no opener will accept.
    #[test]
    fn the_kmd_stamp_is_not_a_rubber_stamp() {
        let buffer = hwa2_admit_create_input(&encode_hwa2(&buffer_input()), PKG).expect("admit");
        assert_eq!(
            hwa2_stamp_create_output(&buffer, 1, HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE, PKG),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::DirectFlipWithoutPrimary
            ))
        );

        let primary = hwa2_admit_create_input(&encode_hwa2(&primary_input()), PKG).expect("admit");
        assert_eq!(
            hwa2_stamp_create_output(&primary, 1, HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY, PKG),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::D3D12RuntimePrimaryNotSentinel { found: 0 }
            ))
        );

        // And the KMD may not author a bit outside its own partition, even a
        // legal one — those belong to the UMD and are echoed, never added.
        assert_eq!(
            hwa2_stamp_create_output(&primary, 1, HELIOS_HWA2_FLAG_PROTECTED, PKG),
            Err(IdentityRefusal::KmdFlagOutsidePartition {
                bits: HELIOS_HWA2_FLAG_PROTECTED
            })
        );
    }

    /// An opener validates the full output contract and produces the same
    /// record it was given.
    ///
    /// The "never writes" half is carried by the signature — [`hwa2_admit_open`]
    /// takes `&[u8]` and there is no `&mut` on the path — so the assertion here
    /// is the observable consequence: re-encoding what the opener parsed
    /// reproduces the original buffer bit for bit, which a normalising or
    /// restamping opener would not.
    #[test]
    fn open_is_a_pure_read() {
        let input = hwa2_admit_create_input(&encode_hwa2(&primary_input()), PKG).expect("admit");
        let output =
            hwa2_stamp_create_output(&input, 0x51, HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE, PKG)
                .expect("stamp");
        let on_the_wire = encode_hwa2(&output);

        let opened = hwa2_admit_open(&on_the_wire, PKG).expect("open");
        assert_eq!(opened, output);
        assert_eq!(encode_hwa2(&opened), on_the_wire);

        // A foreign package generation is refused at open just as at create;
        // there is no "open is lenient" arm.
        assert_eq!(
            hwa2_admit_open(&on_the_wire, PKG ^ 1),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::PackageGeneration {
                    found: PKG,
                    expected: PKG ^ 1,
                }
            ))
        );
    }

    // ── (c) HVM1 role admission ─────────────────────────────────────────────

    /// Role 4 has no CPU VA, so it may not claim a CPU access bit and may never
    /// be locked. Both halves matter: `permitted_access` refuses a record that
    /// *claims* CPU access, and [`hvm1_lock_admissible`] refuses a later `Lock2`
    /// against a record that never claimed it.
    #[test]
    fn hvm1_role_four_refuses_cpu_access_and_is_never_lockable() {
        let (record, role) =
            hvm1_admit_create_input(&encode_hvm1(&device_local_input()), PKG).expect("admit");
        assert_eq!(role, Hvm1Role::VulkanDeviceLocal);
        assert_eq!(record.cache_policy, HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE);
        assert_eq!(
            hvm1_lock_admissible(Hvm1Role::VulkanDeviceLocal),
            Err(IdentityRefusal::Role4NeverLockable)
        );

        for bit in [HELIOS_HVM1_ACCESS_CPU_READ, HELIOS_HVM1_ACCESS_CPU_WRITE] {
            let mut cpu = device_local_input();
            cpu.access |= bit;
            // `.err()` rather than the whole `Result`, here and below:
            // `HeliosVenusMemoryAllocationV1` derives `Pod`/`Zeroable` but not
            // `PartialEq` (`native_render.rs:1798`), so the Ok arm is not
            // comparable and `assert_eq!` on the Result does not compile.
            assert_eq!(
                hvm1_admit_create_input(&encode_hvm1(&cpu), PKG).err(),
                Some(IdentityRefusal::Hvm1(Hvm1Reject::AccessNotRoleCompatible)),
                "role 4 must refuse CPU access bit {bit:#x}"
            );
        }

        for role in [
            Hvm1Role::ReplyPool,
            Hvm1Role::VulkanHostVisible,
            Hvm1Role::Feedback,
        ] {
            assert_eq!(hvm1_lock_admissible(role), Ok(()));
        }
    }

    /// Role 1 fixes its own size, and the KMD entry point enforces it — the
    /// point being that the size gate is reached through the same
    /// parse-then-validate path as everything else, not bypassed by a
    /// convenience constructor. (`protocol` proves the rule on a constructed
    /// record; this proves the KMD's entry reaches it.)
    #[test]
    fn hvm1_reply_pool_size_is_fixed_at_the_kmd_entry_point() {
        let pool = HeliosVenusMemoryAllocationV1::new_reply_pool(PKG);
        assert_eq!(pool.byte_size, HELIOS_HVM1_REPLY_POOL_BYTES);
        assert_eq!(HELIOS_HVM1_REPLY_POOL_BYTES, 4 * 1024 * 1024);
        let (_, role) = hvm1_admit_create_input(&encode_hvm1(&pool), PKG).expect("admit");
        assert_eq!(role, Hvm1Role::ReplyPool);

        let mut half = pool;
        half.byte_size = HELIOS_HVM1_REPLY_POOL_BYTES / 2;
        assert_eq!(
            hvm1_admit_create_input(&encode_hvm1(&half), PKG).err(),
            Some(IdentityRefusal::Hvm1(Hvm1Reject::ByteSizeNotExactForRole))
        );
    }

    /// The write-back fields are refused nonzero on input and exact on output,
    /// and the stamp echoes everything else.
    #[test]
    fn hvm1_write_back_fields_are_refused_nonzero_on_input() {
        let input = host_visible_input();
        for prefill in [
            |r: &mut HeliosVenusMemoryAllocationV1| r.object_generation = 9,
            |r: &mut HeliosVenusMemoryAllocationV1| r.segment_page_shift = 12,
            |r: &mut HeliosVenusMemoryAllocationV1| r.allocation_alignment = 4096,
        ] {
            let mut prefilled = input;
            prefill(&mut prefilled);
            assert_eq!(
                hvm1_admit_create_input(&encode_hvm1(&prefilled), PKG).err(),
                Some(IdentityRefusal::Hvm1(
                    Hvm1Reject::WriteBackFieldNotZeroOnInput
                ))
            );
        }

        let (admitted, role) = hvm1_admit_create_input(&encode_hvm1(&input), PKG).expect("admit");
        assert_eq!(role, Hvm1Role::VulkanHostVisible);
        let output = hvm1_stamp_create_output(&admitted, 42, 4096, PKG).expect("stamp");
        assert_eq!(output.object_generation, 42);
        assert_eq!(output.segment_page_shift, HELIOS_HVM1_SEGMENT_PAGE_SHIFT);
        assert_eq!(output.allocation_alignment, 4096);

        // Everything else is echoed: zero the three write-back fields back out
        // and the record is the admitted input again.
        let mut echoed = output;
        echoed.object_generation = 0;
        echoed.segment_page_shift = 0;
        echoed.allocation_alignment = 0;
        assert!(hvm1_fields_equal(&echoed, &admitted));

        assert_eq!(
            hvm1_stamp_create_output(&admitted, 0, 4096, PKG).err(),
            Some(IdentityRefusal::GenerationZeroAtStamp)
        );
        assert_eq!(
            hvm1_stamp_create_output(&admitted, 42, 4095, PKG).err(),
            Some(IdentityRefusal::Hvm1(
                Hvm1Reject::AllocationAlignmentInvalid
            ))
        );
    }

    /// Placement is refused, per role and by name, until the aperture exists.
    #[test]
    fn hvm1_placement_is_refused_until_the_aperture_segment_exists() {
        for role in [
            Hvm1Role::ReplyPool,
            Hvm1Role::VulkanHostVisible,
            Hvm1Role::Feedback,
            Hvm1Role::VulkanDeviceLocal,
        ] {
            assert_eq!(
                hvm1_admit_placement(role, false),
                Err(IdentityRefusal::ApertureSegmentNotExposed {
                    role: role.to_u32()
                }),
                "role {} must be counted without an aperture",
                role.to_u32()
            );
            let placement = hvm1_admit_placement(role, true).expect("aperture present");
            assert_eq!(placement.preferred_segment, HELIOS_SEGMENT_ID_APERTURE);
        }
    }

    /// The bridge `protocol` does not ship: what the KMD writes must equal what
    /// the role table says, field by field, and the refusal names the field.
    #[test]
    fn hvm1_written_flags_must_match_the_role_table_field_by_field() {
        for role in [
            Hvm1Role::ReplyPool,
            Hvm1Role::VulkanHostVisible,
            Hvm1Role::Feedback,
            Hvm1Role::VulkanDeviceLocal,
        ] {
            let placement = role.placement();
            let faithful = WrittenAllocationFlags {
                cpu_visible: placement.cpu_visible,
                cached: placement.cached,
                accessed_physically: placement.accessed_physically,
                explicit_residency_notification: placement.explicit_residency_notification,
                disable_partial_residency: placement.disable_partial_residency,
                restricted_to_single_segment: placement.restricted_to_single_segment,
                preferred_segment: placement.preferred_segment,
                cache_policy: placement.cache_policy,
            };
            assert_eq!(hvm1_written_flags_match(&placement, &faithful), Ok(()));

            // §10.7 fixes the same four bits for every role; dropping either
            // Flags2 bit is the drift obligation 8 has no field evidence for.
            let mut dropped = faithful;
            dropped.disable_partial_residency = false;
            assert_eq!(
                hvm1_written_flags_match(&placement, &dropped),
                Err(IdentityRefusal::PlacementMismatch {
                    field: PlacementField::DisablePartialResidency
                })
            );
            let mut dropped = faithful;
            dropped.restricted_to_single_segment = false;
            assert_eq!(
                hvm1_written_flags_match(&placement, &dropped),
                Err(IdentityRefusal::PlacementMismatch {
                    field: PlacementField::RestrictedToSingleSegment
                })
            );
            let mut flipped = faithful;
            flipped.cpu_visible = !flipped.cpu_visible;
            assert_eq!(
                hvm1_written_flags_match(&placement, &flipped),
                Err(IdentityRefusal::PlacementMismatch {
                    field: PlacementField::CpuVisible
                })
            );
            let mut foreign = faithful;
            foreign.preferred_segment = u32::MAX;
            assert_eq!(
                hvm1_written_flags_match(&placement, &foreign),
                Err(IdentityRefusal::PlacementMismatch {
                    field: PlacementField::PreferredSegment
                })
            );
        }
    }

    // ── (d) HOC1 pool admission and the extent ledger ───────────────────────

    /// HOC1's single write-back, through the KMD entry points.
    #[test]
    fn hoc1_create_stamps_the_generation_exactly_once() {
        let bytes = encode_hoc1(&HeliosOuterCommandAllocationV1::new(PKG));
        let input = hoc1_admit_create_input(&bytes, PKG).expect("admit");
        assert_eq!(input.allocation_generation, 0);
        assert_eq!(input.byte_size, HELIOS_HOC1_POOL_BYTES);

        let output = hoc1_stamp_create_output(&input, 0x9, PKG).expect("stamp");
        assert_eq!(output.allocation_generation, 0x9);
        assert_eq!(
            hoc1_admit_create_input(&encode_hoc1(&output), PKG),
            Err(IdentityRefusal::Hoc1(
                HeliosOuterCommandAllocRejection::AllocationGenerationNonZeroOnInput { found: 0x9 }
            ))
        );
        assert_eq!(
            hoc1_stamp_create_output(&input, 0, PKG),
            Err(IdentityRefusal::GenerationZeroAtStamp)
        );
    }

    /// The live-extent cap is a property of the ledger across calls, which is
    /// what `validate_pool_extent`'s `live_extents` *argument* cannot express
    /// on its own: reserve 256 and the 257th is refused; retire one and the
    /// next reservation is admitted again.
    #[test]
    fn the_pool_ledger_enforces_the_live_extent_cap_across_calls() {
        let mut pool = Hoc1Pool::new();
        for i in 0..HELIOS_HOC1_MAX_LIVE_EXTENTS {
            let offset = u64::from(i) * u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT);
            let (state, next) = pool
                .reserve(offset, 4096)
                .expect("reservation inside the pool");
            assert_eq!(state, HeliosExtentSealState::Reserved);
            assert!(state.cpu_writable());
            pool = next;
        }
        assert_eq!(pool.live_extents, HELIOS_HOC1_MAX_LIVE_EXTENTS);
        assert_eq!(
            pool.reserve(0, 4096),
            Err(IdentityRefusal::Extent(
                HeliosExtentRejection::LiveExtentLimit {
                    live: HELIOS_HOC1_MAX_LIVE_EXTENTS,
                    limit: HELIOS_HOC1_MAX_LIVE_EXTENTS,
                }
            ))
        );

        let pool = pool.retire(&retirement(10), 10).expect("retire one");
        assert_eq!(pool.live_extents, HELIOS_HOC1_MAX_LIVE_EXTENTS - 1);
        let (_, pool) = pool.reserve(0, 4096).expect("a slot came free");
        assert_eq!(pool.live_extents, HELIOS_HOC1_MAX_LIVE_EXTENTS);
    }

    /// One extent's whole life, with the ledger and the seal state moving
    /// together. `cpu_writable` is true only while Reserved: from Sealed onward
    /// the write-combining drain has already published the bytes.
    #[test]
    fn the_pool_ledger_walks_reserve_seal_submit_retire() {
        let pool = Hoc1Pool::new();
        let (mut state, pool) = pool.reserve(0, HELIOS_HOB1_MAX_BYTES).expect("reserve");
        assert_eq!(pool.live_extents, 1);
        assert!(state.cpu_writable());

        for next in [
            HeliosExtentSealState::Sealed,
            HeliosExtentSealState::Submitted,
            HeliosExtentSealState::Retired,
        ] {
            state = state.advance(next).expect("legal C65 transition");
            assert!(!state.cpu_writable());
        }

        // The GPU has not reached the extent's value yet: retiring now would
        // hand back bytes in-flight work still reads.
        assert_eq!(
            pool.retire(&retirement(12), 11),
            Err(IdentityRefusal::ExtentNotRetiredYet {
                recorded: 12,
                completed: 11,
            })
        );
        let pool = pool.retire(&retirement(12), 12).expect("retire");
        assert_eq!(pool.live_extents, 0);
        assert_eq!(pool.last_retired_hqc1_value, 12);
        assert_eq!(
            state.advance(HeliosExtentSealState::Free),
            Ok(HeliosExtentSealState::Free)
        );

        // Double retire has no live extent to consume.
        assert_eq!(
            pool.retire(&retirement(13), 13),
            Err(IdentityRefusal::NoLiveExtent)
        );
    }

    /// Retirement values must strictly increase *across calls* on one context,
    /// which is the state `HeliosExtentRetirementV1::validate`'s
    /// `previous_hqc1_value` argument leaves to the caller.
    #[test]
    fn retirement_values_must_strictly_increase_across_calls() {
        let (_, pool) = Hoc1Pool::new().reserve(0, 4096).expect("reserve");
        let (_, pool) = pool
            .reserve(u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT), 4096)
            .expect("reserve");
        let pool = pool.retire(&retirement(5), 100).expect("first retire");
        assert_eq!(
            pool.retire(&retirement(5), 100),
            Err(IdentityRefusal::Retirement(
                HeliosRetirementRejection::Hqc1ValueNotIncreasing {
                    found: 5,
                    previous: 5,
                }
            ))
        );
        let pool = pool.retire(&retirement(6), 100).expect("second retire");
        assert_eq!(pool.live_extents, 0);
    }

    /// The two bounds the pool geometry fixes, reached through the ledger: an
    /// extent may not exceed the 15 MiB HOB1 limit and may not leave the 64 MiB
    /// pool, and the offset must be 64 KiB aligned.
    #[test]
    fn extent_bounds_are_the_hob1_limit_and_the_pool_end() {
        let pool = Hoc1Pool::new();
        assert_eq!(HELIOS_HOB1_MAX_BYTES, 15 * 1024 * 1024);
        assert_eq!(HELIOS_HOC1_POOL_BYTES, 64 * 1024 * 1024);
        assert_eq!(HELIOS_HOC1_EXTENT_ALIGNMENT, 64 * 1024);

        assert_eq!(
            pool.reserve(0, HELIOS_HOB1_MAX_BYTES + 1),
            Err(IdentityRefusal::Extent(
                HeliosExtentRejection::ByteLengthAboveHob1Limit {
                    found: HELIOS_HOB1_MAX_BYTES + 1,
                    limit: HELIOS_HOB1_MAX_BYTES,
                }
            ))
        );
        assert_eq!(
            pool.reserve(
                HELIOS_HOC1_POOL_BYTES - u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT),
                65537
            ),
            Err(IdentityRefusal::Extent(
                HeliosExtentRejection::RangeOutsidePool {
                    end: HELIOS_HOC1_POOL_BYTES + 1,
                    pool_bytes: HELIOS_HOC1_POOL_BYTES,
                }
            ))
        );
        assert_eq!(
            pool.reserve(4096, 4096),
            Err(IdentityRefusal::Extent(
                HeliosExtentRejection::OffsetMisaligned {
                    offset: 4096,
                    alignment: HELIOS_HOC1_EXTENT_ALIGNMENT,
                }
            ))
        );
        assert_eq!(
            pool.reserve(0, 0),
            Err(IdentityRefusal::Extent(
                HeliosExtentRejection::ByteLengthZero
            ))
        );
    }

    // ── (e) the bounds discipline of `from_private_data` ────────────────────

    /// One byte short, exactly right, one byte over, and correct-length at an
    /// odd address — for all three records.
    ///
    /// The short arm is the one that matters: a KMD that read 168 bytes out of
    /// a 167-byte runtime buffer takes an out-of-bounds *kernel* read, and no
    /// validator downstream can catch it because the damage is already done.
    /// The oversize arm matters for a different reason — `from_private_data` is
    /// an exact-length gate, not a prefix parser, so a longer buffer is a
    /// disagreement about the ABI and not a superset.
    #[test]
    fn hwa2_private_data_is_an_exact_length_gate() {
        let record = primary_input();
        let bytes = encode_hwa2(&record);
        let expected = HELIOS_HWA2_BYTES as usize;
        assert_eq!(bytes.len(), expected);

        assert_eq!(hwa2_admit_create_input(&bytes, PKG), Ok(record));
        assert_eq!(
            hwa2_admit_create_input(&bytes[..expected - 1], PKG),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::PrivateDataSize {
                    found: expected - 1,
                    expected,
                }
            ))
        );

        let mut oversize = [0u8; (HELIOS_HWA2_BYTES as usize) + 1];
        oversize[..expected].copy_from_slice(&bytes);
        assert_eq!(
            hwa2_admit_create_input(&oversize, PKG),
            Err(IdentityRefusal::Hwa2(
                HeliosAllocDescRejection::PrivateDataSize {
                    found: expected + 1,
                    expected,
                }
            ))
        );

        // Correct length at an odd address must parse, not be refused with a
        // length error naming the correct length: the runtime's buffer carries
        // no alignment promise, so the reader is an unaligned read by contract.
        let mut staging = [0u8; (HELIOS_HWA2_BYTES as usize) + 8];
        staging[3..3 + expected].copy_from_slice(&bytes);
        assert_eq!(
            hwa2_admit_create_input(&staging[3..3 + expected], PKG),
            Ok(record)
        );
    }

    #[test]
    fn hvm1_private_data_is_an_exact_length_gate() {
        let record = host_visible_input();
        let bytes = encode_hvm1(&record);
        let expected = HELIOS_HVM1_SIZE as usize;
        assert_eq!(bytes.len(), expected);

        let (parsed, role) = hvm1_admit_create_input(&bytes, PKG).expect("exact length");
        assert!(hvm1_fields_equal(&parsed, &record));
        assert_eq!(role, Hvm1Role::VulkanHostVisible);

        // `Hvm1Reject::PrivateDataSizeMismatch` carries no found/expected pair,
        // unlike HWA2's and HOC1's `PrivateDataSize { found, expected }`. That
        // asymmetry is `protocol`'s, not this model's; the refusal is still
        // named, and its stable code is 0x0412.
        assert_eq!(
            hvm1_admit_create_input(&bytes[..expected - 1], PKG).err(),
            Some(IdentityRefusal::Hvm1(Hvm1Reject::PrivateDataSizeMismatch))
        );
        let mut oversize = [0u8; (HELIOS_HVM1_SIZE as usize) + 1];
        oversize[..expected].copy_from_slice(&bytes);
        assert_eq!(
            hvm1_admit_create_input(&oversize, PKG).err(),
            Some(IdentityRefusal::Hvm1(Hvm1Reject::PrivateDataSizeMismatch))
        );
        assert_eq!(Hvm1Reject::PrivateDataSizeMismatch.code(), 0x0412);

        let mut staging = [0u8; (HELIOS_HVM1_SIZE as usize) + 8];
        staging[3..3 + expected].copy_from_slice(&bytes);
        let (parsed, _) =
            hvm1_admit_create_input(&staging[3..3 + expected], PKG).expect("odd address");
        assert!(hvm1_fields_equal(&parsed, &record));
    }

    #[test]
    fn hoc1_private_data_is_an_exact_length_gate() {
        let record = HeliosOuterCommandAllocationV1::new(PKG);
        let bytes = encode_hoc1(&record);
        let expected = HELIOS_HOC1_BYTES as usize;
        assert_eq!(bytes.len(), expected);

        assert_eq!(hoc1_admit_create_input(&bytes, PKG), Ok(record));
        assert_eq!(
            hoc1_admit_create_input(&bytes[..expected - 1], PKG),
            Err(IdentityRefusal::Hoc1(
                HeliosOuterCommandAllocRejection::PrivateDataSize {
                    found: expected - 1,
                    expected,
                }
            ))
        );
        let mut oversize = [0u8; (HELIOS_HOC1_BYTES as usize) + 1];
        oversize[..expected].copy_from_slice(&bytes);
        assert_eq!(
            hoc1_admit_create_input(&oversize, PKG),
            Err(IdentityRefusal::Hoc1(
                HeliosOuterCommandAllocRejection::PrivateDataSize {
                    found: expected + 1,
                    expected,
                }
            ))
        );

        let mut staging = [0u8; (HELIOS_HOC1_BYTES as usize) + 8];
        staging[3..3 + expected].copy_from_slice(&bytes);
        assert_eq!(
            hoc1_admit_create_input(&staging[3..3 + expected], PKG),
            Ok(record)
        );
    }

    /// The discriminator, and the reason it cannot be a length switch: HVM1 and
    /// HOC1 are both 64 bytes. A buffer whose magic and length disagree selects
    /// nothing at all — §10.3 forbids falling back to a legacy parser.
    #[test]
    fn the_private_data_discriminator_needs_the_magic_not_just_the_length() {
        assert_eq!(HELIOS_HVM1_SIZE, HELIOS_HOC1_BYTES);
        assert_eq!(
            classify_alloc_private_data(&encode_hwa2(&primary_input())),
            Some(AllocPrivateKind::Hwa2)
        );
        assert_eq!(
            classify_alloc_private_data(&encode_hvm1(&host_visible_input())),
            Some(AllocPrivateKind::Hvm1)
        );
        assert_eq!(
            classify_alloc_private_data(&encode_hoc1(&HeliosOuterCommandAllocationV1::new(PKG))),
            Some(AllocPrivateKind::Hoc1)
        );

        // Right magic, wrong length, and the retired 48-byte record: both
        // select nothing.
        let hvm1 = encode_hvm1(&host_visible_input());
        assert_eq!(classify_alloc_private_data(&hvm1[..48]), None);
        assert_eq!(classify_alloc_private_data(&[0u8; 48]), None);
        assert_eq!(classify_alloc_private_data(&[]), None);
        assert_eq!(classify_alloc_private_data(&[0u8; 3]), None);
    }

    /// The hand-written encoders and `protocol`'s own readers agree on every
    /// offset, in both directions. If either table drifts, this fails — which
    /// is the entire reason the encoders exist rather than borrowing
    /// `bytemuck::bytes_of`.
    #[test]
    fn the_independent_encoder_agrees_with_protocols_reader() {
        for desc in [primary_input(), buffer_input()] {
            assert_eq!(
                HeliosWddmAllocationDescV2::from_private_data(&encode_hwa2(&desc)),
                Ok(desc)
            );
        }
        for record in [
            host_visible_input(),
            device_local_input(),
            HeliosVenusMemoryAllocationV1::new_reply_pool(PKG),
        ] {
            let parsed = HeliosVenusMemoryAllocationV1::from_private_data(&encode_hvm1(&record))
                .expect("round trip");
            assert!(hvm1_fields_equal(&parsed, &record));
        }
        let hoc1 = HeliosOuterCommandAllocationV1::new(PKG);
        assert_eq!(
            HeliosOuterCommandAllocationV1::from_private_data(&encode_hoc1(&hoc1)),
            Ok(hoc1)
        );

        // None of the three ABI versions is negotiable, and the constants the
        // encoders write are the constants the readers require.
        assert_eq!(ADMITTED_ABI_VERSIONS, (2, 1, 1));
    }
}

/// HTS1 translation sessions and HQA1 outer-context attach — the K5 rules, with
/// every kernel handle removed.
///
/// `kmd_render/src/ddi/translation_session.rs` owns the DDI plumbing (the
/// `DXGKARG_CREATECONTEXT` decode, the boxed objects, the CSPRNG, the counters,
/// the locking); this module owns the part that is a function of its arguments,
/// so it can be tested. `helios_protocol` owns every wire record and its
/// validator — nothing here re-derives one, and the wrapping refusal variants
/// carry protocol's own named rejection so the KMD counter and the ETW field
/// share one vocabulary.
pub mod translation_session {
    use helios_protocol::native_render::{
        admit_snapshot, Hnr2CapacityRefusal, HELIOS_HVR1_MAX_SNAPSHOT_BYTES,
    };
    use helios_protocol::native_render::{
        Hvm1Role, HELIOS_HNR2_ACCESS_WRITE, HELIOS_HVM1_REPLY_POOL_BYTES,
        HELIOS_HVM1_REPLY_SLOT_BYTES, HELIOS_HVM1_REPLY_SLOT_COUNT,
    };
    use helios_protocol::translation_session::{
        admit_host_dispatch_enqueue, admit_new_session, check_generation_match, AttachRefusal,
        CapabilityRefusal, GenerationField, GenerationRefusal, HeliosAttachAdmission,
        HeliosAttachExpectation, HeliosCapacityLimit, HeliosCapacityRefusal, HeliosEngineClass,
        HeliosQueueAttachV1, HeliosSessionAdmission, HeliosSessionCapability,
        HeliosTranslationEndpointV1, HeliosTranslationSessionInitV1,
        HeliosTranslationSessionReplyV1, InitRefusal, HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
    };

    /// Endpoint slots one session can hold, as an array bound.
    ///
    /// `#![no_std]` here has no allocator, so the endpoint table is inline. 64
    /// entries × 16 bytes is 1 KiB per session and 16 sessions per process is the
    /// protocol cap, so the worst case is 16 KiB of nonpaged state per process.
    pub const ENDPOINT_SLOTS: usize = HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION as usize;

    /// Reply slots in one session's role-1 pool, as an array bound.
    pub const REPLY_SLOTS: usize = HELIOS_HVM1_REPLY_SLOT_COUNT as usize;

    /// Why a session-model operation was refused. Every variant is a distinct
    /// counted refusal in `kmd_render`; none is ever silently repaired.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SessionRefusal {
        /// An HTS1 INIT request or reply was refused by `protocol`.
        Init(InitRefusal),
        /// An HQA1 attach packet was refused by `protocol`.
        Attach(AttachRefusal),
        /// A bounded capacity declared by `protocol` was exhausted.
        Capacity(HeliosCapacityRefusal),
        /// A bounded native-side pool declared by `protocol` was exhausted. Its
        /// snapshot arms are the point at which the caller drops its lock and
        /// event-waits for the oldest C51 owner; they are not a resource failure.
        Hnr2Capacity(Hnr2CapacityRefusal),

        /// The operation needs a live (INIT-completed) session and this one has
        /// not run INIT yet.
        SessionProvisional,
        /// A second INIT arrived on a session that already ran one. Section 10.4
        /// grants exactly one host Venus context and `VkInstance` per session:
        /// "No second `VkInstance` may be created in that host context."
        SessionAlreadyInitialised,
        /// The session is `Draining` — reset, removal or teardown stopped
        /// admission. Every later admission fails; nothing is revalidated.
        ///
        /// ⚠ No *refusal* reaches this state. Every refusal arm returns before its
        /// mutation, so a refused request leaves the session byte-identical and the
        /// next valid one is admitted. §17.6's "any outer-allocation operand on
        /// control poisons the session" is K6's, in the Render decoder that
        /// classifies the opcode — and it must call [`TranslationSession::begin_draining`]
        /// *and* wake the C51 waiters device-lost (§14:3430), which is why the
        /// escalation lives in the platform half rather than here.
        SessionDraining,
        /// The process released a session it never admitted. An accounting bug,
        /// not a wire event, and refused rather than wrapped: a wrapped live
        /// count would let a 17th session in.
        SessionLedgerUnderflow,
        /// The 64-bit session-generation space is exhausted. Deliberately not
        /// saturating, for [`super::allocation_identity`]'s reason: a saturated
        /// counter hands one generation to two live sessions and every exact-match
        /// check starts admitting the wrong session's packets.
        SessionGenerationSpaceExhausted,
        /// The KMD's CSPRNG produced [`HeliosSessionCapability::INVALID`]. The
        /// all-zero pair is the invalidated sentinel, so it can never be issued
        /// as a live capability — the caller draws again or fails INIT.
        CapabilityIsInvalidSentinel,

        /// A second role-1 pool was offered to a session that already has one.
        /// The raw device creates exactly one (section 10.7, lines 2037-2039).
        ReplyPoolAlreadyBound,
        /// The operation needs the role-1 pool and none is bound.
        ReplyPoolNotBound,
        /// The allocation offered as a reply pool is not role 1.
        ReplyPoolRoleMismatch { found: u32 },
        /// The allocation offered as a reply pool is not exactly 4 MiB.
        /// Unreachable while `protocol`'s HVM1 validator enforces the role's exact
        /// size at create; kept because the pool's slot arithmetic is derived from
        /// this number and an unchecked assumption here is a range error later.
        ReplyPoolSizeMismatch { found: u64 },
        /// The KMD wrote a zero `object_generation` into the pool's HVM1
        /// write-back. Every HNR2 use record naming the pool carries this value,
        /// and `protocol` refuses a zero `expected_allocation_generation` on the
        /// wire — refuse at the source instead.
        ReplyPoolGenerationZero,

        /// An endpoint ordinal outside `1..=endpoint_capacity`.
        EndpointOutOfRange { found: u32, capacity: u32 },
        /// An endpoint was reached before INIT reserved its fixed host ring.
        /// K11 reserves every admitted nonzero ring before publishing capacity.
        EndpointRingUnassigned { endpoint_id: u32 },
        /// The endpoint's host-dispatch FIFO has nothing to retire — an
        /// accounting bug, refused rather than wrapped.
        HostDispatchFifoUnderflow { endpoint_id: u32 },
        /// The endpoint's arrival-order serial space is exhausted.
        HostDispatchSerialExhausted { endpoint_id: u32 },
        /// A snapshot release named more bytes than the session holds live.
        SnapshotAccountingUnderflow,
        /// The endpoint this attach names was already declared with a different
        /// descriptor. The ordinals are cross-checks, so they are compared and
        /// refused — never used to look an endpoint up.
        EndpointDescriptorConflict { endpoint_id: u32 },
        /// The session's live outer-context count would overflow. Bounded by the
        /// same number as one endpoint's host-dispatch FIFO.
        AttachedContextOverflow,
        /// A detach named a context generation this session never admitted.
        DetachUnknownContext { context_generation: u64 },

        /// A control Render arrived on something other than the session's one
        /// HVC1 control context.
        ControlRenderNotOnControlContext,
        /// A control Render with `HAS_REPLY` did not list exactly the one
        /// allocation this carrier may name.
        ControlRenderAllocationCountNotOne { found: u32 },
        /// A control Render without a reply listed an allocation. The reply slot
        /// is the only entry this carrier ever lists (section 10.4, lines
        /// 1337-1345).
        ControlRenderAllocationWithoutReply { found: u32 },
        /// The listed allocation is not this session's own reply pool. Ring zero
        /// never receives or resolves an outer allocation.
        ControlRenderForeignAllocation,
        /// The listed allocation is the pool but its generation is stale.
        ControlRenderPoolGenerationStale { found: u64, expected: u64 },
        /// The use record's access word is not exactly `WRITE`.
        ControlRenderAccessNotWrite { found: u32 },
        /// The reply slot generation did not strictly increase for the slot the
        /// reply offset selects.
        ControlRenderSlotGenerationStale { found: u64, watermark: u64 },
        /// The slot the reply offset selects already has a different generation
        /// in flight. One checkout, one Render.
        ControlRenderSlotBusy { in_flight: u64 },
        /// A publish/retire named a slot generation that is not the one in
        /// flight on that slot.
        ControlRenderSlotGenerationUnknown { found: u64 },
        /// A slot index derived from a reply offset fell outside the pool.
        /// Unreachable while `protocol`'s HNR2 validator enforces
        /// `ReplySlotIndexOutOfRange` first; present so this module has no
        /// panicking index.
        ControlRenderSlotIndexOutOfRange { found: usize },
    }

    // ── the bounded per-process session list (section 17.6, line 4336) ────────

    /// `DxgkDdiCreateProcess`'s bounded session list, as a count.
    ///
    /// The list itself is `kmd_render`'s (it holds pointers); the *admission* is
    /// here, because "session-capacity exhaustion fails device creation or removes
    /// that device" is a rule and a `const` is not.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct ProcessSessionLedger {
        live: u32,
    }

    impl ProcessSessionLedger {
        pub const fn new() -> Self {
            Self { live: 0 }
        }

        pub const fn live(&self) -> u32 {
            self.live
        }

        /// Admit one more session, or refuse. Never waits and never spills.
        pub fn admit(&mut self) -> Result<(), SessionRefusal> {
            admit_new_session(self.live).map_err(SessionRefusal::Capacity)?;
            self.live += 1;
            Ok(())
        }

        pub fn release(&mut self) -> Result<(), SessionRefusal> {
            match self.live.checked_sub(1) {
                Some(next) => {
                    self.live = next;
                    Ok(())
                }
                None => Err(SessionRefusal::SessionLedgerUnderflow),
            }
        }
    }

    // ── the session-generation source ─────────────────────────────────────────

    /// One monotone nonzero session generation per adapter.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SessionGenerationSource {
        next: u64,
    }

    impl Default for SessionGenerationSource {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SessionGenerationSource {
        pub const fn new() -> Self {
            Self { next: 1 }
        }

        pub fn mint(&mut self) -> Result<u64, SessionRefusal> {
            let value = self.next;
            if value == 0 {
                return Err(SessionRefusal::SessionGenerationSpaceExhausted);
            }
            self.next = match value.checked_add(1) {
                Some(next) => next,
                None => 0,
            };
            Ok(value)
        }
    }

    // ── the session phase ─────────────────────────────────────────────────────

    /// Where a session is in its life. The HVC1 control context creates it
    /// `Provisional`; the finite INIT makes it `Live`; reset, removal, or a hard
    /// refusal makes it `Draining`, which is terminal — section 14 (line 3430)
    /// names that transition ("marks every HTS1 session `Draining`, invalidates
    /// all HQA1 capabilities"), and destruction waits for references to drain.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SessionPhase {
        Provisional,
        Live,
        Draining,
    }

    /// One physical lower-queue endpoint of a session.
    ///
    /// The ring index is the KMD's — minted at INIT, unique, nonzero, never
    /// recycled (invariant 12). The descriptor is the guest's, because only the
    /// guest knows its own Vulkan queue topology; see
    /// [`TranslationSession::declare_endpoint`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SessionEndpoint {
        /// `endpoint_id == 0` means "never declared".
        pub descriptor: HeliosTranslationEndpointV1,
        /// Unique nonzero host `INFO_RING_IDX`. Zero before INIT assigns it.
        pub ring_index: u32,
        /// Already-eligible DMAs queued on this endpoint's host-dispatch FIFO.
        pub fifo_depth: u32,
        /// Next arrival-order host-dispatch serial.
        pub next_serial: u64,
    }

    impl SessionEndpoint {
        const UNASSIGNED: Self = Self {
            descriptor: HeliosTranslationEndpointV1 {
                endpoint_id: 0,
                engine_class: 0,
                queue_family: 0,
                queue_index: 0,
            },
            ring_index: 0,
            fifo_depth: 0,
            next_serial: 1,
        };
    }

    // ── the reply pool and its four slots ─────────────────────────────────────

    /// What one reply slot is doing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SlotState {
        /// Nothing in flight. `retired_generation` still bounds what may be
        /// checked out next.
        Idle,
        /// A Render naming this slot has been admitted and its C51 value has not
        /// completed.
        InFlight,
        /// The host published; the CPU has not consumed it yet. Section 10.7:
        /// "A slot is not reusable until its matching C51 value completed, the CPU
        /// copied or decoded the reply, and the slot generation was retired."
        Published,
    }

    /// One slot of the role-1 pool, as the KMD sees it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ReplySlot {
        pub state: SlotState,
        /// Nonzero while `InFlight`/`Published`.
        pub generation: u64,
        /// Highest generation this slot has retired; the strictly-increasing
        /// floor for the next checkout.
        pub retired_generation: u64,
        pub owner_context_generation: u64,
        pub batch_token: u64,
        pub reply_offset: u64,
        pub reply_capacity_bytes: u64,
        pub c51_value: u64,
    }

    impl ReplySlot {
        const IDLE: Self = Self {
            state: SlotState::Idle,
            generation: 0,
            retired_generation: 0,
            owner_context_generation: 0,
            batch_token: 0,
            reply_offset: 0,
            reply_capacity_bytes: 0,
            c51_value: 0,
        };
    }

    /// The session's one role-1 HVM1 pool, as an identity plus its slot states.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ReplyPool {
        /// The HVM1 `object_generation` the KMD wrote back at create. Every HNR2
        /// use record naming the pool must repeat it.
        pub allocation_generation: u64,
        pub slots: [ReplySlot; REPLY_SLOTS],
    }

    /// One control Render's session-level facts, as `DxgkDdiRender` resolved
    /// them. Everything `protocol`'s HNR2 header validator already enforces
    /// (shape, ordering, reply alignment/range/capacity) is assumed done: this is
    /// only what needs the *session*.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ControlRenderRequest {
        /// Is the submitting context this session's HVC1 control context?
        pub on_control_context: bool,
        /// HNR2 `flags & HAS_REPLY`.
        pub has_reply: bool,
        /// The COMMIT `AllocationCount`.
        pub allocation_count: u32,
        /// Did the caller resolve the one listed allocation to this session's own
        /// reply pool? Resolved from the kernel allocation object, never from the
        /// wire.
        pub names_reply_pool: bool,
        /// The use record's `expected_allocation_generation` for that entry.
        pub expected_allocation_generation: u64,
        /// The use record's access word.
        pub access_flags: u32,
        /// HNR2 `reply_offset`.
        pub reply_offset: u64,
        /// HNR2 `reply_capacity_bytes`.
        pub reply_capacity_bytes: u64,
        /// HNR2 `reply_slot_generation`.
        pub reply_slot_generation: u64,
        /// HNR2 `batch_token`.
        pub batch_token: u64,
        /// The submitting context's HQA1 generation, or 0 for the raw HVC1
        /// control context (which has none — it predates every attach).
        pub owner_context_generation: u64,
    }

    /// A control Render admitted: which slot it took, if any.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ControlRenderAdmission {
        /// `None` for a reply-less control Render.
        pub slot_index: Option<usize>,
        pub slot_generation: u64,
        /// Exact range this admitted slot owns. Re-exported from the model so
        /// the K11 producer never re-reads or re-derives wire offsets after
        /// admission.
        pub reply_offset: u64,
        pub reply_capacity_bytes: u64,
        pub batch_token: u64,
    }

    /// One nonzero-endpoint executor reply checkout.  The platform half has
    /// already proved that the reply use names this session's exact role-1
    /// allocation and that the complete outer allocation closure is present;
    /// this model owns only the four-slot generation/lifetime transition.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ExecutionReplyRequest {
        pub names_reply_pool: bool,
        pub expected_allocation_generation: u64,
        pub access_flags: u32,
        pub reply_offset: u64,
        pub reply_capacity_bytes: u64,
        pub reply_slot_generation: u64,
        pub batch_token: u64,
        pub owner_context_generation: u64,
    }

    // ── the session ───────────────────────────────────────────────────────────

    /// The KMD-side HTS1 session: everything `DxgkDdiCreateContext`,
    /// `DxgkDdiCreateAllocation` and the control `DxgkDdiRender` compare against.
    #[derive(Debug, Clone, Copy)]
    pub struct TranslationSession {
        phase: SessionPhase,
        package_generation: u64,
        capset: u32,
        session_generation: u64,
        capability: HeliosSessionCapability,
        endpoint_capacity: u32,
        endpoints: [SessionEndpoint; ENDPOINT_SLOTS],
        highest_context_generation: u64,
        attached_contexts: u32,
        pool: Option<ReplyPool>,
        live_snapshots: u32,
        live_snapshot_bytes: u64,
    }

    impl TranslationSession {
        /// The provisional session the HVC1 control context creates. It has no
        /// generation and no capability yet: both come from INIT, and every
        /// admission that needs them refuses until then.
        pub const fn new_provisional(package_generation: u64, capset: u32) -> Self {
            Self {
                phase: SessionPhase::Provisional,
                package_generation,
                capset,
                session_generation: 0,
                capability: HeliosSessionCapability::INVALID,
                endpoint_capacity: 0,
                endpoints: [SessionEndpoint::UNASSIGNED; ENDPOINT_SLOTS],
                highest_context_generation: 0,
                attached_contexts: 0,
                pool: None,
                live_snapshots: 0,
                live_snapshot_bytes: 0,
            }
        }

        pub const fn phase(&self) -> SessionPhase {
            self.phase
        }

        pub const fn session_generation(&self) -> u64 {
            self.session_generation
        }

        pub const fn endpoint_capacity(&self) -> u32 {
            self.endpoint_capacity
        }

        pub const fn attached_contexts(&self) -> u32 {
            self.attached_contexts
        }

        pub const fn highest_context_generation(&self) -> u64 {
            self.highest_context_generation
        }

        pub const fn pool(&self) -> Option<ReplyPool> {
            self.pool
        }

        /// Does `presented` equal this session's live capability?
        ///
        /// For the ONE caller that needs it as a lookup key rather than as a
        /// validation (§10.4:1215-1217). A `Draining` session's capability is the
        /// invalidated sentinel, which `check_match` refuses, so this is `false`
        /// for it without a separate phase test.
        pub fn capability_matches(&self, presented: HeliosSessionCapability) -> bool {
            presented.check_match(self.capability).is_ok()
        }

        /// Reset, removal, or teardown. Terminal: the capability is invalidated
        /// first so a waiter woken by the caller can never be admitted afterwards
        /// (section 13, "invalidates all HQA1 capabilities … and cancels C51/HQC1
        /// event waiters as device-lost"), and no slot is marked complete —
        /// "Reset poisons all slots and wakes device-lost; it never marks a reply
        /// complete."
        ///
        /// ⚠ It does only the first half of §14:3430. The caller still owes the
        /// device-lost wakeup, which is why no refusal arm in this module calls it:
        /// a silently-draining session with unwoken waiters is a hang.
        pub fn begin_draining(&mut self) {
            self.capability = HeliosSessionCapability::INVALID;
            self.phase = SessionPhase::Draining;
        }

        /// Bind the one role-1 HVM1 pool the raw device created before INIT.
        ///
        /// `role`/`byte_size`/`object_generation` come from the HVM1 record the
        /// allocation path already validated; they are re-checked here because
        /// this is where the slot arithmetic starts depending on them.
        pub fn bind_reply_pool(
            &mut self,
            role: Hvm1Role,
            byte_size: u64,
            object_generation: u64,
        ) -> Result<(), SessionRefusal> {
            if self.phase == SessionPhase::Draining {
                return Err(SessionRefusal::SessionDraining);
            }
            if self.pool.is_some() {
                return Err(SessionRefusal::ReplyPoolAlreadyBound);
            }
            if role != Hvm1Role::ReplyPool {
                return Err(SessionRefusal::ReplyPoolRoleMismatch {
                    found: role.to_u32(),
                });
            }
            if byte_size != HELIOS_HVM1_REPLY_POOL_BYTES {
                return Err(SessionRefusal::ReplyPoolSizeMismatch { found: byte_size });
            }
            if object_generation == 0 {
                return Err(SessionRefusal::ReplyPoolGenerationZero);
            }
            self.pool = Some(ReplyPool {
                allocation_generation: object_generation,
                slots: [ReplySlot::IDLE; REPLY_SLOTS],
            });
            Ok(())
        }

        /// Validate a finite HTS1 INIT request and say how many endpoints the
        /// session may grant. Does not mutate: the caller still has to create the
        /// host Venus context, and a session that fails that must not look
        /// initialised.
        pub fn admit_init(
            &self,
            request: &HeliosTranslationSessionInitV1,
        ) -> Result<u32, SessionRefusal> {
            match self.phase {
                SessionPhase::Draining => return Err(SessionRefusal::SessionDraining),
                SessionPhase::Live => return Err(SessionRefusal::SessionAlreadyInitialised),
                SessionPhase::Provisional => {}
            }
            if self.pool.is_none() {
                return Err(SessionRefusal::ReplyPoolNotBound);
            }
            request
                .validate(self.package_generation, self.capset)
                .map_err(SessionRefusal::Init)
        }

        /// Complete INIT: record the minted generation, the CSPRNG capability and
        /// the granted endpoint capacity, and return the exact reply bytes.
        ///
        /// `granted_endpoint_capacity` may be smaller than what
        /// [`Self::admit_init`] returned but never larger; `protocol`'s own reply
        /// validator is run here so the KMD can never emit a reply the ICD will
        /// refuse.
        pub fn complete_init(
            &mut self,
            requested_endpoint_capacity: u32,
            granted_endpoint_capacity: u32,
            session_generation: u64,
            capability: HeliosSessionCapability,
        ) -> Result<HeliosTranslationSessionReplyV1, SessionRefusal> {
            match self.phase {
                SessionPhase::Draining => return Err(SessionRefusal::SessionDraining),
                SessionPhase::Live => return Err(SessionRefusal::SessionAlreadyInitialised),
                SessionPhase::Provisional => {}
            }
            if self.pool.is_none() {
                return Err(SessionRefusal::ReplyPoolNotBound);
            }
            if capability.is_invalid() {
                return Err(SessionRefusal::CapabilityIsInvalidSentinel);
            }
            if session_generation == 0 {
                return Err(SessionRefusal::Init(InitRefusal::Generation {
                    field: GenerationField::Session,
                    reason: GenerationRefusal::Zero,
                }));
            }
            let reply = HeliosTranslationSessionReplyV1::new(
                self.package_generation,
                session_generation,
                capability,
                self.capset,
                granted_endpoint_capacity,
            );
            let admission: HeliosSessionAdmission = reply
                .validate(
                    self.package_generation,
                    self.capset,
                    requested_endpoint_capacity,
                )
                .map_err(SessionRefusal::Init)?;

            // K11 reserves the complete bounded endpoint/ring namespace before
            // publishing a nonzero capacity. Ring zero remains the control
            // timeline; each granted endpoint owns the matching nonzero ring in
            // this private host context. The same numeric indices may appear in
            // another session because that session owns a distinct host context.
            // Descriptors remain undeclared until the exact HQA1 attach supplies
            // the physical queue topology, and K11 still executes no queue work.
            let mut endpoints = [SessionEndpoint::UNASSIGNED; ENDPOINT_SLOTS];
            let mut i = 0usize;
            while i < admission.endpoint_capacity as usize {
                endpoints[i].ring_index = i as u32 + 1;
                i += 1;
            }
            self.session_generation = admission.session_generation;
            self.capability = admission.capability;
            self.endpoint_capacity = admission.endpoint_capacity;
            self.endpoints = endpoints;
            self.phase = SessionPhase::Live;
            Ok(reply)
        }

        /// The endpoint this session holds for `endpoint_id`. Its `descriptor`
        /// carries `endpoint_id == 0` if the ordinal has never been declared.
        pub fn endpoint(&self, endpoint_id: u32) -> Result<SessionEndpoint, SessionRefusal> {
            if endpoint_id == 0 || endpoint_id > self.endpoint_capacity {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity: self.endpoint_capacity,
                });
            }
            match self.endpoints.get((endpoint_id - 1) as usize) {
                Some(endpoint) => Ok(*endpoint),
                None => Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity: self.endpoint_capacity,
                }),
            }
        }

        /// The unique nonzero host ring bound to `endpoint_id`.
        pub fn ring_index(&self, endpoint_id: u32) -> Result<u32, SessionRefusal> {
            let endpoint = self.endpoint(endpoint_id)?;
            if endpoint.ring_index == 0 {
                return Err(SessionRefusal::EndpointRingUnassigned { endpoint_id });
            }
            Ok(endpoint.ring_index)
        }

        /// Assign the next arrival-order host-dispatch serial on `endpoint_id`.
        ///
        /// `DxgkDdiSubmitCommand` calls this at `DISPATCH_LEVEL` under the short
        /// endpoint lock and releases it before any host/GPU wait (line 1255-1258).
        /// Exhaustion is refused, never waited on.
        pub fn enqueue_host_dispatch(&mut self, endpoint_id: u32) -> Result<u64, SessionRefusal> {
            if self.phase != SessionPhase::Live {
                return Err(match self.phase {
                    SessionPhase::Draining => SessionRefusal::SessionDraining,
                    _ => SessionRefusal::SessionProvisional,
                });
            }
            let capacity = self.endpoint_capacity;
            if endpoint_id == 0 || endpoint_id > capacity {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity,
                });
            }
            let Some(endpoint) = self.endpoints.get_mut((endpoint_id - 1) as usize) else {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity,
                });
            };
            if endpoint.ring_index == 0 {
                return Err(SessionRefusal::EndpointRingUnassigned { endpoint_id });
            }
            admit_host_dispatch_enqueue(endpoint.fifo_depth).map_err(SessionRefusal::Capacity)?;
            let Some(next) = endpoint.next_serial.checked_add(1) else {
                return Err(SessionRefusal::HostDispatchSerialExhausted { endpoint_id });
            };
            let serial = endpoint.next_serial;
            endpoint.next_serial = next;
            endpoint.fifo_depth += 1;
            Ok(serial)
        }

        /// One enqueued host-dispatch entry retired.
        pub fn retire_host_dispatch(&mut self, endpoint_id: u32) -> Result<(), SessionRefusal> {
            let capacity = self.endpoint_capacity;
            if endpoint_id == 0 || endpoint_id > capacity {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity,
                });
            }
            let Some(endpoint) = self.endpoints.get_mut((endpoint_id - 1) as usize) else {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity,
                });
            };
            match endpoint.fifo_depth.checked_sub(1) {
                Some(depth) => {
                    endpoint.fifo_depth = depth;
                    Ok(())
                }
                None => Err(SessionRefusal::HostDispatchFifoUnderflow { endpoint_id }),
            }
        }

        /// Admit one more immutable reply snapshot of `bytes` on this session.
        ///
        /// The refusal is the point at which the caller drops its lock and
        /// event-waits for the oldest exact C51 owner (line 2138-2140) — it is not
        /// a resource failure and must never become a retry loop.
        pub fn admit_snapshot_bytes(&mut self, bytes: u64) -> Result<(), SessionRefusal> {
            if self.phase == SessionPhase::Draining {
                return Err(SessionRefusal::SessionDraining);
            }
            admit_snapshot(self.live_snapshots, self.live_snapshot_bytes, bytes)
                .map_err(SessionRefusal::Hnr2Capacity)?;
            self.live_snapshots += 1;
            self.live_snapshot_bytes = self.live_snapshot_bytes.saturating_add(bytes);
            Ok(())
        }

        /// One snapshot consumed or cancelled.
        pub fn release_snapshot_bytes(&mut self, bytes: u64) -> Result<(), SessionRefusal> {
            let Some(live) = self.live_snapshots.checked_sub(1) else {
                return Err(SessionRefusal::SnapshotAccountingUnderflow);
            };
            let Some(live_bytes) = self.live_snapshot_bytes.checked_sub(bytes) else {
                return Err(SessionRefusal::SnapshotAccountingUnderflow);
            };
            self.live_snapshots = live;
            self.live_snapshot_bytes = live_bytes;
            Ok(())
        }

        pub const fn live_snapshots(&self) -> u32 {
            self.live_snapshots
        }

        pub const fn live_snapshot_bytes(&self) -> u64 {
            self.live_snapshot_bytes
        }

        /// The largest snapshot one session may hold, re-exported so a caller
        /// never re-derives the cap.
        pub const MAX_SNAPSHOT_BYTES: u64 = HELIOS_HVR1_MAX_SNAPSHOT_BYTES;

        /// Declare one physical endpoint, or re-declare it identically.
        ///
        /// ⚠ This is the KMD's only source of endpoint descriptors today. A
        /// record-only translator creates no HVC1 queue context (section 10.7,
        /// lines 1752-1755), and neither the INIT request nor its reply carries an
        /// endpoint array, so nothing on the wire tells the KMD a session's queue
        /// topology before the first HQA1 arrives. The first attach on an ordinal
        /// therefore *declares* it and every later one is compared against that
        /// record; a conflict is refused. See the cross-lane request in
        /// `ROADMAP.md` — if the descriptors are to be KMD-authored, INIT needs a
        /// field it does not have.
        pub fn declare_endpoint(
            &mut self,
            endpoint: HeliosTranslationEndpointV1,
        ) -> Result<(), SessionRefusal> {
            let existing = self.endpoint(endpoint.endpoint_id)?;
            if existing.descriptor.endpoint_id != 0 {
                if existing.descriptor != endpoint {
                    return Err(SessionRefusal::EndpointDescriptorConflict {
                        endpoint_id: endpoint.endpoint_id,
                    });
                }
                return Ok(());
            }
            let capacity = self.endpoint_capacity;
            match self.endpoints.get_mut((endpoint.endpoint_id - 1) as usize) {
                Some(slot) => slot.descriptor = endpoint,
                None => {
                    return Err(SessionRefusal::EndpointOutOfRange {
                        found: endpoint.endpoint_id,
                        capacity,
                    })
                }
            }
            Ok(())
        }

        /// The one-time HQA1 attach `DxgkDdiCreateContext` performs.
        ///
        /// Mutates only on success: the context-generation watermark advances and
        /// the endpoint is declared, so a refused packet leaves nothing behind for
        /// the next one to trip over.
        pub fn attach(
            &mut self,
            packet: &HeliosQueueAttachV1,
        ) -> Result<HeliosAttachAdmission, SessionRefusal> {
            match self.phase {
                SessionPhase::Draining => {
                    return Err(SessionRefusal::Attach(AttachRefusal::Capability(
                        CapabilityRefusal::SessionInvalidated,
                    )))
                }
                SessionPhase::Provisional => return Err(SessionRefusal::SessionProvisional),
                SessionPhase::Live => {}
            }
            if self.attached_contexts == u32::MAX {
                return Err(SessionRefusal::AttachedContextOverflow);
            }

            // The endpoint the packet claims, as this session records it — or, on
            // the first attach to that ordinal, the packet's own descriptor. Shape
            // is validated either way by `HeliosQueueAttachV1::validate`, which
            // runs `HeliosTranslationEndpointV1::validate` on whatever is here.
            let recorded = self.endpoint(packet.endpoint_id)?;
            let expect_endpoint = if recorded.descriptor.endpoint_id == 0 {
                HeliosTranslationEndpointV1 {
                    endpoint_id: packet.endpoint_id,
                    engine_class: packet.engine_class,
                    queue_family: packet.queue_family,
                    queue_index: packet.queue_index,
                }
            } else {
                recorded.descriptor
            };

            let expect = HeliosAttachExpectation {
                package_generation: self.package_generation,
                session_generation: self.session_generation,
                capability: self.capability,
                endpoint: expect_endpoint,
                endpoint_capacity: self.endpoint_capacity,
                highest_context_generation: self.highest_context_generation,
            };
            let admission = packet.validate(&expect).map_err(SessionRefusal::Attach)?;

            self.declare_endpoint(expect_endpoint)?;
            self.highest_context_generation = admission.context_generation;
            self.attached_contexts += 1;
            Ok(admission)
        }

        /// One attached outer context went away. The watermark deliberately does
        /// not move: a generation is "never reused within this HTS1 session", so a
        /// dead context's number stays spent.
        pub fn detach(&mut self, context_generation: u64) -> Result<(), SessionRefusal> {
            if context_generation == 0 || context_generation > self.highest_context_generation {
                return Err(SessionRefusal::DetachUnknownContext { context_generation });
            }
            match self.attached_contexts.checked_sub(1) {
                Some(next) => {
                    self.attached_contexts = next;
                    Ok(())
                }
                None => Err(SessionRefusal::DetachUnknownContext { context_generation }),
            }
        }

        /// Which slot a reply offset selects.
        fn slot_index(reply_offset: u64) -> Result<usize, SessionRefusal> {
            let index = (reply_offset / HELIOS_HVM1_REPLY_SLOT_BYTES) as usize;
            if index >= REPLY_SLOTS {
                return Err(SessionRefusal::ControlRenderSlotIndexOutOfRange { found: index });
            }
            Ok(index)
        }

        /// The session-level admission of one control-context `DxgkDdiRender`.
        ///
        /// Runs on a `Provisional` session too: the INIT that makes a session live
        /// is itself a control Render.
        pub fn admit_control_render(
            &mut self,
            request: &ControlRenderRequest,
        ) -> Result<ControlRenderAdmission, SessionRefusal> {
            if self.phase == SessionPhase::Draining {
                return Err(SessionRefusal::SessionDraining);
            }
            if !request.on_control_context {
                return Err(SessionRefusal::ControlRenderNotOnControlContext);
            }
            if !request.has_reply {
                if request.allocation_count != 0 {
                    return Err(SessionRefusal::ControlRenderAllocationWithoutReply {
                        found: request.allocation_count,
                    });
                }
                return Ok(ControlRenderAdmission {
                    slot_index: None,
                    slot_generation: 0,
                    reply_offset: 0,
                    reply_capacity_bytes: 0,
                    batch_token: request.batch_token,
                });
            }

            let Some(pool) = self.pool else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            if request.allocation_count != 1 {
                return Err(SessionRefusal::ControlRenderAllocationCountNotOne {
                    found: request.allocation_count,
                });
            }
            if !request.names_reply_pool {
                return Err(SessionRefusal::ControlRenderForeignAllocation);
            }
            if request.expected_allocation_generation != pool.allocation_generation {
                return Err(SessionRefusal::ControlRenderPoolGenerationStale {
                    found: request.expected_allocation_generation,
                    expected: pool.allocation_generation,
                });
            }
            // Exactly WRITE, not "WRITE among others": READ on the reply slot
            // would be the control carrier reading a buffer the host is still
            // writing, and an unknown bit is `protocol`'s own hard reject.
            if request.access_flags != HELIOS_HNR2_ACCESS_WRITE {
                return Err(SessionRefusal::ControlRenderAccessNotWrite {
                    found: request.access_flags,
                });
            }

            self.checkout_reply_slot(
                request.reply_offset,
                request.reply_capacity_bytes,
                request.reply_slot_generation,
                request.batch_token,
                request.owner_context_generation,
            )
        }

        /// Admit the exact role-1 reply range of one already-classified
        /// nonzero-ring allocation command.  Unlike the control carrier, this
        /// operation is legal only after INIT and does not require the complete
        /// allocation list to contain only the reply pool.
        pub fn admit_execution_reply(
            &mut self,
            request: &ExecutionReplyRequest,
        ) -> Result<ControlRenderAdmission, SessionRefusal> {
            match self.phase {
                SessionPhase::Provisional => return Err(SessionRefusal::SessionProvisional),
                SessionPhase::Draining => return Err(SessionRefusal::SessionDraining),
                SessionPhase::Live => {}
            }
            let Some(pool) = self.pool else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            if !request.names_reply_pool {
                return Err(SessionRefusal::ControlRenderForeignAllocation);
            }
            if request.expected_allocation_generation != pool.allocation_generation {
                return Err(SessionRefusal::ControlRenderPoolGenerationStale {
                    found: request.expected_allocation_generation,
                    expected: pool.allocation_generation,
                });
            }
            if request.access_flags != HELIOS_HNR2_ACCESS_WRITE {
                return Err(SessionRefusal::ControlRenderAccessNotWrite {
                    found: request.access_flags,
                });
            }
            if request.owner_context_generation == 0 {
                return Err(SessionRefusal::ControlRenderSlotGenerationUnknown { found: 0 });
            }
            self.checkout_reply_slot(
                request.reply_offset,
                request.reply_capacity_bytes,
                request.reply_slot_generation,
                request.batch_token,
                request.owner_context_generation,
            )
        }

        fn checkout_reply_slot(
            &mut self,
            reply_offset: u64,
            reply_capacity_bytes: u64,
            reply_slot_generation: u64,
            batch_token: u64,
            owner_context_generation: u64,
        ) -> Result<ControlRenderAdmission, SessionRefusal> {
            let index = Self::slot_index(reply_offset)?;
            let Some(pool) = self.pool else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            let slot = pool.slots[index];
            if slot.state != SlotState::Idle {
                return Err(SessionRefusal::ControlRenderSlotBusy {
                    in_flight: slot.generation,
                });
            }
            if reply_slot_generation <= slot.retired_generation {
                return Err(SessionRefusal::ControlRenderSlotGenerationStale {
                    found: reply_slot_generation,
                    watermark: slot.retired_generation,
                });
            }

            let Some(pool_mut) = self.pool.as_mut() else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            pool_mut.slots[index] = ReplySlot {
                state: SlotState::InFlight,
                generation: reply_slot_generation,
                retired_generation: slot.retired_generation,
                owner_context_generation,
                batch_token,
                reply_offset,
                reply_capacity_bytes,
                c51_value: 0,
            };
            Ok(ControlRenderAdmission {
                slot_index: Some(index),
                slot_generation: reply_slot_generation,
                reply_offset,
                reply_capacity_bytes,
                batch_token,
            })
        }

        /// The generation of the role-1 reply-pool allocation bound to this
        /// session, or `None` when nothing is bound.
        ///
        /// K6's control Render needs it to decide `names_reply_pool` — "is the
        /// one listed allocation THIS session's pool" — from the kernel object
        /// it resolved, which is a different question from the wire's
        /// `expected_allocation_generation` that [`Self::admit_control_render`]
        /// checks.
        pub fn reply_pool_generation(&self) -> Option<u64> {
            self.pool.map(|pool| pool.allocation_generation)
        }

        /// The host published this slot's reply and its C51 value is known.
        pub fn publish_slot(
            &mut self,
            slot_index: usize,
            slot_generation: u64,
            c51_value: u64,
        ) -> Result<(), SessionRefusal> {
            let slot = self.slot_in_flight(slot_index, slot_generation)?;
            slot.state = SlotState::Published;
            slot.c51_value = c51_value;
            Ok(())
        }

        /// The CPU consumed the reply and the slot generation retires. Only now
        /// is the slot reusable.
        pub fn retire_slot(
            &mut self,
            slot_index: usize,
            slot_generation: u64,
        ) -> Result<(), SessionRefusal> {
            let slot = self.slot_in_flight(slot_index, slot_generation)?;
            if slot.state != SlotState::Published {
                return Err(SessionRefusal::ControlRenderSlotBusy {
                    in_flight: slot.generation,
                });
            }
            *slot = ReplySlot {
                retired_generation: slot_generation,
                ..ReplySlot::IDLE
            };
            Ok(())
        }

        /// Cancel an admitted reply without ever publishing a completion.
        ///
        /// Unlike [`Self::publish_slot`] and [`Self::retire_slot`], this release
        /// remains legal after [`Self::begin_draining`]: a failed INIT first
        /// closes admission, then must give back the exact slot it already owns
        /// before the platform destroys the host namespace.  The generation is
        /// still retired so a non-terminal refusal cannot replay it.
        pub fn abort_slot(
            &mut self,
            slot_index: usize,
            slot_generation: u64,
        ) -> Result<(), SessionRefusal> {
            let Some(pool) = self.pool.as_mut() else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            let Some(slot) = pool.slots.get_mut(slot_index) else {
                return Err(SessionRefusal::ControlRenderSlotIndexOutOfRange { found: slot_index });
            };
            if slot.state != SlotState::InFlight || slot.generation != slot_generation {
                return Err(SessionRefusal::ControlRenderSlotGenerationUnknown {
                    found: slot_generation,
                });
            }
            *slot = ReplySlot {
                retired_generation: slot_generation,
                ..ReplySlot::IDLE
            };
            Ok(())
        }

        fn slot_in_flight(
            &mut self,
            slot_index: usize,
            slot_generation: u64,
        ) -> Result<&mut ReplySlot, SessionRefusal> {
            if self.phase == SessionPhase::Draining {
                return Err(SessionRefusal::SessionDraining);
            }
            let Some(pool) = self.pool.as_mut() else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            let Some(slot) = pool.slots.get_mut(slot_index) else {
                return Err(SessionRefusal::ControlRenderSlotIndexOutOfRange { found: slot_index });
            };
            if slot.state == SlotState::Idle || slot.generation != slot_generation {
                return Err(SessionRefusal::ControlRenderSlotGenerationUnknown {
                    found: slot_generation,
                });
            }
            Ok(slot)
        }
    }

    /// The engine class an endpoint ordinal is admitted with, decoded once.
    /// A convenience for `kmd_render`'s counter naming; it re-decodes nothing the
    /// attach path did not already validate.
    pub fn engine_class_of(
        endpoint: &HeliosTranslationEndpointV1,
        endpoint_capacity: u32,
    ) -> Result<HeliosEngineClass, SessionRefusal> {
        endpoint
            .validate(endpoint_capacity)
            .map_err(|reason| SessionRefusal::Attach(AttachRefusal::Endpoint(reason)))
    }

    /// The capacity limit a [`HeliosCapacityRefusal`] names, for a counter label.
    pub const fn capacity_limit_code(limit: HeliosCapacityLimit) -> u32 {
        match limit {
            HeliosCapacityLimit::SessionsPerProcess => 1,
            HeliosCapacityLimit::RingIndex => 2,
            HeliosCapacityLimit::OutstandingContextBatches => 3,
            HeliosCapacityLimit::ContextBatchBytes => 4,
            HeliosCapacityLimit::HostDispatchFifoDepth => 5,
        }
    }

    /// Package/session generation cross-check for a later record (HOB1/HOS1)
    /// that repeats the session generation as an anti-stale check.
    pub fn check_session_generation(
        found: u64,
        session: &TranslationSession,
    ) -> Result<(), SessionRefusal> {
        check_generation_match(found, session.session_generation).map_err(|reason| {
            SessionRefusal::Init(InitRefusal::Generation {
                field: GenerationField::Session,
                reason,
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use helios_protocol::native_render::{
            HELIOS_HNR2_ACCESS_READ, HELIOS_HVM1_REPLY_SLOT_BYTES,
        };
        use helios_protocol::translation_session::{
            EndpointRefusal, HeliosOuterContextKind, HELIOS_ENGINE_CLASS_COMPUTE,
            HELIOS_ENGINE_CLASS_GRAPHICS, HELIOS_HQA1_FLAGS_MASK, HELIOS_HQA1_MAGIC,
            HELIOS_HQA1_SIZE, HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH,
            HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS,
        };

        const PKG: u64 = 0x2026_0810_0000_0001;
        const CAPSET: u32 = helios_protocol::virtio_gpu::VIRTIO_GPU_CAPSET_VENUS;
        const POOL_GEN: u64 = 0x5EED_0001;
        const CAP: HeliosSessionCapability = HeliosSessionCapability {
            low: 0x0123_4567_89AB_CDEF,
            high: 0xFEDC_BA98_7654_3210,
        };

        fn endpoint(id: u32) -> HeliosTranslationEndpointV1 {
            HeliosTranslationEndpointV1::new(id, HeliosEngineClass::Graphics, 0, 0)
        }

        /// A session taken all the way to `Live` with a bound pool.
        fn live_session(endpoint_capacity: u32) -> TranslationSession {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .expect("pool binds");
            let request = HeliosTranslationSessionInitV1::new(PKG, CAPSET, endpoint_capacity);
            let requested = s.admit_init(&request).expect("INIT admitted");
            s.complete_init(requested, endpoint_capacity, 7, CAP)
                .expect("INIT completes");
            s
        }

        fn attach_packet(endpoint_id: u32, context_generation: u64) -> HeliosQueueAttachV1 {
            HeliosQueueAttachV1::new(
                PKG,
                7,
                CAP,
                endpoint(endpoint_id),
                context_generation,
                HeliosOuterContextKind::D3d11PhysicalRender,
            )
        }

        fn reply_request(slot: usize, slot_generation: u64) -> ControlRenderRequest {
            ControlRenderRequest {
                on_control_context: true,
                has_reply: true,
                allocation_count: 1,
                names_reply_pool: true,
                expected_allocation_generation: POOL_GEN,
                access_flags: HELIOS_HNR2_ACCESS_WRITE,
                reply_offset: slot as u64 * HELIOS_HVM1_REPLY_SLOT_BYTES,
                reply_capacity_bytes: 80 + 4096,
                reply_slot_generation: slot_generation,
                batch_token: 1,
                owner_context_generation: 0,
            }
        }

        fn execution_reply_request(slot: usize, slot_generation: u64) -> ExecutionReplyRequest {
            ExecutionReplyRequest {
                names_reply_pool: true,
                expected_allocation_generation: POOL_GEN,
                access_flags: HELIOS_HNR2_ACCESS_WRITE,
                reply_offset: slot as u64 * HELIOS_HVM1_REPLY_SLOT_BYTES,
                reply_capacity_bytes: 80 + 24,
                reply_slot_generation: slot_generation,
                batch_token: 9,
                owner_context_generation: 0xA11,
            }
        }

        // ── the per-process session ledger ────────────────────────────────

        #[test]
        fn the_session_ledger_admits_exactly_the_protocol_cap() {
            let mut ledger = ProcessSessionLedger::new();
            for _ in 0..HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS {
                ledger.admit().expect("under the cap");
            }
            assert_eq!(ledger.live(), HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS);
            assert!(matches!(
                ledger.admit(),
                Err(SessionRefusal::Capacity(HeliosCapacityRefusal {
                    limit: HeliosCapacityLimit::SessionsPerProcess,
                    ..
                }))
            ));
            // Refusing must not have consumed a slot, or the cap would ratchet
            // down every time a process hit it.
            assert_eq!(ledger.live(), HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS);
            ledger.release().expect("one goes away");
            ledger.admit().expect("and the slot is reusable");
        }

        #[test]
        fn releasing_a_session_that_was_never_admitted_is_a_refusal_not_a_wrap() {
            let mut ledger = ProcessSessionLedger::new();
            assert_eq!(
                ledger.release(),
                Err(SessionRefusal::SessionLedgerUnderflow)
            );
            assert_eq!(ledger.live(), 0);
        }

        // ── the session-generation source ─────────────────────────────────

        #[test]
        fn session_generations_are_nonzero_and_strictly_increasing() {
            let mut src = SessionGenerationSource::new();
            let a = src.mint().expect("first");
            let b = src.mint().expect("second");
            assert_ne!(a, 0);
            assert!(b > a);
        }

        #[test]
        fn session_generation_exhaustion_refuses_rather_than_saturating() {
            let mut src = SessionGenerationSource::new();
            // Drive the counter to the wrap sentinel the way `mint` sets it.
            for _ in 0..2 {
                src.mint().expect("warm up");
            }
            src = SessionGenerationSource { next: u64::MAX };
            assert_eq!(src.mint(), Ok(u64::MAX));
            assert_eq!(
                src.mint(),
                Err(SessionRefusal::SessionGenerationSpaceExhausted)
            );
            // And it stays refused: a second caller must not get 0 either.
            assert_eq!(
                src.mint(),
                Err(SessionRefusal::SessionGenerationSpaceExhausted)
            );
        }

        // ── the reply pool ────────────────────────────────────────────────

        #[test]
        fn the_reply_pool_binds_once_and_only_as_role_one() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            assert_eq!(
                s.bind_reply_pool(
                    Hvm1Role::VulkanHostVisible,
                    HELIOS_HVM1_REPLY_POOL_BYTES,
                    POOL_GEN
                ),
                Err(SessionRefusal::ReplyPoolRoleMismatch {
                    found: Hvm1Role::VulkanHostVisible.to_u32()
                })
            );
            assert_eq!(
                s.bind_reply_pool(
                    Hvm1Role::ReplyPool,
                    HELIOS_HVM1_REPLY_POOL_BYTES / 2,
                    POOL_GEN
                ),
                Err(SessionRefusal::ReplyPoolSizeMismatch {
                    found: HELIOS_HVM1_REPLY_POOL_BYTES / 2
                })
            );
            assert_eq!(
                s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, 0),
                Err(SessionRefusal::ReplyPoolGenerationZero)
            );
            // None of the three refusals may have left a pool behind.
            assert!(s.pool().is_none());

            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .expect("binds");
            assert_eq!(
                s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN),
                Err(SessionRefusal::ReplyPoolAlreadyBound)
            );
        }

        // ── INIT ──────────────────────────────────────────────────────────

        #[test]
        fn init_refuses_before_the_pool_is_bound() {
            let s = TranslationSession::new_provisional(PKG, CAPSET);
            let request = HeliosTranslationSessionInitV1::new(PKG, CAPSET, 4);
            assert_eq!(
                s.admit_init(&request),
                Err(SessionRefusal::ReplyPoolNotBound)
            );
        }

        #[test]
        fn init_refuses_a_foreign_package_or_capset() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            assert!(matches!(
                s.admit_init(&HeliosTranslationSessionInitV1::new(PKG + 1, CAPSET, 4)),
                Err(SessionRefusal::Init(InitRefusal::Generation {
                    field: GenerationField::Package,
                    ..
                }))
            ));
            assert!(matches!(
                s.admit_init(&HeliosTranslationSessionInitV1::new(PKG, CAPSET + 1, 4)),
                Err(SessionRefusal::Init(InitRefusal::CapsetMismatch { .. }))
            ));
        }

        #[test]
        fn init_runs_exactly_once_per_session() {
            let mut s = live_session(4);
            assert_eq!(s.phase(), SessionPhase::Live);
            let request = HeliosTranslationSessionInitV1::new(PKG, CAPSET, 4);
            assert_eq!(
                s.admit_init(&request),
                Err(SessionRefusal::SessionAlreadyInitialised)
            );
            assert_eq!(
                s.complete_init(4, 4, 9, CAP),
                Err(SessionRefusal::SessionAlreadyInitialised)
            );
            // And the second attempt did not overwrite the first's identity.
            assert_eq!(s.session_generation(), 7);
        }

        #[test]
        fn init_may_grant_fewer_endpoints_than_requested_but_never_more() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            assert!(matches!(
                s.complete_init(4, 5, 7, CAP),
                Err(SessionRefusal::Init(
                    InitRefusal::EndpointCapacityExceedsRequest { .. }
                ))
            ));
            // The refused reply must not have made the session live.
            assert_eq!(s.phase(), SessionPhase::Provisional);
            s.complete_init(4, 2, 7, CAP).expect("fewer is fine");
            assert_eq!(s.endpoint_capacity(), 2);
        }

        #[test]
        fn init_refuses_the_all_zero_capability_and_a_zero_generation() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            assert_eq!(
                s.complete_init(4, 4, 7, HeliosSessionCapability::INVALID),
                Err(SessionRefusal::CapabilityIsInvalidSentinel)
            );
            assert!(matches!(
                s.complete_init(4, 4, 0, CAP),
                Err(SessionRefusal::Init(InitRefusal::Generation {
                    field: GenerationField::Session,
                    reason: GenerationRefusal::Zero,
                }))
            ));
            assert_eq!(s.phase(), SessionPhase::Provisional);
        }

        #[test]
        fn a_capability_with_one_zero_half_is_still_a_live_capability() {
            // A CSPRNG may legitimately produce a zero half; only the all-zero
            // pair is the invalidated sentinel.
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            let half = HeliosSessionCapability::from_halves(0, 0xDEAD_BEEF);
            s.complete_init(4, 4, 7, half).expect("admitted");
            let mut packet = attach_packet(1, 1);
            packet.capability_low = 0;
            packet.capability_high = 0xDEAD_BEEF;
            s.attach(&packet).expect("attaches");
        }

        // ── HQA1 attach ───────────────────────────────────────────────────

        #[test]
        fn attach_refuses_before_init() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            assert_eq!(
                s.attach(&attach_packet(1, 1)),
                Err(SessionRefusal::SessionProvisional)
            );
        }

        #[test]
        fn attach_admits_then_refuses_a_repeated_context_generation() {
            let mut s = live_session(4);
            let admission = s.attach(&attach_packet(1, 10)).expect("first attach");
            assert_eq!(admission.context_generation, 10);
            assert_eq!(admission.endpoint_id, 1);
            assert_eq!(admission.engine_class, HeliosEngineClass::Graphics);
            assert_eq!(admission.kind, HeliosOuterContextKind::D3d11PhysicalRender);
            assert_eq!(s.attached_contexts(), 1);

            for repeat in [10u64, 9, 1] {
                assert!(matches!(
                    s.attach(&attach_packet(1, repeat)),
                    Err(SessionRefusal::Attach(AttachRefusal::Generation {
                        field: GenerationField::Context,
                        reason: GenerationRefusal::NotMonotonic { .. },
                    }))
                ));
            }
            assert_eq!(s.attached_contexts(), 1);
            s.attach(&attach_packet(1, 11)).expect("strictly greater");
            assert_eq!(s.attached_contexts(), 2);
        }

        #[test]
        fn a_dead_contexts_generation_stays_spent() {
            let mut s = live_session(4);
            s.attach(&attach_packet(1, 10)).unwrap();
            s.detach(10).expect("context goes away");
            assert_eq!(s.attached_contexts(), 0);
            assert!(matches!(
                s.attach(&attach_packet(1, 10)),
                Err(SessionRefusal::Attach(AttachRefusal::Generation { .. }))
            ));
            assert_eq!(s.highest_context_generation(), 10);
        }

        #[test]
        fn detach_refuses_a_generation_the_session_never_admitted() {
            let mut s = live_session(4);
            s.attach(&attach_packet(1, 10)).unwrap();
            assert_eq!(
                s.detach(0),
                Err(SessionRefusal::DetachUnknownContext {
                    context_generation: 0
                })
            );
            assert_eq!(
                s.detach(11),
                Err(SessionRefusal::DetachUnknownContext {
                    context_generation: 11
                })
            );
            assert_eq!(s.attached_contexts(), 1);
        }

        #[test]
        fn attach_refuses_a_foreign_capability_a_stale_session_and_a_bad_package() {
            let mut s = live_session(4);
            let mut wrong_cap = attach_packet(1, 10);
            wrong_cap.capability_low ^= 1;
            assert_eq!(
                s.attach(&wrong_cap),
                Err(SessionRefusal::Attach(AttachRefusal::Capability(
                    CapabilityRefusal::Mismatch
                )))
            );

            let mut wrong_session = attach_packet(1, 10);
            wrong_session.session_generation = 8;
            assert!(matches!(
                s.attach(&wrong_session),
                Err(SessionRefusal::Attach(AttachRefusal::Generation {
                    field: GenerationField::Session,
                    ..
                }))
            ));

            let mut wrong_pkg = attach_packet(1, 10);
            wrong_pkg.package_generation = PKG + 1;
            assert!(matches!(
                s.attach(&wrong_pkg),
                Err(SessionRefusal::Attach(AttachRefusal::Generation {
                    field: GenerationField::Package,
                    ..
                }))
            ));

            // Not one of the three may have advanced the watermark.
            assert_eq!(s.highest_context_generation(), 0);
            assert_eq!(s.attached_contexts(), 0);
        }

        #[test]
        fn attach_refuses_a_malformed_packet_shape() {
            let mut s = live_session(4);
            for mutate in [
                (|p: &mut HeliosQueueAttachV1| p.magic ^= 1) as fn(&mut HeliosQueueAttachV1),
                |p| p.abi_version += 1,
                |p| p.struct_size += 1,
                |p| p.reserved = 1,
                |p| p.flags = 0,
                |p| p.flags = HELIOS_HQA1_FLAGS_MASK,
                |p| p.flags = 1 << 5,
            ] {
                let mut packet = attach_packet(1, 10);
                mutate(&mut packet);
                assert!(
                    matches!(s.attach(&packet), Err(SessionRefusal::Attach(_))),
                    "a mutated HQA1 must be refused"
                );
            }
            assert_eq!(s.highest_context_generation(), 0);
        }

        #[test]
        fn an_endpoint_ordinal_outside_the_granted_capacity_is_refused() {
            let mut s = live_session(2);
            assert_eq!(
                s.attach(&attach_packet(0, 10)),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 0,
                    capacity: 2
                })
            );
            assert_eq!(
                s.attach(&attach_packet(3, 10)),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 3,
                    capacity: 2
                })
            );
            s.attach(&attach_packet(2, 10)).expect("the last ordinal");
        }

        #[test]
        fn the_first_attach_declares_an_endpoint_and_later_ones_are_cross_checked() {
            let mut s = live_session(4);
            s.attach(&attach_packet(1, 10))
                .expect("declares endpoint 1");
            assert_eq!(s.endpoint(1).unwrap().descriptor, endpoint(1));

            // Same ordinal, different engine class: refused as a mismatch against
            // what the session recorded, not silently re-declared.
            let mut other_class = attach_packet(1, 11);
            other_class.engine_class = HELIOS_ENGINE_CLASS_COMPUTE;
            assert_eq!(
                s.attach(&other_class),
                Err(SessionRefusal::Attach(AttachRefusal::EngineClassMismatch {
                    found: HELIOS_ENGINE_CLASS_COMPUTE,
                    expected: HELIOS_ENGINE_CLASS_GRAPHICS,
                }))
            );

            // Same ordinal, different diagnostic queue index: also a mismatch.
            let mut other_index = attach_packet(1, 11);
            other_index.queue_index = 3;
            assert_eq!(
                s.attach(&other_index),
                Err(SessionRefusal::Attach(AttachRefusal::QueueIndexMismatch {
                    found: 3,
                    expected: 0,
                }))
            );

            // A different ordinal is free to have its own class.
            let mut second = attach_packet(2, 11);
            second.engine_class = HELIOS_ENGINE_CLASS_COMPUTE;
            second.queue_family = 1;
            s.attach(&second)
                .expect("endpoint 2 is its own declaration");
            assert_eq!(
                s.endpoint(2).unwrap().descriptor.engine_class,
                HELIOS_ENGINE_CLASS_COMPUTE
            );
            // Declaring 2 must not have disturbed 1.
            assert_eq!(s.endpoint(1).unwrap().descriptor, endpoint(1));
        }

        #[test]
        fn an_endpoint_may_never_carry_the_hvc1_control_sentinel() {
            let mut s = live_session(4);
            let mut packet = attach_packet(1, 10);
            packet.queue_family = u32::MAX;
            packet.queue_index = u32::MAX;
            assert_eq!(
                s.attach(&packet),
                Err(SessionRefusal::Attach(AttachRefusal::Endpoint(
                    EndpointRefusal::ControlSentinelEndpoint
                )))
            );
        }

        #[test]
        fn declare_endpoint_is_idempotent_and_refuses_a_conflict() {
            let mut s = live_session(4);
            s.declare_endpoint(endpoint(1)).expect("declares");
            s.declare_endpoint(endpoint(1))
                .expect("re-declares identically");
            let conflicting = HeliosTranslationEndpointV1::new(1, HeliosEngineClass::Copy, 0, 0);
            assert_eq!(
                s.declare_endpoint(conflicting),
                Err(SessionRefusal::EndpointDescriptorConflict { endpoint_id: 1 })
            );
        }

        // ── poisoning ─────────────────────────────────────────────────────

        #[test]
        fn no_refusal_arm_moves_the_session_out_of_its_phase() {
            // The invariant a future edit acting on "a hard refusal poisons the
            // session" would silently break: every refusal returns before its
            // mutation, so the next valid request still works.
            let mut s = live_session(4);
            let mut foreign = reply_request(0, 1);
            foreign.names_reply_pool = false;
            assert!(s.admit_control_render(&foreign).is_err());
            assert_eq!(s.phase(), SessionPhase::Live);

            let mut bad_access = reply_request(0, 1);
            bad_access.access_flags = HELIOS_HNR2_ACCESS_READ;
            assert!(s.admit_control_render(&bad_access).is_err());
            assert_eq!(s.phase(), SessionPhase::Live);

            assert!(s.attach(&attach_packet(1, 0)).is_err());
            assert_eq!(s.phase(), SessionPhase::Live);
            assert_eq!(s.highest_context_generation(), 0);

            // ...and the session is still fully usable afterwards.
            s.admit_control_render(&reply_request(0, 1))
                .expect("a refused predecessor left nothing behind");
            s.attach(&attach_packet(1, 1))
                .expect("still admits a good packet");
        }

        #[test]
        fn the_release_paths_deliberately_do_not_gate_on_draining() {
            // detach / retire_host_dispatch / release_snapshot_bytes must keep
            // working while a session drains: `dxgkddi_destroy_context` calls
            // `detach` for attached contexts AFTER the control context has already
            // begun draining, and dxgkrnl does not order the two.
            let mut s = live_session(4);
            s.attach(&attach_packet(1, 10)).unwrap();
            s.enqueue_host_dispatch(1).unwrap();
            s.admit_snapshot_bytes(64).unwrap();

            s.begin_draining();
            s.detach(10).expect("an attached context still detaches");
            s.retire_host_dispatch(1)
                .expect("an enqueued DMA still retires");
            s.release_snapshot_bytes(64)
                .expect("a live snapshot still releases");
            assert_eq!(s.attached_contexts(), 0);
            assert_eq!(s.live_snapshots(), 0);
        }

        #[test]
        fn draining_invalidates_the_capability_before_anything_else() {
            let mut s = live_session(4);
            s.attach(&attach_packet(1, 10)).unwrap();
            s.begin_draining();
            assert_eq!(s.phase(), SessionPhase::Draining);
            // A packet carrying the exact capability the session issued is now
            // refused as an invalidated session, not as a mismatch.
            assert_eq!(
                s.attach(&attach_packet(1, 11)),
                Err(SessionRefusal::Attach(AttachRefusal::Capability(
                    CapabilityRefusal::SessionInvalidated
                )))
            );
            assert_eq!(
                s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN),
                Err(SessionRefusal::SessionDraining)
            );
            assert_eq!(
                s.admit_control_render(&reply_request(0, 1)),
                Err(SessionRefusal::SessionDraining)
            );
        }

        #[test]
        fn draining_never_marks_a_reply_complete_but_exact_abort_still_releases_ownership() {
            let mut s = live_session(4);
            let admitted = s.admit_control_render(&reply_request(0, 1)).unwrap();
            s.begin_draining();
            let slot = s.pool().unwrap().slots[admitted.slot_index.unwrap()];
            assert_eq!(slot.state, SlotState::InFlight);
            assert_eq!(slot.retired_generation, 0);
            assert_eq!(
                s.publish_slot(0, 1, 5),
                Err(SessionRefusal::SessionDraining)
            );
            s.abort_slot(0, 1)
                .expect("draining cancels the exact in-flight slot");
            let slot = s.pool().unwrap().slots[admitted.slot_index.unwrap()];
            assert_eq!(slot.state, SlotState::Idle);
            assert_eq!(slot.retired_generation, 1);
            assert_eq!(slot.c51_value, 0);
        }

        // ── the control-context Render ────────────────────────────────────

        #[test]
        fn a_nonzero_endpoint_allocation_reply_uses_the_same_exact_slot_lifetime() {
            let mut s = live_session(4);
            let admitted = s
                .admit_execution_reply(&execution_reply_request(2, 7))
                .unwrap();
            assert_eq!(admitted.slot_index, Some(2));
            assert_eq!(admitted.slot_generation, 7);
            assert_eq!(admitted.batch_token, 9);
            assert_eq!(s.pool().unwrap().slots[2].owner_context_generation, 0xA11);

            assert_eq!(
                s.admit_execution_reply(&execution_reply_request(2, 8)),
                Err(SessionRefusal::ControlRenderSlotBusy { in_flight: 7 })
            );
            s.publish_slot(2, 7, 0).unwrap();
            s.retire_slot(2, 7).unwrap();
            s.admit_execution_reply(&execution_reply_request(2, 8))
                .expect("a later executor reply reuses the retired slot");
        }

        #[test]
        fn an_execution_reply_requires_the_exact_pool_role_generation_and_context() {
            let mut s = live_session(4);
            let mut request = execution_reply_request(0, 1);
            request.names_reply_pool = false;
            assert_eq!(
                s.admit_execution_reply(&request),
                Err(SessionRefusal::ControlRenderForeignAllocation)
            );

            let mut request = execution_reply_request(0, 1);
            request.expected_allocation_generation -= 1;
            assert_eq!(
                s.admit_execution_reply(&request),
                Err(SessionRefusal::ControlRenderPoolGenerationStale {
                    found: POOL_GEN - 1,
                    expected: POOL_GEN,
                })
            );

            for access in [0, HELIOS_HNR2_ACCESS_READ] {
                let mut request = execution_reply_request(0, 1);
                request.access_flags = access;
                assert_eq!(
                    s.admit_execution_reply(&request),
                    Err(SessionRefusal::ControlRenderAccessNotWrite { found: access })
                );
            }

            let mut request = execution_reply_request(0, 1);
            request.owner_context_generation = 0;
            assert!(s.admit_execution_reply(&request).is_err());
        }

        #[test]
        fn provisional_and_draining_sessions_never_admit_an_execution_reply() {
            let mut provisional = TranslationSession::new_provisional(PKG, CAPSET);
            assert_eq!(
                provisional.admit_execution_reply(&execution_reply_request(0, 1)),
                Err(SessionRefusal::SessionProvisional)
            );

            let mut draining = live_session(4);
            draining.begin_draining();
            assert_eq!(
                draining.admit_execution_reply(&execution_reply_request(0, 1)),
                Err(SessionRefusal::SessionDraining)
            );
        }

        #[test]
        fn a_control_render_must_arrive_on_the_control_context() {
            let mut s = live_session(4);
            let mut request = reply_request(0, 1);
            request.on_control_context = false;
            assert_eq!(
                s.admit_control_render(&request),
                Err(SessionRefusal::ControlRenderNotOnControlContext)
            );
        }

        #[test]
        fn the_reply_slot_is_the_only_allocation_this_carrier_ever_lists() {
            let mut s = live_session(4);

            let mut foreign = reply_request(0, 1);
            foreign.names_reply_pool = false;
            assert_eq!(
                s.admit_control_render(&foreign),
                Err(SessionRefusal::ControlRenderForeignAllocation)
            );

            let mut two = reply_request(0, 1);
            two.allocation_count = 2;
            assert_eq!(
                s.admit_control_render(&two),
                Err(SessionRefusal::ControlRenderAllocationCountNotOne { found: 2 })
            );

            let mut none = reply_request(0, 1);
            none.allocation_count = 0;
            assert_eq!(
                s.admit_control_render(&none),
                Err(SessionRefusal::ControlRenderAllocationCountNotOne { found: 0 })
            );

            let mut no_reply = reply_request(0, 1);
            no_reply.has_reply = false;
            no_reply.allocation_count = 1;
            assert_eq!(
                s.admit_control_render(&no_reply),
                Err(SessionRefusal::ControlRenderAllocationWithoutReply { found: 1 })
            );

            let mut bare = reply_request(0, 1);
            bare.has_reply = false;
            bare.allocation_count = 0;
            assert_eq!(
                s.admit_control_render(&bare),
                Ok(ControlRenderAdmission {
                    slot_index: None,
                    slot_generation: 0,
                    reply_offset: 0,
                    reply_capacity_bytes: 0,
                    batch_token: 1,
                })
            );
        }

        #[test]
        fn a_control_render_must_name_the_pools_current_generation_and_write_access() {
            let mut s = live_session(4);

            let mut stale = reply_request(0, 1);
            stale.expected_allocation_generation = POOL_GEN - 1;
            assert_eq!(
                s.admit_control_render(&stale),
                Err(SessionRefusal::ControlRenderPoolGenerationStale {
                    found: POOL_GEN - 1,
                    expected: POOL_GEN,
                })
            );

            for access in [
                0,
                HELIOS_HNR2_ACCESS_READ,
                HELIOS_HNR2_ACCESS_READ | HELIOS_HNR2_ACCESS_WRITE,
            ] {
                let mut wrong = reply_request(0, 1);
                wrong.access_flags = access;
                assert_eq!(
                    s.admit_control_render(&wrong),
                    Err(SessionRefusal::ControlRenderAccessNotWrite { found: access })
                );
            }
        }

        #[test]
        fn one_checkout_one_render_and_a_slot_is_reusable_only_after_retire() {
            let mut s = live_session(4);
            let admitted = s.admit_control_render(&reply_request(0, 1)).unwrap();
            assert_eq!(admitted.slot_index, Some(0));

            // A second Render naming the same slot while one is in flight.
            assert_eq!(
                s.admit_control_render(&reply_request(0, 2)),
                Err(SessionRefusal::ControlRenderSlotBusy { in_flight: 1 })
            );

            // Published is not retired: the CPU has not consumed it yet.
            s.publish_slot(0, 1, 42).expect("host published");
            assert_eq!(
                s.admit_control_render(&reply_request(0, 2)),
                Err(SessionRefusal::ControlRenderSlotBusy { in_flight: 1 })
            );
            assert_eq!(s.pool().unwrap().slots[0].c51_value, 42);

            s.retire_slot(0, 1).expect("CPU consumed it");
            assert_eq!(s.pool().unwrap().slots[0].state, SlotState::Idle);
            assert_eq!(s.pool().unwrap().slots[0].retired_generation, 1);
            s.admit_control_render(&reply_request(0, 2))
                .expect("the slot is reusable now");
        }

        #[test]
        fn a_retired_slot_generation_can_never_come_back() {
            let mut s = live_session(4);
            s.admit_control_render(&reply_request(0, 5)).unwrap();
            s.publish_slot(0, 5, 1).unwrap();
            s.retire_slot(0, 5).unwrap();
            for replay in [1u64, 5] {
                assert_eq!(
                    s.admit_control_render(&reply_request(0, replay)),
                    Err(SessionRefusal::ControlRenderSlotGenerationStale {
                        found: replay,
                        watermark: 5,
                    })
                );
            }
            s.admit_control_render(&reply_request(0, 6))
                .expect("strictly greater");
        }

        #[test]
        fn an_aborted_reply_retires_without_a_published_completion() {
            let mut s = live_session(4);
            s.admit_control_render(&reply_request(0, 5)).unwrap();
            s.abort_slot(0, 5).expect("exact refusal releases its slot");
            let slot = s.pool().unwrap().slots[0];
            assert_eq!(slot.state, SlotState::Idle);
            assert_eq!(slot.generation, 0);
            assert_eq!(slot.retired_generation, 5);
            assert_eq!(slot.c51_value, 0);
            assert_eq!(
                s.admit_control_render(&reply_request(0, 5)),
                Err(SessionRefusal::ControlRenderSlotGenerationStale {
                    found: 5,
                    watermark: 5,
                })
            );
            s.admit_control_render(&reply_request(0, 6))
                .expect("a later generation remains usable");
        }

        #[test]
        fn retiring_out_of_order_or_with_the_wrong_generation_is_refused() {
            let mut s = live_session(4);
            s.admit_control_render(&reply_request(0, 1)).unwrap();
            assert_eq!(
                s.publish_slot(0, 2, 7),
                Err(SessionRefusal::ControlRenderSlotGenerationUnknown { found: 2 })
            );
            // Retiring before the host published is refused: the reply bytes are
            // not there yet.
            assert_eq!(
                s.retire_slot(0, 1),
                Err(SessionRefusal::ControlRenderSlotBusy { in_flight: 1 })
            );
            // An idle slot has nothing to publish.
            assert_eq!(
                s.publish_slot(1, 1, 7),
                Err(SessionRefusal::ControlRenderSlotGenerationUnknown { found: 1 })
            );
            assert_eq!(
                s.retire_slot(REPLY_SLOTS, 1),
                Err(SessionRefusal::ControlRenderSlotIndexOutOfRange { found: REPLY_SLOTS })
            );
        }

        #[test]
        fn all_four_slots_are_independent() {
            let mut s = live_session(4);
            for slot in 0..REPLY_SLOTS {
                let admitted = s.admit_control_render(&reply_request(slot, 1)).unwrap();
                assert_eq!(admitted.slot_index, Some(slot));
            }
            // Every one is now in flight; each keeps its own generation.
            for slot in 0..REPLY_SLOTS {
                assert_eq!(
                    s.pool().unwrap().slots[slot].state,
                    SlotState::InFlight,
                    "slot {slot}"
                );
                assert_eq!(
                    s.admit_control_render(&reply_request(slot, 2)),
                    Err(SessionRefusal::ControlRenderSlotBusy { in_flight: 1 })
                );
            }
        }

        #[test]
        fn a_reply_offset_past_the_pool_cannot_index_a_slot() {
            let mut s = live_session(4);
            let mut past = reply_request(0, 1);
            past.reply_offset = HELIOS_HVM1_REPLY_POOL_BYTES;
            assert_eq!(
                s.admit_control_render(&past),
                Err(SessionRefusal::ControlRenderSlotIndexOutOfRange { found: REPLY_SLOTS })
            );
        }

        #[test]
        fn a_control_render_before_the_pool_is_bound_is_refused() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            assert_eq!(
                s.admit_control_render(&reply_request(0, 1)),
                Err(SessionRefusal::ReplyPoolNotBound)
            );
        }

        #[test]
        fn the_init_render_itself_runs_on_a_provisional_session() {
            // The Render that carries INIT is a control Render, and the session
            // is not live until its reply lands — so admission may not require it.
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            let admitted = s.admit_control_render(&reply_request(0, 1)).unwrap();
            assert_eq!(admitted.slot_index, Some(0));
            assert_eq!(s.phase(), SessionPhase::Provisional);
        }

        // ── the reply bytes the KMD emits ─────────────────────────────────

        #[test]
        fn the_emitted_init_reply_is_exactly_what_the_icd_validates() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            let reply = s.complete_init(8, 3, 7, CAP).expect("completes");
            // The ICD re-validates with its own requested capacity; the KMD must
            // never emit a reply that fails on the other side.
            let admission = reply.validate(PKG, CAPSET, 8).expect("ICD accepts");
            assert_eq!(admission.session_generation, 7);
            assert_eq!(admission.capability, CAP);
            assert_eq!(admission.endpoint_capacity, 3);
            assert_eq!(reply.reserved, 0);
            assert_eq!(reply.capset, CAPSET);
        }

        #[test]
        fn the_attach_packet_the_umd_builds_from_our_reply_round_trips() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            let reply = s.complete_init(4, 4, 7, CAP).unwrap();
            let packet = HeliosQueueAttachV1::new(
                reply.package_generation,
                reply.session_generation,
                reply.capability(),
                endpoint(1),
                1,
                HeliosOuterContextKind::D3d12VirtualSubmit,
            );
            assert_eq!(packet.magic, HELIOS_HQA1_MAGIC);
            assert_eq!(packet.struct_size, HELIOS_HQA1_SIZE);
            let admission = s.attach(&packet).expect("attaches");
            assert_eq!(admission.kind, HeliosOuterContextKind::D3d12VirtualSubmit);
        }

        #[test]
        fn engine_class_of_decodes_a_declared_endpoint() {
            let mut s = live_session(4);
            s.attach(&attach_packet(1, 10)).unwrap();
            let ep = s.endpoint(1).unwrap().descriptor;
            assert_eq!(
                engine_class_of(&ep, s.endpoint_capacity()),
                Ok(HeliosEngineClass::Graphics)
            );
        }

        #[test]
        fn check_session_generation_refuses_zero_and_a_foreign_value() {
            let s = live_session(4);
            assert_eq!(check_session_generation(7, &s), Ok(()));
            assert!(matches!(
                check_session_generation(0, &s),
                Err(SessionRefusal::Init(InitRefusal::Generation {
                    reason: GenerationRefusal::Zero,
                    ..
                }))
            ));
            assert!(matches!(
                check_session_generation(8, &s),
                Err(SessionRefusal::Init(InitRefusal::Generation {
                    reason: GenerationRefusal::Mismatch { .. },
                    ..
                }))
            ));
            // A provisional session has no generation, so nothing matches it.
            let provisional = TranslationSession::new_provisional(PKG, CAPSET);
            assert!(check_session_generation(7, &provisional).is_err());
        }

        // ── endpoints, rings, and the host-dispatch FIFO ──────────────────

        #[test]
        fn init_reserves_one_private_ring_for_every_granted_endpoint() {
            let s = live_session(4);
            for id in 1..=4u32 {
                assert_eq!(s.ring_index(id), Ok(id));
            }
        }

        #[test]
        fn two_sessions_have_distinct_namespaces_even_when_ring_ordinals_match() {
            let a = live_session(4);
            let b = live_session(2);
            assert_eq!(a.ring_index(1), Ok(1));
            assert_eq!(b.ring_index(1), Ok(1));
            assert_eq!(a.endpoint_capacity(), 4);
            assert_eq!(b.endpoint_capacity(), 2);
        }

        #[test]
        fn a_provisional_session_has_no_rings_and_no_endpoints() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            assert_eq!(
                s.ring_index(1),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 1,
                    capacity: 0
                })
            );
        }

        #[test]
        fn only_the_granted_endpoints_own_a_ring() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            s.complete_init(8, 2, 7, CAP).unwrap();
            assert_eq!(s.ring_index(1), Ok(1));
            assert_eq!(s.ring_index(2), Ok(2));
            assert_eq!(
                s.ring_index(3),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 3,
                    capacity: 2
                })
            );
        }

        #[test]
        fn host_dispatch_serials_are_arrival_order_and_per_endpoint() {
            let mut s = live_session(4);
            let a1 = s.enqueue_host_dispatch(1).unwrap();
            let a2 = s.enqueue_host_dispatch(1).unwrap();
            let b1 = s.enqueue_host_dispatch(2).unwrap();
            assert!(a2 > a1, "same endpoint must be strictly increasing");
            assert_eq!(b1, a1, "a second endpoint starts its own sequence");
        }

        #[test]
        fn the_host_dispatch_fifo_refuses_at_its_bound_and_never_waits() {
            let mut s = live_session(4);
            let depth = HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH;
            for _ in 0..depth {
                s.enqueue_host_dispatch(1).expect("under the bound");
            }
            assert!(matches!(
                s.enqueue_host_dispatch(1),
                Err(SessionRefusal::Capacity(HeliosCapacityRefusal {
                    limit: HeliosCapacityLimit::HostDispatchFifoDepth,
                    ..
                }))
            ));
            // A refused enqueue must not have consumed a serial or a slot.
            s.retire_host_dispatch(1).expect("one retires");
            s.enqueue_host_dispatch(1)
                .expect("and the slot is reusable");
            // Endpoint 2 is untouched by endpoint 1's exhaustion.
            s.enqueue_host_dispatch(2).expect("independent FIFO");
        }

        #[test]
        fn host_dispatch_refuses_an_ungranted_endpoint() {
            let mut s = live_session(1);
            assert_eq!(
                s.enqueue_host_dispatch(2),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 2,
                    capacity: 1
                })
            );
        }

        #[test]
        fn retiring_an_empty_fifo_is_a_refusal_not_a_wrap() {
            let mut s = live_session(4);
            assert_eq!(
                s.retire_host_dispatch(1),
                Err(SessionRefusal::HostDispatchFifoUnderflow { endpoint_id: 1 })
            );
        }

        #[test]
        fn a_draining_session_enqueues_nothing() {
            let mut s = live_session(4);
            s.begin_draining();
            assert_eq!(
                s.enqueue_host_dispatch(1),
                Err(SessionRefusal::SessionDraining)
            );
        }

        // ── snapshot accounting ───────────────────────────────────────────

        #[test]
        fn a_session_holds_at_most_four_live_snapshots() {
            let mut s = live_session(4);
            for _ in 0..4 {
                s.admit_snapshot_bytes(1024).expect("under the bound");
            }
            assert!(matches!(
                s.admit_snapshot_bytes(1024),
                Err(SessionRefusal::Hnr2Capacity(_))
            ));
            assert_eq!(s.live_snapshots(), 4);
            assert_eq!(s.live_snapshot_bytes(), 4096);
            s.release_snapshot_bytes(1024).expect("one is consumed");
            s.admit_snapshot_bytes(1024).expect("and the slot returns");
        }

        #[test]
        fn one_snapshot_may_not_exceed_the_per_result_cap() {
            let mut s = live_session(4);
            assert!(matches!(
                s.admit_snapshot_bytes(TranslationSession::MAX_SNAPSHOT_BYTES + 1),
                Err(SessionRefusal::Hnr2Capacity(_))
            ));
            assert_eq!(s.live_snapshots(), 0);
        }

        #[test]
        fn releasing_more_snapshot_bytes_than_are_live_is_refused() {
            let mut s = live_session(4);
            assert_eq!(
                s.release_snapshot_bytes(1),
                Err(SessionRefusal::SnapshotAccountingUnderflow)
            );
            s.admit_snapshot_bytes(16).unwrap();
            assert_eq!(
                s.release_snapshot_bytes(32),
                Err(SessionRefusal::SnapshotAccountingUnderflow)
            );
            assert_eq!(s.live_snapshots(), 1);
            assert_eq!(s.live_snapshot_bytes(), 16);
        }

        #[test]
        fn a_draining_session_admits_no_new_snapshot() {
            let mut s = live_session(4);
            s.begin_draining();
            assert_eq!(
                s.admit_snapshot_bytes(16),
                Err(SessionRefusal::SessionDraining)
            );
        }

        #[test]
        fn every_capacity_limit_has_a_distinct_counter_code() {
            let codes = [
                capacity_limit_code(HeliosCapacityLimit::SessionsPerProcess),
                capacity_limit_code(HeliosCapacityLimit::RingIndex),
                capacity_limit_code(HeliosCapacityLimit::OutstandingContextBatches),
                capacity_limit_code(HeliosCapacityLimit::ContextBatchBytes),
                capacity_limit_code(HeliosCapacityLimit::HostDispatchFifoDepth),
            ];
            for (i, a) in codes.iter().enumerate() {
                assert_ne!(*a, 0);
                for b in &codes[i + 1..] {
                    assert_ne!(a, b);
                }
            }
        }
    }
}

/// K6's pure half: the per-context HNR2 assembler, its bounded staging pool, and
/// the output-patch plan.
///
/// The RULES live in `helios_protocol::native_render` and are not restated here;
/// what this module owns is the per-context STATE those rules are evaluated
/// against. That split is what makes K6 gateable on Linux: every decision below
/// is a function of a wire record plus five scalars, and the only Windows fact
/// it needs — whether an allocation-list entry was opened for write — arrives as
/// a `bool`.
///
/// ⛔ Keyed by CONTEXT, not by session, which is why it is a separate module
/// from [`translation_session`] rather than an extension of it: K5's types are
/// all per-session and `OWNERSHIP.md` §1 forbids interleaving two units' state
/// in one `pub mod` block.
pub mod native_render {
    use helios_protocol::native_render::kernel_dma::{
        Hnr2DmaReject, Hnr2PhysicalCapability, HELIOS_HNR2_MAX_OUTPUT_PATCHES,
    };
    use helios_protocol::native_render::{
        admit_render_slot, validate_commit_tables, validate_use_write_operation,
        HeliosNativeRenderPatch, HeliosNativeRenderUse, HeliosNativeRenderV2, Hnr2Accept,
        Hnr2CapacityLimit, Hnr2CapacityRefusal, Hnr2Expect, Hnr2FragmentClass, Hnr2OpenBatch,
        Hnr2Reject, Hnr2TableReject, HELIOS_HNR2_MAX_PATCH_RECORDS, HELIOS_HNR2_MAX_USE_RECORDS,
    };
    use helios_protocol::wddm::{HeliosOuterSubmitRejection, HeliosOuterSubmitV1};

    /// Why a K6 render-path operation was refused.
    ///
    /// Three arms forward `helios_protocol`'s own verdicts unchanged; the rest are
    /// decisions only the KMD can make. Every arm is a counted refusal in
    /// `kmd_render` and none is ever silently repaired.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RenderRefusal {
        /// The HNR2 header was refused by `protocol`.
        Header(Hnr2Reject),
        /// A COMMIT's use/patch tables were refused by `protocol`.
        Table(Hnr2TableReject),
        /// A bounded per-context pool was exhausted.
        Capacity(Hnr2CapacityRefusal),
        /// The output patch plan does not fit the patch-location list dxgkrnl
        /// returned for this Render. One WDDM slot per use record is required —
        /// §10.7's "one real output WDDM patch/capability slot per use" — so a
        /// short list is a refusal and never a partial plan.
        PatchListCapacityExceeded { needed: u32, capacity: u32 },
        /// A staging retirement named more bytes or slots than were checked out.
        /// Unreachable by construction from `kmd_render` (every retire is paired
        /// with its own checkout); counted so a future caller cannot make the
        /// pool drift silently instead of failing.
        StagingUnderflow,
        /// A DMA-local physical capability was refused by `protocol`.
        Dma(Hnr2DmaReject),
        /// A second [`apply_placement`] produced a different record. §18.2 has
        /// `DxgkDdiPatch` invoked twice on one DMA buffer, and the second call
        /// must be a no-op — a disagreement is a defect in the snapshot source,
        /// never something to overwrite.
        PlacementNotRepeatable,
        /// The capability table does not fit the DMA buffer dxgkrnl returned.
        CapabilityTableTooLarge { needed: u64, capacity: u32 },
        /// A COMMIT on a **queue** context set `HAS_REPLY`. Only the one control
        /// context owns the session's reply pool (§10.4:1205), so there is no
        /// slot to check out and `admit_control_render` would refuse it one
        /// layer later with a session-shaped reason rather than a render-shaped
        /// one.
        ReplyOnQueueContext,
        /// A HOS1 descriptor was refused by `protocol`.
        OuterSubmit(HeliosOuterSubmitRejection),
    }

    impl RenderRefusal {
        /// Stable numeric reason code for a KMD counter / ETW field.
        ///
        /// ⛔ The K6-assigned arms are `0x07xx` (local) and `0x08xx`/`0x09xx`
        /// (limits `protocol` declares but does not code). They were `0x06xx`
        /// and `0x05xx`, which are `Hnr2DmaReject`'s and `Hvr1Reject`'s — a
        /// registry value would have read as two different refusals. Every
        /// range here is checked pairwise by `refusal_codes_are_nonzero_and_do_not_collide`.
        pub const fn code(self) -> u32 {
            match self {
                Self::Header(reject) => reject.code(),
                Self::Table(reject) => reject.code(),
                Self::Dma(reject) => reject.code(),
                Self::Capacity(refusal) => match refusal.limit {
                    Hnr2CapacityLimit::OutstandingSubmissions => 0x0801,
                    Hnr2CapacityLimit::SlotPoolBytes => 0x0802,
                    Hnr2CapacityLimit::LiveSnapshots => 0x0803,
                    Hnr2CapacityLimit::LiveSnapshotBytes => 0x0804,
                    Hnr2CapacityLimit::SnapshotBytes => 0x0805,
                },
                Self::PatchListCapacityExceeded { .. } => 0x0701,
                Self::StagingUnderflow => 0x0702,
                Self::PlacementNotRepeatable => 0x0703,
                Self::CapabilityTableTooLarge { .. } => 0x0704,
                Self::ReplyOnQueueContext => 0x0705,
                Self::OuterSubmit(reject) => outer_submit_code(reject),
            }
        }
    }

    /// A stable code for a HOS1 rejection. `protocol` gives
    /// [`HeliosOuterSubmitRejection`] no `code()` — it is the one refusal enum in
    /// this path that does not carry one — so K6 assigns `0x09xx` here rather
    /// than letting the counter report a bare "refused".
    pub const fn outer_submit_code(reject: HeliosOuterSubmitRejection) -> u32 {
        use HeliosOuterSubmitRejection as R;
        match reject {
            R::Magic { .. } => 0x0901,
            R::AbiVersion { .. } => 0x0902,
            R::StructSize { .. } => 0x0903,
            R::PrivateDataSize { .. } => 0x0904,
            R::PackageGeneration { .. } => 0x0905,
            R::SessionGeneration { .. } => 0x0906,
            R::ContextGeneration { .. } => 0x0907,
            R::EndpointId { .. } => 0x0908,
            R::NotD3D12VirtualContext { .. } => 0x0909,
            R::BatchIdZero => 0x090A,
            R::BatchIdNotIncreasing { .. } => 0x090B,
            R::Hob1BytesZero => 0x090C,
            R::Hob1BytesBelowHeader { .. } => 0x090D,
            R::Hob1BytesAboveLimit { .. } => 0x090E,
            R::Hob1BytesMismatch { .. } => 0x090F,
            R::ReservedNonZero { .. } => 0x0910,
            R::Hob1BatchIdMismatch { .. } => 0x0911,
            R::Hob1CrcMismatch { .. } => 0x0912,
            R::Hob1TotalBytesMismatch { .. } => 0x0913,
        }
    }

    /// The five `DXGKARG_RENDER` scalars the header rules are evaluated against.
    ///
    /// It exists so the caller cannot forget one: [`Hnr2Expect`] mixes these with
    /// per-context state, and assembling it inside `kmd_render` from two sources
    /// is how a stale `open` gets paired with a fresh `DmaSize`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RenderEnv {
        /// The admitted atomic-package generation.
        pub package_generation: u64,
        /// `DXGKARG_RENDER::AllocationListSize`.
        pub allocation_list_count: u32,
        /// `DXGKARG_RENDER::CommandLength`, as probed and copied.
        pub command_length: u32,
        /// `DXGKARG_RENDER::DmaSize` — the buffer dxgkrnl actually returned.
        pub command_buffer_bytes: u32,
        /// `DXGKARG_RENDER::PatchLocationListInSize`.
        pub patch_location_list_in_size: u32,
    }

    /// The bounded per-context staging pool (§10.7: 64 outstanding submissions,
    /// 15 MiB staged).
    ///
    /// ⛔ Refusal, never a wait: `Hnr2CapacityRefusal`'s own doc fixes that for
    /// the staging arms, and a wait inside a Render is a DDI that does not return.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct StagingPool {
        outstanding: u32,
        staged_bytes: u64,
    }

    impl StagingPool {
        pub const fn new() -> Self {
            Self {
                outstanding: 0,
                staged_bytes: 0,
            }
        }

        pub const fn outstanding(&self) -> u32 {
            self.outstanding
        }

        pub const fn staged_bytes(&self) -> u64 {
            self.staged_bytes
        }

        /// Admit `bytes` of staging. The pool moves only on success.
        pub fn checkout(&mut self, bytes: u64) -> Result<(), RenderRefusal> {
            admit_render_slot(self.outstanding, self.staged_bytes, bytes)
                .map_err(RenderRefusal::Capacity)?;
            // `admit_render_slot` proved both sums fit, so neither add can wrap.
            self.outstanding += 1;
            self.staged_bytes += bytes;
            Ok(())
        }

        /// Release one checked-out staging slot of `bytes`.
        pub fn retire(&mut self, bytes: u64) -> Result<(), RenderRefusal> {
            if self.outstanding == 0 || bytes > self.staged_bytes {
                return Err(RenderRefusal::StagingUnderflow);
            }
            self.outstanding -= 1;
            self.staged_bytes -= bytes;
            Ok(())
        }

        /// Release a staging slot whose finite host operation completed in the
        /// synchronous Render callback. There is no scheduler-owned payload
        /// custody after this terminal; SubmitCommand carries only the fence
        /// edge and must not retain or retire the staging bytes again.
        pub fn finish_synchronous(&mut self, bytes: u64) -> Result<(), RenderRefusal> {
            self.retire(bytes)
        }
    }

    /// Where a COMMIT's output patch entries go in the runtime's
    /// `D3DDDI_PATCHLOCATIONLIST`.
    ///
    /// One entry per USE record, contiguous from `first_slot`. The `DriverId` of
    /// entry *i* is the capability ordinal *i*, so the physical capability table
    /// and the patch list are indexed by the same number and neither can be
    /// re-derived from the other.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PatchPlan {
        pub first_slot: u32,
        pub count: u32,
    }

    impl PatchPlan {
        /// The allocation-list index and capability ordinal of plan entry `i`.
        pub fn entry(&self, uses: &[HeliosNativeRenderUse], i: u32) -> Option<(u32, u32)> {
            if i >= self.count {
                return None;
            }
            let use_record = uses.get(i as usize)?;
            Some((use_record.allocation_list_index, i))
        }
    }

    /// Plan the output patch slots for a COMMIT.
    ///
    /// ⛔ One slot per USE record, NOT per typed-operand patch record. The two
    /// counts differ by up to 2x at the caps (4096 uses, 8192 operands): the typed
    /// operands are positions inside the copied Venus payload and are never WDDM
    /// patch locations (§10.4:1288-1296), while the WDDM patch list is where the
    /// per-allocation physical capability is written back.
    pub fn plan_output_patch_slots(
        uses: &[HeliosNativeRenderUse],
        patch_list_capacity: u32,
    ) -> Result<PatchPlan, RenderRefusal> {
        let needed = uses.len();
        if needed > HELIOS_HNR2_MAX_USE_RECORDS as usize {
            return Err(RenderRefusal::Table(Hnr2TableReject::UseCountMismatch));
        }
        let needed = needed as u32;
        if needed > patch_list_capacity {
            return Err(RenderRefusal::PatchListCapacityExceeded {
                needed,
                capacity: patch_list_capacity,
            });
        }
        Ok(PatchPlan {
            first_slot: 0,
            count: needed,
        })
    }

    /// One context's HNR2 assembler.
    ///
    /// [`Hnr2OpenBatch`] *is* the assembler state, so there is no fragment table,
    /// no cross-context assembler, and nothing to look an incoming fragment up in.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RenderContext {
        open: Option<Hnr2OpenBatch>,
        last_batch_token: u64,
        staging: StagingPool,
    }

    impl Default for RenderContext {
        fn default() -> Self {
            Self::new()
        }
    }

    impl RenderContext {
        pub const fn new() -> Self {
            Self {
                open: None,
                last_batch_token: 0,
                staging: StagingPool::new(),
            }
        }

        pub const fn open_batch(&self) -> Option<Hnr2OpenBatch> {
            self.open
        }

        pub const fn last_batch_token(&self) -> u64 {
            self.last_batch_token
        }

        pub const fn staging(&self) -> &StagingPool {
            &self.staging
        }

        pub fn staging_mut(&mut self) -> &mut StagingPool {
            &mut self.staging
        }

        /// Abandon whatever batch is open, without advancing the token watermark.
        ///
        /// The watermark deliberately survives: a context that abandoned batch N
        /// must still refuse a later fragment that names N, and resetting it would
        /// make an abandoned token replayable.
        pub fn abandon_open_batch(&mut self) {
            self.open = None;
        }
    }

    /// Decide whether one HNR2 fragment is legal on this context, WITHOUT
    /// moving the assembler.
    ///
    /// ⛔ The split from [`apply_render_fragment`] is load-bearing, not
    /// stylistic. `DxgkDdiRender` may legally answer `STATUS_BUFFER_TOO_SMALL`,
    /// and dxgkrnl's documented response is to grow the buffer and CALL THE
    /// DRIVER AGAIN with the same command — so a caller that advanced the
    /// assembler before discovering a short buffer would refuse its own replay
    /// with `BatchTokenNotIncreasing` and lose the context. Validate, do every
    /// fallible thing, then apply.
    pub fn validate_render_fragment(
        context: &RenderContext,
        header: &HeliosNativeRenderV2,
        env: &RenderEnv,
    ) -> Result<Hnr2Accept, RenderRefusal> {
        let expect = Hnr2Expect {
            package_generation: env.package_generation,
            allocation_list_count: env.allocation_list_count,
            command_length: env.command_length,
            command_buffer_bytes: env.command_buffer_bytes,
            patch_location_list_in_size: env.patch_location_list_in_size,
            open: context.open,
            last_batch_token: context.last_batch_token,
        };
        header.validate(&expect).map_err(RenderRefusal::Header)
    }

    /// Move the assembler over a fragment [`validate_render_fragment`] admitted.
    ///
    /// The caller owes it the SAME `header`/`accept` pair, on a context nothing
    /// else has touched in between.
    pub fn apply_render_fragment(
        context: &mut RenderContext,
        header: &HeliosNativeRenderV2,
        accept: &Hnr2Accept,
    ) {
        // The watermark advances when a batch OPENS, because that is the only
        // place `validate` compares it (`BatchTokenNotIncreasing`); a COMMIT of an
        // already-open batch repeats a token that is already at the watermark.
        if accept.class.is_begin() {
            context.last_batch_token = header.batch_token;
        }
        context.open = accept.next_open;
    }

    /// Admit one HNR2 fragment on this context and advance its assembler.
    ///
    /// ⛔ The context moves ONLY on success. A refused fragment leaves the open
    /// batch exactly as it was, so a malformed Render cannot corrupt the batch a
    /// well-formed one is still building — and a caller that refuses is free to
    /// leave the context alive.
    pub fn admit_render_fragment(
        context: &mut RenderContext,
        header: &HeliosNativeRenderV2,
        env: &RenderEnv,
    ) -> Result<Hnr2Accept, RenderRefusal> {
        let accept = validate_render_fragment(context, header, env)?;
        apply_render_fragment(context, header, &accept);
        Ok(accept)
    }

    /// Admit a COMMIT's use and typed-patch tables, including the two checks
    /// `protocol` cannot make.
    ///
    /// ⛔ `validate_commit_tables` does NOT call `validate_use_write_operation` —
    /// it cannot, because the `WriteOperation` bit lives in a `DXGK_ALLOCATIONLIST`
    /// entry `protocol` never sees. Dropping the per-entry loop below therefore
    /// costs nothing at compile time and silently admits a use record that claims
    /// WRITE on a read-only allocation.
    pub fn admit_commit_tables(
        header: &HeliosNativeRenderV2,
        uses: &[HeliosNativeRenderUse],
        patches: &[HeliosNativeRenderPatch],
        allocation_list_count: u32,
        list_write_operations: &[bool],
    ) -> Result<(), RenderRefusal> {
        if patches.len() > HELIOS_HNR2_MAX_PATCH_RECORDS as usize {
            return Err(RenderRefusal::Table(Hnr2TableReject::PatchCountMismatch));
        }
        if list_write_operations.len() as u64 != allocation_list_count as u64 {
            return Err(RenderRefusal::Table(Hnr2TableReject::UseCountMismatch));
        }
        validate_commit_tables(header, uses, patches, allocation_list_count)
            .map_err(RenderRefusal::Table)?;
        for record in uses {
            let bit = list_write_operations
                .get(record.allocation_list_index as usize)
                .copied()
                .ok_or(RenderRefusal::Table(
                    Hnr2TableReject::AllocationIndexOutOfRange,
                ))?;
            validate_use_write_operation(record, bit).map_err(RenderRefusal::Table)?;
        }
        Ok(())
    }

    /// Which fragment shapes carry the COMMIT tables, restated as a predicate the
    /// platform half can branch on without importing `protocol`'s enum.
    pub const fn fragment_carries_tables(class: Hnr2FragmentClass) -> bool {
        class.is_commit()
    }

    // ── the DMA-local capability table ───────────────────────────────────────

    /// One [`Hnr2PhysicalCapability`], as an offset stride.
    pub const CAPABILITY_RECORD_BYTES: u32 = 48;
    const _: () =
        assert!(CAPABILITY_RECORD_BYTES as usize == core::mem::size_of::<Hnr2PhysicalCapability>());

    /// Where a COMMIT's capability table lives in the DMA buffer dxgkrnl
    /// returned for that Render.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CapabilityTablePlan {
        pub offset: u32,
        pub count: u32,
        pub bytes: u32,
    }

    impl CapabilityTablePlan {
        /// Byte offset of entry `i`, or `None` past the end.
        pub const fn entry_offset(&self, i: u32) -> Option<u32> {
            if i >= self.count {
                return None;
            }
            match i.checked_mul(CAPABILITY_RECORD_BYTES) {
                Some(delta) => self.offset.checked_add(delta),
                None => None,
            }
        }

        /// Which entry a patch-location `PatchOffset` names, or `None` when the
        /// offset is outside the table or not on a record boundary.
        ///
        /// `DxgkDdiPatch` is handed offsets it must treat as untrusted even
        /// though this driver wrote them: dxgkrnl owns the buffer in between.
        pub const fn entry_at_offset(&self, offset: u32) -> Option<u32> {
            if offset < self.offset {
                return None;
            }
            let delta = offset - self.offset;
            if delta % CAPABILITY_RECORD_BYTES != 0 {
                return None;
            }
            let index = delta / CAPABILITY_RECORD_BYTES;
            if index >= self.count {
                return None;
            }
            Some(index)
        }
    }

    /// Place a COMMIT's capability table at `offset` in the DMA buffer.
    ///
    /// ⛔ The COMMIT's own tables and Venus payload are NOT copied into the DMA
    /// buffer, which is what leaves room: 112 (the header copy) + 4096 × 48 =
    /// 192 KiB of a 256-KiB buffer, while command + table together (229 KiB +
    /// 192 KiB) would not fit at all.
    pub fn plan_capability_table(
        offset: u32,
        count: u32,
        dma_buffer_bytes: u32,
    ) -> Result<CapabilityTablePlan, RenderRefusal> {
        if count > HELIOS_HNR2_MAX_OUTPUT_PATCHES {
            return Err(RenderRefusal::Dma(Hnr2DmaReject::OutputPatchCountTooLarge));
        }
        let bytes = count as u64 * CAPABILITY_RECORD_BYTES as u64;
        let end = bytes + offset as u64;
        if end > dma_buffer_bytes as u64 {
            return Err(RenderRefusal::CapabilityTableTooLarge {
                needed: end,
                capacity: dma_buffer_bytes,
            });
        }
        Ok(CapabilityTablePlan {
            offset,
            count,
            // `end <= dma_buffer_bytes: u32` above, so the cast cannot truncate.
            bytes: bytes as u32,
        })
    }

    /// Build one unplaced capability from a use record and the allocation's own
    /// size, and shape-check it exactly as `protocol` requires at Render.
    ///
    /// `allocation_generation` and `allocation_bytes` come from the KMD
    /// allocation object the OPEN handle resolves to — never from the wire. The
    /// wire's `expected_allocation_generation` is compared against it by the
    /// caller, which is what makes a stale batch a refusal instead of a patch.
    pub fn build_capability(
        use_record: &HeliosNativeRenderUse,
        allocation_generation: u64,
        allocation_bytes: u64,
    ) -> Result<Hnr2PhysicalCapability, RenderRefusal> {
        let capability = Hnr2PhysicalCapability {
            allocation_generation,
            segment_id: 0,
            access_flags: use_record.access_flags,
            physical_address: 0,
            allocation_offset: 0,
            byte_length: allocation_bytes,
            hpm_epoch: 0,
        };
        capability
            .validate_at_render(allocation_bytes)
            .map_err(RenderRefusal::Dma)?;
        Ok(capability)
    }

    /// Snapshot a placement into a capability, idempotently.
    ///
    /// Returns whether the record changed. §18.2 invokes `DxgkDdiPatch` twice on
    /// one DMA buffer, so the second call must find the record already correct
    /// and write nothing; a *different* placement is [`RenderRefusal::
    /// PlacementNotRepeatable`] rather than an overwrite, because the source of
    /// the disagreement is the thing that is broken.
    ///
    /// ⚠ `hpm_epoch` stays 0: the KMD placement epoch is K2/K3's and has no
    /// producer, so `validate_at_submit` cannot run yet. The caller counts that
    /// (`Nr2NoEpoch`) at the site where the epoch would be read.
    pub fn apply_placement(
        capability: &mut Hnr2PhysicalCapability,
        segment_id: u32,
        physical_address: u64,
        allocation_bytes: u64,
    ) -> Result<bool, RenderRefusal> {
        if capability.is_placed() {
            let same = capability.segment_id == segment_id
                && capability.physical_address == physical_address;
            return if same {
                Ok(false)
            } else {
                Err(RenderRefusal::PlacementNotRepeatable)
            };
        }
        if segment_id == 0 {
            // Still unresident. Not a refusal: `DxgkDdiPatch` may legally run
            // before residency completes, and the record stays legally unplaced.
            return Ok(false);
        }
        let mut candidate = *capability;
        candidate.segment_id = segment_id;
        candidate.physical_address = physical_address;
        candidate
            .validate_at_render(allocation_bytes)
            .map_err(RenderRefusal::Dma)?;
        *capability = candidate;
        Ok(true)
    }

    /// Snapshot the allocation's CURRENT placement into a capability,
    /// overwriting whatever is there. Returns whether the record changed.
    ///
    /// ⛔ THIS, NOT [`apply_placement`], IS WHAT `DxgkDdiPatch` OWES. §10.7 has
    /// Patch "snapshot the exact allocation-list placement" on every call —
    /// *idempotent* there means "no irreversible side effect", not "refuses a
    /// new value". Between Render and Patch, VidMm may evict and re-page the
    /// allocation, which is the single event Patch exists for; treating the new
    /// address as a disagreement would keep the stale one. A repeat call with an
    /// unmoved allocation still returns `false` and writes nothing, which is the
    /// §18.2 double-Patch case.
    ///
    /// An allocation that is NOT resident at Patch time snapshots as unplaced —
    /// zeroing the placement fields, because `validate_at_render` requires them
    /// zero when `segment_id == 0` and a half-cleared record is not a state.
    pub fn snapshot_placement(
        capability: &mut Hnr2PhysicalCapability,
        segment_id: u32,
        physical_address: u64,
        allocation_bytes: u64,
    ) -> Result<bool, RenderRefusal> {
        let (segment_id, physical_address) = if segment_id == 0 {
            (0, 0)
        } else {
            (segment_id, physical_address)
        };
        if capability.segment_id == segment_id && capability.physical_address == physical_address {
            return Ok(false);
        }
        let mut candidate = *capability;
        candidate.segment_id = segment_id;
        candidate.physical_address = physical_address;
        candidate
            .validate_at_render(allocation_bytes)
            .map_err(RenderRefusal::Dma)?;
        *capability = candidate;
        Ok(true)
    }

    // ── HOS1, the D3D12 virtual-submit descriptor ────────────────────────────

    /// The per-context state a HOS1 descriptor is validated against.
    ///
    /// Every field is live KMD state taken from the context object HQA1 attach
    /// created; none of it is a lookup key and none of it comes from the wire.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OuterSubmitContext {
        pub package_generation: u64,
        pub session_generation: u64,
        pub context_generation: u64,
        pub endpoint_id: u32,
        /// The context's HQA1 arm — `HELIOS_HQA1_FLAG_D3D12_VIRTUAL` for the
        /// only arm HOS1 exists on.
        pub arm_flags: u32,
        last_batch_id: u64,
    }

    impl OuterSubmitContext {
        pub const fn new(
            package_generation: u64,
            session_generation: u64,
            context_generation: u64,
            endpoint_id: u32,
            arm_flags: u32,
        ) -> Self {
            Self {
                package_generation,
                session_generation,
                context_generation,
                endpoint_id,
                arm_flags,
                last_batch_id: 0,
            }
        }

        pub const fn last_batch_id(&self) -> u64 {
            self.last_batch_id
        }
    }

    /// Admit one HOS1 descriptor on this context and advance its batch-id
    /// watermark.
    ///
    /// ⛔ [`HeliosOuterSubmitV1::cross_check`] is deliberately NOT called: it
    /// reads the HOB1 at the submitted GPUVA and §10.4 forbids the KMD from
    /// dereferencing that address at all. The watermark moves only on success,
    /// so a refused descriptor cannot burn a batch id.
    pub fn admit_hos1(
        context: &mut OuterSubmitContext,
        record: &HeliosOuterSubmitV1,
        command_length: u64,
    ) -> Result<(), RenderRefusal> {
        let expect = helios_protocol::wddm::HeliosOuterBatchExpectation {
            package_generation: context.package_generation,
            session_generation: context.session_generation,
            context_generation: context.context_generation,
            endpoint_id: context.endpoint_id,
            flags: context.arm_flags,
            // `validate` reads neither of these; they are supplied exactly so a
            // future rule that does read them finds the live values rather than
            // a placeholder. The D3D12 virtual arm has no allocation list.
            max_command_bytes: command_length,
            last_batch_id: context.last_batch_id,
            allocation_list_count: 0,
        };
        record
            .validate(&expect, command_length)
            .map_err(RenderRefusal::OuterSubmit)?;
        context.last_batch_id = record.batch_id;
        Ok(())
    }

    /// Commit a fully validated D3D11 physical HOB1 batch to the same
    /// context-local monotonic watermark used by HOS1. The caller has already
    /// validated the complete immutable record against `last_batch_id()`; this
    /// final compare-and-advance prevents a replay if another admission ever
    /// appears between validation and publication.
    pub fn commit_physical_hob1(context: &mut OuterSubmitContext, batch_id: u64) -> bool {
        if batch_id == 0 || batch_id <= context.last_batch_id {
            return false;
        }
        context.last_batch_id = batch_id;
        true
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use helios_protocol::native_render::{
            HELIOS_HNR2_ABI_VERSION, HELIOS_HNR2_ACCESS_READ, HELIOS_HNR2_ACCESS_WRITE,
            HELIOS_HNR2_FLAG_BEGIN, HELIOS_HNR2_FLAG_COMMIT, HELIOS_HNR2_HEADER_SIZE,
            HELIOS_HNR2_MAGIC, HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS,
            HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX, HELIOS_HNR2_SLOT_POOL_BYTES,
            HELIOS_HVC1_DMA_BUFFER_BYTES,
        };

        const PKG: u64 = helios_protocol::HELIOS_PACKAGE_GENERATION;

        /// One fragment of a `fragment_count`-fragment batch, with no tables.
        fn frag(token: u64, index: u16, count: u16, chunk: u32) -> HeliosNativeRenderV2 {
            // Every field spelled out: `HeliosNativeRenderV2` is `Zeroable` but
            // `bytemuck` is deliberately not nameable from this crate, and a
            // helper that lists every field cannot be surprised by a new one.
            let mut h = HeliosNativeRenderV2 {
                magic: HELIOS_HNR2_MAGIC,
                abi_version: HELIOS_HNR2_ABI_VERSION,
                header_size: HELIOS_HNR2_HEADER_SIZE,
                package_generation: PKG,
                batch_token: token,
                total_payload_bytes: chunk as u64 * count as u64,
                fragment_payload_offset: index as u64 * chunk as u64,
                fragment_payload_bytes: chunk,
                fragment_index: index,
                fragment_count: count,
                use_record_offset: 0,
                use_record_count: 0,
                patch_record_offset: 0,
                patch_record_count: 0,
                reply_allocation_list_index: HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX,
                flags: 0,
                reply_offset: 0,
                reply_capacity_bytes: 0,
                fragment_crc64: 0,
                full_payload_crc64: 0,
                reply_slot_generation: 0,
            };
            if index == 0 {
                h.flags |= HELIOS_HNR2_FLAG_BEGIN;
            }
            if index == count - 1 {
                h.flags |= HELIOS_HNR2_FLAG_COMMIT;
            }
            h
        }

        /// The env for a fragment that carries no allocation list.
        fn env(header: &HeliosNativeRenderV2) -> RenderEnv {
            RenderEnv {
                package_generation: PKG,
                allocation_list_count: 0,
                command_length: HELIOS_HNR2_HEADER_SIZE as u32 + header.fragment_payload_bytes,
                command_buffer_bytes: HELIOS_HVC1_DMA_BUFFER_BYTES as u32,
                patch_location_list_in_size: 0,
            }
        }

        /// A COMMIT fragment carrying `uses` use records and no typed operands.
        fn commit_with_uses(token: u64, uses: u32, chunk: u32) -> HeliosNativeRenderV2 {
            let mut h = frag(token, 0, 1, chunk);
            h.use_record_count = uses;
            h.use_record_offset = if uses == 0 {
                0
            } else {
                HELIOS_HNR2_HEADER_SIZE as u32
            };
            h
        }

        fn use_record(index: u32, write: bool) -> HeliosNativeRenderUse {
            HeliosNativeRenderUse {
                allocation_list_index: index,
                access_flags: if write {
                    HELIOS_HNR2_ACCESS_WRITE
                } else {
                    HELIOS_HNR2_ACCESS_READ
                },
                expected_allocation_generation: 0x1_0000_0001,
                first_patch: 0,
                patch_count: 0,
            }
        }

        #[test]
        fn one_fragment_batch_closes_and_advances_the_watermark() {
            let mut ctx = RenderContext::new();
            let h = frag(7, 0, 1, 64);
            let accept = admit_render_fragment(&mut ctx, &h, &env(&h)).unwrap();
            assert_eq!(accept.class, Hnr2FragmentClass::Complete);
            assert!(fragment_carries_tables(accept.class));
            assert_eq!(ctx.open_batch(), None);
            assert_eq!(ctx.last_batch_token(), 7);
        }

        #[test]
        fn three_fragments_assemble_in_order() {
            let mut ctx = RenderContext::new();
            for i in 0..3u16 {
                let h = frag(9, i, 3, 32);
                let accept = admit_render_fragment(&mut ctx, &h, &env(&h)).unwrap();
                assert_eq!(accept.class.is_commit(), i == 2);
            }
            assert_eq!(ctx.open_batch(), None);
            assert_eq!(ctx.last_batch_token(), 9);
        }

        #[test]
        fn an_out_of_order_fragment_is_refused_and_leaves_the_batch_intact() {
            let mut ctx = RenderContext::new();
            let begin = frag(9, 0, 3, 32);
            admit_render_fragment(&mut ctx, &begin, &env(&begin)).unwrap();
            let before = ctx;

            // Fragment 2 while fragment 1 is expected.
            let skipped = frag(9, 2, 3, 32);
            let err = admit_render_fragment(&mut ctx, &skipped, &env(&skipped)).unwrap_err();
            assert_eq!(
                err,
                RenderRefusal::Header(Hnr2Reject::FragmentIndexOutOfOrder)
            );
            assert_eq!(
                ctx, before,
                "a refused fragment must not move the assembler"
            );

            // The batch is still completable.
            for i in 1..3u16 {
                let h = frag(9, i, 3, 32);
                admit_render_fragment(&mut ctx, &h, &env(&h)).unwrap();
            }
            assert_eq!(ctx.open_batch(), None);
        }

        #[test]
        fn a_batch_token_may_not_be_reused_or_go_backwards() {
            let mut ctx = RenderContext::new();
            let first = frag(5, 0, 1, 16);
            admit_render_fragment(&mut ctx, &first, &env(&first)).unwrap();
            for token in [5u64, 4, 1] {
                let h = frag(token, 0, 1, 16);
                assert_eq!(
                    admit_render_fragment(&mut ctx, &h, &env(&h)).unwrap_err(),
                    RenderRefusal::Header(Hnr2Reject::BatchTokenNotIncreasing)
                );
            }
            let next = frag(6, 0, 1, 16);
            admit_render_fragment(&mut ctx, &next, &env(&next)).unwrap();
        }

        /// ⛔ THE REPLAY CASE. `DxgkDdiRender` may answer STATUS_BUFFER_TOO_SMALL
        /// and dxgkrnl then calls it again with the SAME command. A caller that
        /// advanced the assembler before discovering the short buffer refuses its
        /// own replay; validating without applying is what makes the retry work.
        #[test]
        fn validating_without_applying_leaves_a_replay_admissible() {
            let mut ctx = RenderContext::new();
            let h = frag(7, 0, 1, 64);
            let before = ctx;
            let accept = validate_render_fragment(&ctx, &h, &env(&h)).unwrap();
            assert_eq!(ctx, before, "validation must not move the assembler");
            // The same fragment again — this is the replay, and it must be legal.
            let accept2 = validate_render_fragment(&ctx, &h, &env(&h)).unwrap();
            assert_eq!(accept, accept2);
            apply_render_fragment(&mut ctx, &h, &accept2);
            assert_eq!(ctx.last_batch_token(), 7);
            // And once applied, the replay is refused, exactly as before.
            assert_eq!(
                validate_render_fragment(&ctx, &h, &env(&h)).unwrap_err(),
                RenderRefusal::Header(Hnr2Reject::BatchTokenNotIncreasing)
            );
        }

        #[test]
        fn abandoning_a_batch_keeps_the_token_watermark() {
            let mut ctx = RenderContext::new();
            let begin = frag(12, 0, 2, 16);
            admit_render_fragment(&mut ctx, &begin, &env(&begin)).unwrap();
            ctx.abandon_open_batch();
            assert_eq!(ctx.open_batch(), None);
            // The abandoned token must not be replayable.
            let replay = frag(12, 0, 1, 16);
            assert_eq!(
                admit_render_fragment(&mut ctx, &replay, &env(&replay)).unwrap_err(),
                RenderRefusal::Header(Hnr2Reject::BatchTokenNotIncreasing)
            );
        }

        /// ⛔ THE CHECK `protocol` CANNOT MAKE. `validate_commit_tables` accepts
        /// this table; only the per-entry `WriteOperation` cross-check refuses it.
        #[test]
        fn a_write_use_on_a_read_only_allocation_list_entry_is_refused() {
            let h = commit_with_uses(3, 1, 16);
            let uses = [use_record(0, true)];
            assert!(
                validate_commit_tables(&h, &uses, &[], 1).is_ok(),
                "protocol must accept it, or this test proves nothing"
            );
            assert_eq!(
                admit_commit_tables(&h, &uses, &[], 1, &[false]).unwrap_err(),
                RenderRefusal::Table(Hnr2TableReject::WriteOperationMismatch)
            );
            admit_commit_tables(&h, &uses, &[], 1, &[true]).unwrap();
        }

        #[test]
        fn a_read_use_on_a_write_allocation_list_entry_is_refused() {
            let h = commit_with_uses(3, 1, 16);
            let uses = [use_record(0, false)];
            assert_eq!(
                admit_commit_tables(&h, &uses, &[], 1, &[true]).unwrap_err(),
                RenderRefusal::Table(Hnr2TableReject::WriteOperationMismatch)
            );
            admit_commit_tables(&h, &uses, &[], 1, &[false]).unwrap();
        }

        #[test]
        fn a_write_operation_slice_of_the_wrong_length_is_refused() {
            let h = commit_with_uses(3, 2, 16);
            let uses = [use_record(0, false), use_record(1, false)];
            assert!(admit_commit_tables(&h, &uses, &[], 2, &[false]).is_err());
            admit_commit_tables(&h, &uses, &[], 2, &[false, false]).unwrap();
        }

        #[test]
        fn the_commit_table_admission_runs_the_whole_use_list() {
            let h = commit_with_uses(3, 3, 16);
            let uses = [
                use_record(0, false),
                use_record(1, false),
                use_record(2, true),
            ];
            // Only the LAST entry disagrees, so a loop that stops early passes.
            assert_eq!(
                admit_commit_tables(&h, &uses, &[], 3, &[false, false, false]).unwrap_err(),
                RenderRefusal::Table(Hnr2TableReject::WriteOperationMismatch)
            );
        }

        #[test]
        fn the_patch_plan_is_one_slot_per_use_and_is_repeatable() {
            let uses = [
                use_record(0, false),
                use_record(1, true),
                use_record(2, false),
            ];
            let plan = plan_output_patch_slots(&uses, 16).unwrap();
            assert_eq!(plan.first_slot, 0);
            assert_eq!(plan.count, 3);
            assert_eq!(plan, plan_output_patch_slots(&uses, 16).unwrap());
            assert_eq!(plan.entry(&uses, 0), Some((0, 0)));
            assert_eq!(plan.entry(&uses, 2), Some((2, 2)));
            assert_eq!(plan.entry(&uses, 3), None);
        }

        #[test]
        fn a_short_patch_location_list_is_refused_not_truncated() {
            let uses = [use_record(0, false), use_record(1, false)];
            assert_eq!(
                plan_output_patch_slots(&uses, 1).unwrap_err(),
                RenderRefusal::PatchListCapacityExceeded {
                    needed: 2,
                    capacity: 1
                }
            );
            plan_output_patch_slots(&uses, 2).unwrap();
        }

        #[test]
        fn the_staging_pool_refuses_at_both_caps_and_never_waits() {
            let mut pool = StagingPool::new();
            for _ in 0..HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS {
                pool.checkout(1).unwrap();
            }
            assert_eq!(
                pool.checkout(1).unwrap_err().code(),
                0x0801,
                "outstanding-submission cap"
            );
            let mut bytes = StagingPool::new();
            bytes.checkout(HELIOS_HNR2_SLOT_POOL_BYTES).unwrap();
            assert_eq!(bytes.checkout(1).unwrap_err().code(), 0x0802);
            bytes.retire(HELIOS_HNR2_SLOT_POOL_BYTES).unwrap();
            assert_eq!(bytes.staged_bytes(), 0);
            assert_eq!(bytes.outstanding(), 0);
        }

        #[test]
        fn retiring_more_than_was_checked_out_is_refused() {
            let mut pool = StagingPool::new();
            assert_eq!(pool.retire(1).unwrap_err(), RenderRefusal::StagingUnderflow);
            pool.checkout(8).unwrap();
            assert_eq!(pool.retire(9).unwrap_err(), RenderRefusal::StagingUnderflow);
            pool.retire(8).unwrap();
        }

        #[test]
        fn synchronous_render_terminals_do_not_accumulate_outstanding_slots() {
            let mut pool = StagingPool::new();
            for _ in 0..(HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS * 2) {
                pool.checkout(312).unwrap();
                pool.finish_synchronous(312).unwrap();
            }
            assert_eq!(pool.outstanding(), 0);
            assert_eq!(pool.staged_bytes(), 0);
            assert_eq!(
                pool.finish_synchronous(312).unwrap_err(),
                RenderRefusal::StagingUnderflow
            );
        }

        #[test]
        fn a_staging_checkout_that_would_wrap_is_refused() {
            let mut pool = StagingPool::new();
            pool.checkout(1024).unwrap();
            assert!(pool.checkout(u64::MAX).is_err());
            assert_eq!(pool.staged_bytes(), 1024);
        }

        /// ⛔ EVERY arm, not a sample. The two K6-assigned ranges used to sit on
        /// `Hnr2DmaReject`'s `0x06xx` and `Hvr1Reject`'s `0x05xx`, so a registry
        /// value read as two different refusals; a sampled test could not see it
        /// because the colliding arms were in different enums.
        #[test]
        fn refusal_codes_are_nonzero_and_do_not_collide() {
            let cap = |limit| {
                RenderRefusal::Capacity(Hnr2CapacityRefusal {
                    limit,
                    requested: 0,
                    capacity: 0,
                })
                .code()
            };
            let local = [
                RenderRefusal::PatchListCapacityExceeded {
                    needed: 0,
                    capacity: 0,
                }
                .code(),
                RenderRefusal::StagingUnderflow.code(),
                RenderRefusal::PlacementNotRepeatable.code(),
                RenderRefusal::CapabilityTableTooLarge {
                    needed: 0,
                    capacity: 0,
                }
                .code(),
                RenderRefusal::ReplyOnQueueContext.code(),
                cap(Hnr2CapacityLimit::OutstandingSubmissions),
                cap(Hnr2CapacityLimit::SlotPoolBytes),
                cap(Hnr2CapacityLimit::LiveSnapshots),
                cap(Hnr2CapacityLimit::LiveSnapshotBytes),
                cap(Hnr2CapacityLimit::SnapshotBytes),
            ];
            const TOTAL: usize = 10
                + EVERY_HEADER_REJECT.len()
                + EVERY_TABLE_REJECT.len()
                + EVERY_DMA_REJECT.len()
                + OUTER_SUBMIT_REJECT_COUNT;
            let mut codes = [0u32; TOTAL];
            let mut n = 0;
            let push = |slot: &mut [u32; TOTAL], at: &mut usize, code: u32| {
                slot[*at] = code;
                *at += 1;
            };
            for code in local {
                push(&mut codes, &mut n, code);
            }
            for reject in EVERY_HEADER_REJECT {
                push(&mut codes, &mut n, RenderRefusal::Header(reject).code());
            }
            for reject in EVERY_TABLE_REJECT {
                push(&mut codes, &mut n, RenderRefusal::Table(reject).code());
            }
            for reject in EVERY_DMA_REJECT {
                push(&mut codes, &mut n, RenderRefusal::Dma(reject).code());
            }
            for reject in every_outer_submit_reject() {
                push(
                    &mut codes,
                    &mut n,
                    RenderRefusal::OuterSubmit(reject).code(),
                );
            }
            assert_eq!(n, TOTAL, "every refusal arm must be in the collision proof");
            for (i, a) in codes.iter().enumerate() {
                assert_ne!(*a, 0, "index {i}");
                for (j, b) in codes.iter().enumerate().skip(i + 1) {
                    assert_ne!(a, b, "codes {i} and {j} collide at {a:#06x}");
                }
            }
        }

        // ── the capability table ─────────────────────────────────────────────

        fn placed_env() -> (HeliosNativeRenderUse, u64) {
            (use_record(0, true), 8192)
        }

        #[test]
        fn a_capability_starts_unplaced_and_spans_the_whole_allocation() {
            let (record, bytes) = placed_env();
            let cap = build_capability(&record, 0x1_0000_0001, bytes).unwrap();
            assert!(!cap.is_placed());
            assert_eq!(cap.allocation_offset, 0);
            assert_eq!(cap.byte_length, bytes);
            assert_eq!(cap.hpm_epoch, 0);
            assert_eq!(cap.physical_address, 0);
            assert_eq!(cap.access_flags, HELIOS_HNR2_ACCESS_WRITE);
        }

        #[test]
        fn a_capability_on_a_zero_byte_or_generationless_allocation_is_refused() {
            let (record, bytes) = placed_env();
            assert_eq!(
                build_capability(&record, 0x1_0000_0001, 0).unwrap_err(),
                RenderRefusal::Dma(Hnr2DmaReject::ByteLengthZero)
            );
            assert_eq!(
                build_capability(&record, 0, bytes).unwrap_err(),
                RenderRefusal::Dma(Hnr2DmaReject::AllocationGenerationZero)
            );
        }

        #[test]
        fn placement_is_idempotent_and_a_disagreement_is_refused() {
            let (record, bytes) = placed_env();
            let mut cap = build_capability(&record, 7, bytes).unwrap();
            // Not resident yet: legal, and a no-op rather than a refusal.
            assert_eq!(apply_placement(&mut cap, 0, 0, bytes), Ok(false));
            assert!(!cap.is_placed());

            assert_eq!(apply_placement(&mut cap, 1, 0x1000, bytes), Ok(true));
            assert!(cap.is_placed());
            let after_first = cap;
            // §18.2 invokes Patch twice; the second must write nothing.
            assert_eq!(apply_placement(&mut cap, 1, 0x1000, bytes), Ok(false));
            assert_eq!(cap, after_first);

            assert_eq!(
                apply_placement(&mut cap, 2, 0x1000, bytes).unwrap_err(),
                RenderRefusal::PlacementNotRepeatable
            );
            assert_eq!(
                apply_placement(&mut cap, 1, 0x2000, bytes).unwrap_err(),
                RenderRefusal::PlacementNotRepeatable
            );
            assert_eq!(cap, after_first, "a refused placement must not write");
        }

        /// ⛔ THE EVENT `DxgkDdiPatch` EXISTS FOR. VidMm evicts and re-pages
        /// between Render and Patch; the snapshot must take the NEW address.
        /// `apply_placement` calls that a disagreement — correct for Render,
        /// which never re-places — so Patch needs its own operation, and a test
        /// that would fail if Patch were pointed at the wrong one.
        #[test]
        fn a_patch_snapshot_takes_the_new_placement_and_repeats_are_no_ops() {
            let (record, bytes) = placed_env();
            let mut cap = build_capability(&record, 7, bytes).unwrap();
            assert_eq!(snapshot_placement(&mut cap, 1, 0x1000, bytes), Ok(true));
            assert_eq!(cap.physical_address, 0x1000);
            // The §18.2 second Patch, nothing moved: writes nothing.
            assert_eq!(snapshot_placement(&mut cap, 1, 0x1000, bytes), Ok(false));
            // Relocated: the new address wins, and `apply_placement` would have
            // refused exactly this.
            assert_eq!(snapshot_placement(&mut cap, 1, 0x5000, bytes), Ok(true));
            assert_eq!(cap.segment_id, 1);
            assert_eq!(cap.physical_address, 0x5000);
            assert_eq!(
                apply_placement(&mut cap, 1, 0x9000, bytes).unwrap_err(),
                RenderRefusal::PlacementNotRepeatable
            );
            // Evicted: snapshots as unplaced, with the placement fields zeroed
            // together — `validate_at_render` forbids a half-cleared record.
            assert_eq!(snapshot_placement(&mut cap, 0, 0x5000, bytes), Ok(true));
            assert!(!cap.is_placed());
            assert_eq!(cap.physical_address, 0);
            cap.validate_at_render(bytes).unwrap();
        }

        #[test]
        fn a_patch_snapshot_refuses_an_unknown_segment_without_writing() {
            let (record, bytes) = placed_env();
            let mut cap = build_capability(&record, 7, bytes).unwrap();
            assert_eq!(
                snapshot_placement(&mut cap, 9, 0x1000, bytes).unwrap_err(),
                RenderRefusal::Dma(Hnr2DmaReject::SegmentUnknown)
            );
            assert!(!cap.is_placed());
        }

        #[test]
        fn an_unknown_segment_is_refused_rather_than_recorded() {
            let (record, bytes) = placed_env();
            let mut cap = build_capability(&record, 7, bytes).unwrap();
            assert_eq!(
                apply_placement(&mut cap, 9, 0x1000, bytes).unwrap_err(),
                RenderRefusal::Dma(Hnr2DmaReject::SegmentUnknown)
            );
            assert!(!cap.is_placed(), "a refused segment must not write");
        }

        /// The base is the 112-byte header copy the HNR2 arm puts at DMA offset
        /// 0, so every entry offset is header-relative and never zero.
        const TABLE_BASE: u32 = helios_protocol::native_render::HELIOS_HNR2_HEADER_SIZE as u32;

        #[test]
        fn the_capability_table_is_addressed_by_offset_in_both_directions() {
            let plan = plan_capability_table(TABLE_BASE, 3, HELIOS_HVC1_DMA_BUFFER_BYTES).unwrap();
            assert_eq!(plan.offset, TABLE_BASE);
            assert_eq!(plan.bytes, 3 * CAPABILITY_RECORD_BYTES);
            assert_eq!(plan.entry_offset(0), Some(TABLE_BASE));
            assert_eq!(
                plan.entry_offset(2),
                Some(TABLE_BASE + 2 * CAPABILITY_RECORD_BYTES)
            );
            assert_eq!(plan.entry_offset(3), None);
            assert_eq!(plan.entry_at_offset(TABLE_BASE), Some(0));
            assert_eq!(
                plan.entry_at_offset(TABLE_BASE + 2 * CAPABILITY_RECORD_BYTES),
                Some(2)
            );
            // Below the base, not on a record boundary, and past the end: all
            // `None`, never a rounded index into the middle of a record.
            assert_eq!(plan.entry_at_offset(TABLE_BASE - 1), None);
            assert_eq!(plan.entry_at_offset(0), None);
            assert_eq!(
                plan.entry_at_offset(TABLE_BASE + CAPABILITY_RECORD_BYTES - 1),
                None
            );
            assert_eq!(
                plan.entry_at_offset(TABLE_BASE + 3 * CAPABILITY_RECORD_BYTES),
                None
            );
        }

        #[test]
        fn the_worst_case_capability_table_fits_the_advertised_dma_buffer() {
            // 4096 * 48 = 196,608 <= 262,144. This is the arithmetic that lets
            // the HNR2 arm put the table at offset 0 instead of after the
            // command, and it must fail the test rather than the target.
            let plan = plan_capability_table(
                TABLE_BASE,
                HELIOS_HNR2_MAX_USE_RECORDS,
                HELIOS_HVC1_DMA_BUFFER_BYTES,
            )
            .unwrap();
            assert_eq!(plan.bytes, HELIOS_HNR2_MAX_USE_RECORDS * 48);
            assert!(TABLE_BASE + plan.bytes <= HELIOS_HVC1_DMA_BUFFER_BYTES);
            // A buffer one byte short is a refusal, not a truncated table, and
            // the base counts toward the end.
            assert_eq!(
                plan_capability_table(TABLE_BASE, 2, TABLE_BASE + 2 * CAPABILITY_RECORD_BYTES - 1)
                    .unwrap_err(),
                RenderRefusal::CapabilityTableTooLarge {
                    needed: TABLE_BASE as u64 + 2 * CAPABILITY_RECORD_BYTES as u64,
                    capacity: TABLE_BASE + 2 * CAPABILITY_RECORD_BYTES - 1,
                }
            );
        }

        // ── HOS1 ─────────────────────────────────────────────────────────────

        const D3D12_VIRTUAL: u32 = helios_protocol::wddm::HELIOS_HOB1_FLAG_D3D12_VIRTUAL;
        const D3D11_PHYSICAL: u32 = helios_protocol::wddm::HELIOS_HOB1_FLAG_D3D11_PHYSICAL;
        const HOB1_HEADER: u32 = helios_protocol::wddm::HELIOS_HOB1_HEADER_BYTES as u32;

        fn hos1(ctx: &OuterSubmitContext, batch_id: u64, hob1_bytes: u32) -> HeliosOuterSubmitV1 {
            HeliosOuterSubmitV1 {
                magic: helios_protocol::wddm::HELIOS_HOS1_MAGIC,
                abi_version: helios_protocol::wddm::HELIOS_HOS1_ABI_VERSION,
                struct_size: helios_protocol::wddm::HELIOS_HOS1_BYTES,
                package_generation: ctx.package_generation,
                session_generation: ctx.session_generation,
                context_generation: ctx.context_generation,
                endpoint_id: ctx.endpoint_id,
                hob1_bytes,
                batch_id,
                hob1_crc64: 0,
                reserved: 0,
            }
        }

        fn outer_ctx(arm: u32) -> OuterSubmitContext {
            OuterSubmitContext::new(PKG, 0x5100, 0x0C7, 3, arm)
        }

        #[test]
        fn a_hos1_batch_id_watermark_advances_only_on_success() {
            let mut ctx = outer_ctx(D3D12_VIRTUAL);
            let good = hos1(&ctx, 5, HOB1_HEADER);
            admit_hos1(&mut ctx, &good, HOB1_HEADER as u64).unwrap();
            assert_eq!(ctx.last_batch_id(), 5);

            for replay in [5u64, 4, 1] {
                let stale = hos1(&ctx, replay, HOB1_HEADER);
                assert_eq!(
                    admit_hos1(&mut ctx, &stale, HOB1_HEADER as u64).unwrap_err(),
                    RenderRefusal::OuterSubmit(HeliosOuterSubmitRejection::BatchIdNotIncreasing {
                        found: replay,
                        last: 5,
                    })
                );
                assert_eq!(ctx.last_batch_id(), 5, "a refusal must not burn a batch id");
            }
            let next = hos1(&ctx, 6, HOB1_HEADER);
            admit_hos1(&mut ctx, &next, HOB1_HEADER as u64).unwrap();
            assert_eq!(ctx.last_batch_id(), 6);
        }

        #[test]
        fn hos1_is_refused_on_the_d3d11_physical_arm() {
            let mut ctx = outer_ctx(D3D11_PHYSICAL);
            let record = hos1(&ctx, 1, HOB1_HEADER);
            assert_eq!(
                admit_hos1(&mut ctx, &record, HOB1_HEADER as u64).unwrap_err(),
                RenderRefusal::OuterSubmit(HeliosOuterSubmitRejection::NotD3D12VirtualContext {
                    context_flags: D3D11_PHYSICAL
                })
            );
        }

        #[test]
        fn a_hos1_whose_hob1_length_disagrees_with_the_runtime_is_refused() {
            let mut ctx = outer_ctx(D3D12_VIRTUAL);
            let record = hos1(&ctx, 1, HOB1_HEADER);
            assert_eq!(
                admit_hos1(&mut ctx, &record, HOB1_HEADER as u64 + 8).unwrap_err(),
                RenderRefusal::OuterSubmit(HeliosOuterSubmitRejection::Hob1BytesMismatch {
                    found: HOB1_HEADER,
                    command_length: HOB1_HEADER as u64 + 8,
                })
            );
            let short = hos1(&ctx, 1, HOB1_HEADER - 1);
            assert_eq!(
                admit_hos1(&mut ctx, &short, HOB1_HEADER as u64 - 1).unwrap_err(),
                RenderRefusal::OuterSubmit(HeliosOuterSubmitRejection::Hob1BytesBelowHeader {
                    found: HOB1_HEADER - 1,
                    header_bytes: helios_protocol::wddm::HELIOS_HOB1_HEADER_BYTES,
                })
            );
        }

        #[test]
        fn a_hos1_naming_another_context_is_refused_on_every_identity_field() {
            let mut ctx = outer_ctx(D3D12_VIRTUAL);
            let base = hos1(&ctx, 1, HOB1_HEADER);
            let mut wrong_session = base;
            wrong_session.session_generation ^= 1;
            let mut wrong_context = base;
            wrong_context.context_generation ^= 1;
            let mut wrong_endpoint = base;
            wrong_endpoint.endpoint_id ^= 1;
            let mut wrong_package = base;
            wrong_package.package_generation ^= 1;
            for record in [wrong_session, wrong_context, wrong_endpoint, wrong_package] {
                assert!(admit_hos1(&mut ctx, &record, HOB1_HEADER as u64).is_err());
                assert_eq!(ctx.last_batch_id(), 0);
            }
            admit_hos1(&mut ctx, &base, HOB1_HEADER as u64).unwrap();
        }
    }

    /// Every [`Hnr2Reject`], for the code-collision proof. A `match`-free list
    /// cannot be checked exhaustively by the compiler, so the count is asserted
    /// against `protocol`'s own highest code instead.
    #[cfg(test)]
    const EVERY_HEADER_REJECT: [Hnr2Reject; 53] = [
        Hnr2Reject::MagicMismatch,
        Hnr2Reject::AbiVersionMismatch,
        Hnr2Reject::HeaderSizeMismatch,
        Hnr2Reject::PackageGenerationUnset,
        Hnr2Reject::PackageGenerationMismatch,
        Hnr2Reject::FlagBitsUnknown,
        Hnr2Reject::BatchTokenZero,
        Hnr2Reject::BatchTokenNotIncreasing,
        Hnr2Reject::BatchTokenMismatch,
        Hnr2Reject::BatchAlreadyOpen,
        Hnr2Reject::NoOpenBatch,
        Hnr2Reject::FragmentCountZero,
        Hnr2Reject::FragmentCountTooLarge,
        Hnr2Reject::FragmentCountChanged,
        Hnr2Reject::FragmentIndexOutOfRange,
        Hnr2Reject::FragmentIndexOutOfOrder,
        Hnr2Reject::BeginFlagMisplaced,
        Hnr2Reject::CommitFlagMisplaced,
        Hnr2Reject::TotalPayloadZero,
        Hnr2Reject::TotalPayloadTooLarge,
        Hnr2Reject::TotalPayloadChanged,
        Hnr2Reject::FragmentPayloadZero,
        Hnr2Reject::FragmentOffsetMismatch,
        Hnr2Reject::FragmentPayloadOverrun,
        Hnr2Reject::NonFinalFragmentExhaustsPayload,
        Hnr2Reject::FinalFragmentShort,
        Hnr2Reject::UseRecordsBeforeCommit,
        Hnr2Reject::PatchRecordsBeforeCommit,
        Hnr2Reject::FullPayloadCrcBeforeCommit,
        Hnr2Reject::AllocationListNotEmpty,
        Hnr2Reject::UseRecordCountTooLarge,
        Hnr2Reject::PatchRecordCountTooLarge,
        Hnr2Reject::UseRecordCountMismatch,
        Hnr2Reject::PatchWithoutUse,
        Hnr2Reject::UseRecordOffsetMismatch,
        Hnr2Reject::PatchRecordOffsetMismatch,
        Hnr2Reject::CommandLengthOverflow,
        Hnr2Reject::CommandLengthMismatch,
        Hnr2Reject::ReplyFlagOutsideCommit,
        Hnr2Reject::ReplyIndexNotSentinel,
        Hnr2Reject::ReplyOffsetNotZero,
        Hnr2Reject::ReplyCapacityNotZero,
        Hnr2Reject::ReplySlotGenerationNotZero,
        Hnr2Reject::ReplyIndexOutOfRange,
        Hnr2Reject::ReplySlotGenerationZero,
        Hnr2Reject::ReplyCapacityTooSmall,
        Hnr2Reject::ReplyCapacityTooLarge,
        Hnr2Reject::ReplyOffsetMisaligned,
        Hnr2Reject::ReplySlotIndexOutOfRange,
        Hnr2Reject::ReplyRangeCrossesSlot,
        Hnr2Reject::CommandLengthAboveDmaBuffer,
        Hnr2Reject::CommandBufferBelowAdvertisedMinimum,
        Hnr2Reject::PatchLocationListInNotEmpty,
    ];

    #[cfg(test)]
    const EVERY_TABLE_REJECT: [Hnr2TableReject; 20] = [
        Hnr2TableReject::UseCountMismatch,
        Hnr2TableReject::PatchCountMismatch,
        Hnr2TableReject::AllocationListTooLarge,
        Hnr2TableReject::AccessFlagsUnknownBits,
        Hnr2TableReject::AccessFlagsZero,
        Hnr2TableReject::AllocationGenerationZero,
        Hnr2TableReject::AllocationIndexOutOfRange,
        Hnr2TableReject::AllocationUsedTwice,
        Hnr2TableReject::PatchRunNotContiguous,
        Hnr2TableReject::PatchRunOutOfRange,
        Hnr2TableReject::PatchRunLeavesGap,
        Hnr2TableReject::PatchAllocationMismatch,
        Hnr2TableReject::PatchOperandKindUnknown,
        Hnr2TableReject::PatchOperandWidthMismatch,
        Hnr2TableReject::PatchOffsetMisaligned,
        Hnr2TableReject::PatchOffsetOutOfPayload,
        Hnr2TableReject::PatchReservedNonZero,
        Hnr2TableReject::WriteOperationMismatch,
        Hnr2TableReject::ReplyIndexNotWritable,
        Hnr2TableReject::ReplyIndexHasNoUseRecord,
    ];

    #[cfg(test)]
    const EVERY_DMA_REJECT: [Hnr2DmaReject; 13] = [
        Hnr2DmaReject::OutputPatchCountNotUseCount,
        Hnr2DmaReject::OutputPatchCountTooLarge,
        Hnr2DmaReject::AllocationGenerationZero,
        Hnr2DmaReject::AccessFlagsInvalid,
        Hnr2DmaReject::ByteLengthZero,
        Hnr2DmaReject::RangeOutOfAllocation,
        Hnr2DmaReject::UnplacedCapabilityNotZeroed,
        Hnr2DmaReject::SegmentUnknown,
        Hnr2DmaReject::CapabilityUnplacedAtSubmit,
        Hnr2DmaReject::AllocationGenerationStale,
        Hnr2DmaReject::PlacementEpochStale,
        Hnr2DmaReject::SegmentNotCurrent,
        Hnr2DmaReject::PhysicalAddressMisaligned,
    ];

    #[cfg(test)]
    #[allow(dead_code)]
    const OUTER_SUBMIT_REJECT_COUNT: usize = 19;

    #[cfg(test)]
    fn every_outer_submit_reject() -> [HeliosOuterSubmitRejection; OUTER_SUBMIT_REJECT_COUNT] {
        use HeliosOuterSubmitRejection as R;
        [
            R::Magic { found: 0 },
            R::AbiVersion { found: 0 },
            R::StructSize { found: 0 },
            R::PrivateDataSize { found: 0 },
            R::PackageGeneration {
                found: 0,
                expected: 0,
            },
            R::SessionGeneration {
                found: 0,
                expected: 0,
            },
            R::ContextGeneration {
                found: 0,
                expected: 0,
            },
            R::EndpointId {
                found: 0,
                expected: 0,
            },
            R::NotD3D12VirtualContext { context_flags: 0 },
            R::BatchIdZero,
            R::BatchIdNotIncreasing { found: 0, last: 0 },
            R::Hob1BytesZero,
            R::Hob1BytesBelowHeader {
                found: 0,
                header_bytes: 0,
            },
            R::Hob1BytesAboveLimit { found: 0, limit: 0 },
            R::Hob1BytesMismatch {
                found: 0,
                command_length: 0,
            },
            R::ReservedNonZero { found: 0 },
            R::Hob1BatchIdMismatch { hos1: 0, hob1: 0 },
            R::Hob1CrcMismatch { hos1: 0, hob1: 0 },
            R::Hob1TotalBytesMismatch { hos1: 0, hob1: 0 },
        ]
    }
}
