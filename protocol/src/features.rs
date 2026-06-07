//! VirtIO and virtio-gpu feature bits (TRANSPORT.md §1.3).
//!
//! The device exposes 64 feature bits split into two 32-bit selects. We model
//! them as `u64` masks; the PCI common-config code splits them into the two
//! `device_feature_select` windows when reading/writing.

/// Modern VirtIO (1.0+). REQUIRED — we only support the modern interface.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;
/// Queue reset support (VirtIO 1.2). REQUIRED per TRANSPORT.md §1.3.
pub const VIRTIO_F_RING_RESET: u64 = 1 << 40;
/// Device supports the indirect descriptor flag.
pub const VIRTIO_F_INDIRECT_DESC: u64 = 1 << 28;
/// Device supports `used`/`avail` event suppression.
pub const VIRTIO_F_EVENT_IDX: u64 = 1 << 29;

/// 3D / virgl support (Venus rides on the 3D submit path).
pub const VIRTIO_GPU_F_VIRGL: u64 = 1 << 0;
/// EDID readback. We request it for completeness (render-only ignores it).
pub const VIRTIO_GPU_F_EDID: u64 = 1 << 1;
/// Per-resource UUIDs — needed for Venus blob tracking.
pub const VIRTIO_GPU_F_RESOURCE_UUID: u64 = 1 << 2;
/// Blob resources (zero-copy guest<->host memory). REQUIRED for Venus.
pub const VIRTIO_GPU_F_RESOURCE_BLOB: u64 = 1 << 3;
/// Context init — lets us request the Venus capset on CTX_CREATE. REQUIRED.
pub const VIRTIO_GPU_F_CONTEXT_INIT: u64 = 1 << 4;

/// The set of features Helios requires from the device. Only `VIRTIO_F_VERSION_1`
/// (modern virtio) is truly mandatory for the driver to bind.
///
/// The venus features (VIRGL / RESOURCE_BLOB / CONTEXT_INIT) are NOT required here:
/// requiring them made `init` fail FEATURES_OK negotiation on a plain 2D
/// `virtio-gpu-pci` device (which offers none of them) → device Code 43, even though
/// the 2D desktop needs none of them. They are OPTIONAL below (negotiated when the
/// device is a `virtio-gpu-gl-pci` with venus/blob); the IOCTL Venus path re-checks
/// for them at the point it actually needs them.
pub const HELIOS_REQUIRED_FEATURES: u64 = VIRTIO_F_VERSION_1;

/// Features we will accept if offered but do not strictly require. Includes the
/// venus stack's features (virgl/blob/context-init) — present on a `-gl` device,
/// absent on a plain 2D device.
pub const HELIOS_OPTIONAL_FEATURES: u64 = VIRTIO_GPU_F_VIRGL
    | VIRTIO_GPU_F_RESOURCE_BLOB
    | VIRTIO_GPU_F_CONTEXT_INIT
    | VIRTIO_F_RING_RESET
    | VIRTIO_GPU_F_EDID
    | VIRTIO_GPU_F_RESOURCE_UUID;

// ── Device status bits (VirtIO spec §2.1) ──────────────────────────────────
pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
pub const VIRTIO_STATUS_NEEDS_RESET: u8 = 64;
pub const VIRTIO_STATUS_FAILED: u8 = 128;

// ── PCI vendor capability config types (TRANSPORT.md §1.2) ──────────────────
pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;
pub const VIRTIO_PCI_CAP_PCI_CFG: u8 = 5;
/// Shared-memory region capability (`virtio_pci_cap64`); its `id` byte selects a
/// shmid. virtio-drivers' `PciTransport` ignores this type, so the host-visible
/// blob window (ARCH §6) is found by a manual cap walk over the bus interface.
pub const VIRTIO_PCI_CAP_SHARED_MEMORY_CFG: u8 = 8;

/// PCI device identity for the virtio-gpu device (OVERVIEW.md / TRANSPORT.md).
pub const VIRTIO_PCI_VENDOR_ID: u16 = 0x1AF4;
pub const VIRTIO_GPU_PCI_DEVICE_ID: u16 = 0x1050;
