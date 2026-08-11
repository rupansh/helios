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
//! rule above — see the argument in `kmd_logic/Cargo.toml`. Note that
//! [`native_fence_lifecycle`] predates the decision and still re-declares the
//! HNF1 offset table locally; that is history, not a pattern to copy.
//!
//! Run the tests with `cargo test` inside `kmd_logic/`, the same way `protocol/`
//! is tested. Nothing else runs them.

#![no_std]

pub mod committed_mode;
pub mod committed_mode_lifecycle;
pub mod context_attachment;
pub mod context_lifecycle;
pub mod control_ownership;
pub mod direct_scanout_admission;
pub mod direct_scanout_lifetime;
pub mod display_backing_lifetime;

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
/// Overflow is **sticky and non-panicking**: a write that would exceed
/// [`MAX_CMD_BYTES`] is dropped, the writer is poisoned, and [`Writer::finished`]
/// returns `None` forever after. The caller turns that into a refusal with a
/// named counter. Every write method is infallible so the ~40 encoder bodies
/// stay linear; the single fallible point is where the bytes are handed out.
pub struct Writer {
    buf: [u8; MAX_CMD_BYTES],
    len: usize,
    overflow: bool,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; MAX_CMD_BYTES],
            len: 0,
            overflow: false,
        }
    }

    /// Reserve `n` bytes, or poison the writer and report that there is no room.
    fn reserve(&mut self, n: usize) -> bool {
        if self.overflow || self.len + n > MAX_CMD_BYTES {
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

    /// A Vulkan object handle. Identical bytes to [`Writer::u64`]; the separate
    /// name exists so the KMD's per-class handle newtypes can be written without
    /// spelling out a conversion at every encoder, and so a reader can see at a
    /// glance which 8-byte words in a stream are handles.
    pub fn handle<H: Into<u64>>(&mut self, h: H) {
        self.u64(h.into());
    }

    /// The command header: `VkCommandTypeEXT | VkCommandFlagsEXT`.
    pub fn header(&mut self, cmd_type: u32, flags: u32) {
        self.u32(cmd_type);
        self.u32(flags);
    }

    /// Bytes written so far. Meaningless once [`Writer::overflowed`] is set.
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

/// Which pNext chain a `VkImageCreateInfo` carries.
///
/// An exhaustive enum rather than an ad-hoc `count(true)/i32(ST_…)` sequence, so
/// the nesting order lives in exactly one place and an unrepresentable chain
/// cannot be encoded.
///
/// The `ExternalMemoryWithModifierList` variant the review specifies is NOT
/// here: `ST_IMAGE_DRM_FORMAT_MODIFIER_LIST_CREATE_INFO`, `DRM_FORMAT_MOD_LINEAR`
/// and `IMAGE_TILING_DRM_FORMAT_MODIFIER` all went with T6/R906 when the modifier
/// path was deleted, so it would have zero users.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImagePNext {
    /// An internal image with no external-memory contract.
    None,
    /// `VkExternalMemoryImageCreateInfo` with the given `VkExternalMemoryHandleTypeFlags`.
    ExternalMemory { handle_type: u32 },
}

/// Everything the three image creates differ by. The rest of
/// `VkImageCreateInfo` — 2D, depth 1, one mip, one layer, 1 sample, exclusive
/// sharing, no queue families — is fixed by [`encode_image_create`].
#[derive(Clone, Copy)]
pub struct ImageCreateSpec {
    pub pnext: ImagePNext,
    pub flags: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub tiling: u32,
    pub usage: u32,
    pub initial_layout: u32,
}

/// Encode one `vkCreateImage` command stream.
pub fn encode_image_create(device_id: u64, image_id: u64, spec: &ImageCreateSpec) -> Writer {
    let mut w = Writer::new();
    w.header(CMD_CREATE_IMAGE, CMD_FLAG_GENERATE_REPLY);
    w.u64(device_id);
    w.count(true);
    w.i32(ST_IMAGE_CREATE_INFO);
    match spec.pnext {
        ImagePNext::None => w.count(false),
        ImagePNext::ExternalMemory { handle_type } => {
            w.count(true);
            w.i32(ST_EXTERNAL_MEMORY_IMAGE_CREATE_INFO);
            w.count(false);
            w.u32(handle_type);
        }
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
    /// `VkMemoryDedicatedAllocateInfo` for an image.
    Dedicated { image: u64 },
    /// `VkExportMemoryAllocateInfo` -> `VkMemoryDedicatedAllocateInfo`.
    ExportDedicated { handle_type: u32, image: u64 },
    /// `VkImportMemoryResourceInfoMESA` — adopt an existing virtio resource.
    ImportResource { resource_id: u32 },
}

/// Everything the five memory allocations differ by.
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
        MemoryPNext::Dedicated { image } => {
            w.count(true);
            w.i32(ST_MEMORY_DEDICATED_ALLOCATE_INFO);
            w.count(false);
            w.u64(image);
            w.u64(0); // buffer
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
        MemoryPNext::ImportResource { resource_id } => {
            w.count(true);
            w.i32(ST_IMPORT_MEMORY_RESOURCE_INFO_MESA);
            w.count(false);
            w.u32(resource_id);
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
    // GOLDEN_OPTIMAL_PRESENT_IMAGE_ALIAS (140 bytes)
    const GOLDEN_OPTIMAL_PRESENT_IMAGE_ALIAS: &[u8] = &[
        0x36, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0xe3, 0x9b, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x2c, 0x00, 0x00, 0x00, 0x68, 0x07, 0x00, 0x00, 0x06, 0x04, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x17, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x88, 0x77,
        0x77, 0x66, 0x66, 0x55, 0x55,
    ];
    // GOLDEN_PRESENT_CONVERSION_IMAGE (124 bytes)
    const GOLDEN_PRESENT_CONVERSION_IMAGE: &[u8] = &[
        0x36, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x2c,
        0x00, 0x00, 0x00, 0x68, 0x07, 0x00, 0x00, 0x06, 0x04, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x88, 0x77, 0x77,
        0x66, 0x66, 0x55, 0x55,
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
    // GOLDEN_MEMORY_DEDICATED (100 bytes)
    const GOLDEN_MEMORY_DEDICATED: &[u8] = &[
        0x15, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0xba, 0x9c, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x88, 0x88, 0x77, 0x77, 0x66, 0x66, 0x55, 0x55, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0x99, 0x99,
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
    // GOLDEN_MEMORY_IMPORT_RESOURCE (88 bytes)
    const GOLDEN_MEMORY_IMPORT_RESOURCE: &[u8] = &[
        0x15, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x44, 0x44, 0x33, 0x33, 0x22, 0x22, 0x11,
        0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xa6, 0xa0, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0x99, 0x99,
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
                pnext: ImagePNext::ExternalMemory {
                    handle_type: 0x0000_0200,
                },
                flags: 0,
                format: 44, // VK_FORMAT_B8G8R8A8_UNORM
                width: 1896,
                height: 1030,
                tiling: IMAGE_TILING_LINEAR,
                usage: 0x1 | 0x2,
                initial_layout: 8, // PREINITIALIZED
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_LINEAR_SCANOUT_IMAGE));
    }

    #[test]
    fn optimal_present_image_alias_bytes_are_unchanged() {
        let w = encode_image_create(
            GOLD_DEVICE,
            GOLD_IMAGE,
            &ImageCreateSpec {
                pnext: ImagePNext::ExternalMemory {
                    handle_type: 0x0000_0001,
                },
                flags: 0x8, // MUTABLE_FORMAT
                format: 44,
                width: 1896,
                height: 1030,
                tiling: IMAGE_TILING_OPTIMAL,
                usage: 0x1 | 0x2 | 0x4 | 0x10,
                initial_layout: 0, // UNDEFINED
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_OPTIMAL_PRESENT_IMAGE_ALIAS));
    }

    #[test]
    fn present_conversion_image_bytes_are_unchanged() {
        let w = encode_image_create(
            GOLD_DEVICE,
            GOLD_IMAGE,
            &ImageCreateSpec {
                pnext: ImagePNext::None,
                flags: 0,
                format: 44,
                width: 1896,
                height: 1030,
                tiling: IMAGE_TILING_OPTIMAL,
                usage: 0x1 | 0x2,
                initial_layout: 0,
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_PRESENT_CONVERSION_IMAGE));
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
    fn dedicated_memory_allocate_bytes_are_unchanged() {
        let w = encode_memory_allocate(
            GOLD_DEVICE,
            GOLD_MEMORY,
            &MemoryAllocateSpec {
                pnext: MemoryPNext::Dedicated { image: GOLD_IMAGE },
                size: GOLD_SIZE,
                memory_type_index: GOLD_MTI,
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_MEMORY_DEDICATED));
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

    #[test]
    fn import_resource_memory_allocate_bytes_are_unchanged() {
        let w = encode_memory_allocate(
            GOLD_DEVICE,
            GOLD_MEMORY,
            &MemoryAllocateSpec {
                pnext: MemoryPNext::ImportResource {
                    resource_id: 0x1234,
                },
                size: GOLD_SIZE,
                memory_type_index: GOLD_MTI,
            },
        );
        assert_eq!(w.finished(), Some(GOLDEN_MEMORY_IMPORT_RESOURCE));
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
    //! Pure decisions for scanout-resource retirement while asynchronous binds
    //! share the virtio control FIFO.

    /// Whether a pure-query FIFO barrier is required before the latest
    /// host-selected resource is authoritative. A mismatch means at least one
    /// SET was issued after the last successful response; it may succeed or fail,
    /// and only a later response can distinguish those outcomes.
    pub const fn needs_fifo_barrier(wire_seq: u64, accepted_seq: u64) -> bool {
        wire_seq != accepted_seq
    }

    /// Disable scanout only when the retiring resource is still the final
    /// successful host selection. A different resource is already the lifetime
    /// barrier; queueing zero behind it would blank that newer selection.
    pub const fn needs_disable(retiring_resource: u32, final_host_resource: u32) -> bool {
        retiring_resource != 0 && retiring_resource == final_host_resource
    }
}

#[cfg(test)]
mod scanout_retire_tests {
    use super::scanout_retire::{needs_disable, needs_fifo_barrier};

    #[test]
    fn issued_newer_bind_requires_a_terminal_fifo_response() {
        assert!(needs_fifo_barrier(12, 11));
        assert!(!needs_fifo_barrier(12, 12));
    }

    #[test]
    fn successful_newer_bind_must_not_be_blankened_by_retiring_the_old_one() {
        let retiring_a = 0x121;
        let final_b = 0x128;
        assert!(!needs_disable(retiring_a, final_b));
    }

    #[test]
    fn failed_newer_bind_leaves_the_old_selection_needing_disable() {
        let retiring_a = 0x121;
        assert!(needs_disable(retiring_a, retiring_a));
        assert!(!needs_disable(retiring_a, 0));
    }
}

pub mod scanout_lease {
    /// "This submission is not gated on any host read." Used for paging
    /// buffers, render submissions, and every flip that carries no scan-out
    /// presentation (the MMIO/`FlipOnVSyncMmIo` desktop path).
    pub const NO_LEASE: u64 = 0;

    /// The first epoch a tracker mints. Epoch 0 is reserved for [`NO_LEASE`].
    pub const FIRST_EPOCH: u64 = 1;

    /// Mint the epoch after `previous`.
    ///
    /// Saturating: at `u64::MAX` the counter stops instead of wrapping to 0,
    /// because a wrap to 0 would read as [`NO_LEASE`] and silently ungate every
    /// flip. At 200 presentations per second that bound is ~2.9 billion years
    /// away, so the saturation arm exists to make the failure shape *stuck*
    /// rather than *unsafe*.
    pub const fn next_epoch(previous: u64) -> u64 {
        if previous == u64::MAX {
            u64::MAX
        } else {
            previous + 1
        }
    }

    /// Whether a submission whose display-consumer requirement is `lease` may be
    /// released to Windows.
    pub const fn lease_satisfied(read_epoch: u64, lease: u64) -> bool {
        lease == NO_LEASE || read_epoch >= lease
    }

    /// Monotone merge of a new lease-end watermark into the current one.
    pub const fn merge_read_epoch(current: u64, candidate: u64) -> u64 {
        if candidate > current {
            candidate
        } else {
            current
        }
    }

    /// THE OWNERSHIP GATE (ROADMAP defect 0ab-B, D2). Whether a `RESOURCE_FLUSH`
    /// issued right now would be a SURPLUS re-read of the current binding.
    ///
    /// True means: the generation the host is bound to has already been
    /// published (a flush token covering `bound_epoch` completed) AND a newer
    /// presentation has been minted. The successor's own bind edge owns the next
    /// publish, so re-reading now cannot show the newer frame — it can only
    /// re-read a buffer the app may already have reclaimed and cleared, which is
    /// a manufactured black frame (measured: surplus refresh flushes were ~30 %
    /// of publishes at 43.4 % black, against 2.6 % for first reads).
    ///
    /// `tracked` is the third operand and it is not optional. It says the
    /// CURRENT binding was published with an epoch at all. The MMIO /
    /// `FlipOnVSyncMmIo` desktop contract mints no presentations, so on it
    /// `present_epoch` is frozen at whatever the last DMA-flip app left behind:
    /// a stale `present_epoch > bound_epoch` would then hold forever and drop
    /// every desktop refresh — a frozen desktop, defect 0aa. With `tracked`
    /// false the gate is off and the flush path behaves exactly as it did.
    ///
    /// The three passing cases, stated so a future edit cannot lose them:
    /// * a first publish passes (`read_epoch < bound_epoch`);
    /// * an idle desktop re-publish passes (`present == bound <= read`);
    /// * a same-buffer re-present passes, because it mints AND publishes a fresh
    ///   epoch, so `read_epoch < bound_epoch` again at the marker's fire time.
    pub const fn surplus_republish(
        tracked: bool,
        read_epoch: u64,
        bound_epoch: u64,
        present_epoch: u64,
    ) -> bool {
        tracked
            && bound_epoch != NO_LEASE
            && lease_satisfied(read_epoch, bound_epoch)
            && present_epoch > bound_epoch
    }

    /// Whether `fence` is FORWARD of `last` in the WDDM `SubmissionFenceId`
    /// sequence, which wraps at `u32::MAX`.
    ///
    /// dxgkrnl treats a `SubmissionFenceId` as a watermark and requires
    /// monotonic completion, so a fence that is equal to or behind the last
    /// completed one must be dropped rather than re-signalled. Extracted from
    /// `submit_command::signal_dma_completed` so the wrap arithmetic has a host
    /// test; that function calls this.
    pub const fn fence_is_forward(last: u32, fence: u32) -> bool {
        fence != last && fence.wrapping_sub(last) < 0x8000_0000
    }

    /// Why a presentation epoch's host-reader lease ended.
    ///
    /// Every variant is counted separately by the driver: a fix that "works"
    /// because every lease ends as `Cancelled` is not working, and the only way
    /// to see that is to never merge the reasons.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum LeaseEnd {
        /// The exact `RESOURCE_FLUSH` covering this epoch returned. The healthy
        /// steady-state reason, and the only one that means "the host has the
        /// pixels".
        HostRead,
        /// A later successful `SET_SCANOUT_BLOB` bound a different resource, so
        /// no read of this epoch remains or can be queued.
        Superseded,
        /// The flush could not be enqueued, the host answered with an error, or
        /// the allocation was retired. The command has terminated; no future
        /// read from it exists.
        Cancelled,
        /// Transport failure, preemption/TDR epoch, reset or StopDevice.
        Teardown,
    }

    /// Unsampled per-reason tallies. Mirrored into the service key as the `Ls*`
    /// values.
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct LeaseCounters {
        /// Presentation epochs minted.
        pub minted: u32,
        /// `RESOURCE_FLUSH` reads queued with a lease token.
        pub read_queued: u32,
        /// Read tokens completed (success or host error).
        pub read_completed: u32,
        /// Epochs ended by [`LeaseEnd::HostRead`].
        pub ended_read: u32,
        /// Epochs ended by [`LeaseEnd::Superseded`] — a binding that never got
        /// its own read.
        pub ended_superseded: u32,
        /// Epochs ended by [`LeaseEnd::Cancelled`].
        pub ended_cancelled: u32,
        /// Epochs ended by [`LeaseEnd::Teardown`].
        pub ended_teardown: u32,
        /// Completions whose token was at or behind the watermark: coalesced,
        /// duplicated or reordered. Inert, but counted — a large value means the
        /// flush path is issuing reads nobody is waiting for.
        pub stale_completions: u32,
        /// Retirement attempts refused because the lease was still open.
        pub retire_blocked: u32,
        /// Retirement attempts allowed by a satisfied lease.
        pub retire_released: u32,
        /// Deferred primary addresses actually published to the VSync path.
        pub primary_published: u32,
        /// Bounded-state exhaustion: pending flips dropped because the FIFO was
        /// full. Practically unreachable; loud if it ever is not.
        pub overflow: u32,
    }

    /// Capacity of [`LeaseTracker`]'s model FIFO.
    ///
    /// The shipped driver's queue is `VecDeque<WddmPending>` with
    /// `MAX_WDDM_PENDING = 256`; the model uses a small fixed array so the
    /// exhaustion path is reachable in a test. What is under test is the RULE
    /// (head-of-line blocking, monotonic delivery, overflow clears the queue and
    /// releases every lease), not the number.
    pub const MODEL_FIFO_LEN: usize = 8;

    /// One pending WDDM submission in the model FIFO.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct PendingFlip {
        /// `DXGKARG_SUBMITCOMMAND::SubmissionFenceId`.
        pub fence: u32,
        /// Producer half: this submission's Venus completion watermark, already
        /// satisfied when `true`.
        pub producer_ready: bool,
        /// Display-consumer half: the presentation epoch whose host read must
        /// finish first, or [`NO_LEASE`].
        ///
        /// ⚠ RETIRED IN THE DRIVER (22.22.217.0): the shipped FIFO carries no
        /// lease any more. See the module's "what 22.22.217.0 retired" note.
        pub lease: u64,
    }

    /// What one attempt to retire the head of the FIFO produced.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum RetireStep {
        /// Nothing queued.
        Empty,
        /// The head's producer work has not retired yet.
        BlockedOnProducer,
        /// The head's producer work is done but the host has not finished
        /// reading the presentation named by `lease`.
        ///
        /// ⚠ RETIRED IN THE DRIVER (22.22.217.0) — `WddmTake` has no such arm.
        BlockedOnLease(u64),
        /// Deliver `DXGK_INTERRUPT_DMA_COMPLETED` for this fence.
        Ready(u32),
    }

    /// The executable specification of the ownership state machine.
    ///
    /// Single-threaded and lock-free by construction: the shipped driver splits
    /// these fields across three locks and reproduces each transition with a
    /// monotone atomic, calling the same free functions above.
    #[derive(Clone, Copy, Debug)]
    pub struct LeaseTracker {
        next_epoch: u64,
        bound_epoch: u64,
        bound_resource: u32,
        read_epoch: u64,
        /// The primary address armed at the bind, waiting for its epoch's lease
        /// to end before a CRTC_VSYNC may report it.
        pending_primary: Option<(u64, u64)>,
        /// The address the VSync heartbeat currently reports.
        displayed_primary: u64,
        fifo: [Option<PendingFlip>; MODEL_FIFO_LEN],
        fifo_len: usize,
        /// Last fence handed to Windows; the monotonicity check's left operand.
        last_completed_fence: u32,
        counters: LeaseCounters,
    }

    impl Default for LeaseTracker {
        fn default() -> Self {
            Self::new()
        }
    }

    impl LeaseTracker {
        pub const fn new() -> Self {
            Self {
                next_epoch: FIRST_EPOCH,
                bound_epoch: NO_LEASE,
                bound_resource: 0,
                read_epoch: NO_LEASE,
                pending_primary: None,
                displayed_primary: 0,
                fifo: [None; MODEL_FIFO_LEN],
                fifo_len: 0,
                last_completed_fence: 0,
                counters: LeaseCounters {
                    minted: 0,
                    read_queued: 0,
                    read_completed: 0,
                    ended_read: 0,
                    ended_superseded: 0,
                    ended_cancelled: 0,
                    ended_teardown: 0,
                    stale_completions: 0,
                    retire_blocked: 0,
                    retire_released: 0,
                    primary_published: 0,
                    overflow: 0,
                },
            }
        }

        pub const fn counters(&self) -> LeaseCounters {
            self.counters
        }

        pub const fn read_epoch(&self) -> u64 {
            self.read_epoch
        }

        pub const fn bound_epoch(&self) -> u64 {
            self.bound_epoch
        }

        pub const fn displayed_primary(&self) -> u64 {
            self.displayed_primary
        }

        pub const fn pending_len(&self) -> usize {
            self.fifo_len
        }

        /// Mint the epoch for one presentation. `DxgkDdiSubmitCommand`'s
        /// DMA-flip arm, before the flip's handle is published to the worker.
        pub fn mint_presentation(&mut self) -> u64 {
            let epoch = self.next_epoch;
            self.next_epoch = next_epoch(self.next_epoch);
            self.counters.minted = self.counters.minted.saturating_add(1);
            epoch
        }

        /// The display worker published `epoch`'s buffer to the host.
        ///
        /// `rebound` is true when this call issued a `SET_SCANOUT_BLOB` (the
        /// resource changed); false for a re-present of the already-bound
        /// buffer, which publishes nothing to the host but is still a new
        /// presentation that has to be read.
        pub fn publish_bind(&mut self, epoch: u64, resource: u32, rebound: bool) {
            if epoch == NO_LEASE {
                return;
            }
            if rebound && resource != self.bound_resource && self.bound_epoch != NO_LEASE {
                // Everything published before this bind can no longer be read.
                self.end_leases_through(self.bound_epoch, LeaseEnd::Superseded);
            }
            self.bound_epoch = merge_read_epoch(self.bound_epoch, epoch);
            self.bound_resource = resource;
        }

        /// Arm the primary address a later CRTC_VSYNC may report, gated on
        /// `epoch`'s lease. Publishes immediately when the lease has already
        /// ended.
        ///
        /// ⚠ RETIRED IN THE DRIVER (22.22.217.0): the bind publishes the
        /// address unconditionally now. Kept as the record of the withholding
        /// that was measured inert.
        pub fn arm_primary(&mut self, address: u64, epoch: u64) {
            if epoch == NO_LEASE || lease_satisfied(self.read_epoch, epoch) {
                self.displayed_primary = address;
                self.counters.primary_published = self.counters.primary_published.saturating_add(1);
                return;
            }
            self.pending_primary = Some((address, epoch));
        }

        /// Snapshot the epoch a `RESOURCE_FLUSH` issued now will prove was read.
        pub fn issue_flush(&mut self) -> u64 {
            self.counters.read_queued = self.counters.read_queued.saturating_add(1);
            self.bound_epoch
        }

        /// A flush token came back. `ok` is false for a host error response,
        /// which still terminates the command — it just does not mean the pixels
        /// were published.
        pub fn complete_flush(&mut self, covers: u64, ok: bool) {
            self.counters.read_completed = self.counters.read_completed.saturating_add(1);
            let reason = if ok {
                LeaseEnd::HostRead
            } else {
                LeaseEnd::Cancelled
            };
            if covers == NO_LEASE || covers <= self.read_epoch {
                self.counters.stale_completions = self.counters.stale_completions.saturating_add(1);
                return;
            }
            self.end_leases_through(covers, reason);
        }

        /// End every lease at or below `epoch`, for `reason`.
        pub fn end_leases_through(&mut self, epoch: u64, reason: LeaseEnd) {
            let merged = merge_read_epoch(self.read_epoch, epoch);
            if merged == self.read_epoch {
                return;
            }
            self.read_epoch = merged;
            let slot = match reason {
                LeaseEnd::HostRead => &mut self.counters.ended_read,
                LeaseEnd::Superseded => &mut self.counters.ended_superseded,
                LeaseEnd::Cancelled => &mut self.counters.ended_cancelled,
                LeaseEnd::Teardown => &mut self.counters.ended_teardown,
            };
            *slot = slot.saturating_add(1);
            self.publish_pending_primary();
        }

        /// End every lease that has ever been minted. Reset, StopDevice,
        /// transport failure, allocation retirement.
        pub fn release_all(&mut self, reason: LeaseEnd) {
            let highest = self.next_epoch.saturating_sub(1);
            self.end_leases_through(highest, reason);
        }

        fn publish_pending_primary(&mut self) {
            let Some((address, epoch)) = self.pending_primary else {
                return;
            };
            if !lease_satisfied(self.read_epoch, epoch) {
                return;
            }
            self.pending_primary = None;
            self.displayed_primary = address;
            self.counters.primary_published = self.counters.primary_published.saturating_add(1);
        }

        /// Queue one WDDM submission. Returns true when the caller must signal
        /// `DMA_COMPLETED` immediately (nothing gates it, or the FIFO overflowed
        /// and degraded to the immediate model).
        pub fn submit(&mut self, flip: PendingFlip) -> bool {
            if self.fifo_len == 0
                && flip.producer_ready
                && lease_satisfied(self.read_epoch, flip.lease)
            {
                self.counters.retire_released = self.counters.retire_released.saturating_add(1);
                self.last_completed_fence = flip.fence;
                return true;
            }
            if self.fifo_len == MODEL_FIFO_LEN {
                // Signalling the newest (monotonically largest) fence implicitly
                // completes the queued older ones, so drop them — and release
                // every lease with them, or the next presentation would be gated
                // on a read whose waiter no longer exists.
                self.counters.overflow = self.counters.overflow.saturating_add(1);
                self.fifo = [None; MODEL_FIFO_LEN];
                self.fifo_len = 0;
                self.release_all(LeaseEnd::Teardown);
                self.last_completed_fence = flip.fence;
                return true;
            }
            self.fifo[self.fifo_len] = Some(flip);
            self.fifo_len += 1;
            false
        }

        /// Try to retire the head of the FIFO. Strictly head-of-line: a blocked
        /// head is never bypassed, because `SubmissionFenceId`s are watermarks
        /// to dxgkrnl and must complete monotonically.
        pub fn try_retire(&mut self) -> RetireStep {
            let Some(head) = self.fifo[0] else {
                return RetireStep::Empty;
            };
            if !head.producer_ready {
                return RetireStep::BlockedOnProducer;
            }
            if !lease_satisfied(self.read_epoch, head.lease) {
                self.counters.retire_blocked = self.counters.retire_blocked.saturating_add(1);
                return RetireStep::BlockedOnLease(head.lease);
            }
            let mut i = 1;
            while i < self.fifo_len {
                self.fifo[i - 1] = self.fifo[i];
                i += 1;
            }
            self.fifo[self.fifo_len - 1] = None;
            self.fifo_len -= 1;
            self.counters.retire_released = self.counters.retire_released.saturating_add(1);
            if fence_is_forward(self.last_completed_fence, head.fence) {
                self.last_completed_fence = head.fence;
            }
            RetireStep::Ready(head.fence)
        }

        /// Mark the producer half of the submission carrying `fence` retired.
        pub fn producer_retired(&mut self, fence: u32) {
            let mut i = 0;
            while i < self.fifo_len {
                if let Some(entry) = self.fifo[i].as_mut() {
                    if entry.fence == fence {
                        entry.producer_ready = true;
                    }
                }
                i += 1;
            }
        }

        /// Drop every pending submission (preempt / ResetFromTimeout / reset)
        /// and end every lease with it.
        pub fn abandon(&mut self) -> usize {
            let dropped = self.fifo_len;
            self.fifo = [None; MODEL_FIFO_LEN];
            self.fifo_len = 0;
            self.release_all(LeaseEnd::Teardown);
            dropped
        }

        pub const fn last_completed_fence(&self) -> u32 {
            self.last_completed_fence
        }
    }
}

/// Identity-only cadence rule for the direct scanout marker path.
///
/// A PRESENT marker records the completion boundary for the resource the app
/// just rendered.  It may request a host read immediately only when that exact
/// resource is already bound, because only then can a `RESOURCE_FLUSH` name
/// the marked frame.  A marker for another resource is retained for that
/// resource's bind edge; treating it as a generic dirty edge can overwrite the
/// currently bound resource's pending refresh and strand the real bind edge.
pub mod scanout_cadence {
    /// What the PRESENT-marker edge may do after recording its exact frame
    /// watermark.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum PresentMarkerAction {
        /// The marker names the active resource, or is the intentionally
        /// identity-free HERF edge (`resource_id == 0`).
        QueueImmediate,
        /// A future buffer's marker. Its own bind edge owns scheduling.
        DeferToBind,
    }

    /// Classify one PRESENT marker from exact resource identity only.
    ///
    /// No handle order, frame number, timing, or buffer-count inference is
    /// admissible here: a nonzero marker may queue only when it names the
    /// resource the host is currently bound to.
    pub const fn present_marker_action(
        resource_id: u32,
        active_resource_id: u32,
    ) -> PresentMarkerAction {
        if resource_id == 0 || resource_id == active_resource_id {
            PresentMarkerAction::QueueImmediate
        } else {
            PresentMarkerAction::DeferToBind
        }
    }

    /// Tiny host-test model of the identity rule. The production driver owns
    /// the actual completion-watermark table; this model deliberately carries
    /// only identity, pending-edge, and bind ownership so cadence regressions
    /// can be tested without KMD atomics or transport state.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct State {
        active_resource_id: u32,
        pending_resource_id: u32,
        refresh_queued: bool,
        marked_resources: [u32; 3],
    }

    impl State {
        pub const fn new() -> Self {
            Self {
                active_resource_id: 0,
                pending_resource_id: 0,
                refresh_queued: false,
                marked_resources: [0; 3],
            }
        }

        /// Record one marker, queuing only when [`present_marker_action`]
        /// permits it. The fixed table is sufficient for the A/B/C cadence
        /// model and deliberately has no allocation.
        pub fn present(&mut self, resource_id: u32) -> PresentMarkerAction {
            if resource_id != 0 {
                self.record_marker(resource_id);
            }
            let action = present_marker_action(resource_id, self.active_resource_id);
            if action == PresentMarkerAction::QueueImmediate {
                self.queue(resource_id);
            }
            action
        }

        /// Publish a bind and let only that resource consume its recorded
        /// marker. Returns whether the bind produced a refresh edge.
        pub fn bind(&mut self, resource_id: u32) -> bool {
            self.active_resource_id = resource_id;
            if resource_id != 0 && self.take_marker(resource_id) {
                self.queue(resource_id);
                true
            } else {
                false
            }
        }

        pub const fn active_resource_id(self) -> u32 {
            self.active_resource_id
        }

        pub const fn pending_resource_id(self) -> u32 {
            self.pending_resource_id
        }

        pub const fn refresh_queued(self) -> bool {
            self.refresh_queued
        }

        fn queue(&mut self, resource_id: u32) {
            self.pending_resource_id = resource_id;
            self.refresh_queued = true;
        }

        fn record_marker(&mut self, resource_id: u32) {
            for slot in &mut self.marked_resources {
                if *slot == resource_id {
                    return;
                }
            }
            for slot in &mut self.marked_resources {
                if *slot == 0 {
                    *slot = resource_id;
                    return;
                }
            }
            // The model has exactly the three rotating scanout identities used
            // by its tests. Replace the oldest model slot when a fourth is
            // introduced, matching bounded production bookkeeping.
            self.marked_resources[0] = resource_id;
        }

        fn take_marker(&mut self, resource_id: u32) -> bool {
            for slot in &mut self.marked_resources {
                if *slot == resource_id {
                    *slot = 0;
                    return true;
                }
            }
            false
        }
    }

    impl Default for State {
        fn default() -> Self {
            Self::new()
        }
    }
}

/// Pure decision for the PASSIVE worker after `VirtioGpu` checked one exact
/// direct presentation's producer boundary.
///
/// The host and Windows primary both wait for a D4b snapshot's exact producer
/// boundary. The KMD maps its transport-facing dispatch into [`Dispatch`] and
/// consumes this result without making the testable rule depend on a WDK type.
pub mod scanout_worker_bind {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Dispatch {
        Ready,
        Waiting,
        Abandoned,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Action {
        Deferred,
        AlreadyBound,
        IssueSyncSet,
        RejectAbandoned,
    }

    /// Whether this worker's direct-primary epoch may publish a SET or a
    /// primary address.  This is deliberately separate from [`Dispatch`]: the
    /// producer boundary says when a request is ready; it cannot make an older
    /// or identity-conflicting request become the host selection again.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum EpochPolicy {
        /// A newer presentation owns the binding and heartbeat already.
        Superseded,
        /// The same epoch names a different active identity.  It is terminal:
        /// guessing which identity wins would rebind behind an already-applied
        /// host selection.
        EqualConflict,
        /// The exact epoch/identity is already bound; preserve the ordinary
        /// already-bound path without another SET.
        AlreadyBound,
        /// This is a newer (or untracked) presentation and may stage normally.
        Newer,
    }

    /// An already-bound target cannot bypass [`Dispatch::Waiting`]. Once it is
    /// ready, it avoids a duplicate synchronous SET; an unbound target owns
    /// that SET instead.
    pub const fn decide(already_bound: bool, dispatch: Dispatch) -> Action {
        match dispatch {
            Dispatch::Waiting => Action::Deferred,
            Dispatch::Abandoned => Action::RejectAbandoned,
            Dispatch::Ready if already_bound => Action::AlreadyBound,
            Dispatch::Ready => Action::IssueSyncSet,
        }
    }

    /// Decide the direct-primary epoch before staging a worker SET.  `tracked`
    /// is false for the MMIO/desktop path, whose epoch is intentionally
    /// `NO_LEASE`; it must retain its existing identity-only behavior.
    pub const fn decide_epoch(
        tracked: bool,
        present_epoch: u64,
        bound_epoch: u64,
        same_active_identity: bool,
    ) -> EpochPolicy {
        if !tracked || present_epoch == 0 || bound_epoch == 0 {
            return EpochPolicy::Newer;
        }
        if present_epoch < bound_epoch {
            EpochPolicy::Superseded
        } else if present_epoch == bound_epoch {
            if same_active_identity {
                EpochPolicy::AlreadyBound
            } else {
                EpochPolicy::EqualConflict
            }
        } else {
            EpochPolicy::Newer
        }
    }
}

/// Descriptor-acceptance admission for direct presentation epochs.
///
/// Producer readiness and host-reader retirement determine WHEN a request may
/// bind; this smaller rule determines whether a request can ever travel
/// backward once a newer presentation SET has been accepted by the control
/// queue.  The floor advances at descriptor acceptance, not at host response,
/// so host errors and explicit retire paths cannot reopen an old epoch.
pub mod scanout_presentation_epoch {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Admission {
        /// No direct presentation epoch is attached (fallback/SET(0)).
        Untracked,
        /// Strictly newer than the highest accepted presentation SET epoch.
        Newer,
        /// Equal to or older than the accepted floor; must never reach wire.
        Superseded,
    }

    /// Decide whether a presentation epoch may be admitted. `0` is the
    /// untracked sentinel used by adapter-owned fallback/disable SETs.
    pub const fn decide(floor: u64, present_epoch: u64) -> Admission {
        if present_epoch == 0 {
            Admission::Untracked
        } else if present_epoch > floor {
            Admission::Newer
        } else {
            Admission::Superseded
        }
    }

    /// Advance only for an already-accepted tracked presentation descriptor.
    /// This is monotonic even when the host later rejects that descriptor.
    pub const fn advance_after_accept(floor: u64, present_epoch: u64) -> u64 {
        if present_epoch > floor {
            present_epoch
        } else {
            floor
        }
    }
}

/// Fixed publication-transaction decisions for a host-visible scanout SET.
///
/// One SET is not terminal when it succeeds: the matching `RESOURCE_FLUSH`
/// is the host-reader completion that makes the next presentation SET safe.
/// The KMD stores the full `ScanoutBindRequest` beside this state; this module
/// keeps the scalar matching and phase rules independently testable.
pub mod scanout_publish_txn {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Key {
        pub resource_id: u32,
        pub present_epoch: u64,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Phase {
        /// The SET descriptor has reached the wire but has no terminal reply.
        SetInFlight,
        /// The SET succeeded and must be applied/armed before the flush exists.
        SetSucceeded,
        /// The exact resource/epoch `RESOURCE_FLUSH` is in flight.
        FlushInFlight,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Transaction {
        pub key: Key,
        pub seq: u64,
        pub phase: Phase,
    }

    /// A fixed, value-only transaction slot.  `None` is the only state that
    /// permits another presentation SET; a timeout deliberately leaves the
    /// transaction intact because the host can still complete that descriptor.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct State {
        active: Option<Transaction>,
    }

    impl State {
        pub const fn new() -> Self {
            Self { active: None }
        }

        pub const fn active(&self) -> Option<Transaction> {
            self.active
        }

        /// Claim exactly one accepted SET. Returns false rather than replacing
        /// an active transaction; replacing it would permit a later wire SET
        /// before the earlier host reader has terminated.
        pub fn claim(&mut self, key: Key, seq: u64) -> bool {
            if self.active.is_some() || seq == 0 {
                return false;
            }
            self.active = Some(Transaction {
                key,
                seq,
                phase: Phase::SetInFlight,
            });
            true
        }

        /// A terminal SET response. Error clears only this exact transaction;
        /// success remains active until the matching flush terminal response.
        pub fn complete_set(&mut self, key: Key, seq: u64, ok: bool) -> bool {
            let Some(mut transaction) = self.active else {
                return false;
            };
            if transaction.key != key || transaction.seq != seq {
                return false;
            }
            if ok {
                transaction.phase = Phase::SetSucceeded;
                self.active = Some(transaction);
            } else {
                self.active = None;
            }
            true
        }

        /// The application path successfully placed the exact flush on wire.
        pub fn arm_flush(&mut self, key: Key) -> bool {
            let Some(mut transaction) = self.active else {
                return false;
            };
            if transaction.key != key || transaction.phase != Phase::SetSucceeded {
                return false;
            }
            transaction.phase = Phase::FlushInFlight;
            self.active = Some(transaction);
            true
        }

        /// Undo an arm that happened before the descriptor was accepted.  No
        /// host read exists in this transition, so the exact SET remains ready
        /// for a later flush attempt.
        pub fn rollback_flush(&mut self, key: Key) -> bool {
            let Some(mut transaction) = self.active else {
                return false;
            };
            if transaction.key != key || transaction.phase != Phase::FlushInFlight {
                return false;
            }
            transaction.phase = Phase::SetSucceeded;
            self.active = Some(transaction);
            true
        }

        /// A flush response clears only the exact resource/epoch it covered.
        pub fn complete_flush(&mut self, key: Key) -> bool {
            let Some(transaction) = self.active else {
                return false;
            };
            if transaction.key != key || transaction.phase != Phase::FlushInFlight {
                return false;
            }
            self.active = None;
            true
        }

        /// Exact no-flush termination: stale apply, host-unbound/dead refresh,
        /// enqueue failure, cancellation, or a completed unbind barrier.
        pub fn cancel_exact(&mut self, key: Key) -> bool {
            if self
                .active
                .is_some_and(|transaction| transaction.key == key)
            {
                self.active = None;
                true
            } else {
                false
            }
        }
    }
}

/// Fixed-storage state for a completion-ordered fast scanout bind.
///
/// The producer-completion predicate is supplied by the KMD because it reads
/// transport state. This module owns the bounded coalescing and cancellation
/// rule: an unready request occupies an oldest liveness frontier plus one
/// coalesced latest slot, and a destroyed resource can never be promoted later.
pub mod scanout_fast_bind {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Request {
        pub resource_id: u32,
        pub boundary: u64,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Step {
        /// The exact producer boundary is ready, so issue this bind now.
        Issue(Request),
        /// Retained in bounded fixed storage for a later completion edge.
        Deferred,
        /// No deferred request is present, or its resource was cancelled.
        Empty,
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct State {
        earliest: Option<Request>,
        latest: Option<Request>,
    }

    impl State {
        pub const fn new() -> Self {
            Self {
                earliest: None,
                latest: None,
            }
        }

        /// Submit a request. `ready` and `live` must describe this exact
        /// request's producer boundary and resource identity.
        pub fn submit(&mut self, request: Request, ready: bool, live: bool) -> Step {
            if !live {
                return Step::Empty;
            }
            if ready {
                self.earliest = None;
                self.latest = None;
                Step::Issue(request)
            } else {
                if self.earliest.is_none() {
                    self.earliest = Some(request);
                } else {
                    self.latest = Some(request);
                }
                Step::Deferred
            }
        }

        /// Re-evaluate the retained request after a producer completion.
        pub fn on_completion(
            &mut self,
            earliest_ready: bool,
            earliest_live: bool,
            _latest_ready: bool,
            latest_live: bool,
        ) -> Step {
            let Some(earliest) = self.earliest else {
                return Step::Empty;
            };
            if !earliest_live {
                self.earliest = self.latest.take();
                Step::Empty
            } else if earliest_ready {
                let latest = self.latest.take();
                // A completion first advances the oldest liveness frontier.
                // Keep any live successor even when it is already ready: the
                // SET issued below creates the next completion pass, which can
                // then issue that exact successor without silently losing it.
                self.earliest = latest.filter(|_| latest_live);
                Step::Issue(earliest)
            } else {
                Step::Deferred
            }
        }

        /// Exact DestroyAllocation cancellation. A different resource must not
        /// erase the retained request merely because it happened to retire.
        pub fn cancel_resource(&mut self, resource_id: u32) -> bool {
            let mut cancelled = false;
            if self
                .earliest
                .is_some_and(|request| request.resource_id == resource_id)
            {
                self.earliest = self.latest.take();
                cancelled = true;
            }
            if self
                .latest
                .is_some_and(|request| request.resource_id == resource_id)
            {
                self.latest = None;
                cancelled = true;
            }
            cancelled
        }

        pub const fn deferred(self) -> Option<Request> {
            self.earliest
        }
    }
}

/// Pure, bounded state transitions for completion-ordered scanout refreshes.
///
/// The KMD supplies producer readiness and tagged-stream liveness because both
/// depend on transport state. This module owns only the state machine, so its
/// liveness and coalescing rules are executable in the host test crate.
pub mod scanout_refresh {
    /// One exact dirty edge. Resource identity and producer boundary are an
    /// inseparable pair: coalescing must never attach one resource to another
    /// frame's completion.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Marker {
        resource_id: u32,
        boundary: u64,
    }

    impl Marker {
        pub const fn new(resource_id: u32, boundary: u64) -> Self {
            Self {
                resource_id,
                boundary,
            }
        }

        pub const fn resource_id(self) -> u32 {
            self.resource_id
        }

        pub const fn boundary(self) -> u64 {
            self.boundary
        }
    }

    /// Two fixed marker slots: an oldest liveness frontier and one coalesced
    /// latest marker. This is deliberately not an unbounded queue and contains
    /// no allocation or synchronization.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct State {
        earliest: Option<Marker>,
        latest: Option<Marker>,
    }

    impl State {
        pub const fn new() -> Self {
            Self {
                earliest: None,
                latest: None,
            }
        }

        pub fn clear(&mut self) {
            self.earliest = None;
            self.latest = None;
        }

        /// Record one exact dirty edge. An already-ready current marker is an
        /// immediate refresh request and supersedes outstanding deferred work;
        /// retaining the older frontier would strand the ready edge if the
        /// used-ring becomes idle.
        pub fn note(&mut self, marker: Marker, ready: bool) -> bool {
            if ready {
                self.clear();
                true
            } else {
                self.note_pending(marker);
                false
            }
        }

        /// Record an unready marker without testing transport readiness.
        pub fn note_pending(&mut self, marker: Marker) {
            if self.earliest.is_none() {
                self.earliest = Some(marker);
            } else {
                self.latest = Some(marker);
            }
        }

        pub const fn earliest(self) -> Option<Marker> {
            self.earliest
        }

        pub const fn latest(self) -> Option<Marker> {
            self.latest
        }

        /// Consume one read-safe marker. If the coalesced latest marker is
        /// ready too it supersedes the frontier; otherwise the frontier is
        /// returned and the latest becomes the next frontier.
        pub fn take_ready(&mut self, earliest_ready: bool, latest_ready: bool) -> Option<Marker> {
            let earliest = self.earliest?;
            if !earliest_ready {
                return None;
            }
            match self.latest {
                Some(latest) if latest_ready => {
                    self.clear();
                    Some(latest)
                }
                Some(latest) => {
                    self.earliest = Some(latest);
                    self.latest = None;
                    Some(earliest)
                }
                None => {
                    self.earliest = None;
                    Some(earliest)
                }
            }
        }

        /// Drop explicitly cancelled markers, then promote a live latest
        /// marker only after both slots have been filtered. `marker_live` must
        /// return true for ordinary wire boundaries and for live tagged streams.
        pub fn discard_cancelled<F>(&mut self, marker_live: F)
        where
            F: Fn(Marker) -> bool,
        {
            let earliest = self.earliest.filter(|marker| marker_live(*marker));
            let latest = self.latest.filter(|marker| marker_live(*marker));
            if earliest.is_some() {
                self.earliest = earliest;
                self.latest = latest;
            } else {
                self.earliest = latest;
                self.latest = None;
            }
        }
    }

    impl Default for State {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(test)]
mod scanout_cadence_tests {
    use super::scanout_cadence::{PresentMarkerAction, State};

    #[test]
    fn present_before_bind_defers_exact_future_resource_to_its_bind() {
        let mut state = State::new();
        assert!(!state.bind(10));

        assert_eq!(state.present(20), PresentMarkerAction::DeferToBind);
        assert!(!state.refresh_queued());

        assert!(state.bind(20));
        assert_eq!(state.pending_resource_id(), 20);
    }

    #[test]
    fn bind_before_present_queues_the_now_active_resource() {
        let mut state = State::new();
        assert!(!state.bind(10));

        assert_eq!(state.present(10), PresentMarkerAction::QueueImmediate);
        assert!(state.refresh_queued());
        assert_eq!(state.pending_resource_id(), 10);
    }

    #[test]
    fn already_bound_represent_is_an_exact_immediate_refresh() {
        let mut state = State::new();
        assert!(!state.bind(10));
        assert_eq!(state.present(10), PresentMarkerAction::QueueImmediate);
        assert_eq!(state.active_resource_id(), 10);
        assert_eq!(state.pending_resource_id(), 10);
    }

    #[test]
    fn identity_free_herf_queues_whatever_is_bound() {
        let mut state = State::new();
        assert!(!state.bind(10));

        assert_eq!(state.present(0), PresentMarkerAction::QueueImmediate);
        assert!(state.refresh_queued());
        assert_eq!(state.pending_resource_id(), 0);
    }

    #[test]
    fn rotating_abc_markers_cannot_starve_each_resources_bind_edge() {
        let mut state = State::new();
        assert!(!state.bind(1));

        // B and C are future buffers. Neither may overwrite A's current
        // refresh, and each remains available to exactly its own bind.
        assert_eq!(state.present(2), PresentMarkerAction::DeferToBind);
        assert_eq!(state.present(3), PresentMarkerAction::DeferToBind);
        assert_eq!(state.present(1), PresentMarkerAction::QueueImmediate);
        assert_eq!(state.pending_resource_id(), 1);

        assert!(state.bind(2));
        assert_eq!(state.pending_resource_id(), 2);
        assert!(state.bind(3));
        assert_eq!(state.pending_resource_id(), 3);
    }
}

#[cfg(test)]
mod scanout_worker_bind_tests {
    use super::scanout_worker_bind::{decide, decide_epoch, Action, Dispatch, EpochPolicy};

    #[test]
    fn already_bound_target_still_defers_while_its_producer_is_unready() {
        assert_eq!(decide(true, Dispatch::Waiting), Action::Deferred);
    }

    #[test]
    fn ready_already_bound_target_does_not_issue_a_duplicate_sync_set() {
        assert_eq!(decide(true, Dispatch::Ready), Action::AlreadyBound);
    }

    #[test]
    fn ready_unbound_target_owns_the_sync_set() {
        assert_eq!(decide(false, Dispatch::Ready), Action::IssueSyncSet);
    }

    #[test]
    fn abandoned_producer_stays_terminal() {
        assert_eq!(decide(true, Dispatch::Abandoned), Action::RejectAbandoned);
    }

    #[test]
    fn older_worker_epoch_is_terminal_and_cannot_rebind() {
        assert_eq!(decide_epoch(true, 41, 42, false), EpochPolicy::Superseded);
    }

    #[test]
    fn equal_epoch_with_conflicting_identity_is_terminal() {
        assert_eq!(
            decide_epoch(true, 42, 42, false),
            EpochPolicy::EqualConflict
        );
    }

    #[test]
    fn equal_epoch_with_same_identity_uses_already_bound_path() {
        assert_eq!(decide_epoch(true, 42, 42, true), EpochPolicy::AlreadyBound);
    }

    #[test]
    fn newer_worker_epoch_may_stage_normally() {
        assert_eq!(decide_epoch(true, 43, 42, false), EpochPolicy::Newer);
    }
}

#[cfg(test)]
mod scanout_presentation_epoch_tests {
    use super::scanout_presentation_epoch::{advance_after_accept, decide, Admission};

    #[test]
    fn newer_epoch_is_admitted_and_advances_only_after_acceptance() {
        assert_eq!(decide(1056, 1057), Admission::Newer);
        assert_eq!(advance_after_accept(1056, 1057), 1057);
    }

    #[test]
    fn equal_or_older_epoch_is_terminally_superseded() {
        assert_eq!(decide(1056, 1056), Admission::Superseded);
        assert_eq!(decide(1056, 1055), Admission::Superseded);
        assert_eq!(advance_after_accept(1056, 1055), 1056);
    }

    #[test]
    fn accepted_1056_rejects_later_deferred_1055() {
        let floor = advance_after_accept(0, 1056);
        assert_eq!(floor, 1056);
        assert_eq!(decide(floor, 1055), Admission::Superseded);
    }

    #[test]
    fn untracked_fallback_does_not_participate_in_the_floor() {
        assert_eq!(decide(1056, 0), Admission::Untracked);
        assert_eq!(advance_after_accept(1056, 0), 1056);
    }
}

#[cfg(test)]
mod scanout_publish_txn_tests {
    use super::scanout_publish_txn::{Key, Phase, State, Transaction};

    const A: Key = Key {
        resource_id: 0x121,
        present_epoch: 41,
    };
    const B: Key = Key {
        resource_id: 0x128,
        present_epoch: 42,
    };

    #[test]
    fn active_transaction_blocks_a_ready_second_set() {
        let mut state = State::new();
        assert!(state.claim(A, 7));
        assert!(!state.claim(B, 8));
        assert_eq!(
            state.active(),
            Some(Transaction {
                key: A,
                seq: 7,
                phase: Phase::SetInFlight,
            })
        );
    }

    #[test]
    fn exact_flush_completion_clears_the_transaction() {
        let mut state = State::new();
        assert!(state.claim(A, 7));
        assert!(state.complete_set(A, 7, true));
        assert!(state.arm_flush(A));
        assert!(state.complete_flush(A));
        assert_eq!(state.active(), None);
    }

    #[test]
    fn wrong_resource_or_epoch_cannot_clear_the_transaction() {
        let mut state = State::new();
        assert!(state.claim(A, 7));
        assert!(state.complete_set(A, 7, true));
        assert!(state.arm_flush(A));
        assert!(!state.complete_flush(Key {
            resource_id: B.resource_id,
            present_epoch: A.present_epoch,
        }));
        assert!(!state.complete_flush(Key {
            resource_id: A.resource_id,
            present_epoch: B.present_epoch,
        }));
        assert_eq!(
            state.active(),
            Some(Transaction {
                key: A,
                seq: 7,
                phase: Phase::FlushInFlight,
            })
        );
    }

    #[test]
    fn set_error_is_an_exact_terminal_path() {
        let mut state = State::new();
        assert!(state.claim(A, 7));
        assert!(state.complete_set(A, 7, false));
        assert_eq!(state.active(), None);
    }

    #[test]
    fn timeout_keeps_late_success_transaction_until_its_flush() {
        let mut state = State::new();
        assert!(state.claim(A, 7));
        // Timeout detaches only the caller. It is intentionally no transition.
        assert_eq!(
            state.active(),
            Some(Transaction {
                key: A,
                seq: 7,
                phase: Phase::SetInFlight,
            })
        );
        assert!(state.complete_set(A, 7, true));
        assert!(state.arm_flush(A));
        assert!(state.complete_flush(A));
        assert_eq!(state.active(), None);
    }

    #[test]
    fn prepublication_flush_refusal_rolls_back_exactly() {
        let mut state = State::new();
        assert!(state.claim(A, 7));
        assert!(state.complete_set(A, 7, true));
        assert!(state.arm_flush(A));
        assert!(state.rollback_flush(A));
        assert_eq!(
            state.active(),
            Some(Transaction {
                key: A,
                seq: 7,
                phase: Phase::SetSucceeded,
            })
        );
        assert!(!state.rollback_flush(A));
        assert!(state.arm_flush(A));
        assert!(state.complete_flush(A));
    }
}

#[cfg(test)]
mod scanout_fast_bind_tests {
    use super::scanout_fast_bind::{Request, State, Step};

    const A: Request = Request {
        resource_id: 10,
        boundary: 100,
    };
    const B: Request = Request {
        resource_id: 20,
        boundary: 200,
    };

    #[test]
    fn unready_bind_waits_for_its_exact_producer_completion() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        assert_eq!(
            state.on_completion(false, true, false, true),
            Step::Deferred
        );
        assert_eq!(state.on_completion(true, true, false, true), Step::Issue(A));
        assert_eq!(state.deferred(), None);
    }

    #[test]
    fn already_retired_boundary_issues_without_consuming_storage() {
        let mut state = State::new();
        assert_eq!(state.submit(A, true, true), Step::Issue(A));
        assert_eq!(state.deferred(), None);
    }

    #[test]
    fn ready_successor_waits_behind_the_oldest_frontier() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        assert_eq!(state.submit(B, false, true), Step::Deferred);
        assert_eq!(state.deferred(), Some(A));
        assert_eq!(state.on_completion(true, true, true, true), Step::Issue(A));
        assert_eq!(state.deferred(), Some(B));
        // The SET for A causes the next completion pass. B was already ready,
        // but it remains exact and is issued there rather than being discarded.
        assert_eq!(state.on_completion(true, true, true, true), Step::Issue(B));
        assert_eq!(state.deferred(), None);
    }

    #[test]
    fn continuous_submits_cannot_starve_the_oldest_producer() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        // Every newer frame arrives before A is ready; only the trailing slot
        // changes, so A remains a liveness frontier.
        for boundary in [201, 202, 203, 204] {
            assert_eq!(
                state.submit(
                    Request {
                        resource_id: 20,
                        boundary,
                    },
                    false,
                    true,
                ),
                Step::Deferred
            );
            assert_eq!(state.deferred(), Some(A));
        }
        // The oldest completion always progresses first. The bounded trailing
        // slot remains the newest coalesced request and advances on the SET's
        // following completion pass.
        assert_eq!(state.on_completion(true, true, true, true), Step::Issue(A));
        let newest = Request {
            resource_id: 20,
            boundary: 204,
        };
        assert_eq!(state.deferred(), Some(newest));
        assert_eq!(
            state.on_completion(true, true, true, true),
            Step::Issue(newest)
        );
        assert_eq!(state.deferred(), None);
    }

    #[test]
    fn ready_frontier_does_not_issue_an_unready_latest() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        assert_eq!(state.submit(B, false, true), Step::Deferred);
        assert_eq!(state.on_completion(true, true, false, true), Step::Issue(A));
        assert_eq!(state.deferred(), Some(B));
    }

    #[test]
    fn ready_submit_explicitly_supersedes_retained_frontier() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        // A currently-ready request is an explicit newer selection, distinct
        // from completion service. The production path accounts for this
        // supersession in its timeline/counter before issuing B.
        assert_eq!(state.submit(B, true, true), Step::Issue(B));
        assert_eq!(state.deferred(), None);
    }

    #[test]
    fn cancelled_successor_cannot_issue_after_oldest_progresses() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        assert_eq!(state.submit(B, false, true), Step::Deferred);
        assert_eq!(state.on_completion(true, true, true, true), Step::Issue(A));
        assert_eq!(state.deferred(), Some(B));
        assert!(state.cancel_resource(B.resource_id));
        assert_eq!(state.on_completion(true, true, true, true), Step::Empty);
    }

    #[test]
    fn exact_destroy_cancels_before_a_late_completion_can_issue() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        assert!(!state.cancel_resource(B.resource_id));
        assert!(state.cancel_resource(A.resource_id));
        assert_eq!(state.on_completion(true, true, true, true), Step::Empty);
    }

    #[test]
    fn dead_producer_is_dropped_not_reinterpreted_as_ready() {
        let mut state = State::new();
        assert_eq!(state.submit(A, false, true), Step::Deferred);
        assert_eq!(state.on_completion(false, false, false, false), Step::Empty);
        assert_eq!(state.deferred(), None);
    }
}

#[cfg(test)]
mod scanout_refresh_tests {
    use super::scanout_refresh::{Marker, State};

    #[test]
    fn frontier_is_not_overwritten_by_a_hot_present_stream() {
        let mut state = State::new();
        state.note_pending(Marker::new(11, 100));
        state.note_pending(Marker::new(22, 200));
        state.note_pending(Marker::new(33, 300));

        assert_eq!(state.earliest(), Some(Marker::new(11, 100)));
        assert_eq!(state.latest(), Some(Marker::new(33, 300)));
        assert_eq!(state.take_ready(false, false), None);
    }

    #[test]
    fn refused_old_frontier_cannot_strand_promoted_newer_marker() {
        let mut state = State::new();
        state.note_pending(Marker::new(11, 100));
        state.note_pending(Marker::new(22, 200));

        assert_eq!(state.take_ready(true, false), Some(Marker::new(11, 100)));
        assert_eq!(state.earliest(), Some(Marker::new(22, 200)));
        assert_eq!(state.latest(), None);
        // The KMD executor may reject 11 after a different accepted bind. Its
        // refusal cannot erase the promoted marker; a later DPC still returns
        // the exact resource/boundary pair for 22.
        assert_eq!(state.take_ready(true, false), Some(Marker::new(22, 200)));
        assert_eq!(state.earliest(), None);
    }

    #[test]
    fn ready_latest_supersedes_frontier_without_crossing_identity() {
        let mut state = State::new();
        state.note_pending(Marker::new(11, 100));
        state.note_pending(Marker::new(22, 200));

        assert_eq!(state.take_ready(true, true), Some(Marker::new(22, 200)));
        assert_eq!(state.earliest(), None);
        assert_eq!(state.latest(), None);
    }

    #[test]
    fn ready_current_marker_supersedes_waiting_frontier_without_a_future_dpc() {
        let mut state = State::new();
        state.note_pending(Marker::new(11, 100));

        assert!(state.note(Marker::new(22, 200), true));
        assert_eq!(state.earliest(), None);
        assert_eq!(state.latest(), None);
    }

    #[test]
    fn bind_boundary_precedes_same_resource_represent_boundary() {
        let mut state = State::new();

        // The KMD publishes an accepted bind and inserts its carried W1 under
        // the same notification lock that a re-present uses to classify the
        // resource and insert W2.  W1 must therefore remain the frontier and
        // W2 the coalesced successor, even when both name the same resource.
        assert!(!state.note(Marker::new(11, 100), false));
        assert!(!state.note(Marker::new(11, 200), false));

        assert_eq!(state.take_ready(true, false), Some(Marker::new(11, 100)));
        assert_eq!(state.earliest(), Some(Marker::new(11, 200)));
        assert_eq!(state.latest(), None);
    }

    #[test]
    fn cancelled_frontier_promotes_live_latest() {
        let mut state = State::new();
        state.note_pending(Marker::new(11, 100));
        state.note_pending(Marker::new(22, 200));

        state.discard_cancelled(|marker| marker.boundary() == 200);
        assert_eq!(state.earliest(), Some(Marker::new(22, 200)));
        assert_eq!(state.latest(), None);
    }

    #[test]
    fn two_cancelled_markers_leave_no_frontier() {
        let mut state = State::new();
        state.note_pending(Marker::new(11, 100));
        state.note_pending(Marker::new(22, 200));

        state.discard_cancelled(|_| false);
        assert_eq!(state.earliest(), None);
        assert_eq!(state.latest(), None);
    }
}

#[cfg(test)]
mod scanout_lease_tests {
    use super::scanout_lease::*;

    /// Drive one steady-state presentation: mint, bind, flush, response.
    fn present(tracker: &mut LeaseTracker, resource: u32, rebound: bool, fence: u32) -> u64 {
        let epoch = tracker.mint_presentation();
        tracker.submit(PendingFlip {
            fence,
            producer_ready: false,
            lease: epoch,
        });
        tracker.publish_bind(epoch, resource, rebound);
        epoch
    }

    // 1. normal bind -> flush queued -> response -> reuse
    #[test]
    fn normal_bind_flush_response_releases_the_flip() {
        let mut t = LeaseTracker::new();
        let epoch = present(&mut t, 191, true, 10);
        t.producer_retired(10);
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(epoch));

        let token = t.issue_flush();
        assert_eq!(token, epoch);
        t.complete_flush(token, true);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
        assert_eq!(t.counters().ended_read, 1);
        assert_eq!(t.counters().ended_superseded, 0);
    }

    // 2. producer completion before reader completion
    #[test]
    fn producer_first_still_waits_for_the_reader() {
        let mut t = LeaseTracker::new();
        let epoch = present(&mut t, 191, true, 10);
        t.producer_retired(10);
        for _ in 0..4 {
            assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(epoch));
        }
        let token = t.issue_flush();
        t.complete_flush(token, true);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
        assert_eq!(t.counters().retire_blocked, 4);
    }

    // 3. reader completion before producer completion
    #[test]
    fn reader_first_still_waits_for_the_producer() {
        let mut t = LeaseTracker::new();
        present(&mut t, 191, true, 10);
        let token = t.issue_flush();
        t.complete_flush(token, true);
        assert_eq!(t.try_retire(), RetireStep::BlockedOnProducer);
        t.producer_retired(10);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
    }

    // 4. later bind supersedes an epoch that never got a read
    #[test]
    fn a_later_bind_supersedes_an_unread_epoch() {
        let mut t = LeaseTracker::new();
        let first = present(&mut t, 191, true, 10);
        t.producer_retired(10);
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(first));

        // No flush ever named `first`; the next flip binds the other buffer.
        let second = present(&mut t, 195, true, 11);
        t.producer_retired(11);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(second));
        assert_eq!(t.counters().ended_superseded, 1);
        assert_eq!(t.counters().ended_read, 0);
    }

    // 5. later bind after a read was already queued for the previous epoch
    #[test]
    fn a_queued_read_and_a_later_bind_agree() {
        let mut t = LeaseTracker::new();
        let first = present(&mut t, 191, true, 10);
        let token = t.issue_flush();
        assert_eq!(token, first);

        let second = present(&mut t, 195, true, 11);
        // The bind superseded `first` before its response came back...
        assert!(lease_satisfied(t.read_epoch(), first));
        // ...and the late response is then inert, not a backwards step.
        t.complete_flush(token, true);
        assert_eq!(t.counters().stale_completions, 1);
        assert!(!lease_satisfied(t.read_epoch(), second));
    }

    // 6. repeated presentations of the same still-bound resource
    #[test]
    fn repeated_presents_of_one_buffer_get_distinct_epochs() {
        let mut t = LeaseTracker::new();
        let a = present(&mut t, 191, true, 10);
        let b = present(&mut t, 191, false, 11);
        let c = present(&mut t, 191, false, 12);
        assert!(a < b && b < c);
        t.producer_retired(10);
        t.producer_retired(11);
        t.producer_retired(12);

        // A read taken while `c` is bound covers all three; a read taken when
        // only `a` had been published covers only `a`.
        let mut t2 = LeaseTracker::new();
        let a2 = present(&mut t2, 191, true, 10);
        let token = t2.issue_flush();
        let b2 = present(&mut t2, 191, false, 11);
        t2.producer_retired(10);
        t2.producer_retired(11);
        t2.complete_flush(token, true);
        assert_eq!(t2.try_retire(), RetireStep::Ready(10));
        assert_eq!(t2.try_retire(), RetireStep::BlockedOnLease(b2));
        assert!(a2 < b2);

        // No supersede happened: the resource never changed.
        assert_eq!(t2.counters().ended_superseded, 0);
    }

    // 7. two resources alternating faster than the host reads
    #[test]
    fn two_resources_alternating_never_release_an_unread_epoch() {
        let mut t = LeaseTracker::new();
        let mut fence = 100u32;
        let mut leases = [0u64; 6];
        for (i, lease) in leases.iter_mut().enumerate() {
            let resource = if i % 2 == 0 { 191 } else { 195 };
            *lease = present(&mut t, resource, true, fence);
            t.producer_retired(fence);
            fence += 1;
        }
        // Every retire is either delivered because a later bind proved no read
        // remains, or blocked. Nothing is delivered while its own epoch is both
        // unread and still the bound one.
        let mut delivered = 0;
        loop {
            match t.try_retire() {
                RetireStep::Ready(_) => delivered += 1,
                _ => break,
            }
        }
        // The last presentation is still bound and unread, so it must be held.
        assert_eq!(delivered, leases.len() - 1);
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(leases[5]));
        assert!(!lease_satisfied(t.read_epoch(), leases[5]));
    }

    // 8. coalesced refreshes
    #[test]
    fn one_read_covers_every_epoch_published_before_it() {
        let mut t = LeaseTracker::new();
        let e1 = present(&mut t, 191, true, 10);
        let e2 = present(&mut t, 191, false, 11);
        let e3 = present(&mut t, 191, false, 12);
        t.producer_retired(10);
        t.producer_retired(11);
        t.producer_retired(12);
        let token = t.issue_flush();
        assert_eq!(token, e3);
        t.complete_flush(token, true);
        assert!(lease_satisfied(t.read_epoch(), e1));
        assert!(lease_satisfied(t.read_epoch(), e2));
        assert!(lease_satisfied(t.read_epoch(), e3));
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
        assert_eq!(t.try_retire(), RetireStep::Ready(11));
        assert_eq!(t.try_retire(), RetireStep::Ready(12));
        assert_eq!(t.counters().read_queued, 1);
    }

    // 9. stale completion token
    #[test]
    fn a_stale_token_never_moves_the_watermark_backwards() {
        let mut t = LeaseTracker::new();
        let e1 = present(&mut t, 191, true, 10);
        let stale = t.issue_flush();
        let e2 = present(&mut t, 195, true, 11);
        t.publish_bind(e2, 195, true);
        let fresh = t.issue_flush();
        t.complete_flush(fresh, true);
        let after = t.read_epoch();
        t.complete_flush(stale, true);
        assert_eq!(t.read_epoch(), after);
        assert!(lease_satisfied(t.read_epoch(), e1));
        assert_eq!(t.counters().stale_completions, 1);
        // Zero is never a valid token either.
        t.complete_flush(NO_LEASE, true);
        assert_eq!(t.read_epoch(), after);
        assert_eq!(t.counters().stale_completions, 2);
    }

    // 10. response error and enqueue failure
    #[test]
    fn an_error_response_terminates_the_lease_without_claiming_a_publish() {
        let mut t = LeaseTracker::new();
        let epoch = present(&mut t, 191, true, 10);
        t.producer_retired(10);
        let token = t.issue_flush();
        t.complete_flush(token, false);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
        assert_eq!(t.counters().ended_cancelled, 1);
        assert_eq!(t.counters().ended_read, 0);
        assert!(lease_satisfied(t.read_epoch(), epoch));
    }

    #[test]
    fn an_enqueue_failure_cancels_exactly_the_epoch_it_named() {
        let mut t = LeaseTracker::new();
        let first = present(&mut t, 191, true, 10);
        t.producer_retired(10);
        // The flush could not be enqueued: no host read exists for `first`.
        let token = t.issue_flush();
        t.end_leases_through(token, LeaseEnd::Cancelled);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));

        let second = present(&mut t, 195, true, 11);
        t.producer_retired(11);
        // The cancellation released `first` and NOTHING beyond it.
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(second));
        assert!(first < second);
        assert_eq!(t.counters().ended_cancelled, 1);
    }

    // 11. reset / teardown with outstanding leases
    #[test]
    fn teardown_releases_every_outstanding_lease() {
        let mut t = LeaseTracker::new();
        present(&mut t, 191, true, 10);
        present(&mut t, 195, true, 11);
        t.producer_retired(10);
        t.producer_retired(11);
        let dropped = t.abandon();
        assert_eq!(dropped, 2);
        assert_eq!(t.try_retire(), RetireStep::Empty);

        // A fresh presentation after teardown is gated again, not pre-released.
        let epoch = present(&mut t, 191, true, 12);
        t.producer_retired(12);
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(epoch));
        assert_eq!(t.counters().ended_teardown, 1);
    }

    // 12. WDDM FIFO monotonicity and fence wrap
    #[test]
    fn a_blocked_head_is_never_bypassed() {
        let mut t = LeaseTracker::new();
        let first = present(&mut t, 191, true, 10);
        let second = present(&mut t, 191, false, 11);
        t.producer_retired(11);
        // The younger fence is fully ready, but the head is not: no bypass.
        assert_eq!(t.try_retire(), RetireStep::BlockedOnProducer);
        t.producer_retired(10);
        // Still the HEAD's lease that is reported, not the ready younger one.
        assert_eq!(t.try_retire(), RetireStep::BlockedOnLease(first));
        assert!(first < second);
        assert_eq!(t.last_completed_fence(), 0);

        // Releasing the head's lease releases both, in submission order.
        let token = t.issue_flush();
        t.complete_flush(token, true);
        assert_eq!(t.try_retire(), RetireStep::Ready(10));
        assert_eq!(t.try_retire(), RetireStep::Ready(11));
        assert_eq!(t.last_completed_fence(), 11);
    }

    #[test]
    fn fence_forwardness_survives_the_u32_wrap() {
        assert!(fence_is_forward(0, 1));
        assert!(!fence_is_forward(1, 1));
        assert!(!fence_is_forward(2, 1));
        // Wrap: 0xFFFF_FFFF -> 0 is forward by one.
        assert!(fence_is_forward(u32::MAX, 0));
        assert!(fence_is_forward(u32::MAX - 1, 3));
        // Half the space away is NOT forward.
        assert!(!fence_is_forward(0, 0x8000_0000));
        assert!(fence_is_forward(0, 0x7FFF_FFFF));
    }

    #[test]
    fn epoch_minting_saturates_instead_of_wrapping_to_no_lease() {
        assert_eq!(next_epoch(0), FIRST_EPOCH);
        assert_eq!(next_epoch(u64::MAX - 1), u64::MAX);
        assert_eq!(next_epoch(u64::MAX), u64::MAX);
        assert!(!lease_satisfied(u64::MAX - 1, u64::MAX));
        assert!(lease_satisfied(u64::MAX, u64::MAX));
    }

    // 13. bounded-state exhaustion
    #[test]
    fn fifo_exhaustion_degrades_loudly_and_releases_every_lease() {
        let mut t = LeaseTracker::new();
        let mut fence = 200u32;
        for _ in 0..MODEL_FIFO_LEN {
            present(&mut t, 191, false, fence);
            fence += 1;
        }
        assert_eq!(t.pending_len(), MODEL_FIFO_LEN);
        // One more overflows.
        let epoch = t.mint_presentation();
        let signal_now = t.submit(PendingFlip {
            fence,
            producer_ready: false,
            lease: epoch,
        });
        assert!(signal_now);
        assert_eq!(t.pending_len(), 0);
        assert_eq!(t.counters().overflow, 1);
        assert!(lease_satisfied(t.read_epoch(), epoch));
        assert_eq!(t.try_retire(), RetireStep::Empty);
    }

    // The CRTC_VSYNC edge: the address a VSync may report is gated by the same
    // lease that gates DMA_COMPLETED.
    #[test]
    fn the_displayed_primary_address_waits_for_the_same_lease() {
        let mut t = LeaseTracker::new();
        let epoch = present(&mut t, 191, true, 10);
        t.arm_primary(0xDEAD_0000, epoch);
        assert_eq!(t.displayed_primary(), 0);

        let token = t.issue_flush();
        t.complete_flush(token, true);
        assert_eq!(t.displayed_primary(), 0xDEAD_0000);
        assert_eq!(t.counters().primary_published, 1);
    }

    // ── The ownership gate (D2). These are the SHIPPED decisions: the driver's
    // flush executor calls `surplus_republish` with its three atomics, so a
    // regression here is a black frame or a frozen desktop, not a model bug.

    #[test]
    fn the_first_read_of_a_binding_is_never_surplus() {
        // Bound generation 7 published, nothing read yet.
        assert!(!surplus_republish(true, 6, 7, 7));
        assert!(!surplus_republish(true, 6, 7, 9));
        assert!(!surplus_republish(true, NO_LEASE, 1, 1));
    }

    #[test]
    fn a_reread_with_a_newer_presentation_outstanding_is_surplus() {
        // Generation 7 was read (read >= bound) and flip 8 has been minted:
        // publishing 7 again cannot show 8, and 7 may already be reclaimed.
        assert!(surplus_republish(true, 7, 7, 8));
        assert!(surplus_republish(true, 9, 7, 8));
    }

    #[test]
    fn an_idle_desktop_republish_is_never_surplus() {
        // Nothing newer was presented: this flush IS the freshness edge.
        assert!(!surplus_republish(true, 7, 7, 7));
        assert!(!surplus_republish(true, 9, 7, 7));
    }

    #[test]
    fn a_same_buffer_represent_passes_because_it_publishes_a_fresh_epoch() {
        let mut t = LeaseTracker::new();
        let first = present(&mut t, 191, true, 10);
        let token = t.issue_flush();
        t.complete_flush(token, true);
        // Read caught up with the binding, nothing newer minted: passes.
        assert!(!surplus_republish(
            true,
            t.read_epoch(),
            t.bound_epoch(),
            first
        ));
        // DWM re-presents the SAME buffer: the epoch is minted AND published,
        // so the next marker's flush is a first read again.
        let second = present(&mut t, 191, false, 11);
        assert_eq!(t.bound_epoch(), second);
        assert!(!surplus_republish(
            true,
            t.read_epoch(),
            t.bound_epoch(),
            second
        ));
    }

    /// The MMIO/desktop contract mints no presentations, so `present_epoch` is
    /// frozen at whatever the last DMA-flip app left behind. Without `tracked`
    /// the gate would then drop every desktop refresh forever — a frozen
    /// desktop, which is defect 0aa.
    #[test]
    fn an_untracked_binding_disables_the_gate_entirely() {
        assert!(!surplus_republish(false, 7, 7, 8));
        assert!(!surplus_republish(false, u64::MAX, 1, u64::MAX));
        // A binding that never published an epoch cannot be gated either.
        assert!(!surplus_republish(true, 7, NO_LEASE, 8));
    }

    #[test]
    fn an_ungated_primary_publishes_immediately() {
        let mut t = LeaseTracker::new();
        t.arm_primary(0xBEEF_0000, NO_LEASE);
        assert_eq!(t.displayed_primary(), 0xBEEF_0000);
    }

    #[test]
    fn a_superseded_epoch_still_publishes_its_address() {
        let mut t = LeaseTracker::new();
        let first = present(&mut t, 191, true, 10);
        t.arm_primary(0x1000, first);
        assert_eq!(t.displayed_primary(), 0);
        present(&mut t, 195, true, 11);
        // The supersede ended `first`'s lease, so its address is authoritative
        // now — the alternative is a heartbeat frozen on the previous address.
        assert_eq!(t.displayed_primary(), 0x1000);
    }
}

pub mod scanout_read_ledger {
    //! Generation-qualified v2 READ LEDGER state machine.
    //!
    //! A page has 65 slots: every legal WindowedBlt reader plus the one direct
    //! flush reader. A slot may be recycled at quiescence even if its allocation
    //! stays live. Its nonzero generation makes that safe: an old token must
    //! never retire a replacement claim with the same slot or resource id.

    /// Duplicated from `helios_protocol` to keep this executable model
    /// dependency-free. `kmd_render` pins it to the wire ABI and to the
    /// WindowedBlt bound at the actual KMD use site.
    pub const SLOT_COUNT: usize = 65;
    pub const NO_SLOT: u8 = 0xFF;
    pub const FREE_RESID: u32 = 0;

    /// Exact KMD-side identity carried by every direct-flush and WindowedBlt
    /// reader: a slot number alone is deliberately insufficient after recycle.
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct LedgerTicket {
        pub slot: u8,
        pub resid: u32,
        pub generation: u64,
    }

    impl LedgerTicket {
        pub const NONE: Self = Self {
            slot: NO_SLOT,
            resid: FREE_RESID,
            generation: 0,
        };

        pub const fn is_claimed(self) -> bool {
            self.slot != NO_SLOT && self.resid != FREE_RESID && self.generation != 0
        }
    }

    /// Reader verdict for a stable v2 sample. A generation change is a
    /// same-resid re-claim, so the UMD retries its full scan rather than using
    /// this predicate to turn it into a false no-wait.
    pub const fn reader_in_flight(
        resid_probe: u32,
        generation_probe: u64,
        issued: u64,
        retired: u64,
        generation_reread: u64,
        resid_reread: u32,
    ) -> bool {
        resid_probe != FREE_RESID
            && generation_probe != 0
            && resid_probe == resid_reread
            && generation_probe == generation_reread
            && issued > retired
    }

    /// One model slot. The driver's counterpart is four mapped atomics
    /// (`resid`, `generation`, `issued`, `retired`) plus a KMD-private
    /// retire-wanted bit, serialized by the ledger's mutation leaf lock.
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct SlotModel {
        pub resid: u32,
        pub generation: u64,
        pub issued: u64,
        pub retired: u64,
        pub wanted: bool,
    }

    /// Unsampled tallies, mirrored into the service key as the `Rd*` values.
    /// NOT zeroed by [`LedgerModel::reset`]: the page is per-transport-
    /// generation state, the counters are per-boot (StartDevice) state, and an
    /// orphaned retirement (a token outliving a reset) balances `retired`
    /// against an `issued` from before the reset ONLY because the two are
    /// decoupled.
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct LedgerCounters {
        /// Ledger issues (`RdIss`).
        pub issued: u32,
        /// Ledger retirements, any outcome (`RdRet`).
        pub retired: u32,
        /// Claims refused because all slots were live (`RdOvf`).
        pub overflow: u32,
        /// Retirements performed by the token's `Drop` rather than an explicit
        /// completion (`RdDrp`) — enqueue failure or transport teardown.
        pub drop_retired: u32,
        /// Retirements whose slot no longer named the token's resid (`RdOrp`).
        /// Reachable ONLY when a reset zeroed the ledger with the read in
        /// flight; any other movement is a bug.
        pub orphaned: u32,
    }

    /// The executable specification of the ledger state machine.
    ///
    /// Single-threaded executable form of the driver's mutation leaf lock.
    #[derive(Clone, Copy, Debug)]
    pub struct LedgerModel {
        pub slots: [SlotModel; SLOT_COUNT],
        /// The page's own `slot_overflow` word (reader-visible loud-failure
        /// signal). Zeroed by reset with the slots, unlike the counters.
        pub page_overflow: u32,
        pub counters: LedgerCounters,
        /// KMD-private global source. Transport reset clears the page, never
        /// this source, so a token from before reset cannot alias a new claim.
        pub next_generation: u64,
    }

    impl Default for LedgerModel {
        fn default() -> Self {
            Self::new()
        }
    }

    impl LedgerModel {
        pub const fn new() -> Self {
            Self {
                slots: [SlotModel {
                    resid: FREE_RESID,
                    generation: 0,
                    issued: 0,
                    retired: 0,
                    wanted: false,
                }; SLOT_COUNT],
                page_overflow: 0,
                counters: LedgerCounters {
                    issued: 0,
                    retired: 0,
                    overflow: 0,
                    drop_retired: 0,
                    orphaned: 0,
                },
                next_generation: 1,
            }
        }

        /// Find the slot currently claimed for `resid`, if any.
        pub fn slot_of(&self, resid: u32) -> Option<usize> {
            if resid == FREE_RESID {
                return None;
            }
            self.slots.iter().position(|s| s.resid == resid)
        }

        /// Find, claim, or (only when necessary) recycle one quiescent slot.
        pub fn issue(&mut self, resid: u32) -> LedgerTicket {
            if resid == FREE_RESID {
                return LedgerTicket::NONE;
            }
            if let Some(i) = self.slot_of(resid) {
                self.slots[i].issued += 1;
                self.counters.issued += 1;
                return Self::ticket(i, self.slots[i]);
            }
            if let Some(i) = self.slots.iter().position(|s| s.resid == FREE_RESID) {
                return self.claim_fresh(i, resid);
            }
            // Allocation liveness is not reader liveness. Once every free slot
            // is occupied, reuse any claim with no active reader.
            if let Some(i) = self.slots.iter().position(|s| s.issued == s.retired) {
                return self.claim_fresh(i, resid);
            }
            self.page_overflow += 1;
            self.counters.overflow += 1;
            LedgerTicket::NONE
        }

        /// One token retirement. A stale `{slot,resid,generation}` is counted
        /// orphaned and cannot mutate a replacement claim.
        pub fn token_retire(&mut self, ticket: LedgerTicket, via_drop: bool) {
            if !ticket.is_claimed() {
                return;
            }
            let i = ticket.slot as usize;
            self.counters.retired += 1;
            if via_drop {
                self.counters.drop_retired += 1;
            }
            if i >= SLOT_COUNT
                || self.slots[i].resid != ticket.resid
                || self.slots[i].generation != ticket.generation
            {
                self.counters.orphaned += 1;
                return;
            }
            self.slots[i].retired += 1;
            if self.slots[i].wanted && self.slots[i].issued == self.slots[i].retired {
                self.reclaim(i);
            }
        }

        /// The backing allocation for `resid` is retiring: reclaim its slot
        /// now if no read is in flight, else mark retire-wanted and let the
        /// equalizing [`Self::token_retire`] complete the reclaim (§3.1).
        pub fn alloc_retire(&mut self, resid: u32) {
            let Some(i) = self.slot_of(resid) else {
                return;
            };
            self.slots[i].wanted = true;
            if self.slots[i].issued == self.slots[i].retired {
                self.reclaim(i);
            }
        }

        /// Transport reset orphans old tickets but deliberately preserves the
        /// global generation source.
        pub fn reset(&mut self) {
            for slot in &mut self.slots {
                *slot = SlotModel::default();
            }
            self.page_overflow = 0;
        }

        fn reclaim(&mut self, i: usize) {
            self.slots[i] = SlotModel::default();
        }

        /// Reads in flight for `resid` — what the mapped reader computes.
        pub fn in_flight(&self, resid: u32) -> u64 {
            self.slot_of(resid)
                .map(|i| self.slots[i].issued - self.slots[i].retired)
                .unwrap_or(0)
        }

        fn claim_fresh(&mut self, i: usize, resid: u32) -> LedgerTicket {
            let generation = match self.mint_generation() {
                Some(generation) => generation,
                None => return LedgerTicket::NONE,
            };
            self.slots[i] = SlotModel {
                resid,
                generation,
                issued: 1,
                retired: 0,
                wanted: false,
            };
            self.counters.issued += 1;
            Self::ticket(i, self.slots[i])
        }

        fn mint_generation(&mut self) -> Option<u64> {
            if self.next_generation == 0 {
                return None;
            }
            let generation = self.next_generation;
            self.next_generation = generation.checked_add(1).unwrap_or(0);
            Some(generation)
        }

        fn ticket(i: usize, slot: SlotModel) -> LedgerTicket {
            LedgerTicket {
                slot: i as u8,
                resid: slot.resid,
                generation: slot.generation,
            }
        }
    }
}

#[cfg(test)]
mod scanout_read_ledger_tests {
    use super::scanout_read_ledger::*;

    /// The rules the model must uphold after EVERY operation — checked from
    /// the outside because nothing in the crate itself may panic.
    fn check_invariants(m: &LedgerModel) {
        for (i, s) in m.slots.iter().enumerate() {
            assert!(s.issued >= s.retired, "slot {i}: retired ran ahead");
            if s.resid == FREE_RESID {
                assert_eq!((s.issued, s.retired), (0, 0), "slot {i}: dirty free slot");
                assert_eq!(s.generation, 0, "slot {i}: free slot has generation");
                assert!(!s.wanted, "slot {i}: free slot still wanted");
            } else {
                assert_ne!(s.generation, 0, "slot {i}: claim has zero generation");
            }
        }
    }

    /// Liveness matrix rows 1/2 (FIX-DESIGN-d4a.md §6): a completed read
    /// retires, and the identity `issued == retired` holds at quiescence.
    #[test]
    fn issue_then_retire_balances_and_reuses_the_slot() {
        let mut m = LedgerModel::new();
        let ticket = m.issue(191);
        assert!(ticket.is_claimed());
        assert_eq!(m.in_flight(191), 1);
        m.token_retire(ticket, false);
        assert_eq!(m.in_flight(191), 0);
        let same_claim = m.issue(191);
        assert_eq!(same_claim, ticket);
        m.token_retire(same_claim, false);
        assert_eq!(m.counters.issued, 2);
        assert_eq!(m.counters.retired, 2);
        assert_eq!(m.slots[ticket.slot as usize].issued, 2);
        assert_eq!(m.slots[ticket.slot as usize].retired, 2);
        check_invariants(&m);
    }

    /// A resid of 0 would alias the free-slot sentinel; the model refuses it
    /// exactly as the driver's issue path does.
    #[test]
    fn free_resid_cannot_claim() {
        let mut m = LedgerModel::new();
        assert_eq!(m.issue(FREE_RESID), LedgerTicket::NONE);
        assert_eq!(m.counters.issued, 0);
        check_invariants(&m);
    }

    /// Rows 4/5: a token whose read never completed retires via `Drop` — the
    /// ledger cannot tell the difference, only the census can.
    #[test]
    fn drop_retirement_balances_and_is_counted() {
        let mut m = LedgerModel::new();
        let ticket = m.issue(191);
        m.token_retire(ticket, true);
        assert_eq!(m.counters.issued, m.counters.retired);
        assert_eq!(m.counters.drop_retired, 1);
        assert_eq!(m.in_flight(191), 0);
    }

    /// The v2 capacity is exactly 64 WindowedBlt readers plus one direct
    /// flush. All 65 active claims fit without a refusal.
    #[test]
    fn sixty_five_active_distinct_claims_do_not_overflow() {
        let mut m = LedgerModel::new();
        for resid in 1..=SLOT_COUNT as u32 {
            assert!(m.issue(resid).is_claimed());
        }
        assert_eq!(m.page_overflow, 0);
        assert_eq!(m.counters.overflow, 0);
        check_invariants(&m);
    }

    /// One live desktop ring and two concurrent four-slot WindowedBlt rings
    /// are ordinary legal pressure, not an overflow condition. This is the
    /// minimal mixed population that used to exhaust the v1 eight-slot page.
    #[test]
    fn desktop_ring_plus_two_windowed_blt_rings_fit_without_overflow() {
        let mut m = LedgerModel::new();
        for resid in [6, 821, 822, 823] {
            assert!(m.issue(resid).is_claimed());
        }
        for resid in [992, 993, 994, 995, 1017, 1018, 1019, 1020] {
            assert!(m.issue(resid).is_claimed());
        }
        assert_eq!(m.page_overflow, 0);
        assert_eq!(m.counters.overflow, 0);
        check_invariants(&m);
    }

    #[test]
    fn sixty_sixth_active_distinct_claim_overflows_loudly() {
        let mut m = LedgerModel::new();
        for resid in 1..=SLOT_COUNT as u32 {
            assert!(m.issue(resid).is_claimed());
        }
        let ticket = m.issue(SLOT_COUNT as u32 + 1);
        assert_eq!(ticket, LedgerTicket::NONE);
        assert_eq!(m.page_overflow, 1);
        assert_eq!(m.counters.overflow, 1);
        m.token_retire(ticket, false);
        assert_eq!(m.counters.issued, SLOT_COUNT as u32);
        assert_eq!(m.counters.retired, 0);
    }

    /// Row 6, quiescent half: an allocation retiring with no read in flight
    /// reclaims immediately, and the freed slot is claimable with 0/0.
    #[test]
    fn alloc_retire_without_inflight_read_reclaims_immediately() {
        let mut m = LedgerModel::new();
        let ticket = m.issue(191);
        m.token_retire(ticket, false);
        m.alloc_retire(191);
        assert_eq!(m.slot_of(191), None);
        let reused = m.issue(400);
        assert_eq!(reused.slot, ticket.slot);
        assert_ne!(reused.generation, ticket.generation);
        assert_eq!(m.slots[ticket.slot as usize].issued, 1);
        assert_eq!(m.slots[ticket.slot as usize].retired, 0);
    }

    /// Row 6, pinned half: a live token pins its slot across the allocation
    /// retire, and the equalizing retirement completes the reclaim — so a
    /// token can NEVER retire into a recycled slot.
    #[test]
    fn live_token_pins_the_slot_until_its_retirement_reclaims() {
        let mut m = LedgerModel::new();
        let ticket = m.issue(191);
        m.alloc_retire(191);
        // Pinned: still claimed, retire-wanted, and the reader still sees the
        // in-flight read (correct — the host read has not terminated).
        assert_eq!(m.slot_of(191), Some(ticket.slot as usize));
        assert!(m.slots[ticket.slot as usize].wanted);
        assert_eq!(m.in_flight(191), 1);
        m.token_retire(ticket, false);
        // The retirement won the wanted token and reclaimed.
        assert_eq!(m.slot_of(191), None);
        assert!(!m.slots[ticket.slot as usize].wanted);
        assert_eq!(m.counters.issued, m.counters.retired);
        check_invariants(&m);
    }

    /// A quiescent claim can be recycled without allocation teardown once all
    /// slots are occupied; allocation liveness is not reader liveness.
    #[test]
    fn quiescent_live_claim_recycles_when_no_free_slot_exists() {
        let mut m = LedgerModel::new();
        let old = m.issue(191);
        m.token_retire(old, false);
        for resid in 2..=SLOT_COUNT as u32 {
            assert!(m.issue(resid).is_claimed());
        }
        let replacement = m.issue(500);
        assert_eq!(replacement.slot, old.slot);
        assert_ne!(replacement.generation, old.generation);
        assert_eq!(m.slot_of(191), None);
        check_invariants(&m);
    }

    #[test]
    fn same_resid_reclaim_gets_a_new_generation() {
        let mut m = LedgerModel::new();
        let old = m.issue(191);
        m.token_retire(old, false);
        m.alloc_retire(191);
        let replacement = m.issue(191);
        assert_eq!(replacement.slot, old.slot);
        assert_ne!(replacement.generation, old.generation);
        assert_eq!(replacement.resid, old.resid);
    }

    #[test]
    fn stale_ticket_cannot_retire_a_new_claim() {
        let mut m = LedgerModel::new();
        let old = m.issue(191);
        m.token_retire(old, false);
        m.alloc_retire(191);
        let replacement = m.issue(400);
        m.token_retire(old, true);
        assert_eq!(m.counters.orphaned, 1);
        assert_eq!(m.slots[replacement.slot as usize].retired, 0);
        assert_eq!(m.in_flight(400), 1);
    }

    #[test]
    fn reset_generation_never_aliases_old_ticket() {
        let mut m = LedgerModel::new();
        let old = m.issue(191);
        m.reset();
        assert_eq!(m.page_overflow, 0);
        let replacement = m.issue(191);
        assert_eq!(replacement.slot, old.slot);
        assert_ne!(replacement.generation, old.generation);
        m.token_retire(old, true);
        assert_eq!(m.counters.orphaned, 1);
        assert_eq!(m.in_flight(191), 1);
        m.token_retire(replacement, false);
        assert_eq!(m.counters.issued, m.counters.retired);
        check_invariants(&m);
    }

    #[test]
    fn issued_equals_retired_at_quiescence() {
        let mut m = LedgerModel::new();
        let mut tickets = [LedgerTicket::NONE; SLOT_COUNT];
        for (index, ticket) in tickets.iter_mut().enumerate() {
            *ticket = m.issue(index as u32 + 1);
        }
        for ticket in tickets {
            m.token_retire(ticket, false);
        }
        assert_eq!(m.counters.issued, m.counters.retired);
        for slot in m.slots {
            assert_eq!(slot.issued, slot.retired);
        }
    }

    /// The reader protocol (§3.1): in-flight requires a stable resid and
    /// `issued > retired`; a mid-read reclaim (resid changed) is a no-wait.
    #[test]
    fn reader_protocol_verdicts() {
        assert!(reader_in_flight(191, 7, 5, 4, 7, 191));
        assert!(!reader_in_flight(191, 7, 5, 5, 7, 191));
        assert!(!reader_in_flight(191, 7, 5, 4, 8, 191));
        assert!(!reader_in_flight(191, 7, 5, 4, 7, 0));
        assert!(!reader_in_flight(FREE_RESID, 0, 1, 0, 0, FREE_RESID));
    }
}

pub mod snapshot_bind {
    //! D4b snapshot bind: the Present-time descriptor gate
    //! (FIX-DESIGN-d4b-snapshot.md §4).
    //!
    //! When `HELIOS_PRESENT_PRIVATE_FLAG_SNAPSHOT` is set, the present private
    //! data describes a UMD-owned SNAPSHOT image (filled by a venus-queue-
    //! ordered copy of the presented primary) that the KMD binds and flushes on
    //! the DMA-flip path INSTEAD of the flipped allocation. The descriptor is
    //! guest-supplied, so every field is validated before it may substitute the
    //! bind target; a descriptor that fails ANY check falls back to the flipped
    //! allocation (today's behaviour), counted as `SnFbk`.
    //!
    //! [`validate_layout`] is the undersize guard — the Xid-31 protection —
    //! reproduced byte-for-byte (saturating arithmetic included) from
    //! `ScanoutTarget::from_direct_primary` in
    //! `kmd_render/src/ddi/create_allocation.rs`. It must NEVER be relaxed: an
    //! `alloc_size` smaller than `plane_offset + pitch*height` lets QEMU read
    //! past the blob.

    use crate::ScanoutFormat;

    /// The validated snapshot bind target, carried BY VALUE from the Present
    /// DDI through the flip record to both bind paths. No pointer and no
    /// allocation-table lookup ever resolves the snapshot, so its
    /// `AllocationContext` lifetime cannot be involved; liveness is enforced
    /// where it already lives (the flush executor's `resource_is_live` arm).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SnapshotDescriptor {
        /// Venus resource id of the snapshot image. Never 0 in a valid
        /// descriptor; 0 is the "no substitution" sentinel on the carriers.
        pub resource_id: u32,
        pub width: u32,
        pub height: u32,
        /// Row pitch in bytes (the stride `SET_SCANOUT_BLOB` uses).
        pub pitch: u32,
        /// Exact DXGI format; must resolve via [`ScanoutFormat::from_dxgi`].
        pub dxgi_format: u32,
        /// Memory-plane-0 byte offset within the backing allocation.
        pub plane_offset: u64,
        /// Total venus blob size backing `resource_id` — the undersize guard's
        /// right-hand side.
        pub venus_alloc_size: u64,
        /// Exact Vulkan memory type for a typed WindowedBlt import.  Zero is a
        /// valid type; its presence is proved by the complete v2 command tail
        /// and the WindowedBlt purpose, not by a sentinel value.
        pub memory_type_index: u32,
        /// 0 = direct bind snapshot, 1 = WindowedBlt source snapshot.
        pub purpose: u32,
    }

    /// Why a snapshot descriptor was refused. Every arm is one `SnFbk` and a
    /// fall-back to binding the flipped allocation; none is an error return.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SnapshotReject {
        /// `resource_id == 0` — the no-substitution sentinel arrived flagged.
        ZeroResource,
        /// Descriptor extent differs from the allocation-list source's extent
        /// (a fullscreen transition / geometry change mid-flight).
        ExtentMismatch,
        /// Pitch/offset/size failed the direct-scan-out layout rules — the
        /// undersize guard.
        Layout,
        /// No virtio scan-out encoding for `dxgi_format`.
        Format,
        /// Descriptor format differs from the DXGI allocation-list source.
        /// Both can be individually supported but are not interchangeable.
        SourceFormatMismatch,
        /// A WindowedBlt path received a direct-bind descriptor (or vice
        /// versa). These are different consumers and must never be inferred.
        Purpose,
    }

    /// The full Present-time gate: identity, extent, then layout. The caller
    /// has already established `FLAG_SNAPSHOT` + 48-byte coverage; this
    /// validates everything the wire bytes claim. `source_width`/
    /// `source_height` are the allocation-list source's extent — the identity
    /// Windows placed in the Present call, which the descriptor must agree
    /// with before it may substitute for it.
    pub const fn validate(
        d: &SnapshotDescriptor,
        source_width: u32,
        source_height: u32,
    ) -> Result<(), SnapshotReject> {
        if d.resource_id == 0 {
            return Err(SnapshotReject::ZeroResource);
        }
        if d.width != source_width || d.height != source_height {
            return Err(SnapshotReject::ExtentMismatch);
        }
        validate_layout(d)
    }

    /// The layout half, shared with `ScanoutTarget::from_snapshot_descriptor`
    /// so the constructor cannot restate the arithmetic in a weakened form.
    ///
    /// ⚠ Byte-for-byte from `ScanoutTarget::from_direct_primary`, saturating
    /// arithmetic included. Do not "simplify"; do not relax.
    pub const fn validate_layout(d: &SnapshotDescriptor) -> Result<(), SnapshotReject> {
        let min_size = d
            .plane_offset
            .saturating_add((d.pitch as u64).saturating_mul(d.height as u64));
        let layout_ok = d.pitch >= d.width.saturating_mul(4)
            && d.pitch & 3 == 0
            && d.plane_offset <= u32::MAX as u64
            && d.venus_alloc_size >= min_size;
        if !layout_ok {
            return Err(SnapshotReject::Layout);
        }
        if ScanoutFormat::from_dxgi(d.dxgi_format).is_none() {
            return Err(SnapshotReject::Format);
        }
        Ok(())
    }

    /// Typed WindowedBlt source gate. It is deliberately distinct from
    /// [`validate`]: this snapshot is imported into the KMD Venus context and
    /// copied into DXGI's destination; it is never a `SET_SCANOUT_BLOB` target.
    pub const fn validate_windowed_blt(
        d: &SnapshotDescriptor,
        source_width: u32,
        source_height: u32,
        source_dxgi_format: u32,
    ) -> Result<(), SnapshotReject> {
        if d.purpose != 1 {
            return Err(SnapshotReject::Purpose);
        }
        if d.dxgi_format != source_dxgi_format {
            return Err(SnapshotReject::SourceFormatMismatch);
        }
        validate(d, source_width, source_height)
    }
}

/// Pure exact-membership rule for WindowedBlt WDDM retirement. Tokens are
/// monotonically minted only as local request identities; they are not an
/// ordering relation across generation-qualified present streams.
pub mod windowed_blt_token {
    /// Returns true only for the exact terminal `(token, stream_boundary)`
    /// pair. A greater token on another stream proves nothing about this one.
    pub fn terminal_contains(terminal: &[(u64, u64)], token: u64, stream_boundary: u64) -> bool {
        terminal.iter().any(|&(known_token, known_stream)| {
            known_token == token && known_stream == stream_boundary
        })
    }

    /// Does `candidate` belong to the merged terminal prefix ending at
    /// `(max_token, max_boundary)`? The boundary encoding reserves bit 63 and
    /// carries a generation-qualified stream handle in bits 32..62. Token
    /// order is meaningful only *inside* that exact stream.
    pub const fn prefix_contains(
        max_token: u64,
        max_boundary: u64,
        candidate_token: u64,
        candidate_boundary: u64,
    ) -> bool {
        const TAG: u64 = 1 << 63;
        const HANDLE_MASK: u64 = 0x7fff_ffff;
        max_token != 0
            && max_boundary & TAG != 0
            && candidate_boundary & TAG != 0
            && candidate_token <= max_token
            && ((max_boundary >> 32) & HANDLE_MASK) == ((candidate_boundary >> 32) & HANDLE_MASK)
    }

    /// Whether an exact terminal/pending pair may attach to a newly replayed
    /// SubmitCommand. A preempted buffer can legitimately reach a terminal
    /// before its DMA private data is resubmitted; an already queued WDDM
    /// prefix owns that terminal and must reject a duplicate replay.
    pub const fn can_attach_dependency(
        exact_pending: bool,
        exact_terminal: bool,
        already_owned_by_wddm: bool,
    ) -> bool {
        !already_owned_by_wddm && (exact_pending || exact_terminal)
    }

    /// Number of terminal identities left after successfully consuming one
    /// same-stream WDDM prefix. This makes bounded-capacity behavior a host
    /// testable pure rule rather than a property of the KMD container.
    pub fn count_after_prefix_consume(
        terminal: &[(u64, u64)],
        max_token: u64,
        max_boundary: u64,
    ) -> usize {
        terminal
            .iter()
            .filter(|&&(token, boundary)| {
                !prefix_contains(max_token, max_boundary, token, boundary)
            })
            .count()
    }

    /// Scheduler/lifecycle cancellation may only reclaim an undispatched
    /// request that belongs to the dead same-stream prefix.
    pub const fn prefix_cancel_eligible(
        dispatched: bool,
        max_token: u64,
        max_boundary: u64,
        token: u64,
        boundary: u64,
    ) -> bool {
        !dispatched && prefix_contains(max_token, max_boundary, token, boundary)
    }
}

#[cfg(test)]
mod windowed_blt_token_tests {
    use super::windowed_blt_token::{
        can_attach_dependency, count_after_prefix_consume, prefix_cancel_eligible, prefix_contains,
        terminal_contains,
    };

    #[test]
    fn interleaved_stream_terminal_tokens_do_not_cross_admit() {
        let stream_a = 0x8000_0001_0000_0009u64;
        let stream_b = 0x8000_0002_0000_0007u64;
        // Stream B's numerically later token completed first. It cannot retire
        // stream A's older token just because a global counter would be past it.
        let terminal = [(3u64, stream_b)];
        assert!(!terminal_contains(&terminal, 2, stream_a));
        assert!(terminal_contains(&terminal, 3, stream_b));
    }

    #[test]
    fn merged_prefix_consumes_only_its_own_stream_and_releases_capacity() {
        let stream_a_7 = 0x8000_0011_0000_0007u64;
        let stream_a_9 = 0x8000_0011_0000_0009u64;
        let stream_b_3 = 0x8000_0022_0000_0003u64;
        let terminal = [(7, stream_a_7), (3, stream_b_3), (9, stream_a_9)];
        assert!(prefix_contains(9, stream_a_9, 7, stream_a_7));
        assert!(!prefix_contains(9, stream_a_9, 3, stream_b_3));
        assert_eq!(count_after_prefix_consume(&terminal, 9, stream_a_9), 1);

        let full = [(1, stream_a_7); 64];
        assert_eq!(count_after_prefix_consume(&full, 64, stream_a_9), 0);
    }

    #[test]
    fn dead_prefix_cancels_only_undispatched_same_stream_requests() {
        let stream_a_7 = 0x8000_0011_0000_0007u64;
        let stream_a_9 = 0x8000_0011_0000_0009u64;
        let stream_b_3 = 0x8000_0022_0000_0003u64;
        assert!(prefix_cancel_eligible(false, 9, stream_a_9, 7, stream_a_7));
        assert!(prefix_cancel_eligible(false, 9, stream_a_9, 9, stream_a_9));
        assert!(!prefix_cancel_eligible(false, 9, stream_a_9, 3, stream_b_3));
        assert!(!prefix_cancel_eligible(true, 9, stream_a_9, 7, stream_a_7));
    }

    #[test]
    fn preempted_terminal_can_replay_once_but_stale_or_owned_private_cannot() {
        assert!(can_attach_dependency(false, true, false));
        assert!(!can_attach_dependency(false, false, false));
        assert!(!can_attach_dependency(false, true, true));
    }
}

#[cfg(test)]
mod snapshot_bind_tests {
    use super::snapshot_bind::*;

    /// A descriptor shaped like the real S-ring slot: 1920×1080 BGRA, the
    /// UMD's 256-aligned pitch, plane data at 0, exact-size blob.
    fn good() -> SnapshotDescriptor {
        SnapshotDescriptor {
            resource_id: 0x131,
            width: 1920,
            height: 1080,
            pitch: 7680,
            dxgi_format: 87,
            plane_offset: 0,
            venus_alloc_size: 7680 * 1080,
            memory_type_index: 0,
            purpose: 0,
        }
    }

    #[test]
    fn a_healthy_descriptor_validates() {
        assert_eq!(validate(&good(), 1920, 1080), Ok(()));
    }

    #[test]
    fn zero_resource_is_the_sentinel_and_never_substitutes() {
        let mut d = good();
        d.resource_id = 0;
        assert_eq!(validate(&d, 1920, 1080), Err(SnapshotReject::ZeroResource));
    }

    /// Extent must equal the ALLOCATION-LIST source's, not merely be
    /// self-consistent — the fullscreen-transition fallback row of the §6
    /// matrix.
    #[test]
    fn extent_mismatch_falls_back() {
        assert_eq!(
            validate(&good(), 1896, 1030),
            Err(SnapshotReject::ExtentMismatch)
        );
        let mut d = good();
        d.height = 1030;
        assert_eq!(
            validate(&d, 1920, 1080),
            Err(SnapshotReject::ExtentMismatch)
        );
    }

    /// The undersize guard: `alloc_size >= plane_offset + pitch*height`,
    /// exact at the boundary.
    #[test]
    fn undersize_guard_is_exact_and_never_relaxed() {
        let mut d = good();
        d.venus_alloc_size = 7680 * 1080 - 1;
        assert_eq!(validate(&d, 1920, 1080), Err(SnapshotReject::Layout));
        d.venus_alloc_size = 7680 * 1080;
        assert_eq!(validate(&d, 1920, 1080), Ok(()));
        // The plane offset shifts the requirement by exactly itself.
        d.plane_offset = 4096;
        assert_eq!(validate(&d, 1920, 1080), Err(SnapshotReject::Layout));
        d.venus_alloc_size = 4096 + 7680 * 1080;
        assert_eq!(validate(&d, 1920, 1080), Ok(()));
    }

    /// The widened-u64 arithmetic: the worst representable pitch*height
    /// product (~2^64 - 2^33) must still be demanded IN FULL from
    /// `venus_alloc_size` — a narrower or wrapping formulation would compute a
    /// small `min_size` a tiny blob satisfies. (True u64 saturation is
    /// unreachable from two u32 inputs; `saturating_*` is defense-in-depth,
    /// kept byte-for-byte with `from_direct_primary`.)
    #[test]
    fn near_max_products_demand_the_full_size() {
        let mut d = good();
        d.plane_offset = 0;
        d.pitch = u32::MAX & !3; // 4-aligned, enormous
        d.height = u32::MAX;
        let min_size = (d.pitch as u64) * (d.height as u64);
        d.venus_alloc_size = min_size - 1;
        assert_eq!(validate_layout(&d), Err(SnapshotReject::Layout));
        d.venus_alloc_size = min_size;
        assert_eq!(validate_layout(&d), Ok(()));
    }

    #[test]
    fn pitch_rules_match_the_direct_primary_validator() {
        // pitch < width*4
        let mut d = good();
        d.pitch = 1920 * 4 - 4;
        assert_eq!(validate(&d, 1920, 1080), Err(SnapshotReject::Layout));
        // pitch % 4 != 0
        let mut d = good();
        d.pitch = 7682;
        d.venus_alloc_size = 7682 * 1080;
        assert_eq!(validate(&d, 1920, 1080), Err(SnapshotReject::Layout));
        // width*4 exactly (no 256-alignment REQUIREMENT here, same as the
        // direct-primary validator).
        let mut d = good();
        d.pitch = 1920 * 4;
        d.venus_alloc_size = 1920 * 4 * 1080;
        assert_eq!(validate(&d, 1920, 1080), Ok(()));
    }

    #[test]
    fn plane_offset_must_fit_u32() {
        let mut d = good();
        d.plane_offset = u32::MAX as u64 + 1;
        d.venus_alloc_size = u64::MAX;
        assert_eq!(validate(&d, 1920, 1080), Err(SnapshotReject::Layout));
    }

    /// The strict DXGI set (28/87/88) and NOT the legacy-zero arm: a snapshot
    /// is a freshly created UMD image that always carries its exact format.
    #[test]
    fn format_uses_the_strict_dxgi_set() {
        for (fmt, ok) in [
            (28u32, true),
            (87, true),
            (88, true),
            (0, false),
            (24, false),
        ] {
            let mut d = good();
            d.dxgi_format = fmt;
            assert_eq!(validate(&d, 1920, 1080).is_ok(), ok, "dxgi {fmt}");
        }
    }

    #[test]
    fn windowed_blt_requires_the_exact_dxgi_source_format() {
        let mut d = good();
        d.purpose = 1;
        assert_eq!(validate_windowed_blt(&d, 1920, 1080, 87), Ok(()));
        // RGBA and BGRA are both supported scanout encodings, but importing a
        // BGRA snapshot for a DXGI RGBA source would silently swizzle DWM.
        assert_eq!(
            validate_windowed_blt(&d, 1920, 1080, 28),
            Err(SnapshotReject::SourceFormatMismatch)
        );
        d.purpose = 0;
        assert_eq!(
            validate_windowed_blt(&d, 1920, 1080, 87),
            Err(SnapshotReject::Purpose)
        );
    }
}

/// The registered present-stream value relation, as pure arithmetic.
///
/// One rule lives here rather than inline in the KMD for a specific reason:
/// `kmd_render` is built with `debug-assertions = on` in the profile that
/// actually ships (46th session), so a plain `value - submitted_value` on `u32`
/// is a PANIC — a silent graphics deadlock — for exactly the input the
/// instrument above it exists to observe. Saturating it makes that input
/// unrepresentable, and puts a host-run test under the claim.
/// The consumer-side liveness bound on the WDDM FIFO head (`WddmHeadMs`;
/// `KMD_IMPACT.md` §14a.2 K-F2 and `docs/dx12/PENDING.md` §1 A5).
///
/// Two rules live here because both are places an operator value or a clock edge
/// could silently break correctness, and neither is testable inside `kmd_render`.
pub mod wddm_head_bound {
    /// Clamp an operator-supplied bound, in ms.
    ///
    /// ⛔ CLAMPED IN BOTH DIRECTIONS, and the two directions protect different
    /// things: too LARGE reinstates the unbounded head and therefore the
    /// adapter-wide TDR the bound exists to prevent, too SMALL turns a last-resort
    /// release into a continuous early-fence generator (the 0ab-B stale-frame
    /// class). 0 is preserved exactly — it is the A/B disable and must stay
    /// reachable (CLAUDE.md rule 8).
    pub const fn clamp_bound_ms(raw: u32, min: u32, max: u32) -> u32 {
        if raw == 0 {
            return 0;
        }
        if raw < min {
            return min;
        }
        if raw > max {
            return max;
        }
        raw
    }

    /// What a blocked look at the FIFO head should do about the bound.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Action {
        /// The bound is disabled (`WddmHeadMs = 0`): historical unbounded head.
        Disabled,
        /// Nothing was armed yet; arm the deadline at this absolute time and keep
        /// waiting. The FIRST blocked look at this entry as head does this.
        Arm(u64),
        /// Armed and not yet expired: keep waiting.
        Wait,
        /// Expired: rebase this head's tagged dependencies onto the conservative
        /// wire watermark.
        Rebase,
        /// Already rebased once. A second rebase could only install a NEWER
        /// `next_wire_fence`, i.e. a STRICTER dependency, so it is forbidden rather
        /// than merely pointless.
        AlreadyRebased,
    }

    /// Whether the periodic heartbeat should prompt a completion DPC.
    ///
    /// ⚠ THE ZERO IS THE COMMON CASE AND IT MUST STAY CHEAP. Nothing is armed on
    /// an ordinary desktop, and the caller reads a single relaxed atomic to learn
    /// that — it must not reach for a clock, and it must NOT test the knob instead:
    /// `WddmHeadMs` defaults to 250, so a knob test is true on every shipping boot
    /// and would turn this into a permanent 60 DPC/s tax on the compositor path.
    /// Armed-ness is STATE; the bound is CONFIGURATION.
    pub const fn heartbeat_due(deadline_100ns: u64, now_100ns: u64) -> bool {
        deadline_100ns != 0 && now_100ns >= deadline_100ns
    }

    /// Decide one blocked look. `deadline_100ns == 0` means "not armed".
    pub const fn look(bound_ms: u32, now_100ns: u64, deadline_100ns: u64, rebased: bool) -> Action {
        if bound_ms == 0 {
            return Action::Disabled;
        }
        if rebased {
            return Action::AlreadyRebased;
        }
        if deadline_100ns == 0 {
            // 100 ns units. A saturating add keeps an exhausted interrupt-time
            // representation from wrapping into a deadline already in the past —
            // which would rebase the very next look.
            return Action::Arm(now_100ns.saturating_add(bound_ms as u64 * 10_000));
        }
        if now_100ns < deadline_100ns {
            return Action::Wait;
        }
        Action::Rebase
    }
}

#[cfg(test)]
mod wddm_head_bound_tests {
    use super::wddm_head_bound::{Action, clamp_bound_ms, heartbeat_due, look};

    const MIN: u32 = 100;
    const MAX: u32 = 1000;

    #[test]
    fn zero_survives_the_clamp_because_it_is_the_ab_disable() {
        assert_eq!(clamp_bound_ms(0, MIN, MAX), 0);
    }

    #[test]
    fn an_operator_typo_cannot_reinstate_the_tdr() {
        // The failure this guards: WddmHeadMs=100000 would be 100 s, far past the
        // 2 s default TdrDelay, i.e. the bound never acts.
        assert_eq!(clamp_bound_ms(100_000, MIN, MAX), MAX);
        assert_eq!(clamp_bound_ms(u32::MAX, MIN, MAX), MAX);
    }

    #[test]
    fn an_operator_typo_cannot_turn_the_last_resort_into_a_policy() {
        // 1 ms would rebase healthy frames continuously, which is 0ab-B on purpose.
        assert_eq!(clamp_bound_ms(1, MIN, MAX), MIN);
        assert_eq!(clamp_bound_ms(MIN - 1, MIN, MAX), MIN);
        assert_eq!(clamp_bound_ms(MIN, MIN, MAX), MIN);
        assert_eq!(clamp_bound_ms(250, MIN, MAX), 250);
    }

    #[test]
    fn the_first_blocked_look_arms_and_does_not_release() {
        assert_eq!(look(250, 1_000, 0, false), Action::Arm(1_000 + 2_500_000));
    }

    #[test]
    fn an_armed_head_waits_until_the_deadline_and_then_rebases_once() {
        let deadline = 1_000 + 2_500_000;
        assert_eq!(look(250, deadline - 1, deadline, false), Action::Wait);
        assert_eq!(look(250, deadline, deadline, false), Action::Rebase);
        // ...and never again, however long it stays blocked afterwards.
        assert_eq!(
            look(250, deadline + 10_000_000, deadline, true),
            Action::AlreadyRebased
        );
    }

    #[test]
    fn the_disabled_bound_short_circuits_every_other_state() {
        assert_eq!(look(0, 0, 0, false), Action::Disabled);
        assert_eq!(look(0, u64::MAX, 1, false), Action::Disabled);
        assert_eq!(look(0, u64::MAX, 1, true), Action::Disabled);
    }

    #[test]
    fn an_unarmed_head_never_asks_the_heartbeat_for_anything() {
        // THE COMMON CASE, and the one that must stay false: an ordinary desktop
        // has nothing armed. A caller that tested the KNOB instead of this state
        // would be true on every shipping boot (WddmHeadMs defaults to 250) and
        // would prompt a DPC on all 60 ticks a second, forever.
        assert!(!heartbeat_due(0, 0));
        assert!(!heartbeat_due(0, u64::MAX));
    }

    #[test]
    fn the_heartbeat_fires_only_at_or_after_an_armed_deadline() {
        let deadline = 2_500_000;
        assert!(!heartbeat_due(deadline, deadline - 1));
        assert!(heartbeat_due(deadline, deadline));
        assert!(heartbeat_due(deadline, deadline + 1));
    }

    #[test]
    fn an_exhausted_interrupt_clock_does_not_arm_a_deadline_in_the_past() {
        // Without the saturating add the arm would wrap to a small number and the
        // NEXT look would rebase immediately — a released fence caused by a clock,
        // not by a stuck producer.
        let Action::Arm(deadline) = look(250, u64::MAX, 0, false) else {
            panic!("expected Arm");
        };
        assert_eq!(deadline, u64::MAX);
        assert_eq!(look(250, u64::MAX - 1, deadline, false), Action::Wait);
    }
}

/// How a WDDM DMA fence's wire-fence dependency is chosen from the boundary a
/// guest submission named. Defects A4 and A6 of `docs/dx12/PENDING.md` §1 are both
/// decisions in this one table, which is why the table is here and testable rather
/// than an `if` chain inside the transport.
pub mod wddm_boundary {
    /// How the resulting watermark is compared against the in-flight wire fences.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Kind {
        /// EXCLUSIVE PREFIX: every async fence strictly below the watermark must
        /// have retired — every ring, every process. Conservative, always
        /// eventually satisfied, never a lie, and the fallback for any boundary
        /// that cannot be trusted.
        Prefix,
        /// The watermark IS one wire fence, and only that fence must have retired.
        /// The frame's own boundary.
        Exact,
    }

    /// Why a named boundary was replaced, if it was.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Rejection {
        /// The named boundary was used.
        Accepted,
        /// Zero, or at/beyond the ids this transport generation has assigned — a
        /// malformed or stale marker inside the live generation.
        OutOfRange,
        /// Below this transport generation's first id: a fence from a PREVIOUS
        /// StartDevice, surviving in a client that outlived a device restart.
        ///
        /// ⛔ It is a separate variant because an upper bound cannot see it. Wire
        /// fence ranges STRIDE UP at every StartDevice, so such an id is billions
        /// below the live range, satisfies `< next_wire_fence` trivially, matches
        /// nothing in flight, and would make the dependency complete instantly —
        /// a DMA fence reporting completion before its work exists.
        ForeignGeneration,
    }

    /// The chosen dependency.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Selection {
        pub watermark: u64,
        pub kind: Kind,
        pub rejection: Rejection,
    }

    /// Choose the wire-fence dependency for a submission that named
    /// `gpu_fence_id`.
    ///
    /// `wire_fence_base` is the FIRST id this transport generation may hand out and
    /// `next_wire_fence` the next it will; every id in `base..next` was genuinely
    /// assigned and enqueued, so `[base, next)` is exactly "issued by this
    /// generation".
    ///
    /// `d3d12` says the submission carried a `HeliosD3D12SubmitCmd` record — an
    /// IDENTITY, never a boundary. It selects [`Kind::Exact`] because a D3D12 ECL
    /// packet carries no GPU commands of its own: the only thing its DMA completion
    /// can truthfully report is that the batch the UMD named has finished, so
    /// waiting on the prefix below that fence is waiting on other processes'
    /// frames. The legacy Present writer of the same field keeps [`Kind::Prefix`]
    /// because that is the shipping, measured desktop configuration.
    pub const fn select(
        gpu_fence_id: u64,
        wire_fence_base: u64,
        next_wire_fence: u64,
        d3d12: bool,
    ) -> Selection {
        if gpu_fence_id == 0 || gpu_fence_id >= next_wire_fence {
            return Selection {
                watermark: next_wire_fence,
                kind: Kind::Prefix,
                rejection: Rejection::OutOfRange,
            };
        }
        if gpu_fence_id < wire_fence_base {
            return Selection {
                watermark: next_wire_fence,
                kind: Kind::Prefix,
                rejection: Rejection::ForeignGeneration,
            };
        }
        if d3d12 {
            return Selection {
                watermark: gpu_fence_id,
                kind: Kind::Exact,
                rejection: Rejection::Accepted,
            };
        }
        Selection {
            // The prefix bound is EXCLUSIVE, so naming fence N means waiting for
            // everything below N + 1. `saturating_add` because a u64 fence id at
            // the representation's ceiling must not wrap to 0, which is the "no
            // dependency" watermark.
            watermark: gpu_fence_id.saturating_add(1),
            kind: Kind::Prefix,
            rejection: Rejection::Accepted,
        }
    }
}

#[cfg(test)]
mod wddm_boundary_tests {
    use super::wddm_boundary::{Kind, Rejection, select};

    /// A representative live generation: base 1 + 3·2^32, 40 ids issued.
    const BASE: u64 = 1 + (3u64 << 32);
    const NEXT: u64 = BASE + 40;

    #[test]
    fn a_d3d12_boundary_is_exact_and_names_the_fence_itself() {
        let s = select(BASE + 12, BASE, NEXT, true);
        assert_eq!(s.kind, Kind::Exact);
        assert_eq!(s.rejection, Rejection::Accepted);
        // NOT `+ 1`: an exact test names the fence, and adding one would silently
        // turn it back into the prefix that A4 is about.
        assert_eq!(s.watermark, BASE + 12);
    }

    #[test]
    fn the_legacy_present_writer_keeps_the_exclusive_prefix() {
        let s = select(BASE + 12, BASE, NEXT, false);
        assert_eq!(s.kind, Kind::Prefix);
        assert_eq!(s.rejection, Rejection::Accepted);
        assert_eq!(s.watermark, BASE + 13);
    }

    #[test]
    fn a_foreign_generation_fence_is_rejected_however_plausible_it_looks() {
        // A6. The previous generation's ids are a whole stride below, and every
        // one of them satisfies `< next_wire_fence`.
        for id in [1u64, 42, BASE - 1, 1 + (2u64 << 32) + 7] {
            let s = select(id, BASE, NEXT, true);
            assert_eq!(s.rejection, Rejection::ForeignGeneration, "id {id}");
            // The fallback must be the conservative prefix, never the named id:
            // an exact wait on a fence this generation never issued is satisfied
            // immediately, which is the lie.
            assert_eq!(s.kind, Kind::Prefix, "id {id}");
            assert_eq!(s.watermark, NEXT, "id {id}");
        }
    }

    #[test]
    fn zero_and_beyond_the_range_stay_on_the_old_clamp_and_its_own_counter() {
        for id in [0u64, NEXT, NEXT + 1, u64::MAX] {
            let s = select(id, BASE, NEXT, true);
            assert_eq!(s.rejection, Rejection::OutOfRange, "id {id}");
            assert_eq!(s.kind, Kind::Prefix, "id {id}");
            assert_eq!(s.watermark, NEXT, "id {id}");
        }
    }

    #[test]
    fn exactly_one_rejection_can_apply_to_any_input() {
        // The two rejections must partition, or a counter pair reads as double
        // the truth. Sweep the boundaries of both conditions.
        for id in [0, 1, BASE - 1, BASE, BASE + 1, NEXT - 1, NEXT, NEXT + 1] {
            let s = select(id, BASE, NEXT, true);
            let out_of_range = id == 0 || id >= NEXT;
            let foreign = !out_of_range && id < BASE;
            assert_eq!(
                s.rejection == Rejection::OutOfRange,
                out_of_range,
                "id {id}"
            );
            assert_eq!(
                s.rejection == Rejection::ForeignGeneration,
                foreign,
                "id {id}"
            );
            assert_eq!(
                s.rejection == Rejection::Accepted,
                !out_of_range && !foreign,
                "id {id}"
            );
        }
    }

    #[test]
    fn the_first_generation_starts_at_one_and_accepts_its_own_low_ids() {
        // `NEXT_WIRE_FENCE_BASE` starts at 1, so instance 0's ids ARE small. The
        // foreign-generation test must not reject them.
        let s = select(1, 1, 5, true);
        assert_eq!(s.rejection, Rejection::Accepted);
        assert_eq!(s.watermark, 1);
        assert_eq!(select(0, 1, 5, true).rejection, Rejection::OutOfRange);
    }

    #[test]
    fn an_empty_generation_accepts_nothing() {
        // Nothing assigned yet: base == next, so every id is out of range and no
        // boundary can be honoured. Notably NOT reported as foreign — the
        // out-of-range test runs first, and it is the honest description.
        let s = select(BASE, BASE, BASE, true);
        assert_eq!(s.rejection, Rejection::OutOfRange);
        assert_eq!(s.watermark, BASE);
        assert_eq!(s.kind, Kind::Prefix);
    }

    #[test]
    fn a_prefix_watermark_never_wraps_to_the_no_dependency_sentinel() {
        // 0 is "no dependency". A `+ 1` that wrapped would turn the strongest
        // possible wait into none at all.
        let s = select(u64::MAX - 1, 0, u64::MAX, false);
        assert_eq!(s.rejection, Rejection::Accepted);
        assert_eq!(s.watermark, u64::MAX);
        assert_ne!(s.watermark, 0);
    }
}

pub mod present_stream {
    /// Capacity of the KMD's fixed, allocation-free registered-stream table.
    ///
    /// ⚠ These five constants are the ABI of the tagged boundary and of the
    /// packed handle, and they are DEFINED HERE so that the rules below and the
    /// tests that exercise them cannot drift from the driver's own numbers:
    /// `kmd_render`'s `MAX_PRESENT_STREAMS` / `PRESENT_STREAM_INDEX_BITS` /
    /// `PRESENT_STREAM_GENERATION_BITS` / `PRESENT_STREAM_GENERATION_MAX` /
    /// `PRESENT_STREAM_BOUNDARY_TAG` are aliases of these.
    pub const MAX_STREAMS: usize = 64;
    /// Low bits of a packed handle that carry the raw slot index.
    pub const INDEX_BITS: u32 = 6;
    /// Bits left for the generation inside the 31 the tagged-boundary ABI
    /// reserves for the handle (bit 63 is the namespace tag, bit 31 is unused so
    /// the handle stays positive in every signed reinterpretation).
    pub const GENERATION_BITS: u32 = 31 - INDEX_BITS;
    /// Largest generation a slot may reach before it must be retired rather than
    /// re-registered; the registration scan refuses a slot at this value.
    pub const GENERATION_MAX: u32 = (1 << GENERATION_BITS) - 1;
    /// Bit 63 distinguishes a tagged stream boundary from the legacy exclusive
    /// wire-fence namespace. The two are intentionally incomparable.
    pub const BOUNDARY_TAG: u64 = 1 << 63;

    /// Pack one slot's `(generation, index)` into the opaque handle.
    ///
    /// The generation is nonzero for a live slot, so the whole handle is nonzero
    /// without slot 63 ever carrying into the generation field — which is why
    /// the index occupies the LOW bits and is not added to a shifted generation.
    pub const fn slot_handle(generation: u32, index: usize) -> u32 {
        (generation << INDEX_BITS) | index as u32
    }

    /// Recover the slot index a handle names, or `None` when it is out of range.
    pub const fn handle_index(handle: u32) -> Option<usize> {
        let index = (handle & ((1 << INDEX_BITS) - 1)) as usize;
        if index < MAX_STREAMS {
            Some(index)
        } else {
            None
        }
    }

    /// Readiness for one decoded stream slot.
    ///
    /// A dead or generation-mismatched slot is NEVER success. Its owner must
    /// explicitly discharge any scheduler/scanout wait that still carries the
    /// old boundary before the slot is retired; accepting it here would turn a
    /// rejected producer into a false `DMA_COMPLETED` edge.
    pub const fn slot_ready(
        live: bool,
        generation: u32,
        index: usize,
        handle: u32,
        value: u32,
        retired_value: u32,
    ) -> bool {
        live && slot_handle(generation, index) == handle && retired_value >= value
    }

    /// Monotonic stream retirement: host completions may be observed out of
    /// order, and a stream value must never move backwards.
    pub const fn advance_retired(retired_value: u32, completed_value: u32) -> u32 {
        if completed_value > retired_value {
            completed_value
        } else {
            retired_value
        }
    }

    /// Encode a generation-qualified opaque present-stream boundary.
    pub const fn encode_boundary(handle: u32, value: u32) -> u64 {
        BOUNDARY_TAG | ((handle as u64) << 32) | value as u64
    }

    /// Decode a tagged boundary. Legacy wire-fence boundaries are intentionally
    /// NOT accepted here: their ordering relation is unrelated to stream values,
    /// so a numeric comparison across the two namespaces is meaningless.
    pub const fn decode_boundary(boundary: u64) -> Option<(u32, u32)> {
        if boundary & BOUNDARY_TAG == 0 {
            return None;
        }
        let handle = ((boundary >> 32) & 0x7fff_ffff) as u32;
        if handle == 0 {
            None
        } else {
            Some((handle, boundary as u32))
        }
    }

    /// A value-only lifecycle tag reaches the host-selection ledger only on a
    /// TERMINAL SUCCESSFUL response.
    ///
    /// Generic over the tag so the rule is expressible without the transport's
    /// `SyncScanoutBind`: abandoning a stack waiter detaches only the waiter,
    /// and a late `RESP_OK` must still advance lifecycle selection, while any
    /// non-OK response must not.
    pub const fn terminal_on_response_ok<T: Copy>(response_ok: bool, tag: Option<T>) -> Option<T> {
        if response_ok {
            tag
        } else {
            None
        }
    }

    /// How far a marker `value` runs AHEAD of what the producer has actually
    /// submitted on that stream; 0 when it does not run ahead at all.
    ///
    /// 0 deliberately covers both "already submitted" and "already retired":
    /// neither is a future dependency, so neither is what the instrument
    /// counts.
    pub const fn marker_lookahead(value: u32, submitted_value: u32) -> u32 {
        value.saturating_sub(submitted_value)
    }

    /// Whether the marker names producer work that has not been submitted yet.
    ///
    /// ⚠ This is an OBSERVATION, never a refusal predicate, and the difference
    /// is the whole K-F2 finding: on the shipping D3D11 present path the UMD
    /// delivers the marker BEFORE the frame's `vkQueueSubmit` on purpose (it
    /// skips its own submitted-gate precisely because the marker carries the
    /// dependency), so `true` here is the normal steady state. The tag path's
    /// direction is the opposite one — a PRODUCER advancing the stream must be
    /// strictly ahead of `submitted_value` — and conflating the two would
    /// refuse every legitimate frame.
    pub const fn marker_runs_ahead(value: u32, submitted_value: u32) -> bool {
        marker_lookahead(value, submitted_value) != 0
    }
}

#[cfg(test)]
mod present_stream_tests {
    use super::present_stream::{marker_lookahead, marker_runs_ahead};

    #[test]
    fn steady_state_marker_is_exactly_one_frame_ahead() {
        // The value is minted per Present and the tag lands on the submission
        // thread one frame behind, so this is the expected shipping reading.
        assert_eq!(marker_lookahead(41, 40), 1);
        assert!(marker_runs_ahead(41, 40));
    }

    #[test]
    fn a_fresh_stream_makes_every_marker_run_ahead() {
        // Registered, nothing tagged yet: the first frame's marker cannot be
        // behind anything, which is why "ahead" is not evidence of forgery.
        assert_eq!(marker_lookahead(1, 0), 1);
        assert!(marker_runs_ahead(1, 0));
    }

    #[test]
    fn already_submitted_or_retired_markers_do_not_run_ahead() {
        assert_eq!(marker_lookahead(40, 40), 0);
        assert!(!marker_runs_ahead(40, 40));
        assert_eq!(marker_lookahead(39, 40), 0);
        assert!(!marker_runs_ahead(39, 40));
    }

    #[test]
    fn a_forged_far_future_value_reads_as_its_own_magnitude() {
        // The whole point of the high-water: a forgery is not distinguishable
        // from a legitimate marker by direction, only by MAGNITUDE.
        assert_eq!(marker_lookahead(u32::MAX, 40), u32::MAX - 40);
        assert_eq!(marker_lookahead(1_000_000, 0), 1_000_000);
    }

    #[test]
    fn behind_by_the_full_range_saturates_instead_of_wrapping() {
        // In the shipped profile the unsaturated form would panic here, and a
        // panic in a DDI is a silent graphics deadlock.
        assert_eq!(marker_lookahead(0, u32::MAX), 0);
        assert_eq!(marker_lookahead(1, u32::MAX), 0);
        assert!(!marker_runs_ahead(0, u32::MAX));
    }
}

/// RECOVERED ASSURANCE, 2026-08-06. These six tests lived in
/// `kmd_render/src/virtio/gpu/mod.rs` as `#[cfg(test)] mod present_stream_tests`
/// — inside a `panic = "abort"` `cdylib` whose `build.rs` runs bindgen and
/// `rc.exe`, i.e. a crate that **cannot host a libtest harness at all**. They had
/// therefore never executed, on any platform, since the day they were written,
/// while covering exactly the boundary/handle helpers the D3D12 fence bridge now
/// depends on. CLAUDE.md's invariant table forbids that shape in `kmd_render`;
/// this is the violation being repaid rather than deleted.
#[cfg(test)]
mod present_stream_boundary_tests {
    use super::present_stream::{
        GENERATION_MAX, MAX_STREAMS, advance_retired, decode_boundary, encode_boundary,
        handle_index, slot_handle, slot_ready, terminal_on_response_ok,
    };

    #[test]
    fn tagged_boundary_round_trips_without_entering_legacy_namespace() {
        let boundary = encode_boundary(0x1234_5678, 77);
        assert_eq!(decode_boundary(boundary), Some((0x1234_5678, 77)));
        // 77 on its own is a legacy exclusive wire watermark, and the two
        // namespaces must never be numerically compared.
        assert_eq!(decode_boundary(77), None);
    }

    #[test]
    fn slot_63_and_new_generation_never_alias() {
        let last_index = MAX_STREAMS - 1;
        let first_handle = slot_handle(1, last_index);
        // Bit 31 stays clear: the tagged-boundary ABI reserves 31 bits.
        assert!(first_handle < (1 << 31));
        assert_ne!(first_handle, slot_handle(2, last_index));
        assert_ne!(slot_handle(1, 0), slot_handle(2, 0));
        // The index occupies the LOW bits, so the highest slot cannot carry into
        // the generation field and impersonate the next generation of slot 0.
        assert_eq!(handle_index(first_handle), Some(last_index));
        assert_eq!(handle_index(slot_handle(2, 0)), Some(0));
    }

    #[test]
    fn retirement_is_monotonic_even_when_completions_reorder() {
        let retired = advance_retired(9, 4);
        assert_eq!(retired, 9);
        assert_eq!(advance_retired(retired, 12), 12);
    }

    #[test]
    fn dead_or_generation_mismatched_stream_boundary_never_reads_as_retired() {
        // live, generation 3, slot 0, retired through 6.
        let handle = slot_handle(3, 0);
        assert!(!slot_ready(true, 3, 0, handle, 7, 6));
        assert!(slot_ready(true, 3, 0, handle, 6, 6));
        // A dead slot is not ready even for a value it has already passed.
        assert!(!slot_ready(false, 3, 0, handle, 1, 6));
        // A handle from another generation is not ready either. Flipping bit 6
        // is the lowest generation bit, i.e. the nearest possible collision.
        assert!(!slot_ready(true, 3, 0, handle ^ (1 << 6), 1, 6));
    }

    #[test]
    fn late_successful_sync_set_retains_its_lifecycle_tag() {
        // `abandon_sync` detaches only the stack waiter; the tag remains on the
        // entry and a later RESP_OK must still advance lifecycle selection.
        let bind = (41u64, 77u32);
        assert_eq!(terminal_on_response_ok(true, Some(bind)), Some(bind));
        assert_eq!(terminal_on_response_ok(false, Some(bind)), None);
        assert_eq!(terminal_on_response_ok::<(u64, u32)>(true, None), None);
    }

    #[test]
    fn generation_ceiling_stays_inside_the_reserved_handle_width() {
        // The registration scan refuses a slot whose generation has reached
        // GENERATION_MAX; the largest handle it can therefore mint must still
        // fit the 31 bits the boundary ABI reserves.
        assert!(slot_handle(GENERATION_MAX, MAX_STREAMS - 1) < (1 << 31));
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
    /// HNF1 magic at offset 0 — `0x31464e48`, i.e. the bytes `H N F 1`
    /// little-endian (section 12.1 line 3138).
    pub const HNF1_MAGIC: u32 = 0x3146_4e48;
    /// HNF1 ABI version at offset 4 (section 12.1 line 3139).
    pub const HNF1_ABI_VERSION: u16 = 1;
    /// HNF1 structure size at offset 6, and the whole record's length. Equal to
    /// the WDK's `D3DDDI_NATIVE_FENCE_PDD_SIZE`; `kmd_render` asserts that
    /// equality against the generated bindings.
    pub const HNF1_SIZE: usize = 64;

    /// Byte offsets of the section-12.1 table. Named so the parser and the
    /// encoder cannot drift from each other.
    pub const OFF_MAGIC: usize = 0;
    /// Offset of the 2-byte ABI version.
    pub const OFF_ABI_VERSION: usize = 4;
    /// Offset of the 2-byte structure size.
    pub const OFF_STRUCT_SIZE: usize = 6;
    /// Offset of the 8-byte atomic package generation.
    pub const OFF_PACKAGE_GENERATION: usize = 8;
    /// Offset of the 8-byte KMD-assigned object generation.
    pub const OFF_OBJECT_GENERATION: usize = 16;
    /// Offset of the 4-byte `D3DDDI_NATIVEFENCE_TYPE`.
    pub const OFF_NATIVE_TYPE: usize = 24;
    /// Offset of the 4-byte flag word.
    pub const OFF_FLAGS: usize = 28;
    /// Offset of the 8-byte creating adapter LUID.
    pub const OFF_ADAPTER_LUID: usize = 32;
    /// Offset of the 24 reserved bytes, which must be zero.
    pub const OFF_RESERVED: usize = 40;
    /// Length of the reserved tail.
    pub const RESERVED_LEN: usize = 24;

    /// HNF1 flags bit 0 — the object is shareable (section 12.1 line 3144).
    pub const HNF1_FLAG_SHARED: u32 = 1 << 0;
    /// Every other flag bit, including cross-adapter, is zero in this
    /// generation (section 12.1 line 3144, section 12.1 item 7).
    pub const HNF1_FLAGS_RESERVED_MASK: u32 = !HNF1_FLAG_SHARED;

    /// `D3DDDI_NATIVEFENCE_TYPE_DEFAULT`.
    pub const NATIVE_FENCE_TYPE_DEFAULT: u32 = 0;
    /// `D3DDDI_NATIVEFENCE_TYPE_INTRA_GPU`.
    pub const NATIVE_FENCE_TYPE_INTRA_GPU: u32 = 1;

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

    /// Why a PDD was refused. Every variant is a distinct counted refusal in
    /// `kmd_render`; none of them is ever silently repaired.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum PddReject {
        /// Offset 0 is not [`HNF1_MAGIC`].
        Magic,
        /// Offset 4 is not [`HNF1_ABI_VERSION`]. There is no version fallback
        /// (section 3: "no version or feature fallback").
        AbiVersion,
        /// Offset 6 is not [`HNF1_SIZE`].
        StructSize,
        /// Offset 8 is not this package's generation.
        PackageGeneration,
        /// Offset 16 was nonzero on the way in. The object generation is
        /// KMD-assigned; a caller-supplied value is a forged identity attempt.
        ObjectGenerationNotZero,
        /// Offset 24 is not a documented `D3DDDI_NATIVEFENCE_TYPE`.
        NativeType,
        /// Offset 24 disagrees with the type the OS passed in the DDI argument.
        NativeTypeMismatch,
        /// Offset 28 has a bit set outside [`HNF1_FLAG_SHARED`].
        Flags,
        /// Offset 32 is neither zero nor the exact creating adapter LUID.
        AdapterLuid,
        /// The 24 reserved bytes at offset 40 are not all zero.
        Reserved,
    }

    /// The parsed HNF1 payload. Pointer-free by construction — the record
    /// carries no pointer, NT handle, PID, GPUVA, host token, or allocation ID
    /// (section 12.1 line 3152).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Hnf1 {
        /// Offset 8.
        pub package_generation: u64,
        /// Offset 16.
        pub object_generation: u64,
        /// Offset 24.
        pub native_type: u32,
        /// Offset 28.
        pub flags: u32,
        /// Offset 32.
        pub adapter_luid: i64,
    }

    fn rd_u16(b: &[u8; HNF1_SIZE], off: usize) -> u16 {
        u16::from_le_bytes([b[off], b[off + 1]])
    }

    fn rd_u32(b: &[u8; HNF1_SIZE], off: usize) -> u32 {
        u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
    }

    fn rd_u64(b: &[u8; HNF1_SIZE], off: usize) -> u64 {
        let mut v = [0u8; 8];
        let mut i = 0;
        while i < 8 {
            v[i] = b[off + i];
            i += 1;
        }
        u64::from_le_bytes(v)
    }

    /// Decode the record without judging it. Used by both validators and by the
    /// diagnostics path; every caller that acts on the contents goes through
    /// [`validate_create`] or [`validate_open`] first.
    pub fn parse(bytes: &[u8; HNF1_SIZE]) -> Hnf1 {
        Hnf1 {
            package_generation: rd_u64(bytes, OFF_PACKAGE_GENERATION),
            object_generation: rd_u64(bytes, OFF_OBJECT_GENERATION),
            native_type: rd_u32(bytes, OFF_NATIVE_TYPE),
            flags: rd_u32(bytes, OFF_FLAGS),
            adapter_luid: rd_u64(bytes, OFF_ADAPTER_LUID) as i64,
        }
    }

    /// The header checks both create and open share: magic, ABI version,
    /// structure size, package generation, a zero object generation, a flag
    /// word with no undefined bit, and a zero reserved tail.
    fn validate_header(
        bytes: &[u8; HNF1_SIZE],
        package_generation: u64,
    ) -> Result<Hnf1, PddReject> {
        if rd_u32(bytes, OFF_MAGIC) != HNF1_MAGIC {
            return Err(PddReject::Magic);
        }
        if rd_u16(bytes, OFF_ABI_VERSION) != HNF1_ABI_VERSION {
            return Err(PddReject::AbiVersion);
        }
        if rd_u16(bytes, OFF_STRUCT_SIZE) as usize != HNF1_SIZE {
            return Err(PddReject::StructSize);
        }
        let parsed = parse(bytes);
        if parsed.package_generation != package_generation {
            return Err(PddReject::PackageGeneration);
        }
        if parsed.object_generation != 0 {
            return Err(PddReject::ObjectGenerationNotZero);
        }
        if parsed.flags & HNF1_FLAGS_RESERVED_MASK != 0 {
            return Err(PddReject::Flags);
        }
        let mut i = 0;
        while i < RESERVED_LEN {
            if bytes[OFF_RESERVED + i] != 0 {
                return Err(PddReject::Reserved);
            }
            i += 1;
        }
        Ok(parsed)
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
        let parsed = validate_header(bytes, package_generation)?;
        if !native_type_is_documented(parsed.native_type) {
            return Err(PddReject::NativeType);
        }
        if parsed.native_type != ddi_native_type {
            return Err(PddReject::NativeTypeMismatch);
        }
        if parsed.adapter_luid != 0 && parsed.adapter_luid != adapter_luid {
            return Err(PddReject::AdapterLuid);
        }
        Ok(parsed)
    }

    /// Validate a `DxgkDdiOpenNativeFence` PDD.
    ///
    /// Deliberately weaker than [`validate_create`] on type and LUID: section
    /// 12.1 item 2 puts that comparison on the *opening UMD* ("UMD validates
    /// HNF1/package/LUID/type and stores only the returned local state"), and
    /// the KMD overwrites both fields from the global object before returning.
    /// Requiring the opener to pre-state them would invent a contract the
    /// reference does not have.
    pub fn validate_open(
        bytes: &[u8; HNF1_SIZE],
        package_generation: u64,
    ) -> Result<Hnf1, PddReject> {
        validate_header(bytes, package_generation)
    }

    /// Whether `native_type` is one of the documented `D3DDDI_NATIVEFENCE_TYPE`
    /// values (section 12.1 line 3143).
    pub fn native_type_is_documented(native_type: u32) -> bool {
        native_type == NATIVE_FENCE_TYPE_DEFAULT || native_type == NATIVE_FENCE_TYPE_INTRA_GPU
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
        let mut out = [0u8; HNF1_SIZE];
        out[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&HNF1_MAGIC.to_le_bytes());
        out[OFF_ABI_VERSION..OFF_ABI_VERSION + 2].copy_from_slice(&HNF1_ABI_VERSION.to_le_bytes());
        out[OFF_STRUCT_SIZE..OFF_STRUCT_SIZE + 2]
            .copy_from_slice(&(HNF1_SIZE as u16).to_le_bytes());
        out[OFF_PACKAGE_GENERATION..OFF_PACKAGE_GENERATION + 8]
            .copy_from_slice(&package_generation.to_le_bytes());
        out[OFF_OBJECT_GENERATION..OFF_OBJECT_GENERATION + 8]
            .copy_from_slice(&object_generation.to_le_bytes());
        out[OFF_NATIVE_TYPE..OFF_NATIVE_TYPE + 4].copy_from_slice(&native_type.to_le_bytes());
        out[OFF_FLAGS..OFF_FLAGS + 4].copy_from_slice(&flags.to_le_bytes());
        out[OFF_ADAPTER_LUID..OFF_ADAPTER_LUID + 8]
            .copy_from_slice(&(adapter_luid as u64).to_le_bytes());
        out
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
        /// The object is already draining or dead.
        NotLive,
    }

    /// The observable state of one global native-fence object.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum FenceState {
        /// Created and usable.
        Live,
        /// Destroy has been requested but local references remain.
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
        pub epoch: u32,
        /// The KMD-assigned nonzero diagnostic/stale-validation generation.
        pub object_generation: u64,
    }

    impl GlobalFence {
        /// A freshly created object.
        pub fn new(epoch: u32, object_generation: u64) -> Self {
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
    pub fn epoch_is_current(object_epoch: u32, adapter_epoch: u32) -> bool {
        object_epoch == adapter_epoch
    }

    /// Take a local reference on a global object (`DxgkDdiOpenNativeFence`).
    pub fn open_local(fence: GlobalFence, adapter_epoch: u32) -> Result<GlobalFence, Refusal> {
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
    /// early destroy moves to [`FenceState::Draining`] and is refused with
    /// [`Refusal::LocalReferencesOutstanding`]. Leaking a bounded object is the
    /// fail-closed choice against a use-after-free in a DDI.
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
    /// diagnostics. Saturating rather than wrapping: reaching `u64::MAX` would
    /// otherwise wrap to 0, which is the "UMD supplied it" sentinel.
    pub fn next_object_generation(previous: u64) -> u64 {
        previous.saturating_add(1).max(1)
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
    /// satisfied. All four are conjunctive; a single false makes the whole
    /// surface refuse rather than partially advertise.
    pub fn surface_is_admitted(
        wddm_3_2_reported: bool,
        os_enabled_feature: bool,
        adapter_luid_known: bool,
        native_gpu_fence_cap: bool,
    ) -> bool {
        wddm_3_2_reported && os_enabled_feature && adapter_luid_known && native_gpu_fence_cap
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
    fn open_validates_the_header_but_not_type_or_luid() {
        // Section 12.1 item 2 puts type/LUID validation on the opening UMD,
        // after the KMD writes them back from the global object.
        let b = encode(PKG, 0, 999, 0, LUID ^ 1);
        assert!(validate_open(&b, PKG).is_ok());
        assert_eq!(validate_open(&b, PKG ^ 1), Err(PddReject::PackageGeneration));
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
        assert_eq!(f.state, FenceState::Draining, "one reference still holds it");
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
        assert_eq!(next_object_generation(0), 1);
        assert_eq!(next_object_generation(1), 2);
        // Saturation, not wrap: 0 is the "supplied by the UMD" sentinel and must
        // never be handed back as an assigned generation.
        assert_eq!(next_object_generation(u64::MAX), u64::MAX);
        assert_ne!(next_object_generation(u64::MAX), 0);
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
        assert!(surface_is_admitted(true, true, true, true));
        for i in 0..4 {
            let g = |n: usize| n != i;
            assert!(
                !surface_is_admitted(g(0), g(1), g(2), g(3)),
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
        HELIOS_HWA2_FLAG_KMD_OWNED_MASK, HELIOS_HWA2_MAGIC, HELIOS_SEGMENT_ID_HLM1,
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
        /// HVM1 placement names [`HELIOS_SEGMENT_ID_HLM1`] for every role and
        /// the segment is not exposed yet (K2 is blocked; `FINDINGS.md` F5
        /// parked the QEMU/HPM1 half). Refused and counted per role — never
        /// substituted onto the aperture segment, which `K4-CONTRACT.md` §4
        /// forbids outright.
        Hlm1SegmentNotExposed {
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
        pub fn create(counter: GenerationCounter) -> Result<(Self, GenerationCounter), IdentityRefusal> {
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

    /// The role's placement, or a counted refusal while HLM1 does not exist.
    ///
    /// `Hvm1Role::placement()` names [`HELIOS_SEGMENT_ID_HLM1`] as the
    /// preferred read/write segment for **every** role, and §10.7 makes HLM1's
    /// exposure conditional on `DxgkDdiStartDevice` negotiating HPM1 first.
    /// `FINDINGS.md` F5 parked the host half of that negotiation, so the
    /// condition cannot be satisfied today.
    ///
    /// ⚠ `K4-CONTRACT.md` §4 states the rule for role 4 — "admitted and
    /// counted, not satisfied … never a silent substitution onto the aperture
    /// segment". This model applies it to all four roles, because the
    /// substitution it forbids is a property of the placement record and not of
    /// role 4: every role's `preferred_segment` is HLM1, so satisfying any of
    /// them without HLM1 means substituting. The counter carries the role, so
    /// the role-4 case the contract names stays separable in telemetry.
    pub fn hvm1_admit_placement(
        role: Hvm1Role,
        hlm1_segment_exposed: bool,
    ) -> Result<Hvm1Placement, IdentityRefusal> {
        let placement = role.placement();
        if placement.preferred_segment == HELIOS_SEGMENT_ID_HLM1 && !hlm1_segment_exposed {
            return Err(IdentityRefusal::Hlm1SegmentNotExposed {
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
            Ok((state, Self { live_extents, ..self }))
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
    pub fn encode_hoc1(record: &HeliosOuterCommandAllocationV1) -> [u8; HELIOS_HOC1_BYTES as usize] {
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
        let output = hwa2_stamp_create_output(
            &input,
            0x51,
            HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE,
            PKG,
        )
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
        let output = hwa2_stamp_create_output(
            &input,
            0x51,
            HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE,
            PKG,
        )
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
        assert_eq!(HELIOS_HVM1_REPLY_POOL_BYTES, 64 * 1024 * 1024);
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

        let (admitted, role) =
            hvm1_admit_create_input(&encode_hvm1(&input), PKG).expect("admit");
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

    /// Placement is refused, per role and by name, until HLM1 exists. K2 is
    /// blocked (`FINDINGS.md` F5), and `K4-CONTRACT.md` §4 forbids substituting
    /// the aperture segment: the create fails loudly instead.
    #[test]
    fn hvm1_placement_is_refused_until_the_hlm1_segment_exists() {
        for role in [
            Hvm1Role::ReplyPool,
            Hvm1Role::VulkanHostVisible,
            Hvm1Role::Feedback,
            Hvm1Role::VulkanDeviceLocal,
        ] {
            assert_eq!(
                hvm1_admit_placement(role, false),
                Err(IdentityRefusal::Hlm1SegmentNotExposed {
                    role: role.to_u32()
                }),
                "role {} must be counted, not substituted",
                role.to_u32()
            );
            let placement = hvm1_admit_placement(role, true).expect("HLM1 present");
            assert_eq!(placement.preferred_segment, HELIOS_SEGMENT_ID_HLM1);
            assert_ne!(HELIOS_SEGMENT_ID_HLM1, HELIOS_SEGMENT_ID_APERTURE);
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
            let mut aperture = faithful;
            aperture.preferred_segment = HELIOS_SEGMENT_ID_APERTURE;
            assert_eq!(
                hvm1_written_flags_match(&placement, &aperture),
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
            let (state, next) = pool.reserve(offset, 4096).expect("reservation inside the pool");
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
            pool.reserve(HELIOS_HOC1_POOL_BYTES - u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT), 65537),
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
        Hvm1Role, HELIOS_HNR2_ACCESS_WRITE, HELIOS_HVM1_REPLY_POOL_BYTES,
        HELIOS_HVM1_REPLY_SLOT_BYTES, HELIOS_HVM1_REPLY_SLOT_COUNT,
    };
    use helios_protocol::native_render::{
        admit_snapshot, Hnr2CapacityRefusal, HELIOS_HVR1_MAX_SNAPSHOT_BYTES,
    };
    use helios_protocol::translation_session::{
        admit_host_dispatch_enqueue, admit_new_session, admit_ring_index, check_generation_match,
        AttachRefusal, CapabilityRefusal, GenerationField, GenerationRefusal, HeliosAttachAdmission,
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
        /// The allocation offered as a reply pool is not exactly 64 MiB.
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
        /// An endpoint was reached before a host ring index was bound to it. The
        /// binder is K6/K11 — see [`TranslationSession::bind_ring`].
        EndpointRingUnassigned { endpoint_id: u32 },
        /// A host ring index is already bound, to this endpoint or another one.
        /// Invariant 12 forbids recycling a ring while any job or reference lives.
        EndpointRingAlreadyTaken { ring_index: u32 },
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

            // ⛔ No ring index is minted here. An endpoint ordinal is not a ring
            // (`translation_session.rs:632-635`), and line 1727 gives the ring to a
            // *queue context* bound to a real VkQueue — which a record-only session
            // never creates. Minting one per granted ordinal would burn the 255-entry
            // wire ceiling on endpoints that never materialise. [`Self::bind_ring`]
            // is the seam.
            self.session_generation = admission.session_generation;
            self.capability = admission.capability;
            self.endpoint_capacity = admission.endpoint_capacity;
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

        /// Bind the unique nonzero host `INFO_RING_IDX` for `endpoint_id`, once.
        ///
        /// CROSS-LANE SEAM: the caller is whichever of K6/K11 creates the real
        /// host queue — line 1727 gives the ring to a queue context bound to a
        /// real VkQueue, and nothing in K5 creates one. Rebinding is refused
        /// because invariant 12 forbids recycling a ring before every endpoint
        /// job and context reference retires.
        pub fn bind_ring(&mut self, endpoint_id: u32, ring_index: u32) -> Result<(), SessionRefusal> {
            if self.phase == SessionPhase::Draining {
                return Err(SessionRefusal::SessionDraining);
            }
            admit_ring_index(ring_index).map_err(SessionRefusal::Capacity)?;
            if ring_index == 0 {
                // Ring 0 is the CPU/decode-only control ring; no endpoint owns it.
                return Err(SessionRefusal::EndpointRingUnassigned { endpoint_id });
            }
            let capacity = self.endpoint_capacity;
            if endpoint_id == 0 || endpoint_id > capacity {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity,
                });
            }
            let mut taken = false;
            let mut i = 0usize;
            while i < ENDPOINT_SLOTS {
                if let Some(other) = self.endpoints.get(i) {
                    if other.ring_index == ring_index && i != (endpoint_id - 1) as usize {
                        taken = true;
                    }
                }
                i += 1;
            }
            if taken {
                return Err(SessionRefusal::EndpointRingAlreadyTaken { ring_index });
            }
            let Some(endpoint) = self.endpoints.get_mut((endpoint_id - 1) as usize) else {
                return Err(SessionRefusal::EndpointOutOfRange {
                    found: endpoint_id,
                    capacity,
                });
            };
            if endpoint.ring_index != 0 {
                return Err(SessionRefusal::EndpointRingAlreadyTaken {
                    ring_index: endpoint.ring_index,
                });
            }
            endpoint.ring_index = ring_index;
            Ok(())
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

            let index = Self::slot_index(request.reply_offset)?;
            let slot = pool.slots[index];
            if slot.state != SlotState::Idle {
                return Err(SessionRefusal::ControlRenderSlotBusy {
                    in_flight: slot.generation,
                });
            }
            if request.reply_slot_generation <= slot.retired_generation {
                return Err(SessionRefusal::ControlRenderSlotGenerationStale {
                    found: request.reply_slot_generation,
                    watermark: slot.retired_generation,
                });
            }

            let Some(pool_mut) = self.pool.as_mut() else {
                return Err(SessionRefusal::ReplyPoolNotBound);
            };
            pool_mut.slots[index] = ReplySlot {
                state: SlotState::InFlight,
                generation: request.reply_slot_generation,
                retired_generation: slot.retired_generation,
                owner_context_generation: request.owner_context_generation,
                batch_token: request.batch_token,
                reply_offset: request.reply_offset,
                reply_capacity_bytes: request.reply_capacity_bytes,
                c51_value: 0,
            };
            Ok(ControlRenderAdmission {
                slot_index: Some(index),
                slot_generation: request.reply_slot_generation,
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
    pub fn check_session_generation(found: u64, session: &TranslationSession) -> Result<(), SessionRefusal> {
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
                s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES / 2, POOL_GEN),
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
            s.attach(&attach_packet(1, 10)).expect("declares endpoint 1");
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
            s.attach(&second).expect("endpoint 2 is its own declaration");
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
            s.declare_endpoint(endpoint(1)).expect("re-declares identically");
            let conflicting =
                HeliosTranslationEndpointV1::new(1, HeliosEngineClass::Copy, 0, 0);
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
            s.attach(&attach_packet(1, 1)).expect("still admits a good packet");
        }

        #[test]
        fn the_release_paths_deliberately_do_not_gate_on_draining() {
            // detach / retire_host_dispatch / release_snapshot_bytes must keep
            // working while a session drains: `dxgkddi_destroy_context` calls
            // `detach` for attached contexts AFTER the control context has already
            // begun draining, and dxgkrnl does not order the two.
            let mut s = live_session(4);
            s.bind_ring(1, 1).unwrap();
            s.attach(&attach_packet(1, 10)).unwrap();
            s.enqueue_host_dispatch(1).unwrap();
            s.admit_snapshot_bytes(64).unwrap();

            s.begin_draining();
            s.detach(10).expect("an attached context still detaches");
            s.retire_host_dispatch(1).expect("an enqueued DMA still retires");
            s.release_snapshot_bytes(64).expect("a live snapshot still releases");
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
        fn draining_never_marks_a_reply_complete() {
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
        }

        // ── the control-context Render ────────────────────────────────────

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
                    slot_generation: 0
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
                Err(SessionRefusal::ControlRenderSlotIndexOutOfRange {
                    found: REPLY_SLOTS
                })
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
                Err(SessionRefusal::ControlRenderSlotIndexOutOfRange {
                    found: REPLY_SLOTS
                })
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
        fn init_mints_no_ring_at_all() {
            // An endpoint ordinal is not a ring. Rings belong to queue contexts
            // bound to a real VkQueue, which a record-only session never creates.
            let s = live_session(4);
            for id in 1..=4u32 {
                assert_eq!(
                    s.ring_index(id),
                    Err(SessionRefusal::EndpointRingUnassigned { endpoint_id: id })
                );
            }
        }

        #[test]
        fn a_ring_binds_once_and_is_never_shared_or_recycled() {
            let mut s = live_session(4);
            s.bind_ring(1, 7).expect("first bind");
            assert_eq!(s.ring_index(1), Ok(7));
            assert_eq!(
                s.bind_ring(1, 8),
                Err(SessionRefusal::EndpointRingAlreadyTaken { ring_index: 7 })
            );
            assert_eq!(
                s.bind_ring(2, 7),
                Err(SessionRefusal::EndpointRingAlreadyTaken { ring_index: 7 })
            );
            s.bind_ring(2, 8).expect("its own ring");
            assert_eq!(
                s.bind_ring(3, 0),
                Err(SessionRefusal::EndpointRingUnassigned { endpoint_id: 3 })
            );
            assert!(matches!(
                s.bind_ring(3, u32::MAX),
                Err(SessionRefusal::Capacity(HeliosCapacityRefusal {
                    limit: HeliosCapacityLimit::RingIndex,
                    ..
                }))
            ));
        }

        #[test]
        fn a_draining_session_binds_no_ring() {
            let mut s = live_session(4);
            s.begin_draining();
            assert_eq!(s.bind_ring(1, 7), Err(SessionRefusal::SessionDraining));
        }

        #[test]
        fn a_provisional_session_has_no_rings_and_no_endpoints() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            assert_eq!(
                s.bind_ring(1, 1),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 1,
                    capacity: 0
                })
            );
        }

        #[test]
        fn only_the_granted_endpoints_may_take_a_ring() {
            let mut s = TranslationSession::new_provisional(PKG, CAPSET);
            s.bind_reply_pool(Hvm1Role::ReplyPool, HELIOS_HVM1_REPLY_POOL_BYTES, POOL_GEN)
                .unwrap();
            s.complete_init(8, 2, 7, CAP).unwrap();
            s.bind_ring(1, 1).expect("granted");
            s.bind_ring(2, 2).expect("granted");
            assert_eq!(
                s.bind_ring(3, 3),
                Err(SessionRefusal::EndpointOutOfRange {
                    found: 3,
                    capacity: 2
                })
            );
        }

        #[test]
        fn host_dispatch_serials_are_arrival_order_and_per_endpoint() {
            let mut s = live_session(4);
            s.bind_ring(1, 1).unwrap();
            s.bind_ring(2, 2).unwrap();
            let a1 = s.enqueue_host_dispatch(1).unwrap();
            let a2 = s.enqueue_host_dispatch(1).unwrap();
            let b1 = s.enqueue_host_dispatch(2).unwrap();
            assert!(a2 > a1, "same endpoint must be strictly increasing");
            assert_eq!(b1, a1, "a second endpoint starts its own sequence");
        }

        #[test]
        fn the_host_dispatch_fifo_refuses_at_its_bound_and_never_waits() {
            let mut s = live_session(4);
            s.bind_ring(1, 1).unwrap();
            s.bind_ring(2, 2).unwrap();
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
            s.enqueue_host_dispatch(1).expect("and the slot is reusable");
            // Endpoint 2 is untouched by endpoint 1's exhaustion.
            s.enqueue_host_dispatch(2).expect("independent FIFO");
        }

        #[test]
        fn host_dispatch_refuses_an_endpoint_with_no_ring_bound() {
            let mut s = live_session(4);
            assert_eq!(
                s.enqueue_host_dispatch(1),
                Err(SessionRefusal::EndpointRingUnassigned { endpoint_id: 1 })
            );
        }

        #[test]
        fn retiring_an_empty_fifo_is_a_refusal_not_a_wrap() {
            let mut s = live_session(4);
            s.bind_ring(1, 1).unwrap();
            assert_eq!(
                s.retire_host_dispatch(1),
                Err(SessionRefusal::HostDispatchFifoUnderflow { endpoint_id: 1 })
            );
        }

        #[test]
        fn a_draining_session_enqueues_nothing() {
            let mut s = live_session(4);
            s.bind_ring(1, 1).unwrap();
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
    const _: () = assert!(
        CAPABILITY_RECORD_BYTES as usize == core::mem::size_of::<Hnr2PhysicalCapability>()
    );

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
            return Err(RenderRefusal::Dma(
                Hnr2DmaReject::OutputPatchCountTooLarge,
            ));
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use helios_protocol::native_render::{
            HELIOS_HNR2_ABI_VERSION, HELIOS_HNR2_ACCESS_READ, HELIOS_HNR2_ACCESS_WRITE,
            HELIOS_HNR2_FLAG_BEGIN, HELIOS_HNR2_FLAG_COMMIT, HELIOS_HNR2_HEADER_SIZE,
            HELIOS_HNR2_MAGIC, HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS,
            HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX, HELIOS_HNR2_SLOT_POOL_BYTES,
            HELIOS_HNR2_USE_RECORD_SIZE, HELIOS_HVC1_DMA_BUFFER_BYTES,
        };

        const PKG: u64 = 0x4845_4C49_0000_0001;

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

        fn use_env(header: &HeliosNativeRenderV2) -> RenderEnv {
            let table = header.use_record_count * HELIOS_HNR2_USE_RECORD_SIZE;
            RenderEnv {
                package_generation: PKG,
                allocation_list_count: header.use_record_count,
                command_length: HELIOS_HNR2_HEADER_SIZE as u32
                    + table
                    + header.fragment_payload_bytes,
                command_buffer_bytes: HELIOS_HVC1_DMA_BUFFER_BYTES as u32,
                patch_location_list_in_size: 0,
            }
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
            assert_eq!(ctx, before, "a refused fragment must not move the assembler");

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
            let uses = [use_record(0, false), use_record(1, false), use_record(2, true)];
            // Only the LAST entry disagrees, so a loop that stops early passes.
            assert_eq!(
                admit_commit_tables(&h, &uses, &[], 3, &[false, false, false]).unwrap_err(),
                RenderRefusal::Table(Hnr2TableReject::WriteOperationMismatch)
            );
        }

        #[test]
        fn the_patch_plan_is_one_slot_per_use_and_is_repeatable() {
            let uses = [use_record(0, false), use_record(1, true), use_record(2, false)];
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
            let mut push = |slot: &mut [u32; TOTAL], at: &mut usize, code: u32| {
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
            assert_eq!(snapshot_placement(&mut cap, 2, 0x5000, bytes), Ok(true));
            assert_eq!(cap.segment_id, 2);
            assert_eq!(cap.physical_address, 0x5000);
            assert_eq!(
                apply_placement(&mut cap, 2, 0x9000, bytes).unwrap_err(),
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
            let plan =
                plan_capability_table(TABLE_BASE, 3, HELIOS_HVC1_DMA_BUFFER_BYTES).unwrap();
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
                RenderRefusal::OuterSubmit(
                    HeliosOuterSubmitRejection::NotD3D12VirtualContext {
                        context_flags: D3D11_PHYSICAL
                    }
                )
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

/// K2a — the HLM1 CPU-view binding, as rules rather than as `DXGKARG_*` reads.
///
/// The KMD owns a venus blob per HVM1 allocation and VidMm owns the segment
/// offset it is placed at; the binding maps the one at the other so the guest's
/// `D3DKMTLock2` view and the blob are the same bytes. `FINDINGS.md` F14 records
/// why that is not true today. Everything here is a function of its arguments —
/// the platform half's only job is to fill [`Observation`] from the union
/// `PagingOperation::parse` already resolves.
pub mod hlm1_placement {
    use helios_protocol::HELIOS_SEGMENT_ID_HLM1;

    /// §10.7 fixes HLM1's page granularity at `HELIOS_HVM1_SEGMENT_PAGE_SHIFT`.
    pub const PAGE_BYTES: u64 = 1 << helios_protocol::HELIOS_HVM1_SEGMENT_PAGE_SHIFT;

    /// `DXGK_BUILDPAGINGBUFFER_OPERATION` ordinals, as rules about which
    /// operations can carry a placement at all.
    ///
    /// ⛔ Declared here rather than imported because this crate has no `dxgk`
    /// binding by construction. `kmd_render` static-asserts each against the
    /// generated enum, so a kit that renumbered one is a compile error there and
    /// not a silent misclassification here.
    pub mod op {
        pub const TRANSFER: u32 = 0;
        pub const FILL: u32 = 1;
        pub const DISCARD_CONTENT: u32 = 2;
        pub const MAP_APERTURE_SEGMENT: u32 = 5;
        pub const VIRTUAL_TRANSFER: u32 = 8;
        pub const VIRTUAL_FILL: u32 = 9;
        pub const UPDATE_PAGE_TABLE: u32 = 11;
        pub const NOTIFY_RESIDENCY: u32 = 15;
        pub const MAP_APERTURE_SEGMENT2: u32 = 17;
        pub const NOTIFY_RESIDENCY2: u32 = 21;
        /// ⛔ 23/24/25 exist in WDK 28000 and are NOT the virtual content ops.
        /// `protocol/src/physical_memory.rs` records §10.7's TRANSFER2/FILL2 as
        /// "= VIRTUAL_TRANSFER/VIRTUAL_FILL, not new ordinals"; the shipping kit
        /// falsifies that, and FILL2 is the only op in the set carrying a bare
        /// (SegmentId, SegmentAddress) pair.
        pub const TRANSFER2: u32 = 23;
        pub const FILL2: u32 = 24;
        pub const DISCARD_CONTENT2: u32 = 25;
    }

    /// Whether this operation's descriptor names a segment AND an offset within
    /// it. The virtual content operations do not — they carry GPU virtual
    /// addresses only, which is why `PgDi` moving proves nothing about placement
    /// (F14's correction).
    pub const fn carries_placement(operation: u32) -> bool {
        matches!(
            operation,
            op::TRANSFER
                | op::FILL
                | op::UPDATE_PAGE_TABLE
                | op::NOTIFY_RESIDENCY
                | op::NOTIFY_RESIDENCY2
                | op::MAP_APERTURE_SEGMENT
                | op::MAP_APERTURE_SEGMENT2
                | op::TRANSFER2
                | op::FILL2
        )
    }

    /// One placement-bearing observation, already lifted out of the union.
    ///
    /// `byte_offset` is segment-relative and in BYTES: `UPDATE_PAGE_TABLE`
    /// reports pages and the platform half converts, because a page number and a
    /// byte offset are the same integer for the first 4 KiB and diverge silently
    /// afterwards.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Observation {
        pub operation: u32,
        pub segment_id: u32,
        pub byte_offset: u64,
        pub length_bytes: u64,
    }

    /// An admitted HLM1 placement.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Placement {
        pub byte_offset: u64,
        pub length_bytes: u64,
    }

    /// Why an observation did not yield a binding target.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum PlacementRefusal {
        /// The operation cannot carry a placement — see [`carries_placement`].
        NotPlacementBearing { operation: u32 },
        /// A segment this allocation is not bound to. Not a defect on its own:
        /// `hvm1_placement` keeps the aperture in the supported set, so VidMm may
        /// legally place elsewhere — and then no CPU view exists to bind.
        ForeignSegment { found: u32 },
        Unaligned { found: u64 },
        ZeroLength,
        /// `byte_offset + length_bytes` does not fit a `u64`.
        RangeOverflow,
        /// The range leaves the window partition VidMm owns.
        ExceedsReserve { end: u64, reserve: u64 },
    }

    /// Why an offset is not a legal fixed-map target.
    ///
    /// Split from [`PlacementRefusal`] because `map_blob_at` applies it to an
    /// offset that has already been admitted once: it is the check that stands
    /// between a VidMm placement and a `RESOURCE_MAP_BLOB` overlapping the KMD's
    /// own allocator partition.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum OffsetRefusal {
        Unaligned { found: u64 },
        ZeroLength,
        RangeOverflow,
        ExceedsReserve { end: u64, reserve: u64 },
    }

    /// The VidMm partition is `[0, reserve)`; the KMD's own blob allocator hands
    /// out `[reserve, window_len)`. A fixed map that crosses the mark would put
    /// two allocators in one range with no arbiter.
    pub const fn admissible_window_offset(
        byte_offset: u64,
        length_bytes: u64,
        reserve: u64,
    ) -> Result<(), OffsetRefusal> {
        if length_bytes == 0 {
            return Err(OffsetRefusal::ZeroLength);
        }
        if byte_offset % PAGE_BYTES != 0 {
            return Err(OffsetRefusal::Unaligned { found: byte_offset });
        }
        let Some(end) = byte_offset.checked_add(length_bytes) else {
            return Err(OffsetRefusal::RangeOverflow);
        };
        if end > reserve {
            return Err(OffsetRefusal::ExceedsReserve { end, reserve });
        }
        Ok(())
    }

    /// Admit an observation as an HLM1 placement, or say exactly why not.
    pub const fn admit(
        observation: Observation,
        reserve: u64,
    ) -> Result<Placement, PlacementRefusal> {
        if !carries_placement(observation.operation) {
            return Err(PlacementRefusal::NotPlacementBearing {
                operation: observation.operation,
            });
        }
        if observation.segment_id != HELIOS_SEGMENT_ID_HLM1 {
            return Err(PlacementRefusal::ForeignSegment {
                found: observation.segment_id,
            });
        }
        match admissible_window_offset(observation.byte_offset, observation.length_bytes, reserve) {
            Ok(()) => Ok(Placement {
                byte_offset: observation.byte_offset,
                length_bytes: observation.length_bytes,
            }),
            Err(OffsetRefusal::ZeroLength) => Err(PlacementRefusal::ZeroLength),
            Err(OffsetRefusal::Unaligned { found }) => Err(PlacementRefusal::Unaligned { found }),
            Err(OffsetRefusal::RangeOverflow) => Err(PlacementRefusal::RangeOverflow),
            Err(OffsetRefusal::ExceedsReserve { end, reserve }) => {
                Err(PlacementRefusal::ExceedsReserve { end, reserve })
            }
        }
    }

    /// Sentinel for "no binding", matching `AllocationContext::bar_placed`'s.
    pub const UNBOUND: u64 = u64::MAX;

    /// What the platform half must do about an admitted placement.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Action {
        /// Already mapped there. The host round-trip must not be re-issued.
        None,
        Bind { to: u64 },
        /// The old mapping is torn down by `map_blob_at` itself; the pair is
        /// carried so the caller can count a move rather than a first bind.
        Rebind { from: u64, to: u64 },
    }

    /// The per-allocation binding state. One `u64`, so the platform half can
    /// hold it in the `AtomicU64` it already uses for `bar_placed`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct BindingState(pub u64);

    impl BindingState {
        pub const fn unbound() -> Self {
            Self(UNBOUND)
        }
        pub const fn bound_offset(self) -> Option<u64> {
            if self.0 == UNBOUND {
                None
            } else {
                Some(self.0)
            }
        }
        pub const fn observe(self, placement: Placement) -> Action {
            match self.0 {
                UNBOUND => Action::Bind {
                    to: placement.byte_offset,
                },
                current if current == placement.byte_offset => Action::None,
                current => Action::Rebind {
                    from: current,
                    to: placement.byte_offset,
                },
            }
        }
        /// Apply the action the caller actually completed. Deliberately separate
        /// from [`Self::observe`]: a `map_blob_at` that failed must not advance
        /// the state, or the next observation reports `None` and skips the retry.
        pub const fn applied(self, action: Action) -> Self {
            match action {
                Action::None => self,
                Action::Bind { to } | Action::Rebind { to, .. } => Self(to),
            }
        }
    }

    // ── the verify vocabulary ────────────────────────────────────────────────
    //
    // ONE declaration, consumed by the KMD in Rust and by the probe through the
    // generated C header. Two implementations of a comparison function is how a
    // self-round-trip gets reintroduced.

    /// The byte the GUEST writes at `index` for `nonce`.
    ///
    /// Position- and nonce-dependent so a truncated, shifted or fabricated
    /// readback cannot match: a digest over these bytes changes if any sample is
    /// wrong, missing, or in the wrong order.
    pub const fn verify_byte(nonce: u64, index: u64) -> u8 {
        let mut h = nonce ^ 0x9E37_79B9_7F4A_7C15;
        h = h.wrapping_add(index.wrapping_mul(0xBF58_476D_1CE4_E5B9));
        h ^= h >> 31;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^= h >> 29;
        (h & 0xFF) as u8
    }

    /// The byte the KMD writes at `index` for `nonce`.
    ///
    /// ⛔ `!verify_byte` and not a second hash: this must differ from the guest's
    /// byte at EVERY index, so "the guest reads the stamp" cannot be satisfied by
    /// the guest's own writes for any nonce. A salted hash would collide at ~1
    /// index in 256.
    pub const fn stamp_byte(nonce: u64, index: u64) -> u8 {
        !verify_byte(nonce, index)
    }

    /// How many samples [`sample_offsets`] can produce.
    pub const MAX_SAMPLES: usize = 9;

    /// Deterministic sample positions across `length_bytes`, ending on the last
    /// byte.
    ///
    /// Bounded rather than exhaustive because the HLM1 view is `Cached=0`: WC/UC
    /// kernel reads were measured at ~157 MB/s, so digesting a 64 MiB pool would
    /// be a ~400 ms PASSIVE escape.
    pub fn sample_offsets(length_bytes: u64, out: &mut [u64; MAX_SAMPLES]) -> usize {
        if length_bytes == 0 {
            return 0;
        }
        let last = length_bytes - 1;
        let mut n = 0;
        let mut i = 0u64;
        while i < 8 {
            let offset = length_bytes / 8 * i;
            if n == 0 || out[n - 1] < offset {
                out[n] = offset;
                n += 1;
            }
            i += 1;
        }
        if out[n - 1] < last {
            out[n] = last;
            n += 1;
        }
        n
    }

    /// Position-sensitive digest of the sampled bytes.
    ///
    /// FNV-1a over `(offset, byte)` pairs: swapping two samples changes it, which
    /// a digest over the bytes alone would not.
    pub fn digest(nonce: u64, samples: &[(u64, u8)]) -> u64 {
        let mut h = 0xCBF2_9CE4_8422_2325u64 ^ nonce;
        for &(offset, byte) in samples {
            for chunk in offset.to_le_bytes() {
                h ^= chunk as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01B3);
            }
            h ^= byte as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }
}

#[cfg(test)]
mod hlm1_placement_tests {
    use super::hlm1_placement::*;

    const RESERVE: u64 = 1 << 30;
    const POOL: u64 = 64 * 1024 * 1024;

    fn obs(operation: u32, segment_id: u32, byte_offset: u64, length_bytes: u64) -> Observation {
        Observation {
            operation,
            segment_id,
            byte_offset,
            length_bytes,
        }
    }

    #[test]
    fn virtual_content_ops_carry_no_placement() {
        // F14's correction: these are two of the four sites that move `PgDi`.
        assert!(!carries_placement(op::VIRTUAL_TRANSFER));
        assert!(!carries_placement(op::VIRTUAL_FILL));
        assert!(!carries_placement(op::DISCARD_CONTENT));
        assert!(carries_placement(op::TRANSFER));
        assert!(carries_placement(op::FILL));
        assert!(carries_placement(op::UPDATE_PAGE_TABLE));
        assert!(carries_placement(op::NOTIFY_RESIDENCY));
        assert!(carries_placement(op::NOTIFY_RESIDENCY2));
        assert!(carries_placement(op::TRANSFER2));
        assert!(carries_placement(op::FILL2));
        // DISCARD_CONTENT2 names a segment but describes bytes going away, not
        // a placement to bind to.
        assert!(!carries_placement(op::DISCARD_CONTENT2));
    }

    #[test]
    fn unknown_operation_is_not_placement_bearing() {
        for operation in [3u32, 4, 6, 7, 10, 12, 13, 14, 16, 18, 19, 20, 22, 25, 9999] {
            assert!(!carries_placement(operation), "operation {operation}");
        }
    }

    #[test]
    fn admits_an_hlm1_placement() {
        let placement = admit(obs(op::NOTIFY_RESIDENCY2, 2, 4096, POOL), RESERVE).unwrap();
        assert_eq!(placement.byte_offset, 4096);
        assert_eq!(placement.length_bytes, POOL);
    }

    #[test]
    fn refuses_a_virtual_op_before_it_looks_at_anything_else() {
        // The offset and segment are perfectly legal; the operation is not.
        assert_eq!(
            admit(obs(op::VIRTUAL_FILL, 2, 4096, POOL), RESERVE),
            Err(PlacementRefusal::NotPlacementBearing {
                operation: op::VIRTUAL_FILL
            })
        );
    }

    #[test]
    fn refuses_the_aperture_segment() {
        assert_eq!(
            admit(obs(op::NOTIFY_RESIDENCY, 1, 0, POOL), RESERVE),
            Err(PlacementRefusal::ForeignSegment { found: 1 })
        );
    }

    #[test]
    fn offset_bounds_at_the_four_edges() {
        assert_eq!(admissible_window_offset(0, POOL, RESERVE), Ok(()));
        assert_eq!(admissible_window_offset(RESERVE - POOL, POOL, RESERVE), Ok(()));
        assert_eq!(
            admissible_window_offset(RESERVE - POOL + PAGE_BYTES, POOL, RESERVE),
            Err(OffsetRefusal::ExceedsReserve {
                end: RESERVE + PAGE_BYTES,
                reserve: RESERVE
            })
        );
        assert_eq!(
            admissible_window_offset(u64::MAX & !(PAGE_BYTES - 1), POOL, RESERVE),
            Err(OffsetRefusal::RangeOverflow)
        );
    }

    #[test]
    fn offset_must_be_page_aligned_and_nonempty() {
        assert_eq!(
            admissible_window_offset(4095, POOL, RESERVE),
            Err(OffsetRefusal::Unaligned { found: 4095 })
        );
        // Zero length is checked BEFORE alignment: a zero-length map is refused
        // even at a legal offset, and would otherwise pass every other rule.
        assert_eq!(
            admissible_window_offset(0, 0, RESERVE),
            Err(OffsetRefusal::ZeroLength)
        );
    }

    #[test]
    fn a_reserve_of_zero_admits_nothing() {
        assert_eq!(
            admissible_window_offset(0, PAGE_BYTES, 0),
            Err(OffsetRefusal::ExceedsReserve {
                end: PAGE_BYTES,
                reserve: 0
            })
        );
    }

    #[test]
    fn binding_state_is_idempotent_then_rebinds() {
        let placement = Placement {
            byte_offset: 8192,
            length_bytes: POOL,
        };
        let state = BindingState::unbound();
        assert_eq!(state.bound_offset(), None);

        let first = state.observe(placement);
        assert_eq!(first, Action::Bind { to: 8192 });
        let state = state.applied(first);
        assert_eq!(state.bound_offset(), Some(8192));

        assert_eq!(state.observe(placement), Action::None);

        let moved = Placement {
            byte_offset: 16384,
            length_bytes: POOL,
        };
        let action = state.observe(moved);
        assert_eq!(
            action,
            Action::Rebind {
                from: 8192,
                to: 16384
            }
        );
        assert_eq!(state.applied(action).bound_offset(), Some(16384));
    }

    #[test]
    fn a_failed_map_must_not_advance_the_state() {
        // `applied` is only called with the action that COMPLETED. Dropping it on
        // the failure path is what makes the next observation retry instead of
        // reporting `None`.
        let state = BindingState::unbound();
        let placement = Placement {
            byte_offset: 8192,
            length_bytes: POOL,
        };
        let action = state.observe(placement);
        assert_eq!(state.bound_offset(), None);
        assert_eq!(state.observe(placement), action);
    }

    #[test]
    fn the_stamp_differs_from_the_guest_pattern_at_every_index() {
        for nonce in [0u64, 1, 0xDEAD_BEEF, u64::MAX] {
            for index in 0..4096u64 {
                assert_ne!(verify_byte(nonce, index), stamp_byte(nonce, index));
            }
        }
    }

    #[test]
    fn the_guest_pattern_varies_with_position_and_nonce() {
        let a: [u8; 64] = core::array::from_fn(|i| verify_byte(7, i as u64));
        let b: [u8; 64] = core::array::from_fn(|i| verify_byte(8, i as u64));
        assert_ne!(a, b);
        assert!(a.iter().any(|&byte| byte != a[0]));
    }

    #[test]
    fn samples_cover_the_extent_and_end_on_the_last_byte() {
        let mut out = [0u64; MAX_SAMPLES];
        let n = sample_offsets(POOL, &mut out);
        assert_eq!(n, MAX_SAMPLES);
        assert_eq!(out[0], 0);
        assert_eq!(out[n - 1], POOL - 1);
        // The 16 MiB reply-slot boundaries are in the set.
        for slot in 1..4u64 {
            let boundary = slot * 16 * 1024 * 1024;
            assert!(out[..n].contains(&boundary), "missing slot {slot}");
        }
        assert!(out[..n].windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn samples_degrade_without_duplicating() {
        let mut out = [0u64; MAX_SAMPLES];
        assert_eq!(sample_offsets(0, &mut out), 0);
        assert_eq!(sample_offsets(1, &mut out), 1);
        assert_eq!(out[0], 0);
        let n = sample_offsets(4, &mut out);
        assert!(out[..n].windows(2).all(|w| w[0] < w[1]));
        assert_eq!(out[n - 1], 3);
    }

    #[test]
    fn the_digest_is_position_sensitive() {
        let a = [(0u64, 1u8), (4096, 2)];
        // ⛔ THE load-bearing case: same bytes, same ORDER, different offsets.
        // Swapping the bytes instead only proves FNV-1a is order-sensitive, which
        // it is even with the offset dropped from the mix — a mutation that
        // deleted the offset term passed the swapped-byte version of this test.
        let moved = [(0u64, 1u8), (8192, 2)];
        assert_ne!(digest(9, &a), digest(9, &moved));

        let swapped = [(0u64, 2u8), (4096, 1)];
        assert_ne!(digest(9, &a), digest(9, &swapped));
        assert_ne!(digest(9, &a), digest(10, &a));
        // A prefix is not the whole.
        assert_ne!(digest(9, &a), digest(9, &a[..1]));
        // And an empty readback is not silently the expected value.
        assert_ne!(digest(9, &a), digest(9, &[]));
    }
}
