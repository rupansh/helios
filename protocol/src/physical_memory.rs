//! Helios physical-memory device ABI — `HPM1` paging DMA and the `HLM1`
//! host-visible linear BAR admission
//! (`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.7, §17.1, §17.6, §17.7).
//!
//! # ⛔⛔ HPM1 IS DECLINED. This is the guest half of a protocol the owner said no to.
//!
//! `docs/retirement/FINDINGS.md` **F5** is an **owner decision**, dated
//! 2026-08-10. Not deferred, not blocked, not "pending review" — **declined**,
//! on maintenance grounds. Everything about the host side is already undone:
//!
//! * `qemu-helios` is **reset** from `6a7398a72d` to `d4fde50ccb`, dropping all
//!   three HPM1 commits (the 726-line C mirror, the ~970-line negotiation +
//!   HLM1 BAR admission, and the ~2,780-line paging-DMA executor).
//! * They are preserved on branch `helios/hpm1-parked` and tag
//!   `helios-hpm1-parked-2026-08-10`, and **nothing walking the tree reaches
//!   them** — which F5 says is the point.
//! * ⚠ Therefore `qemu-helios/include/hw/virtio/helios_physical_memory.h`,
//!   which the "Mirroring" section below calls this file's C mirror, **does not
//!   exist at HEAD.** Verify with `ls`; it is on the parked branch only. This
//!   file currently mirrors nothing, and no `_Static_assert` anywhere evaluates
//!   its layout.
//! * F5's Consequence: "no lane in flight has [a QEMU dependency] … **do not
//!   add a new QEMU dependency to any lane**." Reopening HPM1 means un-parking
//!   the branch *and* running its adversarial review first — review before
//!   reachable, not after.
//!
//! # ⛔ Producer status: DECLARED, NOT WIRED — and the producer is *nothing, by decision*
//!
//! **Measured 2026-08-10** (`grep -cE '^pub (fn|struct|enum|const|type|trait|union|mod) '`).
//! Of this module's **57** top-level exported symbols, exactly **three** are
//! referenced outside `protocol/`:
//! [`HELIOS_SEGMENT_ID_SYSTEM`], [`HELIOS_SEGMENT_ID_APERTURE`] and
//! [`HELIOS_SEGMENT_ID_HLM1`], by `kmd_render/src/ddi/create_allocation.rs` and
//! `kmd_logic/src/lib.rs`. Every other symbol — the negotiation record, the DMA
//! packet, the page-run array, the page-table and TLB-epoch machinery, the
//! ~3,000 lines that *are* HPM1 — has **zero** consumers:
//!
//! ```text
//! grep -rn -E 'HELIOS_HPM1|HpmContext|HpmOperation|HpmReject' \
//!   kmd_render/src kmd_logic/src umd/src umd12/src tools icd/mesa/src \
//!   qemu-helios/hw qemu-helios/include
//! → (no output)
//! ```
//!
//! This is unlike every other unwired module in the crate. `translator_dispatch`
//! is waiting on mesa **A5**; `translation_session` on **A1**; `diagnostics` on
//! **D0** and **T1**. **HPM1 is waiting on nobody.** There is no unit, no lane,
//! and no plan that produces or consumes it, and F5's C63 analysis supplies the
//! replacement the KMD will use instead — a bounds-checked offset into the
//! KMD's own permanently-mapped 64 MiB HOC1 command pool, which is *stricter*
//! than HPM1 on the invariant C63 states, plus "option zero", which is what
//! actually runs today: do not adopt GPUVA/HOB1 submit for D3D12 at all.
//!
//! ⚠ **This file compiles and its unit tests pass. That is evidence about this
//! file and nothing else** — and here it is weaker than usual, because the C
//! mirror that would cross-check the layout is not in the tree. This is
//! METHOD.md §3 criterion 6's fourth state, *implemented but never exercised*,
//! and the changeset's report must not count it as implemented. Nothing here
//! has ever run, and on the current plan nothing ever will.
//!
//! ⛔ **Do not build on this module. Do not cite it as an existing mechanism.**
//! If you need what §C55 bought — "the host is the authority on identity" — read
//! [`crate::native_render`]'s banner, which records that the property has been
//! **traded away** and names what replaced it (the KMD substitutes the host
//! resid guest-side; `K4-CONTRACT.md` §5, mesa unit **A3** plus K6).
//!
//! ⭐ **What survives, and must not be deleted by association:** the three
//! segment-id constants above are the *live* two-segment table the shipped KMD
//! uses. They are declared in this file for historical reasons — they were
//! written alongside HPM1 — but they are **not** part of the declined surface,
//! they have real callers today, and deleting this module's HPM1 half must not
//! take them with it. They are marked individually at their declarations.
//!
//! Everything from "The boundary these bytes cross" down describes the
//! **declined** design. It is kept, unedited except for this banner and the
//! per-item markers, so that anyone reopening F5 can read what was proposed —
//! not because any of it is a requirement.
//!
//! # The boundary these bytes cross (⛔ DECLINED — F5; describes what was proposed)
//!
//! This module is the **KMD ↔ QEMU device** contract, and nothing else. Two
//! traffic shapes cross it:
//!
//!   - **Admission.** Before `DXGKQAITYPE_QUERYSEGMENT4` may expose a segment,
//!     `DxgkDdiStartDevice` negotiates HPM1 and validates the complete
//!     prefetchable 64-bit host-visible BAR with
//!     [`HeliosPhysicalMemoryNegotiationV1`]. The negotiated byte capacity and
//!     page shift are *exactly* the size and page granularity later reported for
//!     HLM1 (§10.7); a mismatch, short/overlapping/overflowing capability, or a
//!     non-`READY` service fails adapter initialization rather than reporting a
//!     reduced segment.
//!   - **Paging DMA.** `DxgkDdiBuildPagingBuffer` encodes actual hardware work
//!     as a sequence of [`HeliosPhysicalMemoryDmaV1`] packets, each followed by
//!     its [`HeliosPhysicalPageRunV1`] array, into the OS-supplied paging DMA
//!     buffer. Paging `DxgkDdiPatch` is infallible/side-effect-free/no-size-
//!     change over that already self-contained packet; paging
//!     `DxgkDdiSubmitCommand` runs at `DISPATCH_LEVEL` and only nonblocking-
//!     enqueues it. QEMU executes the copy/fill/discard/bind, publishes the new
//!     placement or TLB epoch, and returns the terminal acknowledgement that
//!     alone permits the KMD to complete the paging `SubmissionFenceId`.
//!
//! # What is NOT in this ABI (§10.7, §15, §17.1)
//!
//! This ABI contains **no UMD allocation handle, no pointer, no PID, no
//! renderer/host resource ID, no CPU-host-aperture opcode, and no user-mode
//! mapping token.** Every field below is a fixed-width integer whose meaning is
//! either an OS-supplied physical page number, an OS-supplied segment/offset/
//! length, or a KMD-owned generation/epoch. HPM1 placement is keyed by the live
//! KMD allocation generation, never by a UMD-provided identity, and QEMU
//! "never accepts a UMD address, allocation handle, resource ID, or unvalidated
//! guest pointer in this packet" (§10.7). The package **does not advertise a CPU
//! Host Aperture at all**: there is deliberately no map/unmap CPU-host-aperture
//! opcode, no page-array field, no implicit/null-allocation resolver, and no
//! flag that could be read as one — pass 68 rejected that carrier outright
//! (§19.1 rows 61/65/68) and §17.6 deletes `kmd_render/src/ddi/cpu_host_aperture.rs`.
//!
//! # Legacy modules this supersedes
//!
//! This module replaces the *byte-authority* half of the Escape/IOCTL blob
//! mapper: the retired Escape ABI's `HELIOS_ESCAPE_ALLOC_BLOB` /
//! `HELIOS_ESCAPE_MAP_BLOB` / `HELIOS_ESCAPE_RELEASE_BLOB` (and their
//! `HeliosEscapeAllocBlob` / `HeliosEscapeMapBlob` / `HeliosEscapeReleaseBlob`
//! payloads) together with the retired `IOCTL_HELIOS_ALLOC_BLOB` /
//! `IOCTL_HELIOS_MAP_BLOB` / `IOCTL_HELIOS_RELEASE_BLOB`. Those carried a whole
//! `RESOURCE_MAP_BLOB` host mapping as the authoritative bytes, which can
//! diverge from VidMm's placement on relocation/eviction (§19.1 row 66). Here
//! the authoritative bytes are the actual WDDM placement, moved by real paging
//! DMA. Both legacy carrier files are deleted and no compatibility route remains.
//!
//! # Mirroring (⛔ FALSE AT HEAD — F5)
//!
//! `qemu-helios/include/hw/virtio/helios_physical_memory.h` **was** the
//! hand-written C mirror of this file, and the rule was "both sides must be
//! edited together". **That file is not in the tree.** F5 reset `qemu-helios`
//! to `d4fde50ccb`; the header exists only on branch `helios/hpm1-parked`
//! (`git -C qemu-helios ls-tree -r --name-only helios/hpm1-parked --
//! include/hw/virtio/`). Consequences a reader must not miss:
//!
//! * There is **one** side, not two. Nothing `_Static_assert`s the offsets this
//!   file asserts, so the usual "a reorder fails the other side's build"
//!   guarantee does not hold here.
//! * `tools/retirement-gates.sh`'s "protocol C mirrors compile" gate compiles
//!   the five headers in `protocol/include/` — `helios_diagnostics.h`,
//!   `helios_native_render.h`, `helios_translation_session.h`,
//!   `helios_translator_dispatch.h`, `helios_wddm.h`. This module has no header
//!   there and is **not** covered by that gate.
//! * `protocol/tools/abi_parity.py` likewise has nothing to compare this file
//!   against.
//!
//! Anyone un-parking HPM1 restores the header and both gates before trusting a
//! single offset below.
//!
//! All structs are `#[repr(C)]`, little-endian, pointer-free, and padding-free
//! (explicit `reserved*` fields, never implicit padding), so they derive
//! `Pod`/`Zeroable` and have a byte layout the C mirror can reproduce exactly.
//!
//! # The package generation
//!
//! Every validator here that checks a `package generation` field takes the
//! expected value as a **parameter**, never as a compile-time read, so the KMD
//! can pin one generation for an adapter's lifetime and a test can exercise a
//! mismatch. That parameter's one production value is
//! [`crate::HELIOS_PACKAGE_GENERATION`] (section 17.1, final bullet: "one
//! protocol/package generation constant shared by protocol, Mesa, UMD11, UMD12,
//! KMD, QEMU, and installer. Generation mismatch is fatal."). Zero is never a
//! wildcard on either side of the comparison.

use bytemuck::{Pod, Zeroable};

// ── Magics, versions, and fixed sizes ───────────────────────────────────────
//
// The four-character magics follow the section-10.7 convention used by HVC1
// (`0x31435648`), HNR2 (`0x32524e48`), HVM1 (`0x314d5648`), and HVR1
// (`0x31525648`): the ASCII tag stored little-endian, so the first byte on the
// wire is `'H'`.

/// `'HPM1'` — magic of [`HeliosPhysicalMemoryDmaV1`].
/// (`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.7)
pub const HELIOS_HPM1_MAGIC: u32 = 0x314D_5048;
/// `'HPMN'` — magic of [`HeliosPhysicalMemoryNegotiationV1`]. Deliberately
/// distinct from [`HELIOS_HPM1_MAGIC`] so an admission record can never be
/// decoded as a paging packet, or vice versa.
pub const HELIOS_HPM1_NEGOTIATION_MAGIC: u32 = 0x4E4D_5048;

/// Current HPM1 ABI version. There is no fallback generation: §3 forbids
/// version/feature fallback, so a mismatch is a hard reject on both sides.
pub const HELIOS_HPM1_ABI_VERSION: u16 = 1;

/// Byte size of the fixed [`HeliosPhysicalMemoryDmaV1`] header. The run array
/// begins immediately after it.
pub const HELIOS_HPM1_DMA_HEADER_BYTES: u16 = 168;
/// Byte size of one [`HeliosPhysicalPageRunV1`] — fixed at 24 by §10.7.
pub const HELIOS_HPM1_RUN_BYTES: usize = 24;
/// Byte size of [`HeliosPhysicalMemoryNegotiationV1`].
pub const HELIOS_HPM1_NEGOTIATION_BYTES: u16 = 80;

/// Page granularity of every page number in this ABI. §10.7 fixes the HVM1
/// "segment page shift" at 12 for this generation, and the negotiated HPM1 page
/// shift "is exactly the … page granularity later reported for HLM1".
pub const HELIOS_HPM1_PAGE_SHIFT: u32 = 12;

/// The only legal [`HeliosPhysicalMemoryDmaV1::physical_adapter_index`]:
/// §10.2 exposes one physical node per logical adapter and §10.7 reports
/// "physical-adapter index 0".
pub const HELIOS_HPM1_PHYSICAL_ADAPTER_INDEX: u32 = 0;

/// Maximum [`HeliosPhysicalMemoryDmaV1::run_count`] in one packet.
///
/// ⚠ §10.7 bounds the run array only as "a bounded run" and says
/// `ceil(runCount/maxRunsPerPacket)` packets are emitted when multipass is
/// required (§16.2); it names no number. This constant **is** that number, and
/// it is a decision made once, here, so the KMD and QEMU cannot disagree: the
/// KMD must report [`HELIOS_HPM1_PAGING_BUFFER_BYTES`] as its
/// `DXGK_DRIVERCAPS::PagingBufferSize` and QEMU must reject a larger count.
///
/// The load-bearing property is the assertion below that one maximal packet
/// always fits one paging DMA buffer. Without it, `MultipassOffset` continuation
/// could make no forward progress and VidMm would spin on a buffer that can
/// never carry even one run.
pub const HELIOS_HPM1_MAX_RUNS_PER_PACKET: u32 = 2048;

/// Byte size of the largest legal packet: header plus a full run array.
pub const HELIOS_HPM1_MAX_PACKET_BYTES: usize = HELIOS_HPM1_DMA_HEADER_BYTES as usize
    + HELIOS_HPM1_RUN_BYTES * HELIOS_HPM1_MAX_RUNS_PER_PACKET as usize;

/// The paging DMA buffer size the KMD reports to dxgkrnl. See
/// [`HELIOS_HPM1_MAX_RUNS_PER_PACKET`] for why this lives here.
pub const HELIOS_HPM1_PAGING_BUFFER_BYTES: u32 = 65536;

// ── Feature negotiation (§10.7 "StartDevice must negotiate HPM1") ───────────

/// Actual bounded paging DMA for transfer/fill/discard placement transitions.
pub const HELIOS_HPM1_FEATURE_PAGING_DMA: u64 = 1 << 0;
/// The complete CPU-visible linear local-memory BAR (HLM1) as one WDDM memory
/// segment, with no CPU Host Aperture protocol.
pub const HELIOS_HPM1_FEATURE_LINEAR_BAR: u64 = 1 << 1;
/// C64's authoritative per-ProcessContext root/PTE/TLB service.
pub const HELIOS_HPM1_FEATURE_PAGE_TABLES: u64 = 1 << 2;
/// Physical (non-IOMMU) ADLs only. §10.7: "The selected target reports no
/// logical DMA remapping/IOMMU mode; all system-memory ADLs consumed by HPM1
/// are physical page numbers." A logical-ADL profile "needs its own proven
/// translation and is not admitted by this package", so this bit is required
/// rather than optional — its absence is a refusal, not a mode switch.
pub const HELIOS_HPM1_FEATURE_PHYSICAL_ADL: u64 = 1 << 3;

/// The conjunctive HPM1 feature set. §2 makes the package all-or-nothing: there
/// is no partial-feature arm, so the accepted set must equal this exactly.
///
/// ⚠ There is deliberately **no** CPU-host-aperture feature bit. Any bit
/// outside this mask is rejected by [`validate_negotiation_reply`], so a host
/// that tried to offer such a service could not be adopted by accident.
pub const HELIOS_HPM1_REQUIRED_FEATURES: u64 = HELIOS_HPM1_FEATURE_PAGING_DMA
    | HELIOS_HPM1_FEATURE_LINEAR_BAR
    | HELIOS_HPM1_FEATURE_PAGE_TABLES
    | HELIOS_HPM1_FEATURE_PHYSICAL_ADL;

// ── The immutable two-segment table (§10.7, §17.6, C52) ─────────────────────
//
// ⭐ LIVE. These three constants are the ONLY symbols in this file with callers
// outside `protocol/` (`kmd_render/src/ddi/create_allocation.rs`,
// `kmd_logic/src/lib.rs`). They are NOT part of the HPM1 surface `FINDINGS.md`
// F5 declined, and a demolition of that surface must NOT take them with it.
// They live here only because they were written alongside HPM1.

/// Segment id used in a run record for **guest system memory**: the page
/// numbers come from the OS-supplied physical ADL/MDL admitted by this
/// non-IOMMU profile. This is WDDM's own `SegmentId == 0` convention.
///
/// ⭐ **LIVE — has real callers. Not part of the declined HPM1 surface.**
pub const HELIOS_SEGMENT_ID_SYSTEM: u32 = 0;
/// Segment 1 — "the sole WDDM aperture segment and `PagingBufferSegmentId`"
/// (§10.7). It remains the only nonzero `DmaBufferSegmentSet` choice for
/// existing D3D runtime contexts; HVC1 native-ICD contexts select zero.
///
/// ⭐ **LIVE — has real callers. Not part of the declined HPM1 surface.**
/// `kmd_render/src/ddi/create_allocation.rs` asserts it equals
/// `crate::ddi::gpummu::APERTURE_SEGMENT_ID`.
pub const HELIOS_SEGMENT_ID_APERTURE: u32 = 1;
/// Segment 2 — HLM1, the package's fully CPU-visible linear local-memory
/// segment backed by the prefetchable 64-bit host-visible BAR.
///
/// ⭐ **LIVE — has real callers. Not part of the declined HPM1 surface.** It is
/// `Hvm1Role::placement()`'s preferred segment for every role. ⚠ Note that the
/// HLM1 *segment id* being live does not make the HLM1 *BAR profile* live:
/// F5 parks the QEMU-side BAR admission, and `FINDINGS.md` F2 records what the
/// shipped segment table actually is.
pub const HELIOS_SEGMENT_ID_HLM1: u32 = 2;
/// The package reports exactly two segments, in this order (§10.7, §17.6).
pub const HELIOS_SEGMENT_COUNT: u32 = 2;

/// Sentinel meaning "this operation has no source/destination segment".
///
/// Zero cannot serve as the sentinel because zero already means *system
/// memory*. An arm that does not use a segment role must therefore write this
/// value, and validation rejects a zeroed field there — the fail-closed
/// direction.
pub const HELIOS_HPM1_SEGMENT_NONE: u32 = 0xFFFF_FFFF;

// HLM1 `DXGK_SEGMENTFLAGS` values, verbatim from §10.7:
//   "segment 2, HLM1, a memory segment with `Aperture=0`, `CpuVisible=1`,
//    `CacheCoherent=0`, `SupportsCpuHostAperture=0`,
//    `SupportsCachedCpuHostAperture=0`, and
//    `CpuTranslatedAddress=BAR guest-physical base`."
// §17.6 repeats them as the QuerySegment4 requirement. They are constants (not
// a policy) so `query_adapter_info.rs` can assert against them rather than
// restating four zeros at the call site.

/// HLM1 `DXGK_SEGMENTFLAGS::Aperture` — HLM1 is a memory segment, not an
/// aperture.
pub const HELIOS_HLM1_FLAG_APERTURE: u32 = 0;
/// HLM1 `DXGK_SEGMENTFLAGS::CpuVisible` — the whole BAR is linearly CPU
/// visible, which is what makes every allocation in it Lock2-able.
pub const HELIOS_HLM1_FLAG_CPU_VISIBLE: u32 = 1;
/// HLM1 `DXGK_SEGMENTFLAGS::CacheCoherent` — C52: `CacheCoherent` applies only
/// to aperture segments, so a memory segment must report zero.
pub const HELIOS_HLM1_FLAG_CACHE_COHERENT: u32 = 0;
/// HLM1 `DXGK_SEGMENTFLAGS::SupportsCpuHostAperture` — the package advertises
/// no CPU Host Aperture and registers neither `DxgkDdiMapCpuHostAperture` nor
/// `DxgkDdiUnmapCpuHostAperture`.
pub const HELIOS_HLM1_FLAG_SUPPORTS_CPU_HOST_APERTURE: u32 = 0;
/// HLM1 `DXGK_SEGMENTFLAGS::SupportsCachedCpuHostAperture` — see above.
pub const HELIOS_HLM1_FLAG_SUPPORTS_CACHED_CPU_HOST_APERTURE: u32 = 0;

// ── Terminal status (§10.7, §10.9 failure table) ────────────────────────────

/// Terminal acknowledgement status of one HPM1 transaction.
///
/// §10.7: QEMU "returns a terminal acknowledgement that alone permits KMD's
/// paging DMA completion", and §10.9 requires that an HPM1 "paging-DMA/ADL/
/// placement/epoch/copy/fill/discard acknowledgement failure" must "never
/// advance its paging SubmissionFenceId, expose divergent bytes, or repair
/// through a resource-ID lookup".
///
/// [`HeliosHpm1Status::Pending`] is the only value legal in the guest→device
/// direction: [`HeliosPhysicalMemoryDmaV1::status`] is reserved-zero on submit
/// and written only by the device's acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HeliosHpm1Status {
    /// No terminal acknowledgement yet. The only value a submitted packet may
    /// carry, and never a completion.
    Pending = 0,
    /// Terminal success: bytes are ready, the new placement/TLB epoch is
    /// atomically visible, and obsolete bindings are revoked. This is the *only*
    /// value that permits the paging `SubmissionFenceId` to complete.
    Complete = 1,
    /// Header/run record malformed: magic, version, size, reserved, flag, or
    /// per-arm field rule violated.
    RejectedMalformed = 2,
    /// A package, adapter, allocation, address-space, root, or placement
    /// generation/epoch did not match live device state.
    RejectedGeneration = 3,
    /// The operation ordinal is not one this package implements. Unknown is
    /// never approximated.
    RejectedUnsupportedOperation = 4,
    /// A segment id, page number, offset, or length fell outside the negotiated
    /// bounds of its named segment.
    RejectedRange = 5,
    /// A placement shape the device does not implement (§10.7: "Unsupported
    /// paging/placement shapes fail the paging or device before mutation").
    RejectedPlacement = 6,
    /// The host copy/fill/discard/bind or its address-space transition failed.
    FailedDevice = 7,
    /// Adapter reset generation-invalidated the transaction. §14: a stale
    /// acknowledgement "cannot publish reply/completion".
    FailedReset = 8,
}

impl HeliosHpm1Status {
    /// Total decode of the wire value. An unknown status is *not* mapped onto a
    /// failure code — the caller must be able to tell "the device said
    /// something I do not understand" from "the device reported a failure I do
    /// understand", because only the former indicates ABI drift.
    #[inline]
    pub const fn from_raw(raw: u32) -> Result<Self, HpmReject> {
        match raw {
            0 => Ok(Self::Pending),
            1 => Ok(Self::Complete),
            2 => Ok(Self::RejectedMalformed),
            3 => Ok(Self::RejectedGeneration),
            4 => Ok(Self::RejectedUnsupportedOperation),
            5 => Ok(Self::RejectedRange),
            6 => Ok(Self::RejectedPlacement),
            7 => Ok(Self::FailedDevice),
            8 => Ok(Self::FailedReset),
            other => Err(HpmReject::UnknownStatus { found: other }),
        }
    }

    /// Whether this status permits the KMD to complete the paging
    /// `SubmissionFenceId`. Exactly one value does.
    #[inline]
    pub const fn permits_paging_completion(self) -> bool {
        matches!(self, Self::Complete)
    }
}

// ── Packet flags (§10.7) ────────────────────────────────────────────────────

/// First packet of a multipass transaction. Mirrors WDDM's
/// `DXGK_TRANSFERFLAGS::TransferStart`; it is set **iff**
/// [`HeliosPhysicalMemoryDmaV1::multipass_offset`] is zero, which is what makes
/// `MultipassOffset` the *sole* continuation state §10.7 requires.
pub const HELIOS_HPM1_FLAG_TRANSACTION_FIRST: u32 = 1 << 0;
/// Final packet of a multipass transaction. Mirrors
/// `DXGK_TRANSFERFLAGS::TransferEnd`. The device may publish the new placement
/// or TLB epoch only on this packet; a single-packet transaction sets both
/// FIRST and LAST.
///
/// ⚠ This bounds *publication*, not the field. `new_epoch` stays `Required` on
/// every arm that has one, LAST or not, deliberately: all passes of one
/// multipass transaction describe the same transition and must name the same
/// epoch, so an intermediate pass carrying zero would make the transaction two
/// different transitions. "Publish only on LAST" is a **device** rule about
/// when the epoch becomes visible, and no single packet contains the evidence
/// for it — a validator here could only check it by remembering the previous
/// packets of the transaction, which is state this crate deliberately does not
/// hold (the caller owns the transaction frontier).
pub const HELIOS_HPM1_FLAG_TRANSACTION_LAST: u32 = 1 << 1;
/// OS told the driver the allocation is idle
/// (`DXGK_TRANSFERFLAGS::AllocationIsIdle` /
/// `DXGK_DISCARDCONTENTFLAGS::AllocationIsIdle`). Carried truthfully; it is
/// never an ordering substitute.
pub const HELIOS_HPM1_FLAG_ALLOCATION_IS_IDLE: u32 = 1 << 2;
/// `DXGK_MAPAPERTUREFLAGS::CacheCoherent` for an aperture map. Preserved
/// verbatim from the OS request.
pub const HELIOS_HPM1_FLAG_CACHE_COHERENT: u32 = 1 << 3;
/// `DXGK_BUILDPAGINGBUFFER_NOTIFYRESIDENCY{,2}::Resident`. Audit-only.
pub const HELIOS_HPM1_FLAG_RESIDENT: u32 = 1 << 4;
/// PTE attribute for the packet's runs: `DXGK_PTE::Valid`.
pub const HELIOS_HPM1_FLAG_PTE_VALID: u32 = 1 << 5;
/// PTE attribute: `DXGK_PTE::Zero` — the entry maps the zero page.
pub const HELIOS_HPM1_FLAG_PTE_ZERO: u32 = 1 << 6;
/// PTE attribute: `DXGK_PTE::ReadOnly`.
pub const HELIOS_HPM1_FLAG_PTE_READ_ONLY: u32 = 1 << 7;
/// PTE attribute: `DXGK_PTE::NoExecute`.
pub const HELIOS_HPM1_FLAG_PTE_NO_EXECUTE: u32 = 1 << 8;
/// PTE attribute: `DXGK_PTE::CacheCoherent`.
pub const HELIOS_HPM1_FLAG_PTE_CACHE_COHERENT: u32 = 1 << 9;
/// PTE attribute: `DXGK_PTE::LargePage`.
pub const HELIOS_HPM1_FLAG_PTE_LARGE_PAGE: u32 = 1 << 10;

/// Every flag bit this ABI defines. An unknown bit is a hard reject; there is no
/// forward-compatible ignore rule, because §3 forbids feature fallback.
pub const HELIOS_HPM1_FLAG_MASK: u32 = HELIOS_HPM1_FLAG_TRANSACTION_FIRST
    | HELIOS_HPM1_FLAG_TRANSACTION_LAST
    | HELIOS_HPM1_FLAG_ALLOCATION_IS_IDLE
    | HELIOS_HPM1_FLAG_CACHE_COHERENT
    | HELIOS_HPM1_FLAG_RESIDENT
    | HELIOS_HPM1_FLAG_PTE_VALID
    | HELIOS_HPM1_FLAG_PTE_ZERO
    | HELIOS_HPM1_FLAG_PTE_READ_ONLY
    | HELIOS_HPM1_FLAG_PTE_NO_EXECUTE
    | HELIOS_HPM1_FLAG_PTE_CACHE_COHERENT
    | HELIOS_HPM1_FLAG_PTE_LARGE_PAGE;

/// The PTE attribute subset, allowed only on the page-table arms.
pub const HELIOS_HPM1_FLAG_PTE_MASK: u32 = HELIOS_HPM1_FLAG_PTE_VALID
    | HELIOS_HPM1_FLAG_PTE_ZERO
    | HELIOS_HPM1_FLAG_PTE_READ_ONLY
    | HELIOS_HPM1_FLAG_PTE_NO_EXECUTE
    | HELIOS_HPM1_FLAG_PTE_CACHE_COHERENT
    | HELIOS_HPM1_FLAG_PTE_LARGE_PAGE;

/// Flags every arm may carry: the two multipass markers.
pub const HELIOS_HPM1_FLAG_COMMON_MASK: u32 =
    HELIOS_HPM1_FLAG_TRANSACTION_FIRST | HELIOS_HPM1_FLAG_TRANSACTION_LAST;

// ── Paging operations (§10.7, §17.6) ────────────────────────────────────────

/// The paging operations this package implements.
///
/// §17.6 requires "actual bounded HPM1 paging DMA for every reachable
/// transfer/transfer2, fill/fill2 and discard operation" plus "C64's
/// authoritative per-process root, PTE-copy/update, TLB, local/aperture
/// map/unmap, context-resource, and residency packets". Each variant records
/// the exact `DXGK_BUILDPAGINGBUFFER_OPERATION` (or DDI) it is built from.
///
/// The wire ordinals are Helios-owned and start at 1 rather than reusing the
/// WDDM ordinals, for one reason: **zero must not decode**. A zero-filled or
/// partially written paging DMA buffer then fails
/// [`HpmOperation::from_raw`] instead of resolving to
/// `DXGK_OPERATION_TRANSFER`.
///
/// Operations the package deliberately does **not** admit — and which therefore
/// reject as [`HpmReject::UnknownOperation`] — are `READ_PHYSICAL`,
/// `WRITE_PHYSICAL`, `SPECIAL_LOCK_TRANSFER`, `SIGNAL_MONITORED_FENCE`,
/// `NOTIFY_FENCE_RESIDENCY`, `NOTIFY_ALLOC`, and `MAP_MMU`/`UNMAP_MMU` (the
/// WDDM-3.2 IOMMU arms: §10.7 reports "no logical DMA remapping/IOMMU mode").
/// There is likewise no CPU-host-aperture opcode of any kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum HpmOperation {
    /// `DXGK_OPERATION_TRANSFER` (0) — classic segment/MDL allocation copy.
    Transfer = 1,
    /// `DXGK_OPERATION_VIRTUAL_TRANSFER` (8) — the GPUVA-addressed transfer. The
    /// device resolves both virtual addresses through its own current page
    /// tables, so the packet carries no runs.
    ///
    /// ⛔ **This is NOT `DXGK_OPERATION_TRANSFER2`, and the name here is only
    /// §10.7's.** Measured 2026-08-11 against the shipping WDK 28000 bindings:
    /// `TRANSFER2 = 23` and `FILL2 = 24` are distinct ordinals with their own
    /// descriptors (`FILL2` is the only operation in the DDI carrying a bare
    /// `(SegmentId, SegmentAddress)` pair). The earlier claim that §10.7's
    /// TRANSFER2/FILL2 "are" the virtual ops rather than new ordinals is
    /// falsified; `FINDINGS.md` F15 has the census. The wire values below are
    /// unchanged — HPM1 is declined (F5) and this enum has no consumer — but a
    /// lane reading the old line would have mapped two different operations onto
    /// one arm.
    Transfer2 = 2,
    /// `DXGK_OPERATION_FILL` (1).
    Fill = 3,
    /// `DXGK_OPERATION_VIRTUAL_FILL` (9). Not `DXGK_OPERATION_FILL2` (24) — see
    /// [`HpmOperation::Transfer2`].
    Fill2 = 4,
    /// `DXGK_OPERATION_DISCARD_CONTENT` (2).
    DiscardContent = 5,
    /// `DXGK_OPERATION_MAP_APERTURE_SEGMENT` (5) — install OS system-page runs
    /// in the segment-1 aperture table.
    MapApertureSegment = 6,
    /// `DXGK_OPERATION_MAP_APERTURE_SEGMENT2` (17) — the ADL-carrying form.
    /// Identical on the wire: the ADL has already been flattened into physical
    /// page runs by `BuildPagingBuffer`, which is the only ADL shape this
    /// non-IOMMU profile admits.
    MapApertureSegment2 = 7,
    /// `DXGK_OPERATION_UNMAP_APERTURE_SEGMENT` (6) — point the named aperture
    /// range at the OS-supplied dummy page.
    UnmapApertureSegment = 8,
    /// `DxgkDdiSetRootPageTable` — not a `BuildPagingBuffer` operation, but
    /// §10.7 makes it an HPM1 packet: "records a new root plus monotonically
    /// increasing address-space generation only while Windows has idled the
    /// target context". The device model owns the page tables, so the root's
    /// identity is exactly `(address_space_id, root_generation)`; no root
    /// physical page is transported.
    SetRootPageTable = 9,
    /// `DXGK_OPERATION_UPDATE_PAGE_TABLE` (11).
    UpdatePageTable = 10,
    /// `DXGK_OPERATION_COPY_PAGE_TABLE_ENTRIES` (14). One packet carries one
    /// copy range; the device copies its own current mappings, so no runs.
    CopyPageTableEntries = 11,
    /// `DXGK_OPERATION_FLUSH_TLB` (12).
    FlushTlb = 12,
    /// `DXGK_OPERATION_INIT_CONTEXT_RESOURCE` (10).
    InitContextResource = 13,
    /// `DXGK_OPERATION_UPDATE_CONTEXT_ALLOCATION` (13).
    UpdateContextAllocation = 14,
    /// `DXGK_OPERATION_NOTIFY_RESIDENCY` (15). §10.7: "`NotifyResidency2` is a
    /// postcondition audit, never the ordering edge" — the same is true here,
    /// which is why both residency arms are forbidden from publishing an epoch.
    NotifyResidency = 15,
    /// `DXGK_OPERATION_NOTIFY_RESIDENCY2` (21).
    NotifyResidency2 = 16,
    /// §10.7's `DISCARD_CONTENT2` — the GPUVA-addressed discard, standing to
    /// [`Self::DiscardContent`] exactly as [`Self::Transfer2`] stands to
    /// [`Self::Transfer`] and [`Self::Fill2`] to [`Self::Fill`].
    ///
    /// ⚠ AMBIGUITY, recorded rather than resolved: §10.7 names this operation in
    /// its `BuildPagingBuffer` obligation list, but no `DXGK_OPERATION_*`
    /// constant with that spelling appears in the released WDK 28000 headers, so
    /// the **`DXGK` ordinal is unverified**. The number below is HPM1's own wire
    /// ordinal and is not derived from a WDK value; it is appended last so the
    /// existing sixteen ordinals keep their encodings. If the WDK arm turns out
    /// to carry runs or a segment, this shape is wrong and must be corrected
    /// before the arm is reachable — but a doc-named operation with *no*
    /// encoding cannot be built at all, which turns a reachable paging op into a
    /// device removal §10.9 gives no recovery for.
    DiscardContent2 = 17,
}

impl HpmOperation {
    /// Total decode. Unknown is refused, never approximated.
    #[inline]
    pub const fn from_raw(raw: u32) -> Result<Self, HpmReject> {
        match raw {
            1 => Ok(Self::Transfer),
            2 => Ok(Self::Transfer2),
            3 => Ok(Self::Fill),
            4 => Ok(Self::Fill2),
            5 => Ok(Self::DiscardContent),
            6 => Ok(Self::MapApertureSegment),
            7 => Ok(Self::MapApertureSegment2),
            8 => Ok(Self::UnmapApertureSegment),
            9 => Ok(Self::SetRootPageTable),
            10 => Ok(Self::UpdatePageTable),
            11 => Ok(Self::CopyPageTableEntries),
            12 => Ok(Self::FlushTlb),
            13 => Ok(Self::InitContextResource),
            14 => Ok(Self::UpdateContextAllocation),
            15 => Ok(Self::NotifyResidency),
            16 => Ok(Self::NotifyResidency2),
            17 => Ok(Self::DiscardContent2),
            other => Err(HpmReject::UnknownOperation { found: other }),
        }
    }

    /// The wire ordinal.
    #[inline]
    pub const fn as_raw(self) -> u32 {
        self as u32
    }

    /// The per-arm field obligation for this operation. See
    /// [`HpmOperationShape`].
    #[inline]
    pub const fn shape(self) -> HpmOperationShape {
        // ORDER OF EVALUATION IS NOT LOAD-BEARING here; every arm is an
        // independent, complete record. What IS load-bearing is that
        // `HpmOperationShape::FAIL_CLOSED` makes every unnamed field
        // Forbidden/Unused, so a new operation added without stating its
        // obligations rejects every packet rather than accepting a max-union.
        use HpmOperationShape as S;
        use Rule::{Optional, Required};
        use SegmentRole::{AnySegment, ExactAperture, ExactSystem};
        match self {
            // Classic allocation copy: paired source/destination page runs in
            // two named segments. At least one side must be a device segment;
            // `validate` enforces that separately since it is a cross-field
            // rule, not a per-field one.
            Self::Transfer => S {
                allocation_generation: Required,
                byte_length: Required,
                runs: Required,
                source_segment: AnySegment,
                destination_segment: AnySegment,
                run_source_page: Required,
                run_destination_page: Required,
                new_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_ALLOCATION_IS_IDLE,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::Transfer2 => S {
                address_space: Required,
                allocation_generation: Required,
                source_gpu_virtual_address: Required,
                destination_gpu_virtual_address: Required,
                byte_length: Required,
                new_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_ALLOCATION_IS_IDLE,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::Fill => S {
                allocation_generation: Required,
                byte_length: Required,
                runs: Required,
                destination_segment: AnySegment,
                run_destination_page: Required,
                fill_pattern: Optional,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::Fill2 => S {
                address_space: Required,
                allocation_generation: Required,
                destination_gpu_virtual_address: Required,
                byte_length: Required,
                fill_pattern: Optional,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            // The GPUVA-addressed discard: the device resolves the range
            // through its own current page tables, so — exactly like
            // `Transfer2`/`Fill2` — the packet carries no runs and no segment.
            Self::DiscardContent2 => S {
                address_space: Required,
                allocation_generation: Required,
                destination_gpu_virtual_address: Required,
                byte_length: Required,
                new_epoch: Required,
                prior_epoch: Optional,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_ALLOCATION_IS_IDLE,
                ..S::FAIL_CLOSED
            },
            Self::DiscardContent => S {
                allocation_generation: Required,
                byte_length: Required,
                runs: Required,
                destination_segment: AnySegment,
                run_destination_page: Required,
                new_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_ALLOCATION_IS_IDLE,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            // Aperture map/unmap program the adapter-global segment-1 PTE
            // table (§10.7: "the ordinary segment-1 aperture PTE table"), which
            // is why they are NOT address-space operations. `hAllocation` may be
            // NULL for implicit/page-table storage, so the allocation
            // generation is Optional — §10.7's "zero only for a documented
            // null-allocation/page-table object".
            Self::MapApertureSegment | Self::MapApertureSegment2 => S {
                allocation_generation: Optional,
                byte_length: Required,
                runs: Required,
                source_segment: ExactSystem,
                destination_segment: ExactAperture,
                run_source_page: Required,
                run_destination_page: Required,
                new_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_CACHE_COHERENT,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            // Unmap still names a source page: the OS-supplied dummy page every
            // revoked aperture entry is pointed at.
            Self::UnmapApertureSegment => S {
                allocation_generation: Optional,
                byte_length: Required,
                runs: Required,
                source_segment: ExactSystem,
                destination_segment: ExactAperture,
                run_source_page: Required,
                run_destination_page: Required,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::SetRootPageTable => S {
                address_space: Required,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            // The runs supply the physical pages; `destination_page` is the
            // zero-based virtual page index within
            // `[destination_gpu_virtual_address, +gpu_virtual_page_count)` — the
            // one place in this ABI where a run page number is not a segment
            // page, which is why the arm sets
            // `run_destination_page_is_virtual` and `validate_runs` bounds it
            // against `gpu_virtual_page_count` instead of a segment size.
            Self::UpdatePageTable => S {
                address_space: Required,
                allocation_generation: Optional,
                destination_gpu_virtual_address: Required,
                gpu_virtual_page_count: Required,
                byte_length: Required,
                runs: Required,
                source_segment: AnySegment,
                run_source_page: Required,
                run_destination_page: Required,
                run_destination_page_is_virtual: true,
                new_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_PTE_MASK,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::CopyPageTableEntries => S {
                address_space: Required,
                source_gpu_virtual_address: Required,
                destination_gpu_virtual_address: Required,
                gpu_virtual_page_count: Required,
                new_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_PTE_MASK,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::FlushTlb => S {
                address_space: Required,
                source_gpu_virtual_address: Required,
                gpu_virtual_page_count: Required,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            Self::InitContextResource => S {
                address_space: Required,
                allocation_generation: Required,
                destination_gpu_virtual_address: Required,
                byte_length: Required,
                runs: Required,
                destination_segment: AnySegment,
                run_destination_page: Required,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            // A context allocation is named only by its GPUVA; it has no
            // `hAllocation`, which is the other documented null-allocation case.
            Self::UpdateContextAllocation => S {
                address_space: Required,
                destination_gpu_virtual_address: Required,
                byte_length: Required,
                new_epoch: Required,
                prior_epoch: Optional,
                ..S::FAIL_CLOSED
            },
            // Audit only: it reports the placement the OS believes is current,
            // so it names the epoch it audits (`prior_epoch`) and may never
            // publish a new one.
            Self::NotifyResidency | Self::NotifyResidency2 => S {
                allocation_generation: Required,
                byte_length: Required,
                runs: Required,
                destination_segment: AnySegment,
                run_destination_page: Required,
                prior_epoch: Required,
                allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK | HELIOS_HPM1_FLAG_RESIDENT,
                ..S::FAIL_CLOSED
            },
        }
    }
}

/// Obligation on one scalar field for one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Must be nonzero.
    Required,
    /// May be zero or nonzero.
    Optional,
    /// Must be zero.
    Forbidden,
}

/// Obligation on one segment-id field for one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentRole {
    /// Must be [`HELIOS_HPM1_SEGMENT_NONE`].
    Unused,
    /// Must be one of the three legal ids (system / aperture / HLM1).
    AnySegment,
    /// Must be exactly [`HELIOS_SEGMENT_ID_SYSTEM`].
    ExactSystem,
    /// Must be exactly [`HELIOS_SEGMENT_ID_APERTURE`].
    ExactAperture,
}

/// The per-arm field obligation for one [`HpmOperation`].
///
/// This exists because the CLAUDE.md invariant "validate every runtime-supplied
/// size & offset **per-arm, not max-union**" cannot be met by an `if/else`
/// chain that mutates shared locals: the obligation has to be a value, stated
/// once, that an exhaustive match must supply. Adding an operation therefore
/// forces a decision here, and forgetting one leaves it at
/// [`HpmOperationShape::FAIL_CLOSED`], which rejects every packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HpmOperationShape {
    /// `address_space_id` and `root_generation` together. §10.7: they are
    /// "zero only for a non-address-space operation".
    pub address_space: Rule,
    pub allocation_generation: Rule,
    pub source_gpu_virtual_address: Rule,
    pub destination_gpu_virtual_address: Rule,
    pub gpu_virtual_page_count: Rule,
    pub byte_length: Rule,
    /// `Required` means `1..=HELIOS_HPM1_MAX_RUNS_PER_PACKET` runs;
    /// `Forbidden` means exactly zero.
    pub runs: Rule,
    pub source_segment: SegmentRole,
    pub destination_segment: SegmentRole,
    pub fill_pattern: Rule,
    pub new_epoch: Rule,
    pub prior_epoch: Rule,
    /// Obligation on [`HeliosPhysicalPageRunV1::source_page`] in every run.
    ///
    /// ⚠ These two carry a **different** `Required` than the scalar fields
    /// above, and the difference is deliberate. For a run page number,
    /// `Forbidden` means "this arm has no such range, so every run must write
    /// zero", while `Required` means "this arm uses the field, so bounds-check
    /// it against its segment". `Required` does **not** mean nonzero: page 0 of
    /// HLM1 and page 0 of the aperture are perfectly legal pages, and rejecting
    /// them would make the first page of the BAR unusable.
    pub run_source_page: Rule,
    /// Obligation on [`HeliosPhysicalPageRunV1::destination_page`] in every run.
    /// See [`Self::run_source_page`] for the `Required`/`Forbidden` meanings,
    /// which differ from the scalar fields'.
    pub run_destination_page: Rule,
    /// ⚠ `true` only on the page-table arm, where
    /// [`HeliosPhysicalPageRunV1::destination_page`] is **not** a page number in
    /// a segment but the zero-based *virtual* page index inside
    /// `[destination_gpu_virtual_address, +gpu_virtual_page_count)`.
    ///
    /// It exists because the segment-keyed bound in `check_run_page` cannot
    /// apply there: the arm's `destination_segment` is `Unused`, so without this
    /// flag the field would be the one page number in the whole ABI that nothing
    /// bounds, and a packet could ask the device to install a PTE outside the
    /// very range it declared. When set, [`HeliosPhysicalMemoryDmaV1::validate_runs`]
    /// requires the runs to tile `[0, gpu_virtual_page_count)` exactly, in
    /// order — which is how `BuildPagingBuffer` flattens one contiguous virtual
    /// range in the first place.
    pub run_destination_page_is_virtual: bool,
    /// Exact set of flag bits this arm may carry.
    pub allowed_flags: u32,
}

impl HpmOperationShape {
    /// The base every arm is written against: nothing is permitted, nothing is
    /// required, no segment role is used, and only the two multipass markers are
    /// legal flags. Omission is therefore the fail-closed direction — for
    /// **every** field, `prior_epoch` included. An arm that may carry a prior
    /// placement/TLB epoch states `prior_epoch: Optional` itself; a new
    /// operation added without stating one rejects a packet that carries an
    /// epoch instead of silently accepting an arbitrary value.
    pub const FAIL_CLOSED: Self = Self {
        address_space: Rule::Forbidden,
        allocation_generation: Rule::Forbidden,
        source_gpu_virtual_address: Rule::Forbidden,
        destination_gpu_virtual_address: Rule::Forbidden,
        gpu_virtual_page_count: Rule::Forbidden,
        byte_length: Rule::Forbidden,
        runs: Rule::Forbidden,
        source_segment: SegmentRole::Unused,
        destination_segment: SegmentRole::Unused,
        fill_pattern: Rule::Forbidden,
        new_epoch: Rule::Forbidden,
        prior_epoch: Rule::Forbidden,
        run_source_page: Rule::Forbidden,
        run_destination_page: Rule::Forbidden,
        run_destination_page_is_virtual: false,
        allowed_flags: HELIOS_HPM1_FLAG_COMMON_MASK,
    };
}

// ── Rejection reasons ───────────────────────────────────────────────────────

/// Which field a rejection is about. Named rather than positional so a counter
/// or ETW event can report the exact offender without a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpmField {
    AddressSpaceId,
    RootGeneration,
    AllocationGeneration,
    AllocationOffset,
    SourceGpuVirtualAddress,
    DestinationGpuVirtualAddress,
    GpuVirtualPageCount,
    ByteLength,
    SourceSegmentId,
    DestinationSegmentId,
    PriorEpoch,
    NewEpoch,
    FillPattern,
    RunCount,
    MultipassOffset,
    Status,
    Reserved0,
    Reserved1,
    RunSourcePage,
    RunDestinationPage,
    RunReserved,
    // HLM1 segment-report fields.
    SegmentId,
    SegmentAperture,
    SegmentCpuVisible,
    SegmentCacheCoherent,
    SegmentSupportsCpuHostAperture,
    SegmentSupportsCachedCpuHostAperture,
    SegmentCpuTranslatedAddress,
    SegmentBaseAddress,
    SegmentSize,
    SegmentCommitLimit,
    SegmentPageShift,
    // Negotiation fields.
    BarGuestPhysicalBase,
    BarLengthBytes,
    Hlm1CapacityBytes,
    PageShift,
    /// The `in` feature word the KMD asked for. Distinct from
    /// [`Self::AcceptedFeatures`] on purpose: an echo mismatch of one is not the
    /// other, and §10.9's failure table requires the two to be distinguishable.
    RequestedFeatures,
    AcceptedFeatures,
    AdapterGeneration,
    /// [`HeliosPhysicalMemoryNegotiationV1::reserved`] — the negotiation
    /// record's single reserved field, not the paging header's
    /// [`Self::Reserved0`]/[`Self::Reserved1`] pair.
    NegotiationReserved,
}

/// Why an HPM1 record was refused.
///
/// Every rejection has its own named reason: a `bool` would collapse "the host
/// offered an unknown feature" into "the BAR is short", and the failure table in
/// §10.9 requires the two to be distinguishable (one rejects the package, the
/// other fails adapter initialization). No variant is a catch-all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpmReject {
    /// Magic word is not [`HELIOS_HPM1_MAGIC`] / the negotiation magic.
    BadMagic { found: u32, expected: u32 },
    /// ABI version is not [`HELIOS_HPM1_ABI_VERSION`]. There is no fallback.
    BadAbiVersion { found: u16, expected: u16 },
    /// Declared structure/header size does not match this build's.
    BadStructureSize { found: u16, expected: u16 },
    /// The atomic package generation did not match. §2: mixed HPS2/new binaries
    /// must not interoperate.
    PackageGenerationMismatch { found: u64, expected: u64 },
    /// The adapter generation did not match; reset invalidates it (§14).
    AdapterGenerationMismatch { found: u64, expected: u64 },
    /// Operation ordinal is not implemented by this package.
    UnknownOperation { found: u32 },
    /// Terminal status ordinal is not one this ABI defines.
    UnknownStatus { found: u32 },
    /// `physical_adapter_index` is not
    /// [`HELIOS_HPM1_PHYSICAL_ADAPTER_INDEX`].
    WrongPhysicalAdapterIndex { found: u32 },
    /// `page_shift` is not the negotiated granularity.
    WrongPageShift { found: u32, expected: u32 },
    /// The **caller's** [`HpmContext::page_shift`] is not
    /// [`HELIOS_HPM1_PAGE_SHIFT`]. Distinct from [`Self::WrongPageShift`],
    /// which is about the wire: this one says the validator was handed a device
    /// profile negotiation could never have produced.
    ContextPageShiftUnsupported { found: u32, expected: u32 },
    /// `transaction_id` is zero; every packet belongs to a numbered
    /// per-paging-context transaction (§16.2 "one per-paging-context
    /// transaction frontier").
    ZeroTransactionId,
    /// `status` was not [`HeliosHpm1Status::Pending`] on a submitted packet.
    StatusNotPending { found: u32 },
    /// A reserved field was nonzero.
    ReservedNonZero { field: HpmField },
    /// Flag word contains bits outside [`HELIOS_HPM1_FLAG_MASK`].
    UnknownFlagBits { bits: u32 },
    /// Flag word contains bits this operation may not carry.
    FlagNotAllowedForOperation { bits: u32 },
    /// A field this operation requires was zero.
    MissingRequiredField { field: HpmField },
    /// A field this operation forbids was nonzero.
    ForbiddenFieldSet { field: HpmField },
    /// A segment id is not one of the three legal ids.
    UnknownSegmentId { field: HpmField, found: u32 },
    /// A segment id is legal but not permitted in this role for this operation
    /// (e.g. an aperture map naming HLM1 as its destination).
    SegmentNotAllowedForRole { field: HpmField, found: u32 },
    /// A classic transfer named system memory on both ends: no device segment
    /// is involved, so there is nothing for HPM1 to do.
    TransferNamesNoDeviceSegment,
    /// A page-table run's `destination_page` — a *virtual* page index — did not
    /// continue the previous run, so the runs do not tile the declared PTE
    /// range in order.
    VirtualRunNotContiguous { index: u32, expected: u64, found: u64 },
    /// A page-table run names virtual pages outside
    /// `[0, gpu_virtual_page_count)`, i.e. outside the range the same packet
    /// declared.
    VirtualPageOutsideRange {
        index: u32,
        page: u64,
        pages: u64,
        gpu_virtual_page_count: u64,
    },
    /// The page-table runs stopped short of `gpu_virtual_page_count`, leaving
    /// declared PTEs that no run writes.
    VirtualRangeNotFullyCovered {
        covered: u64,
        gpu_virtual_page_count: u64,
    },
    /// `run_count` is zero for a run-bearing arm or above
    /// [`HELIOS_HPM1_MAX_RUNS_PER_PACKET`].
    RunCountOutOfRange { found: u32 },
    /// The supplied byte slice is shorter than the packet declares.
    TruncatedPacket { needed: usize, available: usize },
    /// The supplied byte slice is not 8-byte aligned, so the fixed-layout
    /// records cannot be referenced from it.
    MisalignedPacket,
    /// A run declared zero pages.
    ZeroPageCountInRun { index: u32 },
    /// A run's `reserved_zero` field was nonzero.
    RunReservedNonZero { index: u32 },
    /// A run's page range overflows 64 bits, or the packet's total page count
    /// or byte coverage overflows.
    PageRangeOverflow { index: u32 },
    /// A page number falls outside the negotiated bounds of its segment.
    PageOutsideSegment {
        index: u32,
        field: HpmField,
        segment_id: u32,
        page: u64,
    },
    /// The run array does not cover exactly the pages `byte_length` needs.
    /// Over-covering is refused as strictly as under-covering: a packet may not
    /// carry a page run its declared byte range does not reach.
    RunCoverageMismatch {
        byte_length: u64,
        covered_pages: u64,
    },
    /// `byte_length` and `gpu_virtual_page_count` disagree on a page-table arm.
    PteRangeMismatch { byte_length: u64, pte_count: u64 },
    /// Both epochs are nonzero but the new one does not strictly follow the
    /// prior one.
    EpochNotMonotonic { prior: u64, new: u64 },
    /// `HELIOS_HPM1_FLAG_TRANSACTION_FIRST` and a zero `multipass_offset` did
    /// not agree. `MultipassOffset` is the sole continuation state (§10.7), so
    /// the two must be the same fact.
    MultipassOffsetInconsistent { multipass_offset: u32, first: bool },
    /// An allocation byte offset was supplied without an allocation generation.
    AllocationOffsetWithoutAllocation,
    // ── Negotiation ─────────────────────────────────────────────────────────
    /// The device did not accept exactly [`HELIOS_HPM1_REQUIRED_FEATURES`].
    /// `missing` are required bits it withheld; `unexpected` are bits outside
    /// the required set (which includes any hypothetical CPU-host-aperture
    /// service).
    UnsupportedFeatures { missing: u64, unexpected: u64 },
    /// The device returned a zero adapter generation.
    ZeroAdapterGeneration,
    /// The device changed a value the KMD supplied.
    NegotiationEchoMismatch {
        field: HpmField,
        found: u64,
        expected: u64,
    },
    /// The BAR length is zero, so there is no HLM1 to expose.
    ZeroBarLength,
    /// The BAR base or length is not a multiple of the negotiated page size.
    BarNotPageAligned { base: u64, length: u64 },
    /// `base + length` overflows the guest-physical address space.
    BarRangeOverflow { base: u64, length: u64 },
    /// The negotiated HLM1 capacity is not the complete BAR length. §10.7
    /// reserves "the complete prefetchable 64-bit host-visible BAR range" and
    /// §17.6 refuses "a missing, short, overlapping, overflowing, or mismatched
    /// capability".
    Hlm1CapacityIsNotWholeBar { capacity: u64, bar_length: u64 },
    /// The device reported a terminal failure for the negotiation itself.
    NegotiationFailed { status: HeliosHpm1Status },
    // ── HLM1 segment report ─────────────────────────────────────────────────
    /// A `DXGK_SEGMENTFLAGS` value did not match the §10.7 profile.
    WrongSegmentFlag {
        field: HpmField,
        found: u32,
        expected: u32,
    },
    /// A segment bound did not match the admitted BAR.
    SegmentBoundsMismatch {
        field: HpmField,
        found: u64,
        expected: u64,
    },
}

// ── HPM1 feature / generation negotiation and HLM1 BAR admission ────────────

/// `HPM1` feature/generation negotiation and complete HLM1 BAR admission
/// (§10.7 "Before `DXGKQAITYPE_QUERYSEGMENT4` can expose HLM1,
/// `DxgkDdiStartDevice` must negotiate HPM1, reserve the complete prefetchable
/// 64-bit host-visible BAR range, and allocate fixed nonpaged
/// `HeliosPhysicalMemoryState`"; §17.6 `pci_caps.rs`/`adapter/physical_memory.rs`;
/// §17.7 QEMU StartDevice).
///
/// One record travels both ways. The KMD fills the `in` fields from the PCI
/// capability walk — the exact guest-physical base and **non-padded** length of
/// the realized prefetchable 64-bit host-visible BAR — and the device fills the
/// `out` fields. `StartDevice` then requires the returned values to equal what
/// the later HLM1 descriptor will report; a missing, short, overlapping,
/// overflowing, or mismatched capability refuses adapter initialization rather
/// than reporting a reduced segment.
///
/// 80 bytes, pointer-free.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosPhysicalMemoryNegotiationV1 {
    /// == [`HELIOS_HPM1_NEGOTIATION_MAGIC`].
    pub magic: u32,
    /// == [`HELIOS_HPM1_ABI_VERSION`].
    pub abi_version: u16,
    /// == [`HELIOS_HPM1_NEGOTIATION_BYTES`].
    pub structure_size: u16,
    /// in: the exact atomic package generation. Mismatch is fatal (§17.1).
    pub package_generation: u64,
    /// in: zero. out: the device-assigned nonzero adapter generation that every
    /// later paging packet must repeat. Reset changes it (§14).
    pub adapter_generation: u64,
    /// in: [`HELIOS_HPM1_REQUIRED_FEATURES`]. The KMD never requests a subset;
    /// §2 makes the package all-or-nothing.
    pub requested_features: u64,
    /// in: zero. out: the accepted set, which must equal `requested_features`.
    pub accepted_features: u64,
    /// in: the KMD-discovered guest-physical base of the prefetchable 64-bit
    /// host-visible BAR. out: echoed unchanged.
    pub bar_guest_physical_base: u64,
    /// in: the KMD-discovered **non-padded** BAR length in bytes. out: echoed
    /// unchanged.
    pub bar_length_bytes: u64,
    /// in: zero. out: the negotiated HPM1 byte capacity, which §10.7 makes
    /// "exactly the size … later reported for HLM1" — i.e. the complete BAR.
    pub hlm1_capacity_bytes: u64,
    /// in: zero. out: the negotiated page shift, [`HELIOS_HPM1_PAGE_SHIFT`] in
    /// this generation, and "exactly the … page granularity later reported for
    /// HLM1".
    pub page_shift: u32,
    /// in: zero. out: [`HeliosHpm1Status`]; only
    /// [`HeliosHpm1Status::Complete`] admits the adapter.
    pub status: u32,
    /// Reserved, zero in both directions.
    pub reserved: u64,
}

/// The validated result of [`validate_negotiation_reply`] — the facts the
/// segment table, the allocator, and every later paging packet are checked
/// against.
///
/// It is a *conclusion*, not a wire struct: it exists so no caller re-derives
/// the BAR bounds from raw fields at a second site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hlm1BarAdmission {
    /// Device-assigned nonzero adapter generation.
    pub adapter_generation: u64,
    /// Guest-physical base of the complete host-visible BAR; this is exactly
    /// HLM1's `CpuTranslatedAddress` (§10.7).
    pub bar_guest_physical_base: u64,
    /// Complete non-padded BAR length in bytes; this is exactly HLM1's `Size`
    /// and `CommitLimit`.
    pub bar_length_bytes: u64,
    /// Negotiated page granularity ([`HELIOS_HPM1_PAGE_SHIFT`]).
    pub page_shift: u32,
    /// `bar_length_bytes >> page_shift` — the HLM1 page bound every run naming
    /// segment 2 is checked against.
    pub hlm1_page_count: u64,
}

impl HeliosPhysicalMemoryNegotiationV1 {
    /// Build the request the KMD sends at `DxgkDdiStartDevice`.
    #[inline]
    pub const fn request(
        package_generation: u64,
        bar_guest_physical_base: u64,
        bar_length_bytes: u64,
    ) -> Self {
        Self {
            magic: HELIOS_HPM1_NEGOTIATION_MAGIC,
            abi_version: HELIOS_HPM1_ABI_VERSION,
            structure_size: HELIOS_HPM1_NEGOTIATION_BYTES,
            package_generation,
            adapter_generation: 0,
            requested_features: HELIOS_HPM1_REQUIRED_FEATURES,
            accepted_features: 0,
            bar_guest_physical_base,
            bar_length_bytes,
            hlm1_capacity_bytes: 0,
            page_shift: 0,
            status: 0,
            reserved: 0,
        }
    }

    /// Fixed-prefix check shared by both directions.
    #[inline]
    const fn validate_prefix(&self, expected_package_generation: u64) -> Result<(), HpmReject> {
        if self.magic != HELIOS_HPM1_NEGOTIATION_MAGIC {
            return Err(HpmReject::BadMagic {
                found: self.magic,
                expected: HELIOS_HPM1_NEGOTIATION_MAGIC,
            });
        }
        if self.abi_version != HELIOS_HPM1_ABI_VERSION {
            return Err(HpmReject::BadAbiVersion {
                found: self.abi_version,
                expected: HELIOS_HPM1_ABI_VERSION,
            });
        }
        if self.structure_size != HELIOS_HPM1_NEGOTIATION_BYTES {
            return Err(HpmReject::BadStructureSize {
                found: self.structure_size,
                expected: HELIOS_HPM1_NEGOTIATION_BYTES,
            });
        }
        // Zero is never a live package generation on *either* side; see
        // `crate::HELIOS_PACKAGE_GENERATION`.
        if self.package_generation != expected_package_generation || self.package_generation == 0 {
            return Err(HpmReject::PackageGenerationMismatch {
                found: self.package_generation,
                expected: expected_package_generation,
            });
        }
        if self.reserved != 0 {
            // This record has one reserved field, not the paging header's
            // `reserved0`/`reserved1` pair; naming a field the struct does not
            // have would point a maintainer at the wrong record.
            return Err(HpmReject::ReservedNonZero {
                field: HpmField::NegotiationReserved,
            });
        }
        Ok(())
    }
}

/// Validate the KMD's outgoing negotiation request. QEMU calls this before
/// touching any BAR state.
///
/// The BAR bounds are checked here rather than only on the reply, because §17.7
/// requires the host to refuse "a missing, short, overlapping, overflowing, or
/// mismatched capability" before it initializes placement state.
pub fn validate_negotiation_request(
    record: &HeliosPhysicalMemoryNegotiationV1,
    expected_package_generation: u64,
) -> Result<(), HpmReject> {
    record.validate_prefix(expected_package_generation)?;
    if record.adapter_generation != 0 {
        return Err(HpmReject::ForbiddenFieldSet {
            field: HpmField::AdapterGeneration,
        });
    }
    if record.accepted_features != 0 {
        return Err(HpmReject::ForbiddenFieldSet {
            field: HpmField::AcceptedFeatures,
        });
    }
    if record.hlm1_capacity_bytes != 0 {
        return Err(HpmReject::ForbiddenFieldSet {
            field: HpmField::Hlm1CapacityBytes,
        });
    }
    if record.page_shift != 0 {
        return Err(HpmReject::ForbiddenFieldSet {
            field: HpmField::PageShift,
        });
    }
    if record.status != 0 {
        return Err(HpmReject::StatusNotPending {
            found: record.status,
        });
    }
    if record.requested_features != HELIOS_HPM1_REQUIRED_FEATURES {
        let missing = HELIOS_HPM1_REQUIRED_FEATURES & !record.requested_features;
        let unexpected = record.requested_features & !HELIOS_HPM1_REQUIRED_FEATURES;
        return Err(HpmReject::UnsupportedFeatures {
            missing,
            unexpected,
        });
    }
    validate_bar_bounds(
        record.bar_guest_physical_base,
        record.bar_length_bytes,
        HELIOS_HPM1_PAGE_SHIFT,
    )
}

/// Validate the device's negotiation reply and derive the HLM1 admission.
///
/// `request` is the exact record the KMD sent; every `in` field must come back
/// unchanged, which is what makes "StartDevice requires those values to equal
/// the later HLM1 descriptor" (§17.6) checkable at one site.
pub fn validate_negotiation_reply(
    request: &HeliosPhysicalMemoryNegotiationV1,
    reply: &HeliosPhysicalMemoryNegotiationV1,
    expected_package_generation: u64,
) -> Result<Hlm1BarAdmission, HpmReject> {
    reply.validate_prefix(expected_package_generation)?;

    let status = HeliosHpm1Status::from_raw(reply.status)?;
    if !status.permits_paging_completion() {
        return Err(HpmReject::NegotiationFailed { status });
    }

    if reply.requested_features != request.requested_features {
        return Err(HpmReject::NegotiationEchoMismatch {
            // The `requested` word, not the `accepted` one: the whole purpose of
            // this refusal is telling the two feature words apart, so naming the
            // wrong one would triage a StartDevice failure against the wrong
            // field.
            field: HpmField::RequestedFeatures,
            found: reply.requested_features,
            expected: request.requested_features,
        });
    }
    if reply.accepted_features != HELIOS_HPM1_REQUIRED_FEATURES {
        let missing = HELIOS_HPM1_REQUIRED_FEATURES & !reply.accepted_features;
        let unexpected = reply.accepted_features & !HELIOS_HPM1_REQUIRED_FEATURES;
        return Err(HpmReject::UnsupportedFeatures {
            missing,
            unexpected,
        });
    }
    if reply.adapter_generation == 0 {
        return Err(HpmReject::ZeroAdapterGeneration);
    }
    if reply.bar_guest_physical_base != request.bar_guest_physical_base {
        return Err(HpmReject::NegotiationEchoMismatch {
            field: HpmField::BarGuestPhysicalBase,
            found: reply.bar_guest_physical_base,
            expected: request.bar_guest_physical_base,
        });
    }
    if reply.bar_length_bytes != request.bar_length_bytes {
        return Err(HpmReject::NegotiationEchoMismatch {
            field: HpmField::BarLengthBytes,
            found: reply.bar_length_bytes,
            expected: request.bar_length_bytes,
        });
    }
    if reply.page_shift != HELIOS_HPM1_PAGE_SHIFT {
        return Err(HpmReject::WrongPageShift {
            found: reply.page_shift,
            expected: HELIOS_HPM1_PAGE_SHIFT,
        });
    }
    validate_bar_bounds(
        reply.bar_guest_physical_base,
        reply.bar_length_bytes,
        reply.page_shift,
    )?;
    if reply.hlm1_capacity_bytes != reply.bar_length_bytes {
        return Err(HpmReject::Hlm1CapacityIsNotWholeBar {
            capacity: reply.hlm1_capacity_bytes,
            bar_length: reply.bar_length_bytes,
        });
    }

    Ok(Hlm1BarAdmission {
        adapter_generation: reply.adapter_generation,
        bar_guest_physical_base: reply.bar_guest_physical_base,
        bar_length_bytes: reply.bar_length_bytes,
        page_shift: reply.page_shift,
        hlm1_page_count: reply.bar_length_bytes >> reply.page_shift,
    })
}

/// Shared BAR bound check: nonzero, page-aligned, and non-overflowing.
const fn validate_bar_bounds(base: u64, length: u64, page_shift: u32) -> Result<(), HpmReject> {
    if length == 0 {
        return Err(HpmReject::ZeroBarLength);
    }
    let page_mask = (1u64 << page_shift) - 1;
    if (base & page_mask) != 0 || (length & page_mask) != 0 {
        return Err(HpmReject::BarNotPageAligned { base, length });
    }
    if base.checked_add(length).is_none() {
        return Err(HpmReject::BarRangeOverflow { base, length });
    }
    Ok(())
}

// ── HLM1 segment report (§10.7, §17.6 `query_adapter_info.rs`) ──────────────

/// The values `query_adapter_info.rs` is about to place in one
/// `DXGK_SEGMENTDESCRIPTOR4` for HLM1, in a form it can validate before
/// reporting them.
///
/// This is not a wire record — it never leaves the guest — but it is declared
/// here because §10.7's flag values are part of the same admission decision as
/// the BAR bounds, and splitting them would let the two drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosHlm1SegmentReport {
    pub segment_id: u32,
    /// `DXGK_SEGMENTFLAGS::Aperture`.
    pub aperture: u32,
    /// `DXGK_SEGMENTFLAGS::CpuVisible`.
    pub cpu_visible: u32,
    /// `DXGK_SEGMENTFLAGS::CacheCoherent`.
    pub cache_coherent: u32,
    /// `DXGK_SEGMENTFLAGS::SupportsCpuHostAperture`.
    pub supports_cpu_host_aperture: u32,
    /// `DXGK_SEGMENTFLAGS::SupportsCachedCpuHostAperture`.
    pub supports_cached_cpu_host_aperture: u32,
    /// `DXGK_SEGMENTDESCRIPTOR4::CpuTranslatedAddress`.
    pub cpu_translated_address: u64,
    /// `DXGK_SEGMENTDESCRIPTOR4::BaseAddress`. §10.7: "HLM1's segment offset is
    /// exactly the offset in that linear PCI aperture", so the segment starts at
    /// zero and the CPU address is `CpuTranslatedAddress + segment offset`.
    pub base_address: u64,
    /// `DXGK_SEGMENTDESCRIPTOR4::Size`.
    pub size: u64,
    /// `DXGK_SEGMENTDESCRIPTOR4::CommitLimit`.
    pub commit_limit: u64,
    /// The segment page granularity reported for HLM1.
    pub page_shift: u32,
}

impl HeliosHlm1SegmentReport {
    /// Check this descriptor against the admitted BAR and the §10.7 flag
    /// profile. Every mismatch names its field.
    pub fn validate(&self, admission: &Hlm1BarAdmission) -> Result<(), HpmReject> {
        if self.segment_id != HELIOS_SEGMENT_ID_HLM1 {
            return Err(HpmReject::WrongSegmentFlag {
                field: HpmField::SegmentId,
                found: self.segment_id,
                expected: HELIOS_SEGMENT_ID_HLM1,
            });
        }
        // The four zeros and the one one, verbatim from §10.7. A wrong value
        // here is the AddAdapter Code 43 class, so each is named separately.
        let flags: [(HpmField, u32, u32); 5] = [
            (
                HpmField::SegmentAperture,
                self.aperture,
                HELIOS_HLM1_FLAG_APERTURE,
            ),
            (
                HpmField::SegmentCpuVisible,
                self.cpu_visible,
                HELIOS_HLM1_FLAG_CPU_VISIBLE,
            ),
            (
                HpmField::SegmentCacheCoherent,
                self.cache_coherent,
                HELIOS_HLM1_FLAG_CACHE_COHERENT,
            ),
            (
                HpmField::SegmentSupportsCpuHostAperture,
                self.supports_cpu_host_aperture,
                HELIOS_HLM1_FLAG_SUPPORTS_CPU_HOST_APERTURE,
            ),
            (
                HpmField::SegmentSupportsCachedCpuHostAperture,
                self.supports_cached_cpu_host_aperture,
                HELIOS_HLM1_FLAG_SUPPORTS_CACHED_CPU_HOST_APERTURE,
            ),
        ];
        let mut i = 0;
        while i < flags.len() {
            let (field, found, expected) = flags[i];
            if found != expected {
                return Err(HpmReject::WrongSegmentFlag {
                    field,
                    found,
                    expected,
                });
            }
            i += 1;
        }

        let bounds: [(HpmField, u64, u64); 5] = [
            (
                HpmField::SegmentCpuTranslatedAddress,
                self.cpu_translated_address,
                admission.bar_guest_physical_base,
            ),
            (HpmField::SegmentBaseAddress, self.base_address, 0),
            (HpmField::SegmentSize, self.size, admission.bar_length_bytes),
            (
                HpmField::SegmentCommitLimit,
                self.commit_limit,
                admission.bar_length_bytes,
            ),
            (
                HpmField::SegmentPageShift,
                self.page_shift as u64,
                admission.page_shift as u64,
            ),
        ];
        let mut j = 0;
        while j < bounds.len() {
            let (field, found, expected) = bounds[j];
            if found != expected {
                return Err(HpmReject::SegmentBoundsMismatch {
                    field,
                    found,
                    expected,
                });
            }
            j += 1;
        }
        Ok(())
    }
}

// ── The paging DMA packet ───────────────────────────────────────────────────

/// One bounded physical-page run (§10.7: "A bounded run contains
/// `{u64 sourcePage,u64 destinationPage,u32 pageCount,u32 reservedZero}`; page
/// numbers are interpreted only in the named source/destination segment").
///
/// Interpretation, per the packet's segment fields:
///   - a page number in [`HELIOS_SEGMENT_ID_SYSTEM`] is a guest **physical**
///     page frame number taken from the OS ADL/MDL — §10.7 admits only the
///     physical-ADL profile, so no logical/IOMMU translation is applied;
///   - a page number in [`HELIOS_SEGMENT_ID_APERTURE`] is a page index within
///     the segment-1 aperture table;
///   - a page number in [`HELIOS_SEGMENT_ID_HLM1`] is a page index within the
///     linear BAR, i.e. its segment offset in pages;
///   - on an address-space arm ([`HpmOperation::UpdatePageTable`]),
///     `destination_page` is the zero-based *virtual* page index within
///     `[destination_gpu_virtual_address, +gpu_virtual_page_count)`, because
///     the device model — not the guest — owns the page tables.
///
/// 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosPhysicalPageRunV1 {
    /// First page of the source range, in `source_segment_id`.
    pub source_page: u64,
    /// First page of the destination range, in `destination_segment_id`.
    pub destination_page: u64,
    /// Pages in this run; nonzero.
    pub page_count: u32,
    /// Reserved, must be zero.
    pub reserved_zero: u32,
}

/// `HPM1` — the KMD/QEMU paging hardware packet
/// (`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.7: "The selected KMD/QEMU
/// hardware packet is `HeliosPhysicalMemoryDmaV1` (`HPM1`)").
///
/// # Framing
///
/// A packet is this 168-byte header immediately followed by `run_count`
/// [`HeliosPhysicalPageRunV1`] records. One paging DMA buffer may contain a
/// *sequence* of packets — `DxgkDdiBuildPagingBuffer` "returns actual hardware
/// work", and one OS operation whose PTE attributes are not uniform becomes
/// several packets rather than a run record with hidden per-run flags.
///
/// # Multipass
///
/// When the bounded buffer cannot carry every run, `MultipassOffset` is the
/// **sole** continuation state (§10.7). [`Self::multipass_offset`] carries the
/// exact `DXGKARG_BUILDPAGINGBUFFER::MultipassOffset` this packet resumes from,
/// and [`HELIOS_HPM1_FLAG_TRANSACTION_FIRST`] is set iff it is zero. There is no
/// side table, cursor, or per-transaction KMD scratch: a second continuation
/// state would be a second source of truth.
///
/// # Completion
///
/// [`Self::status`] is reserved-zero in the guest→device direction and carries
/// the device's [`HeliosHpm1Status`] on the terminal acknowledgement. Only
/// [`HeliosHpm1Status::Complete`] permits the KMD to raise the paging
/// `SubmissionFenceId`; §10.9 forbids advancing it on any other outcome.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosPhysicalMemoryDmaV1 {
    /// == [`HELIOS_HPM1_MAGIC`].
    pub magic: u32,
    /// == [`HELIOS_HPM1_ABI_VERSION`].
    pub abi_version: u16,
    /// == [`HELIOS_HPM1_DMA_HEADER_BYTES`]; the run array starts here.
    pub header_size: u16,
    /// The exact atomic package generation.
    pub package_generation: u64,
    /// The adapter generation returned by
    /// [`validate_negotiation_reply`]. Reset changes it, so a stale queued
    /// packet or a late acknowledgement cannot be adopted (§14).
    pub adapter_generation: u64,
    /// [`HpmOperation`] wire ordinal; zero never decodes.
    pub operation: u32,
    /// == [`HELIOS_HPM1_PHYSICAL_ADAPTER_INDEX`].
    pub physical_adapter_index: u32,
    /// Nonzero, strictly increasing per paging context: the key of §16.2's
    /// "per-paging-context transaction frontier" and the identity the device's
    /// terminal acknowledgement names. It is not a WDDM `SubmissionFenceId` —
    /// that value does not exist yet at `BuildPagingBuffer` time — and the KMD
    /// maps one to the other at paging `DxgkDdiSubmitCommand`.
    pub transaction_id: u64,
    /// The internal KMD `hProcess`-derived address-space ID (§10.7). **User mode
    /// never sees it**, and it is never a handle, PID, or pointer. Zero only for
    /// a non-address-space operation.
    pub address_space_id: u64,
    /// The current root generation for `address_space_id`, published by
    /// [`HpmOperation::SetRootPageTable`] and monotonically increasing. Zero
    /// only for a non-address-space operation.
    pub root_generation: u64,
    /// The live KMD allocation generation this packet acts on. Zero only for a
    /// documented null-allocation/page-table object (aperture map/unmap with a
    /// NULL `hAllocation`, a page-table arm, or a context allocation named only
    /// by GPUVA). It is a KMD-owned generation, never a UMD identity or a host
    /// resource ID.
    pub allocation_generation: u64,
    /// Byte offset within the allocation. Zero when `allocation_generation` is
    /// zero.
    pub allocation_offset: u64,
    /// Byte length of the transfer/fill/discard/report. For a run-bearing arm
    /// the run array must cover exactly the pages this length needs.
    pub byte_length: u64,
    /// Source GPU virtual address of an address-space arm; zero otherwise.
    pub source_gpu_virtual_address: u64,
    /// Destination GPU virtual address of an address-space arm; zero otherwise.
    pub destination_gpu_virtual_address: u64,
    /// Number of PTEs / virtual pages the GPUVA range covers; zero when no
    /// GPUVA range applies.
    pub gpu_virtual_page_count: u64,
    /// The placement or TLB epoch this packet expects to be current. Zero means
    /// "no prior placement"; on the audit arms it is the epoch being audited and
    /// must be nonzero.
    pub prior_epoch: u64,
    /// The placement or TLB epoch the device publishes on terminal completion.
    /// §10.7: "the new epoch is atomically visible and obsolete bindings are
    /// revoked" only after the copy and any required address-space/RCU
    /// transition complete. Forbidden on the audit arms.
    pub new_epoch: u64,
    /// Segment the run records' `source_page` values live in, or
    /// [`HELIOS_HPM1_SEGMENT_NONE`].
    pub source_segment_id: u32,
    /// Segment the run records' `destination_page` values live in, or
    /// [`HELIOS_HPM1_SEGMENT_NONE`].
    pub destination_segment_id: u32,
    /// `HELIOS_HPM1_FLAG_*`; unknown bits are refused.
    pub flags: u32,
    /// Number of [`HeliosPhysicalPageRunV1`] records following this header;
    /// at most [`HELIOS_HPM1_MAX_RUNS_PER_PACKET`].
    pub run_count: u32,
    /// The `DXGKARG_BUILDPAGINGBUFFER::MultipassOffset` this packet resumes
    /// from. Zero iff [`HELIOS_HPM1_FLAG_TRANSACTION_FIRST`] is set.
    pub multipass_offset: u32,
    /// == the negotiated page shift ([`HELIOS_HPM1_PAGE_SHIFT`]). Every page
    /// number and the run-coverage rule are interpreted at this granularity.
    pub page_shift: u32,
    /// `DXGK_BUILDPAGINGBUFFER_FILL{,VIRTUAL}::FillPattern`; zero on every
    /// other arm.
    pub fill_pattern: u32,
    /// Reserved-zero on submit; [`HeliosHpm1Status`] on the device's terminal
    /// acknowledgement.
    pub status: u32,
    /// Reserved, must be zero.
    pub reserved0: u64,
    /// Reserved, must be zero.
    pub reserved1: u64,
}

/// The live device facts one packet is validated against.
///
/// Passing these in rather than reading a global is what lets the same
/// validator run in the KMD before enqueue, in QEMU on receipt, and in a host
/// unit test — §17.7 requires QEMU to validate "package/adapter/allocation
/// generation, operation, segment/ADL page runs, range, and prior epoch", which
/// is exactly this set plus the caller's own allocation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HpmContext {
    /// The exact atomic package generation.
    pub package_generation: u64,
    /// The adapter generation from [`Hlm1BarAdmission`].
    pub adapter_generation: u64,
    /// The negotiated page shift.
    pub page_shift: u32,
    /// Pages in HLM1 (segment 2) — [`Hlm1BarAdmission::hlm1_page_count`].
    pub hlm1_page_count: u64,
    /// Pages in the segment-1 aperture table. The aperture's size is the KMD's
    /// own choice, so it is supplied rather than derived; a caller that has not
    /// sized its aperture yet must pass zero, which refuses every aperture page.
    pub aperture_page_count: u64,
}

impl HeliosPhysicalMemoryDmaV1 {
    /// A zeroed packet with the fixed prefix, page shift, and both segment
    /// sentinels already correct.
    ///
    /// The sentinels are the reason this constructor exists: an arm that does
    /// not use a segment role must write [`HELIOS_HPM1_SEGMENT_NONE`], and a
    /// caller that built the header with `Zeroable::zeroed()` would instead
    /// claim *system memory* on both ends.
    #[inline]
    pub const fn new(
        package_generation: u64,
        adapter_generation: u64,
        transaction_id: u64,
        operation: HpmOperation,
    ) -> Self {
        Self {
            magic: HELIOS_HPM1_MAGIC,
            abi_version: HELIOS_HPM1_ABI_VERSION,
            header_size: HELIOS_HPM1_DMA_HEADER_BYTES,
            package_generation,
            adapter_generation,
            operation: operation.as_raw(),
            physical_adapter_index: HELIOS_HPM1_PHYSICAL_ADAPTER_INDEX,
            transaction_id,
            address_space_id: 0,
            root_generation: 0,
            allocation_generation: 0,
            allocation_offset: 0,
            byte_length: 0,
            source_gpu_virtual_address: 0,
            destination_gpu_virtual_address: 0,
            gpu_virtual_page_count: 0,
            prior_epoch: 0,
            new_epoch: 0,
            source_segment_id: HELIOS_HPM1_SEGMENT_NONE,
            destination_segment_id: HELIOS_HPM1_SEGMENT_NONE,
            flags: HELIOS_HPM1_FLAG_TRANSACTION_FIRST | HELIOS_HPM1_FLAG_TRANSACTION_LAST,
            run_count: 0,
            multipass_offset: 0,
            page_shift: HELIOS_HPM1_PAGE_SHIFT,
            fill_pattern: 0,
            status: 0,
            reserved0: 0,
            reserved1: 0,
        }
    }

    /// Total byte size of this packet including its run array, or `None` on
    /// overflow. `None` is only reachable for a `run_count` this ABI already
    /// rejects.
    #[inline]
    pub const fn packet_bytes(&self) -> Option<usize> {
        let runs = self.run_count as usize;
        match runs.checked_mul(HELIOS_HPM1_RUN_BYTES) {
            Some(run_bytes) => (HELIOS_HPM1_DMA_HEADER_BYTES as usize).checked_add(run_bytes),
            None => None,
        }
    }

    /// Decode the operation, refusing an unknown ordinal.
    #[inline]
    pub const fn operation(&self) -> Result<HpmOperation, HpmReject> {
        HpmOperation::from_raw(self.operation)
    }

    /// Decode the terminal acknowledgement status.
    #[inline]
    pub const fn status(&self) -> Result<HeliosHpm1Status, HpmReject> {
        HeliosHpm1Status::from_raw(self.status)
    }

    /// Validate the fixed header against the live device facts and this
    /// operation's per-arm obligation.
    ///
    /// This does **not** inspect the run array — [`Self::validate_runs`] does —
    /// so a caller that has not yet bounds-checked the buffer can reject a
    /// malformed header before reading one more byte.
    pub fn validate_header(&self, ctx: &HpmContext) -> Result<HpmOperation, HpmReject> {
        if self.magic != HELIOS_HPM1_MAGIC {
            return Err(HpmReject::BadMagic {
                found: self.magic,
                expected: HELIOS_HPM1_MAGIC,
            });
        }
        if self.abi_version != HELIOS_HPM1_ABI_VERSION {
            return Err(HpmReject::BadAbiVersion {
                found: self.abi_version,
                expected: HELIOS_HPM1_ABI_VERSION,
            });
        }
        if self.header_size != HELIOS_HPM1_DMA_HEADER_BYTES {
            return Err(HpmReject::BadStructureSize {
                found: self.header_size,
                expected: HELIOS_HPM1_DMA_HEADER_BYTES,
            });
        }
        // Zero is never a live package generation on *either* side; see
        // `crate::HELIOS_PACKAGE_GENERATION`.
        if self.package_generation != ctx.package_generation || self.package_generation == 0 {
            return Err(HpmReject::PackageGenerationMismatch {
                found: self.package_generation,
                expected: ctx.package_generation,
            });
        }
        if self.adapter_generation != ctx.adapter_generation {
            return Err(HpmReject::AdapterGenerationMismatch {
                found: self.adapter_generation,
                expected: ctx.adapter_generation,
            });
        }
        if self.physical_adapter_index != HELIOS_HPM1_PHYSICAL_ADAPTER_INDEX {
            return Err(HpmReject::WrongPhysicalAdapterIndex {
                found: self.physical_adapter_index,
            });
        }
        // The caller's own context first. `HpmContext` is a plain struct with no
        // constructor, and negotiation admits exactly one page shift
        // ([`HELIOS_HPM1_PAGE_SHIFT`]), so a context carrying any other value is
        // a caller bug that would otherwise reach `1u64 << ctx.page_shift` in
        // `validate_runs` — a shift-overflow panic, which in the `panic=abort`
        // KMD image is a bugcheck.
        if ctx.page_shift != HELIOS_HPM1_PAGE_SHIFT {
            return Err(HpmReject::ContextPageShiftUnsupported {
                found: ctx.page_shift,
                expected: HELIOS_HPM1_PAGE_SHIFT,
            });
        }
        if self.page_shift != ctx.page_shift {
            return Err(HpmReject::WrongPageShift {
                found: self.page_shift,
                expected: ctx.page_shift,
            });
        }
        if self.transaction_id == 0 {
            return Err(HpmReject::ZeroTransactionId);
        }
        if self.status != HeliosHpm1Status::Pending as u32 {
            return Err(HpmReject::StatusNotPending { found: self.status });
        }
        if self.reserved0 != 0 {
            return Err(HpmReject::ReservedNonZero {
                field: HpmField::Reserved0,
            });
        }
        if self.reserved1 != 0 {
            return Err(HpmReject::ReservedNonZero {
                field: HpmField::Reserved1,
            });
        }

        let operation = self.operation()?;
        let shape = operation.shape();

        // Flags: unknown bits first (ABI drift), then per-arm bits (a legal bit
        // on the wrong operation).
        let unknown = self.flags & !HELIOS_HPM1_FLAG_MASK;
        if unknown != 0 {
            return Err(HpmReject::UnknownFlagBits { bits: unknown });
        }
        let not_allowed = self.flags & !shape.allowed_flags;
        if not_allowed != 0 {
            return Err(HpmReject::FlagNotAllowedForOperation { bits: not_allowed });
        }

        // MultipassOffset is the sole continuation state, so the FIRST marker
        // and a zero offset must be the same fact.
        let first = (self.flags & HELIOS_HPM1_FLAG_TRANSACTION_FIRST) != 0;
        if first != (self.multipass_offset == 0) {
            return Err(HpmReject::MultipassOffsetInconsistent {
                multipass_offset: self.multipass_offset,
                first,
            });
        }

        // The address-space pair is one obligation, not two: a packet with an
        // address-space id but no root generation names an address space whose
        // contents it cannot have validated.
        check_rule(
            shape.address_space,
            self.address_space_id,
            HpmField::AddressSpaceId,
        )?;
        check_rule(
            shape.address_space,
            self.root_generation,
            HpmField::RootGeneration,
        )?;
        check_rule(
            shape.allocation_generation,
            self.allocation_generation,
            HpmField::AllocationGeneration,
        )?;
        if self.allocation_generation == 0 && self.allocation_offset != 0 {
            return Err(HpmReject::AllocationOffsetWithoutAllocation);
        }
        check_rule(
            shape.source_gpu_virtual_address,
            self.source_gpu_virtual_address,
            HpmField::SourceGpuVirtualAddress,
        )?;
        check_rule(
            shape.destination_gpu_virtual_address,
            self.destination_gpu_virtual_address,
            HpmField::DestinationGpuVirtualAddress,
        )?;
        check_rule(
            shape.gpu_virtual_page_count,
            self.gpu_virtual_page_count,
            HpmField::GpuVirtualPageCount,
        )?;
        check_rule(shape.byte_length, self.byte_length, HpmField::ByteLength)?;
        check_rule(
            shape.fill_pattern,
            self.fill_pattern as u64,
            HpmField::FillPattern,
        )?;
        check_rule(shape.new_epoch, self.new_epoch, HpmField::NewEpoch)?;
        check_rule(shape.prior_epoch, self.prior_epoch, HpmField::PriorEpoch)?;
        if self.prior_epoch != 0 && self.new_epoch != 0 && self.new_epoch <= self.prior_epoch {
            return Err(HpmReject::EpochNotMonotonic {
                prior: self.prior_epoch,
                new: self.new_epoch,
            });
        }

        check_segment_role(
            shape.source_segment,
            self.source_segment_id,
            HpmField::SourceSegmentId,
        )?;
        check_segment_role(
            shape.destination_segment,
            self.destination_segment_id,
            HpmField::DestinationSegmentId,
        )?;
        // A classic transfer between two system ranges is not HPM1's work; it
        // would also leave `new_epoch` describing a placement that never moved.
        if matches!(operation, HpmOperation::Transfer)
            && self.source_segment_id == HELIOS_SEGMENT_ID_SYSTEM
            && self.destination_segment_id == HELIOS_SEGMENT_ID_SYSTEM
        {
            return Err(HpmReject::TransferNamesNoDeviceSegment);
        }

        match shape.runs {
            Rule::Required => {
                if self.run_count == 0 || self.run_count > HELIOS_HPM1_MAX_RUNS_PER_PACKET {
                    return Err(HpmReject::RunCountOutOfRange {
                        found: self.run_count,
                    });
                }
            }
            Rule::Forbidden => {
                if self.run_count != 0 {
                    return Err(HpmReject::RunCountOutOfRange {
                        found: self.run_count,
                    });
                }
            }
            // No arm declares `Optional` runs; a run array that may or may not
            // be present is exactly the max-union this table exists to forbid.
            Rule::Optional => {
                return Err(HpmReject::RunCountOutOfRange {
                    found: self.run_count,
                })
            }
        }

        Ok(operation)
    }

    /// Validate the run array against this header.
    ///
    /// Coverage is exact in **both** directions: the runs must cover the pages
    /// `byte_length` needs and no more. Under-covering would leave part of the
    /// range unwritten; over-covering would let a packet touch a page its
    /// declared byte range never reaches, which is the same defect class as the
    /// max-union validation CLAUDE.md forbids.
    ///
    /// On the page-table arm the same exactness is required of the *virtual*
    /// side: `destination_page` is a virtual page index there, so the runs must
    /// tile `[0, gpu_virtual_page_count)` in order. The aggregate page count
    /// alone does not say that — four runs of one page each at virtual page 0
    /// sum correctly while leaving three declared PTEs unwritten and writing one
    /// four times.
    pub fn validate_runs(
        &self,
        runs: &[HeliosPhysicalPageRunV1],
        ctx: &HpmContext,
    ) -> Result<(), HpmReject> {
        if runs.len() != self.run_count as usize {
            return Err(HpmReject::RunCountOutOfRange {
                found: self.run_count,
            });
        }
        // This routine is `pub` and reachable without `validate_header`, so it
        // repeats the context's page-shift gate: `1u64 << ctx.page_shift` below
        // is a shift-overflow panic for `page_shift >= 64`, and a panic in a DDI
        // is a silent graphics deadlock.
        if ctx.page_shift != HELIOS_HPM1_PAGE_SHIFT {
            return Err(HpmReject::ContextPageShiftUnsupported {
                found: ctx.page_shift,
                expected: HELIOS_HPM1_PAGE_SHIFT,
            });
        }
        let operation = self.operation()?;
        let shape = operation.shape();

        let mut total_pages: u64 = 0;
        // The next virtual page index the page-table arm's runs must name.
        let mut next_virtual_page: u64 = 0;
        for (index, run) in runs.iter().enumerate() {
            let index = index as u32;
            if run.reserved_zero != 0 {
                return Err(HpmReject::RunReservedNonZero { index });
            }
            if run.page_count == 0 {
                return Err(HpmReject::ZeroPageCountInRun { index });
            }
            let pages = run.page_count as u64;

            check_run_page(
                shape.run_source_page,
                run.source_page,
                pages,
                index,
                HpmField::RunSourcePage,
                self.source_segment_id,
                ctx,
            )?;
            if shape.run_destination_page_is_virtual {
                // Not a segment page: bound it against the PTE range this same
                // packet declared, and require the runs to tile that range.
                if run.destination_page != next_virtual_page {
                    return Err(HpmReject::VirtualRunNotContiguous {
                        index,
                        expected: next_virtual_page,
                        found: run.destination_page,
                    });
                }
                let end = match run.destination_page.checked_add(pages) {
                    Some(end) => end,
                    None => return Err(HpmReject::PageRangeOverflow { index }),
                };
                if end > self.gpu_virtual_page_count {
                    return Err(HpmReject::VirtualPageOutsideRange {
                        index,
                        page: run.destination_page,
                        pages,
                        gpu_virtual_page_count: self.gpu_virtual_page_count,
                    });
                }
                next_virtual_page = end;
            } else {
                check_run_page(
                    shape.run_destination_page,
                    run.destination_page,
                    pages,
                    index,
                    HpmField::RunDestinationPage,
                    self.destination_segment_id,
                    ctx,
                )?;
            }

            total_pages = match total_pages.checked_add(pages) {
                Some(t) => t,
                None => return Err(HpmReject::PageRangeOverflow { index }),
            };
        }


        if shape.runs == Rule::Forbidden {
            // Already enforced by `validate_header`; nothing further to check.
            return Ok(());
        }

        // Exact coverage: `total_pages` must be the page count `byte_length`
        // needs at the negotiated granularity.
        let page_size = 1u64 << ctx.page_shift;
        let needed_pages = self
            .byte_length
            .checked_add(page_size - 1)
            .map(|v| v >> ctx.page_shift);
        let needed_pages = match needed_pages {
            Some(v) => v,
            None => return Err(HpmReject::PageRangeOverflow { index: 0 }),
        };
        if needed_pages != total_pages {
            return Err(HpmReject::RunCoverageMismatch {
                byte_length: self.byte_length,
                covered_pages: total_pages,
            });
        }

        // A page-table arm additionally states its PTE count; the two must be
        // the same range described twice, never two independent claims.
        if shape.gpu_virtual_page_count == Rule::Required
            && self.gpu_virtual_page_count != total_pages
        {
            return Err(HpmReject::PteRangeMismatch {
                byte_length: self.byte_length,
                pte_count: self.gpu_virtual_page_count,
            });
        }

        // Exact coverage of the *virtual* side, checked last so it can only fire
        // on a packet whose aggregate page count already agrees — i.e. on runs
        // that sum correctly but do not tile the declared PTE range.
        if shape.run_destination_page_is_virtual
            && shape.runs == Rule::Required
            && next_virtual_page != self.gpu_virtual_page_count
        {
            return Err(HpmReject::VirtualRangeNotFullyCovered {
                covered: next_virtual_page,
                gpu_virtual_page_count: self.gpu_virtual_page_count,
            });
        }

        Ok(())
    }
}

/// A validated view over one packet inside a paging DMA buffer.
#[derive(Debug, Clone, Copy)]
pub struct HpmPacket<'a> {
    /// The fixed header.
    pub header: &'a HeliosPhysicalMemoryDmaV1,
    /// Exactly `header.run_count` runs.
    pub runs: &'a [HeliosPhysicalPageRunV1],
    /// The decoded operation.
    pub operation: HpmOperation,
    /// Bytes this packet consumed from the buffer; the next packet, if any,
    /// starts here.
    pub packet_bytes: usize,
}

/// Parse and fully validate one packet at the start of `bytes`.
///
/// Total: no panic, no allocation, and no dereference of anything the header
/// has not already been bounds-checked for. A paging DMA buffer is walked by
/// calling this repeatedly and advancing by
/// [`HpmPacket::packet_bytes`].
///
/// ⛔ **Caller contract — the buffer must not be concurrently writable.** The
/// returned [`HpmPacket`] borrows `bytes`, and the caller then acts on fields it
/// re-reads through that borrow. §10.7 has the KMD *snapshot* the packet at
/// paging `DxgkDdiSubmitCommand` before enqueueing it, and the host must
/// likewise validate the snapshot it will execute — validating bytes their
/// producer can still store to validates a packet that no longer exists.
pub fn parse_packet<'a>(bytes: &'a [u8], ctx: &HpmContext) -> Result<HpmPacket<'a>, HpmReject> {
    let header_bytes = HELIOS_HPM1_DMA_HEADER_BYTES as usize;
    if bytes.len() < header_bytes {
        return Err(HpmReject::TruncatedPacket {
            needed: header_bytes,
            available: bytes.len(),
        });
    }
    let header: &HeliosPhysicalMemoryDmaV1 = match bytemuck::try_from_bytes(&bytes[..header_bytes])
    {
        Ok(h) => h,
        // The only failure `try_from_bytes` can report for an exact-size
        // slice is misalignment.
        Err(_) => return Err(HpmReject::MisalignedPacket),
    };

    let operation = header.validate_header(ctx)?;

    let packet_bytes = match header.packet_bytes() {
        Some(n) => n,
        None => {
            return Err(HpmReject::RunCountOutOfRange {
                found: header.run_count,
            })
        }
    };
    if bytes.len() < packet_bytes {
        return Err(HpmReject::TruncatedPacket {
            needed: packet_bytes,
            available: bytes.len(),
        });
    }
    let runs: &[HeliosPhysicalPageRunV1] =
        match bytemuck::try_cast_slice(&bytes[header_bytes..packet_bytes]) {
            Ok(r) => r,
            Err(_) => return Err(HpmReject::MisalignedPacket),
        };
    header.validate_runs(runs, ctx)?;

    Ok(HpmPacket {
        header,
        runs,
        operation,
        packet_bytes,
    })
}

/// Apply one scalar field obligation.
#[inline]
const fn check_rule(rule: Rule, value: u64, field: HpmField) -> Result<(), HpmReject> {
    match rule {
        Rule::Required if value == 0 => Err(HpmReject::MissingRequiredField { field }),
        Rule::Forbidden if value != 0 => Err(HpmReject::ForbiddenFieldSet { field }),
        _ => Ok(()),
    }
}

/// Apply one segment-role obligation.
#[inline]
const fn check_segment_role(
    role: SegmentRole,
    value: u32,
    field: HpmField,
) -> Result<(), HpmReject> {
    match role {
        SegmentRole::Unused => {
            if value != HELIOS_HPM1_SEGMENT_NONE {
                Err(HpmReject::SegmentNotAllowedForRole {
                    field,
                    found: value,
                })
            } else {
                Ok(())
            }
        }
        SegmentRole::AnySegment => match value {
            HELIOS_SEGMENT_ID_SYSTEM | HELIOS_SEGMENT_ID_APERTURE | HELIOS_SEGMENT_ID_HLM1 => {
                Ok(())
            }
            other => Err(HpmReject::UnknownSegmentId {
                field,
                found: other,
            }),
        },
        SegmentRole::ExactSystem => {
            if value != HELIOS_SEGMENT_ID_SYSTEM {
                Err(HpmReject::SegmentNotAllowedForRole {
                    field,
                    found: value,
                })
            } else {
                Ok(())
            }
        }
        SegmentRole::ExactAperture => {
            if value != HELIOS_SEGMENT_ID_APERTURE {
                Err(HpmReject::SegmentNotAllowedForRole {
                    field,
                    found: value,
                })
            } else {
                Ok(())
            }
        }
    }
}

/// Apply one run page-number obligation, including the per-segment page bound.
///
/// System pages are not bounded here: their legality is the OS ADL's, and this
/// module has no guest RAM map. Aperture and HLM1 pages are bounded, because
/// their sizes are exactly what negotiation admitted.
#[inline]
const fn check_run_page(
    rule: Rule,
    page: u64,
    pages: u64,
    index: u32,
    field: HpmField,
    segment_id: u32,
    ctx: &HpmContext,
) -> Result<(), HpmReject> {
    match rule {
        Rule::Forbidden => {
            if page != 0 {
                return Err(HpmReject::ForbiddenFieldSet { field });
            }
            return Ok(());
        }
        Rule::Required | Rule::Optional => {}
    }
    let end = match page.checked_add(pages) {
        Some(end) => end,
        None => return Err(HpmReject::PageRangeOverflow { index }),
    };
    let bound = match segment_id {
        HELIOS_SEGMENT_ID_APERTURE => ctx.aperture_page_count,
        HELIOS_SEGMENT_ID_HLM1 => ctx.hlm1_page_count,
        // System memory and the virtual-index interpretation have no bound
        // this module can check.
        _ => return Ok(()),
    };
    if end > bound {
        return Err(HpmReject::PageOutsideSegment {
            index,
            field,
            segment_id,
            page,
        });
    }
    Ok(())
}

// ── Layout assertions ───────────────────────────────────────────────────────
//
// A wrong offset must break the build, not a test. Every value below is
// mirrored by a `_Static_assert` in
// `qemu-helios/include/hw/virtio/helios_physical_memory.h`.

const _: () = {
    use core::mem::{align_of, offset_of, size_of};

    // HeliosPhysicalPageRunV1 — §10.7's exact 24-byte run record.
    assert!(size_of::<HeliosPhysicalPageRunV1>() == 24);
    assert!(align_of::<HeliosPhysicalPageRunV1>() == 8);
    assert!(offset_of!(HeliosPhysicalPageRunV1, source_page) == 0);
    assert!(offset_of!(HeliosPhysicalPageRunV1, destination_page) == 8);
    assert!(offset_of!(HeliosPhysicalPageRunV1, page_count) == 16);
    assert!(offset_of!(HeliosPhysicalPageRunV1, reserved_zero) == 20);
    assert!(size_of::<HeliosPhysicalPageRunV1>() == HELIOS_HPM1_RUN_BYTES);

    // HeliosPhysicalMemoryDmaV1 — the fixed header.
    assert!(size_of::<HeliosPhysicalMemoryDmaV1>() == 168);
    assert!(align_of::<HeliosPhysicalMemoryDmaV1>() == 8);
    assert!(size_of::<HeliosPhysicalMemoryDmaV1>() == HELIOS_HPM1_DMA_HEADER_BYTES as usize);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, magic) == 0);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, abi_version) == 4);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, header_size) == 6);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, package_generation) == 8);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, adapter_generation) == 16);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, operation) == 24);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, physical_adapter_index) == 28);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, transaction_id) == 32);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, address_space_id) == 40);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, root_generation) == 48);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, allocation_generation) == 56);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, allocation_offset) == 64);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, byte_length) == 72);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, source_gpu_virtual_address) == 80);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, destination_gpu_virtual_address) == 88);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, gpu_virtual_page_count) == 96);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, prior_epoch) == 104);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, new_epoch) == 112);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, source_segment_id) == 120);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, destination_segment_id) == 124);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, flags) == 128);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, run_count) == 132);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, multipass_offset) == 136);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, page_shift) == 140);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, fill_pattern) == 144);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, status) == 148);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, reserved0) == 152);
    assert!(offset_of!(HeliosPhysicalMemoryDmaV1, reserved1) == 160);

    // HeliosPhysicalMemoryNegotiationV1 — HPM1 feature/generation negotiation
    // plus the complete HLM1 BAR admission fields.
    assert!(size_of::<HeliosPhysicalMemoryNegotiationV1>() == 80);
    assert!(align_of::<HeliosPhysicalMemoryNegotiationV1>() == 8);
    assert!(
        size_of::<HeliosPhysicalMemoryNegotiationV1>() == HELIOS_HPM1_NEGOTIATION_BYTES as usize
    );
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, magic) == 0);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, abi_version) == 4);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, structure_size) == 6);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, package_generation) == 8);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, adapter_generation) == 16);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, requested_features) == 24);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, accepted_features) == 32);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, bar_guest_physical_base) == 40);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, bar_length_bytes) == 48);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, hlm1_capacity_bytes) == 56);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, page_shift) == 64);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, status) == 68);
    assert!(offset_of!(HeliosPhysicalMemoryNegotiationV1, reserved) == 72);

    // Bounds. The second assertion is load-bearing: if one maximal packet did
    // not fit one paging DMA buffer, `MultipassOffset` continuation could make
    // no forward progress.
    assert!(HELIOS_HPM1_MAX_PACKET_BYTES == 168 + 24 * 2048);
    assert!(HELIOS_HPM1_MAX_PACKET_BYTES <= HELIOS_HPM1_PAGING_BUFFER_BYTES as usize);
    assert!(HELIOS_HPM1_PAGE_SHIFT == 12);
    // The one legal page shift must keep `1u64 << shift` in range; every
    // validator that forms a page size from it relies on this.
    assert!(HELIOS_HPM1_PAGE_SHIFT < 64);

    // Both magics are four ASCII bytes read little-endian. A transposed hex
    // digit in either literal would otherwise compile clean and fail only
    // against the QEMU mirror or a live host.
    assert!(HELIOS_HPM1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HPM1_MAGIC.to_le_bytes()[1] == b'P');
    assert!(HELIOS_HPM1_MAGIC.to_le_bytes()[2] == b'M');
    assert!(HELIOS_HPM1_MAGIC.to_le_bytes()[3] == b'1');
    assert!(HELIOS_HPM1_NEGOTIATION_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HPM1_NEGOTIATION_MAGIC.to_le_bytes()[1] == b'P');
    assert!(HELIOS_HPM1_NEGOTIATION_MAGIC.to_le_bytes()[2] == b'M');
    assert!(HELIOS_HPM1_NEGOTIATION_MAGIC.to_le_bytes()[3] == b'N');

    // The segment ids are distinct and the sentinel collides with none of them.
    assert!(HELIOS_SEGMENT_ID_SYSTEM != HELIOS_SEGMENT_ID_APERTURE);
    assert!(HELIOS_SEGMENT_ID_APERTURE != HELIOS_SEGMENT_ID_HLM1);
    assert!(HELIOS_HPM1_SEGMENT_NONE != HELIOS_SEGMENT_ID_SYSTEM);
    assert!(HELIOS_HPM1_SEGMENT_NONE != HELIOS_SEGMENT_ID_APERTURE);
    assert!(HELIOS_HPM1_SEGMENT_NONE != HELIOS_SEGMENT_ID_HLM1);

    // Zero must never decode as an operation, so a zero-filled paging buffer
    // cannot be mistaken for `DXGK_OPERATION_TRANSFER`.
    assert!(HpmOperation::Transfer.as_raw() != 0);

    // The two magics must differ, or an admission record could be walked as a
    // paging packet.
    assert!(HELIOS_HPM1_MAGIC != HELIOS_HPM1_NEGOTIATION_MAGIC);
};

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: u64 = 0x0102_0304_0506_0708;
    const ADAPTER: u64 = 0x1111_2222_3333_4444;
    const BAR_BASE: u64 = 0x0000_0008_0000_0000;
    const BAR_LEN: u64 = 256 * 1024 * 1024;

    fn ctx() -> HpmContext {
        HpmContext {
            package_generation: PKG,
            adapter_generation: ADAPTER,
            page_shift: HELIOS_HPM1_PAGE_SHIFT,
            hlm1_page_count: BAR_LEN >> HELIOS_HPM1_PAGE_SHIFT,
            aperture_page_count: 1 << 20,
        }
    }

    fn reply(request: &HeliosPhysicalMemoryNegotiationV1) -> HeliosPhysicalMemoryNegotiationV1 {
        let mut r = *request;
        r.adapter_generation = ADAPTER;
        r.accepted_features = HELIOS_HPM1_REQUIRED_FEATURES;
        r.hlm1_capacity_bytes = r.bar_length_bytes;
        r.page_shift = HELIOS_HPM1_PAGE_SHIFT;
        r.status = HeliosHpm1Status::Complete as u32;
        r
    }

    /// Zero is never a wildcard on *either* side of the package-generation
    /// comparison. A zeroed negotiation record reaching a host that has not yet
    /// established the package generation must be refused, not admitted because
    /// `0 == 0` — see [`crate::HELIOS_PACKAGE_GENERATION`].
    #[test]
    fn negotiation_refuses_a_zero_package_generation_on_both_sides() {
        let zeroed = HeliosPhysicalMemoryNegotiationV1::request(0, BAR_BASE, BAR_LEN);
        assert_eq!(
            validate_negotiation_request(&zeroed, 0),
            Err(HpmReject::PackageGenerationMismatch {
                found: 0,
                expected: 0,
            })
        );

        let live = HeliosPhysicalMemoryNegotiationV1::request(PKG, BAR_BASE, BAR_LEN);
        assert_eq!(
            validate_negotiation_request(&live, 0),
            Err(HpmReject::PackageGenerationMismatch {
                found: PKG,
                expected: 0,
            })
        );
    }

    #[test]
    fn negotiation_round_trip_yields_the_whole_bar() {
        let req = HeliosPhysicalMemoryNegotiationV1::request(PKG, BAR_BASE, BAR_LEN);
        assert_eq!(validate_negotiation_request(&req, PKG), Ok(()));
        let admission = validate_negotiation_reply(&req, &reply(&req), PKG).unwrap();
        assert_eq!(
            admission,
            Hlm1BarAdmission {
                adapter_generation: ADAPTER,
                bar_guest_physical_base: BAR_BASE,
                bar_length_bytes: BAR_LEN,
                page_shift: HELIOS_HPM1_PAGE_SHIFT,
                hlm1_page_count: BAR_LEN >> HELIOS_HPM1_PAGE_SHIFT,
            }
        );
    }

    /// A host that withholds a required feature, or offers one outside the
    /// required set — the shape a CPU-host-aperture service would arrive in —
    /// is refused rather than adopted.
    #[test]
    fn negotiation_refuses_any_feature_set_but_the_exact_one() {
        let req = HeliosPhysicalMemoryNegotiationV1::request(PKG, BAR_BASE, BAR_LEN);

        let mut short = reply(&req);
        short.accepted_features &= !HELIOS_HPM1_FEATURE_PAGE_TABLES;
        assert_eq!(
            validate_negotiation_reply(&req, &short, PKG),
            Err(HpmReject::UnsupportedFeatures {
                missing: HELIOS_HPM1_FEATURE_PAGE_TABLES,
                unexpected: 0,
            })
        );

        let mut extra = reply(&req);
        extra.accepted_features |= 1 << 20;
        assert_eq!(
            validate_negotiation_reply(&req, &extra, PKG),
            Err(HpmReject::UnsupportedFeatures {
                missing: 0,
                unexpected: 1 << 20,
            })
        );
    }

    #[test]
    fn negotiation_refuses_a_short_or_moved_bar() {
        let req = HeliosPhysicalMemoryNegotiationV1::request(PKG, BAR_BASE, BAR_LEN);

        let mut short = reply(&req);
        short.hlm1_capacity_bytes = BAR_LEN / 2;
        assert_eq!(
            validate_negotiation_reply(&req, &short, PKG),
            Err(HpmReject::Hlm1CapacityIsNotWholeBar {
                capacity: BAR_LEN / 2,
                bar_length: BAR_LEN,
            })
        );

        let mut moved = reply(&req);
        moved.bar_guest_physical_base = BAR_BASE + 4096;
        assert_eq!(
            validate_negotiation_reply(&req, &moved, PKG),
            Err(HpmReject::NegotiationEchoMismatch {
                field: HpmField::BarGuestPhysicalBase,
                found: BAR_BASE + 4096,
                expected: BAR_BASE,
            })
        );

        let unaligned = HeliosPhysicalMemoryNegotiationV1::request(PKG, BAR_BASE + 1, BAR_LEN);
        assert_eq!(
            validate_negotiation_request(&unaligned, PKG),
            Err(HpmReject::BarNotPageAligned {
                base: BAR_BASE + 1,
                length: BAR_LEN,
            })
        );
    }

    #[test]
    fn hlm1_segment_report_matches_the_section_10_7_profile() {
        let req = HeliosPhysicalMemoryNegotiationV1::request(PKG, BAR_BASE, BAR_LEN);
        let admission = validate_negotiation_reply(&req, &reply(&req), PKG).unwrap();
        let good = HeliosHlm1SegmentReport {
            segment_id: HELIOS_SEGMENT_ID_HLM1,
            aperture: HELIOS_HLM1_FLAG_APERTURE,
            cpu_visible: HELIOS_HLM1_FLAG_CPU_VISIBLE,
            cache_coherent: HELIOS_HLM1_FLAG_CACHE_COHERENT,
            supports_cpu_host_aperture: HELIOS_HLM1_FLAG_SUPPORTS_CPU_HOST_APERTURE,
            supports_cached_cpu_host_aperture: HELIOS_HLM1_FLAG_SUPPORTS_CACHED_CPU_HOST_APERTURE,
            cpu_translated_address: BAR_BASE,
            base_address: 0,
            size: BAR_LEN,
            commit_limit: BAR_LEN,
            page_shift: HELIOS_HPM1_PAGE_SHIFT,
        };
        assert_eq!(good.validate(&admission), Ok(()));

        // The one flag that must be set.
        let mut invisible = good;
        invisible.cpu_visible = 0;
        assert_eq!(
            invisible.validate(&admission),
            Err(HpmReject::WrongSegmentFlag {
                field: HpmField::SegmentCpuVisible,
                found: 0,
                expected: 1,
            })
        );

        // The package advertises no CPU Host Aperture at all.
        let mut hap = good;
        hap.supports_cpu_host_aperture = 1;
        assert_eq!(
            hap.validate(&admission),
            Err(HpmReject::WrongSegmentFlag {
                field: HpmField::SegmentSupportsCpuHostAperture,
                found: 1,
                expected: 0,
            })
        );

        // C52: `CacheCoherent` applies only to aperture segments.
        let mut coherent = good;
        coherent.cache_coherent = 1;
        assert_eq!(
            coherent.validate(&admission),
            Err(HpmReject::WrongSegmentFlag {
                field: HpmField::SegmentCacheCoherent,
                found: 1,
                expected: 0,
            })
        );

        let mut short = good;
        short.size = BAR_LEN / 2;
        assert_eq!(
            short.validate(&admission),
            Err(HpmReject::SegmentBoundsMismatch {
                field: HpmField::SegmentSize,
                found: BAR_LEN / 2,
                expected: BAR_LEN,
            })
        );
    }

    /// A system-to-local TRANSFER: the ordinary VidMm placement transition.
    fn system_to_local_transfer() -> (HeliosPhysicalMemoryDmaV1, [HeliosPhysicalPageRunV1; 2]) {
        let mut h = HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 7, HpmOperation::Transfer);
        h.allocation_generation = 0x55;
        h.allocation_offset = 0;
        h.byte_length = 3 * 4096;
        h.source_segment_id = HELIOS_SEGMENT_ID_SYSTEM;
        h.destination_segment_id = HELIOS_SEGMENT_ID_HLM1;
        h.new_epoch = 9;
        h.run_count = 2;
        let runs = [
            HeliosPhysicalPageRunV1 {
                source_page: 0x1000,
                destination_page: 0x40,
                page_count: 2,
                reserved_zero: 0,
            },
            HeliosPhysicalPageRunV1 {
                source_page: 0x2000,
                destination_page: 0x42,
                page_count: 1,
                reserved_zero: 0,
            },
        ];
        (h, runs)
    }

    #[test]
    fn a_well_formed_transfer_validates() {
        let (h, runs) = system_to_local_transfer();
        assert_eq!(h.validate_header(&ctx()), Ok(HpmOperation::Transfer));
        assert_eq!(h.validate_runs(&runs, &ctx()), Ok(()));
    }

    #[test]
    fn run_coverage_is_exact_in_both_directions() {
        let (h, runs) = system_to_local_transfer();

        let mut under = h;
        under.byte_length = 4 * 4096;
        assert_eq!(
            under.validate_runs(&runs, &ctx()),
            Err(HpmReject::RunCoverageMismatch {
                byte_length: 4 * 4096,
                covered_pages: 3,
            })
        );

        let mut over = h;
        over.byte_length = 2 * 4096;
        assert_eq!(
            over.validate_runs(&runs, &ctx()),
            Err(HpmReject::RunCoverageMismatch {
                byte_length: 2 * 4096,
                covered_pages: 3,
            })
        );

        // A partial final page is legal: 2.5 pages needs exactly 3.
        let mut partial = h;
        partial.byte_length = 2 * 4096 + 1;
        assert_eq!(partial.validate_runs(&runs, &ctx()), Ok(()));
    }

    /// Page 0 of HLM1 is the first page of the BAR, and a `Required` run page
    /// must accept it. Pinned because `Rule::Required` means *nonzero* for the
    /// scalar fields, and tightening the run-page arm to match would silently
    /// make the first page of the segment unreachable.
    #[test]
    fn page_zero_of_a_device_segment_is_a_legal_run_page() {
        let (mut h, mut runs) = system_to_local_transfer();
        h.run_count = 1;
        h.byte_length = 4096;
        runs[0].destination_page = 0;
        runs[0].page_count = 1;
        assert_eq!(h.validate_header(&ctx()), Ok(HpmOperation::Transfer));
        assert_eq!(h.validate_runs(&runs[..1], &ctx()), Ok(()));
    }

    #[test]
    fn a_run_may_not_leave_its_segment() {
        let (h, mut runs) = system_to_local_transfer();
        runs[1].destination_page = ctx().hlm1_page_count;
        assert_eq!(
            h.validate_runs(&runs, &ctx()),
            Err(HpmReject::PageOutsideSegment {
                index: 1,
                field: HpmField::RunDestinationPage,
                segment_id: HELIOS_SEGMENT_ID_HLM1,
                page: ctx().hlm1_page_count,
            })
        );
    }

    #[test]
    fn reserved_and_zero_page_counts_are_refused() {
        let (h, mut runs) = system_to_local_transfer();
        runs[0].reserved_zero = 1;
        assert_eq!(
            h.validate_runs(&runs, &ctx()),
            Err(HpmReject::RunReservedNonZero { index: 0 })
        );

        let (h, mut runs) = system_to_local_transfer();
        runs[0].page_count = 0;
        assert_eq!(
            h.validate_runs(&runs, &ctx()),
            Err(HpmReject::ZeroPageCountInRun { index: 0 })
        );
    }

    /// The per-arm table, not a max-union: a field one arm requires is
    /// forbidden on another.
    #[test]
    fn per_arm_field_obligations_are_enforced_both_ways() {
        // TRANSFER is not an address-space operation.
        let (mut h, _) = system_to_local_transfer();
        h.address_space_id = 1;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::ForbiddenFieldSet {
                field: HpmField::AddressSpaceId
            })
        );

        // TRANSFER2 is, and needs both halves of the pair.
        let mut v = HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 7, HpmOperation::Transfer2);
        v.allocation_generation = 0x55;
        v.byte_length = 4096;
        v.source_gpu_virtual_address = 0x1_0000;
        v.destination_gpu_virtual_address = 0x2_0000;
        v.new_epoch = 3;
        v.address_space_id = 0x99;
        assert_eq!(
            v.validate_header(&ctx()),
            Err(HpmReject::MissingRequiredField {
                field: HpmField::RootGeneration
            })
        );
        v.root_generation = 4;
        assert_eq!(v.validate_header(&ctx()), Ok(HpmOperation::Transfer2));

        // …and it carries no runs: the device walks its own page tables.
        v.run_count = 1;
        assert_eq!(
            v.validate_header(&ctx()),
            Err(HpmReject::RunCountOutOfRange { found: 1 })
        );
    }

    #[test]
    fn fill_pattern_is_refused_outside_the_fill_arms() {
        let (mut h, _) = system_to_local_transfer();
        h.fill_pattern = 0xDEAD_BEEF;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::ForbiddenFieldSet {
                field: HpmField::FillPattern
            })
        );

        let mut f = HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 7, HpmOperation::Fill);
        f.allocation_generation = 1;
        f.byte_length = 4096;
        f.destination_segment_id = HELIOS_SEGMENT_ID_HLM1;
        f.source_segment_id = HELIOS_HPM1_SEGMENT_NONE;
        f.fill_pattern = 0xDEAD_BEEF;
        f.new_epoch = 2;
        f.run_count = 1;
        assert_eq!(f.validate_header(&ctx()), Ok(HpmOperation::Fill));
        // A FILL has no source page.
        let runs = [HeliosPhysicalPageRunV1 {
            source_page: 1,
            destination_page: 0,
            page_count: 1,
            reserved_zero: 0,
        }];
        assert_eq!(
            f.validate_runs(&runs, &ctx()),
            Err(HpmReject::ForbiddenFieldSet {
                field: HpmField::RunSourcePage
            })
        );
    }

    #[test]
    fn aperture_map_pins_both_segment_roles() {
        let mut m =
            HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 7, HpmOperation::MapApertureSegment);
        m.byte_length = 4096;
        m.new_epoch = 2;
        m.run_count = 1;
        m.source_segment_id = HELIOS_SEGMENT_ID_SYSTEM;
        m.destination_segment_id = HELIOS_SEGMENT_ID_HLM1;
        assert_eq!(
            m.validate_header(&ctx()),
            Err(HpmReject::SegmentNotAllowedForRole {
                field: HpmField::DestinationSegmentId,
                found: HELIOS_SEGMENT_ID_HLM1,
            })
        );
        m.destination_segment_id = HELIOS_SEGMENT_ID_APERTURE;
        assert_eq!(
            m.validate_header(&ctx()),
            Ok(HpmOperation::MapApertureSegment)
        );
        // hAllocation may legally be NULL for implicit/page-table storage.
        assert_eq!(m.allocation_generation, 0);
    }

    #[test]
    fn residency_audits_may_not_publish_an_epoch() {
        let mut n = HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 7, HpmOperation::NotifyResidency2);
        n.allocation_generation = 5;
        n.byte_length = 4096;
        n.destination_segment_id = HELIOS_SEGMENT_ID_HLM1;
        n.run_count = 1;
        n.prior_epoch = 11;
        n.new_epoch = 12;
        assert_eq!(
            n.validate_header(&ctx()),
            Err(HpmReject::ForbiddenFieldSet {
                field: HpmField::NewEpoch
            })
        );
        n.new_epoch = 0;
        assert_eq!(
            n.validate_header(&ctx()),
            Ok(HpmOperation::NotifyResidency2)
        );
        n.prior_epoch = 0;
        assert_eq!(
            n.validate_header(&ctx()),
            Err(HpmReject::MissingRequiredField {
                field: HpmField::PriorEpoch
            })
        );
    }

    #[test]
    fn epochs_must_advance() {
        let (mut h, _) = system_to_local_transfer();
        h.prior_epoch = 9;
        h.new_epoch = 9;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::EpochNotMonotonic { prior: 9, new: 9 })
        );
    }

    #[test]
    fn multipass_offset_is_the_sole_continuation_state() {
        let (mut h, _) = system_to_local_transfer();
        // FIRST set with a nonzero offset is a second, contradictory claim.
        h.multipass_offset = 64;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::MultipassOffsetInconsistent {
                multipass_offset: 64,
                first: true,
            })
        );
        // A continuation clears FIRST and keeps LAST until the final packet.
        h.flags = HELIOS_HPM1_FLAG_TRANSACTION_LAST;
        assert_eq!(h.validate_header(&ctx()), Ok(HpmOperation::Transfer));
        // A middle packet carries neither marker.
        h.flags = 0;
        assert_eq!(h.validate_header(&ctx()), Ok(HpmOperation::Transfer));
    }

    #[test]
    fn generation_and_prefix_mismatches_are_named_separately() {
        let (mut h, _) = system_to_local_transfer();
        h.magic = 0;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::BadMagic {
                found: 0,
                expected: HELIOS_HPM1_MAGIC,
            })
        );

        let (mut h, _) = system_to_local_transfer();
        h.abi_version = 2;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::BadAbiVersion {
                found: 2,
                expected: HELIOS_HPM1_ABI_VERSION,
            })
        );

        let (mut h, _) = system_to_local_transfer();
        h.package_generation ^= 1;
        assert!(matches!(
            h.validate_header(&ctx()),
            Err(HpmReject::PackageGenerationMismatch { .. })
        ));

        let (mut h, _) = system_to_local_transfer();
        h.adapter_generation ^= 1;
        assert!(matches!(
            h.validate_header(&ctx()),
            Err(HpmReject::AdapterGenerationMismatch { .. })
        ));

        let (mut h, _) = system_to_local_transfer();
        h.transaction_id = 0;
        assert_eq!(h.validate_header(&ctx()), Err(HpmReject::ZeroTransactionId));

        let (mut h, _) = system_to_local_transfer();
        h.status = HeliosHpm1Status::Complete as u32;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::StatusNotPending { found: 1 })
        );

        let (mut h, _) = system_to_local_transfer();
        h.reserved1 = 1;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::ReservedNonZero {
                field: HpmField::Reserved1
            })
        );
    }

    #[test]
    fn unknown_operations_and_flags_are_refused_not_approximated() {
        let (mut h, _) = system_to_local_transfer();
        h.operation = 0;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::UnknownOperation { found: 0 })
        );

        // One past the last defined ordinal (`DiscardContent2 = 17`).
        let (mut h, _) = system_to_local_transfer();
        h.operation = 18;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::UnknownOperation { found: 18 })
        );

        let (mut h, _) = system_to_local_transfer();
        h.flags |= 1 << 31;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::UnknownFlagBits { bits: 1 << 31 })
        );

        // A legal bit on the wrong arm: PTE attributes belong to page tables.
        let (mut h, _) = system_to_local_transfer();
        h.flags |= HELIOS_HPM1_FLAG_PTE_VALID;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::FlagNotAllowedForOperation {
                bits: HELIOS_HPM1_FLAG_PTE_VALID
            })
        );
    }

    #[test]
    fn a_zero_filled_buffer_never_decodes() {
        let bytes = [0u8; HELIOS_HPM1_DMA_HEADER_BYTES as usize];
        // `try_from_bytes` needs 8-byte alignment; a `u64` backing store gives
        // it without depending on the array's incidental alignment.
        let backing = [0u64; (HELIOS_HPM1_DMA_HEADER_BYTES as usize) / 8];
        let aligned: &[u8] = bytemuck::cast_slice(&backing);
        assert_eq!(bytes.len(), aligned.len());
        assert_eq!(
            parse_packet(aligned, &ctx()).err(),
            Some(HpmReject::BadMagic {
                found: 0,
                expected: HELIOS_HPM1_MAGIC,
            })
        );
    }

    #[test]
    fn parse_packet_walks_a_buffer_and_refuses_truncation() {
        let (h, runs) = system_to_local_transfer();
        // Build one packet in an 8-byte-aligned buffer.
        let mut backing = [0u64; (HELIOS_HPM1_MAX_PACKET_BYTES + 7) / 8];
        let buf: &mut [u8] = bytemuck::cast_slice_mut(&mut backing);
        let header_bytes = HELIOS_HPM1_DMA_HEADER_BYTES as usize;
        buf[..header_bytes].copy_from_slice(bytemuck::bytes_of(&h));
        buf[header_bytes..header_bytes + 2 * HELIOS_HPM1_RUN_BYTES]
            .copy_from_slice(bytemuck::cast_slice(&runs));

        let total = header_bytes + 2 * HELIOS_HPM1_RUN_BYTES;
        let packet = parse_packet(&buf[..total], &ctx()).unwrap();
        assert_eq!(packet.operation, HpmOperation::Transfer);
        assert_eq!(packet.runs.len(), 2);
        assert_eq!(packet.packet_bytes, total);

        assert_eq!(
            parse_packet(&buf[..total - 1], &ctx()).err(),
            Some(HpmReject::TruncatedPacket {
                needed: total,
                available: total - 1,
            })
        );
    }

    #[test]
    fn run_count_is_bounded_by_the_paging_buffer() {
        let (mut h, _) = system_to_local_transfer();
        h.run_count = HELIOS_HPM1_MAX_RUNS_PER_PACKET + 1;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::RunCountOutOfRange {
                found: HELIOS_HPM1_MAX_RUNS_PER_PACKET + 1,
            })
        );
    }

    #[test]
    fn a_system_to_system_transfer_has_nothing_for_hpm1_to_do() {
        let (mut h, _) = system_to_local_transfer();
        h.destination_segment_id = HELIOS_SEGMENT_ID_SYSTEM;
        assert_eq!(
            h.validate_header(&ctx()),
            Err(HpmReject::TransferNamesNoDeviceSegment)
        );
    }

    #[test]
    fn page_table_updates_state_one_range_twice_and_it_must_agree() {
        let mut u = HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 7, HpmOperation::UpdatePageTable);
        u.address_space_id = 3;
        u.root_generation = 4;
        u.destination_gpu_virtual_address = 0x10_0000;
        u.gpu_virtual_page_count = 2;
        u.byte_length = 2 * 4096;
        u.source_segment_id = HELIOS_SEGMENT_ID_HLM1;
        u.new_epoch = 5;
        u.run_count = 1;
        u.flags |= HELIOS_HPM1_FLAG_PTE_VALID;
        assert_eq!(u.validate_header(&ctx()), Ok(HpmOperation::UpdatePageTable));

        let runs = [HeliosPhysicalPageRunV1 {
            source_page: 0x10,
            destination_page: 0,
            page_count: 2,
            reserved_zero: 0,
        }];
        assert_eq!(u.validate_runs(&runs, &ctx()), Ok(()));

        u.gpu_virtual_page_count = 3;
        u.byte_length = 3 * 4096;
        assert_eq!(
            u.validate_runs(&runs, &ctx()),
            Err(HpmReject::RunCoverageMismatch {
                byte_length: 3 * 4096,
                covered_pages: 2,
            })
        );
    }

    /// On the page-table arm `destination_page` is a *virtual* page index, so
    /// the segment-keyed bound in `check_run_page` does not apply to it. Without
    /// its own bound and tiling rule it is the one page number in this ABI that
    /// nothing constrains.
    #[test]
    fn page_table_runs_must_tile_the_declared_pte_range() {
        let mut u = HeliosPhysicalMemoryDmaV1::new(PKG, ADAPTER, 9, HpmOperation::UpdatePageTable);
        u.address_space_id = 3;
        u.root_generation = 4;
        u.destination_gpu_virtual_address = 0x10_0000;
        u.source_segment_id = HELIOS_SEGMENT_ID_HLM1;
        u.new_epoch = 5;

        // A single run far outside the declared one-page range.
        u.gpu_virtual_page_count = 1;
        u.byte_length = 4096;
        u.run_count = 1;
        let far = [HeliosPhysicalPageRunV1 {
            source_page: 0x10,
            destination_page: 1 << 40,
            page_count: 1,
            reserved_zero: 0,
        }];
        assert_eq!(
            u.validate_runs(&far, &ctx()),
            Err(HpmReject::VirtualRunNotContiguous {
                index: 0,
                expected: 0,
                found: 1 << 40,
            })
        );

        // Four runs that sum to the declared count but all name virtual page 0:
        // three declared PTEs unwritten, one written four times.
        u.gpu_virtual_page_count = 4;
        u.byte_length = 4 * 4096;
        u.run_count = 4;
        let stacked = [HeliosPhysicalPageRunV1 {
            source_page: 0x10,
            destination_page: 0,
            page_count: 1,
            reserved_zero: 0,
        }; 4];
        assert_eq!(
            u.validate_runs(&stacked, &ctx()),
            Err(HpmReject::VirtualRunNotContiguous {
                index: 1,
                expected: 1,
                found: 0,
            })
        );

        // A run that starts in range and ends outside it.
        u.gpu_virtual_page_count = 2;
        u.byte_length = 2 * 4096;
        u.run_count = 1;
        let over = [HeliosPhysicalPageRunV1 {
            source_page: 0x10,
            destination_page: 0,
            page_count: 3,
            reserved_zero: 0,
        }];
        assert_eq!(
            u.validate_runs(&over, &ctx()),
            Err(HpmReject::VirtualPageOutsideRange {
                index: 0,
                page: 0,
                pages: 3,
                gpu_virtual_page_count: 2,
            })
        );

        // The tiling form is accepted.
        u.gpu_virtual_page_count = 3;
        u.byte_length = 3 * 4096;
        u.run_count = 2;
        let tiled = [
            HeliosPhysicalPageRunV1 {
                source_page: 0x10,
                destination_page: 0,
                page_count: 1,
                reserved_zero: 0,
            },
            HeliosPhysicalPageRunV1 {
                source_page: 0x20,
                destination_page: 1,
                page_count: 2,
                reserved_zero: 0,
            },
        ];
        assert_eq!(u.validate_runs(&tiled, &ctx()), Ok(()));
    }

    /// A caller-supplied page shift the negotiation could never have produced is
    /// refused rather than shifted with: `1u64 << 64` is a panic, and a panic in
    /// a DDI is a silent graphics deadlock.
    #[test]
    fn an_impossible_context_page_shift_is_refused_not_shifted_with() {
        let mut bad = ctx();
        bad.page_shift = 64;
        let (mut h, runs) = system_to_local_transfer();
        h.page_shift = 64;
        assert_eq!(
            h.validate_header(&bad),
            Err(HpmReject::ContextPageShiftUnsupported {
                found: 64,
                expected: HELIOS_HPM1_PAGE_SHIFT,
            })
        );
        assert_eq!(
            h.validate_runs(&runs, &bad),
            Err(HpmReject::ContextPageShiftUnsupported {
                found: 64,
                expected: HELIOS_HPM1_PAGE_SHIFT,
            })
        );
    }

    #[test]
    fn hpm1_wire_ordinals_are_pinned_for_the_c_mirror() {
        // qemu-helios/include/hw/virtio/helios_physical_memory.h carries these
        // by hand as HELIOS_HPM1_OP_*. Pin every literal so a Rust-side
        // renumbering without the matching header edit fails a test naming the
        // header, not a live paging transaction.
        assert_eq!(HpmOperation::Transfer.as_raw(), 1);
        assert_eq!(HpmOperation::Transfer2.as_raw(), 2);
        assert_eq!(HpmOperation::Fill.as_raw(), 3);
        assert_eq!(HpmOperation::Fill2.as_raw(), 4);
        assert_eq!(HpmOperation::DiscardContent.as_raw(), 5);
        assert_eq!(HpmOperation::MapApertureSegment.as_raw(), 6);
        assert_eq!(HpmOperation::MapApertureSegment2.as_raw(), 7);
        assert_eq!(HpmOperation::UnmapApertureSegment.as_raw(), 8);
        assert_eq!(HpmOperation::SetRootPageTable.as_raw(), 9);
        assert_eq!(HpmOperation::UpdatePageTable.as_raw(), 10);
        assert_eq!(HpmOperation::CopyPageTableEntries.as_raw(), 11);
        assert_eq!(HpmOperation::FlushTlb.as_raw(), 12);
        assert_eq!(HpmOperation::InitContextResource.as_raw(), 13);
        assert_eq!(HpmOperation::UpdateContextAllocation.as_raw(), 14);
        assert_eq!(HpmOperation::NotifyResidency.as_raw(), 15);
        assert_eq!(HpmOperation::NotifyResidency2.as_raw(), 16);
        assert_eq!(HpmOperation::DiscardContent2.as_raw(), 17);

        // Every ordinal round-trips, zero never decodes, and one past the last
        // is refused rather than approximated.
        for raw in 1u32..=17 {
            assert_eq!(HpmOperation::from_raw(raw).unwrap().as_raw(), raw);
        }
        assert_eq!(
            HpmOperation::from_raw(0),
            Err(HpmReject::UnknownOperation { found: 0 })
        );
        assert_eq!(
            HpmOperation::from_raw(18),
            Err(HpmReject::UnknownOperation { found: 18 })
        );
    }

    #[test]
    fn only_complete_permits_paging_completion() {
        assert!(HeliosHpm1Status::Complete.permits_paging_completion());
        for raw in [0u32, 2, 3, 4, 5, 6, 7, 8] {
            assert!(!HeliosHpm1Status::from_raw(raw)
                .unwrap()
                .permits_paging_completion());
        }
        assert_eq!(
            HeliosHpm1Status::from_raw(9),
            Err(HpmReject::UnknownStatus { found: 9 })
        );
    }

    /// Every operation must have a shape, and the fail-closed default must stay
    /// fail-closed: nothing is required, nothing is permitted.
    #[test]
    fn the_fail_closed_shape_permits_nothing() {
        let s = HpmOperationShape::FAIL_CLOSED;
        assert_eq!(s.runs, Rule::Forbidden);
        assert_eq!(s.source_segment, SegmentRole::Unused);
        assert_eq!(s.destination_segment, SegmentRole::Unused);
        assert_eq!(s.allowed_flags, HELIOS_HPM1_FLAG_COMMON_MASK);
    }
}
