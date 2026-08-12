//! Stable slot custody makes abandoned rows leak instead of running backend destruction.

use crate::control_ownership::{TransportDomainId, TransportEpoch};
use core::fmt;
use core::marker::PhantomData;
use core::mem::{replace, ManuallyDrop};
use core::num::NonZeroU64;

pub enum ResourceSlotKind {}
pub enum ContextSlotKind {}
pub enum PairSlotKind {}
pub enum WindowSlotKind {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotTableId(NonZeroU64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotTableRootRefusal {
    Zero,
}

#[must_use]
pub struct SlotTableRoot {
    id: SlotTableId,
}

impl SlotTableRoot {
    /// Safety: `raw` is globally unique until this root and every descendant are gone.
    pub const unsafe fn new(raw: u64) -> Result<Self, SlotTableRootRefusal> {
        match NonZeroU64::new(raw) {
            Some(id) => Ok(Self {
                id: SlotTableId(id),
            }),
            None => Err(SlotTableRootRefusal::Zero),
        }
    }

    pub const fn id(&self) -> SlotTableId {
        self.id
    }
}

pub struct SlotHandle<K> {
    table: SlotTableId,
    epoch: TransportEpoch,
    index: u32,
    incarnation: NonZeroU64,
    kind: PhantomData<fn(K) -> K>,
}

impl<K> SlotHandle<K> {
    pub const fn epoch(self) -> TransportEpoch {
        self.epoch
    }

    pub const fn table(self) -> SlotTableId {
        self.table
    }

    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn incarnation(self) -> u64 {
        self.incarnation.get()
    }

    /// Safety: the exact row's one-shot terminal authority has been consumed.
    pub unsafe fn assume_terminal(self) -> TerminalSlot<K> {
        TerminalSlot { handle: self }
    }
}

impl<K> Copy for SlotHandle<K> {}

impl<K> Clone for SlotHandle<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> PartialEq for SlotHandle<K> {
    fn eq(&self, other: &Self) -> bool {
        self.table == other.table
            && self.epoch == other.epoch
            && self.index == other.index
            && self.incarnation == other.incarnation
    }
}

impl<K> Eq for SlotHandle<K> {}

impl<K> fmt::Debug for SlotHandle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlotHandle")
            .field("table", &self.table)
            .field("epoch", &self.epoch)
            .field("index", &self.index)
            .field("incarnation", &self.incarnation)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotState {
    Vacant,
    Occupied,
    Tombstone,
    Extracted,
    Retired,
}

enum SlotStorage<T> {
    Vacant {
        high_water: u64,
    },
    Occupied {
        incarnation: NonZeroU64,
        payload: ManuallyDrop<T>,
    },
    Tombstone {
        incarnation: NonZeroU64,
        payload: ManuallyDrop<T>,
    },
    Extracted {
        incarnation: NonZeroU64,
    },
    Retired,
}

pub struct StableSlot<T> {
    storage: SlotStorage<T>,
}

impl<T> StableSlot<T> {
    pub const fn vacant() -> Self {
        Self {
            storage: SlotStorage::Vacant { high_water: 0 },
        }
    }

    pub const fn state(&self) -> SlotState {
        match self.storage {
            SlotStorage::Vacant { .. } => SlotState::Vacant,
            SlotStorage::Occupied { .. } => SlotState::Occupied,
            SlotStorage::Tombstone { .. } => SlotState::Tombstone,
            SlotStorage::Extracted { .. } => SlotState::Extracted,
            SlotStorage::Retired => SlotState::Retired,
        }
    }

    pub const fn incarnation_high_water(&self) -> u64 {
        match self.storage {
            SlotStorage::Vacant { high_water } => high_water,
            SlotStorage::Occupied { incarnation, .. }
            | SlotStorage::Tombstone { incarnation, .. }
            | SlotStorage::Extracted { incarnation } => incarnation.get(),
            SlotStorage::Retired => u64::MAX,
        }
    }

    #[cfg(test)]
    const fn with_high_water(high_water: u64) -> Self {
        Self {
            storage: SlotStorage::Vacant { high_water },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotTableRefusal {
    CapacityTooLarge { found: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SlotEpochRebindRefusal {
    pub index: u32,
    pub state: SlotState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertRefusal {
    CapacityExhausted,
    IncarnationExhausted { retired_slots: u32 },
}

#[must_use]
pub struct RefusedInsert<T> {
    reason: InsertRefusal,
    payload: ManuallyDrop<T>,
}

impl<T> RefusedInsert<T> {
    pub const fn reason(&self) -> InsertRefusal {
        self.reason
    }

    pub fn into_payload(self) -> T {
        ManuallyDrop::into_inner(self.payload)
    }
}

impl<T> fmt::Debug for RefusedInsert<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedInsert")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotRefusal {
    TableMismatch {
        expected: SlotTableId,
        found: SlotTableId,
    },
    DomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    EpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    IndexOutOfRange {
        index: u32,
        capacity: u32,
    },
    IncarnationMismatch {
        expected: u64,
        found: u64,
    },
    WrongState {
        expected: SlotState,
        found: SlotState,
    },
}

#[must_use]
pub struct TerminalSlot<K> {
    handle: SlotHandle<K>,
}

impl<K> TerminalSlot<K> {
    pub const fn handle(&self) -> SlotHandle<K> {
        self.handle
    }
}

#[must_use]
pub struct RefusedTerminalSlot<K> {
    reason: SlotRefusal,
    authority: TerminalSlot<K>,
}

impl<K> RefusedTerminalSlot<K> {
    pub const fn reason(&self) -> SlotRefusal {
        self.reason
    }

    pub fn into_authority(self) -> TerminalSlot<K> {
        self.authority
    }
}

impl<K> fmt::Debug for RefusedTerminalSlot<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedTerminalSlot")
            .field("reason", &self.reason)
            .field("handle", &self.authority.handle)
            .finish()
    }
}

#[must_use]
pub struct TombstoneSlot<K> {
    handle: SlotHandle<K>,
}

impl<K> TombstoneSlot<K> {
    pub const fn handle(&self) -> SlotHandle<K> {
        self.handle
    }
}

#[must_use]
pub struct RefusedTombstoneSlot<K> {
    reason: SlotRefusal,
    action: TombstoneSlot<K>,
}

impl<K> RefusedTombstoneSlot<K> {
    pub const fn reason(&self) -> SlotRefusal {
        self.reason
    }

    pub fn into_action(self) -> TombstoneSlot<K> {
        self.action
    }
}

impl<K> fmt::Debug for RefusedTombstoneSlot<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedTombstoneSlot")
            .field("reason", &self.reason)
            .field("handle", &self.action.handle)
            .finish()
    }
}

#[must_use]
pub struct ExtractedSlot<T, K> {
    handle: SlotHandle<K>,
    payload: ManuallyDrop<T>,
}

impl<T, K> ExtractedSlot<T, K> {
    pub const fn handle(&self) -> SlotHandle<K> {
        self.handle
    }

    pub fn into_parts(self) -> (T, PendingAck<K>) {
        (
            ManuallyDrop::into_inner(self.payload),
            PendingAck {
                handle: self.handle,
            },
        )
    }
}

#[must_use]
pub struct PendingAck<K> {
    handle: SlotHandle<K>,
}

impl<K> PendingAck<K> {
    pub const fn handle(&self) -> SlotHandle<K> {
        self.handle
    }

    /// Safety: the extracted payload is fully finalized or irreversibly quarantined.
    pub unsafe fn assume_payload_finalized(self) -> FinalizedAck<K> {
        FinalizedAck {
            handle: self.handle,
        }
    }
}

#[must_use]
pub struct FinalizedAck<K> {
    handle: SlotHandle<K>,
}

impl<K> FinalizedAck<K> {
    pub const fn handle(&self) -> SlotHandle<K> {
        self.handle
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AckEffect {
    Vacated,
    RetiredAtIncarnationLimit,
}

#[must_use]
pub struct RefusedFinalizedAck<K> {
    reason: SlotRefusal,
    ack: FinalizedAck<K>,
}

impl<K> RefusedFinalizedAck<K> {
    pub const fn reason(&self) -> SlotRefusal {
        self.reason
    }

    pub fn into_ack(self) -> FinalizedAck<K> {
        self.ack
    }
}

impl<K> fmt::Debug for RefusedFinalizedAck<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedFinalizedAck")
            .field("reason", &self.reason)
            .field("handle", &self.ack.handle)
            .finish()
    }
}

pub struct StableSlots<'a, T, K> {
    table: SlotTableId,
    epoch: TransportEpoch,
    slots: &'a mut [StableSlot<T>],
    kind: PhantomData<fn(K) -> K>,
}

impl<'a, T, K> StableSlots<'a, T, K> {
    /// Safety: `table` descends from its live unique root, and `slots` is the sole canonical array for (`table`, `epoch`, `K`).
    /// The storage remains bound to `table` and `K`; rebinding its epoch requires only Vacant/Retired rows and no live authority or action.
    /// Stale Copy lookup handles may survive because their old epoch cannot match the rebound table.
    pub unsafe fn new(
        table: SlotTableId,
        epoch: TransportEpoch,
        slots: &'a mut [StableSlot<T>],
    ) -> Result<Self, SlotTableRefusal> {
        if slots.len() > u32::MAX as usize {
            return Err(SlotTableRefusal::CapacityTooLarge { found: slots.len() });
        }
        Ok(Self {
            table,
            epoch,
            slots,
            kind: PhantomData,
        })
    }

    /// Safety: the caller established every `new` precondition and validated
    /// that `slots.len()` is representable as `u32`.
    pub(crate) unsafe fn new_canonical(
        table: SlotTableId,
        epoch: TransportEpoch,
        slots: &'a mut [StableSlot<T>],
    ) -> Self {
        Self {
            table,
            epoch,
            slots,
            kind: PhantomData,
        }
    }

    pub const fn epoch(&self) -> TransportEpoch {
        self.epoch
    }

    pub const fn table(&self) -> SlotTableId {
        self.table
    }

    pub fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    pub fn state_at(&self, index: u32) -> Option<SlotState> {
        self.slots.get(index as usize).map(StableSlot::state)
    }

    pub(crate) fn occupied_handle_at(&self, index: u32) -> Option<SlotHandle<K>> {
        let slot = self.slots.get(index as usize)?;
        let SlotStorage::Occupied { incarnation, .. } = slot.storage else {
            return None;
        };
        Some(SlotHandle {
            table: self.table,
            epoch: self.epoch,
            index,
            incarnation,
            kind: PhantomData,
        })
    }

    pub(crate) fn can_mark_terminal(&self, handle: SlotHandle<K>) -> bool {
        let Ok(index) = self.checked_index(handle) else {
            return false;
        };
        self.check_incarnation(index, handle.incarnation.get())
            .is_ok()
            && self.slots[index].state() == SlotState::Occupied
    }

    /// Safety: exact terminal payload cleanup is complete, the canonical row
    /// is Occupied, and no descendant can observe its payload after this call.
    pub(crate) unsafe fn finalize_occupied(
        &mut self,
        handle: SlotHandle<K>,
    ) -> Result<(), SlotRefusal> {
        let index = self.checked_index(handle)?;
        self.check_incarnation(index, handle.incarnation.get())?;
        let old = replace(&mut self.slots[index].storage, SlotStorage::Retired);
        let (incarnation, payload) = match old {
            SlotStorage::Occupied {
                incarnation,
                payload,
            } => (incarnation, payload),
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                return Err(SlotRefusal::WrongState {
                    expected: SlotState::Occupied,
                    found,
                });
            }
        };
        let _payload = payload;
        self.slots[index].storage = if incarnation.get() == u64::MAX {
            SlotStorage::Retired
        } else {
            SlotStorage::Vacant {
                high_water: incarnation.get(),
            }
        };
        Ok(())
    }

    /// Safety: `can_mark_terminal` passed under this sole mutable owner borrow,
    /// and the payload has been fully finalized or irreversibly quarantined.
    pub(crate) unsafe fn finalize_occupied_validated(&mut self, handle: SlotHandle<K>) {
        let index = handle.index as usize;
        let old = replace(&mut self.slots[index].storage, SlotStorage::Retired);
        let (incarnation, payload) = match old {
            SlotStorage::Occupied {
                incarnation,
                payload,
            } if incarnation == handle.incarnation => (incarnation, payload),
            _ => unsafe { core::hint::unreachable_unchecked() },
        };
        let _payload = payload;
        self.slots[index].storage = if incarnation.get() == u64::MAX {
            SlotStorage::Retired
        } else {
            SlotStorage::Vacant {
                high_water: incarnation.get(),
            }
        };
    }

    pub fn incarnation_high_water_at(&self, index: u32) -> Option<u64> {
        self.slots
            .get(index as usize)
            .map(StableSlot::incarnation_high_water)
    }

    pub(crate) fn has_insert_capacity(&self) -> bool {
        self.slots.iter().any(|slot| {
            matches!(
                slot.storage,
                SlotStorage::Vacant { high_water } if high_water != u64::MAX
            )
        })
    }

    pub fn insert(&mut self, payload: T) -> Result<SlotHandle<K>, RefusedInsert<T>> {
        let mut retired_slots = 0_u32;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let high_water = match &slot.storage {
                SlotStorage::Vacant { high_water } => *high_water,
                SlotStorage::Retired => {
                    retired_slots = retired_slots.saturating_add(1);
                    continue;
                }
                _ => continue,
            };
            let Some(raw) = high_water.checked_add(1) else {
                slot.storage = SlotStorage::Retired;
                retired_slots = retired_slots.saturating_add(1);
                continue;
            };
            let Some(incarnation) = NonZeroU64::new(raw) else {
                slot.storage = SlotStorage::Retired;
                retired_slots = retired_slots.saturating_add(1);
                continue;
            };
            slot.storage = SlotStorage::Occupied {
                incarnation,
                payload: ManuallyDrop::new(payload),
            };
            return Ok(SlotHandle {
                table: self.table,
                epoch: self.epoch,
                index: index as u32,
                incarnation,
                kind: PhantomData,
            });
        }
        let reason = if retired_slots == 0 {
            InsertRefusal::CapacityExhausted
        } else {
            InsertRefusal::IncarnationExhausted { retired_slots }
        };
        Err(RefusedInsert {
            reason,
            payload: ManuallyDrop::new(payload),
        })
    }

    pub fn get(&self, handle: SlotHandle<K>) -> Result<&T, SlotRefusal> {
        let slot = self.checked_slot(handle)?;
        match &slot.storage {
            SlotStorage::Occupied { payload, .. } => Ok(payload),
            storage => Err(SlotRefusal::WrongState {
                expected: SlotState::Occupied,
                found: storage.state(),
            }),
        }
    }

    pub(crate) fn find_occupied_handle(
        &self,
        mut predicate: impl FnMut(&T) -> bool,
    ) -> Option<SlotHandle<K>> {
        for (index, slot) in self.slots.iter().enumerate() {
            let SlotStorage::Occupied {
                incarnation,
                payload,
            } = &slot.storage
            else {
                continue;
            };
            if predicate(payload) {
                return Some(SlotHandle {
                    table: self.table,
                    epoch: self.epoch,
                    index: index as u32,
                    incarnation: *incarnation,
                    kind: PhantomData,
                });
            }
        }
        None
    }

    pub(crate) fn find_unique_occupied_handle(
        &self,
        mut predicate: impl FnMut(&T) -> bool,
    ) -> Result<Option<SlotHandle<K>>, ()> {
        let mut found = None;
        for (index, slot) in self.slots.iter().enumerate() {
            let SlotStorage::Occupied {
                incarnation,
                payload,
            } = &slot.storage
            else {
                continue;
            };
            if !predicate(payload) {
                continue;
            }
            if found.is_some() {
                return Err(());
            }
            found = Some(SlotHandle {
                table: self.table,
                epoch: self.epoch,
                index: index as u32,
                incarnation: *incarnation,
                kind: PhantomData,
            });
        }
        Ok(found)
    }

    /// Safety: `apply` is owner-table code, does not move, replace, or drop the
    /// payload, and performs no wait, reentry, or borrow escape.
    pub(crate) unsafe fn with_occupied_mut<R>(
        &mut self,
        handle: SlotHandle<K>,
        apply: impl FnOnce(&mut T) -> R,
    ) -> Result<R, SlotRefusal> {
        let index = self.checked_index(handle)?;
        self.check_incarnation(index, handle.incarnation.get())?;
        match &mut self.slots[index].storage {
            SlotStorage::Occupied { payload, .. } => Ok(apply(payload)),
            storage => Err(SlotRefusal::WrongState {
                expected: SlotState::Occupied,
                found: storage.state(),
            }),
        }
    }

    /// Safety: the exact handle passed `get` or `can_mark_terminal` and no row
    /// mutation occurs between that prevalidation and this sole-owner call.
    pub(crate) unsafe fn with_occupied_mut_validated<R>(
        &mut self,
        handle: SlotHandle<K>,
        apply: impl FnOnce(&mut T) -> R,
    ) -> R {
        let index = handle.index as usize;
        match &mut self.slots[index].storage {
            SlotStorage::Occupied {
                incarnation,
                payload,
            } if *incarnation == handle.incarnation => apply(payload),
            _ => unsafe { core::hint::unreachable_unchecked() },
        }
    }

    /// Safety: `apply` obeys `with_occupied_mut` and returns `input` unchanged
    /// on every refused payload transition.
    pub(crate) unsafe fn with_occupied_mut_input<I, R>(
        &mut self,
        handle: SlotHandle<K>,
        input: I,
        apply: impl FnOnce(&mut T, I) -> R,
    ) -> Result<R, (SlotRefusal, I)> {
        let index = match self.checked_index(handle) {
            Ok(index) => index,
            Err(reason) => return Err((reason, input)),
        };
        if let Err(reason) = self.check_incarnation(index, handle.incarnation.get()) {
            return Err((reason, input));
        }
        match &mut self.slots[index].storage {
            SlotStorage::Occupied { payload, .. } => Ok(apply(payload, input)),
            storage => Err((
                SlotRefusal::WrongState {
                    expected: SlotState::Occupied,
                    found: storage.state(),
                },
                input,
            )),
        }
    }

    pub(crate) fn validate_epoch_rebind(&self) -> Result<(), SlotEpochRebindRefusal> {
        for (index, slot) in self.slots.iter().enumerate() {
            let state = slot.state();
            if !matches!(state, SlotState::Vacant | SlotState::Retired) {
                return Err(SlotEpochRebindRefusal {
                    index: index as u32,
                    state,
                });
            }
        }
        Ok(())
    }

    /// Safety: all canonical arrays passed read-only rebind validation, every
    /// old authority is drained, and `epoch` is their exact sealed successor.
    pub(crate) unsafe fn apply_epoch_rebind(&mut self, epoch: TransportEpoch) {
        self.epoch = epoch;
    }

    pub fn mark_tombstone(
        &mut self,
        authority: TerminalSlot<K>,
    ) -> Result<TombstoneSlot<K>, RefusedTerminalSlot<K>> {
        let handle = authority.handle;
        let index = match self.checked_index(handle) {
            Ok(index) => index,
            Err(reason) => return Err(RefusedTerminalSlot { reason, authority }),
        };
        let found = self.slots[index].storage.state();
        if let Err(reason) = self.check_incarnation(index, handle.incarnation.get()) {
            return Err(RefusedTerminalSlot { reason, authority });
        }
        if found != SlotState::Occupied {
            return Err(RefusedTerminalSlot {
                reason: SlotRefusal::WrongState {
                    expected: SlotState::Occupied,
                    found,
                },
                authority,
            });
        }
        let old = replace(&mut self.slots[index].storage, SlotStorage::Retired);
        match old {
            SlotStorage::Occupied {
                incarnation,
                payload,
            } => {
                self.slots[index].storage = SlotStorage::Tombstone {
                    incarnation,
                    payload,
                };
                Ok(TombstoneSlot { handle })
            }
            storage => {
                self.slots[index].storage = storage;
                Err(RefusedTerminalSlot {
                    reason: SlotRefusal::WrongState {
                        expected: SlotState::Occupied,
                        found,
                    },
                    authority,
                })
            }
        }
    }

    pub fn extract(
        &mut self,
        action: TombstoneSlot<K>,
    ) -> Result<ExtractedSlot<T, K>, RefusedTombstoneSlot<K>> {
        let handle = action.handle;
        let index = match self.checked_index(handle) {
            Ok(index) => index,
            Err(reason) => return Err(RefusedTombstoneSlot { reason, action }),
        };
        if let Err(reason) = self.check_incarnation(index, handle.incarnation.get()) {
            return Err(RefusedTombstoneSlot { reason, action });
        }
        let found = self.slots[index].storage.state();
        if found != SlotState::Tombstone {
            return Err(RefusedTombstoneSlot {
                reason: SlotRefusal::WrongState {
                    expected: SlotState::Tombstone,
                    found,
                },
                action,
            });
        }
        let old = replace(&mut self.slots[index].storage, SlotStorage::Retired);
        match old {
            SlotStorage::Tombstone {
                incarnation,
                payload,
            } => {
                self.slots[index].storage = SlotStorage::Extracted { incarnation };
                Ok(ExtractedSlot { handle, payload })
            }
            storage => {
                self.slots[index].storage = storage;
                Err(RefusedTombstoneSlot {
                    reason: SlotRefusal::WrongState {
                        expected: SlotState::Tombstone,
                        found,
                    },
                    action,
                })
            }
        }
    }

    pub fn ack_finalized(
        &mut self,
        ack: FinalizedAck<K>,
    ) -> Result<AckEffect, RefusedFinalizedAck<K>> {
        let handle = ack.handle;
        let index = match self.checked_index(handle) {
            Ok(index) => index,
            Err(reason) => return Err(RefusedFinalizedAck { reason, ack }),
        };
        if let Err(reason) = self.check_incarnation(index, handle.incarnation.get()) {
            return Err(RefusedFinalizedAck { reason, ack });
        }
        let found = self.slots[index].storage.state();
        if found != SlotState::Extracted {
            return Err(RefusedFinalizedAck {
                reason: SlotRefusal::WrongState {
                    expected: SlotState::Extracted,
                    found,
                },
                ack,
            });
        }
        let effect = if handle.incarnation.get() == u64::MAX {
            self.slots[index].storage = SlotStorage::Retired;
            AckEffect::RetiredAtIncarnationLimit
        } else {
            self.slots[index].storage = SlotStorage::Vacant {
                high_water: handle.incarnation.get(),
            };
            AckEffect::Vacated
        };
        Ok(effect)
    }

    fn checked_slot(&self, handle: SlotHandle<K>) -> Result<&StableSlot<T>, SlotRefusal> {
        let index = self.checked_index(handle)?;
        self.check_incarnation(index, handle.incarnation.get())?;
        Ok(&self.slots[index])
    }

    fn checked_index(&self, handle: SlotHandle<K>) -> Result<usize, SlotRefusal> {
        if handle.table != self.table {
            return Err(SlotRefusal::TableMismatch {
                expected: self.table,
                found: handle.table,
            });
        }
        if handle.epoch.domain() != self.epoch.domain() {
            return Err(SlotRefusal::DomainMismatch {
                expected: self.epoch.domain(),
                found: handle.epoch.domain(),
            });
        }
        if handle.epoch != self.epoch {
            return Err(SlotRefusal::EpochMismatch {
                expected: self.epoch,
                found: handle.epoch,
            });
        }
        let index = handle.index as usize;
        if index >= self.slots.len() {
            return Err(SlotRefusal::IndexOutOfRange {
                index: handle.index,
                capacity: self.slots.len() as u32,
            });
        }
        Ok(index)
    }

    fn check_incarnation(&self, index: usize, found: u64) -> Result<(), SlotRefusal> {
        let expected = self.slots[index].incarnation_high_water();
        if expected != found {
            return Err(SlotRefusal::IncarnationMismatch { expected, found });
        }
        Ok(())
    }
}

impl<T> SlotStorage<T> {
    const fn state(&self) -> SlotState {
        match self {
            Self::Vacant { .. } => SlotState::Vacant,
            Self::Occupied { .. } => SlotState::Occupied,
            Self::Tombstone { .. } => SlotState::Tombstone,
            Self::Extracted { .. } => SlotState::Extracted,
            Self::Retired => SlotState::Retired,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::control_ownership::{TransportDomainRoot, TransportGeneration};
    use core::cell::Cell;
    use std::rc::Rc;

    struct Token {
        id: u32,
        drops: Rc<Cell<u32>>,
    }

    impl Token {
        fn new(id: u32, drops: &Rc<Cell<u32>>) -> Self {
            Self {
                id,
                drops: Rc::clone(drops),
            }
        }
    }

    impl Drop for Token {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    fn epoch(domain: u64, generation: u64) -> TransportEpoch {
        let root = unsafe { TransportDomainRoot::new(domain).unwrap() };
        unsafe { TransportGeneration::restore(root, generation, 0, 0, 0).unwrap() }.epoch()
    }

    fn table(raw: u64) -> SlotTableRoot {
        unsafe { SlotTableRoot::new(raw).unwrap() }
    }

    fn make_slots<'a, T, K>(
        root: &SlotTableRoot,
        epoch: TransportEpoch,
        storage: &'a mut [StableSlot<T>],
    ) -> StableSlots<'a, T, K> {
        unsafe { StableSlots::new(root.id(), epoch, storage).unwrap() }
    }

    fn finalize<T, K>(extracted: ExtractedSlot<T, K>) -> (T, FinalizedAck<K>) {
        let (payload, pending) = extracted.into_parts();
        let ack = unsafe { pending.assume_payload_finalized() };
        (payload, ack)
    }

    #[test]
    fn stale_handle_never_aliases_a_reused_slot() {
        let root = table(1);
        let mut storage = [StableSlot::vacant()];
        let mut slots = make_slots::<u32, ResourceSlotKind>(&root, epoch(1, 1), &mut storage);
        let first = slots.insert(10).unwrap();
        let terminal = unsafe { first.assume_terminal() };
        let tombstone = slots.mark_tombstone(terminal).unwrap();
        let (payload, ack) = finalize(slots.extract(tombstone).unwrap());
        assert_eq!(payload, 10);
        assert_eq!(slots.ack_finalized(ack).unwrap(), AckEffect::Vacated);

        let second = slots.insert(20).unwrap();
        assert_eq!(first.index(), second.index());
        assert_eq!(second.incarnation(), first.incarnation() + 1);
        assert_eq!(
            slots.get(first),
            Err(SlotRefusal::IncarnationMismatch {
                expected: second.incarnation(),
                found: first.incarnation(),
            })
        );
        assert_eq!(slots.get(second), Ok(&20));
    }

    #[test]
    fn foreign_domain_epoch_index_and_state_are_distinct_and_inert() {
        let root = table(2);
        let mut storage = [StableSlot::vacant()];
        let mut slots = make_slots::<u32, ContextSlotKind>(&root, epoch(2, 3), &mut storage);
        let handle = slots.insert(7).unwrap();

        let mut foreign_domain_storage = [StableSlot::vacant()];
        let foreign_domain =
            make_slots::<u32, ContextSlotKind>(&root, epoch(4, 3), &mut foreign_domain_storage);
        assert!(matches!(
            foreign_domain.get(handle),
            Err(SlotRefusal::DomainMismatch { .. })
        ));

        let mut foreign_epoch_storage = [StableSlot::vacant()];
        let foreign_epoch =
            make_slots::<u32, ContextSlotKind>(&root, epoch(2, 4), &mut foreign_epoch_storage);
        assert!(matches!(
            foreign_epoch.get(handle),
            Err(SlotRefusal::EpochMismatch { .. })
        ));

        let forged_index = SlotHandle {
            table: handle.table,
            epoch: handle.epoch,
            index: 1,
            incarnation: handle.incarnation,
            kind: PhantomData,
        };
        assert_eq!(
            slots.get(forged_index),
            Err(SlotRefusal::IndexOutOfRange {
                index: 1,
                capacity: 1,
            })
        );
        let forged_action = TombstoneSlot { handle };
        let refusal = match slots.extract(forged_action) {
            Err(refusal) => refusal,
            Ok(_) => panic!("occupied row extracted"),
        };
        assert!(matches!(
            refusal.reason(),
            SlotRefusal::WrongState {
                expected: SlotState::Tombstone,
                found: SlotState::Occupied,
            }
        ));
        assert_eq!(refusal.into_action().handle(), handle);
        assert_eq!(slots.get(handle), Ok(&7));
    }

    #[test]
    fn identical_epoch_index_and_incarnation_from_another_table_are_refused() {
        let a_root = table(13);
        let b_root = table(14);
        let shared_epoch = epoch(13, 1);
        let mut a_storage = [StableSlot::vacant()];
        let mut b_storage = [StableSlot::vacant()];
        let mut a = make_slots::<u32, ResourceSlotKind>(&a_root, shared_epoch, &mut a_storage);
        let mut b = make_slots::<u32, ResourceSlotKind>(&b_root, shared_epoch, &mut b_storage);
        let a_handle = a.insert(70).unwrap();
        let b_handle = b.insert(80).unwrap();

        assert_eq!(a_handle.index(), b_handle.index());
        assert_eq!(a_handle.incarnation(), b_handle.incarnation());
        assert!(matches!(
            b.get(a_handle),
            Err(SlotRefusal::TableMismatch { .. })
        ));
        let authority = unsafe { a_handle.assume_terminal() };
        let refusal = match b.mark_tombstone(authority) {
            Err(refusal) => refusal,
            Ok(_) => panic!("foreign terminal authority accepted"),
        };
        assert!(matches!(
            refusal.reason(),
            SlotRefusal::TableMismatch { .. }
        ));
        let action = a.mark_tombstone(refusal.into_authority()).unwrap();
        let refusal = match b.extract(action) {
            Err(refusal) => refusal,
            Ok(_) => panic!("foreign tombstone action accepted"),
        };
        assert!(matches!(
            refusal.reason(),
            SlotRefusal::TableMismatch { .. }
        ));
        let (payload, ack) = finalize(a.extract(refusal.into_action()).unwrap());
        assert_eq!(payload, 70);
        assert_eq!(a.ack_finalized(ack).unwrap(), AckEffect::Vacated);
        assert_eq!(b.get(b_handle), Ok(&80));
    }

    #[test]
    fn unrelated_slot_churn_never_moves_a_live_row() {
        let root = table(3);
        let mut storage = [StableSlot::vacant(), StableSlot::vacant()];
        let mut slots = make_slots::<u32, PairSlotKind>(&root, epoch(5, 1), &mut storage);
        let first = slots.insert(11).unwrap();
        let second = slots.insert(22).unwrap();
        let tombstone = slots
            .mark_tombstone(unsafe { first.assume_terminal() })
            .unwrap();
        let (payload, ack) = finalize(slots.extract(tombstone).unwrap());
        assert_eq!(payload, 11);
        slots.ack_finalized(ack).unwrap();
        let replacement = slots.insert(33).unwrap();

        assert_eq!(slots.get(second), Ok(&22));
        assert_eq!(slots.get(replacement), Ok(&33));
        assert_eq!(second.index(), 1);
    }

    #[test]
    fn max_incarnation_retires_only_that_slot_and_falls_back() {
        let root = table(4);
        let mut storage = [
            StableSlot::with_high_water(u64::MAX - 1),
            StableSlot::vacant(),
        ];
        let mut slots = make_slots::<u32, WindowSlotKind>(&root, epoch(6, 9), &mut storage);
        let max = slots.insert(41).unwrap();
        assert_eq!(max.incarnation(), u64::MAX);
        let tombstone = slots
            .mark_tombstone(unsafe { max.assume_terminal() })
            .unwrap();
        let (payload, ack) = finalize(slots.extract(tombstone).unwrap());
        assert_eq!(payload, 41);
        assert_eq!(
            slots.ack_finalized(ack).unwrap(),
            AckEffect::RetiredAtIncarnationLimit
        );
        assert_eq!(slots.state_at(0), Some(SlotState::Retired));

        let fallback = slots.insert(42).unwrap();
        assert_eq!(fallback.index(), 1);
        assert_eq!(fallback.incarnation(), 1);
    }

    #[test]
    fn all_retired_and_full_refusals_recover_the_payload() {
        let root = table(5);
        let mut retired = [StableSlot::<u32>::with_high_water(u64::MAX)];
        let mut slots = make_slots::<u32, ResourceSlotKind>(&root, epoch(7, 1), &mut retired);
        let refusal = slots.insert(51).unwrap_err();
        assert_eq!(
            refusal.reason(),
            InsertRefusal::IncarnationExhausted { retired_slots: 1 }
        );
        assert_eq!(refusal.into_payload(), 51);
        assert_eq!(slots.state_at(0), Some(SlotState::Retired));
        let refusal = slots.insert(52).unwrap_err();
        assert_eq!(
            refusal.reason(),
            InsertRefusal::IncarnationExhausted { retired_slots: 1 }
        );
        assert_eq!(refusal.into_payload(), 52);

        let mut full = [StableSlot::vacant()];
        let mut slots = make_slots::<u32, ResourceSlotKind>(&root, epoch(7, 2), &mut full);
        let _ = slots.insert(61).unwrap();
        let refusal = slots.insert(62).unwrap_err();
        assert_eq!(refusal.reason(), InsertRefusal::CapacityExhausted);
        assert_eq!(refusal.into_payload(), 62);
    }

    #[test]
    fn dropped_storage_and_actions_never_run_backend_drop() {
        let root = table(6);
        let drops = Rc::new(Cell::new(0));
        {
            let mut storage = [StableSlot::vacant()];
            let mut slots = make_slots::<Token, ResourceSlotKind>(&root, epoch(8, 1), &mut storage);
            let _ = slots.insert(Token::new(1, &drops)).unwrap();
        }
        assert_eq!(drops.get(), 0);

        {
            let mut storage = [StableSlot::vacant()];
            let mut slots = make_slots::<Token, ResourceSlotKind>(&root, epoch(8, 2), &mut storage);
            let handle = slots.insert(Token::new(2, &drops)).unwrap();
            let action = slots
                .mark_tombstone(unsafe { handle.assume_terminal() })
                .unwrap();
            drop(action);
            assert_eq!(slots.state_at(0), Some(SlotState::Tombstone));
            drop(slots.insert(Token::new(3, &drops)).unwrap_err());
            assert_eq!(drops.get(), 0);
        }
        assert_eq!(drops.get(), 0);

        {
            let mut storage = [StableSlot::vacant()];
            let mut slots = make_slots::<Token, ResourceSlotKind>(&root, epoch(8, 4), &mut storage);
            let _ = slots.insert(Token::new(4, &drops)).unwrap();
            drop(slots.insert(Token::new(5, &drops)).unwrap_err());
        }
        assert_eq!(drops.get(), 0);

        let mut storage = [StableSlot::vacant()];
        let mut slots = make_slots::<Token, ResourceSlotKind>(&root, epoch(8, 3), &mut storage);
        let handle = slots.insert(Token::new(6, &drops)).unwrap();
        let tombstone = slots
            .mark_tombstone(unsafe { handle.assume_terminal() })
            .unwrap();
        drop(slots.extract(tombstone).unwrap());
        assert_eq!(drops.get(), 0);
        assert_eq!(slots.state_at(0), Some(SlotState::Extracted));
        let refused = slots.insert(Token::new(4, &drops)).unwrap_err();
        assert_eq!(refused.reason(), InsertRefusal::CapacityExhausted);
        let _recovered = refused.into_payload();
        assert_eq!(drops.get(), 0);
    }

    #[test]
    fn finalized_ack_replay_is_inert_before_and_after_slot_reuse() {
        let root = table(16);
        let mut storage = [StableSlot::vacant()];
        let mut slots = make_slots::<u32, ResourceSlotKind>(&root, epoch(16, 1), &mut storage);
        let first = slots.insert(1).unwrap();
        let action = slots
            .mark_tombstone(unsafe { first.assume_terminal() })
            .unwrap();
        let (payload, pending) = slots.extract(action).unwrap().into_parts();
        assert_eq!(payload, 1);
        let replay = FinalizedAck { handle: first };
        let ack = unsafe { pending.assume_payload_finalized() };
        assert_eq!(slots.ack_finalized(ack).unwrap(), AckEffect::Vacated);

        let refusal = slots.ack_finalized(replay).unwrap_err();
        assert_eq!(
            refusal.reason(),
            SlotRefusal::WrongState {
                expected: SlotState::Extracted,
                found: SlotState::Vacant,
            }
        );
        let replay = refusal.into_ack();
        let second = slots.insert(2).unwrap();
        assert_eq!(second.incarnation(), first.incarnation() + 1);
        let refusal = slots.ack_finalized(replay).unwrap_err();
        assert_eq!(
            refusal.reason(),
            SlotRefusal::IncarnationMismatch {
                expected: second.incarnation(),
                found: first.incarnation(),
            }
        );
        assert_eq!(slots.get(second), Ok(&2));
    }

    #[test]
    fn pending_ack_cannot_vacate_and_foreign_ack_is_recoverable() {
        let root = table(7);
        let drops = Rc::new(Cell::new(0));
        let mut a_storage = [StableSlot::vacant()];
        let mut b_storage = [StableSlot::vacant()];
        let mut a = make_slots::<Token, PairSlotKind>(&root, epoch(9, 1), &mut a_storage);
        let mut b = make_slots::<Token, PairSlotKind>(&root, epoch(10, 1), &mut b_storage);
        let handle = a.insert(Token::new(1, &drops)).unwrap();
        let tombstone = a
            .mark_tombstone(unsafe { handle.assume_terminal() })
            .unwrap();
        let (payload, pending) = a.extract(tombstone).unwrap().into_parts();
        assert_eq!(payload.id, 1);
        assert_eq!(a.state_at(0), Some(SlotState::Extracted));
        let refused = a.insert(Token::new(2, &drops)).unwrap_err();
        assert_eq!(refused.reason(), InsertRefusal::CapacityExhausted);
        let _recovered = refused.into_payload();
        drop(payload);
        let ack = unsafe { pending.assume_payload_finalized() };
        let refusal = b.ack_finalized(ack).unwrap_err();
        assert!(matches!(
            refusal.reason(),
            SlotRefusal::DomainMismatch { .. }
        ));
        let ack = refusal.into_ack();
        a.ack_finalized(ack).unwrap();
        assert_eq!(a.state_at(0), Some(SlotState::Vacant));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn dropping_pending_or_finalized_ack_strands_the_extracted_slot() {
        let root = table(15);
        let mut pending_storage = [StableSlot::vacant()];
        let mut pending_slots =
            make_slots::<u32, WindowSlotKind>(&root, epoch(15, 1), &mut pending_storage);
        let handle = pending_slots.insert(90).unwrap();
        let tombstone = pending_slots
            .mark_tombstone(unsafe { handle.assume_terminal() })
            .unwrap();
        let (payload, pending) = pending_slots.extract(tombstone).unwrap().into_parts();
        assert_eq!(payload, 90);
        drop(pending);
        assert_eq!(pending_slots.state_at(0), Some(SlotState::Extracted));
        assert_eq!(
            pending_slots.insert(91).unwrap_err().reason(),
            InsertRefusal::CapacityExhausted
        );

        let mut finalized_storage = [StableSlot::vacant()];
        let mut finalized_slots =
            make_slots::<u32, WindowSlotKind>(&root, epoch(15, 2), &mut finalized_storage);
        let handle = finalized_slots.insert(92).unwrap();
        let tombstone = finalized_slots
            .mark_tombstone(unsafe { handle.assume_terminal() })
            .unwrap();
        let (payload, pending) = finalized_slots.extract(tombstone).unwrap().into_parts();
        assert_eq!(payload, 92);
        drop(unsafe { pending.assume_payload_finalized() });
        assert_eq!(finalized_slots.state_at(0), Some(SlotState::Extracted));
        assert_eq!(
            finalized_slots.insert(93).unwrap_err().reason(),
            InsertRefusal::CapacityExhausted
        );
    }

    #[test]
    fn wrong_terminal_authority_is_recoverable_and_does_not_mutate() {
        let root = table(8);
        let mut storage = [StableSlot::vacant(), StableSlot::vacant()];
        let mut slots = make_slots::<u32, ContextSlotKind>(&root, epoch(11, 1), &mut storage);
        let first = slots.insert(1).unwrap();
        let second = slots.insert(2).unwrap();
        let authority = unsafe { first.assume_terminal() };
        let _action = slots.mark_tombstone(authority).unwrap();
        let duplicate = unsafe { first.assume_terminal() };
        let refusal = match slots.mark_tombstone(duplicate) {
            Err(refusal) => refusal,
            Ok(_) => panic!("duplicate terminal authority accepted"),
        };
        assert_eq!(
            refusal.reason(),
            SlotRefusal::WrongState {
                expected: SlotState::Occupied,
                found: SlotState::Tombstone,
            }
        );
        assert_eq!(refusal.into_authority().handle(), first);
        assert_eq!(slots.get(second), Ok(&2));
    }

    #[test]
    fn bounded_operation_sequences_preserve_state_and_incarnation() {
        let root = table(9);
        for code in 0_u32..256 {
            let mut storage = [StableSlot::vacant()];
            let mut slots = make_slots::<u32, ResourceSlotKind>(&root, epoch(12, 1), &mut storage);
            let mut current = None;
            let mut terminal = None;
            let mut value = code;
            for step in 0..4 {
                match (code >> (step * 2)) & 3 {
                    0 => {
                        if current.is_none() {
                            if let Ok(handle) = slots.insert(value) {
                                current = Some(handle);
                                value = value.wrapping_add(1);
                            }
                        }
                    }
                    1 => {
                        if terminal.is_none() {
                            if let Some(handle) = current {
                                let authority = unsafe { handle.assume_terminal() };
                                if let Ok(action) = slots.mark_tombstone(authority) {
                                    terminal = Some(action);
                                }
                            }
                        }
                    }
                    2 => {
                        if let Some(action) = terminal.take() {
                            if let Ok(extracted) = slots.extract(action) {
                                let (payload, pending) = extracted.into_parts();
                                let _ = payload;
                                let ack = unsafe { pending.assume_payload_finalized() };
                                slots.ack_finalized(ack).unwrap();
                                current = None;
                            }
                        }
                    }
                    _ => {
                        if let Some(handle) = current {
                            let state = slots.state_at(handle.index()).unwrap();
                            assert!(matches!(state, SlotState::Occupied | SlotState::Tombstone));
                            assert_eq!(
                                slots.incarnation_high_water_at(handle.index()),
                                Some(handle.incarnation())
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn kind_markers_are_distinct_lookup_types() {
        fn resource_only(_: SlotHandle<ResourceSlotKind>) {}
        fn window_only(_: SlotHandle<WindowSlotKind>) {}

        let _ = resource_only as fn(SlotHandle<ResourceSlotKind>);
        let _ = window_only as fn(SlotHandle<WindowSlotKind>);
        assert_ne!(
            core::any::TypeId::of::<SlotHandle<ResourceSlotKind>>(),
            core::any::TypeId::of::<SlotHandle<WindowSlotKind>>()
        );
        assert_eq!(
            unsafe { SlotTableRoot::new(0) }.err(),
            Some(SlotTableRootRefusal::Zero)
        );
    }
}
