//! The KMD allocation object's *identity*: the allocation generation, the one
//! epoch that invalidates every generation at once, and nothing else.
//!
//! Normative: `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md`
//!
//! * §10.3:1049 — HWA2 offset 16 is a "nonzero KMD-assigned
//!   stale-validation/diagnostic generation" and is **never an identity lookup
//!   key**. §10.7:1955 says the same of HVM1 offset 16; §10.6:1502 of HOC1
//!   offset 16.
//! * §14:3421 ("Allocation reuse") — a new WDDM allocation object / GPUVA
//!   mapping / NT resource handle **is a new object**; KMD owns a new backing
//!   reference for each allocation generation. Create/open private data is only
//!   an immutable descriptor paired with that object.
//! * §14:3430 ("Adapter reset") — KMD invalidates queue, endpoint/ring,
//!   **allocation**/HPM1, host-batch, native-fence mapping, WSI and flip
//!   generations **together**. The allocation generation is part of that one
//!   coordinated invalidation, not an independent counter.
//! * §15:3516-3520 — HOB1 carries "the expected live allocation generation";
//!   neither it nor HOS1 carries a reusable discovery key. `protocol`'s field is
//!   `HeliosNativeRenderUse::expected_allocation_generation`, and its own doc
//!   states the same rule: "nothing resolves an allocation *from* it".
//!
//! # Why there is no table here, and why that is the requirement
//!
//! §13.3 (`:3398-3406`) forbids a host callback acquiring an adapter-global map,
//! and §3 (`:373-379`) forbids an adapter/process-global resource *discovery*
//! table outright. The two structures this module replaces —
//! `adapter/tracking.rs::VidMmTrackerTable` (an 8192-entry linear scan under a
//! DISPATCH-raising spinlock) and the create-time VidMm attestation it served —
//! were exactly that shape. So the generation is a plain `u64` **stored on the
//! allocation object itself**, reachable directly from `hAllocation`, and this
//! module owns only the minter and the epoch.
//!
//! # The lifetime rules, stated once
//!
//! 1. **Nonzero.** The epoch starts at 1 and occupies the high 32 bits, so a
//!    minted generation can never be 0 — which is what lets `0` keep meaning
//!    "not yet assigned" on the create-*input* side of all three records.
//! 2. **Assigned exactly once per allocation, at create.** `create_one` mints
//!    one value, stamps it into the descriptor it writes back, stores the same
//!    value on the `AllocationContext`, and never writes it again. There is no
//!    re-mint on open: `DxgkDdiOpenAllocation` writes no byte of the private
//!    buffer.
//! 3. **Never an identity lookup key.** Nothing in this module maps a generation
//!    *to* an allocation, and nothing may be added that does.
//! 4. **Survives nothing across an adapter reset.** [`invalidate_all`] bumps the
//!    epoch; every generation minted before it fails [`is_current`] from that
//!    instant, with no enumeration and nothing to scan.
//!
//! Rule 4 is wired at the three adapter-epoch boundaries: StopDevice and
//! RemoveDevice in `ddi/lifecycle.rs`, plus ResetFromTimeout in
//! `ddi/submit_command.rs`. Each call is immediately beside
//! `native_fence::invalidate_all(adapter, boundary)` and precedes device-lost publication, so
//! allocation and native-fence generations change as one capability epoch.
//! Runtime proof still requires `AcGenEpoch` to move on the target; this source
//! wiring alone is not a runtime-correctness claim.

use core::sync::atomic::{AtomicU32, Ordering};

/// The adapter-wide validity epoch, in the high 32 bits of every generation.
///
/// Starts at 1, not 0: a zero epoch would make the first minted generation
/// `0x0000_0000_0000_0001`, which is still nonzero — but it would also make
/// "epoch 0" a legal value, and epoch 0 is the value a zeroed-memory read
/// produces. Starting at 1 keeps "the epoch field is 0" impossible.
static ALLOCATION_EPOCH: AtomicU32 = AtomicU32::new(1);

/// The global monotone ordinal, in the low 32 bits.
///
/// Deliberately **not** reset by [`invalidate_all`]. Resetting it would need the
/// epoch bump and the ordinal store to be one atomic operation for a concurrent
/// mint not to pair a new epoch with a stale ordinal; leaving it monotone makes
/// two generations from different epochs distinct by construction and costs only
/// the 2^32 budget below.
static ALLOCATION_ORDINAL: AtomicU32 = AtomicU32::new(0);

/// Creates refused because the 32-bit ordinal space is exhausted.
///
/// Must read 0. 2^32 allocations at 1000 creates/second is ~49 days of
/// continuous churn on one boot; a nonzero value means either that happened or
/// the minter is being called from somewhere it should not be.
///
/// ⚠ NOT separately published, and deliberately: this is the MINTER's own copy
/// of an event `ddi/create_allocation.rs` already publishes as `AcGenExh`, from
/// `CREATE_GENERATION_EXHAUSTED`, at each of the three admit sites that handle a
/// `None` from [`mint`]. The two are equal by construction, and giving them one
/// registry name would mean two writers racing to the same value while giving
/// them two names would publish one fact twice. This one is the static an
/// ntoseye/TDR symbol read finds in the module that owns the rule.
pub(crate) static GENERATION_EXHAUSTED: AtomicU32 = AtomicU32::new(0);

/// Adapter-reset epoch bumps (`AcGenEpoch`). A nonzero value is target evidence
/// that one of the wired Stop/Remove/TDR epoch boundaries executed.
///
/// Published by `ddi/create_allocation.rs::ALLOC_COUNTERS`, a K4-owned PASSIVE
/// dump site, as a FAILURE entry — so the first create after a reset mirrors it.
/// The module note's earlier claim that no dump site was reachable from this unit
/// was true of the counters it lists but not of this one, and it is closed.
pub(crate) static GENERATION_EPOCH_BUMPS: AtomicU32 = AtomicU32::new(0);

/// Mint the one allocation generation for one allocation.
///
/// `None` means the ordinal space is exhausted; the caller must **fail the
/// create** rather than reuse or wrap a value, because a repeated generation
/// would let a stale HOB1 use record validate against a live allocation
/// (§15:3516-3520). Counted in [`GENERATION_EXHAUSTED`].
///
/// Lock-free, allocation-free, panic-free: legal at any IRQL, though every
/// caller today is the PASSIVE `DxgkDdiCreateAllocation`.
pub(crate) fn mint() -> Option<u64> {
    let mut current = ALLOCATION_ORDINAL.load(Ordering::Relaxed);
    let ordinal = loop {
        if current == u32::MAX {
            GENERATION_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // `compare_exchange_weak` may fail spuriously; the loop re-reads and
        // retries, so a spurious failure costs an iteration and never a wrong
        // value. `fetch_add` cannot be used here: it would wrap past `u32::MAX`
        // and start minting duplicates instead of refusing.
        match ALLOCATION_ORDINAL.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => break current + 1,
            Err(actual) => current = actual,
        }
    };
    let epoch = ALLOCATION_EPOCH.load(Ordering::Acquire);
    Some(pack(epoch, ordinal))
}

/// `(epoch, ordinal)` as the single `u64` the three records carry.
const fn pack(epoch: u32, ordinal: u32) -> u64 {
    ((epoch as u64) << 32) | (ordinal as u64)
}

/// The epoch half of a generation.
pub(crate) const fn epoch_of(generation: u64) -> u32 {
    (generation >> 32) as u32
}

/// Was `generation` minted in the current adapter epoch?
///
/// This is the ONLY comparison anything may make against a generation. It
/// answers "is this value stale", never "which allocation is this" — §10.3:1049
/// and the "⛔ **Never an identity lookup key.**" rule on
/// [`helios_protocol::HeliosWddmAllocationDescV2::allocation_generation`] both
/// forbid the second reading. (Cite the symbol: `:384-387` was that rule's
/// address before the two-stage split and now lands on the echo description.)
///
/// A zero generation is never current: zero is the create-*input* value of all
/// three records, so a caller that reaches here with one has skipped the
/// KMD write-back.
///
/// ⚠ **IMPLEMENTED BUT NEVER EXERCISED.** Its reader is K6: §15:3516-3520 puts
/// the expected live allocation generation in HOB1 and §18.1:4723-4726 makes
/// Render/Patch the place it is checked, and `ddi/native_render.rs` does not
/// exist. Written here, with the minter, because a generation whose staleness
/// test lives in another unit is a generation with two definitions of "current".
#[allow(dead_code)] // reader is K6 (`ddi/native_render.rs`).
pub(crate) fn is_current(generation: u64) -> bool {
    generation != 0 && epoch_of(generation) == ALLOCATION_EPOCH.load(Ordering::Acquire)
}

/// Invalidate every allocation generation minted so far (§14:3430).
///
/// One `fetch_add` with no enumeration: after it returns, [`is_current`] is
/// false for every previously minted value, and a stale HOB1 use record naming
/// one is refused by the check K6 performs rather than by a sweep.
///
/// Saturates instead of wrapping: an epoch that wrapped to a previously used
/// value would silently re-validate that epoch's generations. Saturation freezes
/// the epoch at `u32::MAX`, after which nothing is ever invalidated again — a
/// state that is wrong but *loudly* wrong (`GENERATION_EPOCH_BUMPS` stops
/// moving), unlike a wrap, which is quietly wrong.
///
/// Called only from the Stop/Remove/TDR adapter-epoch boundaries documented in
/// the module header; create/open paths must never invalidate the epoch.
pub(crate) fn invalidate_all() {
    let mut current = ALLOCATION_EPOCH.load(Ordering::Relaxed);
    loop {
        if current == u32::MAX {
            return;
        }
        match ALLOCATION_EPOCH.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                GENERATION_EPOCH_BUMPS.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(actual) => current = actual,
        }
    }
}

/// The live ordinal, for the diagnostic dump only. Not an allocation count:
/// refused creates consume an ordinal too, which is deliberate — a generation is
/// never reused, not even by a create that failed after minting one.
///
/// ⚠ **IMPLEMENTED BUT NEVER EXERCISED.** Unlike [`GENERATION_EPOCH_BUMPS`],
/// which is now published by `ddi/create_allocation.rs::ALLOC_COUNTERS`, this is
/// a FUNCTION and `diag::CounterBlock` takes a `&'static AtomicU32` — so homing
/// it means either exposing [`ALLOCATION_ORDINAL`] directly or writing a bespoke
/// dump. Neither is worth doing for a value whose only consumer is a human
/// reading a diagnostic: it is one `dt` away under ntoseye, and §17.6:4288
/// replaces every registry counter in this driver with ETW in a file that does
/// not exist yet. Left as the named accessor so that whoever writes that file has
/// the value already exposed with its meaning attached.
#[allow(dead_code)] // diagnostic accessor; see the note above.
pub(crate) fn minted_count() -> u32 {
    ALLOCATION_ORDINAL.load(Ordering::Relaxed)
}
