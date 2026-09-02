//! One-node WDDM scheduler retirement in exact submission-arrival order.
//!
//! Host rings may complete concurrently, but this driver exposes one WDDM
//! render node/engine.  A host completion therefore makes one entry *ready*;
//! it does not by itself authorize a `DXGK_INTERRUPT_DMA_COMPLETED`.  Only the
//! ready head of this bounded frontier may be reported to VidSch.  The KMD
//! owns the callback/lifetime mechanics; this module owns every state
//! transition so the ordering and stale-generation rules have a host oracle.

/// The fixed device-wide frontier depth used by K9.
///
/// It matches both the existing WDDM pending bound and one physical endpoint's
/// bounded host-dispatch FIFO.  Exhaustion is terminal for the current engine
/// generation: completing a newer fence to escape pressure would implicitly
/// and falsely complete every older entry.
pub const MAX_ORDERED_ENGINE_SUBMISSIONS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubmissionTicket {
    epoch: u64,
    serial: u64,
    slot: u32,
}

impl SubmissionTicket {
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    pub const fn serial(self) -> u64 {
        self.serial
    }

    pub const fn slot(self) -> u32 {
        self.slot
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReadySubmission {
    ticket: SubmissionTicket,
    fence: u32,
}

impl ReadySubmission {
    pub const fn ticket(self) -> SubmissionTicket {
        self.ticket
    }

    pub const fn fence(self) -> u32 {
        self.fence
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AdmissionRefusal {
    Closed,
    Poisoned,
    Capacity,
    SerialExhausted,
    SlotIndexExhausted,
    NotForward { previous: u32, found: u32 },
    CorruptOccupiedSlot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompletionDisposition {
    /// The exact live ticket became host-terminal. `retained_early` means an
    /// older scheduler entry remains at the head.
    Marked {
        retained_early: bool,
    },
    AlreadyCompleted,
    StaleEpoch,
    StaleTicket,
    Poisoned,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FailureDisposition {
    Poisoned,
    AlreadyPoisoned,
    StaleEpoch,
    StaleTicket,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RetirementRefusal {
    NotReady,
    WrongHead,
    Poisoned,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Invalidation {
    pub dropped: usize,
    pub epoch_advanced: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Free,
    AwaitingHost,
    HostCompleted,
}

#[derive(Clone, Copy)]
struct Slot {
    epoch: u64,
    serial: u64,
    fence: u32,
    state: SlotState,
}

impl Slot {
    const EMPTY: Self = Self {
        epoch: 0,
        serial: 0,
        fence: 0,
        state: SlotState::Free,
    };

    const fn ticket(self, slot: usize) -> SubmissionTicket {
        SubmissionTicket {
            epoch: self.epoch,
            serial: self.serial,
            slot: slot as u32,
        }
    }

    const fn matches(self, ticket: SubmissionTicket, slot: usize) -> bool {
        ticket.slot as usize == slot
            && self.epoch == ticket.epoch
            && self.serial == ticket.serial
            && !matches!(self.state, SlotState::Free)
    }
}

/// Fixed, allocation-free-after-construction ordered engine state.
///
/// The KMD places the 256-slot instance on the heap and mutates it only while
/// holding its WDDM notification spinlock.  Tests use small inline instances.
pub struct OrderedEngine<const N: usize> {
    slots: [Slot; N],
    head: usize,
    len: usize,
    epoch: u64,
    next_serial: u64,
    last_retired_serial: u64,
    last_admitted_fence: u32,
    open: bool,
    poisoned: bool,
    epoch_exhausted: bool,
}

impl<const N: usize> OrderedEngine<N> {
    pub const fn new() -> Self {
        Self {
            slots: [Slot::EMPTY; N],
            head: 0,
            len: 0,
            epoch: 1,
            next_serial: 0,
            last_retired_serial: 0,
            last_admitted_fence: 0,
            open: false,
            poisoned: false,
            epoch_exhausted: false,
        }
    }

    /// Initialize an already allocated instance without materializing the
    /// multi-KiB slot array on the caller's stack.
    ///
    /// # Safety
    /// `destination` must be non-null, aligned, writable uninitialized storage
    /// for one `Self`. No field may be read until this function returns.
    pub unsafe fn initialize_at(destination: *mut Self) {
        let first = unsafe { core::ptr::addr_of_mut!((*destination).slots).cast::<Slot>() };
        for index in 0..N {
            unsafe { first.add(index).write(Slot::EMPTY) };
        }
        unsafe {
            core::ptr::addr_of_mut!((*destination).head).write(0);
            core::ptr::addr_of_mut!((*destination).len).write(0);
            core::ptr::addr_of_mut!((*destination).epoch).write(1);
            core::ptr::addr_of_mut!((*destination).next_serial).write(0);
            core::ptr::addr_of_mut!((*destination).last_retired_serial).write(0);
            core::ptr::addr_of_mut!((*destination).last_admitted_fence).write(0);
            core::ptr::addr_of_mut!((*destination).open).write(false);
            core::ptr::addr_of_mut!((*destination).poisoned).write(false);
            core::ptr::addr_of_mut!((*destination).epoch_exhausted).write(false);
        }
    }

    pub const fn is_open(&self) -> bool {
        self.open && !self.poisoned
    }

    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Open a new scheduler interval only from an empty, valid generation.
    pub fn reopen(&mut self, completed_fence: u32) -> bool {
        if self.open || self.len != 0 || self.poisoned || self.epoch_exhausted || N == 0 {
            return false;
        }
        self.last_admitted_fence = completed_fence;
        self.open = true;
        true
    }

    fn poison(&mut self) {
        self.open = false;
        self.poisoned = true;
    }

    /// Admit the exact OS-supplied fence in scheduler callback arrival order.
    pub fn admit(&mut self, fence: u32) -> Result<SubmissionTicket, AdmissionRefusal> {
        if self.poisoned {
            return Err(AdmissionRefusal::Poisoned);
        }
        if !self.open {
            return Err(AdmissionRefusal::Closed);
        }
        if self.len >= N {
            self.poison();
            return Err(AdmissionRefusal::Capacity);
        }
        if !crate::scanout_lease::fence_is_forward(self.last_admitted_fence, fence) {
            let previous = self.last_admitted_fence;
            self.poison();
            return Err(AdmissionRefusal::NotForward {
                previous,
                found: fence,
            });
        }
        let Some(serial) = self.next_serial.checked_add(1) else {
            self.poison();
            return Err(AdmissionRefusal::SerialExhausted);
        };
        let slot = (self.head + self.len) % N;
        let Ok(slot_u32) = u32::try_from(slot) else {
            self.poison();
            return Err(AdmissionRefusal::SlotIndexExhausted);
        };
        if !matches!(self.slots[slot].state, SlotState::Free) {
            self.poison();
            return Err(AdmissionRefusal::CorruptOccupiedSlot);
        }
        let ticket = SubmissionTicket {
            epoch: self.epoch,
            serial,
            slot: slot_u32,
        };
        self.slots[slot] = Slot {
            epoch: ticket.epoch,
            serial: ticket.serial,
            fence,
            state: SlotState::AwaitingHost,
        };
        self.next_serial = serial;
        self.last_admitted_fence = fence;
        self.len += 1;
        Ok(ticket)
    }

    /// Mark the exact direct callback ticket terminal without changing the
    /// scheduler frontier.
    pub fn mark_host_completed(&mut self, ticket: SubmissionTicket) -> CompletionDisposition {
        if ticket.epoch != self.epoch {
            return CompletionDisposition::StaleEpoch;
        }
        if self.poisoned {
            return CompletionDisposition::Poisoned;
        }
        let slot = ticket.slot as usize;
        if slot >= N || !self.slots[slot].matches(ticket, slot) {
            return CompletionDisposition::StaleTicket;
        }
        match self.slots[slot].state {
            SlotState::AwaitingHost => {
                self.slots[slot].state = SlotState::HostCompleted;
                CompletionDisposition::Marked {
                    retained_early: slot != self.head,
                }
            }
            SlotState::HostCompleted => CompletionDisposition::AlreadyCompleted,
            SlotState::Free => CompletionDisposition::StaleTicket,
        }
    }

    /// Make a current host rejection terminal for this scheduler generation.
    /// No later ready entry may bypass it.
    pub fn fail_submission(&mut self, ticket: SubmissionTicket) -> FailureDisposition {
        if ticket.epoch != self.epoch {
            return FailureDisposition::StaleEpoch;
        }
        if self.poisoned {
            return FailureDisposition::AlreadyPoisoned;
        }
        let slot = ticket.slot as usize;
        if slot >= N || !self.slots[slot].matches(ticket, slot) {
            return FailureDisposition::StaleTicket;
        }
        self.poison();
        FailureDisposition::Poisoned
    }

    /// The head ticket while it still awaits its host terminal — every earlier
    /// entry has retired, so a producer whose work must run AFTER them (the
    /// D5b present copy reads what the app's earlier renders wrote) may now
    /// submit on this ticket's behalf. `None` if the head is already
    /// host-complete, the engine is empty, or it is closed.
    pub fn head_awaiting_host(&self) -> Option<SubmissionTicket> {
        if !self.is_open() || self.len == 0 {
            return None;
        }
        let slot = self.slots[self.head];
        matches!(slot.state, SlotState::AwaitingHost).then(|| slot.ticket(self.head))
    }

    pub fn peek_ready(&self) -> Option<ReadySubmission> {
        if !self.is_open() || self.len == 0 {
            return None;
        }
        let slot = self.slots[self.head];
        if !matches!(slot.state, SlotState::HostCompleted) {
            return None;
        }
        Some(ReadySubmission {
            ticket: slot.ticket(self.head),
            fence: slot.fence,
        })
    }

    /// Retire exactly the head whose DMA completion notification succeeded.
    pub fn retire_ready(&mut self, ready: ReadySubmission) -> Result<u32, RetirementRefusal> {
        if self.poisoned {
            return Err(RetirementRefusal::Poisoned);
        }
        let Some(head) = self.peek_ready() else {
            return Err(RetirementRefusal::NotReady);
        };
        if head != ready {
            self.poison();
            return Err(RetirementRefusal::WrongHead);
        }
        let fence = head.fence;
        self.last_retired_serial = head.ticket.serial;
        self.slots[self.head] = Slot::EMPTY;
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Ok(fence)
    }

    /// Whether this exact ticket still occupies its current-generation slot.
    pub fn ticket_is_live(&self, ticket: SubmissionTicket) -> bool {
        if ticket.epoch != self.epoch {
            return false;
        }
        let slot = ticket.slot as usize;
        slot < N && self.slots[slot].matches(ticket, slot)
    }

    /// True only for a ticket retired by a successful notification in this
    /// still-current engine epoch. This lets the compatibility Venus FIFO
    /// discharge its separate ownership token after an earlier drain already
    /// reported the scheduler watermark.
    pub const fn ticket_was_retired(&self, ticket: SubmissionTicket) -> bool {
        ticket.epoch == self.epoch
            && ticket.serial != 0
            && ticket.serial <= self.last_retired_serial
    }

    /// A ticket minted before the last `invalidate`. It can never retire in
    /// this engine: VidSch either already saw its fence or will resubmit the
    /// packet under a fresh ticket.
    pub const fn ticket_is_stale_epoch(&self, ticket: SubmissionTicket) -> bool {
        ticket.epoch != self.epoch
    }

    /// Close and invalidate the current scheduler interval. Repeated close on
    /// an already-empty closed generation is idempotent.
    pub fn invalidate(&mut self) -> Invalidation {
        if !self.open && self.len == 0 && !self.poisoned {
            return Invalidation {
                dropped: 0,
                epoch_advanced: false,
            };
        }
        let dropped = self.len;
        for slot in &mut self.slots {
            *slot = Slot::EMPTY;
        }
        self.head = 0;
        self.len = 0;
        self.last_admitted_fence = 0;
        self.open = false;
        self.poisoned = false;
        let epoch_advanced = match self.epoch.checked_add(1) {
            Some(next) => {
                self.epoch = next;
                true
            }
            None => {
                self.epoch_exhausted = true;
                self.poisoned = true;
                false
            }
        };
        Invalidation {
            dropped,
            epoch_advanced,
        }
    }
}

impl<const N: usize> Default for OrderedEngine<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidate_makes_every_prior_ticket_stale_and_never_retired() {
        let mut engine: OrderedEngine<4> = OrderedEngine::new();
        assert!(engine.reopen(0));
        let retired = engine.admit(1).unwrap();
        assert!(matches!(
            engine.mark_host_completed(retired),
            CompletionDisposition::Marked { .. }
        ));
        let ready = engine.peek_ready().unwrap();
        assert_eq!(engine.retire_ready(ready), Ok(1));
        assert!(engine.ticket_was_retired(retired));
        assert!(!engine.ticket_is_stale_epoch(retired));
        let pending = engine.admit(2).unwrap();

        assert!(engine.invalidate().epoch_advanced);
        assert!(engine.reopen(1));
        // The reaper's predicate used to be `ticket_was_retired` alone, which
        // is false forever for both of these after the epoch moved.
        assert!(!engine.ticket_was_retired(retired));
        assert!(engine.ticket_is_stale_epoch(retired));
        assert!(!engine.ticket_was_retired(pending));
        assert!(engine.ticket_is_stale_epoch(pending));
        let fresh = engine.admit(3).unwrap();
        assert!(!engine.ticket_is_stale_epoch(fresh));
        assert!(!engine.ticket_was_retired(fresh));
    }

    fn open<const N: usize>() -> OrderedEngine<N> {
        let mut engine = OrderedEngine::new();
        assert!(engine.reopen(0));
        engine
    }

    #[test]
    fn head_awaiting_host_names_only_an_unfinished_head() {
        let mut engine = open::<4>();
        assert_eq!(engine.head_awaiting_host(), None, "empty");
        let first = engine.admit(1).unwrap();
        let second = engine.admit(2).unwrap();
        assert_eq!(engine.head_awaiting_host(), Some(first));
        // A later entry completing early does not move the head.
        assert!(matches!(
            engine.mark_host_completed(second),
            CompletionDisposition::Marked {
                retained_early: true
            }
        ));
        assert_eq!(engine.head_awaiting_host(), Some(first));
        // Once the head is host-complete it is ready, not awaiting.
        assert!(matches!(
            engine.mark_host_completed(first),
            CompletionDisposition::Marked {
                retained_early: false
            }
        ));
        assert_eq!(engine.head_awaiting_host(), None);
        let ready = engine.peek_ready().unwrap();
        assert_eq!(engine.retire_ready(ready), Ok(1));
        // The second entry is already complete: still not "awaiting".
        assert_eq!(engine.head_awaiting_host(), None);
        let ready = engine.peek_ready().unwrap();
        assert_eq!(engine.retire_ready(ready), Ok(2));
        let third = engine.admit(3).unwrap();
        assert_eq!(engine.head_awaiting_host(), Some(third));
        assert!(engine.invalidate().epoch_advanced);
        assert_eq!(engine.head_awaiting_host(), None, "closed after invalidate");
    }

    #[test]
    fn closed_frontier_admits_nothing() {
        let mut engine = OrderedEngine::<4>::new();
        assert_eq!(engine.admit(1), Err(AdmissionRefusal::Closed));
    }

    #[test]
    fn early_completion_is_retained_until_the_head_completes() {
        let mut engine = open::<4>();
        let first = engine.admit(10).unwrap();
        let second = engine.admit(40).unwrap();
        assert_eq!(
            engine.mark_host_completed(second),
            CompletionDisposition::Marked {
                retained_early: true
            }
        );
        assert_eq!(engine.peek_ready(), None);
        assert_eq!(
            engine.mark_host_completed(first),
            CompletionDisposition::Marked {
                retained_early: false
            }
        );
        assert_eq!(engine.peek_ready().unwrap().fence(), 10);
        let ready = engine.peek_ready().unwrap();
        assert_eq!(engine.retire_ready(ready), Ok(10));
        assert_eq!(engine.peek_ready().unwrap().fence(), 40);
    }

    #[test]
    fn notification_failure_keeps_the_same_ready_head() {
        let mut engine = open::<2>();
        let ticket = engine.admit(7).unwrap();
        engine.mark_host_completed(ticket);
        let before = engine.peek_ready().unwrap();
        // A failed callback simply does not call retire_ready.
        assert_eq!(engine.peek_ready(), Some(before));
        assert_eq!(engine.len(), 1);
    }

    #[test]
    fn reset_invalidates_a_late_callback_and_allows_a_fresh_generation() {
        let mut engine = open::<2>();
        let stale = engine.admit(9).unwrap();
        let invalidation = engine.invalidate();
        assert_eq!(invalidation.dropped, 1);
        assert!(invalidation.epoch_advanced);
        assert_eq!(
            engine.mark_host_completed(stale),
            CompletionDisposition::StaleEpoch
        );
        assert!(engine.reopen(8));
        let fresh = engine.admit(9).unwrap();
        assert_ne!(fresh.epoch(), stale.epoch());
        assert_eq!(
            engine.mark_host_completed(fresh),
            CompletionDisposition::Marked {
                retained_early: false
            }
        );
    }

    #[test]
    fn capacity_exhaustion_poisoned_the_generation_instead_of_bypassing() {
        let mut engine = open::<2>();
        let first = engine.admit(1).unwrap();
        engine.admit(2).unwrap();
        assert_eq!(engine.admit(3), Err(AdmissionRefusal::Capacity));
        assert!(engine.is_poisoned());
        assert_eq!(
            engine.mark_host_completed(first),
            CompletionDisposition::Poisoned
        );
        assert_eq!(engine.peek_ready(), None);
        engine.invalidate();
        assert!(engine.reopen(0));
    }

    #[test]
    fn duplicate_or_backward_scheduler_fence_fails_closed() {
        let mut duplicate = open::<2>();
        duplicate.admit(4).unwrap();
        assert_eq!(
            duplicate.admit(4),
            Err(AdmissionRefusal::NotForward {
                previous: 4,
                found: 4
            })
        );
        assert!(duplicate.is_poisoned());

        let mut backward = open::<2>();
        backward.admit(8).unwrap();
        assert_eq!(
            backward.admit(7),
            Err(AdmissionRefusal::NotForward {
                previous: 8,
                found: 7
            })
        );
        assert!(backward.is_poisoned());
    }

    #[test]
    fn submission_fence_wrap_uses_wddm_half_range_ordering() {
        let mut engine = OrderedEngine::<2>::new();
        assert!(engine.reopen(u32::MAX - 1));
        engine.admit(u32::MAX).unwrap();
        engine.admit(0).unwrap();
    }

    #[test]
    fn host_rejection_never_advances_a_later_completion() {
        let mut engine = open::<3>();
        let failed = engine.admit(1).unwrap();
        let later = engine.admit(2).unwrap();
        assert_eq!(engine.fail_submission(failed), FailureDisposition::Poisoned);
        assert_eq!(
            engine.mark_host_completed(later),
            CompletionDisposition::Poisoned
        );
        assert_eq!(engine.peek_ready(), None);
    }

    #[test]
    fn a_reused_physical_slot_does_not_accept_its_old_ticket() {
        let mut engine = open::<1>();
        let old = engine.admit(1).unwrap();
        engine.mark_host_completed(old);
        let ready = engine.peek_ready().unwrap();
        engine.retire_ready(ready).unwrap();
        assert!(engine.ticket_was_retired(old));
        let fresh = engine.admit(2).unwrap();
        assert_eq!(old.slot(), fresh.slot());
        assert_ne!(old.serial(), fresh.serial());
        assert_eq!(
            engine.mark_host_completed(old),
            CompletionDisposition::StaleTicket
        );
    }

    #[test]
    fn zero_capacity_never_opens() {
        let mut engine = OrderedEngine::<0>::new();
        assert!(!engine.reopen(0));
        assert_eq!(engine.admit(1), Err(AdmissionRefusal::Closed));
    }

    #[test]
    fn in_place_initialization_matches_the_const_constructor() {
        let mut storage = core::mem::MaybeUninit::<OrderedEngine<4>>::uninit();
        unsafe { OrderedEngine::initialize_at(storage.as_mut_ptr()) };
        let mut engine = unsafe { storage.assume_init() };
        assert_eq!(engine.epoch(), 1);
        assert_eq!(engine.len(), 0);
        assert!(!engine.is_open());
        assert!(engine.reopen(0));
        assert!(engine.admit(1).is_ok());
    }
}
