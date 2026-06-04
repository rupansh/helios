//! WDF glue: GUIDs, IRQL/mode constants, and Rust replicas of the WDF `*_INIT`
//! initializers.
//!
//! The WDK's `WDF_*_INIT` helpers (and `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE`,
//! `MmGetSystemAddressForMdlSafe`, ...) are `FORCEINLINE` C functions that
//! bindgen does NOT emit, so we reconstruct the ones we need here. Each sets the
//! struct's `Size` to `size_of::<T>()` (KMDF rejects a wrong `Size`) and the
//! same defaults the C macros set. WDF functions themselves are invoked at the
//! call sites via `wdk_sys::call_unsafe_wdf_function_binding!`.

use core::mem::size_of;

use wdk_sys::{
    GUID, ULONG, WDF_DRIVER_CONFIG, WDF_EXECUTION_LEVEL, WDF_FILEOBJECT_CLASS, WDF_FILEOBJECT_CONFIG,
    WDF_INTERRUPT_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_OBJECT_ATTRIBUTES, WDF_PNPPOWER_EVENT_CALLBACKS,
    WDF_SYNCHRONIZATION_SCOPE, WDF_TRI_STATE, _WDF_EXECUTION_LEVEL, _WDF_FILEOBJECT_CLASS,
    _WDF_IO_QUEUE_DISPATCH_TYPE, _WDF_SYNCHRONIZATION_SCOPE, _WDF_TRI_STATE,
    PFN_WDF_DRIVER_DEVICE_ADD, PFN_WDF_DEVICE_PREPARE_HARDWARE, PFN_WDF_DEVICE_RELEASE_HARDWARE,
    PFN_WDF_DEVICE_D0_ENTRY, PFN_WDF_DEVICE_D0_EXIT, PFN_WDF_FILE_CLEANUP,
    PFN_WDF_IO_QUEUE_IO_DEVICE_CONTROL, PFN_WDF_INTERRUPT_ISR, PFN_WDF_INTERRUPT_DPC,
    PCWDF_OBJECT_CONTEXT_TYPE_INFO,
};

/// `GUID_DEVINTERFACE_HELIOS` — the device interface the ICD opens. Built from
/// the field constants in `helios_protocol::ioctl` (single source of truth).
/// A `static` (not `const`) so `&GUID_DEVINTERFACE_HELIOS as *const GUID` has a
/// stable address for the WDF call; `GUID` is all-integer, hence `Sync`.
pub static GUID_DEVINTERFACE_HELIOS: GUID = GUID {
    Data1: helios_protocol::GUID_DEVINTERFACE_HELIOS_DATA1,
    Data2: helios_protocol::GUID_DEVINTERFACE_HELIOS_DATA2,
    Data3: helios_protocol::GUID_DEVINTERFACE_HELIOS_DATA3,
    Data4: helios_protocol::GUID_DEVINTERFACE_HELIOS_DATA4,
};

/// `GUID_BUS_INTERFACE_STANDARD` = {496B8280-6F25-11D0-BEAF-08002BE2092F}.
/// Defined in `wdmguid.h` via `DEFINE_GUID`, which bindgen does not emit, so we
/// hard-code it. Passed to `WdfFdoQueryForInterface` to obtain the PCI bus's
/// `BUS_INTERFACE_STANDARD` (GetBusData/SetBusData) for config-space access.
pub static GUID_BUS_INTERFACE_STANDARD: GUID = GUID {
    Data1: 0x496B_8280,
    Data2: 0x6F25,
    Data3: 0x11D0,
    Data4: [0xBE, 0xAF, 0x08, 0x00, 0x2B, 0xE2, 0x09, 0x2F],
};

/// `PCI_WHICHSPACE_CONFIG` — `DataType` arg to GetBusData/SetBusData for PCI
/// configuration space (vs. device-specific space).
pub const PCI_WHICHSPACE_CONFIG: ULONG = 0;

/// `KPROCESSOR_MODE` values (the enum lives in a bindgen module; spell the two
/// we use as explicit typed constants — `KPROCESSOR_MODE` is a `CCHAR`/`i8`).
pub const KERNEL_MODE: i8 = 0;
/// `UserMode` — `MmMapLockedPagesSpecifyCache` access mode for the `MAP_BLOB`
/// user-space mapping path (ioctl.rs).
pub const USER_MODE: i8 = 1;

/// `WDF_DRIVER_CONFIG_INIT(config, EvtDriverDeviceAdd)`.
pub fn driver_config(device_add: PFN_WDF_DRIVER_DEVICE_ADD) -> WDF_DRIVER_CONFIG {
    WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        EvtDriverDeviceAdd: device_add,
        ..Default::default()
    }
}

/// `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(attrs, <context>)` — sets the context
/// type info plus the InheritFromParent execution/synchronization defaults the C
/// `WDF_OBJECT_ATTRIBUTES_INIT` establishes (Default would leave them `Invalid`).
pub fn object_attributes_for_context(
    type_info: PCWDF_OBJECT_CONTEXT_TYPE_INFO,
) -> WDF_OBJECT_ATTRIBUTES {
    WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<WDF_OBJECT_ATTRIBUTES>() as ULONG,
        // PASSIVE_LEVEL callbacks: our IOCTL handler does PASSIVE-only work
        // (DmaBuffer alloc) and the synchronous virtio round-trip should not run
        // at DISPATCH.
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive as WDF_EXECUTION_LEVEL,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent
            as WDF_SYNCHRONIZATION_SCOPE,
        ContextTypeInfo: type_info,
        ..Default::default()
    }
}

/// `WDF_PNPPOWER_EVENT_CALLBACKS_INIT(callbacks)` + assignment of the four
/// lifecycle callbacks Helios services.
pub fn pnp_power_callbacks(
    prepare_hardware: PFN_WDF_DEVICE_PREPARE_HARDWARE,
    release_hardware: PFN_WDF_DEVICE_RELEASE_HARDWARE,
    d0_entry: PFN_WDF_DEVICE_D0_ENTRY,
    d0_exit: PFN_WDF_DEVICE_D0_EXIT,
) -> WDF_PNPPOWER_EVENT_CALLBACKS {
    WDF_PNPPOWER_EVENT_CALLBACKS {
        Size: size_of::<WDF_PNPPOWER_EVENT_CALLBACKS>() as ULONG,
        EvtDevicePrepareHardware: prepare_hardware,
        EvtDeviceReleaseHardware: release_hardware,
        EvtDeviceD0Entry: d0_entry,
        EvtDeviceD0Exit: d0_exit,
        ..Default::default()
    }
}

/// `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(config, WdfIoQueueDispatchParallel)`
/// with an `EvtIoDeviceControl` handler — a default, non-power-managed parallel
/// queue that auto-receives every request type (including IRP_MJ_DEVICE_CONTROL).
///
/// CRITICAL: a parallel queue MUST set `Settings.Parallel.NumberOfPresentedRequests`
/// to the number of requests WDF may present to the driver concurrently. The real
/// `WDF_IO_QUEUE_CONFIG_INIT` FORCEINLINE macro sets it to `(ULONG)-1` (unlimited)
/// for `WdfIoQueueDispatchParallel`; our hand-rolled `_INIT` replica originally
/// omitted it, so `..Default::default()` left it **0**. WDF's dispatch gate is
/// `if (DriverIoCount < NumberOfPresentedRequests) present_request()` — with the
/// field at 0 the queue *accepts* requests but presents **zero** of them, so every
/// `DeviceIoControl` pended forever (cancellable, CPU idle) and `EvtIoDeviceControl`
/// was never called. This was the Phase 3 IOCTL-dispatch blocker; toggling
/// queue type/power/routing never touched this field. Non-power-managed so it
/// dispatches regardless of device power state (the transport has its own
/// virtio_lock).
pub fn io_queue_config(io_device_control: PFN_WDF_IO_QUEUE_IO_DEVICE_CONTROL) -> WDF_IO_QUEUE_CONFIG {
    let mut cfg = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel
            as wdk_sys::WDF_IO_QUEUE_DISPATCH_TYPE,
        DefaultQueue: 1, // default queue — auto-receives all request types
        PowerManaged: _WDF_TRI_STATE::WdfFalse as WDF_TRI_STATE,
        EvtIoDeviceControl: io_device_control,
        ..Default::default()
    };
    // Unlimited concurrent presentation, matching WDF_IO_QUEUE_CONFIG_INIT's
    // `(ULONG)-1`. Writing a union field is safe; this is the fix for the blocker.
    cfg.Settings.Parallel.NumberOfPresentedRequests = ULONG::MAX;
    cfg
}

/// `WDF_FILEOBJECT_CONFIG_INIT(config, NULL create, NULL close, cleanup)`.
///
/// Registers an `EvtFileCleanup` callback so the KMD can unmap host-visible blob
/// mappings in the closing process's context (Phase 4c teardown, ioctl.rs). We
/// need no FsContext storage (mappings are tracked in the AdapterContext mapping
/// table), so `FileObjectClass = WdfFileObjectWdfCannotUseFsContexts` and no file
/// object context is attached. `AutoForwardCleanupClose = WdfUseDefault` keeps the
/// framework's default Create/Cleanup/Close IRP forwarding. All fields are set
/// explicitly (no `..Default::default()`) so this does not depend on whether
/// bindgen derived `Default` for `WDF_FILEOBJECT_CONFIG`.
pub fn fileobject_config(cleanup: PFN_WDF_FILE_CLEANUP) -> WDF_FILEOBJECT_CONFIG {
    WDF_FILEOBJECT_CONFIG {
        Size: size_of::<WDF_FILEOBJECT_CONFIG>() as ULONG,
        EvtDeviceFileCreate: None,
        EvtFileClose: None,
        EvtFileCleanup: cleanup,
        AutoForwardCleanupClose: _WDF_TRI_STATE::WdfUseDefault as WDF_TRI_STATE,
        FileObjectClass: _WDF_FILEOBJECT_CLASS::WdfFileObjectWdfCannotUseFsContexts
            as WDF_FILEOBJECT_CLASS,
    }
}

/// `WDF_INTERRUPT_CONFIG_INIT(config, isr, dpc)`.
pub fn interrupt_config(
    isr: PFN_WDF_INTERRUPT_ISR,
    dpc: PFN_WDF_INTERRUPT_DPC,
) -> WDF_INTERRUPT_CONFIG {
    WDF_INTERRUPT_CONFIG {
        Size: size_of::<WDF_INTERRUPT_CONFIG>() as ULONG,
        EvtInterruptIsr: isr,
        EvtInterruptDpc: dpc,
        ..Default::default()
    }
}
