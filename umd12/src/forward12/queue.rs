//! L2 — command queues, pools, recorders and command-list lifetime.
//!
//! Owns 17 of `DEVICE_FUNCS_CORE_0109` (group (d)) **and all 7** of
//! `COMMAND_QUEUE_FUNCS_CORE_0001`.
//!
//! ⚠ The three device-side queue entry points are members **27, 28 and 29** of
//! the 124 (`d3d12umddi.h:13488-13490`). ⛔ NOT "slots 38-40" — that was a `sed`
//! line offset inside the struct misread as a member index (`DECISIONS.md`
//! §4.1), and it is exactly the kind of number `noop12`'s offset proof now makes
//! unrepresentable.
//!
//! # ⭐⭐ The pool / recorder / list split, and the mapping this lane chose
//!
//! D3D12's **API** has `ID3D12CommandAllocator` + `ID3D12GraphicsCommandList`.
//! The **DDI** at `_0040` and later has three objects (`DDI_REFERENCE.md` §8.1),
//! and the create args are what decide the mapping:
//!
//! | DDI object | its create args carry | so it maps to |
//! |---|---|---|
//! | pool (`D3D12DDIARG_CREATE_COMMAND_POOL_0040`) | `PoolFlags` — **one enum with one value, `NONE`** | one lazily-created `ID3D12CommandAllocator` per list class |
//! | recorder (`D3D12DDIARG_CREATE_COMMAND_RECORDER_0040`) | `QueueFlags`, `RecorderFlags` | **no engine object at all** |
//! | list (`D3D12DDIARG_CREATE_COMMAND_LIST_0040`) | `Type` (DIRECT/BUNDLE), `QueueFlags`, `ID`, `CommandListFlags`, `NodeMask` | an `ID3D12GraphicsCommandList` |
//!
//! ⭐ **The pool cannot create its allocator at `pfnCreateCommandPool`**, and
//! that is the one non-obvious consequence of the shape above.
//! `ID3D12Device::CreateCommandAllocator` takes a `D3D12_COMMAND_LIST_TYPE`; the
//! pool's create args are a single flags word that carries no type, no size and
//! no pointer. The only DDI that ever brings a queue class into contact with a
//! pool with the authoritative list class is `pfnResetCommandList`: the recorder
//! names its current pool and the list being reset names DIRECT, BUNDLE, COMPUTE
//! or COPY. ⇒ **this lane creates that pool's allocator for the exact list class
//! lazily at first reset.** A pool can consequently serve different classes
//! without ever passing a mismatched allocator to the engine.
//!
//! ⚠ `DDI_REFERENCE.md` §9.3's mapping table writes this row as
//! *"`pfnCreateCommandPool` → `ID3D12Device::CreateCommandAllocator(type)`"* and
//! does not say where `type` comes from. It comes from the list being reset,
//! after its recorder names the pool; the per-class lazy creation is that gap
//! closed, not a deviation from the table.
//!
//! ⭐ **The recorder has no engine object behind it, and that is a legitimate
//! answer rather than a stub.** vkd3d fuses "the recording engine" into
//! `ID3D12GraphicsCommandList` itself, so a DDI recorder is exactly a
//! driver-side record of *which pool a subsequent `pfnResetCommandList` should
//! draw its allocator from* — which is why `D3D12DDIARG_RESETCOMMANDLIST_0040`
//! carries `hDrvCommandRecorder` and nothing else about memory. §9.3 says the
//! same in one line: *"`pfnCreateCommandRecorder` → no vkd3d object — a
//! Helios-side shadow naming its current pool."*
//!
//! ⭐ **A command list is created with `ID3D12Device4::CreateCommandList1`**, not
//! with `CreateCommandList` + `Close()`. `D3D12DDIARG_CREATE_COMMAND_LIST_0040`
//! names **no pool and no recorder** — those arrive at
//! `pfnResetCommandList` — so at create time there is no allocator to pass, and
//! `CreateCommandList1` is the D3D12 entry point that exists for exactly that:
//! it returns a **closed** list bound to no allocator. §9.3's *"then immediately
//! `Close()`"* describes the same end state reached the long way round, and the
//! long way round is not available here.
//!
//! ⭐ **Bundles use the same rule.** The recorder carries no bundle bit, but the
//! command list's `Type` does. At reset, that authoritative type selects the
//! pool's BUNDLE allocator slot. Time Spy also proved why the recorder cannot be
//! the class source: it binds a DIRECT-compatible recorder and later resets a
//! COMPUTE list through it.
//!
//! # Current direct submission contract
//!
//! Each command queue owns one HQA1-described virtual context. ExecuteCommandLists
//! forwards the ordinary engine command lists, then the landed A5/A7 path submits
//! one complete HOB1 plus the fixed 64-byte HOS1 descriptor synchronously through
//! pfnSubmitCommandCb. The exact outer-allocation token is resolved device-locally
//! before submission and K9 owns scheduler completion.
//!
//! There is no legacy pfnRenderCb marker, present-stream record, sampled raw
//! resource id, or named-fence side channel in this lane. Ordinary D3D12 Present
//! remains owned by the runtime and the separate VK_LAYER_HELIOS_present path.
use core::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};

use helios_umd_common::hr::{Hresult, E_FAIL, E_INVALIDARG, E_NOTIMPL, S_OK};
use helios_umd_common::refusals::RefusalCounter;
use helios_umd_common::slot::{Boxed, Com, DdiHandle, Slot};
use helios_umd_common::throttle::LogThrottle;
// ⚠ Imported for `Interface::cast` (the `QueryInterface` that reaches
// `ID3D12Device4`), which is a trait method and therefore invisible to method
// resolution unless the trait is in scope.
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12CommandSignature,
    ID3D12Device4, ID3D12GraphicsCommandList, D3D12_COMMAND_LIST_FLAG_NONE,
    D3D12_COMMAND_LIST_TYPE, D3D12_COMMAND_LIST_TYPE_BUNDLE, D3D12_COMMAND_LIST_TYPE_COMPUTE,
    D3D12_COMMAND_LIST_TYPE_COPY, D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC,
    D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_COMMAND_SIGNATURE_DESC, D3D12_INDIRECT_ARGUMENT_DESC,
    D3D12_INDIRECT_ARGUMENT_TYPE, D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH,
    D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH_MESH, D3D12_INDIRECT_ARGUMENT_TYPE_DRAW,
    D3D12_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED,
};

use super::fence;
use super::pso;
use super::tables12::{self, stage, CommandQueueTable, DeviceCoreTable, Filling};
use crate::{ddi12, device12, log_error, note_refusal, trace_line};

fn lock_ignore_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Handle payloads
// ---------------------------------------------------------------------------

// ⛔ **The payload is a property of the HANDLE TYPE, declared once, here.**
// `ARCHITECTURE.md` §12 rule 7 / R803: choosing the payload at the call site
// compiled and produced a `ManuallyDrop` whose vtable pointer was a struct field
// — a wild call on first use.
//
// ⚠ `D3D12DDI_HCOMMANDLIST` is declared **in this file** even though L3a
// (`cmdlist.rs`) owns the 23 recording slots that read it, because this lane owns
// the list's *lifetime* — `pfnCreateCommandList` is what puts a value in the slot
// and `pfnDestroyCommandList` is what takes it out. One declaration, in the lane
// that writes it.
//
// ⭐ **It was a bare `com_handles!` word until S6 Round 2, and what promoted it
// was not shadow state but an ERROR CHANNEL.** Every one of the 75 command-list
// slots takes `D3D12DDI_HCOMMANDLIST` and **nothing else** — no `hDevice`
// (`pfnDrawInstanced`, `d3d12umddi.h`), and 74 of the 75 return `VOID`. So a
// recording DDI that fails cannot report through its return value and cannot
// reach a callback from its arguments alone: it needs something the create-time
// handles carry. `pfnCreateCommandList` is the one DDI in the list's whole life
// that is handed both the device handle and the runtime's list handle, so it is
// the only place either can be captured. ⇒ [`CommandListState`], and the
// promotion is this lane's to make (`PARALLEL.md` §4 gives it the handle) on
// behalf of L3a/L3b/L3c/L8, which all four need it.
//
// ⛔ **The first version of this comment said the channel was `pfnSetErrorCb`,
// "which is device-scoped", and called it the only one. That was WRONG**, and
// three lanes copied the sentence into 49 call sites before the `PARALLEL.md`
// §10 review opened the header. `pfnSetCommandListErrorCb` sits one field below
// `pfnSetErrorCb` in the same `_0062` struct this file already reads
// `pfnSetCommandListDDITableCb` out of; it quarantines one list where the device
// callback removes the whole device. `device12::set_command_list_error` has the
// full account. That is why [`CommandListState`] carries `h_rt_list` as well as
// `h_device`.
helios_umd_common::boxed_handles!(
    crate::ddi12::D3D12DDI_HCOMMANDLIST => CommandListState,
    crate::ddi12::D3D12DDI_HCOMMANDQUEUE => QueueState,
    crate::ddi12::D3D12DDI_HCOMMANDPOOL_0040 => PoolState,
    crate::ddi12::D3D12DDI_HCOMMANDRECORDER_0040 => RecorderState,
);

// ⭐ **S-4: `D3D12DDI_HCOMMANDSIGNATURE` now HAS a payload, and it is one bare
// owning COM word.** Until `pfnCreateCommandSignature` was implemented this file
// deliberately declared none — the handle carried nothing, and a marker impl
// saying otherwise would have been a claim about an object that was never built.
// It is built now, so the declaration lands with it, in the lane that owns the
// handle (`PARALLEL.md` §4) and reaches `Slot::from_priv` through
// `DdiHandle::drv_private` rather than by reading `pDrvPrivate` at each site.
//
// ⚠ `com_handles!`, not `boxed_handles!`: an `ID3D12CommandSignature` needs no
// shadow state. Everything the DDI's create args carry is either forwarded into
// the engine object or refused at create, so there is nothing left for the driver
// to remember — which is the opposite of `D3D12DDI_HFENCE`, whose watermark is the
// whole reason that one is boxed.
helios_umd_common::com_handles!(crate::ddi12::D3D12DDI_HCOMMANDSIGNATURE,);

/// The private-block size every `CalcPrivate*` in this lane returns.
///
/// ⛔ **One machine word, and never 0.** Same rule and same reasoning as
/// `fence::PRIVATE_SLOT_SIZE`: `umd_common::slot`'s encoding is one word inside
/// driver-sized private memory, the shipping D3D11 driver answers 8 from every
/// real `CalcPrivate*Size` (`umd/src/forward/queries.rs:9-14`), and a 0 would
/// make the runtime hand the paired `Create` a zero-byte region to write through
/// — `umd/src/device_funcs.rs:708-723`, where a transient 0 produced heap
/// corruption surfacing as a wild call inside a 3DMark worker.
const PRIVATE_SLOT_SIZE: usize = core::mem::size_of::<*mut c_void>();

// ---------------------------------------------------------------------------
// Log budgets
// ---------------------------------------------------------------------------

// ⛔ **`log_error!` is unbounded by construction** — `umd_common/src/log.rs:279`
// formats and writes on **every** call — and T2 measured what one unbounded UMD
// log site costs: ~9k mutex-serialized writes per second from a single per-call
// line (`umd/src/device_funcs.rs:713-723`). Every site in this file that can
// repeat with the workload goes through one of these budgets: the first 8, then
// every 4096th, with the ordinal printed so a suppressed burst is still visible
// as a jump.
//
// ⚠ Grouped **per object family** rather than per call site. A burst is a
// property of the path that produces it (every queue create failing, every
// submit failing), not of which of that path's four lines fired, and one budget
// per family keeps the failure legible instead of interleaving four independent
// countdowns.

/// Budget for the queue-lifetime lines, including the `CreateContext` capture.
static QUEUE_LOG: LogThrottle = LogThrottle::new();
/// Budget for the command-pool lines.
static POOL_LOG: LogThrottle = LogThrottle::new();
/// Budget for the command-recorder lines.
static RECORDER_LOG: LogThrottle = LogThrottle::new();
/// Budget for the command-list lifetime lines.
static LIST_LOG: LogThrottle = LogThrottle::new();
/// Budget for the `pfnExecuteCommandLists` lines.
static ECL_LOG: LogThrottle = LogThrottle::new();
/// Budget for the `pfnSignalFence` / `pfnWaitForFence` lines.

/// The one budget shape this lane uses: the first 8, then every 4096th.
///
/// Returns the occurrence ordinal (0-based) when the line should be emitted.
fn budget(t: &LogThrottle) -> Option<usize> {
    t.first_n_then_every(8, 4096)
}

/// Sanity bound on `pfnExecuteCommandLists`' `Count`.
///
/// CLAUDE.md: *validate every runtime-supplied size before reading.* The DDI
/// declares the array `_In_reads_(Count)` and the runtime is the authority, so
/// this is not a semantic cap — no D3D12 rule limits how many lists one
/// `ExecuteCommandLists` may carry. It bounds the allocation a corrupt count
/// would demand, and its counter says if a real workload ever approached it.
const MAX_EXECUTE_COMMAND_LISTS: usize = 65_536;

/// Per-`ID3D12CommandQueue` shadow state (`DDI_REFERENCE.md` §9.1 row 1).
///
/// ⚠ **`pub`, not `pub(crate)`, and that is forced rather than chosen.**
/// `BoxedHandle` is a `pub` trait in `helios_umd_common`, so an associated type
/// less visible than the trait is E0446 (*crate-private type in public
/// interface*). It escapes nowhere: `forward12` and `queue` are both
/// `pub(crate) mod` inside a `cdylib` that exports no Rust API. Same shape as the
/// D3D11 side's `pub struct ResourceState` (`umd/src/forward/state.rs:24`), and
/// `umd_common/src/slot.rs:94-97`'s *"the associated type may be a type private
/// to the implementing crate"* is about module reachability, not about this
/// keyword. Every field below is private, so nothing about the layout escapes.
pub struct QueueState {
    /// Owning DDI device; the queue table has only a device-scoped error path.
    h_device: ddi12::D3D12DDI_HDEVICE,
    /// One record-only engine queue. Dropping the state releases its COM reference.
    engine_queue: ID3D12CommandQueue,
    /// Stable direct A5/runtime association installed before publication.
    outer: Arc<OuterQueueAssociation12>,
}
struct OuterRuntimeQueue12 {
    h_context: core::ptr::NonNull<c_void>,
    context_generation: u64,
    endpoint_id: u32,
    context_flags: u32,
    hqc1: core::num::NonZeroU32,
    hqc1_cpu: core::ptr::NonNull<u64>,
    next_progress: std::sync::atomic::AtomicU64,
    last_submitted_progress: std::sync::atomic::AtomicU64,
    last_batch_id: std::sync::atomic::AtomicU64,
    active_scope: Mutex<Option<helios_protocol::HeliosTranslatorScope>>,
    submit_lock: Mutex<()>,
}

/// One immutable queue/runtime association.  It is boxed before the engine
/// queue is created so every callback address is stable through direct rundown.
pub(crate) struct OuterQueueAssociation12 {
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_rt_queue: ddi12::D3D12DDI_HRTCOMMANDQUEUE,
    runtime: OnceLock<OuterRuntimeQueue12>,
    device_lost: std::sync::atomic::AtomicU32,
    teardown_lifetime: Mutex<OuterQueueTeardownLifetime12>,
    teardown_drained: Condvar,
}

struct OuterQueueTeardownLifetime12 {
    accepting: bool,
    active: u32,
}

impl OuterQueueAssociation12 {
    fn new(h_device: ddi12::D3D12DDI_HDEVICE, h_rt_queue: ddi12::D3D12DDI_HRTCOMMANDQUEUE) -> Self {
        Self {
            h_device,
            h_rt_queue,
            runtime: OnceLock::new(),
            device_lost: std::sync::atomic::AtomicU32::new(0),
            teardown_lifetime: Mutex::new(OuterQueueTeardownLifetime12 {
                accepting: true,
                active: 0,
            }),
            teardown_drained: Condvar::new(),
        }
    }

    fn acquire_teardown(&self) -> bool {
        let mut lifetime = lock_ignore_poison(&self.teardown_lifetime);
        if !lifetime.accepting || lifetime.active == u32::MAX {
            return false;
        }
        lifetime.active += 1;
        true
    }

    fn release_teardown(&self) {
        let mut lifetime = lock_ignore_poison(&self.teardown_lifetime);
        debug_assert!(lifetime.active != 0);
        if lifetime.active != 0 {
            lifetime.active -= 1;
        }
        if lifetime.active == 0 {
            self.teardown_drained.notify_all();
        }
    }

    fn close_and_wait(&self) {
        let mut lifetime = lock_ignore_poison(&self.teardown_lifetime);
        lifetime.accepting = false;
        while lifetime.active != 0 {
            lifetime = self
                .teardown_drained
                .wait(lifetime)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// Bounded device-owned set of live outer queues. Weak entries express only
/// queue lifetime; allocation identity remains the explicit generation/token.
pub(crate) struct OuterQueueRegistry12 {
    entries: Mutex<Vec<Weak<OuterQueueAssociation12>>>,
}

impl OuterQueueRegistry12 {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    fn register(&self, outer: &Arc<OuterQueueAssociation12>) -> bool {
        let mut entries = lock_ignore_poison(&self.entries);
        entries.retain(|entry| entry.strong_count() != 0);
        if entries.len() >= helios_protocol::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION as usize {
            return false;
        }
        entries.push(Arc::downgrade(outer));
        true
    }

    fn acquire(&self, preferred_context_generation: u64) -> Option<Arc<OuterQueueAssociation12>> {
        let mut entries = lock_ignore_poison(&self.entries);
        entries.retain(|entry| entry.strong_count() != 0);
        let live: Vec<_> = entries.iter().filter_map(Weak::upgrade).collect();
        drop(entries);
        if preferred_context_generation != 0 {
            if let Some(outer) = live.iter().find(|outer| {
                outer.runtime.get().is_some_and(|runtime| {
                    runtime.context_generation == preferred_context_generation
                })
            }) {
                if outer.acquire_teardown() {
                    return Some(Arc::clone(outer));
                }
            }
        }
        live.into_iter()
            .find(|outer| outer.runtime.get().is_some() && outer.acquire_teardown())
    }

    fn unregister(&self, outer: &Arc<OuterQueueAssociation12>) {
        let mut entries = lock_ignore_poison(&self.entries);
        entries.retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|candidate| !Arc::ptr_eq(&candidate, outer))
        });
    }
}

/// Engine allocators materialised for one DDI command pool, indexed by command
/// list class.
///
/// `D3D12DDIARG_CREATE_COMMAND_POOL_0040` carries no class, and the recorder's
/// `QueueFlags` describes queue compatibility rather than the class of the list
/// that will later name that recorder at `pfnResetCommandList`. Time Spy proves
/// those can differ: a COMPUTE list arrived through a recorder whose queue class
/// was DIRECT. The list itself is therefore the first authoritative class, so
/// each class is initialised independently on first reset.
struct PoolAllocators {
    direct: OnceLock<ID3D12CommandAllocator>,
    bundle: OnceLock<ID3D12CommandAllocator>,
    compute: OnceLock<ID3D12CommandAllocator>,
    copy: OnceLock<ID3D12CommandAllocator>,
}

impl PoolAllocators {
    fn new() -> Self {
        Self {
            direct: OnceLock::new(),
            bundle: OnceLock::new(),
            compute: OnceLock::new(),
            copy: OnceLock::new(),
        }
    }

    fn slot(
        &self,
        list_type: D3D12_COMMAND_LIST_TYPE,
    ) -> Option<&OnceLock<ID3D12CommandAllocator>> {
        match list_type {
            D3D12_COMMAND_LIST_TYPE_DIRECT => Some(&self.direct),
            D3D12_COMMAND_LIST_TYPE_BUNDLE => Some(&self.bundle),
            D3D12_COMMAND_LIST_TYPE_COMPUTE => Some(&self.compute),
            D3D12_COMMAND_LIST_TYPE_COPY => Some(&self.copy),
            _ => None,
        }
    }

    fn initialized(
        &self,
    ) -> impl Iterator<Item = (D3D12_COMMAND_LIST_TYPE, &ID3D12CommandAllocator)> {
        [
            (D3D12_COMMAND_LIST_TYPE_DIRECT, self.direct.get()),
            (D3D12_COMMAND_LIST_TYPE_BUNDLE, self.bundle.get()),
            (D3D12_COMMAND_LIST_TYPE_COMPUTE, self.compute.get()),
            (D3D12_COMMAND_LIST_TYPE_COPY, self.copy.get()),
        ]
        .into_iter()
        .filter_map(|(list_type, allocator)| allocator.map(|allocator| (list_type, allocator)))
    }
}

/// Per-command-pool shadow state.
///
/// The `Arc` lets a recorder retain the pool's allocator set without ever
/// dereferencing runtime-owned pool private memory after `pfnDestroyCommandPool`.
/// Each allocator slot is a `OnceLock`, so free-threaded first use has one winner
/// and releases the losing engine object.
pub struct PoolState {
    allocators: Arc<PoolAllocators>,
}

/// Per-command-list shadow state.
///
/// ⛔ **The first two fields are the reason this type exists.** See the
/// `boxed_handles!` comment above: a command-list DDI is handed the list handle
/// and nothing else, so a recording slot can only report a failure through
/// handles captured at create. `h_rt_list` is what
/// `device12::set_command_list_error` takes — the correct, list-scoped channel —
/// and `h_device` is how the device that owns the callback table is found.
/// `pfnCreateCommandList` is the only DDI in the list's life that is told
/// either.
///
/// ⚠ **Nothing else is shadowed, deliberately.** A forwarder is at its most
/// correct when it holds no state the engine also holds: the current PSO, the
/// bound descriptor heaps, the topology and the recording/closed flag all live
/// inside vkd3d's own `d3d12_command_list` and a second copy here could only
/// disagree with it. ⭐ The one obligation that *looked* like it needed shadow
/// state — `SUBSTRATE.md` §4.5's *"the `DYNAMIC_*` PSO flags do not relieve the
/// driver of applying the PSO's own depth-bias and IB-strip-cut on every
/// `pfnSetPipelineState`"* — is discharged by the engine; see
/// [`super::pso::L6Refusals::pso_dynamic_state_flag_forwarded`], which carries
/// the source citation.
///
/// ⚠ `pub` for the same E0446 reason as [`QueueState`]: it is named as the
/// associated type of the `pub` `BoxedHandle` trait. Both fields are private.
pub struct CommandListState {
    /// The device this list was created against — the only error channel a
    /// `VOID`-returning recording slot has.
    h_device: ddi12::D3D12DDI_HDEVICE,
    /// The engine list. **Owned** — dropping this state releases it.
    engine: ID3D12GraphicsCommandList,
    /// The **runtime's** handle for this list.
    ///
    /// ⭐ Stored for `pfnSetCommandListErrorCb`, which is what a recording slot
    /// must use instead of the device-scoped `pfnSetErrorCb` — see
    /// `device12::set_command_list_error`, whose doc records that the spine
    /// originally claimed no such channel existed. The callback takes this
    /// handle and nothing else identifies the list to the runtime, so like
    /// `h_device` it can only be captured here, at `pfnCreateCommandList`.
    h_rt_list: ddi12::D3D12DDI_HRTCOMMANDLIST,

    /// The class this list was created as, from `Type` + `QueueFlags`.
    ///
    /// ⭐ Kept so `pfnResetCommandList` can select the bound pool's allocator for
    /// the exact list class. The recorder's queue compatibility flags are not
    /// authoritative: Time Spy sends a COMPUTE list through a DIRECT-compatible
    /// recorder, and only this field preserves the class the engine requires.
    list_type: D3D12_COMMAND_LIST_TYPE,
}

impl CommandListState {
    /// The engine command list, borrowed for the caller's DDI call.
    ///
    /// ⚠ A shared reference rather than a `ManuallyDrop<ID3D12GraphicsCommandList>`:
    /// the box keeps the owning reference, and borrowing it as `&` makes
    /// releasing it *unwritable* where a `ManuallyDrop` merely makes it
    /// unlikely. Same choice, and the same reasoning, as
    /// `resource12::engine_resource`.
    pub(crate) fn engine(&self) -> &ID3D12GraphicsCommandList {
        &self.engine
    }

    /// The device handle. ⚠ Used to *find* the corelayer callback table, not to
    /// scope the error: a command-list failure is reported through
    /// `device12::set_command_list_error` with [`Self::h_rt_list`], never through
    /// the device-scoped `device12::set_error`.
    pub(crate) fn h_device(&self) -> ddi12::D3D12DDI_HDEVICE {
        self.h_device
    }

    /// The class this list was created as. See the field doc — its only reader is
    /// `pfnResetCommandList`'s allocator-class check.
    pub(crate) fn list_type(&self) -> D3D12_COMMAND_LIST_TYPE {
        self.list_type
    }

    /// The runtime's handle for this list, for `pfnSetCommandListErrorCb`.
    pub(crate) fn h_rt_list(&self) -> ddi12::D3D12DDI_HRTCOMMANDLIST {
        self.h_rt_list
    }
}

/// What a recorder is currently pointed at: the pool's identity, and an **owned**
/// reference to that pool's per-class allocator set.
///
/// ⛔ **The owned reference is the whole point, and it replaces a raw
/// `AtomicPtr<c_void>` that could not be made safe.** Until S6 Round 2 this lane
/// stored only the pool's `pDrvPrivate` and never dereferenced it. Re-deriving
/// `PoolState` from that identity at reset would read memory **the runtime owns
/// and frees at `pfnDestroyCommandPool`**. The `Arc` makes that freed-pool read
/// unrepresentable while still deferring allocator-class selection until the
/// command list supplies the authoritative class.
///
/// ⚠ `pool` is a `usize`, not a pointer: it is **identity only**, for the trace
/// lines and the rebind check, and is never dereferenced. Storing it as an
/// integer says so in the type.
struct RecorderTarget {
    pool: usize,
    allocators: Arc<PoolAllocators>,
}

/// Per-command-recorder shadow state. There is no engine object — see the module
/// doc.
pub struct RecorderState {
    /// The queue compatibility class from
    /// `D3D12DDIARG_CREATE_COMMAND_RECORDER_0040::QueueFlags`.
    ///
    /// ⛔ This is diagnostic state, not an allocator class. Time Spy supplies a
    /// DIRECT-compatible recorder for a COMPUTE list, proving that choosing an
    /// allocator from this value is wrong. `pfnResetCommandList` supplies the
    /// list class that selects the allocator.
    queue_type: D3D12_COMMAND_LIST_TYPE,
    /// The pool this recorder last targeted, and that pool's allocator.
    ///
    /// ⚠ A `Mutex` rather than the `OnceLock`+atomic pair the pool uses, because
    /// unlike a pool's allocator this is **rebindable**: the runtime may point a
    /// recorder at a different pool at any time, so there is no
    /// initialise-once shape to exploit. The critical section is one `Option`
    /// swap and one `AddRef`, on a path the runtime drives once per
    /// `ID3D12GraphicsCommandList::Reset`.
    ///
    /// ⛔ A poisoned lock is treated as a live one (`unwrap_or_else(|e|
    /// e.into_inner())`): this crate is `panic = "abort"`, so no lock in it can
    /// actually be poisoned, and `.unwrap()` on runtime data is forbidden
    /// (`PARALLEL.md` §9.3). The recovery arm is what keeps the forbidden call
    /// out of the file rather than a claim that it could not fire.
    target: Mutex<Option<RecorderTarget>>,
}

// ---------------------------------------------------------------------------
// Slot accessors
// ---------------------------------------------------------------------------

// ⭐ One named accessor per handle type, never a generic `state::<S>(h)`: a
// generic form would put the payload type back at the call site, which is the
// R803 shape the `BoxedHandle` marker exists to remove.
//
// ⛔⛔ **THE D3D12 SOUNDNESS ARGUMENT, RE-DERIVED — it is NOT inherited from
// D3D11.** `umd_common/src/slot.rs:304-322` states plainly that
// `Slot<Boxed<S>>::get() -> &'static S` rests on the D3D11 runtime's
// `CUseCountedObject` first-created/last-destroyed ordering, that *"`d3d12umddi`
// has no THREADING cap and no `CUseCountedObject` statement has been located for
// it"*, and that whoever first calls it from `umd12` *"owes the equivalent
// derivation … or must reach for `Slot::ptr()` and carry the lifetime
// themselves."* `PARALLEL.md` §9.4 repeats it.
//
// **This lane takes the second door.** None of the accessors below calls `get()`.
// Each takes `Slot::ptr()` and hands back a reference whose lifetime is the
// caller's own binding — one DDI invocation — exactly as `device12::device` does
// for the device handle. The argument is then narrower than D3D11's and does not
// depend on `CUseCountedObject`:
//
//   * the returned reference is **not** `'static`;
//   * the only path that drops a box is that object's `pfnDestroy*`, and these
//     are COM objects whose Destroy DDI the runtime issues when the
//     application's last `Release` retires the runtime-side object. An
//     application that releases an `ID3D12CommandQueue` while another of its
//     threads is calling `ExecuteCommandLists` on it has already destroyed the
//     *runtime's* object under that call; no driver can defend against it and
//     none is expected to;
//   * ⚠ concurrent **reads** across free-threaded workers are permitted by `&`
//     and are the expected case. The two fields that change after construction
//     are the `OnceLock` slots inside `PoolState::allocators`, each initialised
//     once for one list class, and `RecorderState::target`, a
//     `Mutex<Option<RecorderTarget>>` because a recorder's target is
//     **rebindable** and so has no initialise-once shape to exploit. There is
//     deliberately no `&mut` accessor.
//
//     ⛔ The `Mutex` carries one argument the atomic it replaced could not:
//     `recorder_allocator` hands back an **owned** `ID3D12CommandAllocator`
//     clone, taken under the lock, which is what makes a concurrent
//     `pfnCommandRecorderSetCommandPoolAsTarget` unable to release the allocator
//     a `pfnResetCommandList` is about to reset against. ⚠ Every holder —
//     `bind_target`, `unbind_target`, `target_pool_identity`,
//     `recorder_allocator` — releases the guard before returning, so no lock is
//     ever held across a call back into the runtime or into the engine.
//
//     ⚠ The premise the compiler never checks, stated because it is a premise:
//     `RecorderTarget` holds an `Arc` whose allocator slots contain windows-rs
//     COM interfaces, which carry no
//     `unsafe impl Send`/`Sync`. No auto-trait obligation is ever raised, because
//     every `&RecorderState` in this file is conjured from `Slot::ptr()` inside
//     `unsafe` rather than obtained from a `Sync` container — so the sharing
//     rests on D3D12's free-threading contract, not on a bound Rust enforced.

/// The queue state behind a DDI queue handle, borrowed for the caller's DDI call.
///
/// # Safety
/// `h` must be a handle [`create_command_queue`] returned `S_OK` for and
/// [`destroy_command_queue`] has not been called on, and the returned reference
/// must not outlive the DDI call that obtained it.
unsafe fn queue_state<'a>(h: ddi12::D3D12DDI_HCOMMANDQUEUE) -> Option<&'a QueueState> {
    // SAFETY: the caller guarantees a live handle, so its slot lies inside the
    // private block `calc_private_command_queue_size` sized.
    let slot = unsafe { Slot::<Boxed<QueueState>>::from_priv(h.drv_private()) }?;
    // SAFETY: same precondition; `ptr` reads the word and reports an empty slot
    // as null rather than fabricating a reference.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null per the check, and the box it points at was written by
    // `create_command_queue` and is dropped only by `destroy_command_queue`. See
    // the re-derived argument above for why no borrow can overlap that drop.
    Some(unsafe { &*p })
}

/// Return the exact virtual WDDM context owned by this queue.
///
/// # Safety
/// `h` must be a live queue handle created by this driver.
pub(crate) unsafe fn present_context(h: ddi12::D3D12DDI_HCOMMANDQUEUE) -> Option<*mut c_void> {
    let queue = unsafe { queue_state(h) }?;
    Some(queue.outer.runtime.get()?.h_context.as_ptr())
}
/// The pool state behind a DDI pool handle. Same argument as [`queue_state`].
///
/// # Safety
/// As [`queue_state`], for a handle [`create_command_pool`] returned `S_OK` for.
unsafe fn pool_state<'a>(h: ddi12::D3D12DDI_HCOMMANDPOOL_0040) -> Option<&'a PoolState> {
    // SAFETY: as `queue_state`.
    let slot = unsafe { Slot::<Boxed<PoolState>>::from_priv(h.drv_private()) }?;
    // SAFETY: as `queue_state`.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: as `queue_state`.
    Some(unsafe { &*p })
}

/// The recorder state behind a DDI recorder handle. Same argument as
/// [`queue_state`].
///
/// # Safety
/// As [`queue_state`], for a handle [`create_command_recorder`] returned `S_OK`
/// for.
unsafe fn recorder_state<'a>(
    h: ddi12::D3D12DDI_HCOMMANDRECORDER_0040,
) -> Option<&'a RecorderState> {
    // SAFETY: as `queue_state`.
    let slot = unsafe { Slot::<Boxed<RecorderState>>::from_priv(h.drv_private()) }?;
    // SAFETY: as `queue_state`.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: as `queue_state`.
    Some(unsafe { &*p })
}

/// The command-list state behind a DDI command-list handle. Same argument as
/// [`queue_state`].
///
/// ⭐ **`pub(crate)`, and it is the seam the whole command-list table stands
/// on.** L3a (`cmdlist.rs`), L3b (`rootargs.rs`), L3c (`copy.rs`) and L8
/// (`present12.rs`) between them own 72 of the 75 command-list slots, every one
/// of which starts by turning this handle into an engine list and a device to
/// report against. R803's scar is that the payload must be derived from the
/// handle **type** in one place rather than decoded at each call site; this lane
/// owns the handle, so this is that one place — the same shape
/// `resource12::engine_resource` takes for `D3D12DDI_HRESOURCE`.
///
/// # Safety
/// As [`queue_state`], for a handle [`create_command_list`] returned `S_OK` for.
pub(crate) unsafe fn command_list_state<'a>(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
) -> Option<&'a CommandListState> {
    // SAFETY: as `queue_state`.
    let slot = unsafe { Slot::<Boxed<CommandListState>>::from_priv(h.drv_private()) }?;
    // SAFETY: as `queue_state`.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: as `queue_state`.
    Some(unsafe { &*p })
}

/// Resolve one Core-0114 application handle through the runtime-owned bypass
/// header to the exact driver-private object. This is the sole conversion seam
/// for every signature changed by `COMMAND_LIST_FUNCS_3D_0114`.
///
/// A null application object is preserved only when `allow_null` is true. A
/// non-null application handle must name an aligned runtime-bypass header with
/// both the runtime vtable and driver object present; malformed headers are a
/// named refusal and are never treated as driver-private storage directly.
///
/// # Safety
/// A non-null `private` must point at a live `D3D12DDI_RUNTIME_BYPASS_HEADER`
/// supplied by the D3D12 runtime for the duration of this DDI call.
unsafe fn bypass_driver_object(
    private: *mut core::ffi::c_void,
    allow_null: bool,
) -> Option<*mut core::ffi::c_void> {
    if private.is_null() {
        if allow_null {
            return Some(core::ptr::null_mut());
        }
        note_refusal(&L2_REFUSALS.runtime_bypass_handle_invalid);
        return None;
    }
    if !(private as usize)
        .is_multiple_of(core::mem::align_of::<ddi12::D3D12DDI_RUNTIME_BYPASS_HEADER>())
    {
        note_refusal(&L2_REFUSALS.runtime_bypass_handle_invalid);
        return None;
    }
    // SAFETY: the caller supplies a live runtime header and alignment was
    // checked above; the borrow ends before this DDI invocation returns.
    let header = unsafe { &*private.cast::<ddi12::D3D12DDI_RUNTIME_BYPASS_HEADER>() };
    if header.pInterfaceVtable.is_null() || header.pDriverObject.is_null() {
        note_refusal(&L2_REFUSALS.runtime_bypass_handle_invalid);
        return None;
    }
    Some(header.pDriverObject)
}

/// Convert the required Core-0114 command-list application handle.
///
/// # Safety
/// As [`bypass_driver_object`].
pub(crate) unsafe fn command_list_from_api(
    h: ddi12::D3D12DDI_API_HCOMMANDLIST,
) -> Option<ddi12::D3D12DDI_HCOMMANDLIST> {
    Some(ddi12::D3D12DDI_HCOMMANDLIST {
        // SAFETY: forwarded from this function's contract.
        pDrvPrivate: unsafe { bypass_driver_object(h.pDrvPrivate, false) }?,
    })
}

/// Convert a Core-0114 pipeline-state application handle.
///
/// # Safety
/// As [`bypass_driver_object`].
pub(crate) unsafe fn pipeline_state_from_api(
    h: ddi12::D3D12DDI_API_HPIPELINESTATE,
) -> Option<ddi12::D3D12DDI_HPIPELINESTATE> {
    Some(ddi12::D3D12DDI_HPIPELINESTATE {
        // A null PSO is preserved for the existing typed handler to reject.
        pDrvPrivate: unsafe { bypass_driver_object(h.pDrvPrivate, true) }?,
    })
}

/// Convert a Core-0114 root-signature application handle.
///
/// # Safety
/// As [`bypass_driver_object`].
pub(crate) unsafe fn root_signature_from_api(
    h: ddi12::D3D12DDI_API_HROOTSIGNATURE,
) -> Option<ddi12::D3D12DDI_HROOTSIGNATURE> {
    Some(ddi12::D3D12DDI_HROOTSIGNATURE {
        // Null means the API requested an unbound root signature.
        pDrvPrivate: unsafe { bypass_driver_object(h.pDrvPrivate, true) }?,
    })
}

/// Convert a Core-0114 state-object application handle.
///
/// # Safety
/// As [`bypass_driver_object`].
pub(crate) unsafe fn state_object_from_api(
    h: ddi12::D3D12DDI_API_HSTATEOBJECT,
) -> Option<ddi12::D3D12DDI_HSTATEOBJECT_0054> {
    Some(ddi12::D3D12DDI_HSTATEOBJECT_0054 {
        pDrvPrivate: unsafe { bypass_driver_object(h.pDrvPrivate, true) }?,
    })
}

// ---------------------------------------------------------------------------
// Queue-class translation
// ---------------------------------------------------------------------------

/// Translate `D3D12DDI_COMMAND_QUEUE_FLAGS` into the engine list class.
///
/// ⛔ **Translated, never forwarded.** `DDI_REFERENCE.md` §9.6.1 is the scar: the
/// descriptor-heap DDI and API flag enums collide on `0x1` with *different
/// meanings*, the member types make the compiler blind to it, and the result was
/// the wrong object with no error. These two enums are unrelated by construction
/// (`D3D12DDI_COMMAND_QUEUE_FLAGS` is a bitmask, `D3D12_COMMAND_LIST_TYPE` is an
/// ordinal), so the translation is mandatory rather than merely prudent.
///
/// ⚠ `_PAGING` and the three video classes are refused rather than folded into
/// DIRECT. `DECISIONS.md` D5 maps every queue class onto WDDM **NodeOrdinal 0**
/// because Helios advertises one engine node — that is a scheduling decision and
/// costs parallelism, not correctness. Backing a video queue with a 3D
/// `ID3D12CommandAllocator` would be a different thing entirely: an answer the
/// engine cannot honour, offered by a driver whose caps report no video support.
fn engine_list_type(
    queue_flags: ddi12::D3D12DDI_COMMAND_QUEUE_FLAGS,
) -> Option<D3D12_COMMAND_LIST_TYPE> {
    use ddi12::{
        D3D12DDI_COMMAND_QUEUE_FLAGS_D3D12DDI_COMMAND_QUEUE_FLAG_3D as DDI_3D,
        D3D12DDI_COMMAND_QUEUE_FLAGS_D3D12DDI_COMMAND_QUEUE_FLAG_COMPUTE as DDI_COMPUTE,
        D3D12DDI_COMMAND_QUEUE_FLAGS_D3D12DDI_COMMAND_QUEUE_FLAG_COPY as DDI_COPY,
    };
    // ⚠ Matched in `3D`-before-`COMPUTE`-before-`COPY` order because the field is
    // a bitmask: a queue that claims both `3D` and `COPY` is a DIRECT queue, and
    // a DIRECT `ID3D12CommandAllocator` can back copy work while the reverse is
    // false. Widening beats narrowing when the answer must be a single ordinal.
    if queue_flags & DDI_3D != 0 {
        Some(D3D12_COMMAND_LIST_TYPE_DIRECT)
    } else if queue_flags & DDI_COMPUTE != 0 {
        Some(D3D12_COMMAND_LIST_TYPE_COMPUTE)
    } else if queue_flags & DDI_COPY != 0 {
        Some(D3D12_COMMAND_LIST_TYPE_COPY)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// (d) Command queues — 3 slots
// ---------------------------------------------------------------------------

/// `pfnCalcPrivateCommandQueueSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live
/// `D3D12DDIARG_CREATECOMMANDQUEUE_0050` for the duration of the call.
unsafe extern "C" fn calc_private_command_queue_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATECOMMANDQUEUE_0050,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L2_REFUSALS.queue_bad_arg);
    }
    // ⛔ Answered unconditionally — see `PRIVATE_SLOT_SIZE`.
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// `pfnCreateCommandQueue` — one `ID3D12CommandQueue` **plus** one WDDM context
/// (`DDI_REFERENCE.md` §9.2). Read the module doc before changing the context
/// half.
///
/// # Safety
/// `h_device` must be a live handle from `device12::create_device`; `arg` must
/// point at a live `D3D12DDIARG_CREATECOMMANDQUEUE_0050`; `h_queue`'s
/// `pDrvPrivate` must address the private block
/// [`calc_private_command_queue_size`] sized; `h_rt_queue` must be the runtime's
/// handle for this queue.
unsafe extern "C" fn create_command_queue(
    h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATECOMMANDQUEUE_0050,
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    h_rt_queue: ddi12::D3D12DDI_HRTCOMMANDQUEUE,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) = (unsafe { Slot::<Boxed<QueueState>>::from_priv(h_queue.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.queue_bad_arg);
        return E_INVALIDARG;
    };
    // ⛔ Clear first, so every refusal below leaves a null slot rather than
    // whatever the runtime's allocator left there.
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L2_REFUSALS.queue_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*arg };

    let Some(list_type) = engine_list_type(a.QueueFlags) else {
        note_refusal(&L2_REFUSALS.queue_class_unsupported);
        if let Some(n) = budget(&QUEUE_LOG) {
            log_error!(
                "CreateCommandQueue: QueueFlags={:#x} names no 3D/COMPUTE/COPY class this driver \
                 backs -> E_INVALIDARG (x{})",
                a.QueueFlags,
                n + 1,
            );
        }
        return E_INVALIDARG;
    };

    // ⚠ Three inputs accepted-and-counted rather than refused. None of them can
    // change what this driver builds, and refusing on any of them would fail a
    // queue create over a hint:
    //   * GLOBAL_REALTIME_PRIORITY is a scheduling request against a
    //     software-scheduled adapter that has one engine node;
    //   * a scheduling group is L9's `pfnCreateSchedulingGroup`, still a counting
    //     noop, so there is no group object to join;
    //   * `NodeMask` beyond the single node Helios advertises is the
    //     `ARCHITECTURE.md` §13 UNVERIFIED-11 multi-adapter surface.
    if a.QueueCreationFlags
        != ddi12::D3D12DDI_COMMAND_QUEUE_CREATION_FLAGS_D3D12DDI_COMMAND_QUEUE_CREATION_FLAG_NONE
    {
        note_refusal(&L2_REFUSALS.queue_creation_flags_ignored);
    }
    if !a.SchedulingGroup.pDrvPrivate.is_null() {
        note_refusal(&L2_REFUSALS.queue_scheduling_group_ignored);
    }
    if a.NodeMask > 1 {
        note_refusal(&L2_REFUSALS.queue_node_mask_ignored);
    }

    // SAFETY: this is a device-scope DDI, so the runtime passes a handle
    // `create_device` returned `S_OK` for; the borrow lives only until the end of
    // this call, which is `device12::device`'s stated precondition.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L2_REFUSALS.queue_no_device);
        return E_FAIL;
    };
    // ⚠ `Priority: 0` is `D3D12_COMMAND_QUEUE_PRIORITY_NORMAL`, and it is the
    // only honest answer: the DDI carries no priority (`D3D12DDIARG_CREATECOMMANDQUEUE_0050`
    // has `QueueCreationFlags`, not a priority), and the runtime keeps priority
    // for itself unless a driver answers
    // `D3D12DDICAPS_TYPE_0023_UMD_BASED_COMMAND_QUEUE_PRIORITY`, which this
    // driver does not.
    let desc = D3D12_COMMAND_QUEUE_DESC {
        Type: list_type,
        Priority: 0,
        Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
        NodeMask: a.NodeMask,
    };
    let outer = Arc::new(OuterQueueAssociation12::new(h_device, h_rt_queue));
    let outer_cookie = Arc::as_ptr(&outer) as usize;
    // SAFETY: the descriptor and stable callback object remain live for the
    // synchronous call.  The bridge installs all callbacks before returning the
    // owned queue, so a generic record-only queue is never observable.
    let created = unsafe {
        dev.engine.create_associated_queue(
            core::ptr::from_ref(&desc) as usize,
            outer_cookie,
            outer_submit_begin as *const () as usize,
            outer_submit_finish as *const () as usize,
            outer_submit_join as *const () as usize,
        )
    };
    let (engine_queue, vk_family, vk_index) = match created {
        Some(queue) => queue,
        None => {
            note_refusal(&L2_REFUSALS.queue_engine_failed);
            if let Some(n) = budget(&QUEUE_LOG) {
                log_error!(
                    "CreateCommandQueue: record-only associated engine queue(type={}) failed \
                     (x{})",
                    list_type.0,
                    n + 1,
                );
            }
            return E_FAIL;
        }
    };

    // HQA1 is complete before this one runtime context callback.  The endpoint
    // is the exact lower queue the associated vkd3d object selected.
    let runtime = match unsafe {
        create_outer_virtual_context(
            dev,
            h_rt_queue,
            outer_cookie,
            vk_family,
            vk_index,
            engine_class_for_list_type(list_type),
        )
    } {
        Ok(runtime) => runtime,
        Err(hr) => {
            drop(engine_queue);
            return hr;
        }
    };
    if outer.runtime.set(runtime).is_err() {
        drop(engine_queue);
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return E_FAIL;
    }
    if !dev.outer_queues.register(&outer) {
        unsafe { destroy_outer_queue_parts(dev, outer.as_ref()) };
        drop(engine_queue);
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return E_FAIL;
    }

    // SAFETY: the slot lies in the sized private block and is currently null
    // (cleared above); `store` boxes the state and moves the box into it, so the
    // slot owns both the box and, through it, the engine queue's reference.
    unsafe {
        slot.store(QueueState {
            h_device,
            engine_queue,
            outer,
        });
    }
    S_OK
}

fn engine_class_for_list_type(list_type: D3D12_COMMAND_LIST_TYPE) -> u32 {
    match list_type {
        D3D12_COMMAND_LIST_TYPE_COMPUTE => helios_protocol::HELIOS_ENGINE_CLASS_COMPUTE,
        D3D12_COMMAND_LIST_TYPE_COPY => helios_protocol::HELIOS_ENGINE_CLASS_COPY,
        _ => helios_protocol::HELIOS_ENGINE_CLASS_GRAPHICS,
    }
}

fn direct_status(
    error: helios_umd_common::direct_translator::DirectTranslatorError,
) -> helios_protocol::HeliosTranslatorStatus {
    match error {
        helios_umd_common::direct_translator::DirectTranslatorError::Refused(status) => status,
        _ => helios_protocol::HeliosTranslatorStatus::HostCallbackFailed,
    }
}

const VK_SUCCESS: i32 = 0;
const VK_ERROR_DEVICE_LOST: i32 = -4;
const S_FALSE_OUTER: i32 = 1;
const E_PENDING_OUTER: i32 = 0x8000_000au32 as i32;
const WAIT_OBJECT_0_OUTER: u32 = 0;
const INFINITE_OUTER: u32 = u32::MAX;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        security_attributes: *mut c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

unsafe fn outer_from_cookie<'a>(cookie: *mut c_void) -> Option<&'a OuterQueueAssociation12> {
    if cookie.is_null() {
        None
    } else {
        Some(unsafe { &*cookie.cast::<OuterQueueAssociation12>() })
    }
}

fn mark_outer_lost(outer: &OuterQueueAssociation12, site: &str) {
    if outer
        .device_lost
        .swap(1, std::sync::atomic::Ordering::AcqRel)
        == 0
    {
        log_error!("A7 D3D12 outer queue lost at {site}");
    }
}

fn outer_progress_result(
    outer: &OuterQueueAssociation12,
    runtime: &OuterRuntimeQueue12,
) -> helios_protocol::HeliosSyncProgressResultV1 {
    let last = runtime
        .last_submitted_progress
        .load(std::sync::atomic::Ordering::Acquire);
    let completed = (unsafe { runtime.hqc1_cpu.as_ptr().read_volatile() }).min(last);
    helios_protocol::HeliosSyncProgressResultV1 {
        struct_bytes: helios_protocol::HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES,
        abi_version: helios_protocol::HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
        completed_progress_value: completed,
        last_submitted_progress_value: last,
        flags: if outer.device_lost.load(std::sync::atomic::Ordering::Acquire) != 0 {
            helios_protocol::HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST
        } else {
            0
        },
        reserved: 0,
    }
}

unsafe fn signal_outer_hqc1(
    dev: &device12::HeliosD3D12Device,
    outer: &OuterQueueAssociation12,
    runtime: &OuterRuntimeQueue12,
) -> Result<u64, helios_protocol::HeliosTranslatorStatus> {
    if outer.device_lost.load(std::sync::atomic::Ordering::Acquire) != 0
        || dev.kt_callbacks.is_null()
    {
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    let progress = runtime
        .next_progress
        .load(std::sync::atomic::Ordering::Acquire);
    if progress == 0 || progress == u64::MAX {
        mark_outer_lost(outer, "HQC1 progress overflow");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    let Some(signal) = (unsafe { (*dev.kt_callbacks).pfnSignalSynchronizationObjectFromGpuCb })
    else {
        mark_outer_lost(outer, "missing FromGpu callback");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    };
    let sync = runtime.hqc1.get();
    let mut arg = ddi12::D3DDDICB_SIGNALSYNCHRONIZATIONOBJECTFROMGPU::default();
    arg.hContext = runtime.h_context.as_ptr();
    arg.ObjectCount = 1;
    arg.ObjectHandleArray = &sync;
    arg.__bindgen_anon_1.MonitoredFenceValueArray = &progress;
    let hr = unsafe { signal(dev.h_rt_device.handle, &arg) };
    if hr < 0 {
        mark_outer_lost(outer, "HQC1 FromGpu signal");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    runtime
        .last_submitted_progress
        .store(progress, std::sync::atomic::Ordering::Release);
    runtime
        .next_progress
        .store(progress + 1, std::sync::atomic::Ordering::Release);
    Ok(progress)
}

unsafe fn wait_outer_hqc1(
    dev: &device12::HeliosD3D12Device,
    outer: &OuterQueueAssociation12,
    runtime: &OuterRuntimeQueue12,
    required: u64,
) -> Result<(), helios_protocol::HeliosTranslatorStatus> {
    let last = runtime
        .last_submitted_progress
        .load(std::sync::atomic::Ordering::Acquire);
    if required == 0 || required > last || dev.kt_callbacks.is_null() {
        return Err(helios_protocol::HeliosTranslatorStatus::HostCallbackFailed);
    }
    if unsafe { runtime.hqc1_cpu.as_ptr().read_volatile() } >= required {
        return Ok(());
    }
    let event = unsafe { CreateEventW(core::ptr::null_mut(), 0, 0, core::ptr::null()) };
    if event.is_null() {
        mark_outer_lost(outer, "HQC1 event create");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    let Some(wait) = (unsafe { (*dev.kt_callbacks).pfnWaitForSynchronizationObjectFromCpuCb })
    else {
        unsafe { CloseHandle(event) };
        mark_outer_lost(outer, "missing FromCpu callback");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    };
    let sync = runtime.hqc1.get();
    let mut arg = ddi12::D3DDDICB_WAITFORSYNCHRONIZATIONOBJECTFROMCPU::default();
    arg.ObjectCount = 1;
    arg.ObjectHandleArray = &sync;
    arg.FenceValueArray = &required;
    arg.hAsyncEvent = event;
    let hr = unsafe { wait(dev.h_rt_device.handle, &arg) };
    if hr != S_OK && hr != E_PENDING_OUTER {
        unsafe { CloseHandle(event) };
        mark_outer_lost(outer, "HQC1 FromCpu callback");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    let waited = unsafe { WaitForSingleObject(event, INFINITE_OUTER) };
    unsafe { CloseHandle(event) };
    if waited != WAIT_OBJECT_0_OUTER
        || unsafe { runtime.hqc1_cpu.as_ptr().read_volatile() } < required
    {
        mark_outer_lost(outer, "HQC1 event completion");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    Ok(())
}

unsafe fn wait_pool_owner(queue_context: usize, retire_value: u64) -> bool {
    if queue_context == 0 || retire_value == 0 {
        return false;
    }
    let outer = unsafe { &*(queue_context as *const OuterQueueAssociation12) };
    let Some(runtime) = outer.runtime.get() else {
        return false;
    };
    let Some(dev) = (unsafe { device12::device(outer.h_device) }) else {
        return false;
    };
    unsafe { wait_outer_hqc1(dev, outer, runtime, retire_value) }.is_ok()
}

/// Seal, resolve, publish, submit and close one exact scope.  The caller holds
/// this context's `submit_lock`; no allocation lock survives a runtime call.
unsafe fn submit_outer_scope12(
    dev: &device12::HeliosD3D12Device,
    outer: &OuterQueueAssociation12,
    runtime: &OuterRuntimeQueue12,
    scope: helios_protocol::HeliosTranslatorScope,
    terminal_token: Option<u64>,
) -> Result<Option<u64>, helios_protocol::HeliosTranslatorStatus> {
    use helios_umd_common::direct_translator::{
        DirectHobContext, DirectIdentityRefusal, DirectResolvedUse, DirectTranslatorError,
    };

    let batch = match dev.translator.seal_and_copy(scope) {
        Ok(batch) => batch,
        Err(DirectTranslatorError::Refused(
            helios_protocol::HeliosTranslatorStatus::ScopeEmpty,
        )) => {
            dev.translator
                .close_outer_scope(scope, None)
                .map_err(direct_status)?;
            if terminal_token.is_some() {
                return Err(helios_protocol::HeliosTranslatorStatus::HostCallbackFailed);
            }
            return Ok(None);
        }
        Err(error) => {
            let status = direct_status(error);
            let _ = dev.translator.close_outer_scope(scope, None);
            return Err(status);
        }
    };

    let hob_context = DirectHobContext {
        context_generation: runtime.context_generation,
        endpoint_id: runtime.endpoint_id,
        context_flags: runtime.context_flags,
        max_command_bytes: helios_protocol::HELIOS_HOC1_POOL_BYTES,
        last_batch_id: runtime
            .last_batch_id
            .load(std::sync::atomic::Ordering::Acquire),
        allocation_list_count: 0,
    };
    let mut allocation_handles = Vec::with_capacity(batch.uses.len());
    let mut seen_tokens = Vec::with_capacity(batch.uses.len());
    let identities = super::identity12::lock(&dev.outer_allocations);
    let hob = dev
        .translator
        .encode_hob1(&batch, hob_context, |_index, use_record| {
            if seen_tokens.contains(&use_record.outer_allocation_token) {
                return Err(DirectIdentityRefusal::DuplicateAssociation);
            }
            let identity = identities
                .resolve_token(
                    dev.translator.session_generation(),
                    use_record.outer_allocation_token,
                    use_record.byte_offset,
                    use_record.byte_length,
                    terminal_token,
                )
                .map_err(|refusal| match refusal {
                    super::identity12::IdentityRefusal::ForeignDeviceGeneration => {
                        DirectIdentityRefusal::ForeignDevice
                    }
                    super::identity12::IdentityRefusal::RangeOverflow => {
                        DirectIdentityRefusal::RangeOverflow
                    }
                    super::identity12::IdentityRefusal::RangeOutOfBounds => {
                        DirectIdentityRefusal::RangeOutOfBounds
                    }
                    super::identity12::IdentityRefusal::ZeroAllocationGeneration => {
                        DirectIdentityRefusal::StaleGeneration
                    }
                    super::identity12::IdentityRefusal::UseAfterDestroy => {
                        DirectIdentityRefusal::UseAfterDestroy
                    }
                    _ => DirectIdentityRefusal::MissingToken,
                })?;
            let address = identity
                .gpu_virtual_address
                .checked_add(use_record.byte_offset)
                .ok_or(DirectIdentityRefusal::RangeOverflow)?;
            if address == 0 || identity.allocation_generation == 0 || identity.h_allocation == 0 {
                return Err(DirectIdentityRefusal::StaleGeneration);
            }
            seen_tokens.push(use_record.outer_allocation_token);
            allocation_handles.push(identity.h_allocation);
            Ok(DirectResolvedUse {
                address_or_index: address,
                allocation_generation: identity.allocation_generation,
            })
        });
    drop(identities);
    let hob = match hob {
        Ok(hob) => hob,
        Err(error) => {
            log_error!("A7 D3D12 sealed-use/HOB1 refusal: {error:?}");
            let _ = dev.translator.close_outer_scope(scope, None);
            return Err(helios_protocol::HeliosTranslatorStatus::HostCallbackFailed);
        }
    };

    if unsafe { dev.outer_command_pool.make_resident(&allocation_handles) }.is_err() {
        let _ = dev.translator.close_outer_scope(scope, None);
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }

    let queue_context = outer as *const OuterQueueAssociation12 as usize;
    let reservation = {
        let mut reserved = None;
        for _ in 0..=helios_protocol::HELIOS_HOC1_MAX_LIVE_EXTENTS {
            match dev.outer_command_pool.reserve(
                hob.as_bytes().len(),
                queue_context,
                runtime.context_generation,
                hob.header().batch_id,
            ) {
                super::command_pool12::PoolReserve::Reserved(value) => {
                    reserved = Some(value);
                    break;
                }
                super::command_pool12::PoolReserve::Wait(wait) => {
                    if !unsafe { wait_pool_owner(wait.queue_context, wait.retire_value) } {
                        break;
                    }
                }
                super::command_pool12::PoolReserve::Refused => break,
            }
        }
        reserved
    };
    let Some(reservation) = reservation else {
        let _ = dev.translator.close_outer_scope(scope, None);
        return Err(helios_protocol::HeliosTranslatorStatus::BatchBoundExceeded);
    };

    unsafe {
        core::ptr::copy_nonoverlapping(
            hob.as_bytes().as_ptr(),
            reservation.cpu.as_ptr(),
            reservation.hob1_bytes,
        );
        #[cfg(target_arch = "x86_64")]
        core::arch::x86_64::_mm_sfence();
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::Release);
    if !dev.outer_command_pool.seal(reservation) {
        dev.outer_command_pool.abandon(reservation.lease);
        let _ = dev.translator.close_outer_scope(scope, None);
        return Err(helios_protocol::HeliosTranslatorStatus::HostCallbackFailed);
    }

    let mut hos1 = hob.outer_submit();
    let expectation = helios_protocol::HeliosOuterBatchExpectation {
        package_generation: helios_protocol::HELIOS_PACKAGE_GENERATION,
        session_generation: dev.translator.session_generation(),
        context_generation: runtime.context_generation,
        endpoint_id: runtime.endpoint_id,
        flags: runtime.context_flags,
        max_command_bytes: helios_protocol::HELIOS_HOC1_POOL_BYTES,
        last_batch_id: runtime
            .last_batch_id
            .load(std::sync::atomic::Ordering::Acquire),
        allocation_list_count: 0,
    };
    if hos1
        .validate(&expectation, reservation.hob1_bytes as u64)
        .is_err()
        || hos1.cross_check(hob.header()).is_err()
        || core::mem::size_of_val(&hos1) != helios_protocol::HELIOS_HOS1_BYTES as usize
    {
        dev.outer_command_pool.abandon(reservation.lease);
        let _ = dev.translator.close_outer_scope(scope, None);
        return Err(helios_protocol::HeliosTranslatorStatus::HostCallbackFailed);
    }
    let Some(submit) = (unsafe { (*dev.kt_callbacks).pfnSubmitCommandCb }) else {
        dev.outer_command_pool.abandon(reservation.lease);
        let _ = dev.translator.close_outer_scope(scope, None);
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    };
    let mut arg = ddi12::D3DDDICB_SUBMITCOMMAND::default();
    arg.Commands = reservation.gpuva;
    arg.CommandLength = reservation.hob1_bytes as u32;
    arg.BroadcastContextCount = 1;
    arg.BroadcastContext[0] = runtime.h_context.as_ptr();
    arg.pPrivateDriverData = core::ptr::from_mut(&mut hos1).cast();
    arg.PrivateDriverDataSize = u32::from(helios_protocol::HELIOS_HOS1_BYTES);
    // NumPrimaries/WrittenPrimaries remain the runtime-approved zeroed set for
    // this non-present batch; no HOS1 field substitutes for them.
    let hr = unsafe { submit(dev.h_rt_device.handle, &arg) };
    if hr < 0 {
        dev.outer_command_pool.abandon(reservation.lease);
        let _ = dev.translator.close_outer_scope(scope, None);
        mark_outer_lost(outer, "pfnSubmitCommandCb");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }

    runtime
        .last_batch_id
        .store(hob.header().batch_id, std::sync::atomic::Ordering::Release);
    let progress = match unsafe { signal_outer_hqc1(dev, outer, runtime) } {
        Ok(progress) => progress,
        Err(status) => {
            dev.outer_command_pool.poison(reservation.lease);
            let _ = dev.translator.close_outer_scope(scope, None);
            return Err(status);
        }
    };
    if !dev
        .outer_command_pool
        .submitted(reservation, runtime.hqc1_cpu, progress)
    {
        dev.outer_command_pool.poison(reservation.lease);
        let _ = dev.translator.close_outer_scope(scope, None);
        mark_outer_lost(outer, "HOC1 retirement tag");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    if let Err(error) = dev.translator.close_outer_scope(scope, Some(progress)) {
        log_error!("A7 D3D12 committed scope close refused: {error:?}");
        mark_outer_lost(outer, "committed scope close");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    if let Err(refusal) = super::identity12::lock(&dev.outer_allocations).note_context(
        dev.translator.session_generation(),
        &seen_tokens,
        runtime.context_generation,
        terminal_token,
    ) {
        log_error!("A7 D3D12 context ownership update refused: {refusal:?}");
        mark_outer_lost(outer, "allocation context ownership");
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    Ok(Some(progress))
}

unsafe fn join_outer_scope12(
    dev: &device12::HeliosD3D12Device,
    outer: &OuterQueueAssociation12,
    runtime: &OuterRuntimeQueue12,
    required_progress: u64,
) -> Result<helios_protocol::HeliosSyncProgressResultV1, helios_protocol::HeliosTranslatorStatus> {
    if outer.device_lost.load(std::sync::atomic::Ordering::Acquire) != 0 {
        return Err(helios_protocol::HeliosTranslatorStatus::DeviceLost);
    }
    let mut active = lock_ignore_poison(&runtime.active_scope);
    let cut_progress = if let Some(scope) = active.take() {
        let _submit_guard = lock_ignore_poison(&runtime.submit_lock);
        let progress = match unsafe { submit_outer_scope12(dev, outer, runtime, scope, None) }? {
            Some(progress) => progress,
            None => unsafe { signal_outer_hqc1(dev, outer, runtime) }?,
        };
        let reopened = dev
            .translator
            .open_outer_scope(runtime.context_generation, runtime.endpoint_id)
            .map_err(direct_status)?;
        *active = Some(reopened);
        Some(progress)
    } else {
        None
    };

    let target = if required_progress != 0 {
        required_progress
    } else if let Some(progress) = cut_progress {
        progress
    } else {
        runtime
            .last_submitted_progress
            .load(std::sync::atomic::Ordering::Acquire)
    };
    drop(active);
    if target != 0 {
        unsafe { wait_outer_hqc1(dev, outer, runtime, target) }?;
    }
    let result = outer_progress_result(outer, runtime);
    result
        .validate_join(required_progress)
        .map_err(|_| helios_protocol::HeliosTranslatorStatus::HostCallbackFailed)?;
    Ok(result)
}

pub(crate) extern "C" fn translator_sync_progress_join(
    host_context_cookie: *mut c_void,
    request: *const helios_protocol::HeliosSyncProgressJoinV1,
    out_result: *mut helios_protocol::HeliosSyncProgressResultV1,
) -> helios_protocol::HeliosTranslatorStatusCode {
    if request.is_null() || out_result.is_null() {
        return helios_protocol::HeliosTranslatorStatus::NullArgument as _;
    }
    let Some(outer) = (unsafe { outer_from_cookie(host_context_cookie) }) else {
        return helios_protocol::HeliosTranslatorStatus::UnknownContext as _;
    };
    let Some(runtime) = outer.runtime.get() else {
        return helios_protocol::HeliosTranslatorStatus::UnknownContext as _;
    };
    let request = unsafe { &*request };
    if let Err(status) = request.validate(runtime.context_generation) {
        return status as _;
    }
    let Some(dev) = (unsafe { device12::device(outer.h_device) }) else {
        return helios_protocol::HeliosTranslatorStatus::UnknownContext as _;
    };
    match unsafe { join_outer_scope12(dev, outer, runtime, request.required_progress_value) } {
        Ok(result) => {
            unsafe { out_result.write(result) };
            helios_protocol::HeliosTranslatorStatus::Ok as _
        }
        Err(status) => status as _,
    }
}

pub(crate) extern "C" fn translator_sync_progress_query(
    host_context_cookie: *mut c_void,
    context_generation: u64,
    out_result: *mut helios_protocol::HeliosSyncProgressResultV1,
) -> helios_protocol::HeliosTranslatorStatusCode {
    if out_result.is_null() {
        return helios_protocol::HeliosTranslatorStatus::NullArgument as _;
    }
    let Some(outer) = (unsafe { outer_from_cookie(host_context_cookie) }) else {
        return helios_protocol::HeliosTranslatorStatus::UnknownContext as _;
    };
    let Some(runtime) = outer.runtime.get() else {
        return helios_protocol::HeliosTranslatorStatus::UnknownContext as _;
    };
    if context_generation == 0 || context_generation != runtime.context_generation {
        return helios_protocol::HeliosTranslatorStatus::UnknownContext as _;
    }
    unsafe { out_result.write(outer_progress_result(outer, runtime)) };
    helios_protocol::HeliosTranslatorStatus::Ok as _
}

extern "C" fn outer_submit_begin(cookie: *mut c_void) -> i32 {
    let Some(outer) = (unsafe { outer_from_cookie(cookie) }) else {
        return E_FAIL;
    };
    let Some(runtime) = outer.runtime.get() else {
        return E_FAIL;
    };
    let Some(dev) = (unsafe { device12::device(outer.h_device) }) else {
        return E_FAIL;
    };
    if outer.device_lost.load(std::sync::atomic::Ordering::Acquire) != 0 {
        return E_FAIL;
    }
    let mut active = lock_ignore_poison(&runtime.active_scope);
    if active.is_some() {
        return E_FAIL;
    }
    match dev
        .translator
        .open_outer_scope(runtime.context_generation, runtime.endpoint_id)
    {
        Ok(scope) => {
            *active = Some(scope);
            S_OK
        }
        Err(error) => {
            log_error!("A7 D3D12 scope open refused: {error:?}");
            E_FAIL
        }
    }
}

extern "C" fn outer_submit_finish(cookie: *mut c_void, lower_result: i32) -> i32 {
    let Some(outer) = (unsafe { outer_from_cookie(cookie) }) else {
        return E_FAIL;
    };
    let Some(runtime) = outer.runtime.get() else {
        return E_FAIL;
    };
    let Some(dev) = (unsafe { device12::device(outer.h_device) }) else {
        return E_FAIL;
    };
    let mut active = lock_ignore_poison(&runtime.active_scope);
    let Some(scope) = active.take() else {
        return E_FAIL;
    };
    if lower_result != VK_SUCCESS {
        let _ = dev.translator.close_outer_scope(scope, None);
        return lower_result;
    }
    let _submit_guard = lock_ignore_poison(&runtime.submit_lock);
    match unsafe { submit_outer_scope12(dev, outer, runtime, scope, None) } {
        Ok(_) => VK_SUCCESS,
        Err(status) => {
            log_error!("A7 D3D12 outer submit refused: {status:?}");
            mark_outer_lost(outer, "outer submit");
            VK_ERROR_DEVICE_LOST
        }
    }
}

extern "C" fn outer_submit_join(cookie: *mut c_void) -> i32 {
    let Some(outer) = (unsafe { outer_from_cookie(cookie) }) else {
        return E_FAIL;
    };
    let Some(runtime) = outer.runtime.get() else {
        return E_FAIL;
    };
    let Some(dev) = (unsafe { device12::device(outer.h_device) }) else {
        return E_FAIL;
    };
    let target = runtime
        .last_submitted_progress
        .load(std::sync::atomic::Ordering::Acquire);
    if target == 0 || unsafe { wait_outer_hqc1(dev, outer, runtime, target) }.is_ok() {
        S_OK
    } else {
        E_FAIL
    }
}

struct OuterAllocationTeardownScope12 {
    device_context: *mut c_void,
    device_generation: u64,
    outer_allocation_token: u64,
    outer: Arc<OuterQueueAssociation12>,
}

unsafe fn device_from_outer_context<'a>(
    context: *mut c_void,
) -> Option<&'a device12::HeliosD3D12Device> {
    let outer = unsafe { vkd3d_outer_context(context) }?;
    unsafe { device12::device(outer.h_device) }
}

unsafe fn vkd3d_outer_context<'a>(
    context: *mut c_void,
) -> Option<&'a device12::Vkd3dOuterContext12> {
    (!context.is_null()).then(|| unsafe { &*context.cast::<device12::Vkd3dOuterContext12>() })
}

/// Direct forward edge for one vkd3d-internal VkDeviceMemory allocation.
/// This can run while the engine device is still being constructed, so it uses
/// only the stable context and never dereferences the runtime device block.
pub(crate) extern "C" fn outer_allocation_create(
    context: *mut c_void,
    bytes: u64,
    cpu_visible: u32,
    device_local: u32,
    association_out: *mut helios_protocol::HeliosResourceAssociationV1,
) -> i32 {
    if association_out.is_null() || bytes == 0 || cpu_visible > 1 || device_local > 1 {
        return E_INVALIDARG;
    }
    unsafe { association_out.write(core::mem::zeroed()) };
    let Some(outer) = (unsafe { vkd3d_outer_context(context) }) else {
        return E_INVALIDARG;
    };
    match unsafe {
        super::resource12::allocate_vkd3d_internal_wddm_memory(
            outer,
            bytes,
            cpu_visible != 0,
            device_local != 0,
        )
    } {
        Ok(association) => {
            unsafe { association_out.write(association) };
            S_OK
        }
        Err(hr) => {
            log_error!(
                "A7 D3D12 internal WDDM allocation REFUSED: bytes={} cpuVisible={} deviceLocal={} hr={:#010x}",
                bytes,
                cpu_visible,
                device_local,
                hr as u32,
            );
            if hr < 0 {
                hr
            } else {
                E_FAIL
            }
        }
    }
}

/// Arm the exact token before vkd3d frees the associated VkDeviceMemory.
/// DDI resources arrive already armed by DestroyHeapAndResource; standalone
/// internal allocations transition here for the first time.
pub(crate) extern "C" fn outer_allocation_teardown_begin(
    context: *mut c_void,
    device_generation: u64,
    outer_allocation_token: u64,
) -> i32 {
    let Some(outer) = (unsafe { vkd3d_outer_context(context) }) else {
        return E_INVALIDARG;
    };
    if device_generation == 0
        || device_generation != outer.device_generation
        || outer_allocation_token == 0
    {
        return E_INVALIDARG;
    }
    match super::identity12::lock(&outer.outer_allocations)
        .arm_teardown_by_token(device_generation, outer_allocation_token)
    {
        Ok(_) => S_OK,
        Err(refusal) => {
            log_error!("A7 D3D12 teardown arm refusal: {refusal:?}");
            E_FAIL
        }
    }
}

/// Construction-time vkd3d callback. It chooses a currently live outer queue
/// only after resolving the exact pending token in this device's own registry.
/// The returned scope pointer is lifetime state, never allocation identity.
pub(crate) extern "C" fn outer_allocation_begin(
    context: *mut c_void,
    device_generation: u64,
    outer_allocation_token: u64,
    out_scope: *mut *mut c_void,
) -> i32 {
    if out_scope.is_null() {
        return E_INVALIDARG;
    }
    unsafe { out_scope.write(core::ptr::null_mut()) };
    let Some(device_outer) = (unsafe { vkd3d_outer_context(context) }) else {
        return E_INVALIDARG;
    };
    if device_generation == 0
        || device_generation != device_outer.device_generation
        || outer_allocation_token == 0
    {
        return E_INVALIDARG;
    }
    let identity = match super::identity12::lock(&device_outer.outer_allocations)
        .pending_teardown(device_generation, outer_allocation_token)
    {
        Ok(identity) => identity,
        Err(refusal) => {
            log_error!("A7 D3D12 teardown begin identity refusal: {refusal:?}");
            return E_FAIL;
        }
    };
    if identity.last_context_generation == 0 {
        return S_FALSE_OUTER;
    }
    let Some(dev) = (unsafe { device_from_outer_context(context) }) else {
        return E_FAIL;
    };
    let Some(outer) = dev.outer_queues.acquire(identity.last_context_generation) else {
        log_error!(
            "A7 D3D12 teardown begin has no live outer queue for token={} lastContext={}",
            outer_allocation_token,
            identity.last_context_generation,
        );
        return E_FAIL;
    };
    if outer.h_device.pDrvPrivate != device_outer.h_device.pDrvPrivate {
        outer.release_teardown();
        return E_FAIL;
    }
    let begin = outer_submit_begin(Arc::as_ptr(&outer).cast_mut().cast());
    if begin != S_OK {
        outer.release_teardown();
        return begin;
    }
    let scope = Box::new(OuterAllocationTeardownScope12 {
        device_context: context,
        device_generation,
        outer_allocation_token,
        outer,
    });
    unsafe { out_scope.write(Box::into_raw(scope).cast()) };
    S_OK
}

/// Submit and close the one terminal Mesa allocation scope. The Arc keeps the
/// selected queue association alive while queue rundown waits on its bounded
/// active-teardown count.
pub(crate) extern "C" fn outer_allocation_finish(
    context: *mut c_void,
    scope: *mut c_void,
    lower_result: i32,
) -> i32 {
    if scope.is_null() {
        return E_INVALIDARG;
    }
    let scope = unsafe { Box::from_raw(scope.cast::<OuterAllocationTeardownScope12>()) };
    let outer = Arc::clone(&scope.outer);
    let context_matches = context == scope.device_context;
    let result = if let (Some(dev), Some(runtime)) = (
        unsafe { device_from_outer_context(scope.device_context) },
        outer.runtime.get(),
    ) {
        let mut active = lock_ignore_poison(&runtime.active_scope);
        let translator_scope = active.take();
        if !context_matches || dev.translator.session_generation() != scope.device_generation {
            if let Some(translator_scope) = translator_scope {
                let _ = dev.translator.close_outer_scope(translator_scope, None);
            }
            mark_outer_lost(outer.as_ref(), "terminal allocation scope provenance");
            E_INVALIDARG
        } else if let Some(translator_scope) = translator_scope {
            if lower_result != VK_SUCCESS {
                let _ = dev.translator.close_outer_scope(translator_scope, None);
                lower_result
            } else {
                let _submit_guard = lock_ignore_poison(&runtime.submit_lock);
                match unsafe {
                    submit_outer_scope12(
                        dev,
                        outer.as_ref(),
                        runtime,
                        translator_scope,
                        Some(scope.outer_allocation_token),
                    )
                } {
                    Ok(Some(_)) => VK_SUCCESS,
                    Ok(None) => E_FAIL,
                    Err(status) => {
                        log_error!("A7 D3D12 terminal allocation submit refused: {status:?}");
                        mark_outer_lost(outer.as_ref(), "terminal allocation submit");
                        VK_ERROR_DEVICE_LOST
                    }
                }
            }
        } else {
            E_FAIL
        }
    } else {
        E_FAIL
    };
    outer.release_teardown();
    result
}

/// Reverse the exact pending token/WDDM ownership after vkd3d has completed
/// the terminal Vulkan object graph. Removal is atomic under the device lock;
/// no storage can be reused while the association remains live.
pub(crate) extern "C" fn outer_allocation_retire(
    context: *mut c_void,
    device_generation: u64,
    outer_allocation_token: u64,
    teardown_result: i32,
) -> i32 {
    let Some(outer) = (unsafe { vkd3d_outer_context(context) }) else {
        return E_INVALIDARG;
    };
    let retired = match super::identity12::lock(&outer.outer_allocations)
        .retire_teardown(device_generation, outer_allocation_token)
    {
        Ok(retired) => retired,
        Err(refusal) => {
            log_error!("A7 D3D12 teardown retire identity refusal: {refusal:?}");
            if let Some(dev) = unsafe { device_from_outer_context(context) } {
                let _ = device12::set_error(dev, E_FAIL);
            }
            return E_FAIL;
        }
    };
    let deallocated =
        unsafe { super::resource12::retire_outer_allocation(outer, retired.identity) };
    retired.complete(deallocated);
    if teardown_result != VK_SUCCESS || !deallocated {
        if let Some(dev) = unsafe { device_from_outer_context(context) } {
            let _ = device12::set_error(dev, E_FAIL);
        }
        E_FAIL
    } else {
        S_OK
    }
}

unsafe fn destroy_outer_context_parts(
    dev: &device12::HeliosD3D12Device,
    h_rt_queue: ddi12::D3D12DDI_HRTCOMMANDQUEUE,
    h_context: *mut c_void,
    hqc1: u32,
) {
    if hqc1 != 0 && !dev.kt_callbacks.is_null() {
        if let Some(destroy_sync) = unsafe { (*dev.kt_callbacks).pfnDestroySynchronizationObjectCb }
        {
            let arg = ddi12::D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT { hSyncObject: hqc1 };
            let _ = unsafe { destroy_sync(dev.h_rt_device.handle, &arg) };
        }
    }
    if !h_context.is_null() && !dev.um_callbacks.is_null() {
        if let Some(destroy_context) = unsafe { (*dev.um_callbacks).pfnDestroyContextCb } {
            let arg = ddi12::D3DDDICB_DESTROYCONTEXT {
                hContext: h_context,
            };
            let _ = unsafe { destroy_context(h_rt_queue, &arg) };
        }
    }
}

/// Build HQA1 first, then mint exactly one virtual runtime context and HQC1 for
/// this queue.  `outer_cookie` already names the final stable Box address.
unsafe fn create_outer_virtual_context(
    dev: &device12::HeliosD3D12Device,
    h_rt_queue: ddi12::D3D12DDI_HRTCOMMANDQUEUE,
    outer_cookie: usize,
    vk_family: u32,
    vk_index: u32,
    engine_class: u32,
) -> Result<OuterRuntimeQueue12, ddi12::HRESULT> {
    if dev.um_callbacks.is_null() || dev.kt_callbacks.is_null() || outer_cookie == 0 {
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(E_FAIL);
    }
    let Some(create_context) = (unsafe { (*dev.um_callbacks).pfnCreateContextVirtualCb }) else {
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(E_FAIL);
    };
    let Some(create_sync) = (unsafe { (*dev.kt_callbacks).pfnCreateSynchronizationObject2Cb })
    else {
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(E_FAIL);
    };
    if unsafe { (*dev.um_callbacks).pfnDestroyContextCb }.is_none()
        || unsafe { (*dev.kt_callbacks).pfnDestroySynchronizationObjectCb }.is_none()
        || unsafe { (*dev.kt_callbacks).pfnSubmitCommandCb }.is_none()
        || unsafe { (*dev.kt_callbacks).pfnSignalSynchronizationObjectFromGpuCb }.is_none()
        || unsafe { (*dev.kt_callbacks).pfnWaitForSynchronizationObjectFromCpuCb }.is_none()
    {
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(E_FAIL);
    }

    let endpoints = dev.translator.endpoints().map_err(|error| {
        log_error!("CreateCommandQueue: A5 endpoint enumeration refused: {error:?}");
        E_FAIL
    })?;
    let Some(endpoint) = endpoints.into_iter().find(|endpoint| {
        endpoint
            .validate(dev.translator.endpoint_capacity())
            .is_ok()
            && endpoint.queue_family == vk_family
            && endpoint.queue_index == vk_index
            && endpoint.engine_class == engine_class
    }) else {
        log_error!(
            "CreateCommandQueue: no exact A5 endpoint for family={} index={} class={}",
            vk_family,
            vk_index,
            engine_class,
        );
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(E_FAIL);
    };

    let context_generation = dev
        .next_outer_context_generation
        .fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |value| (value != 0 && value != u64::MAX).then_some(value + 1),
        )
        .map_err(|_| E_FAIL)?;
    let attach = dev
        .translator
        .build_queue_attach(
            context_generation,
            endpoint.endpoint_id,
            engine_class,
            helios_protocol::HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
        )
        .map_err(|error| {
            log_error!("CreateCommandQueue: A5 HQA1 construction refused: {error:?}");
            E_FAIL
        })?;

    let mut create = ddi12::D3DDDICB_CREATECONTEXTVIRTUAL::default();
    create.NodeOrdinal = 0;
    create.EngineAffinity = 0;
    create.pPrivateDriverData = core::ptr::from_ref(&attach).cast_mut().cast();
    create.PrivateDriverDataSize = core::mem::size_of_val(&attach) as u32;
    let hr = unsafe { create_context(h_rt_queue, &mut create) };
    let Some(h_context) = core::ptr::NonNull::new(create.hContext) else {
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(if hr < 0 { hr } else { E_FAIL });
    };
    if hr < 0 {
        unsafe { destroy_outer_context_parts(dev, h_rt_queue, create.hContext, 0) };
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(hr);
    }

    let mut sync = ddi12::D3DDDICB_CREATESYNCHRONIZATIONOBJECT2::default();
    sync.Info.Type = ddi12::_D3DDDI_SYNCHRONIZATIONOBJECT_TYPE_D3DDDI_MONITORED_FENCE;
    sync.Info.Flags.__bindgen_anon_1.Value = (1 << 6) | (1 << 7);
    sync.Info.__bindgen_anon_1.MonitoredFence = Default::default();
    sync.Info.__bindgen_anon_1.MonitoredFence.InitialFenceValue = 0;
    sync.Info.__bindgen_anon_1.MonitoredFence.EngineAffinity = 1;
    let sync_hr = unsafe { create_sync(dev.h_rt_device.handle, &mut sync) };
    let hqc1 = core::num::NonZeroU32::new(sync.hSyncObject);
    let hqc1_cpu = core::ptr::NonNull::new(
        unsafe {
            sync.Info
                .__bindgen_anon_1
                .MonitoredFence
                .FenceValueCPUVirtualAddress
        }
        .cast::<u64>(),
    );
    let (Some(hqc1), Some(hqc1_cpu)) = (hqc1, hqc1_cpu) else {
        unsafe { destroy_outer_context_parts(dev, h_rt_queue, create.hContext, sync.hSyncObject) };
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(if sync_hr < 0 { sync_hr } else { E_FAIL });
    };
    if sync_hr < 0 {
        unsafe { destroy_outer_context_parts(dev, h_rt_queue, create.hContext, hqc1.get()) };
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(sync_hr);
    }

    if let Err(error) = dev.translator.attach_outer_context(
        context_generation,
        endpoint.endpoint_id,
        helios_protocol::HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
        outer_cookie as *mut c_void,
    ) {
        log_error!("CreateCommandQueue: A5 context attach refused: {error:?}");
        unsafe { destroy_outer_context_parts(dev, h_rt_queue, create.hContext, hqc1.get()) };
        note_refusal(&L2_REFUSALS.queue_context_failed);
        return Err(E_FAIL);
    }

    log_error!(
        "CreateCommandQueue: A5 HQA1 virtual context generation={} endpoint={} family={} index={} class={} hContext={:p} HQC1=0x{:x}",
        context_generation,
        endpoint.endpoint_id,
        vk_family,
        vk_index,
        engine_class,
        h_context.as_ptr(),
        hqc1.get(),
    );
    Ok(OuterRuntimeQueue12 {
        h_context,
        context_generation,
        endpoint_id: endpoint.endpoint_id,
        context_flags: helios_protocol::HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
        hqc1,
        hqc1_cpu,
        next_progress: std::sync::atomic::AtomicU64::new(1),
        last_submitted_progress: std::sync::atomic::AtomicU64::new(0),
        last_batch_id: std::sync::atomic::AtomicU64::new(0),
        active_scope: Mutex::new(None),
        submit_lock: Mutex::new(()),
    })
}

/// `pfnDestroyCommandQueue`.
///
/// # Safety
/// `h_queue` must be a handle [`create_command_queue`] returned `S_OK` for and
/// which has not already been destroyed.
unsafe extern "C" fn destroy_command_queue(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_queue`.
    let Some(slot) = (unsafe { Slot::<Boxed<QueueState>>::from_priv(h_queue.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.queue_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null or the one box `create_command_queue`
    // moved in; `take` empties it, so a second destroy is a no-op rather than a
    // double free.
    let Some(state) = (unsafe { slot.take() }) else {
        note_refusal(&L2_REFUSALS.queue_bad_arg);
        return;
    };

    // Join and directly rundown A5/HQC1/HQA1 while the stable callback box and
    // engine queue are both still alive.  The engine queue drops only after no
    // pool extent or translator attachment can name this association.
    unsafe { destroy_outer_queue(&state) };

    // Dropping the box releases the engine queue's single reference.
    drop(state);
}

unsafe fn destroy_outer_queue(state: &QueueState) {
    let outer = state.outer.as_ref();
    let Some(dev) = (unsafe { device12::device(state.h_device) }) else {
        note_refusal(&L2_REFUSALS.queue_context_destroy_failed);
        return;
    };

    dev.outer_queues.unregister(&state.outer);
    outer.close_and_wait();

    unsafe { destroy_outer_queue_parts(dev, outer) };
}

unsafe fn destroy_outer_queue_parts(
    dev: &device12::HeliosD3D12Device,
    outer: &OuterQueueAssociation12,
) {
    let Some(runtime) = outer.runtime.get() else {
        return;
    };

    let active = lock_ignore_poison(&runtime.active_scope).take();
    if let Some(scope) = active {
        let _ = dev.translator.close_outer_scope(scope, None);
    }
    let last = runtime
        .last_submitted_progress
        .load(std::sync::atomic::Ordering::Acquire);
    if last != 0 && unsafe { wait_outer_hqc1(dev, outer, runtime, last) }.is_err() {
        note_refusal(&L2_REFUSALS.queue_context_destroy_failed);
        mark_outer_lost(outer, "queue rundown join");
    }

    dev.outer_command_pool
        .purge_queue(outer as *const OuterQueueAssociation12 as usize);
    if let Err(error) = dev
        .translator
        .detach_outer_context(runtime.context_generation)
    {
        log_error!(
            "DestroyCommandQueue: A5 detach generation={} refused: {error:?}",
            runtime.context_generation,
        );
        note_refusal(&L2_REFUSALS.queue_context_destroy_failed);
    }
    unsafe {
        destroy_outer_context_parts(
            dev,
            outer.h_rt_queue,
            runtime.h_context.as_ptr(),
            runtime.hqc1.get(),
        )
    };
}

// ---------------------------------------------------------------------------
// (d) Command pools — 4 slots
// ---------------------------------------------------------------------------

/// `pfnCalcPrivateCommandPoolSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live
/// `D3D12DDIARG_CREATE_COMMAND_POOL_0040` for the duration of the call.
unsafe extern "C" fn calc_private_command_pool_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_POOL_0040,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L2_REFUSALS.pool_bad_arg);
    }
    // ⛔ Answered unconditionally — see `PRIVATE_SLOT_SIZE`.
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// `pfnCreateCommandPool`.
///
/// ⚠ **Creates no engine object.** `D3D12DDIARG_CREATE_COMMAND_POOL_0040` is one
/// flags word with a single legal value and carries no allocator class. The
/// allocator for each class is created on first `pfnResetCommandList`, where the
/// list finally supplies that authoritative class.
///
/// # Safety
/// `h_pool`'s `pDrvPrivate` must address the private block
/// [`calc_private_command_pool_size`] sized.
unsafe extern "C" fn create_command_pool(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_POOL_0040,
    h_pool: ddi12::D3D12DDI_HCOMMANDPOOL_0040,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) = (unsafe { Slot::<Boxed<PoolState>>::from_priv(h_pool.drv_private()) }) else {
        note_refusal(&L2_REFUSALS.pool_bad_arg);
        return E_INVALIDARG;
    };
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L2_REFUSALS.pool_bad_arg);
        return E_INVALIDARG;
    }
    // ⚠ `PoolFlags` is deliberately not examined: `D3D12DDI_COMMAND_POOL_FLAGS`
    // has exactly one enumerator, `NONE = 0`, so there is nothing to branch on
    // and a check against a one-value enum would be a claim about a future
    // header rather than about this one.

    // SAFETY: the slot lies in the sized private block and is currently null.
    unsafe {
        slot.store(PoolState {
            allocators: Arc::new(PoolAllocators::new()),
        });
    }
    S_OK
}

/// `pfnResetCommandPool` -> `ID3D12CommandAllocator::Reset()`.
///
/// Returns `VOID`. Every class materialised for this DDI pool is reset. A reset
/// before any list first used the pool has no engine allocator and is counted as
/// the benign no-op it is.
///
/// # Safety
/// `h_pool` must be a handle [`create_command_pool`] returned `S_OK` for.
unsafe extern "C" fn reset_command_pool(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_pool: ddi12::D3D12DDI_HCOMMANDPOOL_0040,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_pool`.
    let Some(pool) = (unsafe { pool_state(h_pool) }) else {
        note_refusal(&L2_REFUSALS.pool_bad_arg);
        return;
    };
    let mut any = false;
    for (list_type, allocator) in pool.allocators.initialized() {
        any = true;
        // SAFETY: the allocator is an owned member of the pool's `Arc`; `Reset`
        // takes no arguments and returns an HRESULT.
        if let Err(e) = unsafe { allocator.Reset() } {
            note_refusal(&L2_REFUSALS.pool_reset_engine_failed);
            if let Some(n) = budget(&POOL_LOG) {
                log_error!(
                    "ResetCommandPool: engine Reset(type={}) failed hr={:#010x} (x{})",
                    list_type.0,
                    e.code().0 as u32,
                    n + 1,
                );
            }
        }
    }
    if !any {
        note_refusal(&L2_REFUSALS.pool_reset_no_allocator);
    }
}

/// `pfnDestroyCommandPool`.
///
/// # Safety
/// `h_pool` must be a handle [`create_command_pool`] returned `S_OK` for and
/// which has not already been destroyed.
unsafe extern "C" fn destroy_command_pool(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_pool: ddi12::D3D12DDI_HCOMMANDPOOL_0040,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_pool`.
    let Some(slot) = (unsafe { Slot::<Boxed<PoolState>>::from_priv(h_pool.drv_private()) }) else {
        note_refusal(&L2_REFUSALS.pool_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null or the one box `create_command_pool`
    // moved in. Dropping the box releases its `Arc`; recorder targets may keep
    // the allocator set alive without touching this runtime-owned private block.
    let Some(state) = (unsafe { slot.take() }) else {
        note_refusal(&L2_REFUSALS.pool_bad_arg);
        return;
    };
    drop(state);
}

// ---------------------------------------------------------------------------
// (d) Command recorders — 4 slots
// ---------------------------------------------------------------------------

/// `pfnCalcPrivateCommandRecorderSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live
/// `D3D12DDIARG_CREATE_COMMAND_RECORDER_0040` for the duration of the call.
unsafe extern "C" fn calc_private_command_recorder_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_RECORDER_0040,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L2_REFUSALS.recorder_bad_arg);
    }
    // ⛔ Answered unconditionally — see `PRIVATE_SLOT_SIZE`.
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// `pfnCreateCommandRecorder` — driver-side only, no engine object.
///
/// # Safety
/// `h_recorder`'s `pDrvPrivate` must address the private block
/// [`calc_private_command_recorder_size`] sized.
unsafe extern "C" fn create_command_recorder(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_RECORDER_0040,
    h_recorder: ddi12::D3D12DDI_HCOMMANDRECORDER_0040,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) = (unsafe { Slot::<Boxed<RecorderState>>::from_priv(h_recorder.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.recorder_bad_arg);
        return E_INVALIDARG;
    };
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L2_REFUSALS.recorder_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*arg };

    let Some(queue_type) = engine_list_type(a.QueueFlags) else {
        note_refusal(&L2_REFUSALS.recorder_class_unsupported);
        if let Some(n) = budget(&RECORDER_LOG) {
            log_error!(
                "CreateCommandRecorder: QueueFlags={:#x} names no 3D/COMPUTE/COPY class this \
                 driver backs -> E_INVALIDARG (x{})",
                a.QueueFlags,
                n + 1,
            );
        }
        return E_INVALIDARG;
    };
    // ⚠ `RecorderFlags` has one enumerator, `NONE = 0`, so it carries nothing to
    // branch on — same reasoning as `PoolFlags` in `create_command_pool`.

    // SAFETY: the slot lies in the sized private block and is currently null.
    unsafe {
        slot.store(RecorderState {
            queue_type,
            target: Mutex::new(None),
        });
    }
    S_OK
}

/// `pfnCommandRecorderSetCommandPoolAsTarget` — bind a pool to a recorder.
///
/// The recorder's `QueueFlags` is not the allocator class. Time Spy's exact DDI
/// stream binds a DIRECT-compatible recorder and later resets a COMPUTE list
/// through it. This call therefore captures the pool's owned allocator set; the
/// list class selects and lazily creates the member at `pfnResetCommandList`.
///
/// Returns `VOID`; there is no engine call and therefore no deferred failure at
/// this point.
///
/// # Safety
/// `h_device` must be a live device handle, `h_recorder` a live recorder handle
/// and `h_pool` a live pool handle.
unsafe extern "C" fn command_recorder_set_command_pool_as_target(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_recorder: ddi12::D3D12DDI_HCOMMANDRECORDER_0040,
    h_pool: ddi12::D3D12DDI_HCOMMANDPOOL_0040,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_recorder`.
    let Some(recorder) = (unsafe { recorder_state(h_recorder) }) else {
        note_refusal(&L2_REFUSALS.recorder_bad_arg);
        return;
    };
    // SAFETY: the caller guarantees a live handle from `create_command_pool`.
    let Some(pool) = (unsafe { pool_state(h_pool) }) else {
        // ⛔ The recorder DID resolve, so it may still be pointing at a previous
        // pool. See `unbind_target`: a bind that failed must leave no binding.
        unbind_target(recorder);
        note_refusal(&L2_REFUSALS.pool_bad_arg);
        return;
    };

    // ⚠ The previous binding is read rather than overwritten blind: reporting
    // *what changed* is what makes this field an instrument rather than
    // write-only state, and the trace line below is the readout.
    let previous = target_pool_identity(recorder);
    trace_line!(
        "CommandRecorderSetCommandPoolAsTarget: recorder={:p} pool {:#x} -> {:p}",
        h_recorder.pDrvPrivate,
        previous,
        h_pool.pDrvPrivate,
    );

    bind_target(recorder, h_pool.drv_private() as usize, &pool.allocators);
}

/// This recorder's current pool identity, or 0.
///
/// ⚠ Identity only — the value is never dereferenced. See [`RecorderTarget`].
fn target_pool_identity(recorder: &RecorderState) -> usize {
    lock_target(recorder).as_ref().map_or(0, |t| t.pool)
}

/// Forget whatever this recorder was pointed at.
///
/// ⛔ **Called from every failure arm of
/// `pfnCommandRecorderSetCommandPoolAsTarget`, and that is not tidiness.** The
/// slot's contract is *"this recorder now targets this pool"*; if it cannot be
/// honoured, leaving the PREVIOUS binding in place is worse than leaving none,
/// because `recorder_allocator` then answers `Ready` with an allocator belonging
/// to a pool the runtime did not name — one that may already back another
/// recording list, and one that `pfnResetCommandPool` on the *new* pool will
/// never reset. L3a's `L3aResetNoAllocator`, whose doc grades it *"Expected 0,
/// and a hit is a real finding about DDI ordering"*, is the loud-failure path
/// built for exactly this and is bypassed unless the stale target is cleared.
fn unbind_target(recorder: &RecorderState) {
    *lock_target(recorder) = None;
}

/// Point a recorder at a pool, retaining the allocator set independently of the
/// runtime-owned pool private block.
fn bind_target(recorder: &RecorderState, pool: usize, allocators: &Arc<PoolAllocators>) {
    *lock_target(recorder) = Some(RecorderTarget {
        pool,
        allocators: Arc::clone(allocators),
    });
}

/// Take a recorder's target lock, treating a poisoned lock as a live one.
///
/// ⛔ `unwrap_or_else(|e| e.into_inner())`, never `.unwrap()`. A `Mutex` is
/// poisoned only by a panic while it is held, and this crate is `panic = "abort"`
/// — so the poisoned arm cannot fire. `PARALLEL.md` §9.3 forbids `.unwrap()` on
/// runtime data regardless, and writing the recovery is how that stays true
/// without an argument at the call site.
fn lock_target(recorder: &RecorderState) -> MutexGuard<'_, Option<RecorderTarget>> {
    recorder
        .target
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What [`recorder_allocator`] found when a `pfnResetCommandList` asked a
/// recorder for the allocator to reset against.
///
/// Distinguishable failures rather than one `Option`, so L3a can report the
/// reset failure through the command-list error callback without guessing why
/// no allocator exists.
pub(crate) enum RecorderAllocator {
    /// The recorder names a pool and that pool is backed.
    Ready {
        /// **Owned** — one `AddRef` the caller releases by dropping.
        allocator: ID3D12CommandAllocator,
        /// The class requested by the command list and used at creation. The API
        /// cannot be asked for an allocator's class, so it is carried.
        list_type: D3D12_COMMAND_LIST_TYPE,
    },
    /// The recorder handle did not resolve to a live [`RecorderState`].
    NoRecorder,
    /// No `pfnCommandRecorderSetCommandPoolAsTarget` has ever run on it, so
    /// there is no pool and no allocator.
    NoPoolBound,
    /// The device or engine device needed for lazy creation was unavailable.
    NoDevice,
    /// The command list named a class this allocator set does not support.
    UnsupportedClass,
    /// `ID3D12Device::CreateCommandAllocator` failed.
    EngineFailed,
}

/// The allocator `pfnResetCommandList` must reset a list against.
///
/// ⭐ **`pub(crate)` because L3a's `pfnResetCommandList` is its only caller and
/// the chain it walks is entirely this lane's private state** — recorder ->
/// bound pool -> per-class `ID3D12CommandAllocator`. `PARALLEL.md` §4 gives L3a
/// the slot and this lane the three objects, and this function is that seam, in
/// the file that owns the objects. ⛔ It clones the target's `Arc` while holding
/// the recorder mutex, then releases that mutex before entering the engine.
///
/// # Safety
/// As [`recorder_state`], for a handle [`create_command_recorder`] returned
/// `S_OK` for.
pub(crate) unsafe fn recorder_allocator(
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_recorder: ddi12::D3D12DDI_HCOMMANDRECORDER_0040,
    list_type: D3D12_COMMAND_LIST_TYPE,
) -> RecorderAllocator {
    // SAFETY: forwarded unchanged; the caller's guarantee is `recorder_state`'s.
    let Some(recorder) = (unsafe { recorder_state(h_recorder) }) else {
        return RecorderAllocator::NoRecorder;
    };
    let allocators = match lock_target(recorder).as_ref() {
        Some(target) => Arc::clone(&target.allocators),
        None => return RecorderAllocator::NoPoolBound,
    };
    let Some(slot) = allocators.slot(list_type) else {
        return RecorderAllocator::UnsupportedClass;
    };
    if let Some(allocator) = slot.get() {
        return RecorderAllocator::Ready {
            allocator: allocator.clone(),
            list_type,
        };
    }

    // SAFETY: device-scope lookup; the borrow lives only for this call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L2_REFUSALS.pool_no_device);
        return RecorderAllocator::NoDevice;
    };
    let Some(engine) = dev.engine.d3d12_device() else {
        note_refusal(&L2_REFUSALS.pool_no_device);
        return RecorderAllocator::NoDevice;
    };
    // SAFETY: `engine` is the live bridge device and `list_type` is the exact
    // class carried by the DDI command list being reset.
    let allocator =
        match unsafe { engine.CreateCommandAllocator::<ID3D12CommandAllocator>(list_type) } {
            Ok(allocator) => allocator,
            Err(e) => {
                note_refusal(&L2_REFUSALS.pool_allocator_engine_failed);
                if let Some(n) = budget(&POOL_LOG) {
                    log_error!(
                        "ResetCommandList: engine CreateCommandAllocator(type={}) failed \
                     hr={:#010x} (x{})",
                        list_type.0,
                        e.code().0 as u32,
                        n + 1,
                    );
                }
                return RecorderAllocator::EngineFailed;
            }
        };
    if slot.set(allocator).is_err() {
        trace_line!(
            "ResetCommandList: lost type-{} allocator init race",
            list_type.0
        );
    }
    let Some(allocator) = slot.get() else {
        note_refusal(&L2_REFUSALS.pool_allocator_engine_failed);
        return RecorderAllocator::EngineFailed;
    };
    RecorderAllocator::Ready {
        allocator: allocator.clone(),
        list_type,
    }
}

/// `pfnDestroyCommandRecorder`.
///
/// The log line is this lane's readout of `RecorderState::target`: *did the
/// runtime ever bind a pool to this recorder?* is a real triage question, and one
/// line per recorder teardown is the cheapest place to answer it.
///
/// # Safety
/// `h_recorder` must be a handle [`create_command_recorder`] returned `S_OK` for
/// and which has not already been destroyed.
unsafe extern "C" fn destroy_command_recorder(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_recorder: ddi12::D3D12DDI_HCOMMANDRECORDER_0040,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_recorder`.
    let Some(slot) = (unsafe { Slot::<Boxed<RecorderState>>::from_priv(h_recorder.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.recorder_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null or the one box
    // `create_command_recorder` moved in.
    let Some(state) = (unsafe { slot.take() }) else {
        note_refusal(&L2_REFUSALS.recorder_bad_arg);
        return;
    };
    trace_line!(
        "DestroyCommandRecorder: recorder={:p} queueType={} lastPool={:#x}",
        h_recorder.pDrvPrivate,
        state.queue_type.0,
        target_pool_identity(&state),
    );
    // Dropping the box drops the target's `Arc` reference to the pool allocators.
    drop(state);
}

// ---------------------------------------------------------------------------
// (d) Command lists — 3 slots
// ---------------------------------------------------------------------------

/// `pfnCalcPrivateCommandListSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live
/// `D3D12DDIARG_CREATE_COMMAND_LIST_0040` for the duration of the call.
unsafe extern "C" fn calc_private_command_list_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_LIST_0040,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L2_REFUSALS.command_list_bad_arg);
    }
    // ⛔ Answered unconditionally — see `PRIVATE_SLOT_SIZE`.
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// `pfnCreateCommandList`.
///
/// # ⭐ `pfnSetCommandListDDITableCb` is mandatory here, and index 0 is the answer
///
/// The runtime says so in its own words:
///
/// > `Driver didn't call pfnSetCommandListDDITableCb or called it with invalid
/// > D3D12DDI_HRTTABLE at command list creation, defaulting to stubbed DDIs.`
/// > — strings:30
///
/// **What happens if the index is wrong**, stated because the brief asks and
/// because the failure is quiet: an `HRTTABLE` the runtime does not recognise
/// (including the `0` a never-stashed index would yield) makes it install *its
/// own* stubs over this list — the list is created, every recording DDI silently
/// goes to the runtime instead of to this driver, and the only signal is that
/// debug string. Nothing crashes; the list simply records nothing.
///
/// **Which index.** `D12-G5` measured the runtime filling
/// `D3D12DDI_TABLE_TYPE_COMMAND_LIST_3D` **twice** at device creation, indices 0
/// and 1, with two distinct handles (`0x3E0`, `0x638`), and WARP then calling
/// `pfnSetCommandListDDITableCb(hRTCommandList, 0x3E0)` — index **0** — on every
/// command-list create (`DDI_REFERENCE.md` §2.2, §9.3, §15.1 #9). Index 1 exists
/// so a driver can *swap* a list's table later: §9.3's design is a **recording**
/// table installed after a successful `pfnResetCommandList` and a
/// **closed/erroring** table installed after `pfnCloseCommandList`, which is how
/// a forwarder avoids an `if (!recording)` check at the top of all 75 recording
/// entry points. Helios fills both indices with the same content today
/// (`tables12::fill_command_list` does not vary by index), so index 1 would be
/// observationally identical — and picking it would be a claim this driver
/// cannot defend. ⇒ index 0, matching the only measurement.
///
/// # Safety
/// `h_device` must be a live device handle; `arg` must point at a live
/// `D3D12DDIARG_CREATE_COMMAND_LIST_0040`; `h_list`'s `pDrvPrivate` must address
/// the private block [`calc_private_command_list_size`] sized; `h_rt_list` must
/// be the runtime's handle for this list.
unsafe extern "C" fn create_command_list(
    h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_LIST_0040,
    h_list: ddi12::D3D12DDI_HCOMMANDLIST,
    h_rt_list: ddi12::D3D12DDI_HRTCOMMANDLIST,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) = (unsafe { Slot::<Boxed<CommandListState>>::from_priv(h_list.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.command_list_bad_arg);
        return E_INVALIDARG;
    };
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L2_REFUSALS.command_list_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*arg };

    // `Type` distinguishes DIRECT-style lists from BUNDLE; COMPUTE and COPY are
    // expressed by `QueueFlags`. Bundle is authoritative by itself, while the
    // other DDI type must be translated through the queue flags. The resulting
    // API class is also the class that lazily selects this pool's allocator at
    // reset, so the two engine objects cannot disagree.
    let list_type = if a.Type == ddi12::D3D12DDI_COMMAND_LIST_TYPE_D3D12DDI_COMMAND_LIST_TYPE_BUNDLE
    {
        Some(D3D12_COMMAND_LIST_TYPE_BUNDLE)
    } else if a.Type == ddi12::D3D12DDI_COMMAND_LIST_TYPE_D3D12DDI_COMMAND_LIST_TYPE_DIRECT {
        engine_list_type(a.QueueFlags)
    } else {
        None
    };
    let Some(list_type) = list_type else {
        note_refusal(&L2_REFUSALS.command_list_class_unsupported);
        if let Some(n) = budget(&LIST_LOG) {
            log_error!(
                "CreateCommandList: Type={} QueueFlags={:#x} names no class this driver \
                 backs -> E_INVALIDARG (x{})",
                a.Type,
                a.QueueFlags,
                n + 1,
            );
        }
        return E_INVALIDARG;
    };

    // ⚠ `D3D12DDI_COMMAND_LIST_FLAGS` carries two marker hints
    // (`ENABLE_MARKERS`, `_0010_ENABLE_FULLPIPELINE_MARKERS`) that the **API**
    // `D3D12_COMMAND_LIST_FLAGS` has no counterpart for — it defines only `NONE`.
    // They are debug-tooling hints, so they are dropped and counted rather than
    // refused.
    if a.CommandListFlags != ddi12::D3D12DDI_COMMAND_LIST_FLAGS_D3D12DDI_COMMAND_LIST_FLAG_NONE {
        note_refusal(&L2_REFUSALS.command_list_flags_ignored);
    }

    // SAFETY: device-scope DDI; the borrow lives only until the end of the call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L2_REFUSALS.command_list_no_device);
        return E_FAIL;
    };
    let Some(engine) = dev.engine.d3d12_device() else {
        note_refusal(&L2_REFUSALS.command_list_no_device);
        return E_FAIL;
    };
    // ⭐ `ID3D12Device4` for `CreateCommandList1` — the entry point that returns a
    // **closed** list bound to no allocator, which is the only shape the DDI's
    // create args can describe (module doc). `cast` is a `QueryInterface` on the
    // engine's own object; it costs one vtable call and one AddRef that the
    // returned wrapper releases.
    let engine4 = match engine.cast::<ID3D12Device4>() {
        Ok(d) => d,
        Err(e) => {
            note_refusal(&L2_REFUSALS.command_list_engine_failed);
            if let Some(n) = budget(&LIST_LOG) {
                log_error!(
                    "CreateCommandList: engine has no ID3D12Device4 (CreateCommandList1) \
                     hr={:#010x} (x{})",
                    e.code().0 as u32,
                    n + 1,
                );
            }
            return E_FAIL;
        }
    };

    // SAFETY: all three arguments are by-value scalars; the out-param is the
    // wrapper's own.
    let created = unsafe {
        engine4.CreateCommandList1::<ID3D12GraphicsCommandList>(
            a.NodeMask,
            list_type,
            D3D12_COMMAND_LIST_FLAG_NONE,
        )
    };
    let list = match created {
        Ok(l) => l,
        Err(e) => {
            note_refusal(&L2_REFUSALS.command_list_engine_failed);
            if let Some(n) = budget(&LIST_LOG) {
                log_error!(
                    "CreateCommandList: engine CreateCommandList1(type={}) failed hr={:#010x} \
                     (x{})",
                    list_type.0,
                    e.code().0 as u32,
                    n + 1,
                );
            }
            return E_FAIL;
        }
    };

    // ── The mandatory DDI-table callback ────────────────────────────────────
    // SAFETY: `dev` is the live device borrowed above and `h_rt_list` is the
    // runtime's handle for the command list being created — this call site is
    // inside `pfnCreateCommandList`, which is `set_command_list_ddi_table`'s
    // precondition and the moment the runtime requires the call.
    if !unsafe { set_command_list_ddi_table(dev, h_rt_list) } {
        // ⚠ Not fatal, and that is deliberate: the runtime's own answer to a
        // missing or invalid call is to install its stubs, not to fail the
        // create. Failing here would turn a recoverable mis-wiring into a dead
        // device, and the counter already names it.
        if let Some(n) = budget(&LIST_LOG) {
            log_error!(
                "CreateCommandList: this list will record through the RUNTIME's stub table, not \
                 this driver's (x{})",
                n + 1,
            );
        }
    }

    // SAFETY: the slot lies in the sized private block and is currently null;
    // `store` boxes the state and moves the box into it, so the slot owns both
    // the box and, through it, the single reference `CreateCommandList1`
    // returned.
    unsafe {
        slot.store(CommandListState {
            h_device,
            h_rt_list,
            engine: list,
            list_type,
        });
    }
    S_OK
}

/// Hand the runtime this list's DDI table, from command-list table index 0.
///
/// Returns `false` when the table could not be handed over, which the caller
/// logs — see [`create_command_list`]'s doc for what the runtime then does.
///
/// # Safety
/// `dev` must be a live device and `h_rt_list` the runtime's handle for the
/// command list currently being created.
unsafe fn set_command_list_ddi_table(
    dev: &device12::HeliosD3D12Device,
    h_rt_list: ddi12::D3D12DDI_HRTCOMMANDLIST,
) -> bool {
    // ⭐ Index 0 — the only index `D12-G5` ever saw a driver use. See
    // `create_command_list`'s doc, and `tables12::command_list_rt_table`.
    let handle = tables12::command_list_rt_table(0);
    if handle == 0 {
        note_refusal(&L2_REFUSALS.command_list_rt_table_missing);
        return false;
    }
    if dev.um_callbacks.is_null() {
        note_refusal(&L2_REFUSALS.command_list_ddi_table_cb_missing);
        return false;
    }
    // SAFETY: `um_callbacks` was null-checked in `create_device` and is the
    // runtime's `_0062` table, which outlives the device.
    let Some(cb) = (unsafe { (*dev.um_callbacks).pfnSetCommandListDDITableCb }) else {
        note_refusal(&L2_REFUSALS.command_list_ddi_table_cb_missing);
        return false;
    };
    let h_rt_table = ddi12::D3D12DDI_HRTTABLE {
        handle: handle as *mut c_void,
    };
    // SAFETY: a non-null callback from the runtime's own table, given the
    // runtime's own command-list handle and the `D3D12DDI_HRTTABLE` the runtime
    // itself passed to `pfnFillDDITable` for command-list table index 0.
    unsafe { cb(h_rt_list, h_rt_table) };
    true
}

/// `pfnDestroyCommandList`.
///
/// # Safety
/// `h_list` must be a handle [`create_command_list`] returned `S_OK` for and
/// which has not already been destroyed.
unsafe extern "C" fn destroy_command_list(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_list: ddi12::D3D12DDI_HCOMMANDLIST,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_list`.
    let Some(slot) = (unsafe { Slot::<Boxed<CommandListState>>::from_priv(h_list.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.command_list_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null or the one box `create_command_list`
    // moved in; `take` empties it, so a second destroy is a no-op rather than a
    // double free. Dropping the box releases the engine list's single reference.
    let Some(state) = (unsafe { slot.take() }) else {
        note_refusal(&L2_REFUSALS.command_list_bad_arg);
        return;
    };
    drop(state);
}

// ---------------------------------------------------------------------------
// (d) Command signatures — 3 slots. S-4: the NATIVE classes are implemented;
//     the state-template classes are refused LOUDLY, at create.
// ---------------------------------------------------------------------------

/// What one `D3D12DDI_INDIRECT_ARGUMENT_DESC::Type` means for this driver.
///
/// ⛔ **Four classes, not two, and the split comes from the ENGINE's source rather
/// than from the header.** `d3d12_command_signature_create`
/// (`vkd3d-proton-helios/libs/vkd3d/command.c:26289`) sorts the twelve DDI argument
/// types into *action* commands — which it lowers to a native
/// `vkCmdDraw*Indirect*` / `vkCmdDispatchIndirect` — and everything else, which sets
/// `requires_state_template` and needs `VK_EXT_device_generated_commands`.
///
/// ⚠ No derives: it is produced and matched in one expression, and a `PartialEq`
/// nothing compares would be capability this file does not use.
enum IndirectArgClass {
    /// An action command with a native Vulkan lowering on this guest.
    Action(D3D12_INDIRECT_ARGUMENT_TYPE),
    /// A class that sets vkd3d's `requires_state_template` — root constants, root
    /// descriptors, and the VBV/IBV rebinds. ⛔ **Refused**, see
    /// [`create_command_signature`].
    StateTemplate,
    /// `DISPATCH_RAYS`. An action command *to vkd3d*, but this driver reports no
    /// raytracing tier, so a signature naming it is a caps inconsistency rather
    /// than a capability gap and gets its own counter.
    Raytracing,
    /// A value this build's `d3d12umddi.h` does not name.
    Unknown,
}

/// Classify one DDI indirect-argument type.
///
/// ⛔ **Translated, never cast.** All twelve `D3D12DDI_INDIRECT_ARGUMENT_TYPE`
/// enumerators are value-identical to their `D3D12_INDIRECT_ARGUMENT_TYPE` twins in
/// this SDK — and `DDI_REFERENCE.md` §9.6.1 is the standing evidence that a DDI enum
/// and its API twin can agree on a *value* while disagreeing on its meaning, with
/// the compiler silent because the member types match. Writing the arms out is what
/// makes the agreement something the compiler re-checks when either header moves.
/// ⚠ It also encodes the *classification*, which is not in either header at all.
fn indirect_argument_class(t: ddi12::D3D12DDI_INDIRECT_ARGUMENT_TYPE) -> IndirectArgClass {
    use ddi12::{
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_CONSTANT as DDI_CONSTANT,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_CONSTANT_BUFFER_VIEW as DDI_CBV,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_DISPATCH as DDI_DISPATCH,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_DISPATCH_MESH as DDI_DISPATCH_MESH,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_DISPATCH_RAYS as DDI_DISPATCH_RAYS,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_DRAW as DDI_DRAW,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED as DDI_DRAW_INDEXED,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_INCREMENTING_CONSTANT as DDI_INCR_CONSTANT,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_INDEX_BUFFER_VIEW as DDI_IBV,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_SHADER_RESOURCE_VIEW as DDI_SRV,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_UNORDERED_ACCESS_VIEW as DDI_UAV,
        D3D12DDI_INDIRECT_ARGUMENT_TYPE_D3D12DDI_INDIRECT_ARGUMENT_TYPE_VERTEX_BUFFER_VIEW as DDI_VBV,
    };
    match t {
        DDI_DRAW => IndirectArgClass::Action(D3D12_INDIRECT_ARGUMENT_TYPE_DRAW),
        DDI_DRAW_INDEXED => IndirectArgClass::Action(D3D12_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED),
        DDI_DISPATCH => IndirectArgClass::Action(D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH),
        DDI_DISPATCH_MESH => IndirectArgClass::Action(D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH_MESH),
        DDI_DISPATCH_RAYS => IndirectArgClass::Raytracing,
        // The eight that set `requires_state_template` (`command.c:26350`, `:26356`,
        // `:26363`, `:26371`, `:26377`).
        DDI_CONSTANT | DDI_INCR_CONSTANT | DDI_SRV | DDI_UAV | DDI_CBV | DDI_VBV | DDI_IBV => {
            IndirectArgClass::StateTemplate
        }
        // ⚠ Not an `else` that picks the largest arm (`DECISIONS.md` §7.4): a type
        // this header does not name is refused, never guessed at.
        _ => IndirectArgClass::Unknown,
    }
}

/// Sanity bound on `D3D12DDIARG_CREATE_COMMAND_SIGNATURE_0001::NumArgumentDescs`.
///
/// CLAUDE.md: *validate every runtime-supplied size before reading.* No D3D12 rule
/// caps the count, so this is not a semantic limit — it bounds the loop a corrupt
/// count would run, and its counter says if a real workload ever approached it.
/// ⚠ Signatures this driver *accepts* have exactly one desc; the bound exists for
/// the ones it walks in order to refuse them with the offending type named.
const MAX_INDIRECT_ARGUMENT_DESCS: usize = 256;

/// `pfnCalcPrivateCommandSignatureSize`.
///
/// One machine word: the slot holds a bare owning `ID3D12CommandSignature*`.
/// ⛔ Answered unconditionally, and never 0 — see [`PRIVATE_SLOT_SIZE`]. A 0 would
/// hand the paired create a zero-byte region to write the slot word through.
///
/// # Safety
/// `arg`, when non-null, must point at a live
/// `D3D12DDIARG_CREATE_COMMAND_SIGNATURE_0001` for the duration of the call.
unsafe extern "C" fn calc_private_command_signature_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_SIGNATURE_0001,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L2_REFUSALS.command_signature_bad_arg);
    }
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// The engine `ID3D12CommandSignature` behind a DDI signature handle, borrowed for
/// the caller's DDI call.
///
/// ⭐ **L3a's door into this lane**, and it exists so the payload of
/// `D3D12DDI_HCOMMANDSIGNATURE` is decoded in exactly one place — the
/// `com_handles!` invocation at the top of this file (`ARCHITECTURE.md` §12 rule 7 /
/// R803). `pfnExecuteIndirect` lives in `cmdlist.rs` and needs the object this
/// lane's create built; the same shape [`command_list_state`] takes for
/// `D3D12DDI_HCOMMANDLIST` and `fence::engine_query_heap` takes for
/// `D3D12DDI_HQUERYHEAP`.
///
/// ⚠ A `ManuallyDrop`, i.e. **borrowed**: [`create_command_signature`] moved the one
/// owning reference into the slot and [`destroy_command_signature`] releases it.
///
/// ⛔ A null `pDrvPrivate` and an empty slot both fold to `None`, and the caller
/// cannot tell them apart from here. That is deliberate and safe for this handle:
/// unlike a root signature, there is no legal "the runtime named no command
/// signature" call — `pfnExecuteIndirect` without one is meaningless — so both cases
/// are the same refusal.
///
/// # Safety
/// `h` must be a handle [`create_command_signature`] returned `S_OK` for and
/// [`destroy_command_signature`] has not been called on, and the returned value must
/// not outlive the DDI call that obtained it.
pub(crate) unsafe fn engine_command_signature(
    h: ddi12::D3D12DDI_HCOMMANDSIGNATURE,
) -> Option<core::mem::ManuallyDrop<ID3D12CommandSignature>> {
    // SAFETY: the caller guarantees a live handle, so its slot lies inside the
    // private block `calc_private_command_signature_size` sized.
    let slot = unsafe { Slot::<Com<ID3D12CommandSignature>>::from_priv(h.drv_private()) }?;
    // SAFETY: same precondition; `load` reads the slot word and reports an empty
    // slot as `None` rather than fabricating an interface.
    unsafe { slot.load() }
}

/// `pfnCreateCommandSignature` — **IMPLEMENTED for the four native action classes,
/// refused loudly for everything else.**
///
/// # ⛔⛔ Why a partial implementation is the CORRECT answer here, and a full
/// forward would be the dangerous one
///
/// `VK_EXT_device_generated_commands` is **absent on this guest** (zero occurrences
/// in `docs/dx12/research/guest-vulkaninfo-full.txt`), and vkd3d's response to that
/// is not a failure — it is a **silent downgrade**:
///
/// ```text
///     if ((object->requires_state_template = requires_state_template))
///     {
///         if (!device->device_info.device_generated_commands_features.deviceGeneratedCommands)
///         {
///             FIXME("Device generated commands is not supported by implementation.\n");
///             object->requires_state_template = false;
///             goto out;                       // ← command.c:26447-26453, still S_OK
///         }
/// ```
///
/// and the paired `ExecuteIndirect` then discards the whole call:
///
/// ```text
///     arg_buffer_offset += sig_impl->argument_buffer_offset_for_command;
///     if (sig_impl->argument_buffer_offset_for_command)
///     {
///         d3d12_command_list_debug_mark_label(list, "DGC skip", …);
///         return;                             // ← command.c:17811-17818
///     }
/// ```
///
/// ⇒ **a naive forward turns a loud `E_NOTIMPL` into an empty scene with a score.**
/// That is exactly the failure shape this project has burned sessions on, and it is
/// why the classification lives in the driver rather than being delegated to an
/// engine that answers `S_OK` and then draws nothing.
///
/// ⚠ **And the offset check is not conditional on DGC**, which is why the refusal is
/// keyed on the argument TYPES and not on "does the engine have DGC". Any signature
/// with a non-action argument before its action has a non-zero
/// `argument_buffer_offset_for_command` (`command.c:26306-26383` sets it to the byte
/// offset of the action) and takes the skip above regardless. There is even a
/// pathological middle case — `[CONSTANT{Num32BitValuesToSet: 0}, DRAW]`, whose
/// offset stays 0 — where the draw *would* execute with the root constants silently
/// unapplied. Keying on the types covers that one too.
///
/// # ⛔ The `DDI_REFERENCE.md` §14.2 argument this slot used to make is INVALID
///
/// Its previous doc closed with *"`DDI_REFERENCE.md` §14.2's 99-slot minimum-viable
/// list does not include the command-signature triple"*, and `cmdlist.rs`'s
/// `pfnExecuteIndirect` said the same. ⛔ **§14.0 of that same document forbids that
/// reading in as many words**: *"treat a slot in 99-but-not-70 as 'not exercised
/// yet', never as 'not needed'."* The list was being used as licence for the exact
/// inference it rules out. What actually settles the priority is that every engine
/// with GPU-driven rendering calls `CreateCommandSignature` **at startup**, so an
/// `E_NOTIMPL` here is an init-time failure for a whole class of applications.
///
/// # ⭐ The two blockers the old doc named are both discharged
///
/// * the `D3D12DDI_INDIRECT_ARGUMENT_DESC` → `D3D12_INDIRECT_ARGUMENT_DESC`
///   translation is [`indirect_argument_class`], and for the shapes this driver
///   accepts it is only the `Type` field: an action desc's union arm is unused by
///   both the API and the engine;
/// * `hRootSignature`'s payload is **L6's, declared once, in `pso.rs`**, and
///   `pso::root_signature` is already `pub(crate)`. Reading it from here is one call
///   to that accessor, not a second declaration — `DECISIONS.md` D13 is satisfied,
///   and the old doc's claim that it could not be is stale.
///
/// ⚠ The root signature is **forwarded as given**, including when it is non-null on
/// an action-only signature — a case vkd3d answers `E_INVALIDARG`
/// (`command.c:26421-26425`: *"Command signature does not require root signature"*).
/// Passing `None` instead would make such a call succeed, and nothing semantic would
/// be lost, but it would be this driver silently discarding something the
/// application passed. `CommandSignatureRootSigUnexpected` counts it so the decision
/// can be revisited with evidence rather than by preference.
///
/// # Safety
/// `h_device` must be a live handle from `device12::create_device`; `arg` must point
/// at a live `D3D12DDIARG_CREATE_COMMAND_SIGNATURE_0001` whose `pArgumentDescs`
/// addresses `NumArgumentDescs` readable `D3D12DDI_INDIRECT_ARGUMENT_DESC`s for the
/// call; `h_signature`'s `pDrvPrivate` must address the private block
/// [`calc_private_command_signature_size`] sized.
unsafe extern "C" fn create_command_signature(
    h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_COMMAND_SIGNATURE_0001,
    h_signature: ddi12::D3D12DDI_HCOMMANDSIGNATURE,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) =
        (unsafe { Slot::<Com<ID3D12CommandSignature>>::from_priv(h_signature.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.command_signature_bad_arg);
        return E_INVALIDARG;
    };
    // ⛔ Clear first, so every refusal below leaves a null slot rather than whatever
    // the runtime's allocator left there, and the paired destroy finds `None`.
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L2_REFUSALS.command_signature_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*arg };

    // ⛔ Validate the runtime-supplied count and pointer BEFORE reading the array,
    // per-arm. CLAUDE.md's rule.
    let count = a.NumArgumentDescs as usize;
    if a.pArgumentDescs.is_null() || count == 0 || count > MAX_INDIRECT_ARGUMENT_DESCS {
        note_refusal(&L2_REFUSALS.command_signature_bad_arg);
        if let Some(n) = budget(&QUEUE_LOG) {
            log_error!(
                "CreateCommandSignature: NumArgumentDescs={} pArgumentDescs={:p} -- refused (x{})",
                a.NumArgumentDescs,
                a.pArgumentDescs,
                n + 1,
            );
        }
        return E_INVALIDARG;
    }

    // ⚠ Every desc is classified even though only a one-desc signature can be
    // accepted, so a refusal names the argument type that caused it instead of just
    // the count. That is the difference between a counter that says "some engine
    // wanted GPU-driven rendering" and one that says which class to implement next.
    let mut action: Option<D3D12_INDIRECT_ARGUMENT_TYPE> = None;
    let mut state_template = false;
    let mut raytracing = false;
    let mut unknown: Option<ddi12::D3D12DDI_INDIRECT_ARGUMENT_TYPE> = None;
    for i in 0..count {
        // SAFETY: `pArgumentDescs` is non-null and `i < count == NumArgumentDescs`,
        // so this element is inside the array the DDI declares
        // `_Field_size_(NumArgumentDescs)`.
        let ty = unsafe { (*a.pArgumentDescs.add(i)).Type };
        match indirect_argument_class(ty) {
            IndirectArgClass::Action(api) => action = Some(api),
            IndirectArgClass::StateTemplate => state_template = true,
            IndirectArgClass::Raytracing => raytracing = true,
            IndirectArgClass::Unknown => unknown = Some(ty),
        }
    }

    if let Some(ty) = unknown {
        note_refusal(&L2_REFUSALS.command_signature_arg_type_unknown);
        if let Some(n) = budget(&QUEUE_LOG) {
            log_error!(
                "CreateCommandSignature: D3D12DDI_INDIRECT_ARGUMENT_TYPE {ty} is not named by this \
                 build's header -> E_INVALIDARG (x{})",
                n + 1,
            );
        }
        return E_INVALIDARG;
    }
    if raytracing {
        // ⛔ Coherent with the caps this driver publishes rather than with what the
        // engine could do: `RaytracingTier` is NOT_SUPPORTED, so no raytracing
        // pipeline can exist for an indirect dispatch to reach.
        note_refusal(&L2_REFUSALS.command_signature_raytracing_refused);
        if let Some(n) = budget(&QUEUE_LOG) {
            log_error!(
                "CreateCommandSignature: DISPATCH_RAYS refused -- this driver reports no \
                 raytracing tier (x{})",
                n + 1,
            );
        }
        return E_NOTIMPL;
    }
    // ⛔⛔ THE LOUD REFUSAL S-4 EXISTS FOR. `count != 1` and `state_template` are one
    // condition in practice — vkd3d requires exactly one action and requires it LAST
    // (`command.c:26385-26401`), and every non-action class sets
    // `requires_state_template` — but they are tested together rather than assumed
    // equivalent, because the equivalence is a property of the engine's validator
    // and not of the DDI.
    if state_template || count != 1 || action.is_none() {
        note_refusal(&L2_REFUSALS.command_signature_state_template_refused);
        if let Some(n) = budget(&QUEUE_LOG) {
            log_error!(
                "CreateCommandSignature: {} argument desc(s), stateTemplate={state_template}, \
                 action={} -- this driver backs only a single DRAW / DRAW_INDEXED / DISPATCH / \
                 DISPATCH_MESH desc, because VK_EXT_device_generated_commands is absent on this \
                 guest and vkd3d would accept the signature and then SILENTLY SKIP every \
                 ExecuteIndirect (command.c:17811-17818) -> E_NOTIMPL (x{})",
                count,
                action.is_some(),
                n + 1,
            );
        }
        return E_NOTIMPL;
    }
    // Established by the refusal above.
    let Some(action) = action else {
        note_refusal(&L2_REFUSALS.command_signature_state_template_refused);
        return E_NOTIMPL;
    };

    // SAFETY: this is a device-scope DDI, so the runtime passes a handle
    // `create_device` returned `S_OK` for; the borrow lives only until the end of
    // this call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L2_REFUSALS.command_signature_no_device);
        return E_FAIL;
    };
    let Some(engine) = dev.engine.d3d12_device() else {
        note_refusal(&L2_REFUSALS.command_signature_no_device);
        return E_FAIL;
    };

    // ⚠ `pDrvPrivate` is tested directly rather than through the accessor, because
    // `pso::root_signature` folds "the runtime named none" and "this driver could
    // not resolve one it named" into the same `None` and its own doc says the caller
    // must separate them.
    let root_signature = if a.hRootSignature.pDrvPrivate.is_null() {
        None
    } else {
        note_refusal(&L2_REFUSALS.command_signature_root_sig_unexpected);
        // SAFETY: a non-null `pDrvPrivate` on a root-signature handle the runtime
        // handed this create is a handle L6's `pfnCreateRootSignature` sized and
        // wrote; the borrow does not outlive this call.
        let resolved = unsafe { pso::root_signature(a.hRootSignature) };
        if resolved.is_none() {
            note_refusal(&L2_REFUSALS.command_signature_root_sig_unresolved);
            if let Some(n) = budget(&QUEUE_LOG) {
                log_error!(
                    "CreateCommandSignature: hRootSignature={:p} carries no engine root signature \
                     -> E_INVALIDARG (x{})",
                    a.hRootSignature.pDrvPrivate,
                    n + 1,
                );
            }
            return E_INVALIDARG;
        }
        resolved
    };

    // ⚠ One desc, `Type` translated and the union left zeroed. An action desc has no
    // union arm — the API's `D3D12_INDIRECT_ARGUMENT_DESC_0` members all describe
    // root or buffer-view rebinds — and `Default` zero-fills it, so this is exact
    // rather than a partial copy.
    let api_desc = D3D12_INDIRECT_ARGUMENT_DESC {
        Type: action,
        ..Default::default()
    };
    // ⚠ `ByteStride` and `NodeMask` are forwarded verbatim. The stride's minimum is
    // the engine's own validation (`command.c:26409-26414` refuses a stride below
    // the computed signature size) and duplicating it here would be a second
    // authority that can drift; `NodeMask`'s only legal values on a one-node adapter
    // are 0 and 1 and both mean "the single node" to vkd3d, so narrowing it would
    // hide a multi-node request instead of letting the engine reject it — the same
    // reasoning `fence::create_query_heap` records.
    let desc = D3D12_COMMAND_SIGNATURE_DESC {
        ByteStride: a.ByteStride,
        NumArgumentDescs: 1,
        pArgumentDescs: &api_desc,
        NodeMask: a.NodeMask,
    };
    let mut signature: Option<ID3D12CommandSignature> = None;
    // SAFETY: `desc` and `api_desc` are live locals for the call and `desc`'s
    // `pArgumentDescs` addresses `api_desc`, which outlives it; `root_signature` is
    // a borrowed engine object (or `None`) and the wrapper takes it by reference;
    // `signature` is writable storage the wrapper initialises on success.
    if let Err(e) =
        unsafe { engine.CreateCommandSignature(&desc, root_signature.as_deref(), &mut signature) }
    {
        note_refusal(&L2_REFUSALS.command_signature_engine_failed);
        if let Some(n) = budget(&QUEUE_LOG) {
            log_error!(
                "CreateCommandSignature: engine refused stride={} type={} hr={:#010x} (x{})",
                a.ByteStride,
                action.0,
                e.code().0 as u32,
                n + 1,
            );
        }
        return E_FAIL;
    }
    let Some(signature) = signature else {
        // ⚠ `S_OK` with no object out — the engine breaking its own COM contract.
        // Counted rather than assumed impossible, same as `create_query_heap`.
        note_refusal(&L2_REFUSALS.command_signature_engine_failed);
        return E_FAIL;
    };

    // SAFETY: the slot lies in the sized private block and is currently null
    // (cleared above); `store` moves the single reference the engine returned into
    // it, and `destroy_command_signature` releases it.
    unsafe { slot.store(signature) };
    note_refusal(&L2_REFUSALS.command_signature_created);
    S_OK
}

/// `pfnDestroyCommandSignature`.
///
/// # Safety
/// `h_signature` must be a handle the runtime associated with a
/// `pfnCreateCommandSignature` call on this device.
unsafe extern "C" fn destroy_command_signature(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_signature: ddi12::D3D12DDI_HCOMMANDSIGNATURE,
) {
    // SAFETY: the caller guarantees a handle from `pfnCreateCommandSignature`.
    let Some(slot) =
        (unsafe { Slot::<Com<ID3D12CommandSignature>>::from_priv(h_signature.drv_private()) })
    else {
        note_refusal(&L2_REFUSALS.command_signature_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null — a create this driver refused, which is
    // legal and is what `CommandSignatureDestroyUnexpected` used to count — or the
    // one reference `create_command_signature` moved in. `release` is idempotent on
    // null.
    unsafe { slot.release() };
}
// ---------------------------------------------------------------------------
// Direct A5/HOB1 submission
// ---------------------------------------------------------------------------

/// Report failure of the direct A5/A7 lower submission to the runtime.
/// ExecuteCommandLists has no HRESULT return, so a device-level error callback is
/// the only truthful channel once the queue's exact submission failed.
fn report_direct_submit_error(queue: &QueueState, hr: ddi12::HRESULT) {
    // SAFETY: `h_device` is the device this queue was created against; the borrow
    // lives only until the end of this statement.
    let reported =
        unsafe { device12::device(queue.h_device) }.is_some_and(|dev| device12::set_error(dev, hr));
    if !reported {
        note_refusal(&L2_REFUSALS.queue_set_error_unavailable);
    }
}

// ---------------------------------------------------------------------------
// The command-queue table — 7 slots
// ---------------------------------------------------------------------------

/// `pfnExecuteCommandLists` forwards each ordinary engine list. Every
/// lower-queue forward owns one complete A5 scope and therefore produces one
/// immutable HOB1/HOS1 direct submission; no legacy Render marker is emitted.
/// # Safety
/// `h_queue` must be a live queue handle; `lists` must address `count` readable
/// `D3D12DDI_HCOMMANDLIST`s, each a live handle from [`create_command_list`].
unsafe extern "C" fn execute_command_lists(
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    count: ddi12::UINT,
    lists: *const ddi12::D3D12DDI_HCOMMANDLIST,
) {
    // SAFETY: the caller guarantees a live handle from `create_command_queue`.
    let Some(queue) = (unsafe { queue_state(h_queue) }) else {
        note_refusal(&L2_REFUSALS.execute_command_lists_bad_arg);
        return;
    };
    let n = count as usize;
    if n == 0 {
        // Legal and degenerate: nothing to submit, nothing to report.
        return;
    }
    // ⛔ Validate the runtime-supplied count and pointer BEFORE reading the
    // array. CLAUDE.md's rule, and the bound is `MAX_EXECUTE_COMMAND_LISTS`.
    if lists.is_null() || n > MAX_EXECUTE_COMMAND_LISTS {
        note_refusal(&L2_REFUSALS.execute_command_lists_bad_arg);
        // ⚠ `k`, not `n`: `n` is the list count in this function and shadowing it
        // with a log ordinal is how a line ends up reporting the wrong number.
        if let Some(k) = budget(&ECL_LOG) {
            log_error!(
                "ExecuteCommandLists: Count={count} pCommandLists={lists:p} -- refused (x{})",
                k + 1,
            );
        }
        return;
    }

    // ⚠ The per-list trace detail is formatted only when the trace gate is
    // ALREADY open. This is per-submit traffic and the loop below is on it; a
    // `String` built unconditionally would be R420's cost with none of its
    // evidence.
    let tracing = crate::log::trace_enabled();
    let mut traced_lists = String::new();

    // ⚠ One `AddRef`/`Release` pair per list per submit, on purpose. The engine's
    // wrapper takes `&[Option<ID3D12CommandList>]`, i.e. owned references, while
    // each slot only *lends* one; cloning is the encoding of that difference
    // that cannot be got wrong. The alternative — reinterpreting the slot words
    // as an `Option<Interface>` array — is a layout assumption about someone
    // else's crate on a path nobody has measured.
    let mut engine_lists: Vec<Option<ID3D12CommandList>> = Vec::with_capacity(n);
    for i in 0..n {
        // SAFETY: `lists` is non-null and `i < n <= count`, so this element is
        // inside the array the DDI declares `_In_reads_(Count)`.
        let h = unsafe { *lists.add(i) };
        // SAFETY: the caller guarantees each entry is a live handle from
        // `create_command_list`, so its slot lies in the sized private block.
        let state = unsafe { command_list_state(h) };
        let Some(state) = state else {
            note_refusal(&L2_REFUSALS.execute_command_lists_list_missing);
            // ⚠ `k`, not `n` — see the refusal above.
            if let Some(k) = budget(&ECL_LOG) {
                log_error!(
                    "ExecuteCommandLists: entry {i} of {count} carries no engine command list \
                     (x{})",
                    k + 1,
                );
            }
            return;
        };
        // `ID3D12GraphicsCommandList` derefs to its COM base `ID3D12CommandList`
        // (single inheritance, the `windows` crate's own `interface_hierarchy!`);
        // `clone` is the AddRef the vector's drop then balances.
        engine_lists.push(Some((**state.engine()).clone()));
        if tracing {
            // ⭐ Both identities, because neither alone answers the question
            // `tmp/dx12/gates/G8-r0/RESULT.md` asked and never got an instrument
            // for: `pDrvPrivate` ties this entry back to the `CreateCommandList`
            // / `ResetCommandList` / `CloseCommandList` lines for the SAME list,
            // and the engine pointer is what a vkd3d log names. Without the
            // pair, "the recorded commands reached the submitted list" is an
            // inference across two logs that do not share a vocabulary.
            // ⚠ The deref is to the COM base `ID3D12CommandList` — the exact
            // interface pushed above — so the printed pointer is the one handed
            // to the engine, not a sibling QI of it.
            traced_lists.push_str(&format!(
                " [{i}]priv={:p},list={:p}",
                h.drv_private(),
                (**state.engine()).as_raw(),
            ));
        }
    }

    if tracing {
        if let Some(runtime) = queue.outer.runtime.get() {
            trace_line!(
                "ExecuteCommandLists: Count={count} queue={:p} ctx={:p} endpoint={} \
                 generation={}{}",
                queue.engine_queue.as_raw(),
                runtime.h_context.as_ptr(),
                runtime.endpoint_id,
                runtime.context_generation,
                traced_lists,
            );
        }
    }

    // SAFETY: `engine_lists` is a live slice of owned interfaces for the whole
    // call, and `engine_queue` is the live queue this state owns.
    // Record-only vkd3d executes this call synchronously on the ECL thread.
    // Submit one command list at a time: each lower queue submit owns exactly
    // one A5 scope and therefore produces exactly one immutable HOB1 batch.
    for engine_list in &engine_lists {
        unsafe {
            queue
                .engine_queue
                .ExecuteCommandLists(core::slice::from_ref(engine_list));
        }
        L2_REFUSALS.command_lists_forwarded.bump();
        if queue
            .outer
            .device_lost
            .load(std::sync::atomic::Ordering::Acquire)
            != 0
        {
            report_direct_submit_error(queue, E_FAIL);
            break;
        }
    }
}

/// Reserved queue-table slot 1: counted rather than left null.
unsafe extern "C" fn queue_unused_slot() {
    note_refusal(&L2_REFUSALS.queue_unused_slot_called);
}

/// `pfnUnused2` — queue-table slot 2. Separate function, separate counter, for
/// the one reason that matters: a shared body could not say *which* of the two
/// the runtime called, and that is the entire content of the observation.
unsafe extern "C" fn queue_unused2_slot() {
    note_refusal(&L2_REFUSALS.queue_unused2_slot_called);
}

/// `pfnUpdateTileMappings` — **REFUSED**, `TileMappingsRefused`.
///
/// ⛔ This driver reports `TiledResourcesTier = NOT_SUPPORTED` (`caps12.rs`), so
/// no tiled resource can exist for this DDI to remap. Refusing is the coherent
/// answer, and it is the same shape `caps12::get_mip_packing` takes for the same
/// reason: counted, and deliberately **not** raised through `pfnSetErrorCb`,
/// because a hit means a caps inconsistency somewhere else and removing the
/// device would not fix it. ⚠ No log line either: the counter is the readout,
/// and this DDI is per-remap traffic that a budgeted line would only half cover.
///
/// ⚠ **`DX12.md` §4.4 makes `TiledResourcesTier >= 2` a feature-level 12_1
/// floor**, and it lands with these two slots plus `pfnCopyTiles`,
/// `pfnGetMipPacking` and the reserved-resource arm of `pfnCreateHeapAndResource`.
/// ⛔ The tier is **UMD-only** — Vulkan sparse binding, which the guest supports
/// end to end (`DECISIONS.md` §2) — and **not** a KMD dependency. That claim was
/// made twice and falsified twice; do not cost the feature level as if the KMD
/// were on its critical path.
///
/// # Safety
/// The arguments are the runtime's and this body reads none of them.
unsafe extern "C" fn update_tile_mappings(
    _h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    _h_resource: ddi12::D3D12DDI_HRESOURCE,
    _num_regions: ddi12::UINT,
    _region_start_coords: *const ddi12::D3D12DDI_TILED_RESOURCE_COORDINATE,
    _region_sizes: *const ddi12::D3D12DDI_TILE_REGION_SIZE,
    _h_heap: ddi12::D3D12DDI_HHEAP,
    _num_ranges: ddi12::UINT,
    _range_flags: *const ddi12::D3D12DDI_TILE_RANGE_FLAGS,
    _heap_start_offsets: *const ddi12::UINT,
    _range_tile_counts: *const ddi12::UINT,
    _flags: ddi12::D3D12DDI_TILE_MAPPING_FLAGS,
) {
    note_refusal(&L2_REFUSALS.tile_mappings_refused);
}

/// `pfnCopyTileMappings` — **REFUSED**, `TileMappingsRefused`. Same reasoning as
/// [`update_tile_mappings`]; same counter, because the two are one capability.
///
/// # Safety
/// The arguments are the runtime's and this body reads none of them.
unsafe extern "C" fn copy_tile_mappings(
    _h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    _h_dst_resource: ddi12::D3D12DDI_HRESOURCE,
    _dst_start_coord: *const ddi12::D3D12DDI_TILED_RESOURCE_COORDINATE,
    _h_src_resource: ddi12::D3D12DDI_HRESOURCE,
    _src_start_coord: *const ddi12::D3D12DDI_TILED_RESOURCE_COORDINATE,
    _region_size: *const ddi12::D3D12DDI_TILE_REGION_SIZE,
    _flags: ddi12::D3D12DDI_TILE_MAPPING_FLAGS,
) {
    note_refusal(&L2_REFUSALS.tile_mappings_refused);
}

/// What a fence operation is: the two queue slots differ only in which engine
/// method they reach, so they share [`fence_operation`] and name themselves here.
#[derive(Clone, Copy)]
enum FenceOp {
    Signal,
    Wait,
}

impl FenceOp {
    fn name(self) -> &'static str {
        match self {
            FenceOp::Signal => "SignalFence",
            FenceOp::Wait => "WaitForFence",
        }
    }
}

/// Queue one Core-0116 native-fence operation on the exact outer WDDM context.
///
/// Any active record-only scope is sealed and actually submitted first. The
/// application fence is then named only by its callback-returned local
/// `hSyncObject`; vkd3d receives no wait/signal and owns no shadow timeline.
///
/// # Safety
/// `h_queue` and `op_arg` must be live runtime-owned DDI values.
unsafe fn fence_operation(
    which: FenceOp,
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    op_arg: *mut ddi12::D3D12DDIARG_FENCE_OPERATION,
) {
    note_refusal(match which {
        FenceOp::Signal => &L2_REFUSALS.fence_signal_entered,
        FenceOp::Wait => &L2_REFUSALS.fence_wait_entered,
    });
    if op_arg.is_null() {
        note_refusal(&L2_REFUSALS.fence_op_bad_arg);
        return;
    }
    let op = unsafe { &mut *op_arg };
    op.PhysicalAdapterMask = 1;
    let Some(queue) = (unsafe { queue_state(h_queue) }) else {
        note_refusal(&L2_REFUSALS.fence_op_bad_arg);
        return;
    };
    let Some(fence) = (unsafe { fence::fence_state(op.Fence) }) else {
        note_refusal(&L2_REFUSALS.fence_op_fence_missing);
        return;
    };
    if !fence.belongs_to(queue.h_device) || op.Value == u64::MAX {
        note_refusal(&L2_REFUSALS.fence_op_fence_missing);
        return;
    }
    let Some(runtime) = queue.outer.runtime.get() else {
        note_refusal(&L2_REFUSALS.fence_op_bad_arg);
        return;
    };
    let Some(dev) = (unsafe { device12::device(queue.h_device) }) else {
        note_refusal(&L2_REFUSALS.fence_op_bad_arg);
        return;
    };
    if queue
        .outer
        .device_lost
        .load(std::sync::atomic::Ordering::Acquire)
        != 0
        || dev.kt_callbacks.is_null()
    {
        note_refusal(&L2_REFUSALS.fence_op_engine_failed);
        let _ = device12::set_error(dev, E_FAIL);
        return;
    }

    let mut active = lock_ignore_poison(&runtime.active_scope);
    let _submit_guard = lock_ignore_poison(&runtime.submit_lock);
    if let Some(scope) = active.take() {
        if unsafe { submit_outer_scope12(dev, queue.outer.as_ref(), runtime, scope, None) }.is_err()
        {
            note_refusal(&L2_REFUSALS.fence_op_engine_failed);
            mark_outer_lost(queue.outer.as_ref(), "native fence pre-submit");
            let _ = device12::set_error(dev, E_FAIL);
            return;
        }
    }
    drop(active);

    let sync = fence.h_sync_object();
    let hr = match which {
        FenceOp::Signal => {
            let Some(callback) =
                (unsafe { (*dev.kt_callbacks).pfnSignalSynchronizationObjectFromGpuCb })
            else {
                note_refusal(&L2_REFUSALS.fence_op_engine_failed);
                let _ = device12::set_error(dev, E_FAIL);
                return;
            };
            let mut args = ddi12::D3DDDICB_SIGNALSYNCHRONIZATIONOBJECTFROMGPU::default();
            args.hContext = runtime.h_context.as_ptr();
            args.ObjectCount = 1;
            args.ObjectHandleArray = &sync;
            args.__bindgen_anon_1.MonitoredFenceValueArray = &op.Value;
            unsafe { callback(dev.h_rt_device.handle, &args) }
        }
        FenceOp::Wait => {
            let Some(callback) =
                (unsafe { (*dev.kt_callbacks).pfnWaitForSynchronizationObjectFromGpuCb })
            else {
                note_refusal(&L2_REFUSALS.fence_op_engine_failed);
                let _ = device12::set_error(dev, E_FAIL);
                return;
            };
            let mut args = ddi12::D3DDDICB_WAITFORSYNCHRONIZATIONOBJECTFROMGPU::default();
            args.hContext = runtime.h_context.as_ptr();
            args.ObjectCount = 1;
            args.ObjectHandleArray = &sync;
            args.__bindgen_anon_1.MonitoredFenceValueArray = &op.Value;
            unsafe { callback(dev.h_rt_device.handle, &args) }
        }
    };
    if hr < 0 {
        note_refusal(&L2_REFUSALS.fence_op_engine_failed);
        mark_outer_lost(queue.outer.as_ref(), which.name());
        if !device12::set_error(dev, hr) {
            note_refusal(&L2_REFUSALS.queue_set_error_unavailable);
        }
    } else {
        note_refusal(match which {
            FenceOp::Signal => &L2_REFUSALS.fence_signal_forwarded,
            FenceOp::Wait => &L2_REFUSALS.fence_wait_forwarded,
        });
    }
}

/// `pfnSignalFence`.
///
/// # Safety
/// As [`fence_operation`].
unsafe extern "C" fn signal_fence(
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    op_arg: *mut ddi12::D3D12DDIARG_FENCE_OPERATION,
) {
    // SAFETY: forwarded unchanged; the caller's guarantee is `fence_operation`'s.
    unsafe { fence_operation(FenceOp::Signal, h_queue, op_arg) }
}

/// `pfnWaitForFence`.
///
/// # Safety
/// As [`fence_operation`].
unsafe extern "C" fn wait_for_fence(
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    op_arg: *mut ddi12::D3D12DDIARG_FENCE_OPERATION,
) {
    // SAFETY: forwarded unchanged; the caller's guarantee is `fence_operation`'s.
    unsafe { fence_operation(FenceOp::Wait, h_queue, op_arg) }
}

/// Exact Core command-queue fence handlers installed below. U11 derives the
/// native-fence cap from these typed values and the Core-0116 create handler,
/// avoiding a second feature flag that could drift from the dispatch tables.
pub(crate) const NATIVE_FENCE_SIGNAL_HANDLER: ddi12::PFND3D12DDI_SIGNAL_FENCE = Some(signal_fence);
pub(crate) const NATIVE_FENCE_WAIT_HANDLER: ddi12::PFND3D12DDI_WAIT_FOR_FENCE =
    Some(wait_for_fence);

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Install L2's 17 device-core slots.
///
/// Chain position: `CapsSlots` -> `QueueSlots` on the device-core table.
pub(crate) fn install_core(
    mut filling: Filling<'_, DeviceCoreTable, stage::CapsSlots>,
) -> Filling<'_, DeviceCoreTable, stage::QueueSlots> {
    let table = filling.table();
    // command queues — 3
    table.pfnCalcPrivateCommandQueueSize = Some(calc_private_command_queue_size);
    table.pfnCreateCommandQueue = Some(create_command_queue);
    table.pfnDestroyCommandQueue = Some(destroy_command_queue);
    // command pools — 4
    table.pfnCalcPrivateCommandPoolSize = Some(calc_private_command_pool_size);
    table.pfnCreateCommandPool = Some(create_command_pool);
    table.pfnDestroyCommandPool = Some(destroy_command_pool);
    table.pfnResetCommandPool = Some(reset_command_pool);
    // command lists — 3
    table.pfnCalcPrivateCommandListSize = Some(calc_private_command_list_size);
    table.pfnCreateCommandList = Some(create_command_list);
    table.pfnDestroyCommandList = Some(destroy_command_list);
    // command recorders — 4
    table.pfnCalcPrivateCommandRecorderSize = Some(calc_private_command_recorder_size);
    table.pfnCreateCommandRecorder = Some(create_command_recorder);
    table.pfnDestroyCommandRecorder = Some(destroy_command_recorder);
    table.pfnCommandRecorderSetCommandPoolAsTarget =
        Some(command_recorder_set_command_pool_as_target);
    // command signatures — 3, refused
    table.pfnCalcPrivateCommandSignatureSize = Some(calc_private_command_signature_size);
    table.pfnCreateCommandSignature = Some(create_command_signature);
    table.pfnDestroyCommandSignature = Some(destroy_command_signature);
    filling.advance()
}

/// Install all 7 command-queue slots.
///
/// Chain position: `Stubbed` -> `QueueSlots` on the command-queue table.
pub(crate) fn install_queue(
    mut filling: Filling<'_, CommandQueueTable, stage::Stubbed>,
) -> Filling<'_, CommandQueueTable, stage::QueueSlots> {
    let table = filling.table();
    table.pfnExecuteCommandLists = Some(execute_command_lists);
    // ⚠ These two are `void*` in the header, not a typed `Option<fn>`, so the
    // compiler cannot check them against a signature — there is none. The cast
    // is what a bare `void*` slot requires; see `queue_unused_slot`'s doc for why
    // a counting stub rather than a NULL.
    table.pfnUnused = queue_unused_slot as *mut c_void;
    table.pfnUnused2 = queue_unused2_slot as *mut c_void;
    table.pfnUpdateTileMappings = Some(update_tile_mappings);
    table.pfnCopyTileMappings = Some(copy_tile_mappings);
    table.pfnSignalFence = NATIVE_FENCE_SIGNAL_HANDLER;
    table.pfnWaitForFence = NATIVE_FENCE_WAIT_HANDLER;
    filling.advance()
}

// ---------------------------------------------------------------------------
// Refusal counters
// ---------------------------------------------------------------------------

/// L2's refusal counters. One instance, [`L2_REFUSALS`]; the set that prints
/// them is [`REFUSALS`].
pub(crate) struct L2Refusals {
    /// A queue slot was called with a null arg or a null `pDrvPrivate`, or a
    /// destroy hit an already-empty slot. **Expected 0.**
    queue_bad_arg: RefusalCounter,
    /// A queue slot could not reach the engine: the `hDevice` did not resolve, or
    /// the bridge carries no `ID3D12Device`. **Expected 0.**
    queue_no_device: RefusalCounter,
    /// `D3D12DDIARG_CREATECOMMANDQUEUE_0050::QueueFlags` named no 3D/COMPUTE/COPY
    /// class, so the create was refused. **Expected 0** on a graphics workload; a
    /// hit means an application asked for a paging or video queue, which this
    /// driver's caps do not offer.
    queue_class_unsupported: RefusalCounter,
    /// `ID3D12Device::CreateCommandQueue` on the engine failed.
    queue_engine_failed: RefusalCounter,
    /// A queue asked for `GLOBAL_REALTIME_PRIORITY` and did not get it.
    /// ⚠ May legitimately be non-zero; it is a scheduling hint against a
    /// software-scheduled adapter with one engine node.
    queue_creation_flags_ignored: RefusalCounter,
    /// A queue named a scheduling group and was created outside it. ⚠ Expected
    /// non-zero only once L9's `pfnCreateSchedulingGroup` stops being a counting
    /// noop; until then no group object exists to join.
    queue_scheduling_group_ignored: RefusalCounter,
    /// A queue asked for a `NodeMask` beyond the single node Helios advertises.
    /// ⛔ Expected 0 — a hit means `ARCHITECTURE.md` §13 UNVERIFIED-11's
    /// multi-adapter surface has been reached for real.
    queue_node_mask_ignored: RefusalCounter,
    /// The corelayer `pfnCreateContextCb` was missing, failed, or returned `S_OK`
    /// with a null `hContext`, and the queue create was **failed**.
    ///
    /// ⛔ **Expected 0, and this is the counter to read first if D3D12 dies at
    /// `CreateCommandQueue`.** The WDDM context can be minted nowhere else (the
    /// runtime enforces it: *"CreateContextCb or CreateContextVirtualCb called
    /// outside of queue creation."*), and a queue without one can never present
    /// or submit — so this lane fails loudly rather than handing back a queue
    /// that looks alive. The softening edit, if a gate ever needs it, is to log
    /// and continue with a null `h_context` instead of returning here.
    queue_context_failed: RefusalCounter,
    /// The corelayer `pfnDestroyContextCb` was missing or failed at queue
    /// teardown, so a WDDM context was leaked. **Expected 0.**
    queue_context_destroy_failed: RefusalCounter,
    /// A queue-table slot needed `pfnSetErrorCb` and there was none. **Expected
    /// 0** — it is the first member of `D3D12DDI_CORELAYER_DEVICECALLBACKS_0062`
    /// and the only error channel a `VOID` DDI has, so a hit means an error the
    /// runtime will never learn about.
    queue_set_error_unavailable: RefusalCounter,
    /// A pool slot was called with a null arg or a null `pDrvPrivate`, or a
    /// destroy hit an already-empty slot. **Expected 0.**
    pool_bad_arg: RefusalCounter,
    /// A pool slot could not reach the engine. **Expected 0.**
    pool_no_device: RefusalCounter,
    /// `ID3D12Device::CreateCommandAllocator` failed at a class's first reset,
    /// so that pool has no backing allocator for the class. **Expected 0.**
    pool_allocator_engine_failed: RefusalCounter,
    /// ⛔ **RETIRED.** This counted a recorder class disagreeing with the single
    /// allocator formerly attached to a pool. Pools now own one allocator slot
    /// per list class and the list selects it at reset, so nothing increments
    /// this counter. It remains only to preserve the append-only evidence line.
    pool_type_mismatch: RefusalCounter,
    /// `pfnResetCommandPool` ran before any class allocator had been created.
    /// ⚠ May legitimately be non-zero: a pool created and reset before any
    /// recording is a no-op, not a fault.
    pool_reset_no_allocator: RefusalCounter,
    /// `ID3D12CommandAllocator::Reset` failed. ⚠ May legitimately be non-zero:
    /// D3D12 requires the GPU to be done with the allocator's lists first, and
    /// that is the application's obligation, not the driver's.
    pool_reset_engine_failed: RefusalCounter,
    /// A recorder slot was called with a null arg or a null `pDrvPrivate`, or a
    /// destroy hit an already-empty slot. **Expected 0.**
    recorder_bad_arg: RefusalCounter,
    /// `D3D12DDIARG_CREATE_COMMAND_RECORDER_0040::QueueFlags` named no
    /// 3D/COMPUTE/COPY class. **Expected 0**, same reasoning as
    /// `QueueClassUnsupported`.
    recorder_class_unsupported: RefusalCounter,
    /// A command-list slot was called with a null arg or a null `pDrvPrivate`,
    /// **or a destroy hit an already-empty slot**. **Expected 0.**
    ///
    /// ⚠ The second condition was added when `pfnDestroyCommandList` started
    /// taking a box rather than releasing a bare COM word, and it is the same
    /// second condition `PoolBadArg` and `RecorderBadArg` already document — this
    /// was the one of the three that was missed. It fires when a create FAILED
    /// (leaving the slot cleared) and the runtime then destroyed the handle
    /// anyway, which is legal. ⇒ **read `CommandListEngineFailed`,
    /// `CommandListClassUnsupported` and `L2BundleListRefused` first**: a hit
    /// here with one of those non-zero is the runtime cleaning up after a refused
    /// create, not a bad pointer.
    command_list_bad_arg: RefusalCounter,
    /// A command-list slot could not reach the engine. **Expected 0.**
    command_list_no_device: RefusalCounter,
    /// `D3D12DDIARG_CREATE_COMMAND_LIST_0040`'s `Type`/`QueueFlags` pair named no
    /// class this driver backs. **Expected 0.**
    command_list_class_unsupported: RefusalCounter,
    /// The engine has no `ID3D12Device4`, or `CreateCommandList1` failed.
    ///
    /// ⛔ **Expected 0, and the first thing to read if D3D12 dies at
    /// `CreateCommandList`.** `CreateCommandList1` is the only entry point that
    /// produces a closed list bound to no allocator, which is the only shape the
    /// DDI's create args can describe (module doc). vkd3d-proton implements
    /// `ID3D12Device4`; that it does is UNVERIFIED here, because the engine
    /// submodule is not checked out on the host that wrote this lane.
    command_list_engine_failed: RefusalCounter,
    /// A command list carried `D3D12DDI_COMMAND_LIST_FLAGS` marker hints the API
    /// enum has no counterpart for, and they were dropped. ⚠ Expected non-zero
    /// under a debug layer or a PIX capture; they are tooling hints, not
    /// behaviour.
    command_list_flags_ignored: RefusalCounter,
    /// `tables12::command_list_rt_table(0)` was still 0 at command-list creation,
    /// so `pfnSetCommandListDDITableCb` could not be called with a valid handle.
    ///
    /// ⛔ **Expected 0, and a hit is not cosmetic**: the runtime's answer is to
    /// install *its own* stubs over the list, so every recording DDI silently
    /// bypasses this driver and the list records nothing. The handle cannot be
    /// recovered any other way (`DDI_REFERENCE.md` §2.2).
    command_list_rt_table_missing: RefusalCounter,
    /// The corelayer `pfnSetCommandListDDITableCb` was missing. **Expected 0** —
    /// it is the third member of `D3D12DDI_CORELAYER_DEVICECALLBACKS_0062` and
    /// the runtime requires the call (strings:30). Same consequence as
    /// `CommandListRtTableMissing`.
    command_list_ddi_table_cb_missing: RefusalCounter,
    /// A command-signature slot was called with a null arg or a null
    /// `pDrvPrivate`. **Expected 0.**
    command_signature_bad_arg: RefusalCounter,
    /// ⛔ **RETIRED BY S-4, and kept only so the `D3D12 DDI refusals:` line does not
    /// shift.** It counted the blanket `pfnCreateCommandSignature` → `E_NOTIMPL`;
    /// that blanket refusal is gone, replaced by four per-cause counters
    /// (`CommandSignatureStateTemplateRefused`, `…RaytracingRefused`,
    /// `…ArgTypeUnknown`, `…EngineFailed`) plus the success counter
    /// `CommandSignatureCreated`.
    ///
    /// ⛔ **Expected 0 forever now.** Nothing increments it. It is not deleted
    /// because removing an entry from [`REFUSALS`] shifts every counter after it,
    /// and that array is the evidence contract diffed across builds — the same rule
    /// that forces new counters to the end.
    command_signature_refused: RefusalCounter,
    /// ⛔ **RE-GRADED BY S-4.** It used to mean *"the runtime destroyed a signature
    /// after the create refused it"* and was expected to track
    /// `CommandSignatureRefused`. `pfnDestroyCommandSignature` now releases a real
    /// engine object, so that arm folded into the destroy's idempotent
    /// `Slot::release` and this counter is **no longer incremented at all**.
    ///
    /// ⛔ **Expected 0 forever.** Kept for the append-only reason above. ⚠ A destroy
    /// after a refused create is still legal and still silent — the slot was cleared
    /// by the create, so `release` finds null and does nothing.
    command_signature_destroy_unexpected: RefusalCounter,
    /// ⛔ **RETIRED.** Bundle lists now create normally and select a BUNDLE
    /// allocator from the bound pool at reset. Nothing increments this counter;
    /// it remains only to preserve the append-only evidence line.
    bundle_list_refused: RefusalCounter,
    /// `pfnExecuteCommandLists` was called with an unresolvable queue, a null
    /// array, or a count above `MAX_EXECUTE_COMMAND_LISTS`. **Expected 0.**
    execute_command_lists_bad_arg: RefusalCounter,
    /// An entry of an `pfnExecuteCommandLists` array carried no engine command
    /// list, and the whole submit was refused rather than partially forwarded.
    /// **Expected 0** — a list the runtime submits is a list this driver created.
    execute_command_lists_list_missing: RefusalCounter,
    /// The queue table's `pfnUnused` was actually called. ⛔ **Expected 0** — the
    /// header names it unused (`DDI_REFERENCE.md` §14.1.1 classifies it
    /// RESERVED). A hit is the header being wrong, which is exactly what the stub
    /// exists to turn into a number.
    queue_unused_slot_called: RefusalCounter,
    /// The queue table's `pfnUnused2` was actually called. **Expected 0**, same
    /// reasoning.
    queue_unused2_slot_called: RefusalCounter,
    /// `pfnUpdateTileMappings` or `pfnCopyTileMappings` was called and refused.
    ///
    /// ⛔ **Expected 0** while `caps12` reports `TiledResourcesTier =
    /// NOT_SUPPORTED`: a hit means the runtime reached a tiled-resource path on a
    /// driver that advertises no tier, which is a caps inconsistency elsewhere.
    /// ⚠ It becomes the *implementation* marker for `DX12.md` §4.4's feature
    /// level 12_1 floor, which needs `TiledResourcesTier >= 2`.
    tile_mappings_refused: RefusalCounter,
    /// `pfnSignalFence` / `pfnWaitForFence` with a null
    /// `D3D12DDIARG_FENCE_OPERATION` or an unresolvable queue. **Expected 0.**
    fence_op_bad_arg: RefusalCounter,
    /// A fence operation named a `D3D12DDI_HFENCE` with no `fence::FenceState`
    /// behind it. **Expected 0** — L7's `pfnCreateFence` either stores one or
    /// fails.
    fence_op_fence_missing: RefusalCounter,
    /// `ID3D12CommandQueue::Signal` or `::Wait` on the engine failed, and the
    /// failure was raised to the runtime through `pfnSetErrorCb`. **Expected 0.**
    fence_op_engine_failed: RefusalCounter,
    /// `pfnWaitForFence` asked for a `Value` above the watermark **on a fence this
    /// driver HAS signalled**, so real cross-queue ordering was dropped — and the
    /// drop was raised to the runtime through `pfnSetErrorCb`, which removes the
    /// `ID3D12Device`.
    ///
    /// ⛔⛔ **Expected 0, and RE-GRADED: this counter used to absorb three cases and
    /// was graded *"may legitimately be non-zero"*.** It no longer does. The benign
    /// two moved to `FenceWaitRuntimeOwned`, and what is left is the one case with no
    /// honest reading: this driver is on that fence's engine timeline and is being
    /// asked for a value it has not issued. Forwarding would block the vkd3d queue
    /// forever; dropping silently produces wrong pixels. ⇒ it is loud.
    ///
    /// ⛔ The old grading also named a fix that **cannot exist** — *"until §10.4's
    /// `pfnWaitForSynchronizationObjectFromGpuCb` half exists"*. §10.4's own
    /// correction block (`DDI_REFERENCE.md:2306-2331`) struck that design because no
    /// such callback can name a `D3D12DDI_HFENCE`, and `KMD_IMPACT.md` §14a.5
    /// forbids it. Nothing pending closes this gap; `fence.rs`'s module doc has what
    /// would.
    fence_wait_not_forwarded: RefusalCounter,
    /// `pfnSignalFence` reached its engine forward and
    /// `ID3D12CommandQueue::Signal` returned success.
    ///
    /// ⚠ **Expected NON-ZERO once any D3D12 workload signals a fence.**
    ///
    /// ⛔ **RE-GRADED: a zero here does NOT mean "the runtime never enters this
    /// slot".** That inference was in this doc and it was unsound — a zero is equally
    /// consistent with entering and being diverted by `FenceOpBadArg`,
    /// `FenceOpFenceMissing` or `FenceOpEngineFailed`, all three of which are shared
    /// with `pfnWaitForFence` and so cannot be attributed to a direction.
    /// **`FenceSignalEntered` is the counter that answers it**, and it was added
    /// (S-2) precisely because this one was being read as if it did. `METHOD.md` §5,
    /// *"trusting a zero"*.
    ///
    /// ⚠ `DDI_REFERENCE.md` §14.0's WARP reading — WARP never entering this slot
    /// across 20 frames of `ID3D12CommandQueue::Signal` + `SetEventOnCompletion` — is
    /// still the expected shape, and still not evidence either way about this driver.
    ///
    /// ⭐ **Why a success counter exists at all**, against this file's own
    /// convention that counters name refusals: until it did, `pfnSignalFence`
    /// succeeding was **invisible**. `fence_operation` incremented nothing and
    /// logged nothing on its success path, so `tmp/dx12/gates/G8-r0/RESULT.md`'s
    /// claim that *"the queue-table `Signal` path ran"* rested on three **zero**
    /// readings — `FenceOpEngineFailed`, `FenceOpBadArg`, `FenceWaitNotForwarded`
    /// — and this project has twice written, in this very file
    /// (`queue.rs`'s `fence_operation` doc) and in `fence.rs:61-64`, that a zero
    /// reading is not evidence a path works.
    fence_signal_forwarded: RefusalCounter,
    /// `pfnWaitForFence` reached its engine forward and
    /// `ID3D12CommandQueue::Wait` returned success. Same grading and the same
    /// reason for existing as [`Self::fence_signal_forwarded`].
    ///
    /// ⚠ Expected non-zero only on a workload that waits on a value this driver
    /// has itself signalled: everything above that watermark is diverted by
    /// `FenceWaitRuntimeOwned` (benign) or `FenceWaitNotForwarded` (loud) before it
    /// can reach the forward. ⇒ read all three together —
    /// `FenceWaitForwarded = 0` with `FenceWaitRuntimeOwned > 0` is the shadow-fence
    /// policy working on runtime-owned timelines, whereas
    /// `FenceWaitNotForwarded > 0` is dropped ordering and removes the device.
    /// ⛔ And read `FenceWaitEntered` first: only that one distinguishes any of this
    /// from a slot the runtime never enters.
    fence_wait_forwarded: RefusalCounter,
    /// Command lists forwarded to the lower engine queue. This counts ordinary
    /// direct A5/A7 submissions and has no retired WDDM-marker meaning.
    command_lists_forwarded: RefusalCounter,
    /// `Umd12FenceSignalDelayUs` was non-zero and `pfnSignalFence` slept before
    /// returning. **Expected 0** on any run that did not deliberately set the
    /// knob; see `knobs12::UMD12_FENCE_SIGNAL_DELAY_US` for the question it is
    /// attached to. ⚠ That doc used to name "the commit that must delete it" —
    /// K-F1 was that commit and deliberately kept both delay arms; they retire
    /// with `KMD_IMPACT.md` §14a.1's UV1 instead.
    fence_signal_delayed: RefusalCounter,
    /// ⭐ **S-2's entry instrument: the runtime entered `pfnSignalFence`.** Counted as
    /// the function's first statement, above the null-argument check, so it counts
    /// *entries* and not successes.
    ///
    /// ⛔ **This is the number that settles *"`pfnSignalFence` is never called"***
    /// (`KMD_IMPACT.md` §14a.5, the 83rd session). That claim rested on
    /// `FenceSignalForwarded = 0` plus *"no trace line ever emitted"* — and the trace
    /// line is gated on `Umd12Trace`, **off by default**, so half the evidence did not
    /// exist on a default run. The other counters cannot substitute: `FenceOpBadArg`,
    /// `FenceOpFenceMissing` and `FenceOpEngineFailed` are shared between the two
    /// directions, so no arithmetic over them recovers a per-direction entry count.
    ///
    /// ⚠ `FenceSignalEntered > 0` with `FenceSignalForwarded == 0` is a completely
    /// different finding from both being 0, and before this counter the two were
    /// indistinguishable.
    fence_signal_entered: RefusalCounter,
    /// ⭐ **S-2's entry instrument for `pfnWaitForFence`.** Same construction and the
    /// same reason as [`Self::fence_signal_entered`].
    ///
    /// ⚠ Its partition is exact and worth checking as arithmetic:
    /// `FenceWaitEntered == FenceWaitForwarded + FenceWaitRuntimeOwned +
    /// FenceWaitNotForwarded +` (that direction's share of `FenceOpBadArg` /
    /// `FenceOpFenceMissing` / `FenceOpEngineFailed`). A run where
    /// `FenceWaitEntered` exceeds everything accountable is a path this file does not
    /// know it has.
    fence_wait_entered: RefusalCounter,
    /// `pfnWaitForFence` asked for a value on a fence **this driver has never
    /// signalled**, so the wait was dropped as already-satisfied.
    ///
    /// ⚠ **Expected NON-ZERO and correct in the reachable-and-legal case**: a
    /// `CreateFence(InitialValue = N)` — the DDI carries no initial value
    /// (`D3D12DDIARG_CREATE_FENCE` is `{FenceCount, Fences}`) — or a CPU
    /// `ID3D12Fence::Signal` that `DDI_REFERENCE.md` §10.3 says never reaches the
    /// driver. Both are waits the runtime **already considers satisfied**, so
    /// dropping them is exact rather than approximate, and `CreateFence(1)` +
    /// `queue->Wait(f, 1)` is a common idiom this driver must not answer by removing
    /// the device.
    ///
    /// ⛔⛔ **AND IT IS NOT CLEAN, which is the whole reason it is graded here rather
    /// than called benign.** A CPU `Signal` that has **not happened yet** —
    /// `queueB->Wait(f, N)` and only later `fence->Signal(N)` from the CPU — is
    /// **indistinguishable** from the satisfied case at this DDI, and it is a real
    /// ordering gap that yields wrong pixels. It cannot be routed elsewhere (nothing
    /// observable separates it) and it cannot be forwarded (the engine fence can
    /// never reach `N`, so the engine wait would block that vkd3d queue forever).
    /// ⇒ **a large count next to visible cross-queue corruption is this case**, and
    /// the only real fix is the runtime's monitored fence — which §10.4's correction
    /// proves this driver can never name.
    fence_wait_runtime_owned: RefusalCounter,
    /// ⭐ **S-4's success counter: a real `ID3D12CommandSignature` was built.**
    ///
    /// ⚠ **Expected non-zero on any engine with GPU-driven rendering**, which calls
    /// `CreateCommandSignature` at startup. ⛔ Read it beside
    /// `CommandSignatureStateTemplateRefused`: the two partition every create, and
    /// the ratio is how much of a workload's indirect rendering this driver actually
    /// backs. `L3aExecuteIndirectForwarded` is its downstream half — a signature that
    /// is created and never executed is `METHOD.md` saturation criterion 6's
    /// *implemented-but-never-exercised*, and only those two together can show it.
    command_signature_created: RefusalCounter,
    /// ⛔⛔ **A command signature named a root-argument / state-template class, or
    /// more than one argument desc, and was refused `E_NOTIMPL` AT CREATE.**
    ///
    /// ⚠ **Expected NON-ZERO on any modern engine, and that is a real capability gap
    /// rather than an instrument.** `VK_EXT_device_generated_commands` is absent on
    /// this guest, and vkd3d's response is to accept the signature (clearing
    /// `requires_state_template`, `command.c:26447-26453`) and then **silently skip**
    /// every `ExecuteIndirect` that uses it (`command.c:17811-17818`). Refusing at
    /// create converts *"an empty scene with a score"* into a failure the application
    /// can act on — the bundle lesson, one DDI earlier.
    ///
    /// ⛔ **It is the counter that says whether DGC is worth pursuing.** A large count
    /// on a real workload promotes `VK_EXT_device_generated_commands` in the ICD/host
    /// from a named gap to scheduled work; a zero says the native four are enough.
    command_signature_state_template_refused: RefusalCounter,
    /// A command signature named `DISPATCH_RAYS` and was refused `E_NOTIMPL`.
    ///
    /// ⛔ **Expected 0** while `caps12` reports no raytracing tier: no raytracing
    /// pipeline can exist for an indirect dispatch to reach, so a hit is a caps
    /// inconsistency elsewhere rather than a missing forward. ⚠ Its own counter
    /// because vkd3d *does* treat `DISPATCH_RAYS` as an action command — the refusal
    /// is this driver's caps talking, not the engine's capability.
    command_signature_raytracing_refused: RefusalCounter,
    /// A `D3D12DDI_INDIRECT_ARGUMENT_DESC::Type` was a value this build's
    /// `d3d12umddi.h` does not name, and the create was refused `E_INVALIDARG`.
    /// **Expected 0**; a hit means the header this build was generated from is older
    /// than the runtime asking.
    command_signature_arg_type_unknown: RefusalCounter,
    /// A command-signature slot could not reach the engine. **Expected 0** — it is a
    /// device-scope DDI and a device exists by construction.
    command_signature_no_device: RefusalCounter,
    /// `ID3D12Device::CreateCommandSignature` on the engine failed, or returned
    /// `S_OK` with no object.
    ///
    /// ⚠ **May legitimately be non-zero**: vkd3d validates the stride against the
    /// computed signature size (`command.c:26409-26414`) and the root-signature
    /// pairing (`:26412-26424`), and this driver forwards both verbatim rather than
    /// duplicating checks that would then be a second authority able to drift.
    /// ⇒ read it beside `CommandSignatureRootSigUnexpected`.
    command_signature_engine_failed: RefusalCounter,
    /// An action-only command signature arrived with a **non-null**
    /// `hRootSignature`, which this driver forwarded as given.
    ///
    /// ⚠ **Expected 0, and a hit is a decision to revisit rather than a fault.**
    /// vkd3d refuses that pairing (`command.c:26421-26425`: *"Command signature does
    /// not require root signature, root signature must be NULL"*), so a hit here
    /// arrives with `CommandSignatureEngineFailed` and the application's create
    /// fails. Passing `None` instead would make it succeed and lose nothing semantic
    /// — an action-only signature binds no root arguments — but it would be this
    /// driver silently discarding something the application passed. ⇒ the counter
    /// exists so that trade is settled by evidence.
    command_signature_root_sig_unexpected: RefusalCounter,
    /// `hRootSignature` was non-null and carried no engine `ID3D12RootSignature`, so
    /// the create was refused `E_INVALIDARG` rather than forwarded with `None`.
    ///
    /// ⛔ **Expected 0** — L6's `pfnCreateRootSignature` either stores one or fails.
    /// ⚠ Forwarding `None` here would silently reinterpret *"this driver lost the
    /// root signature"* as *"the application passed none"*, which is the exact
    /// conflation `pso::root_signature`'s own doc warns callers to separate.
    command_signature_root_sig_unresolved: RefusalCounter,
    /// A Core-0114 application handle did not name an aligned, complete
    /// `D3D12DDI_RUNTIME_BYPASS_HEADER`. The call was dropped rather than
    /// interpreting runtime storage as a Helios private object. **Expected 0.**
    runtime_bypass_handle_invalid: RefusalCounter,
}

pub(crate) static L2_REFUSALS: L2Refusals = L2Refusals {
    queue_bad_arg: RefusalCounter::new("QueueBadArg"),
    queue_no_device: RefusalCounter::new("QueueNoDevice"),
    queue_class_unsupported: RefusalCounter::new("QueueClassUnsupported"),
    queue_engine_failed: RefusalCounter::new("QueueEngineFailed"),
    queue_creation_flags_ignored: RefusalCounter::new("QueueCreationFlagsIgnored"),
    queue_scheduling_group_ignored: RefusalCounter::new("QueueSchedulingGroupIgnored"),
    queue_node_mask_ignored: RefusalCounter::new("QueueNodeMaskIgnored"),
    queue_context_failed: RefusalCounter::new("QueueContextFailed"),
    queue_context_destroy_failed: RefusalCounter::new("QueueContextDestroyFailed"),
    queue_set_error_unavailable: RefusalCounter::new("QueueSetErrorUnavailable"),
    pool_bad_arg: RefusalCounter::new("PoolBadArg"),
    pool_no_device: RefusalCounter::new("PoolNoDevice"),
    pool_allocator_engine_failed: RefusalCounter::new("PoolAllocatorEngineFailed"),
    pool_type_mismatch: RefusalCounter::new("PoolTypeMismatch"),
    pool_reset_no_allocator: RefusalCounter::new("PoolResetNoAllocator"),
    pool_reset_engine_failed: RefusalCounter::new("PoolResetEngineFailed"),
    recorder_bad_arg: RefusalCounter::new("RecorderBadArg"),
    recorder_class_unsupported: RefusalCounter::new("RecorderClassUnsupported"),
    command_list_bad_arg: RefusalCounter::new("CommandListBadArg"),
    command_list_no_device: RefusalCounter::new("CommandListNoDevice"),
    command_list_class_unsupported: RefusalCounter::new("CommandListClassUnsupported"),
    command_list_engine_failed: RefusalCounter::new("CommandListEngineFailed"),
    command_list_flags_ignored: RefusalCounter::new("CommandListFlagsIgnored"),
    command_list_rt_table_missing: RefusalCounter::new("CommandListRtTableMissing"),
    command_list_ddi_table_cb_missing: RefusalCounter::new("CommandListDdiTableCbMissing"),
    command_signature_bad_arg: RefusalCounter::new("CommandSignatureBadArg"),
    command_signature_refused: RefusalCounter::new("CommandSignatureRefused"),
    command_signature_destroy_unexpected: RefusalCounter::new("CommandSignatureDestroyUnexpected"),
    bundle_list_refused: RefusalCounter::new("L2BundleListRefused"),
    execute_command_lists_bad_arg: RefusalCounter::new("ExecuteCommandListsBadArg"),
    execute_command_lists_list_missing: RefusalCounter::new("ExecuteCommandListsListMissing"),
    queue_unused_slot_called: RefusalCounter::new("QueueUnusedSlotCalled"),
    queue_unused2_slot_called: RefusalCounter::new("QueueUnused2SlotCalled"),
    tile_mappings_refused: RefusalCounter::new("TileMappingsRefused"),
    fence_op_bad_arg: RefusalCounter::new("FenceOpBadArg"),
    fence_op_fence_missing: RefusalCounter::new("FenceOpFenceMissing"),
    fence_op_engine_failed: RefusalCounter::new("FenceOpEngineFailed"),
    fence_wait_not_forwarded: RefusalCounter::new("FenceWaitNotForwarded"),
    fence_signal_forwarded: RefusalCounter::new("FenceSignalForwarded"),
    fence_wait_forwarded: RefusalCounter::new("FenceWaitForwarded"),
    command_lists_forwarded: RefusalCounter::new("CommandListsForwarded"),
    fence_signal_delayed: RefusalCounter::new("FenceSignalDelayed"),
    fence_signal_entered: RefusalCounter::new("FenceSignalEntered"),
    fence_wait_entered: RefusalCounter::new("FenceWaitEntered"),
    fence_wait_runtime_owned: RefusalCounter::new("FenceWaitRuntimeOwned"),
    command_signature_created: RefusalCounter::new("CommandSignatureCreated"),
    command_signature_state_template_refused: RefusalCounter::new(
        "CommandSignatureStateTemplateRefused",
    ),
    command_signature_raytracing_refused: RefusalCounter::new("CommandSignatureRaytracingRefused"),
    command_signature_arg_type_unknown: RefusalCounter::new("CommandSignatureArgTypeUnknown"),
    command_signature_no_device: RefusalCounter::new("CommandSignatureNoDevice"),
    command_signature_engine_failed: RefusalCounter::new("CommandSignatureEngineFailed"),
    command_signature_root_sig_unexpected: RefusalCounter::new("CommandSignatureRootSigUnexpected"),
    command_signature_root_sig_unresolved: RefusalCounter::new("CommandSignatureRootSigUnresolved"),
    runtime_bypass_handle_invalid: RefusalCounter::new("RuntimeBypassHandleInvalid"),
};

/// L2's refusal counters, printed by `crate::log_refusal_summary` at this
/// lane's position in `lib.rs`'s `UMD12_REFUSAL_SETS`.
///
/// ⭐ **Declared here rather than in `lib.rs` so this lane's diff against the
/// crate root is empty.** Every one of the eleven S6 lanes needs counters
/// (`PARALLEL.md` §9.1: *every skipped or refused path gets a named counter*),
/// and one flat array in `lib.rs` would have been the split's hottest merge
/// point — §5's shared-file table does not even list `lib.rs`. Same move
/// `forward12::tables12` makes for the 206 slots: name all eleven up front and
/// the lanes become substitutive instead of additive.
///
/// ⛔ **Append only.** Counter order inside a set, and set order in
/// `UMD12_REFUSAL_SETS`, are both the evidence contract: `D3D12 DDI refusals:`
/// lines get diffed across builds.
pub(crate) static REFUSALS: &[&RefusalCounter] = &[
    &L2_REFUSALS.queue_bad_arg,
    &L2_REFUSALS.queue_no_device,
    &L2_REFUSALS.queue_class_unsupported,
    &L2_REFUSALS.queue_engine_failed,
    &L2_REFUSALS.queue_creation_flags_ignored,
    &L2_REFUSALS.queue_scheduling_group_ignored,
    &L2_REFUSALS.queue_node_mask_ignored,
    &L2_REFUSALS.queue_context_failed,
    &L2_REFUSALS.queue_context_destroy_failed,
    &L2_REFUSALS.queue_set_error_unavailable,
    &L2_REFUSALS.pool_bad_arg,
    &L2_REFUSALS.pool_no_device,
    &L2_REFUSALS.pool_allocator_engine_failed,
    &L2_REFUSALS.pool_type_mismatch,
    &L2_REFUSALS.pool_reset_no_allocator,
    &L2_REFUSALS.pool_reset_engine_failed,
    &L2_REFUSALS.recorder_bad_arg,
    &L2_REFUSALS.recorder_class_unsupported,
    &L2_REFUSALS.command_list_bad_arg,
    &L2_REFUSALS.command_list_no_device,
    &L2_REFUSALS.command_list_class_unsupported,
    &L2_REFUSALS.command_list_engine_failed,
    &L2_REFUSALS.command_list_flags_ignored,
    &L2_REFUSALS.command_list_rt_table_missing,
    &L2_REFUSALS.command_list_ddi_table_cb_missing,
    &L2_REFUSALS.command_signature_bad_arg,
    &L2_REFUSALS.command_signature_refused,
    &L2_REFUSALS.command_signature_destroy_unexpected,
    &L2_REFUSALS.execute_command_lists_bad_arg,
    &L2_REFUSALS.execute_command_lists_list_missing,
    &L2_REFUSALS.queue_unused_slot_called,
    &L2_REFUSALS.queue_unused2_slot_called,
    &L2_REFUSALS.tile_mappings_refused,
    &L2_REFUSALS.fence_op_bad_arg,
    &L2_REFUSALS.fence_op_fence_missing,
    &L2_REFUSALS.fence_op_engine_failed,
    &L2_REFUSALS.fence_wait_not_forwarded,
    // ⛔ APPENDED, S6 Round 2. It was first written into the middle of this array,
    // between `CommandSignatureDestroyUnexpected` and `ExecuteCommandListsBadArg`,
    // where it read tidily beside the other create-time refusals -- and shifted
    // the nine counters after it in every `D3D12 DDI refusals:` line this driver
    // will ever print. That is precisely what the append-only rule above exists
    // to stop, and the rule was violated in the same commit that quotes it.
    // ⇒ new counters go HERE, at the end, however badly they group.
    &L2_REFUSALS.bundle_list_refused,
    // Live queue and fence forwards retained as internal diagnostics.
    &L2_REFUSALS.fence_signal_forwarded,
    &L2_REFUSALS.fence_wait_forwarded,
    &L2_REFUSALS.command_lists_forwarded,
    &L2_REFUSALS.fence_signal_delayed,
    // ⛔ APPENDED, S-2. Two ENTRY counters (the only per-direction instrument for
    // "did the runtime enter this slot", which no arithmetic over the shared
    // `FenceOp*` counters can recover) and the benign half of the dropped-wait
    // split. `FenceWaitNotForwarded` keeps its position above and its NAME, and
    // only its meaning narrowed — moving it would shift eleven counters.
    &L2_REFUSALS.fence_signal_entered,
    &L2_REFUSALS.fence_wait_entered,
    &L2_REFUSALS.fence_wait_runtime_owned,
    // ⛔ APPENDED, S-4. One success counter and seven per-cause refusals for
    // `pfnCreateCommandSignature`. ⚠ `CommandSignatureRefused` and
    // `CommandSignatureDestroyUnexpected` keep their positions above and are now
    // **dead** — expected 0 forever — because removing an array entry shifts every
    // counter after it, and this array is diffed across builds. Their docs say so.
    &L2_REFUSALS.command_signature_created,
    &L2_REFUSALS.command_signature_state_template_refused,
    &L2_REFUSALS.command_signature_raytracing_refused,
    &L2_REFUSALS.command_signature_arg_type_unknown,
    &L2_REFUSALS.command_signature_no_device,
    &L2_REFUSALS.command_signature_engine_failed,
    &L2_REFUSALS.command_signature_root_sig_unexpected,
    &L2_REFUSALS.command_signature_root_sig_unresolved,
    // APPENDED with Core-0114 runtime-bypass support. Never insert counters
    // above this point: refusal-summary field order is an evidence contract.
    &L2_REFUSALS.runtime_bypass_handle_invalid,
];

// ⚠ `Hresult` is imported for the `E_*`/`S_OK` constants this file returns; the
// DDI's own `HRESULT` (bindgen's `c_long`) is the declared return type and the
// two are the same `i32` (`umd_common/src/hr.rs:31-34`).
const _: () = assert!(core::mem::size_of::<Hresult>() == core::mem::size_of::<ddi12::HRESULT>());
