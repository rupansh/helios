//! `OpenAdapter12` and the eight `D3D12DDI_ADAPTERFUNCS_0109` slots — **stage
//! S5, the commit that makes this body reachable.**
//!
//! `DECISIONS.md` §7.1 / R908: *`OpenAdapter12` must stop refusing in the same
//! commit that makes its body reachable, or the body must not be written yet.*
//! This file is the second half of that sentence. Everything in it is reached
//! from the export below, and the export is registered at
//! `UserModeDriverName[3]` by the same commit.
//!
//! Modelled on `umd/src/adapter.rs`, which is the same eight-step shape for
//! D3D11. Two things there must not be reinvented and are not:
//!
//! * the **closed enum + exhaustive match** on the negotiated interface
//!   (`ARCHITECTURE.md` §12 trap 2). The D3D11 `if/else-if/else` treated
//!   "unknown or older" as D3D11.0 and bulk-filled 150 pointer slots into a
//!   table sized for 101 — *a 376..392 byte out-of-bounds write into the
//!   runtime's heap*;
//! * the ZST **adapter token** (`umd/src/adapter.rs:120-121`), address-taken, so
//!   a handle that is not ours is countable rather than dereferenced.
//!
//! # What is real here and what refuses, at S5
//!
//! | slot | S5 |
//! |---|---|
//! | `pfnGetSupportedVersions` | **real** — the one-token set, D12; which token is `Umd12CoreDdi`'s |
//! | `pfnGetOptionalDDITables` | **real** — `*puEntries = 0`, the measured-correct answer (`DDI_REFERENCE.md` §2.2) |
//! | `pfnCloseAdapter` | **real** — validates the handle and dumps the refusal set |
//! | `pfnGetCaps` | **real** as of L1 — delegates to `caps12` |
//! | `pfnFillDDITable` | **real** as of S6-0 — delegates to `forward12::tables12` |
//! | `pfnCalcPrivateDeviceSize` | **real** as of S6-0b — `device12` |
//! | `pfnCreateDevice` | **real** as of S6-0b — `device12`, engine and all |
//! | `pfnDestroyDevice` | **real** as of S6-0b — `device12` |
//!
//! ⛔ Those are **documented refusals with named counters, not silent stubs**
//! (CLAUDE.md rule 2). Each is reached by the runtime on a knob-ON adapter open,
//! so none of it is the unreachable scaffolding R908 deleted.
//!
//! ⭐ The two bounded log lines are not decoration. `D12-G5` had to be run
//! against WARP through a spy proxy to learn which caps types this runtime asks
//! for and what `TableSize` it passes; from S5 the same contract is recorded on
//! **our own adapter**, for free, every time the knob is on — which is exactly
//! the input L1 (caps) and S6-0 (the tables) need.
//!
//! # The order the runtime calls these in
//!
//! `ARCHITECTURE.md` §1.2, steps 7-12: `pfnGetSupportedVersions` →
//! `pfnGetCaps` ×43 → `pfnGetOptionalDDITables` → `pfnFillDDITable` ×N →
//! `pfnCalcPrivateDeviceSize` → `pfnCreateDevice`. At S5 the runtime never
//! gets past `pfnGetCaps`, and it says so in English on ETW: *"Driver did not
//! respond to D3D12DDICAPS_TYPE_D3D12_OPTIONS caps query."*
//!
//! ⛔ **CORRECTION, measured 2026-08-09 on 26100.8737: `pfnFillDDITable` runs
//! AFTER `pfnCreateDevice`, not before.** This paragraph used to say the
//! opposite — *"the tables are adapter-scoped and filled before any device
//! exists"* — and `ARCHITECTURE.md` §1.2 still does. The order this driver
//! actually sees, on its own adapter, is
//!
//! ```text
//! OpenAdapter12 -> pfnGetCaps(1074) -> pfnGetSupportedVersions x2
//!   -> pfnCalcPrivateDeviceSize -> pfnCreateDevice
//!   -> pfnGetCaps x24 -> pfnGetOptionalDDITables -> pfnFillDDITable x5
//! ```
//!
//! (`tmp/dx12/core-ddi-0116/final-A-110-default/umd12.log:11-47`, three
//! device cycles in one process, identical each time). The tables are still
//! **adapter**-scoped — they are filled through the adapter table and not by
//! `CreateDevice`, which is the real difference from D3D11 — but "before any
//! device exists" was wrong, and `device12::create_device`'s table-shape gate
//! depends on which way round it is.

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use helios_umd_common::hr::{Hresult, DXGI_ERROR_UNSUPPORTED, E_INVALIDARG, E_OUTOFMEMORY, S_OK};

use crate::ddi12;
use crate::caps12;
use crate::device12;
use crate::forward12;
use crate::knobs12;
use crate::{init_once, log_error, log_refusal_summary, note_refusal, UMD12_REFUSALS};

// ---------------------------------------------------------------------------
// Version negotiation — `DECISIONS.md` D12
// ---------------------------------------------------------------------------

/// Compose a `D3D12DDI_SUPPORTED_*` token from the header's own two halves.
///
/// ⛔ **The token is composed, never transcribed.** `DECISIONS.md` §7.2 bans
/// hand-written DDI ABI values, and `DDI_REFERENCE.md` §1.5 records a worked
/// example in the research corpus that got `_0080` wrong by assuming the build
/// half is the decimal `NNNN` (it is the rev digit below `_0090` and the full
/// number from `_0090` up). Both inputs below are bindgen'd `#define`s.
///
/// ⚠ bindgen does **not** emit `D3D12DDI_SUPPORTED_0110` itself — that macro
/// casts through `(UINT64)`, which bindgen cannot constant-fold — so composing
/// from the two halves it *does* emit is the only generated-source route to the
/// value. The formula is `d3d12umddi.h:37-56` verbatim:
/// `((UINT64)INTERFACE_VERSION_Rn << 32) | ((UINT64)BUILD_VERSION_NNNN << 16)`.
const fn ddi12_supported(interface_version: u32, build_version: u32) -> u64 {
    ((interface_version as u64) << 32) | ((build_version as u64) << 16)
}

/// `D3D12DDI_BUILD_VERSION_0116` — **the one hand-written DDI ABI value in this
/// crate**, and the comment below is what licenses it.
///
/// ⛔ `DECISIONS.md` §7.2 bans hand-written DDI ABI values and [`ddi12_supported`]
/// above says why. This constant is the single exception, because the header
/// this build's bindings are generated from **cannot supply it**: the guest's
/// installed WDK is 10.0.26100.0 and its `d3d12umddi.h` stops at
/// `D3D12DDI_SUPPORTED_0110`. Regenerating against WDK 28000 is the retirement
/// lane's U0 and is a separate, larger change (it moves the table shapes too).
///
/// ✅ Transcribed from the staged WDK 28000 header, which is in the tree and
/// readable on both sides (`tmp/wdk-28000/Include/10.0.28000.0/um/d3d12umddi.h`,
/// `Z:\tmp\wdk-28000\…` from the VM):
///
/// ```text
/// 14734: #define D3D12DDI_BUILD_VERSION_0116 116
/// 14735: #define D3D12DDI_SUPPORTED_0116 ((((UINT64)D3D12DDI_INTERFACE_VERSION_R8) << 32) | (((UINT64)D3D12DDI_BUILD_VERSION_0116) << 16))
/// 10301: #define D3D12DDI_MINOR_VERSION_R8 80
/// ```
///
/// ⭐ Two things that transcription rests on are **machine-checked below rather
/// than asserted in prose**: that `_0116` is an **R8** token (so the interface
/// half is the generated `D3D12DDI_INTERFACE_VERSION_R8`, not another constant),
/// and that at this generation the build half is the decimal `NNNN` — proved by
/// the generated `D3D12DDI_BUILD_VERSION_0110 == 110` sitting in the same
/// numbering run. `DDI_REFERENCE.md` §1.5 records the worked example that got
/// `_0080` wrong by assuming that rule *below* `_0090`, where it does not hold.
const BUILD_VERSION_0116: u32 = 116;

/// The one-element set advertised on the **110** arm — today's behaviour.
const SUPPORTED_DDI_VERSIONS_0110: &[u64] = &[ddi12_supported(
    ddi12::D3D12DDI_INTERFACE_VERSION_R8,
    ddi12::D3D12DDI_BUILD_VERSION_0110,
)];

/// The one-element set advertised on the **116** arm — the experiment.
const SUPPORTED_DDI_VERSIONS_0116: &[u64] = &[ddi12_supported(
    ddi12::D3D12DDI_INTERFACE_VERSION_R8,
    BUILD_VERSION_0116,
)];

/// The negotiated DDI interface, as a **closed set**.
///
/// `ARCHITECTURE.md` §12 trap 2: never let an unknown interface fall into an
/// `else` that fills the largest table. The D3D11 driver paid 376..392 bytes of
/// the runtime's heap to learn that. Here the set has one member **at runtime**
/// — the arm [`Ddi12Interface::selected`] resolves to — so [`Self::from_pair`]
/// admits exactly one pair and every other pair is a counted refusal.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum Ddi12Interface {
    /// `D3D12DDI_SUPPORTED_0110` — release R8, build 110. Fills the
    /// `_0109`-generation tables (D12: `_0110` adds no table struct of its own).
    /// **The default, and the only arm whose tables this driver implements.**
    R8_0110,
    /// `D3D12DDI_SUPPORTED_0116` — release R8, build 116. The Core DDI the
    /// HPS2-retirement reference requires (`HELIOS_PRESENT_SYNC_RETIREMENT.md`
    /// §10.2, line 982: *"build 28000+ and negotiated `D3D12DDI_SUPPORTED_0116`
    /// or later"* — Core 0112 adds native-fence **create**, 0116 native-fence
    /// **open**).
    ///
    /// ⛔⛔ **THE TABLE SHAPES FOR THIS ARM ARE NOT IMPLEMENTED**, and that is
    /// why `device12::create_device` refuses it before constructing anything.
    /// This variant exists to answer exactly one question — *does the inbox
    /// D3D12 runtime on build 26100 accept the 0116 token at all, or is the
    /// 26100 WDK header limit also a runtime limit?* — and it answers it by
    /// being advertised, not by being served.
    R8_0116,
}

impl Ddi12Interface {
    /// The token's **build** half, as the header's `D3D12DDI_BUILD_VERSION_NNNN`
    /// spells it.
    const fn build(self) -> u32 {
        match self {
            Self::R8_0110 => ddi12::D3D12DDI_BUILD_VERSION_0110,
            Self::R8_0116 => BUILD_VERSION_0116,
        }
    }

    /// The high 32 bits of the token: `(12 << 16) | MINOR_VERSION_R8`.
    ///
    /// ⚠ Identical for both arms and that is a **fact about the header, not a
    /// simplification**: WDK 28000 gives `_0110` through `_0119` the same
    /// `D3D12DDI_INTERFACE_VERSION_R8 == ((12 << 16) | 80)` and differs them
    /// only in `D3D12DDI_BUILD_VERSION_NNNN`. So the 116 experiment moves the
    /// *build* number and nothing else — which is also why an 0116 negotiation
    /// cannot be detected by looking at `Interface` alone.
    const fn interface(self) -> u32 {
        match self {
            Self::R8_0110 | Self::R8_0116 => ddi12::D3D12DDI_INTERFACE_VERSION_R8,
        }
    }

    /// The low 32 bits: `BUILD_VERSION_NNNN << 16`.
    const fn version(self) -> u32 {
        self.build() << 16
    }

    /// The whole `D3D12DDI_SUPPORTED_NNNN` token.
    const fn token(self) -> u64 {
        ddi12_supported(self.interface(), self.build())
    }

    /// This arm's advertised set — what `pfnGetSupportedVersions` hands back.
    ///
    /// ⛔ **Exactly one entry, and that is `DECISIONS.md` D12's load-bearing
    /// half.** With a one-element set the runtime either negotiates that token
    /// or fails the handshake with its own string (*"Failed to find matching
    /// DDI versions"*), so there is exactly one legal `(Interface, Version)`
    /// pair and exactly one legal table shape. A second entry would make a
    /// second table shape reachable — precisely the `ARCHITECTURE.md` §12 trap 2
    /// / R702 surface D12 closes by construction rather than by guarding.
    ///
    /// ⚠ **`Umd12CoreDdi` selects WHICH one-element set, never how many
    /// elements.** Both arms are one-element constants and the compile-time
    /// block below asserts the length of *each*, so the property above survives
    /// the knob rather than depending on it.
    const fn advertised(self) -> &'static [u64] {
        match self {
            Self::R8_0110 => SUPPORTED_DDI_VERSIONS_0110,
            Self::R8_0116 => SUPPORTED_DDI_VERSIONS_0116,
        }
    }

    /// The arm `HKLM\SOFTWARE\Helios!Umd12CoreDdi` selects. **Absent = 110.**
    ///
    /// ⛔ An unrecognised value is a **counted refusal that falls back to 110**,
    /// never a value passed through to the runtime: advertising a token this
    /// driver has no arm for would put the runtime into a negotiation whose
    /// table shape nothing in this crate knows, which is the
    /// `ARCHITECTURE.md` §12 trap 2 surface arriving through the registry
    /// instead of through an `else`.
    ///
    /// Read through [`knobs12::umd12_core_ddi`], whose `OnceLock` makes the
    /// answer stable for the process — load-bearing, because
    /// `pfnGetSupportedVersions` and `pfnCreateDevice` must agree about which
    /// single pair is legal.
    pub(crate) fn selected() -> Self {
        match knobs12::umd12_core_ddi() {
            110 => Self::R8_0110,
            116 => Self::R8_0116,
            other => {
                UMD12_REFUSALS.core_ddi_knob_unknown.bump();
                let n = UMD12_REFUSALS.core_ddi_knob_unknown.get();
                if n <= LOG_BUDGET {
                    log_error!(
                        "Umd12CoreDdi={other} names no Core DDI build this driver can advertise \
                         (110 or 116) -- falling back to 110 (x{n})",
                    );
                }
                Self::R8_0110
            }
        }
    }

    /// ✅ `D12-G5` confirmed the split rather than inferring it:
    /// `D3D12DDIARG_CREATEDEVICE::Interface` carries the token's **high** 32
    /// bits and `::Version` its **low** 32 bits, and
    /// `((u64)Interface << 32) | Version` matched the advertised entry bit for
    /// bit. Matching on the pair keeps this site independent of that split
    /// being re-derived correctly a second time.
    ///
    /// ⛔ It admits the **selected** arm and nothing else — not "any arm this
    /// enum can name". A runtime that handed back 116 while this process
    /// advertised 110 is a `Ddi12VersionMismatch`, exactly as a runtime that
    /// handed back `_0040` would be: the one-element set means one legal pair,
    /// and that property must not weaken just because the enum grew a variant.
    ///
    /// Panic-free: two `u32` comparisons, no indexing.
    pub(crate) fn from_pair(interface: u32, version: u32) -> Option<Self> {
        let selected = Self::selected();
        if interface == selected.interface() && version == selected.version() {
            Some(selected)
        } else {
            None
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::R8_0110 => "_0110",
            Self::R8_0116 => "_0116",
        }
    }
}

/// The Core DDI **build** number this process actually advertises, for
/// `knobs12::resolved_inventory`.
///
/// ⚠ The effective value, not the raw DWORD: an unrecognised `Umd12CoreDdi`
/// falls back to 110 and the inventory line must say 110, because that is the
/// configuration the run had. `CoreDdiKnobUnknown` beside it is what says the
/// registry value was rejected rather than honoured.
pub(crate) fn selected_core_ddi_build() -> u32 {
    Ddi12Interface::selected().build()
}

/// Decode any `(Interface, Version)` pair the way `d3d12umddi.h` composes one:
/// `Interface = (MAJOR << 16) | MINOR_Rn` and `Version = BUILD << 16`. Returns
/// `(major, minor, build)`.
///
/// ⭐ Deliberately total — it decodes a pair this driver did **not** advertise,
/// which is the only case where the numbers are worth printing. A log line that
/// can only decode the values we already know is not an instrument.
pub(crate) fn decode_pair(interface: u32, version: u32) -> (u32, u32, u32) {
    (interface >> 16, interface & 0xffff, version >> 16)
}

// Keep the enum and the advertised sets in lockstep at COMPILE time, the way
// `umd/src/adapter.rs:90-95` does: adding a token without adding a variant
// fails here, and adding a variant without an arm fails the exhaustive matches
// above. That is the property the `else`-as-default did not have.
//
// ⭐ Every assertion is stated **per arm**, not over "the advertised set", so
// the knob cannot make one of them vacuous — a knob-selected set whose
// invariants were only checked on the default arm would be exactly the shape of
// assurance that is not real.
const _: () = {
    // The build half is the decimal `NNNN` at this generation. Generated, and
    // the premise `BUILD_VERSION_0116` is transcribed under.
    assert!(ddi12::D3D12DDI_BUILD_VERSION_0110 == 110);

    assert!(SUPPORTED_DDI_VERSIONS_0110.len() == 1);
    assert!((SUPPORTED_DDI_VERSIONS_0110[0] >> 32) as u32 == Ddi12Interface::R8_0110.interface());
    assert!(SUPPORTED_DDI_VERSIONS_0110[0] as u32 == Ddi12Interface::R8_0110.version());
    assert!(SUPPORTED_DDI_VERSIONS_0110[0] == Ddi12Interface::R8_0110.token());

    assert!(SUPPORTED_DDI_VERSIONS_0116.len() == 1);
    assert!((SUPPORTED_DDI_VERSIONS_0116[0] >> 32) as u32 == Ddi12Interface::R8_0116.interface());
    assert!(SUPPORTED_DDI_VERSIONS_0116[0] as u32 == Ddi12Interface::R8_0116.version());
    assert!(SUPPORTED_DDI_VERSIONS_0116[0] == Ddi12Interface::R8_0116.token());

    // The two arms must be distinguishable, or the experiment reads its own
    // control arm and calls it a result.
    assert!(Ddi12Interface::R8_0110.token() != Ddi12Interface::R8_0116.token());
};

// `D3D12DDIARG_OPENADAPTER::pAdapterFuncs` is typed `D3D12DDI_ADAPTERFUNCS*`
// (the base shape), but the version is not negotiated until `pfnGetSupportedVersions`
// runs — which is AFTER this table is written (`ARCHITECTURE.md` §1.2, steps 6-7).
// So the driver must commit to one shape before it knows the answer.
//
// That is sound here only because the two shapes are byte-identical: 8 slots,
// 64 bytes, same offsets, differing solely in `pfnCreateDevice`'s *signature*
// (`_0003` vs `_0109`), which is one pointer either way. Asserted rather than
// asserted-in-prose, because it is the premise of the cast in `OpenAdapter12`.
//
// ⚠ The signature difference is still real and is handled by D12, not by this
// assert: `D3D12DDIARG_CREATEDEVICE_0109` is `_0003` plus two trailing fields,
// so reading it against a `_0003` arg would read past the end. Advertising one
// token means a `_0003`-generation create can never be negotiated.
//
// ⭐ **`Umd12CoreDdi=116` does not touch this table**, and that was checked
// rather than assumed: WDK 28000's `d3d12umddi.h` still declares exactly two
// adapter-funcs shapes, `D3D12DDI_ADAPTERFUNCS` (`:2686`) and
// `D3D12DDI_ADAPTERFUNCS_0109` (`:13797`), both eight slots. There is no
// `_0116` shape, and in any case this table is written **before** any version
// is negotiated, so its shape cannot depend on the advertised token.
const _: () = {
    assert!(
        core::mem::size_of::<ddi12::D3D12DDI_ADAPTERFUNCS>()
            == core::mem::size_of::<ddi12::D3D12DDI_ADAPTERFUNCS_0109>()
    );
    assert!(
        core::mem::offset_of!(ddi12::D3D12DDI_ADAPTERFUNCS, pfnCreateDevice)
            == core::mem::offset_of!(ddi12::D3D12DDI_ADAPTERFUNCS_0109, pfnCreateDevice)
    );
    assert!(
        core::mem::offset_of!(ddi12::D3D12DDI_ADAPTERFUNCS, pfnDestroyDevice)
            == core::mem::offset_of!(ddi12::D3D12DDI_ADAPTERFUNCS_0109, pfnDestroyDevice)
    );
};

// ---------------------------------------------------------------------------
// The adapter identity token
// ---------------------------------------------------------------------------

/// The value handed back as this adapter's `pDrvPrivate`.
///
/// A zero-sized type, address-taken — `umd/src/adapter.rs:120-121` (R821) and
/// the same reasoning: a ZST says *"this pointer is not dereferenceable state"*
/// in a way a `usize` carrying a magic number does not.
///
/// ⚠ D3D12 has no per-adapter driver state to keep here. Every adapter-scoped
/// answer is a constant of the build (the version set, the caps policy), and
/// `hRTAdapter` is not stashed because nothing in this driver needs it: the
/// D3D11 side stashes it for `pfnEscapeCb` through the scan-out acquire path
/// (`umd/src/adapter.rs:225`), which is a D3D11 present-vehicle mechanism this
/// DLL deliberately does not have (`probe12`'s module doc: a D3D12 export in the
/// `helios_umd_*` family would steal the D3D11 vehicle). The first D3D12 caller
/// that needs it adds a field here and says why.
struct AdapterToken;
static ADAPTER_TOKEN: AdapterToken = AdapterToken;

/// Validate an adapter handle against the token we handed out. **Reports only.**
///
/// Deliberately not a refusal, exactly as `umd/src/adapter.rs:132-149` is: the
/// counter has to be observed at zero on a real boot before any DDI starts
/// rejecting on it. Returning `bool` rather than nothing keeps that decision at
/// the call site if it ever changes.
fn adapter_ok(h: ddi12::D3D12DDI_HADAPTER) -> bool {
    let expected = core::ptr::addr_of!(ADAPTER_TOKEN) as *const c_void;
    if core::ptr::eq(h.pDrvPrivate as *const c_void, expected) {
        return true;
    }
    UMD12_REFUSALS.adapter_unrecognised.bump();
    let n = UMD12_REFUSALS.adapter_unrecognised.get();
    if n <= LOG_BUDGET {
        log_error!(
            "adapter handle not ours: pDrvPrivate={:p} expected={:p} (x{n}) -- counted only",
            h.pDrvPrivate,
            expected,
        );
    }
    false
}

/// How many times a bounded, per-site evidence line may repeat.
///
/// ⚠ These lines are one-shot contract capture, not per-op tracing, so they are
/// not behind `Umd12Trace`: a caps gauntlet is ~43 lines *per adapter open* and
/// the whole point is that it is readable without having set a knob first. The
/// budget is what keeps a pathological caller from turning that into a log
/// flood. `helios_umd_common::throttle` is the per-op mechanism and is
/// deliberately not what these use — it exists for repeat traffic on a hot
/// path, and none of these sites is on one.
const LOG_BUDGET: usize = 64;

// ---------------------------------------------------------------------------
// The export
// ---------------------------------------------------------------------------

/// The D3D12 adapter entry point — **reachable as of S5.**
///
/// The loader resolves this by name out of `UserModeDriverName[3]`, so a missing
/// export is a different and worse failure than a clean refusal.
///
/// Steps, matching `ARCHITECTURE.md` §1.2 rows 1-6:
///
/// 1. `init_once()` — name this DLL's log file **above the first log line**;
/// 2. the `UmdD3D12` kill switch (D11). Absent ⇒ `DXGI_ERROR_UNSUPPORTED`,
///    i.e. bit-identical to a build with no D3D12 path;
/// 3. validate `open_data` and the two out-pointers inside it;
/// 4. hand out the adapter token;
/// 5. fill all **8** slots of `D3D12DDI_ADAPTERFUNCS_0109`.
///
/// ⚠ There is no `Interface`/`Version` in `D3D12DDIARG_OPENADAPTER` — unlike
/// `D3D10DDIARG_OPENADAPTER`, which is why `umd/src/adapter.rs` can dispatch
/// inside `OpenAdapter10` and this cannot (`DDI_REFERENCE.md` §1.2). All
/// negotiation is `pfnGetSupportedVersions` + `pfnGetOptionalDDITables` +
/// `pfnFillDDITable`, all of them **after** this returns.
///
/// # Safety
/// `open_data` is the runtime's `D3D12DDIARG_OPENADAPTER*`. It must point at a
/// live, writable, correctly aligned `D3D12DDIARG_OPENADAPTER` for the duration
/// of the call, and its `pAdapterFuncs` must point at a writable
/// `D3D12DDI_ADAPTERFUNCS`-sized (64-byte) table the runtime owns.
///
/// ⛔ The 64 bytes are the contract, and this body writes exactly that many:
/// it stores an `D3D12DDI_ADAPTERFUNCS_0109` through a cast pointer, whose size
/// and field offsets are asserted equal to the declared type's at the top of
/// this file. Writing a table sized for a version the runtime did not ask for
/// is `ARCHITECTURE.md` §12 trap 2 — a 376..392-byte out-of-bounds write into
/// the runtime's heap.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn OpenAdapter12(open_data: *mut c_void) -> Hresult {
    // ⛔ FIRST, above this entry point's first log line. `log::init`'s basename
    // defaults to `"umd"` and the log PATH is a `OnceLock` latched by the first
    // line of any kind, so arriving late puts this driver's evidence in D3D11's
    // file permanently. See `crate::init_once`.
    init_once();

    // ── 2. The kill switch (D11) ────────────────────────────────────────────
    // ⛔ Above every other check, including the null test, and that ordering is
    // deliberate: with the knob absent this function must be indistinguishable
    // from the pre-S5 refusal, which examined nothing. A null-argument counter
    // that could tick on a knob-OFF machine would make "D3D12 is off" and "the
    // runtime handed us a bad pointer" share an evidence channel.
    //
    // ⚠ dwm.exe already calls this in production (`DECISIONS.md` §7.13). The
    // first boot with `UmdD3D12=1` is a change to the compositor.
    if !knobs12::umd_d3d12() {
        // ⚠ ONE log line for one event, and it is the set summary rather than a
        // bespoke "OpenAdapter12 refused" line beside it. R911: an already-loud
        // arm must not also emit the summary, and the inverse holds too — a
        // second line saying what `OpenAdapter12=1` already says makes the count
        // and the prose two things that can disagree.
        note_refusal(&UMD12_REFUSALS.open_adapter12);
        // ⛔ Declining an unimplemented DDI is DXGI_ERROR_UNSUPPORTED
        // (0x887A_0004), NEVER DXGI_ERROR_DRIVER_INTERNAL_ERROR (0x887A_0020) —
        // the latter is recorded by the runtime and by ETW as a *driver fault*,
        // so a client's ordinary "this driver has no D3D12 DDI" negotiation
        // would be logged as a Helios bug. `umd`'s copy of this export returned
        // the wrong one until R801 because the two shared a constant name and
        // both printed identically in our own logs.
        return DXGI_ERROR_UNSUPPORTED;
    }

    // ── 3. Validate ─────────────────────────────────────────────────────────
    if open_data.is_null() {
        note_refusal(&UMD12_REFUSALS.open_adapter12_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check above, and the caller guarantees a live,
    // aligned, writable `D3D12DDIARG_OPENADAPTER` for the duration of the call.
    let open = unsafe { &mut *open_data.cast::<ddi12::D3D12DDIARG_OPENADAPTER>() };

    let funcs = open.pAdapterFuncs;
    if funcs.is_null() {
        note_refusal(&UMD12_REFUSALS.open_adapter12_bad_arg);
        return E_INVALIDARG;
    }

    let selected = Ddi12Interface::selected();
    log_error!(
        "OpenAdapter12: knob ON, hRTAdapter={:p} pAdapterCallbacks={:p} advertising {} \
         token={:#018x} Interface={:#010x} (major={} minor={}) Version={:#010x} (build={})",
        open.hRTAdapter.handle,
        open.pAdapterCallbacks,
        selected.name(),
        selected.token(),
        selected.interface(),
        selected.interface() >> 16,
        selected.interface() & 0xffff,
        selected.version(),
        selected.build(),
    );

    // ── 4. The driver's adapter handle ──────────────────────────────────────
    open.hAdapter.pDrvPrivate = core::ptr::addr_of!(ADAPTER_TOKEN) as *mut c_void;

    // ── 5. All eight slots ──────────────────────────────────────────────────
    // Built as a value and written once, rather than eight field stores through
    // a raw pointer: a `D3D12DDI_ADAPTERFUNCS_0109` literal cannot leave a slot
    // NULL, because the struct has no `..Default::default()` here and the
    // compiler requires every field. "The runtime calls through an
    // uninitialised slot" is the failure this shape makes unrepresentable.
    let table = ddi12::D3D12DDI_ADAPTERFUNCS_0109 {
        pfnCalcPrivateDeviceSize: Some(calc_private_device_size),
        pfnCreateDevice: Some(create_device),
        pfnCloseAdapter: Some(close_adapter),
        pfnGetSupportedVersions: Some(get_supported_versions),
        pfnGetCaps: Some(get_caps),
        pfnGetOptionalDDITables: Some(get_optional_ddi_tables),
        pfnFillDDITable: Some(fill_ddi_table),
        pfnDestroyDevice: Some(destroy_device),
    };
    // SAFETY: `funcs` is non-null per the check above and the caller guarantees
    // it points at a writable `D3D12DDI_ADAPTERFUNCS` the runtime owns. The cast
    // to the `_0109` shape writes exactly the same 64 bytes at the same offsets
    // — asserted at compile time at the top of this file — and D12's one-token
    // set makes the `_0003`-generation `pfnCreateDevice` signature unreachable.
    unsafe {
        core::ptr::write(funcs.cast::<ddi12::D3D12DDI_ADAPTERFUNCS_0109>(), table);
    }

    S_OK
}

// ---------------------------------------------------------------------------
// The eight slots
//
// ⛔ Every one is `unsafe extern "C"`, not `extern "system"`. Measured by
// fault-injection against the host cross-check (`PARALLEL.md` §5): the
// `d3d12umddi` PFN typedefs are `extern "C"`. On x86_64 Windows the two are the
// same ABI, so this is a *type* error and not a calling-convention bug — which
// is exactly why it would otherwise have been written wrong 214 times and
// caught by nothing until the first compile. ⚠ Note this differs from
// `OpenAdapter12` above, which the loader resolves by name and which keeps the
// D3D11 side's `extern "system"`.
// ---------------------------------------------------------------------------

/// `pfnGetSupportedVersions` — the count-then-fill idiom, D12's one-token set.
///
/// `_Inout_ UINT32* puEntries` + `_Out_writes_opt_(*puEntries)`: the runtime may
/// call with a null buffer to learn the count, then again with storage.
/// ⚠ **UNVERIFIED** that this runtime actually makes the first call with a null
/// buffer (`DDI_REFERENCE.md` §1.3); this handles both shapes, and the log line
/// below records which one arrived — settling it as a side effect of S5 rather
/// than needing the §15 spy again.
unsafe extern "C" fn get_supported_versions(
    h_adapter: ddi12::D3D12DDI_HADAPTER,
    entries: *mut ddi12::UINT32,
    supported_versions: *mut ddi12::UINT64,
) -> ddi12::HRESULT {
    let _ = adapter_ok(h_adapter);

    if entries.is_null() {
        note_refusal(&UMD12_REFUSALS.get_supported_versions_bad_arg);
        return E_INVALIDARG;
    }

    // SAFETY: non-null per the check above; the DDI declares it `_Inout_`, so
    // the runtime guarantees a live, writable `UINT32` for the call.
    let requested = unsafe { *entries };

    // ⛔ Resolved ONCE per call and reused for the log line, the count and the
    // fill. Reading the knob three times would be three chances for the three to
    // disagree about which single pair is legal — and this DDI's whole contract
    // is that there is exactly one.
    let selected = Ddi12Interface::selected();
    let advertised = selected.advertised();
    log_error!(
        "GetSupportedVersions: requested={requested} bufNull={} advertising {} entries={} \
         token={:#018x} Interface={:#010x} (major={} minor={}) Version={:#010x} (build={})",
        supported_versions.is_null(),
        selected.name(),
        advertised.len(),
        selected.token(),
        selected.interface(),
        selected.interface() >> 16,
        selected.interface() & 0xffff,
        selected.version(),
        selected.build(),
    );
    // SAFETY: as above. Written before the early return below, because the
    // count-query form's whole purpose is this store.
    unsafe { *entries = advertised.len() as ddi12::UINT32 };

    if supported_versions.is_null() {
        return S_OK;
    }
    if (requested as usize) < advertised.len() {
        // ⚠ Not a refusal counter: the runtime asking with a short buffer is a
        // legal first half of the count-then-fill idiom, and `*entries` above
        // has already told it the real count. Counting it would put a normal
        // negotiation in a line whose whole purpose is to read zero.
        return E_OUTOFMEMORY;
    }

    for (index, version) in advertised.iter().enumerate() {
        // SAFETY: the runtime declared storage for `requested` entries and
        // `requested >= advertised.len()` was just checked, so every index in
        // this loop is inside the buffer.
        unsafe { *supported_versions.add(index) = *version };
    }
    S_OK
}

/// `pfnGetCaps` — the DDI slot; [`caps12`] owns the 43 answers.
///
/// ⭐ **This is the call the runtime makes FIRST**, before
/// `pfnGetSupportedVersions`, and refusing it aborts device creation two calls
/// in — measured at S5 (`tmp/dx12/gates/G6/RESULT.md`), and the reason
/// `ARCHITECTURE.md` §1.2's step order was corrected. Nothing `caps12` answers
/// may depend on a negotiated version, because there is not one yet.
unsafe extern "C" fn get_caps(
    h_adapter: ddi12::D3D12DDI_HADAPTER,
    arg: *const ddi12::D3D12DDIARG_GETCAPS,
) -> ddi12::HRESULT {
    let _ = adapter_ok(h_adapter);
    // SAFETY: forwarded unchanged; the DDI declares `arg` `_In_ CONST`, and
    // `caps12` null-checks both it and its `pData` rather than trusting them.
    unsafe { caps12::get_caps(arg) }
}

/// `pfnGetOptionalDDITables` — **real**: this driver wants no extra tables.
///
/// ✅ The measured-correct answer (`DDI_REFERENCE.md` §2.2): WARP answers
/// `*puEntries = 0` and the runtime still fills two command-list tables, so the
/// second table is not something a driver asks for. The runtime also states the
/// only legal use of this entry point in its own strings — *"…only supports
/// `D3D12DDI_TABLE_TYPE_COMMAND_LIST_3D`. An unsupported table type was
/// requested."* — so 0 is the answer that cannot be misread.
unsafe extern "C" fn get_optional_ddi_tables(
    h_adapter: ddi12::D3D12DDI_HADAPTER,
    entries: *mut ddi12::UINT32,
    requests: *mut ddi12::D3D12DDI_TABLE_REQUEST,
) -> ddi12::HRESULT {
    let _ = adapter_ok(h_adapter);

    if entries.is_null() {
        note_refusal(&UMD12_REFUSALS.get_optional_ddi_tables_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check above; the DDI declares it `_Inout_`.
    let requested = unsafe { *entries };
    log_error!(
        "GetOptionalDDITables: requested={requested} bufNull={} -> 0 tables",
        requests.is_null(),
    );
    // SAFETY: as above. ⛔ `requests` is deliberately NOT written: with
    // `*entries = 0` there is no element to write, and touching a
    // `_Out_writes_opt_(*puEntries)` buffer past the count it was just given is
    // the same class of overrun as writing a table sized for the wrong version.
    unsafe { *entries = 0 };
    S_OK
}

/// `pfnFillDDITable` — the DDI slot; [`forward12::tables12`] owns the tables.
///
/// The split is deliberate and follows `ARCHITECTURE.md` §5: this file is the
/// **adapter** surface, and `forward12` is the 206-slot DDI surface. Keeping the
/// slot here and the fill there means the eleven-lane sequencer lives beside the
/// lanes rather than inside the adapter.
///
/// ⛔ **The `SIZE_T` is the contract, and `tables12` is where it is honoured.**
/// `ARCHITECTURE.md` §12 rule 16 / R702: 24H2 passed 576 bytes for a 592-byte
/// `DRIVERCAPS` and the D3D11 driver wrote past it. D3D12 parameterises the size
/// explicitly and it *moves with the version* — 992/600/56 at `_0110`,
/// 768/464/56 at `_0040` — so `size_of::<T>()` is never the count. Both
/// directions of disagreement are counted (`FillDDITableTruncated`,
/// `FillDDITableOversized`).
///
/// The line below records the runtime's own numbers on *this* adapter, which is
/// what `D12-G5` needed a WARP spy proxy to obtain.
unsafe extern "C" fn fill_ddi_table(
    h_adapter: ddi12::D3D12DDI_HADAPTER,
    table_type: ddi12::D3D12DDI_TABLE_TYPE,
    table: *mut c_void,
    table_size: ddi12::SIZE_T,
    index: ddi12::UINT,
    h_rt_table: ddi12::D3D12DDI_HRTTABLE,
) -> ddi12::HRESULT {
    let _ = adapter_ok(h_adapter);

    FILL_DDI_TABLE_CALLS.fetch_add(1, Ordering::Relaxed);
    let n = FILL_DDI_TABLE_CALLS.load(Ordering::Relaxed);
    if n <= LOG_BUDGET {
        log_error!(
            "FillDDITable: type={table_type} size={table_size} index={index} pTable={table:p} \
             hRTTable={:p} (x{n})",
            h_rt_table.handle,
        );
    }
    // SAFETY: forwarded unchanged. `tables12::fill` re-states this function's
    // own precondition — `table` points at `table_size` writable, pointer-aligned
    // bytes the runtime owns — and validates the null/too-small cases itself
    // rather than trusting them.
    unsafe { forward12::tables12::fill(table_type, table, table_size, index, h_rt_table) }
}

/// How many times the runtime has asked this adapter to fill a table.
///
/// ⚠ **Not a refusal counter, and deliberately outside the refusal set**: a fill
/// is the normal path now, not a skip. It exists to bound the evidence line
/// above, and because "how many tables did this runtime ask for, of which types
/// and at which sizes" is the one question `D12-G5` had to build a spy proxy to
/// answer. The refusals that *can* happen inside a fill have their own named
/// counters in `UMD12_REFUSALS`.
static FILL_DDI_TABLE_CALLS: AtomicUsize = AtomicUsize::new(0);

/// `pfnCalcPrivateDeviceSize` — the DDI slot; [`device12`] owns the answer.
///
/// ⛔ **The size and the construction are one function of `Flags`.**
/// `D3D12DDI_CREATE_DEVICE_FLAG_DEBUGGABLE` arrives on *both* this arg and
/// `D3D12DDIARG_CREATEDEVICE_0109` (`DDI_REFERENCE.md` §1.4), so a size computed
/// here and a `size_of::<Device>()` written there is a buffer overrun waiting
/// for a debug-layer client. Both sites call `device12::device_private_size`.
unsafe extern "C" fn calc_private_device_size(
    h_adapter: ddi12::D3D12DDI_HADAPTER,
    arg: *const ddi12::D3D12DDIARG_CALCPRIVATEDEVICESIZE,
) -> ddi12::SIZE_T {
    let _ = adapter_ok(h_adapter);
    // SAFETY: forwarded unchanged; the DDI declares `arg` `_In_ CONST`, and
    // `device12` null-checks it rather than trusting that.
    let size = unsafe { device12::calc_private_device_size(arg) };
    // `SIZE_T` is the WDK's spelling and is a distinct type from `usize` even
    // though both are 64-bit here, so the PFN type needs the conversion.
    size as ddi12::SIZE_T
}

/// `pfnCreateDevice` — the DDI slot; [`device12`] owns the eight steps.
///
/// ⚠ There is no table fill in it. D3D12 fills its tables at **adapter** scope
/// through `pfnFillDDITable`, before any device exists — measured at S5, and the
/// opposite of the D3D11 shape where `CreateDevice` writes the device-funcs
/// table itself.
unsafe extern "C" fn create_device(
    h_adapter: ddi12::D3D12DDI_HADAPTER,
    arg: *const ddi12::D3D12DDIARG_CREATEDEVICE_0109,
) -> ddi12::HRESULT {
    let _ = adapter_ok(h_adapter);
    // SAFETY: forwarded unchanged; `device12::create_device` validates every
    // runtime-supplied pointer before constructing anything, which is the
    // ordering `DeviceUnderConstruction`'s docstring exists to record.
    unsafe { device12::create_device(arg) }
}

/// `pfnDestroyDevice` — the DDI slot; [`device12`] owns the teardown.
///
/// ⚠ It lives on the **adapter** table (`d3d12umddi.h:13649`), not the device
/// table. That is a shape difference from D3D11 and a classic place to leave a
/// NULL (`DDI_REFERENCE.md` §1.3).
unsafe extern "C" fn destroy_device(h_device: ddi12::D3D12DDI_HDEVICE) {
    // SAFETY: the runtime passes back a handle this driver returned `S_OK` for
    // from `create_device`, exactly once.
    unsafe { device12::destroy_device(h_device) }
}

/// `pfnCloseAdapter` — **real**, and the set's readout point.
///
/// ⭐ This is where the refusal set becomes readable. `note_refusal` emits the
/// summary on a counter's *first* hit, but the two highest-volume S5 refusals
/// (`GetCaps12Unimplemented`, `FillDdiTable12Unimplemented`) use `bump()`
/// because they already log their own line (R911) — so without a readout here a
/// run in which only those fired would leave the set unprinted. T5's lesson,
/// restated: *an instrument nothing can read is not an instrument.*
unsafe extern "C" fn close_adapter(h_adapter: ddi12::D3D12DDI_HADAPTER) -> ddi12::HRESULT {
    let _ = adapter_ok(h_adapter);
    log_error!("CloseAdapter");
    log_refusal_summary();
    // ⭐ And the other instrument, for the same reason: the per-slot noop hit
    // counts. `PARALLEL.md` §9.2 makes "its noop hit counters read zero for its
    // slots under a real workload" a per-lane definition of done, and
    // `CONFORMANCE.md`'s charter is to drive them to zero — neither is
    // executable unless something prints them.
    forward12::noop12::log_noop_hits();
    // And the two runtime table handles the fill stashed — the one piece of the
    // D3D12 contract that cannot be recovered after the fill returns
    // (`DDI_REFERENCE.md` §2.2).
    forward12::tables12::log_command_list_tables();
    S_OK
}
