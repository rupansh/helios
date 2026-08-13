# WDDM 3.2 `DRIVER_INITIALIZATION_DATA` slot-and-cap audit

GENERATED — do not edit by hand. Regenerate with
`python3 kmd_render/tools/gen_wddm32_slot_audit.py`; edit the
classification in `kmd_render/tools/wddm32_slot_classes.tsv`.

Audited header: `kmd_render/tools/wdk-28000/km/dispmprt.h`

Slots: **192** (plus `Version`), struct size **1544** bytes.

* `Implemented` — 92
* `Disabled` — 100 (unreachable behind a truthful zero capability)
* `Pending` — 0 (pre-D9 only; D9 requires zero)
* `Retiring` — 0 (pre-D9 only; D9 requires zero)

The machine-checked half of this table lives in
`kmd_render/src/ddi/wddm32_slot_audit.rs`: compile-time `offset_of!`/
`size_of!` assertions plus a `DriverEntry` walk of the built table that
refuses to load if any slot disagrees with its class here.

| # | Offset | Slot | Since | Class | Reason |
|--:|-------:|------|-------|-------|--------|
| 0 | 8 | `DxgkDdiAddDevice` | BASE | Implemented | PnP: adapter object creation. |
| 1 | 16 | `DxgkDdiStartDevice` | BASE | Implemented | PnP: transport bring-up, segment/placement init. |
| 2 | 24 | `DxgkDdiStopDevice` | BASE | Implemented | PnP: drain and tear down the transport. |
| 3 | 32 | `DxgkDdiRemoveDevice` | BASE | Implemented | PnP: free the adapter object. |
| 4 | 40 | `DxgkDdiDispatchIoRequest` | BASE | Implemented | Legacy video-port IRP path; refuses every code and counts through StVrp. |
| 5 | 48 | `DxgkDdiInterruptRoutine` | BASE | Implemented | INTx ISR: read-to-clear virtio ISR status, queue the DPC. |
| 6 | 56 | `DxgkDdiDpcRoutine` | BASE | Implemented | Drain the used ring and complete WDDM submissions. |
| 7 | 64 | `DxgkDdiQueryChildRelations` | BASE | Implemented | One video-output child when the display half is active. |
| 8 | 72 | `DxgkDdiQueryChildStatus` | BASE | Implemented | Child connection status for that one target. |
| 9 | 80 | `DxgkDdiQueryDeviceDescriptor` | BASE | Implemented | EDID for the one monitor target. |
| 10 | 88 | `DxgkDdiSetPowerState` | BASE | Implemented | Power transitions for the adapter and its one child. |
| 11 | 96 | `DxgkDdiNotifyAcpiEvent` | BASE | Implemented | Platform ACPI events; services none and reports zero flags. |
| 12 | 104 | `DxgkDdiResetDevice` | BASE | Implemented | Crash-dump quiesce; deliberately programs no hardware. |
| 13 | 112 | `DxgkDdiUnload` | BASE | Implemented | Driver-wide unload; releases the cached BAR MMIO mappings. |
| 14 | 120 | `DxgkDdiQueryInterface` | BASE | Implemented | Exports no driver interface; logs the requested GUID and refuses. |
| 15 | 128 | `DxgkDdiControlEtwLogging` | BASE | Implemented | C42 provider enable bit and level (section 12.3). |
| 16 | 136 | `DxgkDdiQueryAdapterInfo` | BASE | Implemented | The whole capability surface, including NATIVE_FENCE_CAPS. |
| 17 | 144 | `DxgkDdiCreateDevice` | BASE | Implemented | Per-D3D-device state, exact hKmdProcess. |
| 18 | 152 | `DxgkDdiCreateAllocation` | BASE | Implemented | HWA2 create-time descriptor, HVM1 roles, HOC1 admission. |
| 19 | 160 | `DxgkDdiDestroyAllocation` | BASE | Implemented | Allocation teardown with generation-safe host unref. |
| 20 | 168 | `DxgkDdiDescribeAllocation` | BASE | Implemented | Primary geometry for the display path. |
| 21 | 176 | `DxgkDdiGetStandardAllocationDriverData` | BASE | Implemented | Standard (shared primary / cross-adapter) allocation shapes. |
| 22 | 184 | `DxgkDdiAcquireSwizzlingRange` | BASE | Disabled | DXGK_DRIVERCAPS::NumberOfSwizzlingRanges=0; the swizzling-range family is unreachable. |
| 23 | 192 | `DxgkDdiReleaseSwizzlingRange` | BASE | Disabled | DXGK_DRIVERCAPS::NumberOfSwizzlingRanges=0; the swizzling-range family is unreachable. |
| 24 | 200 | `DxgkDdiPatch` | BASE | Implemented | Infallible idempotent physical-capability snapshotter (section 10.7:1856). |
| 25 | 208 | `DxgkDdiSubmitCommand` | BASE | Implemented | Physical submit: epoch validation plus one fenced SUBMIT_3D. |
| 26 | 216 | `DxgkDdiPreemptCommand` | BASE | Implemented | Preemption request; reports DMA_PREEMPTED. |
| 27 | 224 | `DxgkDdiBuildPagingBuffer` | BASE | Implemented | HPM1 paging DMA encoding (section 17.6:4421). |
| 28 | 232 | `DxgkDdiSetPalette` | BASE | Disabled | No palettized mode is enumerated; every VidPn source mode is 32bpp RGB. |
| 29 | 240 | `DxgkDdiSetPointerPosition` | BASE | Implemented | Hardware pointer position. |
| 30 | 248 | `DxgkDdiSetPointerShape` | BASE | Implemented | Hardware pointer shape. |
| 31 | 256 | `DxgkDdiResetFromTimeout` | BASE | Implemented | TDR reset. |
| 32 | 264 | `DxgkDdiRestartFromTimeout` | BASE | Implemented | TDR restart. |
| 33 | 272 | `DxgkDdiEscape` | BASE | Disabled | D9 leaves the WDDM 3.2 initialization slot NULL. No replacement Escape, IOCTL, registry, mapped-page, ticket, name, or discovery channel is advertised; wider K1 source demolition remains a later all-or-nothing retirement gate. |
| 34 | 280 | `DxgkDdiCollectDbgInfo` | BASE | Implemented | OS-requested bounded debug snapshot. |
| 35 | 288 | `DxgkDdiQueryCurrentFence` | BASE | Implemented | Last completed submission fence for a node. |
| 36 | 296 | `DxgkDdiIsSupportedVidPn` | BASE | Implemented | VidPn validation. |
| 37 | 304 | `DxgkDdiRecommendFunctionalVidPn` | BASE | Implemented | VidPn recommendation. |
| 38 | 312 | `DxgkDdiEnumVidPnCofuncModality` | BASE | Implemented | Cofunctional modality enumeration. |
| 39 | 320 | `DxgkDdiSetVidPnSourceAddress` | BASE | Implemented | Classic scanout binding through validate_direct_scanout_binding. |
| 40 | 328 | `DxgkDdiSetVidPnSourceVisibility` | BASE | Implemented | Source visibility. |
| 41 | 336 | `DxgkDdiCommitVidPn` | BASE | Implemented | VidPn commit. |
| 42 | 344 | `DxgkDdiUpdateActiveVidPnPresentPath` | BASE | Implemented | Active present-path update. |
| 43 | 352 | `DxgkDdiRecommendMonitorModes` | BASE | Implemented | Monitor mode recommendation. |
| 44 | 360 | `DxgkDdiRecommendVidPnTopology` | BASE | Disabled | Optional. One source and one target, so the OS-built topology is accepted unchanged. |
| 45 | 368 | `DxgkDdiGetScanLine` | BASE | Implemented | Scan-line query. |
| 46 | 376 | `DxgkDdiStopCapture` | BASE | Disabled | No capture/VPE surface is advertised. |
| 47 | 384 | `DxgkDdiControlInterrupt` | BASE | Implemented | CRTC_VSYNC enable/disable; every other class is refused. |
| 48 | 392 | `DxgkDdiCreateOverlay` | BASE | Disabled | DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable. |
| 49 | 400 | `DxgkDdiDestroyDevice` | BASE | Implemented | Per-device teardown. |
| 50 | 408 | `DxgkDdiOpenAllocation` | BASE | Implemented | Ordinary open; reads private data only, never restamps it. |
| 51 | 416 | `DxgkDdiCloseAllocation` | BASE | Implemented | Per-device allocation close. |
| 52 | 424 | `DxgkDdiRender` | BASE | Implemented | HVC1/HNR2 fragment assembly, validation and output patch generation. |
| 53 | 432 | `DxgkDdiPresent` | BASE | Implemented | Present DMA packet construction. |
| 54 | 440 | `DxgkDdiUpdateOverlay` | BASE | Disabled | DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable. |
| 55 | 448 | `DxgkDdiFlipOverlay` | BASE | Disabled | DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable. |
| 56 | 456 | `DxgkDdiDestroyOverlay` | BASE | Disabled | DXGK_DRIVERCAPS::MaxOverlays=0; the legacy overlay family is unreachable. |
| 57 | 464 | `DxgkDdiCreateContext` | BASE | Implemented | HVC1 / HQA1 context admission (section 10.7, 10.4). |
| 58 | 472 | `DxgkDdiDestroyContext` | BASE | Implemented | Context teardown. |
| 59 | 480 | `DxgkDdiLinkDevice` | BASE | Disabled | Section 10.2 topology row: one physical node, LDA is not advertised. |
| 60 | 488 | `DxgkDdiSetDisplayPrivateDriverFormat` | BASE | Disabled | No private display format is negotiated; the host owns the scanout format. |
| 61 | 496 | `DxgkDdiDescribePageTable` | WIN7 | Disabled | Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it. |
| 62 | 504 | `DxgkDdiUpdatePageTable` | WIN7 | Disabled | Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it. |
| 63 | 512 | `DxgkDdiUpdatePageDirectory` | WIN7 | Disabled | Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it. |
| 64 | 520 | `DxgkDdiMovePageDirectory` | WIN7 | Disabled | Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it. |
| 65 | 528 | `DxgkDdiSubmitRender` | WIN7 | Disabled | Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it. |
| 66 | 536 | `DxgkDdiCreateAllocation2` | WIN7 | Disabled | Obsolete WIN7-era PVOID slot; the WDK declares no callback type for it. |
| 67 | 544 | `DxgkDdiRenderKm` | WIN7 | Implemented | Kernel-mode command buffer arm. |
| 68 | 552 | `Reserved` | WIN7 | Disabled | Reserved by the WDK; must stay NULL. |
| 69 | 560 | `DxgkDdiQueryVidPnHWCapability` | WIN7 | Implemented | Per-path hardware capability. |
| 70 | 568 | `DxgkDdiSetPowerComponentFState` | WIN8 | Disabled | No power components are declared and SupportRuntimePowerManagement is 0, so no F-state transition is requested. |
| 71 | 576 | `DxgkDdiQueryDependentEngineGroup` | WIN8 | Implemented | One node, one engine. |
| 72 | 584 | `DxgkDdiQueryEngineStatus` | WIN8 | Implemented | Per-engine progress for the TDR path. |
| 73 | 592 | `DxgkDdiResetEngine` | WIN8 | Implemented | Per-engine reset (SupportPerEngineTDR=1). |
| 74 | 600 | `DxgkDdiStopDeviceAndReleasePostDisplayOwnership` | WIN8 | Implemented | PnP stop with post-display ownership release. |
| 75 | 608 | `DxgkDdiSystemDisplayEnable` | WIN8 | Implemented | Bugcheck/system-display path. |
| 76 | 616 | `DxgkDdiSystemDisplayWrite` | WIN8 | Implemented | Bugcheck/system-display path. |
| 77 | 624 | `DxgkDdiCancelCommand` | WIN8 | Implemented | Cancel a queued packet. |
| 78 | 632 | `DxgkDdiGetChildContainerId` | WIN8 | Implemented | Mandatory once a monitor target is advertised. |
| 79 | 640 | `DxgkDdiPowerRuntimeControlRequest` | WIN8 | Implemented | Runtime power control requests. |
| 80 | 648 | `DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay` | WIN8 | Disabled | Superseded revision. Section 17.6:4465 selects only the MPO3 revision. |
| 81 | 656 | `DxgkDdiNotifySurpriseRemoval` | WIN8 | Disabled | DXGK_DRIVERCAPS::SupportSurpriseRemoval=0; the OS never notifies surprise removal. |
| 82 | 664 | `DxgkDdiGetNodeMetadata` | WDDM1_3 | Implemented | One node, GpuMmuSupported consistent with the reported surface. |
| 83 | 672 | `DxgkDdiSetPowerPState` | WDDM1_3 | Disabled | No GPU P-states are declared; the device has no programmable clock domain. |
| 84 | 680 | `DxgkDdiControlInterrupt2` | WDDM1_3 | Disabled | The driver services exactly one interrupt class (CRTC_VSYNC) and registers ControlInterrupt; revisions 2 and 3 only add classes it never raises. |
| 85 | 688 | `DxgkDdiCheckMultiPlaneOverlaySupport` | WDDM1_3 | Disabled | Superseded revision. Section 17.6:4465 selects only the MPO3 revision; MaxPlanes=1 is advertised there. |
| 86 | 696 | `DxgkDdiCalibrateGpuClock` | WDDM1_3 | Implemented | GPU clock calibration for the scheduler. |
| 87 | 704 | `DxgkDdiFormatHistoryBuffer` | WDDM1_3 | Implemented | History buffer formatting. |
| 88 | 712 | `DxgkDdiRenderGdi` | WDDM2_0 | Implemented | PENDING DELETE with the retired GDI path (K6; A.4 row 5770). Registered until the display lane confirms nothing lands here. |
| 89 | 720 | `DxgkDdiSubmitCommandVirtual` | WDDM2_0 | Implemented | D3D12 virtual submit: HOS1 validation plus nonblocking GPUVA enqueue. |
| 90 | 728 | `DxgkDdiSetRootPageTable` | WDDM2_0 | Implemented | C64 authoritative per-process root. |
| 91 | 736 | `DxgkDdiGetRootPageTableSize` | WDDM2_0 | Implemented | GpuMmu root table sizing. |
| 92 | 744 | `DxgkDdiMapCpuHostAperture` | WDDM2_0 | Implemented | COUPLED DELETE: goes with the HLM1 two-segment table (K1/K2); the current segment table still advertises SupportsCpuHostAperture, so removing the slot alone would break a live cap. |
| 93 | 752 | `DxgkDdiUnmapCpuHostAperture` | WDDM2_0 | Implemented | COUPLED DELETE: pairs with MapCpuHostAperture; see that row. |
| 94 | 760 | `DxgkDdiCheckMultiPlaneOverlaySupport2` | WDDM2_0 | Disabled | Superseded revision. Section 17.6:4465 selects only the MPO3 revision. |
| 95 | 768 | `DxgkDdiCreateProcess` | WDDM2_0 | Implemented | One bounded ProcessContext HTS1 session list (section 17.6:4336). |
| 96 | 776 | `DxgkDdiDestroyProcess` | WDDM2_0 | Implemented | Process teardown cancels all sessions. |
| 97 | 784 | `DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay2` | WDDM2_0 | Disabled | Superseded revision. Section 17.6:4465 selects only the MPO3 revision. |
| 98 | 792 | `Reserved1` | WDDM2_0 | Disabled | Reserved by the WDK; must stay NULL. |
| 99 | 800 | `Reserved2` | WDDM2_0 | Disabled | Reserved by the WDK; must stay NULL. |
| 100 | 808 | `DxgkDdiPowerRuntimeSetDeviceHandle` | WDDM2_0 | Implemented | Runtime power device handle. |
| 101 | 816 | `DxgkDdiSetStablePowerState` | WDDM2_0 | Implemented | Stable power state for profiling tools. |
| 102 | 824 | `DxgkDdiSetVideoProtectedRegion` | WDDM2_0 | Disabled | No protected-content surface is advertised (no OPM/PVP capability). |
| 103 | 832 | `DxgkDdiCheckMultiPlaneOverlaySupport3` | WDDM2_1 | Implemented | Active D3 exact-allocation one-primary validator; unsupported shapes return a reserved-clean false result. |
| 104 | 840 | `DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3` | WDDM2_1 | Implemented | Active D3 PASSIVE-retry binder over the D2 candidate and parking state; the D2 predicate derives solely from SURFACE. |
| 105 | 848 | `DxgkDdiPostMultiPlaneOverlayPresent` | WDDM2_1 | Implemented | Bounded always-success D3 diagnostic callback; no output path requests PostPresentNeeded. |
| 106 | 856 | `DxgkDdiValidateUpdateAllocationProperty` | WDDM2_1 | Implemented | Active D3 exact-allocation validation; all property mutations are rejected. |
| 107 | 864 | `DxgkDdiControlModeBehavior` | WDDM2_1 | Implemented | D3 leaves unsupported requested behaviors clear in both Satisfied and NotSatisfied, per the WDK contract. |
| 108 | 872 | `DxgkDdiUpdateMonitorLinkInfo` | WDDM2_1 | Implemented | Mandatory once a monitor target is advertised; revalidated under the 3.2 table. |
| 109 | 880 | `DxgkDdiCreateHwContext` | WDDM2_2 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero. |
| 110 | 888 | `DxgkDdiDestroyHwContext` | WDDM2_2 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero. |
| 111 | 896 | `DxgkDdiCreateHwQueue` | WDDM2_2 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero. |
| 112 | 904 | `DxgkDdiDestroyHwQueue` | WDDM2_2 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero. |
| 113 | 912 | `DxgkDdiSubmitCommandToHwQueue` | WDDM2_2 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero. |
| 114 | 920 | `DxgkDdiSwitchToHwContextList` | WDDM2_2 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; SchedulingCaps keeps every HwQueuePacketCap bit zero. |
| 115 | 928 | `DxgkDdiResetHwEngine` | WDDM2_2 | Disabled | HWS-only engine reset; per-engine TDR uses DxgkDdiResetEngine instead. |
| 116 | 936 | `DxgkDdiCreatePeriodicFrameNotification` | WDDM2_2 | Disabled | No periodic frame notification is advertised. |
| 117 | 944 | `DxgkDdiDestroyPeriodicFrameNotification` | WDDM2_2 | Disabled | No periodic frame notification is advertised. |
| 118 | 952 | `DxgkDdiSetTimingsFromVidPn` | WDDM2_2 | Disabled | Display-Core timing programming is not advertised; the host owns modeset and the guest programs no CRTC. |
| 119 | 960 | `DxgkDdiSetTargetGamma` | WDDM2_2 | Disabled | No per-target gamma/LUT hardware is exposed. |
| 120 | 968 | `DxgkDdiSetTargetContentType` | WDDM2_2 | Disabled | No per-target content-type signalling is exposed. |
| 121 | 976 | `DxgkDdiSetTargetAnalogCopyProtection` | WDDM2_2 | Disabled | No analog output and no copy-protection hardware. |
| 122 | 984 | `DxgkDdiSetTargetAdjustedColorimetry` | WDDM2_2 | Disabled | No per-target colorimetry adjustment hardware; only SDR RGB is advertised. |
| 123 | 992 | `DxgkDdiDisplayDetectControl` | WDDM2_2 | Disabled | The single child is indicated statically from the virtio-gpu config-change event; there is no detection engine to gate. |
| 124 | 1000 | `DxgkDdiQueryConnectionChange` | WDDM2_2 | Disabled | No DisplayPort topology/connection-change surface is exposed. |
| 125 | 1008 | `DxgkDdiExchangePreStartInfo` | WDDM2_2 | Implemented | Pre-start info exchange. |
| 126 | 1016 | `DxgkDdiGetMultiPlaneOverlayCaps` | WDDM2_2 | Implemented | Active D3 exact one-RGB-plane caps; all YUV, transform, scaling, and additional-plane authority remains zero. |
| 127 | 1024 | `DxgkDdiGetPostCompositionCaps` | WDDM2_2 | Implemented | Active D3 unity-only post-composition caps; no post-composition transform or scaling authority is advertised. |
| 128 | 1032 | `DxgkDdiUpdateHwContextState` | WDDM2_3 | Disabled | HWS-only; the hardware-scheduling family is unreachable. |
| 129 | 1040 | `DxgkDdiCreateProtectedSession` | WDDM2_3 | Disabled | No protected-content session surface is advertised. |
| 130 | 1048 | `DxgkDdiDestroyProtectedSession` | WDDM2_3 | Disabled | No protected-content session surface is advertised. |
| 131 | 1056 | `DxgkDdiSetSchedulingLogBuffer` | WDDM2_4 | Disabled | Scheduling log buffers are HWS-scoped; HWS is not advertised. |
| 132 | 1064 | `DxgkDdiSetupPriorityBands` | WDDM2_4 | Disabled | No priority bands are advertised. |
| 133 | 1072 | `DxgkDdiNotifyFocusPresent` | WDDM2_4 | Disabled | Focus-present notification is not requested by this driver. |
| 134 | 1080 | `DxgkDdiSetContextSchedulingProperties` | WDDM2_4 | Disabled | Context scheduling properties are not advertised. |
| 135 | 1088 | `DxgkDdiSuspendContext` | WDDM2_4 | Disabled | Context suspend/resume is HWS-scoped; HWS is not advertised. |
| 136 | 1096 | `DxgkDdiResumeContext` | WDDM2_4 | Disabled | Context suspend/resume is HWS-scoped; HWS is not advertised. |
| 137 | 1104 | `DxgkDdiSetVirtualMachineData` | WDDM2_4 | Implemented | VM data hand-off. |
| 138 | 1112 | `DxgkDdiBeginExclusiveAccess` | WDDM2_4 | Disabled | No GPU-P exclusive-access surface is advertised. |
| 139 | 1120 | `DxgkDdiEndExclusiveAccess` | WDDM2_4 | Disabled | No GPU-P exclusive-access surface is advertised. |
| 140 | 1128 | `DxgkDdiQueryDiagnosticTypesSupport` | WDDM2_4 | Disabled | The WDK 28000 WDDM 2.4 contract covers only PSR notification and SyncLock progression types. Helios supports neither category and leaves this slot NULL; black-screen collection is the independent CollectDiagnosticInfo contract. |
| 141 | 1136 | `DxgkDdiControlDiagnosticReporting` | WDDM2_4 | Disabled | The WDK 28000 WDDM 2.4 contract controls only the PSR and SyncLock diagnostic types that Helios truthfully reports unsupported, so this slot remains NULL. |
| 142 | 1144 | `DxgkDdiResumeHwEngine` | WDDM2_4 | Disabled | HWS-only engine resume; the hardware-scheduling family is unreachable. |
| 143 | 1152 | `DxgkDdiSignalMonitoredFence` | WDDM2_5 | Disabled | HWQueue-scoped monitored-fence signalling; HWS/HWQueue is not advertised, and native fences use the Core-0116 slots instead. |
| 144 | 1160 | `DxgkDdiPresentToHwQueue` | WDDM2_5 | Disabled | D9 unregisters the complete hardware-context and hardware-queue family; hardware flip queues and every HwQueuePacketCap bit remain zero. |
| 145 | 1168 | `DxgkDdiValidateSubmitCommand` | WDDM2_5 | Disabled | Not advertised; SubmitCommand validates its own packet inline. |
| 146 | 1176 | `DxgkDdiSetTargetAdjustedColorimetry2` | WDDM2_5 | Disabled | No per-target colorimetry adjustment hardware; only SDR RGB is advertised. |
| 147 | 1184 | `DxgkDdiSetTrackedWorkloadPowerLevel` | WDDM2_5 | Disabled | No tracked workloads are advertised. |
| 148 | 1192 | `DxgkDdiSaveMemoryForHotUpdate` | WDDM2_6 | Disabled | Driver hot-update is not supported by this package. |
| 149 | 1200 | `DxgkDdiRestoreMemoryForHotUpdate` | WDDM2_6 | Disabled | Driver hot-update is not supported by this package. |
| 150 | 1208 | `DxgkDdiCollectDiagnosticInfo` | WDDM2_6 | Implemented | C42 bounded PASSIVE snapshot callback with optional AddDevice adapter context and required WDDM 2.7 black-screen support; validates the WDK 28000 input before publishing strings, size, or bytes. |
| 151 | 1216 | `Reserved3` | WDDM2_6 | Disabled | Reserved by the WDK; must stay NULL. |
| 152 | 1224 | `DxgkDdiControlInterrupt3` | WDDM2_7 | Disabled | The driver services exactly one interrupt class (CRTC_VSYNC) and registers ControlInterrupt; revisions 2 and 3 only add classes it never raises. |
| 153 | 1232 | `DxgkDdiSetFlipQueueLogBuffer` | WDDM2_9 | Disabled | Section 17.6:4497 forbids the hardware-flip-queue flags; no HW flip queue is advertised. |
| 154 | 1240 | `DxgkDdiUpdateFlipQueueLog` | WDDM2_9 | Disabled | Section 17.6:4497 forbids the hardware-flip-queue flags; no HW flip queue is advertised. |
| 155 | 1248 | `DxgkDdiCancelQueuedFlips` | WDDM2_9 | Disabled | Section 17.6:4497 forbids the hardware-flip-queue flags; no HW flip queue is advertised. |
| 156 | 1256 | `DxgkDdiSetInterruptTargetPresentId` | WDDM2_9 | Disabled | Requires the HW flip queue / target PresentId interrupt, neither of which is advertised. |
| 157 | 1264 | `DxgkDdiSetAllocationBackingStore` | WDDM3_0 | Implemented | WDDM 3.1+ shared backing: pins the exact HVM1 section and imports its PFNs as a guest-memory Venus blob. |
| 158 | 1272 | `DxgkDdiCreateCpuEvent` | WDDM3_0 | Disabled | DXGK_FEATURE_KMD_SIGNAL_CPU_EVENT is not enabled. |
| 159 | 1280 | `DxgkDdiDestroyCpuEvent` | WDDM3_0 | Disabled | DXGK_FEATURE_KMD_SIGNAL_CPU_EVENT is not enabled. |
| 160 | 1288 | `DxgkDdiCancelFlips` | WDDM3_0 | Disabled | No HW flip queue is advertised, so there is no queued flip to cancel. |
| 161 | 1296 | `DxgkDdiCreateNativeFence` | WDDM3_1 | Implemented | Core-0116 native fence create (section 12.1); returns the driver global handle and validated HNF1 PDD. |
| 162 | 1304 | `DxgkDdiDestroyNativeFence` | WDDM3_1 | Implemented | Native fence destroy after the final global reference. |
| 163 | 1312 | `DxgkDdiUpdateMonitoredValues` | WDDM3_1 | Implemented | OS-driven monitored-value publication into the OS-owned storage. |
| 164 | 1320 | `DxgkDdiUpdateCurrentValuesFromCpu` | WDDM3_1 | Implemented | OS-driven CPU-side current-value publication. |
| 165 | 1328 | `DxgkDdiCreateDoorbell` | WDDM3_1 | Disabled | DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface. |
| 166 | 1336 | `DxgkDdiConnectDoorbell` | WDDM3_1 | Disabled | DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface. |
| 167 | 1344 | `DxgkDdiDisconnectDoorbell` | WDDM3_1 | Disabled | DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface. |
| 168 | 1352 | `DxgkDdiDestroyDoorbell` | WDDM3_1 | Disabled | DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface. |
| 169 | 1360 | `DxgkDdiNotifyWorkSubmission` | WDDM3_1 | Disabled | DXGK_FEATURE_USER_MODE_SUBMISSION is not enabled; there is no doorbell surface. |
| 170 | 1368 | `Reserved4` | WDDM3_1 | Disabled | Reserved by the WDK; must stay NULL. |
| 171 | 1376 | `DxgkDdiCreateMemoryBasis` | WDDM3_2 | Disabled | DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the memory-basis/dirty-tracking family is unreachable. |
| 172 | 1384 | `DxgkDdiDestroyMemoryBasis` | WDDM3_2 | Disabled | DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the memory-basis/dirty-tracking family is unreachable. |
| 173 | 1392 | `DxgkDdiStartDirtyTracking` | WDDM3_2 | Disabled | DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the dirty-tracking family is unreachable. |
| 174 | 1400 | `DxgkDdiStopDirtyTracking` | WDDM3_2 | Disabled | DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the dirty-tracking family is unreachable. |
| 175 | 1408 | `DxgkDdiQueryDirtyBitData` | WDDM3_2 | Disabled | DXGKQAITYPE_DIRTYBITTRACKINGCAPS is reported all-zero; the dirty-tracking family is unreachable. |
| 176 | 1416 | `DxgkDdiPrepareLiveMigration` | WDDM3_2 | Disabled | No GPU-P live migration is advertised. |
| 177 | 1424 | `DxgkDdiSaveImmutableMigrationData` | WDDM3_2 | Disabled | No GPU-P live migration is advertised. |
| 178 | 1432 | `DxgkDdiSaveMutableMigrationData` | WDDM3_2 | Disabled | No GPU-P live migration is advertised. |
| 179 | 1440 | `DxgkDdiEndLiveMigration` | WDDM3_2 | Disabled | No GPU-P live migration is advertised. |
| 180 | 1448 | `DxgkDdiRestoreImmutableMigrationData` | WDDM3_2 | Disabled | No GPU-P live migration is advertised. |
| 181 | 1456 | `DxgkDdiRestoreMutableMigrationData` | WDDM3_2 | Disabled | No GPU-P live migration is advertised. |
| 182 | 1464 | `DxgkDdiWriteVirtualizedInterrupt` | WDDM3_2 | Disabled | No GPU partitioning (GPU-P/SR-IOV) surface is advertised. |
| 183 | 1472 | `DxgkDdiSetVirtualGpuResources2` | WDDM3_2 | Disabled | No GPU partitioning (GPU-P/SR-IOV) surface is advertised. |
| 184 | 1480 | `DxgkDdiSetVirtualFunctionPauseState` | WDDM3_2 | Disabled | No GPU partitioning (GPU-P/SR-IOV) surface is advertised. |
| 185 | 1488 | `DxgkDdiOpenNativeFence` | WDDM3_2 | Implemented | Cross-process open; returns the driver local handle and validated HNF1 PDD. |
| 186 | 1496 | `DxgkDdiCloseNativeFence` | WDDM3_2 | Implemented | Per-process local close. |
| 187 | 1504 | `DxgkDdiSetNativeFenceLogBuffer` | WDDM3_2 | Disabled | Native-fence log buffers are HWQueue-scoped (DXGKARG_SETNATIVEFENCELOGBUFFER::hHwQueue) and DXGK_VIDSCHCAPS::OptimizedNativeFenceSignaledInterrupt=0, so dxgkrnl rescans waiters instead of reading a log. |
| 188 | 1512 | `DxgkDdiUpdateNativeFenceLogs` | WDDM3_2 | Disabled | Native-fence log buffers are HWQueue-scoped and OptimizedNativeFenceSignaledInterrupt=0; see SetNativeFenceLogBuffer. |
| 189 | 1520 | `DxgkDdiCollectDbgInfo2` | WDDM3_2 | Implemented | C42 WDDM 3.2 TDR callback validates reason, TDR enum, optional versioned payload, pointer-size coherence, alignment, and IRQL before publishing a bounded snapshot or extension output. |
| 190 | 1528 | `DxgkDdiNotifyContextPriorityChange` | WDDM3_2 | Disabled | Context priority-change notification is not requested by this driver. |
| 191 | 1536 | `DxgkDdiResetDisplayEngine` | WDDM3_2 | Disabled | There is no guest-side display engine to reset; scanout is a host SET_SCANOUT_BLOB. Display lane to revisit if the cold-DWM admission gate demands it. |
