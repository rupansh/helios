//! KMD runtime for the pure control-owner model.
//! Identity descends from the unique transport domain; the adapter address is
//! only a one-shot construction guard and grants no authority.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::{size_of, ManuallyDrop};
use core::num::NonZeroU64;
use core::ptr::NonNull;
use core::slice;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::context_attachment::AttachmentFinishEffect;
use helios_kmd_logic::context_lifecycle::ContextFinishEffect;
use helios_kmd_logic::control_owner_slots::{
    ContextSlotKind, PairSlotKind, ResourceSlotKind, SlotTableRoot, WindowSlotKind,
};
use helios_kmd_logic::control_owner_table::{
    ContextOwnerSlot, ContextOwnerTicket, ContextPayloadAction, CreatorPayloadAction, DispatchWork,
    DormantOwnerSeed, DormantOwnerState, DormantTransportObservation, DormantTransportRemoval,
    DormantTransportUnavailable, ExternalRunnerRundown, FinalizedResetPayload, ObservedOwnerWork,
    OwnerConfig, OwnerFinalizationRefusal, OwnerPhase, OwnerResetAction, OwnerStorage, OwnerTable,
    OwnerTableRefusal, PairKind, PairOwnerSlot, PairOwnerTicket, PairPayloadAction, PairUseLease,
    ResetPreparation, ResourceOwnerSlot, ResourceOwnerTicket, ResourcePayloadAction,
    ReturnedCustody, WindowOwnerSlot, WindowOwnerTicket, WindowPayloadAction,
};
use helios_kmd_logic::control_ownership::{
    ResourceFinishEffect, TransportDomainRoot, TransportGeneration as OwnershipGeneration,
    TransportWindow, WindowFinishEffect,
};

use crate::adapter::AdapterContext;
use crate::ddi::wddm_surface::{WddmSurface, SURFACE};
use crate::sync::SpinLock;

static TRANSPORT_DOMAIN_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
pub(crate) static TRANSPORT_DOMAIN_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_REBIND_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_OBSERVE_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_REMOVE_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_INSTANCE_INVALID: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_ACTIVATE_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_WINDOW_UNAVAILABLE: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
static TRANSPORT_STORAGE_EXHAUSTED: [AtomicU32; 9] = [const { AtomicU32::new(0) }; 9];

const STORAGE_DIAG_BASE: u32 = 0x0A00_00E6;
const TRANSITION_DIAG_BASE: u32 = 0x0A00_00F0;
const _: () = assert!(TRANSITION_DIAG_BASE > STORAGE_DIAG_BASE + 8);

/// Compatibility name for the D2 authority predicate used throughout the
/// already-reviewed D2-D5 call graph. It is intentionally not a second switch:
/// D9 leaves [`SURFACE`] as the sole durable activation authority, making a
/// mixed WDDM/D2 state unconstructible.
pub(crate) const KMD_D2_OWNER_ENABLED: bool = matches!(SURFACE, WddmSurface::Wddm3_2GpuMmu);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportOwnerCreateError {
    DomainExhausted,
    StorageExhausted,
}

const RESOURCE_CAPACITY: usize = super::gpu::MAX_RESOURCES;
const CONTEXT_CAPACITY: usize = super::gpu::MAX_CONTEXTS;
// Pair=resource is a new activation bound, not a many-to-many proof; its named
// exhaustion refusal remains mandatory. Window rows count live blobs, not free ranges.
const PAIR_CAPACITY: usize = super::gpu::MAX_RESOURCES;
const WINDOW_CAPACITY: usize = super::gpu::MAX_BLOBS;
// Every finalizer-producing Venus allocation is serialized by the adapter's
// Venus mutex (or is the one StartDevice-local client before publication) and
// checks owner admission before creating an object. Once the first finalizer is
// quarantined the table closes, so at most one already-started, not-yet-admitted
// backing can exist in addition to the RESOURCE_CAPACITY canonical rows.
const FINALIZER_QUARANTINE_CAPACITY: usize = RESOURCE_CAPACITY + 1;

struct ResourceCustody {
    size: u64,
    owner: Option<super::gpu::DeviceOwner>,
    creator_context: u32,
    finalizer: ResourceBackingFinalizer,
}

/// Exact kernel-Venus objects whose lifetime is subordinate to one host
/// resource. The canonical resource row owns this token from before CREATE is
/// dispatched until either orderly UNREF finalization or a verified physical
/// transport reset. A zero field means that stage was never needed or has
/// already completed.
#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ResourceBackingFinalizer {
    pub(crate) image_id: u64,
    pub(crate) memory_id: u64,
    /// Locked MDL whose PFNs back a guest-memory blob. Released only after
    /// host UNREF or a verified physical reset.
    pub(crate) guest_mdl: usize,
}

impl ResourceBackingFinalizer {
    pub(crate) const fn none() -> Self {
        Self {
            image_id: 0,
            memory_id: 0,
            guest_mdl: 0,
        }
    }

    pub(crate) const fn memory(memory_id: u64) -> Self {
        Self {
            image_id: 0,
            memory_id,
            guest_mdl: 0,
        }
    }

    pub(crate) const fn image_memory(image_id: u64, memory_id: u64) -> Self {
        Self {
            image_id,
            memory_id,
            guest_mdl: 0,
        }
    }

    pub(crate) const fn guest_pages(mdl: usize) -> Self {
        Self {
            image_id: 0,
            memory_id: 0,
            guest_mdl: mdl,
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.image_id == 0 && self.memory_id == 0 && self.guest_mdl == 0
    }
}

#[must_use]
pub(crate) struct ResourceCreateBeginRefusal {
    error: super::VirtioError,
    /// Present only when the table returned the caller's input custody. `None`
    /// means the table quarantined it and reset now owns the only release path.
    finalizer: Option<ResourceBackingFinalizer>,
}

impl ResourceCreateBeginRefusal {
    pub(crate) fn into_parts(self) -> (super::VirtioError, Option<ResourceBackingFinalizer>) {
        (self.error, self.finalizer)
    }
}

#[derive(PartialEq, Eq)]
struct ContextCustody {
    owner: Option<super::gpu::DeviceOwner>,
}

/// One exact live resource/context association borrowed for renderer work.
///
/// K11's finite direct `SUBMIT_3D` is not a virtio-gpu lifecycle control, so it
/// has no `DispatchWork` ticket.  The canonical pair's use lease is the proper
/// owner-table edge instead: close refuses new leases, reset waits for every
/// returned lease, and detach/context destruction cannot pass a live one.
pub(crate) struct PairUseGuard<'a> {
    owner: &'a TransportOwner,
    lease: Option<PairUseLease>,
}

impl Drop for PairUseGuard<'_> {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        let mut state = self.owner.state.lock();
        let returned = match state.table_mut() {
            Ok(table) => table.return_pair_use(lease).is_ok(),
            Err(_) => {
                // Preserve move-only custody when the arena itself vanished.
                // A legitimate reset cannot reach this arm: it first closes
                // admission and proves the pair-use census is zero.
                core::mem::forget(lease);
                false
            }
        };
        if !returned {
            // Losing a move-only lease would let reset pass a live renderer
            // access. `RefusedAdmission` retains the failed-return custody;
            // record the invariant loss rather than fabricating a return.
            crate::diag::record(TRANSITION_DIAG_BASE + 8);
        }
    }
}

struct AssociationCustody {
    creator: bool,
}

struct WindowCustody {
    offset: u64,
    length: u64,
}

type ResourceSlot = ResourceOwnerSlot<ResourceCustody>;
type ContextSlot = ContextOwnerSlot<ContextCustody>;
type PairSlot = PairOwnerSlot<AssociationCustody>;
type WindowSlot = WindowOwnerSlot<WindowCustody>;
type ResourceTicket = ResourceOwnerTicket<super::VirtioError>;
type ContextTicket = ContextOwnerTicket<super::VirtioError>;
type PairTicket = PairOwnerTicket<super::VirtioError>;
type WindowTicket = WindowOwnerTicket<super::VirtioError>;

const fn checked_storage_bytes() -> Option<usize> {
    let resource_width = match size_of::<ResourceSlot>().checked_add(size_of::<ResourceTicket>()) {
        Some(bytes) => bytes,
        None => return None,
    };
    let resource = match resource_width.checked_mul(RESOURCE_CAPACITY) {
        Some(bytes) => bytes,
        None => return None,
    };
    let context_width = match size_of::<ContextSlot>().checked_add(size_of::<ContextTicket>()) {
        Some(bytes) => bytes,
        None => return None,
    };
    let context = match context_width.checked_mul(CONTEXT_CAPACITY) {
        Some(bytes) => bytes,
        None => return None,
    };
    let pair_width = match size_of::<PairSlot>().checked_add(size_of::<PairTicket>()) {
        Some(bytes) => bytes,
        None => return None,
    };
    let pair = match pair_width.checked_mul(PAIR_CAPACITY) {
        Some(bytes) => bytes,
        None => return None,
    };
    let window_width = match size_of::<WindowSlot>().checked_add(size_of::<WindowTicket>()) {
        Some(bytes) => bytes,
        None => return None,
    };
    let window = match window_width.checked_mul(WINDOW_CAPACITY) {
        Some(bytes) => bytes,
        None => return None,
    };
    match resource.checked_add(context) {
        Some(bytes) => match bytes.checked_add(pair) {
            Some(bytes) => bytes.checked_add(window),
            None => None,
        },
        None => None,
    }
}

const OWNER_STORAGE_BYTES: usize = match checked_storage_bytes() {
    Some(bytes) => bytes,
    None => 0,
};

const _: () = assert!(OWNER_STORAGE_BYTES == 19_161_088);
const _: () = assert!(PAIR_CAPACITY == RESOURCE_CAPACITY);
const _: () = assert!(WINDOW_CAPACITY == super::gpu::MAX_BLOBS);

#[derive(Clone, Copy)]
#[repr(usize)]
enum StorageClass {
    ResourceRows,
    ContextRows,
    PairRows,
    WindowRows,
    ResourceTickets,
    ContextTickets,
    PairTickets,
    WindowTickets,
    Shape,
}

struct OwnerStorageArena {
    resources: ManuallyDrop<Box<[ResourceSlot]>>,
    contexts: ManuallyDrop<Box<[ContextSlot]>>,
    pairs: ManuallyDrop<Box<[PairSlot]>>,
    windows: ManuallyDrop<Box<[WindowSlot]>>,
    resource_tickets: ManuallyDrop<Box<[ResourceTicket]>>,
    context_tickets: ManuallyDrop<Box<[ContextTicket]>>,
    pair_tickets: ManuallyDrop<Box<[PairTicket]>>,
    window_tickets: ManuallyDrop<Box<[WindowTicket]>>,
}

impl OwnerStorageArena {
    fn allocate() -> Result<Self, TransportOwnerCreateError> {
        // Keep every allocation in an ordinary owning Box until all eight have
        // succeeded. Wrapping an early field in ManuallyDrop inside a fallible
        // struct literal would leak that field if a later allocation failed.
        let resources = allocate_exact(
            RESOURCE_CAPACITY,
            StorageClass::ResourceRows,
            ResourceSlot::vacant,
        )?;
        let contexts = allocate_exact(
            CONTEXT_CAPACITY,
            StorageClass::ContextRows,
            ContextSlot::vacant,
        )?;
        let pairs = allocate_exact(PAIR_CAPACITY, StorageClass::PairRows, PairSlot::vacant)?;
        let windows = allocate_exact(
            WINDOW_CAPACITY,
            StorageClass::WindowRows,
            WindowSlot::vacant,
        )?;
        let resource_tickets = allocate_exact(
            RESOURCE_CAPACITY,
            StorageClass::ResourceTickets,
            ResourceTicket::empty,
        )?;
        let context_tickets = allocate_exact(
            CONTEXT_CAPACITY,
            StorageClass::ContextTickets,
            ContextTicket::empty,
        )?;
        let pair_tickets =
            allocate_exact(PAIR_CAPACITY, StorageClass::PairTickets, PairTicket::empty)?;
        let window_tickets = allocate_exact(
            WINDOW_CAPACITY,
            StorageClass::WindowTickets,
            WindowTicket::empty,
        )?;
        let arena = Self {
            resources: ManuallyDrop::new(resources),
            contexts: ManuallyDrop::new(contexts),
            pairs: ManuallyDrop::new(pairs),
            windows: ManuallyDrop::new(windows),
            resource_tickets: ManuallyDrop::new(resource_tickets),
            context_tickets: ManuallyDrop::new(context_tickets),
            pair_tickets: ManuallyDrop::new(pair_tickets),
            window_tickets: ManuallyDrop::new(window_tickets),
        };
        if !arena.has_exact_shape() {
            return Err(record_storage_exhaustion(StorageClass::Shape));
        }
        Ok(arena)
    }

    fn has_exact_shape(&self) -> bool {
        self.resources.len() == RESOURCE_CAPACITY
            && self.contexts.len() == CONTEXT_CAPACITY
            && self.pairs.len() == PAIR_CAPACITY
            && self.windows.len() == WINDOW_CAPACITY
            && self.resource_tickets.len() == self.resources.len()
            && self.context_tickets.len() == self.contexts.len()
            && self.pair_tickets.len() == self.pairs.len()
            && self.window_tickets.len() == self.windows.len()
    }

    /// Safety: the returned projection is retained only by the table stored
    /// beside this arena. Every boxed slice has a stable allocation, the arena
    /// is never exposed, and `ActiveOwner::drop` destroys the table first.
    unsafe fn project_static(
        &mut self,
    ) -> OwnerStorage<
        'static,
        ResourceCustody,
        ContextCustody,
        AssociationCustody,
        WindowCustody,
        super::VirtioError,
    > {
        OwnerStorage {
            resources: unsafe {
                slice::from_raw_parts_mut(self.resources.as_mut_ptr(), self.resources.len())
            },
            contexts: unsafe {
                slice::from_raw_parts_mut(self.contexts.as_mut_ptr(), self.contexts.len())
            },
            pairs: unsafe { slice::from_raw_parts_mut(self.pairs.as_mut_ptr(), self.pairs.len()) },
            windows: unsafe {
                slice::from_raw_parts_mut(self.windows.as_mut_ptr(), self.windows.len())
            },
            resource_tickets: unsafe {
                slice::from_raw_parts_mut(
                    self.resource_tickets.as_mut_ptr(),
                    self.resource_tickets.len(),
                )
            },
            context_tickets: unsafe {
                slice::from_raw_parts_mut(
                    self.context_tickets.as_mut_ptr(),
                    self.context_tickets.len(),
                )
            },
            pair_tickets: unsafe {
                slice::from_raw_parts_mut(self.pair_tickets.as_mut_ptr(), self.pair_tickets.len())
            },
            window_tickets: unsafe {
                slice::from_raw_parts_mut(
                    self.window_tickets.as_mut_ptr(),
                    self.window_tickets.len(),
                )
            },
        }
    }
}

impl Drop for OwnerStorageArena {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.window_tickets);
            ManuallyDrop::drop(&mut self.pair_tickets);
            ManuallyDrop::drop(&mut self.context_tickets);
            ManuallyDrop::drop(&mut self.resource_tickets);
            ManuallyDrop::drop(&mut self.windows);
            ManuallyDrop::drop(&mut self.pairs);
            ManuallyDrop::drop(&mut self.contexts);
            ManuallyDrop::drop(&mut self.resources);
        }
    }
}

type CanonicalOwnerTable = OwnerTable<
    'static,
    ResourceCustody,
    ContextCustody,
    AssociationCustody,
    WindowCustody,
    super::VirtioError,
>;

struct ActiveOwner {
    table: ManuallyDrop<CanonicalOwnerTable>,
    arena: ManuallyDrop<OwnerStorageArena>,
    window_gpa_base: u64,
}

impl ActiveOwner {
    fn activate(
        seed: DormantOwnerSeed,
        mut arena: OwnerStorageArena,
        physical_instance: NonZeroU64,
        window_gpa_base: u64,
    ) -> Result<Self, (DormantOwnerSeed, OwnerStorageArena)> {
        let storage = unsafe { arena.project_static() };
        match seed.activate(physical_instance, storage) {
            Ok(table) => Ok(Self {
                table: ManuallyDrop::new(table),
                arena: ManuallyDrop::new(arena),
                window_gpa_base,
            }),
            Err(refused) => {
                let (seed, storage) = refused.into_parts();
                drop(storage);
                Err((seed, arena))
            }
        }
    }
}

impl Drop for ActiveOwner {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.table);
            ManuallyDrop::drop(&mut self.arena);
        }
    }
}

struct OwnerState {
    construction_address: usize,
    seed: Option<DormantOwnerSeed>,
    arena: Option<OwnerStorageArena>,
    active: Option<ActiveOwner>,
    reset_rundown: Option<ExternalRunnerRundown>,
    reset_preparation: Option<ResetPreparation>,
    reset_ack: Option<FinalizedResetPayload>,
    /// Backing finalizers whose orderly Venus command became ambiguous. The
    /// vector is fully reserved at AddDevice, so quarantining never allocates
    /// under the owner spinlock. The first entry closes admission; consequently
    /// at most the already-live resource rows can contribute one entry each.
    finalizer_quarantine: Vec<ResourceBackingFinalizer>,
}

impl OwnerState {
    fn table_mut(&mut self) -> Result<&mut CanonicalOwnerTable, super::VirtioError> {
        self.active
            .as_mut()
            .map(|active| &mut *active.table)
            .ok_or(super::VirtioError::DeviceError)
    }
}

enum ResourceCompletion {
    Direct(ResourceFinishEffect<super::VirtioError>),
    Creator(CreatorPayloadAction<AssociationCustody, super::VirtioError>),
    Backing(ResourcePayloadAction<ResourceCustody, super::VirtioError>),
}

enum ContextCompletion {
    Direct(ContextFinishEffect<super::VirtioError>),
    Release(ContextPayloadAction<ContextCustody, super::VirtioError>),
}

enum PairCompletion {
    Direct(AttachmentFinishEffect<super::VirtioError>),
    Release(PairPayloadAction<AssociationCustody, super::VirtioError>),
}

enum WindowCompletion {
    Direct(WindowFinishEffect<super::VirtioError>),
    Release(WindowPayloadAction<WindowCustody, super::VirtioError>),
}

pub(crate) struct TransportOwner {
    state: SpinLock<OwnerState>,
}

impl TransportOwner {
    /// PASSIVE_LEVEL: exhaustion publishes its fixed diagnostic breadcrumbs.
    pub(crate) fn unbound() -> Result<Self, TransportOwnerCreateError> {
        let root = mint_domain_root()?;
        let domain = root.id();
        let generation = OwnershipGeneration::bootstrap(root);
        let slot_root = match unsafe { SlotTableRoot::new(domain.get()) } {
            Ok(root) => root,
            Err(_) => return Err(record_domain_exhaustion()),
        };
        // SAFETY: both values descend from the freshly minted `domain`; neither
        // has descendants before the dormant seed consumes them.
        let seed = unsafe { DormantOwnerSeed::new(slot_root, generation) };
        let arena = OwnerStorageArena::allocate()?;
        let mut finalizer_quarantine = Vec::new();
        if finalizer_quarantine
            .try_reserve_exact(FINALIZER_QUARANTINE_CAPACITY)
            .is_err()
        {
            return Err(record_storage_exhaustion(StorageClass::Shape));
        }
        Ok(Self {
            state: SpinLock::new(OwnerState {
                construction_address: 0,
                seed: Some(seed),
                arena: Some(arena),
                active: None,
                reset_rundown: None,
                reset_preparation: None,
                reset_ack: None,
                finalizer_quarantine,
            }),
        })
    }

    pub(crate) fn begin_context_create(
        &self,
        owner: Option<super::gpu::DeviceOwner>,
    ) -> Result<(u32, DispatchWork<ContextSlotKind>), super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let row = table
            .reserve_context(ContextCustody { owner })
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_custody());
                error
            })?;
        let id = table.context(row).map_err(owner_refusal)?.id();
        let prepared = table.begin_context_create(row).map_err(owner_refusal)?;
        table
            .dispatch_context(prepared)
            .map(|work| (id, work))
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_work());
                error
            })
    }

    /// Exact precondition for creating a Venus object that will later transfer
    /// a backing finalizer into a resource row. Callers hold the one Venus
    /// producer serialization boundary while checking and creating the object.
    pub(crate) fn backing_creation_open(&self) -> bool {
        let mut state = self.state.lock();
        state
            .table_mut()
            .is_ok_and(|table| table.phase() == OwnerPhase::Open)
    }

    pub(crate) fn begin_context_destroy(
        &self,
        owner: Option<super::gpu::DeviceOwner>,
        context_id: u32,
    ) -> Result<DispatchWork<ContextSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let row = table
            .context_handle_by_id(context_id)
            .map_err(owner_refusal)?;
        if table.context_owner(row).map_err(owner_refusal)?.owner != owner {
            return Err(super::VirtioError::NotOwned);
        }
        // Close attachment admission in the same owner-table critical section
        // that begins destruction.  Context creation opens this gate after its
        // host completion; without the close, even an otherwise empty context
        // is correctly refused as non-terminal and K11 can never retire its
        // per-session host namespace.
        table.close_context_admission(row).map_err(owner_refusal)?;
        let prepared = table.begin_context_destroy(row).map_err(owner_refusal)?;
        table.dispatch_context(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn first_context_for_owner(
        &self,
        owner: Option<super::gpu::DeviceOwner>,
    ) -> Option<u32> {
        let mut state = self.state.lock();
        let table = state.table_mut().ok()?;
        let custody = ContextCustody { owner };
        let row = table.first_context_handle_by_owner(&custody).ok()??;
        table.context(row).ok().map(|context| context.id())
    }

    /// Borrow the exact attached resource/context pair for one finite renderer
    /// dispatch.  No scalar is accepted as authority: both ids are resolved in
    /// the canonical owner table, the context owner must be the raw KMT device
    /// that owns this HTS1 session, and the pair must still be attached.
    pub(crate) fn borrow_session_pair(
        &self,
        owner: super::gpu::DeviceOwner,
        resource_id: u32,
        context_id: u32,
    ) -> Result<PairUseGuard<'_>, super::VirtioError> {
        let lease = {
            let mut state = self.state.lock();
            let table = state.table_mut()?;
            let context = table
                .context_handle_by_id(context_id)
                .map_err(owner_refusal)?;
            if table.context_owner(context).map_err(owner_refusal)?.owner != Some(owner) {
                return Err(super::VirtioError::NotOwned);
            }
            let resource = table
                .resource_handle_by_id(resource_id)
                .map_err(owner_refusal)?;
            table
                .borrow_pair_use(resource, context)
                .map_err(owner_refusal)?
        };
        Ok(PairUseGuard {
            owner: self,
            lease: Some(lease),
        })
    }

    pub(crate) fn begin_resource_create(
        &self,
        size: u64,
        owner: Option<super::gpu::DeviceOwner>,
        context_id: u32,
        finalizer: ResourceBackingFinalizer,
    ) -> Result<(u32, DispatchWork<ResourceSlotKind>), ResourceCreateBeginRefusal> {
        if size == 0 {
            return Err(ResourceCreateBeginRefusal {
                error: super::VirtioError::DeviceError,
                finalizer: Some(finalizer),
            });
        }
        let mut state = self.state.lock();
        let table = match state.table_mut() {
            Ok(table) => table,
            Err(error) => {
                return Err(ResourceCreateBeginRefusal {
                    error,
                    finalizer: Some(finalizer),
                })
            }
        };
        let context = match table.context_handle_by_id(context_id) {
            Ok(context) => context,
            Err(reason) => {
                return Err(ResourceCreateBeginRefusal {
                    error: owner_refusal(reason),
                    finalizer: Some(finalizer),
                })
            }
        };
        match table.context_owner(context) {
            Ok(custody) if custody.owner == owner => {}
            Ok(_) => {
                return Err(ResourceCreateBeginRefusal {
                    error: super::VirtioError::NotOwned,
                    finalizer: Some(finalizer),
                })
            }
            Err(reason) => {
                return Err(ResourceCreateBeginRefusal {
                    error: owner_refusal(reason),
                    finalizer: Some(finalizer),
                })
            }
        }
        let row = match table.reserve_resource(ResourceCustody {
            size,
            owner,
            creator_context: context_id,
            finalizer,
        }) {
            Ok(row) => row,
            Err(refused) => {
                let error = owner_refusal(refused.reason());
                let finalizer = match refused.into_custody() {
                    ReturnedCustody::Input(custody) | ReturnedCustody::Quarantined(custody) => {
                        Some(custody.finalizer)
                    }
                    ReturnedCustody::TableQuarantined => None,
                };
                return Err(ResourceCreateBeginRefusal { error, finalizer });
            }
        };
        let id = match table.resource(row) {
            Ok(resource) => resource.id(),
            Err(reason) => {
                return Err(ResourceCreateBeginRefusal {
                    error: owner_refusal(reason),
                    finalizer: None,
                })
            }
        };
        let prepared = match table.begin_resource_create(row) {
            Ok(prepared) => prepared,
            Err(reason) => {
                return Err(ResourceCreateBeginRefusal {
                    error: owner_refusal(reason),
                    finalizer: None,
                })
            }
        };
        match table.dispatch_resource(prepared) {
            Ok(work) => Ok((id, work)),
            Err(refused) => {
                let error = owner_refusal(refused.reason());
                // The prepared ticket remains canonical table state. Preserve
                // the move-only authority until reset; it has no destructor and
                // must never be mistaken for returned backing custody.
                core::mem::forget(refused.into_work());
                Err(ResourceCreateBeginRefusal {
                    error,
                    finalizer: None,
                })
            }
        }
    }

    pub(crate) fn begin_creator_attach(
        &self,
        resource_id: u32,
        context_id: u32,
    ) -> Result<DispatchWork<ResourceSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let context = table
            .context_handle_by_id(context_id)
            .map_err(owner_refusal)?;
        let admission = table
            .begin_creator_attach(resource, context, AssociationCustody { creator: true })
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_custody());
                error
            })?;
        let (_, prepared) = admission.into_parts();
        table.dispatch_resource(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn begin_secondary_attach(
        &self,
        resource_id: u32,
        context_id: u32,
    ) -> Result<DispatchWork<PairSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let context = table
            .context_handle_by_id(context_id)
            .map_err(owner_refusal)?;
        let admission = table
            .begin_secondary_attach(resource, context, AssociationCustody { creator: false })
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_custody());
                error
            })?;
        let (_, prepared) = admission.into_parts();
        table.dispatch_pair(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn begin_secondary_detach(
        &self,
        resource_id: u32,
        context_id: u32,
    ) -> Result<DispatchWork<PairSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let pair = table
            .pair_handle_by_ids(resource_id, context_id, PairKind::Secondary)
            .map_err(owner_refusal)?;
        let prepared = table.begin_secondary_detach(pair).map_err(owner_refusal)?;
        table.dispatch_pair(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn pair_kind(
        &self,
        resource_id: u32,
        context_id: u32,
    ) -> Result<PairKind, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let context = table
            .context_handle_by_id(context_id)
            .map_err(owner_refusal)?;
        table
            .canonical_pair_kind(resource, context)
            .map_err(owner_refusal)?
            .ok_or(super::VirtioError::DeviceError)
    }

    pub(crate) fn begin_creator_detach(
        &self,
        resource_id: u32,
    ) -> Result<DispatchWork<ResourceSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let prepared = table
            .begin_creator_detach(resource)
            .map_err(owner_refusal)?;
        table.dispatch_resource(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn begin_resource_unref(
        &self,
        resource_id: u32,
    ) -> Result<DispatchWork<ResourceSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let prepared = table
            .begin_resource_unref(resource)
            .map_err(owner_refusal)?;
        table.dispatch_resource(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn begin_window_map(
        &self,
        resource_id: u32,
        offset: u64,
    ) -> Result<DispatchWork<WindowSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let size = table
            .resource_backing(resource)
            .map_err(owner_refusal)?
            .size;
        let length = helios_kmd_logic::round_up_page(size);
        let identity = TransportWindow::new(
            table.resource(resource).map_err(owner_refusal)?,
            offset,
            length,
        )
        .map_err(|_| super::VirtioError::DeviceError)?;
        let admission = table
            .begin_window_map(resource, identity, WindowCustody { offset, length })
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_custody());
                error
            })?;
        let (_, prepared) = admission.into_parts();
        table.dispatch_window(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    pub(crate) fn begin_window_map_first_fit(
        &self,
        resource_id: u32,
    ) -> Result<(u64, DispatchWork<WindowSlotKind>), super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let size = table
            .resource_backing(resource)
            .map_err(owner_refusal)?
            .size;
        let length = helios_kmd_logic::round_up_page(size);
        let offset = table
            .first_available_window_offset(length)
            .map_err(owner_refusal)?;
        let identity = TransportWindow::new(
            table.resource(resource).map_err(owner_refusal)?,
            offset,
            length,
        )
        .map_err(|_| super::VirtioError::DeviceError)?;
        let admission = table
            .begin_window_map(resource, identity, WindowCustody { offset, length })
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_custody());
                error
            })?;
        let (_, prepared) = admission.into_parts();
        table
            .dispatch_window(prepared)
            .map(|work| (offset, work))
            .map_err(|refused| {
                let error = owner_refusal(refused.reason());
                drop(refused.into_work());
                error
            })
    }

    pub(crate) fn resource_size(&self, resource_id: u32) -> Result<u64, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        Ok(table
            .resource_backing(resource)
            .map_err(owner_refusal)?
            .size)
    }

    pub(crate) fn first_overlapping_window_resource(
        &self,
        resource_id: u32,
        offset: u64,
        length: u64,
    ) -> Result<Option<u32>, super::VirtioError> {
        let mut state = self.state.lock();
        state
            .table_mut()?
            .first_overlapping_window_resource(resource_id, offset, length)
            .map_err(owner_refusal)
    }

    pub(crate) fn begin_window_unmap(
        &self,
        resource_id: u32,
    ) -> Result<DispatchWork<WindowSlotKind>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let window = table
            .window_handle_by_resource_id(resource_id)
            .map_err(owner_refusal)?;
        let prepared = table.begin_window_unmap(window).map_err(owner_refusal)?;
        table.dispatch_window(prepared).map_err(|refused| {
            let error = owner_refusal(refused.reason());
            drop(refused.into_work());
            error
        })
    }

    /// Finish one resource verb and run any terminal backing finalizer outside
    /// the owner spinlock. A finalizer that cannot prove completion returns its
    /// remaining exact token; that token is quarantined before the resource-row
    /// release is acknowledged, and admission is closed until physical reset.
    pub(crate) fn finish_resource<F>(
        &self,
        observed: ObservedOwnerWork<super::VirtioError, ResourceSlotKind>,
        finalize: F,
    ) -> Result<ResourceFinishEffect<super::VirtioError>, super::VirtioError>
    where
        F: FnOnce(ResourceBackingFinalizer) -> Result<(), ResourceBackingFinalizer>,
    {
        let completion = {
            let mut state = self.state.lock();
            let table = state.table_mut()?;
            let action = table
                .finish_resource_work(observed)
                .map_err(|_| super::VirtioError::DeviceError)?;
            let pending = table
                .apply_resource_control(action)
                .map_err(|_| super::VirtioError::DeviceError)?;
            match pending.effect() {
                ResourceFinishEffect::CreateDefiniteNotEnqueued(_)
                | ResourceFinishEffect::CreateHostRejected(_)
                | ResourceFinishEffect::UnrefCompleted => table
                    .begin_resource_payload_release(pending)
                    .map(ResourceCompletion::Backing)
                    .map_err(|_| super::VirtioError::DeviceError)?,
                ResourceFinishEffect::AttachDefiniteNotEnqueued(_)
                | ResourceFinishEffect::AttachHostRejected(_)
                | ResourceFinishEffect::DetachCompleted => table
                    .begin_creator_payload_release(pending)
                    .map(ResourceCompletion::Creator)
                    .map_err(|_| super::VirtioError::DeviceError)?,
                _ => ResourceCompletion::Direct(
                    table
                        .ack_resource_completion(pending)
                        .map_err(|_| super::VirtioError::DeviceError)?,
                ),
            }
        };
        match completion {
            ResourceCompletion::Direct(effect) => Ok(effect),
            ResourceCompletion::Creator(action) => {
                let (association, effect, pending) = action.into_parts();
                debug_assert!(association.creator);
                drop(association);
                let finalized = unsafe { pending.assume_association_finalized() };
                let mut state = self.state.lock();
                state
                    .table_mut()?
                    .ack_creator_payload_release(finalized)
                    .map_err(|_| super::VirtioError::DeviceError)?;
                Ok(effect)
            }
            ResourceCompletion::Backing(action) => {
                let (mut backing, effect, pending) = action.into_parts();
                if !backing.finalizer.is_empty() {
                    if let Err(remaining) = finalize(backing.finalizer) {
                        backing.finalizer = remaining;
                        self.quarantine_resource_finalizer(backing.finalizer);
                    }
                }
                let finalized = unsafe { pending.assume_backing_finalized() };
                let mut state = self.state.lock();
                state
                    .table_mut()?
                    .ack_resource_payload_release(finalized)
                    .map_err(|_| super::VirtioError::DeviceError)?;
                Ok(effect)
            }
        }
    }

    /// Retain one failed finalizer until the exact producing transport has been
    /// reset and raw device status was observed as zero. The pre-reserved vector
    /// and close-on-first-entry invariant make this allocation-free.
    pub(crate) fn quarantine_resource_finalizer(&self, finalizer: ResourceBackingFinalizer) {
        if finalizer.is_empty() {
            return;
        }
        let mut state = self.state.lock();
        if state.finalizer_quarantine.len() < state.finalizer_quarantine.capacity() {
            state.finalizer_quarantine.push(finalizer);
        } else {
            // This can only follow an invariant loss: the first quarantine
            // closes admission; every later Venus producer fails its exact
            // phase check; and the one producer that raced close is covered by
            // the extra reserved slot. Preserve safety by leaking the token
            // rather than releasing backing without proof.
            core::mem::forget(finalizer);
            crate::diag::record(TRANSITION_DIAG_BASE + 7);
        }
        if let Ok(table) = state.table_mut() {
            if table.phase() == OwnerPhase::Open {
                let _ = table.close();
            }
        }
    }

    pub(crate) fn finish_context(
        &self,
        observed: ObservedOwnerWork<super::VirtioError, ContextSlotKind>,
    ) -> Result<ContextFinishEffect<super::VirtioError>, super::VirtioError> {
        let completion = {
            let mut state = self.state.lock();
            let table = state.table_mut()?;
            let action = table
                .finish_context_work(observed)
                .map_err(|_| super::VirtioError::DeviceError)?;
            let pending = table
                .apply_context_control(action)
                .map_err(|_| super::VirtioError::DeviceError)?;
            match pending.effect() {
                ContextFinishEffect::CreateDefiniteNotEnqueued(_)
                | ContextFinishEffect::CreateHostRejected(_)
                | ContextFinishEffect::DestroyCompleted => table
                    .begin_context_payload_release(pending)
                    .map(ContextCompletion::Release)
                    .map_err(|_| super::VirtioError::DeviceError)?,
                _ => ContextCompletion::Direct(
                    table
                        .ack_context_completion(pending)
                        .map_err(|_| super::VirtioError::DeviceError)?,
                ),
            }
        };
        match completion {
            ContextCompletion::Direct(effect) => Ok(effect),
            ContextCompletion::Release(action) => {
                let (owner, effect, pending) = action.into_parts();
                drop(owner);
                let finalized = unsafe { pending.assume_owner_finalized() };
                let mut state = self.state.lock();
                state
                    .table_mut()?
                    .ack_context_payload_release(finalized)
                    .map_err(|_| super::VirtioError::DeviceError)?;
                Ok(effect)
            }
        }
    }

    pub(crate) fn finish_pair(
        &self,
        observed: ObservedOwnerWork<super::VirtioError, PairSlotKind>,
    ) -> Result<AttachmentFinishEffect<super::VirtioError>, super::VirtioError> {
        let completion = {
            let mut state = self.state.lock();
            let table = state.table_mut()?;
            let action = table
                .finish_pair_work(observed)
                .map_err(|_| super::VirtioError::DeviceError)?;
            let pending = table
                .apply_pair_control(action)
                .map_err(|_| super::VirtioError::DeviceError)?;
            match pending.effect() {
                AttachmentFinishEffect::AttachDefiniteNotEnqueued(_)
                | AttachmentFinishEffect::AttachHostRejected(_)
                | AttachmentFinishEffect::DetachCompleted => table
                    .begin_pair_payload_release(pending)
                    .map(PairCompletion::Release)
                    .map_err(|_| super::VirtioError::DeviceError)?,
                _ => PairCompletion::Direct(
                    table
                        .ack_pair_completion(pending)
                        .map_err(|_| super::VirtioError::DeviceError)?,
                ),
            }
        };
        match completion {
            PairCompletion::Direct(effect) => Ok(effect),
            PairCompletion::Release(action) => {
                let (association, effect, pending) = action.into_parts();
                debug_assert!(!association.creator);
                drop(association);
                let finalized = unsafe { pending.assume_association_finalized() };
                let mut state = self.state.lock();
                state
                    .table_mut()?
                    .ack_pair_payload_release(finalized)
                    .map_err(|_| super::VirtioError::DeviceError)?;
                Ok(effect)
            }
        }
    }

    pub(crate) fn finish_window(
        &self,
        observed: ObservedOwnerWork<super::VirtioError, WindowSlotKind>,
    ) -> Result<WindowFinishEffect<super::VirtioError>, super::VirtioError> {
        let completion = {
            let mut state = self.state.lock();
            let table = state.table_mut()?;
            let action = table
                .finish_window_work(observed)
                .map_err(|_| super::VirtioError::DeviceError)?;
            let pending = table
                .apply_window_control(action)
                .map_err(|_| super::VirtioError::DeviceError)?;
            match pending.effect() {
                WindowFinishEffect::MapDefiniteNotEnqueued(_)
                | WindowFinishEffect::MapHostRejected(_)
                | WindowFinishEffect::UnmapCompleted => table
                    .begin_window_payload_release(pending)
                    .map(WindowCompletion::Release)
                    .map_err(|_| super::VirtioError::DeviceError)?,
                _ => WindowCompletion::Direct(
                    table
                        .ack_window_completion(pending)
                        .map_err(|_| super::VirtioError::DeviceError)?,
                ),
            }
        };
        match completion {
            WindowCompletion::Direct(effect) => Ok(effect),
            WindowCompletion::Release(action) => {
                let (reservation, effect, pending) = action.into_parts();
                debug_assert_eq!(
                    reservation.length,
                    helios_kmd_logic::round_up_page(reservation.length)
                );
                let _offset = reservation.offset;
                drop(reservation);
                let finalized = unsafe { pending.assume_reservation_finalized() };
                let mut state = self.state.lock();
                state
                    .table_mut()?
                    .ack_window_payload_release(finalized)
                    .map_err(|_| super::VirtioError::DeviceError)?;
                Ok(effect)
            }
        }
    }

    pub(crate) fn resource_for_owner(
        &self,
        owner: Option<super::gpu::DeviceOwner>,
    ) -> Option<(u32, u32)> {
        let mut state = self.state.lock();
        let table = state.table_mut().ok()?;
        let row = table
            .first_resource_handle_where(|backing| backing.owner == owner)
            .ok()??;
        let resource = table.resource(row).ok()?.id();
        let context = table.resource_backing(row).ok()?.creator_context;
        Some((context, resource))
    }

    pub(crate) fn resource_owned_by(
        &self,
        owner: Option<super::gpu::DeviceOwner>,
        context_id: u32,
        resource_id: u32,
    ) -> bool {
        let mut state = self.state.lock();
        let Ok(table) = state.table_mut() else {
            return false;
        };
        let Ok(row) = table.resource_handle_by_id(resource_id) else {
            return false;
        };
        table
            .resource_backing(row)
            .is_ok_and(|backing| backing.owner == owner && backing.creator_context == context_id)
    }

    /// Fail closed after K11 loses terminal proof for one exact private
    /// context. The direct context id and its DeviceOwner are both validated
    /// before the adapter table is quarantined; a stale session therefore
    /// cannot seal a successor transport by replaying an old scalar id.
    pub(crate) fn quarantine_session_context(
        &self,
        owner: super::gpu::DeviceOwner,
        context_id: u32,
    ) -> Result<(), super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let context = table
            .context_handle_by_id(context_id)
            .map_err(owner_refusal)?;
        if table.context_owner(context).map_err(owner_refusal)?.owner != Some(owner) {
            return Err(super::VirtioError::NotOwned);
        }
        table.quarantine().map_err(owner_refusal)
    }

    pub(crate) fn resource_matches_filter(
        &self,
        filter: super::gpu::OwnerFilter,
        resource_id: u32,
    ) -> bool {
        let mut state = self.state.lock();
        let Ok(table) = state.table_mut() else {
            return false;
        };
        let Ok(row) = table.resource_handle_by_id(resource_id) else {
            return false;
        };
        table
            .resource_backing(row)
            .is_ok_and(|backing| match filter {
                super::gpu::OwnerFilter::Any => true,
                super::gpu::OwnerFilter::Exactly(owner) => backing.owner == owner,
            })
    }

    pub(crate) fn mapped_blob_offset(
        &self,
        resource_id: u32,
    ) -> Result<Option<u64>, super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        let window = match table.window_handle_by_resource_id(resource_id) {
            Ok(window) => window,
            Err(OwnerTableRefusal::WindowNotFound) => return Ok(None),
            Err(reason) => return Err(owner_refusal(reason)),
        };
        match table.mapped_window(window) {
            Ok(identity) => Ok(Some(identity.offset())),
            Err(OwnerTableRefusal::WindowNotFound) => Err(super::VirtioError::DeviceError),
            Err(reason) => Err(owner_refusal(reason)),
        }
    }

    pub(crate) fn mapped_blob(
        &self,
        resource_id: u32,
    ) -> Result<Option<super::gpu::BlobMapPrep>, super::VirtioError> {
        let mut state = self.state.lock();
        let active = state
            .active
            .as_mut()
            .ok_or(super::VirtioError::DeviceError)?;
        let table = &mut *active.table;
        let resource = table
            .resource_handle_by_id(resource_id)
            .map_err(owner_refusal)?;
        let size = table
            .resource_backing(resource)
            .map_err(owner_refusal)?
            .size;
        let window = match table.window_handle_by_resource_id(resource_id) {
            Ok(window) => window,
            Err(OwnerTableRefusal::WindowNotFound) => return Ok(None),
            Err(reason) => return Err(owner_refusal(reason)),
        };
        let identity = table.mapped_window(window).map_err(owner_refusal)?;
        let map_cache = table.mapped_window_info(window).map_err(owner_refusal)?;
        let gpa = active
            .window_gpa_base
            .checked_add(identity.offset())
            .ok_or(super::VirtioError::DeviceError)?;
        Ok(Some(super::gpu::BlobMapPrep {
            gpa,
            size,
            map_cache,
        }))
    }

    pub(crate) fn close_for_transport_reset(
        &self,
        expected_instance: u64,
    ) -> Result<(), super::VirtioError> {
        let mut state = self.state.lock();
        let table = state.table_mut()?;
        if expected_instance == 0 || table.physical_instance().get() != expected_instance {
            return Err(super::VirtioError::DeviceError);
        }
        match table.phase() {
            OwnerPhase::Open => table.close().map_err(owner_refusal)?,
            OwnerPhase::Closing
            | OwnerPhase::Quarantined
            | OwnerPhase::RundownSealed
            | OwnerPhase::ResetPrepared
            | OwnerPhase::Resetting
            | OwnerPhase::Ready
            | OwnerPhase::Exhausted => {}
            OwnerPhase::ResetAuthorized => return Err(super::VirtioError::DeviceError),
        }
        Ok(())
    }

    pub(crate) fn prepare_physical_reset(
        &self,
        passive: crate::irql::PassiveLevel,
    ) -> Result<u64, super::VirtioError> {
        // One already-running control operation may consume the full 30-second
        // synchronous round-trip budget. A resource payload finalizer can then
        // issue both image-destroy and memory-free work before acknowledging its
        // ReleasePending row. Keep this bounded, but do not time out the reset
        // before those two legal out-of-lock stages can finish.
        const MAX_SLICES: u32 = 65_000;
        let mut slices = 0u32;
        let rundown = loop {
            let decision = {
                let mut state = self.state.lock();
                if let Some(preparation) = state.reset_preparation.as_ref() {
                    return Ok(preparation.physical_instance().get());
                }
                let saved = state.reset_rundown.take();
                let table = state.table_mut()?;
                if matches!(
                    table.phase(),
                    OwnerPhase::Resetting | OwnerPhase::Ready | OwnerPhase::Exhausted
                ) {
                    return Ok(table.physical_instance().get());
                }
                if let Some(rundown) = saved {
                    Some(Ok(rundown))
                } else if table.active_runs() == 0 {
                    Some(unsafe { table.assume_external_runner_rundown() }.map_err(owner_refusal))
                } else {
                    None
                }
            };
            if let Some(decision) = decision {
                break decision?;
            }
            if slices >= MAX_SLICES {
                return Err(super::VirtioError::Timeout);
            }
            slices += 1;
            crate::virtio::ctrl::sleep_ms(passive, 1);
        };

        let mut rundown = rundown;
        loop {
            let attempt = {
                let mut state = self.state.lock();
                match state.table_mut()?.prepare_reset(rundown) {
                    Ok(preparation) => {
                        let physical_instance = preparation.physical_instance().get();
                        state.reset_preparation = Some(preparation);
                        Ok(physical_instance)
                    }
                    Err(refused) => Err(refused),
                }
            };
            match attempt {
                Ok(physical_instance) => return Ok(physical_instance),
                Err(refused)
                    if refused.reason() == OwnerTableRefusal::ExternalActionOutstanding =>
                {
                    rundown = refused.into_rundown();
                    if slices >= MAX_SLICES {
                        let mut state = self.state.lock();
                        state.reset_rundown = Some(rundown);
                        return Err(super::VirtioError::Timeout);
                    }
                    slices += 1;
                    crate::virtio::ctrl::sleep_ms(passive, 1);
                }
                Err(refused) => {
                    let mut state = self.state.lock();
                    state.reset_rundown = Some(refused.into_rundown());
                    return Err(super::VirtioError::DeviceError);
                }
            }
        }
    }

    pub(crate) fn finish_physical_reset(
        &self,
        passive: crate::irql::PassiveLevel,
        raw_status: u32,
    ) -> Result<(), super::VirtioError> {
        let preparation = {
            let mut state = self.state.lock();
            state.reset_preparation.take()
        };
        if let Some(preparation) = preparation {
            let verified = match unsafe { preparation.verify_raw_zero(raw_status) } {
                Ok(verified) => verified,
                Err(refused) => {
                    let mut state = self.state.lock();
                    state.reset_preparation = Some(refused.into_preparation());
                    return Err(super::VirtioError::DeviceError);
                }
            };
            let mut state = self.state.lock();
            if let Err(refused) = state.table_mut()?.authorize_reset(verified) {
                state.reset_preparation = Some(refused.into_preparation());
                return Err(super::VirtioError::DeviceError);
            }
        } else {
            let mut state = self.state.lock();
            let table = state.table_mut()?;
            if raw_status != 0 {
                return Err(super::VirtioError::DeviceError);
            }
            match table.phase() {
                OwnerPhase::Resetting => {}
                OwnerPhase::Ready | OwnerPhase::Exhausted => return Ok(()),
                _ => return Err(super::VirtioError::DeviceError),
            }
        }
        loop {
            let retry_failed = {
                let mut state = self.state.lock();
                if let Some(finalized) = state.reset_ack.take() {
                    match state.table_mut()?.ack_reset_action(finalized) {
                        Ok(()) => false,
                        Err(OwnerFinalizationRefusal::Recoverable { action, .. }) => {
                            state.reset_ack = Some(action);
                            true
                        }
                        Err(OwnerFinalizationRefusal::Poisoned) => {
                            return Err(super::VirtioError::DeviceError)
                        }
                    }
                } else {
                    false
                }
            };
            if retry_failed {
                return Err(super::VirtioError::DeviceError);
            }
            let action = {
                let mut state = self.state.lock();
                state
                    .table_mut()?
                    .next_reset_action()
                    .map_err(owner_refusal)?
            };
            let Some(action) = action else {
                break;
            };
            let pending = match action {
                OwnerResetAction::Window(action) => {
                    let (reservation, _, pending) = action.into_parts();
                    drop(reservation);
                    pending
                }
                OwnerResetAction::SecondaryPair(action) => {
                    let (association, pending) = action.into_parts();
                    drop(association);
                    pending
                }
                OwnerResetAction::Resource(action) => {
                    let (mut backing, association, pending) = action.into_parts();
                    let finalizer = core::mem::replace(
                        &mut backing.finalizer,
                        ResourceBackingFinalizer::none(),
                    );
                    drop(backing);
                    drop(association);
                    crate::virtio::ctrl::finalize_resource_backing_after_reset(passive, finalizer);
                    pending
                }
                OwnerResetAction::Context(action) => {
                    let (owner, pending) = action.into_parts();
                    drop(owner);
                    pending
                }
            };
            let finalized = unsafe { pending.assume_payloads_finalized() };
            let mut state = self.state.lock();
            match state.table_mut()?.ack_reset_action(finalized) {
                Ok(()) => {}
                Err(OwnerFinalizationRefusal::Recoverable { action, .. }) => {
                    state.reset_ack = Some(action);
                    return Err(super::VirtioError::DeviceError);
                }
                Err(OwnerFinalizationRefusal::Poisoned) => {
                    return Err(super::VirtioError::DeviceError)
                }
            }
        }

        // These tokens were irreversibly quarantined only after an orderly
        // finalizer could not prove completion. Raw-zero reset is now verified
        // and every canonical reset action is acknowledged, so the old Venus
        // object identities no longer name live host objects. Move the reserved
        // vector out, drop its payloads without the spinlock, then return its
        // allocation for the successor epoch.
        let mut quarantine = {
            let mut state = self.state.lock();
            core::mem::take(&mut state.finalizer_quarantine)
        };
        for finalizer in quarantine.drain(..) {
            crate::virtio::ctrl::finalize_resource_backing_after_reset(passive, finalizer);
        }
        let mut state = self.state.lock();
        debug_assert!(state.finalizer_quarantine.is_empty());
        state.finalizer_quarantine = quarantine;
        Ok(())
    }

    /// Safety: at PASSIVE_LEVEL, `adapter` is the owner's final construction
    /// address, recorded exactly once before publication.
    pub(crate) unsafe fn bind_adapter_once(&self, adapter: NonNull<AdapterContext>) {
        let refused = {
            let mut state = self.state.lock();
            if state.construction_address != 0 {
                true
            } else {
                state.construction_address = adapter.as_ptr() as usize;
                false
            }
        };
        if refused {
            let count = TRANSPORT_OWNER_REBIND_REFUSED
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            crate::diag::record_named_bytes(b"CtDomReb", count);
            crate::diag::record(0x0A00_00E5);
        }
    }

    /// Begin one StartDevice diagnostic interval. Reset only the inert
    /// transition facts that the following probe/install attempt can republish;
    /// domain/storage construction evidence remains lifetime-cumulative.
    pub(crate) fn reset_transition_diagnostics(&self, _passive: crate::irql::PassiveLevel) {
        TRANSPORT_OWNER_OBSERVE_REFUSED.store(0, Ordering::Relaxed);
        TRANSPORT_OWNER_REMOVE_REFUSED.store(0, Ordering::Relaxed);
        TRANSPORT_OWNER_INSTANCE_INVALID.store(0, Ordering::Relaxed);
        TRANSPORT_OWNER_ACTIVATE_REFUSED.store(0, Ordering::Relaxed);
        crate::diag::record_named_bytes(b"CtObsRef", 0);
        crate::diag::record_named_bytes(b"CtRemRef", 0);
        crate::diag::record_named_bytes(b"CtInst0", 0);
        crate::diag::record_named_bytes(b"CtActRef", 0);
        publish_window_availability(None);
    }

    /// Observe one fully configured StartDevice-local transport. With KMD D2
    /// disabled this remains diagnostic-only; with it enabled the same exact
    /// observation constructs or reopens the sole canonical owner table.
    ///
    /// # Safety
    /// `adapter` is this owner's bound adapter and `gpu` is the exact local
    /// transport candidate consumed by its serialized install transition.
    pub(crate) unsafe fn observe_initialized_transport(
        &self,
        _passive: crate::irql::PassiveLevel,
        adapter: NonNull<AdapterContext>,
        gpu: &super::VirtioGpu,
    ) -> Result<(), super::VirtioError> {
        enum Disposition {
            Accepted,
            Refused,
            ActivationRefused,
            Unavailable,
        }

        let Some(physical_instance) = NonZeroU64::new(gpu.scanout_transport_instance()) else {
            record_transition_refusal(
                &TRANSPORT_OWNER_INSTANCE_INVALID,
                b"CtInst0",
                TRANSITION_DIAG_BASE,
            );
            return Err(super::VirtioError::DeviceError);
        };
        let (geometry, window_gpa_base, unavailable) = match gpu.owner_window_geometry() {
            None => (
                Err(DormantTransportUnavailable::NoWindow),
                0,
                Some(DormantTransportUnavailable::NoWindow),
            ),
            Some((window_base, window_length, first_fit_base)) => {
                match OwnerConfig::new(0, window_length, super::gpu::BLOB_PAGE)
                    .and_then(|config| config.with_first_fit_base(first_fit_base))
                {
                    Ok(config) => (Ok(config), window_base, None),
                    Err(_) => (
                        Err(DormantTransportUnavailable::InvalidBounds),
                        0,
                        Some(DormantTransportUnavailable::InvalidBounds),
                    ),
                }
            }
        };
        let disposition = {
            let mut state = self.state.lock();
            if state.construction_address != adapter.as_ptr() as usize {
                Disposition::Refused
            } else if let Some(active) = state.active.as_mut() {
                match geometry {
                    Ok(config) if active.table.phase() == OwnerPhase::Ready => {
                        match active.table.request_next_transport() {
                            Ok(request) => {
                                let ready =
                                    unsafe { request.assume_ready(physical_instance, config) };
                                match active.table.reopen(ready) {
                                    Ok(()) => {
                                        active.window_gpa_base = window_gpa_base;
                                        Disposition::Accepted
                                    }
                                    Err(refused) => {
                                        let ready = refused.into_ready();
                                        let _ = active.table.cancel_next_transport(ready);
                                        Disposition::Refused
                                    }
                                }
                            }
                            Err(_) => Disposition::Refused,
                        }
                    }
                    Err(_) if KMD_D2_OWNER_ENABLED => Disposition::Unavailable,
                    _ => Disposition::Refused,
                }
            } else if state.arena.is_none() {
                Disposition::Refused
            } else {
                let observed = match state.seed.as_mut() {
                    Some(seed) => {
                        let table = seed.table();
                        let epoch = seed.epoch();
                        let observation = unsafe {
                            match geometry {
                                Ok(config) => DormantTransportObservation::assume_ready(
                                    table,
                                    epoch,
                                    physical_instance,
                                    config,
                                ),
                                Err(reason) => DormantTransportObservation::assume_unavailable(
                                    table,
                                    epoch,
                                    physical_instance,
                                    reason,
                                ),
                            }
                        };
                        seed.observe_transport(observation).is_ok()
                    }
                    None => false,
                };
                if !observed {
                    Disposition::Refused
                } else if !KMD_D2_OWNER_ENABLED {
                    Disposition::Accepted
                } else if geometry.is_err() {
                    let removed = match state.seed.as_mut() {
                        Some(seed) => {
                            let removal = unsafe {
                                DormantTransportRemoval::assume_removed(
                                    seed.table(),
                                    seed.epoch(),
                                    physical_instance,
                                )
                            };
                            seed.observe_transport_removed(removal).is_ok()
                        }
                        None => false,
                    };
                    if removed {
                        Disposition::Unavailable
                    } else {
                        Disposition::Refused
                    }
                } else {
                    match (state.seed.take(), state.arena.take()) {
                        (Some(seed), Some(arena)) => match ActiveOwner::activate(
                            seed,
                            arena,
                            physical_instance,
                            window_gpa_base,
                        ) {
                            Ok(active) => {
                                state.active = Some(active);
                                Disposition::Accepted
                            }
                            Err((mut seed, arena)) => {
                                let removal = unsafe {
                                    DormantTransportRemoval::assume_removed(
                                        seed.table(),
                                        seed.epoch(),
                                        physical_instance,
                                    )
                                };
                                let removed = seed.observe_transport_removed(removal).is_ok();
                                state.seed = Some(seed);
                                state.arena = Some(arena);
                                if removed {
                                    Disposition::ActivationRefused
                                } else {
                                    Disposition::Refused
                                }
                            }
                        },
                        (seed, arena) => {
                            state.seed = seed;
                            state.arena = arena;
                            Disposition::Refused
                        }
                    }
                }
            }
        };
        match disposition {
            Disposition::Accepted => {
                publish_window_availability(unavailable);
                Ok(())
            }
            Disposition::Refused => {
                record_transition_refusal(
                    &TRANSPORT_OWNER_OBSERVE_REFUSED,
                    b"CtObsRef",
                    TRANSITION_DIAG_BASE + 1,
                );
                Err(super::VirtioError::DeviceError)
            }
            Disposition::ActivationRefused => {
                record_transition_refusal(
                    &TRANSPORT_OWNER_ACTIVATE_REFUSED,
                    b"CtActRef",
                    TRANSITION_DIAG_BASE + 5,
                );
                Err(super::VirtioError::DeviceError)
            }
            Disposition::Unavailable => {
                publish_window_availability(unavailable);
                Err(super::VirtioError::DeviceError)
            }
        }
    }

    /// Clear only the observation belonging to the exact transport removed
    /// from this adapter's slot. Foreign/replayed identities remain inert.
    pub(crate) fn observe_removed_transport(
        &self,
        _passive: crate::irql::PassiveLevel,
        adapter: NonNull<AdapterContext>,
        physical_instance: u64,
    ) {
        let Some(physical_instance) = NonZeroU64::new(physical_instance) else {
            record_transition_refusal(
                &TRANSPORT_OWNER_INSTANCE_INVALID,
                b"CtInst0",
                TRANSITION_DIAG_BASE,
            );
            return;
        };
        let refused = {
            let mut state = self.state.lock();
            if state.construction_address != adapter.as_ptr() as usize {
                true
            } else if let Some(active) = state.active.as_mut() {
                active.table.physical_instance() != physical_instance
                    || !matches!(
                        active.table.phase(),
                        OwnerPhase::Ready | OwnerPhase::Exhausted
                    )
            } else {
                match state.seed.as_mut() {
                    Some(seed) => {
                        let table = seed.table();
                        let epoch = seed.epoch();
                        // SAFETY: the caller extracted the exact transport from this
                        // adapter's slot under `virtio_lock`; no later producer can
                        // reach it. The seed supplies its own exact identity here.
                        let removal = unsafe {
                            DormantTransportRemoval::assume_removed(table, epoch, physical_instance)
                        };
                        seed.observe_transport_removed(removal).is_err()
                    }
                    None => true,
                }
            }
        };
        if refused {
            record_transition_refusal(
                &TRANSPORT_OWNER_REMOVE_REFUSED,
                b"CtRemRef",
                TRANSITION_DIAG_BASE + 4,
            );
        } else {
            publish_window_availability(None);
        }
    }

    /// Reconcile an already-empty transport slot. This clears per-instance
    /// availability telemetry but never clears a seed that still observes an
    /// exact transport; that mismatch remains a named fail-closed refusal.
    pub(crate) fn observe_transport_slot_absent(
        &self,
        _passive: crate::irql::PassiveLevel,
        adapter: NonNull<AdapterContext>,
    ) {
        let refused = {
            let state = self.state.lock();
            state.construction_address != adapter.as_ptr() as usize
                || match (&state.seed, &state.active) {
                    (Some(seed), None) => seed.state() != DormantOwnerState::NoTransport,
                    (None, Some(active)) => !matches!(
                        active.table.phase(),
                        OwnerPhase::Ready | OwnerPhase::Exhausted
                    ),
                    _ => true,
                }
        };
        if refused {
            record_transition_refusal(
                &TRANSPORT_OWNER_REMOVE_REFUSED,
                b"CtRemRef",
                TRANSITION_DIAG_BASE + 4,
            );
        }
        publish_window_availability(None);
    }
}

const _: () = {
    fn assert_send_sync<T: Send + Sync>() {}
    let _ = assert_send_sync::<TransportOwner>;
};

fn allocate_exact<T, F>(
    count: usize,
    class: StorageClass,
    mut make: F,
) -> Result<Box<[T]>, TransportOwnerCreateError>
where
    F: FnMut() -> T,
{
    let mut entries = Vec::new();
    if entries.try_reserve_exact(count).is_err() {
        return Err(record_storage_exhaustion(class));
    }
    let mut index = 0;
    while index < count {
        entries.push(make());
        index += 1;
    }
    Ok(entries.into_boxed_slice())
}

fn owner_refusal(reason: OwnerTableRefusal) -> super::VirtioError {
    match reason {
        OwnerTableRefusal::ResourceCapacityExhausted
        | OwnerTableRefusal::ContextCapacityExhausted
        | OwnerTableRefusal::PairCapacityExhausted
        | OwnerTableRefusal::WindowCapacityExhausted => super::VirtioError::OutOfMemory,
        _ => super::VirtioError::DeviceError,
    }
}

fn mint_domain_root() -> Result<TransportDomainRoot, TransportOwnerCreateError> {
    let previous = TRANSPORT_DOMAIN_HIGH_WATER
        .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| record_domain_exhaustion())?;
    let Some(issued) = previous.checked_add(1) else {
        return Err(record_domain_exhaustion());
    };

    // SAFETY: the non-wrapping driver-load high-water issues each nonzero value once.
    unsafe { TransportDomainRoot::new(issued) }.map_err(|_| record_domain_exhaustion())
}

fn record_domain_exhaustion() -> TransportOwnerCreateError {
    let count = TRANSPORT_DOMAIN_EXHAUSTED
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    crate::diag::record_named_bytes(b"CtDomExh", count);
    crate::diag::record(0x0A00_00E4);
    TransportOwnerCreateError::DomainExhausted
}

fn record_storage_exhaustion(class: StorageClass) -> TransportOwnerCreateError {
    let (counter, name) = match class {
        StorageClass::ResourceRows => (&TRANSPORT_STORAGE_EXHAUSTED[0], b"CtStoRes" as &[u8]),
        StorageClass::ContextRows => (&TRANSPORT_STORAGE_EXHAUSTED[1], b"CtStoCtx" as &[u8]),
        StorageClass::PairRows => (&TRANSPORT_STORAGE_EXHAUSTED[2], b"CtStoPair" as &[u8]),
        StorageClass::WindowRows => (&TRANSPORT_STORAGE_EXHAUSTED[3], b"CtStoWin" as &[u8]),
        StorageClass::ResourceTickets => (&TRANSPORT_STORAGE_EXHAUSTED[4], b"CtTicRes" as &[u8]),
        StorageClass::ContextTickets => (&TRANSPORT_STORAGE_EXHAUSTED[5], b"CtTicCtx" as &[u8]),
        StorageClass::PairTickets => (&TRANSPORT_STORAGE_EXHAUSTED[6], b"CtTicPair" as &[u8]),
        StorageClass::WindowTickets => (&TRANSPORT_STORAGE_EXHAUSTED[7], b"CtTicWin" as &[u8]),
        StorageClass::Shape => (&TRANSPORT_STORAGE_EXHAUSTED[8], b"CtStoShape" as &[u8]),
    };
    let count = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    crate::diag::record_named_bytes(name, count);
    crate::diag::record(STORAGE_DIAG_BASE + class as u32);
    TransportOwnerCreateError::StorageExhausted
}

fn record_transition_refusal(counter: &AtomicU32, name: &[u8], code: u32) {
    let count = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    crate::diag::record_named_bytes(name, count);
    crate::diag::record(code);
}

fn publish_window_availability(unavailable: Option<DormantTransportUnavailable>) {
    match unavailable {
        Some(DormantTransportUnavailable::NoWindow) => {
            TRANSPORT_OWNER_WINDOW_UNAVAILABLE[1].store(0, Ordering::Relaxed);
            crate::diag::record_named_bytes(b"CtBadWnd", 0);
            record_transition_refusal(
                &TRANSPORT_OWNER_WINDOW_UNAVAILABLE[0],
                b"CtNoWnd",
                TRANSITION_DIAG_BASE + 2,
            );
        }
        Some(DormantTransportUnavailable::InvalidBounds) => {
            TRANSPORT_OWNER_WINDOW_UNAVAILABLE[0].store(0, Ordering::Relaxed);
            crate::diag::record_named_bytes(b"CtNoWnd", 0);
            record_transition_refusal(
                &TRANSPORT_OWNER_WINDOW_UNAVAILABLE[1],
                b"CtBadWnd",
                TRANSITION_DIAG_BASE + 3,
            );
        }
        None => {
            TRANSPORT_OWNER_WINDOW_UNAVAILABLE[0].store(0, Ordering::Relaxed);
            TRANSPORT_OWNER_WINDOW_UNAVAILABLE[1].store(0, Ordering::Relaxed);
            crate::diag::record_named_bytes(b"CtNoWnd", 0);
            crate::diag::record_named_bytes(b"CtBadWnd", 0);
        }
    }
}
