//! Per-object state behind `pDrvPrivate`, and the accessors that reach it.
//!
//! `ResourceState`, `RtvState`, `ResidentAllocation` with its eviction Drop,
//! the allocation-ownership and deallocate-form enums, the OPEN-time HWA2
//! allocation-descriptor reader (K4 — it replaced the tolerant legacy trailer
//! parsers and the `HeliosWddmOpenIdentity` restamp readers), the device/context
//! getters, and the store/load/release helpers for COM handles, resources and
//! RTVs.
//!
//! The typed slot decoding itself lives in [`super::handles`] (T5/R803); this
//! module is the state those slots point AT.
//!
//! Moved verbatim out of `forward.rs` by T8/R1107.

use super::*;

// ⚠ `pub`, not `pub(crate)`, for ONE reason and it is a compiler rule, not a
// design choice: this struct is named as `<H as helios_umd_common::slot::BoxedHandle>::State`
// by the `boxed_handles!` impl in `forward/handles.rs`. `BoxedHandle` became a
// PUBLIC trait of a foreign crate when the slot encoding moved to `umd_common`
// at S1, and E0446 forbids a `pub(crate)` type appearing in a public trait
// impl's associated type. It is NOT a widening in any observable sense: the
// module holding it (`mod state;` / `mod layout;` in `forward.rs`) is private,
// so nothing outside `forward` can name the type, and `helios_umd` is a
// `cdylib` with no library consumers at all.
pub struct ResourceState {
    pub(crate) com_raw: usize,
    /// The same object's `ID3D11Buffer` interface pointer, pre-cast ONCE at
    /// store time; 0 for non-buffer resources. Constant-buffer bind DDIs read
    /// this word instead of a per-call `QueryInterface` + transient
    /// AddRef/Release pair, which profiled as part of an ~8 % render-thread
    /// cost (tmp/handoff-perf-saturation/reports/p1-attribution.md §2).
    /// No reference is held through this field: `com_raw` owns the object,
    /// and a COM interface pointer stays valid while its object lives.
    pub(crate) buffer_raw: usize,
    /// A WDDM allocation is stored only after pfnMakeResidentCb has added one
    /// device-residency reference. Dropping the guard removes exactly that
    /// reference before the allocation is deallocated.
    pub(crate) allocation: Option<ResidentAllocation>,
    /// Package-owned identity of that same WDDM allocation. It is copied into
    /// DXVK/Mesa only during resource creation and resolved back to the current
    /// allocation list entry during Render; neither handle nor pointer is used
    /// as the token.
    pub(crate) outer_allocation: Option<OuterAllocationIdentity>,
    /// Optional CPU view that dxgkrnl uses as pSystemMem for this exact WDDM
    /// allocation. The pointer is data access only and never participates in
    /// token resolution.
    pub(crate) cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
    pub(crate) km_resource: ddi::D3DKMT_HANDLE,
    pub(crate) rt_resource: ddi::HANDLE,
    /// True when this UMD allocated `allocation` itself (pfnAllocateCb in
    /// `create_resource`); false for handles the runtime handed us at
    /// `open_resource`. `release_resource` may only pass allocation handles it
    /// created to pfnDeallocateCb's HandleList form — deallocating opened
    /// handles that way is what returned 0x80070057 and leaked the runtime's
    /// side of the open.
    pub(crate) ownership: AllocationOwnership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OuterAllocationIdentity {
    /// Exact A5/HTS1 generation of the owning device.  Tokens are monotone
    /// only within one device generation, so this field is what makes a
    /// same-process foreign resource distinguishable even when both devices
    /// have assigned token 1.
    pub(crate) device_generation: u64,
    pub(crate) token: u64,
    pub(crate) allocation_generation: u64,
    pub(crate) bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OuterAllocationRefusal {
    ZeroAllocation,
    ZeroGeneration,
    ZeroBytes,
    DuplicateAllocation,
    LiveSetFull,
    TokenOverflow,
    ForeignDeviceGeneration,
    MissingToken,
    TeardownAlreadyPending,
    TeardownNotPending,
}

/// WDDM ownership held after the DDI handle has been invalidated but before
/// DXVK destroys the exact associated VkDeviceMemory.  This record lives in
/// the owning device's bounded allocation set; the token remains resolvable
/// until Mesa has accepted the terminal outer batch.
pub(crate) struct PendingOuterAllocationTeardown {
    identity: OuterAllocationIdentity,
    resident: Option<ResidentAllocation>,
    km_resource: ddi::D3DKMT_HANDLE,
    rt_resource: ddi::HANDLE,
    ownership: AllocationOwnership,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
}

/// WDDM storage created directly for a DXVK-internal VkDeviceMemory. It stays
/// on the exact owning device until DXVK begins destruction of that exact HRA1
/// allocation; no process-global lookup or pointer identity is involved.
pub(crate) struct InternalOuterAllocation {
    identity: OuterAllocationIdentity,
    resident: ResidentAllocation,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
}

pub(crate) fn assign_outer_allocation(
    outer: &crate::device_funcs::OuterDevice,
    allocation: ddi::D3DKMT_HANDLE,
    allocation_generation: u64,
    bytes: u64,
    cpu_mapping: *mut c_void,
) -> Result<(OuterAllocationIdentity, HeliosResourceAssociationV1), OuterAllocationRefusal> {
    use OuterAllocationRefusal as R;

    if allocation == 0 {
        return Err(R::ZeroAllocation);
    }
    if allocation_generation == 0 {
        return Err(R::ZeroGeneration);
    }
    if bytes == 0 {
        return Err(R::ZeroBytes);
    }
    let device_generation = outer.translator.session_generation();
    if device_generation == 0 {
        return Err(R::ForeignDeviceGeneration);
    }
    let mut set = lock_ignore_poison(&outer.allocations);
    if set.entries.len() >= crate::device_funcs::HELIOS_MAX_LIVE_OUTER_ALLOCATIONS {
        return Err(R::LiveSetFull);
    }
    if set
        .entries
        .iter()
        .any(|entry| entry.allocation == allocation)
    {
        return Err(R::DuplicateAllocation);
    }
    let token = set.next_token;
    if token == 0 || token == u64::MAX {
        return Err(R::TokenOverflow);
    }
    set.next_token = token + 1;
    set.entries.push(crate::device_funcs::OuterAllocationState {
        token,
        allocation,
        allocation_generation,
        bytes,
    });
    drop(set);

    let identity = OuterAllocationIdentity {
        device_generation,
        token,
        allocation_generation,
        bytes,
    };
    let association = HeliosResourceAssociationV1 {
        s_type: HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE,
        struct_bytes: HELIOS_RESOURCE_ASSOCIATION_BYTES,
        p_next: core::ptr::null(),
        abi_version: HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION,
        reserved: 0,
        package_generation: HELIOS_PACKAGE_GENERATION,
        device_generation,
        outer_allocation_token: token,
        outer_allocation_bytes: bytes,
        cpu_mapping,
        association_flags: if cpu_mapping.is_null() {
            0
        } else {
            helios_protocol::HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING
        },
        reserved1: 0,
    };
    association
        .validate(HELIOS_PACKAGE_GENERATION, device_generation)
        .map_err(|_| R::ForeignDeviceGeneration)?;
    Ok((identity, association))
}

pub(crate) fn retain_internal_outer_allocation(
    outer: &crate::device_funcs::OuterDevice,
    identity: OuterAllocationIdentity,
    resident: ResidentAllocation,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
) -> Result<
    (),
    (
        OuterAllocationRefusal,
        ResidentAllocation,
        Option<helios_umd_common::cpu_backing::CpuBacking>,
    ),
> {
    use OuterAllocationRefusal as R;

    if identity.device_generation == 0
        || identity.device_generation != outer.translator.session_generation()
    {
        return Err((R::ForeignDeviceGeneration, resident, cpu_backing));
    }
    let mut set = lock_ignore_poison(&outer.allocations);
    let exact_live = set.entries.iter().any(|entry| {
        entry.token == identity.token
            && entry.allocation == resident.handle()
            && entry.allocation_generation == identity.allocation_generation
            && entry.bytes == identity.bytes
    });
    if !exact_live {
        return Err((R::MissingToken, resident, cpu_backing));
    }
    if set.internal_owned.iter().any(|owned| {
        owned.identity.device_generation == identity.device_generation
            && owned.identity.token == identity.token
    }) {
        return Err((R::DuplicateAllocation, resident, cpu_backing));
    }
    set.internal_owned.push(InternalOuterAllocation {
        identity,
        resident,
        cpu_backing,
    });
    Ok(())
}

/// Move either the UMD-owned internal allocation, or an already-armed DDI
/// resource allocation, into the one terminal-batch teardown state.
pub(crate) fn arm_dxvk_outer_allocation_teardown(
    outer: &crate::device_funcs::OuterDevice,
    device_generation: u64,
    token: u64,
) -> Result<(), OuterAllocationRefusal> {
    use OuterAllocationRefusal as R;

    if device_generation == 0 || device_generation != outer.translator.session_generation() {
        return Err(R::ForeignDeviceGeneration);
    }
    if token == 0 {
        return Err(R::MissingToken);
    }

    let mut set = lock_ignore_poison(&outer.allocations);
    if set.pending_teardown.iter().any(|pending| {
        pending.identity.device_generation == device_generation && pending.identity.token == token
    }) {
        return Ok(());
    }
    if set.pending_teardown.len() >= crate::device_funcs::HELIOS_MAX_LIVE_OUTER_ALLOCATIONS {
        return Err(R::LiveSetFull);
    }
    let Some(owned_index) = set.internal_owned.iter().position(|owned| {
        owned.identity.device_generation == device_generation && owned.identity.token == token
    }) else {
        return Err(R::TeardownNotPending);
    };
    let owned = set.internal_owned.swap_remove(owned_index);
    let allocation = owned.resident.handle();
    let exact_live = set.entries.iter().any(|entry| {
        entry.token == owned.identity.token
            && entry.allocation == allocation
            && entry.allocation_generation == owned.identity.allocation_generation
            && entry.bytes == owned.identity.bytes
    });
    if !exact_live {
        set.internal_owned.push(owned);
        return Err(R::MissingToken);
    }
    set.pending_teardown.push(PendingOuterAllocationTeardown {
        identity: owned.identity,
        resident: Some(owned.resident),
        km_resource: 0,
        rt_resource: core::ptr::null_mut(),
        ownership: AllocationOwnership::CreatedByUmd,
        cpu_backing: owned.cpu_backing,
    });
    Ok(())
}

pub(crate) fn remove_outer_allocation(
    outer: &crate::device_funcs::OuterDevice,
    identity: OuterAllocationIdentity,
) -> bool {
    if identity.device_generation == 0
        || identity.device_generation != outer.translator.session_generation()
    {
        return false;
    }
    let mut set = lock_ignore_poison(&outer.allocations);
    if set.pending_teardown.iter().any(|pending| {
        pending.identity.token == identity.token
            && pending.identity.allocation_generation == identity.allocation_generation
    }) {
        return false;
    }
    let before = set.entries.len();
    set.entries.retain(|entry| {
        !(entry.token == identity.token
            && entry.allocation_generation == identity.allocation_generation)
    });
    before != set.entries.len()
}

pub(crate) fn arm_outer_allocation_teardown(
    dev: &crate::device_funcs::HeliosDevice,
    identity: OuterAllocationIdentity,
    resident: Option<ResidentAllocation>,
    km_resource: ddi::D3DKMT_HANDLE,
    rt_resource: ddi::HANDLE,
    ownership: AllocationOwnership,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
) -> Result<
    (),
    (
        OuterAllocationRefusal,
        Option<ResidentAllocation>,
        Option<helios_umd_common::cpu_backing::CpuBacking>,
    ),
> {
    use OuterAllocationRefusal as R;

    if identity.device_generation == 0
        || identity.device_generation != dev.outer.translator.session_generation()
    {
        return Err((R::ForeignDeviceGeneration, resident, cpu_backing));
    }
    let mut set = lock_ignore_poison(&dev.outer.allocations);
    let exact_live = set.entries.iter().any(|entry| {
        entry.token == identity.token
            && entry.allocation
                == resident
                    .as_ref()
                    .map(ResidentAllocation::handle)
                    .unwrap_or(0)
            && entry.allocation_generation == identity.allocation_generation
            && entry.bytes == identity.bytes
    });
    if !exact_live {
        return Err((R::MissingToken, resident, cpu_backing));
    }
    if set.pending_teardown.iter().any(|pending| {
        pending.identity.token == identity.token
            && pending.identity.allocation_generation == identity.allocation_generation
    }) {
        return Err((R::TeardownAlreadyPending, resident, cpu_backing));
    }
    if set.pending_teardown.len() >= crate::device_funcs::HELIOS_MAX_LIVE_OUTER_ALLOCATIONS {
        return Err((R::LiveSetFull, resident, cpu_backing));
    }
    set.pending_teardown.push(PendingOuterAllocationTeardown {
        identity,
        resident,
        km_resource,
        rt_resource,
        ownership,
        cpu_backing,
    });
    Ok(())
}

pub(crate) unsafe fn retire_outer_allocation(
    outer: &crate::device_funcs::OuterDevice,
    device_generation: u64,
    token: u64,
) -> Result<(), OuterAllocationRefusal> {
    use OuterAllocationRefusal as R;

    if device_generation == 0 || device_generation != outer.translator.session_generation() {
        return Err(R::ForeignDeviceGeneration);
    }
    if token == 0 {
        return Err(R::MissingToken);
    }

    let pending = {
        let mut set = lock_ignore_poison(&outer.allocations);
        let pending_index = set
            .pending_teardown
            .iter()
            .position(|pending| {
                pending.identity.device_generation == device_generation
                    && pending.identity.token == token
            })
            .ok_or(R::TeardownNotPending)?;
        let entry_index = set
            .entries
            .iter()
            .position(|entry| {
                entry.token == token
                    && entry.allocation_generation
                        == set.pending_teardown[pending_index]
                            .identity
                            .allocation_generation
            })
            .ok_or(R::MissingToken)?;
        let pending = set.pending_teardown.swap_remove(pending_index);
        set.entries.swap_remove(entry_index);
        pending
    };

    complete_pending_outer_allocation(outer, pending)
}

unsafe fn complete_pending_outer_allocation(
    outer: &crate::device_funcs::OuterDevice,
    pending: PendingOuterAllocationTeardown,
) -> Result<(), OuterAllocationRefusal> {
    use OuterAllocationRefusal as R;

    let token = pending.identity.token;
    let mut cpu_backing = pending.cpu_backing;
    let allocation = pending
        .resident
        .as_ref()
        .map(ResidentAllocation::handle)
        .unwrap_or(0);
    drop(pending.resident);

    let needs_deallocate = allocation != 0 || !pending.rt_resource.is_null();
    if needs_deallocate
        && (outer.kt_callbacks.is_null() || (*outer.kt_callbacks).pfnDeallocateCb.is_none())
    {
        log_error!(
            "DDI outer allocation retire missing pfnDeallocateCb: token={} alloc=0x{:x} rt={:p}",
            token,
            allocation,
            pending.rt_resource
        );
        if let Some(backing) = cpu_backing.take() {
            backing.leak();
        }
        return Err(R::TeardownNotPending);
    }

    if needs_deallocate {
        if let Some(deallocate_cb) = (*outer.kt_callbacks).pfnDeallocateCb {
            let mut allocation = allocation;
            let form = DeallocateForm::select(pending.rt_resource, pending.ownership, allocation);
            let mut dealloc = match form {
                DeallocateForm::ByResource(h_resource) => ddi::D3DDDICB_DEALLOCATE {
                    hResource: h_resource,
                    NumAllocations: 0,
                    HandleList: core::ptr::null_mut(),
                },
                DeallocateForm::ByHandleList(handle) => {
                    allocation = handle.get();
                    ddi::D3DDDICB_DEALLOCATE {
                        hResource: core::ptr::null_mut(),
                        NumAllocations: 1,
                        HandleList: &mut allocation,
                    }
                }
                DeallocateForm::Nothing { reason } => {
                    log_error!(
                        "DDI outer allocation retire skip: {} token={} alloc=0x{:x} km=0x{:x}",
                        reason,
                        token,
                        allocation,
                        pending.km_resource
                    );
                    ddi::D3DDDICB_DEALLOCATE {
                        hResource: core::ptr::null_mut(),
                        NumAllocations: 0,
                        HandleList: core::ptr::null_mut(),
                    }
                }
            };
            if !matches!(form, DeallocateForm::Nothing { .. }) {
                let hr = deallocate_cb(outer.h_rt_device, &mut dealloc);
                log_error!(
                    "DDI outer allocation retired: token={} hr=0x{:08x} alloc=0x{:x} km=0x{:x} rt={:p} owned={}",
                    token,
                    hr as u32,
                    allocation,
                    pending.km_resource,
                    pending.rt_resource,
                    pending.ownership.owns()
                );
                if hr != 0 {
                    if let Some(backing) = cpu_backing.take() {
                        backing.leak();
                    }
                    return Err(R::TeardownNotPending);
                }
            }
        }
    }
    drop(cpu_backing);
    Ok(())
}

/// Last-resort device rundown after the DXVK bridge has been destroyed.  A
/// nonzero result is a named invariant failure: every pending record should
/// have been consumed by the exact allocation destructor callback first.
pub(crate) unsafe fn drain_outer_allocation_teardown(
    outer: &crate::device_funcs::OuterDevice,
) -> (usize, usize) {
    let (mut pending, internal, live) = {
        let mut set = lock_ignore_poison(&outer.allocations);
        let pending = core::mem::take(&mut set.pending_teardown);
        let internal = core::mem::take(&mut set.internal_owned);
        let live = set.entries.len();
        set.entries.clear();
        (pending, internal, live)
    };
    for owned in internal {
        pending.push(PendingOuterAllocationTeardown {
            identity: owned.identity,
            resident: Some(owned.resident),
            km_resource: 0,
            rt_resource: core::ptr::null_mut(),
            ownership: AllocationOwnership::CreatedByUmd,
            cpu_backing: owned.cpu_backing,
        });
    }
    let pending_count = pending.len();
    for teardown in pending {
        if let Err(refusal) = complete_pending_outer_allocation(outer, teardown) {
            log_error!("DDI outer allocation forced rundown REFUSED: {refusal:?}");
        }
    }
    (pending_count, live)
}

/// Who owns the WDDM allocation behind a resource.
///
/// This used to be an unnamed positional `bool` sitting between `km_resource`
/// and `rt_resource` in a seven-argument call, and the deallocate form was
/// reconstructed from `(rt_resource.is_null(), owns_allocation)` at destroy
/// time. R804.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocationOwnership {
    /// This UMD called `pfnAllocateCb` for it, in `create_resource`.
    CreatedByUmd,
    /// The runtime handed us the handle at `open_resource`. Passing these to
    /// `pfnDeallocateCb`'s HandleList form is what returned 0x80070057 and
    /// leaked the runtime's side of the open.
    ///
    /// A3/A7 make this live without supplying a host resource id: the opener
    /// assigns its own HRA1 token to this exact handle, and the KMD resolves the
    /// token from the allocation list when it executes the sealed batch.
    OpenedByRuntime,
}

impl AllocationOwnership {
    /// The value the `owned=` log key has always carried. Kept so the
    /// `DDI deallocate_resource:` line stays byte-identical across R804.
    pub(crate) fn owns(self) -> bool {
        matches!(self, Self::CreatedByUmd)
    }
}

/// The one legal shape of a `D3DDDICB_DEALLOCATE` call.
///
/// The wire contract is three-way: EITHER `hResource` alone, OR
/// `NumAllocations`+`HandleList` with `hResource` NULL. Both together is
/// E_INVALIDARG -- the old 0x80070057, which also leaked opened resources.
/// Constructing this enum is the only way a `D3DDDICB_DEALLOCATE` is built, so
/// the both-set combination is unrepresentable rather than merely avoided.
///
/// `Nothing` must still exist: it is the (currently unreachable) case of no
/// runtime resource and an allocation we did not create. The guarantee is that
/// it is named and logged, not that it is gone.
pub(crate) enum DeallocateForm {
    ByResource(ddi::HANDLE),
    ByHandleList(core::num::NonZeroU32),
    Nothing { reason: &'static str },
}

impl DeallocateForm {
    /// The single exhaustive decision. Order matters and is the pre-R804
    /// behaviour exactly: a runtime resource handle wins over ownership,
    /// because the runtime then releases every allocation it tracks for the
    /// resource -- created AND opened instances.
    pub(crate) fn select(
        rt_resource: ddi::HANDLE,
        ownership: AllocationOwnership,
        allocation: ddi::D3DKMT_HANDLE,
    ) -> Self {
        if !rt_resource.is_null() {
            return Self::ByResource(rt_resource);
        }
        match (ownership, core::num::NonZeroU32::new(allocation)) {
            (AllocationOwnership::CreatedByUmd, Some(a)) => Self::ByHandleList(a),
            (AllocationOwnership::CreatedByUmd, None) => Self::Nothing {
                reason: "owner but no allocation handle",
            },
            (AllocationOwnership::OpenedByRuntime, _) => Self::Nothing {
                reason: "not owner",
            },
        }
    }
}

pub(crate) type EvictCallback =
    unsafe extern "C" fn(ddi::HANDLE, *mut ddi::D3DDDICB_EVICT) -> ddi::HRESULT;

/// One persistent WDDM 2.x device-residency reference.
///
/// This type is deliberately non-Clone and non-Copy: every successful
/// pfnMakeResidentCb call creates exactly one guard, and moving/dropping that
/// guard is the only way the reference can change ownership or be evicted.
pub(crate) struct ResidentAllocation {
    pub(crate) handle: core::num::NonZeroU32,
    pub(crate) h_rt_device: ddi::HANDLE,
    pub(crate) evict_cb: EvictCallback,
}

impl ResidentAllocation {
    pub(crate) fn handle(&self) -> ddi::D3DKMT_HANDLE {
        self.handle.get()
    }
}

impl Drop for ResidentAllocation {
    fn drop(&mut self) {
        let handle = self.handle.get();
        let mut evict = ddi::D3DDDICB_EVICT::default();
        evict.NumAllocations = 1;
        evict.AllocationList = &handle;
        let hr = unsafe { (self.evict_cb)(self.h_rt_device, &mut evict) };
        trace_line!(
            "WDDM residency: Evict alloc=0x{:x} hr=0x{:08x}",
            handle,
            hr as u32
        );
        if hr != 0 {
            log_error!(
                "WDDM residency: Evict FAILED alloc=0x{:x} hr=0x{:08x}",
                handle,
                hr as u32
            );
        }
    }
}

// ⚠ `pub`, not `pub(crate)`, for ONE reason and it is a compiler rule, not a
// design choice: this struct is named as `<H as helios_umd_common::slot::BoxedHandle>::State`
// by the `boxed_handles!` impl in `forward/handles.rs`. `BoxedHandle` became a
// PUBLIC trait of a foreign crate when the slot encoding moved to `umd_common`
// at S1, and E0446 forbids a `pub(crate)` type appearing in a public trait
// impl's associated type. It is NOT a widening in any observable sense: the
// module holding it (`mod state;` / `mod layout;` in `forward.rs`) is private,
// so nothing outside `forward` can name the type, and `helios_umd` is a
// `cdylib` with no library consumers at all.
pub struct RtvState {
    pub(crate) com_raw: usize,
    /// Non-owning resource pointer; the RTV itself keeps the resource alive.
    pub(crate) resource_raw: usize,
    pub(crate) allocation: ddi::D3DKMT_HANDLE,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: u32,
}

// ⛔ K4: `RuntimeAllocPrivate` (the 96-byte `HeliosWddmAllocPrivate` +
// `HeliosWddmAllocMeta` pair this driver used to send into `pfnAllocateCb`) is
// DELETED. The create-time record is one `HeliosWddmAllocationDescV2`, built by
// `alloc::Hwa2CreateInput::build`. It had no `const _` layout assertion pinning
// the two halves' adjacency either, which HWA2 does not need: it is a single
// `#[repr(C)]` protocol struct whose every offset is asserted in
// `protocol/src/wddm.rs`.

#[inline]
pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// The three present-path debug knobs, read ONCE per process.
///
/// Each was an uncached `GetEnvironmentVariableW` plus an `OsString`
/// allocation on every present: two from the debug hooks and one for
/// `bOptimizeForComposition`. OBSERVABLE CHANGE, stated deliberately: these
/// become read-once-per-process, so setting them on a live process no longer
/// takes effect — they must be set before the process starts, like every
/// registry knob in `lib.rs`, all of which already use `OnceLock`.
///
/// Cost honesty (the review asks for it): three env reads per present against
/// a 10 ms frame gate is not where the present path spends its time. This is
/// removing avoidable per-frame work, not a measured win.
pub(crate) fn present_readback_enabled() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_flag("HELIOS_PRESENT_READBACK"))
}

pub(crate) fn present_force_opaque_enabled() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_flag("HELIOS_PRESENT_FORCE_OPAQUE"))
}

pub(crate) fn present_optimize_composition_enabled() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_flag("HELIOS_PRESENT_OPTIMIZE_COMPOSITION"))
}

// ⛔⛔ K4 DELETED four things that used to live here, and none of them may come
// back under another name:
//
//  * `StandardAllocMetaV1` / `StandardAllocMetaV2` and `read_alloc_meta` — the
//    TOLERANT trailer parser that accepted a 16-, 24- or 48-byte trailer and
//    zero-extended it. HWA2 is exact-length by construction
//    (`from_private_data` requires `bytes.len() == HELIOS_HWA2_BYTES`, not
//    `>=`) because §10.3 says a malformed or truncated descriptor "makes
//    create/open fail; it never selects a legacy parser". A tolerant parser IS
//    a legacy parser.
//  * `OpenedAllocation`, `read_open_identity`, `read_opened_allocation` — the
//    readers of the KMD's open-time `HeliosWddmOpenIdentity` restamp. ⛔ The
//    whole open-time restamp EXPECTATION is gone with them: the KMD no longer
//    writes any byte of the private buffer at `DxgkDdiOpenAllocation` (K4
//    acceptance obligation 5), the descriptor is written once at create and is
//    `const` for every opener. An opener validates and reads; it never waits
//    for a kernel write and never distinguishes "identity present, trailer
//    absent".
//
// The successor is one function, below.

/// Read and validate the OPEN-time HWA2 descriptor out of one
/// `(pPrivateDriverData, PrivateDriverDataSize)` pair.
///
/// Two-stage on purpose, and the stages are separately countable at the call
/// site: a buffer of the wrong LENGTH is a different fact from a 168-byte
/// buffer whose CONTENT is invalid. The old reader collapsed both (and a third,
/// "no identity at all") into `None`, so `open_resource`'s refusal could not say
/// which had happened.
///
/// # Safety
///
/// `ptr` must be null or point to at least `size` readable bytes — i.e. exactly
/// the contract dxgkrnl states for the pair. Nothing beyond `size` is read: the
/// slice is built with the runtime's own length and
/// [`HeliosWddmAllocationDescV2::from_private_data`] does the exact-length
/// check, so a short buffer is refused rather than over-read. That is the
/// out-of-bounds read the constructor's doc exists to prevent, and it is why
/// this never casts the pointer to the struct type.
pub(crate) unsafe fn read_open_descriptor(
    ptr: *const c_void,
    size: u32,
) -> Result<HeliosWddmAllocationDescV2, HeliosAllocDescRejection> {
    if ptr.is_null() {
        return Err(HeliosAllocDescRejection::PrivateDataSize {
            found: 0,
            expected: HELIOS_HWA2_BYTES as usize,
        });
    }
    // SAFETY: the caller's contract above — `ptr` is readable for `size` bytes.
    // `size` is a `u32`, so the length can never exceed `isize::MAX` and the
    // slice cannot wrap the address space.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, size as usize) };
    let desc = HeliosWddmAllocationDescV2::from_private_data(bytes)?;
    // The OUTPUT/open-time validator: `validate` requires a nonzero
    // KMD-assigned `allocation_generation`, which is exactly what distinguishes
    // a descriptor the kernel has stamped from a create-input request.
    desc.validate(HELIOS_PACKAGE_GENERATION)?;
    Ok(desc)
}

// --- handle <-> COM helpers -------------------------------------------------

/// The two object kinds that can live behind an HDEVICE, discriminated by
/// the tag word every resolver reads before casting.
pub(crate) enum DrvHandle<'a> {
    Device(&'a HeliosDevice),
    Deferred(&'a crate::device_funcs::HeliosDeferredContext),
}

/// Discriminate an HDEVICE. A block carrying neither tag is refused loudly,
/// never cast — that wild cast is the worst failure of the DC feature.
pub(crate) unsafe fn drv_handle<'a>(h: Hdevice) -> Option<DrvHandle<'a>> {
    let p = h.pDrvPrivate;
    if p.is_null() {
        return None;
    }
    match *(p as *const usize) {
        crate::device_funcs::HELIOS_TAG_DEVICE => {
            Some(DrvHandle::Device(&*(p as *const HeliosDevice)))
        }
        crate::device_funcs::HELIOS_TAG_DEFERRED => Some(DrvHandle::Deferred(
            &*(p as *const crate::device_funcs::HeliosDeferredContext),
        )),
        tag => {
            crate::device_funcs::note_device_tag_mismatch("drv_handle", tag);
            None
        }
    }
}

pub(crate) unsafe fn d3d11_device(h: Hdevice) -> Option<ManuallyDrop<ID3D11Device>> {
    // Borrowed, not adopted: the bridge keeps the owning reference. The
    // ManuallyDrop lives inside the wrapper now, so this cannot be written as
    // an adopting `from_raw` by a future edit. R813.
    helios_device(h)?.dxvk.d3d11_device()
}

/// Borrow the `HeliosDevice` behind an HDEVICE handle. Does not take
/// ownership. Tag-discriminated: an HDEVICE can be a deferred context — this
/// resolver then returns the DC's PARENT device, so the ~66 forwarder sites
/// that need device-scoped machinery (COM device, caches, kernel callbacks)
/// keep working on both handle kinds.
pub(crate) unsafe fn helios_device<'a>(h: Hdevice) -> Option<&'a HeliosDevice> {
    match drv_handle(h)? {
        DrvHandle::Device(dev) => Some(dev),
        // SAFETY: `parent` is written once at DC construction from a live
        // device handle, and the runtime destroys every DC before its device.
        DrvHandle::Deferred(dc) => Some(&*dc.parent),
    }
}

/// The binding shadow of the context this handle records on: the device's
/// immediate-context copy, or the DC's own.
pub(crate) unsafe fn ctx_bindings<'a>(h: Hdevice) -> Option<&'a crate::device_funcs::CtxBindings> {
    match drv_handle(h)? {
        DrvHandle::Device(dev) => Some(&dev.owned.bindings),
        DrvHandle::Deferred(dc) => Some(&dc.bindings),
    }
}

/// Report an error to the D3D11 runtime for a VOID-returning DDI via the
/// corelayer `pfnSetErrorCb`. The runtime fails the API call that invoked the
/// DDI (e.g. `OpenSharedResource`), which is the contractual way for
/// `open_resource`/`create_*` to fail loudly instead of leaving a null handle
/// the runtime will dereference.
///
/// Tag-aware: an error on a deferred context goes to the DC's OWN corelayer
/// (`hRTCoreLayer` + callbacks from `D3D11DDIARG_CREATEDEFERREDCONTEXT`), per
/// the WDK contract — never to the parent device's.
pub(crate) unsafe fn set_runtime_error(h: Hdevice, hr: i32) {
    let (core_layer, um_callbacks) = match drv_handle(h) {
        Some(DrvHandle::Device(dev)) => (dev.h_rt_core_layer, dev.um_callbacks),
        Some(DrvHandle::Deferred(dc)) => (dc.dc_core_layer, dc.dc_um_callbacks),
        None => return,
    };
    if um_callbacks.is_null() {
        log_error!("set_runtime_error: no corelayer callbacks");
        return;
    }
    // pfnSetErrorCb is the first member of every D3D11DDI_CORELAYER_DEVICECALLBACKS
    // revision, so reading it through the 11.0 layout is version-independent.
    let cb = &*(um_callbacks as *const ddi::D3D11DDI_CORELAYER_DEVICECALLBACKS);
    if let Some(f) = cb.pfnSetErrorCb {
        f(ddi::D3D10DDI_HRTCORELAYER { handle: core_layer }, hr);
    }
}

/// The D3D11 context this handle records on, borrowed: the bridge's immediate
/// context for a device handle, the DC's own DXVK deferred COM context for a
/// deferred-context handle. Every context forwarder resolves through this, so
/// one dispatch serves both tables.
pub(crate) unsafe fn d3d11_context(h: Hdevice) -> Option<ManuallyDrop<ID3D11DeviceContext>> {
    match drv_handle(h)? {
        DrvHandle::Device(dev) => dev.dxvk.d3d11_context(),
        DrvHandle::Deferred(dc) => {
            let raw = dc.dc.as_ref()?.as_raw();
            // SAFETY: `dc.dc` owns the reference for the DC's whole life;
            // ManuallyDrop borrows it without taking one.
            Some(ManuallyDrop::new(ID3D11DeviceContext::from_raw(raw)))
        }
    }
}

/// The immediate context as `ID3D11DeviceContext1`, borrowed.
///
/// The bridge's immediate context is DXVK's `D3D11ImmediateContext`, whose
/// `QueryInterface` returns `this` for every `ID3D11DeviceContext{,1..4}`
/// IID (dxvk-helios `d3d11_context.cpp:87-96`) — one vtable serves the whole
/// chain, so the same pointer IS the derived interface. The previous per-call
/// `.cast()` was a QI + transient AddRef/Release on the hot
/// `SetConstantBuffers1` family (p1-attribution §2).
pub(crate) unsafe fn d3d11_context1(h: Hdevice) -> Option<ManuallyDrop<ID3D11DeviceContext1>> {
    let context = d3d11_context(h)?;
    // SAFETY: same object, same vtable (see above); ManuallyDrop keeps this a
    // borrow — the bridge holds the owning reference.
    Some(ManuallyDrop::new(ID3D11DeviceContext1::from_raw(
        context.as_raw(),
    )))
}

/// As [`d3d11_context1`], for the tiled-resource DDIs.
pub(crate) unsafe fn d3d11_context2(h: Hdevice) -> Option<ManuallyDrop<ID3D11DeviceContext2>> {
    let context = d3d11_context(h)?;
    // SAFETY: as d3d11_context1 — the DXVK QI covers ID3D11DeviceContext2.
    Some(ManuallyDrop::new(ID3D11DeviceContext2::from_raw(
        context.as_raw(),
    )))
}

pub(crate) unsafe fn d3d11_device2(h: Hdevice) -> Option<ID3D11Device2> {
    let device = d3d11_device(h)?;
    (*device).cast::<ID3D11Device2>().ok()
}

/// Store a COM interface's raw pointer (ownership transferred) in a DDI handle.
///
/// The null check is new: this was the one writer of the three that lacked it,
/// so a null slot wrote through a null pointer where `store_raw_com` and
/// `clear_handle` returned quietly. R803.
pub(crate) unsafe fn store_com<T: Interface>(h: impl ComHandle, obj: T) {
    match Slot::<Com<T>>::from_priv(h.drv_private()) {
        Some(slot) => slot.store(obj),
        // Dropping `obj` here releases the reference we were asked to hand to
        // the runtime. That is correct: with no slot to put it in, the
        // alternative is leaking it.
        None => drop(obj),
    }
}

/// Map a DXVK create failure onto an HRESULT the invoking API is documented to
/// return. Every `Create*` DDI this is used from documents exactly
/// `E_OUTOFMEMORY` and `E_INVALIDARG`; an HRESULT outside that set is itself
/// logged by the runtime as a driver bug, so a DXVK code outside it is
/// substituted rather than passed through.
pub(crate) fn create_error_hr(e: &windows::core::Error) -> i32 {
    match e.code().0 {
        hr @ (E_OUTOFMEMORY | E_INVALIDARG) => hr,
        _ => E_OUTOFMEMORY,
    }
}

/// The only way a VOID-returning `Create*` DDI may leave its handle slot:
/// either `store` runs, or the runtime is told the invoking API call failed.
///
/// The DDI has no return value, so `pfnSetErrorCb` is the sole channel through
/// which a failed create becomes a failed `CreateRasterizerState` /
/// `CreateRenderTargetView` / `CreateTexture2D` instead of an S_OK with a null
/// driver handle that the app then binds to nothing.
///
/// `result` is the DXVK call's `Result<()>` and `obj` its out-param: S_OK with
/// no object is as much a fake success as an error HRESULT, so both report.
/// Panic-free by construction (no `unwrap`, no indexing) — these are
/// `extern "C"` entry points under `panic = "abort"`.
pub(crate) unsafe fn finish_create<T: Interface>(
    h: Hdevice,
    result: windows::core::Result<()>,
    obj: Option<T>,
    store: impl FnOnce(T),
) {
    match result {
        Ok(()) => match obj {
            Some(o) => store(o),
            None => set_runtime_error(h, E_OUTOFMEMORY),
        },
        Err(e) => set_runtime_error(h, create_error_hr(&e)),
    }
}

pub(crate) unsafe fn store_raw_com(h: impl ComHandle, raw: usize) {
    if let Some(slot) = Slot::<Com<IUnknown>>::from_priv(h.drv_private()) {
        slot.store_raw(raw);
    }
}

/// Null a handle's slot. Payload-agnostic on purpose -- this writes the word
/// and never touches what it pointed at -- so it takes any `DdiHandle`,
/// including the boxed-payload ones. Every `Create*`/`Open*` DDI calls it on
/// entry so a failure leaves a null handle rather than stale garbage.
pub(crate) unsafe fn clear_handle(h: impl DdiHandle) {
    if let Some(slot) = Slot::<Com<IUnknown>>::from_priv(h.drv_private()) {
        slot.clear();
    }
}

/// The raw word behind a bare-COM DDI handle (0 when absent).
///
/// Correct for the shader/DSV/state slots it is used on, and silently the
/// `Box` pointer if ever applied to a resource or RTV slot -- which is exactly
/// the confusion `Slot<P>` exists to remove. Retained with its old signature so
/// this commit churns no call sites; the per-family conversions replace each
/// use with `Slot::<Com<T>>::word()` on a typed handle.
pub(crate) unsafe fn handle_com_raw(h: impl ComHandle) -> usize {
    handle_com_raw_at(h.drv_private())
}

/// `handle_com_raw` for the runtime-tag dispatches, which receive a bare
/// `pDrvPrivate` whose payload is selected by a `D3D11DDI_HANDLETYPE` value at
/// run time and so cannot be keyed on a static handle type. Callers must be an
/// arm of such a dispatch that has already matched a bare-COM tag.
pub(crate) unsafe fn handle_com_raw_at(handle_priv: *mut c_void) -> usize {
    match Slot::<Com<IUnknown>>::from_priv(handle_priv) {
        Some(slot) => slot.word(),
        None => 0,
    }
}

pub(crate) unsafe fn store_resource(
    h_res: ddi::D3D10DDI_HRESOURCE,
    obj: ID3D11Resource,
    allocation: Option<ResidentAllocation>,
    outer_allocation: Option<OuterAllocationIdentity>,
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
    km_resource: ddi::D3DKMT_HANDLE,
    rt_resource: ddi::HANDLE,
    ownership: AllocationOwnership,
) {
    let Some(slot) = boxed_slot(h_res) else {
        drop(obj);
        return;
    };
    // One QI here instead of one per bind call. The temporary's release is
    // fine: `com_raw` keeps the object alive, which keeps the pointer valid.
    let buffer_raw = obj
        .cast::<ID3D11Buffer>()
        .map(|b| b.as_raw() as usize)
        .unwrap_or(0);
    slot.store(ResourceState {
        com_raw: obj.into_raw() as usize,
        buffer_raw,
        allocation,
        outer_allocation,
        cpu_backing,
        km_resource,
        rt_resource,
        ownership,
    });
}

pub(crate) unsafe fn load_resource(
    h_res: ddi::D3D10DDI_HRESOURCE,
) -> Option<ManuallyDrop<ID3D11Resource>> {
    load_resource_at(h_res.pDrvPrivate)
}

/// `load_resource` for the runtime-tag dispatches (`Discard`, the tiled-resource
/// barrier), which receive a bare `pDrvPrivate` whose payload is selected by a
/// `D3D11DDI_HANDLETYPE` value at run time and so cannot be keyed on a static
/// handle type. Callers must be an arm of such a dispatch that has already
/// matched `HT_RESOURCE`. Same contract as `handle_com_raw_at`/`load_com_at`.
pub(crate) unsafe fn load_resource_at(
    handle_priv: *mut c_void,
) -> Option<ManuallyDrop<ID3D11Resource>> {
    let state = resource_state_at(handle_priv)?;
    if state.com_raw == 0 {
        return None;
    }
    Some(ManuallyDrop::new(ID3D11Resource::from_raw(
        state.com_raw as *mut c_void,
    )))
}

/// The `ResourceState` behind a DDI resource handle, or `None` for an empty
/// slot. The single place the resource slot is decoded; every reader below
/// goes through it instead of repeating the two-step null dance.
///
/// Taking the handle rather than its `pDrvPrivate` is what makes the payload
/// follow from the handle's type: `resource_state(h_rtv)` does not resolve.
pub(crate) unsafe fn resource_state(
    h_res: ddi::D3D10DDI_HRESOURCE,
) -> Option<&'static ResourceState> {
    boxed_slot(h_res)?.get()
}

/// `resource_state` for the runtime-tag dispatches. See `load_resource_at`.
pub(crate) unsafe fn resource_state_at(handle_priv: *mut c_void) -> Option<&'static ResourceState> {
    Slot::<Boxed<ResourceState>>::from_priv(handle_priv)?.get()
}

/// Raw ID3D11Resource COM pointer behind a DDI resource handle (0 when
/// absent) — for bridge calls that inspect the DXVK image without taking a COM
/// reference.
pub(crate) unsafe fn resource_com_raw(h_res: ddi::D3D10DDI_HRESOURCE) -> usize {
    resource_state(h_res).map_or(0, |s| s.com_raw)
}

pub(crate) unsafe fn resource_allocation(h_res: ddi::D3D10DDI_HRESOURCE) -> ddi::D3DKMT_HANDLE {
    resource_state(h_res).map_or(0, |s| {
        s.allocation
            .as_ref()
            .map(ResidentAllocation::handle)
            .unwrap_or(0)
    })
}

pub(crate) unsafe fn resource_parent_handles(
    h_res: ddi::D3D10DDI_HRESOURCE,
) -> (ddi::HANDLE, ddi::D3DKMT_HANDLE) {
    resource_state(h_res).map_or((core::ptr::null_mut(), 0), |s| {
        (s.rt_resource, s.km_resource)
    })
}

pub(crate) unsafe fn resource_dimensions(h_res: ddi::D3D10DDI_HRESOURCE) -> (u32, u32) {
    let Some(res) = load_resource(h_res) else {
        return (0, 0);
    };
    let Ok(tex) = (*res).cast::<ID3D11Texture2D>() else {
        return (0, 0);
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut desc);
    (desc.Width, desc.Height)
}

pub(crate) unsafe fn resource_sample_count(h_res: ddi::D3D10DDI_HRESOURCE) -> u32 {
    let Some(res) = load_resource(h_res) else {
        return 1;
    };
    let Ok(tex) = (*res).cast::<ID3D11Texture2D>() else {
        return 1;
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut desc);
    desc.SampleDesc.Count.max(1)
}

/// Which 1D view shape an array size selects.
///
/// One of the two decisions that the four view-descriptor translators each
/// wrote out by hand. Purpose-scoped rather than one big `ViewShape` enum so
/// each translator's `match` is exhaustive with no catch-all: adding a shape
/// here is a compile error in every translator, which is the whole point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tex1DShape {
    Plain,
    Array,
}

/// Which 2D view shape an (array size, sample count) pair selects.
///
/// This is the fork that was written out THREE times — RTV, DSV and SRV each
/// had `if is_msaa && ArraySize > 1 / else if is_msaa / else if ArraySize > 1
/// / else` — and the one where the wrong answer silently mis-binds a Fire
/// Strike MSAA render target rather than failing. RTV, DSV and SRV can no
/// longer disagree about which shape a resource is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tex2DShape {
    Plain,
    Array,
    Ms,
    MsArray,
}

pub(crate) const fn tex1d_shape(array_size: u32) -> Tex1DShape {
    if array_size > 1 {
        Tex1DShape::Array
    } else {
        Tex1DShape::Plain
    }
}

/// `sample_count` is the resource's, from [`resource_sample_count`], which
/// clamps to 1 for a non-texture or unloadable handle.
///
/// The evaluation order is load-bearing and matches all three originals: MSAA
/// wins over array-ness, so a multisampled array is `MsArray` and NOT
/// `Array`.
pub(crate) const fn tex2d_shape(array_size: u32, sample_count: u32) -> Tex2DShape {
    match (sample_count > 1, array_size > 1) {
        (true, true) => Tex2DShape::MsArray,
        (true, false) => Tex2DShape::Ms,
        (false, true) => Tex2DShape::Array,
        (false, false) => Tex2DShape::Plain,
    }
}

/// The sample count a translator passes to [`tex2d_shape`] when the view kind
/// has no multisampled form at all.
///
/// D3D11 has no multisampled UAV — there is no
/// `D3D11_UAV_DIMENSION_TEXTURE2DMS` — so `uav_desc` never consulted the
/// resource's sample count and must keep not consulting it. Naming the 1 says
/// that is deliberate rather than a missing `resource_sample_count` call.
pub(crate) const NO_MULTISAMPLED_FORM: u32 = 1;

pub(crate) unsafe fn resource_dxgi_format(h_res: ddi::D3D10DDI_HRESOURCE) -> DXGI_FORMAT {
    let Some(res) = load_resource(h_res) else {
        return DXGI_FORMAT(0);
    };
    let Ok(tex) = (*res).cast::<ID3D11Texture2D>() else {
        return DXGI_FORMAT(0);
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut desc);
    desc.Format
}

pub(crate) unsafe fn resource_summary(
    h_res: ddi::D3D10DDI_HRESOURCE,
) -> (ddi::D3DKMT_HANDLE, &'static str, u32, u32, u32, u32) {
    let allocation = resource_allocation(h_res);
    let Some(res) = load_resource(h_res) else {
        return (allocation, "missing", 0, 0, 0, 0);
    };
    if let Ok(buf) = (*res).cast::<ID3D11Buffer>() {
        let mut desc = D3D11_BUFFER_DESC::default();
        buf.GetDesc(&mut desc);
        return (allocation, "buffer", desc.ByteWidth, 1, 1, 0);
    }
    if let Ok(tex) = (*res).cast::<ID3D11Texture2D>() {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);
        return (
            allocation,
            "tex2d",
            desc.Width,
            desc.Height,
            desc.ArraySize,
            desc.Format.0 as u32,
        );
    }
    if let Ok(tex) = (*res).cast::<ID3D11Texture3D>() {
        let mut desc = D3D11_TEXTURE3D_DESC::default();
        tex.GetDesc(&mut desc);
        return (
            allocation,
            "tex3d",
            desc.Width,
            desc.Height,
            desc.Depth,
            desc.Format.0 as u32,
        );
    }
    (allocation, "resource", 0, 0, 0, 0)
}

pub(crate) unsafe fn store_rtv(
    h_rtv: ddi::D3D10DDI_HRENDERTARGETVIEW,
    obj: ID3D11RenderTargetView,
    resource_raw: usize,
    allocation: ddi::D3DKMT_HANDLE,
    width: u32,
    height: u32,
    format: u32,
) {
    let Some(slot) = boxed_slot(h_rtv) else {
        drop(obj);
        return;
    };
    slot.store(RtvState {
        com_raw: obj.into_raw() as usize,
        resource_raw,
        allocation,
        width,
        height,
        format,
    });
}

/// The `RtvState` behind a DDI render-target-view handle, or `None` for an
/// empty slot. The single place the RTV slot is decoded.
pub(crate) unsafe fn rtv_state(
    h_rtv: ddi::D3D10DDI_HRENDERTARGETVIEW,
) -> Option<&'static RtvState> {
    boxed_slot(h_rtv)?.get()
}

/// `rtv_state` for the runtime-tag dispatches. See `load_resource_at`.
pub(crate) unsafe fn rtv_state_at(handle_priv: *mut c_void) -> Option<&'static RtvState> {
    Slot::<Boxed<RtvState>>::from_priv(handle_priv)?.get()
}

pub(crate) unsafe fn load_rtv(
    h_rtv: ddi::D3D10DDI_HRENDERTARGETVIEW,
) -> Option<ManuallyDrop<ID3D11RenderTargetView>> {
    load_rtv_at(h_rtv.pDrvPrivate)
}

/// `load_rtv` for the runtime-tag dispatches (`Discard`, `ClearView`, the
/// tiled-resource barrier). See `load_resource_at`.
pub(crate) unsafe fn load_rtv_at(
    handle_priv: *mut c_void,
) -> Option<ManuallyDrop<ID3D11RenderTargetView>> {
    let state = rtv_state_at(handle_priv)?;
    if state.com_raw == 0 {
        return None;
    }
    Some(ManuallyDrop::new(ID3D11RenderTargetView::from_raw(
        state.com_raw as *mut c_void,
    )))
}

/// Geometry + identity of the RTV behind a handle, all zeros when absent.
///
/// `clear_rtv` used to re-derive these by casting the slot inline, duplicating
/// this function's body four lines from a `load_rtv` call on the same handle.
/// That copy is gone; the log line it feeds is unchanged. R803.
pub(crate) unsafe fn rtv_info(
    h_rtv: ddi::D3D10DDI_HRENDERTARGETVIEW,
) -> (ddi::D3DKMT_HANDLE, u32, u32, u32, usize) {
    rtv_state(h_rtv).map_or((0, 0, 0, 0, 0), |s| {
        (s.allocation, s.width, s.height, s.format, s.resource_raw)
    })
}

pub(crate) unsafe fn release_rtv(h_rtv: ddi::D3D10DDI_HRENDERTARGETVIEW) {
    let Some(slot) = boxed_slot(h_rtv) else {
        return;
    };
    // `take` empties the slot as it hands the box over, so a second release on
    // the same handle finds `None` instead of freeing twice.
    if let Some(state) = slot.take() {
        if state.com_raw != 0 {
            drop(IUnknown::from_raw(state.com_raw as *mut c_void));
        }
    }
}

pub(crate) unsafe fn release_resource(h: Hdevice, h_res: ddi::D3D10DDI_HRESOURCE) {
    let Some(slot) = boxed_slot(h_res) else {
        return;
    };
    // Take the box (and empty the slot) BEFORE the teardown below, which
    // re-enters this driver through helios_device/pfnDeallocateCb. A second
    // release of the same handle now finds an empty slot instead of freeing
    // twice. `state` is an owned Box, so the drop at the end of this scope is
    // what frees it -- the explicit `Box::from_raw` is gone.
    if let Some(mut state) = slot.take() {
        let state = &mut *state;
        let outer_identity = state.outer_allocation.take();
        let allocation = (*state)
            .allocation
            .as_ref()
            .map(ResidentAllocation::handle)
            .unwrap_or(0);
        /* An associated resource cannot retire its WDDM identity at the DDI
         * handle edge: DXVK may still own internal references, and Mesa's
         * DestroyBuffer/DestroyImage/FreeMemory records must all land in one
         * exact outer batch.  Move WDDM ownership into the bounded device set,
         * then release COM.  The allocation destructor synchronously opens and
         * finishes that batch when the last reference actually disappears and
         * calls back with the immutable generation/token pair. */
        let mut outer_teardown_armed = false;
        if let Some(identity) = outer_identity {
            if state.com_raw != 0 {
                if let Some(dev) = helios_device(h) {
                    let resident = state.allocation.take();
                    let cpu_backing = state.cpu_backing.take();
                    match arm_outer_allocation_teardown(
                        dev,
                        identity,
                        resident,
                        state.km_resource,
                        state.rt_resource,
                        state.ownership,
                        cpu_backing,
                    ) {
                        Ok(()) => outer_teardown_armed = true,
                        Err((refusal, resident, cpu_backing)) => {
                            state.allocation = resident;
                            state.cpu_backing = cpu_backing;
                            let removed = remove_outer_allocation(&dev.outer, identity);
                            dev.outer.device_lost.store(1, Ordering::Release);
                            log_error!(
                                "DDI outer allocation teardown REFUSED: {:?} token={} generation={} removed={}",
                                refusal,
                                identity.token,
                                identity.allocation_generation,
                                removed
                            );
                        }
                    }
                }
            }

            if outer_teardown_armed {
                let com_raw = core::mem::replace(&mut state.com_raw, 0);
                drop(IUnknown::from_raw(com_raw as *mut c_void));
                return;
            }

            /* Construction invariants make this path unreachable.  Keep it
             * fail-closed: erase a still-live token before COM storage can be
             * reused, let the lower destructor name its missing-pending
             * refusal, and only then fall through to ordinary WDDM cleanup. */
            if let Some(dev) = helios_device(h) {
                let _ = remove_outer_allocation(&dev.outer, identity);
                dev.outer.device_lost.store(1, Ordering::Release);
            }
            if state.com_raw != 0 {
                let com_raw = core::mem::replace(&mut state.com_raw, 0);
                drop(IUnknown::from_raw(com_raw as *mut c_void));
            }
        }

        // Evict while the allocation and runtime device are both still valid.
        // Option::take makes it impossible for ResourceState::drop to evict a
        // second time after pfnDeallocateCb.
        drop((*state).allocation.take());
        if allocation != 0 || !(*state).rt_resource.is_null() {
            if let Some(dev) = helios_device(h) {
                if !dev.kt_callbacks.is_null() {
                    if let Some(deallocate_cb) = (*dev.kt_callbacks).pfnDeallocateCb {
                        // D3DDDICB_DEALLOCATE contract: EITHER hResource (the
                        // runtime releases/closes every allocation it tracks
                        // for the resource — created AND opened instances) OR
                        // NumAllocations+HandleList with hResource NULL. Both
                        // together is E_INVALIDARG (the old 0x80070057, which
                        // also leaked opened resources).
                        let mut allocation = allocation;
                        // One exhaustive decision, then one construction. The
                        // hResource-and-HandleList-together shape the runtime
                        // rejects cannot be built from any DeallocateForm.
                        let form = DeallocateForm::select(
                            (*state).rt_resource,
                            (*state).ownership,
                            allocation,
                        );
                        let mut dealloc = match form {
                            DeallocateForm::ByResource(h_resource) => ddi::D3DDDICB_DEALLOCATE {
                                hResource: h_resource,
                                NumAllocations: 0,
                                HandleList: core::ptr::null_mut(),
                            },
                            DeallocateForm::ByHandleList(handle) => {
                                // Take the handle from the form, not from the
                                // surrounding local: the form is what validated
                                // it as non-zero, and `HandleList` must point at
                                // exactly that value.
                                allocation = handle.get();
                                ddi::D3DDDICB_DEALLOCATE {
                                    hResource: core::ptr::null_mut(),
                                    NumAllocations: 1,
                                    HandleList: &mut allocation,
                                }
                            }
                            DeallocateForm::Nothing { reason } => {
                                log_error!(
                                    "DDI deallocate_resource skip: {} alloc=0x{:x} km=0x{:x}",
                                    reason,
                                    allocation,
                                    (*state).km_resource
                                );
                                ddi::D3DDDICB_DEALLOCATE {
                                    hResource: core::ptr::null_mut(),
                                    NumAllocations: 0,
                                    HandleList: core::ptr::null_mut(),
                                }
                            }
                        };
                        if !matches!(form, DeallocateForm::Nothing { .. }) {
                            let hr = deallocate_cb(dev.h_rt_device, &mut dealloc);
                            trace_line!(
                                "DDI deallocate_resource: hr=0x{:08x} alloc=0x{:x} km=0x{:x} rt={:p} owned={}",
                                hr as u32,
                                allocation,
                                (*state).km_resource,
                                (*state).rt_resource,
                                (*state).ownership.owns()
                            );
                        }
                    }
                }
            }
        }
        if (*state).com_raw != 0 {
            let com_raw = core::mem::replace(&mut state.com_raw, 0);
            drop(IUnknown::from_raw(com_raw as *mut c_void));
        }
    }
}

/// Borrow the COM interface stored in a DDI handle (does not take ownership).
pub(crate) unsafe fn load_com<T: Interface>(h: impl ComHandle) -> Option<ManuallyDrop<T>> {
    load_com_at::<T>(h.drv_private())
}

/// Move the COM reference stored in a DDI handle out of its slot.
///
/// This is intentionally separate from [`load_com`]: the latter borrows an
/// IC-owned reference for normal forwarding, whereas BUILD_2 command-list
/// recycling must clear the IC slot before handing that exact owned reference
/// to the originating deferred context. A null or already-drained slot is a
/// normal no-candidate outcome.
pub(crate) unsafe fn take_com<T: Interface>(h: impl ComHandle) -> Option<T> {
    Slot::<Com<T>>::from_priv(h.drv_private())?.take()
}

/// `load_com` for the three runtime-tag dispatches (`discard_11_1`,
/// `clear_view_11_1`, `tiled_barrier_child`).
///
/// Those receive one `pDrvPrivate` plus a `D3D11DDI_HANDLETYPE` that selects
/// the payload at run time, so the static `ComHandle` bound cannot apply. Each
/// already matches the tag and calls the decoder its arm proved correct --
/// `load_resource` for HT_RESOURCE, `load_rtv` for HT_RENDERTARGETVIEW, this
/// for the bare-COM view tags. Do not call it from anywhere else: it is the
/// unchecked form `load_com` exists to replace.
pub(crate) unsafe fn load_com_at<T: Interface>(
    handle_priv: *mut c_void,
) -> Option<ManuallyDrop<T>> {
    Slot::<Com<T>>::from_priv(handle_priv)?.load()
}

/// Release the COM interface stored in a DDI handle.
pub(crate) unsafe fn release_com(h: impl ComHandle) {
    if let Some(slot) = Slot::<Com<IUnknown>>::from_priv(h.drv_private()) {
        slot.release();
    }
}

pub(crate) fn cpu_access(usage: u32) -> u32 {
    // D3D11_USAGE: DEFAULT=0, IMMUTABLE=1, DYNAMIC=2, STAGING=3.
    match usage {
        2 => D3D11_CPU_ACCESS_WRITE.0 as u32,
        3 => (D3D11_CPU_ACCESS_WRITE.0 | D3D11_CPU_ACCESS_READ.0) as u32,
        _ => 0,
    }
}

pub(crate) fn api_bind_flags(ddi_bind: u32) -> u32 {
    const DDI_PIPELINE_MASK: u32 = 0x0000_007f;
    const DDI_BIND_PRESENT: u32 = 0x0000_0080;
    const DDI_BIND_UAV: u32 = 0x0000_0100;
    const DDI_BIND_DECODER: u32 = 0x0000_0200;
    const DDI_BIND_VIDEO_ENCODER: u32 = 0x0000_0400;
    const DDI_BIND_CAPTURE: u32 = 0x0000_0800;

    let mut out = ddi_bind & DDI_PIPELINE_MASK;
    if ddi_bind & DDI_BIND_UAV != 0 {
        out |= D3D11_BIND_UNORDERED_ACCESS.0 as u32;
    }
    if ddi_bind & DDI_BIND_DECODER != 0 {
        out |= D3D11_BIND_DECODER.0 as u32;
    }
    if ddi_bind & DDI_BIND_VIDEO_ENCODER != 0 {
        out |= D3D11_BIND_VIDEO_ENCODER.0 as u32;
    }
    let _ = DDI_BIND_PRESENT | DDI_BIND_CAPTURE;
    out
}

pub(crate) fn api_misc_flags(ddi_misc: u32, ddi_bind: u32, is_buffer: bool) -> u32 {
    const DDI_MISC_AUTO_GEN_MIP_MAP: u32 = 0x0000_0001;
    const DDI_MISC_SHARED: u32 = 0x0000_0002;
    const DDI_MISC_DRAWINDIRECT_ARGS: u32 = 0x0000_0010;
    const DDI_MISC_BUFFER_ALLOW_RAW_VIEWS: u32 = 0x0000_0020;
    const DDI_MISC_BUFFER_STRUCTURED: u32 = 0x0000_0040;
    const DDI_MISC_RESOURCE_CLAMP: u32 = 0x0000_0080;
    const DDI_MISC_SHARED_KEYEDMUTEX: u32 = 0x0000_0100;
    const DDI_MISC_GDI_COMPATIBLE: u32 = 0x0000_0200;
    const DDI_MISC_TILED: u32 = 0x0000_4000;
    const DDI_MISC_TILE_POOL: u32 = 0x0000_8000;
    const DDI_BIND_PRESENT: u32 = 0x0000_0080;
    const API_MISC_SHARED_NTHANDLE: u32 = 0x0000_0800;
    const API_MISC_TILE_POOL: u32 = 0x0002_0000;
    const API_MISC_TILED: u32 = 0x0004_0000;

    if is_buffer {
        let mut out = ddi_misc
            & (DDI_MISC_DRAWINDIRECT_ARGS
                | DDI_MISC_BUFFER_ALLOW_RAW_VIEWS
                | DDI_MISC_BUFFER_STRUCTURED
                | DDI_MISC_RESOURCE_CLAMP);
        if ddi_misc & DDI_MISC_TILE_POOL != 0 {
            out |= API_MISC_TILE_POOL;
        }
        if ddi_misc & DDI_MISC_TILED != 0 {
            out |= API_MISC_TILED;
        }
        out
    } else {
        let mut out = ddi_misc
            & (DDI_MISC_AUTO_GEN_MIP_MAP
                | DDI_MISC_SHARED
                | DDI_MISC_RESOURCE_CLAMP
                | DDI_MISC_GDI_COMPATIBLE);

        // Producer-side D3D11 resources still need DXVK's NT-shareable export
        // path so native DWM can create shared handles normally. The DDI
        // open_resource path below intentionally imports the KMT resource as
        // plain shared to avoid creating a second DXVK keyed mutex.
        if ddi_misc & DDI_MISC_SHARED != 0 {
            out |= API_MISC_SHARED_NTHANDLE;
        }
        if ddi_misc & DDI_MISC_SHARED_KEYEDMUTEX != 0 || ddi_bind & DDI_BIND_PRESENT != 0 {
            out |= DDI_MISC_SHARED;
        }
        if ddi_misc & DDI_MISC_TILED != 0 {
            out |= API_MISC_TILED;
        }
        out
    }
}
