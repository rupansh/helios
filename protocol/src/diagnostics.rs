//! Helios graphics ETW schema — the **one-way, lossy** kernel-provider ABI.
//!
//! HELIOS_PRESENT_SYNC_RETIREMENT.md §12.3 ("ETW and OS-diagnostic contract"),
//! with the emission contract in §17.6 and the module mandate in §17.1.
//!
//! # The boundary these bytes cross
//!
//! `kmd_render` registers ONE task-owned kernel provider at PASSIVE driver
//! initialization (`EtwRegister`, [`HELIOS_ETW_PROVIDER_GUID`]) and keeps the
//! returned `REGHANDLE` for driver lifetime. At each instrumented site it
//! constructs one [`HeliosGraphicsEtwPayloadV1`] and issues one `EtwWrite` with
//! exactly ONE data descriptor ([`HELIOS_ETW_DATA_DESCRIPTOR_COUNT`], far below
//! the documented [`HELIOS_ETW_MAX_DATA_DESCRIPTORS`] limit). Standard ETW
//! tooling on the other side (`tools/helios_etw_capture.ps1`, §17.7) enables the
//! fixed GUID and decodes only these versioned IDs and this payload. Nothing
//! travels the other way: **there is no reader, no acknowledgement, no query,
//! and no control verb in this file.**
//!
//! # What this module supersedes
//!
//! It replaces the private `D3DKMTEscape` observability ABI in [`crate::escape`]
//! — `HELIOS_ESCAPE_OP_QUERY_STATS` / `QUERY_SCANOUT_TIMELINE`,
//! `HeliosEscapeQueryStats*`, `HeliosEscapeQueryScanoutTimeline`,
//! `HeliosScanoutTimelineEvent`, and the mapped `HeliosReadLedgerPage` — which
//! §17.1 deletes outright with no observability fallback. That file still
//! exists only because `kmd_render`/`umd`/`umd12` have not migrated yet; its
//! deletion is a later cleanup phase. Nothing here is a compatibility carrier
//! for it: no verb, no opcode, no request/reply, no mapped page.
//!
//! # Loss is free (§12.3, §17.6)
//!
//! ETW can drop events. **No reader acknowledgement, event delivery, or counter
//! value participates in synchronization, recovery, lifetime, or package
//! admission.** An event that is gated off, dropped by the trace session, or
//! rejected by a decoder must change no graphics behavior whatsoever, and a
//! failed `EtwRegister` disables ETW without disturbing ordering. Therefore
//! every function here is pure: it inspects bytes and returns a verdict. None
//! of them can be on a correctness path by construction.
//!
//! # No identity travels in this schema (asserted)
//!
//! §12.3 and §17.1 both state it, so state it here too: this payload carries
//! **no handle, no pointer, no host/backing token, no raw virtio `resid`, no
//! PID, and no object name.** Field by field, the 72 bytes are:
//!
//! | field | why it is not an identity |
//! |---|---|
//! | `package_generation` | build-wide ABI generation; a version, not a lookup key |
//! | `adapter_generation` | "diagnostic generation, **never an object lookup key**" (§12.3) |
//! | `object_generation` | monotonic per-object generation counter; "never a handle/token" |
//! | `context_generation` | monotonic per-context/plane generation; "never a handle/token" |
//! | `sequence_value` | a fence/native/binding *value*, not an object reference |
//! | `status` | an `NTSTATUS` |
//! | `flags` | a bounded sub-kind + reserved zeros |
//! | `vidpn_source` | an OS VidPn source id (or `D3DDDI_ID_UNINITIALIZED`) |
//! | `node_or_plane` | a bounded small index (or `UINT32_MAX`) |
//! | `aux0`, `aux1` | reserved-zero in this revision; see the field docs |
//!
//! The compile-time block below pins the total to exactly the sum of those
//! scalars, so no field can be widened into a pointer/handle slot without
//! breaking the build.
//!
//! # Layout rules
//!
//! Every struct is `#[repr(C)]`, little-endian, pointer-free and padding-free
//! (bytemuck's `Pod` derive refuses implicit padding), and every size,
//! alignment and field offset in §12.3 is asserted at compile time. The C
//! mirror is `protocol/include/helios_diagnostics.h`; this file is its single
//! source of truth.
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

// ── Provider identity (§12.3) ───────────────────────────────────────────────

/// Windows `GUID` layout, declared here because this crate is `no_std` and has
/// no WDK bindgen edge. Byte-identical to `_GUID` (`Data1`/`Data2`/`Data3`/
/// `Data4[8]`, size 16, align 4), so the KMD may pass a pointer to
/// [`HELIOS_ETW_PROVIDER_GUID`] straight to `EtwRegister`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosEtwGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl HeliosEtwGuid {
    /// The 16-byte binary GUID encoding: `Data1`/`Data2`/`Data3` little-endian
    /// followed by `Data4` in order. This is what an ETW consumer compares
    /// against a captured provider id.
    pub const fn to_binary(&self) -> [u8; 16] {
        let d1 = self.data1.to_le_bytes();
        let d2 = self.data2.to_le_bytes();
        let d3 = self.data3.to_le_bytes();
        // Grouped one GUID field per line; the trailing markers keep the
        // grouping visible to `rustfmt`.
        [
            d1[0],
            d1[1],
            d1[2],
            d1[3], //
            d2[0],
            d2[1], //
            d3[0],
            d3[1], //
            self.data4[0],
            self.data4[1],
            self.data4[2],
            self.data4[3], //
            self.data4[4],
            self.data4[5],
            self.data4[6],
            self.data4[7],
        ]
    }
}

/// `{6D9A1A95-2B6A-4DEF-BCF7-847B6F158B0E}` — the task-owned kernel provider
/// GUID (§12.3). It is FIXED: the capture tool, the KMD's `EtwRegister`, and
/// any decoder all hard-code this value, so it must never be regenerated.
pub const HELIOS_ETW_PROVIDER_GUID: HeliosEtwGuid = HeliosEtwGuid {
    data1: 0x6D9A_1A95,
    data2: 0x2B6A,
    data3: 0x4DEF,
    data4: [0xBC, 0xF7, 0x84, 0x7B, 0x6F, 0x15, 0x8B, 0x0E],
};

/// [`HELIOS_ETW_PROVIDER_GUID`] in the 16-byte binary encoding, spelled out so
/// the C mirror and any byte-level comparison have a literal to check against.
pub const HELIOS_ETW_PROVIDER_GUID_BYTES: [u8; 16] = [
    0x95, 0x1A, 0x9A, 0x6D, 0x6A, 0x2B, 0xEF, 0x4D, 0xBC, 0xF7, 0x84, 0x7B, 0x6F, 0x15, 0x8B, 0x0E,
];

// ── Event descriptor constants (§12.3) ──────────────────────────────────────

/// "Every event has ETW descriptor version 1" (§12.3). A decoder that sees any
/// other version is looking at a different ABI and must not reinterpret it.
pub const HELIOS_ETW_EVENT_VERSION: u8 = 1;

/// `EVENT_DESCRIPTOR::Channel`. The provider is manifest-free — the capture
/// tool decodes by event ID and payload (§17.7) — so no channel is claimed.
pub const HELIOS_ETW_CHANNEL_NONE: u8 = 0;

/// `EVENT_DESCRIPTOR::Opcode` = `WINEVENT_OPCODE_INFO`. §12.3 defines no
/// opcodes: every event ID that names two or three alternatives
/// (`...CreateOrOpen`, `...LatchOrCancel`, …) discriminates them in the
/// [`HeliosGraphicsEtwPayloadV1::flags`] sub-kind, never in the opcode. Keeping
/// the opcode at Info means a decoder can never disagree with the payload.
pub const HELIOS_ETW_OPCODE_INFO: u8 = 0;

/// `EVENT_DESCRIPTOR::Task`. §12.3 defines no task taxonomy; the event ID is
/// the whole taxonomy, so the field stays zero and is asserted zero.
pub const HELIOS_ETW_TASK_NONE: u16 = 0;

// `EVENT_DESCRIPTOR::Level` — the standard `WINEVENT_LEVEL_*` ladder, declared
// here for the same reason as the GUID (no WDK bindgen edge in this crate).
pub const HELIOS_ETW_LEVEL_LOG_ALWAYS: u8 = 0;
pub const HELIOS_ETW_LEVEL_CRITICAL: u8 = 1;
pub const HELIOS_ETW_LEVEL_ERROR: u8 = 2;
pub const HELIOS_ETW_LEVEL_WARNING: u8 = 3;
pub const HELIOS_ETW_LEVEL_INFORMATIONAL: u8 = 4;
pub const HELIOS_ETW_LEVEL_VERBOSE: u8 = 5;

/// The level of every Helios event.
///
/// §12.3 does not assign per-event levels; it says only that
/// `DxgkDdiControlEtwLogging` sets "a maximum logging level" and that emission
/// requires `EtwProviderEnabled(REGHANDLE, level, keyword)` to accept. One
/// uniform INFORMATIONAL level is the conservative reading: it is filterable by
/// that maximum (which `LOG_ALWAYS` would silently defeat), and it assigns no
/// severity meaning the doc never states. Severity classification, if it is
/// ever wanted, belongs in a later ABI revision that bumps
/// [`HELIOS_ETW_EVENT_VERSION`] — not in an emitter picking its own level.
pub const HELIOS_ETW_EVENT_LEVEL: u8 = HELIOS_ETW_LEVEL_INFORMATIONAL;

// ── Keywords (§12.3) ────────────────────────────────────────────────────────
//
// "Keywords are bit 0 submission, bit 1 native synchronization, bit 2 display
// lifetime, bit 3 device lifecycle, and bit 4 translation-session lifetime."
//
// ⚠ These belong to `EtwProviderEnabled`/the trace session ONLY. §12.3:
// `DxgkDdiControlEtwLogging`'s `Flags` "must be zero and is not treated as a
// keyword mask" — see [`validate_control_etw_logging_flags`].

/// Bit 0 — batch submit/complete/cancel-or-fault (events 1-3).
pub const HELIOS_ETW_KEYWORD_SUBMISSION: u64 = 1 << 0;
/// Bit 1 — native fence create/open, wait/signal, close/reset (events 4-6).
pub const HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION: u64 = 1 << 1;
/// Bit 2 — plane candidate/latch/cancel/reader-release/unbind (events 7-9),
/// i.e. the §10.8 display consumption and host-reader retirement transitions.
pub const HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME: u64 = 1 << 2;
/// Bit 3 — device reset/removal (event 10).
pub const HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE: u64 = 1 << 3;
/// Bit 4 — HTS1 translation-session create/attach/drain and translated
/// synchronous progress (events 11-12).
pub const HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME: u64 = 1 << 4;

/// Every keyword this ABI defines. Bits above 4 are unassigned; a capture that
/// enables them gets nothing, and the provider never emits with them set.
pub const HELIOS_ETW_KEYWORD_ALL: u64 = HELIOS_ETW_KEYWORD_SUBMISSION
    | HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION
    | HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME
    | HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE
    | HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME;

// ── EtwWrite shape (§12.3) ──────────────────────────────────────────────────

/// "each `EtwWrite` uses one data descriptor" — the whole event is one
/// [`HeliosGraphicsEtwPayloadV1`].
pub const HELIOS_ETW_DATA_DESCRIPTOR_COUNT: u32 = 1;
/// The documented `EtwWrite` data-descriptor limit §12.3 measures against.
/// Declared so the emitter's bound has a name rather than a literal.
pub const HELIOS_ETW_MAX_DATA_DESCRIPTORS: u32 = 128;

// ── Event IDs (§12.3) ───────────────────────────────────────────────────────

/// Number of event IDs this ABI revision defines.
pub const HELIOS_ETW_EVENT_COUNT: usize = 12;
/// Lowest legal event ID. ID 0 is not an event.
pub const HELIOS_ETW_EVENT_ID_MIN: u16 = 1;
/// Highest legal event ID.
pub const HELIOS_ETW_EVENT_ID_MAX: u16 = 12;

/// The complete §12.3 event-ID set.
///
/// Names are verbatim from the doc, including the `...OrX` pairs: one ID covers
/// both/all of the named transitions and the [`HeliosGraphicsEtwPayloadV1`]
/// flags sub-kind says which one it was.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosEtwEventId {
    /// 1 — a batch was submitted (§10.1 invariant 2/3: the batch that actually
    /// performs the writes).
    BatchSubmit = 1,
    /// 2 — a batch reached terminal host GPU completion.
    BatchComplete = 2,
    /// 3 — a batch was cancelled, or faulted. Sub-kind distinguishes them.
    BatchCancelOrFault = 3,
    /// 4 — a Core-0116 native fence object was created, or opened.
    NativeFenceCreateOrOpen = 4,
    /// 5 — a native-fence wait, or signal, was issued.
    NativeFenceWaitOrSignal = 5,
    /// 6 — a native fence was closed, or reset.
    NativeFenceCloseOrReset = 6,
    /// 7 — a §10.8 plane candidate was validated and retained (pre-latch).
    PlaneCandidate = 7,
    /// 8 — a candidate was latched as the current plane binding, or cancelled
    /// before latch (§10.8: "a candidate canceled before latch releases only
    /// its candidate reference and never displaces the current plane").
    PlaneLatchOrCancel = 8,
    /// 9 — the backend acknowledged that no reader uses a former binding
    /// (release), or the plane/source was explicitly unbound (§10.8).
    PlaneReaderReleaseOrUnbind = 9,
    /// 10 — device reset, or removal.
    DeviceResetOrRemoval = 10,
    /// 11 — an HTS1 translation session was created, attached to, or drained.
    /// §12.3: events 11-12 "serialize generations, endpoint, sequence, and
    /// result state only; they never serialize HQA1's capability or a
    /// host/KMT/resource handle."
    TranslationSessionCreateAttachOrDrain = 11,
    /// 12 — progress of a translated synchronous operation (§10.1 invariant
    /// 15). Same no-capability/no-handle rule as ID 11.
    TranslationSynchronousProgress = 12,
}

impl HeliosEtwEventId {
    /// Every event ID, in ID order. `ALL[i].id() == i as u16 + 1`.
    pub const ALL: [HeliosEtwEventId; HELIOS_ETW_EVENT_COUNT] = [
        Self::BatchSubmit,
        Self::BatchComplete,
        Self::BatchCancelOrFault,
        Self::NativeFenceCreateOrOpen,
        Self::NativeFenceWaitOrSignal,
        Self::NativeFenceCloseOrReset,
        Self::PlaneCandidate,
        Self::PlaneLatchOrCancel,
        Self::PlaneReaderReleaseOrUnbind,
        Self::DeviceResetOrRemoval,
        Self::TranslationSessionCreateAttachOrDrain,
        Self::TranslationSynchronousProgress,
    ];

    /// The numeric `EVENT_DESCRIPTOR::Id`.
    #[inline]
    pub const fn id(self) -> u16 {
        self as u16
    }

    /// Total: an unknown ID yields `None` rather than a guess. A decoder that
    /// receives an out-of-range ID from a future package generation must drop
    /// the event, not reinterpret its payload.
    #[inline]
    pub const fn from_id(id: u16) -> Option<Self> {
        match id {
            1 => Some(Self::BatchSubmit),
            2 => Some(Self::BatchComplete),
            3 => Some(Self::BatchCancelOrFault),
            4 => Some(Self::NativeFenceCreateOrOpen),
            5 => Some(Self::NativeFenceWaitOrSignal),
            6 => Some(Self::NativeFenceCloseOrReset),
            7 => Some(Self::PlaneCandidate),
            8 => Some(Self::PlaneLatchOrCancel),
            9 => Some(Self::PlaneReaderReleaseOrUnbind),
            10 => Some(Self::DeviceResetOrRemoval),
            11 => Some(Self::TranslationSessionCreateAttachOrDrain),
            12 => Some(Self::TranslationSynchronousProgress),
            _ => None,
        }
    }

    /// The event's keyword, from the §12.3 five-class assignment. The KMD
    /// passes exactly this to `EtwProviderEnabled` and puts it in the
    /// descriptor; the two must agree or the gate would test a different class
    /// from the one it emits.
    #[inline]
    pub const fn keyword(self) -> u64 {
        match self {
            Self::BatchSubmit | Self::BatchComplete | Self::BatchCancelOrFault => {
                HELIOS_ETW_KEYWORD_SUBMISSION
            }
            Self::NativeFenceCreateOrOpen
            | Self::NativeFenceWaitOrSignal
            | Self::NativeFenceCloseOrReset => HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION,
            Self::PlaneCandidate | Self::PlaneLatchOrCancel | Self::PlaneReaderReleaseOrUnbind => {
                HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME
            }
            Self::DeviceResetOrRemoval => HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE,
            Self::TranslationSessionCreateAttachOrDrain | Self::TranslationSynchronousProgress => {
                HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME
            }
        }
    }

    /// Uniform [`HELIOS_ETW_EVENT_LEVEL`]; see that constant for why §12.3's
    /// silence on per-event levels is resolved this way.
    #[inline]
    pub const fn level(self) -> u8 {
        HELIOS_ETW_EVENT_LEVEL
    }

    /// How many sub-kinds this event's own name enumerates: 1 for a single
    /// transition, 2 for an `XOrY` pair, 3 for
    /// [`Self::TranslationSessionCreateAttachOrDrain`]. Legal sub-kind values
    /// are `0..count`; everything else is rejected.
    #[inline]
    pub const fn subkind_count(self) -> u32 {
        match self {
            Self::BatchSubmit
            | Self::BatchComplete
            | Self::PlaneCandidate
            | Self::TranslationSynchronousProgress => 1,
            Self::BatchCancelOrFault
            | Self::NativeFenceCreateOrOpen
            | Self::NativeFenceWaitOrSignal
            | Self::NativeFenceCloseOrReset
            | Self::PlaneLatchOrCancel
            | Self::PlaneReaderReleaseOrUnbind
            | Self::DeviceResetOrRemoval => 2,
            Self::TranslationSessionCreateAttachOrDrain => 3,
        }
    }

    /// The complete, fixed `EVENT_DESCRIPTOR` for this event.
    #[inline]
    pub const fn descriptor(self) -> HeliosEtwEventDescriptor {
        HeliosEtwEventDescriptor {
            id: self.id(),
            version: HELIOS_ETW_EVENT_VERSION,
            channel: HELIOS_ETW_CHANNEL_NONE,
            level: self.level(),
            opcode: HELIOS_ETW_OPCODE_INFO,
            task: HELIOS_ETW_TASK_NONE,
            keyword: self.keyword(),
        }
    }
}

// ── Event descriptor (mirrors `EVENT_DESCRIPTOR`, wdm.h) ────────────────────

/// Windows `EVENT_DESCRIPTOR`, declared here so the KMD can hold the fixed
/// descriptor table in `.rdata` without a WDK type and so a decoder can compare
/// captured descriptor bytes field by field.
///
/// Layout is the OS ABI: `Id`(0,2) `Version`(2,1) `Channel`(3,1) `Level`(4,1)
/// `Opcode`(5,1) `Task`(6,2) `Keyword`(8,8) — 16 bytes, align 8, no padding.
/// The KMD may cast `&HeliosEtwEventDescriptor` to `*const EVENT_DESCRIPTOR`
/// for `EtwWrite`; that cast is where the `// SAFETY:` comment belongs.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosEtwEventDescriptor {
    /// `HeliosEtwEventId::id()`.
    pub id: u16,
    /// Always [`HELIOS_ETW_EVENT_VERSION`].
    pub version: u8,
    /// Always [`HELIOS_ETW_CHANNEL_NONE`].
    pub channel: u8,
    /// Always [`HELIOS_ETW_EVENT_LEVEL`].
    pub level: u8,
    /// Always [`HELIOS_ETW_OPCODE_INFO`].
    pub opcode: u8,
    /// Always [`HELIOS_ETW_TASK_NONE`].
    pub task: u16,
    /// The event's single class keyword.
    pub keyword: u64,
}

/// The fixed descriptor table, indexed by `id - 1`. Bounded, immutable, and
/// nonpaged when the KMD holds it in `.rdata` — §12.3 requires descriptors used
/// above `APC_LEVEL` to be bounded nonpaged system-space objects.
pub const HELIOS_ETW_EVENT_DESCRIPTORS: [HeliosEtwEventDescriptor; HELIOS_ETW_EVENT_COUNT] = [
    HeliosEtwEventId::BatchSubmit.descriptor(),
    HeliosEtwEventId::BatchComplete.descriptor(),
    HeliosEtwEventId::BatchCancelOrFault.descriptor(),
    HeliosEtwEventId::NativeFenceCreateOrOpen.descriptor(),
    HeliosEtwEventId::NativeFenceWaitOrSignal.descriptor(),
    HeliosEtwEventId::NativeFenceCloseOrReset.descriptor(),
    HeliosEtwEventId::PlaneCandidate.descriptor(),
    HeliosEtwEventId::PlaneLatchOrCancel.descriptor(),
    HeliosEtwEventId::PlaneReaderReleaseOrUnbind.descriptor(),
    HeliosEtwEventId::DeviceResetOrRemoval.descriptor(),
    HeliosEtwEventId::TranslationSessionCreateAttachOrDrain.descriptor(),
    HeliosEtwEventId::TranslationSynchronousProgress.descriptor(),
];

// ── Payload flags (§12.3 row 44: "event-ID-specific, with reserved bits zero")
//
// §12.3 delegates the flag meaning to "the ABI file" (this file) without
// enumerating bits, and requires that "unknown flags are zero on emission".
// This revision therefore defines EXACTLY ONE flag field and nothing else: a
// two-bit sub-kind that names which alternative of the event's own name
// occurred. Every other bit is reserved and must be zero, in both directions —
// the emitter writes zero, the validator rejects nonzero. Adding a real flag
// means editing this file and bumping the ABI, never an emitter inventing a
// bit at its call site.

/// Shift of the sub-kind field within [`HeliosGraphicsEtwPayloadV1::flags`].
pub const HELIOS_ETW_FLAG_SUBKIND_SHIFT: u32 = 0;
/// Mask of the sub-kind field. Two bits: the widest event
/// ([`HeliosEtwEventId::TranslationSessionCreateAttachOrDrain`]) has three
/// alternatives.
pub const HELIOS_ETW_FLAG_SUBKIND_MASK: u32 = 0x0000_0003;
/// Every bit outside the sub-kind field. Reserved, zero on emission, and a
/// hard reject when set (`FlagsReservedBitsSet`).
pub const HELIOS_ETW_FLAGS_RESERVED_MASK: u32 = !HELIOS_ETW_FLAG_SUBKIND_MASK;

/// Sub-kind of an event whose name enumerates exactly one transition
/// (`BatchSubmit`, `BatchComplete`, `PlaneCandidate`,
/// `TranslationSynchronousProgress`). Its only legal value is 0.
pub const HELIOS_ETW_SUBKIND_SOLE: u32 = 0;

/// `BatchCancelOrFault`: the batch was cancelled.
pub const HELIOS_ETW_SUBKIND_BATCH_CANCEL: u32 = 0;
/// `BatchCancelOrFault`: the batch faulted.
pub const HELIOS_ETW_SUBKIND_BATCH_FAULT: u32 = 1;

/// `NativeFenceCreateOrOpen`: the object was created.
pub const HELIOS_ETW_SUBKIND_NATIVE_FENCE_CREATE: u32 = 0;
/// `NativeFenceCreateOrOpen`: the object was opened.
pub const HELIOS_ETW_SUBKIND_NATIVE_FENCE_OPEN: u32 = 1;

/// `NativeFenceWaitOrSignal`: a wait was issued.
pub const HELIOS_ETW_SUBKIND_NATIVE_FENCE_WAIT: u32 = 0;
/// `NativeFenceWaitOrSignal`: a signal was issued.
pub const HELIOS_ETW_SUBKIND_NATIVE_FENCE_SIGNAL: u32 = 1;

/// `NativeFenceCloseOrReset`: the object was closed.
pub const HELIOS_ETW_SUBKIND_NATIVE_FENCE_CLOSE: u32 = 0;
/// `NativeFenceCloseOrReset`: the object was reset.
pub const HELIOS_ETW_SUBKIND_NATIVE_FENCE_RESET: u32 = 1;

/// `PlaneLatchOrCancel`: the candidate became the current binding.
pub const HELIOS_ETW_SUBKIND_PLANE_LATCH: u32 = 0;
/// `PlaneLatchOrCancel`: the candidate was cancelled before latch.
pub const HELIOS_ETW_SUBKIND_PLANE_CANCEL: u32 = 1;

/// `PlaneReaderReleaseOrUnbind`: the backend released the old reader.
pub const HELIOS_ETW_SUBKIND_PLANE_READER_RELEASE: u32 = 0;
/// `PlaneReaderReleaseOrUnbind`: the plane/source was explicitly unbound.
pub const HELIOS_ETW_SUBKIND_PLANE_UNBIND: u32 = 1;

/// `DeviceResetOrRemoval`: reset.
pub const HELIOS_ETW_SUBKIND_DEVICE_RESET: u32 = 0;
/// `DeviceResetOrRemoval`: removal.
pub const HELIOS_ETW_SUBKIND_DEVICE_REMOVAL: u32 = 1;

/// `TranslationSessionCreateAttachOrDrain`: session created.
pub const HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_CREATE: u32 = 0;
/// `TranslationSessionCreateAttachOrDrain`: outer context attached.
pub const HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_ATTACH: u32 = 1;
/// `TranslationSessionCreateAttachOrDrain`: session drained.
pub const HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_DRAIN: u32 = 2;

// ── Payload sentinels (§12.3 rows 48 and 52) ────────────────────────────────

/// `D3DDDI_ID_UNINITIALIZED` (`(UINT)(~0)`, WDK `d3dukmdt.h:1111`) — the value
/// [`HeliosGraphicsEtwPayloadV1::vidpn_source`] carries when the event has no
/// VidPn source. §10.8/C44 give the same sentinel its "any source on this exact
/// adapter" meaning for a `D3D12_RUNTIME_PRIMARY`; it is never a concrete
/// identity to compare against.
pub const HELIOS_ETW_VIDPN_SOURCE_UNINITIALIZED: u32 = 0xFFFF_FFFF;

/// `UINT32_MAX` — the value [`HeliosGraphicsEtwPayloadV1::node_or_plane`]
/// carries when the event has no node or plane index.
pub const HELIOS_ETW_NODE_OR_PLANE_NONE: u32 = 0xFFFF_FFFF;

/// The largest legal node/plane index in this generation.
///
/// §12.3 says "exact bounded node/plane index" without stating the bound; the
/// bound is the selected profile's, and it is 0 on both axes — §10.8 fixes
/// `MaxPlanes=1`/`LayerIndex=0` for display and §10.8/§17.6 fix the one-node,
/// one-engine adapter for submission. A larger index cannot describe this
/// package, so a validator rejects it rather than printing it.
pub const HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX: u32 = 0;

// ── The 72-byte payload (§12.3) ─────────────────────────────────────────────

/// Byte size of [`HeliosGraphicsEtwPayloadV1`]; the single `EtwWrite` data
/// descriptor is exactly this long.
pub const HELIOS_ETW_PAYLOAD_V1_SIZE: usize = 72;

/// The one data descriptor every Helios ETW event carries (§12.3 table).
///
/// | Offset | Size | Field | Rule |
/// |---:|---:|---|---|
/// | 0 | 8 | package generation | exact atomic-package generation |
/// | 8 | 8 | adapter generation | diagnostic generation, never an object lookup key |
/// | 16 | 8 | object/allocation generation | zero when not applicable; never a handle/token |
/// | 24 | 8 | context/plane generation | zero when not applicable; never a handle/token |
/// | 32 | 8 | sequence/value | submission fence, native value, or binding sequence according to event ID |
/// | 40 | 4 | NTSTATUS/result | exact result, zero for an event without one |
/// | 44 | 4 | flags | event-ID-specific, with reserved bits zero |
/// | 48 | 4 | VidPn source | exact OS value or `D3DDDI_ID_UNINITIALIZED` |
/// | 52 | 4 | node/plane | exact bounded node/plane index or `UINT32_MAX` |
/// | 56 | 8 | auxiliary value 0 | documented per event ID |
/// | 64 | 8 | auxiliary value 1 | documented per event ID |
///
/// 72 bytes, align 8, pointer-free. Construct it with [`Self::new`], which
/// makes the reserved-zero rules unrepresentable rather than merely documented.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosGraphicsEtwPayloadV1 {
    /// Offset 0 — the exact atomic package generation this driver was built
    /// with (the one §17.1 shares across protocol/Mesa/UMD11/UMD12/KMD/QEMU/
    /// installer). A decoder that sees a generation it does not know must drop
    /// the event; generation mismatch is fatal everywhere else in the package.
    pub package_generation: u64,
    /// Offset 8 — the adapter's diagnostic generation. §12.3: "never an object
    /// lookup key". It correlates events across one adapter lifetime and
    /// nothing else.
    pub adapter_generation: u64,
    /// Offset 16 — the object/allocation generation the event is about, or 0
    /// when the event has no object. §12.3: "never a handle/token".
    pub object_generation: u64,
    /// Offset 24 — the context/plane generation the event is about, or 0 when
    /// the event has no context or plane. §12.3: "never a handle/token".
    pub context_generation: u64,
    /// Offset 32 — a submission fence value, a native-fence monitored value, or
    /// a plane-binding sequence, selected by the event ID. A *value*, never a
    /// reference: nothing can be looked up with it.
    pub sequence_value: u64,
    /// Offset 40 — the exact `NTSTATUS` result, or 0 for an event that has no
    /// result. Signed, matching `NTSTATUS`/`LONG`, so a failure code survives
    /// the round trip byte-exactly.
    pub status: i32,
    /// Offset 44 — [`HELIOS_ETW_FLAG_SUBKIND_MASK`] holds the event's sub-kind;
    /// every bit in [`HELIOS_ETW_FLAGS_RESERVED_MASK`] is reserved and zero.
    pub flags: u32,
    /// Offset 48 — the exact OS VidPn source id, or
    /// [`HELIOS_ETW_VIDPN_SOURCE_UNINITIALIZED`].
    pub vidpn_source: u32,
    /// Offset 52 — the exact bounded node or plane index (see
    /// [`HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX`]), or
    /// [`HELIOS_ETW_NODE_OR_PLANE_NONE`].
    pub node_or_plane: u32,
    /// Offset 56 — auxiliary value 0. §12.3 says it is "documented per event
    /// ID" and delegates that documentation to this file; **this ABI revision
    /// documents no interpretation for any event ID**, and §12.3's own rule
    /// that "unknown flags are zero on emission" makes reserved-zero the
    /// fail-closed reading. [`Self::new`] cannot set it and
    /// [`validate_payload_v1`] rejects a nonzero value
    /// (`AuxiliaryValue0NotZero`). Giving it a meaning is an edit to this file
    /// plus a [`HELIOS_ETW_EVENT_VERSION`] bump — never an emitter decision.
    pub aux0: u64,
    /// Offset 64 — auxiliary value 1. Same reserved-zero rule as [`Self::aux0`].
    pub aux1: u64,
}

impl HeliosGraphicsEtwPayloadV1 {
    /// Build a payload. `aux0`/`aux1` are forced to zero because this revision
    /// defines no interpretation for them; there is deliberately no constructor
    /// that can set them.
    ///
    /// This is a plain value constructor: it validates nothing, allocates
    /// nothing, and cannot fail. Emitters run [`validate_payload_v1`] in
    /// checked builds/tests — never on the emission path's critical section,
    /// where §12.3 forbids all extra work.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub const fn new(
        package_generation: u64,
        adapter_generation: u64,
        object_generation: u64,
        context_generation: u64,
        sequence_value: u64,
        status: i32,
        flags: u32,
        vidpn_source: u32,
        node_or_plane: u32,
    ) -> Self {
        Self {
            package_generation,
            adapter_generation,
            object_generation,
            context_generation,
            sequence_value,
            status,
            flags,
            vidpn_source,
            node_or_plane,
            aux0: 0,
            aux1: 0,
        }
    }

    /// The sub-kind encoded in [`Self::flags`].
    #[inline]
    pub const fn subkind(&self) -> u32 {
        (self.flags & HELIOS_ETW_FLAG_SUBKIND_MASK) >> HELIOS_ETW_FLAG_SUBKIND_SHIFT
    }
}

// ── Compile-time layout proof (§12.3 table) ─────────────────────────────────
//
// A wrong offset breaks the build here, not a test run. The C mirror in
// protocol/include/helios_diagnostics.h repeats every one of these with
// `_Static_assert`.

const _: () = {
    // Provider GUID: the OS `GUID` shape, and the binary encoding must agree
    // with the structured constant.
    assert!(core::mem::size_of::<HeliosEtwGuid>() == 16);
    assert!(core::mem::align_of::<HeliosEtwGuid>() == 4);
    assert!(core::mem::offset_of!(HeliosEtwGuid, data1) == 0);
    assert!(core::mem::offset_of!(HeliosEtwGuid, data2) == 4);
    assert!(core::mem::offset_of!(HeliosEtwGuid, data3) == 6);
    assert!(core::mem::offset_of!(HeliosEtwGuid, data4) == 8);
    let guid = HELIOS_ETW_PROVIDER_GUID.to_binary();
    let mut i = 0;
    while i < 16 {
        assert!(guid[i] == HELIOS_ETW_PROVIDER_GUID_BYTES[i]);
        i += 1;
    }

    // `EVENT_DESCRIPTOR` (wdm.h) shape.
    assert!(core::mem::size_of::<HeliosEtwEventDescriptor>() == 16);
    assert!(core::mem::align_of::<HeliosEtwEventDescriptor>() == 8);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, id) == 0);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, version) == 2);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, channel) == 3);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, level) == 4);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, opcode) == 5);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, task) == 6);
    assert!(core::mem::offset_of!(HeliosEtwEventDescriptor, keyword) == 8);

    // The 72-byte payload, offset by offset, straight off the §12.3 table.
    assert!(core::mem::size_of::<HeliosGraphicsEtwPayloadV1>() == HELIOS_ETW_PAYLOAD_V1_SIZE);
    assert!(core::mem::size_of::<HeliosGraphicsEtwPayloadV1>() == 72);
    assert!(core::mem::align_of::<HeliosGraphicsEtwPayloadV1>() == 8);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, package_generation) == 0);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, adapter_generation) == 8);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, object_generation) == 16);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, context_generation) == 24);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, sequence_value) == 32);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, status) == 40);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, flags) == 44);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, vidpn_source) == 48);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, node_or_plane) == 52);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, aux0) == 56);
    assert!(core::mem::offset_of!(HeliosGraphicsEtwPayloadV1, aux1) == 64);
    // The 72 bytes are EXACTLY five u64 generations/values, four 32-bit
    // scalars, and two reserved u64 — there is no room for a handle, pointer,
    // backing token, or raw `resid`, and widening any field to make room
    // breaks this assert.
    assert!(
        core::mem::size_of::<HeliosGraphicsEtwPayloadV1>()
            == 5 * core::mem::size_of::<u64>()
                + 4 * core::mem::size_of::<u32>()
                + 2 * core::mem::size_of::<u64>()
    );

    // The descriptor table is complete, in ID order, and internally consistent
    // with the per-event accessors.
    assert!(HELIOS_ETW_EVENT_DESCRIPTORS.len() == HELIOS_ETW_EVENT_COUNT);
    assert!(HeliosEtwEventId::ALL.len() == HELIOS_ETW_EVENT_COUNT);
    assert!(HELIOS_ETW_EVENT_ID_MAX as usize == HELIOS_ETW_EVENT_COUNT);
    let mut i = 0;
    while i < HELIOS_ETW_EVENT_COUNT {
        let d = HELIOS_ETW_EVENT_DESCRIPTORS[i];
        assert!(d.id as usize == i + 1);
        assert!(d.id >= HELIOS_ETW_EVENT_ID_MIN && d.id <= HELIOS_ETW_EVENT_ID_MAX);
        assert!(d.version == HELIOS_ETW_EVENT_VERSION);
        assert!(d.channel == HELIOS_ETW_CHANNEL_NONE);
        assert!(d.level == HELIOS_ETW_EVENT_LEVEL);
        assert!(d.opcode == HELIOS_ETW_OPCODE_INFO);
        assert!(d.task == HELIOS_ETW_TASK_NONE);
        // Exactly one keyword bit, and it is one of the five defined classes.
        assert!(d.keyword & HELIOS_ETW_KEYWORD_ALL == d.keyword);
        assert!(d.keyword.count_ones() == 1);
        i += 1;
    }

    // Sub-kind field: two bits, and it holds the widest event's alternatives.
    assert!(HELIOS_ETW_FLAG_SUBKIND_MASK.count_ones() == 2);
    assert!(HELIOS_ETW_FLAG_SUBKIND_SHIFT == 0);
    assert!(HELIOS_ETW_FLAGS_RESERVED_MASK == !HELIOS_ETW_FLAG_SUBKIND_MASK);
    assert!(HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_DRAIN <= HELIOS_ETW_FLAG_SUBKIND_MASK);
    assert!(HELIOS_ETW_DATA_DESCRIPTOR_COUNT < HELIOS_ETW_MAX_DATA_DESCRIPTORS);
};

// ── Rejection reasons ───────────────────────────────────────────────────────

/// Why a descriptor/payload was refused. Every refusal is named so the KMD can
/// count it and the capture tool can print it; there is no anonymous `false`
/// anywhere in this module.
///
/// ⚠ A rejection here is a *decoding/self-check* verdict about diagnostic
/// bytes. §12.3 forbids it from having any other effect: it must never gate a
/// present, a fence, a lifetime, or package admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosEtwReject {
    /// `EVENT_DESCRIPTOR::Id` is not one of the twelve §12.3 IDs.
    UnknownEventId,
    /// `EVENT_DESCRIPTOR::Version` is not [`HELIOS_ETW_EVENT_VERSION`].
    DescriptorVersionMismatch,
    /// `EVENT_DESCRIPTOR::Channel` is not [`HELIOS_ETW_CHANNEL_NONE`].
    DescriptorChannelMismatch,
    /// `EVENT_DESCRIPTOR::Level` is not the event's fixed level.
    DescriptorLevelMismatch,
    /// `EVENT_DESCRIPTOR::Opcode` is not [`HELIOS_ETW_OPCODE_INFO`].
    DescriptorOpcodeMismatch,
    /// `EVENT_DESCRIPTOR::Task` is not [`HELIOS_ETW_TASK_NONE`].
    DescriptorTaskMismatch,
    /// `EVENT_DESCRIPTOR::Keyword` is not the event's class keyword.
    DescriptorKeywordMismatch,
    /// The data descriptor is not exactly [`HELIOS_ETW_PAYLOAD_V1_SIZE`] bytes.
    PayloadLengthMismatch,
    /// The payload bytes could not be read as the POD payload at all.
    PayloadNotReadable,
    /// `package_generation` is zero. §12.3 gives this row no "zero when not
    /// applicable" escape, unlike the object/context rows; a zeroed generation
    /// is an uninitialized payload, so it fails closed.
    PackageGenerationZero,
    /// `package_generation` is not the generation the caller expects. Package
    /// generation mismatch is fatal package-wide (§17.1).
    PackageGenerationMismatch,
    /// The caller passed 0 as the expected package generation. There is no
    /// "skip the check" value: a comparison that cannot be made is a refusal,
    /// never a silent pass.
    ExpectedPackageGenerationZero,
    /// `adapter_generation` is zero. Same reasoning as
    /// [`Self::PackageGenerationZero`]: every §12.3 event belongs to some
    /// adapter lifetime.
    AdapterGenerationZero,
    /// A bit in [`HELIOS_ETW_FLAGS_RESERVED_MASK`] is set. §12.3: "unknown
    /// flags are zero on emission".
    FlagsReservedBitsSet,
    /// The flags sub-kind is `>= event.subkind_count()` — a transition this
    /// event ID does not name.
    FlagsSubkindOutOfRange,
    /// `node_or_plane` is neither [`HELIOS_ETW_NODE_OR_PLANE_NONE`] nor within
    /// [`HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX`].
    NodeOrPlaneOutOfRange,
    /// `aux0` is nonzero while this revision defines no interpretation for it.
    AuxiliaryValue0NotZero,
    /// `aux1` is nonzero while this revision defines no interpretation for it.
    AuxiliaryValue1NotZero,
    /// `DXGKARG_CONTROLETWLOGGING::Flags` is nonzero. §12.3: "its currently
    /// undefined `Flags` must be zero and is not treated as a keyword mask".
    ControlEtwLoggingFlagsNotZero,
}

impl HeliosEtwReject {
    /// Stable small code, so a KMD counter array can be indexed by reason
    /// without a string table. Codes are append-only.
    #[inline]
    pub const fn code(self) -> u32 {
        match self {
            Self::UnknownEventId => 1,
            Self::DescriptorVersionMismatch => 2,
            Self::DescriptorChannelMismatch => 3,
            Self::DescriptorLevelMismatch => 4,
            Self::DescriptorOpcodeMismatch => 5,
            Self::DescriptorTaskMismatch => 6,
            Self::DescriptorKeywordMismatch => 7,
            Self::PayloadLengthMismatch => 8,
            Self::PayloadNotReadable => 9,
            Self::PackageGenerationZero => 10,
            Self::PackageGenerationMismatch => 11,
            Self::ExpectedPackageGenerationZero => 12,
            Self::AdapterGenerationZero => 13,
            Self::FlagsReservedBitsSet => 14,
            Self::FlagsSubkindOutOfRange => 15,
            Self::NodeOrPlaneOutOfRange => 16,
            Self::AuxiliaryValue0NotZero => 17,
            Self::AuxiliaryValue1NotZero => 18,
            Self::ControlEtwLoggingFlagsNotZero => 19,
        }
    }

    /// Reason name for the capture tool's output.
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UnknownEventId => "UnknownEventId",
            Self::DescriptorVersionMismatch => "DescriptorVersionMismatch",
            Self::DescriptorChannelMismatch => "DescriptorChannelMismatch",
            Self::DescriptorLevelMismatch => "DescriptorLevelMismatch",
            Self::DescriptorOpcodeMismatch => "DescriptorOpcodeMismatch",
            Self::DescriptorTaskMismatch => "DescriptorTaskMismatch",
            Self::DescriptorKeywordMismatch => "DescriptorKeywordMismatch",
            Self::PayloadLengthMismatch => "PayloadLengthMismatch",
            Self::PayloadNotReadable => "PayloadNotReadable",
            Self::PackageGenerationZero => "PackageGenerationZero",
            Self::PackageGenerationMismatch => "PackageGenerationMismatch",
            Self::ExpectedPackageGenerationZero => "ExpectedPackageGenerationZero",
            Self::AdapterGenerationZero => "AdapterGenerationZero",
            Self::FlagsReservedBitsSet => "FlagsReservedBitsSet",
            Self::FlagsSubkindOutOfRange => "FlagsSubkindOutOfRange",
            Self::NodeOrPlaneOutOfRange => "NodeOrPlaneOutOfRange",
            Self::AuxiliaryValue0NotZero => "AuxiliaryValue0NotZero",
            Self::AuxiliaryValue1NotZero => "AuxiliaryValue1NotZero",
            Self::ControlEtwLoggingFlagsNotZero => "ControlEtwLoggingFlagsNotZero",
        }
    }
}

/// Highest [`HeliosEtwReject::code`] this revision defines — the size a
/// reason-indexed counter array needs (`code` is 1-based, so `+ 1` slots).
pub const HELIOS_ETW_REJECT_CODE_MAX: u32 = 19;

// ── Emission gate (§12.3) ───────────────────────────────────────────────────

/// Verdict of the driver-owned half of the §12.3 emission gate.
///
/// "An event is constructed only when both that OS gate and
/// `EtwProviderEnabled(REGHANDLE, level, keyword)` accept it." This enum is the
/// first half — the atomic enabled bit and maximum level that
/// `DxgkDdiControlEtwLogging` maintains. The caller must still call
/// `EtwProviderEnabled` with [`HeliosEtwEventId::level`] and
/// [`HeliosEtwEventId::keyword`]; keywords are owned there, never here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosEtwGate {
    /// Both driver-side conditions pass; the caller may now consult
    /// `EtwProviderEnabled` and, if that also accepts, build the payload.
    Emit,
    /// The OS enabled bit is clear. No payload construction, no trace write.
    SuppressedByOsDisable,
    /// The event's level exceeds the OS-supplied maximum logging level.
    SuppressedByLevel,
}

/// The driver-side emission gate. Pure, branch-only, and safe at any IRQL — it
/// allocates nothing, waits on nothing, and looks nothing up, which is exactly
/// what §12.3 requires of an event site.
#[inline]
pub const fn gate_event(
    event: HeliosEtwEventId,
    os_enabled: bool,
    os_max_level: u8,
) -> HeliosEtwGate {
    if !os_enabled {
        return HeliosEtwGate::SuppressedByOsDisable;
    }
    if event.level() > os_max_level {
        return HeliosEtwGate::SuppressedByLevel;
    }
    HeliosEtwGate::Emit
}

/// Validate `DXGKARG_CONTROLETWLOGGING::Flags`.
///
/// §12.3: the callback "changes only an atomic enabled bit and maximum logging
/// level; its currently undefined `Flags` must be zero and is not treated as a
/// keyword mask." The parameter is `u64` so a nonzero high half can never be
/// truncated away regardless of the WDK field's width (the WDK header is not
/// available to this crate).
#[inline]
pub const fn validate_control_etw_logging_flags(flags: u64) -> Result<(), HeliosEtwReject> {
    if flags != 0 {
        return Err(HeliosEtwReject::ControlEtwLoggingFlagsNotZero);
    }
    Ok(())
}

// ── Validation (total, named-reason, panic-free) ────────────────────────────

/// Encode a sub-kind into a [`HeliosGraphicsEtwPayloadV1::flags`] word.
///
/// Rejects a sub-kind the event ID does not name, so an emitter cannot write a
/// transition that has no meaning. All reserved bits are zero by construction.
#[inline]
pub const fn encode_flags(event: HeliosEtwEventId, subkind: u32) -> Result<u32, HeliosEtwReject> {
    if subkind >= event.subkind_count() {
        return Err(HeliosEtwReject::FlagsSubkindOutOfRange);
    }
    // `subkind < 3` here, so the shift cannot leave the mask.
    Ok((subkind << HELIOS_ETW_FLAG_SUBKIND_SHIFT) & HELIOS_ETW_FLAG_SUBKIND_MASK)
}

/// Validate a captured/constructed `EVENT_DESCRIPTOR` against the fixed §12.3
/// table and return the event it names.
///
/// Every field is compared, not just the ID: §17.7's decoder acceptance is
/// "byte-exact decoding of all section-12.3 IDs/flags/keywords".
pub const fn validate_descriptor_v1(
    descriptor: &HeliosEtwEventDescriptor,
) -> Result<HeliosEtwEventId, HeliosEtwReject> {
    let event = match HeliosEtwEventId::from_id(descriptor.id) {
        Some(event) => event,
        None => return Err(HeliosEtwReject::UnknownEventId),
    };
    if descriptor.version != HELIOS_ETW_EVENT_VERSION {
        return Err(HeliosEtwReject::DescriptorVersionMismatch);
    }
    if descriptor.channel != HELIOS_ETW_CHANNEL_NONE {
        return Err(HeliosEtwReject::DescriptorChannelMismatch);
    }
    if descriptor.level != event.level() {
        return Err(HeliosEtwReject::DescriptorLevelMismatch);
    }
    if descriptor.opcode != HELIOS_ETW_OPCODE_INFO {
        return Err(HeliosEtwReject::DescriptorOpcodeMismatch);
    }
    if descriptor.task != HELIOS_ETW_TASK_NONE {
        return Err(HeliosEtwReject::DescriptorTaskMismatch);
    }
    if descriptor.keyword != event.keyword() {
        return Err(HeliosEtwReject::DescriptorKeywordMismatch);
    }
    Ok(event)
}

/// Structural validation of one payload against its event ID.
///
/// Checks exactly what §12.3 states and nothing it does not:
///
/// - `package_generation`/`adapter_generation` are present (nonzero) — those
///   two rows, unlike the object/context rows, carry no "zero when not
///   applicable" clause;
/// - flags carry no reserved bits and a sub-kind the event ID names;
/// - `node_or_plane` is within the selected profile's bound or the sentinel;
/// - `aux0`/`aux1` are zero, this revision defining no interpretation.
///
/// Deliberately NOT checked, because §12.3 does not make them checkable
/// per event: `object_generation`/`context_generation` ("zero when not
/// applicable" — applicability is not enumerated), `status` ("zero for an event
/// without one" — likewise), `sequence_value` (any value), and `vidpn_source`
/// (any "exact OS value" is legal, so no bound may be imposed).
pub const fn validate_payload_v1(
    event: HeliosEtwEventId,
    payload: &HeliosGraphicsEtwPayloadV1,
) -> Result<(), HeliosEtwReject> {
    if payload.package_generation == 0 {
        return Err(HeliosEtwReject::PackageGenerationZero);
    }
    if payload.adapter_generation == 0 {
        return Err(HeliosEtwReject::AdapterGenerationZero);
    }
    if payload.flags & HELIOS_ETW_FLAGS_RESERVED_MASK != 0 {
        return Err(HeliosEtwReject::FlagsReservedBitsSet);
    }
    if payload.subkind() >= event.subkind_count() {
        return Err(HeliosEtwReject::FlagsSubkindOutOfRange);
    }
    if payload.node_or_plane != HELIOS_ETW_NODE_OR_PLANE_NONE
        && payload.node_or_plane > HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX
    {
        return Err(HeliosEtwReject::NodeOrPlaneOutOfRange);
    }
    if payload.aux0 != 0 {
        return Err(HeliosEtwReject::AuxiliaryValue0NotZero);
    }
    if payload.aux1 != 0 {
        return Err(HeliosEtwReject::AuxiliaryValue1NotZero);
    }
    Ok(())
}

/// [`validate_payload_v1`] plus the exact package-generation cross-check.
///
/// `expected_package_generation` is the caller's own package generation (the
/// §17.1 shared constant, which this module deliberately does not import — it
/// belongs to whichever module owns it, and hard-coding a second copy here
/// would be a second source of truth). Zero is not a wildcard: a check that
/// cannot be performed is [`HeliosEtwReject::ExpectedPackageGenerationZero`].
pub const fn validate_payload_v1_for_generation(
    event: HeliosEtwEventId,
    payload: &HeliosGraphicsEtwPayloadV1,
    expected_package_generation: u64,
) -> Result<(), HeliosEtwReject> {
    if expected_package_generation == 0 {
        return Err(HeliosEtwReject::ExpectedPackageGenerationZero);
    }
    if let Err(reason) = validate_payload_v1(event, payload) {
        return Err(reason);
    }
    if payload.package_generation != expected_package_generation {
        return Err(HeliosEtwReject::PackageGenerationMismatch);
    }
    Ok(())
}

/// Decode one captured event: descriptor + the single data descriptor's bytes.
///
/// Total — a short, long, or malformed capture yields a named reason. The bytes
/// are read unaligned, because an ETW consumer hands out whatever alignment the
/// trace buffer happened to have.
pub fn decode_event_v1(
    descriptor: &HeliosEtwEventDescriptor,
    payload_bytes: &[u8],
) -> Result<(HeliosEtwEventId, HeliosGraphicsEtwPayloadV1), HeliosEtwReject> {
    let event = validate_descriptor_v1(descriptor)?;
    if payload_bytes.len() != HELIOS_ETW_PAYLOAD_V1_SIZE {
        return Err(HeliosEtwReject::PayloadLengthMismatch);
    }
    let payload: HeliosGraphicsEtwPayloadV1 = match bytemuck::try_pod_read_unaligned(payload_bytes)
    {
        Ok(payload) => payload,
        Err(_) => return Err(HeliosEtwReject::PayloadNotReadable),
    };
    match validate_payload_v1(event, &payload) {
        Ok(()) => Ok((event, payload)),
        Err(reason) => Err(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload that passes structural validation, for mutation in the
    /// rejection tests below.
    fn good(event: HeliosEtwEventId) -> HeliosGraphicsEtwPayloadV1 {
        HeliosGraphicsEtwPayloadV1::new(
            7,
            3,
            0,
            0,
            42,
            0,
            encode_flags(event, HELIOS_ETW_SUBKIND_SOLE).unwrap(),
            HELIOS_ETW_VIDPN_SOURCE_UNINITIALIZED,
            HELIOS_ETW_NODE_OR_PLANE_NONE,
        )
    }

    #[test]
    fn provider_guid_matches_the_documented_text() {
        // {6D9A1A95-2B6A-4DEF-BCF7-847B6F158B0E}
        assert_eq!(HELIOS_ETW_PROVIDER_GUID.data1, 0x6D9A_1A95);
        assert_eq!(HELIOS_ETW_PROVIDER_GUID.data2, 0x2B6A);
        assert_eq!(HELIOS_ETW_PROVIDER_GUID.data3, 0x4DEF);
        assert_eq!(
            HELIOS_ETW_PROVIDER_GUID.data4,
            [0xBC, 0xF7, 0x84, 0x7B, 0x6F, 0x15, 0x8B, 0x0E]
        );
        assert_eq!(
            HELIOS_ETW_PROVIDER_GUID.to_binary(),
            HELIOS_ETW_PROVIDER_GUID_BYTES
        );
    }

    #[test]
    fn every_event_id_round_trips_and_has_one_class_keyword() {
        for (index, event) in HeliosEtwEventId::ALL.iter().copied().enumerate() {
            assert_eq!(event.id() as usize, index + 1);
            assert_eq!(HeliosEtwEventId::from_id(event.id()), Some(event));
            assert_eq!(HELIOS_ETW_EVENT_DESCRIPTORS[index], event.descriptor());
            assert_eq!(event.keyword().count_ones(), 1);
            assert_eq!(event.keyword() & !HELIOS_ETW_KEYWORD_ALL, 0);
            assert_eq!(validate_descriptor_v1(&event.descriptor()), Ok(event));
        }
        assert_eq!(HeliosEtwEventId::from_id(0), None);
        assert_eq!(HeliosEtwEventId::from_id(HELIOS_ETW_EVENT_ID_MAX + 1), None);
    }

    #[test]
    fn keyword_classes_follow_the_section_12_3_bit_assignment() {
        assert_eq!(
            HeliosEtwEventId::BatchCancelOrFault.keyword(),
            HELIOS_ETW_KEYWORD_SUBMISSION
        );
        assert_eq!(
            HeliosEtwEventId::NativeFenceWaitOrSignal.keyword(),
            HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION
        );
        // The §10.8 plane latch/cancel and reader-release/unbind events.
        assert_eq!(
            HeliosEtwEventId::PlaneLatchOrCancel.keyword(),
            HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME
        );
        assert_eq!(
            HeliosEtwEventId::PlaneReaderReleaseOrUnbind.keyword(),
            HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME
        );
        assert_eq!(
            HeliosEtwEventId::DeviceResetOrRemoval.keyword(),
            HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE
        );
        assert_eq!(
            HeliosEtwEventId::TranslationSynchronousProgress.keyword(),
            HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME
        );
    }

    #[test]
    fn subkind_encoding_is_bounded_by_the_event_name() {
        // One-transition events accept only the sole sub-kind.
        assert_eq!(
            encode_flags(HeliosEtwEventId::BatchSubmit, HELIOS_ETW_SUBKIND_SOLE),
            Ok(0)
        );
        assert_eq!(
            encode_flags(HeliosEtwEventId::BatchSubmit, 1),
            Err(HeliosEtwReject::FlagsSubkindOutOfRange)
        );
        // Pairs accept 0 and 1 only.
        assert_eq!(
            encode_flags(
                HeliosEtwEventId::PlaneLatchOrCancel,
                HELIOS_ETW_SUBKIND_PLANE_CANCEL
            ),
            Ok(1)
        );
        assert_eq!(
            encode_flags(HeliosEtwEventId::PlaneLatchOrCancel, 2),
            Err(HeliosEtwReject::FlagsSubkindOutOfRange)
        );
        // Only the translation-session event has a third.
        assert_eq!(
            encode_flags(
                HeliosEtwEventId::TranslationSessionCreateAttachOrDrain,
                HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_DRAIN
            ),
            Ok(2)
        );
        assert_eq!(
            encode_flags(HeliosEtwEventId::TranslationSessionCreateAttachOrDrain, 3),
            Err(HeliosEtwReject::FlagsSubkindOutOfRange)
        );
    }

    #[test]
    fn a_well_formed_event_decodes_byte_exactly() {
        let event = HeliosEtwEventId::PlaneLatchOrCancel;
        let payload = HeliosGraphicsEtwPayloadV1::new(
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
            0x2122_2324_2526_2728,
            0x3132_3334_3536_3738,
            0x4142_4344_4546_4748,
            -1_073_741_823, // an NTSTATUS failure code survives sign-exactly
            encode_flags(event, HELIOS_ETW_SUBKIND_PLANE_LATCH).unwrap(),
            3,
            0,
        );
        let bytes = bytemuck::bytes_of(&payload);
        assert_eq!(bytes.len(), HELIOS_ETW_PAYLOAD_V1_SIZE);
        // Offsets, read straight out of the bytes rather than via the fields.
        assert_eq!(&bytes[0..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&bytes[8..16], &0x1112_1314_1516_1718u64.to_le_bytes());
        assert_eq!(&bytes[16..24], &0x2122_2324_2526_2728u64.to_le_bytes());
        assert_eq!(&bytes[24..32], &0x3132_3334_3536_3738u64.to_le_bytes());
        assert_eq!(&bytes[32..40], &0x4142_4344_4546_4748u64.to_le_bytes());
        assert_eq!(&bytes[40..44], &(-1_073_741_823i32).to_le_bytes());
        assert_eq!(&bytes[44..48], &0u32.to_le_bytes());
        assert_eq!(&bytes[48..52], &3u32.to_le_bytes());
        assert_eq!(&bytes[52..56], &0u32.to_le_bytes());
        assert_eq!(&bytes[56..64], &0u64.to_le_bytes());
        assert_eq!(&bytes[64..72], &0u64.to_le_bytes());

        // Decoded from an intentionally unaligned copy.
        let mut unaligned = [0u8; HELIOS_ETW_PAYLOAD_V1_SIZE + 1];
        unaligned[1..].copy_from_slice(bytes);
        assert_eq!(
            decode_event_v1(&event.descriptor(), &unaligned[1..]),
            Ok((event, payload))
        );
    }

    #[test]
    fn descriptor_mismatches_are_named_not_guessed() {
        let event = HeliosEtwEventId::BatchSubmit;
        let mut d = event.descriptor();
        d.id = 0;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::UnknownEventId)
        );
        d = event.descriptor();
        d.version = 2;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::DescriptorVersionMismatch)
        );
        d = event.descriptor();
        d.channel = 1;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::DescriptorChannelMismatch)
        );
        d = event.descriptor();
        d.level = HELIOS_ETW_LEVEL_VERBOSE;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::DescriptorLevelMismatch)
        );
        d = event.descriptor();
        d.opcode = 1;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::DescriptorOpcodeMismatch)
        );
        d = event.descriptor();
        d.task = 1;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::DescriptorTaskMismatch)
        );
        d = event.descriptor();
        d.keyword = HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME;
        assert_eq!(
            validate_descriptor_v1(&d),
            Err(HeliosEtwReject::DescriptorKeywordMismatch)
        );
    }

    #[test]
    fn payload_rejections_are_named_not_guessed() {
        let event = HeliosEtwEventId::BatchSubmit;
        assert_eq!(validate_payload_v1(event, &good(event)), Ok(()));

        let mut p = good(event);
        p.package_generation = 0;
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::PackageGenerationZero)
        );

        p = good(event);
        p.adapter_generation = 0;
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::AdapterGenerationZero)
        );

        p = good(event);
        p.flags = 1 << 5;
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::FlagsReservedBitsSet)
        );

        p = good(event);
        p.flags = 1; // BatchSubmit names exactly one transition
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::FlagsSubkindOutOfRange)
        );

        p = good(event);
        p.node_or_plane = HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX + 1;
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::NodeOrPlaneOutOfRange)
        );
        p.node_or_plane = HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX;
        assert_eq!(validate_payload_v1(event, &p), Ok(()));

        p = good(event);
        p.aux0 = 1;
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::AuxiliaryValue0NotZero)
        );

        p = good(event);
        p.aux1 = 1;
        assert_eq!(
            validate_payload_v1(event, &p),
            Err(HeliosEtwReject::AuxiliaryValue1NotZero)
        );

        // A default/zeroed payload never passes: it is an uninitialized site.
        assert_eq!(
            validate_payload_v1(event, &HeliosGraphicsEtwPayloadV1::default()),
            Err(HeliosEtwReject::PackageGenerationZero)
        );
    }

    #[test]
    fn generation_crosscheck_has_no_wildcard() {
        let event = HeliosEtwEventId::BatchSubmit;
        let p = good(event); // package_generation == 7
        assert_eq!(validate_payload_v1_for_generation(event, &p, 7), Ok(()));
        assert_eq!(
            validate_payload_v1_for_generation(event, &p, 8),
            Err(HeliosEtwReject::PackageGenerationMismatch)
        );
        assert_eq!(
            validate_payload_v1_for_generation(event, &p, 0),
            Err(HeliosEtwReject::ExpectedPackageGenerationZero)
        );
    }

    #[test]
    fn decode_rejects_a_short_or_long_data_descriptor() {
        let event = HeliosEtwEventId::BatchSubmit;
        let payload = good(event);
        let bytes = bytemuck::bytes_of(&payload);
        assert_eq!(
            decode_event_v1(
                &event.descriptor(),
                &bytes[..HELIOS_ETW_PAYLOAD_V1_SIZE - 1]
            ),
            Err(HeliosEtwReject::PayloadLengthMismatch)
        );
        let mut long = [0u8; HELIOS_ETW_PAYLOAD_V1_SIZE + 8];
        long[..HELIOS_ETW_PAYLOAD_V1_SIZE].copy_from_slice(bytes);
        assert_eq!(
            decode_event_v1(&event.descriptor(), &long),
            Err(HeliosEtwReject::PayloadLengthMismatch)
        );
    }

    #[test]
    fn the_os_gate_is_enable_bit_plus_maximum_level_only() {
        let event = HeliosEtwEventId::BatchSubmit;
        assert_eq!(
            gate_event(event, false, HELIOS_ETW_LEVEL_VERBOSE),
            HeliosEtwGate::SuppressedByOsDisable
        );
        assert_eq!(
            gate_event(event, true, HELIOS_ETW_LEVEL_WARNING),
            HeliosEtwGate::SuppressedByLevel
        );
        assert_eq!(
            gate_event(event, true, HELIOS_ETW_LEVEL_INFORMATIONAL),
            HeliosEtwGate::Emit
        );
        assert_eq!(
            gate_event(event, true, HELIOS_ETW_LEVEL_VERBOSE),
            HeliosEtwGate::Emit
        );
    }

    #[test]
    fn control_etw_logging_flags_must_be_zero() {
        assert_eq!(validate_control_etw_logging_flags(0), Ok(()));
        // Notably including a value that looks like a keyword mask — §12.3
        // says the field is NOT one.
        assert_eq!(
            validate_control_etw_logging_flags(HELIOS_ETW_KEYWORD_ALL),
            Err(HeliosEtwReject::ControlEtwLoggingFlagsNotZero)
        );
        assert_eq!(
            validate_control_etw_logging_flags(1u64 << 63),
            Err(HeliosEtwReject::ControlEtwLoggingFlagsNotZero)
        );
    }

    #[test]
    fn every_rejection_reason_has_a_unique_code_and_name() {
        const ALL: [HeliosEtwReject; 19] = [
            HeliosEtwReject::UnknownEventId,
            HeliosEtwReject::DescriptorVersionMismatch,
            HeliosEtwReject::DescriptorChannelMismatch,
            HeliosEtwReject::DescriptorLevelMismatch,
            HeliosEtwReject::DescriptorOpcodeMismatch,
            HeliosEtwReject::DescriptorTaskMismatch,
            HeliosEtwReject::DescriptorKeywordMismatch,
            HeliosEtwReject::PayloadLengthMismatch,
            HeliosEtwReject::PayloadNotReadable,
            HeliosEtwReject::PackageGenerationZero,
            HeliosEtwReject::PackageGenerationMismatch,
            HeliosEtwReject::ExpectedPackageGenerationZero,
            HeliosEtwReject::AdapterGenerationZero,
            HeliosEtwReject::FlagsReservedBitsSet,
            HeliosEtwReject::FlagsSubkindOutOfRange,
            HeliosEtwReject::NodeOrPlaneOutOfRange,
            HeliosEtwReject::AuxiliaryValue0NotZero,
            HeliosEtwReject::AuxiliaryValue1NotZero,
            HeliosEtwReject::ControlEtwLoggingFlagsNotZero,
        ];
        for (index, reason) in ALL.iter().copied().enumerate() {
            assert_eq!(reason.code() as usize, index + 1);
            assert!(reason.code() <= HELIOS_ETW_REJECT_CODE_MAX);
            assert!(!reason.name().is_empty());
        }
        assert_eq!(ALL.len() as u32, HELIOS_ETW_REJECT_CODE_MAX);
    }
}
