//! `virtio_drivers::transport::pci::bus::ConfigurationAccess` backed by the PCI
//! bus driver's `BUS_INTERFACE_STANDARD` (GetBusData/SetBusData).
//!
//! A System-class KMDF function driver does not own the PCI bus, so it reaches
//! its own device's config space through the `BUS_INTERFACE_STANDARD` interface
//! obtained via `WdfFdoQueryForInterface(GUID_BUS_INTERFACE_STANDARD)` (see
//! pnp.rs). That interface is already scoped to our device (`Context`), so the
//! `DeviceFunction` argument from virtio-drivers is ignored — we always access
//! our own device. `GetBusData`/`SetBusData` are callable up to DISPATCH_LEVEL.

use core::ffi::c_void;

use virtio_drivers::transport::pci::bus::{ConfigurationAccess, DeviceFunction};
use wdk_sys::{BUS_INTERFACE_STANDARD, PGET_SET_DEVICE_DATA, PVOID, ULONG};

use crate::wdf::PCI_WHICHSPACE_CONFIG;

/// PCI config-space accessor over the bus interface. `Copy` (a context pointer +
/// two callback pointers) so `unsafe_clone` and `PciRoot`'s cloning are trivial.
#[derive(Clone, Copy)]
pub struct KmdfConfigAccess {
    /// Opaque per-device context the bus driver gave us; first arg to the calls.
    context: PVOID,
    get_bus_data: PGET_SET_DEVICE_DATA,
    set_bus_data: PGET_SET_DEVICE_DATA,
}

// SAFETY: the context + callback pointers are valid for as long as the bus
// interface is referenced (we hold a reference for the device's PrepareHardware
// lifetime). The accessor is only used under the AdapterContext virtio lock.
unsafe impl Send for KmdfConfigAccess {}
unsafe impl Sync for KmdfConfigAccess {}

impl KmdfConfigAccess {
    /// Capture the context + GetBusData/SetBusData callbacks from a queried
    /// `BUS_INTERFACE_STANDARD`.
    pub fn new(bus: &BUS_INTERFACE_STANDARD) -> Self {
        Self {
            context: bus.Context,
            get_bus_data: bus.GetBusData,
            set_bus_data: bus.SetBusData,
        }
    }

    /// Read a 32-bit dword from our device's PCI config space at byte `offset`.
    /// `offset` is a `u16` (not `u8`) so the host-visible cap walk can index the
    /// full 256-byte config window without `u8` add-overflow on `cap + 20`.
    pub fn read32(&self, offset: u16) -> u32 {
        let mut val: u32 = 0;
        if let Some(get) = self.get_bus_data {
            // SAFETY: reads 4 bytes of our device's PCI config space at `offset`;
            // `val` is a valid 4-byte buffer. The bus driver returns the number
            // of bytes read (ignored — a short read leaves the remaining bytes 0,
            // which the cap walk treats as "no capability").
            unsafe {
                get(
                    self.context,
                    PCI_WHICHSPACE_CONFIG,
                    (&mut val as *mut u32).cast::<c_void>(),
                    offset as ULONG,
                    4,
                );
            }
        }
        val
    }
}

impl ConfigurationAccess for KmdfConfigAccess {
    fn read_word(&self, _device_function: DeviceFunction, register_offset: u8) -> u32 {
        self.read32(register_offset as u16)
    }

    fn write_word(&mut self, _device_function: DeviceFunction, register_offset: u8, data: u32) {
        let mut data = data;
        if let Some(set) = self.set_bus_data {
            // SAFETY: writes 4 bytes to our device's PCI config space.
            unsafe {
                set(
                    self.context,
                    PCI_WHICHSPACE_CONFIG,
                    (&mut data as *mut u32).cast::<c_void>(),
                    register_offset as ULONG,
                    4,
                );
            }
        }
    }

    unsafe fn unsafe_clone(&self) -> Self {
        *self
    }
}
