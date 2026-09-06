//! Allocation-scoped producer completion. The KMD serializes this production
//! state with publication, retirement and event registration under one lock.
//! IDs below are resolved, retained dxgkrnl allocations, never resource IDs.
//! Storage is reserved at PASSIVE; every operation after `new` is allocation-free.

extern crate alloc;
use alloc::vec::Vec;

pub const LIVE: u32 = 0;
pub const CANCELLED: u32 = 1;
pub const FAILED: u32 = 2;
pub const REMOVED: u32 = 3;
const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub slot: u32,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub generation: u64,
    pub announced: u64,
    pub completed: u64,
    pub status: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Capacity,
    Terminal(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predicate {
    Ready,
    Pending,
    Terminal(u32),
}

#[derive(Clone, Copy)]
struct Open {
    pointer: usize,
    process: usize,
    key: Key,
}

/// OpenAllocation associates its new private pointer with the global state under
/// an acquired dxgkrnl allocation reference. Binding resolves only the exact open
/// under its own reference; it never pairs two independently resolved handles.
pub struct Opens {
    entries: Vec<Option<Open>>,
}

impl Opens {
    pub fn new(capacity: usize) -> Result<Self, Error> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Capacity)?;
        entries.resize(capacity, None);
        Ok(Self { entries })
    }

    pub fn register(&mut self, pointer: usize, process: usize, key: Key) -> Result<(), Error> {
        if pointer == 0
            || key.generation == 0
            || self.entries.iter().flatten().any(|o| o.pointer == pointer)
        {
            return Err(Error::Invalid);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|o| o.is_none())
            .ok_or(Error::Capacity)?;
        *entry = Some(Open {
            pointer,
            process,
            key,
        });
        Ok(())
    }

    /// Caller holds the exact acquired runtime reference through this lookup
    /// and Table::retain, which rejects a removed or reused allocation generation.
    pub fn resolve(&self, pointer: usize, process: usize) -> Result<Key, Error> {
        if process == 0 {
            return Err(Error::Invalid);
        }
        let entry = self
            .entries
            .iter()
            .flatten()
            .find(|o| o.pointer == pointer && o.process == process)
            .ok_or(Error::Invalid)?;
        Ok(entry.key)
    }

    pub fn remove(&mut self, pointer: usize) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|o| o.is_some_and(|o| o.pointer == pointer))
        {
            *entry = None;
        }
    }
}

/// Scope one OS allocation reference around a status operation. A nonzero
/// release token owns a reference even if the requested private data is null.
/// Neither the private pointer nor a release token becomes a binding identity.
pub fn with_reference<T>(
    acquire: impl FnOnce() -> (usize, usize),
    release: impl FnOnce(usize),
    use_data: impl FnOnce(usize) -> Result<T, Error>,
) -> Result<T, Error> {
    struct Reference<F: FnOnce(usize)> {
        token: usize,
        release: Option<F>,
    }
    impl<F: FnOnce(usize)> Drop for Reference<F> {
        fn drop(&mut self) {
            if self.token != 0 {
                if let Some(release) = self.release.take() {
                    release(self.token);
                }
            }
        }
    }
    let (data, token) = acquire();
    let _reference = Reference {
        token,
        release: Some(release),
    };
    if data == 0 || token == 0 {
        return Err(Error::Invalid);
    }
    use_data(data)
}

#[derive(Clone, Copy)]
struct Allocation {
    id: usize,
    bindings: u32,
    dirty: bool,
    snapshot: Snapshot,
    head: usize,
    tail: usize,
}

impl Allocation {
    const EMPTY: Self = Self {
        id: 0,
        bindings: 0,
        dirty: false,
        snapshot: Snapshot {
            generation: 0,
            announced: 0,
            completed: 0,
            status: REMOVED,
        },
        head: NONE,
        tail: NONE,
    };
}

#[derive(Clone, Copy)]
struct Pending {
    key: Key,
    epoch: u64,
    stream: u64,
    value: u32,
    next: usize,
    done: bool,
}

#[derive(Clone, Copy)]
struct Writer {
    key: Key,
    stream: u64,
    value: u32,
}

pub struct Table {
    allocations: Vec<Allocation>,
    pending: Vec<Option<Pending>>,
    writers: Vec<Option<Writer>>,
    generation: u64,
    dirty: Vec<u32>,
}

impl Table {
    /// Only construction allocates. No kernel stack-sized inline arrays.
    pub fn new(allocations: usize, pending: usize, writers: usize) -> Result<Self, Error> {
        if allocations == 0 || allocations > u32::MAX as usize || pending == 0 || writers == 0 {
            return Err(Error::Invalid);
        }
        let mut a = Vec::new();
        let mut p = Vec::new();
        let mut w = Vec::new();
        let mut dirty = Vec::new();
        a.try_reserve_exact(allocations)
            .map_err(|_| Error::Capacity)?;
        p.try_reserve_exact(pending).map_err(|_| Error::Capacity)?;
        w.try_reserve_exact(writers).map_err(|_| Error::Capacity)?;
        dirty
            .try_reserve_exact(allocations)
            .map_err(|_| Error::Capacity)?;
        a.resize(allocations, Allocation::EMPTY);
        p.resize(pending, None);
        w.resize(writers, None);
        Ok(Self {
            allocations: a,
            pending: p,
            writers: w,
            generation: 0,
            dirty,
        })
    }

    pub fn find(&self, id: usize) -> Option<Key> {
        if id == 0 {
            return None;
        }
        self.allocations
            .iter()
            .enumerate()
            .find(|(_, a)| a.id == id)
            .map(|(slot, a)| Key {
                slot: slot as u32,
                generation: a.snapshot.generation,
            })
    }

    pub fn register(&mut self, id: usize) -> Result<Key, Error> {
        if id == 0 || self.find(id).is_some() {
            return Err(Error::Invalid);
        }
        let slot = self
            .allocations
            .iter()
            .position(|a| a.id == 0 && a.bindings == 0)
            .ok_or(Error::Capacity)?;
        let generation = self.generation.checked_add(1).ok_or(Error::Capacity)?;
        self.generation = generation;
        self.allocations[slot] = Allocation {
            id,
            bindings: 0,
            dirty: self.allocations[slot].dirty,
            snapshot: Snapshot {
                generation,
                announced: 0,
                completed: 0,
                status: LIVE,
            },
            head: NONE,
            tail: NONE,
        };
        self.changed(slot);
        Ok(Key {
            slot: slot as u32,
            generation,
        })
    }

    pub fn snapshot(&self, key: Key) -> Result<Snapshot, Error> {
        let a = self
            .allocations
            .get(key.slot as usize)
            .ok_or(Error::Invalid)?;
        if key.generation == 0 || a.snapshot.generation != key.generation {
            return Err(Error::Invalid);
        }
        Ok(a.snapshot)
    }

    pub fn retain(&mut self, key: Key) -> Result<(), Error> {
        let s = self.snapshot(key)?;
        if s.status != LIVE {
            return Err(Error::Terminal(s.status));
        }
        let a = &mut self.allocations[key.slot as usize];
        a.bindings = a.bindings.checked_add(1).ok_or(Error::Capacity)?;
        Ok(())
    }

    pub fn release(&mut self, key: Key) -> Result<(), Error> {
        self.snapshot(key)?;
        let a = &mut self.allocations[key.slot as usize];
        a.bindings = a.bindings.checked_sub(1).ok_or(Error::Invalid)?;
        Ok(())
    }

    pub fn predicate(&self, key: Key, epoch: u64) -> Result<Predicate, Error> {
        let s = self.snapshot(key)?;
        if epoch > s.announced {
            return Err(Error::Invalid);
        }
        // Terminal state takes precedence even for an old completed target.
        if s.status != LIVE {
            return Ok(Predicate::Terminal(s.status));
        }
        Ok(if s.completed >= epoch {
            Predicate::Ready
        } else {
            Predicate::Pending
        })
    }

    /// `stream` is a generation-qualified registration, not its slot index.
    /// `already_complete` is supplied only by that exact stream's retirement
    /// proof under the same serialization as this call. Announcement may precede
    /// submission, and retirement may precede announcement.
    pub fn publish(
        &mut self,
        key: Key,
        stream: u64,
        value: u32,
        already_complete: bool,
    ) -> Result<u64, Error> {
        let s = self.snapshot(key)?;
        if s.status != LIVE {
            return Err(Error::Terminal(s.status));
        }
        if stream == 0 || value == 0 {
            return Err(Error::Invalid);
        }
        let known = self
            .writers
            .iter()
            .position(|w| w.is_some_and(|w| w.key == key && w.stream == stream));
        if known.is_some_and(|i| self.writers[i].is_some_and(|w| value <= w.value)) {
            return Err(Error::Invalid);
        }
        let writer = known
            .or_else(|| self.writers.iter().position(Option::is_none))
            .ok_or(Error::Capacity)?;
        let pending = self
            .pending
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        let epoch = s.announced.checked_add(1).ok_or(Error::Capacity)?;
        // All failure checks precede the first mutation: a rejected publication
        // never leaves an announced epoch with no dependency.
        self.writers[writer] = Some(Writer { key, stream, value });
        self.pending[pending] = Some(Pending {
            key,
            epoch,
            stream,
            value,
            next: NONE,
            done: already_complete,
        });
        let a = &mut self.allocations[key.slot as usize];
        if let Some(tail) = self.pending.get_mut(a.tail).and_then(Option::as_mut) {
            tail.next = pending;
        } else {
            a.head = pending;
        }
        a.tail = pending;
        a.snapshot.announced = epoch;
        self.advance(key);
        Ok(epoch)
    }

    fn advance(&mut self, key: Key) {
        let a = &mut self.allocations[key.slot as usize];
        while let Some(p) = self.pending.get(a.head).copied().flatten() {
            if !p.done {
                break;
            }
            a.snapshot.completed = p.epoch;
            self.pending[a.head] = None;
            a.head = p.next;
        }
        if a.head == NONE {
            a.tail = NONE;
        }
        self.changed(key.slot as usize);
    }

    /// Complete only the exact operation whose host response was successful.
    /// Across queues, a higher resource epoch proves nothing about an earlier
    /// one. The per-resource linked prefix keeps unrelated resources moving.
    pub fn complete(&mut self, stream: u64, value: u32) {
        for i in 0..self.pending.len() {
            let Some(p) = self.pending[i] else {
                continue;
            };
            if p.stream == stream && p.value == value {
                if let Some(entry) = &mut self.pending[i] {
                    entry.done = true;
                }
                self.advance(p.key);
            }
        }
    }

    pub fn fail_stream(&mut self, stream: u64, status: u32) {
        if status == LIVE {
            return;
        }
        // Mark affected allocations first, then sweep pending entries once.
        // Reset/stream teardown must stay linear while the kernel lock is held.
        for i in 0..self.pending.len() {
            if let Some(p) = self.pending[i] {
                if p.stream == stream {
                    let a = &mut self.allocations[p.key.slot as usize];
                    if a.snapshot.status == LIVE {
                        a.snapshot.status = status;
                    }
                    a.head = NONE;
                    a.tail = NONE;
                    self.changed(p.key.slot as usize);
                }
            }
        }
        for p in &mut self.pending {
            if p.is_some_and(|p| self.allocations[p.key.slot as usize].snapshot.status != LIVE) {
                *p = None;
            }
        }
        for w in &mut self.writers {
            if w.is_some_and(|w| w.stream == stream) {
                *w = None;
            }
        }
    }

    pub fn terminal(&mut self, key: Key, status: u32) {
        if self.snapshot(key).is_err() || status == LIVE {
            return;
        }
        let a = &mut self.allocations[key.slot as usize];
        if a.snapshot.status == LIVE {
            a.snapshot.status = status;
        }
        // Failure/cancellation never manufactures a completion.
        a.head = NONE;
        a.tail = NONE;
        for p in &mut self.pending {
            if p.is_some_and(|p| p.key == key) {
                *p = None;
            }
        }
        self.changed(key.slot as usize);
    }

    /// Called only when dxgkrnl destroys the allocation, after every acquired
    /// reference has been released. The mapped storage itself remains alive.
    pub fn remove(&mut self, key: Key) {
        if self.snapshot(key).is_err() {
            return;
        }
        self.terminal(key, REMOVED);
        self.allocations[key.slot as usize].id = 0;
        for w in &mut self.writers {
            if w.is_some_and(|w| w.key == key) {
                *w = None;
            }
        }
    }

    pub fn reset(&mut self) {
        for i in 0..self.allocations.len() {
            let a = &mut self.allocations[i];
            if a.id != 0 {
                if a.snapshot.status == LIVE {
                    a.snapshot.status = REMOVED;
                }
                a.head = NONE;
                a.tail = NONE;
                self.changed(i);
            }
        }
        self.pending.fill(None);
        self.writers.fill(None);
    }

    pub fn slots(&self) -> usize {
        self.allocations.len()
    }
    pub fn slot_snapshot(&self, slot: usize) -> Option<Snapshot> {
        self.allocations.get(slot).map(|a| a.snapshot)
    }

    fn changed(&mut self, slot: usize) {
        if !self.allocations[slot].dirty {
            self.allocations[slot].dirty = true;
            self.dirty.push(slot as u32);
        }
    }

    pub fn take_changed(&mut self) -> Option<(u32, Snapshot)> {
        let slot = self.dirty.pop()?;
        let a = &mut self.allocations[slot as usize];
        a.dirty = false;
        Some((slot, a.snapshot))
    }
}

/// Referenced-event ownership without OS operations. The caller holds the same
/// lock for Table changes, registration and draining; the returned event is
/// transferred exactly once, either to cancellation or to the wake callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    pub binding: u64,
    pub owner: usize,
    pub key: Key,
    pub epoch: u64,
    pub event: usize,
}

pub struct Waiters {
    entries: Vec<Option<Wait>>,
}

impl Waiters {
    pub fn new(capacity: usize) -> Result<Self, Error> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| Error::Capacity)?;
        entries.resize(capacity, None);
        Ok(Self { entries })
    }

    pub fn register(&mut self, table: &Table, wait: Wait) -> Result<Predicate, Error> {
        if wait.event == 0 || wait.binding == 0 || wait.owner == 0 {
            return Err(Error::Invalid);
        }
        let predicate = table.predicate(wait.key, wait.epoch)?;
        if predicate == Predicate::Pending {
            if self
                .entries
                .iter()
                .flatten()
                .any(|w| w.owner == wait.owner && w.event == wait.event)
            {
                return Err(Error::Invalid);
            }
            let slot = self
                .entries
                .iter_mut()
                .find(|w| w.is_none())
                .ok_or(Error::Capacity)?;
            *slot = Some(wait);
        }
        Ok(predicate)
    }

    pub fn cancel(&mut self, owner: usize, binding: u64, event: usize) -> Option<Wait> {
        self.entries
            .iter_mut()
            .find(|w| {
                w.is_some_and(|w| w.owner == owner && w.binding == binding && w.event == event)
            })?
            .take()
    }

    pub fn drain(
        &mut self,
        table: &Table,
        owner: Option<usize>,
        binding: Option<u64>,
        mut wake: impl FnMut(Wait),
    ) {
        for slot in &mut self.entries {
            let Some(wait) = *slot else {
                continue;
            };
            let cancelled =
                owner == Some(wait.owner) && (binding.is_none() || binding == Some(wait.binding));
            if cancelled
                || !matches!(
                    table.predicate(wait.key, wait.epoch),
                    Ok(Predicate::Pending)
                )
            {
                *slot = None;
                wake(wait);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table() -> Table {
        Table::new(4, 8, 8).unwrap()
    }
    fn wait(key: Key, epoch: u64) -> Wait {
        Wait {
            owner: 1,
            binding: 1,
            key,
            epoch,
            event: 1,
        }
    }

    #[test]
    fn announcement_before_submission_stays_pending() {
        let mut t = table();
        let a = t.register(11).unwrap();
        assert_eq!(t.publish(a, 101, 77, false), Ok(1));
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Pending));
        t.complete(102, 77);
        t.complete(101, 76); // wrong stream/value
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Pending));
        t.complete(101, 77);
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Ready));
    }

    #[test]
    fn exact_retirement_before_publication_is_ready() {
        let mut t = table();
        let a = t.register(11).unwrap();
        // Production passes the exact registered stream's retirement proof.
        assert_eq!(t.publish(a, 101, 77, true), Ok(1));
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Ready));
    }

    #[test]
    fn out_of_order_resources_advance_independently_and_only_through_prefix() {
        let mut t = table();
        let a = t.register(11).unwrap();
        let b = t.register(12).unwrap();
        t.publish(a, 101, 40, false).unwrap();
        t.publish(a, 202, 1, false).unwrap(); // independent producer's namespace
        t.publish(b, 202, 1, false).unwrap();
        t.complete(202, 1);
        assert_eq!(t.snapshot(a).unwrap().completed, 0);
        assert_eq!(t.snapshot(b).unwrap().completed, 1);
        t.complete(101, 40);
        assert_eq!(t.snapshot(a).unwrap().completed, 2);
    }

    #[test]
    fn completed_later_epoch_does_not_cover_pending_earlier_epoch_on_publication() {
        let mut t = table();
        let a = t.register(11).unwrap();
        t.publish(a, 101, 1, false).unwrap();
        t.publish(a, 202, 6, true).unwrap();
        assert_eq!(t.predicate(a, 2), Ok(Predicate::Pending));
        t.complete(101, 1);
        assert_eq!(t.predicate(a, 2), Ok(Predicate::Ready));
    }

    #[test]
    fn independent_opens_retain_one_allocation_incarnation() {
        let mut t = table();
        let a = t.register(11).unwrap();
        let open1 = t.find(11).unwrap();
        let open2 = t.find(11).unwrap();
        t.retain(open1).unwrap();
        t.retain(open2).unwrap();
        t.publish(open1, 101, 1, false).unwrap();
        assert_eq!(t.snapshot(open2).unwrap().announced, 1);
        t.release(open1).unwrap(); // consumer release is not producer completion
        assert_eq!(t.predicate(open2, 1), Ok(Predicate::Pending));
        t.complete(101, 1);
        assert_eq!(t.snapshot(open2), t.snapshot(a));
    }

    #[test]
    fn stale_generation_and_recycled_pointer_never_inherit_readiness() {
        let mut t = Table::new(1, 4, 4).unwrap();
        let a = t.register(11).unwrap();
        t.retain(a).unwrap();
        t.publish(a, 101, 1, false).unwrap();
        t.remove(a);
        assert_eq!(t.register(11), Err(Error::Capacity)); // mapped binding keeps slot alive
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Terminal(REMOVED)));
        t.release(a).unwrap();
        let b = t.register(11).unwrap();
        assert_ne!(a.generation, b.generation);
        assert_eq!(t.snapshot(a), Err(Error::Invalid));
        t.publish(b, 202, 1, false).unwrap();
        t.complete(101, 1);
        assert_eq!(t.predicate(b, 1), Ok(Predicate::Pending));
    }

    #[test]
    fn rejection_is_atomic_and_stream_values_are_not_resource_epochs() {
        let mut t = Table::new(2, 1, 2).unwrap();
        let a = t.register(11).unwrap();
        t.publish(a, 101, 900, false).unwrap();
        let before = t.snapshot(a).unwrap();
        for (stream, value, error) in [
            (101, 900, Error::Invalid),
            (101, 899, Error::Invalid),
            (0, 1, Error::Invalid),
            (101, 0, Error::Invalid),
            (202, 1, Error::Capacity),
        ] {
            assert_eq!(t.publish(a, stream, value, false), Err(error));
            assert_eq!(t.snapshot(a).unwrap(), before);
        }
        t.complete(101, 900);
        assert_eq!(t.publish(a, 202, 1, false), Ok(2));
    }

    #[test]
    fn producer_failure_cancels_prefix_without_completing_it() {
        let mut t = table();
        let a = t.register(11).unwrap();
        let b = t.register(12).unwrap();
        t.publish(a, 101, 1, true).unwrap();
        t.publish(a, 101, 2, false).unwrap();
        t.publish(a, 202, 5, true).unwrap();
        t.publish(b, 202, 5, true).unwrap();
        t.fail_stream(101, FAILED);
        t.complete(101, 2);
        assert_eq!(t.snapshot(a).unwrap().completed, 1);
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Terminal(FAILED)));
        assert_eq!(t.publish(a, 303, 1, true), Err(Error::Terminal(FAILED)));
        assert_eq!(t.predicate(b, 1), Ok(Predicate::Ready));
    }

    #[test]
    fn register_retire_and_cancel_permutations_transfer_event_once() {
        for retire_first in [false, true] {
            for cancel_first in [false, true] {
                let mut t = table();
                let mut w = Waiters::new(2).unwrap();
                let a = t.register(11).unwrap();
                t.publish(a, 101, 1, false).unwrap();
                let request = wait(a, 1);
                if retire_first {
                    t.complete(101, 1);
                }
                let predicate = w.register(&t, request).unwrap();
                let mut transferred = usize::from(predicate != Predicate::Pending);
                if cancel_first {
                    transferred += usize::from(w.cancel(1, 1, 1).is_some());
                }
                t.complete(101, 1);
                w.drain(&t, None, None, |_| transferred += 1);
                transferred += usize::from(w.cancel(1, 1, 1).is_some());
                w.drain(&t, None, None, |_| panic!("event transferred twice"));
                assert_eq!(transferred, 1);
            }
        }
    }

    #[test]
    fn cancelled_event_reuse_cannot_cancel_a_different_binding() {
        let mut t = table();
        let mut w = Waiters::new(2).unwrap();
        let a = t.register(11).unwrap();
        t.publish(a, 101, 1, false).unwrap();
        let request = wait(a, 1);
        w.register(&t, request).unwrap();
        assert_eq!(w.cancel(2, 1, 1), None);
        assert_eq!(w.cancel(1, 2, 1), None);
        assert_eq!(w.cancel(1, 1, 1), Some(request));
        let next = Wait {
            binding: 2,
            ..request
        };
        w.register(&t, next).unwrap();
        assert_eq!(w.cancel(1, 1, 1), None);
        t.complete(101, 1);
        let mut got = None;
        w.drain(&t, None, None, |x| got = Some(x));
        assert_eq!(got, Some(next));
    }

    #[test]
    fn binding_close_only_cancels_its_own_waiters() {
        let mut t = table();
        let mut w = Waiters::new(3).unwrap();
        let a = t.register(11).unwrap();
        t.publish(a, 101, 1, false).unwrap();
        for binding in 1..=3 {
            w.register(
                &t,
                Wait {
                    binding,
                    event: binding as usize,
                    ..wait(a, 1)
                },
            )
            .unwrap();
        }
        let mut got = Vec::new();
        w.drain(&t, Some(1), Some(2), |x| got.push(x.binding));
        assert_eq!(got, [2]);
        assert_eq!(t.predicate(a, 1), Ok(Predicate::Pending));
        w.drain(&t, Some(1), None, |x| got.push(x.binding));
        assert_eq!(got, [2, 1, 3]);
    }

    #[test]
    fn reset_and_allocation_teardown_wake_pending_without_completion() {
        for reset in [false, true] {
            let mut t = table();
            let mut w = Waiters::new(1).unwrap();
            let a = t.register(11).unwrap();
            t.publish(a, 101, 1, false).unwrap();
            w.register(&t, wait(a, 1)).unwrap();
            if reset {
                t.reset();
            } else {
                t.remove(a);
            }
            let mut count = 0;
            w.drain(&t, None, None, |_| count += 1);
            assert_eq!(count, 1);
            assert_eq!(t.snapshot(a).unwrap().completed, 0);
            assert_eq!(t.predicate(a, 1), Ok(Predicate::Terminal(REMOVED)));
            assert_eq!(w.register(&t, wait(a, 1)), Ok(Predicate::Terminal(REMOVED)));
        }
    }

    #[test]
    fn invalid_future_or_capacity_wait_does_not_take_event_ownership() {
        let mut t = table();
        let mut w = Waiters::new(1).unwrap();
        let a = t.register(11).unwrap();
        t.publish(a, 101, 1, false).unwrap();
        assert_eq!(w.register(&t, wait(a, 2)), Err(Error::Invalid));
        w.register(&t, wait(a, 1)).unwrap();
        assert_eq!(w.register(&t, wait(a, 1)), Err(Error::Invalid));
        assert_eq!(
            w.register(
                &t,
                Wait {
                    event: 2,
                    ..wait(a, 1)
                }
            ),
            Err(Error::Capacity)
        );
        assert_eq!(w.cancel(1, 1, 2), None);
    }

    #[test]
    fn dirty_view_reports_latest_snapshot_once_after_slot_reuse() {
        let mut t = table();
        let a = t.register(11).unwrap();
        t.publish(a, 101, 1, true).unwrap();
        t.remove(a);
        let b = t.register(12).unwrap();
        assert_eq!(a.slot, b.slot);
        assert_eq!(t.take_changed(), Some((b.slot, t.snapshot(b).unwrap())));
        assert_eq!(t.take_changed(), None);
    }

    #[test]
    fn different_process_opens_share_the_registered_global_allocation() {
        let mut t = table();
        let mut opens = Opens::new(3).unwrap();
        let key = t.register(11).unwrap();
        opens.register(101, 1, key).unwrap();
        opens.register(102, 2, key).unwrap();
        t.retain(opens.resolve(101, 1).unwrap()).unwrap();
        t.retain(opens.resolve(102, 2).unwrap()).unwrap();
        t.publish(key, 1001, 1, true).unwrap();
        assert_eq!(t.snapshot(key).unwrap().completed, 1);
        opens.remove(101);
        t.release(key).unwrap();
        assert_eq!(opens.resolve(102, 2), Ok(key));
        assert_eq!(t.snapshot(key).unwrap().completed, 1);
    }

    #[test]
    fn open_resolution_rejects_foreign_process_and_changed_generation() {
        let mut t = table();
        let mut opens = Opens::new(1).unwrap();
        let old = t.register(11).unwrap();
        opens.register(101, 1, old).unwrap();
        assert_eq!(opens.resolve(101, 2), Err(Error::Invalid));
        assert_eq!(opens.resolve(999, 1), Err(Error::Invalid));
        assert_eq!(opens.resolve(101, 1), Ok(old));
        t.remove(old);
        let new = t.register(12).unwrap();
        assert_eq!(old.slot, new.slot);
        // A status slot can be reused, but the open cannot retarget itself.
        assert_eq!(
            t.retain(opens.resolve(101, 1).unwrap()),
            Err(Error::Invalid)
        );
        opens.remove(101);
        assert_eq!(opens.resolve(101, 1), Err(Error::Invalid));
        opens.register(101, 1, new).unwrap();
        assert_eq!(opens.resolve(101, 1), Ok(new));
    }

    #[test]
    fn opens_release_capacity_on_close_and_require_a_process_for_binding() {
        let key = Key {
            slot: 0,
            generation: 1,
        };
        let mut opens = Opens::new(1).unwrap();
        opens.register(101, 0, key).unwrap();
        assert_eq!(opens.resolve(101, 0), Err(Error::Invalid));
        assert_eq!(opens.register(101, 1, key), Err(Error::Invalid));
        assert_eq!(opens.register(102, 1, key), Err(Error::Capacity));
        opens.remove(101);
        opens.register(102, 1, key).unwrap();
    }

    #[test]
    fn acquired_reference_spans_use_and_releases_once_on_success_or_refusal() {
        use core::cell::Cell;
        for result in [Ok(7), Err(Error::Capacity), Err(Error::Terminal(REMOVED))] {
            let held = Cell::new(false);
            let released = Cell::new(0);
            let got = with_reference(
                || {
                    held.set(true);
                    (11, 12)
                },
                |token| {
                    assert_eq!(token, 12);
                    assert!(held.replace(false));
                    released.set(released.get() + 1);
                },
                |data| {
                    assert_eq!(data, 11);
                    assert!(held.get());
                    result
                },
            );
            assert_eq!(got, result);
            assert!(!held.get());
            assert_eq!(released.get(), 1);
        }
    }

    #[test]
    fn null_private_data_still_releases_an_acquired_reference() {
        use core::cell::Cell;
        let released = Cell::new(0);
        let result: Result<(), Error> = with_reference(
            || (0, 12),
            |token| {
                assert_eq!(token, 12);
                released.set(released.get() + 1);
            },
            |_| panic!("null private data must not be used"),
        );
        assert_eq!(result, Err(Error::Invalid));
        assert_eq!(released.get(), 1);
    }

    #[test]
    fn failed_acquire_does_not_release_or_use_unprotected_data() {
        for data in [0, 11] {
            let result: Result<(), Error> = with_reference(
                || (data, 0),
                |_| panic!("no reference was acquired"),
                |_| panic!("unprotected private data must not be used"),
            );
            assert_eq!(result, Err(Error::Invalid));
        }
    }
}
