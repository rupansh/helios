//! Fixed-capacity composition for transport generations, canonical rows, and controls.

use crate::context_attachment::{
    AttachmentFinish, AttachmentFinishEffect, AttachmentPhase, ContextAttachmentLifecycle,
};
use crate::context_lifecycle::{
    ContextFinish, ContextFinishEffect, ContextPhase, LeasedAttachmentReservation,
    ReleasedAttachmentLease, TransportContextLifecycle,
};
use crate::control_owner_slots::{
    ContextSlotKind, PairSlotKind, ResourceSlotKind, SlotHandle, SlotState, SlotTableRoot,
    StableSlot, StableSlots, WindowSlotKind,
};
use crate::control_owner_tickets::{
    ControlTicketSlot, ControlTickets, ControlWireKey, DispatchPermit, LifecycleAction,
    ObservedDispatch, PendingResetAck, PostBeginReservation, PreparedTicket, RundownRelease,
    RunnerOutcome, TicketReservation, TicketRowKind, TicketState,
};
use crate::control_ownership::{
    ControlVerb, PreparedControl, ResourceFinish, ResourceFinishEffect, ResourceLifecycle,
    ResourcePhase, TransportAttachment, TransportContext, TransportEpoch, TransportGeneration,
    TransportReset, TransportResource, TransportSuccessor, TransportWindow, WindowFinish,
    WindowFinishEffect, WindowLifecycle,
};
use core::marker::PhantomData;
use core::mem::{replace, ManuallyDrop};
use core::num::NonZeroU64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerPhase {
    Open,
    Closing,
    Quarantined,
    RundownSealed,
    ResetPrepared,
    ResetAuthorized,
    Resetting,
    Ready,
    Exhausted,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::control_owner_slots::SlotTableRoot;
    use crate::control_ownership::{TransportDomainRoot, TransportGeneration};
    use core::mem::size_of;
    use helios_protocol::virtio_gpu::{
        VirtioGpuCtrlHdr, VirtioGpuRespMapInfo, VIRTIO_GPU_RESP_OK_MAP_INFO,
        VIRTIO_GPU_RESP_OK_NODATA,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    trait TestResult<T, E> {
        fn must(self) -> T;
        fn must_err(self) -> E;
    }

    impl<T, E> TestResult<T, E> for Result<T, E> {
        fn must(self) -> T {
            match self {
                Ok(value) => value,
                Err(_) => panic!("expected success"),
            }
        }

        fn must_err(self) -> E {
            match self {
                Ok(_) => panic!("expected refusal"),
                Err(error) => error,
            }
        }
    }

    trait TestOption<T> {
        fn must(self) -> T;
    }

    impl<T> TestOption<T> for Option<T> {
        fn must(self) -> T {
            match self {
                Some(value) => value,
                None => panic!("expected value"),
            }
        }
    }

    #[derive(Debug)]
    struct Token(Rc<Cell<u32>>);

    impl Token {
        fn new(drops: &Rc<Cell<u32>>) -> Self {
            Self(Rc::clone(drops))
        }
    }

    impl Drop for Token {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    type TestTable<'a> = OwnerTable<'a, Token, Token, Token, Token, u8>;

    fn generation(domain: u64, epoch: u64) -> TransportGeneration {
        let root = unsafe { TransportDomainRoot::new(domain) }.must();
        if epoch == 1 {
            TransportGeneration::bootstrap(root)
        } else {
            unsafe { TransportGeneration::restore(root, epoch, 0, 0, 0) }.must()
        }
    }

    macro_rules! table {
        ($name:ident, $generation:expr, $table_id:expr, $capacity:expr) => {
            let mut resource_slots =
                core::array::from_fn::<_, $capacity, _>(|_| ResourceOwnerSlot::<Token>::vacant());
            let mut context_slots =
                core::array::from_fn::<_, $capacity, _>(|_| ContextOwnerSlot::<Token>::vacant());
            let mut pair_slots =
                core::array::from_fn::<_, $capacity, _>(|_| PairOwnerSlot::<Token>::vacant());
            let mut window_slots =
                core::array::from_fn::<_, $capacity, _>(|_| WindowOwnerSlot::<Token>::vacant());
            let mut resource_tickets =
                core::array::from_fn::<_, $capacity, _>(|_| ResourceOwnerTicket::<u8>::empty());
            let mut context_tickets =
                core::array::from_fn::<_, $capacity, _>(|_| ContextOwnerTicket::<u8>::empty());
            let mut pair_tickets =
                core::array::from_fn::<_, $capacity, _>(|_| PairOwnerTicket::<u8>::empty());
            let mut window_tickets =
                core::array::from_fn::<_, $capacity, _>(|_| WindowOwnerTicket::<u8>::empty());
            let slot_root = unsafe { SlotTableRoot::new($table_id) }.must();
            let config = OwnerConfig::new(0x1000, 0x20_000, 0x1000).must();
            let storage = OwnerStorage {
                resources: &mut resource_slots,
                contexts: &mut context_slots,
                pairs: &mut pair_slots,
                windows: &mut window_slots,
                resource_tickets: &mut resource_tickets,
                context_tickets: &mut context_tickets,
                pair_tickets: &mut pair_tickets,
                window_tickets: &mut window_tickets,
            };
            let mut $name = match unsafe {
                OwnerTable::new(slot_root, $generation, NonZeroU64::MIN, config, storage)
            } {
                Ok(table) => table,
                Err(_) => panic!("fresh owner table refused"),
            };
        };
    }

    fn nodata() -> RunnerOutcome<u8> {
        RunnerOutcome::HostResponse {
            response_type: VIRTIO_GPU_RESP_OK_NODATA,
            written_length: size_of::<VirtioGpuCtrlHdr>(),
            map_info: 0,
        }
    }

    fn map_info(value: u32) -> RunnerOutcome<u8> {
        RunnerOutcome::HostResponse {
            response_type: VIRTIO_GPU_RESP_OK_MAP_INFO,
            written_length: size_of::<VirtioGpuRespMapInfo>(),
            map_info: value,
        }
    }

    fn finish_resource(
        table: &mut TestTable<'_>,
        prepared: PreparedOwnerControl<ResourceSlotKind>,
        outcome: RunnerOutcome<u8>,
    ) -> PendingResourceCompletion<u8> {
        let work = match table.dispatch_resource(prepared) {
            Ok(work) => work,
            Err(_) => panic!("resource dispatch refused"),
        };
        let observed = unsafe { work.run_once(|_| outcome) };
        let action = match table.finish_resource_work(observed) {
            Ok(action) => action,
            Err(_) => panic!("resource observation refused"),
        };
        match table.apply_resource_control(action) {
            Ok(pending) => pending,
            Err(_) => panic!("resource completion refused"),
        }
    }

    fn finish_context(
        table: &mut TestTable<'_>,
        prepared: PreparedOwnerControl<ContextSlotKind>,
        outcome: RunnerOutcome<u8>,
    ) -> PendingContextCompletion<u8> {
        let work = match table.dispatch_context(prepared) {
            Ok(work) => work,
            Err(_) => panic!("context dispatch refused"),
        };
        let observed = unsafe { work.run_once(|_| outcome) };
        let action = match table.finish_context_work(observed) {
            Ok(action) => action,
            Err(_) => panic!("context observation refused"),
        };
        match table.apply_context_control(action) {
            Ok(pending) => pending,
            Err(_) => panic!("context completion refused"),
        }
    }

    fn finish_pair(
        table: &mut TestTable<'_>,
        prepared: PreparedOwnerControl<PairSlotKind>,
        outcome: RunnerOutcome<u8>,
    ) -> PendingPairCompletion<u8> {
        let work = match table.dispatch_pair(prepared) {
            Ok(work) => work,
            Err(_) => panic!("pair dispatch refused"),
        };
        let observed = unsafe { work.run_once(|_| outcome) };
        let action = match table.finish_pair_work(observed) {
            Ok(action) => action,
            Err(_) => panic!("pair observation refused"),
        };
        match table.apply_pair_control(action) {
            Ok(pending) => pending,
            Err(_) => panic!("pair completion refused"),
        }
    }

    fn finish_window(
        table: &mut TestTable<'_>,
        prepared: PreparedOwnerControl<WindowSlotKind>,
        outcome: RunnerOutcome<u8>,
    ) -> PendingWindowCompletion<u8> {
        let work = match table.dispatch_window(prepared) {
            Ok(work) => work,
            Err(_) => panic!("window dispatch refused"),
        };
        let observed = unsafe { work.run_once(|_| outcome) };
        let action = match table.finish_window_work(observed) {
            Ok(action) => action,
            Err(_) => panic!("window observation refused"),
        };
        match table.apply_window_control(action) {
            Ok(pending) => pending,
            Err(_) => panic!("window completion refused"),
        }
    }

    fn create_resource(table: &mut TestTable<'_>, drops: &Rc<Cell<u32>>) -> ResourceHandle {
        let row = table.reserve_resource(Token::new(drops)).must();
        let prepared = table.begin_resource_create(row).must();
        let pending = finish_resource(table, prepared, nodata());
        table.ack_resource_completion(pending).must();
        row
    }

    fn create_context(table: &mut TestTable<'_>, drops: &Rc<Cell<u32>>) -> ContextHandle {
        let row = table.reserve_context(Token::new(drops)).must();
        let prepared = table.begin_context_create(row).must();
        let pending = finish_context(table, prepared, nodata());
        table.ack_context_completion(pending).must();
        row
    }

    fn prepare_verified_reset(table: &mut TestTable<'_>) {
        let rundown = unsafe { table.assume_external_runner_rundown() }.must();
        let preparation = match table.prepare_reset(rundown) {
            Ok(preparation) => preparation,
            Err(_) => panic!("reset preparation refused"),
        };
        // Exercise a bit above the legacy u8 status width: every raw transport
        // status bit must participate in the physical-reset proof.
        let preparation = match unsafe { preparation.verify_raw_zero(0x100) } {
            Ok(_) => panic!("nonzero status accepted"),
            Err(refused) => refused.into_preparation(),
        };
        let verified = unsafe { preparation.verify_raw_zero(0) }.must();
        table.authorize_reset(verified).must();
    }

    fn finalize_reset(
        table: &mut TestTable<'_>,
        action: OwnerResetAction<Token, Token, Token, Token>,
    ) {
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
                let (backing, association, pending) = action.into_parts();
                drop(backing);
                drop(association);
                pending
            }
            OwnerResetAction::Context(action) => {
                let (owner, pending) = action.into_parts();
                drop(owner);
                pending
            }
        };
        table
            .ack_reset_action(unsafe { pending.assume_payloads_finalized() })
            .must();
    }

    #[test]
    fn full_cursor_drains_windows_pairs_resources_contexts_and_reopens() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7101, 1), 0x8101, 2);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        assert_eq!(
            table.resource_handle_by_id(table.resource(resource).must().id()),
            Ok(resource)
        );
        assert_eq!(
            table.context_handle_by_id(table.context(context).must().id()),
            Ok(context)
        );
        let admission = table
            .begin_secondary_attach(resource, context, Token::new(&drops))
            .must();
        let (pair, prepared) = admission.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();
        assert_eq!(
            table.pair_handle_by_ids(
                table.resource(resource).must().id(),
                table.context(context).must().id(),
                PairKind::Secondary,
            ),
            Ok(pair)
        );
        let identity = table.resource(resource).must();
        let window_identity = TransportWindow::new(identity, 0x2000, 0x1000).must();
        let admission = table
            .begin_window_map(resource, window_identity, Token::new(&drops))
            .must();
        let (window, prepared) = admission.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(0));
        table.ack_window_completion(pending).must();
        assert_eq!(
            table.window_handle_by_resource_id(table.resource(resource).must().id()),
            Ok(window)
        );
        assert_eq!(table.mapped_window_info(window), Ok(0));
        assert_eq!(
            table.mapped_resource_at_offset(0x2000),
            Ok(Some(table.resource(resource).must().id()))
        );
        assert_eq!(
            table.mapped_resource_at_offset(0x2fff),
            Ok(Some(table.resource(resource).must().id()))
        );
        assert_eq!(table.mapped_resource_at_offset(0x3000), Ok(None));

        let pair_use = table.borrow_pair_use(resource, context).must();
        let window_use = table.borrow_window_use(window).must();
        table.close().must();
        assert_eq!(
            table.borrow_pair_use(resource, context).must_err(),
            OwnerTableRefusal::WrongPhase {
                found: OwnerPhase::Closing
            }
        );
        let rundown = unsafe { table.assume_external_runner_rundown() }.must();
        let refusal = table.prepare_reset(rundown).must_err();
        assert_eq!(
            refusal.reason(),
            OwnerTableRefusal::ExternalActionOutstanding
        );
        table.return_pair_use(pair_use).must();
        table.return_window_use(window_use).must();
        let preparation = table.prepare_reset(refusal.into_rundown()).must();
        let verified = unsafe { preparation.verify_raw_zero(0) }.must();
        table.authorize_reset(verified).must();

        let first = table.next_reset_action().must().must();
        let pending = match first {
            OwnerResetAction::Window(action) => {
                let (reservation, info, pending) = action.into_parts();
                assert_eq!(info, Some(0));
                drop(reservation);
                pending
            }
            _ => panic!("window was not first"),
        };
        let wrong = FinalizedResetPayload {
            identity: ResetActionIdentity {
                stage: ResetStage::Contexts,
                ..pending.identity
            },
        };
        assert!(matches!(
            table.ack_reset_action(wrong),
            Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ResetActionMismatch,
                ..
            })
        ));
        let replay_identity = pending.identity;
        table
            .ack_reset_action(unsafe { pending.assume_payloads_finalized() })
            .must();
        assert!(matches!(
            table.ack_reset_action(FinalizedResetPayload {
                identity: replay_identity
            }),
            Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ResetActionMismatch,
                ..
            })
        ));

        for expected in [
            ResetStage::SecondaryPairs,
            ResetStage::Resources,
            ResetStage::Contexts,
        ] {
            let action = table.next_reset_action().must().must();
            let found = match &action {
                OwnerResetAction::SecondaryPair(_) => ResetStage::SecondaryPairs,
                OwnerResetAction::Resource(_) => ResetStage::Resources,
                OwnerResetAction::Context(_) => ResetStage::Contexts,
                OwnerResetAction::Window(_) => ResetStage::Windows,
            };
            assert_eq!(found, expected);
            finalize_reset(&mut table, action);
        }
        assert!(table.next_reset_action().must().is_none());
        assert_eq!(table.phase(), OwnerPhase::Ready);
        let request = table.request_next_transport().must();
        let successor_config = OwnerConfig::new(0, 0x40_000, 0x1000).must();
        let mut ready =
            unsafe { request.assume_ready(NonZeroU64::new(2).must(), successor_config) };
        let exact_table = ready.request.table;
        ready.request.table = unsafe { SlotTableRoot::new(0x81ff) }.must().id();
        let refusal = table.reopen(ready).must_err();
        assert_eq!(refusal.reason(), OwnerTableRefusal::NextTransportMismatch);
        let mut ready = refusal.into_ready();
        ready.request.table = exact_table;
        table.cancel_next_transport(ready).must();
        let request = table.request_next_transport().must();
        let stale = unsafe { request.assume_ready(table.physical_instance(), successor_config) };
        let refused = table.reopen(stale).must_err();
        assert_eq!(refused.reason(), OwnerTableRefusal::NextTransportMismatch);
        table.cancel_next_transport(refused.into_ready()).must();
        let request = table.request_next_transport().must();
        let ready = unsafe { request.assume_ready(NonZeroU64::new(2).must(), successor_config) };
        table.reopen(ready).must();
        assert_eq!(table.phase(), OwnerPhase::Open);
        assert_eq!(table.config(), successor_config);
        assert_eq!(
            table.resource(resource),
            Err(OwnerTableRefusal::ResourceNotFound)
        );
        assert_eq!(
            table.context(context),
            Err(OwnerTableRefusal::ContextNotFound)
        );
        assert_eq!(drops.get(), 4);
        let _ = pair;
    }

    #[test]
    fn creator_reset_releases_pair_lease_before_context() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7102, 1), 0x8102, 2);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        let admission = table
            .begin_creator_attach(resource, context, Token::new(&drops))
            .must();
        let (_, prepared) = admission.into_parts();
        let pending = finish_resource(&mut table, prepared, nodata());
        table.ack_resource_completion(pending).must();
        let refused = table
            .begin_secondary_attach(resource, context, Token::new(&drops))
            .must_err();
        assert_eq!(
            refused.reason(),
            OwnerTableRefusal::CanonicalPairExists {
                kind: PairKind::Creator
            }
        );
        let token = match refused.into_custody() {
            ReturnedCustody::Input(token) => token,
            _ => panic!("canonical refusal lost input"),
        };
        drop(token);
        table.close().must();
        prepare_verified_reset(&mut table);
        let action = table.next_reset_action().must().must();
        let pending = match action {
            OwnerResetAction::Resource(action) => {
                let (backing, association, pending) = action.into_parts();
                drop(backing);
                drop(association.expect("creator association"));
                pending
            }
            _ => panic!("creator resource was not first external reset action"),
        };
        assert!(matches!(
            table.next_reset_action(),
            Err(OwnerTableRefusal::ResetActionOutstanding)
        ));
        table
            .ack_reset_action(unsafe { pending.assume_payloads_finalized() })
            .must();
        let action = table.next_reset_action().must().must();
        assert!(matches!(action, OwnerResetAction::Context(_)));
        finalize_reset(&mut table, action);
        assert!(table.next_reset_action().must().is_none());
        assert_eq!(drops.get(), 4);
    }

    #[test]
    fn release_pending_blocks_reset_until_terminal_payload_ack() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7103, 1), 0x8103, 1);
        let resource = table.reserve_resource(Token::new(&drops)).must();
        let prepared = table.begin_resource_create(resource).must();
        let pending = finish_resource(&mut table, prepared, RunnerOutcome::DefiniteNotEnqueued(7));
        table.close_resource_admission(resource).must();
        table.close().must();
        let rundown = unsafe { table.assume_external_runner_rundown() }.must();
        let refusal = table.prepare_reset(rundown).must_err();
        assert_eq!(
            refusal.reason(),
            OwnerTableRefusal::ExternalActionOutstanding
        );
        let action = table.begin_resource_payload_release(pending).must();
        let (backing, _, pending) = action.into_parts();
        drop(backing);
        table
            .ack_resource_payload_release(unsafe { pending.assume_backing_finalized() })
            .must();
        let preparation = table.prepare_reset(refusal.into_rundown()).must();
        table
            .authorize_reset(unsafe { preparation.verify_raw_zero(0) }.must())
            .must();
        assert!(table.next_reset_action().must().is_none());
        assert_eq!(table.phase(), OwnerPhase::Ready);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn dropped_reset_action_strands_cursor_without_dropping_payload() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7104, 1), 0x8104, 1);
        let _resource = table.reserve_resource(Token::new(&drops)).must();
        table.close().must();
        prepare_verified_reset(&mut table);
        let action = table.next_reset_action().must().must();
        drop(action);
        assert_eq!(drops.get(), 0);
        assert!(matches!(
            table.next_reset_action(),
            Err(OwnerTableRefusal::ResetActionOutstanding)
        ));
    }

    #[test]
    fn quarantine_drains_old_work_but_refuses_new_dispatch() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7105, 1), 0x8105, 2);
        let resource = table.reserve_resource(Token::new(&drops)).must();
        let context = table.reserve_context(Token::new(&drops)).must();
        let resource_prepared = table.begin_resource_create(resource).must();
        let context_prepared = table.begin_context_create(context).must();
        let work = table.dispatch_resource(resource_prepared).must();
        table.quarantine().must();
        table.quarantine().must();
        let refusal = table.dispatch_context(context_prepared).must_err();
        assert_eq!(
            refusal.reason(),
            OwnerTableRefusal::WrongPhase {
                found: OwnerPhase::Quarantined
            }
        );
        let _stranded_identity = refusal.into_work();
        let observed = unsafe { work.run_once(|_| nodata()) };
        let action = table.finish_resource_work(observed).must();
        let pending = table.apply_resource_control(action).must();
        table.ack_resource_completion(pending).must();
        assert_eq!(table.active_runs(), 0);
        let rundown = unsafe { table.assume_external_runner_rundown() }.must();
        let preparation = table.prepare_reset(rundown).must();
        table
            .authorize_reset(unsafe { preparation.verify_raw_zero(0) }.must())
            .must();
        while let Some(action) = table.next_reset_action().must() {
            finalize_reset(&mut table, action);
        }
        assert_eq!(table.phase(), OwnerPhase::Ready);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn runner_rundown_seals_prepared_teardown_before_token_escapes() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7114, 1), 0x8114, 1);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        let admission = table
            .begin_secondary_attach(resource, context, Token::new(&drops))
            .must();
        let (pair, prepared) = admission.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();
        table.close().must();
        let prepared = table.begin_secondary_detach(pair).must();
        let rundown = unsafe { table.assume_external_runner_rundown() }.must();
        assert_eq!(table.phase(), OwnerPhase::RundownSealed);
        assert!(matches!(
            unsafe { table.assume_external_runner_rundown() },
            Err(OwnerTableRefusal::WrongPhase {
                found: OwnerPhase::RundownSealed
            })
        ));
        let refused = table.dispatch_pair(prepared).must_err();
        assert_eq!(
            refused.reason(),
            OwnerTableRefusal::WrongPhase {
                found: OwnerPhase::RundownSealed
            }
        );
        let _prepared_identity = refused.into_work();
        let preparation = table.prepare_reset(rundown).must();
        table
            .authorize_reset(unsafe { preparation.verify_raw_zero(0) }.must())
            .must();
        while let Some(action) = table.next_reset_action().must() {
            finalize_reset(&mut table, action);
        }
        assert_eq!(table.phase(), OwnerPhase::Ready);
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn window_use_refusals_distinguish_exhaustion_from_outstanding_use() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7115, 1), 0x8115, 1);
        let resource = create_resource(&mut table, &drops);
        let window_id =
            TransportWindow::new(table.resource(resource).must(), 0x2000, 0x1000).must();
        let admission = table
            .begin_window_map(resource, window_id, Token::new(&drops))
            .must();
        let (window, prepared) = admission.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(0));
        table.ack_window_completion(pending).must();

        let lease = table.borrow_window_use(window).must();
        let replay = WindowUseLease {
            window: lease.window,
            id: lease.id,
        };
        assert_eq!(
            table.begin_window_unmap(window).must_err(),
            OwnerTableRefusal::WindowUsesOutstanding { count: 1 }
        );
        table.return_window_use(lease).must();
        let refused = table.return_window_use(replay).must_err();
        assert_eq!(refused.reason(), OwnerTableRefusal::WindowUseMismatch);
        let replay = match refused.into_custody() {
            ReturnedCustody::Input(lease) => lease,
            _ => panic!("window replay refusal lost its move-only lease"),
        };
        let foreign = WindowUseLease {
            window,
            id: NonZeroU64::new(replay.id.get() + 1).must(),
        };
        let refused = table.return_window_use(foreign).must_err();
        assert_eq!(refused.reason(), OwnerTableRefusal::WindowUseMismatch);
        let _foreign = match refused.into_custody() {
            ReturnedCustody::Input(lease) => lease,
            _ => panic!("foreign window refusal lost its move-only lease"),
        };
        unsafe {
            table
                .windows
                .with_occupied_mut(window, |row| row.use_high_water = u64::MAX)
                .must();
        }
        assert_eq!(
            table.borrow_window_use(window).must_err(),
            OwnerTableRefusal::WindowUseExhausted
        );
    }

    #[test]
    fn reset_ack_correlated_refusal_preserves_pending_action() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7116, 1), 0x8116, 1);
        let resource = create_resource(&mut table, &drops);
        let window_id =
            TransportWindow::new(table.resource(resource).must(), 0x2000, 0x1000).must();
        let admission = table
            .begin_window_map(resource, window_id, Token::new(&drops))
            .must();
        let (window, prepared) = admission.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(0));
        table.ack_window_completion(pending).must();
        table.close().must();
        prepare_verified_reset(&mut table);

        let action = table.next_reset_action().must().must();
        let pending = match action {
            OwnerResetAction::Window(action) => {
                let (reservation, _, pending) = action.into_parts();
                drop(reservation);
                pending
            }
            _ => panic!("window reset action expected"),
        };
        unsafe {
            table
                .resources
                .with_occupied_mut(resource, |row| row.windows = 0)
                .must();
        }
        let refused = table
            .ack_reset_action(unsafe { pending.assume_payloads_finalized() })
            .must_err();
        let action = match refused {
            OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action,
            } => action,
            _ => panic!("correlated reset refusal lost finalization action"),
        };
        assert_eq!(table.phase(), OwnerPhase::Resetting);
        assert_eq!(
            table.windows.state_at(window.index()),
            Some(SlotState::Occupied)
        );
        assert_eq!(
            table.window_tickets.state_at(window.index()),
            Some(TicketState::Empty)
        );
        assert!(matches!(
            table.next_reset_action(),
            Err(OwnerTableRefusal::ResetActionOutstanding)
        ));
        unsafe {
            table
                .resources
                .with_occupied_mut(resource, |row| row.windows = 1)
                .must();
        }
        table.ack_reset_action(action).must();
        assert_eq!(
            table.windows.state_at(window.index()),
            Some(SlotState::Vacant)
        );
    }

    #[test]
    fn max_epoch_drains_old_rows_without_reopen_authority() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7106, u64::MAX), 0x8106, 1);
        let _resource = table.reserve_resource(Token::new(&drops)).must();
        table.close().must();
        prepare_verified_reset(&mut table);
        let action = table.next_reset_action().must().must();
        finalize_reset(&mut table, action);
        assert!(table.next_reset_action().must().is_none());
        assert_eq!(table.phase(), OwnerPhase::Exhausted);
        assert!(matches!(
            table.request_next_transport(),
            Err(OwnerTableRefusal::WrongPhase {
                found: OwnerPhase::Exhausted
            })
        ));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn fixed_capacity_and_canonical_pair_refusals_recover_payloads() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7107, 1), 0x8107, 1);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        let refused = table.reserve_resource(Token::new(&drops)).must_err();
        assert_eq!(
            refused.reason(),
            OwnerTableRefusal::ResourceCapacityExhausted
        );
        let token = match refused.into_custody() {
            ReturnedCustody::Input(token) => token,
            _ => panic!("capacity refusal lost input"),
        };
        drop(token);
        let admission = table
            .begin_secondary_attach(resource, context, Token::new(&drops))
            .must();
        let (_, prepared) = admission.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();
        let refused = table
            .begin_creator_attach(resource, context, Token::new(&drops))
            .must_err();
        assert_eq!(
            refused.reason(),
            OwnerTableRefusal::CanonicalPairExists {
                kind: PairKind::Secondary
            }
        );
        let token = match refused.into_custody() {
            ReturnedCustody::Input(token) => token,
            _ => panic!("canonical refusal lost input"),
        };
        drop(token);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn range_overlap_and_second_window_refuse_without_consuming_reservation() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7108, 1), 0x8108, 2);
        let first_resource = create_resource(&mut table, &drops);
        let second_resource = create_resource(&mut table, &drops);
        let first =
            TransportWindow::new(table.resource(first_resource).must(), 0x2000, 0x2000).must();
        let admission = table
            .begin_window_map(first_resource, first, Token::new(&drops))
            .must();
        let (_, prepared) = admission.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(3));
        table.ack_window_completion(pending).must();

        let second =
            TransportWindow::new(table.resource(second_resource).must(), 0x3000, 0x1000).must();
        let refused = table
            .begin_window_map(second_resource, second, Token::new(&drops))
            .must_err();
        assert_eq!(refused.reason(), OwnerTableRefusal::WindowRangeOverlaps);
        let token = match refused.into_custody() {
            ReturnedCustody::Input(token) => token,
            _ => panic!("overlap refusal lost input"),
        };
        drop(token);

        let duplicate =
            TransportWindow::new(table.resource(first_resource).must(), 0x6000, 0x1000).must();
        let refused = table
            .begin_window_map(first_resource, duplicate, Token::new(&drops))
            .must_err();
        assert_eq!(refused.reason(), OwnerTableRefusal::CanonicalWindowExists);
        let token = match refused.into_custody() {
            ReturnedCustody::Input(token) => token,
            _ => panic!("duplicate refusal lost input"),
        };
        drop(token);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn pending_window_participates_in_overlap_scan() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7113, 1), 0x8113, 2);
        let first_resource = create_resource(&mut table, &drops);
        let second_resource = create_resource(&mut table, &drops);
        let first =
            TransportWindow::new(table.resource(first_resource).must(), 0x2000, 0x2000).must();
        let _pending = table
            .begin_window_map(first_resource, first, Token::new(&drops))
            .must();
        let overlap =
            TransportWindow::new(table.resource(second_resource).must(), 0x3000, 0x1000).must();
        let refused = table
            .begin_window_map(second_resource, overlap, Token::new(&drops))
            .must_err();
        assert_eq!(refused.reason(), OwnerTableRefusal::WindowRangeOverlaps);
        let token = match refused.into_custody() {
            ReturnedCustody::Input(token) => token,
            _ => panic!("pending overlap refusal lost input"),
        };
        drop(token);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn initializing_creator_orphan_reset_cancels_resource_ticket_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7109, 1), 0x8109, 2);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        let earlier_resource = create_resource(&mut table, &drops);
        let earlier_context = create_context(&mut table, &drops);
        let earlier = table
            .begin_secondary_attach(earlier_resource, earlier_context, Token::new(&drops))
            .must();
        let (_, prepared) = earlier.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();
        let resource_id = table.resource(resource).must();
        let context_id = table.context(context).must();
        let generation = table.generation.take().must();
        let allocation = unsafe { generation.allocate_attachment(resource_id, context_id) }.must();
        let (generation, reservation) = allocation.into_parts();
        table.generation = Some(generation);
        let leased = unsafe {
            table
                .contexts
                .with_occupied_mut_input(context, reservation, |row, reservation| {
                    let Some(lifecycle) = row.lifecycle.as_mut() else {
                        return Err(reservation);
                    };
                    lifecycle
                        .lease_attachment(reservation)
                        .map_err(|refused| refused.into_reservation())
                })
        }
        .must()
        .must();
        let attachment = leased.attachment();
        let pair = table
            .pairs
            .insert(PairRow {
                resource,
                context,
                kind: PairKind::Creator,
                attachment,
                use_high_water: 0,
                uses: 0,
                state: PairRowState::Quarantined,
            })
            .must();
        let rundown = table.mint_rundown().must();
        let reservation = unsafe { table.resource_tickets.reserve(resource, rundown) }.must();
        table.orphan_pair_custody = Some(OrphanPairCustody::Initializing {
            pair,
            resource,
            context,
            attachment,
            leased: ManuallyDrop::new(leased),
            association: ManuallyDrop::new(Token::new(&drops)),
            reservation: ManuallyDrop::new(reservation),
        });
        table.enter_quarantined();
        prepare_verified_reset(&mut table);

        let earlier = table.next_reset_action().must().must();
        assert!(matches!(earlier, OwnerResetAction::SecondaryPair(_)));
        finalize_reset(&mut table, earlier);
        let action = table.next_reset_action().must().must();
        let pending = match action {
            OwnerResetAction::SecondaryPair(action) => {
                let (association, pending) = action.into_parts();
                drop(association);
                pending
            }
            _ => panic!("initializing creator orphan was not a pair-stage action"),
        };
        let replay_identity = pending.identity;
        let wrong = FinalizedResetPayload {
            identity: ResetActionIdentity {
                index: pending.identity.index + 1,
                ..pending.identity
            },
        };
        assert!(matches!(
            table.ack_reset_action(wrong),
            Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ResetActionMismatch,
                ..
            })
        ));
        table
            .ack_reset_action(unsafe { pending.assume_payloads_finalized() })
            .must();
        assert_eq!(
            table.resource_tickets.state_at(resource.index()),
            Some(TicketState::Empty)
        );
        assert_eq!(
            table.pair_tickets.state_at(pair.index()),
            Some(TicketState::Empty)
        );
        assert!(matches!(
            table.ack_reset_action(FinalizedResetPayload {
                identity: replay_identity
            }),
            Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ResetActionMismatch,
                ..
            })
        ));
        while let Some(action) = table.next_reset_action().must() {
            finalize_reset(&mut table, action);
        }
        assert_eq!(table.phase(), OwnerPhase::Ready);
        assert_eq!(drops.get(), 6);
    }

    #[test]
    fn first_fit_respects_prefix_and_every_window_state_remains_exact() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x710b, 1), 0x810b, 3);
        table.config = table.config.with_first_fit_base(0x5000).must();

        let prefix_resource = create_resource(&mut table, &drops);
        let prefix_identity =
            TransportWindow::new(table.resource(prefix_resource).must(), 0x1000, 0x1000).must();
        let prefix = table
            .begin_window_map(prefix_resource, prefix_identity, Token::new(&drops))
            .must();
        let (_, prefix_control) = prefix.into_parts();
        let pending = finish_window(&mut table, prefix_control, map_info(3));
        table.ack_window_completion(pending).must();
        assert_eq!(table.first_available_window_offset(0x1000), Ok(0x5000));

        let first_fit_resource = create_resource(&mut table, &drops);
        let first_fit_id = table.resource(first_fit_resource).must().id();
        let first_fit_identity =
            TransportWindow::new(table.resource(first_fit_resource).must(), 0x5000, 0x1000).must();
        let admission = table
            .begin_window_map(first_fit_resource, first_fit_identity, Token::new(&drops))
            .must();
        let (_, prepared) = admission.into_parts();

        // Initializing rows reserve their exact range but do not claim that its
        // bytes are mapped until the matching host observation lands.
        assert_eq!(table.first_available_window_offset(0x1000), Ok(0x6000));
        assert_eq!(table.mapped_resource_at_offset(0x5000), Ok(None));
        assert_eq!(
            table.first_overlapping_window_resource(0, 0x5800, 0x100),
            Ok(Some(first_fit_id))
        );

        let pending = finish_window(&mut table, prepared, map_info(7));
        table.ack_window_completion(pending).must();
        assert_eq!(
            table.mapped_resource_at_offset(0x5fff),
            Ok(Some(first_fit_id))
        );
    }

    #[test]
    fn orderly_closing_drains_secondary_window_resource_and_context() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7110, 1), 0x8110, 2);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        let admission = table
            .begin_secondary_attach(resource, context, Token::new(&drops))
            .must();
        let (pair, prepared) = admission.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();
        let window_id =
            TransportWindow::new(table.resource(resource).must(), 0x2000, 0x1000).must();
        let admission = table
            .begin_window_map(resource, window_id, Token::new(&drops))
            .must();
        let (window, prepared) = admission.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(5));
        table.ack_window_completion(pending).must();
        table.close().must();

        let prepared = table.begin_window_unmap(window).must();
        let pending = finish_window(&mut table, prepared, nodata());
        let action = table.begin_window_payload_release(pending).must();
        let (reservation, _, pending) = action.into_parts();
        drop(reservation);
        table
            .ack_window_payload_release(unsafe { pending.assume_reservation_finalized() })
            .must();

        let prepared = table.begin_secondary_detach(pair).must();
        let pending = finish_pair(&mut table, prepared, nodata());
        let action = table.begin_pair_payload_release(pending).must();
        let (association, _, pending) = action.into_parts();
        drop(association);
        table
            .ack_pair_payload_release(unsafe { pending.assume_association_finalized() })
            .must();

        table.close_resource_admission(resource).must();
        let prepared = table.begin_resource_unref(resource).must();
        let pending = finish_resource(&mut table, prepared, nodata());
        let action = table.begin_resource_payload_release(pending).must();
        let (backing, _, pending) = action.into_parts();
        drop(backing);
        table
            .ack_resource_payload_release(unsafe { pending.assume_backing_finalized() })
            .must();

        table.close_context_admission(context).must();
        let prepared = table.begin_context_destroy(context).must();
        let pending = finish_context(&mut table, prepared, nodata());
        let action = table.begin_context_payload_release(pending).must();
        let (owner, _, pending) = action.into_parts();
        drop(owner);
        table
            .ack_context_payload_release(unsafe { pending.assume_owner_finalized() })
            .must();
        assert_eq!(
            table.resource(resource),
            Err(OwnerTableRefusal::ResourceNotFound)
        );
        assert_eq!(
            table.context(context),
            Err(OwnerTableRefusal::ContextNotFound)
        );
        assert_eq!(drops.get(), 4);
    }

    #[test]
    fn orderly_creator_detach_returns_context_lease_before_destroy() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7111, 1), 0x8111, 1);
        let resource = create_resource(&mut table, &drops);
        let context = create_context(&mut table, &drops);
        let admission = table
            .begin_creator_attach(resource, context, Token::new(&drops))
            .must();
        let (_, prepared) = admission.into_parts();
        let pending = finish_resource(&mut table, prepared, nodata());
        table.ack_resource_completion(pending).must();
        table.close().must();

        table.close_context_admission(context).must();
        assert!(matches!(
            table.begin_context_destroy(context),
            Err(OwnerTableRefusal::CreatorAttachmentOutstanding)
        ));
        let prepared = table.begin_creator_detach(resource).must();
        let pending = finish_resource(&mut table, prepared, nodata());
        let action = table.begin_creator_payload_release(pending).must();
        let (association, _, pending) = action.into_parts();
        drop(association);
        table
            .ack_creator_payload_release(unsafe { pending.assume_association_finalized() })
            .must();

        table.close_resource_admission(resource).must();
        let prepared = table.begin_resource_unref(resource).must();
        let pending = finish_resource(&mut table, prepared, nodata());
        let action = table.begin_resource_payload_release(pending).must();
        let (backing, _, pending) = action.into_parts();
        drop(backing);
        table
            .ack_resource_payload_release(unsafe { pending.assume_backing_finalized() })
            .must();
        let prepared = table.begin_context_destroy(context).must();
        let pending = finish_context(&mut table, prepared, nodata());
        let action = table.begin_context_payload_release(pending).must();
        let (owner, _, pending) = action.into_parts();
        drop(owner);
        table
            .ack_context_payload_release(unsafe { pending.assume_owner_finalized() })
            .must();
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn reset_cursor_scans_holes_before_nonzero_rows_in_every_stage() {
        let drops = Rc::new(Cell::new(0));
        table!(table, generation(0x7112, 1), 0x8112, 2);

        let retired_resource = table.reserve_resource(Token::new(&drops)).must();
        let prepared = table.begin_resource_create(retired_resource).must();
        let pending = finish_resource(&mut table, prepared, RunnerOutcome::DefiniteNotEnqueued(1));
        table.close_resource_admission(retired_resource).must();
        let action = table.begin_resource_payload_release(pending).must();
        let (backing, _, pending) = action.into_parts();
        drop(backing);
        table
            .ack_resource_payload_release(unsafe { pending.assume_backing_finalized() })
            .must();
        let resource = create_resource(&mut table, &drops);

        let retired_context = table.reserve_context(Token::new(&drops)).must();
        let prepared = table.begin_context_create(retired_context).must();
        let pending = finish_context(&mut table, prepared, RunnerOutcome::DefiniteNotEnqueued(2));
        let action = table.begin_context_payload_release(pending).must();
        let (owner, _, pending) = action.into_parts();
        drop(owner);
        table
            .ack_context_payload_release(unsafe { pending.assume_owner_finalized() })
            .must();
        let context = create_context(&mut table, &drops);

        let first = table
            .begin_secondary_attach(resource, context, Token::new(&drops))
            .must();
        let (first_pair, prepared) = first.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();
        let prepared = table.begin_secondary_detach(first_pair).must();
        let pending = finish_pair(&mut table, prepared, nodata());
        let action = table.begin_pair_payload_release(pending).must();
        let (association, _, pending) = action.into_parts();
        drop(association);
        table
            .ack_pair_payload_release(unsafe { pending.assume_association_finalized() })
            .must();
        let second_context = create_context(&mut table, &drops);
        let second = table
            .begin_secondary_attach(resource, second_context, Token::new(&drops))
            .must();
        let (_, prepared) = second.into_parts();
        let pending = finish_pair(&mut table, prepared, nodata());
        table.ack_pair_completion(pending).must();

        let first_window_id =
            TransportWindow::new(table.resource(resource).must(), 0x2000, 0x1000).must();
        let first_window = table
            .begin_window_map(resource, first_window_id, Token::new(&drops))
            .must();
        let (first_window, prepared) = first_window.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(1));
        table.ack_window_completion(pending).must();
        let prepared = table.begin_window_unmap(first_window).must();
        let pending = finish_window(&mut table, prepared, nodata());
        let action = table.begin_window_payload_release(pending).must();
        let (reservation, _, pending) = action.into_parts();
        drop(reservation);
        table
            .ack_window_payload_release(unsafe { pending.assume_reservation_finalized() })
            .must();
        let second_window_id =
            TransportWindow::new(table.resource(resource).must(), 0x4000, 0x1000).must();
        let second_window = table
            .begin_window_map(resource, second_window_id, Token::new(&drops))
            .must();
        let (_, prepared) = second_window.into_parts();
        let pending = finish_window(&mut table, prepared, map_info(7));
        table.ack_window_completion(pending).must();

        table.close().must();
        prepare_verified_reset(&mut table);
        for expected in [
            ResetStage::Windows,
            ResetStage::SecondaryPairs,
            ResetStage::Resources,
            ResetStage::Contexts,
            ResetStage::Contexts,
        ] {
            let action = table.next_reset_action().must().must();
            let found = match &action {
                OwnerResetAction::Window(_) => ResetStage::Windows,
                OwnerResetAction::SecondaryPair(_) => ResetStage::SecondaryPairs,
                OwnerResetAction::Resource(_) => ResetStage::Resources,
                OwnerResetAction::Context(_) => ResetStage::Contexts,
            };
            assert_eq!(found, expected);
            finalize_reset(&mut table, action);
        }
        assert!(table.next_reset_action().must().is_none());
        assert_eq!(table.phase(), OwnerPhase::Ready);
        assert_eq!(drops.get(), 9);
    }

    fn dormant_seed(raw: u64) -> DormantOwnerSeed {
        let domain = unsafe { TransportDomainRoot::new(raw) }.must();
        let generation = TransportGeneration::bootstrap(domain);
        let root = unsafe { SlotTableRoot::new(raw) }.must();
        unsafe { DormantOwnerSeed::new(root, generation) }
    }

    #[test]
    fn dormant_seed_observes_and_exactly_removes_one_transport() {
        let mut seed = dormant_seed(0x7120);
        let instance = NonZeroU64::new(7).must();
        let config = OwnerConfig::new(0, 0x20_0000, 0x1000).must();
        let observation = unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), instance, config)
        };

        seed.observe_transport(observation).must();
        assert_eq!(
            seed.state(),
            DormantOwnerState::Ready {
                physical_instance: instance,
                config,
            }
        );

        let removal = unsafe {
            DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), instance)
        };
        seed.observe_transport_removed(removal).must();
        assert_eq!(seed.state(), DormantOwnerState::NoTransport);
        assert_eq!(seed.epoch().get(), 1);
    }

    #[test]
    fn dormant_seed_refuses_duplicate_foreign_and_replayed_inputs_losslessly() {
        let mut seed = dormant_seed(0x7121);
        let current = NonZeroU64::new(11).must();
        let foreign = NonZeroU64::new(12).must();
        let config = OwnerConfig::new(0, 0x10_0000, 0x1000).must();
        seed.observe_transport(unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), current, config)
        })
        .must();

        let refused = seed
            .observe_transport(unsafe {
                DormantTransportObservation::assume_ready(
                    seed.table(),
                    seed.epoch(),
                    foreign,
                    config,
                )
            })
            .must_err();
        assert_eq!(
            refused.reason(),
            DormantOwnerRefusal::TransportAlreadyObserved { current }
        );
        assert_eq!(refused.into_input().physical_instance(), foreign);

        let refused = seed
            .observe_transport_removed(unsafe {
                DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), foreign)
            })
            .must_err();
        assert_eq!(
            refused.reason(),
            DormantOwnerRefusal::PhysicalInstanceMismatch {
                expected: current,
                found: foreign,
            }
        );
        assert_eq!(refused.into_input().physical_instance(), foreign);
        assert_eq!(
            seed.state(),
            DormantOwnerState::Ready {
                physical_instance: current,
                config,
            }
        );

        seed.observe_transport_removed(unsafe {
            DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), current)
        })
        .must();
        let replay = seed
            .observe_transport_removed(unsafe {
                DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), current)
            })
            .must_err();
        assert_eq!(replay.reason(), DormantOwnerRefusal::NoTransportObserved);
        assert_eq!(replay.into_input().physical_instance(), current);
    }

    #[test]
    fn dormant_seed_records_unavailable_geometry_without_minting_a_table() {
        for (raw, instance, reason) in [
            (
                0x7122,
                NonZeroU64::new(21).must(),
                DormantTransportUnavailable::NoWindow,
            ),
            (
                0x7123,
                NonZeroU64::new(22).must(),
                DormantTransportUnavailable::InvalidBounds,
            ),
        ] {
            let mut seed = dormant_seed(raw);
            seed.observe_transport(unsafe {
                DormantTransportObservation::assume_unavailable(
                    seed.table(),
                    seed.epoch(),
                    instance,
                    reason,
                )
            })
            .must();
            assert_eq!(
                seed.state(),
                DormantOwnerState::Unavailable {
                    physical_instance: instance,
                    reason,
                }
            );
            seed.observe_transport_removed(unsafe {
                DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), instance)
            })
            .must();
            assert_eq!(seed.state(), DormantOwnerState::NoTransport);

            let next = NonZeroU64::new(instance.get() + 1).must();
            let config = OwnerConfig::new(0, 0x20_0000, 0x1000).must();
            seed.observe_transport(unsafe {
                DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), next, config)
            })
            .must();
            assert_eq!(
                seed.state(),
                DormantOwnerState::Ready {
                    physical_instance: next,
                    config,
                }
            );
        }
    }

    #[test]
    fn dormant_seed_rejects_a_foreign_table_observation_losslessly() {
        let mut target = dormant_seed(0x7124);
        let foreign = dormant_seed(0x7125);
        let instance = NonZeroU64::new(31).must();
        let config = OwnerConfig::new(0, 0x10_0000, 0x1000).must();
        let observation = unsafe {
            DormantTransportObservation::assume_ready(
                foreign.table(),
                foreign.epoch(),
                instance,
                config,
            )
        };

        let refused = target.observe_transport(observation).must_err();
        assert_eq!(refused.reason(), DormantOwnerRefusal::TableMismatch);
        assert_eq!(refused.into_input().physical_instance(), instance);
        assert_eq!(target.state(), DormantOwnerState::NoTransport);
    }

    #[test]
    fn dormant_seed_never_reaccepts_a_removed_physical_instance() {
        let mut seed = dormant_seed(0x7126);
        let instance = NonZeroU64::new(41).must();
        let config = OwnerConfig::new(0, 0x10_0000, 0x1000).must();
        seed.observe_transport(unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), instance, config)
        })
        .must();
        seed.observe_transport_removed(unsafe {
            DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), instance)
        })
        .must();

        let stale = seed
            .observe_transport(unsafe {
                DormantTransportObservation::assume_ready(
                    seed.table(),
                    seed.epoch(),
                    instance,
                    config,
                )
            })
            .must_err();
        assert_eq!(
            stale.reason(),
            DormantOwnerRefusal::PhysicalInstanceNotNewer {
                high_water: instance.get(),
                found: instance,
            }
        );
        assert_eq!(stale.into_input().physical_instance(), instance);
        assert_eq!(seed.state(), DormantOwnerState::NoTransport);

        let next = NonZeroU64::new(instance.get() + 1).must();
        seed.observe_transport(unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), next, config)
        })
        .must();
        assert_eq!(
            seed.state(),
            DormantOwnerState::Ready {
                physical_instance: next,
                config,
            }
        );

        let stale_removal = seed
            .observe_transport_removed(unsafe {
                DormantTransportRemoval::assume_removed(seed.table(), seed.epoch(), instance)
            })
            .must_err();
        assert_eq!(
            stale_removal.reason(),
            DormantOwnerRefusal::PhysicalInstanceMismatch {
                expected: next,
                found: instance,
            }
        );
        assert_eq!(
            seed.state(),
            DormantOwnerState::Ready {
                physical_instance: next,
                config,
            }
        );
    }

    #[test]
    fn dormant_seed_refuses_foreign_epoch_and_removal_authority() {
        let mut seed = dormant_seed(0x7127);
        let foreign = dormant_seed(0x7128);
        let instance = NonZeroU64::new(51).must();
        let config = OwnerConfig::new(0, 0x10_0000, 0x1000).must();
        let wrong_epoch = TransportEpoch::test_from_raw(seed.epoch().domain(), 2).must();
        let wrong_epoch_observation = unsafe {
            DormantTransportObservation::assume_ready(seed.table(), wrong_epoch, instance, config)
        };
        let refused = seed.observe_transport(wrong_epoch_observation).must_err();
        assert_eq!(
            refused.reason(),
            DormantOwnerRefusal::EpochMismatch {
                expected: seed.epoch(),
                found: wrong_epoch,
            }
        );
        assert_eq!(refused.into_input().physical_instance(), instance);

        seed.observe_transport(unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), instance, config)
        })
        .must();
        let foreign_removal = unsafe {
            DormantTransportRemoval::assume_removed(foreign.table(), foreign.epoch(), instance)
        };
        let refused = seed.observe_transport_removed(foreign_removal).must_err();
        assert_eq!(refused.reason(), DormantOwnerRefusal::TableMismatch);
        assert_eq!(refused.into_input().physical_instance(), instance);
        assert_eq!(
            seed.state(),
            DormantOwnerState::Ready {
                physical_instance: instance,
                config,
            }
        );
    }

    #[test]
    fn dormant_seed_activates_only_its_exact_observed_transport() {
        let mut seed = dormant_seed(0x7129);
        let instance = NonZeroU64::new(61).must();
        let config = OwnerConfig::new(0, 0x10_0000, 0x1000).must();
        seed.observe_transport(unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), instance, config)
        })
        .must();

        let mut resources =
            core::array::from_fn::<_, 2, _>(|_| ResourceOwnerSlot::<Token>::vacant());
        let mut contexts = core::array::from_fn::<_, 2, _>(|_| ContextOwnerSlot::<Token>::vacant());
        let mut pairs = core::array::from_fn::<_, 2, _>(|_| PairOwnerSlot::<Token>::vacant());
        let mut windows = core::array::from_fn::<_, 2, _>(|_| WindowOwnerSlot::<Token>::vacant());
        let mut resource_tickets =
            core::array::from_fn::<_, 2, _>(|_| ResourceOwnerTicket::<u8>::empty());
        let mut context_tickets =
            core::array::from_fn::<_, 2, _>(|_| ContextOwnerTicket::<u8>::empty());
        let mut pair_tickets = core::array::from_fn::<_, 2, _>(|_| PairOwnerTicket::<u8>::empty());
        let mut window_tickets =
            core::array::from_fn::<_, 2, _>(|_| WindowOwnerTicket::<u8>::empty());
        let storage = OwnerStorage {
            resources: &mut resources,
            contexts: &mut contexts,
            pairs: &mut pairs,
            windows: &mut windows,
            resource_tickets: &mut resource_tickets,
            context_tickets: &mut context_tickets,
            pair_tickets: &mut pair_tickets,
            window_tickets: &mut window_tickets,
        };

        let table = seed.activate(instance, storage).must();
        assert_eq!(table.phase(), OwnerPhase::Open);
        assert_eq!(table.physical_instance(), instance);
        assert_eq!(table.epoch().must().get(), 1);
    }

    #[test]
    fn dormant_activation_refuses_foreign_instance_without_losing_custody() {
        let mut seed = dormant_seed(0x7130);
        let instance = NonZeroU64::new(71).must();
        let foreign = NonZeroU64::new(72).must();
        let config = OwnerConfig::new(0, 0x10_0000, 0x1000).must();
        seed.observe_transport(unsafe {
            DormantTransportObservation::assume_ready(seed.table(), seed.epoch(), instance, config)
        })
        .must();

        let mut resources =
            core::array::from_fn::<_, 1, _>(|_| ResourceOwnerSlot::<Token>::vacant());
        let mut contexts = core::array::from_fn::<_, 1, _>(|_| ContextOwnerSlot::<Token>::vacant());
        let mut pairs = core::array::from_fn::<_, 1, _>(|_| PairOwnerSlot::<Token>::vacant());
        let mut windows = core::array::from_fn::<_, 1, _>(|_| WindowOwnerSlot::<Token>::vacant());
        let mut resource_tickets =
            core::array::from_fn::<_, 1, _>(|_| ResourceOwnerTicket::<u8>::empty());
        let mut context_tickets =
            core::array::from_fn::<_, 1, _>(|_| ContextOwnerTicket::<u8>::empty());
        let mut pair_tickets = core::array::from_fn::<_, 1, _>(|_| PairOwnerTicket::<u8>::empty());
        let mut window_tickets =
            core::array::from_fn::<_, 1, _>(|_| WindowOwnerTicket::<u8>::empty());
        let storage = OwnerStorage {
            resources: &mut resources,
            contexts: &mut contexts,
            pairs: &mut pairs,
            windows: &mut windows,
            resource_tickets: &mut resource_tickets,
            context_tickets: &mut context_tickets,
            pair_tickets: &mut pair_tickets,
            window_tickets: &mut window_tickets,
        };

        let refused = seed.activate(foreign, storage).must_err();
        assert_eq!(
            refused.reason(),
            DormantOwnerActivationRefusal::PhysicalInstanceMismatch {
                expected: instance,
                found: foreign,
            }
        );
        let (seed, storage) = refused.into_parts();
        assert_eq!(
            seed.state(),
            DormantOwnerState::Ready {
                physical_instance: instance,
                config,
            }
        );
        assert_eq!(storage.resources.len(), 1);
        assert!(storage.resource_tickets[0].is_fresh());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairKind {
    Creator,
    Secondary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerConfig {
    window_base: u64,
    window_length: u64,
    window_alignment: u64,
    first_fit_base: u64,
}

impl OwnerConfig {
    pub const fn new(
        window_base: u64,
        window_length: u64,
        window_alignment: u64,
    ) -> Result<Self, OwnerTableRefusal> {
        if window_length == 0
            || window_alignment == 0
            || !window_alignment.is_power_of_two()
            || window_base.checked_add(window_length).is_none()
        {
            return Err(OwnerTableRefusal::WindowBoundsInvalid);
        }
        Ok(Self {
            window_base,
            window_length,
            window_alignment,
            first_fit_base: window_base,
        })
    }

    /// Reserve a prefix for a different exact allocator while retaining the
    /// complete transport window for fixed placements. The first-fit cursor may
    /// begin only at an aligned address inside the configured bounds.
    pub const fn with_first_fit_base(
        mut self,
        first_fit_base: u64,
    ) -> Result<Self, OwnerTableRefusal> {
        let Some(window_end) = self.window_base.checked_add(self.window_length) else {
            return Err(OwnerTableRefusal::WindowBoundsInvalid);
        };
        if first_fit_base < self.window_base
            || first_fit_base > window_end
            || first_fit_base & (self.window_alignment - 1) != 0
        {
            return Err(OwnerTableRefusal::WindowBoundsInvalid);
        }
        self.first_fit_base = first_fit_base;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DormantTransportUnavailable {
    NoWindow,
    InvalidBounds,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DormantOwnerState {
    NoTransport,
    Ready {
        physical_instance: NonZeroU64,
        config: OwnerConfig,
    },
    Unavailable {
        physical_instance: NonZeroU64,
        reason: DormantTransportUnavailable,
    },
}

#[must_use]
pub struct DormantTransportObservation {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
    physical_instance: NonZeroU64,
    config: Result<OwnerConfig, DormantTransportUnavailable>,
}

impl DormantTransportObservation {
    /// Safety: `table`/`epoch` identify the exact dormant seed owning this one
    /// observation; no observation for this install has been minted before;
    /// and `physical_instance` names its current successfully initialized local
    /// install candidate and cannot be reused by another transport.
    pub const unsafe fn assume_ready(
        table: crate::control_owner_slots::SlotTableId,
        epoch: TransportEpoch,
        physical_instance: NonZeroU64,
        config: OwnerConfig,
    ) -> Self {
        Self {
            table,
            epoch,
            physical_instance,
            config: Ok(config),
        }
    }

    /// Safety: `table`/`epoch` identify the exact dormant seed owning this one
    /// observation; no observation for this install has been minted before;
    /// and `physical_instance` names its current successfully initialized local
    /// install candidate whose owner geometry was unavailable and cannot be
    /// reused by another transport.
    pub const unsafe fn assume_unavailable(
        table: crate::control_owner_slots::SlotTableId,
        epoch: TransportEpoch,
        physical_instance: NonZeroU64,
        reason: DormantTransportUnavailable,
    ) -> Self {
        Self {
            table,
            epoch,
            physical_instance,
            config: Err(reason),
        }
    }

    pub const fn physical_instance(&self) -> NonZeroU64 {
        self.physical_instance
    }
}

#[must_use]
pub struct DormantTransportRemoval {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
    physical_instance: NonZeroU64,
}

impl DormantTransportRemoval {
    /// Safety: `table`/`epoch` identify the exact dormant seed, and its exact
    /// transport named by `physical_instance` is no longer installed and cannot
    /// publish more work or observations into it.
    pub const unsafe fn assume_removed(
        table: crate::control_owner_slots::SlotTableId,
        epoch: TransportEpoch,
        physical_instance: NonZeroU64,
    ) -> Self {
        Self {
            table,
            epoch,
            physical_instance,
        }
    }

    pub const fn physical_instance(&self) -> NonZeroU64 {
        self.physical_instance
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DormantOwnerRefusal {
    TableMismatch,
    EpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    TransportAlreadyObserved {
        current: NonZeroU64,
    },
    NoTransportObserved,
    PhysicalInstanceMismatch {
        expected: NonZeroU64,
        found: NonZeroU64,
    },
    PhysicalInstanceNotNewer {
        high_water: u64,
        found: NonZeroU64,
    },
}

#[must_use]
pub struct RefusedDormantTransition<T> {
    reason: DormantOwnerRefusal,
    input: T,
}

impl<T> RefusedDormantTransition<T> {
    pub const fn reason(&self) -> DormantOwnerRefusal {
        self.reason
    }

    pub fn into_input(self) -> T {
        self.input
    }
}

/// Move-only custody for a canonical owner identity before an operational
/// owner table exists.
///
/// This type deliberately owns no storage projection and exposes no activate,
/// reserve, dispatch, close, reset, or reopen transition. Observing a transport
/// records only inert provenance for a later atomic activation checkpoint.
#[must_use]
pub struct DormantOwnerSeed {
    root: SlotTableRoot,
    generation: TransportGeneration,
    physical_instance_high_water: u64,
    state: DormantOwnerState,
}

impl DormantOwnerSeed {
    /// Safety: `root` and `generation` descend from the same globally unique
    /// transport domain, and neither has any live descendant.
    pub const unsafe fn new(root: SlotTableRoot, generation: TransportGeneration) -> Self {
        Self {
            root,
            generation,
            physical_instance_high_water: 0,
            state: DormantOwnerState::NoTransport,
        }
    }

    pub const fn table(&self) -> crate::control_owner_slots::SlotTableId {
        self.root.id()
    }

    pub const fn epoch(&self) -> TransportEpoch {
        self.generation.epoch()
    }

    pub const fn state(&self) -> DormantOwnerState {
        self.state
    }

    pub fn observe_transport(
        &mut self,
        observation: DormantTransportObservation,
    ) -> Result<(), RefusedDormantTransition<DormantTransportObservation>> {
        if observation.table != self.root.id() {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::TableMismatch,
                input: observation,
            });
        }
        if observation.epoch != self.generation.epoch() {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::EpochMismatch {
                    expected: self.generation.epoch(),
                    found: observation.epoch,
                },
                input: observation,
            });
        }
        if observation.physical_instance.get() <= self.physical_instance_high_water {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::PhysicalInstanceNotNewer {
                    high_water: self.physical_instance_high_water,
                    found: observation.physical_instance,
                },
                input: observation,
            });
        }
        let current = match self.state {
            DormantOwnerState::NoTransport => None,
            DormantOwnerState::Ready {
                physical_instance, ..
            }
            | DormantOwnerState::Unavailable {
                physical_instance, ..
            } => Some(physical_instance),
        };
        if let Some(current) = current {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::TransportAlreadyObserved { current },
                input: observation,
            });
        }
        self.state = match observation.config {
            Ok(config) => DormantOwnerState::Ready {
                physical_instance: observation.physical_instance,
                config,
            },
            Err(reason) => DormantOwnerState::Unavailable {
                physical_instance: observation.physical_instance,
                reason,
            },
        };
        self.physical_instance_high_water = observation.physical_instance.get();
        Ok(())
    }

    pub fn observe_transport_removed(
        &mut self,
        removal: DormantTransportRemoval,
    ) -> Result<(), RefusedDormantTransition<DormantTransportRemoval>> {
        if removal.table != self.root.id() {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::TableMismatch,
                input: removal,
            });
        }
        if removal.epoch != self.generation.epoch() {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::EpochMismatch {
                    expected: self.generation.epoch(),
                    found: removal.epoch,
                },
                input: removal,
            });
        }
        let expected = match self.state {
            DormantOwnerState::NoTransport => {
                return Err(RefusedDormantTransition {
                    reason: DormantOwnerRefusal::NoTransportObserved,
                    input: removal,
                });
            }
            DormantOwnerState::Ready {
                physical_instance, ..
            }
            | DormantOwnerState::Unavailable {
                physical_instance, ..
            } => physical_instance,
        };
        if expected != removal.physical_instance {
            return Err(RefusedDormantTransition {
                reason: DormantOwnerRefusal::PhysicalInstanceMismatch {
                    expected,
                    found: removal.physical_instance,
                },
                input: removal,
            });
        }
        self.state = DormantOwnerState::NoTransport;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdmissionGate {
    Open,
    Closed,
}

#[must_use]
pub struct OwnerRundown {
    nonce: NonZeroU64,
}

pub struct ResourceRow<B> {
    lifecycle: Option<ResourceLifecycle<B>>,
    attachment_gate: AdmissionGate,
    window_gate: AdmissionGate,
    secondary_pairs: u64,
    windows: u64,
    creator_pair: Option<PairHandle>,
}

pub struct ContextRow<C> {
    lifecycle: Option<TransportContextLifecycle<C>>,
    association_admission_closed: bool,
}

enum PairRowState<A> {
    Initializing {
        leased: ManuallyDrop<LeasedAttachmentReservation>,
        association: ManuallyDrop<A>,
    },
    Creator(ManuallyDrop<A>),
    Secondary(ContextAttachmentLifecycle<A>),
    ReleasePending,
    Quarantined,
}

pub struct PairRow<A> {
    resource: SlotHandle<ResourceSlotKind>,
    context: SlotHandle<ContextSlotKind>,
    kind: PairKind,
    attachment: TransportAttachment,
    use_high_water: u64,
    uses: u64,
    state: PairRowState<A>,
}

enum WindowRowState<W> {
    Initializing(ManuallyDrop<W>),
    Live(WindowLifecycle<W>),
    ReleasePending,
    Quarantined,
}

enum OrphanPairCustody<A> {
    Initializing {
        pair: PairHandle,
        resource: ResourceHandle,
        context: ContextHandle,
        attachment: TransportAttachment,
        leased: ManuallyDrop<LeasedAttachmentReservation>,
        association: ManuallyDrop<A>,
        reservation: ManuallyDrop<TicketReservation<ResourceSlotKind>>,
    },
    CreatorLifecycleBegun {
        pair: PairHandle,
        resource: ResourceHandle,
        context: ContextHandle,
        attachment: TransportAttachment,
        association: ManuallyDrop<A>,
        post: ManuallyDrop<PostBeginReservation<ResourceSlotKind>>,
        request: ManuallyDrop<PreparedControl>,
    },
}

pub struct WindowRow<W> {
    resource: SlotHandle<ResourceSlotKind>,
    window: TransportWindow,
    map_info: Option<u32>,
    use_high_water: u64,
    uses: u64,
    state: WindowRowState<W>,
}

pub type ResourceOwnerSlot<B> = StableSlot<ResourceRow<B>>;
pub type ContextOwnerSlot<C> = StableSlot<ContextRow<C>>;
pub type PairOwnerSlot<A> = StableSlot<PairRow<A>>;
pub type WindowOwnerSlot<W> = StableSlot<WindowRow<W>>;
pub type ResourceOwnerTicket<E> = ControlTicketSlot<OwnerRundown, E, ResourceSlotKind>;
pub type ContextOwnerTicket<E> = ControlTicketSlot<OwnerRundown, E, ContextSlotKind>;
pub type PairOwnerTicket<E> = ControlTicketSlot<OwnerRundown, E, PairSlotKind>;
pub type WindowOwnerTicket<E> = ControlTicketSlot<OwnerRundown, E, WindowSlotKind>;

pub type ResourceHandle = SlotHandle<ResourceSlotKind>;
pub type ContextHandle = SlotHandle<ContextSlotKind>;
pub type PairHandle = SlotHandle<PairSlotKind>;
pub type WindowHandle = SlotHandle<WindowSlotKind>;

pub struct OwnerStorage<'a, B, C, A, W, E> {
    pub resources: &'a mut [ResourceOwnerSlot<B>],
    pub contexts: &'a mut [ContextOwnerSlot<C>],
    pub pairs: &'a mut [PairOwnerSlot<A>],
    pub windows: &'a mut [WindowOwnerSlot<W>],
    pub resource_tickets: &'a mut [ResourceOwnerTicket<E>],
    pub context_tickets: &'a mut [ContextOwnerTicket<E>],
    pub pair_tickets: &'a mut [PairOwnerTicket<E>],
    pub window_tickets: &'a mut [WindowOwnerTicket<E>],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DormantOwnerActivationRefusal {
    NoTransportObserved,
    TransportUnavailable(DormantTransportUnavailable),
    PhysicalInstanceMismatch {
        expected: NonZeroU64,
        found: NonZeroU64,
    },
    OwnerTable(OwnerTableRefusal),
}

#[must_use]
pub struct RefusedDormantOwnerActivation<'a, B, C, A, W, E> {
    reason: DormantOwnerActivationRefusal,
    seed: DormantOwnerSeed,
    storage: OwnerStorage<'a, B, C, A, W, E>,
}

impl<'a, B, C, A, W, E> RefusedDormantOwnerActivation<'a, B, C, A, W, E> {
    pub const fn reason(&self) -> DormantOwnerActivationRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (DormantOwnerSeed, OwnerStorage<'a, B, C, A, W, E>) {
        (self.seed, self.storage)
    }
}

impl DormantOwnerSeed {
    pub fn activate<'a, B, C, A, W, E>(
        self,
        physical_instance: NonZeroU64,
        storage: OwnerStorage<'a, B, C, A, W, E>,
    ) -> Result<OwnerTable<'a, B, C, A, W, E>, RefusedDormantOwnerActivation<'a, B, C, A, W, E>>
    {
        let config = match self.state {
            DormantOwnerState::NoTransport => {
                return Err(RefusedDormantOwnerActivation {
                    reason: DormantOwnerActivationRefusal::NoTransportObserved,
                    seed: self,
                    storage,
                });
            }
            DormantOwnerState::Unavailable { reason, .. } => {
                return Err(RefusedDormantOwnerActivation {
                    reason: DormantOwnerActivationRefusal::TransportUnavailable(reason),
                    seed: self,
                    storage,
                });
            }
            DormantOwnerState::Ready {
                physical_instance: expected,
                config,
            } => {
                if expected != physical_instance {
                    return Err(RefusedDormantOwnerActivation {
                        reason: DormantOwnerActivationRefusal::PhysicalInstanceMismatch {
                            expected,
                            found: physical_instance,
                        },
                        seed: self,
                        storage,
                    });
                }
                config
            }
        };

        if let Some(reason) = OwnerTable::<B, C, A, W, E>::validate_storage(&storage) {
            return Err(RefusedDormantOwnerActivation {
                reason: DormantOwnerActivationRefusal::OwnerTable(reason),
                seed: self,
                storage,
            });
        }

        let DormantOwnerSeed {
            root,
            generation,
            physical_instance_high_water,
            state,
        } = self;
        match unsafe { OwnerTable::new(root, generation, physical_instance, config, storage) } {
            Ok(table) => Ok(table),
            Err(refused) => {
                let (root, generation, _, storage) = refused.into_parts();
                Err(RefusedDormantOwnerActivation {
                    reason: DormantOwnerActivationRefusal::OwnerTable(
                        OwnerTableRefusal::InvariantLost,
                    ),
                    seed: DormantOwnerSeed {
                        root,
                        generation,
                        physical_instance_high_water,
                        state,
                    },
                    storage,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerTableRefusal {
    CapacityTooLarge,
    ResourceTicketCapacityMismatch,
    ContextTicketCapacityMismatch,
    PairTicketCapacityMismatch,
    WindowTicketCapacityMismatch,
    WindowBoundsInvalid,
    StorageNotFresh,
    WrongPhase { found: OwnerPhase },
    GenerationUnavailable,
    ResourceCapacityExhausted,
    ContextCapacityExhausted,
    PairCapacityExhausted,
    WindowCapacityExhausted,
    ResourceNotFound,
    ContextNotFound,
    PairNotFound,
    PairNotUsable,
    WindowNotFound,
    AttachmentAdmissionClosed,
    WindowAdmissionClosed,
    CanonicalPairExists { kind: PairKind },
    CanonicalWindowExists,
    WindowRangeOverlaps,
    WindowMisaligned,
    WindowOutOfBounds,
    PairUseExhausted,
    WindowUseExhausted,
    PairUseMismatch,
    WindowUseMismatch,
    PairUsesOutstanding { count: u64 },
    WindowUsesOutstanding { count: u64 },
    SecondaryPairsOutstanding { count: u64 },
    WindowsOutstanding { count: u64 },
    CreatorAttachmentOutstanding,
    RundownExhausted,
    ActiveRunExhausted,
    PhysicalStatusNotZero,
    ExternalActionOutstanding,
    ResetActionOutstanding,
    ResetActionMismatch,
    ResetCursorBlocked,
    SuccessorUnavailable,
    NextTransportMismatch,
    ResourceIdExhausted,
    ContextIdExhausted,
    AttachmentIdExhausted,
    ControlSequenceExhausted,
    AssociationCensusExhausted,
    WindowCensusExhausted,
    TicketRefused,
    LifecycleBeginRefused,
    WorkMismatch,
    CompletionRequiresRelease,
    OrphanCustodyOutstanding,
    InvariantLost,
}

#[derive(Clone, Copy)]
struct ResourceEdgeScan {
    creator: Option<PairHandle>,
    secondary: u64,
    windows: u64,
}

#[must_use]
pub struct RefusedOwnerTableNew<'a, B, C, A, W, E> {
    reason: OwnerTableRefusal,
    root: SlotTableRoot,
    generation: TransportGeneration,
    config: OwnerConfig,
    storage: OwnerStorage<'a, B, C, A, W, E>,
}

impl<'a, B, C, A, W, E> RefusedOwnerTableNew<'a, B, C, A, W, E> {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_parts(
        self,
    ) -> (
        SlotTableRoot,
        TransportGeneration,
        OwnerConfig,
        OwnerStorage<'a, B, C, A, W, E>,
    ) {
        (self.root, self.generation, self.config, self.storage)
    }
}

#[must_use]
pub enum RefusedCustody<T, Q> {
    Input(ManuallyDrop<T>),
    Quarantined(ManuallyDrop<Q>),
    TableQuarantined,
}

#[must_use]
pub struct RefusedAdmission<T, Q = T> {
    reason: OwnerTableRefusal,
    custody: RefusedCustody<T, Q>,
}

impl<T, Q> RefusedAdmission<T, Q> {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_custody(self) -> ReturnedCustody<T, Q> {
        match self.custody {
            RefusedCustody::Input(payload) => {
                ReturnedCustody::Input(ManuallyDrop::into_inner(payload))
            }
            RefusedCustody::Quarantined(payload) => {
                ReturnedCustody::Quarantined(ManuallyDrop::into_inner(payload))
            }
            RefusedCustody::TableQuarantined => ReturnedCustody::TableQuarantined,
        }
    }
}

#[must_use]
pub enum ReturnedCustody<T, Q> {
    Input(T),
    Quarantined(Q),
    TableQuarantined,
}

#[must_use]
pub struct PairUseLease {
    pair: PairHandle,
    id: NonZeroU64,
}

#[must_use]
pub struct WindowUseLease {
    window: WindowHandle,
    id: NonZeroU64,
}

#[must_use]
pub struct PreparedOwnerControl<K> {
    row: SlotHandle<K>,
    ticket: PreparedTicket<K>,
}

#[must_use]
pub struct PairAdmission<K> {
    pair: PairHandle,
    control: PreparedOwnerControl<K>,
}

impl<K> PairAdmission<K> {
    pub const fn pair(&self) -> PairHandle {
        self.pair
    }

    pub fn into_parts(self) -> (PairHandle, PreparedOwnerControl<K>) {
        (self.pair, self.control)
    }
}

#[must_use]
pub struct OwnerWindowAdmission {
    window: WindowHandle,
    control: PreparedOwnerControl<WindowSlotKind>,
}

impl OwnerWindowAdmission {
    pub const fn window(&self) -> WindowHandle {
        self.window
    }

    pub fn into_parts(self) -> (WindowHandle, PreparedOwnerControl<WindowSlotKind>) {
        (self.window, self.control)
    }
}

#[must_use]
pub struct DispatchWork<K> {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
    row: SlotHandle<K>,
    run_id: NonZeroU64,
    permit: DispatchPermit<K>,
}

#[must_use]
pub struct ObservedOwnerWork<E, K> {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
    row: SlotHandle<K>,
    run_id: NonZeroU64,
    observed: ObservedDispatch<E, K>,
}

impl<K> DispatchWork<K> {
    /// Safety: `runner` publishes this exact work at most once, returns its
    /// lossless written length, and retains no key or publication authority.
    pub unsafe fn run_once<E, F>(self, runner: F) -> ObservedOwnerWork<E, K>
    where
        F: FnOnce(ControlWireKey) -> RunnerOutcome<E>,
    {
        let observed = unsafe { self.permit.run_once(runner) };
        ObservedOwnerWork {
            table: self.table,
            epoch: self.epoch,
            row: self.row,
            run_id: self.run_id,
            observed,
        }
    }
}

#[must_use]
pub struct OwnerLifecycleAction<K> {
    row: SlotHandle<K>,
    action: LifecycleAction<K>,
}

#[must_use]
pub struct PendingResourceCompletion<E> {
    row: ResourceHandle,
    finish: ManuallyDrop<ResourceFinish<E>>,
    rundown: ManuallyDrop<RundownRelease<OwnerRundown, ResourceSlotKind>>,
}

#[must_use]
pub struct PendingContextCompletion<E> {
    row: ContextHandle,
    finish: ManuallyDrop<ContextFinish<E>>,
    rundown: ManuallyDrop<RundownRelease<OwnerRundown, ContextSlotKind>>,
}

#[must_use]
pub struct PendingPairCompletion<E> {
    row: PairHandle,
    finish: ManuallyDrop<AttachmentFinish<E>>,
    rundown: ManuallyDrop<RundownRelease<OwnerRundown, PairSlotKind>>,
}

#[must_use]
pub struct PendingWindowCompletion<E> {
    row: WindowHandle,
    finish: ManuallyDrop<WindowFinish<E>>,
    rundown: ManuallyDrop<RundownRelease<OwnerRundown, WindowSlotKind>>,
}

#[must_use]
pub struct PairPayloadAction<A, E> {
    association: ManuallyDrop<A>,
    effect: ManuallyDrop<AttachmentFinishEffect<E>>,
    pending: PendingPairPayloadAck,
}

#[must_use]
pub struct PendingPairPayloadAck {
    pair: PairHandle,
    resource: ResourceHandle,
    context: ContextHandle,
    released: Option<ManuallyDrop<crate::context_lifecycle::ReleasedAttachmentLease>>,
    rundown: Option<ManuallyDrop<RundownRelease<OwnerRundown, PairSlotKind>>>,
}

#[must_use]
pub struct FinalizedPairPayloadAck {
    pending: PendingPairPayloadAck,
}

impl<A, E> PairPayloadAction<A, E> {
    pub fn into_parts(self) -> (A, AttachmentFinishEffect<E>, PendingPairPayloadAck) {
        (
            ManuallyDrop::into_inner(self.association),
            ManuallyDrop::into_inner(self.effect),
            self.pending,
        )
    }
}

impl PendingPairPayloadAck {
    /// Safety: the association payload is fully finalized or irreversibly quarantined.
    pub unsafe fn assume_association_finalized(self) -> FinalizedPairPayloadAck {
        FinalizedPairPayloadAck { pending: self }
    }
}

#[must_use]
pub struct CreatorPayloadAction<A, E> {
    association: ManuallyDrop<A>,
    effect: ManuallyDrop<ResourceFinishEffect<E>>,
    pending: PendingCreatorPayloadAck,
}

#[must_use]
pub struct PendingCreatorPayloadAck {
    pair: PairHandle,
    resource: ResourceHandle,
    context: ContextHandle,
    released: Option<ManuallyDrop<crate::context_lifecycle::ReleasedAttachmentLease>>,
    rundown: Option<ManuallyDrop<RundownRelease<OwnerRundown, ResourceSlotKind>>>,
}

#[must_use]
pub struct FinalizedCreatorPayloadAck {
    pending: PendingCreatorPayloadAck,
}

impl<A, E> CreatorPayloadAction<A, E> {
    pub fn into_parts(self) -> (A, ResourceFinishEffect<E>, PendingCreatorPayloadAck) {
        (
            ManuallyDrop::into_inner(self.association),
            ManuallyDrop::into_inner(self.effect),
            self.pending,
        )
    }
}

impl PendingCreatorPayloadAck {
    /// Safety: the creator association is fully finalized or irreversibly quarantined.
    pub unsafe fn assume_association_finalized(self) -> FinalizedCreatorPayloadAck {
        FinalizedCreatorPayloadAck { pending: self }
    }
}

#[must_use]
pub struct WindowPayloadAction<W, E> {
    reservation: ManuallyDrop<W>,
    effect: ManuallyDrop<WindowFinishEffect<E>>,
    pending: PendingWindowPayloadAck,
}

#[must_use]
pub struct PendingWindowPayloadAck {
    window: WindowHandle,
    resource: ResourceHandle,
    rundown: Option<ManuallyDrop<RundownRelease<OwnerRundown, WindowSlotKind>>>,
}

#[must_use]
pub struct FinalizedWindowPayloadAck {
    pending: PendingWindowPayloadAck,
}

#[must_use]
pub struct ResourcePayloadAction<B, E> {
    backing: ManuallyDrop<B>,
    effect: ManuallyDrop<ResourceFinishEffect<E>>,
    pending: PendingResourcePayloadAck,
}

#[must_use]
pub struct PendingResourcePayloadAck {
    resource: ResourceHandle,
    rundown: Option<ManuallyDrop<RundownRelease<OwnerRundown, ResourceSlotKind>>>,
}

#[must_use]
pub struct FinalizedResourcePayloadAck {
    pending: PendingResourcePayloadAck,
}

impl<B, E> ResourcePayloadAction<B, E> {
    pub fn into_parts(self) -> (B, ResourceFinishEffect<E>, PendingResourcePayloadAck) {
        (
            ManuallyDrop::into_inner(self.backing),
            ManuallyDrop::into_inner(self.effect),
            self.pending,
        )
    }
}

impl PendingResourcePayloadAck {
    /// Safety: the resource backing is fully finalized or irreversibly quarantined.
    pub unsafe fn assume_backing_finalized(self) -> FinalizedResourcePayloadAck {
        FinalizedResourcePayloadAck { pending: self }
    }
}

#[must_use]
pub struct ContextPayloadAction<C, E> {
    owner: ManuallyDrop<C>,
    effect: ManuallyDrop<ContextFinishEffect<E>>,
    pending: PendingContextPayloadAck,
}

#[must_use]
pub struct PendingContextPayloadAck {
    context: ContextHandle,
    rundown: Option<ManuallyDrop<RundownRelease<OwnerRundown, ContextSlotKind>>>,
}

#[must_use]
pub struct FinalizedContextPayloadAck {
    pending: PendingContextPayloadAck,
}

impl<C, E> ContextPayloadAction<C, E> {
    pub fn into_parts(self) -> (C, ContextFinishEffect<E>, PendingContextPayloadAck) {
        (
            ManuallyDrop::into_inner(self.owner),
            ManuallyDrop::into_inner(self.effect),
            self.pending,
        )
    }
}

impl PendingContextPayloadAck {
    /// Safety: the context owner is fully finalized or irreversibly quarantined.
    pub unsafe fn assume_owner_finalized(self) -> FinalizedContextPayloadAck {
        FinalizedContextPayloadAck { pending: self }
    }
}

impl<W, E> WindowPayloadAction<W, E> {
    pub fn into_parts(self) -> (W, WindowFinishEffect<E>, PendingWindowPayloadAck) {
        (
            ManuallyDrop::into_inner(self.reservation),
            ManuallyDrop::into_inner(self.effect),
            self.pending,
        )
    }
}

impl PendingWindowPayloadAck {
    /// Safety: the range reservation is fully finalized or irreversibly quarantined.
    pub unsafe fn assume_reservation_finalized(self) -> FinalizedWindowPayloadAck {
        FinalizedWindowPayloadAck { pending: self }
    }
}

impl<E> PendingResourceCompletion<E> {
    pub fn effect(&self) -> &ResourceFinishEffect<E> {
        self.finish.effect()
    }
}

impl<E> PendingContextCompletion<E> {
    pub fn effect(&self) -> &ContextFinishEffect<E> {
        self.finish.effect()
    }
}

impl<E> PendingPairCompletion<E> {
    pub fn effect(&self) -> &AttachmentFinishEffect<E> {
        self.finish.effect()
    }
}

impl<E> PendingWindowCompletion<E> {
    pub fn effect(&self) -> &WindowFinishEffect<E> {
        self.finish.effect()
    }
}

#[must_use]
pub enum OwnerApplyRefusal<K> {
    Recoverable {
        reason: OwnerTableRefusal,
        action: OwnerLifecycleAction<K>,
    },
    Poisoned,
}

#[must_use]
pub struct RefusedCompletion<A> {
    reason: OwnerTableRefusal,
    action: ManuallyDrop<A>,
}

#[must_use]
pub enum OwnerFinalizationRefusal<A> {
    Recoverable {
        reason: OwnerTableRefusal,
        action: A,
    },
    Poisoned,
}

impl<A> RefusedCompletion<A> {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_action(self) -> A {
        ManuallyDrop::into_inner(self.action)
    }
}

#[must_use]
pub struct RefusedWork<T> {
    reason: OwnerTableRefusal,
    work: ManuallyDrop<T>,
}

impl<T> RefusedWork<T> {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_work(self) -> T {
        ManuallyDrop::into_inner(self.work)
    }
}

#[must_use]
pub struct ResetPreparation {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
    physical_instance: NonZeroU64,
}

#[must_use]
pub struct ExternalRunnerRundown {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
}

#[must_use]
pub struct RefusedResetPreparation {
    reason: OwnerTableRefusal,
    rundown: ExternalRunnerRundown,
}

impl RefusedResetPreparation {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_rundown(self) -> ExternalRunnerRundown {
        self.rundown
    }
}

#[must_use]
pub struct RefusedPhysicalReset {
    reason: OwnerTableRefusal,
    preparation: ResetPreparation,
}

impl RefusedPhysicalReset {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_preparation(self) -> ResetPreparation {
        self.preparation
    }
}

#[must_use]
pub struct VerifiedPhysicalReset {
    preparation: ResetPreparation,
}

impl ResetPreparation {
    pub const fn physical_instance(&self) -> NonZeroU64 {
        self.physical_instance
    }

    /// Safety: `raw_status` came from a volatile read of this exact transport;
    /// every runner, pair/window user, and external effect is drained, and no
    /// old work can publish or retain backend custody.
    pub unsafe fn verify_raw_zero(
        self,
        raw_status: u32,
    ) -> Result<VerifiedPhysicalReset, RefusedPhysicalReset> {
        if raw_status != 0 {
            return Err(RefusedPhysicalReset {
                reason: OwnerTableRefusal::PhysicalStatusNotZero,
                preparation: self,
            });
        }
        Ok(VerifiedPhysicalReset { preparation: self })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetStage {
    Windows,
    SecondaryPairs,
    Resources,
    Contexts,
}

#[derive(Clone, Copy)]
struct ResetActionIdentity {
    table: crate::control_owner_slots::SlotTableId,
    epoch: TransportEpoch,
    stage: ResetStage,
    index: u32,
}

#[must_use]
pub struct PendingResetPayload {
    identity: ResetActionIdentity,
}

#[must_use]
pub struct FinalizedResetPayload {
    identity: ResetActionIdentity,
}

impl PendingResetPayload {
    /// Safety: every payload in the reset action is fully finalized or
    /// irreversibly quarantined at its required execution context.
    pub unsafe fn assume_payloads_finalized(self) -> FinalizedResetPayload {
        FinalizedResetPayload {
            identity: self.identity,
        }
    }
}

#[must_use]
pub struct ResetWindowAction<W> {
    reservation: ManuallyDrop<W>,
    map_info: Option<u32>,
    pending: PendingResetPayload,
}

impl<W> ResetWindowAction<W> {
    pub fn into_parts(self) -> (W, Option<u32>, PendingResetPayload) {
        (
            ManuallyDrop::into_inner(self.reservation),
            self.map_info,
            self.pending,
        )
    }
}

#[must_use]
pub struct ResetPairAction<A> {
    association: ManuallyDrop<A>,
    pending: PendingResetPayload,
}

impl<A> ResetPairAction<A> {
    pub fn into_parts(self) -> (A, PendingResetPayload) {
        (ManuallyDrop::into_inner(self.association), self.pending)
    }
}

#[must_use]
pub struct ResetResourceAction<B, A> {
    backing: ManuallyDrop<B>,
    creator_association: Option<ManuallyDrop<A>>,
    pending: PendingResetPayload,
}

impl<B, A> ResetResourceAction<B, A> {
    pub fn into_parts(self) -> (B, Option<A>, PendingResetPayload) {
        (
            ManuallyDrop::into_inner(self.backing),
            self.creator_association.map(ManuallyDrop::into_inner),
            self.pending,
        )
    }
}

#[must_use]
pub struct ResetContextAction<C> {
    owner: ManuallyDrop<C>,
    pending: PendingResetPayload,
}

impl<C> ResetContextAction<C> {
    pub fn into_parts(self) -> (C, PendingResetPayload) {
        (ManuallyDrop::into_inner(self.owner), self.pending)
    }
}

#[must_use]
pub enum OwnerResetAction<B, C, A, W> {
    Window(ResetWindowAction<W>),
    SecondaryPair(ResetPairAction<A>),
    Resource(ResetResourceAction<B, A>),
    Context(ResetContextAction<C>),
}

enum PendingResetTicket<K> {
    Empty,
    Reset {
        rundown: ManuallyDrop<OwnerRundown>,
        ack: PendingResetAck<K>,
    },
}

enum StalledResetTicket {
    Window(PendingResetTicket<WindowSlotKind>),
    Pair(PendingResetTicket<PairSlotKind>),
    Resource(PendingResetTicket<ResourceSlotKind>),
    Context(PendingResetTicket<ContextSlotKind>),
}

struct PendingCreatorReset {
    pair: PairHandle,
    context: ContextHandle,
    release: ManuallyDrop<ReleasedAttachmentLease>,
}

enum PendingResetAction {
    Stalled {
        identity: ResetActionIdentity,
        ticket: StalledResetTicket,
    },
    Window {
        identity: ResetActionIdentity,
        window: WindowHandle,
        resource: ResourceHandle,
        ticket: PendingResetTicket<WindowSlotKind>,
    },
    SecondaryPair {
        identity: ResetActionIdentity,
        pair: PairHandle,
        resource: ResourceHandle,
        context: ContextHandle,
        release: ManuallyDrop<ReleasedAttachmentLease>,
        ticket: PendingResetTicket<PairSlotKind>,
    },
    InitializingPair {
        identity: ResetActionIdentity,
        pair: PairHandle,
        resource: ResourceHandle,
        context: ContextHandle,
        attachment: TransportAttachment,
        leased: ManuallyDrop<LeasedAttachmentReservation>,
        reservation: ManuallyDrop<TicketReservation<ResourceSlotKind>>,
    },
    Resource {
        identity: ResetActionIdentity,
        resource: ResourceHandle,
        creator: Option<PendingCreatorReset>,
        ticket: PendingResetTicket<ResourceSlotKind>,
    },
    Context {
        identity: ResetActionIdentity,
        context: ContextHandle,
        ticket: PendingResetTicket<ContextSlotKind>,
    },
}

#[allow(dead_code)]
enum OrphanResetCustody<B, C> {
    ResourceLifecycle {
        identity: ResetActionIdentity,
        resource: ResourceHandle,
        lifecycle: ManuallyDrop<ResourceLifecycle<B>>,
        release: Option<ManuallyDrop<ReleasedAttachmentLease>>,
        ticket: PendingResetTicket<ResourceSlotKind>,
    },
    Resource {
        identity: ResetActionIdentity,
        resource: ResourceHandle,
        backing: ManuallyDrop<B>,
        release: Option<ManuallyDrop<ReleasedAttachmentLease>>,
        ticket: PendingResetTicket<ResourceSlotKind>,
    },
    ContextLifecycle {
        identity: ResetActionIdentity,
        context: ContextHandle,
        lifecycle: ManuallyDrop<TransportContextLifecycle<C>>,
        ticket: PendingResetTicket<ContextSlotKind>,
    },
}

#[must_use]
pub struct NextTransportRequest {
    table: crate::control_owner_slots::SlotTableId,
    retired: TransportEpoch,
    successor: TransportEpoch,
}

#[must_use]
pub struct NextTransportReady {
    request: NextTransportRequest,
    physical_instance: NonZeroU64,
    config: OwnerConfig,
}

#[must_use]
pub struct RefusedNextTransportReady {
    reason: OwnerTableRefusal,
    ready: NextTransportReady,
}

impl RefusedNextTransportReady {
    pub const fn reason(&self) -> OwnerTableRefusal {
        self.reason
    }

    pub fn into_ready(self) -> NextTransportReady {
        self.ready
    }
}

impl NextTransportRequest {
    /// Safety: the exact successor transport is initialized and published,
    /// with no old transport authority retained at this physical instance.
    pub unsafe fn assume_ready(
        self,
        physical_instance: NonZeroU64,
        config: OwnerConfig,
    ) -> NextTransportReady {
        NextTransportReady {
            request: self,
            physical_instance,
            config,
        }
    }
}

pub struct OwnerTable<'a, B, C, A, W, E> {
    root: SlotTableRoot,
    generation: Option<TransportGeneration>,
    physical_instance: NonZeroU64,
    physical_instance_high_water: u64,
    config: OwnerConfig,
    phase: OwnerPhase,
    rundown_high_water: u64,
    run_high_water: u64,
    active_runs: u64,
    orphan_pair_custody: Option<OrphanPairCustody<A>>,
    orphan_reset_custody: Option<OrphanResetCustody<B, C>>,
    reset: Option<TransportReset>,
    sealed_next: Option<TransportGeneration>,
    reset_exhausted: bool,
    reset_stage: ResetStage,
    reset_index: u32,
    pending_reset: Option<PendingResetAction>,
    next_request_issued: bool,
    resources: StableSlots<'a, ResourceRow<B>, ResourceSlotKind>,
    contexts: StableSlots<'a, ContextRow<C>, ContextSlotKind>,
    pairs: StableSlots<'a, PairRow<A>, PairSlotKind>,
    windows: StableSlots<'a, WindowRow<W>, WindowSlotKind>,
    resource_tickets: ControlTickets<'a, OwnerRundown, E, ResourceSlotKind>,
    context_tickets: ControlTickets<'a, OwnerRundown, E, ContextSlotKind>,
    pair_tickets: ControlTickets<'a, OwnerRundown, E, PairSlotKind>,
    window_tickets: ControlTickets<'a, OwnerRundown, E, WindowSlotKind>,
    marker: PhantomData<fn(E) -> E>,
}

impl<'a, B, C, A, W, E> OwnerTable<'a, B, C, A, W, E> {
    /// Safety: `root` and every slice are the unique canonical storage for this
    /// owner. No descendant from a prior binding of these slices remains live.
    pub unsafe fn new(
        root: SlotTableRoot,
        generation: TransportGeneration,
        physical_instance: NonZeroU64,
        config: OwnerConfig,
        storage: OwnerStorage<'a, B, C, A, W, E>,
    ) -> Result<Self, RefusedOwnerTableNew<'a, B, C, A, W, E>> {
        let reason = Self::validate_storage(&storage);
        if let Some(reason) = reason {
            return Err(RefusedOwnerTableNew {
                reason,
                root,
                generation,
                config,
                storage,
            });
        }
        let table = root.id();
        let epoch = generation.epoch();
        let OwnerStorage {
            resources,
            contexts,
            pairs,
            windows,
            resource_tickets,
            context_tickets,
            pair_tickets,
            window_tickets,
        } = storage;
        Ok(Self {
            root,
            generation: Some(generation),
            physical_instance,
            physical_instance_high_water: physical_instance.get(),
            config,
            phase: OwnerPhase::Open,
            rundown_high_water: 0,
            run_high_water: 0,
            active_runs: 0,
            orphan_pair_custody: None,
            orphan_reset_custody: None,
            reset: None,
            sealed_next: None,
            reset_exhausted: false,
            reset_stage: ResetStage::Windows,
            reset_index: 0,
            pending_reset: None,
            next_request_issued: false,
            resources: unsafe { StableSlots::new_canonical(table, epoch, resources) },
            contexts: unsafe { StableSlots::new_canonical(table, epoch, contexts) },
            pairs: unsafe { StableSlots::new_canonical(table, epoch, pairs) },
            windows: unsafe { StableSlots::new_canonical(table, epoch, windows) },
            resource_tickets: unsafe {
                ControlTickets::new_canonical(table, epoch, resource_tickets)
            },
            context_tickets: unsafe {
                ControlTickets::new_canonical(table, epoch, context_tickets)
            },
            pair_tickets: unsafe { ControlTickets::new_canonical(table, epoch, pair_tickets) },
            window_tickets: unsafe { ControlTickets::new_canonical(table, epoch, window_tickets) },
            marker: PhantomData,
        })
    }

    pub const fn phase(&self) -> OwnerPhase {
        self.phase
    }

    pub fn epoch(&self) -> Option<crate::control_ownership::TransportEpoch> {
        self.generation.as_ref().map(TransportGeneration::epoch)
    }

    pub const fn physical_instance(&self) -> NonZeroU64 {
        self.physical_instance
    }

    pub const fn config(&self) -> OwnerConfig {
        self.config
    }

    pub const fn active_runs(&self) -> u64 {
        self.active_runs
    }

    pub fn close(&mut self) -> Result<(), OwnerTableRefusal> {
        if self.phase != OwnerPhase::Open {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        self.phase = OwnerPhase::Closing;
        Ok(())
    }

    /// Seal ordinary lifecycle dispatch after an exact owner has lost proof of
    /// teardown. Existing dispatched work may still finish, but only verified
    /// physical reset may release the retained rows. This is idempotent so two
    /// subordinate objects can report the same cleanup failure without
    /// reopening or advancing the table.
    pub fn quarantine(&mut self) -> Result<(), OwnerTableRefusal> {
        match self.phase {
            OwnerPhase::Open | OwnerPhase::Closing | OwnerPhase::Quarantined => {
                self.enter_quarantined();
                Ok(())
            }
            found => Err(OwnerTableRefusal::WrongPhase { found }),
        }
    }

    /// Safety: every dispatch runner has returned or is irreversibly revoked;
    /// no copied old wire tuple or observation authority can be published.
    /// This call atomically seals every later lifecycle begin and dispatch.
    pub unsafe fn assume_external_runner_rundown(
        &mut self,
    ) -> Result<ExternalRunnerRundown, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Closing | OwnerPhase::Quarantined) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let rundown = ExternalRunnerRundown {
            table: self.root.id(),
            epoch: self
                .epoch()
                .ok_or(OwnerTableRefusal::GenerationUnavailable)?,
        };
        self.active_runs = 0;
        self.phase = OwnerPhase::RundownSealed;
        Ok(rundown)
    }

    pub fn prepare_reset(
        &mut self,
        rundown: ExternalRunnerRundown,
    ) -> Result<ResetPreparation, RefusedResetPreparation> {
        let refuse = |reason, rundown| RefusedResetPreparation { reason, rundown };
        if self.phase != OwnerPhase::RundownSealed {
            return Err(refuse(
                OwnerTableRefusal::WrongPhase { found: self.phase },
                rundown,
            ));
        }
        let Some(epoch) = self.epoch() else {
            return Err(refuse(OwnerTableRefusal::GenerationUnavailable, rundown));
        };
        if rundown.table != self.root.id() || rundown.epoch != epoch {
            return Err(refuse(OwnerTableRefusal::ResetActionMismatch, rundown));
        }
        if !self.local_uses_drained() {
            return Err(refuse(
                OwnerTableRefusal::ExternalActionOutstanding,
                rundown,
            ));
        }
        if self.pending_reset.is_some() {
            return Err(refuse(OwnerTableRefusal::ResetActionOutstanding, rundown));
        }
        if self.has_release_pending_or_extracted() {
            return Err(refuse(
                OwnerTableRefusal::ExternalActionOutstanding,
                rundown,
            ));
        }
        if self.active_runs != 0 {
            return Err(refuse(
                OwnerTableRefusal::ExternalActionOutstanding,
                rundown,
            ));
        }
        self.phase = OwnerPhase::ResetPrepared;
        Ok(ResetPreparation {
            table: self.root.id(),
            epoch,
            physical_instance: self.physical_instance,
        })
    }

    pub fn authorize_reset(
        &mut self,
        verified: VerifiedPhysicalReset,
    ) -> Result<(), RefusedPhysicalReset> {
        let preparation = verified.preparation;
        let exact = self.phase == OwnerPhase::ResetPrepared
            && preparation.table == self.root.id()
            && Some(preparation.epoch) == self.epoch()
            && preparation.physical_instance == self.physical_instance
            && self.active_runs == 0
            && self.local_uses_drained()
            && !self.has_release_pending_or_extracted();
        if !exact {
            return Err(RefusedPhysicalReset {
                reason: OwnerTableRefusal::ResetActionMismatch,
                preparation,
            });
        }
        let Some(generation) = self.generation.take() else {
            return Err(RefusedPhysicalReset {
                reason: OwnerTableRefusal::GenerationUnavailable,
                preparation,
            });
        };
        self.phase = OwnerPhase::ResetAuthorized;
        let retirement = unsafe { generation.advance() };
        let (successor, reset) = retirement.into_parts();
        match successor {
            TransportSuccessor::Next(next) => {
                self.sealed_next = Some(next);
                self.reset_exhausted = false;
            }
            TransportSuccessor::EpochExhausted(_) => {
                self.sealed_next = None;
                self.reset_exhausted = true;
            }
        }
        self.reset = Some(reset);
        self.reset_stage = ResetStage::Windows;
        self.reset_index = 0;
        self.phase = OwnerPhase::Resetting;
        Ok(())
    }

    pub fn next_reset_action(
        &mut self,
    ) -> Result<Option<OwnerResetAction<B, C, A, W>>, OwnerTableRefusal> {
        if self.phase != OwnerPhase::Resetting {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        if self.pending_reset.is_some() {
            return Err(OwnerTableRefusal::ResetActionOutstanding);
        }
        loop {
            let action = match self.reset_stage {
                ResetStage::Windows => self.next_window_reset_action()?,
                ResetStage::SecondaryPairs => self.next_pair_reset_action()?,
                ResetStage::Resources => self.next_resource_reset_action()?,
                ResetStage::Contexts => self.next_context_reset_action()?,
            };
            if action.is_some() {
                return Ok(action);
            }
            if !self.advance_reset_stage()? {
                return Ok(None);
            }
        }
    }

    pub fn ack_reset_action(
        &mut self,
        action: FinalizedResetPayload,
    ) -> Result<(), OwnerFinalizationRefusal<FinalizedResetPayload>> {
        if self.phase != OwnerPhase::Resetting {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action,
            });
        }
        let Some(pending) = self.pending_reset.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ResetActionMismatch,
                action,
            });
        };
        if !Self::reset_action_matches(&pending, action.identity) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ResetActionMismatch,
                action,
            });
        }
        let Some(next) = self.reset_index.checked_add(1) else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action,
            });
        };
        if !self.can_commit_reset_action(pending) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action,
            });
        }
        let pending = match self.pending_reset.take() {
            Some(pending) => pending,
            None => unsafe { core::hint::unreachable_unchecked() },
        };
        unsafe {
            match pending {
                PendingResetAction::Stalled { .. } => {
                    core::hint::unreachable_unchecked();
                }
                PendingResetAction::Window {
                    window,
                    resource,
                    ticket,
                    ..
                } => self.commit_window_reset_validated(window, resource, ticket),
                PendingResetAction::SecondaryPair {
                    pair,
                    resource,
                    context,
                    release,
                    ticket,
                    ..
                } => self.commit_pair_reset_validated(
                    pair,
                    resource,
                    context,
                    ManuallyDrop::into_inner(release),
                    ticket,
                ),
                PendingResetAction::InitializingPair {
                    pair,
                    resource,
                    context,
                    attachment,
                    leased,
                    reservation,
                    ..
                } => self.commit_initializing_pair_reset_validated(
                    pair,
                    resource,
                    context,
                    attachment,
                    ManuallyDrop::into_inner(leased),
                    ManuallyDrop::into_inner(reservation),
                ),
                PendingResetAction::Resource {
                    resource,
                    creator,
                    ticket,
                    ..
                } => self.commit_resource_reset_validated(resource, creator, ticket),
                PendingResetAction::Context {
                    context, ticket, ..
                } => self.commit_context_reset_validated(context, ticket),
            }
        }
        self.reset_index = next;
        Ok(())
    }

    pub fn request_next_transport(&mut self) -> Result<NextTransportRequest, OwnerTableRefusal> {
        if self.phase != OwnerPhase::Ready {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let successor = self
            .sealed_next
            .as_ref()
            .ok_or(OwnerTableRefusal::SuccessorUnavailable)?
            .epoch();
        let retired = self
            .reset
            .as_ref()
            .ok_or(OwnerTableRefusal::InvariantLost)?
            .retired_epoch();
        if self.next_request_issued {
            return Err(OwnerTableRefusal::ExternalActionOutstanding);
        }
        self.next_request_issued = true;
        Ok(NextTransportRequest {
            table: self.root.id(),
            retired,
            successor,
        })
    }

    pub fn reopen(&mut self, ready: NextTransportReady) -> Result<(), RefusedNextTransportReady> {
        let expected_successor = self.sealed_next.as_ref().map(TransportGeneration::epoch);
        let exact = self.phase == OwnerPhase::Ready
            && ready.request.table == self.root.id()
            && self.reset.as_ref().map(TransportReset::retired_epoch)
                == Some(ready.request.retired)
            && expected_successor == Some(ready.request.successor)
            && ready.physical_instance.get() > self.physical_instance_high_water;
        if !exact {
            return Err(RefusedNextTransportReady {
                reason: OwnerTableRefusal::NextTransportMismatch,
                ready,
            });
        }
        let Some(next) = self.sealed_next.take() else {
            return Err(RefusedNextTransportReady {
                reason: OwnerTableRefusal::SuccessorUnavailable,
                ready,
            });
        };
        if !self.validate_successor_rebind() {
            self.sealed_next = Some(next);
            return Err(RefusedNextTransportReady {
                reason: OwnerTableRefusal::InvariantLost,
                ready,
            });
        }
        let epoch = next.epoch();
        unsafe { self.apply_successor_rebind(epoch) };
        self.generation = Some(next);
        self.physical_instance = ready.physical_instance;
        self.physical_instance_high_water = ready.physical_instance.get();
        self.config = ready.config;
        self.reset = None;
        self.reset_exhausted = false;
        self.reset_stage = ResetStage::Windows;
        self.reset_index = 0;
        self.next_request_issued = false;
        self.phase = OwnerPhase::Open;
        Ok(())
    }

    /// Return an exact rejected install candidate without spending the sealed
    /// successor. This is the StartDevice failure edge: the physical candidate
    /// is reset and discarded, then a later candidate may request the same
    /// successor epoch.
    pub fn cancel_next_transport(
        &mut self,
        ready: NextTransportReady,
    ) -> Result<(), RefusedNextTransportReady> {
        let exact = self.phase == OwnerPhase::Ready
            && self.next_request_issued
            && ready.request.table == self.root.id()
            && self.reset.as_ref().map(TransportReset::retired_epoch)
                == Some(ready.request.retired)
            && self.sealed_next.as_ref().map(TransportGeneration::epoch)
                == Some(ready.request.successor);
        if !exact {
            return Err(RefusedNextTransportReady {
                reason: OwnerTableRefusal::NextTransportMismatch,
                ready,
            });
        }
        self.next_request_issued = false;
        Ok(())
    }

    pub fn resource(&self, handle: ResourceHandle) -> Result<TransportResource, OwnerTableRefusal> {
        self.resources
            .get(handle)
            .and_then(|row| {
                row.lifecycle
                    .as_ref()
                    .ok_or(crate::control_owner_slots::SlotRefusal::WrongState {
                        expected: SlotState::Occupied,
                        found: SlotState::Tombstone,
                    })
            })
            .map(ResourceLifecycle::resource)
            .map_err(|_| OwnerTableRefusal::ResourceNotFound)
    }

    pub fn context(&self, handle: ContextHandle) -> Result<TransportContext, OwnerTableRefusal> {
        self.contexts
            .get(handle)
            .and_then(|row| {
                row.lifecycle
                    .as_ref()
                    .ok_or(crate::control_owner_slots::SlotRefusal::WrongState {
                        expected: SlotState::Occupied,
                        found: SlotState::Tombstone,
                    })
            })
            .map(TransportContextLifecycle::context)
            .map_err(|_| OwnerTableRefusal::ContextNotFound)
    }

    pub fn resource_handle_by_id(&self, id: u32) -> Result<ResourceHandle, OwnerTableRefusal> {
        self.resources
            .find_unique_occupied_handle(|row| {
                row.lifecycle
                    .as_ref()
                    .is_some_and(|lifecycle| lifecycle.resource().id() == id)
            })
            .map_err(|()| OwnerTableRefusal::InvariantLost)?
            .ok_or(OwnerTableRefusal::ResourceNotFound)
    }

    pub fn context_handle_by_id(&self, id: u32) -> Result<ContextHandle, OwnerTableRefusal> {
        self.contexts
            .find_unique_occupied_handle(|row| {
                row.lifecycle
                    .as_ref()
                    .is_some_and(|lifecycle| lifecycle.context().id() == id)
            })
            .map_err(|()| OwnerTableRefusal::InvariantLost)?
            .ok_or(OwnerTableRefusal::ContextNotFound)
    }

    pub fn pair_handle_by_ids(
        &self,
        resource_id: u32,
        context_id: u32,
        kind: PairKind,
    ) -> Result<PairHandle, OwnerTableRefusal> {
        let resource = self.resource_handle_by_id(resource_id)?;
        let context = self.context_handle_by_id(context_id)?;
        self.pairs
            .find_unique_occupied_handle(|row| {
                row.resource == resource && row.context == context && row.kind == kind
            })
            .map_err(|()| OwnerTableRefusal::InvariantLost)?
            .ok_or(OwnerTableRefusal::PairNotFound)
    }

    pub fn window_handle_by_resource_id(
        &self,
        resource_id: u32,
    ) -> Result<WindowHandle, OwnerTableRefusal> {
        let resource = self.resource_handle_by_id(resource_id)?;
        self.windows
            .find_unique_occupied_handle(|row| row.resource == resource)
            .map_err(|()| OwnerTableRefusal::InvariantLost)?
            .ok_or(OwnerTableRefusal::WindowNotFound)
    }

    pub fn resource_backing(&self, handle: ResourceHandle) -> Result<&B, OwnerTableRefusal> {
        self.resources
            .get(handle)
            .ok()
            .and_then(|row| row.lifecycle.as_ref())
            .map(ResourceLifecycle::backing)
            .ok_or(OwnerTableRefusal::ResourceNotFound)
    }

    pub fn first_resource_handle_where<F>(
        &self,
        mut predicate: F,
    ) -> Result<Option<ResourceHandle>, OwnerTableRefusal>
    where
        F: FnMut(&B) -> bool,
    {
        Ok(self.resources.find_occupied_handle(|row| {
            row.lifecycle
                .as_ref()
                .is_some_and(|lifecycle| predicate(lifecycle.backing()))
        }))
    }

    pub fn context_owner(&self, handle: ContextHandle) -> Result<&C, OwnerTableRefusal> {
        self.contexts
            .get(handle)
            .ok()
            .and_then(|row| row.lifecycle.as_ref())
            .map(TransportContextLifecycle::owner)
            .ok_or(OwnerTableRefusal::ContextNotFound)
    }

    pub fn first_context_handle_by_owner(
        &self,
        owner: &C,
    ) -> Result<Option<ContextHandle>, OwnerTableRefusal>
    where
        C: PartialEq,
    {
        Ok(self.contexts.find_occupied_handle(|row| {
            row.lifecycle
                .as_ref()
                .is_some_and(|lifecycle| lifecycle.owner() == owner)
        }))
    }

    pub fn mapped_window_info(&self, handle: WindowHandle) -> Result<u32, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let row = self
            .windows
            .get(handle)
            .map_err(|_| OwnerTableRefusal::WindowNotFound)?;
        let WindowRowState::Live(lifecycle) = &row.state else {
            return Err(OwnerTableRefusal::WindowNotFound);
        };
        if lifecycle.phase() != crate::control_ownership::WindowPhase::Mapped {
            return Err(OwnerTableRefusal::WindowNotFound);
        }
        row.map_info.ok_or(OwnerTableRefusal::InvariantLost)
    }

    pub fn mapped_window(
        &self,
        handle: WindowHandle,
    ) -> Result<TransportWindow, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let row = self
            .windows
            .get(handle)
            .map_err(|_| OwnerTableRefusal::WindowNotFound)?;
        let WindowRowState::Live(lifecycle) = &row.state else {
            return Err(OwnerTableRefusal::WindowNotFound);
        };
        if lifecycle.phase() != crate::control_ownership::WindowPhase::Mapped {
            return Err(OwnerTableRefusal::WindowNotFound);
        }
        Ok(lifecycle.window())
    }

    /// Exact mapped resource whose live window contains `offset`. Initializing,
    /// unmapping, release-pending, and quarantined rows deliberately do not
    /// answer: none proves bytes are currently addressable at that offset.
    pub fn mapped_resource_at_offset(&self, offset: u64) -> Result<Option<u32>, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        for index in 0..self.windows.capacity() {
            let Some(handle) = self.windows.occupied_handle_at(index) else {
                continue;
            };
            let row = self
                .windows
                .get(handle)
                .map_err(|_| OwnerTableRefusal::InvariantLost)?;
            let WindowRowState::Live(lifecycle) = &row.state else {
                continue;
            };
            if lifecycle.phase() != crate::control_ownership::WindowPhase::Mapped {
                continue;
            }
            let window = lifecycle.window();
            let end = window
                .offset()
                .checked_add(window.length())
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            if window.offset() <= offset && offset < end {
                return self
                    .resource(row.resource)
                    .map(|resource| Some(resource.id()));
            }
        }
        Ok(None)
    }

    pub fn first_available_window_offset(&self, length: u64) -> Result<u64, OwnerTableRefusal> {
        if length == 0 || length & (self.config.window_alignment - 1) != 0 {
            return Err(OwnerTableRefusal::WindowMisaligned);
        }
        let bounds_end = self
            .config
            .window_base
            .checked_add(self.config.window_length)
            .ok_or(OwnerTableRefusal::WindowBoundsInvalid)?;
        let mut candidate = self.config.first_fit_base;
        for _ in 0..=self.windows.capacity() {
            let end = candidate
                .checked_add(length)
                .ok_or(OwnerTableRefusal::WindowOutOfBounds)?;
            if end > bounds_end {
                return Err(OwnerTableRefusal::WindowOutOfBounds);
            }
            let mut next = None;
            for index in 0..self.windows.capacity() {
                let Some(handle) = self.windows.occupied_handle_at(index) else {
                    continue;
                };
                let row = self
                    .windows
                    .get(handle)
                    .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                let row_end = row
                    .window
                    .offset()
                    .checked_add(row.window.length())
                    .ok_or(OwnerTableRefusal::InvariantLost)?;
                if candidate < row_end && row.window.offset() < end {
                    next = Some(next.map_or(row_end, |found: u64| found.max(row_end)));
                }
            }
            match next {
                None => return Ok(candidate),
                Some(found) => {
                    candidate = found
                        .checked_add(self.config.window_alignment - 1)
                        .map(|value| value & !(self.config.window_alignment - 1))
                        .ok_or(OwnerTableRefusal::WindowOutOfBounds)?;
                }
            }
        }
        Err(OwnerTableRefusal::InvariantLost)
    }

    pub fn first_overlapping_window_resource(
        &self,
        except_resource_id: u32,
        offset: u64,
        length: u64,
    ) -> Result<Option<u32>, OwnerTableRefusal> {
        let end = offset
            .checked_add(length)
            .ok_or(OwnerTableRefusal::WindowOutOfBounds)?;
        for index in 0..self.windows.capacity() {
            let Some(handle) = self.windows.occupied_handle_at(index) else {
                continue;
            };
            let row = self
                .windows
                .get(handle)
                .map_err(|_| OwnerTableRefusal::InvariantLost)?;
            let resource = self.resource(row.resource)?.id();
            let row_end = row
                .window
                .offset()
                .checked_add(row.window.length())
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            if resource != except_resource_id && offset < row_end && row.window.offset() < end {
                return Ok(Some(resource));
            }
        }
        Ok(None)
    }

    pub fn reserve_resource(&mut self, backing: B) -> Result<ResourceHandle, RefusedAdmission<B>> {
        if self.phase != OwnerPhase::Open {
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                custody: RefusedCustody::Input(ManuallyDrop::new(backing)),
            });
        }
        if !self.resources.has_insert_capacity() {
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::ResourceCapacityExhausted,
                custody: RefusedCustody::Input(ManuallyDrop::new(backing)),
            });
        }
        let Some(generation) = self.generation.take() else {
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::GenerationUnavailable,
                custody: RefusedCustody::Input(ManuallyDrop::new(backing)),
            });
        };
        let allocation = match generation.allocate_resource() {
            Ok(allocation) => allocation,
            Err(refused) => {
                self.generation = Some(refused.into_generation());
                return Err(RefusedAdmission {
                    reason: OwnerTableRefusal::ResourceIdExhausted,
                    custody: RefusedCustody::Input(ManuallyDrop::new(backing)),
                });
            }
        };
        let (generation, reservation) = allocation.into_parts();
        self.generation = Some(generation);
        let row = ResourceRow {
            lifecycle: Some(ResourceLifecycle::new(reservation, backing)),
            attachment_gate: AdmissionGate::Open,
            window_gate: AdmissionGate::Open,
            secondary_pairs: 0,
            windows: 0,
            creator_pair: None,
        };
        match self.resources.insert(row) {
            Ok(handle) => Ok(handle),
            Err(refused) => {
                let mut row = refused.into_payload();
                // SAFETY: `row` was constructed immediately above and the sole
                // intervening operation failed to publish it. No ticket, wire
                // request, attachment, window, or lookup handle escaped, so its
                // lifecycle is still the `Some(Reserved)` installed above.
                let lifecycle = unsafe { row.lifecycle.take().unwrap_unchecked() };
                let backing = unsafe { lifecycle.assume_unpublished_cancelled() };
                Err(RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::Input(ManuallyDrop::new(backing)),
                })
            }
        }
    }

    pub fn reserve_context(&mut self, owner: C) -> Result<ContextHandle, RefusedAdmission<C>> {
        if self.phase != OwnerPhase::Open {
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                custody: RefusedCustody::Input(ManuallyDrop::new(owner)),
            });
        }
        if !self.contexts.has_insert_capacity() {
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::ContextCapacityExhausted,
                custody: RefusedCustody::Input(ManuallyDrop::new(owner)),
            });
        }
        let Some(generation) = self.generation.take() else {
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::GenerationUnavailable,
                custody: RefusedCustody::Input(ManuallyDrop::new(owner)),
            });
        };
        let allocation = match generation.allocate_context() {
            Ok(allocation) => allocation,
            Err(refused) => {
                self.generation = Some(refused.into_generation());
                return Err(RefusedAdmission {
                    reason: OwnerTableRefusal::ContextIdExhausted,
                    custody: RefusedCustody::Input(ManuallyDrop::new(owner)),
                });
            }
        };
        let (generation, reservation) = allocation.into_parts();
        self.generation = Some(generation);
        let row = ContextRow {
            lifecycle: Some(TransportContextLifecycle::new(reservation, owner)),
            association_admission_closed: true,
        };
        match self.contexts.insert(row) {
            Ok(handle) => Ok(handle),
            Err(refused) => {
                let mut row = refused.into_payload();
                // SAFETY: `row` was constructed immediately above and the sole
                // intervening operation failed to publish it. No ticket, wire
                // request, lease, or lookup handle escaped, so its lifecycle is
                // still the `Some(Reserved)` installed above.
                let lifecycle = unsafe { row.lifecycle.take().unwrap_unchecked() };
                let owner = unsafe { lifecycle.assume_unpublished_cancelled() };
                Err(RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::Input(ManuallyDrop::new(owner)),
                })
            }
        }
    }

    pub fn canonical_pair_kind(
        &self,
        resource: ResourceHandle,
        context: ContextHandle,
    ) -> Result<Option<PairKind>, OwnerTableRefusal> {
        self.resource(resource)?;
        self.context(context)?;
        let found = self
            .pairs
            .find_unique_occupied_handle(|row| row.resource == resource && row.context == context)
            .map_err(|()| OwnerTableRefusal::InvariantLost)?;
        match found {
            Some(handle) => self
                .pairs
                .get(handle)
                .map(|row| Some(row.kind))
                .map_err(|_| OwnerTableRefusal::PairNotFound),
            None => Ok(None),
        }
    }

    pub fn borrow_pair_use(
        &mut self,
        resource: ResourceHandle,
        context: ContextHandle,
    ) -> Result<PairUseLease, OwnerTableRefusal> {
        self.require_open()?;
        self.resource(resource)?;
        self.context(context)?;
        let Some(pair) = self
            .pairs
            .find_unique_occupied_handle(|row| row.resource == resource && row.context == context)
            .map_err(|()| OwnerTableRefusal::InvariantLost)?
        else {
            return Err(OwnerTableRefusal::PairNotFound);
        };
        let pair_row = self
            .pairs
            .get(pair)
            .map_err(|_| OwnerTableRefusal::PairNotFound)?;
        let usable = match &pair_row.state {
            PairRowState::Secondary(lifecycle) => lifecycle.phase() == AttachmentPhase::Attached,
            PairRowState::Creator(_) => self.resources.get(resource).ok().is_some_and(|owner| {
                owner.creator_pair == Some(pair)
                    && owner.lifecycle.as_ref().is_some_and(|lifecycle| {
                        lifecycle.phase() == ResourcePhase::Live
                            && lifecycle.attachment() == Some(pair_row.attachment)
                    })
            }),
            PairRowState::Initializing { .. }
            | PairRowState::ReleasePending
            | PairRowState::Quarantined => false,
        };
        if !usable {
            return Err(OwnerTableRefusal::PairNotUsable);
        }
        unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                Self::mint_use(&mut row.use_high_water, &mut row.uses, pair)
            })
        }
        .map_err(|_| OwnerTableRefusal::PairNotFound)?
    }

    pub fn return_pair_use(
        &mut self,
        lease: PairUseLease,
    ) -> Result<(), RefusedAdmission<PairUseLease>> {
        let result = unsafe {
            self.pairs.with_occupied_mut(lease.pair, |row| {
                Self::return_use(
                    row.use_high_water,
                    &mut row.uses,
                    lease.id,
                    OwnerTableRefusal::PairUseMismatch,
                )
            })
        }
        .map_err(|_| OwnerTableRefusal::PairNotFound)
        .and_then(|result| result);
        match result {
            Ok(()) => Ok(()),
            Err(reason) => Err(RefusedAdmission {
                reason,
                custody: RefusedCustody::Input(ManuallyDrop::new(lease)),
            }),
        }
    }

    pub fn borrow_window_use(
        &mut self,
        window: WindowHandle,
    ) -> Result<WindowUseLease, OwnerTableRefusal> {
        self.require_open()?;
        let row = self
            .windows
            .get(window)
            .map_err(|_| OwnerTableRefusal::WindowNotFound)?;
        if !matches!(&row.state, WindowRowState::Live(lifecycle)
            if lifecycle.phase() == crate::control_ownership::WindowPhase::Mapped)
        {
            return Err(OwnerTableRefusal::WindowNotFound);
        }
        unsafe {
            self.windows.with_occupied_mut(window, |row| {
                Self::mint_use_id(&mut row.use_high_water, &mut row.uses)
                    .map(|id| WindowUseLease { window, id })
                    .map_err(|_| OwnerTableRefusal::WindowUseExhausted)
            })
        }
        .map_err(|_| OwnerTableRefusal::WindowNotFound)?
    }

    pub fn return_window_use(
        &mut self,
        lease: WindowUseLease,
    ) -> Result<(), RefusedAdmission<WindowUseLease>> {
        let result = unsafe {
            self.windows.with_occupied_mut(lease.window, |row| {
                Self::return_use(
                    row.use_high_water,
                    &mut row.uses,
                    lease.id,
                    OwnerTableRefusal::WindowUseMismatch,
                )
            })
        }
        .map_err(|_| OwnerTableRefusal::WindowNotFound)
        .and_then(|result| result);
        match result {
            Ok(()) => Ok(()),
            Err(reason) => Err(RefusedAdmission {
                reason,
                custody: RefusedCustody::Input(ManuallyDrop::new(lease)),
            }),
        }
    }

    pub fn close_resource_admission(
        &mut self,
        resource: ResourceHandle,
    ) -> Result<(), OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        unsafe {
            self.resources.with_occupied_mut(resource, |row| {
                row.attachment_gate = AdmissionGate::Closed;
                row.window_gate = AdmissionGate::Closed;
            })
        }
        .map_err(|_| OwnerTableRefusal::ResourceNotFound)?;
        if let Err(reason) = self.validate_resource_edges_closed(resource) {
            if matches!(
                reason,
                OwnerTableRefusal::InvariantLost | OwnerTableRefusal::OrphanCustodyOutstanding
            ) {
                self.enter_quarantined();
            }
            return Err(reason);
        }
        let identity = self.resource(resource)?;
        let terminal = self
            .resources
            .get(resource)
            .ok()
            .and_then(|row| row.lifecycle.as_ref())
            .is_some_and(|lifecycle| lifecycle.phase() == ResourcePhase::Terminal);
        if terminal {
            return Ok(());
        }
        let installed = unsafe {
            self.resources.with_occupied_mut(resource, |row| {
                let Some(lifecycle) = row.lifecycle.as_mut() else {
                    return false;
                };
                if lifecycle.attachments_are_closed() {
                    return true;
                }
                lifecycle
                    .install_attachments_closed(crate::control_ownership::AttachmentsClosed::new(
                        identity,
                    ))
                    .is_ok()
            })
        };
        if installed == Ok(true) {
            Ok(())
        } else {
            self.enter_quarantined();
            Err(OwnerTableRefusal::InvariantLost)
        }
    }

    pub fn begin_resource_create(
        &mut self,
        row: ResourceHandle,
    ) -> Result<PreparedOwnerControl<ResourceSlotKind>, OwnerTableRefusal> {
        self.require_open()?;
        let resource = self.resource(row)?;
        if self
            .resources
            .get(row)
            .ok()
            .and_then(|owner| owner.lifecycle.as_ref())
            .is_some_and(|lifecycle| lifecycle.control_high_water() == u64::MAX)
        {
            return Err(OwnerTableRefusal::ControlSequenceExhausted);
        }
        let rundown = self.mint_rundown()?;
        let reservation = match unsafe { self.resource_tickets.reserve(row, rundown) } {
            Ok(reservation) => reservation,
            Err(_) => return Err(OwnerTableRefusal::TicketRefused),
        };
        let post = unsafe { reservation.begin_lifecycle() };
        let begun = unsafe {
            self.resources.with_occupied_mut(row, |owner| {
                let Some(lifecycle) = owner.lifecycle.as_mut() else {
                    return None;
                };
                lifecycle
                    .begin_create(resource)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let request = match begun {
            Ok(Some(request)) => request,
            _ => {
                let reservation = unsafe { post.assume_lifecycle_not_begun() };
                if !Self::cancel_resource_ticket(&mut self.resource_tickets, reservation) {
                    self.enter_quarantined();
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                return Err(OwnerTableRefusal::LifecycleBeginRefused);
            }
        };
        match unsafe { self.resource_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl { row, ticket }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn begin_context_create(
        &mut self,
        row: ContextHandle,
    ) -> Result<PreparedOwnerControl<ContextSlotKind>, OwnerTableRefusal> {
        self.require_open()?;
        let context = self.context(row)?;
        if self
            .contexts
            .get(row)
            .ok()
            .and_then(|owner| owner.lifecycle.as_ref())
            .is_some_and(|lifecycle| lifecycle.control_high_water() == u64::MAX)
        {
            return Err(OwnerTableRefusal::ControlSequenceExhausted);
        }
        let rundown = self.mint_rundown()?;
        let reservation = match unsafe { self.context_tickets.reserve(row, rundown) } {
            Ok(reservation) => reservation,
            Err(_) => return Err(OwnerTableRefusal::TicketRefused),
        };
        let post = unsafe { reservation.begin_lifecycle() };
        let begun = unsafe {
            self.contexts.with_occupied_mut(row, |owner| {
                let Some(lifecycle) = owner.lifecycle.as_mut() else {
                    return None;
                };
                lifecycle
                    .begin_create(context)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let request = match begun {
            Ok(Some(request)) => request,
            _ => {
                let reservation = unsafe { post.assume_lifecycle_not_begun() };
                if !Self::cancel_context_ticket(&mut self.context_tickets, reservation) {
                    self.enter_quarantined();
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                return Err(OwnerTableRefusal::LifecycleBeginRefused);
            }
        };
        match unsafe { self.context_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl { row, ticket }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn begin_secondary_attach(
        &mut self,
        resource: ResourceHandle,
        context: ContextHandle,
        association: A,
    ) -> Result<PairAdmission<PairSlotKind>, RefusedAdmission<A, PairRow<A>>> {
        let reject = |reason, association| RefusedAdmission {
            reason,
            custody: RefusedCustody::Input(ManuallyDrop::new(association)),
        };
        if self.phase != OwnerPhase::Open {
            return Err(reject(
                OwnerTableRefusal::WrongPhase { found: self.phase },
                association,
            ));
        }
        if self.orphan_pair_custody.is_some() {
            return Err(reject(
                OwnerTableRefusal::OrphanCustodyOutstanding,
                association,
            ));
        }
        let resource_id = match self.resource(resource) {
            Ok(resource) => resource,
            Err(reason) => return Err(reject(reason, association)),
        };
        let context_id = match self.context(context) {
            Ok(context) => context,
            Err(reason) => return Err(reject(reason, association)),
        };
        let resource_ok = self.resources.get(resource).ok().is_some_and(|row| {
            row.attachment_gate == AdmissionGate::Open
                && row.secondary_pairs != u64::MAX
                && row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    matches!(
                        lifecycle.phase(),
                        ResourcePhase::Created | ResourcePhase::Live | ResourcePhase::Detached
                    )
                })
        });
        if !resource_ok {
            if self
                .resources
                .get(resource)
                .ok()
                .is_some_and(|row| row.secondary_pairs == u64::MAX)
            {
                return Err(reject(
                    OwnerTableRefusal::AssociationCensusExhausted,
                    association,
                ));
            }
            return Err(reject(
                OwnerTableRefusal::AttachmentAdmissionClosed,
                association,
            ));
        }
        let context_ok = self.contexts.get(context).ok().is_some_and(|row| {
            !row.association_admission_closed
                && row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    lifecycle.phase() == ContextPhase::Live
                        && lifecycle.lease_high_water() != u64::MAX
                        && lifecycle.lease_census() != u64::MAX
                })
        });
        if !context_ok {
            if self.contexts.get(context).ok().is_some_and(|row| {
                row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    lifecycle.lease_high_water() == u64::MAX || lifecycle.lease_census() == u64::MAX
                })
            }) {
                return Err(reject(
                    OwnerTableRefusal::AssociationCensusExhausted,
                    association,
                ));
            }
            return Err(reject(OwnerTableRefusal::ContextNotFound, association));
        }
        match self.canonical_pair_kind(resource, context) {
            Ok(Some(kind)) => {
                return Err(reject(
                    OwnerTableRefusal::CanonicalPairExists { kind },
                    association,
                ))
            }
            Ok(None) => {}
            Err(reason) => return Err(reject(reason, association)),
        }
        if !self.pairs.has_insert_capacity() {
            return Err(reject(
                OwnerTableRefusal::PairCapacityExhausted,
                association,
            ));
        }
        let rundown = match self.mint_rundown() {
            Ok(rundown) => rundown,
            Err(reason) => return Err(reject(reason, association)),
        };
        let Some(generation) = self.generation.take() else {
            return Err(reject(
                OwnerTableRefusal::GenerationUnavailable,
                association,
            ));
        };
        let allocation = match unsafe { generation.allocate_attachment(resource_id, context_id) } {
            Ok(allocation) => allocation,
            Err(refused) => {
                self.generation = Some(refused.into_generation());
                return Err(reject(
                    OwnerTableRefusal::AttachmentIdExhausted,
                    association,
                ));
            }
        };
        let (generation, reservation) = allocation.into_parts();
        self.generation = Some(generation);
        let leased = match unsafe {
            self.contexts
                .with_occupied_mut_input(context, reservation, |row, reservation| {
                    let Some(lifecycle) = row.lifecycle.as_mut() else {
                        return Err(reservation);
                    };
                    lifecycle
                        .lease_attachment(reservation)
                        .map_err(|refused| refused.into_reservation())
                })
        } {
            Ok(Ok(leased)) => leased,
            Ok(Err(_)) | Err((_, _)) => {
                return Err(reject(OwnerTableRefusal::ContextNotFound, association))
            }
        };
        let attachment = leased.attachment();
        let pair_row = PairRow {
            resource,
            context,
            kind: PairKind::Secondary,
            attachment,
            use_high_water: 0,
            uses: 0,
            state: PairRowState::Initializing {
                leased: ManuallyDrop::new(leased),
                association: ManuallyDrop::new(association),
            },
        };
        let pair = match self.pairs.insert(pair_row) {
            Ok(pair) => pair,
            Err(refused) => {
                return match self.cancel_local_pair(refused.into_payload()) {
                    Ok(association) => Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::Input(ManuallyDrop::new(association)),
                    }),
                    Err(row) => Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::Quarantined(ManuallyDrop::new(row)),
                    }),
                }
            }
        };
        let ticket = match unsafe { self.pair_tickets.reserve(pair, rundown) } {
            Ok(ticket) => ticket,
            Err(refused) => {
                let _nonce = refused.into_rundown().nonce;
                return Err(self.rollback_pair_refusal(pair, OwnerTableRefusal::TicketRefused));
            }
        };
        let post = unsafe { ticket.begin_lifecycle() };
        let begun = unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                let old = replace(&mut row.state, PairRowState::Quarantined);
                let PairRowState::Initializing {
                    leased,
                    association,
                } = old
                else {
                    row.state = old;
                    return None;
                };
                let leased = ManuallyDrop::into_inner(leased);
                let association = ManuallyDrop::into_inner(association);
                match ContextAttachmentLifecycle::reserve(leased, association) {
                    Ok(admission) => {
                        let (lifecycle, request) = admission.into_parts();
                        row.state = PairRowState::Secondary(lifecycle);
                        Some(request)
                    }
                    Err(refused) => {
                        let (leased, association) = refused.into_parts();
                        row.state = PairRowState::Initializing {
                            leased: ManuallyDrop::new(leased),
                            association: ManuallyDrop::new(association),
                        };
                        None
                    }
                }
            })
        };
        let request = match begun {
            Ok(Some(request)) => request,
            _ => {
                let ticket = unsafe { post.assume_lifecycle_not_begun() };
                if !Self::cancel_pair_ticket(&mut self.pair_tickets, ticket) {
                    self.enter_quarantined();
                    return Err(self.rollback_pair_refusal(pair, OwnerTableRefusal::InvariantLost));
                }
                return Err(
                    self.rollback_pair_refusal(pair, OwnerTableRefusal::LifecycleBeginRefused)
                );
            }
        };
        let incremented = unsafe {
            self.resources.with_occupied_mut(resource, |row| {
                let Some(next) = row.secondary_pairs.checked_add(1) else {
                    return false;
                };
                row.secondary_pairs = next;
                true
            })
        };
        if incremented != Ok(true) {
            self.enter_quarantined();
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::TableQuarantined,
            });
        }
        match unsafe { self.pair_tickets.install(post, request) } {
            Ok(ticket) => Ok(PairAdmission {
                pair,
                control: PreparedOwnerControl { row: pair, ticket },
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::TableQuarantined,
                })
            }
        }
    }

    pub fn begin_creator_attach(
        &mut self,
        resource: ResourceHandle,
        context: ContextHandle,
        association: A,
    ) -> Result<PairAdmission<ResourceSlotKind>, RefusedAdmission<A, PairRow<A>>> {
        let reject = |reason, association| RefusedAdmission {
            reason,
            custody: RefusedCustody::Input(ManuallyDrop::new(association)),
        };
        if self.phase != OwnerPhase::Open {
            return Err(reject(
                OwnerTableRefusal::WrongPhase { found: self.phase },
                association,
            ));
        }
        if self.orphan_pair_custody.is_some() {
            return Err(reject(
                OwnerTableRefusal::OrphanCustodyOutstanding,
                association,
            ));
        }
        let resource_id = match self.resource(resource) {
            Ok(resource) => resource,
            Err(reason) => return Err(reject(reason, association)),
        };
        let context_id = match self.context(context) {
            Ok(context) => context,
            Err(reason) => return Err(reject(reason, association)),
        };
        let resource_ok = self.resources.get(resource).ok().is_some_and(|row| {
            row.attachment_gate == AdmissionGate::Open
                && row.creator_pair.is_none()
                && row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    lifecycle.phase() == ResourcePhase::Created
                        && lifecycle.attachment().is_none()
                        && lifecycle.control_high_water() != u64::MAX
                })
        });
        if !resource_ok {
            if self
                .resources
                .get(resource)
                .ok()
                .and_then(|row| row.lifecycle.as_ref())
                .is_some_and(|lifecycle| lifecycle.control_high_water() == u64::MAX)
            {
                return Err(reject(
                    OwnerTableRefusal::ControlSequenceExhausted,
                    association,
                ));
            }
            return Err(reject(
                OwnerTableRefusal::AttachmentAdmissionClosed,
                association,
            ));
        }
        let context_ok = self.contexts.get(context).ok().is_some_and(|row| {
            !row.association_admission_closed
                && row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    lifecycle.phase() == ContextPhase::Live
                        && lifecycle.lease_high_water() != u64::MAX
                        && lifecycle.lease_census() != u64::MAX
                })
        });
        if !context_ok {
            if self.contexts.get(context).ok().is_some_and(|row| {
                row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    lifecycle.lease_high_water() == u64::MAX || lifecycle.lease_census() == u64::MAX
                })
            }) {
                return Err(reject(
                    OwnerTableRefusal::AssociationCensusExhausted,
                    association,
                ));
            }
            return Err(reject(OwnerTableRefusal::ContextNotFound, association));
        }
        match self.canonical_pair_kind(resource, context) {
            Ok(Some(kind)) => {
                return Err(reject(
                    OwnerTableRefusal::CanonicalPairExists { kind },
                    association,
                ))
            }
            Ok(None) => {}
            Err(reason) => return Err(reject(reason, association)),
        }
        if !self.pairs.has_insert_capacity() {
            return Err(reject(
                OwnerTableRefusal::PairCapacityExhausted,
                association,
            ));
        }
        let rundown = match self.mint_rundown() {
            Ok(rundown) => rundown,
            Err(reason) => return Err(reject(reason, association)),
        };
        let Some(generation) = self.generation.take() else {
            return Err(reject(
                OwnerTableRefusal::GenerationUnavailable,
                association,
            ));
        };
        let allocation = match unsafe { generation.allocate_attachment(resource_id, context_id) } {
            Ok(allocation) => allocation,
            Err(refused) => {
                self.generation = Some(refused.into_generation());
                return Err(reject(
                    OwnerTableRefusal::AttachmentIdExhausted,
                    association,
                ));
            }
        };
        let (generation, reservation) = allocation.into_parts();
        self.generation = Some(generation);
        let leased = match unsafe {
            self.contexts
                .with_occupied_mut_input(context, reservation, |row, reservation| {
                    let Some(lifecycle) = row.lifecycle.as_mut() else {
                        return Err(reservation);
                    };
                    lifecycle
                        .lease_attachment(reservation)
                        .map_err(|refused| refused.into_reservation())
                })
        } {
            Ok(Ok(leased)) => leased,
            Ok(Err(_)) | Err((_, _)) => {
                return Err(reject(OwnerTableRefusal::ContextNotFound, association))
            }
        };
        let attachment = leased.attachment();
        let pair_row = PairRow {
            resource,
            context,
            kind: PairKind::Creator,
            attachment,
            use_high_water: 0,
            uses: 0,
            state: PairRowState::Initializing {
                leased: ManuallyDrop::new(leased),
                association: ManuallyDrop::new(association),
            },
        };
        let pair = match self.pairs.insert(pair_row) {
            Ok(pair) => pair,
            Err(refused) => {
                return match self.cancel_local_pair(refused.into_payload()) {
                    Ok(association) => Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::Input(ManuallyDrop::new(association)),
                    }),
                    Err(row) => Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::Quarantined(ManuallyDrop::new(row)),
                    }),
                }
            }
        };
        let ticket = match unsafe { self.resource_tickets.reserve(resource, rundown) } {
            Ok(ticket) => ticket,
            Err(refused) => {
                let _nonce = refused.into_rundown().nonce;
                return Err(self.rollback_pair_refusal(pair, OwnerTableRefusal::TicketRefused));
            }
        };
        let post = unsafe { ticket.begin_lifecycle() };
        let taken = unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                replace(&mut row.state, PairRowState::Quarantined)
            })
        };
        let Ok(PairRowState::Initializing {
            leased,
            association,
        }) = taken
        else {
            self.enter_quarantined();
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::TableQuarantined,
            });
        };
        let begun = unsafe {
            self.resources.with_occupied_mut_input(
                resource,
                ManuallyDrop::into_inner(leased),
                |row, leased| {
                    let Some(lifecycle) = row.lifecycle.as_mut() else {
                        return Err(leased);
                    };
                    match lifecycle.begin_attach(leased) {
                        Ok(begin) => {
                            row.creator_pair = Some(pair);
                            Ok(begin.into_request())
                        }
                        Err(refused) => Err(refused.into_leased()),
                    }
                },
            )
        };
        let request = match begun {
            Ok(Ok(request)) => {
                let installed = unsafe {
                    self.pairs
                        .with_occupied_mut_input(pair, association, |row, association| {
                            row.state = PairRowState::Creator(association);
                        })
                };
                if let Err((_, association)) = installed {
                    self.orphan_pair_custody = Some(OrphanPairCustody::CreatorLifecycleBegun {
                        pair,
                        resource,
                        context,
                        attachment,
                        association,
                        post: ManuallyDrop::new(post),
                        request: ManuallyDrop::new(request),
                    });
                    self.enter_quarantined();
                    return Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::TableQuarantined,
                    });
                }
                request
            }
            Ok(Err(leased)) | Err((_, leased)) => {
                let restored = unsafe {
                    self.pairs.with_occupied_mut_input(
                        pair,
                        (leased, association),
                        |row, (leased, association)| {
                            row.state = PairRowState::Initializing {
                                leased: ManuallyDrop::new(leased),
                                association,
                            };
                        },
                    )
                };
                if let Err((_, (leased, association))) = restored {
                    let reservation = unsafe { post.assume_lifecycle_not_begun() };
                    self.orphan_pair_custody = Some(OrphanPairCustody::Initializing {
                        pair,
                        resource,
                        context,
                        attachment,
                        leased: ManuallyDrop::new(leased),
                        association,
                        reservation: ManuallyDrop::new(reservation),
                    });
                    self.enter_quarantined();
                    return Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::TableQuarantined,
                    });
                }
                let ticket = unsafe { post.assume_lifecycle_not_begun() };
                if !Self::cancel_resource_ticket(&mut self.resource_tickets, ticket) {
                    self.enter_quarantined();
                    return Err(RefusedAdmission {
                        reason: OwnerTableRefusal::InvariantLost,
                        custody: RefusedCustody::TableQuarantined,
                    });
                }
                return Err(
                    self.rollback_pair_refusal(pair, OwnerTableRefusal::LifecycleBeginRefused)
                );
            }
        };
        match unsafe { self.resource_tickets.install(post, request) } {
            Ok(ticket) => Ok(PairAdmission {
                pair,
                control: PreparedOwnerControl {
                    row: resource,
                    ticket,
                },
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::TableQuarantined,
                })
            }
        }
    }

    pub fn begin_window_map(
        &mut self,
        resource: ResourceHandle,
        window: TransportWindow,
        reservation: W,
    ) -> Result<OwnerWindowAdmission, RefusedAdmission<W, WindowRow<W>>> {
        let reject = |reason, reservation| RefusedAdmission {
            reason,
            custody: RefusedCustody::Input(ManuallyDrop::new(reservation)),
        };
        if self.phase != OwnerPhase::Open {
            return Err(reject(
                OwnerTableRefusal::WrongPhase { found: self.phase },
                reservation,
            ));
        }
        let resource_id = match self.resource(resource) {
            Ok(resource) => resource,
            Err(reason) => return Err(reject(reason, reservation)),
        };
        if window.resource() != resource_id {
            return Err(reject(OwnerTableRefusal::ResourceNotFound, reservation));
        }
        if window.offset() & (self.config.window_alignment - 1) != 0
            || window.length() & (self.config.window_alignment - 1) != 0
        {
            return Err(reject(OwnerTableRefusal::WindowMisaligned, reservation));
        }
        let Some(window_end) = window.offset().checked_add(window.length()) else {
            return Err(reject(OwnerTableRefusal::WindowOutOfBounds, reservation));
        };
        let bounds_end = self.config.window_base + self.config.window_length;
        if window.offset() < self.config.window_base || window_end > bounds_end {
            return Err(reject(OwnerTableRefusal::WindowOutOfBounds, reservation));
        }
        let resource_ok = self.resources.get(resource).ok().is_some_and(|row| {
            row.window_gate == AdmissionGate::Open
                && row.windows != u64::MAX
                && row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    matches!(
                        lifecycle.phase(),
                        ResourcePhase::Created | ResourcePhase::Live | ResourcePhase::Detached
                    )
                })
        });
        if !resource_ok {
            if self
                .resources
                .get(resource)
                .ok()
                .is_some_and(|row| row.windows == u64::MAX)
            {
                return Err(reject(
                    OwnerTableRefusal::WindowCensusExhausted,
                    reservation,
                ));
            }
            return Err(reject(
                OwnerTableRefusal::WindowAdmissionClosed,
                reservation,
            ));
        }
        if self
            .windows
            .find_occupied_handle(|row| row.resource == resource)
            .is_some()
        {
            return Err(reject(
                OwnerTableRefusal::CanonicalWindowExists,
                reservation,
            ));
        }
        if self
            .windows
            .find_occupied_handle(|row| Self::windows_overlap(row.window, window))
            .is_some()
        {
            return Err(reject(OwnerTableRefusal::WindowRangeOverlaps, reservation));
        }
        if !self.windows.has_insert_capacity() {
            return Err(reject(
                OwnerTableRefusal::WindowCapacityExhausted,
                reservation,
            ));
        }
        let rundown = match self.mint_rundown() {
            Ok(rundown) => rundown,
            Err(reason) => return Err(reject(reason, reservation)),
        };
        let owner_row = WindowRow {
            resource,
            window,
            map_info: None,
            use_high_water: 0,
            uses: 0,
            state: WindowRowState::Initializing(ManuallyDrop::new(reservation)),
        };
        let handle = match self.windows.insert(owner_row) {
            Ok(handle) => handle,
            Err(refused) => {
                let mut row = refused.into_payload();
                let old = replace(&mut row.state, WindowRowState::Quarantined);
                return match old {
                    WindowRowState::Initializing(reservation) => Err(reject(
                        OwnerTableRefusal::InvariantLost,
                        ManuallyDrop::into_inner(reservation),
                    )),
                    state => {
                        row.state = state;
                        Err(RefusedAdmission {
                            reason: OwnerTableRefusal::InvariantLost,
                            custody: RefusedCustody::Quarantined(ManuallyDrop::new(row)),
                        })
                    }
                };
            }
        };
        let ticket = match unsafe { self.window_tickets.reserve(handle, rundown) } {
            Ok(ticket) => ticket,
            Err(refused) => {
                let _nonce = refused.into_rundown().nonce;
                return Err(self.rollback_window_refusal(handle, OwnerTableRefusal::TicketRefused));
            }
        };
        let post = unsafe { ticket.begin_lifecycle() };
        let begun = unsafe {
            self.windows.with_occupied_mut(handle, |row| {
                let old = replace(&mut row.state, WindowRowState::Quarantined);
                let WindowRowState::Initializing(reservation) = old else {
                    row.state = old;
                    return None;
                };
                let admission =
                    WindowLifecycle::reserve(window, ManuallyDrop::into_inner(reservation));
                let (lifecycle, request) = admission.into_parts();
                row.state = WindowRowState::Live(lifecycle);
                Some(request)
            })
        };
        let Some(request) = begun.ok().flatten() else {
            let ticket = unsafe { post.assume_lifecycle_not_begun() };
            if !Self::cancel_window_ticket(&mut self.window_tickets, ticket) {
                self.enter_quarantined();
                return Err(RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::TableQuarantined,
                });
            }
            return Err(
                self.rollback_window_refusal(handle, OwnerTableRefusal::LifecycleBeginRefused)
            );
        };
        let incremented = unsafe {
            self.resources.with_occupied_mut(resource, |row| {
                let Some(next) = row.windows.checked_add(1) else {
                    return false;
                };
                row.windows = next;
                true
            })
        };
        if incremented != Ok(true) {
            self.enter_quarantined();
            return Err(RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::TableQuarantined,
            });
        }
        match unsafe { self.window_tickets.install(post, request) } {
            Ok(ticket) => Ok(OwnerWindowAdmission {
                window: handle,
                control: PreparedOwnerControl {
                    row: handle,
                    ticket,
                },
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::TableQuarantined,
                })
            }
        }
    }

    pub fn begin_secondary_detach(
        &mut self,
        pair: PairHandle,
    ) -> Result<PreparedOwnerControl<PairSlotKind>, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let attachment = {
            let row = self
                .pairs
                .get(pair)
                .map_err(|_| OwnerTableRefusal::PairNotFound)?;
            if row.kind != PairKind::Secondary
                || !matches!(&row.state, PairRowState::Secondary(_))
                || self.context(row.context).ok() != Some(row.attachment.context())
                || self.resource(row.resource).ok() != Some(row.attachment.resource())
                || row.uses != 0
            {
                return Err(if row.uses != 0 {
                    OwnerTableRefusal::PairUsesOutstanding { count: row.uses }
                } else {
                    OwnerTableRefusal::PairNotUsable
                });
            }
            if let PairRowState::Secondary(lifecycle) = &row.state {
                if lifecycle.control_high_water() == u64::MAX {
                    return Err(OwnerTableRefusal::ControlSequenceExhausted);
                }
            }
            row.attachment
        };
        let rundown = self.mint_rundown()?;
        let reservation = unsafe { self.pair_tickets.reserve(pair, rundown) }
            .map_err(|_| OwnerTableRefusal::TicketRefused)?;
        let post = unsafe { reservation.begin_lifecycle() };
        let request = unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                let PairRowState::Secondary(lifecycle) = &mut row.state else {
                    return None;
                };
                lifecycle
                    .begin_detach(attachment)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let Some(request) = request.ok().flatten() else {
            let reservation = unsafe { post.assume_lifecycle_not_begun() };
            if !Self::cancel_pair_ticket(&mut self.pair_tickets, reservation) {
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            }
            return Err(OwnerTableRefusal::LifecycleBeginRefused);
        };
        match unsafe { self.pair_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl { row: pair, ticket }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn begin_creator_detach(
        &mut self,
        resource: ResourceHandle,
    ) -> Result<PreparedOwnerControl<ResourceSlotKind>, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let (pair, attachment) = {
            let row = self
                .resources
                .get(resource)
                .map_err(|_| OwnerTableRefusal::ResourceNotFound)?;
            let pair = row.creator_pair.ok_or(OwnerTableRefusal::PairNotFound)?;
            let attachment = row
                .lifecycle
                .as_ref()
                .and_then(ResourceLifecycle::attachment)
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            (pair, attachment)
        };
        let pair_row = self
            .pairs
            .get(pair)
            .map_err(|_| OwnerTableRefusal::PairNotFound)?;
        if pair_row.kind != PairKind::Creator
            || pair_row.resource != resource
            || self.context(pair_row.context).ok() != Some(attachment.context())
            || pair_row.attachment != attachment
            || !matches!(&pair_row.state, PairRowState::Creator(_))
            || pair_row.uses != 0
        {
            return Err(if pair_row.uses != 0 {
                OwnerTableRefusal::PairUsesOutstanding {
                    count: pair_row.uses,
                }
            } else {
                OwnerTableRefusal::InvariantLost
            });
        }
        if self
            .resources
            .get(resource)
            .ok()
            .and_then(|row| row.lifecycle.as_ref())
            .is_some_and(|lifecycle| lifecycle.control_high_water() == u64::MAX)
        {
            return Err(OwnerTableRefusal::ControlSequenceExhausted);
        }
        let rundown = self.mint_rundown()?;
        let reservation = unsafe { self.resource_tickets.reserve(resource, rundown) }
            .map_err(|_| OwnerTableRefusal::TicketRefused)?;
        let post = unsafe { reservation.begin_lifecycle() };
        let request = unsafe {
            self.resources.with_occupied_mut(resource, |row| {
                let lifecycle = row.lifecycle.as_mut()?;
                lifecycle
                    .begin_detach(attachment)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let Some(request) = request.ok().flatten() else {
            let reservation = unsafe { post.assume_lifecycle_not_begun() };
            if !Self::cancel_resource_ticket(&mut self.resource_tickets, reservation) {
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            }
            return Err(OwnerTableRefusal::LifecycleBeginRefused);
        };
        match unsafe { self.resource_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl {
                row: resource,
                ticket,
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn begin_window_unmap(
        &mut self,
        window: WindowHandle,
    ) -> Result<PreparedOwnerControl<WindowSlotKind>, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let identity = {
            let row = self
                .windows
                .get(window)
                .map_err(|_| OwnerTableRefusal::WindowNotFound)?;
            if row.uses != 0 {
                return Err(OwnerTableRefusal::WindowUsesOutstanding { count: row.uses });
            }
            let WindowRowState::Live(lifecycle) = &row.state else {
                return Err(OwnerTableRefusal::WindowNotFound);
            };
            if lifecycle.control_high_water() == u64::MAX {
                return Err(OwnerTableRefusal::ControlSequenceExhausted);
            }
            lifecycle.window()
        };
        let rundown = self.mint_rundown()?;
        let reservation = unsafe { self.window_tickets.reserve(window, rundown) }
            .map_err(|_| OwnerTableRefusal::TicketRefused)?;
        let post = unsafe { reservation.begin_lifecycle() };
        let request = unsafe {
            self.windows.with_occupied_mut(window, |row| {
                let WindowRowState::Live(lifecycle) = &mut row.state else {
                    return None;
                };
                lifecycle
                    .begin_unmap(identity)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let Some(request) = request.ok().flatten() else {
            let reservation = unsafe { post.assume_lifecycle_not_begun() };
            if !Self::cancel_window_ticket(&mut self.window_tickets, reservation) {
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            }
            return Err(OwnerTableRefusal::LifecycleBeginRefused);
        };
        match unsafe { self.window_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl {
                row: window,
                ticket,
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn begin_resource_unref(
        &mut self,
        resource: ResourceHandle,
    ) -> Result<PreparedOwnerControl<ResourceSlotKind>, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        self.close_resource_admission(resource)?;
        let identity = self.resource(resource)?;
        let lifecycle = self
            .resources
            .get(resource)
            .ok()
            .and_then(|row| row.lifecycle.as_ref())
            .ok_or(OwnerTableRefusal::ResourceNotFound)?;
        if !lifecycle.attachments_are_closed() {
            self.enter_quarantined();
            return Err(OwnerTableRefusal::InvariantLost);
        }
        if lifecycle.control_high_water() == u64::MAX {
            return Err(OwnerTableRefusal::ControlSequenceExhausted);
        }
        let rundown = self.mint_rundown()?;
        let reservation = unsafe { self.resource_tickets.reserve(resource, rundown) }
            .map_err(|_| OwnerTableRefusal::TicketRefused)?;
        let post = unsafe { reservation.begin_lifecycle() };
        let request = unsafe {
            self.resources.with_occupied_mut(resource, |row| {
                row.lifecycle
                    .as_mut()?
                    .begin_unref(identity)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let Some(request) = request.ok().flatten() else {
            let reservation = unsafe { post.assume_lifecycle_not_begun() };
            if !Self::cancel_resource_ticket(&mut self.resource_tickets, reservation) {
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            }
            return Err(OwnerTableRefusal::LifecycleBeginRefused);
        };
        match unsafe { self.resource_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl {
                row: resource,
                ticket,
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn close_context_admission(
        &mut self,
        context: ContextHandle,
    ) -> Result<(), OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        unsafe {
            self.contexts.with_occupied_mut(context, |row| {
                row.association_admission_closed = true;
            })
        }
        .map_err(|_| OwnerTableRefusal::ContextNotFound)
    }

    pub fn begin_context_destroy(
        &mut self,
        context: ContextHandle,
    ) -> Result<PreparedOwnerControl<ContextSlotKind>, OwnerTableRefusal> {
        if !matches!(self.phase, OwnerPhase::Open | OwnerPhase::Closing) {
            return Err(OwnerTableRefusal::WrongPhase { found: self.phase });
        }
        let identity = self.context(context)?;
        let row = self
            .contexts
            .get(context)
            .map_err(|_| OwnerTableRefusal::ContextNotFound)?;
        let lifecycle = row
            .lifecycle
            .as_ref()
            .ok_or(OwnerTableRefusal::ContextNotFound)?;
        if !row.association_admission_closed || lifecycle.lease_census() != 0 {
            return Err(OwnerTableRefusal::CreatorAttachmentOutstanding);
        }
        let pair_exists = self
            .pairs
            .find_occupied_handle(|row| row.context == context)
            .is_some();
        if pair_exists {
            return Err(OwnerTableRefusal::InvariantLost);
        }
        if lifecycle.control_high_water() == u64::MAX {
            return Err(OwnerTableRefusal::ControlSequenceExhausted);
        }
        let runnable = matches!(
            lifecycle.phase(),
            ContextPhase::Live | ContextPhase::RetirementRequired
        );
        if !runnable {
            return Err(OwnerTableRefusal::LifecycleBeginRefused);
        }
        let rundown = self.mint_rundown()?;
        let reservation = unsafe { self.context_tickets.reserve(context, rundown) }
            .map_err(|_| OwnerTableRefusal::TicketRefused)?;
        let post = unsafe { reservation.begin_lifecycle() };
        let request = unsafe {
            self.contexts.with_occupied_mut(context, |row| {
                row.lifecycle
                    .as_mut()?
                    .begin_destroy(identity)
                    .ok()
                    .map(|begin| begin.into_request())
            })
        };
        let Some(request) = request.ok().flatten() else {
            self.enter_quarantined();
            return Err(OwnerTableRefusal::InvariantLost);
        };
        match unsafe { self.context_tickets.install(post, request) } {
            Ok(ticket) => Ok(PreparedOwnerControl {
                row: context,
                ticket,
            }),
            Err(_) => {
                self.enter_quarantined();
                Err(OwnerTableRefusal::InvariantLost)
            }
        }
    }

    pub fn dispatch_resource(
        &mut self,
        prepared: PreparedOwnerControl<ResourceSlotKind>,
    ) -> Result<DispatchWork<ResourceSlotKind>, RefusedWork<PreparedOwnerControl<ResourceSlotKind>>>
    {
        Self::begin_work(
            self.root.id(),
            self.phase,
            &mut self.run_high_water,
            &mut self.active_runs,
            &mut self.resource_tickets,
            prepared,
        )
    }

    pub fn dispatch_context(
        &mut self,
        prepared: PreparedOwnerControl<ContextSlotKind>,
    ) -> Result<DispatchWork<ContextSlotKind>, RefusedWork<PreparedOwnerControl<ContextSlotKind>>>
    {
        Self::begin_work(
            self.root.id(),
            self.phase,
            &mut self.run_high_water,
            &mut self.active_runs,
            &mut self.context_tickets,
            prepared,
        )
    }

    pub fn dispatch_pair(
        &mut self,
        prepared: PreparedOwnerControl<PairSlotKind>,
    ) -> Result<DispatchWork<PairSlotKind>, RefusedWork<PreparedOwnerControl<PairSlotKind>>> {
        Self::begin_work(
            self.root.id(),
            self.phase,
            &mut self.run_high_water,
            &mut self.active_runs,
            &mut self.pair_tickets,
            prepared,
        )
    }

    pub fn dispatch_window(
        &mut self,
        prepared: PreparedOwnerControl<WindowSlotKind>,
    ) -> Result<DispatchWork<WindowSlotKind>, RefusedWork<PreparedOwnerControl<WindowSlotKind>>>
    {
        Self::begin_work(
            self.root.id(),
            self.phase,
            &mut self.run_high_water,
            &mut self.active_runs,
            &mut self.window_tickets,
            prepared,
        )
    }

    pub fn finish_resource_work(
        &mut self,
        work: ObservedOwnerWork<E, ResourceSlotKind>,
    ) -> Result<
        OwnerLifecycleAction<ResourceSlotKind>,
        RefusedWork<ObservedOwnerWork<E, ResourceSlotKind>>,
    > {
        Self::finish_work(
            self.root.id(),
            self.epoch(),
            self.phase,
            &mut self.active_runs,
            &mut self.resource_tickets,
            work,
        )
    }

    pub fn finish_context_work(
        &mut self,
        work: ObservedOwnerWork<E, ContextSlotKind>,
    ) -> Result<
        OwnerLifecycleAction<ContextSlotKind>,
        RefusedWork<ObservedOwnerWork<E, ContextSlotKind>>,
    > {
        Self::finish_work(
            self.root.id(),
            self.epoch(),
            self.phase,
            &mut self.active_runs,
            &mut self.context_tickets,
            work,
        )
    }

    pub fn finish_pair_work(
        &mut self,
        work: ObservedOwnerWork<E, PairSlotKind>,
    ) -> Result<OwnerLifecycleAction<PairSlotKind>, RefusedWork<ObservedOwnerWork<E, PairSlotKind>>>
    {
        Self::finish_work(
            self.root.id(),
            self.epoch(),
            self.phase,
            &mut self.active_runs,
            &mut self.pair_tickets,
            work,
        )
    }

    pub fn finish_window_work(
        &mut self,
        work: ObservedOwnerWork<E, WindowSlotKind>,
    ) -> Result<
        OwnerLifecycleAction<WindowSlotKind>,
        RefusedWork<ObservedOwnerWork<E, WindowSlotKind>>,
    > {
        Self::finish_work(
            self.root.id(),
            self.epoch(),
            self.phase,
            &mut self.active_runs,
            &mut self.window_tickets,
            work,
        )
    }

    pub fn apply_resource_control(
        &mut self,
        owner_action: OwnerLifecycleAction<ResourceSlotKind>,
    ) -> Result<PendingResourceCompletion<E>, OwnerApplyRefusal<ResourceSlotKind>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open | OwnerPhase::Closing | OwnerPhase::Quarantined
        ) {
            return Err(OwnerApplyRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: owner_action,
            });
        }
        let OwnerLifecycleAction { row, action } = owner_action;
        let resources = &mut self.resources;
        let applied = unsafe {
            self.resource_tickets.apply_once(action, |completion| {
                let verb = completion.verb();
                completion.apply_unit(|classified| {
                    match resources.with_occupied_mut_input(row, classified, |owner, completion| {
                        let Some(lifecycle) = owner.lifecycle.as_mut() else {
                            return Err(completion);
                        };
                        match verb {
                            ControlVerb::Create => lifecycle
                                .finish_create(completion)
                                .map_err(|refused| refused.into_completion()),
                            ControlVerb::Attach => lifecycle
                                .finish_attach(completion)
                                .map_err(|refused| refused.into_completion()),
                            ControlVerb::Detach => lifecycle
                                .finish_detach(completion)
                                .map_err(|refused| refused.into_completion()),
                            ControlVerb::Unref => lifecycle
                                .finish_unref(completion)
                                .map_err(|refused| refused.into_completion()),
                            _ => Err(completion),
                        }
                    }) {
                        Ok(result) => result,
                        Err((_, completion)) => Err(completion),
                    }
                })
            })
        };
        match applied {
            Ok((finish, rundown)) => Ok(PendingResourceCompletion {
                row,
                finish: ManuallyDrop::new(finish),
                rundown: ManuallyDrop::new(rundown),
            }),
            Err(refusal) => match refusal.into_lifecycle_action() {
                Ok(action) => Err(OwnerApplyRefusal::Recoverable {
                    reason: OwnerTableRefusal::LifecycleBeginRefused,
                    action: OwnerLifecycleAction { row, action },
                }),
                Err(refusal) => match refusal.into_before_parts() {
                    Ok((action, _)) => Err(OwnerApplyRefusal::Recoverable {
                        reason: OwnerTableRefusal::TicketRefused,
                        action: OwnerLifecycleAction { row, action },
                    }),
                    Err(_) => {
                        self.enter_quarantined();
                        Err(OwnerApplyRefusal::Poisoned)
                    }
                },
            },
        }
    }

    pub fn apply_context_control(
        &mut self,
        owner_action: OwnerLifecycleAction<ContextSlotKind>,
    ) -> Result<PendingContextCompletion<E>, OwnerApplyRefusal<ContextSlotKind>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open | OwnerPhase::Closing | OwnerPhase::Quarantined
        ) {
            return Err(OwnerApplyRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: owner_action,
            });
        }
        let OwnerLifecycleAction { row, action } = owner_action;
        let contexts = &mut self.contexts;
        let applied = unsafe {
            self.context_tickets.apply_once(action, |completion| {
                let verb = completion.verb();
                completion.apply_context(|classified| {
                    match contexts.with_occupied_mut_input(row, classified, |owner, completion| {
                        let Some(lifecycle) = owner.lifecycle.as_mut() else {
                            return Err(completion);
                        };
                        match verb {
                            ControlVerb::ContextCreate => lifecycle
                                .finish_create(completion)
                                .map_err(|refused| refused.into_completion()),
                            ControlVerb::ContextDestroy => lifecycle
                                .finish_destroy(completion)
                                .map_err(|refused| refused.into_completion()),
                            _ => Err(completion),
                        }
                    }) {
                        Ok(result) => result,
                        Err((_, completion)) => Err(completion),
                    }
                })
            })
        };
        match applied {
            Ok((finish, rundown)) => Ok(PendingContextCompletion {
                row,
                finish: ManuallyDrop::new(finish),
                rundown: ManuallyDrop::new(rundown),
            }),
            Err(refusal) => match refusal.into_lifecycle_action() {
                Ok(action) => Err(OwnerApplyRefusal::Recoverable {
                    reason: OwnerTableRefusal::LifecycleBeginRefused,
                    action: OwnerLifecycleAction { row, action },
                }),
                Err(refusal) => match refusal.into_before_parts() {
                    Ok((action, _)) => Err(OwnerApplyRefusal::Recoverable {
                        reason: OwnerTableRefusal::TicketRefused,
                        action: OwnerLifecycleAction { row, action },
                    }),
                    Err(_) => {
                        self.enter_quarantined();
                        Err(OwnerApplyRefusal::Poisoned)
                    }
                },
            },
        }
    }

    pub fn apply_pair_control(
        &mut self,
        owner_action: OwnerLifecycleAction<PairSlotKind>,
    ) -> Result<PendingPairCompletion<E>, OwnerApplyRefusal<PairSlotKind>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open | OwnerPhase::Closing | OwnerPhase::Quarantined
        ) {
            return Err(OwnerApplyRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: owner_action,
            });
        }
        let OwnerLifecycleAction { row, action } = owner_action;
        let pairs = &mut self.pairs;
        let applied = unsafe {
            self.pair_tickets.apply_once(action, |completion| {
                let verb = completion.verb();
                completion.apply_unit(|classified| {
                    match pairs.with_occupied_mut_input(row, classified, |owner, completion| {
                        let PairRowState::Secondary(lifecycle) = &mut owner.state else {
                            return Err(completion);
                        };
                        match verb {
                            ControlVerb::Attach => lifecycle
                                .finish_attach(completion)
                                .map_err(|refused| refused.into_completion()),
                            ControlVerb::Detach => lifecycle
                                .finish_detach(completion)
                                .map_err(|refused| refused.into_completion()),
                            _ => Err(completion),
                        }
                    }) {
                        Ok(result) => result,
                        Err((_, completion)) => Err(completion),
                    }
                })
            })
        };
        match applied {
            Ok((finish, rundown)) => Ok(PendingPairCompletion {
                row,
                finish: ManuallyDrop::new(finish),
                rundown: ManuallyDrop::new(rundown),
            }),
            Err(refusal) => match refusal.into_lifecycle_action() {
                Ok(action) => Err(OwnerApplyRefusal::Recoverable {
                    reason: OwnerTableRefusal::LifecycleBeginRefused,
                    action: OwnerLifecycleAction { row, action },
                }),
                Err(refusal) => match refusal.into_before_parts() {
                    Ok((action, _)) => Err(OwnerApplyRefusal::Recoverable {
                        reason: OwnerTableRefusal::TicketRefused,
                        action: OwnerLifecycleAction { row, action },
                    }),
                    Err(_) => {
                        self.enter_quarantined();
                        Err(OwnerApplyRefusal::Poisoned)
                    }
                },
            },
        }
    }

    pub fn apply_window_control(
        &mut self,
        owner_action: OwnerLifecycleAction<WindowSlotKind>,
    ) -> Result<PendingWindowCompletion<E>, OwnerApplyRefusal<WindowSlotKind>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open | OwnerPhase::Closing | OwnerPhase::Quarantined
        ) {
            return Err(OwnerApplyRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: owner_action,
            });
        }
        struct AppliedWindow<E> {
            finish: WindowFinish<E>,
            map_info: Option<u32>,
        }
        let OwnerLifecycleAction { row, action } = owner_action;
        let windows = &mut self.windows;
        let applied = unsafe {
            self.window_tickets.apply_once(action, |completion| {
                let verb = completion.verb();
                match verb {
                    ControlVerb::Map => {
                        let map_info = completion.map_info();
                        completion
                            .apply_map(|classified| {
                                match windows.with_occupied_mut_input(
                                    row,
                                    classified,
                                    |owner, completion| {
                                        let WindowRowState::Live(lifecycle) = &mut owner.state
                                        else {
                                            return Err(completion);
                                        };
                                        match lifecycle.finish_map(completion) {
                                            Ok(finish) => {
                                                if matches!(
                                                    finish.effect(),
                                                    WindowFinishEffect::MapCompleted
                                                ) {
                                                    owner.map_info = map_info;
                                                }
                                                Ok(finish)
                                            }
                                            Err(refused) => Err(refused.into_completion()),
                                        }
                                    },
                                ) {
                                    Ok(result) => result,
                                    Err((_, completion)) => Err(completion),
                                }
                            })
                            .map(|(finish, map_info)| AppliedWindow { finish, map_info })
                    }
                    ControlVerb::Unmap => completion
                        .apply_unit(|classified| {
                            match windows.with_occupied_mut_input(
                                row,
                                classified,
                                |owner, completion| {
                                    let WindowRowState::Live(lifecycle) = &mut owner.state else {
                                        return Err(completion);
                                    };
                                    match lifecycle.finish_unmap(completion) {
                                        Ok(finish) => {
                                            if matches!(
                                                finish.effect(),
                                                WindowFinishEffect::UnmapCompleted
                                            ) {
                                                owner.map_info = None;
                                            }
                                            Ok(finish)
                                        }
                                        Err(refused) => Err(refused.into_completion()),
                                    }
                                },
                            ) {
                                Ok(result) => result,
                                Err((_, completion)) => Err(completion),
                            }
                        })
                        .map(|finish| AppliedWindow {
                            finish,
                            map_info: None,
                        }),
                    _ => Err(completion),
                }
            })
        };
        match applied {
            Ok((applied, rundown)) => {
                let _map_info = applied.map_info;
                Ok(PendingWindowCompletion {
                    row,
                    finish: ManuallyDrop::new(applied.finish),
                    rundown: ManuallyDrop::new(rundown),
                })
            }
            Err(refusal) => match refusal.into_lifecycle_action() {
                Ok(action) => Err(OwnerApplyRefusal::Recoverable {
                    reason: OwnerTableRefusal::LifecycleBeginRefused,
                    action: OwnerLifecycleAction { row, action },
                }),
                Err(refusal) => match refusal.into_before_parts() {
                    Ok((action, _)) => Err(OwnerApplyRefusal::Recoverable {
                        reason: OwnerTableRefusal::TicketRefused,
                        action: OwnerLifecycleAction { row, action },
                    }),
                    Err(_) => {
                        self.enter_quarantined();
                        Err(OwnerApplyRefusal::Poisoned)
                    }
                },
            },
        }
    }

    pub fn ack_resource_completion(
        &mut self,
        pending: PendingResourceCompletion<E>,
    ) -> Result<ResourceFinishEffect<E>, RefusedCompletion<PendingResourceCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.destroy_authority().is_some()
            || pending.finish.attachment_release().is_some()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingResourceCompletion {
            row,
            finish,
            rundown,
        } = pending;
        let release = ManuallyDrop::into_inner(rundown);
        if self
            .resource_tickets
            .validate_rundown_release(&release)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(PendingResourceCompletion {
                    row,
                    finish,
                    rundown: ManuallyDrop::new(release),
                }),
            });
        }
        let owner_rundown = match self.resource_tickets.consume_rundown_release(release) {
            Ok(rundown) => rundown,
            Err(refused) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::TicketRefused,
                    action: ManuallyDrop::new(PendingResourceCompletion {
                        row,
                        finish,
                        rundown: ManuallyDrop::new(refused.into_action()),
                    }),
                })
            }
        };
        let _nonce = owner_rundown.nonce;
        let (effect, _, _) = ManuallyDrop::into_inner(finish).into_parts();
        Ok(effect)
    }

    pub fn ack_context_completion(
        &mut self,
        pending: PendingContextCompletion<E>,
    ) -> Result<ContextFinishEffect<E>, RefusedCompletion<PendingContextCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.release_authority().is_some() {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        let row = pending.row;
        let PendingContextCompletion {
            row: _,
            finish,
            rundown,
        } = pending;
        let release = ManuallyDrop::into_inner(rundown);
        if self
            .context_tickets
            .validate_rundown_release(&release)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(PendingContextCompletion {
                    row,
                    finish,
                    rundown: ManuallyDrop::new(release),
                }),
            });
        }
        let previous_gate = self
            .contexts
            .get(row)
            .map(|owner| owner.association_admission_closed)
            .unwrap_or(true);
        let live = self
            .contexts
            .get(row)
            .ok()
            .and_then(|owner| owner.lifecycle.as_ref())
            .is_some_and(|lifecycle| lifecycle.phase() == ContextPhase::Live);
        if live {
            let _ = unsafe {
                self.contexts.with_occupied_mut(row, |owner| {
                    owner.association_admission_closed = false;
                })
            };
        }
        let owner_rundown = match self.context_tickets.consume_rundown_release(release) {
            Ok(rundown) => rundown,
            Err(refused) => {
                let _ = unsafe {
                    self.contexts.with_occupied_mut(row, |owner| {
                        owner.association_admission_closed = previous_gate;
                    })
                };
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::TicketRefused,
                    action: ManuallyDrop::new(PendingContextCompletion {
                        row,
                        finish,
                        rundown: ManuallyDrop::new(refused.into_action()),
                    }),
                });
            }
        };
        let _nonce = owner_rundown.nonce;
        let (effect, _) = ManuallyDrop::into_inner(finish).into_parts();
        Ok(effect)
    }

    pub fn ack_pair_completion(
        &mut self,
        pending: PendingPairCompletion<E>,
    ) -> Result<AttachmentFinishEffect<E>, RefusedCompletion<PendingPairCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.release_authority().is_some() {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingPairCompletion {
            row,
            finish,
            rundown,
        } = pending;
        let release = ManuallyDrop::into_inner(rundown);
        if self
            .pair_tickets
            .validate_rundown_release(&release)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(PendingPairCompletion {
                    row,
                    finish,
                    rundown: ManuallyDrop::new(release),
                }),
            });
        }
        let owner_rundown = match self.pair_tickets.consume_rundown_release(release) {
            Ok(rundown) => rundown,
            Err(refused) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::TicketRefused,
                    action: ManuallyDrop::new(PendingPairCompletion {
                        row,
                        finish,
                        rundown: ManuallyDrop::new(refused.into_action()),
                    }),
                })
            }
        };
        let _nonce = owner_rundown.nonce;
        let (effect, _) = ManuallyDrop::into_inner(finish).into_parts();
        Ok(effect)
    }

    pub fn ack_window_completion(
        &mut self,
        pending: PendingWindowCompletion<E>,
    ) -> Result<WindowFinishEffect<E>, RefusedCompletion<PendingWindowCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.release_authority().is_some() {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingWindowCompletion {
            row,
            finish,
            rundown,
        } = pending;
        let release = ManuallyDrop::into_inner(rundown);
        if self
            .window_tickets
            .validate_rundown_release(&release)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(PendingWindowCompletion {
                    row,
                    finish,
                    rundown: ManuallyDrop::new(release),
                }),
            });
        }
        let owner_rundown = match self.window_tickets.consume_rundown_release(release) {
            Ok(rundown) => rundown,
            Err(refused) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::TicketRefused,
                    action: ManuallyDrop::new(PendingWindowCompletion {
                        row,
                        finish,
                        rundown: ManuallyDrop::new(refused.into_action()),
                    }),
                })
            }
        };
        let _nonce = owner_rundown.nonce;
        let (effect, _) = ManuallyDrop::into_inner(finish).into_parts();
        Ok(effect)
    }

    pub fn begin_pair_payload_release(
        &mut self,
        pending: PendingPairCompletion<E>,
    ) -> Result<PairPayloadAction<A, E>, RefusedCompletion<PendingPairCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        let Some(authority_ref) = pending.finish.release_authority() else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        };
        let pair = pending.row;
        let pair_row = match self.pairs.get(pair) {
            Ok(row) => row,
            Err(_) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::PairNotFound,
                    action: ManuallyDrop::new(pending),
                })
            }
        };
        if pair_row.kind != PairKind::Secondary
            || pair_row.uses != 0
            || authority_ref.attachment() != pair_row.attachment
            || !matches!(pair_row.state, PairRowState::Secondary(_))
        {
            return Err(RefusedCompletion {
                reason: if pair_row.uses != 0 {
                    OwnerTableRefusal::PairUsesOutstanding {
                        count: pair_row.uses,
                    }
                } else {
                    OwnerTableRefusal::InvariantLost
                },
                action: ManuallyDrop::new(pending),
            });
        }
        let resource = pair_row.resource;
        let context = pair_row.context;
        if !self
            .resources
            .get(resource)
            .ok()
            .is_some_and(|row| row.secondary_pairs != 0)
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(pending),
            });
        }
        if self
            .pair_tickets
            .validate_rundown_release(&pending.rundown)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingPairCompletion {
            row: _,
            finish,
            rundown,
        } = pending;
        let (effect, release) = ManuallyDrop::into_inner(finish).into_parts();
        let Some(authority) = release else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(PendingPairCompletion {
                    row: pair,
                    finish: ManuallyDrop::new(AttachmentFinish::from_parts(effect, None)),
                    rundown,
                }),
            });
        };
        let consumed = unsafe {
            self.pairs
                .with_occupied_mut_input(pair, authority, |row, authority| {
                    let old = replace(&mut row.state, PairRowState::ReleasePending);
                    let PairRowState::Secondary(lifecycle) = old else {
                        row.state = old;
                        return Err(authority);
                    };
                    match lifecycle.consume_released(authority) {
                        Ok(released) => Ok(released),
                        Err(refused) => {
                            let (lifecycle, authority) = refused.into_parts();
                            row.state = PairRowState::Secondary(lifecycle);
                            Err(authority)
                        }
                    }
                })
        };
        let released = match consumed {
            Ok(Ok(released)) => released,
            Ok(Err(authority)) | Err((_, authority)) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::InvariantLost,
                    action: ManuallyDrop::new(PendingPairCompletion {
                        row: pair,
                        finish: ManuallyDrop::new(AttachmentFinish::from_parts(
                            effect,
                            Some(authority),
                        )),
                        rundown,
                    }),
                })
            }
        };
        let (released, association) = released.into_parts();
        Ok(PairPayloadAction {
            association: ManuallyDrop::new(association),
            effect: ManuallyDrop::new(effect),
            pending: PendingPairPayloadAck {
                pair,
                resource,
                context,
                released: Some(ManuallyDrop::new(released)),
                rundown: Some(rundown),
            },
        })
    }

    pub fn ack_pair_payload_release(
        &mut self,
        action: FinalizedPairPayloadAck,
    ) -> Result<(), OwnerFinalizationRefusal<FinalizedPairPayloadAck>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action,
            });
        }
        let mut pending = action.pending;
        let valid_pair = self.pairs.get(pending.pair).ok().is_some_and(|row| {
            row.resource == pending.resource
                && row.context == pending.context
                && row.kind == PairKind::Secondary
                && row.uses == 0
                && matches!(row.state, PairRowState::ReleasePending)
        });
        let valid_resource = self
            .resources
            .get(pending.resource)
            .ok()
            .is_some_and(|row| row.secondary_pairs != 0);
        if !valid_pair || !valid_resource {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedPairPayloadAck { pending },
            });
        }
        let Some(rundown_ref) = pending.rundown.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedPairPayloadAck { pending },
            });
        };
        if self
            .pair_tickets
            .validate_rundown_release(rundown_ref)
            .is_err()
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::TicketRefused,
                action: FinalizedPairPayloadAck { pending },
            });
        }
        let Some(released_ref) = pending.released.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedPairPayloadAck { pending },
            });
        };
        if !self.contexts.get(pending.context).ok().is_some_and(|row| {
            row.lifecycle
                .as_ref()
                .is_some_and(|lifecycle| lifecycle.can_return_attachment(released_ref))
        }) || !self.pairs.can_mark_terminal(pending.pair)
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedPairPayloadAck { pending },
            });
        }
        let Some(released) = pending.released.take() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedPairPayloadAck { pending },
            });
        };
        let released = ManuallyDrop::into_inner(released);
        let returned = unsafe {
            self.contexts
                .with_occupied_mut_input(pending.context, released, |row, released| {
                    let Some(lifecycle) = row.lifecycle.as_mut() else {
                        return Err(released);
                    };
                    lifecycle
                        .return_attachment(released)
                        .map_err(|refused| refused.into_released())
                })
        };
        if let Ok(Ok(_)) = returned {
        } else {
            let released = match returned {
                Ok(Err(released)) | Err((_, released)) => released,
                Ok(Ok(_)) => return Err(OwnerFinalizationRefusal::Poisoned),
            };
            pending.released = Some(ManuallyDrop::new(released));
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ContextNotFound,
                action: FinalizedPairPayloadAck { pending },
            });
        }
        let decremented = unsafe {
            self.resources.with_occupied_mut(pending.resource, |row| {
                if row.secondary_pairs == 0 {
                    return false;
                }
                row.secondary_pairs -= 1;
                true
            })
        };
        if decremented != Ok(true) {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        if unsafe { self.pairs.finalize_occupied(pending.pair) }.is_err() {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        let Some(rundown) = pending.rundown.take() else {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        };
        let rundown = ManuallyDrop::into_inner(rundown);
        let owner_rundown = unsafe { self.pair_tickets.consume_rundown_release_validated(rundown) };
        let _nonce = owner_rundown.nonce;
        Ok(())
    }

    pub fn begin_creator_payload_release(
        &mut self,
        pending: PendingResourceCompletion<E>,
    ) -> Result<CreatorPayloadAction<A, E>, RefusedCompletion<PendingResourceCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.destroy_authority().is_some() {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        let Some(released_ref) = pending.finish.attachment_release() else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        };
        let resource = pending.row;
        let pair = match self.resources.get(resource) {
            Ok(row) => match row.creator_pair {
                Some(pair) => pair,
                None => {
                    return Err(RefusedCompletion {
                        reason: OwnerTableRefusal::InvariantLost,
                        action: ManuallyDrop::new(pending),
                    })
                }
            },
            Err(_) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::ResourceNotFound,
                    action: ManuallyDrop::new(pending),
                })
            }
        };
        let pair_row = match self.pairs.get(pair) {
            Ok(row) => row,
            Err(_) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::PairNotFound,
                    action: ManuallyDrop::new(pending),
                })
            }
        };
        if pair_row.kind != PairKind::Creator
            || pair_row.resource != resource
            || pair_row.uses != 0
            || released_ref.reservation().attachment() != pair_row.attachment
            || !matches!(pair_row.state, PairRowState::Creator(_))
        {
            return Err(RefusedCompletion {
                reason: if pair_row.uses != 0 {
                    OwnerTableRefusal::PairUsesOutstanding {
                        count: pair_row.uses,
                    }
                } else {
                    OwnerTableRefusal::InvariantLost
                },
                action: ManuallyDrop::new(pending),
            });
        }
        let context = pair_row.context;
        if self
            .resource_tickets
            .validate_rundown_release(&pending.rundown)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingResourceCompletion {
            row: _,
            finish,
            rundown,
        } = pending;
        let (effect, destroy, released) = ManuallyDrop::into_inner(finish).into_parts();
        let Some(released) = released else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(PendingResourceCompletion {
                    row: resource,
                    finish: ManuallyDrop::new(ResourceFinish::from_parts(effect, destroy, None)),
                    rundown,
                }),
            });
        };
        let association = unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                let old = replace(&mut row.state, PairRowState::ReleasePending);
                match old {
                    PairRowState::Creator(association) => Some(association),
                    state => {
                        row.state = state;
                        None
                    }
                }
            })
        };
        let Some(association) = association.ok().flatten() else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(PendingResourceCompletion {
                    row: resource,
                    finish: ManuallyDrop::new(ResourceFinish::from_parts(
                        effect,
                        destroy,
                        Some(released),
                    )),
                    rundown,
                }),
            });
        };
        Ok(CreatorPayloadAction {
            association,
            effect: ManuallyDrop::new(effect),
            pending: PendingCreatorPayloadAck {
                pair,
                resource,
                context,
                released: Some(ManuallyDrop::new(released)),
                rundown: Some(rundown),
            },
        })
    }

    pub fn ack_creator_payload_release(
        &mut self,
        action: FinalizedCreatorPayloadAck,
    ) -> Result<(), OwnerFinalizationRefusal<FinalizedCreatorPayloadAck>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action,
            });
        }
        let mut pending = action.pending;
        let valid_pair = self.pairs.get(pending.pair).ok().is_some_and(|row| {
            row.resource == pending.resource
                && row.context == pending.context
                && row.kind == PairKind::Creator
                && row.uses == 0
                && matches!(row.state, PairRowState::ReleasePending)
        });
        let valid_resource = self
            .resources
            .get(pending.resource)
            .ok()
            .is_some_and(|row| row.creator_pair == Some(pending.pair));
        if !valid_pair || !valid_resource {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedCreatorPayloadAck { pending },
            });
        }
        let Some(released_ref) = pending.released.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedCreatorPayloadAck { pending },
            });
        };
        if !self.contexts.get(pending.context).ok().is_some_and(|row| {
            row.lifecycle
                .as_ref()
                .is_some_and(|lifecycle| lifecycle.can_return_attachment(released_ref))
        }) || !self.pairs.can_mark_terminal(pending.pair)
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedCreatorPayloadAck { pending },
            });
        }
        let Some(rundown_ref) = pending.rundown.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedCreatorPayloadAck { pending },
            });
        };
        if self
            .resource_tickets
            .validate_rundown_release(rundown_ref)
            .is_err()
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::TicketRefused,
                action: FinalizedCreatorPayloadAck { pending },
            });
        }
        let Some(released) = pending.released.take() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedCreatorPayloadAck { pending },
            });
        };
        let released = ManuallyDrop::into_inner(released);
        let returned = unsafe {
            self.contexts
                .with_occupied_mut_input(pending.context, released, |row, released| {
                    let Some(lifecycle) = row.lifecycle.as_mut() else {
                        return Err(released);
                    };
                    lifecycle
                        .return_attachment(released)
                        .map_err(|refused| refused.into_released())
                })
        };
        if let Ok(Ok(_)) = returned {
        } else {
            let released = match returned {
                Ok(Err(released)) | Err((_, released)) => released,
                Ok(Ok(_)) => return Err(OwnerFinalizationRefusal::Poisoned),
            };
            pending.released = Some(ManuallyDrop::new(released));
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::ContextNotFound,
                action: FinalizedCreatorPayloadAck { pending },
            });
        }
        let cleared = unsafe {
            self.resources.with_occupied_mut(pending.resource, |row| {
                if row.creator_pair != Some(pending.pair) {
                    return false;
                }
                row.creator_pair = None;
                true
            })
        };
        if cleared != Ok(true) {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        if unsafe { self.pairs.finalize_occupied(pending.pair) }.is_err() {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        let Some(rundown) = pending.rundown.take() else {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        };
        let rundown = ManuallyDrop::into_inner(rundown);
        let owner_rundown = unsafe {
            self.resource_tickets
                .consume_rundown_release_validated(rundown)
        };
        let _nonce = owner_rundown.nonce;
        Ok(())
    }

    pub fn begin_window_payload_release(
        &mut self,
        pending: PendingWindowCompletion<E>,
    ) -> Result<WindowPayloadAction<W, E>, RefusedCompletion<PendingWindowCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        let Some(authority_ref) = pending.finish.release_authority() else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        };
        let window = pending.row;
        let window_row = match self.windows.get(window) {
            Ok(row) => row,
            Err(_) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::WindowNotFound,
                    action: ManuallyDrop::new(pending),
                })
            }
        };
        if authority_ref.window() != window_row.window
            || !matches!(window_row.state, WindowRowState::Live(_))
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(pending),
            });
        }
        let resource = window_row.resource;
        if !self
            .resources
            .get(resource)
            .ok()
            .is_some_and(|row| row.windows != 0)
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(pending),
            });
        }
        if self
            .window_tickets
            .validate_rundown_release(&pending.rundown)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingWindowCompletion {
            row: _,
            finish,
            rundown,
        } = pending;
        let (effect, release) = ManuallyDrop::into_inner(finish).into_parts();
        let Some(authority) = release else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(PendingWindowCompletion {
                    row: window,
                    finish: ManuallyDrop::new(WindowFinish::from_parts(effect, None)),
                    rundown,
                }),
            });
        };
        let consumed = unsafe {
            self.windows
                .with_occupied_mut_input(window, authority, |row, authority| {
                    let old = replace(&mut row.state, WindowRowState::ReleasePending);
                    let WindowRowState::Live(lifecycle) = old else {
                        row.state = old;
                        return Err(authority);
                    };
                    match lifecycle.consume_unmapped(authority) {
                        Ok(reservation) => Ok(reservation),
                        Err(refused) => {
                            let (lifecycle, authority) = refused.into_parts();
                            row.state = WindowRowState::Live(lifecycle);
                            Err(authority)
                        }
                    }
                })
        };
        let reservation = match consumed {
            Ok(Ok(reservation)) => reservation,
            Ok(Err(authority)) | Err((_, authority)) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::InvariantLost,
                    action: ManuallyDrop::new(PendingWindowCompletion {
                        row: window,
                        finish: ManuallyDrop::new(WindowFinish::from_parts(
                            effect,
                            Some(authority),
                        )),
                        rundown,
                    }),
                })
            }
        };
        Ok(WindowPayloadAction {
            reservation: ManuallyDrop::new(reservation),
            effect: ManuallyDrop::new(effect),
            pending: PendingWindowPayloadAck {
                window,
                resource,
                rundown: Some(rundown),
            },
        })
    }

    pub fn ack_window_payload_release(
        &mut self,
        action: FinalizedWindowPayloadAck,
    ) -> Result<(), OwnerFinalizationRefusal<FinalizedWindowPayloadAck>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action,
            });
        }
        let mut pending = action.pending;
        let valid_window = self.windows.get(pending.window).ok().is_some_and(|row| {
            row.resource == pending.resource && matches!(row.state, WindowRowState::ReleasePending)
        });
        let valid_resource = self
            .resources
            .get(pending.resource)
            .ok()
            .is_some_and(|row| row.windows != 0);
        if !valid_window || !valid_resource {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedWindowPayloadAck { pending },
            });
        }
        if !self.windows.can_mark_terminal(pending.window) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedWindowPayloadAck { pending },
            });
        }
        let Some(rundown_ref) = pending.rundown.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedWindowPayloadAck { pending },
            });
        };
        if self
            .window_tickets
            .validate_rundown_release(rundown_ref)
            .is_err()
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::TicketRefused,
                action: FinalizedWindowPayloadAck { pending },
            });
        }
        let decremented = unsafe {
            self.resources.with_occupied_mut(pending.resource, |row| {
                if row.windows == 0 {
                    return false;
                }
                row.windows -= 1;
                true
            })
        };
        if decremented != Ok(true) {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        if unsafe { self.windows.finalize_occupied(pending.window) }.is_err() {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        let Some(rundown) = pending.rundown.take() else {
            self.enter_quarantined();
            return Err(OwnerFinalizationRefusal::Poisoned);
        };
        let rundown = ManuallyDrop::into_inner(rundown);
        let owner_rundown = unsafe {
            self.window_tickets
                .consume_rundown_release_validated(rundown)
        };
        let _nonce = owner_rundown.nonce;
        Ok(())
    }

    pub fn begin_resource_payload_release(
        &mut self,
        pending: PendingResourceCompletion<E>,
    ) -> Result<ResourcePayloadAction<B, E>, RefusedCompletion<PendingResourceCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.destroy_authority().is_none() {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        if pending.finish.attachment_release().is_some() {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        }
        let resource = pending.row;
        let valid = self.resources.get(resource).ok().is_some_and(|row| {
            row.creator_pair.is_none()
                && row.secondary_pairs == 0
                && row.windows == 0
                && row.attachment_gate == AdmissionGate::Closed
                && row.window_gate == AdmissionGate::Closed
        });
        if !valid {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(pending),
            });
        }
        if self
            .resource_tickets
            .validate_rundown_release(&pending.rundown)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingResourceCompletion {
            row: _,
            finish,
            rundown,
        } = pending;
        let (effect, authority, _) = ManuallyDrop::into_inner(finish).into_parts();
        let Some(authority) = authority else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(PendingResourceCompletion {
                    row: resource,
                    finish: ManuallyDrop::new(ResourceFinish::from_parts(effect, None, None)),
                    rundown,
                }),
            });
        };
        let consumed = unsafe {
            self.resources
                .with_occupied_mut_input(resource, authority, |row, authority| {
                    let Some(lifecycle) = row.lifecycle.take() else {
                        return Err(authority);
                    };
                    match lifecycle.consume_terminal(authority) {
                        Ok(backing) => Ok(backing),
                        Err(refused) => {
                            let (lifecycle, authority) = refused.into_parts();
                            row.lifecycle = Some(lifecycle);
                            Err(authority)
                        }
                    }
                })
        };
        let backing = match consumed {
            Ok(Ok(backing)) => backing,
            Ok(Err(authority)) | Err((_, authority)) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::InvariantLost,
                    action: ManuallyDrop::new(PendingResourceCompletion {
                        row: resource,
                        finish: ManuallyDrop::new(ResourceFinish::from_parts(
                            effect,
                            Some(authority),
                            None,
                        )),
                        rundown,
                    }),
                })
            }
        };
        Ok(ResourcePayloadAction {
            backing: ManuallyDrop::new(backing),
            effect: ManuallyDrop::new(effect),
            pending: PendingResourcePayloadAck {
                resource,
                rundown: Some(rundown),
            },
        })
    }

    pub fn ack_resource_payload_release(
        &mut self,
        action: FinalizedResourcePayloadAck,
    ) -> Result<(), OwnerFinalizationRefusal<FinalizedResourcePayloadAck>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action,
            });
        }
        let mut pending = action.pending;
        if !self
            .resources
            .get(pending.resource)
            .ok()
            .is_some_and(|row| {
                row.lifecycle.is_none()
                    && row.creator_pair.is_none()
                    && row.secondary_pairs == 0
                    && row.windows == 0
            })
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedResourcePayloadAck { pending },
            });
        }
        if !self.resources.can_mark_terminal(pending.resource) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedResourcePayloadAck { pending },
            });
        }
        let Some(rundown_ref) = pending.rundown.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedResourcePayloadAck { pending },
            });
        };
        if self
            .resource_tickets
            .validate_rundown_release(rundown_ref)
            .is_err()
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::TicketRefused,
                action: FinalizedResourcePayloadAck { pending },
            });
        }
        if unsafe { self.resources.finalize_occupied(pending.resource) }.is_err() {
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        let Some(rundown) = pending.rundown.take() else {
            return Err(OwnerFinalizationRefusal::Poisoned);
        };
        let rundown = ManuallyDrop::into_inner(rundown);
        let _owner_rundown = unsafe {
            self.resource_tickets
                .consume_rundown_release_validated(rundown)
        };
        Ok(())
    }

    pub fn begin_context_payload_release(
        &mut self,
        pending: PendingContextCompletion<E>,
    ) -> Result<ContextPayloadAction<C, E>, RefusedCompletion<PendingContextCompletion<E>>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action: ManuallyDrop::new(pending),
            });
        }
        let Some(authority_ref) = pending.finish.release_authority() else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::CompletionRequiresRelease,
                action: ManuallyDrop::new(pending),
            });
        };
        let context = pending.row;
        let valid = self.contexts.get(context).ok().is_some_and(|row| {
            row.association_admission_closed
                && row.lifecycle.as_ref().is_some_and(|lifecycle| {
                    lifecycle.lease_census() == 0 && lifecycle.context() == authority_ref.context()
                })
        });
        let pair_exists = self
            .pairs
            .find_occupied_handle(|row| row.context == context)
            .is_some();
        if !valid || pair_exists {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(pending),
            });
        }
        if self
            .context_tickets
            .validate_rundown_release(&pending.rundown)
            .is_err()
        {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::TicketRefused,
                action: ManuallyDrop::new(pending),
            });
        }
        let PendingContextCompletion {
            row: _,
            finish,
            rundown,
        } = pending;
        let (effect, authority) = ManuallyDrop::into_inner(finish).into_parts();
        let Some(authority) = authority else {
            return Err(RefusedCompletion {
                reason: OwnerTableRefusal::InvariantLost,
                action: ManuallyDrop::new(PendingContextCompletion {
                    row: context,
                    finish: ManuallyDrop::new(ContextFinish::from_parts(effect, None)),
                    rundown,
                }),
            });
        };
        let consumed = unsafe {
            self.contexts
                .with_occupied_mut_input(context, authority, |row, authority| {
                    let Some(lifecycle) = row.lifecycle.take() else {
                        return Err(authority);
                    };
                    match lifecycle.consume_terminal(authority) {
                        Ok(released) => Ok(released),
                        Err(refused) => {
                            let (lifecycle, authority) = refused.into_parts();
                            row.lifecycle = Some(lifecycle);
                            Err(authority)
                        }
                    }
                })
        };
        let released = match consumed {
            Ok(Ok(released)) => released,
            Ok(Err(authority)) | Err((_, authority)) => {
                return Err(RefusedCompletion {
                    reason: OwnerTableRefusal::InvariantLost,
                    action: ManuallyDrop::new(PendingContextCompletion {
                        row: context,
                        finish: ManuallyDrop::new(ContextFinish::from_parts(
                            effect,
                            Some(authority),
                        )),
                        rundown,
                    }),
                })
            }
        };
        let (_reservation, owner) = released.into_parts();
        Ok(ContextPayloadAction {
            owner: ManuallyDrop::new(owner),
            effect: ManuallyDrop::new(effect),
            pending: PendingContextPayloadAck {
                context,
                rundown: Some(rundown),
            },
        })
    }

    pub fn ack_context_payload_release(
        &mut self,
        action: FinalizedContextPayloadAck,
    ) -> Result<(), OwnerFinalizationRefusal<FinalizedContextPayloadAck>> {
        if !matches!(
            self.phase,
            OwnerPhase::Open
                | OwnerPhase::Closing
                | OwnerPhase::Quarantined
                | OwnerPhase::RundownSealed
        ) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::WrongPhase { found: self.phase },
                action,
            });
        }
        let mut pending = action.pending;
        if !self
            .contexts
            .get(pending.context)
            .ok()
            .is_some_and(|row| row.lifecycle.is_none() && row.association_admission_closed)
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedContextPayloadAck { pending },
            });
        }
        if !self.contexts.can_mark_terminal(pending.context) {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedContextPayloadAck { pending },
            });
        }
        let Some(rundown_ref) = pending.rundown.as_ref() else {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::InvariantLost,
                action: FinalizedContextPayloadAck { pending },
            });
        };
        if self
            .context_tickets
            .validate_rundown_release(rundown_ref)
            .is_err()
        {
            return Err(OwnerFinalizationRefusal::Recoverable {
                reason: OwnerTableRefusal::TicketRefused,
                action: FinalizedContextPayloadAck { pending },
            });
        }
        if unsafe { self.contexts.finalize_occupied(pending.context) }.is_err() {
            return Err(OwnerFinalizationRefusal::Poisoned);
        }
        let Some(rundown) = pending.rundown.take() else {
            return Err(OwnerFinalizationRefusal::Poisoned);
        };
        let rundown = ManuallyDrop::into_inner(rundown);
        let _owner_rundown = unsafe {
            self.context_tickets
                .consume_rundown_release_validated(rundown)
        };
        Ok(())
    }

    fn validate_storage(storage: &OwnerStorage<'_, B, C, A, W, E>) -> Option<OwnerTableRefusal> {
        let capacities = [
            storage.resources.len(),
            storage.contexts.len(),
            storage.pairs.len(),
            storage.windows.len(),
        ];
        if capacities
            .iter()
            .any(|capacity| *capacity > u32::MAX as usize)
        {
            return Some(OwnerTableRefusal::CapacityTooLarge);
        }
        if storage.resources.len() != storage.resource_tickets.len() {
            return Some(OwnerTableRefusal::ResourceTicketCapacityMismatch);
        }
        if storage.contexts.len() != storage.context_tickets.len() {
            return Some(OwnerTableRefusal::ContextTicketCapacityMismatch);
        }
        if storage.pairs.len() != storage.pair_tickets.len() {
            return Some(OwnerTableRefusal::PairTicketCapacityMismatch);
        }
        if storage.windows.len() != storage.window_tickets.len() {
            return Some(OwnerTableRefusal::WindowTicketCapacityMismatch);
        }
        let stable_fresh =
            storage.resources.iter().all(|slot| {
                slot.state() == SlotState::Vacant && slot.incarnation_high_water() == 0
            }) && storage.contexts.iter().all(|slot| {
                slot.state() == SlotState::Vacant && slot.incarnation_high_water() == 0
            }) && storage.pairs.iter().all(|slot| {
                slot.state() == SlotState::Vacant && slot.incarnation_high_water() == 0
            }) && storage.windows.iter().all(|slot| {
                slot.state() == SlotState::Vacant && slot.incarnation_high_water() == 0
            });
        let tickets_fresh = storage
            .resource_tickets
            .iter()
            .all(ControlTicketSlot::is_fresh)
            && storage
                .context_tickets
                .iter()
                .all(ControlTicketSlot::is_fresh)
            && storage.pair_tickets.iter().all(ControlTicketSlot::is_fresh)
            && storage
                .window_tickets
                .iter()
                .all(ControlTicketSlot::is_fresh);
        if !stable_fresh || !tickets_fresh {
            return Some(OwnerTableRefusal::StorageNotFresh);
        }
        None
    }

    fn require_open(&self) -> Result<(), OwnerTableRefusal> {
        if self.phase == OwnerPhase::Open {
            Ok(())
        } else {
            Err(OwnerTableRefusal::WrongPhase { found: self.phase })
        }
    }

    fn scan_resource_edges(
        &self,
        resource: ResourceHandle,
    ) -> Result<ResourceEdgeScan, OwnerTableRefusal> {
        let resource_id = self.resource(resource)?;
        let mut scan = ResourceEdgeScan {
            creator: None,
            secondary: 0,
            windows: 0,
        };
        for index in 0..self.pairs.capacity() {
            match self.pairs.state_at(index) {
                Some(SlotState::Occupied) => {
                    let pair = self
                        .pairs
                        .occupied_handle_at(index)
                        .ok_or(OwnerTableRefusal::InvariantLost)?;
                    let row = self
                        .pairs
                        .get(pair)
                        .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                    if row.resource != resource {
                        continue;
                    }
                    let context_id = self.context(row.context)?;
                    if row.attachment.resource() != resource_id
                        || row.attachment.context() != context_id
                    {
                        return Err(OwnerTableRefusal::InvariantLost);
                    }
                    match row.kind {
                        PairKind::Creator => {
                            if scan.creator.replace(pair).is_some() {
                                return Err(OwnerTableRefusal::InvariantLost);
                            }
                        }
                        PairKind::Secondary => {
                            scan.secondary = scan
                                .secondary
                                .checked_add(1)
                                .ok_or(OwnerTableRefusal::InvariantLost)?;
                        }
                    }
                }
                Some(SlotState::Tombstone | SlotState::Extracted) => {
                    return Err(OwnerTableRefusal::InvariantLost)
                }
                Some(SlotState::Vacant | SlotState::Retired) => {}
                None => return Err(OwnerTableRefusal::InvariantLost),
            }
        }
        for index in 0..self.windows.capacity() {
            match self.windows.state_at(index) {
                Some(SlotState::Occupied) => {
                    let window = self
                        .windows
                        .occupied_handle_at(index)
                        .ok_or(OwnerTableRefusal::InvariantLost)?;
                    let row = self
                        .windows
                        .get(window)
                        .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                    if row.resource == resource {
                        scan.windows = scan
                            .windows
                            .checked_add(1)
                            .ok_or(OwnerTableRefusal::InvariantLost)?;
                    }
                }
                Some(SlotState::Tombstone | SlotState::Extracted) => {
                    return Err(OwnerTableRefusal::InvariantLost)
                }
                Some(SlotState::Vacant | SlotState::Retired) => {}
                None => return Err(OwnerTableRefusal::InvariantLost),
            }
        }
        if self.orphan_pair_custody.is_some() {
            return Err(OwnerTableRefusal::OrphanCustodyOutstanding);
        }
        Ok(scan)
    }

    fn validate_resource_edges_closed(
        &self,
        resource: ResourceHandle,
    ) -> Result<(), OwnerTableRefusal> {
        let row = self
            .resources
            .get(resource)
            .map_err(|_| OwnerTableRefusal::ResourceNotFound)?;
        let scan = self.scan_resource_edges(resource)?;
        if scan.secondary != row.secondary_pairs
            || scan.windows != row.windows
            || scan.creator != row.creator_pair
        {
            return Err(OwnerTableRefusal::InvariantLost);
        }
        if row.secondary_pairs != 0 {
            return Err(OwnerTableRefusal::SecondaryPairsOutstanding {
                count: row.secondary_pairs,
            });
        }
        if row.windows != 0 {
            return Err(OwnerTableRefusal::WindowsOutstanding { count: row.windows });
        }
        if row.creator_pair.is_some()
            || row
                .lifecycle
                .as_ref()
                .and_then(ResourceLifecycle::attachment)
                .is_some()
            || row
                .lifecycle
                .as_ref()
                .is_some_and(ResourceLifecycle::attachment_may_be_live)
        {
            return Err(OwnerTableRefusal::CreatorAttachmentOutstanding);
        }
        Ok(())
    }

    fn has_release_pending_or_extracted(&self) -> bool {
        let stable_busy = (0..self.resources.capacity()).any(|index| {
            matches!(
                self.resources.state_at(index),
                Some(SlotState::Tombstone | SlotState::Extracted)
            )
        }) || (0..self.contexts.capacity()).any(|index| {
            matches!(
                self.contexts.state_at(index),
                Some(SlotState::Tombstone | SlotState::Extracted)
            )
        }) || (0..self.pairs.capacity()).any(|index| {
            matches!(
                self.pairs.state_at(index),
                Some(SlotState::Tombstone | SlotState::Extracted)
            )
        }) || (0..self.windows.capacity()).any(|index| {
            matches!(
                self.windows.state_at(index),
                Some(SlotState::Tombstone | SlotState::Extracted)
            )
        });
        let ticket_release = (0..self.resources.capacity()).any(|index| {
            self.resource_tickets.state_at(index) == Some(TicketState::ReleasePending)
        }) || (0..self.contexts.capacity())
            .any(|index| self.context_tickets.state_at(index) == Some(TicketState::ReleasePending))
            || (0..self.pairs.capacity()).any(|index| {
                self.pair_tickets.state_at(index) == Some(TicketState::ReleasePending)
            })
            || (0..self.windows.capacity()).any(|index| {
                self.window_tickets.state_at(index) == Some(TicketState::ReleasePending)
            });
        stable_busy || ticket_release
    }

    fn local_uses_drained(&self) -> bool {
        (0..self.pairs.capacity()).all(|index| {
            self.pairs
                .occupied_handle_at(index)
                .and_then(|handle| self.pairs.get(handle).ok())
                .is_none_or(|row| row.uses == 0)
        }) && (0..self.windows.capacity()).all(|index| {
            self.windows
                .occupied_handle_at(index)
                .and_then(|handle| self.windows.get(handle).ok())
                .is_none_or(|row| row.uses == 0)
        })
    }

    fn reset_identity(&self) -> Result<ResetActionIdentity, OwnerTableRefusal> {
        let epoch = self
            .reset
            .as_ref()
            .map(TransportReset::retired_epoch)
            .ok_or(OwnerTableRefusal::InvariantLost)?;
        Ok(ResetActionIdentity {
            table: self.root.id(),
            epoch,
            stage: self.reset_stage,
            index: self.reset_index,
        })
    }

    fn prepare_ticket_reset<K: TicketRowKind>(
        tickets: &mut ControlTickets<'_, OwnerRundown, E, K>,
        row: SlotHandle<K>,
        reset: &TransportReset,
    ) -> Result<PendingResetTicket<K>, OwnerTableRefusal> {
        let state = tickets
            .state_at(row.index())
            .ok_or(OwnerTableRefusal::InvariantLost)?;
        if state == TicketState::Empty {
            return Ok(PendingResetTicket::Empty);
        }
        if !matches!(
            state,
            TicketState::Reserved
                | TicketState::Prepared
                | TicketState::MayHaveSubmitted
                | TicketState::LifecyclePending
                | TicketState::LifecycleApplying
                | TicketState::LifecyclePoisoned
        ) {
            return Err(if state == TicketState::ReleasePending {
                OwnerTableRefusal::ExternalActionOutstanding
            } else {
                OwnerTableRefusal::InvariantLost
            });
        }
        let release = unsafe { tickets.reset_pending(row, reset) }
            .map_err(|_| OwnerTableRefusal::InvariantLost)?;
        let (rundown, ack) = release.into_parts();
        Ok(PendingResetTicket::Reset {
            rundown: ManuallyDrop::new(rundown),
            ack,
        })
    }

    fn can_prepare_ticket_reset<K: TicketRowKind>(
        tickets: &ControlTickets<'_, OwnerRundown, E, K>,
        row: SlotHandle<K>,
        reset: &TransportReset,
    ) -> bool {
        match tickets.state_at(row.index()) {
            Some(TicketState::Empty) => true,
            Some(
                TicketState::Reserved
                | TicketState::Prepared
                | TicketState::MayHaveSubmitted
                | TicketState::LifecyclePending
                | TicketState::LifecycleApplying
                | TicketState::LifecyclePoisoned,
            ) => tickets.can_reset_pending(row, reset),
            Some(
                TicketState::ReleasePending | TicketState::ResetPending | TicketState::Retired,
            )
            | None => false,
        }
    }

    fn next_window_reset_action(
        &mut self,
    ) -> Result<Option<OwnerResetAction<B, C, A, W>>, OwnerTableRefusal> {
        while self.reset_index < self.windows.capacity() {
            let index = self.reset_index;
            let Some(window) = self.windows.occupied_handle_at(index) else {
                if !matches!(
                    self.windows.state_at(index),
                    Some(SlotState::Vacant | SlotState::Retired)
                ) {
                    return Err(OwnerTableRefusal::ExternalActionOutstanding);
                }
                if !matches!(
                    self.window_tickets.state_at(index),
                    Some(TicketState::Empty | TicketState::Retired)
                ) {
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                self.reset_index += 1;
                continue;
            };
            let (resource, identity, map_info) = {
                let row = self
                    .windows
                    .get(window)
                    .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                let WindowRowState::Live(lifecycle) = &row.state else {
                    return Err(OwnerTableRefusal::ResetCursorBlocked);
                };
                let reset = self
                    .reset
                    .as_ref()
                    .ok_or(OwnerTableRefusal::InvariantLost)?;
                if !lifecycle.can_reset(lifecycle.window(), reset) {
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                (row.resource, lifecycle.window(), row.map_info)
            };
            let reset = self
                .reset
                .as_ref()
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            let action_identity = self.reset_identity()?;
            if !Self::can_prepare_ticket_reset(&self.window_tickets, window, reset) {
                return Err(OwnerTableRefusal::InvariantLost);
            }
            let ticket = Self::prepare_ticket_reset(&mut self.window_tickets, window, reset)?;
            let reservation = unsafe {
                self.windows.with_occupied_mut(window, |row| {
                    let old = replace(&mut row.state, WindowRowState::ReleasePending);
                    let lifecycle = match old {
                        WindowRowState::Live(lifecycle) => lifecycle,
                        state => {
                            row.state = state;
                            return None;
                        }
                    };
                    match lifecycle.reset_consume(identity, reset) {
                        Ok(reservation) => Some(reservation),
                        Err(lifecycle) => {
                            row.state = WindowRowState::Live(lifecycle);
                            None
                        }
                    }
                })
            };
            let Ok(Some(reservation)) = reservation else {
                self.pending_reset = Some(PendingResetAction::Stalled {
                    identity: action_identity,
                    ticket: StalledResetTicket::Window(ticket),
                });
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            };
            self.pending_reset = Some(PendingResetAction::Window {
                identity: action_identity,
                window,
                resource,
                ticket,
            });
            return Ok(Some(OwnerResetAction::Window(ResetWindowAction {
                reservation: ManuallyDrop::new(reservation),
                map_info,
                pending: PendingResetPayload {
                    identity: action_identity,
                },
            })));
        }
        Ok(None)
    }

    fn next_pair_reset_action(
        &mut self,
    ) -> Result<Option<OwnerResetAction<B, C, A, W>>, OwnerTableRefusal> {
        let initializing_pair = match self.orphan_pair_custody.as_ref() {
            Some(OrphanPairCustody::Initializing { pair, .. }) => Some(*pair),
            Some(OrphanPairCustody::CreatorLifecycleBegun { .. }) | None => None,
        };
        if let Some(pair) = initializing_pair {
            let mut can_advance = true;
            for index in self.reset_index..pair.index() {
                match self.pairs.state_at(index) {
                    Some(SlotState::Vacant | SlotState::Retired) => {}
                    Some(SlotState::Occupied) => {
                        let handle = self
                            .pairs
                            .occupied_handle_at(index)
                            .ok_or(OwnerTableRefusal::InvariantLost)?;
                        let row = self
                            .pairs
                            .get(handle)
                            .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                        if row.kind == PairKind::Secondary {
                            can_advance = false;
                            break;
                        }
                    }
                    Some(SlotState::Tombstone | SlotState::Extracted) => {
                        return Err(OwnerTableRefusal::ExternalActionOutstanding)
                    }
                    None => return Err(OwnerTableRefusal::InvariantLost),
                }
            }
            if can_advance {
                self.reset_index = pair.index();
                let Some(OrphanPairCustody::Initializing {
                    pair,
                    resource,
                    context,
                    attachment,
                    leased,
                    association,
                    reservation,
                }) = self.orphan_pair_custody.take()
                else {
                    return Err(OwnerTableRefusal::InvariantLost);
                };
                let exact = self.pairs.get(pair).ok().is_some_and(|row| {
                    row.resource == resource
                        && row.context == context
                        && row.attachment == attachment
                        && row.uses == 0
                        && matches!(row.state, PairRowState::Quarantined)
                });
                if !exact {
                    self.orphan_pair_custody = Some(OrphanPairCustody::Initializing {
                        pair,
                        resource,
                        context,
                        attachment,
                        leased,
                        association,
                        reservation,
                    });
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                let context_can_cancel = self.contexts.get(context).ok().is_some_and(|row| {
                    row.lifecycle
                        .as_ref()
                        .is_some_and(|lifecycle| lifecycle.can_cancel_attachment(&leased))
                });
                let ticket_can_cancel = self
                    .resource_tickets
                    .validate_reservation(&reservation)
                    .is_ok();
                if !context_can_cancel || !ticket_can_cancel {
                    self.orphan_pair_custody = Some(OrphanPairCustody::Initializing {
                        pair,
                        resource,
                        context,
                        attachment,
                        leased,
                        association,
                        reservation,
                    });
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                let action_identity = self.reset_identity()?;
                let changed = unsafe {
                    self.pairs.with_occupied_mut(pair, |row| {
                        row.state = PairRowState::ReleasePending;
                    })
                };
                if changed.is_err() {
                    self.orphan_pair_custody = Some(OrphanPairCustody::Initializing {
                        pair,
                        resource,
                        context,
                        attachment,
                        leased,
                        association,
                        reservation,
                    });
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                self.pending_reset = Some(PendingResetAction::InitializingPair {
                    identity: action_identity,
                    pair,
                    resource,
                    context,
                    attachment,
                    leased,
                    reservation,
                });
                return Ok(Some(OwnerResetAction::SecondaryPair(ResetPairAction {
                    association,
                    pending: PendingResetPayload {
                        identity: action_identity,
                    },
                })));
            }
        }
        while self.reset_index < self.pairs.capacity() {
            let index = self.reset_index;
            let Some(pair) = self.pairs.occupied_handle_at(index) else {
                if !matches!(
                    self.pairs.state_at(index),
                    Some(SlotState::Vacant | SlotState::Retired)
                ) {
                    return Err(OwnerTableRefusal::ExternalActionOutstanding);
                }
                self.reset_index += 1;
                continue;
            };
            let (kind, resource, context, attachment) = {
                let row = self
                    .pairs
                    .get(pair)
                    .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                (row.kind, row.resource, row.context, row.attachment)
            };
            if kind == PairKind::Creator {
                self.reset_index += 1;
                continue;
            }
            let reset = self
                .reset
                .as_ref()
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            let pair_resettable = self.pairs.get(pair).ok().is_some_and(|row| {
                matches!(&row.state, PairRowState::Secondary(lifecycle)
                    if row.uses == 0 && lifecycle.can_reset(attachment, reset))
            });
            if !pair_resettable || !Self::can_prepare_ticket_reset(&self.pair_tickets, pair, reset)
            {
                return Err(OwnerTableRefusal::InvariantLost);
            }
            let action_identity = self.reset_identity()?;
            let ticket = Self::prepare_ticket_reset(&mut self.pair_tickets, pair, reset)?;
            let released = unsafe {
                self.pairs.with_occupied_mut(pair, |row| {
                    let old = replace(&mut row.state, PairRowState::ReleasePending);
                    let lifecycle = match old {
                        PairRowState::Secondary(lifecycle) => lifecycle,
                        state => {
                            row.state = state;
                            return None;
                        }
                    };
                    match lifecycle.reset_consume(attachment, reset) {
                        Ok(released) => Some(released),
                        Err(lifecycle) => {
                            row.state = PairRowState::Secondary(lifecycle);
                            None
                        }
                    }
                })
            };
            let Ok(Some(released)) = released else {
                self.pending_reset = Some(PendingResetAction::Stalled {
                    identity: action_identity,
                    ticket: StalledResetTicket::Pair(ticket),
                });
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            };
            let (release, association) = released.into_parts();
            self.pending_reset = Some(PendingResetAction::SecondaryPair {
                identity: action_identity,
                pair,
                resource,
                context,
                release: ManuallyDrop::new(release),
                ticket,
            });
            return Ok(Some(OwnerResetAction::SecondaryPair(ResetPairAction {
                association: ManuallyDrop::new(association),
                pending: PendingResetPayload {
                    identity: action_identity,
                },
            })));
        }
        Ok(None)
    }

    fn next_resource_reset_action(
        &mut self,
    ) -> Result<Option<OwnerResetAction<B, C, A, W>>, OwnerTableRefusal> {
        while self.reset_index < self.resources.capacity() {
            let index = self.reset_index;
            let Some(resource) = self.resources.occupied_handle_at(index) else {
                if !matches!(
                    self.resources.state_at(index),
                    Some(SlotState::Vacant | SlotState::Retired)
                ) {
                    return Err(OwnerTableRefusal::ExternalActionOutstanding);
                }
                self.reset_index += 1;
                continue;
            };
            let (identity, creator_pair) = {
                let row = self
                    .resources
                    .get(resource)
                    .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                if row.secondary_pairs != 0 || row.windows != 0 {
                    return Err(OwnerTableRefusal::ResetCursorBlocked);
                }
                let lifecycle = row
                    .lifecycle
                    .as_ref()
                    .ok_or(OwnerTableRefusal::InvariantLost)?;
                (lifecycle.resource(), row.creator_pair)
            };
            let reset = self
                .reset
                .as_ref()
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            let lifecycle_resettable = self
                .resources
                .get(resource)
                .ok()
                .and_then(|row| row.lifecycle.as_ref())
                .is_some_and(|lifecycle| lifecycle.can_reset(identity, reset));
            let creator_preflight = match creator_pair {
                None => self
                    .resources
                    .get(resource)
                    .ok()
                    .and_then(|row| row.lifecycle.as_ref())
                    .is_some_and(|lifecycle| lifecycle.attachment().is_none()),
                Some(pair) => {
                    let exact_pair = self.pairs.get(pair).ok().is_some_and(|row| {
                        row.kind == PairKind::Creator
                            && row.resource == resource
                            && row.attachment.resource() == identity
                            && row.uses == 0
                            && matches!(
                                row.state,
                                PairRowState::Creator(_) | PairRowState::Quarantined
                            )
                    });
                    let exact_lifecycle = self
                        .resources
                        .get(resource)
                        .ok()
                        .and_then(|row| row.lifecycle.as_ref())
                        .and_then(ResourceLifecycle::attachment)
                        .is_some_and(|attachment| {
                            self.pairs.get(pair).ok().is_some_and(|row| {
                                row.attachment == attachment
                                    && self.contexts.get(row.context).ok().is_some_and(|owner| {
                                        owner.lifecycle.as_ref().is_some_and(|lifecycle| {
                                            lifecycle.context() == attachment.context()
                                        })
                                    })
                            })
                        });
                    let orphan_exact = match self.orphan_pair_custody.as_ref() {
                        None => self
                            .pairs
                            .get(pair)
                            .ok()
                            .is_some_and(|row| matches!(row.state, PairRowState::Creator(_))),
                        Some(OrphanPairCustody::CreatorLifecycleBegun {
                            pair: found_pair,
                            resource: found_resource,
                            context: found_context,
                            attachment: found_attachment,
                            ..
                        }) => self.pairs.get(pair).ok().is_some_and(|row| {
                            *found_pair == pair
                                && *found_resource == resource
                                && *found_context == row.context
                                && *found_attachment == row.attachment
                                && matches!(row.state, PairRowState::Quarantined)
                        }),
                        Some(OrphanPairCustody::Initializing { .. }) => false,
                    };
                    exact_pair && exact_lifecycle && orphan_exact
                }
            };
            if !lifecycle_resettable
                || !creator_preflight
                || !Self::can_prepare_ticket_reset(&self.resource_tickets, resource, reset)
            {
                return Err(OwnerTableRefusal::InvariantLost);
            }
            let action_identity = self.reset_identity()?;
            let ticket = Self::prepare_ticket_reset(&mut self.resource_tickets, resource, reset)?;

            let lifecycle = unsafe {
                self.resources
                    .with_occupied_mut(resource, |row| row.lifecycle.take())
            };
            let Ok(Some(lifecycle)) = lifecycle else {
                self.pending_reset = Some(PendingResetAction::Stalled {
                    identity: action_identity,
                    ticket: StalledResetTicket::Resource(ticket),
                });
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            };
            let (backing, release) = match lifecycle.reset_consume(identity, reset) {
                Ok(parts) => parts,
                Err((lifecycle, release)) => {
                    self.orphan_reset_custody = Some(OrphanResetCustody::ResourceLifecycle {
                        identity: action_identity,
                        resource,
                        lifecycle: ManuallyDrop::new(lifecycle),
                        release: release.map(ManuallyDrop::new),
                        ticket,
                    });
                    self.enter_quarantined();
                    return Err(OwnerTableRefusal::InvariantLost);
                }
            };
            let backing = ManuallyDrop::new(backing);
            let mut release = release.map(ManuallyDrop::new);

            let creator = match creator_pair {
                None => {
                    if release.is_some() {
                        self.orphan_reset_custody = Some(OrphanResetCustody::Resource {
                            identity: action_identity,
                            resource,
                            backing,
                            release,
                            ticket,
                        });
                        self.enter_quarantined();
                        return Err(OwnerTableRefusal::InvariantLost);
                    }
                    None
                }
                Some(pair) => {
                    let (context, attachment) = match self.pairs.get(pair) {
                        Ok(row) => (row.context, row.attachment),
                        Err(_) => {
                            self.orphan_reset_custody = Some(OrphanResetCustody::Resource {
                                identity: action_identity,
                                resource,
                                backing,
                                release,
                                ticket,
                            });
                            self.enter_quarantined();
                            return Err(OwnerTableRefusal::InvariantLost);
                        }
                    };
                    let Some(release_token) = release.take() else {
                        self.orphan_reset_custody = Some(OrphanResetCustody::Resource {
                            identity: action_identity,
                            resource,
                            backing,
                            release,
                            ticket,
                        });
                        self.enter_quarantined();
                        return Err(OwnerTableRefusal::InvariantLost);
                    };
                    if release_token.reservation().attachment() != attachment {
                        release = Some(release_token);
                        self.orphan_reset_custody = Some(OrphanResetCustody::Resource {
                            identity: action_identity,
                            resource,
                            backing,
                            release,
                            ticket,
                        });
                        self.enter_quarantined();
                        return Err(OwnerTableRefusal::InvariantLost);
                    }
                    let association = match self
                        .take_creator_reset_association(pair, resource, context, attachment)
                    {
                        Ok(association) => association,
                        Err(_) => {
                            release = Some(release_token);
                            self.orphan_reset_custody = Some(OrphanResetCustody::Resource {
                                identity: action_identity,
                                resource,
                                backing,
                                release,
                                ticket,
                            });
                            self.enter_quarantined();
                            return Err(OwnerTableRefusal::InvariantLost);
                        }
                    };
                    Some((
                        PendingCreatorReset {
                            pair,
                            context,
                            release: release_token,
                        },
                        association,
                    ))
                }
            };
            let (pending_creator, association) = match creator {
                Some((pending, association)) => {
                    (Some(pending), Some(ManuallyDrop::new(association)))
                }
                None => (None, None),
            };
            self.pending_reset = Some(PendingResetAction::Resource {
                identity: action_identity,
                resource,
                creator: pending_creator,
                ticket,
            });
            return Ok(Some(OwnerResetAction::Resource(ResetResourceAction {
                backing,
                creator_association: association,
                pending: PendingResetPayload {
                    identity: action_identity,
                },
            })));
        }
        Ok(None)
    }

    fn next_context_reset_action(
        &mut self,
    ) -> Result<Option<OwnerResetAction<B, C, A, W>>, OwnerTableRefusal> {
        while self.reset_index < self.contexts.capacity() {
            let index = self.reset_index;
            let Some(context) = self.contexts.occupied_handle_at(index) else {
                if !matches!(
                    self.contexts.state_at(index),
                    Some(SlotState::Vacant | SlotState::Retired)
                ) {
                    return Err(OwnerTableRefusal::ExternalActionOutstanding);
                }
                self.reset_index += 1;
                continue;
            };
            let identity = {
                let row = self
                    .contexts
                    .get(context)
                    .map_err(|_| OwnerTableRefusal::InvariantLost)?;
                let lifecycle = row
                    .lifecycle
                    .as_ref()
                    .ok_or(OwnerTableRefusal::InvariantLost)?;
                if lifecycle.lease_census() != 0 {
                    return Err(OwnerTableRefusal::ResetCursorBlocked);
                }
                lifecycle.context()
            };
            if self
                .pairs
                .find_occupied_handle(|row| row.context == context)
                .is_some()
            {
                return Err(OwnerTableRefusal::ResetCursorBlocked);
            }
            let reset = self
                .reset
                .as_ref()
                .ok_or(OwnerTableRefusal::InvariantLost)?;
            let resettable = self
                .contexts
                .get(context)
                .ok()
                .and_then(|row| row.lifecycle.as_ref())
                .is_some_and(|lifecycle| lifecycle.can_reset(identity, reset));
            if !resettable || !Self::can_prepare_ticket_reset(&self.context_tickets, context, reset)
            {
                return Err(OwnerTableRefusal::InvariantLost);
            }
            let action_identity = self.reset_identity()?;
            let ticket = Self::prepare_ticket_reset(&mut self.context_tickets, context, reset)?;
            let lifecycle = unsafe {
                self.contexts
                    .with_occupied_mut(context, |row| row.lifecycle.take())
            };
            let Ok(Some(lifecycle)) = lifecycle else {
                self.pending_reset = Some(PendingResetAction::Stalled {
                    identity: action_identity,
                    ticket: StalledResetTicket::Context(ticket),
                });
                self.enter_quarantined();
                return Err(OwnerTableRefusal::InvariantLost);
            };
            let released = match lifecycle.reset_consume(identity, reset) {
                Ok(released) => released,
                Err(lifecycle) => {
                    self.orphan_reset_custody = Some(OrphanResetCustody::ContextLifecycle {
                        identity: action_identity,
                        context,
                        lifecycle: ManuallyDrop::new(lifecycle),
                        ticket,
                    });
                    self.enter_quarantined();
                    return Err(OwnerTableRefusal::InvariantLost);
                }
            };
            let (_, owner) = released.into_parts();
            self.pending_reset = Some(PendingResetAction::Context {
                identity: action_identity,
                context,
                ticket,
            });
            return Ok(Some(OwnerResetAction::Context(ResetContextAction {
                owner: ManuallyDrop::new(owner),
                pending: PendingResetPayload {
                    identity: action_identity,
                },
            })));
        }
        Ok(None)
    }

    fn take_creator_reset_association(
        &mut self,
        pair: PairHandle,
        resource: ResourceHandle,
        context: ContextHandle,
        attachment: TransportAttachment,
    ) -> Result<A, OwnerTableRefusal> {
        if matches!(
            self.orphan_pair_custody,
            Some(OrphanPairCustody::CreatorLifecycleBegun { pair: found, .. }) if found == pair
        ) {
            let Some(OrphanPairCustody::CreatorLifecycleBegun {
                pair: found_pair,
                resource: found_resource,
                context: found_context,
                attachment: found_attachment,
                association,
                post,
                request,
            }) = self.orphan_pair_custody.take()
            else {
                return Err(OwnerTableRefusal::InvariantLost);
            };
            if found_pair != pair
                || found_resource != resource
                || found_context != context
                || found_attachment != attachment
            {
                self.orphan_pair_custody = Some(OrphanPairCustody::CreatorLifecycleBegun {
                    pair: found_pair,
                    resource: found_resource,
                    context: found_context,
                    attachment: found_attachment,
                    association,
                    post,
                    request,
                });
                return Err(OwnerTableRefusal::InvariantLost);
            }
            let changed = unsafe {
                self.pairs.with_occupied_mut(pair, |row| {
                    if !matches!(row.state, PairRowState::Quarantined) {
                        return false;
                    }
                    row.state = PairRowState::ReleasePending;
                    true
                })
            };
            if changed != Ok(true) {
                self.orphan_pair_custody = Some(OrphanPairCustody::CreatorLifecycleBegun {
                    pair: found_pair,
                    resource: found_resource,
                    context: found_context,
                    attachment: found_attachment,
                    association,
                    post,
                    request,
                });
                return Err(OwnerTableRefusal::InvariantLost);
            }
            let _post = post;
            let _request = request;
            return Ok(ManuallyDrop::into_inner(association));
        }
        unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                if row.resource != resource
                    || row.context != context
                    || row.attachment != attachment
                    || row.kind != PairKind::Creator
                {
                    return None;
                }
                let old = replace(&mut row.state, PairRowState::ReleasePending);
                match old {
                    PairRowState::Creator(association) => {
                        Some(ManuallyDrop::into_inner(association))
                    }
                    state => {
                        row.state = state;
                        None
                    }
                }
            })
        }
        .map_err(|_| OwnerTableRefusal::InvariantLost)?
        .ok_or(OwnerTableRefusal::InvariantLost)
    }

    fn can_commit_ticket_reset<K: TicketRowKind>(
        tickets: &ControlTickets<'_, OwnerRundown, E, K>,
        row: SlotHandle<K>,
        ticket: &PendingResetTicket<K>,
    ) -> bool {
        match ticket {
            PendingResetTicket::Empty => tickets.can_commit_empty(row),
            PendingResetTicket::Reset { ack, .. } => tickets.can_ack_reset(ack),
        }
    }

    fn can_commit_reset_action(&self, pending: &PendingResetAction) -> bool {
        match pending {
            PendingResetAction::Stalled { ticket, .. } => {
                match ticket {
                    StalledResetTicket::Window(ticket) => {
                        let _ = ticket;
                    }
                    StalledResetTicket::Pair(ticket) => {
                        let _ = ticket;
                    }
                    StalledResetTicket::Resource(ticket) => {
                        let _ = ticket;
                    }
                    StalledResetTicket::Context(ticket) => {
                        let _ = ticket;
                    }
                }
                false
            }
            PendingResetAction::Window {
                window,
                resource,
                ticket,
                ..
            } => {
                self.windows.get(*window).ok().is_some_and(|row| {
                    row.resource == *resource
                        && row.uses == 0
                        && matches!(row.state, WindowRowState::ReleasePending)
                }) && self
                    .resources
                    .get(*resource)
                    .ok()
                    .is_some_and(|row| row.windows != 0)
                    && self.windows.can_mark_terminal(*window)
                    && Self::can_commit_ticket_reset(&self.window_tickets, *window, ticket)
            }
            PendingResetAction::SecondaryPair {
                pair,
                resource,
                context,
                release,
                ticket,
                ..
            } => {
                self.pairs.get(*pair).ok().is_some_and(|row| {
                    row.resource == *resource
                        && row.context == *context
                        && row.kind == PairKind::Secondary
                        && row.uses == 0
                        && matches!(row.state, PairRowState::ReleasePending)
                        && row.attachment == release.reservation().attachment()
                }) && self.contexts.get(*context).ok().is_some_and(|row| {
                    row.lifecycle
                        .as_ref()
                        .is_some_and(|lifecycle| lifecycle.can_return_attachment(release))
                }) && self
                    .resources
                    .get(*resource)
                    .ok()
                    .is_some_and(|row| row.secondary_pairs != 0)
                    && self.pairs.can_mark_terminal(*pair)
                    && Self::can_commit_ticket_reset(&self.pair_tickets, *pair, ticket)
            }
            PendingResetAction::InitializingPair {
                pair,
                resource,
                context,
                attachment,
                leased,
                reservation,
                ..
            } => {
                self.pairs.get(*pair).ok().is_some_and(|row| {
                    row.resource == *resource
                        && row.context == *context
                        && row.kind == PairKind::Creator
                        && row.attachment == *attachment
                        && row.uses == 0
                        && matches!(row.state, PairRowState::ReleasePending)
                }) && self.contexts.get(*context).ok().is_some_and(|row| {
                    row.lifecycle
                        .as_ref()
                        .is_some_and(|lifecycle| lifecycle.can_cancel_attachment(leased))
                }) && self
                    .resource_tickets
                    .validate_reservation(reservation)
                    .is_ok()
                    && self.pair_tickets.can_commit_empty(*pair)
                    && self.pairs.can_mark_terminal(*pair)
            }
            PendingResetAction::Resource {
                resource,
                creator,
                ticket,
                ..
            } => {
                let Some(row) = self.resources.get(*resource).ok() else {
                    return false;
                };
                let creator_valid = match creator {
                    Some(creator) => {
                        row.creator_pair == Some(creator.pair)
                            && self.pairs.get(creator.pair).ok().is_some_and(|pair| {
                                pair.resource == *resource
                                    && pair.context == creator.context
                                    && pair.kind == PairKind::Creator
                                    && pair.uses == 0
                                    && matches!(pair.state, PairRowState::ReleasePending)
                                    && pair.attachment == creator.release.reservation().attachment()
                            })
                            && self.contexts.get(creator.context).ok().is_some_and(|row| {
                                row.lifecycle.as_ref().is_some_and(|lifecycle| {
                                    lifecycle.can_return_attachment(&creator.release)
                                })
                            })
                            && self.pairs.can_mark_terminal(creator.pair)
                            && self.pair_tickets.can_commit_empty(creator.pair)
                    }
                    None => row.creator_pair.is_none(),
                };
                let pairs_exact = self
                    .pairs
                    .find_unique_occupied_handle(|pair| pair.resource == *resource)
                    .ok()
                    .is_some_and(|found| found == creator.as_ref().map(|creator| creator.pair));
                let windows_empty = self
                    .windows
                    .find_unique_occupied_handle(|window| window.resource == *resource)
                    .ok()
                    .is_some_and(|found| found.is_none());
                row.lifecycle.is_none()
                    && row.secondary_pairs == 0
                    && row.windows == 0
                    && creator_valid
                    && pairs_exact
                    && windows_empty
                    && self.resources.can_mark_terminal(*resource)
                    && Self::can_commit_ticket_reset(&self.resource_tickets, *resource, ticket)
            }
            PendingResetAction::Context {
                context, ticket, ..
            } => {
                self.contexts
                    .get(*context)
                    .ok()
                    .is_some_and(|row| row.lifecycle.is_none())
                    && self
                        .pairs
                        .find_unique_occupied_handle(|pair| pair.context == *context)
                        .ok()
                        .is_some_and(|found| found.is_none())
                    && self.contexts.can_mark_terminal(*context)
                    && Self::can_commit_ticket_reset(&self.context_tickets, *context, ticket)
            }
        }
    }

    unsafe fn commit_ticket_reset_validated<K: TicketRowKind>(
        tickets: &mut ControlTickets<'_, OwnerRundown, E, K>,
        ticket: PendingResetTicket<K>,
    ) {
        if let PendingResetTicket::Reset { rundown, ack } = ticket {
            let _nonce = ManuallyDrop::into_inner(rundown).nonce;
            unsafe { tickets.ack_reset_validated(ack) };
        }
    }

    unsafe fn commit_window_reset_validated(
        &mut self,
        window: WindowHandle,
        resource: ResourceHandle,
        ticket: PendingResetTicket<WindowSlotKind>,
    ) {
        unsafe {
            self.resources.with_occupied_mut_validated(resource, |row| {
                row.windows = row.windows.wrapping_sub(1);
            });
            self.windows.finalize_occupied_validated(window);
            Self::commit_ticket_reset_validated(&mut self.window_tickets, ticket);
        }
    }

    unsafe fn commit_pair_reset_validated(
        &mut self,
        pair: PairHandle,
        resource: ResourceHandle,
        context: ContextHandle,
        release: ReleasedAttachmentLease,
        ticket: PendingResetTicket<PairSlotKind>,
    ) {
        unsafe {
            self.contexts.with_occupied_mut_validated(context, |row| {
                let lifecycle = match row.lifecycle.as_mut() {
                    Some(lifecycle) => lifecycle,
                    None => core::hint::unreachable_unchecked(),
                };
                let _released = lifecycle.return_attachment_validated(release);
            });
            self.resources.with_occupied_mut_validated(resource, |row| {
                row.secondary_pairs = row.secondary_pairs.wrapping_sub(1);
            });
            self.pairs.finalize_occupied_validated(pair);
            Self::commit_ticket_reset_validated(&mut self.pair_tickets, ticket);
        }
    }

    unsafe fn commit_initializing_pair_reset_validated(
        &mut self,
        pair: PairHandle,
        _resource: ResourceHandle,
        context: ContextHandle,
        _attachment: TransportAttachment,
        leased: LeasedAttachmentReservation,
        reservation: TicketReservation<ResourceSlotKind>,
    ) {
        unsafe {
            self.contexts.with_occupied_mut_validated(context, |row| {
                let lifecycle = match row.lifecycle.as_mut() {
                    Some(lifecycle) => lifecycle,
                    None => core::hint::unreachable_unchecked(),
                };
                let _released = lifecycle.cancel_attachment_validated(leased);
            });
            self.pairs.finalize_occupied_validated(pair);
            let rundown = match self
                .resource_tickets
                .cancel_reservation_validated(reservation)
            {
                Some(rundown) => rundown,
                None => core::hint::unreachable_unchecked(),
            };
            let _nonce = rundown.nonce;
        }
    }

    unsafe fn commit_resource_reset_validated(
        &mut self,
        resource: ResourceHandle,
        creator: Option<PendingCreatorReset>,
        ticket: PendingResetTicket<ResourceSlotKind>,
    ) {
        unsafe {
            if let Some(creator) = creator {
                let release = ManuallyDrop::into_inner(creator.release);
                self.contexts
                    .with_occupied_mut_validated(creator.context, |row| {
                        let lifecycle = match row.lifecycle.as_mut() {
                            Some(lifecycle) => lifecycle,
                            None => core::hint::unreachable_unchecked(),
                        };
                        let _released = lifecycle.return_attachment_validated(release);
                    });
                self.resources.with_occupied_mut_validated(resource, |row| {
                    row.creator_pair = None;
                });
                self.pairs.finalize_occupied_validated(creator.pair);
            }
            self.resources.finalize_occupied_validated(resource);
            Self::commit_ticket_reset_validated(&mut self.resource_tickets, ticket);
        }
    }

    unsafe fn commit_context_reset_validated(
        &mut self,
        context: ContextHandle,
        ticket: PendingResetTicket<ContextSlotKind>,
    ) {
        unsafe {
            self.contexts.finalize_occupied_validated(context);
            Self::commit_ticket_reset_validated(&mut self.context_tickets, ticket);
        }
    }

    fn reset_action_matches(pending: &PendingResetAction, found: ResetActionIdentity) -> bool {
        let expected = match pending {
            PendingResetAction::Stalled { identity, .. }
            | PendingResetAction::Window { identity, .. }
            | PendingResetAction::SecondaryPair { identity, .. }
            | PendingResetAction::InitializingPair { identity, .. }
            | PendingResetAction::Resource { identity, .. }
            | PendingResetAction::Context { identity, .. } => *identity,
        };
        expected.table == found.table
            && expected.epoch == found.epoch
            && expected.stage == found.stage
            && expected.index == found.index
    }

    fn advance_reset_stage(&mut self) -> Result<bool, OwnerTableRefusal> {
        self.reset_index = 0;
        self.reset_stage = match self.reset_stage {
            ResetStage::Windows => ResetStage::SecondaryPairs,
            ResetStage::SecondaryPairs => ResetStage::Resources,
            ResetStage::Resources => ResetStage::Contexts,
            ResetStage::Contexts => {
                if !self.validate_successor_rebind() {
                    return Err(OwnerTableRefusal::InvariantLost);
                }
                self.phase = if self.reset_exhausted {
                    OwnerPhase::Exhausted
                } else {
                    OwnerPhase::Ready
                };
                return Ok(false);
            }
        };
        Ok(true)
    }

    fn validate_successor_rebind(&self) -> bool {
        if self.pending_reset.is_some()
            || self.orphan_pair_custody.is_some()
            || self.orphan_reset_custody.is_some()
            || self.active_runs != 0
            || self.resources.validate_epoch_rebind().is_err()
            || self.contexts.validate_epoch_rebind().is_err()
            || self.pairs.validate_epoch_rebind().is_err()
            || self.windows.validate_epoch_rebind().is_err()
            || self.resource_tickets.validate_epoch_rebind().is_err()
            || self.context_tickets.validate_epoch_rebind().is_err()
            || self.pair_tickets.validate_epoch_rebind().is_err()
            || self.window_tickets.validate_epoch_rebind().is_err()
        {
            return false;
        }
        Self::validate_slot_ticket_pairs(&self.resources, &self.resource_tickets)
            && Self::validate_slot_ticket_pairs(&self.contexts, &self.context_tickets)
            && Self::validate_slot_ticket_pairs(&self.pairs, &self.pair_tickets)
            && Self::validate_slot_ticket_pairs(&self.windows, &self.window_tickets)
    }

    fn validate_slot_ticket_pairs<T, K: TicketRowKind>(
        slots: &StableSlots<'_, T, K>,
        tickets: &ControlTickets<'_, OwnerRundown, E, K>,
    ) -> bool {
        (0..slots.capacity()).all(|index| {
            matches!(
                (slots.state_at(index), tickets.state_at(index)),
                (
                    Some(SlotState::Vacant | SlotState::Retired),
                    Some(TicketState::Empty | TicketState::Retired)
                )
            )
        })
    }

    unsafe fn apply_successor_rebind(&mut self, epoch: TransportEpoch) {
        unsafe {
            self.resource_tickets.apply_epoch_rebind(epoch, |index| {
                self.resources.state_at(index) == Some(SlotState::Vacant)
            });
            self.context_tickets.apply_epoch_rebind(epoch, |index| {
                self.contexts.state_at(index) == Some(SlotState::Vacant)
            });
            self.pair_tickets.apply_epoch_rebind(epoch, |index| {
                self.pairs.state_at(index) == Some(SlotState::Vacant)
            });
            self.window_tickets.apply_epoch_rebind(epoch, |index| {
                self.windows.state_at(index) == Some(SlotState::Vacant)
            });
            self.resources.apply_epoch_rebind(epoch);
            self.contexts.apply_epoch_rebind(epoch);
            self.pairs.apply_epoch_rebind(epoch);
            self.windows.apply_epoch_rebind(epoch);
        }
    }

    fn enter_quarantined(&mut self) {
        if !matches!(self.phase, OwnerPhase::Ready | OwnerPhase::Exhausted) {
            self.phase = OwnerPhase::Quarantined;
        }
    }

    fn mint_rundown(&mut self) -> Result<OwnerRundown, OwnerTableRefusal> {
        let Some(raw) = self.rundown_high_water.checked_add(1) else {
            return Err(OwnerTableRefusal::RundownExhausted);
        };
        let Some(nonce) = NonZeroU64::new(raw) else {
            return Err(OwnerTableRefusal::RundownExhausted);
        };
        self.rundown_high_water = raw;
        Ok(OwnerRundown { nonce })
    }

    fn cancel_resource_ticket(
        tickets: &mut ControlTickets<'_, OwnerRundown, E, ResourceSlotKind>,
        reservation: TicketReservation<ResourceSlotKind>,
    ) -> bool {
        let Ok(release) = tickets.cancel_reservation(reservation) else {
            return false;
        };
        let (rundown, pending) = release.into_parts();
        let _nonce = rundown.nonce;
        let ack = unsafe { pending.assume_released() };
        tickets.ack_release(ack).is_ok()
    }

    fn cancel_context_ticket(
        tickets: &mut ControlTickets<'_, OwnerRundown, E, ContextSlotKind>,
        reservation: TicketReservation<ContextSlotKind>,
    ) -> bool {
        let Ok(release) = tickets.cancel_reservation(reservation) else {
            return false;
        };
        let (rundown, pending) = release.into_parts();
        let _nonce = rundown.nonce;
        let ack = unsafe { pending.assume_released() };
        tickets.ack_release(ack).is_ok()
    }

    fn cancel_pair_ticket(
        tickets: &mut ControlTickets<'_, OwnerRundown, E, PairSlotKind>,
        reservation: TicketReservation<PairSlotKind>,
    ) -> bool {
        let Ok(release) = tickets.cancel_reservation(reservation) else {
            return false;
        };
        let (rundown, pending) = release.into_parts();
        let _nonce = rundown.nonce;
        let ack = unsafe { pending.assume_released() };
        tickets.ack_release(ack).is_ok()
    }

    fn cancel_window_ticket(
        tickets: &mut ControlTickets<'_, OwnerRundown, E, WindowSlotKind>,
        reservation: TicketReservation<WindowSlotKind>,
    ) -> bool {
        let Ok(release) = tickets.cancel_reservation(reservation) else {
            return false;
        };
        let (rundown, pending) = release.into_parts();
        let _nonce = rundown.nonce;
        let ack = unsafe { pending.assume_released() };
        tickets.ack_release(ack).is_ok()
    }

    fn rollback_window_refusal(
        &mut self,
        handle: WindowHandle,
        reason: OwnerTableRefusal,
    ) -> RefusedAdmission<W, WindowRow<W>> {
        let taken = unsafe {
            self.windows.with_occupied_mut(handle, |row| {
                replace(&mut row.state, WindowRowState::Quarantined)
            })
        };
        let Ok(WindowRowState::Initializing(reservation)) = taken else {
            self.enter_quarantined();
            return RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::TableQuarantined,
            };
        };
        let reservation = ManuallyDrop::into_inner(reservation);
        let terminal = unsafe { handle.assume_terminal() };
        let tombstone = match self.windows.mark_tombstone(terminal) {
            Ok(tombstone) => tombstone,
            Err(_) => {
                self.enter_quarantined();
                return RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::Input(ManuallyDrop::new(reservation)),
                };
            }
        };
        let extracted = match self.windows.extract(tombstone) {
            Ok(extracted) => extracted,
            Err(_) => {
                self.enter_quarantined();
                return RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::Input(ManuallyDrop::new(reservation)),
                };
            }
        };
        let (_row, pending) = extracted.into_parts();
        let ack = unsafe { pending.assume_payload_finalized() };
        if self.windows.ack_finalized(ack).is_err() {
            self.enter_quarantined();
            return RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::Input(ManuallyDrop::new(reservation)),
            };
        }
        RefusedAdmission {
            reason,
            custody: RefusedCustody::Input(ManuallyDrop::new(reservation)),
        }
    }

    fn cancel_local_pair(&mut self, mut row: PairRow<A>) -> Result<A, PairRow<A>> {
        let old = replace(&mut row.state, PairRowState::Quarantined);
        let PairRowState::Initializing {
            leased,
            association,
        } = old
        else {
            row.state = old;
            return Err(row);
        };
        let leased = ManuallyDrop::into_inner(leased);
        let association = ManuallyDrop::into_inner(association);
        let cancelled = unsafe {
            self.contexts
                .with_occupied_mut_input(row.context, leased, |context, leased| {
                    let Some(lifecycle) = context.lifecycle.as_mut() else {
                        return Err(leased);
                    };
                    lifecycle
                        .cancel_attachment(leased)
                        .map_err(|refused| refused.into_leased())
                })
        };
        match cancelled {
            Ok(Ok(_)) => Ok(association),
            Ok(Err(leased)) | Err((_, leased)) => {
                row.state = PairRowState::Initializing {
                    leased: ManuallyDrop::new(leased),
                    association: ManuallyDrop::new(association),
                };
                Err(row)
            }
        }
    }

    fn rollback_pair_refusal(
        &mut self,
        pair: PairHandle,
        reason: OwnerTableRefusal,
    ) -> RefusedAdmission<A, PairRow<A>> {
        let context = match self.pairs.get(pair) {
            Ok(row) => row.context,
            Err(_) => {
                self.enter_quarantined();
                return RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::TableQuarantined,
                };
            }
        };
        let taken = unsafe {
            self.pairs.with_occupied_mut(pair, |row| {
                replace(&mut row.state, PairRowState::Quarantined)
            })
        };
        let Ok(PairRowState::Initializing {
            leased,
            association,
        }) = taken
        else {
            self.enter_quarantined();
            return RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::TableQuarantined,
            };
        };
        let leased = ManuallyDrop::into_inner(leased);
        let association = ManuallyDrop::into_inner(association);
        let cancelled = unsafe {
            self.contexts
                .with_occupied_mut_input(context, leased, |owner, leased| {
                    let Some(lifecycle) = owner.lifecycle.as_mut() else {
                        return Err(leased);
                    };
                    lifecycle
                        .cancel_attachment(leased)
                        .map_err(|refused| refused.into_leased())
                })
        };
        match cancelled {
            Ok(Ok(_)) => {}
            Ok(Err(leased)) | Err((_, leased)) => {
                let _ = unsafe {
                    self.pairs.with_occupied_mut(pair, |row| {
                        row.state = PairRowState::Initializing {
                            leased: ManuallyDrop::new(leased),
                            association: ManuallyDrop::new(association),
                        };
                    })
                };
                self.enter_quarantined();
                return RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::TableQuarantined,
                };
            }
        }
        let terminal = unsafe { pair.assume_terminal() };
        let tombstone = match self.pairs.mark_tombstone(terminal) {
            Ok(tombstone) => tombstone,
            Err(_) => {
                self.enter_quarantined();
                return RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::Input(ManuallyDrop::new(association)),
                };
            }
        };
        let extracted = match self.pairs.extract(tombstone) {
            Ok(extracted) => extracted,
            Err(_) => {
                self.enter_quarantined();
                return RefusedAdmission {
                    reason: OwnerTableRefusal::InvariantLost,
                    custody: RefusedCustody::Input(ManuallyDrop::new(association)),
                };
            }
        };
        let (_row, pending) = extracted.into_parts();
        let ack = unsafe { pending.assume_payload_finalized() };
        if self.pairs.ack_finalized(ack).is_err() {
            self.enter_quarantined();
            return RefusedAdmission {
                reason: OwnerTableRefusal::InvariantLost,
                custody: RefusedCustody::Input(ManuallyDrop::new(association)),
            };
        }
        RefusedAdmission {
            reason,
            custody: RefusedCustody::Input(ManuallyDrop::new(association)),
        }
    }

    fn begin_work<K: TicketRowKind>(
        table: crate::control_owner_slots::SlotTableId,
        phase: OwnerPhase,
        run_high_water: &mut u64,
        active_runs: &mut u64,
        tickets: &mut ControlTickets<'_, OwnerRundown, E, K>,
        prepared: PreparedOwnerControl<K>,
    ) -> Result<DispatchWork<K>, RefusedWork<PreparedOwnerControl<K>>> {
        let prepared_verb = tickets.prepared_verb(&prepared.ticket);
        let phase_allows = phase == OwnerPhase::Open
            || (phase == OwnerPhase::Closing
                && prepared_verb.is_ok_and(|verb| {
                    matches!(
                        verb,
                        ControlVerb::Detach
                            | ControlVerb::Unmap
                            | ControlVerb::Unref
                            | ControlVerb::ContextDestroy
                    )
                }));
        if !phase_allows {
            return Err(RefusedWork {
                reason: OwnerTableRefusal::WrongPhase { found: phase },
                work: ManuallyDrop::new(prepared),
            });
        }
        let Some(raw) = run_high_water.checked_add(1) else {
            return Err(RefusedWork {
                reason: OwnerTableRefusal::ActiveRunExhausted,
                work: ManuallyDrop::new(prepared),
            });
        };
        let Some(run_id) = NonZeroU64::new(raw) else {
            return Err(RefusedWork {
                reason: OwnerTableRefusal::ActiveRunExhausted,
                work: ManuallyDrop::new(prepared),
            });
        };
        let Some(next_active) = active_runs.checked_add(1) else {
            return Err(RefusedWork {
                reason: OwnerTableRefusal::ActiveRunExhausted,
                work: ManuallyDrop::new(prepared),
            });
        };
        let PreparedOwnerControl { row, ticket } = prepared;
        match tickets.begin_dispatch(ticket) {
            Ok(permit) => {
                *run_high_water = raw;
                *active_runs = next_active;
                Ok(DispatchWork {
                    table,
                    epoch: row.epoch(),
                    row,
                    run_id,
                    permit,
                })
            }
            Err(refused) => Err(RefusedWork {
                reason: OwnerTableRefusal::TicketRefused,
                work: ManuallyDrop::new(PreparedOwnerControl {
                    row,
                    ticket: refused.into_action(),
                }),
            }),
        }
    }

    fn finish_work<K: TicketRowKind>(
        table: crate::control_owner_slots::SlotTableId,
        epoch: Option<TransportEpoch>,
        phase: OwnerPhase,
        active_runs: &mut u64,
        tickets: &mut ControlTickets<'_, OwnerRundown, E, K>,
        work: ObservedOwnerWork<E, K>,
    ) -> Result<OwnerLifecycleAction<K>, RefusedWork<ObservedOwnerWork<E, K>>> {
        if !matches!(
            phase,
            OwnerPhase::Open | OwnerPhase::Closing | OwnerPhase::Quarantined
        ) || work.table != table
            || Some(work.epoch) != epoch
            || *active_runs == 0
        {
            return Err(RefusedWork {
                reason: OwnerTableRefusal::WorkMismatch,
                work: ManuallyDrop::new(work),
            });
        }
        let ObservedOwnerWork {
            table,
            epoch,
            row,
            run_id,
            observed,
        } = work;
        match tickets.finish_observed(observed) {
            Ok(action) => {
                *active_runs -= 1;
                Ok(OwnerLifecycleAction { row, action })
            }
            Err(refused) => Err(RefusedWork {
                reason: OwnerTableRefusal::TicketRefused,
                work: ManuallyDrop::new(ObservedOwnerWork {
                    table,
                    epoch,
                    row,
                    run_id,
                    observed: refused.into_action(),
                }),
            }),
        }
    }

    fn mint_use(
        high_water: &mut u64,
        census: &mut u64,
        pair: PairHandle,
    ) -> Result<PairUseLease, OwnerTableRefusal> {
        let id = Self::mint_use_id(high_water, census)?;
        Ok(PairUseLease { pair, id })
    }

    fn mint_use_id(
        high_water: &mut u64,
        census: &mut u64,
    ) -> Result<NonZeroU64, OwnerTableRefusal> {
        let Some(raw) = high_water.checked_add(1) else {
            return Err(OwnerTableRefusal::PairUseExhausted);
        };
        let Some(id) = NonZeroU64::new(raw) else {
            return Err(OwnerTableRefusal::PairUseExhausted);
        };
        let Some(next) = census.checked_add(1) else {
            return Err(OwnerTableRefusal::PairUseExhausted);
        };
        *high_water = raw;
        *census = next;
        Ok(id)
    }

    fn return_use(
        high_water: u64,
        census: &mut u64,
        id: NonZeroU64,
        mismatch: OwnerTableRefusal,
    ) -> Result<(), OwnerTableRefusal> {
        if id.get() > high_water || *census == 0 {
            return Err(mismatch);
        }
        *census -= 1;
        Ok(())
    }

    fn windows_overlap(left: TransportWindow, right: TransportWindow) -> bool {
        let Some(left_end) = left.offset().checked_add(left.length()) else {
            return true;
        };
        let Some(right_end) = right.offset().checked_add(right.length()) else {
            return true;
        };
        left.offset() < right_end && right.offset() < left_end
    }
}
