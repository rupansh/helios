//! Helios private direct-dispatch translator ABI — the **only** sanctioned way a
//! D3D translator (DXVK, vkd3d) reaches the Helios Venus ICD
//! (HELIOS_PRESENT_SYNC_RETIREMENT.md sections 2.8, 10.4, 10.7, 13, 17.3).
//!
//! # Why this module exists
//!
//! Section 10.4 (lines 1182-1184) requires "a private, versioned in-process
//! interface … between the D3D UMD bridge, DXVK/vkd3d, and Helios Mesa" with "no
//! global discovery: the UMD receives the function table directly while creating
//! its translator instance". Section 2 item 8 (lines 250-261) requires
//! translators to reach the ICD through that private entry point *rather than the
//! Vulkan loader*, so the call graph in section 13.1 stays acyclic. The reference
//! never names the entry point, its signature, its table layout, or its
//! versioning; three lanes independently reported it as unspecified
//! (`docs/retirement/lane-mesa.md` A3, `lane-dxvk-umd11.md` 6.14,
//! `lane-vkd3d-umd12.md` item 9). `docs/retirement/OWNERSHIP.md` section 4 records
//! the decision: it gets exactly one declaration, here, and this file is it.
//!
//! Mesa **will implement** this ABI (`icd/mesa/src/virtio/vulkan/
//! vn_helios_direct_dispatch.{c,h}`, section 17.3). DXVK, vkd3d, `umd/bridge` and
//! `umd12/bridge` **will consume** it. No consumer may declare its own copy of
//! the table, the entry-point name, or the mode constant.
//!
//! # ⛔ Producer status: DECLARED, NOT WIRED — all five parties are hypothetical
//!
//! **Measured 2026-08-10.** Of this module's 65 exported symbols, **zero** are
//! referenced anywhere outside `protocol/`:
//!
//! ```text
//! grep -rn -E 'HELIOS_TRANSLATOR|HeliosTranslator|helios_translator|PfnHeliosTranslator|HELIOS_ICD_CREATE_TRANSLATOR' \
//!   kmd_render kmd_logic umd umd12 umd_common dxvk-helios/src icd/mesa/src \
//!   vkd3d-proton-helios/libs tools packaging qemu-helios
//! → tools/retirement-gates.sh:78:  "#include \"helios_translator_dispatch.h\""
//! ```
//!
//! That single hit is the gate script feeding the C mirror to `gcc
//! -fsyntax-only` so its `_Static_assert`s are evaluated. **Nothing calls this
//! ABI and nothing implements it.** Concretely, against the five parties
//! `OWNERSHIP.md` §4 names:
//!
//! | party | role per OWNERSHIP §4 | state at HEAD |
//! |---|---|---|
//! | Helios Mesa | implements | `icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.{c,h}` **does not exist** (`ls` that path). Mesa lane unit **A5** creates it; A5 depends on A1 and A4, neither of which has started. |
//! | DXVK | consumes | no reference in `dxvk-helios/src` |
//! | vkd3d | consumes | no reference in `vkd3d-proton-helios/libs` |
//! | `umd/bridge` | consumes | no reference in `umd/` |
//! | `umd12/bridge` | consumes | no reference in `umd12/` |
//!
//! ⚠ **This file compiles, `abi_parity.py` passes, the C mirror's every
//! `_Static_assert` holds, and its unit tests are green. None of that is
//! evidence that a consumer exists** — it is evidence about a 4298-line Rust
//! file and its 2264-line transliteration. This is METHOD.md §3 criterion 6's
//! fourth state, *implemented but never exercised*, and the changeset's report
//! must not count it as implemented. Nothing here has ever run.
//!
//! Everything below this banner therefore describes an **intended** contract.
//! The refusal rules, the counters, and the `_INVALID` sentinels are real
//! declarations that a future implementer is bound by — they are not behaviour
//! anything exhibits today, and no counter in this file can have moved.
//!
//! # This is the one module in the crate that is NOT a wire format
//!
//! Every other module here defines bytes that cross a trust or machine boundary
//! and is therefore `Pod`, pointer-free, and padding-free. **These records never
//! leave the process.** They contain function pointers and caller-owned buffer
//! pointers by construction, so they are deliberately *not* `Pod`, *not*
//! `Zeroable`, and must never be memcpy'd into a WDDM private-data blob, a Render
//! command buffer, an HNR2 payload, an HVM1 reply slot, or an ETW record. The
//! records that *do* cross those boundaries already exist in this crate and are
//! **referenced**, never restated: [`crate::translation_session::HeliosQueueAttachV1`]
//! (HQA1), [`crate::translation_session::HeliosTranslationEndpointV1`], and
//! [`crate::wddm`]'s HOB1 family.
//!
//! # The single entry point
//!
//! One exported symbol, [`HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME`], resolved
//! directly on the already-loaded Helios ICD module. It creates the record-only
//! translator instance, returns that instance's const dispatch table — which is
//! what section 10.4 means by "receives the function table directly while
//! creating its translator instance" — and returns the one `VkInstance` that
//! instance's HTS1 `INIT` created.
//!
//! ⭐ **The `VkInstance` is returned, never re-created.** Section 10.4's INIT
//! paragraph ("creates one distinct host virgl/Venus context and `VkInstance`
//! … No second `VkInstance` may be created in that host context") means the
//! translator has no legal way to mint its own: a `vkCreateInstance` resolved
//! through this table would be exactly the "second instance … requested in one
//! host context" that section 10.9's HTS1 row orders to *fail translator device
//! creation*. So [`HeliosTranslatorInstanceV1::vk_instance`] hands the
//! translator the instance INIT already made, and
//! [`HeliosTranslatorDispatchV1::get_instance_proc_addr`] must refuse the name
//! `vkCreateInstance` outright — it returns NULL and counts
//! [`HeliosTranslatorRefusalCountersV1::withheld_proc_addr_refused`].
//!
//! Resolving this symbol on the ICD module is the **only** sanctioned way a
//! translator reaches the ICD. In particular:
//!
//!   - **not** through the Vulkan loader (`vulkan-1.dll`) — section 13.2 disables
//!     DXVK's `vulkan_loader.cpp` search in Helios builds and section 13.2's
//!     static import test rejects a `vulkan-1.dll` import from any
//!     translator-bearing UMD binary;
//!   - **not** through the Helios WSI layer, which lives *above* the loader and
//!     has no edge to a translator instance at all;
//!   - **not** through `GetProcAddress`-by-name discovery of any other ICD export.
//!     Section 17.3 deletes the ten name-resolved exports
//!     (`umd/bridge/bridge_icd_exports.cpp`) this replaces; appendix A.4 calls
//!     that mechanism loaded-module discovery and forbids it.
//!
//! Once the table is in hand, every Vulkan entry point the translator needs is
//! resolved through [`HeliosTranslatorDispatchV1::get_instance_proc_addr`], and a
//! returned pointer whose owning module is not
//! [`HeliosTranslatorDispatchV1::icd_module_base`] is rejected (section 13.2,
//! lines 3357-3358).
//!
//! # The two halves
//!
//! The interface is **bidirectional** and both halves are modelled explicitly,
//! because the C60 synchronous-progress join runs the other way:
//!
//! | Half | Declared by | Implemented by | Called by |
//! |---|---|---|---|
//! | [`HeliosTranslatorDispatchV1`] (down) | this file | Helios Mesa (the ICD) | the D3D UMD bridge and, for `get_instance_proc_addr`, the translator |
//! | [`HeliosTranslatorHostCallbacksV1`] (up) | this file | the D3D UMD bridge (`umd/bridge`, `umd12/bridge12.rs`) | the ICD |
//!
//! The up half exists because sections 10.4 (1354-1360), 17.4 (4124-4128) and
//! 17.5 (4239-4243) all require the *UMD* to own the HQC1 milestone: a
//! GPU-dependent synchronous Vulkan call inside Mesa must "first force the owning
//! outer UMD to submit pending batches, signal that context's private HQC1 value,
//! and perform one event-backed CPU wait" before it may fetch a bounded control
//! reply. Mesa cannot do any of that itself — it owns no outer context and may
//! not submit — so it calls up.
//!
//! ⛔ **The up half terminates at the UMD bridge and the WDDM runtime callback
//! table.** It is not, and may never become, an edge to D3D or DXGI: section 10.1
//! invariant 6 and section 13.1 forbid an `ICD -> DXGI/D3D` edge, and that is
//! exactly the edge that would close the `ICD -> DXGI -> D3D UMD -> translator ->
//! ICD` cycle section 13.1 proves impossible. Section 13.2's static import test
//! (`dxgi.dll`, `d3d11.dll`, `d3d12.dll` rejected from the ICD DLL) is the
//! enforcement; the direction column on every slot below is the specification.
//!
//! # What the ICD owns and what the UMD owns
//!
//! One line decides most of the layout questions in this file, and it is section
//! 10.4 line 1398: *"The outer UMD encodes the sealed batch as complete bounded
//! `HeliosOuterBatchV1` (HOB1) command bytes in its runtime-approved command
//! buffer."*
//!
//!   - **The ICD produces content**: the Venus payload, the resource-use table in
//!     *token* form, and the typed operand table with *payload-relative* offsets.
//!   - **The UMD produces the record**: it encodes HOB1 (header, both tables,
//!     payload), resolves every use to a D3D11 allocation-list index or a D3D12
//!     GPUVA plus the allocation generation it observed, relocates operand
//!     offsets, and computes the HOB1 CRC64 with [`crate::wddm`].
//!
//! That split is why **no slot in either half accepts or returns a GPUVA, an
//! allocation-list index, an allocation generation, a KMT handle, or a host
//! resource id**: the ICD never learns WDDM identity, and section 17.3's audit
//! ("no actual virtio `resource_id` may reach user-mode storage or protocol
//! state") stays satisfiable by construction.
//!
//! # Prohibited payloads, and how each prohibition is kept
//!
//! | Prohibited (section 3 lines 361-369; section 10.1 invariants 6/16) | How this ABI keeps it out |
//! |---|---|
//! | raw host resource id / virtio `resid` | no field of any type here is one; the operand table carries `operand_kind`/`encoded_width` only, and section 10.4 requires the payload bytes it names to be **zero** |
//! | PID / process generation | absent; identity is the live instance and the live outer context, both pointer-reachable |
//! | KMT handle (`D3DKMT_HANDLE`), NT handle, or WDDM allocation handle | absent; outer allocations appear only as [`HeliosSealedResourceUseV1::outer_allocation_token`], a UMD-assigned opaque scalar the ICD echoes and never interprets |
//! | GPUVA / allocation-list index / allocation generation | absent; see the ownership split above |
//! | a Vulkan handle owned by another scope | exactly one Vulkan handle exists in this ABI — [`HeliosTranslatorInstanceV1::vk_instance`], the one this instance's own `INIT` created. A handle from another translator instance, from a loader-created device, or from the WSI layer is [`HeliosTranslatorStatus::ForeignVulkanHandle`] |
//! | an HTS1 session capability (invariant 16) | **no slot returns one, and no field, parameter, or accessor in this file is of capability type.** Section 17.3 requires the capability to reach the UMD "only through the private direct table" and section 10.4 requires the UMD to put it in HQA1, so it transits — but only *inside the sealed HQA1 packet the ICD builds*: [`HeliosTranslatorDispatchV1::build_queue_attach`]. ⚠ Stated precisely, because the imprecise version invites a wrong inference: that packet is [`crate::translation_session::HeliosQueueAttachV1`], whose `capability_low`/`capability_high` are ordinary public fields at offsets 24 and 32. The bridge **can** read the nonce. Section 17.5 ("No host/session capability is stored in Render/Submit PDD or a resource") makes not doing so a **rule the bridge must obey**; what this ABI supplies is a *shape that never asks it to* — no scalar capability crosses any signature, so there is nothing to key a table with unless the bridge goes looking. Invariant 16 disclaims being a boundary against code already in the process, and so does this row. |
//!
//! Those are properties of the declarations, not of prose: every record below
//! carries a `const` size assertion, so a field added to smuggle any of the above
//! in breaks the build rather than a review.
//!
//! # What cannot be reached without a live outer-operation scope
//!
//! Section 10.4 (1387-1390) says the five-point record-only contract "covers
//! every queue entry point, not only ordinary draw submits. The private
//! dispatcher rejects any `vkQueueSubmit`, `vkQueueSubmit2`,
//! `vkQueueBindSparse`, `vkQueuePresentKHR`, or queue-idle operation that arrives
//! without a live outer-operation scope."
//!
//! Those are **Vulkan** entry points, reached through
//! [`HeliosTranslatorDispatchV1::get_instance_proc_addr`], not slots of this
//! table — so this ABI makes the rule enforceable in three ways rather than
//! stating it:
//!
//!   1. **There is no submit slot.** The table has no verb that submits, presents,
//!      signals a timeline, or idles a queue. The whole surface is create /
//!      discover / attach / scope / seal / copy / close / count / destroy. Point 5
//!      of the contract ("perform no queue-work KMT Render/SubmitCommand/HWQueue
//!      call and signal no independent GPU timeline") is therefore a property of
//!      the table, not a promise about its implementation.
//!   2. **The scope is the only thing that makes recording legal.**
//!      [`HeliosTranslatorDispatchV1::open_outer_scope`] is the sole producer of a
//!      [`HeliosTranslatorScope`], the UMD bridge is its sole caller, and the
//!      dispatcher consults the calling thread's live scope when a queue entry
//!      point arrives. No scope, no recording:
//!      [`HeliosTranslatorStatus::NoOuterScope`].
//!   3. **Every refusal is counted and readable.**
//!      [`HeliosTranslatorRefusalCountersV1`] has one monotonic counter per
//!      refusal class in that sentence, plus the loader-provenance and
//!      opcode-classification classes from sections 13.2 and 10.9. A nonzero
//!      counter is a translator bug and the UMD must fail closed — see the type's
//!      documentation.
//!
//! `vkQueuePresentKHR` is a special case: section 17.3 (3987-3989) makes
//! `vn_QueuePresentKHR` **unavailable** to a translator instance, so it is refused
//! unconditionally rather than scope-gated, and its counter is named accordingly.
//!
//! # Locks, blocking, and the one sanctioned reentrant edge
//!
//! Section 13.3 governs, and every slot below states its rule. Two conclusions
//! are load-bearing:
//!
//!   - **No lock is ever held across a call in either direction.** Section 13.3:
//!     "UMD callbacks similarly hold no translator/Mesa lock while calling the
//!     runtime submission callback", and "a synchronous control call holds no
//!     translator, endpoint, session-list, or UMD runtime lock while waiting". The
//!     queue-local mutex section 10.4 (1416-1418) permits is taken *inside* a
//!     recording, seal, or copy call and released before it returns; it never
//!     spans a scope, a runtime callback, or GPU completion.
//!   - **[`HeliosTranslatorHostCallbacksV1::sync_progress_join`] is the only
//!     reentrant edge in the ABI**, and it is reentrant by requirement: the join
//!     must seal and submit pending work, which means the UMD calls back *down*
//!     into `seal_outer_scope`/`copy_sealed_batch`/`close_outer_scope`/
//!     `open_outer_scope` on the same outer context while the ICD's Vulkan call
//!     is still on the stack. Because no lock is held in either direction, no
//!     inversion is possible.
//!
//!     ⚠ **The join arrives with a scope already open, and that is the whole
//!     difficulty.** The triggering case is a GPU-dependent synchronous Vulkan
//!     call (`vkGetFenceStatus` with a wait, `vkWaitForFences`, a query `WAIT`,
//!     `vkQueueWaitIdle`) made by the translator *while recording*, i.e. from
//!     inside the D3D11 flush or D3D12 ECL that opened the scope. Neither of
//!     the two obvious moves works: opening a second scope is
//!     [`HeliosTranslatorStatus::ScopeAlreadyOpen`], and sealing/closing the
//!     caller's scope would leave the bridge's own outer frame holding a dead
//!     [`HeliosTranslatorScope`]. So the protocol is stated, once, here, and it
//!     is the **cut-and-reopen** form — see
//!     [`HeliosTranslatorHostCallbacksV1::sync_progress_join`] for the numbered
//!     steps. Its two load-bearing consequences:
//!
//!       1. the bridge is the sole owner of the scope handle, and the join
//!          *replaces* the bridge's own handle with a fresh scope on the same
//!          context, so no dangling handle exists at any point;
//!       2. the ICD must therefore **never cache a `HeliosTranslatorScope`
//!          across an up-call**. Its thread-current scope identity changes
//!          across a join by design; it re-reads it on return.
//!
//!     A *second* join nested inside the first is a cycle and is refused:
//!     [`HeliosTranslatorStatus::ReentrantJoin`], counted by
//!     [`HeliosTranslatorRefusalCountersV1::reentrant_join_refused`]. **The ICD
//!     is the detector** — it is the only party that initiates a join, so it is
//!     the only party that can know one is already in progress on this thread,
//!     and it refuses the inner one before the callback is reached.
//!
//! # Versioning: total, C-expressible, and never partial
//!
//! Every record starts with `struct_bytes` and `abi_version`, and the three
//! top-level records ([`HeliosTranslatorCreateInfoV1`],
//! [`HeliosTranslatorHostCallbacksV1`], [`HeliosTranslatorDispatchV1`]) also carry
//! `package_generation`. The check is **total**: exact size, exact ABI version,
//! exact package generation, and every slot non-NULL.
//!
//! ⛔ There is no partial adopt, no "use the slots I recognise", no
//! smaller-is-older tolerance, and no zero wildcard. A mismatch fails translator
//! **device creation** before any context, allocation, endpoint, or resource is
//! exposed — the same rule section 10.9's HTS1 row states for a failed HTS1
//! `INIT` and that [`crate::HELIOS_PACKAGE_GENERATION`] states for the package
//! as a whole.
//!
//! # Every validator has a C twin, because the implementer is C
//!
//! Section 17.3 makes `vn_helios_translation_session.c` — C — the party that
//! implements this ABI, and DXVK's bridge is C++. A validator that exists only
//! in Rust is a gate that the party required to run it cannot call, and it
//! would be hand-rolled in Mesa instead: three copies of the fail-closed logic
//! this file exists to centralise. So each check below is written twice, once
//! per language, and the pairs are:
//!
//! **Every record that crosses this interface as an input has one**, in both
//! languages. That completeness is the property, not the count: a record with
//! no validator is a record whose stated constraints are prose the receiver has
//! to remember, and the three that were missing when this table was first
//! written were [`HeliosSealedBatchCopyV1`] — three caller-owned buffers the
//! ICD *writes into* — [`HeliosSyncProgressJoinV1`], whose unchecked
//! `context_generation` is a use-after-free, and
//! [`HeliosOuterContextAttachV1`], whose NULL cookie is uncatchable later.
//!
//! | Rust | C | run by |
//! |---|---|---|
//! | [`HeliosTranslatorCreateInfoV1::validate`] | `helios_translator_check_create_info` | ICD |
//! | [`HeliosTranslatorHostCallbacksV1::validate`] | `helios_translator_check_host_callbacks` | ICD |
//! | [`HeliosTranslatorDispatchV1::validate`] | `helios_translator_check_dispatch` | bridge |
//! | [`HeliosTranslatorInstanceV1::validate`] | `helios_translator_check_instance` | bridge |
//! | [`HeliosQueueAttachRequestV1::validate`] | `helios_translator_check_queue_attach_request` | ICD |
//! | [`HeliosOuterContextAttachV1::validate`] | `helios_translator_check_context_attach` | ICD |
//! | [`HeliosOuterScopeBeginV1::validate`] | `helios_translator_check_scope_begin` | ICD |
//! | [`HeliosSealedBatchV1::validate`] | `helios_translator_check_sealed_batch` | bridge |
//! | [`HeliosSealedBatchCopyV1::validate`] | `helios_translator_check_sealed_batch_copy` | ICD |
//! | [`HeliosSealedResourceUseV1::validate`] | `helios_translator_check_sealed_use` | bridge |
//! | [`HeliosSealedOperandV1::validate`] | `helios_translator_check_sealed_operand` | bridge |
//! | [`HeliosSealedOperandV1::validate_payload_zero`] | `helios_translator_check_operand_payload_zero` | bridge |
//! | [`HeliosOuterScopeCloseV1::validate`] | `helios_translator_check_scope_close` | ICD |
//! | [`HeliosSyncProgressJoinV1::validate`] | `helios_translator_check_join_request` | bridge |
//! | [`HeliosSyncProgressResultV1::validate_join`] | `helios_translator_check_join_result` | ICD |
//! | [`HeliosSyncProgressResultV1::validate_query`] | `helios_translator_check_query_result` | ICD |
//!
//! `protocol/src/translator_dispatch.rs` remains the source of truth: where the
//! two disagree the Rust wins and the header is the bug.

use core::ffi::{c_char, c_void};

use crate::translation_session::{HeliosQueueAttachV1, HeliosTranslationEndpointV1};

// ── The exported entry point ────────────────────────────────────────────────

/// The one exported symbol a translator resolves on the Helios ICD module.
///
/// Section 10.4 (1182-1184) forbids global discovery; this is a direct
/// `GetProcAddress` on a module the consumer already holds, which discovers
/// nothing it did not already have. It is the **only** sanctioned way a
/// translator reaches the ICD — see the module header.
///
/// Mesa exports it from `vn_helios_direct_dispatch.c` (section 17.3) and lists it
/// in the ICD's `.def`; every other private export named in
/// `vn_renderer_helios.c` is deleted by the same section.
pub const HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME: &str = "helios_icd_create_translator_v1";

/// [`HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME`] as a NUL-terminated byte string, for
/// a `GetProcAddress` call site that needs a `LPCSTR` without allocating.
pub const HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME_CSTR: &[u8] = b"helios_icd_create_translator_v1\0";

/// The ABI version of *this interface*, independent of
/// [`crate::HELIOS_PACKAGE_GENERATION`].
///
/// Both are checked, and they answer different questions: the package generation
/// says "these binaries are one package", the ABI version says "these binaries
/// agree on the shape of this table". A change to any declaration in this file
/// bumps this **and** the package generation ordinal.
pub const HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION: u32 = 1;

/// The `submission_mode` of a translator instance: **record-only**.
///
/// Section 10.4 line 1368: "A translator instance is created with
/// `HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`." This is that constant, and
/// this is its only declaration — DXVK, vkd3d, `umd/bridge` and `umd12/bridge`
/// all use *this* symbol (or its C mirror), never a private copy.
///
/// It means the five-point contract of section 10.4 (1373-1385) in full, and in
/// particular point 5: the instance performs **no** queue-work KMT
/// `Render`/`SubmitCommand`/HWQueue call and signals no independent GPU timeline.
/// Section 10.7 (1742-1747) adds the structural consequence: a record-only
/// instance uses the same raw HVC1 control/session INIT but creates **no raw HVC1
/// GPU queue context**; its outer D3D contexts attach to the pre-existing session
/// through HQA1. That, and the normal native-Vulkan form which never touches this
/// ABI at all, "are the only two session endpoint attachment forms".
pub const HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY: u32 = 1;

/// The rejected `submission_mode` sentinel.
///
/// Zero is never a wildcard and never a default: a zeroed
/// [`HeliosTranslatorCreateInfoV1`] must be refused, not silently promoted to
/// record-only. [`HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`] is the only
/// value this generation defines; any other value, including a future one,
/// is [`HeliosTranslatorStatus::SubmissionMode`].
pub const HELIOS_TRANSLATOR_SUBMISSION_MODE_INVALID: u32 = 0;

// ── Status codes ────────────────────────────────────────────────────────────

/// The FFI representation of a [`HeliosTranslatorStatus`].
///
/// Every slot in both halves returns this. It is a plain `i32` on the wire rather
/// than the enum, because a foreign implementation returning a discriminant this
/// build does not know must be *decoded and refused*, not transmuted — see
/// [`HeliosTranslatorStatus::from_wire`].
pub type HeliosTranslatorStatusCode = i32;

/// Every named refusal this ABI can produce.
///
/// The crate's rule ("never a `bool`, never a panic, a named reason per
/// rejection") applied to an in-process interface. Each variant names exactly one
/// cause, and the doc comment names the section that requires the refusal.
///
/// ⛔ No variant carries a payload. A refusal is returned, counted, and traced;
/// none of the values that could be interesting to log here — a capability, a
/// token, a pointer — may be copied into a diagnostic record (section 17.1's
/// diagnostics bullet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum HeliosTranslatorStatus {
    /// The call succeeded. The only non-refusal.
    Ok = 0,

    // ── Handshake (fails translator device creation, section 10.9:2860) ──
    /// A required pointer argument was NULL, or a required slot was NULL.
    NullArgument = 1,
    /// `struct_bytes` is not exactly this build's `sizeof`. Not "at least":
    /// a short record cannot be zero-extended and a long one cannot be truncated,
    /// because either would be a partial adopt.
    StructBytes = 2,
    /// `abi_version` is not [`HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`].
    AbiVersion = 3,
    /// `package_generation` is not the expected [`crate::HELIOS_PACKAGE_GENERATION`],
    /// or is zero. Section 17.1: "Generation mismatch is fatal."
    PackageGeneration = 4,
    /// `submission_mode` is not [`HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`].
    SubmissionMode = 5,
    /// The up-half callback table is absent, mis-sized, mis-versioned, or has a
    /// NULL slot. Section 17.5 (4239): the bridge exposes these callbacks, and a
    /// record-only instance cannot function without them.
    HostCallbacks = 6,
    /// The adapter LUID is zero or otherwise not a usable adapter identity.
    AdapterLuid = 7,
    /// The exact adapter named by the LUID could not be opened, or the raw KMT
    /// device could not be created on it (section 10.4, line 1189).
    AdapterUnavailable = 8,
    /// HTS1 `INIT` failed: the control context, the role-1 HVM1 reply pool, the
    /// host context/`VkInstance`, or the C51 reply did not come up
    /// (section 10.4, lines 1188-1203; section 10.9, row 2860).
    SessionInit = 9,
    /// Session or ring capacity is exhausted for this process
    /// ([`crate::translation_session::HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS`]).
    SessionCapacity = 10,
    /// The session has been poisoned or lost (reset, removal, an opcode-class
    /// violation, or a reply-schema mismatch). Every later call on this instance
    /// returns this; the UMD must fail or remove the device. Section 10.9 rows
    /// 2864-2865. There is no un-poison and no retry.
    SessionPoisoned = 11,

    // ── Endpoints and outer contexts ──
    /// The requested endpoint capacity is zero or above
    /// [`crate::translation_session::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION`], or
    /// the caller's array is smaller than the session's endpoint count.
    EndpointCapacity = 12,
    /// `endpoint_id` is zero or names no endpoint of this session. Never a
    /// lookup miss to be repaired — the bridge selects from
    /// [`HeliosTranslatorDispatchV1::enumerate_endpoints`] and nowhere else.
    UnknownEndpoint = 13,
    /// [`HeliosQueueAttachRequestV1::engine_class`] is zero, undefined, or not
    /// the class the selected endpoint admits (section 10.4, HQA1 table row
    /// `offset 44`: "exact graphics/compute/copy class admitted for the outer
    /// context").
    ///
    /// The field exists so that this refusal is *reachable*. The outer
    /// context's engine class is a bridge fact — the D3D12 command-queue type
    /// or the D3D11 node — and if the bridge did not state it, the ICD could
    /// only copy the endpoint's own class into HQA1 and the KMD's later
    /// `EngineClassMismatch` check would be the endpoint compared against
    /// itself. With the field, a D3D12 COPY queue that selects a GRAPHICS
    /// endpoint is refused here, before any HQA1 exists.
    EngineClass = 14,
    /// `context_flags` is not exactly one of
    /// [`crate::translation_session::HELIOS_HQA1_FLAG_D3D11_PHYSICAL`] /
    /// [`crate::translation_session::HELIOS_HQA1_FLAG_D3D12_VIRTUAL`]
    /// (section 10.4, table row `offset 64`: "exactly one set").
    ContextFlags = 15,
    /// `context_generation` is zero, not greater than every generation this
    /// session has already issued, or reused. Section 10.4 (1233): "nonzero,
    /// monotonically increasing and never reused within this HTS1 session".
    ContextGeneration = 16,
    /// No outer context with that generation is attached to this instance.
    UnknownContext = 17,
    /// That context generation is already attached. Invariant 13: an outer D3D
    /// context attaches "exactly once".
    ContextAlreadyAttached = 18,

    // ── Scopes (section 10.4, lines 1387-1396) ──
    /// A queue entry point, or a slot that requires one, arrived with no live
    /// outer-operation scope on the calling thread. This is the refusal section
    /// 10.4 (1388-1390) demands for `vkQueueSubmit`, `vkQueueSubmit2`,
    /// `vkQueueBindSparse` and queue-idle.
    NoOuterScope = 19,
    /// A scope is already open on that outer context. Scopes do not nest and do
    /// not queue: a second concurrent scope on one context is a bridge bug, and
    /// refusing is correct where blocking would invent a lock section 13.3 does
    /// not allow.
    ScopeAlreadyOpen = 20,
    /// The scope was used from a thread other than the one that opened it.
    ScopeForeignThread = 21,
    /// [`HeliosTranslatorDispatchV1::copy_sealed_batch`] was called on a scope
    /// that has not been sealed.
    ScopeNotSealed = 22,
    /// [`HeliosTranslatorDispatchV1::seal_outer_scope`] was called twice, or a
    /// recording arrived after the seal. A sealed batch is immutable
    /// (section 10.4, point 3).
    ScopeAlreadySealed = 23,
    /// Teardown was requested while a scope is still open. Destruction may wait
    /// for submitted work; it may not abandon a half-recorded batch.
    ScopeStillLive = 24,
    /// A caller-supplied destination buffer is smaller than the sealed batch
    /// reported. Never a partial copy.
    BufferTooSmall = 25,
    /// The batch would exceed [`crate::wddm::HELIOS_HOB1_MAX_BYTES`],
    /// [`crate::wddm::HELIOS_HOB1_MAX_USE_RECORDS`], or
    /// [`crate::wddm::HELIOS_HOB1_MAX_OPERAND_RECORDS`]. Section 10.4
    /// (1298-1304) requires the split to happen at a complete generated
    /// operation boundary *before* sealing, so reaching this is a producer bug,
    /// and section 10.9 (2866) forbids truncating instead.
    BatchBoundExceeded = 26,
    /// A sealed use on a D3D11 physical context carries a nonzero
    /// [`HeliosSealedResourceUseV1::byte_offset`]. HOB1's `identity_kind == 1`
    /// arm names an allocation-list index and has nowhere to put a sub-allocation
    /// offset, so a sub-range use cannot be encoded on that arm and must not be
    /// produced for it.
    D3D11SubrangeUse = 27,
    /// A Vulkan queue entry point was refused outright rather than scope-gated.
    /// `vkQueuePresentKHR` on a record-only instance is the case section 17.3
    /// (3987-3989) requires; no D3D-facing surface or swapchain exists there.
    QueueEntryPointRefused = 28,
    /// A Vulkan handle from another scope was presented — another translator
    /// instance, the WSI layer, or a normal loader-created device. The only
    /// Vulkan handle any slot accepts is
    /// [`HeliosTranslatorInstanceV1::vk_instance`], the one this instance
    /// produced.
    ///
    /// ⚠ [`HeliosTranslatorDispatchV1::get_instance_proc_addr`] is the one slot
    /// that cannot *return* a status: its signature returns a function pointer.
    /// It signals this refusal by returning `NULL` **and** incrementing
    /// [`HeliosTranslatorRefusalCountersV1::foreign_vulkan_handle_rejected`],
    /// which is what keeps it a counted refusal rather than a silent one. The
    /// enum value is still needed: the ICD traces it, and a consumer that maps
    /// codes to strings needs the name.
    ForeignVulkanHandle = 29,
    /// A resolved procedure's owning module is not
    /// [`HeliosTranslatorDispatchV1::icd_module_base`] — the Vulkan loader or
    /// the WSI layer owns it (section 13.2, "a pointer whose owning module is
    /// the Vulkan loader or WSI layer is rejected").
    ///
    /// ⚠ **The consumer performs this check, so the consumer owns the
    /// refusal.** It cannot be recorded in
    /// [`HeliosTranslatorRefusalCountersV1`], which only the ICD can write —
    /// see that field's documentation for the division.
    LoaderProvenance = 30,

    // ── The up half ──
    /// A [`HeliosTranslatorHostCallbacksV1::sync_progress_join`] was requested
    /// while one is already in progress on this thread. The join is the ABI's one
    /// reentrant edge; nesting it is a cycle.
    ReentrantJoin = 31,
    /// An up-call returned a refusal, so the down-call that needed it cannot
    /// proceed. The ICD does not interpret the bridge's reason; it fails closed.
    HostCallbackFailed = 32,
    /// The outer device was removed, or HQC1 creation/signal/wait failed while a
    /// synchronous result waited. Section 10.9 (2867): "return device lost/failure;
    /// never use ring-zero completion or a later queue value as success".
    DeviceLost = 33,

    // ── Field-level ──
    /// A reserved field was nonzero.
    ReservedNonZero = 34,
    /// [`HeliosOuterScopeCloseV1::disposition`] is not a defined
    /// [`HeliosTranslatorScopeDisposition`].
    Disposition = 35,
    /// [`HeliosSealedResourceUseV1::access_flags`] is zero, outside
    /// [`crate::wddm::HELIOS_HOB1_ACCESS_MASK`], or `PRIMARY_WRITE` without
    /// `WRITE` (section 10.4, line 1283: "primary implies write").
    AccessFlags = 36,
    /// [`HeliosSealedOperandV1`]'s `operand_kind` is not
    /// `GENERATED_RESOURCE`, or its `encoded_width` is neither 4 nor 8.
    /// Section 10.4 (1284-1286): an operand "identifies only a generated
    /// resource operand whose payload bytes are zero; arbitrary patches and raw
    /// renderer IDs are rejected".
    OperandEncoding = 37,
    /// [`HeliosSealedBatchV1::batch_id`] is zero. Section 10.4 (table row
    /// `offset 32`): nonzero and strictly increasing on this context.
    BatchId = 38,

    // ── Distinct causes that used to borrow a neighbour's code ──
    //
    // Each of the seven below replaces a refusal that was previously returned
    // under a code whose documented meaning was a *different* cause. The
    // thesis of this enum is that a variant names exactly one cause; a gate or
    // ETW record showing `BatchBoundExceeded` must not send someone hunting a
    // size-split bug when the defect is a four-byte alignment error.
    /// A record's `session_generation` is zero or is not the live HTS1
    /// session's. Distinct from [`Self::SessionInit`], which means the session
    /// never came up at all.
    SessionGeneration = 39,
    /// A [`HeliosSealedResourceUseV1`] names an illegal byte range: zero
    /// `byte_length`, or `byte_offset + byte_length` overflowing 64 bits.
    /// Distinct from [`Self::BufferTooSmall`] (a *destination* buffer) and from
    /// [`Self::BatchBoundExceeded`] (a producer that failed to split).
    SealedUseRange = 40,
    /// A [`HeliosSealedOperandV1::use_index`] is outside the sealed batch's use
    /// table. An index error, not a bound overrun.
    OperandUseIndex = 41,
    /// A progress value contradicts the record carrying it: a `COMMITTED`
    /// [`HeliosOuterScopeCloseV1`] with no value, an `ABANDONED` one with a
    /// value, or a [`HeliosSyncProgressResultV1`] whose `completed` exceeds its
    /// `last_submitted`. A malformed argument, not a failed callback
    /// ([`Self::HostCallbackFailed`]) and not a reserved field
    /// ([`Self::ReservedNonZero`]).
    ProgressValue = 42,
    /// [`HeliosTranslatorDispatchV1::destroy_instance`] was called while an
    /// outer context is still attached. Section 17.5 requires session,
    /// endpoint, HQA1 and HQC1 teardown on every queue/device/error path, so
    /// the bridge must detach every context first; without this code that
    /// precondition would be prose that fails open.
    ContextStillAttached = 43,
    /// A [`HeliosSealedResourceUseV1::outer_allocation_token`] names no
    /// allocation of the consumer's device.
    ///
    /// The token is opaque to the ICD by construction, so only the bridge can
    /// make this determination — and it must, because an unresolvable token
    /// cannot become an allocation-list index or a GPUVA. Declared here so the
    /// three consuming lanes do not each pick a different existing code.
    UnknownAllocationToken = 44,
    /// An operand's `encoded_width` payload bytes are not zero. Section 10.4:
    /// an operand "identifies only a generated resource operand whose payload
    /// bytes are zero" — a nonzero placeholder is how a raw host resource id
    /// would reach the host at a position the operand table blesses.
    PayloadPlaceholderNonZero = 45,
}

/// An `i32` returned by a foreign implementation that this build does not know.
///
/// It is deliberately **not** convertible to a refusal reason: an unknown status
/// is a package mismatch that the generation check should already have caught, so
/// it is a hard failure (fail translator device creation, or remove the device if
/// one exists) and never a success, a retry, or a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosTranslatorUnknownStatus {
    pub code: HeliosTranslatorStatusCode,
}

impl HeliosTranslatorStatus {
    /// The `i32` this status is on the wire.
    #[inline]
    pub const fn wire(self) -> HeliosTranslatorStatusCode {
        self as HeliosTranslatorStatusCode
    }

    /// Total decode of a returned status code.
    ///
    /// Exhaustive by construction: adding a variant without adding an arm here
    /// fails to compile, because the `match` below is over the full enum.
    pub const fn from_wire(
        code: HeliosTranslatorStatusCode,
    ) -> Result<Self, HeliosTranslatorUnknownStatus> {
        use HeliosTranslatorStatus::*;
        // Written as an explicit ladder rather than a transmute: `from_wire` is
        // the only place a foreign integer becomes an enum, and it must be
        // impossible for it to fabricate a discriminant.
        let s = match code {
            0 => Ok,
            1 => NullArgument,
            2 => StructBytes,
            3 => AbiVersion,
            4 => PackageGeneration,
            5 => SubmissionMode,
            6 => HostCallbacks,
            7 => AdapterLuid,
            8 => AdapterUnavailable,
            9 => SessionInit,
            10 => SessionCapacity,
            11 => SessionPoisoned,
            12 => EndpointCapacity,
            13 => UnknownEndpoint,
            14 => EngineClass,
            15 => ContextFlags,
            16 => ContextGeneration,
            17 => UnknownContext,
            18 => ContextAlreadyAttached,
            19 => NoOuterScope,
            20 => ScopeAlreadyOpen,
            21 => ScopeForeignThread,
            22 => ScopeNotSealed,
            23 => ScopeAlreadySealed,
            24 => ScopeStillLive,
            25 => BufferTooSmall,
            26 => BatchBoundExceeded,
            27 => D3D11SubrangeUse,
            28 => QueueEntryPointRefused,
            29 => ForeignVulkanHandle,
            30 => LoaderProvenance,
            31 => ReentrantJoin,
            32 => HostCallbackFailed,
            33 => DeviceLost,
            34 => ReservedNonZero,
            35 => Disposition,
            36 => AccessFlags,
            37 => OperandEncoding,
            38 => BatchId,
            39 => SessionGeneration,
            40 => SealedUseRange,
            41 => OperandUseIndex,
            42 => ProgressValue,
            43 => ContextStillAttached,
            44 => UnknownAllocationToken,
            45 => PayloadPlaceholderNonZero,
            other => return Err(HeliosTranslatorUnknownStatus { code: other }),
        };
        Result::Ok(s)
    }

    /// The highest defined discriminant. A C consumer bounds-checks against its
    /// mirror of this before indexing any status-name table.
    pub const MAX: HeliosTranslatorStatusCode =
        HeliosTranslatorStatus::PayloadPlaceholderNonZero.wire();

    #[inline]
    pub const fn is_ok(self) -> bool {
        matches!(self, HeliosTranslatorStatus::Ok)
    }
}

// ── Opaque handles ──────────────────────────────────────────────────────────

/// The ICD-owned translator instance object. Opaque by construction: the
/// consumer may hold, compare, and pass it back, and may never dereference it.
#[repr(C)]
#[derive(Debug)]
pub struct HeliosTranslatorInstanceOpaque {
    _private: [u8; 0],
}

/// A live record-only translator instance (one Mesa `vn_instance`, one HTS1
/// session, one host Venus namespace). Section 10.4 (1211-1213): multiple
/// instances in one process receive different sessions, host contexts,
/// capabilities, object namespaces, and ring allocators — so this handle is the
/// instance's whole identity and nothing about it is process-global.
pub type HeliosTranslatorHandle = *mut HeliosTranslatorInstanceOpaque;

/// The ICD-owned outer-operation scope object.
#[repr(C)]
#[derive(Debug)]
pub struct HeliosTranslatorScopeOpaque {
    _private: [u8; 0],
}

/// A live outer-operation scope: the window during which the translator may
/// record into the batch that the owning D3D11 flush or D3D12 ECL association
/// will submit. See [`HeliosTranslatorDispatchV1::open_outer_scope`].
///
/// ⚠ **Two things end a scope, not one.** The obvious one is
/// [`HeliosTranslatorDispatchV1::close_outer_scope`] on this handle. The other
/// is a [`HeliosTranslatorHostCallbacksV1::sync_progress_join`] initiated from
/// *inside* it: the join is required to seal and submit pending work, so it
/// ends this scope and reopens a fresh one on the same context. The handle is
/// dead across such a join and the caller re-acquires — see that slot's
/// "what the cut invalidates".
pub type HeliosTranslatorScope = *mut HeliosTranslatorScopeOpaque;

// ── Scope disposition ───────────────────────────────────────────────────────

/// How a scope ended. See [`HeliosOuterScopeCloseV1`].
pub const HELIOS_TRANSLATOR_SCOPE_DISPOSITION_INVALID: u32 = 0;
/// The UMD encoded the sealed batch into its command buffer **and** the outer
/// WDDM submission succeeded.
pub const HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED: u32 = 1;
/// The scope ended without a successful outer submission: the translator failed
/// mid-record, the encode failed, or `pfnRenderCb`/`pfnSubmitCommandCb` failed.
pub const HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED: u32 = 2;

/// The decoded [`HeliosOuterScopeCloseV1::disposition`].
///
/// The distinction is not bookkeeping. Section 10.4 (1346-1352): an
/// outer-allocation-backed operation "become\[s\] real only in the first actual
/// outer batch whose allocation list/GPUVA names every exact allocation", and "no
/// deferred success is exposed unless its WDDM allocation/backing reservation has
/// already succeeded". `Committed` is what tells the ICD that the deferred
/// operations recorded in this batch have actually happened; `Abandoned` is what
/// tells it they have not, and must not be reported to the application as done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosTranslatorScopeDisposition {
    /// The batch reached the host: encoded, submitted, and accepted by the
    /// runtime callback.
    Committed,
    /// The batch did not. Its deferred operations stay unrealised and its
    /// context-local batch ID is retired, never reissued — section 10.4 (1265)
    /// requires batch IDs to be "strictly increasing", which permits a gap and
    /// forbids a reuse.
    Abandoned,
}

impl HeliosTranslatorScopeDisposition {
    #[inline]
    pub const fn wire(self) -> u32 {
        match self {
            Self::Committed => HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED,
            Self::Abandoned => HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED,
        }
    }

    /// Total decode. Zero and every undefined value are
    /// [`HeliosTranslatorStatus::Disposition`]; there is no default arm, because
    /// defaulting a zeroed field to `Committed` would report unrealised deferred
    /// work as done and defaulting it to `Abandoned` would silently drop a batch.
    #[inline]
    pub const fn from_wire(value: u32) -> Result<Self, HeliosTranslatorStatus> {
        match value {
            HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED => Ok(Self::Committed),
            HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED => Ok(Self::Abandoned),
            _ => Err(HeliosTranslatorStatus::Disposition),
        }
    }
}

// ── Progress flags ──────────────────────────────────────────────────────────

/// [`HeliosSyncProgressResultV1::flags`]: the outer device is lost or removed.
///
/// The ICD must convert this to `VK_ERROR_DEVICE_LOST` and must never treat the
/// reported progress values as satisfied (section 10.9, row 2867).
pub const HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST: u32 = 1 << 0;
/// The complete set of defined [`HeliosSyncProgressResultV1::flags`] bits.
pub const HELIOS_TRANSLATOR_PROGRESS_FLAGS_MASK: u32 = HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST;

// ── Record sizes (mirrored byte-for-byte in the C header) ───────────────────
//
// Pinned as constants so the C mirror's `_Static_assert`s and this file's
// `const` assertions are checking the same literals, and so a consumer can
// bounds-check a `struct_bytes` field without `sizeof`. Every one of these is
// the x86-64 size; the ABI is x64-only (the C header enforces that with an
// `#error`) precisely so a single set of numbers can be normative.

/// [`HeliosTranslatorCreateInfoV1`] size.
pub const HELIOS_TRANSLATOR_CREATE_INFO_BYTES: u32 = 40;
/// [`HeliosTranslatorInstanceV1`] size.
pub const HELIOS_TRANSLATOR_INSTANCE_BYTES: u32 = 48;
/// [`HeliosTranslatorHostCallbacksV1`] size.
pub const HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES: u32 = 32;
/// [`HeliosTranslatorDispatchV1`] size.
pub const HELIOS_TRANSLATOR_DISPATCH_BYTES: u32 = 112;
/// [`HeliosOuterContextAttachV1`] size.
pub const HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES: u32 = 32;
/// [`HeliosQueueAttachRequestV1`] size.
pub const HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES: u32 = 32;
/// [`HeliosOuterScopeBeginV1`] size.
pub const HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES: u32 = 24;
/// [`HeliosOuterScopeCloseV1`] size.
pub const HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES: u32 = 24;
/// [`HeliosSealedBatchV1`] size.
pub const HELIOS_TRANSLATOR_SEALED_BATCH_BYTES: u32 = 72;
/// [`HeliosSealedResourceUseV1`] size — deliberately the same 40 bytes as HOB1's
/// use record ([`crate::wddm::HeliosOuterBatchUseV1`]), with every field at the
/// same offset as the HOB1 field it stands in for.
///
/// ⚠ **Same offsets, not the same fields.** Three of the eight carry a
/// different meaning, and they are exactly the WDDM identity the ICD may not
/// supply: `outer_allocation_token` at 0 stands where `address_or_index` does,
/// `byte_offset` at 16 where `expected_allocation_generation` does, and
/// `reserved0` at 28 where `identity_kind` does. The other five —
/// `byte_length`, `access_flags`, `operand_count`, `first_operand`, `reserved1`
/// — are the same field with the same meaning. So this is a **field-by-field
/// conversion, never a memcpy plus a patch**: a block copy would leave
/// `identity_kind` holding a reserved zero, which is an invalid arm, and the
/// encoder that "only patched the identity pair" would never notice. The
/// `const` block at the bottom of this file asserts both sides' offsets so the
/// parallel cannot silently drift.
pub const HELIOS_TRANSLATOR_SEALED_USE_BYTES: u32 = 40;
/// [`HeliosSealedOperandV1`] size — the same 16 bytes as HOB1's typed operand.
pub const HELIOS_TRANSLATOR_SEALED_OPERAND_BYTES: u32 = 16;
/// [`HeliosSealedBatchCopyV1`] size.
pub const HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES: u32 = 56;
/// [`HeliosSyncProgressJoinV1`] size.
pub const HELIOS_TRANSLATOR_SYNC_PROGRESS_JOIN_BYTES: u32 = 24;
/// [`HeliosSyncProgressResultV1`] size.
pub const HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES: u32 = 32;
/// [`HeliosTranslatorRefusalCountersV1`] size.
pub const HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES: u32 = 112;

/// The exact byte size the [`HeliosTranslatorDispatchV1::build_queue_attach`] out
/// buffer must have: [`crate::translation_session::HELIOS_HQA1_SIZE`].
///
/// Restated here as a `u32` so a slot's parameter contract can be stated in the
/// units it is checked in, without a consumer having to reach for a sibling
/// record's `sizeof`. The `const` block at the bottom pins it against
/// [`HeliosQueueAttachV1`], and the C mirror carries the twin pin against both
/// `HELIOS_HQA1_SIZE` and `sizeof(HeliosQueueAttachV1)`, so the restatement
/// cannot drift from what it restates.
pub const HELIOS_TRANSLATOR_HQA1_BYTES: u32 =
    crate::translation_session::HELIOS_HQA1_SIZE as u32;

/// The exact byte size of one [`crate::translation_session::HeliosTranslationEndpointV1`]
/// element in the [`HeliosTranslatorDispatchV1::enumerate_endpoints`] array.
/// Pinned for the same reason as [`HELIOS_TRANSLATOR_HQA1_BYTES`].
pub const HELIOS_TRANSLATOR_ENDPOINT_BYTES: u32 =
    crate::translation_session::HELIOS_HTS1_ENDPOINT_SIZE as u32;

// ── Create info / instance ──────────────────────────────────────────────────

/// Everything the ICD needs to create one record-only translator instance.
///
/// The UMD bridge fills this and passes it to the entry point. It carries **no**
/// handle: not the adapter's, not a KMT device's, not a D3D device's. The ICD
/// opens the exact adapter itself from the LUID and creates its own raw KMT
/// device (section 10.4, line 1189), which is what makes the "identical
/// `hKmdProcess`, adapter object, node, and package generation" gate at line
/// 1208-1210 a *mandatory observed target gate rather than an inference from a
/// PID".
///
/// 40 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosTranslatorCreateInfoV1 {
    /// `== HELIOS_TRANSLATOR_CREATE_INFO_BYTES`. Exact, never "at least".
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// `== crate::HELIOS_PACKAGE_GENERATION`. Zero is not a wildcard.
    pub package_generation: u64,
    /// `== HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`.
    pub submission_mode: u32,
    /// How many physical lower-queue endpoints this translator will need, in
    /// `1..=`[`crate::translation_session::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION`].
    /// It becomes the HTS1 `INIT` request's requested capacity, and the granted
    /// capacity is reported back in [`HeliosTranslatorInstanceV1::endpoint_capacity`].
    pub requested_endpoint_capacity: u32,
    /// Low half of the adapter LUID the outer D3D device is running on.
    ///
    /// The LUID is the OS adapter identity, not a handle: it names no object,
    /// confers no access, and is already public to any process that can call
    /// `IDXGIAdapter::GetDesc`. It is here because the alternative — letting the
    /// ICD *discover* which adapter to open — is exactly the discovery section 3
    /// (363-369) forbids. A zero LUID is [`HeliosTranslatorStatus::AdapterLuid`].
    pub adapter_luid_low: u32,
    /// High half of the adapter LUID, signed to mirror the OS `LUID::HighPart`.
    pub adapter_luid_high: i32,
    /// The up half: the bridge's callback table. **Required, never NULL.**
    ///
    /// The pointer and the table it names must remain valid until
    /// [`HeliosTranslatorDispatchV1::destroy_instance`] returns; the ICD may
    /// either copy the table or retain the pointer, so the bridge must treat it
    /// as retained. A NULL or invalid table is
    /// [`HeliosTranslatorStatus::HostCallbacks`] and the instance is not created.
    pub host_callbacks: *const HeliosTranslatorHostCallbacksV1,
}

/// What the entry point returns: the instance handle, its const dispatch table,
/// the one `VkInstance` its `INIT` created, and the two session facts the UMD
/// needs in order to encode HOB1/HOS1.
///
/// The caller zero-initialises this and sets `struct_bytes`/`abi_version` before
/// the call; the ICD fills the rest. On any refusal the ICD leaves it zeroed
/// apart from those two fields — there is no partially created instance.
///
/// 48 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosTranslatorInstanceV1 {
    /// `== HELIOS_TRANSLATOR_INSTANCE_BYTES`, set by the caller.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`, set by the caller.
    pub abi_version: u32,
    /// The live instance. NULL on any refusal.
    pub handle: HeliosTranslatorHandle,
    /// The instance's dispatch table.
    ///
    /// ICD-owned, `const`, and valid exactly as long as `handle` is: it is not
    /// copied out, not freed by the caller, and not shared between instances
    /// (two `vn_instance` objects in one process are two sessions and two tables,
    /// section 10.4 lines 1211-1213). The caller must run
    /// [`HeliosTranslatorDispatchV1::validate`] on it before using a single slot.
    pub dispatch: *const HeliosTranslatorDispatchV1,
    /// The one `VkInstance` this instance's HTS1 `INIT` created. Never NULL on
    /// success.
    ///
    /// ⭐ **This is the only route by which a translator obtains a
    /// `VkInstance`, and it exists because there is no other legal one.**
    /// Section 10.4's INIT paragraph creates "one distinct host virgl/Venus
    /// context and `VkInstance`" and then says "No second `VkInstance` may be
    /// created in that host context"; section 10.9's HTS1 row makes "a second
    /// instance is requested in one host context" a *fail translator device
    /// creation* condition. A translator that resolved `vkCreateInstance`
    /// through [`HeliosTranslatorDispatchV1::get_instance_proc_addr`] and
    /// minted its own would be precisely that case, so that name is refused.
    ///
    /// Typed `*mut c_void` rather than `VkInstance` because this crate is
    /// `no_std` and must not depend on `vulkan.h`/`ash`; `VkInstance` is a
    /// dispatchable (pointer) handle, so the representation is exact. The
    /// consumer casts it once, at the boundary.
    ///
    /// ⛔ It is **not** an identity the ICD looks anything up by: two
    /// translator instances in one process have two `VkInstance`s and two
    /// sessions, and the ICD compares a presented handle against its own
    /// instance's rather than searching a table
    /// ([`HeliosTranslatorStatus::ForeignVulkanHandle`]).
    pub vk_instance: *mut c_void,
    /// The nonzero HTS1 session generation this instance's `INIT` returned.
    ///
    /// The UMD needs it to fill HOB1 `offset 16` and HOS1 `offset 16`; it is a
    /// generation counter, not a key, and no lookup uses it (invariant 10).
    pub session_generation: u64,
    /// The endpoint capacity the session actually granted:
    /// `1..=requested_endpoint_capacity`, and never more than requested.
    pub endpoint_capacity: u32,
    /// Echoed [`HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`].
    ///
    /// A cross-check, not the tag: section 13.2 (3359) requires translator
    /// instances to "carry a non-forgeable record-only tag", which is ICD-internal
    /// state. This field lets the bridge assert that the instance it got is the
    /// mode it asked for, and it must refuse the instance if it is not.
    pub submission_mode: u32,
}

// ── The up half: callbacks the UMD bridge implements ────────────────────────

/// A blocking C60 synchronous-progress join request. See
/// [`HeliosTranslatorHostCallbacksV1::sync_progress_join`].
///
/// 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosSyncProgressJoinV1 {
    /// `== HELIOS_TRANSLATOR_SYNC_PROGRESS_JOIN_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// The HQC1 value that must be complete before this call returns.
    ///
    ///   - **Zero** means "everything pending on this outer context": the bridge
    ///     seals and submits whatever the translator has recorded, signals the
    ///     next HQC1 value from that exact context, and waits for it. This is the
    ///     `vkQueueWaitIdle` / `vkDeviceWaitIdle` / teardown form, and it is what
    ///     sections 17.4 (4125-4128) and 17.5 (4239-4243) describe.
    ///   - **Nonzero** means "at least this value", where the value came from a
    ///     [`HeliosOuterScopeCloseV1::progress_value`] the bridge reported for a
    ///     batch this instance recorded. This is the fence-status /
    ///     query-`WAIT` form.
    ///
    /// A value the bridge has never issued is
    /// [`HeliosTranslatorStatus::HostCallbackFailed`]; it is never waited on
    /// speculatively, because a wait for a value that will never be signalled is
    /// the hang section 10.9's HQC1 row exists to prevent.
    pub required_progress_value: u64,
    /// Anti-stale echo: the generation of the outer context `host_context_cookie`
    /// names, as [`HeliosOuterContextAttachV1::context_generation`] recorded it.
    ///
    /// ⛔ **This field is why the up half is not the one place in the ABI where a
    /// stale value is a use-after-free.** Every other record here carries an
    /// anti-stale echo, and the join is the *only* call in either direction that
    /// makes one party dereference a pointer the other party owns. Without it, a
    /// D3D12 app destroying a command queue on thread A (bridge frees its queue
    /// object; `detach_outer_context` in flight) while thread B sits inside
    /// `vkWaitForFences` gives the bridge a cookie it has already freed and *no
    /// field to compare it against*. Section 14's "Late host callback" row
    /// requires exactly this: "context … generations are validated; stale
    /// callback releases only its own held ref and cannot complete or replace a
    /// newer object."
    ///
    /// The bridge compares it against the live generation of the context that
    /// cookie belongs to and refuses a mismatch with
    /// [`HeliosTranslatorStatus::UnknownContext`] — a refusal, never a lookup
    /// that finds the right context. Generations are never reused within a
    /// session (section 10.4), so a mismatch is always a stale caller and never
    /// an ambiguity.
    pub context_generation: u64,
}

/// The outer context's progress, as the bridge knows it.
///
/// 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosSyncProgressResultV1 {
    /// `== HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES`, set by the ICD.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`, set by the ICD.
    pub abi_version: u32,
    /// The highest HQC1 value known complete on this outer context.
    ///
    /// ⛔ This is *outer GPU completion*, and only that. Section 10.1 invariant 15
    /// and section 10.9 (2868): ring-zero/control completion never satisfies a GPU
    /// wait, a queue idle, a device idle, a query `WAIT`, or a destruction
    /// dependency, and no context may borrow another's fence.
    pub completed_progress_value: u64,
    /// The highest HQC1 value the bridge has submitted-and-signalled on this
    /// context. `completed <= last_submitted` always. A nonblocking status query
    /// that finds `required > completed` returns "not ready" locally, with no
    /// control round trip (section 10.4, lines 1359-1360).
    pub last_submitted_progress_value: u64,
    /// `HELIOS_TRANSLATOR_PROGRESS_FLAG_*`.
    pub flags: u32,
    /// Reserved, zero.
    pub reserved: u32,
}

/// `sync_progress_join` — **implemented by the UMD bridge, called by the ICD.**
///
/// MAY BLOCK. See [`HeliosTranslatorHostCallbacksV1::sync_progress_join`].
pub type PfnHeliosTranslatorSyncProgressJoin = Option<
    extern "C" fn(
        host_context_cookie: *mut c_void,
        request: *const HeliosSyncProgressJoinV1,
        out_result: *mut HeliosSyncProgressResultV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// `sync_progress_query` — **implemented by the UMD bridge, called by the ICD.**
///
/// MUST NOT BLOCK. See [`HeliosTranslatorHostCallbacksV1::sync_progress_query`].
///
/// `context_generation` is the same anti-stale echo
/// [`HeliosSyncProgressJoinV1::context_generation`] carries, passed as a scalar
/// because the nonblocking form needs no request record. It is not optional:
/// the query dereferences the bridge's cookie exactly as the join does, so it
/// needs exactly the same protection against a context retired on another
/// thread.
pub type PfnHeliosTranslatorSyncProgressQuery = Option<
    extern "C" fn(
        host_context_cookie: *mut c_void,
        context_generation: u64,
        out_result: *mut HeliosSyncProgressResultV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// The up half of the interface: the *only* two things the ICD may ask the D3D
/// UMD bridge to do.
///
/// Section 17.5 (4239-4240): "`bridge12.rs` exposes only bounded pure-control and
/// HQC1-join callbacks to the record-only Mesa instance." Two slots, both about
/// the HQC1 milestone, and nothing else — no allocate, no submit-on-my-behalf, no
/// present, no resource open, no D3D or DXGI verb of any kind. Adding a third
/// slot is an ABI-version bump and must be justified against section 13.1's
/// acyclicity proof, because every up-slot is a potential edge back into D3D.
///
/// 32 bytes.
//
// No `PartialEq`: two tables are equal when they are the same table, and
// comparing function pointers is not a meaningful way to establish that (the same
// function can have two addresses across codegen units, and two functions can be
// merged to one). A consumer compares the *pointer* it was handed, never the
// contents.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HeliosTranslatorHostCallbacksV1 {
    /// `== HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// `== crate::HELIOS_PACKAGE_GENERATION`.
    pub package_generation: u64,

    /// **Direction:** ICD → UMD bridge. **May block: YES.**
    /// **Locks:** the ICD holds no translator, session, endpoint, scope, or
    /// object-graph lock across this call; the bridge holds no UMD, DXVK/vkd3d,
    /// Mesa, or session lock while waiting (section 13.3; section 17.4:4126-4128).
    ///
    /// The C60 GPU-dependent synchronous join. Section 10.4 (1354-1360): queue
    /// idle, device idle, fence status/wait, query `WAIT`, and teardown "first
    /// force the owning outer UMD to submit pending batches, signal that context's
    /// private HQC1 value, and perform one event-backed CPU wait. Only after that
    /// exact endpoint milestone completes may HVC1 fetch bounded result bytes."
    /// This slot is that whole sentence: seal-and-submit pending work on the
    /// exact outer context named by `host_context_cookie`, FromGpu-signal HQC1
    /// from that same context, then one FromCpu event wait.
    ///
    /// **Reentrancy: the cut-and-reopen protocol.** This is the ABI's one
    /// sanctioned reentrant edge. While the ICD's Vulkan call is on the stack
    /// the bridge calls back *down* into this table for the same outer context,
    /// because that is how "submit pending batches" is expressed here. It is
    /// safe because no lock is held in either direction — but the *order* is
    /// not free, because the triggering call is almost always made while a
    /// scope is already open (the translator is recording inside the D3D11
    /// flush or D3D12 ECL that opened it). Two orders do not work and must not
    /// be attempted:
    ///
    ///   - opening a second scope first is
    ///     [`HeliosTranslatorStatus::ScopeAlreadyOpen`], so the join would fail
    ///     on a legal API sequence;
    ///   - sealing and closing the caller's scope and stopping there leaves the
    ///     bridge's own outer frame holding a dead
    ///     [`HeliosTranslatorScope`] — a use-after-free with no defined refusal.
    ///
    /// The bridge therefore performs exactly this, in this order, on the
    /// calling thread:
    ///
    ///   1. if a scope is live on this context **and** the translator has
    ///      recorded into it: [`HeliosTranslatorDispatchV1::seal_outer_scope`],
    ///      [`HeliosTranslatorDispatchV1::copy_sealed_batch`], encode HOB1,
    ///      submit it through the runtime callback, FromGpu-signal the next
    ///      HQC1 value `V` from this exact outer context, then
    ///      [`HeliosTranslatorDispatchV1::close_outer_scope`] with
    ///      `COMMITTED` and `V`;
    ///   2. if a scope is live but nothing was recorded: close it `ABANDONED`
    ///      (no empty batch is ever sealed — a sealed batch has nonzero
    ///      `payload_bytes`) and signal `V` from the context anyway, which is
    ///      what "signal that context's private HQC1 value" requires and is a
    ///      signal on the *outer* context, never a cleanup submission on a
    ///      lower queue;
    ///   3. **reopen**: [`HeliosTranslatorDispatchV1::open_outer_scope`] on the
    ///      same context, and store the new handle in place of the old one. The
    ///      bridge is the sole owner of the scope handle, so this replacement
    ///      is entirely within its own frame and no dangling handle ever
    ///      exists. The outer operation continues recording into the new scope
    ///      when the join returns, and the batch ID advances (gaps are legal,
    ///      reuse is not);
    ///   4. perform the one event-backed FromCpu wait for `V` (or for
    ///      `required_progress_value` when it is nonzero), then fill
    ///      [`HeliosSyncProgressResultV1`].
    ///
    /// **What the cut invalidates, and what the ICD must re-acquire.** Step 1
    /// is a *seal*, and a seal is terminal: section 10.4 (1354-1360) says the
    /// join must "force the owning outer UMD to submit pending batches", and
    /// the only way this ABI expresses that is
    /// [`HeliosTranslatorDispatchV1::seal_outer_scope`] followed by
    /// [`HeliosTranslatorDispatchV1::close_outer_scope`]. So a join does not
    /// suspend the caller's scope — it **ends** it, and the scope the caller
    /// resumes into is a different one with a different batch. Three
    /// consequences, all of them the ICD's obligation:
    ///
    ///   1. **The scope handle is dead.** Its thread-current scope identity
    ///      changes across a join by design; it re-reads it on return, and must
    ///      never cache a [`HeliosTranslatorScope`] across an up-call. This is
    ///      the one place in the ABI where a scope handle is invalidated by
    ///      something other than `close_outer_scope` being called on it
    ///      directly.
    ///   2. **Everything derived from the sealed batch is dead with it** — the
    ///      context-local batch ID, every index into that batch's use and
    ///      operand tables, and any partially built record the ICD was
    ///      accumulating for it. The reopened scope starts an empty batch at
    ///      the next batch ID (gaps are legal, reuse is not), and recording
    ///      resumes there. Nothing already sealed is re-recorded into it.
    ///   3. **A join may therefore only be initiated at a complete generated
    ///      command/API operation boundary**, exactly like the size split of
    ///      section 10.4 (1297-1304), and for the same reason: the seal it
    ///      performs cannot bisect a generated operation. A GPU-dependent
    ///      synchronous Vulkan call is such a boundary — it is *between*
    ///      operations by construction — which is why the triggering set is
    ///      the fence/query/idle calls and nothing else. An ICD that called up
    ///      from the middle of expanding one API command into several Venus
    ///      commands would split where the contract forbids splitting, and no
    ///      refusal here could detect it: the ICD is the only party that knows
    ///      where its operation boundaries are.
    ///
    /// The pre-join batch's deferred outer-allocation-backed operations are
    /// realised (`COMMITTED`) or not (`ABANDONED`) as of the close in step 1
    /// or 2, on the ordinary rules of
    /// [`HeliosTranslatorDispatchV1::close_outer_scope`]; the join adds no
    /// third disposition and no deferred re-realisation in the new scope.
    ///
    /// A join nested inside a join is a cycle and is refused with
    /// [`HeliosTranslatorStatus::ReentrantJoin`], counted by
    /// [`HeliosTranslatorRefusalCountersV1::reentrant_join_refused`]. **The ICD
    /// detects it**, before the callback is reached: it is the only party that
    /// initiates joins, so it is the only party that can know one is already in
    /// progress on this thread.
    ///
    /// **Never** a ring-zero or control-fence approximation
    /// (section 10.4, line 1366), never a poll, never a `vkQueueWaitIdle` on a
    /// lower queue (section 17.5, line 4242).
    pub sync_progress_join: PfnHeliosTranslatorSyncProgressJoin,

    /// **Direction:** ICD → UMD bridge. **May block: NO.**
    /// **Locks:** none held by either side; the bridge must answer from state it
    /// already has (its last signalled value and the monitored fence's current
    /// value), with no runtime callback, no event wait, and no re-entry into the
    /// ICD.
    ///
    /// The nonblocking half of C60. Section 10.4: "A nonblocking status query
    /// returns the locally known not-ready state without a control round trip
    /// when the milestone has not completed." `vkGetFenceStatus`,
    /// `vkGetEventStatus`, and `vkGetQueryPoolResults` *without* `WAIT` use this
    /// and must not be turned into a join.
    ///
    /// ⛔ **Its result is checked with
    /// [`HeliosSyncProgressResultV1::validate_query`], never with
    /// `validate_join`.** `completed < last_submitted` is the *normal* answer
    /// here — it is precisely what "not ready" means — while for a join it is a
    /// failure. Running the join check on a query result would make
    /// `vkGetFenceStatus` fail (as a callback failure, which this file maps to
    /// device removal) whenever any work is outstanding: the exact inversion of
    /// the required behaviour.
    pub sync_progress_query: PfnHeliosTranslatorSyncProgressQuery,
}

// ── Down-half parameter records ─────────────────────────────────────────────

/// What the bridge asks the ICD to seal into an HQA1 packet.
///
/// Section 10.4 (1215-1218): "Before the D3D UMD calls the runtime's
/// context-create callback, its direct bridge selects the translator's physical
/// lower-queue endpoint and supplies the following 72-byte pointer-free `HQA1` as
/// the complete create-context private data." The bridge selects; the ICD, which
/// is the only party that holds the session generation and the admission
/// capability, fills the packet.
///
/// 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosQueueAttachRequestV1 {
    /// `== HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// The context generation the UMD has chosen for the context it is about to
    /// create: nonzero, monotonically increasing, and never reused within this
    /// HTS1 session (section 10.4, HQA1 table row `offset 56`). The UMD owns it
    /// because the UMD owns the outer context; the ICD records it so that a
    /// later [`HeliosTranslatorDispatchV1::attach_outer_context`] and every
    /// sealed batch can be checked against it.
    pub context_generation: u64,
    /// The endpoint the bridge selected, from
    /// [`HeliosTranslatorDispatchV1::enumerate_endpoints`] and nowhere else.
    /// Several logical D3D12 queues may select the same endpoint
    /// (section 10.6, step 1).
    pub endpoint_id: u32,
    /// `crate::translation_session::HELIOS_ENGINE_CLASS_*` — the engine class of
    /// the **outer D3D context** this HQA1 is for: the D3D12 command-queue type,
    /// or the D3D11 node.
    ///
    /// ⭐ **The bridge states it; the ICD checks it against the endpoint it
    /// resolves.** HQA1's `offset 44` row is "exact graphics/compute/copy class
    /// admitted for the outer context", and the outer context is a bridge fact
    /// that no other field of this request carries. Without it the ICD could
    /// only copy the selected endpoint's own class into the packet, and the
    /// KMD's later `EngineClassMismatch` check would compare that endpoint
    /// against itself — a tautology, and
    /// [`HeliosTranslatorStatus::EngineClass`] would be a declared refusal that
    /// nothing can trigger. With it, a D3D12 COPY queue that selects a GRAPHICS
    /// endpoint is refused before any packet exists, and the KMD's check
    /// becomes a genuine two-party agreement.
    pub engine_class: u32,
    /// Exactly one of
    /// [`crate::translation_session::HELIOS_HQA1_FLAG_D3D11_PHYSICAL`] /
    /// [`crate::translation_session::HELIOS_HQA1_FLAG_D3D12_VIRTUAL`].
    pub context_flags: u32,
    /// Reserved, zero.
    pub reserved: u32,
}

/// Confirmation that a runtime create-context succeeded and the KMD accepted the
/// HQA1, plus the per-context cookie the up half will be called with.
///
/// 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosOuterContextAttachV1 {
    /// `== HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// The generation from the [`HeliosQueueAttachRequestV1`] whose packet the
    /// runtime accepted. A generation the ICD never sealed a packet for is
    /// [`HeliosTranslatorStatus::UnknownContext`]; one already attached is
    /// [`HeliosTranslatorStatus::ContextAlreadyAttached`] (invariant 13: exactly
    /// once).
    pub context_generation: u64,
    /// Anti-stale cross-check: the endpoint from the same request.
    pub endpoint_id: u32,
    /// Anti-stale cross-check: the flags from the same request.
    pub context_flags: u32,
    /// The bridge's opaque per-outer-context cookie.
    ///
    /// The ICD stores it and passes it, unread, as the first argument of every
    /// up-call for this context. **The ICD never dereferences it, never compares
    /// it to anything but itself, and never copies it into a record.** It is the
    /// bridge's own queue/context object in practice; that it is a pointer is an
    /// in-process implementation detail, and it is the reason the up-calls need
    /// no handle namespace and no lookup.
    pub host_context_cookie: *mut c_void,
}

/// Opening an outer-operation scope.
///
/// 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosOuterScopeBeginV1 {
    /// `== HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// The attached outer context this scope records for.
    pub context_generation: u64,
    /// Anti-stale cross-check against the endpoint recorded at attach; a
    /// mismatch is [`HeliosTranslatorStatus::UnknownEndpoint`], never a
    /// re-selection. The endpoint is fixed at HQA1 time and cannot be changed
    /// afterwards (invariant 13: "no attach, capability lookup, or endpoint
    /// discovery is permitted after context creation").
    pub endpoint_id: u32,
    /// Reserved, zero.
    pub reserved: u32,
}

/// Closing an outer-operation scope, and reporting what the outer submission did.
///
/// 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosOuterScopeCloseV1 {
    /// `== HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// `HELIOS_TRANSLATOR_SCOPE_DISPOSITION_*`. Zero is refused; see
    /// [`HeliosTranslatorScopeDisposition::from_wire`].
    pub disposition: u32,
    /// Reserved, zero.
    pub reserved: u32,
    /// On `COMMITTED`: the HQC1 value the bridge signalled from this exact outer
    /// context **after** the submission that carried this batch — the value that
    /// completes when this batch's GPU work completes (object table row
    /// "Outer translated progress HQC1", section 12). Nonzero and strictly
    /// increasing on that context.
    ///
    /// It is the correlation the ICD later passes back as
    /// [`HeliosSyncProgressJoinV1::required_progress_value`], which is how a
    /// `vkGetFenceStatus` or a query `WAIT` names *this* batch's completion
    /// rather than the whole context's backlog. (The KMD lane learned the cost of
    /// the superset reading the hard way: waiting on a whole backlog instead of
    /// the frame's own boundary is the present-queue stall in ROADMAP WS2.)
    ///
    /// On `ABANDONED`: zero. There is no completion to name.
    pub progress_value: u64,
}

// ── The sealed batch ────────────────────────────────────────────────────────

/// One entry of the sealed batch's **complete resource-use table**, in the token
/// form the ICD can produce.
///
/// Section 10.4 point 3 requires the seal to carry the "complete resource-use
/// table", and point 4 requires it returned synchronously to the outer bridge.
///
/// # The parallel with HOB1's use record, stated by offset
///
/// It is the same 40 bytes as [`crate::wddm::HeliosOuterBatchUseV1`], and the
/// fields the two records **share sit at identical offsets** — that is checked
/// at compile time at the bottom of this file, so the claim cannot rot. The
/// three fields that differ are exactly the three that are WDDM identity:
///
/// | Offset | HOB1 `HeliosOuterBatchUseV1` | this record | same? |
/// |---:|---|---|---|
/// | 0 | `address_or_index` | `outer_allocation_token` | ⛔ identity |
/// | 8 | `byte_length` | `byte_length` | ✔ |
/// | 16 | `expected_allocation_generation` | `byte_offset` | ⛔ identity |
/// | 24 | `access_flags` | `access_flags` | ✔ |
/// | 28 | `identity_kind` (u16) | `reserved0` (u16) | ⛔ identity |
/// | 30 | `operand_count` (u16) | `operand_count` (u16) | ✔ |
/// | 32 | `first_operand` | `first_operand` | ✔ |
/// | 36 | `reserved` | `reserved1` | ✔ |
///
/// ⛔ **This is still not a `memcpy`.** Two POD structs of the same size whose
/// three identity fields have different meanings are exactly the shape that
/// invites a block copy plus a patch, and a block copy would leave
/// `identity_kind` zero — an invalid arm — while the encoder that "only patched
/// the identity pair" would never notice. The conversion is field by field, and
/// the encoder **writes** `identity_kind`; it never inherits it.
///
/// The UMD converts, because the UMD is the encoder (section 10.4: "The outer
/// UMD encodes the sealed batch as complete bounded `HeliosOuterBatchV1` (HOB1)
/// command bytes in its runtime-approved command buffer"):
///
/// | HOB1 field | D3D11 physical (`identity_kind = 1`) | D3D12 virtual (`identity_kind = 2`) |
/// |---|---|---|
/// | `address_or_index` | the allocation-list index the UMD assigned to `outer_allocation_token`, upper 32 bits zero | the allocation's GPUVA base plus `byte_offset` |
/// | `expected_allocation_generation` | the HWA2 generation the UMD observed for that allocation | same |
/// | `identity_kind` | `1`, written by the encoder | `2`, written by the encoder |
/// | `byte_length`, `access_flags`, `operand_count`, `first_operand` | copied | copied |
///
/// ⛔ The ICD never sees either column on the right. That is the point.
///
/// 40 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeliosSealedResourceUseV1 {
    /// The UMD-assigned opaque token naming the outer WDDM allocation this use
    /// touches.
    ///
    /// **Opaque to the ICD**: echoed, compared for equality when deduplicating,
    /// and never interpreted, indexed, or stored beyond the batch. It is not a
    /// handle, not a GPUVA, not a `resid`, and confers nothing — the UMD chooses
    /// the values and is free to make them process-unique ordinals.
    ///
    /// ⚠ **How a token reaches the ICD is not part of this table.** It arrives on
    /// the resource-creation path the DXVK/vkd3d and UMD lanes own, alongside the
    /// deferred frontend handles of section 10.4 (1346-1352). This ABI defines
    /// only that the sealed use table names allocations by token; see the lane's
    /// AMBIGUITIES note and cross-lane request.
    pub outer_allocation_token: u64,
    /// Byte length of the used range. Nonzero. **HOB1 offset 8, unchanged.**
    pub byte_length: u64,
    /// Byte offset of the used range within that allocation. Sits where HOB1
    /// keeps `expected_allocation_generation`, because both are the half of the
    /// identity pair the other side does not have.
    ///
    /// Must be zero on a D3D11 physical context: HOB1's `identity_kind == 1` arm
    /// is an allocation-list index with no room for a sub-allocation offset, so a
    /// nonzero value there is [`HeliosTranslatorStatus::D3D11SubrangeUse`] at
    /// seal time — a producer-side refusal, because the ICD knows the context
    /// kind and the UMD would otherwise have to fail an already-sealed batch.
    pub byte_offset: u64,
    /// `crate::wddm::HELIOS_HOB1_ACCESS_*`: nonzero, within
    /// [`crate::wddm::HELIOS_HOB1_ACCESS_MASK`], and never `PRIMARY_WRITE`
    /// without `WRITE`. Copied into HOB1 unchanged. **HOB1 offset 24.**
    pub access_flags: u32,
    /// Reserved, zero. Sits where HOB1 keeps `identity_kind`, which is WDDM
    /// identity the ICD must never supply: the encoder **writes** the kind.
    pub reserved0: u16,
    /// How many operands of the sealed operand table belong to this use.
    /// **HOB1 offset 30.**
    pub operand_count: u16,
    /// Index of this use's first operand. The `{first_operand, operand_count}`
    /// runs tile the operand table in order, exactly as HOB1 requires.
    /// **HOB1 offset 32.**
    pub first_operand: u32,
    /// Reserved, zero. **HOB1 offset 36.**
    pub reserved1: u32,
}

/// One entry of the sealed batch's typed operand table, in **payload-relative**
/// form.
///
/// The 16 bytes and the field roles are HOB1's
/// ([`crate::wddm::HeliosOuterBatchOperandV1`]), with one deliberate difference
/// that earns the separate type: HOB1's `payload_offset` is measured **from the
/// HOB1 start**, and the ICD does not know where in the record the UMD will place
/// the payload. So this record carries the offset **from the payload start**, and
/// the UMD adds HOB1's `payload_offset` when it encodes.
///
/// Reusing HOB1's type with a different meaning for the same field name is
/// exactly the drift the crate's `_pad`/`adopt_resource_id` history warns about,
/// so the field is renamed as well as re-based.
///
/// ⛔ Like HOB1's operand, this identifies "only a generated resource operand
/// whose payload bytes are zero" (section 10.4). The ICD writes zeros at the
/// named offset; a nonzero placeholder is how a raw host resource id would reach
/// the host at a position the operand table blesses.
///
/// That rule is **producer-checked**, not merely asserted:
/// [`Self::validate_payload_zero`] reads the `encoded_width` bytes the operand
/// names and refuses a nonzero placeholder with
/// [`HeliosTranslatorStatus::PayloadPlaceholderNonZero`], for the same reason
/// [`HeliosTranslatorStatus::D3D11SubrangeUse`] is producer-side: the consumer
/// would otherwise have to fail an already-sealed batch. `crate::wddm` carries
/// the downstream backstop, so this is defence in depth on the leakage rule the
/// whole interface exists to keep.
///
/// 16 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeliosSealedOperandV1 {
    /// Offset of the operand's encoded bytes **from the start of the Venus
    /// payload**, [`crate::wddm::HELIOS_HOB1_OPERAND_ALIGN`]-aligned, with the
    /// whole `encoded_width` inside the payload.
    pub payload_relative_offset: u32,
    /// Index into the sealed resource-use table.
    pub use_index: u32,
    /// `crate::wddm::HELIOS_HOB1_OPERAND_KIND_*`.
    pub operand_kind: u16,
    /// 4 or 8.
    pub encoded_width: u16,
    /// Reserved, zero.
    pub reserved: u32,
}

/// The seal descriptor: everything section 10.4 point 3 requires a sealed batch
/// to be stamped with, returned synchronously to the outer bridge (point 4).
///
/// Point 3's list is "a version, HTS1/session generation, endpoint ID, owning
/// outer-context/queue generation, monotonically increasing **context-local**
/// batch ID, byte length, checksum, and complete resource-use table". Every item
/// is here except one, and the exception is deliberate:
///
/// **The checksum.** HOB1's CRC64 covers the *whole assembled record with the CRC
/// field zeroed*, and line 1398 makes the UMD the assembler. A checksum computed
/// by the ICD over a different byte image would be meaningless, so the UMD
/// computes the HOB1 CRC with [`crate::wddm`] as it encodes. What the ICD does
/// provide is [`Self::payload_crc64`] over the payload bytes it produced, which
/// catches a mis-copy between `copy_sealed_batch` and the encoder — the one
/// window the HOB1 CRC cannot cover, because the HOB1 CRC is computed after it.
///
/// Point 3 also ends with "no field orders work against another WDDM context",
/// and nothing here can: `batch_id` is context-local, and the only cross-context
/// order that exists is an explicit native-fence dependency on the outer queues
/// (section 10.4, lines 1246-1248).
///
/// 72 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeliosSealedBatchV1 {
    /// `== HELIOS_TRANSLATOR_SEALED_BATCH_BYTES`, set by the caller before the
    /// call and re-checked by the ICD.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// `== crate::HELIOS_PACKAGE_GENERATION`. Copied into HOB1 `offset 8`.
    pub package_generation: u64,
    /// The live HTS1 session generation. Copied into HOB1 `offset 16`.
    pub session_generation: u64,
    /// The owning outer context. Copied into HOB1 `offset 24`.
    pub context_generation: u64,
    /// Context-local batch ID: nonzero and strictly increasing on this context
    /// only. Copied into HOB1 `offset 32` and HOS1 `offset 40`.
    ///
    /// The ICD owns it because the ICD is the sealer (point 3). An abandoned
    /// batch's ID is retired, never reissued; gaps are legal, reuse is not.
    pub batch_id: u64,
    /// Exact size of the finite Venus payload the ICD will copy out. Nonzero;
    /// it becomes HOB1 `offset 60` (`payload bytes`).
    pub payload_bytes: u64,
    /// CRC64-ECMA-182 ([`crate::wddm::HELIOS_CRC64_ECMA182_POLY`]) over the
    /// `payload_bytes` payload bytes only. A copy integrity cross-check, not
    /// identity and not the HOB1 checksum — see the type doc.
    pub payload_crc64: u64,
    /// The endpoint this batch is bound to. Copied into HOB1 `offset 40` and
    /// HOS1 `offset 32`.
    pub endpoint_id: u32,
    /// Exactly one HQA1/HOB1 context-kind flag. Copied into HOB1 `offset 44`.
    pub context_flags: u32,
    /// Number of [`HeliosSealedResourceUseV1`] entries. At most
    /// [`crate::wddm::HELIOS_HOB1_MAX_USE_RECORDS`]; the D3D11 arm additionally
    /// requires them to be the complete **unique**-allocation closure.
    pub use_count: u32,
    /// Number of [`HeliosSealedOperandV1`] entries. At most
    /// [`crate::wddm::HELIOS_HOB1_MAX_OPERAND_RECORDS`].
    pub operand_count: u32,
}

/// Where the bridge wants the sealed batch's three arrays written.
///
/// The bridge sizes its buffers from the [`HeliosSealedBatchV1`] the seal
/// returned, so every capacity is known-exact before the call; a capacity below
/// the sealed size is [`HeliosTranslatorStatus::BufferTooSmall`] and nothing is
/// written. There is no partial copy and no resize handshake.
///
/// ⚠ The three destinations are **caller-owned in-process memory**. For D3D12
/// they are typically inside the C65 HOB1 GPUVA pool extent the UMD reserved
/// (section 10.6); for D3D11 they are inside the runtime-approved command buffer.
/// Neither is a fact the ICD may rely on or learn: it writes bytes at pointers,
/// and it is the UMD that knows those bytes are about to become a WDDM command.
///
/// 56 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosSealedBatchCopyV1 {
    /// `== HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// Destination for the Venus payload. Non-NULL.
    pub payload: *mut c_void,
    /// Capacity at `payload`, in bytes. Must be `>= payload_bytes`.
    pub payload_capacity: u64,
    /// Destination array for the resource-use table. Non-NULL when
    /// `use_capacity > 0`; a batch with `use_count == 0` is legal (a pure
    /// state-setting operation touches no allocation).
    pub uses: *mut HeliosSealedResourceUseV1,
    /// Element capacity at `uses`. Must be `>= use_count`.
    pub use_capacity: u32,
    /// Reserved, zero.
    pub reserved0: u32,
    /// Destination array for the typed operand table.
    pub operands: *mut HeliosSealedOperandV1,
    /// Element capacity at `operands`. Must be `>= operand_count`.
    pub operand_capacity: u32,
    /// Reserved, zero.
    pub reserved1: u32,
}

// ── Refusal counters ────────────────────────────────────────────────────────

/// Monotonic counters for every refusal class this ABI makes impossible.
///
/// CLAUDE.md's standing rule — "every skipped/refused path gets a named registry
/// counter or atomic; loud failure over fake success" — applied to the rules
/// sections 10.4, 13.2 and 10.9 state as prose. They exist so that "the private
/// dispatcher rejects any `vkQueueSubmit` … without a live outer-operation scope"
/// is an *observable* property of a running system rather than an assertion about
/// the source.
///
/// Every counter is monotonic for the life of the instance and never resets. A
/// nonzero value is a translator or bridge **bug**, not a tolerated condition:
/// each increment already refused the operation, and the corresponding Vulkan
/// call already failed. The bridge reads them at teardown (and any gate script
/// reads them mid-run) and must fail the run rather than average them away.
///
/// ⚠ **Every counter here is ICD-owned and ICD-written.** The read path,
/// [`HeliosTranslatorDispatchV1::query_refusal_counters`], is an ICD slot, and
/// there is no slot by which a consumer could increment one. A refusal performed
/// *by the consumer* therefore cannot appear here — see
/// [`Self::loader_provenance_rejected`], the only such class, for how its other
/// half is kept loud.
///
/// 112 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeliosTranslatorRefusalCountersV1 {
    /// `== HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES`, set by the caller.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`, set by the caller.
    pub abi_version: u32,
    /// `vkQueueSubmit` with no live outer scope (section 10.4, line 1388).
    pub queue_submit_without_scope: u64,
    /// `vkQueueSubmit2` with no live outer scope (line 1388).
    pub queue_submit2_without_scope: u64,
    /// `vkQueueBindSparse` with no live outer scope (line 1389). Section 17.5
    /// additionally requires vkd3d's sparse binding to become sealed outer work
    /// or a C34 WDDM paging operation, never a lower-queue bind.
    pub queue_bind_sparse_without_scope: u64,
    /// `vkQueuePresentKHR` on a record-only instance. Unconditional, not
    /// scope-gated: section 17.3 (3987-3989) makes it unavailable to translator
    /// instances, and section 2 item 8 gives them no Win32 WSI extension to reach
    /// it with in the first place.
    pub queue_present_refused: u64,
    /// `vkQueueWaitIdle` with no live outer scope (line 1389, "queue-idle
    /// operation"). With a scope it is a C60 join, not a lower-queue idle.
    pub queue_wait_idle_without_scope: u64,
    /// `vkDeviceWaitIdle` with no live outer scope. The doc's "queue-idle
    /// operation" is read to include the device-wide form: it is a queue idle
    /// over every queue, and the fail-closed reading refuses it on the same
    /// terms rather than leaving the wider operation less gated than the
    /// narrower one.
    pub device_wait_idle_without_scope: u64,
    /// A proc-address request this instance refused because the **ICD itself**
    /// determined the resolution would not be its own — the section 13.2 rule
    /// ("a pointer whose owning module is the Vulkan loader or WSI layer is
    /// rejected") applied on the producing side.
    ///
    /// ⚠ **The consumer-side half of that rule is not counted here, and cannot
    /// be.** The comparison section 13.2 describes — take
    /// [`HeliosTranslatorDispatchV1::icd_module_base`], resolve the owning
    /// module of each returned procedure, reject a mismatch — is performed by
    /// DXVK/vkd3d/`umd/bridge`, and this record is written only by the ICD:
    /// there is no slot through which a consumer could increment a field of it.
    /// Reading zero here is therefore **not** evidence that no provenance
    /// rejection occurred, and a gate must not treat it as such.
    ///
    /// What keeps the consumer's half loud is that it is *fatal*, not counted:
    /// a provenance mismatch fails translator **device creation**
    /// ([`HeliosTranslatorStatus::LoaderProvenance`]), which is louder than any
    /// counter and is the section 10.9 disposition for a handshake failure. The
    /// consumer records it in its own diagnostics; it never writes it here.
    pub loader_provenance_rejected: u64,
    /// A control request whose opcode was in the wrong C60 class, or a pure-control
    /// request naming anything but its own session's HVM1 reply/feedback range
    /// (section 10.9, row 2864). The session is poisoned as well as counted.
    pub control_opcode_class_violation: u64,
    /// An outer-allocation-backed deferred operation that tried to reach the host
    /// without an exact outer allocation use (section 10.9, row 2864).
    pub deferred_use_without_outer_batch: u64,
    /// A batch that would have exceeded 4096 uses, 8192 operands, or 15 MiB
    /// (section 10.9's legacy-batch row). The encoder must split at a complete
    /// generated operation *before* sealing, so this counts a producer bug;
    /// truncating is forbidden.
    pub batch_bound_exceeded: u64,
    /// [`HeliosTranslatorDispatchV1::get_instance_proc_addr`] was called with a
    /// `vk_instance` that is not this instance's
    /// [`HeliosTranslatorInstanceV1::vk_instance`]
    /// ([`HeliosTranslatorStatus::ForeignVulkanHandle`]).
    ///
    /// That slot returns a function pointer, not a status, so `NULL` is its only
    /// refusal signal — and a NULL with no counter is exactly the silent
    /// layering bypass section 13.2 exists to make impossible. This field is
    /// what makes it observable.
    pub foreign_vulkan_handle_rejected: u64,
    /// A name a record-only instance may never vend was requested through
    /// [`HeliosTranslatorDispatchV1::get_instance_proc_addr`]: `vkCreateInstance`
    /// (section 10.4: "No second `VkInstance` may be created in that host
    /// context", and section 10.9's HTS1 row makes a second instance in one host
    /// context a device-creation failure), or any Win32 surface/swapchain entry
    /// point (section 13.2: a translator instance "advertise\[s\] no Win32
    /// surface/swapchain extension").
    ///
    /// Also returned as `NULL`, and counted for the same reason as its
    /// neighbour above.
    pub withheld_proc_addr_refused: u64,
    /// A [`HeliosTranslatorHostCallbacksV1::sync_progress_join`] was requested
    /// while one is already in progress on this thread
    /// ([`HeliosTranslatorStatus::ReentrantJoin`]).
    ///
    /// The ICD is the detector — it is the only party that initiates joins — so
    /// this is an ICD-side counter with no consumer half. The join is the ABI's
    /// one reentrant edge, and this counter is the only bound on it, which is
    /// precisely why it must be observable rather than assumed.
    pub reentrant_join_refused: u64,
}

// ── Down-half slot types ────────────────────────────────────────────────────

/// A Vulkan `PFN_vkVoidFunction`, declared without depending on `vulkan.h`.
pub type PfnHeliosTranslatorVoidFunction = Option<extern "C" fn()>;

/// `get_instance_proc_addr` — ICD-implemented. See the slot documentation.
pub type PfnHeliosTranslatorGetInstanceProcAddr = Option<
    extern "C" fn(
        vk_instance: *mut c_void,
        name: *const c_char,
    ) -> PfnHeliosTranslatorVoidFunction,
>;

/// `enumerate_endpoints` — ICD-implemented.
///
/// `endpoint_bytes` must be exactly [`HELIOS_TRANSLATOR_ENDPOINT_BYTES`], for
/// the same reason `build_queue_attach` takes `out_hqa1_bytes`: this is a
/// cross-module bounded copy, and the *stride* must be agreed at run time and
/// not merely at each side's compile time. Mesa is a separate submodule with an
/// independent build; if
/// [`crate::translation_session::HeliosTranslationEndpointV1`] ever grows, a
/// Mesa binary compiled against the newer header would otherwise write 24-byte
/// elements into a consumer's 16-byte-stride array — an unchecked heap overflow
/// in the one call whose whole job is a bounded copy. The package-generation
/// handshake is defence in depth, not a bound.
pub type PfnHeliosTranslatorEnumerateEndpoints = Option<
    extern "C" fn(
        instance: HeliosTranslatorHandle,
        endpoint_count: *mut u32,
        endpoints: *mut HeliosTranslationEndpointV1,
        endpoint_bytes: u32,
    ) -> HeliosTranslatorStatusCode,
>;

/// `build_queue_attach` — ICD-implemented.
pub type PfnHeliosTranslatorBuildQueueAttach = Option<
    extern "C" fn(
        instance: HeliosTranslatorHandle,
        request: *const HeliosQueueAttachRequestV1,
        out_hqa1: *mut HeliosQueueAttachV1,
        out_hqa1_bytes: u32,
    ) -> HeliosTranslatorStatusCode,
>;

/// `attach_outer_context` — ICD-implemented.
pub type PfnHeliosTranslatorAttachOuterContext = Option<
    extern "C" fn(
        instance: HeliosTranslatorHandle,
        attach: *const HeliosOuterContextAttachV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// `detach_outer_context` — ICD-implemented.
pub type PfnHeliosTranslatorDetachOuterContext = Option<
    extern "C" fn(
        instance: HeliosTranslatorHandle,
        context_generation: u64,
    ) -> HeliosTranslatorStatusCode,
>;

/// `open_outer_scope` — ICD-implemented.
pub type PfnHeliosTranslatorOpenOuterScope = Option<
    extern "C" fn(
        instance: HeliosTranslatorHandle,
        begin: *const HeliosOuterScopeBeginV1,
        out_scope: *mut HeliosTranslatorScope,
    ) -> HeliosTranslatorStatusCode,
>;

/// `seal_outer_scope` — ICD-implemented.
pub type PfnHeliosTranslatorSealOuterScope = Option<
    extern "C" fn(
        scope: HeliosTranslatorScope,
        out_sealed: *mut HeliosSealedBatchV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// `copy_sealed_batch` — ICD-implemented.
pub type PfnHeliosTranslatorCopySealedBatch = Option<
    extern "C" fn(
        scope: HeliosTranslatorScope,
        destination: *const HeliosSealedBatchCopyV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// `close_outer_scope` — ICD-implemented.
pub type PfnHeliosTranslatorCloseOuterScope = Option<
    extern "C" fn(
        scope: HeliosTranslatorScope,
        close: *const HeliosOuterScopeCloseV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// `query_refusal_counters` — ICD-implemented.
pub type PfnHeliosTranslatorQueryRefusalCounters = Option<
    extern "C" fn(
        instance: HeliosTranslatorHandle,
        out_counters: *mut HeliosTranslatorRefusalCountersV1,
    ) -> HeliosTranslatorStatusCode,
>;

/// `destroy_instance` — ICD-implemented.
pub type PfnHeliosTranslatorDestroyInstance =
    Option<extern "C" fn(instance: HeliosTranslatorHandle) -> HeliosTranslatorStatusCode>;

/// The exported entry point's own type.
pub type PfnHeliosIcdCreateTranslatorV1 = Option<
    extern "C" fn(
        create_info: *const HeliosTranslatorCreateInfoV1,
        out_instance: *mut HeliosTranslatorInstanceV1,
    ) -> HeliosTranslatorStatusCode,
>;

// ── The down half: the ICD's dispatch table ─────────────────────────────────

/// The private direct-dispatch table: everything a translator and its D3D UMD
/// bridge may ask the Helios ICD to do.
///
/// **Eleven slots, and no twelfth by accident**: `struct_bytes` is asserted equal
/// to 112 at compile time on both sides, so adding a slot without bumping
/// [`HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`] and both size constants breaks the
/// build.
///
/// # Slot contract summary (section 13.3 governs the last two columns)
///
/// | Slot | Implemented by | Called by | May block | Lock rule |
/// |---|---|---|---|---|
/// | [`Self::get_instance_proc_addr`] | ICD | translator (DXVK/vkd3d) and bridge | no | none held; concurrent |
/// | [`Self::enumerate_endpoints`] | ICD | bridge, before create-context | no | brief session lock, released before return |
/// | [`Self::build_queue_attach`] | ICD | bridge, before create-context | no | brief session lock, no up-call while held |
/// | [`Self::attach_outer_context`] | ICD | bridge, after create-context | no | brief session lock, no up-call while held |
/// | [`Self::detach_outer_context`] | ICD | bridge, on queue/device/error teardown | **yes** | none held in either direction; may up-call |
/// | [`Self::open_outer_scope`] | ICD | bridge, inside its DDI entry | no | none held; refuses rather than waits |
/// | [`Self::seal_outer_scope`] | ICD | bridge | no | queue-local mutex taken and released inside |
/// | [`Self::copy_sealed_batch`] | ICD | bridge | no | none: a sealed batch is immutable |
/// | [`Self::close_outer_scope`] | ICD | bridge, after the outer submit | no | brief context lock, released before return |
/// | [`Self::query_refusal_counters`] | ICD | bridge, gates, teardown | no | none: relaxed atomics |
/// | [`Self::destroy_instance`] | ICD | bridge, on device teardown | **yes** | none held in either direction; **no** up-call |
///
/// The two blocking slots block for the same reason and under the same rule:
/// section 10.4 allows destruction to "CPU-wait for already submitted WDDM work
/// when the API requires it" but forbids it from creating a cleanup submission
/// on a lower queue, and section 13.3 requires that wait to hold no translator,
/// endpoint, session-list, or UMD runtime lock. Everything else in this table is
/// non-blocking by contract, which is what keeps a D3D DDI entry point free of a
/// GPU-completion wait.
///
/// They differ in exactly one way, and it matters: `detach_outer_context` may
/// up-call [`HeliosTranslatorHostCallbacksV1::sync_progress_join`] and
/// `destroy_instance` may not. Detach is the last call that holds a context's
/// cookie; destroy runs only after every context is detached, when no cookie
/// exists to join on.
///
/// # Ordering
///
/// ```text
///   helios_icd_create_translator_v1                        (once per vn_instance)
///     -> validate()                                        (mandatory, total)
///     -> enumerate_endpoints                               (bridge selects one)
///     -> build_queue_attach                                (per outer context)
///        [ runtime pfnCreateContextCb / pfnCreateContextVirtualCb ]
///     -> attach_outer_context                              (on success only)
///        ... per outer operation:
///          -> open_outer_scope
///             [ translator records; queue entry points legal only here ]
///          -> seal_outer_scope        -> HeliosSealedBatchV1
///          -> copy_sealed_batch       -> payload + uses + operands
///             [ UMD encodes HOB1, submits via pfnRenderCb/pfnSubmitCommandCb,
///               signals HQC1 ]
///          -> close_outer_scope       (COMMITTED with the HQC1 value, or ABANDONED)
///     -> detach_outer_context                              (per outer context)
///     -> query_refusal_counters                            (must be all zero)
///     -> destroy_instance
/// ```
///
/// 112 bytes.
//
// No `PartialEq`, for the reason given on `HeliosTranslatorHostCallbacksV1`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HeliosTranslatorDispatchV1 {
    /// `== HELIOS_TRANSLATOR_DISPATCH_BYTES`.
    pub struct_bytes: u32,
    /// `== HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION`.
    pub abi_version: u32,
    /// `== crate::HELIOS_PACKAGE_GENERATION`.
    pub package_generation: u64,

    /// The ICD module's base address, for the section 13.2 provenance check.
    ///
    /// Section 13.2 (3357-3358): "every instance/device/queue proc-address
    /// request is resolved from that table; a pointer whose owning module is the
    /// Vulkan loader or WSI layer is rejected." The consumer performs that
    /// rejection, and this is the datum that lets it: resolve the owning module of
    /// each returned procedure (`GetModuleHandleExW` with
    /// `GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | ..._UNCHANGED_REFCOUNT`) and
    /// compare it with this value. A mismatch increments
    /// [`HeliosTranslatorRefusalCountersV1::loader_provenance_rejected`] and fails
    /// translator device creation.
    ///
    /// ⛔ **Compare-only.** It must never be passed to `GetProcAddress`,
    /// `LoadLibrary`, `FreeLibrary`, or any other module operation. It discloses
    /// nothing: the consumer already holds this module, because that is where it
    /// resolved the entry point.
    pub icd_module_base: *const c_void,

    /// **Direction:** translator/bridge → ICD. **May block: NO. Locks:** none.
    ///
    /// The private replacement for the Vulkan loader's
    /// `vkGetInstanceProcAddr`. Every instance, device, and queue procedure the
    /// translator uses is resolved through this slot and no other (section
    /// 13.2: "every instance/device/queue proc-address request is resolved from
    /// that table"); `vkGetDeviceProcAddr` itself is obtained through it.
    ///
    /// `vk_instance` must be [`HeliosTranslatorInstanceV1::vk_instance`] — the
    /// handle this same translator instance's `INIT` created and the entry point
    /// returned. There is no other way to obtain one, which is what makes the
    /// requirement satisfiable: the ICD compares the presented pointer against
    /// its own instance's rather than searching a namespace, and a handle from
    /// another translator instance, from a loader-created device, or from the
    /// WSI layer is [`HeliosTranslatorStatus::ForeignVulkanHandle`].
    ///
    /// ⚠ **This is the one slot that cannot return a status**, because its
    /// return type is the resolved procedure. Every refusal here is a `NULL`
    /// return *plus* a counter, and there are exactly two classes:
    ///
    ///   - a foreign or NULL-when-required `vk_instance` →
    ///     [`HeliosTranslatorRefusalCountersV1::foreign_vulkan_handle_rejected`];
    ///   - a **withheld name** →
    ///     [`HeliosTranslatorRefusalCountersV1::withheld_proc_addr_refused`].
    ///     `vkCreateInstance` is withheld unconditionally, including for the
    ///     `vk_instance == NULL` global-command form: section 10.4 permits one
    ///     `VkInstance` per host context and section 10.9's HTS1 row fails
    ///     device creation for a second, so the global escape hatch that would
    ///     let a translator mint its own is closed here rather than left to
    ///     three consumers' discretion. Every Win32 surface/swapchain entry
    ///     point is withheld on the same terms.
    ///
    /// With `vk_instance == NULL` this slot resolves only the global commands
    /// the ICD chooses to expose, and `vkCreateInstance` is not among them.
    ///
    /// ⛔ The returned procedures are the ICD's own. This slot never returns a
    /// loader trampoline or a layer's next-chain pointer, and it never returns a
    /// Win32 WSI or swapchain entry point: a record-only instance advertises no
    /// Win32 surface/swapchain extension at all (section 13.2), so
    /// `vkQueuePresentKHR` is refused rather than dispatched
    /// ([`HeliosTranslatorRefusalCountersV1::queue_present_refused`]).
    pub get_instance_proc_addr: PfnHeliosTranslatorGetInstanceProcAddr,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** a brief session
    /// lock, released before return; never held across a KMT, runtime, host, or
    /// GPU operation (section 13.3).
    ///
    /// Session/endpoint discovery, and the *only* discovery this ABI has. The
    /// bridge calls it **before** the runtime's create-context callback, because
    /// section 10.4 (1215-1217) requires the endpoint to be selected before HQA1
    /// exists, and section 10.6 steps 1-2 order it the same way for D3D12.
    ///
    /// Two-call form: with `endpoints == NULL`, `*endpoint_count` is set to the
    /// session's endpoint count; otherwise `*endpoint_count` is the caller's
    /// element capacity on entry and the number written on exit, and a capacity
    /// below the count is [`HeliosTranslatorStatus::EndpointCapacity`] with
    /// nothing written. `endpoint_bytes` is the caller's element **stride** and
    /// must be exactly [`HELIOS_TRANSLATOR_ENDPOINT_BYTES`]; any other value is
    /// [`HeliosTranslatorStatus::StructBytes`] and nothing is written, on the
    /// same "exact size, never partial" terms as `build_queue_attach`'s
    /// `out_hqa1_bytes`. It is required on both call forms, so a
    /// count-only probe from a skewed build is refused before the copy call.
    /// `endpoints` is an array of
    /// [`HeliosTranslationEndpointV1`], the record `protocol/src/translation_session.rs`
    /// already owns — session-local ordinals, engine class, and two
    /// diagnostic-only queue ordinals, and explicitly **not** a host
    /// `INFO_RING_IDX`.
    ///
    /// ⛔ This slot returns descriptors, never a capability, never a session
    /// generation to be presented later, and never a host object id. Selection is
    /// the bridge's; resolution stays the ICD's.
    pub enumerate_endpoints: PfnHeliosTranslatorEnumerateEndpoints,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** a brief session
    /// lock; no up-call may be made while it is held.
    ///
    /// Seals the complete 72-byte HQA1 create-context private data for one outer
    /// context, from the endpoint and context generation the bridge chose. The
    /// bridge then passes those exact bytes to `pfnCreateContextCb` (D3D11) or
    /// `pfnCreateContextVirtualCb` (D3D12) as the whole of `pPrivateDriverData`.
    ///
    /// `out_hqa1_bytes` must be exactly [`HELIOS_TRANSLATOR_HQA1_BYTES`]; any
    /// other value is [`HeliosTranslatorStatus::StructBytes`] and nothing is
    /// written. The packet itself is validated on arrival by the KMD with
    /// [`crate::translation_session::HeliosQueueAttachV1::validate`] — this slot
    /// is the producer, that is the consumer, and both are in this crate so they
    /// cannot disagree.
    ///
    /// ⭐ **This is the one place a session capability crosses the interface, and
    /// it crosses sealed inside HQA1.** Section 17.3 (3993-3995) requires the
    /// capability to reach the UMD "only through the private direct table", and
    /// section 10.4 requires the UMD to deliver it in HQA1; giving the bridge the
    /// finished packet satisfies both without ever handing it a bare nonce it
    /// could key a table with, log, or copy into a Render/Submit private-data blob
    /// (which section 17.5 line 4244 forbids outright). Invariant 16's reading is
    /// preserved exactly: the capability is an admission nonce that is compared
    /// once at context creation and never looked anything up.
    pub build_queue_attach: PfnHeliosTranslatorBuildQueueAttach,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** a brief session
    /// lock; no up-call while held.
    ///
    /// Called **only after** the runtime create-context callback returned success
    /// — which, per section 10.4 (1237-1241), means the KMD validated the HQA1,
    /// accepted the context generation, and took a strong direct
    /// session/endpoint reference. Until this call the ICD has a sealed packet and
    /// no context; after it the ICD may record for that context and may up-call
    /// through its cookie.
    ///
    /// A context generation the ICD never sealed a packet for is
    /// [`HeliosTranslatorStatus::UnknownContext`]; a second attach of the same
    /// generation is [`HeliosTranslatorStatus::ContextAlreadyAttached`], which is
    /// invariant 13's "exactly once" made a refusal.
    ///
    /// If the runtime callback **failed**, the bridge calls
    /// [`Self::detach_outer_context`] with the same generation to retire the
    /// sealed packet. It must not silently drop it: the generation must never be
    /// reused, and the ICD is the only party tracking that.
    pub attach_outer_context: PfnHeliosTranslatorAttachOuterContext,

    /// **Direction:** bridge → ICD. **May block: YES** (it may need to join
    /// already-submitted work before the ICD can release its per-context state).
    /// **Locks:** none held by either side; it may up-call
    /// [`HeliosTranslatorHostCallbacksV1::sync_progress_join`].
    ///
    /// Retires one outer context: on ordinary D3D queue/context destruction, and
    /// on the failure path where the runtime create-context callback did not
    /// succeed (in which case there is nothing to join and it cannot block).
    /// Section 17.5 (4243): "Session, endpoint, HQA1, and HQC1 teardown is added
    /// to every queue/device/error path."
    ///
    /// It refuses with [`HeliosTranslatorStatus::ScopeStillLive`] if a scope is
    /// open on that context. It never creates a cleanup submission on a lower
    /// queue (section 10.4: destruction "cannot create a cleanup submission on a
    /// lower queue").
    ///
    /// ⭐ **This is where all joining happens, and it is the last call that may
    /// touch this context's cookie.** On return the ICD holds no
    /// [`HeliosOuterContextAttachV1::host_context_cookie`] for this generation,
    /// makes no further up-call with it, and treats the generation as retired —
    /// which is what makes [`Self::destroy_instance`] a non-up-calling slot and
    /// keeps the bridge free to release its queue object immediately after this
    /// returns.
    pub detach_outer_context: PfnHeliosTranslatorDetachOuterContext,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** none held on
    /// return; it never waits for another scope.
    ///
    /// Opens the outer-operation scope: the window in which the translator's
    /// recording is legal. The bridge calls it inside the D3D DDI entry point
    /// that owns the operation — the D3D11 flush, or the D3D12 ECL
    /// (`ExecuteCommandLists`) association — **before** it invokes the translator,
    /// and closes it after the corresponding outer submission.
    ///
    /// This slot is the mechanism behind section 10.4 (1387-1390). While a scope
    /// is live on the calling thread, `vkQueueSubmit`, `vkQueueSubmit2`,
    /// `vkQueueBindSparse` and queue-idle record into this batch; with no scope
    /// they are refused with [`HeliosTranslatorStatus::NoOuterScope`] and counted.
    /// The same rule closes section 10.4's list of translator-internal work
    /// (upload, clear, initialization, query, breadcrumb, sparse, fence-worker,
    /// swapchain, drain, teardown): it is either inside somebody's scope — hence
    /// "recorded into the owning D3D11 flush/D3D12 ECL batch" — or it is refused.
    /// "No background worker may submit GPU work after the outer DDI has
    /// returned" is exactly the statement that no worker holds a scope.
    ///
    /// **The scope is thread-affine.** It is usable only from the thread that
    /// opened it ([`HeliosTranslatorStatus::ScopeForeignThread`]), and a second
    /// concurrent scope on one outer context is
    /// [`HeliosTranslatorStatus::ScopeAlreadyOpen`] rather than a wait. Both are
    /// the conservative reading: D3D serialises its own per-context calls, and
    /// blocking here would invent a lock that section 13.3 does not allow and that
    /// would be held across translator code.
    ///
    /// ⚠ **The bridge also calls this slot from inside step 3 of a
    /// [`HeliosTranslatorHostCallbacksV1::sync_progress_join`]**, to replace the
    /// scope that join's seal-and-submit just ended. That reopen is an ordinary
    /// open on the same context — it is legal precisely because the join closed
    /// the previous scope first, so it is never `ScopeAlreadyOpen`.
    pub open_outer_scope: PfnHeliosTranslatorOpenOuterScope,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** the bounded
    /// queue-local mutex of section 10.4 (1416-1418) may be taken to serialise the
    /// final append/seal, and is released before return — "it is released before
    /// runtime callbacks and never spans GPU completion".
    ///
    /// Performs points 1-3 of the section 10.4 contract and reports the result:
    /// validate every resource against the outer device's wrappers, serialise
    /// every translated command, barrier, translator-internal dependency and
    /// referenced-resource access for the one logical queue operation, then seal
    /// an **immutable** batch stamped with the fields
    /// [`HeliosSealedBatchV1`] carries.
    ///
    /// After a successful seal the scope is `SEALED`: further recording is
    /// [`HeliosTranslatorStatus::ScopeAlreadySealed`], and the only legal
    /// continuations are [`Self::copy_sealed_batch`] and
    /// [`Self::close_outer_scope`].
    ///
    /// It splits nothing. Section 10.4 (1297-1304) requires the record-only
    /// encoder to split "only at a complete generated command/API operation
    /// boundary **before** HOB1 sealing", so a batch that would exceed 4096 uses,
    /// 8192 operands or 15 MiB is [`HeliosTranslatorStatus::BatchBoundExceeded`]
    /// here, not a truncation and not a fragment.
    pub seal_outer_scope: PfnHeliosTranslatorSealOuterScope,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** none — the sealed
    /// batch is immutable, so the copy needs no exclusion.
    ///
    /// Point 4 of the contract: "return the sealed bytes/resource table
    /// synchronously to the outer bridge". Writes the Venus payload, the complete
    /// resource-use table, and the typed operand table into the bridge's buffers.
    /// Synchronous and total: either everything named by the seal is written, or
    /// nothing is and the call refuses.
    ///
    /// It may be called more than once on a sealed scope with different
    /// destinations (a D3D12 bridge that must move an extent, for instance);
    /// the bytes are identical every time, which is what "immutable" means.
    pub copy_sealed_batch: PfnHeliosTranslatorCopySealedBatch,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** a brief per-context
    /// lock to retire the batch and realise or discard its deferred operations,
    /// released before return.
    ///
    /// Ends the scope and tells the ICD what happened to the batch. On
    /// `COMMITTED` the bridge has encoded HOB1, submitted it through the runtime
    /// callback, and signalled the HQC1 value it reports here; the ICD may now
    /// treat this batch's deferred outer-allocation-backed operations as real
    /// (section 10.4, lines 1346-1352) and may later name that value in a join.
    /// On `ABANDONED` it may not, and the batch ID is retired.
    ///
    /// The scope handle is invalid on return in both cases.
    pub close_outer_scope: PfnHeliosTranslatorCloseOuterScope,

    /// **Direction:** bridge → ICD. **May block: NO. Locks:** none; relaxed
    /// atomic loads.
    ///
    /// Reads [`HeliosTranslatorRefusalCountersV1`]. Every value must be zero on a
    /// correct run; the bridge reads them at teardown and any acceptance gate
    /// reads them mid-run. This is the observable form of the rules this table
    /// makes impossible — see that type's documentation.
    pub query_refusal_counters: PfnHeliosTranslatorQueryRefusalCounters,

    /// **Direction:** bridge → ICD. **May block: YES** — a bounded CPU wait for
    /// its own already-submitted control work. **May up-call: NO.** **Locks:**
    /// none held by either side.
    ///
    /// ⛔ **It cannot up-call, and saying it may would be an instruction to
    /// retain a dead pointer.** Its own precondition is that the bridge has
    /// detached every outer context first, and
    /// [`Self::detach_outer_context`] is where the ICD drops that context's
    /// cookie — so by the time this slot runs there is no
    /// `host_context_cookie` left to join on. An implementer who took a "may
    /// up-call `sync_progress_join`" clause at face value could only make it
    /// reachable by keeping cookies alive past detach, which is a dangling
    /// pointer into the bridge's freed queue object. All joining happens in
    /// `detach_outer_context`; this slot only waits for what it submitted
    /// itself.
    ///
    /// Destroys the instance: the Mesa `vn_instance`, its HTS1 session, its host
    /// Venus context, its HVC1 control context, its role-1 HVM1 reply pool, and
    /// its raw KMT device. Section 12's HTS1 row spells the order out
    /// (`Active -> Draining -> Dead`: invalidate the capability and wake
    /// device-lost first, cancel snapshots, drain raw/outer contexts, endpoint
    /// jobs, slot owners, C51/HQC1 and host refs, unlock/destroy the pool, then
    /// destroy host context and session).
    ///
    /// Refuses with [`HeliosTranslatorStatus::ScopeStillLive`] if any scope is
    /// open, and with [`HeliosTranslatorStatus::ContextStillAttached`] if any
    /// outer context is still attached — the second is a refusal rather than
    /// prose precisely so that "the bridge must detach every outer context
    /// first" cannot fail open. It may CPU-wait for already-submitted WDDM work;
    /// it may not create a cleanup submission on a lower queue (section 10.4).
    ///
    /// After it returns, `handle`, the table pointer, and every scope, endpoint
    /// and cookie derived from them are dead. The instance's capability is
    /// invalidated before any waiter is woken, and is never reused within this
    /// package generation (invariant 16).
    pub destroy_instance: PfnHeliosTranslatorDestroyInstance,
}

// ── Total validation (the C mirror has the same three checks, inline) ───────

impl HeliosTranslatorHostCallbacksV1 {
    /// Total handshake check on the up half, performed by the ICD inside the
    /// entry point before it creates anything.
    ///
    /// Exact size, exact ABI version, exact package generation, and **every slot
    /// non-NULL**. There is no optional callback in this generation: a
    /// record-only instance whose GPU-dependent synchronous calls cannot join the
    /// outer milestone would have to approximate one, and section 10.4 (line
    /// 1366) forbids exactly that ("There is no blocking host Venus
    /// `vkQueueWaitIdle`, shared ring-head sample, or control-fence
    /// approximation").
    pub fn validate(
        &self,
        expected_package_generation: u64,
    ) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if expected_package_generation == 0
            || self.package_generation != expected_package_generation
        {
            return Err(HeliosTranslatorStatus::PackageGeneration);
        }
        if self.sync_progress_join.is_none() || self.sync_progress_query.is_none() {
            return Err(HeliosTranslatorStatus::HostCallbacks);
        }
        Ok(())
    }
}

impl HeliosTranslatorDispatchV1 {
    /// Total handshake check on the down half, performed by the bridge before it
    /// calls a single slot.
    ///
    /// ⛔ Not "at least this size", not "the slots I recognise", not "zero means
    /// any". A mismatch fails translator device creation before any context,
    /// allocation, endpoint, or resource is exposed (section 10.9, row 2860).
    pub fn validate(
        &self,
        expected_package_generation: u64,
    ) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_DISPATCH_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if expected_package_generation == 0
            || self.package_generation != expected_package_generation
        {
            return Err(HeliosTranslatorStatus::PackageGeneration);
        }
        if self.icd_module_base.is_null() {
            // Without it the section 13.2 provenance check is unperformable, and
            // an unperformable proof is not a proof.
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        // Every slot, named individually: a loop over a transmuted pointer array
        // would be both `unsafe` and silently tolerant of a reordering.
        if self.get_instance_proc_addr.is_none()
            || self.enumerate_endpoints.is_none()
            || self.build_queue_attach.is_none()
            || self.attach_outer_context.is_none()
            || self.detach_outer_context.is_none()
            || self.open_outer_scope.is_none()
            || self.seal_outer_scope.is_none()
            || self.copy_sealed_batch.is_none()
            || self.close_outer_scope.is_none()
            || self.query_refusal_counters.is_none()
            || self.destroy_instance.is_none()
        {
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        Ok(())
    }
}

impl HeliosTranslatorCreateInfoV1 {
    /// Total check on the create info, performed by the ICD on entry.
    ///
    /// It deliberately does **not** dereference [`Self::host_callbacks`]: a
    /// pointer check and a table check are separate refusals, and the ICD
    /// validates the table itself with
    /// [`HeliosTranslatorHostCallbacksV1::validate`] in its own `unsafe` block —
    /// this crate has none and keeps none.
    pub fn validate(
        &self,
        expected_package_generation: u64,
    ) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_CREATE_INFO_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if expected_package_generation == 0
            || self.package_generation != expected_package_generation
        {
            return Err(HeliosTranslatorStatus::PackageGeneration);
        }
        if self.submission_mode != HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY {
            return Err(HeliosTranslatorStatus::SubmissionMode);
        }
        if self.requested_endpoint_capacity == 0
            || self.requested_endpoint_capacity
                > crate::translation_session::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION
        {
            return Err(HeliosTranslatorStatus::EndpointCapacity);
        }
        if self.adapter_luid_low == 0 && self.adapter_luid_high == 0 {
            return Err(HeliosTranslatorStatus::AdapterLuid);
        }
        if self.host_callbacks.is_null() {
            return Err(HeliosTranslatorStatus::HostCallbacks);
        }
        Ok(())
    }
}

impl HeliosTranslatorInstanceV1 {
    /// What the bridge checks on the instance the entry point filled in, before
    /// it trusts a single field of it.
    ///
    /// The `dispatch` table it names must additionally pass
    /// [`HeliosTranslatorDispatchV1::validate`]; that is a separate call because
    /// dereferencing the pointer is the caller's `unsafe`, not this crate's.
    pub fn validate(&self) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_INSTANCE_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.handle.is_null() || self.dispatch.is_null() || self.vk_instance.is_null() {
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        if self.session_generation == 0 {
            // Section 10.4: INIT "returns a nonzero session generation".
            return Err(HeliosTranslatorStatus::SessionInit);
        }
        if self.endpoint_capacity == 0
            || self.endpoint_capacity
                > crate::translation_session::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION
        {
            return Err(HeliosTranslatorStatus::EndpointCapacity);
        }
        if self.submission_mode != HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY {
            return Err(HeliosTranslatorStatus::SubmissionMode);
        }
        Ok(())
    }
}

impl HeliosQueueAttachRequestV1 {
    /// Total check on the request, run by the **ICD** before it seals a single
    /// byte of HQA1.
    ///
    /// ⚠ **Every check here is field-local, and that is the point of the
    /// division.** Three of this record's fields are only fully checkable
    /// against session state the ICD holds and this crate does not:
    ///
    ///   - [`Self::context_generation`] must be "nonzero, monotonically
    ///     increasing and never reused within this HTS1 session"
    ///     (section 10.4, 1233). Only *nonzero* is a property of the record;
    ///     monotonic-and-unused is a property of the session's issued set, so
    ///     the ICD re-checks it there and returns the same
    ///     [`HeliosTranslatorStatus::ContextGeneration`].
    ///   - [`Self::endpoint_id`] must name an endpoint of *this* session. Only
    ///     nonzero is checkable here; the ICD's resolution returns
    ///     [`HeliosTranslatorStatus::UnknownEndpoint`].
    ///   - [`Self::engine_class`] must be "the exact graphics/compute/copy class
    ///     admitted for the outer context" — i.e. the selected endpoint's class.
    ///     Only *defined* is checkable here; the comparison against the resolved
    ///     endpoint is the ICD's, and returns the same
    ///     [`HeliosTranslatorStatus::EngineClass`].
    ///
    /// So passing this is necessary and not sufficient, by construction. It is
    /// still worth having as the one place both languages agree on the shape,
    /// and it is why each of those three refusals has exactly one code shared by
    /// the field check and the state check: a consumer that saw two codes for
    /// one cause would have to learn which layer refused it.
    pub fn validate(&self) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.reserved != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        if self.context_generation == 0 {
            return Err(HeliosTranslatorStatus::ContextGeneration);
        }
        if self.endpoint_id == 0 {
            return Err(HeliosTranslatorStatus::UnknownEndpoint);
        }
        if self.engine_class != crate::translation_session::HELIOS_ENGINE_CLASS_GRAPHICS
            && self.engine_class != crate::translation_session::HELIOS_ENGINE_CLASS_COMPUTE
            && self.engine_class != crate::translation_session::HELIOS_ENGINE_CLASS_COPY
        {
            return Err(HeliosTranslatorStatus::EngineClass);
        }
        if self.context_flags != crate::translation_session::HELIOS_HQA1_FLAG_D3D11_PHYSICAL
            && self.context_flags != crate::translation_session::HELIOS_HQA1_FLAG_D3D12_VIRTUAL
        {
            return Err(HeliosTranslatorStatus::ContextFlags);
        }
        Ok(())
    }
}

impl HeliosSealedBatchV1 {
    /// Total check the bridge runs on a seal descriptor before it sizes a buffer
    /// or encodes a byte.
    ///
    /// `expected_*` come from the bridge's own state — the instance's session
    /// generation, the context it opened the scope for — never from the
    /// descriptor. That is what makes this a check rather than a lookup.
    pub fn validate(
        &self,
        expected_package_generation: u64,
        expected_session_generation: u64,
        expected_context_generation: u64,
        expected_endpoint_id: u32,
        expected_context_flags: u32,
    ) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_SEALED_BATCH_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if expected_package_generation == 0
            || self.package_generation != expected_package_generation
        {
            return Err(HeliosTranslatorStatus::PackageGeneration);
        }
        if expected_session_generation == 0
            || self.session_generation != expected_session_generation
        {
            return Err(HeliosTranslatorStatus::SessionGeneration);
        }
        if expected_context_generation == 0
            || self.context_generation != expected_context_generation
        {
            return Err(HeliosTranslatorStatus::ContextGeneration);
        }
        if self.batch_id == 0 {
            return Err(HeliosTranslatorStatus::BatchId);
        }
        if expected_endpoint_id == 0 || self.endpoint_id != expected_endpoint_id {
            return Err(HeliosTranslatorStatus::UnknownEndpoint);
        }
        if self.context_flags != expected_context_flags
            || (self.context_flags != crate::translation_session::HELIOS_HQA1_FLAG_D3D11_PHYSICAL
                && self.context_flags
                    != crate::translation_session::HELIOS_HQA1_FLAG_D3D12_VIRTUAL)
        {
            return Err(HeliosTranslatorStatus::ContextFlags);
        }
        if self.payload_bytes == 0 {
            return Err(HeliosTranslatorStatus::BatchBoundExceeded);
        }
        if self.use_count > crate::wddm::HELIOS_HOB1_MAX_USE_RECORDS
            || self.operand_count > crate::wddm::HELIOS_HOB1_MAX_OPERAND_RECORDS
        {
            return Err(HeliosTranslatorStatus::BatchBoundExceeded);
        }
        // ⛔ The 15 MiB cap is on the WHOLE HOB1 RECORD, not on the payload.
        //
        // `HELIOS_HOB1_MAX_BYTES` is HOB1's `total_bytes` limit — "header
        // through payload … at most 15 MiB" — and `crate::wddm` enforces it as
        // such. Bounding only `payload_bytes` against it lets a seal pass here
        // and become unencodable later: 15,728,600 payload bytes with 4096 uses
        // and 8192 operands is 16,023,624 assembled bytes, refused by
        // `HeliosOuterBatchV1::validate` *after* the seal — at which point
        // section 10.4 has already made splitting impossible (the encoder
        // "splits only at a complete generated command/API operation boundary
        // before HOB1 sealing") and section 10.9 forbids truncating instead.
        //
        // So the check is the minimum assembled size, computed exactly as the
        // encoder will lay it out: header + both tables + payload. It is a
        // necessary condition rather than the final one — the encoder may add
        // alignment padding between sections and must re-check — which is the
        // fail-closed direction.
        let assembled = (crate::wddm::HELIOS_HOB1_HEADER_BYTES as u64)
            .checked_add(self.use_count as u64 * crate::wddm::HELIOS_HOB1_USE_RECORD_BYTES as u64)
            .and_then(|n| {
                n.checked_add(
                    self.operand_count as u64
                        * crate::wddm::HELIOS_HOB1_OPERAND_RECORD_BYTES as u64,
                )
            })
            .and_then(|n| n.checked_add(self.payload_bytes));
        match assembled {
            Some(total) if total <= crate::wddm::HELIOS_HOB1_MAX_BYTES => Ok(()),
            _ => Err(HeliosTranslatorStatus::BatchBoundExceeded),
        }
    }
}

impl HeliosSealedBatchCopyV1 {
    /// Total check on the copy destination, run by the **ICD** before
    /// [`HeliosTranslatorDispatchV1::copy_sealed_batch`] writes a single byte.
    ///
    /// ⛔ **This is the one validator in the file whose absence is a memory
    /// error rather than a wrong answer.** Every other record here is data the
    /// receiver interprets; this one is three caller-owned buffers and their
    /// capacities, and the ICD is about to *write* into them. "Must be `>=
    /// payload_bytes`" as a doc comment is a rule the ICD has to remember; as a
    /// function it is a rule the ICD cannot forget, and the crate's whole
    /// premise is that the check lives once rather than three times in three
    /// repositories.
    ///
    /// `batch` is the [`HeliosSealedBatchV1`] the seal returned, which is where
    /// every required size comes from — never from the descriptor. Validate the
    /// batch first ([`HeliosSealedBatchV1::validate`]); this function trusts its
    /// counts, and they are already bounded there.
    ///
    /// A short capacity is [`HeliosTranslatorStatus::BufferTooSmall`] and
    /// **nothing is written**: section 10.4 point 4 makes the copy "synchronous
    /// and total", so there is no partial copy and no resize handshake.
    pub fn validate(&self, batch: &HeliosSealedBatchV1) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.reserved0 != 0 || self.reserved1 != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        // The payload is never empty — a sealed batch has nonzero
        // `payload_bytes` — so its destination is unconditionally required.
        if self.payload.is_null() {
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        if self.payload_capacity < batch.payload_bytes {
            return Err(HeliosTranslatorStatus::BufferTooSmall);
        }
        // ⚠ The NULL check is conditioned on the batch's count, not on the
        // caller's capacity. A descriptor with `uses = NULL, use_capacity = 8`
        // would otherwise pass a capacity test and be dereferenced; and a batch
        // with `use_count == 0` is legal (a pure state-setting operation
        // touches no allocation), so an unconditional NULL check would refuse a
        // correct caller.
        if batch.use_count > 0 && self.uses.is_null() {
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        if self.use_capacity < batch.use_count {
            return Err(HeliosTranslatorStatus::BufferTooSmall);
        }
        if batch.operand_count > 0 && self.operands.is_null() {
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        if self.operand_capacity < batch.operand_count {
            return Err(HeliosTranslatorStatus::BufferTooSmall);
        }
        Ok(())
    }
}

impl HeliosOuterContextAttachV1 {
    /// Total check on the attach record, run by the **ICD** when the bridge
    /// reports that the runtime accepted an HQA1.
    ///
    /// Field-local, on the same division as
    /// [`HeliosQueueAttachRequestV1::validate`]: whether this generation is one
    /// the ICD actually sealed a packet for, and whether it is already attached,
    /// are properties of the session's state, and the ICD answers them with
    /// [`HeliosTranslatorStatus::UnknownContext`] and
    /// [`HeliosTranslatorStatus::ContextAlreadyAttached`] respectively.
    pub fn validate(&self) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.context_generation == 0 {
            return Err(HeliosTranslatorStatus::ContextGeneration);
        }
        if self.endpoint_id == 0 {
            return Err(HeliosTranslatorStatus::UnknownEndpoint);
        }
        if self.context_flags != crate::translation_session::HELIOS_HQA1_FLAG_D3D11_PHYSICAL
            && self.context_flags != crate::translation_session::HELIOS_HQA1_FLAG_D3D12_VIRTUAL
        {
            return Err(HeliosTranslatorStatus::ContextFlags);
        }
        // The cookie is the only argument every later up-call carries. A NULL
        // one produces an up-call the bridge cannot resolve, and there is no
        // later point at which it becomes checkable.
        if self.host_context_cookie.is_null() {
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        Ok(())
    }
}

impl HeliosOuterScopeBeginV1 {
    /// Total check on the scope-begin record, run by the **ICD** in
    /// [`HeliosTranslatorDispatchV1::open_outer_scope`].
    ///
    /// Field-local for the same reason as its neighbours: that the named context
    /// is attached to *this* instance is session state, and the ICD answers it
    /// with [`HeliosTranslatorStatus::UnknownContext`].
    pub fn validate(&self) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.reserved != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        if self.context_generation == 0 {
            return Err(HeliosTranslatorStatus::ContextGeneration);
        }
        if self.endpoint_id == 0 {
            return Err(HeliosTranslatorStatus::UnknownEndpoint);
        }
        Ok(())
    }
}

impl HeliosSyncProgressJoinV1 {
    /// Total check on a join request, run by the **UMD bridge** the moment the
    /// up-call arrives and before it dereferences anything.
    ///
    /// `live_context_generation` is the generation of the context that
    /// `host_context_cookie` belongs to, read from the bridge's own state. This
    /// is the check [`Self::context_generation`] exists for, and it is the one
    /// place in the ABI where skipping a validator is a **use-after-free**
    /// rather than a wrong answer: without it a join in flight on thread B
    /// against a queue thread A has already destroyed hands the bridge a freed
    /// cookie and nothing to compare it against.
    ///
    /// A mismatch is [`HeliosTranslatorStatus::UnknownContext`] — a refusal,
    /// never a lookup that finds the right context. Generations are never
    /// reused within a session (section 10.4), so a mismatch is always a stale
    /// caller and never an ambiguity.
    ///
    /// [`Self::required_progress_value`] is deliberately unconstrained here:
    /// zero is the legal "everything pending" form, and whether a nonzero value
    /// is one this bridge ever issued is bridge state
    /// ([`HeliosTranslatorStatus::HostCallbackFailed`]).
    pub fn validate(&self, live_context_generation: u64) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_SYNC_PROGRESS_JOIN_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if live_context_generation == 0 || self.context_generation != live_context_generation {
            return Err(HeliosTranslatorStatus::UnknownContext);
        }
        Ok(())
    }
}

impl HeliosSealedResourceUseV1 {
    /// Total check on one sealed use, run by the bridge as it converts the entry
    /// into an HOB1 use record.
    ///
    /// `context_flags` is the owning context's kind, which is what decides
    /// whether a nonzero [`Self::byte_offset`] is encodable at all.
    pub fn validate(&self, context_flags: u32) -> Result<(), HeliosTranslatorStatus> {
        if self.reserved0 != 0 || self.reserved1 != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        if self.outer_allocation_token == 0 {
            // Zero is the "no allocation" reading of an uninitialised buffer, and
            // an allocation-naming record that names none is never legal.
            return Err(HeliosTranslatorStatus::NullArgument);
        }
        // A zero-length use and an overflowing range are one cause — an illegal
        // byte range — and neither is a destination buffer that came up short
        // (`BufferTooSmall`) or a producer that failed to split
        // (`BatchBoundExceeded`).
        if self.byte_length == 0 || self.byte_offset.checked_add(self.byte_length).is_none() {
            return Err(HeliosTranslatorStatus::SealedUseRange);
        }
        let access = self.access_flags;
        if access == 0
            || access & !crate::wddm::HELIOS_HOB1_ACCESS_MASK != 0
            || (access & crate::wddm::HELIOS_HOB1_ACCESS_PRIMARY_WRITE != 0
                && access & crate::wddm::HELIOS_HOB1_ACCESS_WRITE == 0)
        {
            return Err(HeliosTranslatorStatus::AccessFlags);
        }
        if context_flags == crate::translation_session::HELIOS_HQA1_FLAG_D3D11_PHYSICAL
            && self.byte_offset != 0
        {
            return Err(HeliosTranslatorStatus::D3D11SubrangeUse);
        }
        Ok(())
    }
}

impl HeliosSealedOperandV1 {
    /// Total check on one sealed operand against the payload it must lie inside.
    ///
    /// Every refusal here names its own cause. In particular a misaligned or
    /// out-of-payload offset is [`HeliosTranslatorStatus::OperandEncoding`] —
    /// the operand's *encoding* is illegal — and an out-of-range `use_index` is
    /// [`HeliosTranslatorStatus::OperandUseIndex`]. Neither is
    /// [`HeliosTranslatorStatus::BatchBoundExceeded`], whose documented meaning
    /// and matching counter are "4096 uses / 8192 operands / 15 MiB, a producer
    /// that failed to split": a gate that saw that code for a four-byte
    /// alignment error would go hunting the wrong defect.
    pub fn validate(
        &self,
        payload_bytes: u64,
        use_count: u32,
    ) -> Result<(), HeliosTranslatorStatus> {
        if self.reserved != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        if self.operand_kind != crate::wddm::HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE {
            return Err(HeliosTranslatorStatus::OperandEncoding);
        }
        if self.encoded_width != 4 && self.encoded_width != 8 {
            return Err(HeliosTranslatorStatus::OperandEncoding);
        }
        if self.use_index >= use_count {
            return Err(HeliosTranslatorStatus::OperandUseIndex);
        }
        if self.payload_relative_offset % crate::wddm::HELIOS_HOB1_OPERAND_ALIGN != 0 {
            return Err(HeliosTranslatorStatus::OperandEncoding);
        }
        let end = self.payload_relative_offset as u64 + self.encoded_width as u64;
        if end > payload_bytes {
            return Err(HeliosTranslatorStatus::OperandEncoding);
        }
        Ok(())
    }

    /// The section 10.4 placeholder rule, checked against the bytes rather than
    /// asserted: an operand "identifies only a generated resource operand whose
    /// **payload bytes are zero**".
    ///
    /// ⛔ This is the leakage check the whole interface exists to keep. A
    /// nonzero placeholder at an offset the operand table blesses is precisely
    /// how a raw host resource id would reach the host, and section 17.3's
    /// audit ("no actual virtio `resource_id` may reach user-mode storage or
    /// protocol state") is only satisfiable if somebody looks. `crate::wddm`
    /// carries the consumer-side backstop; this is the producer-side one, run
    /// on the payload the ICD just copied out, for the same reason
    /// [`HeliosTranslatorStatus::D3D11SubrangeUse`] is producer-side.
    ///
    /// `payload` is the Venus payload the seal produced, exactly
    /// `HeliosSealedBatchV1::payload_bytes` long. Call [`Self::validate`] first;
    /// this one re-derives the bounds rather than trusting them, so it is total
    /// on its own.
    pub fn validate_payload_zero(&self, payload: &[u8]) -> Result<(), HeliosTranslatorStatus> {
        let start = self.payload_relative_offset as usize;
        let width = self.encoded_width as usize;
        let end = match start.checked_add(width) {
            Some(end) => end,
            None => return Err(HeliosTranslatorStatus::OperandEncoding),
        };
        let bytes = match payload.get(start..end) {
            Some(bytes) => bytes,
            // Out of the payload it names: an encoding error, and never a
            // silent skip — a check that cannot run is not a check.
            None => return Err(HeliosTranslatorStatus::OperandEncoding),
        };
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != 0 {
                return Err(HeliosTranslatorStatus::PayloadPlaceholderNonZero);
            }
            i += 1;
        }
        Ok(())
    }
}

impl HeliosOuterScopeCloseV1 {
    /// Total check on a close record, run by the ICD.
    pub fn validate(&self) -> Result<HeliosTranslatorScopeDisposition, HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.reserved != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        let disposition = HeliosTranslatorScopeDisposition::from_wire(self.disposition)?;
        match disposition {
            // A committed batch must name the HQC1 value that completes it;
            // without one no later join can wait for exactly this batch.
            // Both arms are one cause — a progress value that contradicts the
            // disposition beside it — and it is a malformed *down-call
            // argument*, not a failed up-call (`HostCallbackFailed`) and not a
            // reserved field (`ReservedNonZero`).
            HeliosTranslatorScopeDisposition::Committed if self.progress_value == 0 => {
                Err(HeliosTranslatorStatus::ProgressValue)
            }
            // An abandoned batch has no completion to name, and reporting one
            // would arm a wait for a value that will never be signalled.
            HeliosTranslatorScopeDisposition::Abandoned if self.progress_value != 0 => {
                Err(HeliosTranslatorStatus::ProgressValue)
            }
            other => Ok(other),
        }
    }
}

impl HeliosSyncProgressResultV1 {
    /// The field checks both up-calls share: shape, reserved, flag mask, and
    /// the one invariant the record itself states (`completed <=
    /// last_submitted`).
    ///
    /// Split out because the two up-calls' *interpretations* are opposites, and
    /// merging them is how a nonblocking query gets turned into a failure — see
    /// [`Self::validate_query`].
    fn validate_common(&self) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        if self.reserved != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        // An undefined flag bit is a reserved bit: same cause, same code.
        if self.flags & !HELIOS_TRANSLATOR_PROGRESS_FLAGS_MASK != 0 {
            return Err(HeliosTranslatorStatus::ReservedNonZero);
        }
        if self.flags & HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST != 0 {
            return Err(HeliosTranslatorStatus::DeviceLost);
        }
        // Not a failed callback — a self-contradictory one. Completion cannot
        // outrun submission on a context that has one writer.
        if self.completed_progress_value > self.last_submitted_progress_value {
            return Err(HeliosTranslatorStatus::ProgressValue);
        }
        Ok(())
    }

    /// Total check on the result of a **blocking**
    /// [`HeliosTranslatorHostCallbacksV1::sync_progress_join`].
    ///
    /// `required` is the value the ICD asked for (zero for the "everything
    /// pending" form). A join that returns without having reached the value it
    /// was asked for is a failed join, never a partial success — section 10.9's
    /// HQC1 row forbids treating a later or lesser value as satisfaction.
    ///
    /// ⛔ **Not for a query result.** [`Self::validate_query`] is that one, and
    /// the difference is not stylistic: the tail of this function treats
    /// `completed < last_submitted` as a failure, which is exactly the answer a
    /// nonblocking query returns when work is outstanding.
    pub fn validate_join(&self, required: u64) -> Result<(), HeliosTranslatorStatus> {
        self.validate_common()?;
        if required != 0 && self.completed_progress_value < required {
            return Err(HeliosTranslatorStatus::HostCallbackFailed);
        }
        if required == 0 && self.completed_progress_value < self.last_submitted_progress_value {
            // The "everything pending" form must have joined everything.
            return Err(HeliosTranslatorStatus::HostCallbackFailed);
        }
        Ok(())
    }

    /// Total check on the result of a **nonblocking**
    /// [`HeliosTranslatorHostCallbacksV1::sync_progress_query`].
    ///
    /// It is the shared field checks and nothing else, because there is no
    /// value the query was obliged to reach. Section 10.4: "A nonblocking status
    /// query returns the locally known not-ready state without a control round
    /// trip when the milestone has not completed" — so `completed <
    /// last_submitted` is the **normal, correct** answer here and must never be
    /// a refusal. `vkGetFenceStatus`, `vkGetEventStatus` and a non-`WAIT`
    /// `vkGetQueryPoolResults` all live on this path; making them fail whenever
    /// work is outstanding would invert the required behaviour, and mapping
    /// that failure to a callback failure would remove the device.
    ///
    /// The caller decides readiness by comparing
    /// [`Self::completed_progress_value`] against the value it cares about;
    /// this function does not, because "not ready" is a result and not an
    /// error.
    pub fn validate_query(&self) -> Result<(), HeliosTranslatorStatus> {
        self.validate_common()
    }
}

impl HeliosTranslatorRefusalCountersV1 {
    /// The shape check the ICD owes this record **before it writes a byte of
    /// it**, and the consumer owes it after the call returns.
    ///
    /// [`PfnHeliosTranslatorQueryRefusalCounters`] takes `out_counters` as a
    /// `*mut` into the *consumer's* storage, and the two parties are separately
    /// compiled binaries — the ICD is Mesa, the consumer is DXVK/vkd3d or a
    /// `umd/bridge`. Every other record in this ABI re-checks `struct_bytes`
    /// and `abi_version` on entry even though
    /// [`HeliosTranslatorCreateInfoV1::validate`] already refused a mismatched
    /// `abi_version` at create time; the redundancy is the point, because the
    /// case it catches is a record whose size changed without the version
    /// being bumped, which is precisely the error create-time negotiation
    /// cannot see.
    ///
    /// This record was the one exception. Its own field documentation has said
    /// `struct_bytes` is "`== HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES`, set by
    /// the caller" since it was written, and nothing enforced it — a stated
    /// precondition with no code behind it. Its sibling out-parameter
    /// [`HeliosSyncProgressResultV1`], which has the identical `*mut`-filled-by-
    /// the-other-binary shape, has always been validated; the asymmetry was an
    /// oversight rather than a decision, so it is closed here rather than
    /// documented as intentional.
    ///
    /// There is no cross-field invariant to check: all thirteen fields are
    /// independent monotonic counters, and a counter is never "too large".
    pub fn validate(&self) -> Result<(), HeliosTranslatorStatus> {
        if self.struct_bytes != HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES {
            return Err(HeliosTranslatorStatus::StructBytes);
        }
        if self.abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION {
            return Err(HeliosTranslatorStatus::AbiVersion);
        }
        Ok(())
    }
}

// ── Compile-time layout assertions ──────────────────────────────────────────
//
// The same discipline the wire modules use, for the same reason: a wrong size
// must break the BUILD. Here it does double duty — because every prohibited
// payload in the module header is prohibited by *absence*, a size assertion is
// what stops a field being added to smuggle one in. The C mirror carries the
// twin of every assertion below.

const _: () = {
    // The interface is x86-64 only, which is what lets one set of byte counts be
    // normative for Rust and C at once. The C header enforces the same with an
    // `#error`; here the pointer width is the check.
    assert!(core::mem::size_of::<*const c_void>() == 8);
    assert!(core::mem::size_of::<PfnHeliosTranslatorDestroyInstance>() == 8);

    // ── Create info / instance ──
    assert!(core::mem::size_of::<HeliosTranslatorCreateInfoV1>() == 40);
    assert!(
        core::mem::size_of::<HeliosTranslatorCreateInfoV1>()
            == HELIOS_TRANSLATOR_CREATE_INFO_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosTranslatorCreateInfoV1>() == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, submission_mode) == 16);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, requested_endpoint_capacity) == 20);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, adapter_luid_low) == 24);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, adapter_luid_high) == 28);
    assert!(core::mem::offset_of!(HeliosTranslatorCreateInfoV1, host_callbacks) == 32);

    assert!(core::mem::size_of::<HeliosTranslatorInstanceV1>() == 48);
    assert!(
        core::mem::size_of::<HeliosTranslatorInstanceV1>()
            == HELIOS_TRANSLATOR_INSTANCE_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosTranslatorInstanceV1>() == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, handle) == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, dispatch) == 16);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, vk_instance) == 24);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, session_generation) == 32);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, endpoint_capacity) == 40);
    assert!(core::mem::offset_of!(HeliosTranslatorInstanceV1, submission_mode) == 44);

    // ── The two tables ──
    assert!(core::mem::size_of::<HeliosTranslatorHostCallbacksV1>() == 32);
    assert!(
        core::mem::size_of::<HeliosTranslatorHostCallbacksV1>()
            == HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosTranslatorHostCallbacksV1>() == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorHostCallbacksV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosTranslatorHostCallbacksV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslatorHostCallbacksV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorHostCallbacksV1, sync_progress_join) == 16);
    assert!(core::mem::offset_of!(HeliosTranslatorHostCallbacksV1, sync_progress_query) == 24);

    assert!(core::mem::size_of::<HeliosTranslatorDispatchV1>() == 112);
    assert!(
        core::mem::size_of::<HeliosTranslatorDispatchV1>()
            == HELIOS_TRANSLATOR_DISPATCH_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosTranslatorDispatchV1>() == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, icd_module_base) == 16);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, get_instance_proc_addr) == 24);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, enumerate_endpoints) == 32);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, build_queue_attach) == 40);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, attach_outer_context) == 48);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, detach_outer_context) == 56);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, open_outer_scope) == 64);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, seal_outer_scope) == 72);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, copy_sealed_batch) == 80);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, close_outer_scope) == 88);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, query_refusal_counters) == 96);
    assert!(core::mem::offset_of!(HeliosTranslatorDispatchV1, destroy_instance) == 104);

    // ── Parameter records ──
    assert!(core::mem::size_of::<HeliosQueueAttachRequestV1>() == 32);
    assert!(
        core::mem::size_of::<HeliosQueueAttachRequestV1>()
            == HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES as usize
    );
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, context_generation) == 8);
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, endpoint_id) == 16);
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, engine_class) == 20);
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, context_flags) == 24);
    assert!(core::mem::offset_of!(HeliosQueueAttachRequestV1, reserved) == 28);

    assert!(core::mem::size_of::<HeliosOuterContextAttachV1>() == 32);
    assert!(
        core::mem::size_of::<HeliosOuterContextAttachV1>()
            == HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosOuterContextAttachV1>() == 8);
    assert!(core::mem::offset_of!(HeliosOuterContextAttachV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosOuterContextAttachV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosOuterContextAttachV1, context_generation) == 8);
    assert!(core::mem::offset_of!(HeliosOuterContextAttachV1, endpoint_id) == 16);
    assert!(core::mem::offset_of!(HeliosOuterContextAttachV1, context_flags) == 20);
    assert!(core::mem::offset_of!(HeliosOuterContextAttachV1, host_context_cookie) == 24);

    assert!(core::mem::size_of::<HeliosOuterScopeBeginV1>() == 24);
    assert!(
        core::mem::size_of::<HeliosOuterScopeBeginV1>()
            == HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES as usize
    );
    assert!(core::mem::offset_of!(HeliosOuterScopeBeginV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosOuterScopeBeginV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosOuterScopeBeginV1, context_generation) == 8);
    assert!(core::mem::offset_of!(HeliosOuterScopeBeginV1, endpoint_id) == 16);
    assert!(core::mem::offset_of!(HeliosOuterScopeBeginV1, reserved) == 20);

    assert!(core::mem::size_of::<HeliosOuterScopeCloseV1>() == 24);
    assert!(
        core::mem::size_of::<HeliosOuterScopeCloseV1>()
            == HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES as usize
    );
    assert!(core::mem::offset_of!(HeliosOuterScopeCloseV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosOuterScopeCloseV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosOuterScopeCloseV1, disposition) == 8);
    assert!(core::mem::offset_of!(HeliosOuterScopeCloseV1, reserved) == 12);
    assert!(core::mem::offset_of!(HeliosOuterScopeCloseV1, progress_value) == 16);

    // ── The sealed batch ──
    assert!(core::mem::size_of::<HeliosSealedBatchV1>() == 72);
    assert!(
        core::mem::size_of::<HeliosSealedBatchV1>() == HELIOS_TRANSLATOR_SEALED_BATCH_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosSealedBatchV1>() == 8);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, session_generation) == 16);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, context_generation) == 24);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, batch_id) == 32);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, payload_bytes) == 40);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, payload_crc64) == 48);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, endpoint_id) == 56);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, context_flags) == 60);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, use_count) == 64);
    assert!(core::mem::offset_of!(HeliosSealedBatchV1, operand_count) == 68);

    // The sealed use record is the 40-byte HOB1 use record with the identity pair
    // replaced; if HOB1's ever changes size, this parallel must be re-derived
    // rather than silently diverging.
    assert!(core::mem::size_of::<HeliosSealedResourceUseV1>() == 40);
    assert!(
        core::mem::size_of::<HeliosSealedResourceUseV1>()
            == HELIOS_TRANSLATOR_SEALED_USE_BYTES as usize
    );
    assert!(
        core::mem::size_of::<HeliosSealedResourceUseV1>()
            == crate::wddm::HELIOS_HOB1_USE_RECORD_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosSealedResourceUseV1>() == 8);
    // ⚠ Every offset below is the offset of the HOB1 field it stands in for, so
    // the pairs are pinned rather than described: `byte_length` and
    // `access_flags`/`operand_count`/`first_operand`/`reserved1` are the same
    // field at the same offset, and the three that differ — token/`address_or_index`
    // at 0, `byte_offset`/`expected_allocation_generation` at 16, and
    // `reserved0`/`identity_kind` at 28 — are exactly the WDDM identity the ICD
    // may not supply. An accidental field reorder here silently breaks the
    // field-by-field conversion in the UMD encoder, so the parallel is asserted.
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, outer_allocation_token) == 0);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, byte_length) == 8);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, byte_offset) == 16);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, access_flags) == 24);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, reserved0) == 28);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, operand_count) == 30);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, first_operand) == 32);
    assert!(core::mem::offset_of!(HeliosSealedResourceUseV1, reserved1) == 36);
    // The HOB1 twins, so a change on either side breaks the build here.
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, address_or_index) == 0);
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, byte_length) == 8);
    assert!(
        core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, expected_allocation_generation)
            == 16
    );
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, access_flags) == 24);
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, identity_kind) == 28);
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, operand_count) == 30);
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, first_operand) == 32);
    assert!(core::mem::offset_of!(crate::wddm::HeliosOuterBatchUseV1, reserved) == 36);

    assert!(core::mem::size_of::<HeliosSealedOperandV1>() == 16);
    assert!(
        core::mem::size_of::<HeliosSealedOperandV1>()
            == HELIOS_TRANSLATOR_SEALED_OPERAND_BYTES as usize
    );
    assert!(
        core::mem::size_of::<HeliosSealedOperandV1>()
            == crate::wddm::HELIOS_HOB1_OPERAND_RECORD_BYTES as usize
    );
    assert!(core::mem::offset_of!(HeliosSealedOperandV1, payload_relative_offset) == 0);
    assert!(core::mem::offset_of!(HeliosSealedOperandV1, use_index) == 4);
    assert!(core::mem::offset_of!(HeliosSealedOperandV1, operand_kind) == 8);
    assert!(core::mem::offset_of!(HeliosSealedOperandV1, encoded_width) == 10);
    assert!(core::mem::offset_of!(HeliosSealedOperandV1, reserved) == 12);

    assert!(core::mem::size_of::<HeliosSealedBatchCopyV1>() == 56);
    assert!(
        core::mem::size_of::<HeliosSealedBatchCopyV1>()
            == HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosSealedBatchCopyV1>() == 8);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, payload) == 8);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, payload_capacity) == 16);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, uses) == 24);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, use_capacity) == 32);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, reserved0) == 36);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, operands) == 40);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, operand_capacity) == 48);
    assert!(core::mem::offset_of!(HeliosSealedBatchCopyV1, reserved1) == 52);

    // ── The up half's records ──
    assert!(core::mem::size_of::<HeliosSyncProgressJoinV1>() == 24);
    assert!(
        core::mem::size_of::<HeliosSyncProgressJoinV1>()
            == HELIOS_TRANSLATOR_SYNC_PROGRESS_JOIN_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosSyncProgressJoinV1>() == 8);
    assert!(core::mem::offset_of!(HeliosSyncProgressJoinV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosSyncProgressJoinV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosSyncProgressJoinV1, required_progress_value) == 8);
    assert!(core::mem::offset_of!(HeliosSyncProgressJoinV1, context_generation) == 16);

    assert!(core::mem::size_of::<HeliosSyncProgressResultV1>() == 32);
    assert!(
        core::mem::size_of::<HeliosSyncProgressResultV1>()
            == HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosSyncProgressResultV1>() == 8);
    assert!(core::mem::offset_of!(HeliosSyncProgressResultV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosSyncProgressResultV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosSyncProgressResultV1, completed_progress_value) == 8);
    assert!(core::mem::offset_of!(HeliosSyncProgressResultV1, last_submitted_progress_value) == 16);
    assert!(core::mem::offset_of!(HeliosSyncProgressResultV1, flags) == 24);
    assert!(core::mem::offset_of!(HeliosSyncProgressResultV1, reserved) == 28);

    assert!(core::mem::size_of::<HeliosTranslatorRefusalCountersV1>() == 112);
    assert!(
        core::mem::size_of::<HeliosTranslatorRefusalCountersV1>()
            == HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosTranslatorRefusalCountersV1>() == 8);
    // Every counter's offset, not a first-and-last pair: a consumer that reads
    // these through the C mirror indexes by offset, and an inserted field would
    // otherwise renumber every counter after it without breaking anything.
    assert!(core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, struct_bytes) == 0);
    assert!(core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, queue_submit_without_scope) == 8);
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, queue_submit2_without_scope) == 16
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, queue_bind_sparse_without_scope)
            == 24
    );
    assert!(core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, queue_present_refused) == 32);
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, queue_wait_idle_without_scope) == 40
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, device_wait_idle_without_scope)
            == 48
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, loader_provenance_rejected) == 56
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, control_opcode_class_violation)
            == 64
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, deferred_use_without_outer_batch)
            == 72
    );
    assert!(core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, batch_bound_exceeded) == 80);
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, foreign_vulkan_handle_rejected)
            == 88
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, withheld_proc_addr_refused) == 96
    );
    assert!(
        core::mem::offset_of!(HeliosTranslatorRefusalCountersV1, reentrant_join_refused) == 104
    );

    // ── Cross-module pins ──
    // The two constants this module restates as `u32` for the C mirror must equal
    // the records they stand in for; if HQA1 or the endpoint descriptor ever
    // changes size, this breaks here rather than at a 72-byte memcpy.
    assert!(HELIOS_TRANSLATOR_HQA1_BYTES as usize == core::mem::size_of::<HeliosQueueAttachV1>());
    assert!(
        HELIOS_TRANSLATOR_ENDPOINT_BYTES as usize
            == core::mem::size_of::<HeliosTranslationEndpointV1>()
    );

    // ── Vocabulary ──
    assert!(HELIOS_TRANSLATOR_SUBMISSION_MODE_INVALID == 0);
    assert!(HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY == 1);
    assert!(HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION == 1);
    assert!(HELIOS_TRANSLATOR_SCOPE_DISPOSITION_INVALID == 0);
    assert!(HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED == 1);
    assert!(HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED == 2);
    assert!(HELIOS_TRANSLATOR_PROGRESS_FLAGS_MASK == 1);
    assert!(HeliosTranslatorStatus::Ok.wire() == 0);
    assert!(HeliosTranslatorStatus::MAX == 45);
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translation_session::{
        HELIOS_HQA1_FLAG_D3D11_PHYSICAL, HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
    };
    use crate::HELIOS_PACKAGE_GENERATION;

    extern "C" fn stub_join(
        _cookie: *mut c_void,
        _req: *const HeliosSyncProgressJoinV1,
        _out: *mut HeliosSyncProgressResultV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_query(
        _cookie: *mut c_void,
        _context_generation: u64,
        _out: *mut HeliosSyncProgressResultV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_gipa(
        _vk_instance: *mut c_void,
        _name: *const c_char,
    ) -> PfnHeliosTranslatorVoidFunction {
        None
    }

    extern "C" fn stub_enumerate(
        _i: HeliosTranslatorHandle,
        _c: *mut u32,
        _e: *mut HeliosTranslationEndpointV1,
        _b: u32,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_build(
        _i: HeliosTranslatorHandle,
        _r: *const HeliosQueueAttachRequestV1,
        _o: *mut HeliosQueueAttachV1,
        _b: u32,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_attach(
        _i: HeliosTranslatorHandle,
        _a: *const HeliosOuterContextAttachV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_detach(_i: HeliosTranslatorHandle, _g: u64) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_open(
        _i: HeliosTranslatorHandle,
        _b: *const HeliosOuterScopeBeginV1,
        _s: *mut HeliosTranslatorScope,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_seal(
        _s: HeliosTranslatorScope,
        _o: *mut HeliosSealedBatchV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_copy(
        _s: HeliosTranslatorScope,
        _d: *const HeliosSealedBatchCopyV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_close(
        _s: HeliosTranslatorScope,
        _c: *const HeliosOuterScopeCloseV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_counters(
        _i: HeliosTranslatorHandle,
        _o: *mut HeliosTranslatorRefusalCountersV1,
    ) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    extern "C" fn stub_destroy(_i: HeliosTranslatorHandle) -> HeliosTranslatorStatusCode {
        HeliosTranslatorStatus::Ok.wire()
    }

    fn good_callbacks() -> HeliosTranslatorHostCallbacksV1 {
        HeliosTranslatorHostCallbacksV1 {
            struct_bytes: HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            package_generation: HELIOS_PACKAGE_GENERATION,
            sync_progress_join: Some(stub_join),
            sync_progress_query: Some(stub_query),
        }
    }

    fn good_dispatch() -> HeliosTranslatorDispatchV1 {
        HeliosTranslatorDispatchV1 {
            struct_bytes: HELIOS_TRANSLATOR_DISPATCH_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            package_generation: HELIOS_PACKAGE_GENERATION,
            // Any non-null value: the validator only requires the provenance
            // anchor to exist. A function's own address is a stand-in for the
            // module base in a test that never dereferences it.
            icd_module_base: stub_destroy as *const c_void,
            get_instance_proc_addr: Some(stub_gipa),
            enumerate_endpoints: Some(stub_enumerate),
            build_queue_attach: Some(stub_build),
            attach_outer_context: Some(stub_attach),
            detach_outer_context: Some(stub_detach),
            open_outer_scope: Some(stub_open),
            seal_outer_scope: Some(stub_seal),
            copy_sealed_batch: Some(stub_copy),
            close_outer_scope: Some(stub_close),
            query_refusal_counters: Some(stub_counters),
            destroy_instance: Some(stub_destroy),
        }
    }

    fn good_create_info(cb: &HeliosTranslatorHostCallbacksV1) -> HeliosTranslatorCreateInfoV1 {
        HeliosTranslatorCreateInfoV1 {
            struct_bytes: HELIOS_TRANSLATOR_CREATE_INFO_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            package_generation: HELIOS_PACKAGE_GENERATION,
            submission_mode: HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY,
            requested_endpoint_capacity: 4,
            adapter_luid_low: 0x1234,
            adapter_luid_high: 0,
            host_callbacks: cb,
        }
    }

    fn good_sealed() -> HeliosSealedBatchV1 {
        HeliosSealedBatchV1 {
            struct_bytes: HELIOS_TRANSLATOR_SEALED_BATCH_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            package_generation: HELIOS_PACKAGE_GENERATION,
            session_generation: 7,
            context_generation: 11,
            batch_id: 1,
            payload_bytes: 4096,
            payload_crc64: 0,
            endpoint_id: 2,
            context_flags: HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
            use_count: 1,
            operand_count: 1,
        }
    }

    // ── The handshake is total and never partially adopts ───────────────────

    #[test]
    fn a_complete_dispatch_table_validates() {
        assert_eq!(good_dispatch().validate(HELIOS_PACKAGE_GENERATION), Ok(()));
    }

    #[test]
    fn a_short_or_long_table_is_refused_rather_than_partially_adopted() {
        let mut t = good_dispatch();
        t.struct_bytes = HELIOS_TRANSLATOR_DISPATCH_BYTES - 8;
        assert_eq!(
            t.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::StructBytes)
        );
        t.struct_bytes = HELIOS_TRANSLATOR_DISPATCH_BYTES + 8;
        assert_eq!(
            t.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::StructBytes)
        );
    }

    #[test]
    fn every_missing_slot_is_a_refusal_and_zero_generation_is_not_a_wildcard() {
        // One at a time: a table that is complete except for slot N must fail.
        let base = good_dispatch();
        let mutators: [fn(&mut HeliosTranslatorDispatchV1); 11] = [
            |t| t.get_instance_proc_addr = None,
            |t| t.enumerate_endpoints = None,
            |t| t.build_queue_attach = None,
            |t| t.attach_outer_context = None,
            |t| t.detach_outer_context = None,
            |t| t.open_outer_scope = None,
            |t| t.seal_outer_scope = None,
            |t| t.copy_sealed_batch = None,
            |t| t.close_outer_scope = None,
            |t| t.query_refusal_counters = None,
            |t| t.destroy_instance = None,
        ];
        for m in mutators {
            let mut t = base;
            m(&mut t);
            assert_eq!(
                t.validate(HELIOS_PACKAGE_GENERATION),
                Err(HeliosTranslatorStatus::NullArgument)
            );
        }

        // Zero is never a wildcard, on either side of the comparison.
        let mut t = base;
        t.package_generation = 0;
        assert_eq!(
            t.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::PackageGeneration)
        );
        assert_eq!(
            base.validate(0),
            Err(HeliosTranslatorStatus::PackageGeneration)
        );
        assert_eq!(
            base.validate(HELIOS_PACKAGE_GENERATION + 1),
            Err(HeliosTranslatorStatus::PackageGeneration)
        );
    }

    #[test]
    fn the_up_half_has_no_optional_callback() {
        let mut cb = good_callbacks();
        assert_eq!(cb.validate(HELIOS_PACKAGE_GENERATION), Ok(()));
        cb.sync_progress_query = None;
        assert_eq!(
            cb.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::HostCallbacks)
        );
        let mut cb = good_callbacks();
        cb.sync_progress_join = None;
        assert_eq!(
            cb.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::HostCallbacks)
        );
    }

    #[test]
    fn only_record_only_creates_an_instance() {
        let cb = good_callbacks();
        let mut ci = good_create_info(&cb);
        assert_eq!(ci.validate(HELIOS_PACKAGE_GENERATION), Ok(()));

        ci.submission_mode = HELIOS_TRANSLATOR_SUBMISSION_MODE_INVALID;
        assert_eq!(
            ci.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::SubmissionMode)
        );
        // No future mode is tolerated by this generation either.
        ci.submission_mode = HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY + 1;
        assert_eq!(
            ci.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::SubmissionMode)
        );

        let mut ci = good_create_info(&cb);
        ci.adapter_luid_low = 0;
        ci.adapter_luid_high = 0;
        assert_eq!(
            ci.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::AdapterLuid)
        );

        let mut ci = good_create_info(&cb);
        ci.host_callbacks = core::ptr::null();
        assert_eq!(
            ci.validate(HELIOS_PACKAGE_GENERATION),
            Err(HeliosTranslatorStatus::HostCallbacks)
        );
    }

    #[test]
    fn a_queue_attach_request_states_the_outer_contexts_own_class() {
        let base = HeliosQueueAttachRequestV1 {
            struct_bytes: HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            context_generation: 11,
            endpoint_id: 2,
            engine_class: crate::translation_session::HELIOS_ENGINE_CLASS_COPY,
            context_flags: HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
            reserved: 0,
        };
        assert_eq!(base.validate(), Ok(()));

        // A zeroed record is refused field by field, never defaulted.
        let mut r = base;
        r.context_generation = 0;
        assert_eq!(
            r.validate(),
            Err(HeliosTranslatorStatus::ContextGeneration)
        );

        let mut r = base;
        r.endpoint_id = 0;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::UnknownEndpoint));

        // Zero is not "whatever the endpoint says": the field exists so that a
        // COPY queue selecting a GRAPHICS endpoint is refusable at all.
        let mut r = base;
        r.engine_class = 0;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::EngineClass));
        let mut r = base;
        r.engine_class = 4;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::EngineClass));

        // Exactly one context-kind flag: neither zero nor both.
        let mut r = base;
        r.context_flags = 0;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::ContextFlags));
        let mut r = base;
        r.context_flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL | HELIOS_HQA1_FLAG_D3D12_VIRTUAL;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::ContextFlags));

        let mut r = base;
        r.reserved = 1;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::ReservedNonZero));

        let mut r = base;
        r.struct_bytes = HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES - 8;
        assert_eq!(r.validate(), Err(HeliosTranslatorStatus::StructBytes));
    }

    // ── The refusal counters ────────────────────────────────────────────────

    #[test]
    fn the_refusal_counter_block_is_shape_checked_like_every_other_record() {
        // `query_refusal_counters` hands this record's storage to a separately
        // compiled binary as a `*mut`. The case that matters is a size change
        // WITHOUT an abi_version bump, which create-time negotiation cannot
        // see — so a short block must be refused even though the version is
        // the one currently negotiated.
        let good = HeliosTranslatorRefusalCountersV1 {
            struct_bytes: HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            ..Default::default()
        };
        assert_eq!(good.validate(), Ok(()));

        let mut short = good;
        short.struct_bytes = HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES - 8;
        assert_eq!(short.validate(), Err(HeliosTranslatorStatus::StructBytes));

        let mut grown = good;
        grown.struct_bytes = HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES + 8;
        assert_eq!(grown.validate(), Err(HeliosTranslatorStatus::StructBytes));

        let mut wrong_abi = good;
        wrong_abi.abi_version = HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION + 1;
        assert_eq!(wrong_abi.validate(), Err(HeliosTranslatorStatus::AbiVersion));

        // All-zero is the state a consumer allocates before filling the header
        // in, and it must NOT pass: a zeroed block is indistinguishable from a
        // record whose header the caller forgot to set.
        assert_eq!(
            HeliosTranslatorRefusalCountersV1::default().validate(),
            Err(HeliosTranslatorStatus::StructBytes)
        );
    }

    // ── The sealed batch ────────────────────────────────────────────────────

    #[test]
    fn a_seal_is_checked_against_the_bridges_own_state_not_its_own_fields() {
        let s = good_sealed();
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                11,
                2,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Ok(())
        );
        // A stale session, context, or endpoint is a named refusal, never a
        // lookup that finds the right one — and a stale *generation* is
        // `SessionGeneration`, not `SessionInit`, which means the session never
        // came up at all.
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                8,
                11,
                2,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Err(HeliosTranslatorStatus::SessionGeneration)
        );
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                12,
                2,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Err(HeliosTranslatorStatus::ContextGeneration)
        );
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                11,
                3,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Err(HeliosTranslatorStatus::UnknownEndpoint)
        );
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                11,
                2,
                HELIOS_HQA1_FLAG_D3D11_PHYSICAL
            ),
            Err(HeliosTranslatorStatus::ContextFlags)
        );
    }

    #[test]
    fn a_seal_may_not_exceed_the_hob1_bounds() {
        let mut s = good_sealed();
        s.payload_bytes = crate::wddm::HELIOS_HOB1_MAX_BYTES + 1;
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                11,
                2,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Err(HeliosTranslatorStatus::BatchBoundExceeded)
        );
        let mut s = good_sealed();
        s.use_count = crate::wddm::HELIOS_HOB1_MAX_USE_RECORDS + 1;
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                11,
                2,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Err(HeliosTranslatorStatus::BatchBoundExceeded)
        );
        let mut s = good_sealed();
        s.operand_count = crate::wddm::HELIOS_HOB1_MAX_OPERAND_RECORDS + 1;
        assert_eq!(
            s.validate(
                HELIOS_PACKAGE_GENERATION,
                7,
                11,
                2,
                HELIOS_HQA1_FLAG_D3D12_VIRTUAL
            ),
            Err(HeliosTranslatorStatus::BatchBoundExceeded)
        );
    }

    #[test]
    fn a_d3d11_context_cannot_carry_a_sub_allocation_offset() {
        let u = HeliosSealedResourceUseV1 {
            outer_allocation_token: 0x51,
            byte_offset: 256,
            byte_length: 4096,
            access_flags: crate::wddm::HELIOS_HOB1_ACCESS_READ,
            operand_count: 0,
            reserved0: 0,
            first_operand: 0,
            reserved1: 0,
        };
        // Legal on the D3D12 GPUVA arm...
        assert_eq!(u.validate(HELIOS_HQA1_FLAG_D3D12_VIRTUAL), Ok(()));
        // ...and unencodable on the D3D11 allocation-list-index arm.
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL),
            Err(HeliosTranslatorStatus::D3D11SubrangeUse)
        );
    }

    #[test]
    fn a_use_record_names_a_real_allocation_with_legal_access() {
        let base = HeliosSealedResourceUseV1 {
            outer_allocation_token: 0x51,
            byte_offset: 0,
            byte_length: 4096,
            access_flags: crate::wddm::HELIOS_HOB1_ACCESS_WRITE,
            operand_count: 0,
            reserved0: 0,
            first_operand: 0,
            reserved1: 0,
        };
        assert_eq!(base.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL), Ok(()));

        let mut u = base;
        u.outer_allocation_token = 0;
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL),
            Err(HeliosTranslatorStatus::NullArgument)
        );

        // A zero-length use is an illegal *range*, not a too-small destination
        // buffer: `BufferTooSmall` names a caller's copy-out buffer.
        let mut u = base;
        u.byte_length = 0;
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL),
            Err(HeliosTranslatorStatus::SealedUseRange)
        );

        // …and so is a range that wraps 64 bits, which is the other half of
        // that code's documented meaning.
        let mut u = base;
        u.byte_offset = u64::MAX - 16;
        u.byte_length = 4096;
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D12_VIRTUAL),
            Err(HeliosTranslatorStatus::SealedUseRange)
        );

        // PRIMARY_WRITE implies WRITE (section 10.4, line 1283).
        let mut u = base;
        u.access_flags = crate::wddm::HELIOS_HOB1_ACCESS_PRIMARY_WRITE;
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL),
            Err(HeliosTranslatorStatus::AccessFlags)
        );
        let mut u = base;
        u.access_flags = crate::wddm::HELIOS_HOB1_ACCESS_PRIMARY_WRITE
            | crate::wddm::HELIOS_HOB1_ACCESS_WRITE;
        assert_eq!(u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL), Ok(()));

        let mut u = base;
        u.access_flags = 0;
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL),
            Err(HeliosTranslatorStatus::AccessFlags)
        );
        let mut u = base;
        u.reserved1 = 1;
        assert_eq!(
            u.validate(HELIOS_HQA1_FLAG_D3D11_PHYSICAL),
            Err(HeliosTranslatorStatus::ReservedNonZero)
        );
    }

    #[test]
    fn an_operand_must_lie_inside_the_payload_it_names() {
        let base = HeliosSealedOperandV1 {
            payload_relative_offset: 16,
            use_index: 0,
            operand_kind: crate::wddm::HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE,
            encoded_width: 8,
            reserved: 0,
        };
        assert_eq!(base.validate(4096, 1), Ok(()));

        // An operand that runs off the end of the payload, and a misaligned one,
        // are both illegal *encodings*. Neither is `BatchBoundExceeded`, which
        // means "the producer failed to split at 4096 uses / 8192 operands /
        // 15 MiB" and would send a gate hunting the wrong defect.
        let mut o = base;
        o.payload_relative_offset = 4092;
        assert_eq!(
            o.validate(4096, 1),
            Err(HeliosTranslatorStatus::OperandEncoding)
        );

        let mut o = base;
        o.payload_relative_offset = 17;
        assert_eq!(
            o.validate(4096, 1),
            Err(HeliosTranslatorStatus::OperandEncoding)
        );

        // An out-of-range use index is an index error and has its own code.
        let mut o = base;
        o.use_index = 1;
        assert_eq!(
            o.validate(4096, 1),
            Err(HeliosTranslatorStatus::OperandUseIndex)
        );

        let mut o = base;
        o.encoded_width = 2;
        assert_eq!(
            o.validate(4096, 1),
            Err(HeliosTranslatorStatus::OperandEncoding)
        );

        let mut o = base;
        o.operand_kind = crate::wddm::HELIOS_HOB1_OPERAND_KIND_INVALID;
        assert_eq!(
            o.validate(4096, 1),
            Err(HeliosTranslatorStatus::OperandEncoding)
        );
    }

    #[test]
    fn a_copy_destination_is_checked_against_the_seal_not_against_itself() {
        let batch = good_sealed();
        let mut uses = [HeliosSealedResourceUseV1::default(); 4];
        let mut operands = [HeliosSealedOperandV1::default(); 4];
        let mut payload = [0u8; 4096];
        let good = HeliosSealedBatchCopyV1 {
            struct_bytes: HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            payload: payload.as_mut_ptr() as *mut c_void,
            payload_capacity: payload.len() as u64,
            uses: uses.as_mut_ptr(),
            use_capacity: uses.len() as u32,
            reserved0: 0,
            operands: operands.as_mut_ptr(),
            operand_capacity: operands.len() as u32,
            reserved1: 0,
        };
        assert_eq!(good.validate(&batch), Ok(()));

        // One byte short is a refusal, not a truncated copy.
        let mut d = good;
        d.payload_capacity = batch.payload_bytes - 1;
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::BufferTooSmall)
        );

        let mut d = good;
        d.use_capacity = batch.use_count - 1;
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::BufferTooSmall)
        );

        let mut d = good;
        d.operand_capacity = batch.operand_count - 1;
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::BufferTooSmall)
        );

        // ⛔ The trap this validator exists for: a NULL destination carrying a
        // capacity large enough to pass every size test. Checking capacity
        // alone would accept it and the ICD would write through NULL.
        let mut d = good;
        d.uses = core::ptr::null_mut();
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::NullArgument)
        );
        let mut d = good;
        d.operands = core::ptr::null_mut();
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::NullArgument)
        );
        let mut d = good;
        d.payload = core::ptr::null_mut();
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::NullArgument)
        );

        // ...and the converse: a batch that touches no allocation is legal, and
        // a NULL table for it must NOT be refused.
        let mut empty = batch;
        empty.use_count = 0;
        empty.operand_count = 0;
        let mut d = good;
        d.uses = core::ptr::null_mut();
        d.use_capacity = 0;
        d.operands = core::ptr::null_mut();
        d.operand_capacity = 0;
        assert_eq!(d.validate(&empty), Ok(()));

        let mut d = good;
        d.reserved1 = 1;
        assert_eq!(
            d.validate(&batch),
            Err(HeliosTranslatorStatus::ReservedNonZero)
        );
    }

    #[test]
    fn an_attach_and_a_scope_begin_are_checked_before_the_session_is_consulted() {
        let mut cookie = 0u64;
        let attach = HeliosOuterContextAttachV1 {
            struct_bytes: HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            context_generation: 11,
            endpoint_id: 2,
            context_flags: HELIOS_HQA1_FLAG_D3D11_PHYSICAL,
            host_context_cookie: (&mut cookie) as *mut u64 as *mut c_void,
        };
        assert_eq!(attach.validate(), Ok(()));

        // The cookie is every later up-call's only argument; a NULL one is an
        // up-call the bridge can never resolve, and nothing later can catch it.
        let mut a = attach;
        a.host_context_cookie = core::ptr::null_mut();
        assert_eq!(a.validate(), Err(HeliosTranslatorStatus::NullArgument));

        let mut a = attach;
        a.context_generation = 0;
        assert_eq!(a.validate(), Err(HeliosTranslatorStatus::ContextGeneration));

        let mut a = attach;
        a.context_flags = 0;
        assert_eq!(a.validate(), Err(HeliosTranslatorStatus::ContextFlags));

        let begin = HeliosOuterScopeBeginV1 {
            struct_bytes: HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            context_generation: 11,
            endpoint_id: 2,
            reserved: 0,
        };
        assert_eq!(begin.validate(), Ok(()));

        let mut b = begin;
        b.reserved = 1;
        assert_eq!(b.validate(), Err(HeliosTranslatorStatus::ReservedNonZero));

        let mut b = begin;
        b.endpoint_id = 0;
        assert_eq!(b.validate(), Err(HeliosTranslatorStatus::UnknownEndpoint));
    }

    #[test]
    fn a_join_request_is_refused_when_its_context_has_moved_on() {
        let req = HeliosSyncProgressJoinV1 {
            struct_bytes: HELIOS_TRANSLATOR_SYNC_PROGRESS_JOIN_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            // Zero is the legal "everything pending on this context" form and
            // must never be refused as a missing value.
            required_progress_value: 0,
            context_generation: 11,
        };
        assert_eq!(req.validate(11), Ok(()));

        // The use-after-free guard: thread A destroyed the queue and the
        // bridge's live generation moved; thread B's in-flight join must be
        // refused rather than resolved against the newer object.
        assert_eq!(req.validate(12), Err(HeliosTranslatorStatus::UnknownContext));
        // A bridge with no live context for that cookie refuses too — zero is
        // never a wildcard that matches.
        assert_eq!(req.validate(0), Err(HeliosTranslatorStatus::UnknownContext));

        let mut r = req;
        r.required_progress_value = 42;
        assert_eq!(r.validate(11), Ok(()));

        let mut r = req;
        r.struct_bytes = 16;
        assert_eq!(r.validate(11), Err(HeliosTranslatorStatus::StructBytes));
    }

    // ── Scope disposition and progress ──────────────────────────────────────

    #[test]
    fn a_zeroed_close_record_is_refused_not_defaulted() {
        let close = HeliosOuterScopeCloseV1 {
            struct_bytes: HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            disposition: HELIOS_TRANSLATOR_SCOPE_DISPOSITION_INVALID,
            reserved: 0,
            progress_value: 0,
        };
        assert_eq!(close.validate(), Err(HeliosTranslatorStatus::Disposition));
    }

    #[test]
    fn committed_must_name_a_progress_value_and_abandoned_must_not() {
        let mut close = HeliosOuterScopeCloseV1 {
            struct_bytes: HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            disposition: HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED,
            reserved: 0,
            progress_value: 0,
        };
        // A value that contradicts the disposition beside it is a malformed
        // argument (`ProgressValue`) — not a failed up-call and not a reserved
        // field, both of which would point a reader at the wrong subsystem.
        assert_eq!(close.validate(), Err(HeliosTranslatorStatus::ProgressValue));
        close.progress_value = 42;
        assert_eq!(
            close.validate(),
            Ok(HeliosTranslatorScopeDisposition::Committed)
        );

        close.disposition = HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED;
        assert_eq!(close.validate(), Err(HeliosTranslatorStatus::ProgressValue));
        close.progress_value = 0;
        assert_eq!(
            close.validate(),
            Ok(HeliosTranslatorScopeDisposition::Abandoned)
        );
    }

    #[test]
    fn a_join_that_did_not_reach_its_value_is_a_failure_not_a_partial_success() {
        let mut r = HeliosSyncProgressResultV1 {
            struct_bytes: HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            completed_progress_value: 10,
            last_submitted_progress_value: 10,
            flags: 0,
            reserved: 0,
        };
        assert_eq!(r.validate_join(10), Ok(()));
        assert_eq!(r.validate_join(0), Ok(()));
        assert_eq!(
            r.validate_join(11),
            Err(HeliosTranslatorStatus::HostCallbackFailed)
        );

        // "Everything pending" must have joined everything.
        r.completed_progress_value = 9;
        assert_eq!(
            r.validate_join(0),
            Err(HeliosTranslatorStatus::HostCallbackFailed)
        );

        // Completion may never exceed what was submitted. That is a
        // self-contradictory record, not a callback that failed to reach a
        // value — `ProgressValue`, and the same code from a query.
        r.completed_progress_value = 11;
        assert_eq!(
            r.validate_join(10),
            Err(HeliosTranslatorStatus::ProgressValue)
        );
        assert_eq!(r.validate_query(), Err(HeliosTranslatorStatus::ProgressValue));

        // Device loss is device loss, whatever the values say.
        r.completed_progress_value = 10;
        r.flags = HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST;
        assert_eq!(r.validate_join(10), Err(HeliosTranslatorStatus::DeviceLost));
    }

    // ── Status decoding ─────────────────────────────────────────────────────

    #[test]
    fn every_status_round_trips_and_an_unknown_one_is_refused() {
        for code in 0..=HeliosTranslatorStatus::MAX {
            let decoded = HeliosTranslatorStatus::from_wire(code)
                .expect("every code up to MAX must decode");
            assert_eq!(decoded.wire(), code);
        }
        assert_eq!(
            HeliosTranslatorStatus::from_wire(HeliosTranslatorStatus::MAX + 1),
            Err(HeliosTranslatorUnknownStatus {
                code: HeliosTranslatorStatus::MAX + 1
            })
        );
        assert_eq!(
            HeliosTranslatorStatus::from_wire(-1),
            Err(HeliosTranslatorUnknownStatus { code: -1 })
        );
        assert!(HeliosTranslatorStatus::Ok.is_ok());
        assert!(!HeliosTranslatorStatus::NoOuterScope.is_ok());
    }

    /// `protocol/include/helios_translator_dispatch.h` hand-copies every constant
    /// below. Pin the exact literals here so a change on the Rust side without the
    /// matching header edit is caught by a failing test that names the header, not
    /// by three repositories linking against two different ABIs. (Offsets and
    /// sizes need no test: both sides assert them at compile time.)
    #[test]
    fn c_mirror_carries_these_exact_constants() {
        // protocol/include/helios_translator_dispatch.h

        // The entry point. Its spelling IS the ABI: a typo here is a
        // GetProcAddress that returns NULL at translator device creation.
        assert_eq!(
            HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME,
            "helios_icd_create_translator_v1"
        );
        assert_eq!(
            HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME_CSTR,
            b"helios_icd_create_translator_v1\0"
        );
        // ...and the two spellings must be the same string.
        assert_eq!(
            &HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME_CSTR
                [..HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME_CSTR.len() - 1],
            HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME.as_bytes()
        );

        assert_eq!(HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION, 1);
        assert_eq!(HELIOS_TRANSLATOR_SUBMISSION_MODE_INVALID, 0);
        assert_eq!(HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY, 1);

        assert_eq!(HELIOS_TRANSLATOR_CREATE_INFO_BYTES, 40);
        assert_eq!(HELIOS_TRANSLATOR_INSTANCE_BYTES, 48);
        assert_eq!(HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES, 32);
        assert_eq!(HELIOS_TRANSLATOR_DISPATCH_BYTES, 112);
        assert_eq!(HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES, 32);
        assert_eq!(HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES, 32);
        assert_eq!(HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES, 24);
        assert_eq!(HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES, 24);
        assert_eq!(HELIOS_TRANSLATOR_SEALED_BATCH_BYTES, 72);
        assert_eq!(HELIOS_TRANSLATOR_SEALED_USE_BYTES, 40);
        assert_eq!(HELIOS_TRANSLATOR_SEALED_OPERAND_BYTES, 16);
        assert_eq!(HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES, 56);
        assert_eq!(HELIOS_TRANSLATOR_SYNC_PROGRESS_JOIN_BYTES, 24);
        assert_eq!(HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES, 32);
        assert_eq!(HELIOS_TRANSLATOR_REFUSAL_COUNTERS_BYTES, 112);

        // The two constants restated for the C mirror because their records live
        // in a sibling header.
        assert_eq!(HELIOS_TRANSLATOR_HQA1_BYTES, 72);
        assert_eq!(HELIOS_TRANSLATOR_ENDPOINT_BYTES, 16);

        assert_eq!(HELIOS_TRANSLATOR_SCOPE_DISPOSITION_INVALID, 0);
        assert_eq!(HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED, 1);
        assert_eq!(HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED, 2);

        assert_eq!(HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST, 1);
        assert_eq!(HELIOS_TRANSLATOR_PROGRESS_FLAGS_MASK, 1);

        // Every status code, by value. The C mirror is an enum with the same
        // explicit values, and a consumer that maps them to strings indexes by
        // these numbers.
        assert_eq!(HeliosTranslatorStatus::Ok.wire(), 0);
        assert_eq!(HeliosTranslatorStatus::NullArgument.wire(), 1);
        assert_eq!(HeliosTranslatorStatus::StructBytes.wire(), 2);
        assert_eq!(HeliosTranslatorStatus::AbiVersion.wire(), 3);
        assert_eq!(HeliosTranslatorStatus::PackageGeneration.wire(), 4);
        assert_eq!(HeliosTranslatorStatus::SubmissionMode.wire(), 5);
        assert_eq!(HeliosTranslatorStatus::HostCallbacks.wire(), 6);
        assert_eq!(HeliosTranslatorStatus::AdapterLuid.wire(), 7);
        assert_eq!(HeliosTranslatorStatus::AdapterUnavailable.wire(), 8);
        assert_eq!(HeliosTranslatorStatus::SessionInit.wire(), 9);
        assert_eq!(HeliosTranslatorStatus::SessionCapacity.wire(), 10);
        assert_eq!(HeliosTranslatorStatus::SessionPoisoned.wire(), 11);
        assert_eq!(HeliosTranslatorStatus::EndpointCapacity.wire(), 12);
        assert_eq!(HeliosTranslatorStatus::UnknownEndpoint.wire(), 13);
        assert_eq!(HeliosTranslatorStatus::EngineClass.wire(), 14);
        assert_eq!(HeliosTranslatorStatus::ContextFlags.wire(), 15);
        assert_eq!(HeliosTranslatorStatus::ContextGeneration.wire(), 16);
        assert_eq!(HeliosTranslatorStatus::UnknownContext.wire(), 17);
        assert_eq!(HeliosTranslatorStatus::ContextAlreadyAttached.wire(), 18);
        assert_eq!(HeliosTranslatorStatus::NoOuterScope.wire(), 19);
        assert_eq!(HeliosTranslatorStatus::ScopeAlreadyOpen.wire(), 20);
        assert_eq!(HeliosTranslatorStatus::ScopeForeignThread.wire(), 21);
        assert_eq!(HeliosTranslatorStatus::ScopeNotSealed.wire(), 22);
        assert_eq!(HeliosTranslatorStatus::ScopeAlreadySealed.wire(), 23);
        assert_eq!(HeliosTranslatorStatus::ScopeStillLive.wire(), 24);
        assert_eq!(HeliosTranslatorStatus::BufferTooSmall.wire(), 25);
        assert_eq!(HeliosTranslatorStatus::BatchBoundExceeded.wire(), 26);
        assert_eq!(HeliosTranslatorStatus::D3D11SubrangeUse.wire(), 27);
        assert_eq!(HeliosTranslatorStatus::QueueEntryPointRefused.wire(), 28);
        assert_eq!(HeliosTranslatorStatus::ForeignVulkanHandle.wire(), 29);
        assert_eq!(HeliosTranslatorStatus::LoaderProvenance.wire(), 30);
        assert_eq!(HeliosTranslatorStatus::ReentrantJoin.wire(), 31);
        assert_eq!(HeliosTranslatorStatus::HostCallbackFailed.wire(), 32);
        assert_eq!(HeliosTranslatorStatus::DeviceLost.wire(), 33);
        assert_eq!(HeliosTranslatorStatus::ReservedNonZero.wire(), 34);
        assert_eq!(HeliosTranslatorStatus::Disposition.wire(), 35);
        assert_eq!(HeliosTranslatorStatus::AccessFlags.wire(), 36);
        assert_eq!(HeliosTranslatorStatus::OperandEncoding.wire(), 37);
        assert_eq!(HeliosTranslatorStatus::BatchId.wire(), 38);
        // The seven causes that used to borrow a neighbour's code. Appended, so
        // no existing value moved — a C consumer built against the previous
        // header still decodes 0..=38 identically.
        assert_eq!(HeliosTranslatorStatus::SessionGeneration.wire(), 39);
        assert_eq!(HeliosTranslatorStatus::SealedUseRange.wire(), 40);
        assert_eq!(HeliosTranslatorStatus::OperandUseIndex.wire(), 41);
        assert_eq!(HeliosTranslatorStatus::ProgressValue.wire(), 42);
        assert_eq!(HeliosTranslatorStatus::ContextStillAttached.wire(), 43);
        assert_eq!(HeliosTranslatorStatus::UnknownAllocationToken.wire(), 44);
        assert_eq!(HeliosTranslatorStatus::PayloadPlaceholderNonZero.wire(), 45);
        assert_eq!(HeliosTranslatorStatus::MAX, 45);
    }

    /// The prohibitions in the module header are kept by *absence*, and absence
    /// is only enforceable if the records cannot grow. Every size is asserted at
    /// compile time above; this test states the inventory in one place so a
    /// reviewer can see what the sizes are protecting.
    #[test]
    fn the_prohibited_payload_inventory_is_pinned_by_size() {
        // If any of these change, a field was added or removed — and the reviewer
        // must re-check the "prohibited payloads" table in the module header
        // before updating the number.
        assert_eq!(core::mem::size_of::<HeliosTranslatorCreateInfoV1>(), 40);
        assert_eq!(core::mem::size_of::<HeliosTranslatorInstanceV1>(), 48);
        assert_eq!(core::mem::size_of::<HeliosTranslatorHostCallbacksV1>(), 32);
        assert_eq!(core::mem::size_of::<HeliosTranslatorDispatchV1>(), 112);
        assert_eq!(core::mem::size_of::<HeliosQueueAttachRequestV1>(), 32);
        assert_eq!(core::mem::size_of::<HeliosOuterContextAttachV1>(), 32);
        assert_eq!(core::mem::size_of::<HeliosOuterScopeBeginV1>(), 24);
        assert_eq!(core::mem::size_of::<HeliosOuterScopeCloseV1>(), 24);
        assert_eq!(core::mem::size_of::<HeliosSealedBatchV1>(), 72);
        assert_eq!(core::mem::size_of::<HeliosSealedResourceUseV1>(), 40);
        assert_eq!(core::mem::size_of::<HeliosSealedOperandV1>(), 16);
        assert_eq!(core::mem::size_of::<HeliosSealedBatchCopyV1>(), 56);
        assert_eq!(core::mem::size_of::<HeliosSyncProgressJoinV1>(), 24);
        assert_eq!(core::mem::size_of::<HeliosSyncProgressResultV1>(), 32);
        assert_eq!(
            core::mem::size_of::<HeliosTranslatorRefusalCountersV1>(),
            112
        );
        // Eleven down slots and two up slots. A twelfth or a third is an ABI
        // version bump, not an addition.
        assert_eq!(
            (core::mem::size_of::<HeliosTranslatorDispatchV1>() - 24) / 8,
            11
        );
        assert_eq!(
            (core::mem::size_of::<HeliosTranslatorHostCallbacksV1>() - 16) / 8,
            2
        );
    }
}
