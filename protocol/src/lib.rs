//! helios_protocol — shared wire-format definitions for the Helios vGPU stack.
//!
//! This crate is the single source of truth for every byte that crosses a
//! trust/ABI boundary in Helios:
//!
//!   ICD / UMD (user-mode)  --WDDM DDI private data-->  KMD (kernel-mode, no_std)
//!   KMD                    --virtqueue / paging DMA->  virtio-gpu device / QEMU
//!
//! The `kmd_render`, `umd`, and `umd12` crates depend on this crate, and Mesa
//! and QEMU mirror the same records in C, so no two halves of a boundary can
//! ever drift on struct layout. It is `#![no_std]` so the kernel-mode KMD can
//! use it. The three C mirrors, each with `_Static_assert`s on every size,
//! alignment, and offset its Rust side asserts:
//!
//! | C header | Mirrors |
//! |---|---|
//! | `protocol/include/helios_wddm.h` | [`wddm`] — HWA2, HOB1 + its use/operand records, HOS1, HOC1 |
//! | `protocol/include/helios_diagnostics.h` | [`diagnostics`] — the §12.3 ETW schema |
//! | `qemu-helios/include/hw/virtio/helios_physical_memory.h` | [`physical_memory`] — HPM1 and the HLM1 BAR profile |
//!
//! # Module map (HELIOS_PRESENT_SYNC_RETIREMENT.md section 17.1)
//!
//! | Module | Boundary | Doc section |
//! |---|---|---|
//! | [`wddm`] | D3D UMD -> KMD allocation identity (HWA2), outer batch (HOB1), D3D12 submit descriptor (HOS1), command pool (HOC1) | 10.3, 10.4, 10.6 |
//! | [`translation_session`] | Mesa `vn_instance` -> KMD session establishment (HTS1) and outer-context attach (HQA1) | 10.4 |
//! | [`native_render`] | native Vulkan ICD -> KMD Render (HVC1/HNR2), allocation (HVM1), synchronous reply (HVR1) | 10.7 |
//! | [`physical_memory`] | KMD paging DMA -> QEMU device page tables (HPM1) and the HLM1 BAR profile | 10.7 |
//! | [`diagnostics`] | KMD -> OS ETW, one-way lossy schema | 12.3 |
//! | [`virtio_gpu`] | standard virtio-gpu control headers/capsets | VirtIO 1.2 §5.7 |
//! | [`features`] | virtio feature bits | — |
//!
//! ⚠ [`escape`] and [`ioctl`] are the **legacy System-class** ABI. Section 17.1
//! of the retirement manifest deletes both files outright — `escape.rs` because
//! every verb `0x0001..0x0011` is graphics/presentation/synchronization/
//! allocation or their private diagnostic query and none belongs to the new
//! package (explicitly including `QUERY_STATS` and `QUERY_SCANOUT_TIMELINE`,
//! which may **not** be retained as an observability fallback), and `ioctl.rs`
//! with the obsolete `kmd/` package, since no IOCTL verb may become a
//! compatibility carrier for the new generation. [`wddm_legacy`] joins them: it
//! is the pre-retirement content of [`wddm`], carried verbatim so the same three
//! consumers keep compiling while each migrates to HWA2/HOB1/HOS1/HOC1.
//! They remain declared here only
//! because their current consumers (`kmd_render`, `umd`, `umd12`) have not yet
//! migrated; deletion is a later cleanup phase of the same one-shot change.
//! **They must not gain a new caller, a new symbol, or a new field.** The
//! superseding module is named in each new module's header comment.
//!
//! ⚠ That prohibition is a review rule, not a mechanism, and the obvious
//! mechanism is unavailable: `#[deprecated]` on either module would fail the
//! build outright, because `umd/src/lib.rs` and `umd12/src/lib.rs` both carry
//! `#![deny(deprecated)]` and there are live callers today. So the guard cannot
//! be tightened before those callers move — the deletion and the migration are
//! one step, and §17.8's atomic activation is not satisfied until both land.
//!
//! References:
//!   - `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` (this repo) — normative for
//!     every record in [`wddm`], [`translation_session`], [`native_render`],
//!     [`physical_memory`], and [`diagnostics`]
//!   - TRANSPORT.md (this repo) — virtio-gpu layouts
//!   - VirtIO 1.2 spec §5.7 (GPU Device):
//!     <https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html#sec-gpu>

#![no_std]
#![allow(non_camel_case_types, non_upper_case_globals)]

pub mod diagnostics;
pub mod escape;
pub mod features;
pub mod ioctl;
pub mod native_render;
pub mod physical_memory;
pub mod translation_session;
pub mod virtio_gpu;
pub mod wddm;
pub mod wddm_legacy;

pub use diagnostics::*;
pub use escape::*;
pub use features::*;
pub use ioctl::*;
pub use native_render::*;
pub use physical_memory::*;
pub use translation_session::*;
pub use virtio_gpu::*;
pub use wddm::*;
pub use wddm_legacy::*;

// ── The atomic package generation ───────────────────────────────────────────
//
// HELIOS_PRESENT_SYNC_RETIREMENT.md section 17.1, final bullet: "Add one
// protocol/package generation constant shared by protocol, Mesa, UMD11, UMD12,
// KMD, QEMU, and installer. Generation mismatch is fatal."
//
// This is that constant, and this is its only definition in the guest tree. All
// three C mirrors listed in the module header carry a `_Static_assert`-guarded
// copy under the same name (`helios_wddm.h` and `helios_diagnostics.h` guard
// theirs with `#ifndef` so both may be included in one translation unit), and
// the value must be changed in all four places together.

/// The ASCII tag `'HELI'` occupying the high 32 bits of
/// [`HELIOS_PACKAGE_GENERATION`].
///
/// The tag is constant across generations on purpose: it makes a mis-decoded or
/// uninitialized generation field obvious in a hex dump (a real generation
/// always reads `48 45 4C 49 ..`), and it guarantees the constant can never be
/// zero, `u32::MAX`, or a small ordinal that some other field might plausibly
/// hold.
pub const HELIOS_PACKAGE_GENERATION_TAG: u32 = 0x4845_4C49;

/// The monotonically increasing ordinal in the low 32 bits of
/// [`HELIOS_PACKAGE_GENERATION`].
///
/// `1` is the HPS2-retirement generation: the first generation in which
/// `helios_present_sync_v2.bin` is neither published nor read, HWA2/HOB1/HOS1/
/// HOC1/HQA1/HTS1/HVC1/HNR2/HVM1/HVR1/HPM1 are the complete guest ABI, and the
/// `escape`/`ioctl` verbs are retired. Bump it for **any** change to any record
/// in this crate, including a field that only widens a reserved region.
pub const HELIOS_PACKAGE_GENERATION_ORDINAL: u32 = 1;

/// The exact atomic package generation.
///
/// Every record in this crate that carries a `package generation` field
/// (HWA2 offset 8, HOB1 offset 8, HOS1 offset 8, HOC1, HQA1 offset 8, HTS1
/// INIT/reply, HVC1 offset 8, HNR2 offset 8, HVM1, HVR1, HPM1, and the
/// section-12.3 ETW payload offset 0) must carry exactly this value.
///
/// # Mismatch is fatal, everywhere, with no fallback
///
/// Section 10.2's admission table makes "layer, translators, ICD, UMDs, KMD,
/// protocol, host all match" a conjunctive gate whose purpose is to "prevent
/// mixed HPS2/new binaries", and section 17.8 step 5 requires that "before KMD
/// admits any allocation or command, the host and KMD mutually exchange and
/// compare the exact protocol/package generation and required feature bitmap.
/// Either mismatch fails adapter start without accepting work." Concretely:
///
///   - **KMD**: a mismatched generation in any private-data record fails the
///     owning DDI with a documented error and increments the owning counter; a
///     mismatch against the host at adapter start fails `StartDevice`.
///   - **UMD11 / UMD12 / ICD**: a mismatch fails device creation before any
///     context, allocation, or resource is exposed.
///   - **QEMU**: a mismatch fails the HPM1 negotiation and the device never
///     accepts a paging or batch transaction.
///   - **Installer**: a mismatch across the staged KMD/UMDs/ICD/translators/WSI
///     layer/manifests/host attestation fails activation (section 17.8 step 6).
///
/// No component may downgrade, translate, or tolerate a foreign generation, and
/// none may treat `0` as a wildcard — see [`check_package_generation`].
pub const HELIOS_PACKAGE_GENERATION: u64 =
    ((HELIOS_PACKAGE_GENERATION_TAG as u64) << 32) | (HELIOS_PACKAGE_GENERATION_ORDINAL as u64);

const _: () = {
    // A zero generation is the "uninitialized buffer" reading every validator in
    // this crate rejects; the live constant may never collide with it.
    assert!(HELIOS_PACKAGE_GENERATION != 0);
    // Nor with the all-ones sentinel that `D3DDDI_ID_UNINITIALIZED`-style fields
    // use, so a memset(0xFF) buffer cannot pass either.
    assert!(HELIOS_PACKAGE_GENERATION != u64::MAX);
    assert!((HELIOS_PACKAGE_GENERATION >> 32) as u32 == HELIOS_PACKAGE_GENERATION_TAG);
    assert!((HELIOS_PACKAGE_GENERATION & 0xFFFF_FFFF) as u32 == HELIOS_PACKAGE_GENERATION_ORDINAL);
    // The tag is ASCII 'H','E','L','I' read big-endian; pin the bytes so a
    // careless edit to the literal cannot silently keep the assertions above.
    assert!(HELIOS_PACKAGE_GENERATION_TAG.to_be_bytes()[0] == b'H');
    assert!(HELIOS_PACKAGE_GENERATION_TAG.to_be_bytes()[1] == b'E');
    assert!(HELIOS_PACKAGE_GENERATION_TAG.to_be_bytes()[2] == b'L');
    assert!(HELIOS_PACKAGE_GENERATION_TAG.to_be_bytes()[3] == b'I');
    assert!(HELIOS_PACKAGE_GENERATION_ORDINAL != 0);
};

/// Compare a wire-supplied package generation against [`HELIOS_PACKAGE_GENERATION`].
///
/// This is the one comparison every module in the crate is expected to make when
/// it has no reason to accept a caller-supplied expectation instead. It reuses
/// [`translation_session::check_generation_match`] so the crate has exactly one
/// generation-comparison rule: zero is never a wildcard on either side, and a
/// difference is a hard refusal with both values named.
///
/// Validators in [`wddm`], [`native_render`], [`physical_memory`],
/// [`translation_session`], and [`diagnostics`] deliberately take the expected
/// generation as a *parameter* rather than reading this constant directly, so
/// that the KMD can pin one generation for an adapter's lifetime and a unit test
/// can exercise a mismatch. The parameter's production value is this constant.
#[inline]
pub fn check_package_generation(found: u64) -> Result<(), translation_session::GenerationRefusal> {
    translation_session::check_generation_match(found, HELIOS_PACKAGE_GENERATION)
}

#[cfg(test)]
mod package_generation_tests {
    use super::*;

    #[test]
    fn live_generation_is_accepted_and_zero_is_not_a_wildcard() {
        assert_eq!(check_package_generation(HELIOS_PACKAGE_GENERATION), Ok(()));
        assert_eq!(
            check_package_generation(0),
            Err(translation_session::GenerationRefusal::Zero)
        );
        assert_eq!(
            check_package_generation(HELIOS_PACKAGE_GENERATION + 1),
            Err(translation_session::GenerationRefusal::Mismatch {
                found: HELIOS_PACKAGE_GENERATION + 1,
                expected: HELIOS_PACKAGE_GENERATION,
            })
        );
    }

    /// The two C mirrors hand-copy this value. Pin the exact literal here so a
    /// change to the Rust side without the matching header edit is caught by a
    /// failing test naming the headers, not by a runtime generation mismatch on
    /// a deployed VM.
    #[test]
    fn c_mirrors_carry_this_exact_value() {
        // protocol/include/helios_wddm.h: HELIOS_PACKAGE_GENERATION
        // protocol/include/helios_diagnostics.h: HELIOS_PACKAGE_GENERATION
        // qemu-helios/include/hw/virtio/helios_physical_memory.h: HELIOS_PACKAGE_GENERATION
        assert_eq!(HELIOS_PACKAGE_GENERATION, 0x4845_4C49_0000_0001);
    }
}
