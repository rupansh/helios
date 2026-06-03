//! helios_protocol — shared wire-format definitions for the Helios vGPU stack.
//!
//! This crate is the single source of truth for every byte that crosses a
//! trust/ABI boundary in Helios:
//!
//!   ICD (user-mode, std)  --D3DKMTEscape-->  KMD (kernel-mode, no_std)
//!   KMD                    --virtqueue-->     virtio-gpu device / virglrenderer
//!
//! Both the `helios_kmd` and `helios_icd` crates depend on this crate so the
//! escape structs and virtio-gpu command structs can never drift apart. It is
//! `#![no_std]` so the kernel-mode KMD can use it; std crates can depend on a
//! no_std crate freely.
//!
//! References:
//!   - TRANSPORT.md (this repo) — escape protocol + virtio-gpu layouts
//!   - KMD.md Phase 2 — virtio-gpu command structs
//!   - VirtIO 1.2 spec §5.7 (GPU Device):
//!     https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html#sec-gpu

#![no_std]
#![allow(non_camel_case_types, non_upper_case_globals)]

pub mod virtio_gpu;
pub mod escape;
pub mod features;

pub use escape::*;
pub use features::*;
pub use virtio_gpu::*;
