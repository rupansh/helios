//! TEMPORARY post-start bring-up tracer (remove once Code 43 / AddAdapter clears).
//!
//! dxgkrnl's StartAdapter→AddAdapter sequence drives a series of our DDIs and can
//! fail internally (e.g. `STATUS_OBJECT_NAME_NOT_FOUND`) with no NTSTATUS we get
//! to see. To find which DDI dxgkrnl is calling (and which we answer how) right
//! before it gives up, each instrumented PASSIVE-level DDI calls [`record`],
//! which appends a `REG_DWORD` breadcrumb as values `S0`, `S1`, `S2`, … under
//! `HKLM\SYSTEM\CurrentControlSet\Services\helios_kmd_render`. After a repro read them in
//! order (`reg query` / `Get-ItemProperty`); the last few before the failure
//! point at the culprit.
//!
//! IRQL: `RtlWriteRegistryValue` requires PASSIVE_LEVEL — only call [`record`]
//! from PASSIVE DDIs (never the DPC/ISR or DISPATCH paging paths).
//!
//! Breadcrumb code encoding (high byte = which DDI, low bytes = detail):
//!   0x01_00_0000 | type     QueryAdapterInfo entry (DXGK_QUERYADAPTERINFOTYPE)
//!   0x02_00_0000 | type     QueryAdapterInfo answered STATUS_NOT_SUPPORTED (type)
//!   0x03_00_0000 | ordinal  GetNodeMetadata entry
//!   0x04_00_0000            QueryInterface entry (followed by the GUID Data1)
//!   0x05_00_0000            GetRootPageTableSize entry
//!   0x06_00_0000            CreateProcess entry
//!   raw value               an interface GUID Data1 logged after a 0x04 marker
//!
//! COLLISIONS FIXED 2026-07-27 (T4b/R722) — these are owner debugging ABI, so
//! the old values are recorded here rather than only in git:
//!   0x0B00_00E7  was BOTH venus-bring-up-failed (ddi/lifecycle.rs) and
//!                HPD-worker-create-failed (adapter.rs), both inside the
//!                StartDevice window. HPD moved to 0x0B00_00EA.
//!   0x0E00_0001  was BOTH DestroyDevice entry (device.rs) and
//!                ExchangePreStartInfo entry (display.rs). The 0x0E00_* block is
//!                the device-teardown family, so ExchangePreStartInfo moved to
//!                0x0E10_0001 (and its success marker 0x0E00_0002 -> 0x0E10_0002).

use core::sync::atomic::{AtomicU32, Ordering};

use wdk_sys::ntddk::RtlWriteRegistryValue;

/// Cached `DiagLevel` service-key knob (u32::MAX = not read yet).
/// Level 0 (default, PSC stage): the `S<idx>` breadcrumb ring is OFF — it is
/// bring-up archaeology, and its steady-state writers (QueryAdapterInfo
/// polling, paging/allocation paths) each cost a synchronous kernel registry
/// write. Level >= 1 restores full breadcrumb tracing. Named counters
/// (`record_named*`) are NOT gated here — each caller decides its own flush
/// cadence; failure counters must stay loud.
static DIAG_LEVEL: AtomicU32 = AtomicU32::new(u32::MAX);

/// Read (once) and cache the `DiagLevel` knob. PASSIVE_LEVEL only — every
/// legal [`record`] caller already is. Benign race on first concurrent calls.
pub fn level() -> u32 {
    let cached = DIAG_LEVEL.load(Ordering::Relaxed);
    if cached != u32::MAX {
        return cached;
    }
    let level = read_config_dword(knobs::DIAG_LEVEL, 0);
    DIAG_LEVEL.store(level, Ordering::Relaxed);
    level
}

/// `RTL_REGISTRY_SERVICES` — Path is relative to
/// `\Registry\Machine\System\CurrentControlSet\Services`.
const RTL_REGISTRY_SERVICES: u32 = 1;
/// `REG_DWORD`.
const REG_DWORD: u32 = 4;
/// Cap on breadcrumbs so a chatty steady state can't grow the key unbounded.
const MAX_STEPS: u32 = 3000;

static STEP: AtomicU32 = AtomicU32::new(0);

/// `"helios_kmd_render\0"` as UTF-16 — the service subkey under Services.
static SERVICE_NAME: [u16; 18] = [
    b'h' as u16,
    b'e' as u16,
    b'l' as u16,
    b'i' as u16,
    b'o' as u16,
    b's' as u16,
    b'_' as u16,
    b'k' as u16,
    b'm' as u16,
    b'd' as u16,
    b'_' as u16,
    b'r' as u16,
    b'e' as u16,
    b'n' as u16,
    b'd' as u16,
    b'e' as u16,
    b'r' as u16,
    0,
];

/// Write a DWORD breadcrumb to a FIXED value name (not the `S<idx>` ring). The
/// `S*` ring is overwritten within ~1s by steady-state QueryAdapterInfo polling,
/// so it is useless for one-shot tracing of a rare DDI (e.g. Present). A fixed
/// name persists until the next write, so it can be read live from the registry.
/// `name` must be a NUL-terminated UTF-16 value name. PASSIVE_LEVEL only.
///
/// PRIVATE on purpose: [`record_named_bytes`] is the entry point. `display.rs`
/// carried a byte-identical copy of that wrapper (`rec_named`) that called this
/// directly, which is how 79 registry writes accumulated on the Present path
/// outside every policy this module documents. With the raw writer private,
/// a future bypass has to be a deliberate edit to this file.
fn record_named(name: &[u16], mut code: u32) {
    // SAFETY: PASSIVE_LEVEL (see module note). `name` is a caller-provided
    // NUL-terminated UTF-16 value name; ValueData points to a 4-byte DWORD that
    // RtlWriteRegistryValue copies before returning.
    unsafe {
        let _ = RtlWriteRegistryValue(
            RTL_REGISTRY_SERVICES,
            SERVICE_NAME.as_ptr(),
            name.as_ptr(),
            REG_DWORD,
            (&mut code as *mut u32).cast::<core::ffi::c_void>(),
            4,
        );
    }
}

/// A lifecycle failure that must stay visible on a **default** boot.
///
/// [`record`] returns early when `DiagLevel` is 0 (the default), so a refusal
/// reported through it leaves no trace at all in production — the driver starts
/// degraded and silent. The module contract at the top of this file already says
/// failure counters must stay loud; this enum is how that is enforced. A
/// `FaultCounter` cannot be passed to the gated ring, and a raw `u32` breadcrumb
/// cannot be passed to [`fault`], so at every converted site "this failure is
/// reported through the lossy mechanism" is a type error.
///
/// It does not stop a future author from reaching for [`record`] on a *new*
/// failure path; that remains a review rule.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FaultCounter {
    /// `VirtioGpu::init` failed — value is the resulting NTSTATUS. The adapter
    /// starts render-only with no transport.
    StVio,
    /// Venus client bring-up failed — value is the resulting NTSTATUS.
    StVnu,
    /// The HPD worker thread could not be created — value is the NTSTATUS.
    StHpd,
    /// The exact transport local-memory capacity was rejected — value is the
    /// rejected size in MiB.
    StBar,
    /// The display half asked the host for its scan-out mode but the transport
    /// was gone — value is the NTSTATUS. The mode falls back to a fabricated
    /// default, so the OS is told about a monitor whose size we invented.
    StTxG,
    /// The transport answered the mode query but reported nothing usable —
    /// value is 1. Same fallback, different cause.
    StMdB,
    /// The display half was demoted to render-only for this start because the
    /// transport is absent — value is the NTSTATUS that killed it, or 1 if the
    /// transport was already gone for another reason. The adapter still binds.
    StNoTx,
    /// `DxgkDdiDispatchIoRequest` was called — value is the IoControlCode. A
    /// WDDM display miniport is effectively never called on this legacy
    /// video-port path, so any movement here is itself the news.
    StVrp,
    /// `DxgkDdiQueryChildStatus` was called with the display half off — value is
    /// the child status Type. Behaviour-neutral in the field
    /// (`NumberOfChildren` is 0 in that configuration), so movement means the
    /// two are out of step.
    StQcs,
    /// `MmMapIoSpace` failed for the virtio ISR-status register — value is the
    /// failure count. NOT a benign degrade on this INTx device: with no ISR ack
    /// the level-triggered line stays asserted and Windows' interrupt-storm
    /// detector Code-43s the adapter.
    StIsr,
    /// A Venus LINEAR scan-out copy was requested with no target image — value
    /// is 0. Previously reported only through the DiagLevel-gated `diag(0x0136)`,
    /// so a default boot saw nothing but `ScCpy=0xE` / `CpCpy=0xE3`.
    CpTgtE,
    /// The virtio control ring latched its corruption failure. Value packs the
    /// saturated overlength-completion count in the high half and unmatched-
    /// token count in the low half.
    StRing,
    /// `stop_hpd` could not prove the HPD worker exited — value is the
    /// `ObReferenceObjectByHandle` status (STATUS_SUCCESS means the bounded join
    /// timed out instead). The adapter context is deliberately leaked rather
    /// than freed under a live worker.
    StHpdX,
    /// The virtio device did not clear its status within the reset handshake's
    /// spin bound — value is the spin count. Not reachable on the supported host
    /// (QEMU services the status write synchronously inside `virtio_reset()`),
    /// but every later assumption in `init` rests on that reset.
    StVioR,
    /// A `DXGK_DRIVERCAPS` field did not fit the versioned buffer dxgkrnl
    /// supplied and was SKIPPED — value is the count of skipped fields on the
    /// last `DXGKQAITYPE_DRIVERCAPS` query. The adapter still reports the
    /// maximal valid prefix, so this is a truncated capability surface rather
    /// than a failure, but any movement means the OS is being told less than
    /// this driver believes it said. Expected 0 on 24H2, which passes the full
    /// 592-byte struct.
    CapTrunc,
    /// The descriptor pass of the two-call segment protocol disagreed with the
    /// count reported on the descriptor-NULL call — value is the count the
    /// render pass wanted. The write loop is clamped to the reported count, so
    /// this is the counter behind that clamp's SAFETY comment. Not constructible
    /// with an immutable post-StartDevice table; it exists so that stays true.
    SegCntMis,
}

impl FaultCounter {
    /// Registry value name. Must stay ≤14 bytes: [`record_named_bytes`]
    /// truncates beyond that, which would silently merge two counters.
    const fn name(self) -> &'static [u8] {
        match self {
            FaultCounter::StVio => b"StVio",
            FaultCounter::StVnu => b"StVnu",
            FaultCounter::StHpd => b"StHpd",
            FaultCounter::StBar => b"StBar",
            FaultCounter::StTxG => b"StTxG",
            FaultCounter::StMdB => b"StMdB",
            FaultCounter::StNoTx => b"StNoTx",
            FaultCounter::StVrp => b"StVrp",
            FaultCounter::StQcs => b"StQcs",
            FaultCounter::StIsr => b"StIsr",
            FaultCounter::CpTgtE => b"CpTgtE",
            FaultCounter::StRing => b"StRing",
            FaultCounter::StHpdX => b"StHpdX",
            FaultCounter::StVioR => b"StVioR",
            FaultCounter::CapTrunc => b"CapTrunc",
            FaultCounter::SegCntMis => b"SegCntMis",
        }
    }

    /// Every counter, so StartDevice can zero the whole set in one place.
    const ALL: &'static [FaultCounter] = &[
        FaultCounter::StVio,
        FaultCounter::StVnu,
        FaultCounter::StHpd,
        FaultCounter::StBar,
        FaultCounter::StTxG,
        FaultCounter::StMdB,
        FaultCounter::StNoTx,
        FaultCounter::StVrp,
        FaultCounter::StQcs,
        FaultCounter::StIsr,
        FaultCounter::CpTgtE,
        FaultCounter::StRing,
        FaultCounter::StHpdX,
        FaultCounter::StVioR,
        FaultCounter::CapTrunc,
        FaultCounter::SegCntMis,
    ];
}

/// Compile-time proof that no fault-counter name is truncated.
///
/// [`record_named_bytes`] silently clamps to [`MAX_CONFIG_NAME`], so two counters
/// whose names share a 14-byte prefix would MERGE into one registry value — a
/// failure counter reading someone else's number. The rule was a doc comment on
/// [`FaultCounter::name`]; this makes it a build failure.
///
/// Scope, stated honestly: this covers the counter set, and [`KnobName`] covers
/// every knob READ. The ~478 remaining raw `record_named_bytes(b"…")` call sites
/// still rely on the review rule — converting them is a mechanical sweep too
/// large to land inside a tranche that has to stay reviewable.
const _: () = {
    let mut i = 0;
    while i < FaultCounter::ALL.len() {
        assert!(
            FaultCounter::ALL[i].name().len() <= MAX_CONFIG_NAME,
            "FaultCounter name exceeds MAX_CONFIG_NAME and would merge with another counter"
        );
        i += 1;
    }
};

/// Report a lifecycle failure through the **ungated** named-counter path.
/// PASSIVE_LEVEL only, like every other writer here.
pub fn fault(counter: FaultCounter, value: u32) {
    record_named_bytes(counter.name(), value);
}

/// Zero every [`FaultCounter`] once, at StartDevice entry.
///
/// Registry values persist across boots, so without this a stale nonzero value
/// from an earlier boot is indistinguishable from a fault that happened on this
/// one. The gate's rule is "verify a counter moved this boot"; this is what
/// makes that rule applicable.
pub fn reset_fault_counters() {
    let mut i = 0;
    while i < FaultCounter::ALL.len() {
        record_named_bytes(FaultCounter::ALL[i].name(), 0);
        i += 1;
    }
}

/// Sampling period for [`sample_tick`] at the default `DiagLevel`.
///
/// 600 is the period this driver already uses for its other throttled
/// telemetry (`create_allocation`'s breadcrumbs, the scanout pacing block), so
/// a sampled value refreshes about every 10 seconds at 60 Hz.
pub const SAMPLE_EVERY: u32 = 600;

/// The THIRD diag channel, alongside [`fault`] (ungated named counter) and
/// [`record`] (DiagLevel-gated ring): throttled named IDENTITY values.
///
/// Returns whether this call is a sampled one, so a whole identity block can be
/// gated on a single tick and therefore stay internally consistent — every
/// value in a sampled block comes from the same operation.
///
/// The problem it exists for: `record_named*` is one synchronous
/// `RtlWriteRegistryValue` per call with no gate, and `dxgkddi_present_inner`
/// performed ~30 of them for a DWM flip and ~55 for an app BLT — almost none of
/// them failure counters, just per-call dumps of geometry, formats, handles and
/// sizes that mattered during bring-up. That is a per-frame kernel registry tax
/// on the exact path the PSC stage is trying to measure.
///
/// Policy, deliberately explicit at every call site:
///   - a FAILURE goes through [`fault`] or an unconditional `record_named_bytes`;
///   - an IDENTITY value goes through this;
///   - bring-up archaeology goes through [`record`].
/// At `DiagLevel >= 1` this returns true every time, restoring the per-call
/// behaviour for a debugging session.
pub fn sample_tick(ticks: &AtomicU32) -> bool {
    let n = ticks.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    level() >= 1 || n == 1 || n % SAMPLE_EVERY == 0
}

/// One counter in a [`CounterBlock`].
pub struct CounterEntry {
    /// Registry value name (≤14 chars, as [`record_named_bytes`] requires).
    pub name: &'static [u8],
    pub value: CounterRef,
    /// A FAILURE counter: when one of these changes, the block flushes
    /// immediately regardless of the throttle, so a failure always surfaces on
    /// the operation that produced it.
    pub failure: bool,
}

/// The two atomic widths this driver's counter blocks hold. `U64Low` reports the
/// low 32 bits, exactly as the hand-rolled dumps did.
pub enum CounterRef {
    U32(&'static AtomicU32),
    U64Low(&'static core::sync::atomic::AtomicU64),
}

impl CounterRef {
    fn load(&self) -> u32 {
        match self {
            CounterRef::U32(a) => a.load(Ordering::Relaxed),
            CounterRef::U64Low(a) => a.load(Ordering::Relaxed) as u32,
        }
    }
}

/// How often a [`CounterBlock`] mirrors itself into the registry.
pub enum FlushPolicy {
    /// The 1st call and every Nth after it, at the default `DiagLevel`.
    EveryNth(u32),
}

/// A named counter block that can only be emitted through a throttled emitter.
///
/// Several modules once implemented the same "flush my counters to fixed
/// registry names" routine and had drifted on throttling. The remaining blocks
/// all use one bounded periodic policy plus immediate failure publication.
///
/// Values stay cumulative atomics, so flushing less often does not change what a
/// `reg query` reads at rest; only failure latency changes, and the
/// flush-on-failure-change rule bounds that. Every atomic stays a named `static`,
/// so the TDR report and ntoseye symbol reads are unaffected.
pub struct CounterBlock {
    pub entries: &'static [CounterEntry],
    /// Call counter driving [`FlushPolicy::EveryNth`].
    pub ticks: &'static AtomicU32,
    /// Last observed sum of the `failure` entries, for the flush-on-change rule.
    pub failures: &'static AtomicU32,
    pub policy: FlushPolicy,
}

impl CounterBlock {
    /// Mirror the block into the registry if the policy (or a changed failure
    /// counter, or `DiagLevel >= 1`) says so. PASSIVE_LEVEL only. Returns
    /// whether it published, so a caller can ride the same cadence.
    pub fn flush(&self) -> bool {
        let mut fail_sum: u32 = 0;
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].failure {
                fail_sum = fail_sum.wrapping_add(self.entries[i].value.load());
            }
            i += 1;
        }
        let previous = self.failures.swap(fail_sum, Ordering::Relaxed);
        let n = self.ticks.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let FlushPolicy::EveryNth(period) = self.policy;
        let due = n == 1 || n % period == 0;
        // A changed failure counter always wins over the throttle.
        if !(due || fail_sum != previous || level() >= 1) {
            return false;
        }
        self.publish();
        true
    }

    /// Mirror the block into the registry UNCONDITIONALLY. PASSIVE_LEVEL only.
    ///
    /// For the call that MATTERS, not for a steady state. [`Self::flush`]'s
    /// throttle can write nothing at all, and a success does not move a failure
    /// counter — so an event whose only evidence is a value counter that moved has
    /// to publish through this or risk being graded on the pre-event reading.
    pub fn publish(&self) {
        let mut i = 0;
        while i < self.entries.len() {
            record_named_bytes(self.entries[i].name, self.entries[i].value.load());
            i += 1;
        }
    }
}

/// Live set of UNBOUNDED `KeWaitForSingleObject` waits currently blocked, one
/// bit per [`wait`] code. A wait that never returns leaves its bit set, which is
/// what a single last-value breadcrumb cannot express: several threads block on
/// different objects at once and the last writer would hide the rest.
static WAIT_MASK: AtomicU32 = AtomicU32::new(0);

/// `DiagStep` cache: 0 = unread, 1 = off, 2 = on. The knob lookup is itself a
/// registry round-trip, so it is read once.
static STEP_DIAG: AtomicU32 = AtomicU32::new(0);

/// Is per-step / per-wait breadcrumbing enabled? PASSIVE_LEVEL only.
pub fn diag_step_on() -> bool {
    match STEP_DIAG.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = read_config_dword(knobs::DIAG_STEP, 0) != 0;
            STEP_DIAG.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// Codes for [`wait`]. Each names one untimed wait; the driver has no other way
/// to block forever, so `SxWait` nonzero after a hang IS the hang.
pub mod waits {
    pub const VENUS_MUTEX: u32 = 1;
    pub const SCANOUT_MUTEX: u32 = 2;
    pub const K11_COMPLETION: u32 = 3;
    pub const RING_ZERO: u32 = 4;
    pub const ATTACH_CHANGED: u32 = 5;
    pub const K11_RUNDOWN: u32 = 6;
    pub const OUTER_WORKER: u32 = 7;
    pub const NATIVE_CONTEXT: u32 = 8;
    pub const OPEN_OUTER: u32 = 9;
    pub const OPEN_EXECUTION: u32 = 10;
}

/// Bracket one untimed wait. PASSIVE_LEVEL only (every untimed wait is).
///
/// ⚠ `SxWait` must be PRE-CREATED in the service key: once the key is full the
/// registry silently refuses to create new values, which is what made three
/// earlier instruments read as "never fired".
pub fn wait(code: u32, entering: bool) {
    let bit = 1u32 << (code & 31);
    let mask = if entering {
        WAIT_MASK.fetch_or(bit, Ordering::Relaxed) | bit
    } else {
        WAIT_MASK.fetch_and(!bit, Ordering::Relaxed) & !bit
    };
    if diag_step_on() {
        record_named_bytes(b"SxWait", mask);
    }
}

/// Sites that have returned `STATUS_NOT_SUPPORTED` from a DDI, one bit each.
static NOT_SUPPORTED_SITES: AtomicU32 = AtomicU32::new(0);
static NOT_SUPPORTED_COUNT: AtomicU32 = AtomicU32::new(0);

/// Return `STATUS_NOT_SUPPORTED` from a DDI and record WHICH site did.
///
/// dxgkrnl logs an illegal DDI return as `AzureTriage` "Driver returned an
/// invalid NTSTATUS code", and on 2026-08-27 a burst of 12 of those
/// (0xC00000BB) landed at boot+65 s, exactly when the session wedges. The
/// driver has ~30 sites that can return this and the trace names only a
/// dxgkrnl-internal function id, so the site has to identify itself.
///
/// `NotSupM` is a bitmask of every site that has fired (a last-value counter
/// would be swamped by the legal, expected returns); `NotSup` carries the last
/// site and a total. PASSIVE_LEVEL only.
pub fn not_supported(site: u32) -> crate::dxgk::NTSTATUS {
    let bit = 1u32 << (site & 31);
    let mask = NOT_SUPPORTED_SITES.fetch_or(bit, Ordering::Relaxed) | bit;
    let count = NOT_SUPPORTED_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if diag_step_on() {
        record_named_bytes(b"NotSupM", mask);
        record_named_bytes(b"NotSup", (site << 16) | (count & 0xffff));
    }
    crate::dxgk::STATUS_NOT_SUPPORTED
}

/// `record_named` convenience: build the UTF-16 value name from an ASCII byte
/// slice (≤14 chars). PASSIVE_LEVEL only.
pub fn record_named_bytes(name: &[u8], value: u32) {
    let mut buf = [0u16; 16];
    let n = name.len().min(14);
    let mut i = 0;
    while i < n {
        buf[i] = name[i] as u16;
        i += 1;
    }
    buf[n] = 0;
    record_named(&buf[..=n], value);
}

/// `RTL_QUERY_REGISTRY_DIRECT` — store the value straight into EntryContext
/// (for REG_DWORD data that fits a ULONG). No callback routine.
const RTL_QUERY_REGISTRY_DIRECT: u32 = 0x20;

/// Read a REG_DWORD config value from the service key (the same key the
/// breadcrumbs live under), or `default` if absent/unreadable. The value name
/// is ASCII (≤14 chars). PASSIVE_LEVEL only. Bring-up experiment knobs: lets
/// AddAdapter-shape experiments iterate via `reg add` + `devcon restart`
/// instead of a rebuild+reboot per variant.
///
/// The value MUST be REG_DWORD (RTL_QUERY_REGISTRY_DIRECT without TYPECHECK
/// interprets string data as a UNICODE_STRING buffer — only this driver's own
/// documented knobs are read here).
/// Longest service-key value name [`read_config_dword`] can look up.
///
/// The lookup builds a UTF-16 name in a fixed 16-word buffer and NUL-terminates
/// it, so anything longer is silently TRUNCATED and the lookup then misses —
/// returning `default` forever with no diagnostic. `ScanoutForceReject` (18)
/// was created that way and read as 0 on every boot, which cost a deploy cycle.
pub const MAX_CONFIG_NAME: usize = 14;

/// A service-key value name PROVEN to survive the lookup buffer.
///
/// The constructor is `const fn` and every [`knobs`] entry is a `const` item, so
/// the length assert is evaluated during `cargo check` — not only at
/// monomorphisation, which is where the previous inline-const-in-a-generic-fn
/// form fired (a build failure but not a check failure, so `cargo check` passed
/// on a knob that could never be read).
#[derive(Clone, Copy)]
pub struct KnobName(&'static [u8]);

impl KnobName {
    /// Fails the BUILD if the name cannot survive the lookup buffer.
    pub const fn new(name: &'static [u8]) -> Self {
        assert!(
            name.len() <= MAX_CONFIG_NAME,
            "knob name exceeds the RtlQueryRegistryValues lookup buffer and would silently read as its default"
        );
        Self(name)
    }
}

/// Every service-key knob the driver reads, in one place.
///
/// A knob that is not here cannot be read, which makes this the inventory rather
/// than a list someone maintains in ROADMAP.md by grepping. Each is a `const`
/// item, so [`KnobName::new`]'s length assert runs on every build of this module.
pub mod knobs {
    use super::KnobName;

    /// Breadcrumb ring level. 0 (default) = the `S<idx>` ring is off.
    pub const DIAG_LEVEL: KnobName = KnobName::new(b"DiagLevel");
    /// CpuVisible cached-allocation kill switch (default 1 = cached).
    pub const ALLOC_CACHED: KnobName = KnobName::new(b"AllocCached");
    /// Retire ordinary (non-paging) WDDM DMA fences on host GPU COMPLETION
    /// rather than host DECODE (default 1 = GPU completion). 0 restores the
    /// historical decode-only behaviour. The ONE reader (and the contract this
    /// restores) is `crate::virtio::gpu::VirtioGpu::dma_gpu_fence`; the unread
    /// `AdapterKnobs` copy was deleted 2026-08-05.
    pub const DMA_GPU_FENCE: KnobName = KnobName::new(b"DmaGpuFence");
    /// Render+display adapter shape (default 1 = the render+display miniport,
    /// which is the product). 0 restores the boot-era render-only surface.
    pub const DISPLAY_HALF: KnobName = KnobName::new(b"DisplayHalf");
    /// Advertise `DXGK_VIDMMCAPS.CrossAdapterResource` (default 0).
    /// Exactly [`super::MAX_CONFIG_NAME`] bytes — the assert's live subject.
    pub const CROSS_ADAPT_CAPS: KnobName = KnobName::new(b"CrossAdaptCaps");
    /// `DXGK_FLIPCAPS` OVERRIDE. 0 (default) = the driver's own word
    /// (`FlipOnVSyncMmIo | FlipImmediateMmIo`); nonzero replaces it verbatim,
    /// so `FlipCapsX=2` restores the pre-2026-07-29 advertisement for an A/B.
    /// Bit order (bindgen, WDK 10.0.26100): 0 `FlipOnVSyncWithNoWait`,
    /// 1 `FlipOnVSyncMmIo`, 2 `FlipInterval`, 3 `FlipImmediateMmIo`. Read at
    /// AddAdapter, so `pnputil /restart-device` applies it without a rebuild.
    pub const FLIP_CAPS_EXTRA: KnobName = KnobName::new(b"FlipCapsX");
    /// `DXGK_DRIVERCAPS.MaxQueuedFlipOnVSync` — how many flips dxgkrnl may keep
    /// queued and pending on this adapter at once. Default 1 is the historical
    /// advertisement; a Helios flip retires only when its DMA fence completes,
    /// which by design waits on the venus work outstanding at submit, so a
    /// depth of 1 makes present N+1 wait for frame N's host completion. Read at
    /// AddAdapter, so `pnputil /restart-device` applies it without a rebuild.
    /// 0 is coerced to 1 (a zero-depth flip queue is not representable) and the
    /// value actually advertised is mirrored in the `FlipQueV` counter.
    pub const FLIP_QUEUE_DEPTH: KnobName = KnobName::new(b"FlipQueueN");
    /// MPO vsync shape (default 1): report `CRTC_VSYNC_WITH_MULTIPLANE_OVERLAY2`
    /// (INFO2, carries the completed PresentId — the non-HW-flip-queue model).
    /// 0 restores the INFO3 shape, whose only completion channel is the
    /// flip-queue log this driver does not implement: measured on .353-.356
    /// (2026-08-24), every MPO flip then never completes and dxgkrnl removes
    /// DWM's device seconds after its first present. Read at
    /// `direct_scanout::start`, so `pnputil /restart-device` applies it.
    pub const MPO_VSYNC2: KnobName = KnobName::new(b"MpoVsync2");
    /// Diagnostic: fill the D2 parking image with this byte instead of zeroing
    /// it, and flush the parking bind so the host reads it. Default 0 = zeroed
    /// and unflushed, the shipping behaviour. Nonzero turns parking into an
    /// end-to-end oracle: a host readback that still shows zero proves the
    /// KMD's own writes do not reach the scanned-out blob, which no counter can
    /// distinguish from "DWM rendered nothing". Read at `direct_scanout::start`.
    pub const PARK_PAINT: KnobName = KnobName::new(b"D2ParkPaint");
    /// Per-step progress brackets in `DxgkDdiCloseAllocation` and
    /// `DxgkDdiDestroyAllocation` (`CaStep`/`DaStep`). Default 0 = off, because
    /// each step is a synchronous `RtlWriteRegistryValue` on a DDI the
    /// compositor calls constantly. 1 = on: the LAST value written names the
    /// step that never returned, which is the only way to attribute a hang that
    /// holds dxgkrnl's adapter DDI lock and so silences everything after it.
    pub const DIAG_STEP: KnobName = KnobName::new(b"DiagStep");
    /// Diagnostic: after each scan-out flush, sample the flushed blob through
    /// the KMD's own canonical map and publish what the GUEST sees
    /// (`D2PxNz`/`D2PxMax`). Default 0 = off. It separates "the producer never
    /// wrote these bytes" from "the guest wrote them and the host reads a
    /// different allocation", which no host-side counter can. Read at
    /// `direct_scanout::start`; it leaks one window mapping per scanned-out
    /// resource on purpose.
    pub const PIXEL_PROBE: KnobName = KnobName::new(b"D2PxProbe");
    /// Extra `DXGK_SEGMENTFLAGS` bits OR'd into the LOCAL (segment 2)
    /// descriptor only. Default 0 = the driver's own word, bit-identical to a
    /// build without this knob.
    ///
    /// The subject it exists to test is `CpuVisible` (0x04). DxgKrnl ETW
    /// (2026-08-30) shows VidMm answering a CPU lock of a segment-2 allocation
    /// by EVICTING it and re-reserving with
    /// `VidMmPlacementRestrictionApertureSegment`, i.e. it does not treat
    /// segment 2 as CPU-reachable and never calls `DxgkDdiMapCpuHostAperture`.
    /// `d3dkmddi.h`'s union comment makes `CpuVisible` the "CPU can reach this
    /// segment" flag and `SupportsCpuHostAperture` only how; the two were never
    /// measured together with the aperture still in the allocation's supported
    /// set. Read at AddAdapter, so `pnputil /restart-device` applies it.
    pub const BAR_SEG_FLAGS_EXTRA: KnobName = KnobName::new(b"BarSegFlagsX");
    /// Let an HWA2 `SHARED` allocation prefer local memory (default 0 = no).
    ///
    /// Same ETW run: every resource DXVK actually maps arrives
    /// `CpuVisible|Shareable` with `SupportedSegmentSet = 1` — segment 2 is not
    /// even a candidate — because `hwa2_may_prefer_local_memory` excludes
    /// SHARED. The exclusion was earned (local placement destabilized
    /// LogonUI/DWM), so it stays the default; this is the A/B for re-testing it
    /// once a CPU lock can be served in place. Read at AddAdapter.
    pub const BAR_LOCAL_SHARED: KnobName = KnobName::new(b"BarLocalShare");
    /// Drop the linear aperture from a local-preferring allocation's SUPPORTED
    /// segment set, so segment 2 is the only place VidMm may put it
    /// (default 0 = both, the measured shape).
    ///
    /// This is the arm that FORCES the CPU-host-aperture path: with no aperture
    /// segment to escape to, a CPU lock must either be served through
    /// `DxgkDdiMapCpuHostAperture` or fail. Removing it alone historically made
    /// `pfnAllocateCb` return `E_INVALIDARG` on every CPU-visible allocation —
    /// which is the expected answer when the sole supported segment is a
    /// memory segment dxgkrnl does not consider CPU-reachable. Pair it with
    /// `BarSegFlagsX=4`, which supplies the `CpuVisible` bit that objection is
    /// about; that combination has never been measured. Read at AddAdapter.
    pub const BAR_SEGMENT_ONLY: KnobName = KnobName::new(b"BarSegOnly");
    /// Back a CPU-visible linear HWA2 allocation with the CREATOR'S OWN PAGES
    /// (`HeliosWddmAllocationDescV2::cpu_backing_va`), imported by the host as
    /// a guest blob, instead of a fresh host-side `vkAllocateMemory`.
    ///
    /// This is the two-buffer fix. Default 1 since 2026-09-01: every accepted
    /// desktop since 2026-08-30 ran with it, and 0 (re-measured by accident
    /// that day) is a black desktop with every readback zero. The UMD half is
    /// `UmdGuestBacking`; the two flip together. Every refusal falls back to
    /// the host allocation and is counted `Gb*`.
    pub const HWA2_GUEST_MEM: KnobName = KnobName::new(b"Hwa2GuestMem");
}

/// Read a service-key REG_DWORD knob, or `default` if absent.
pub fn read_config_dword(name: KnobName, default: u32) -> u32 {
    let name = name.0;
    let mut name_buf = [0u16; 16];
    let n = name.len().min(MAX_CONFIG_NAME);
    let mut i = 0;
    while i < n {
        name_buf[i] = name[i] as u16;
        i += 1;
    }
    name_buf[n] = 0;

    let mut value: u32 = default;
    // SAFETY: zeroed RTL_QUERY_REGISTRY_TABLE entries are valid; the second,
    // all-zero entry terminates the table (Name == NULL, QueryRoutine == NULL).
    let mut table: [wdk_sys::RTL_QUERY_REGISTRY_TABLE; 2] = unsafe { core::mem::zeroed() };
    table[0].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[0].Name = name_buf.as_ptr() as *mut u16;
    table[0].EntryContext = (&mut value as *mut u32).cast();
    // DefaultType/DefaultData stay zero (REG_NONE): an absent value leaves
    // `value` at `default`.
    // SAFETY: PASSIVE_LEVEL; Path is the NUL-terminated service subkey relative
    // to RTL_REGISTRY_SERVICES; the table is NUL-entry-terminated; EntryContext
    // points at a live ULONG for the duration of the call.
    unsafe {
        let _ = wdk_sys::ntddk::RtlQueryRegistryValues(
            RTL_REGISTRY_SERVICES,
            SERVICE_NAME.as_ptr(),
            table.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
    }
    value
}

/// Append one DWORD breadcrumb. Cheap and lossy by design (best-effort tracing).
/// No-op at `DiagLevel` 0 (the default) — see [`level`].
pub fn record(mut code: u32) {
    if level() == 0 {
        return;
    }
    let idx = STEP.fetch_add(1, Ordering::Relaxed);
    if idx >= MAX_STEPS {
        return;
    }
    // Build the value name "S<idx>\0" as UTF-16. `idx` is a u32 (up to 10 digits);
    // MAX_STEPS lets it exceed 999, so size both buffers for the full u32 range —
    // `digits[d]` previously overflowed `[0u8; 3]` once idx reached 1000, panicking
    // (→ the no_std loop{} handler hangs the thread under dxgkrnl's adapter lock and
    // deadlocks the whole graphics stack). 'S' + up to 10 digits + NUL = 12.
    let mut name = [0u16; 12];
    name[0] = b'S' as u16;
    let mut digits = [0u8; 10];
    let mut n = idx;
    let mut d = 0usize;
    if n == 0 {
        digits[0] = b'0';
        d = 1;
    } else {
        while n > 0 {
            digits[d] = b'0' + (n % 10) as u8;
            n /= 10;
            d += 1;
        }
    }
    let mut i = 0;
    while i < d {
        name[1 + i] = digits[d - 1 - i] as u16;
        i += 1;
    }
    name[1 + d] = 0;

    // SAFETY: PASSIVE_LEVEL (see module note). Path/ValueName are NUL-terminated
    // UTF-16; ValueData points to a 4-byte DWORD. RtlWriteRegistryValue copies the
    // value, so `code`'s lifetime ending after the call is fine.
    unsafe {
        let _ = RtlWriteRegistryValue(
            RTL_REGISTRY_SERVICES,
            SERVICE_NAME.as_ptr(),
            name.as_ptr(),
            REG_DWORD,
            (&mut code as *mut u32).cast::<core::ffi::c_void>(),
            4,
        );
    }
}
