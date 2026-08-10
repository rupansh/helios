//! L4's second file — the resource → **kernel-allocation-identity** table.
//!
//! `KMD_IMPACT.md` §14a.3 UP-4. This is the bookkeeping the D3D12 present path
//! needs and nothing else in this driver has: for a resource that may reach the
//! scanout, the chain
//!
//! ```text
//! HRESOURCE -> ResourceState -> ID3D12Resource* -> VkDeviceMemory
//!           -> pfnAllocateCb{HeliosWddmAllocationDescV2} -> D3DKMT_HANDLE
//! ```
//!
//! ⚠ **The middle link used to be `helios_venus_memory_res_id`, and it is gone.**
//! The chain ran through the ICD's host resource id because the retired create
//! record carried one; HWA2 carries none and no UMD may supply one (§10.3,
//! `docs/retirement/K4-CONTRACT.md` §5). The engine's memory is still what the
//! create *describes* — its extent is HWA2's `byte_size` — but the kernel allocation
//! is no longer the same host object, and joining them is mesa unit **A3**'s work.
//!
//! has to be walkable *after* the create that built it. Every link but the
//! fourth arrow exists on all three sides already (§14a.3); what does not exist
//! is anywhere to *keep* the answer, because [`super::resource12::ResourceState`]
//! is written once inside `pfnCreateHeapAndResource` and read by handle, while
//! `pfnPresent`, `pfnCheckResourceAllocationHandle` and the unwind path all need
//! the same record.
//!
//! ⚠ **No longer inert, and the entry condition changed with it.** At UP-4 nothing
//! consumed this table and every entry's `vk_memory`/`venus_res_id` halves were 0
//! and counted. UP-5 landed the `pfnAllocateCb` call, and the rule is now stronger
//! than "filled in": **an entry exists if and only if this driver owns a WDDM
//! allocation for that resource**. [`record`] is called only after the callback
//! returned a handle, so
//!
//! * `h_allocation != 0` on every recorded entry — a partially-resolved identity is
//!   no longer representable in the table, it is a refusal at the create;
//! * `pfnDestroyHeapAndResource` must `pfnDeallocateCb` exactly the entries
//!   [`take`] hands back, which makes the deallocate unconditional rather than
//!   gated on a second flag that could disagree with the table;
//! * `IdentityRecorded − IdentityRemoved` is therefore both the live-entry count
//!   **and** the live WDDM-allocation count, so the 54th session's leak class is
//!   one subtraction away rather than needing its own instrument.
//!
//! # ⛔ D13: this table is NOT shared private data
//!
//! `DECISIONS.md` D13's refined form: private data that crosses a module
//! boundary is declared once in `helios_protocol`; private data that does not
//! stays in the crate that owns it. **Nothing outside `helios_umd12.dll` reads a
//! byte of this table.** It is process-local bookkeeping that sits *beside* the one
//! record which does cross — `helios_protocol::HeliosWddmAllocationDescV2`, built
//! and validated in `resource12::create_committed_allocation`. So the table is
//! `umd12`-local by D13's own rule, and the record it sits beside is not.
//!
//! ⛔ **And it is not a place to keep what HWA2 refuses to carry.** The retired
//! entry held `venus_res_id`, `venus_alloc_size` and `memory_type_index`; all three
//! are gone, and K4-CONTRACT §5 forbids re-adding them here or anywhere else — "you
//! may not invent a replacement field, stash it elsewhere, or keep the legacy record
//! alive as a side channel".
//!
//! # ⭐ Why a table and not a field on `ResourceState`
//!
//! A field was the first design and it is the better one for *lifetime* — it
//! cannot leak and needs no bound. It was rejected for two reasons that are
//! properties of the D3D12 DDI rather than preferences:
//!
//! 1. **`ResourceState` is written exactly once and never mutated**, and that
//!    single-writer/single-taker property is the whole of
//!    `resource12::heap_state`'s re-derived soundness argument (D13 shares
//!    declarations, not `CUseCountedObject` claims). UP-5 must write back a
//!    `D3DKMT_HANDLE` that only exists *after* `pfnAllocateCb` returns, and UP-6
//!    must read it from `pfnCheckResourceAllocationHandle`; making that field
//!    mutable would put a lock inside the block whose soundness rests on nobody
//!    needing one.
//! 2. **The consumers do not all hold a `D3D12DDI_HRESOURCE`.** `pfnPresent`'s
//!    `_0110` shape was measured handing `hDstResource = NULL` on the WARP
//!    control arm (`KMD_IMPACT.md` §16 U3), and the present identity is built
//!    from an `ID3D12Resource*` the queue lane already has. A table keyed on the
//!    engine pointer answers for both callers; a field answers only for the one
//!    that came in through a handle.
//!
//! # ⛔ The key is an ADDRESS, and it is never dereferenced
//!
//! [`AllocationIdentity::engine_resource`] is `ID3D12Resource::as_raw() as
//! usize`. This module holds **no** COM reference and performs **no** load
//! through that value: it is an identity token compared with `==`, nothing more.
//! The owning reference lives in `ResourceState`, whose box outlives every entry
//! by construction — [`take`] is called from `pfnDestroyHeapAndResource` while
//! that box is still alive, before it is dropped.
//!
//! ⚠ **The hazard an address key has, and how it is closed.** COM object
//! addresses are recycled: the allocator is free to hand a later
//! `ID3D12Resource` the address a released one had. An entry that outlived its
//! resource would then be matched by a *different* resource and would describe
//! the wrong memory — a silent wrong answer, the class this project treats as
//! worse than a failure. Two things close it, and both are counted:
//!
//! * [`take`] on every `pfnDestroyHeapAndResource` whose resource block
//!   resolved, so the normal path leaves nothing behind. `IdentityRecorded`
//!   minus `IdentityRemoved` is the live-entry count and therefore a leak
//!   detector that needs no extra instrument.
//! * [`record`] **overwrites** a colliding key and reports
//!   [`RecordOutcome::Replaced`], which the caller counts as
//!   `IdentityReplaced`. A collision means a destroy failed to remove — so the
//!   stale entry is replaced by the live one (correct) *and* the failure is
//!   visible (loud), instead of the new resource inheriting the old memory.
//!
//! # ⛔ The second collision is RETIRED, and its absence is deliberate
//!
//! ⚠ **This block used to describe `RecordOutcome::ResIdShared`** — a refusal, not a
//! replacement, that fired when two live D3D12 resources shared one `venus_res_id`,
//! because the engine had suballocated them out of one `VkDeviceMemory` and an
//! N-buffered swapchain would then present one surface (the 56th session's *"scanout
//! pinned to ONE resource"* class by a new route). **It is gone with the id it was
//! about.** The table holds no host resource id, so it can see no such collision, and
//! §10.3 forbids it keeping one in order to. The property still matters; the place it
//! is now settled is the kernel's own allocation objects plus mesa unit **A3**.
//!
//! # `ctx_id`, and why the field that *was* absent is now present
//!
//! ⚠ This block used to say `ctx_id` was *"deliberately absent"* because the only
//! value `umd12` could obtain was `HeliosVkd3dDevice::venus_context_id()`, which
//! reads the ICD's **process-global** `helios_current_ctx_id` — and
//! `bridge_icd_anchor.h` says of it: *"evidence only. Never stamp an identity with
//! this value"*, because it is last-writer-wins across instances. That reasoning
//! was right and its conclusion has been discharged rather than overturned: the
//! bridge now also captures `helios_venus_instance_ctx_id` at device create, on the
//! creating thread, and **that** is the value stored here. The forbidden one is
//! still not stamped; both are read so that a disagreement between them is a log
//! line rather than a silent wrong id.
//!
//! ⚠ A `ctx_id` of 0 is legal and counted. It used to travel into
//! `HeliosWddmOpenIdentity::ctx_id`, which that record's own doc called *"diagnostic
//! only"*; that record is retired and HWA2 has **no context field at all**, so the
//! value now stays inside this process and never reaches the kernel. Refusing a
//! create over a diagnostic would be the wrong severity.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The resource geometry the create DDI supplies, verbatim.
///
/// ⚠ Every field is taken from `D3D12DDIARG_CREATERESOURCE_0109` as the runtime
/// gave it (`umd12/bindgen/cached/d3d12umddi.rs:87456-87473`), in the DDI's own
/// widths — not from the `D3D12_RESOURCE_DESC1` this lane *builds* for the
/// engine. The two agree today, and recording the input rather than the
/// translation means a future translation bug shows up as a disagreement instead
/// of being reproduced identically on both sides.
///
/// ⛔ **No row pitch, and that is a statement about THIS struct rather than
/// about the table.** The create DDI carries none — a D3D12 resource's layout is
/// the engine's, obtainable only through `GetCopyableFootprints` — so a pitch
/// here would be a second, unchecked derivation of a number the engine already
/// owns. UP-5 asks the engine instead, and UP-9 needs the answer for
/// `HeliosPresentPrivateData::pitch`, so it is kept as
/// [`AllocationIdentity::pitch`]: outside this struct precisely because it is
/// not one of the DDI's own fields.
#[derive(Clone, Copy)]
pub(crate) struct IdentityGeometry {
    /// `Width` — `UINT64` at the DDI, even for a texture.
    pub(crate) width: u64,
    pub(crate) height: u32,
    pub(crate) depth_or_array_size: u16,
    pub(crate) mip_levels: u16,
    /// `SampleDesc.Count`. A primary is single-sampled, so anything else here on
    /// a recorded entry is itself a finding.
    pub(crate) sample_count: u32,
    /// The creator's exact `DXGI_FORMAT`, which is what
    /// `HeliosWddmAllocationDescV2::dxgi_format` (offset 48) carries and what a
    /// cross-process opener must rebuild with — the lossy `D3DDDIFORMAT` at offset 52
    /// collapses every non-BGRA surface to BGRA, which is why HWA2 carries both.
    pub(crate) dxgi_format: u32,
}

/// One committed resource's WDDM allocation identity.
///
/// `Copy`, and every field is a plain integer: this record holds no COM
/// reference, no pointer it may dereference and nothing with a destructor. That
/// makes copying a record out under the registry lock free of any ownership
/// question.
#[derive(Clone, Copy)]
pub(crate) struct AllocationIdentity {
    /// `ID3D12Resource::as_raw() as usize` — the table key. **Never
    /// dereferenced**; see the module doc.
    pub(crate) engine_resource: usize,
    /// The `VkDeviceMemory` the engine bound the resource's image or buffer to,
    /// as a 64-bit handle. **0 = unresolved** (see the module doc's table).
    pub(crate) vk_memory: u64,
    /// The resource's byte offset within `vk_memory`. ⚠ Non-zero means vkd3d
    /// suballocated, which `create_committed_allocation` refuses: HWA2's `byte_size`
    /// is the whole bound `VkDeviceMemory` and every plane record is bounded against
    /// it, so a resource that does not own its extent cannot be described.
    pub(crate) memory_offset: u64,
    /// The size of the whole `vk_memory` object — its
    /// `VkMemoryAllocateInfo::allocationSize`, *not* the resource's size. It is what
    /// `HeliosWddmAllocationDescV2::byte_size` (offset 24) carries: the *exact backing
    /// extent*, which every plane record is bounded against.
    pub(crate) memory_size: u64,
    /// The KMD-assigned `HeliosWddmAllocationDescV2::allocation_generation` out of
    /// the **validated output descriptor** the create read back.
    ///
    /// ⛔ **Never an identity lookup key** (§10.3, and the field's own doc in
    /// `protocol/src/wddm.rs`): nothing resolves an allocation *from* it. It is a
    /// stale-validation and diagnostic value, kept so that a later descriptor
    /// mismatch has something to be compared against and so the create's evidence
    /// line reports what the kernel assigned rather than what this driver hoped for.
    ///
    /// ⚠ **Nonzero on every recorded entry** — `create_committed_allocation` refuses
    /// and rolls back a create whose write-back left it 0, because an allocation with
    /// no descriptor is one no opener can describe.
    pub(crate) allocation_generation: u64,
    /// The `D3DKMT_HANDLE` `pfnAllocateCb` minted for this resource (UP-5).
    ///
    /// ⭐ **This is the field the whole table exists to hold**, and it is what
    /// `pfnCheckResourceAllocationHandle` (UP-6) and `pfnPresent`'s
    /// `BroadcastSrcAllocation[0]` (UP-7) answer with. Never 0 on a recorded
    /// entry: [`record`] is called only after the callback succeeded, so an entry
    /// exists **iff** this driver owns a WDDM allocation for the resource — which
    /// is also what makes `pfnDeallocateCb` on the destroy path unconditional
    /// rather than conditional on a second flag.
    pub(crate) h_allocation: u32,
    /// The `hKMResource` the same callback returned, or 0.
    ///
    /// ⚠ Recorded but **not** used to deallocate. The wire contract is *either*
    /// `hResource` *or* `NumAllocations`+`HandleList`, never both
    /// (`umd/src/forward/state.rs:90-105`: both together is `E_INVALIDARG`, and
    /// that is a measured 0x80070057 rather than a reading of the header), and
    /// this driver deallocates by handle list. It is kept because it is the only
    /// evidence that dxgkrnl created a kernel *resource* object for the
    /// allocation, which a cross-process opener needs and a shared surface's
    /// absence of would explain.
    pub(crate) h_km_resource: u32,
    /// The runtime's `D3D12DDI_HRTRESOURCE::handle` for this resource, as an
    /// integer.
    ///
    /// ⛔ **An identity token, never dereferenced**, exactly like
    /// [`Self::engine_resource`]. It is kept for one reason: `pfnDeallocateCb`'s
    /// legal `hResource` form needs it at destroy time, and
    /// `pfnDestroyHeapAndResource` is handed only the *driver* handles. Storing it
    /// here rather than in `ResourceState` keeps that block write-once, and it lives
    /// exactly as long as the allocation it releases.
    pub(crate) h_rt_resource: usize,
    /// The venus context id of the `VkInstance` this resource's engine belongs to.
    ///
    /// ⛔ The **instance-scoped** one (`helios_venus_instance_ctx_id`), never the
    /// process-global `helios_venus_current_ctx_id` — see
    /// `bridge12::BridgeDevice12::venus_instance_context_id`. ⚠ It used to be
    /// *stamped into* `HeliosWddmAllocPrivate::ctx_id`; HWA2 has no context field, so
    /// it now stays inside this process. 0 is legal and counted.
    pub(crate) ctx_id: u32,
    pub(crate) geometry: IdentityGeometry,
    /// The engine's row pitch for subresource 0, as `GetCopyableFootprints`
    /// answered it at create time — the value the create put in the descriptor's
    /// plane record (`HeliosWddmAllocationDescV2::planes[0].row_pitch`), kept so any
    /// later consumer uses the **same** number rather than a second derivation.
    ///
    /// ⚠ **Never 0 on a recorded entry, and that is a change.** It used to be
    /// carried unvalidated because nothing on the windowed path read it; HWA2's
    /// shared validator rejects `PlaneRowPitchZero`, so
    /// `create_committed_allocation` refuses a create whose engine declined a pitch
    /// rather than describing a plane it cannot lay out.
    pub(crate) pitch: u32,
    /// The raw `D3D12DDI_HEAP_FLAGS` word the create arrived with.
    ///
    /// ⭐ This keeps the runtime's declaration unreduced. The allocation registry
    /// admits every committed resource because no later shared-resource signal
    /// exists; the raw word still records whether the runtime also declared the
    /// resource a `HEAP_FLAG_PRIMARY` and which other bits it carried.
    pub(crate) heap_flags: u32,
}

/// What [`record`] did, so the caller can count it.
///
/// `#[must_use]`: an ignored outcome is a dropped identity nobody counted, which
/// is the silent-failure shape CLAUDE.md rule 2 forbids.
#[must_use = "every outcome has a named counter; dropping it makes an identity failure silent"]
pub(crate) enum RecordOutcome {
    /// The registry took a new entry.
    Inserted,
    /// An entry for the same `engine_resource` already existed and was
    /// overwritten. ⛔ Means a destroy did not remove one.
    Replaced,
    /// The registry could not reserve storage and the identity was **dropped**.
    RegistryAllocationFailed,
}

/// Process-local allocation identities, indexed both ways required by the
/// contract: resource identity for DDI lookups and venus resource id for the
/// one-resource/one-allocation collision check.
///
/// ⛔ This used to be a 64-entry fixed array because only runtime-declared
/// `PRIMARY` resources were admitted. The deployed runtime sends no such flag:
/// every committed resource must receive its WDDM identity at create time,
/// because the DDI exposes no later shared-resource declaration. Keeping the
/// old bound would make an ordinary application fail on its 65th committed
/// resource. Two hash maps preserve O(1) lookup without an arbitrary resource
/// ceiling; [`record`] uses `try_reserve` so allocation failure is a loud,
/// unwindable result rather than a panic in a DDI.
///
/// ⚠ **The second index is GONE, and its absence is the retirement.** A
/// `by_venus_res_id` map lived here so that two live resources sharing one venus
/// resource id could be refused — the rotation-collapse detector. HWA2 carries no
/// host resource id and this driver no longer obtains one for any purpose
/// (K4-CONTRACT §5), so there is nothing left to index by and nothing left to
/// collide. ⛔ It must not be reintroduced under another name: §10.3's *"no host
/// resource token, resid … or independently usable identity"* forbids exactly this
/// table keeping one. The property it protected — N back buffers must be N distinct
/// allocations — is now a property of the kernel's own allocation objects, and mesa
/// unit **A3** is where the host side of it is settled.
#[derive(Default)]
struct IdentityRegistry {
    by_resource: HashMap<usize, AllocationIdentity>,
}

/// D3D12 DDIs are FREETHREADED (`DDI_REFERENCE.md` §7.1), so create, lookup and
/// destroy share one short critical section. Empty `HashMap`s allocate no
/// storage when `OnceLock` initialises the registry.
static IDENTITIES: OnceLock<Mutex<IdentityRegistry>> = OnceLock::new();

/// Lock the table, ignoring poisoning.
///
/// ⚠ Poisoning cannot occur: both `umd12` profiles set `panic = "abort"`
/// (`umd12/Cargo.toml`), so no unwind can leave the guard poisoned in the first
/// place. `unwrap_or_else(PoisonError::into_inner)` rather than `unwrap()`
/// because a `panic!` in a DDI is a silent graphics deadlock (CLAUDE.md's
/// invariant table) and this crate must not contain a reachable one, even a
/// theoretically unreachable one.
fn identities() -> MutexGuard<'static, IdentityRegistry> {
    IDENTITIES
        .get_or_init(|| Mutex::new(IdentityRegistry::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Record one committed resource's allocation identity.
///
/// Overwrites any entry with the same `engine_resource` — see the module doc's
/// address-recycling argument for why that is the correct direction and not a
/// convenience.
pub(crate) fn record(identity: AllocationIdentity) -> RecordOutcome {
    let mut registry = identities();

    // Reserve before mutating, so allocation failure is a loud, unwindable result
    // rather than a panic in a DDI. Once the reservation succeeds the insertion below
    // cannot allocate.
    if registry.by_resource.try_reserve(1).is_err() {
        return RecordOutcome::RegistryAllocationFailed;
    }

    let replaced = registry
        .by_resource
        .insert(identity.engine_resource, identity);

    if replaced.is_some() {
        RecordOutcome::Replaced
    } else {
        RecordOutcome::Inserted
    }
}

/// The recorded identity for `engine_resource`, if there is one.
///
/// ⚠ Returns a **copy**, taken under the lock and never a reference into the
/// table: the entry may be removed by a concurrent `pfnDestroyHeapAndResource` on
/// another thread the instant the lock is dropped (D3D12 DDIs are FREETHREADED),
/// so a borrow would be a use-after-free waiting for a scheduler. Every consumer
/// wants two integers anyway.
///
/// ⛔ **The copy is a snapshot, and callers must not treat it as a liveness
/// claim.** `pfnCheckResourceAllocationHandle` and `pfnPresent` are both called by
/// the runtime for a resource it is holding, so the resource cannot be destroyed
/// under them — that is the runtime's own object-lifetime guarantee, not something
/// this table provides.
pub(crate) fn lookup(engine_resource: usize) -> Option<AllocationIdentity> {
    identities().by_resource.get(&engine_resource).copied()
}

/// Take the entry for `engine_resource`, if there is one.
///
/// Returns the removed record, which is how the caller distinguishes "this destroy
/// retired an identity" from the ordinary case of destroying a resource that was
/// not created on the committed arm — **and** how it learns which
/// `D3DKMT_HANDLE` to hand `pfnDeallocateCb`.
///
/// ⭐ **Take, not read-then-remove**, and the atomicity is the point: the slot is
/// cleared under the same lock acquisition that reads it, so two concurrent
/// destroys of one resource cannot both come away with the handle and deallocate it
/// twice. A `lookup` + `remove` pair would have that race, and a double
/// `pfnDeallocateCb` is a kernel-handle double free.
pub(crate) fn take(engine_resource: usize) -> Option<AllocationIdentity> {
    identities().by_resource.remove(&engine_resource)
}
