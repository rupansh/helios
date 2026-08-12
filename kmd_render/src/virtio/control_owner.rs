//! Dormant KMD storage for the pure control-owner model.
//! Identity descends from the unique transport domain; the adapter address is
//! only a one-shot construction guard and grants no authority.

use alloc::vec::Vec;
use core::mem::size_of;
use core::num::{NonZeroU64, NonZeroUsize};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::control_owner_slots::SlotTableRoot;
use helios_kmd_logic::control_owner_table::{
    ContextOwnerSlot, ContextOwnerTicket, DormantOwnerSeed, DormantOwnerState,
    DormantTransportObservation, DormantTransportRemoval, DormantTransportUnavailable, OwnerConfig,
    PairOwnerSlot, PairOwnerTicket, ResourceOwnerSlot, ResourceOwnerTicket, WindowOwnerSlot,
    WindowOwnerTicket,
};
use helios_kmd_logic::control_ownership::{
    TransportDomainRoot, TransportGeneration as OwnershipGeneration,
};

use crate::adapter::AdapterContext;
use crate::sync::SpinLock;

static TRANSPORT_DOMAIN_HIGH_WATER: AtomicU64 = AtomicU64::new(0);
pub(crate) static TRANSPORT_DOMAIN_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_REBIND_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_OBSERVE_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_REMOVE_REFUSED: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_INSTANCE_INVALID: AtomicU32 = AtomicU32::new(0);
static TRANSPORT_OWNER_WINDOW_UNAVAILABLE: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
static TRANSPORT_STORAGE_EXHAUSTED: [AtomicU32; 9] = [const { AtomicU32::new(0) }; 9];

const STORAGE_DIAG_BASE: u32 = 0x0A00_00E6;
const TRANSITION_DIAG_BASE: u32 = 0x0A00_00F0;
const _: () = assert!(TRANSITION_DIAG_BASE > STORAGE_DIAG_BASE + 8);

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

struct ResourceCustody {
    _identity: NonZeroUsize,
}

struct ContextCustody {
    _identity: NonZeroUsize,
}

struct AssociationCustody {
    _identity: NonZeroUsize,
}

struct WindowCustody {
    _identity: NonZeroUsize,
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

const _: () = assert!(OWNER_STORAGE_BYTES == 18_571_264);
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
    seed: DormantOwnerSeed,
    _resources: Vec<ResourceSlot>,
    _contexts: Vec<ContextSlot>,
    _pairs: Vec<PairSlot>,
    _windows: Vec<WindowSlot>,
    _resource_tickets: Vec<ResourceTicket>,
    _context_tickets: Vec<ContextTicket>,
    _pair_tickets: Vec<PairTicket>,
    _window_tickets: Vec<WindowTicket>,
}

impl OwnerStorageArena {
    fn allocate(
        slot_root: SlotTableRoot,
        generation: OwnershipGeneration,
    ) -> Result<Self, TransportOwnerCreateError> {
        // SAFETY: both values descend from the one freshly minted `domain`
        // inside `TransportOwner::unbound`; neither has descendants yet.
        let seed = unsafe { DormantOwnerSeed::new(slot_root, generation) };
        let arena = Self {
            seed,
            _resources: allocate_exact(
                RESOURCE_CAPACITY,
                StorageClass::ResourceRows,
                ResourceSlot::vacant,
            )?,
            _contexts: allocate_exact(
                CONTEXT_CAPACITY,
                StorageClass::ContextRows,
                ContextSlot::vacant,
            )?,
            _pairs: allocate_exact(PAIR_CAPACITY, StorageClass::PairRows, PairSlot::vacant)?,
            _windows: allocate_exact(
                WINDOW_CAPACITY,
                StorageClass::WindowRows,
                WindowSlot::vacant,
            )?,
            _resource_tickets: allocate_exact(
                RESOURCE_CAPACITY,
                StorageClass::ResourceTickets,
                ResourceTicket::empty,
            )?,
            _context_tickets: allocate_exact(
                CONTEXT_CAPACITY,
                StorageClass::ContextTickets,
                ContextTicket::empty,
            )?,
            _pair_tickets: allocate_exact(
                PAIR_CAPACITY,
                StorageClass::PairTickets,
                PairTicket::empty,
            )?,
            _window_tickets: allocate_exact(
                WINDOW_CAPACITY,
                StorageClass::WindowTickets,
                WindowTicket::empty,
            )?,
        };
        if !arena.has_exact_shape() {
            return Err(record_storage_exhaustion(StorageClass::Shape));
        }
        Ok(arena)
    }

    fn has_exact_shape(&self) -> bool {
        self._resources.len() == RESOURCE_CAPACITY
            && self._resources.capacity() >= RESOURCE_CAPACITY
            && self._contexts.len() == CONTEXT_CAPACITY
            && self._contexts.capacity() >= CONTEXT_CAPACITY
            && self._pairs.len() == PAIR_CAPACITY
            && self._pairs.capacity() >= PAIR_CAPACITY
            && self._windows.len() == WINDOW_CAPACITY
            && self._windows.capacity() >= WINDOW_CAPACITY
            && self._resource_tickets.len() == self._resources.len()
            && self._context_tickets.len() == self._contexts.len()
            && self._pair_tickets.len() == self._pairs.len()
            && self._window_tickets.len() == self._windows.len()
    }
}

impl Drop for OwnerStorageArena {
    fn drop(&mut self) {
        self._window_tickets.clear();
        self._pair_tickets.clear();
        self._context_tickets.clear();
        self._resource_tickets.clear();
        self._windows.clear();
        self._pairs.clear();
        self._contexts.clear();
        self._resources.clear();
    }
}

struct OwnerState {
    construction_address: usize,
    _storage: OwnerStorageArena,
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
        let storage = OwnerStorageArena::allocate(slot_root, generation)?;
        Ok(Self {
            state: SpinLock::new(OwnerState {
                construction_address: 0,
                _storage: storage,
            }),
        })
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
        crate::diag::record_named_bytes(b"CtObsRef", 0);
        crate::diag::record_named_bytes(b"CtRemRef", 0);
        crate::diag::record_named_bytes(b"CtInst0", 0);
        publish_window_availability(None);
    }

    /// Record inert geometry/provenance for one successfully initialized local
    /// transport. This creates no operational owner table or storage view.
    ///
    /// # Safety
    /// `adapter` is this owner's bound adapter and `gpu` is the exact local
    /// transport candidate consumed by its serialized install transition.
    pub(crate) unsafe fn observe_initialized_transport(
        &self,
        _passive: crate::irql::PassiveLevel,
        adapter: NonNull<AdapterContext>,
        gpu: &super::VirtioGpu,
    ) {
        let Some(physical_instance) = NonZeroU64::new(gpu.scanout_transport_instance()) else {
            record_transition_refusal(
                &TRANSPORT_OWNER_INSTANCE_INVALID,
                b"CtInst0",
                TRANSITION_DIAG_BASE,
            );
            return;
        };
        let (geometry, unavailable) = match gpu.host_visible() {
            None => (
                Err(DormantTransportUnavailable::NoWindow),
                Some(DormantTransportUnavailable::NoWindow),
            ),
            Some(window) => match OwnerConfig::new(0, window.len, super::gpu::BLOB_PAGE) {
                Ok(config) => (Ok(config), None),
                Err(_) => (
                    Err(DormantTransportUnavailable::InvalidBounds),
                    Some(DormantTransportUnavailable::InvalidBounds),
                ),
            },
        };
        let refused = {
            let mut state = self.state.lock();
            if state.construction_address != adapter.as_ptr() as usize {
                true
            } else {
                let table = state._storage.seed.table();
                let epoch = state._storage.seed.epoch();
                // SAFETY: `gpu` is the exact successfully initialized local
                // transport for this adapter, its instance allocator never
                // wraps or reuses, and `table`/`epoch` come from the consuming
                // seed while its owner lock is held.
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
                state._storage.seed.observe_transport(observation).is_err()
            }
        };
        if refused {
            record_transition_refusal(
                &TRANSPORT_OWNER_OBSERVE_REFUSED,
                b"CtObsRef",
                TRANSITION_DIAG_BASE + 1,
            );
            return;
        }
        publish_window_availability(unavailable);
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
            } else {
                let table = state._storage.seed.table();
                let epoch = state._storage.seed.epoch();
                // SAFETY: the caller extracted the exact transport from this
                // adapter's slot under `virtio_lock`; no later producer can
                // reach it. The seed supplies its own exact identity here.
                let removal = unsafe {
                    DormantTransportRemoval::assume_removed(table, epoch, physical_instance)
                };
                state
                    ._storage
                    .seed
                    .observe_transport_removed(removal)
                    .is_err()
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
                || state._storage.seed.state() != DormantOwnerState::NoTransport
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
) -> Result<Vec<T>, TransportOwnerCreateError>
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
    Ok(entries)
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
