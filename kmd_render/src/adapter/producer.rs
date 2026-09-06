//! Allocation-bound producer status. Lock order: virtio -> producer. The leaf
//! lock does no allocation, callback reference release, wait or registry IO.
//! Dxgkrnl references protect binding resolution only. Thereafter a binding
//! retains our status object, so DestroyAllocation can invalidate it without
//! waiting for DestroyDevice to release a circular allocation reference.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use helios_kmd_logic::producer_completion::{
    self as logic, Error, Key, Opens, Predicate, Table, Wait, Waiters,
};
use helios_protocol::{HeliosProducerStatus, HELIOS_PRODUCER_SLOTS};
use wdk_sys::ntddk::{KeSetEvent, MmAllocateContiguousMemory, ObDereferenceObjectDeferDelete};
use wdk_sys::{KEVENT, PHYSICAL_ADDRESS};

const BINDINGS: usize = 16384;
const OPENS: usize = 32768;
const WAITS: usize = 1024;
pub(crate) const MAP_BYTES: usize = HELIOS_PRODUCER_SLOTS * 64;
pub(crate) static REFUSED: AtomicU32 = AtomicU32::new(0);
pub(crate) static PUBLISHED: AtomicU32 = AtomicU32::new(0);
pub(crate) static RETIRED: AtomicU32 = AtomicU32::new(0);

/// WDDM 2.x requires Acquire/ReleaseHandleData for BOTH private-data views.
/// Legacy GetHandleData is rejected by dxgkrnl's WDDM2 validation and calling
/// it with an acquired reference outstanding bugchecks 0x113/0x26 (.263 dump).
/// Keep the reference on this PASSIVE thread and release outside driver locks.
pub(crate) fn with_allocation_reference<T>(
    _passive: crate::irql::PassiveLevel,
    dxg: &crate::dxgk::DXGKRNL_INTERFACE,
    allocation: u32,
    device_specific: bool,
    use_data: impl FnOnce(usize) -> Result<T, Error>,
) -> Result<T, Error> {
    let acquire = dxg.DxgkCbAcquireHandleData.ok_or(Error::Invalid)?;
    let release = dxg.DxgkCbReleaseHandleData.ok_or(Error::Invalid)?;
    let mut query = crate::dxgk::DXGKARGCB_GETHANDLEDATA::default();
    query.Type = crate::dxgk::_DXGK_HANDLE_TYPE::DXGK_HANDLE_ALLOCATION;
    query.hObject = allocation;
    query.Flags.__bindgen_anon_1.Value = u32::from(device_specific);
    logic::with_reference(
        || {
            let mut pin = core::ptr::null_mut();
            // SAFETY: PASSIVE caller, WDK typed runtime handle and valid output.
            // Private data is only compared against our registered objects.
            let data = unsafe { acquire(&query, &mut pin) };
            (data as usize, pin as usize)
        },
        |pin| {
            // SAFETY: exactly one release for this acquired allocation reference,
            // on the acquiring PASSIVE thread after use_data dropped all locks.
            unsafe {
                release(crate::dxgk::DXGKARGCB_RELEASEHANDLEDATA {
                    ReleaseHandle: pin as *mut _,
                    Type: crate::dxgk::_DXGK_HANDLE_TYPE::DXGK_HANDLE_ALLOCATION,
                })
            }
        },
        use_data,
    )
}

#[derive(Clone, Copy)]
struct Binding {
    token: u64,
    owner: usize,
    key: Key,
}

struct State {
    table: Table,
    opens: Opens,
    bindings: Vec<Option<Binding>>,
    waits: Waiters,
    next_binding: u64,
}

fn slots<T: Clone>(count: usize, empty: T) -> Result<Vec<T>, Error> {
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| Error::Capacity)?;
    entries.resize(count, empty);
    Ok(entries)
}

impl State {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            table: Table::new(HELIOS_PRODUCER_SLOTS, 8192, 16384)?,
            opens: Opens::new(OPENS)?,
            bindings: slots(BINDINGS, None)?,
            waits: Waiters::new(WAITS)?,
            next_binding: 0,
        })
    }

    fn binding(&self, owner: usize, token: u64) -> Result<Binding, Error> {
        self.bindings
            .iter()
            .flatten()
            .find(|b| b.owner == owner && b.token == token)
            .copied()
            .ok_or(Error::Invalid)
    }

    fn wake(&mut self, owner: Option<usize>, binding: Option<u64>) {
        self.waits.drain(&self.table, owner, binding, |e| {
            // SAFETY: Waiters transfers each owned KEVENT reference once;
            // non-waiting signal/deferred dereference are legal up to DISPATCH.
            unsafe {
                KeSetEvent(e.event as *mut KEVENT, 0, 0);
                ObDereferenceObjectDeferDelete(e.event as *mut _);
            }
        });
    }
}

/// Atomic view is byte-identical to the public read-only ABI.
#[repr(C, align(64))]
struct Shared {
    sequence: AtomicU64,
    generation: AtomicU64,
    announced: AtomicU64,
    completed: AtomicU64,
    status: AtomicU32,
    reserved: [AtomicU32; 7],
}
const _: () = {
    assert!(core::mem::size_of::<Shared>() == core::mem::size_of::<HeliosProducerStatus>());
    assert!(
        core::mem::offset_of!(Shared, status)
            == core::mem::offset_of!(HeliosProducerStatus, status)
    );
};

pub(crate) struct ProducerCompletion {
    state: crate::sync::SpinLock<Option<State>>,
    page: AtomicUsize,
    transport: AtomicU32,
}

impl ProducerCompletion {
    pub fn new() -> Self {
        Self {
            state: crate::sync::SpinLock::new(None),
            page: AtomicUsize::new(0),
            transport: AtomicU32::new(0),
        }
    }

    /// StartDevice only; the pages survive Stop until every device mapping is gone.
    pub fn init(&self) -> bool {
        if self.page.load(Ordering::Acquire) != 0 {
            return true;
        }
        let Ok(state) = State::new() else {
            return false;
        };
        // SAFETY: POD physical address, PASSIVE PnP initialization.
        let mut highest: PHYSICAL_ADDRESS = unsafe { core::mem::zeroed() };
        highest.QuadPart = i64::MAX;
        // SAFETY: PASSIVE; page-aligned nonpaged storage, or null on failure.
        let page = unsafe { MmAllocateContiguousMemory(MAP_BYTES as u64, highest) } as usize;
        if page == 0 {
            return false;
        }
        // SAFETY: exclusively owned, unpublished allocation; do not expose pool bytes.
        unsafe { core::ptr::write_bytes(page as *mut u8, 0, MAP_BYTES) };
        *self.state.lock() = Some(state);
        self.page.store(page, Ordering::Release);
        true
    }

    pub fn start_transport(&self) -> Option<u32> {
        self.transport
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| x.checked_add(1))
            .ok()
            .map(|x| x + 1)
    }

    pub fn page(&self) -> Option<usize> {
        let p = self.page.load(Ordering::Acquire);
        (p != 0).then_some(p)
    }

    pub fn take_page(&self) -> usize {
        self.page.swap(0, Ordering::AcqRel)
    }

    fn sync(&self, s: &mut State) {
        let Some(page) = self.page() else {
            return;
        };
        while let Some((slot, value)) = s.table.take_changed() {
            // SAFETY: slot comes from the bounded table; the nonpaged mapping
            // remains alive until AdapterContext::drop after all devices/DPCs.
            let out = unsafe { &*((page as *const Shared).add(slot as usize)) };
            out.sequence.fetch_add(1, Ordering::AcqRel);
            out.generation.store(value.generation, Ordering::Relaxed);
            out.announced.store(value.announced, Ordering::Relaxed);
            out.completed.store(value.completed, Ordering::Relaxed);
            out.status.store(value.status, Ordering::Relaxed);
            out.sequence.fetch_add(1, Ordering::Release);
        }
        s.wake(None, None);
    }

    pub fn register_allocation(&self, pointer: usize) -> Result<Key, Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let key = s.table.register(pointer)?;
        self.sync(s);
        Ok(key)
    }

    pub fn remove_allocation(&self, pointer: usize) {
        let mut guard = self.state.lock();
        if let Some(s) = guard.as_mut() {
            if let Some(key) = s.table.find(pointer) {
                s.table.remove(key);
            }
            self.sync(s);
        }
    }

    /// Associate the exact open we will return with a referenced global allocation.
    pub fn register_open(
        &self,
        pointer: usize,
        process: usize,
        global: usize,
    ) -> Result<(), Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let key = s.table.find(global).ok_or(Error::Invalid)?;
        s.opens.register(pointer, process, key)
    }

    pub fn remove_open(&self, pointer: usize) {
        let mut guard = self.state.lock();
        if let Some(s) = guard.as_mut() {
            s.opens.remove(pointer);
        }
    }

    /// Caller holds an acquired device-specific allocation reference through this
    /// lookup and retain. The global association was fixed in OpenAllocation.
    pub fn bind(&self, owner: usize, process: usize, open: usize) -> Result<(u64, Key), Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let key = s.opens.resolve(open, process)?;
        let i = s
            .bindings
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let token = s.next_binding.checked_add(1).ok_or(Error::Capacity)?;
        s.table.retain(key)?;
        s.next_binding = token;
        s.bindings[i] = Some(Binding { token, owner, key });
        Ok((token, key))
    }

    pub fn publish(
        &self,
        owner: usize,
        token: u64,
        stream: u64,
        value: u32,
        ready: bool,
    ) -> Result<u64, Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let b = s.binding(owner, token)?;
        let result = s.table.publish(b.key, stream, value, ready);
        self.sync(s);
        if result.is_ok() {
            PUBLISHED.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    pub fn complete(&self, stream: u64, value: u32) {
        let mut guard = self.state.lock();
        if let Some(s) = guard.as_mut() {
            s.table.complete(stream, value);
            self.sync(s);
            RETIRED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn fail_stream(&self, stream: u64) {
        let mut guard = self.state.lock();
        if let Some(s) = guard.as_mut() {
            s.table.fail_stream(stream, logic::FAILED);
            self.sync(s);
        }
    }

    /// On success the table takes the caller's event reference only for Pending.
    pub fn wait(
        &self,
        owner: usize,
        token: u64,
        epoch: u64,
        event: usize,
    ) -> Result<Predicate, Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let b = s.binding(owner, token)?;
        s.waits.register(
            &s.table,
            Wait {
                binding: token,
                owner,
                key: b.key,
                epoch,
                event,
            },
        )
    }

    /// Transfers the stored reference to the caller for PASSIVE dereference.
    pub fn cancel(&self, owner: usize, token: u64, event: usize) -> Option<usize> {
        let mut guard = self.state.lock();
        let s = guard.as_mut()?;
        s.waits.cancel(owner, token, event).map(|w| w.event)
    }

    pub fn abort(&self, owner: usize, token: u64) -> Result<(), Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let b = s.binding(owner, token)?;
        s.table.terminal(b.key, logic::FAILED);
        self.sync(s);
        Ok(())
    }

    pub fn release(&self, owner: usize, token: u64) -> Result<(), Error> {
        let mut guard = self.state.lock();
        let s = guard.as_mut().ok_or(Error::Capacity)?;
        let i = s
            .bindings
            .iter()
            .position(|b| b.is_some_and(|b| b.owner == owner && b.token == token))
            .ok_or(Error::Invalid)?;
        let b = s.bindings[i].take().ok_or(Error::Invalid)?;
        // Closing a consumer binding cancels only its waits. It cannot complete
        // or poison another process's producer operation.
        s.wake(Some(owner), Some(token));
        s.table.release(b.key)
    }

    pub fn remove_device(&self, owner: usize) {
        let mut guard = self.state.lock();
        if let Some(s) = guard.as_mut() {
            s.wake(Some(owner), None);
            for b in &mut s.bindings {
                if let Some(entry) = *b {
                    if entry.owner == owner {
                        *b = None;
                        let _ = s.table.release(entry.key);
                    }
                }
            }
        }
    }

    pub fn reset(&self) {
        let mut guard = self.state.lock();
        if let Some(s) = guard.as_mut() {
            s.table.reset();
            self.sync(s);
        }
    }
}
