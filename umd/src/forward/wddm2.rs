//! WDDM 2.0/2.1 ABI-specific D3D11 entry points.
//!
//! WDDM 2.0 changes the signatures of six existing table slots and appends
//! four device operations; WDDM 2.1 appends AcquireResource/ReleaseResource.
//! The older prefix handlers cannot simply be left behind a cast: the runtime
//! passes the newer descriptor layouts (including PlaneSlice/ContextType).

use super::*;

static WDDM2_REFUSAL_LOG: LogThrottle = LogThrottle::new();
static SYNC_TOKEN_TRACE: LogThrottle = LogThrottle::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wddm2Refusal {
    NullArgument,
    PlaneSlice,
    ConservativeRasterization,
    MissingD3d11_1Device,
    UnsupportedHardwareProtection,
    UnsupportedResourceLayout,
    UnsupportedShaderComment,
    MissingResource,
    MissingAssociation,
    ForeignDevice,
    StaleGeneration,
    ZeroSyncToken,
    MissingPhysicalContext,
    MissingRuntimeCallback,
    SubmitFailed,
    RuntimeCallback(i32),
}

unsafe fn refuse_void(h: Hdevice, site: &str, reason: Wddm2Refusal, hr: i32) {
    if WDDM2_REFUSAL_LOG.first_n_then_every(32, 1024).is_some() {
        log_error!("WDDM2.1 {site} refused: {reason:?} hr=0x{:08x}", hr as u32);
    }
    set_runtime_error(h, hr);
}

pub(crate) unsafe extern "C" fn flush_wddm2(
    h: Hdevice,
    _context_type: u32,
    _flush_flags: u32,
) -> ddi::BOOL {
    // Helios exposes one physical graphics endpoint for this D3D11 device.
    // Flushing it is a safe superset of any requested context-type subset.
    flush(h);
    1
}

pub(crate) unsafe extern "C" fn calc_size_srv_wddm2(
    _h: Hdevice,
    _arg: *const ddi::D3DWDDM2_0DDIARG_CREATESHADERRESOURCEVIEW,
) -> u64 {
    8
}

pub(crate) unsafe extern "C" fn create_srv_wddm2(
    h: Hdevice,
    arg: *const ddi::D3DWDDM2_0DDIARG_CREATESHADERRESOURCEVIEW,
    h_srv: ddi::D3D10DDI_HSHADERRESOURCEVIEW,
    h_rt: ddi::D3D10DDI_HRTSHADERRESOURCEVIEW,
) {
    clear_handle(h_srv);
    let Some(a) = arg.as_ref() else {
        refuse_void(
            h,
            "CreateShaderResourceView",
            Wddm2Refusal::NullArgument,
            E_INVALIDARG,
        );
        return;
    };
    let mut old = ddi::D3D11DDIARG_CREATESHADERRESOURCEVIEW::default();
    old.hDrvResource = a.hDrvResource;
    old.Format = a.Format;
    old.ResourceDimension = a.ResourceDimension;
    match a.ResourceDimension {
        RES_BUFFER => old.__bindgen_anon_1.Buffer = a.__bindgen_anon_1.Buffer,
        RES_BUFFEREX => old.__bindgen_anon_1.BufferEx = a.__bindgen_anon_1.BufferEx,
        RES_TEX1D => old.__bindgen_anon_1.Tex1D = a.__bindgen_anon_1.Tex1D,
        RES_TEX2D => {
            let tex = a.__bindgen_anon_1.Tex2D;
            if tex.PlaneSlice != 0 {
                refuse_void(
                    h,
                    "CreateShaderResourceView",
                    Wddm2Refusal::PlaneSlice,
                    E_NOTIMPL,
                );
                return;
            }
            old.__bindgen_anon_1.Tex2D = ddi::D3D10DDIARG_TEX2D_SHADERRESOURCEVIEW {
                MostDetailedMip: tex.MostDetailedMip,
                FirstArraySlice: tex.FirstArraySlice,
                MipLevels: tex.MipLevels,
                ArraySize: tex.ArraySize,
            };
        }
        RES_TEX3D => old.__bindgen_anon_1.Tex3D = a.__bindgen_anon_1.Tex3D,
        RES_TEXCUBE => old.__bindgen_anon_1.TexCube = a.__bindgen_anon_1.TexCube,
        _ => {
            refuse_void(
                h,
                "CreateShaderResourceView",
                Wddm2Refusal::MissingResource,
                E_INVALIDARG,
            );
            return;
        }
    }
    create_srv(h, &old, h_srv, h_rt);
}

pub(crate) unsafe extern "C" fn calc_size_rtv_wddm2(
    _h: Hdevice,
    _arg: *const ddi::D3DWDDM2_0DDIARG_CREATERENDERTARGETVIEW,
) -> u64 {
    8
}

pub(crate) unsafe extern "C" fn create_rtv_wddm2(
    h: Hdevice,
    arg: *const ddi::D3DWDDM2_0DDIARG_CREATERENDERTARGETVIEW,
    h_rtv: ddi::D3D10DDI_HRENDERTARGETVIEW,
    h_rt: ddi::D3D10DDI_HRTRENDERTARGETVIEW,
) {
    clear_handle(h_rtv);
    let Some(a) = arg.as_ref() else {
        refuse_void(
            h,
            "CreateRenderTargetView",
            Wddm2Refusal::NullArgument,
            E_INVALIDARG,
        );
        return;
    };
    let mut old = ddi::D3D10DDIARG_CREATERENDERTARGETVIEW::default();
    old.hDrvResource = a.hDrvResource;
    old.Format = a.Format;
    old.ResourceDimension = a.ResourceDimension;
    match a.ResourceDimension {
        RES_BUFFER | RES_BUFFEREX => old.__bindgen_anon_1.Buffer = a.__bindgen_anon_1.Buffer,
        RES_TEX1D => old.__bindgen_anon_1.Tex1D = a.__bindgen_anon_1.Tex1D,
        RES_TEX2D => {
            let tex = a.__bindgen_anon_1.Tex2D;
            if tex.PlaneSlice != 0 {
                refuse_void(
                    h,
                    "CreateRenderTargetView",
                    Wddm2Refusal::PlaneSlice,
                    E_NOTIMPL,
                );
                return;
            }
            old.__bindgen_anon_1.Tex2D = ddi::D3D10DDIARG_TEX2D_RENDERTARGETVIEW {
                MipSlice: tex.MipSlice,
                FirstArraySlice: tex.FirstArraySlice,
                ArraySize: tex.ArraySize,
            };
        }
        RES_TEX3D => old.__bindgen_anon_1.Tex3D = a.__bindgen_anon_1.Tex3D,
        RES_TEXCUBE => old.__bindgen_anon_1.TexCube = a.__bindgen_anon_1.TexCube,
        _ => {
            refuse_void(
                h,
                "CreateRenderTargetView",
                Wddm2Refusal::MissingResource,
                E_INVALIDARG,
            );
            return;
        }
    }
    create_rtv(h, &old, h_rtv, h_rt);
}

pub(crate) unsafe extern "C" fn calc_size_uav_wddm2(
    _h: Hdevice,
    _arg: *const ddi::D3DWDDM2_0DDIARG_CREATEUNORDEREDACCESSVIEW,
) -> u64 {
    8
}

pub(crate) unsafe extern "C" fn create_uav_wddm2(
    h: Hdevice,
    arg: *const ddi::D3DWDDM2_0DDIARG_CREATEUNORDEREDACCESSVIEW,
    h_uav: ddi::D3D11DDI_HUNORDEREDACCESSVIEW,
    h_rt: ddi::D3D11DDI_HRTUNORDEREDACCESSVIEW,
) {
    clear_handle(h_uav);
    let Some(a) = arg.as_ref() else {
        refuse_void(
            h,
            "CreateUnorderedAccessView",
            Wddm2Refusal::NullArgument,
            E_INVALIDARG,
        );
        return;
    };
    let mut old = ddi::D3D11DDIARG_CREATEUNORDEREDACCESSVIEW::default();
    old.hDrvResource = a.hDrvResource;
    old.Format = a.Format;
    old.ResourceDimension = a.ResourceDimension;
    match a.ResourceDimension {
        RES_BUFFER | RES_BUFFEREX => old.__bindgen_anon_1.Buffer = a.__bindgen_anon_1.Buffer,
        RES_TEX1D => old.__bindgen_anon_1.Tex1D = a.__bindgen_anon_1.Tex1D,
        RES_TEX2D => {
            let tex = a.__bindgen_anon_1.Tex2D;
            if tex.PlaneSlice != 0 {
                refuse_void(
                    h,
                    "CreateUnorderedAccessView",
                    Wddm2Refusal::PlaneSlice,
                    E_NOTIMPL,
                );
                return;
            }
            old.__bindgen_anon_1.Tex2D = ddi::D3D11DDIARG_TEX2D_UNORDEREDACCESSVIEW {
                MipSlice: tex.MipSlice,
                FirstArraySlice: tex.FirstArraySlice,
                ArraySize: tex.ArraySize,
            };
        }
        RES_TEX3D => old.__bindgen_anon_1.Tex3D = a.__bindgen_anon_1.Tex3D,
        _ => {
            refuse_void(
                h,
                "CreateUnorderedAccessView",
                Wddm2Refusal::MissingResource,
                E_INVALIDARG,
            );
            return;
        }
    }
    create_uav(h, &old, h_uav, h_rt);
}

pub(crate) unsafe extern "C" fn calc_size_raster_wddm2(
    _h: Hdevice,
    _desc: *const ddi::D3DWDDM2_0DDI_RASTERIZER_DESC,
) -> u64 {
    8
}

pub(crate) unsafe extern "C" fn create_raster_wddm2(
    h: Hdevice,
    desc: *const ddi::D3DWDDM2_0DDI_RASTERIZER_DESC,
    h_rs: ddi::D3D10DDI_HRASTERIZERSTATE,
    _h_rt: ddi::D3D10DDI_HRTRASTERIZERSTATE,
) {
    clear_handle(h_rs);
    let Some(d) = desc.as_ref() else {
        refuse_void(
            h,
            "CreateRasterizerState",
            Wddm2Refusal::NullArgument,
            E_INVALIDARG,
        );
        return;
    };
    if d.ConservativeRasterizationMode
        != ddi::D3DWDDM2_0DDI_CONSERVATIVE_RASTERIZATION_MODE_D3DWDDM2_0DDI_CONSERVATIVE_RASTERIZATION_OFF
    {
        refuse_void(h, "CreateRasterizerState", Wddm2Refusal::ConservativeRasterization, E_NOTIMPL);
        return;
    }

    let Some(device) = d3d11_device(h) else {
        return;
    };
    let Ok(device1) = device.cast::<ID3D11Device1>() else {
        refuse_void(
            h,
            "CreateRasterizerState",
            Wddm2Refusal::MissingD3d11_1Device,
            E_NOTIMPL,
        );
        return;
    };

    // The WDDM 2.x descriptor carries the D3D11.1 ForcedSampleCount field.
    // Dropping it into the D3D10 prefix changes the requested state, while
    // refusing a nonzero value makes d3d11.dll tear down an otherwise valid
    // device. DXVK implements ID3D11Device1 and consumes this field through
    // CreateRasterizerState1, so preserve it exactly at that ABI boundary.
    let rd = D3D11_RASTERIZER_DESC1 {
        FillMode: D3D11_FILL_MODE(d.FillMode),
        CullMode: D3D11_CULL_MODE(d.CullMode),
        FrontCounterClockwise: BOOL(d.FrontCounterClockwise),
        DepthBias: d.DepthBias,
        DepthBiasClamp: d.DepthBiasClamp,
        SlopeScaledDepthBias: d.SlopeScaledDepthBias,
        DepthClipEnable: BOOL(d.DepthClipEnable),
        ScissorEnable: BOOL(d.ScissorEnable),
        MultisampleEnable: BOOL(d.MultisampleEnable),
        AntialiasedLineEnable: BOOL(d.AntialiasedLineEnable),
        ForcedSampleCount: d.ForcedSampleCount,
    };
    let mut rs: Option<ID3D11RasterizerState1> = None;
    let created = device1.CreateRasterizerState1(&rd, Some(&mut rs));
    if let Err(ref e) = created {
        log_error!(
            "WDDM2.1 CreateRasterizerState1 failed: forced_samples={} {e:?}",
            d.ForcedSampleCount
        );
    }
    let base = match rs {
        Some(s) => match s.cast::<ID3D11RasterizerState>() {
            Ok(b) => Some(b),
            Err(e) => {
                log_error!("WDDM2.1 CreateRasterizerState1 base cast failed: {e:?}");
                None
            }
        },
        None => None,
    };
    finish_create(h, created, base, |s| store_com(h_rs, s));
}

pub(crate) unsafe extern "C" fn calc_size_query_wddm2(
    _h: Hdevice,
    _arg: *const ddi::D3DWDDM2_0DDIARG_CREATEQUERY,
) -> u64 {
    8
}

pub(crate) unsafe extern "C" fn create_query_wddm2(
    h: Hdevice,
    arg: *const ddi::D3DWDDM2_0DDIARG_CREATEQUERY,
    h_query: ddi::D3D10DDI_HQUERY,
    h_rt: ddi::D3D10DDI_HRTQUERY,
) {
    clear_handle(h_query);
    let Some(a) = arg.as_ref() else {
        refuse_void(h, "CreateQuery", Wddm2Refusal::NullArgument, E_INVALIDARG);
        return;
    };
    // The selected D3D11 device owns one physical graphics context; every
    // D3D11 query is therefore created on that context irrespective of the
    // runtime's subset hint.
    let old = ddi::D3D10DDIARG_CREATEQUERY {
        Query: a.Query,
        MiscFlags: a.MiscFlags,
    };
    create_query(h, &old, h_query, h_rt);
}

pub(crate) unsafe extern "C" fn set_hardware_protection_wddm2(
    h: Hdevice,
    _resource: ddi::D3D10DDI_HRESOURCE,
    _protected: ddi::BOOL,
) {
    refuse_void(
        h,
        "SetHardwareProtection",
        Wddm2Refusal::UnsupportedHardwareProtection,
        E_NOTIMPL,
    );
}

pub(crate) unsafe extern "C" fn get_resource_layout_wddm2(
    h: Hdevice,
    _resource: ddi::D3D10DDI_HRESOURCE,
    _subresource_count: u32,
    _allocations: *mut ddi::D3DKMT_HANDLE,
    _layout: *mut ddi::D3DWDDM2_0DDI_TEXTURE_LAYOUT,
    _mip_transition: *mut u32,
    _subresources: *mut ddi::D3DWDDM2_0DDI_SUBRESOURCE_LAYOUT,
) {
    refuse_void(
        h,
        "GetResourceLayout",
        Wddm2Refusal::UnsupportedResourceLayout,
        E_NOTIMPL,
    );
}

pub(crate) unsafe extern "C" fn retrieve_shader_comment_wddm2(
    _h: Hdevice,
    _shader: ddi::D3D10DDI_HSHADER,
    _buffer: *mut ddi::WCHAR,
    _characters: *mut ddi::SIZE_T,
) -> i32 {
    if WDDM2_REFUSAL_LOG.first_n_then_every(32, 1024).is_some() {
        log_error!(
            "WDDM2.1 RetrieveShaderComment refused: {:?}",
            Wddm2Refusal::UnsupportedShaderComment
        );
    }
    E_NOTIMPL
}

pub(crate) unsafe extern "C" fn set_hardware_protection_state_wddm2(
    h: Hdevice,
    _enabled: ddi::BOOL,
) {
    refuse_void(
        h,
        "SetHardwareProtectionState",
        Wddm2Refusal::UnsupportedHardwareProtection,
        E_NOTIMPL,
    );
}

/// The resource-side identity of one sync-token call. Its result is recorded,
/// never used to fail the DDI — see the call site.
unsafe fn sync_token_resource_identity(
    dev: &'static HeliosDevice,
    resource: ddi::D3D10DDI_HRESOURCE,
) -> Result<(), Wddm2Refusal> {
    let state = unsafe { resource_state(resource) }.ok_or(Wddm2Refusal::MissingResource)?;
    let identity = state
        .outer_allocation
        .ok_or(Wddm2Refusal::MissingAssociation)?;
    let device_generation = dev.outer.translator.session_generation();
    if device_generation == 0 || identity.device_generation != device_generation {
        return Err(Wddm2Refusal::ForeignDevice);
    }
    let resident = state
        .allocation
        .as_ref()
        .ok_or(Wddm2Refusal::MissingAssociation)?;
    let allocations = lock_ignore_poison(&dev.outer.allocations);
    let Some(current) = allocations
        .entries
        .iter()
        .find(|entry| entry.token == identity.token)
    else {
        return Err(Wddm2Refusal::StaleGeneration);
    };
    if current.allocation != resident.handle()
        || current.allocation_generation != identity.allocation_generation
        || current.bytes != identity.bytes
    {
        return Err(Wddm2Refusal::StaleGeneration);
    }
    Ok(())
}

unsafe fn exact_sync_token_context(
    h: Hdevice,
    resource: ddi::D3D10DDI_HRESOURCE,
    sync_token: ddi::HANDLE,
) -> Result<(&'static HeliosDevice, ddi::D3DDDICB_SYNCTOKEN), Wddm2Refusal> {
    if sync_token.is_null() {
        return Err(Wddm2Refusal::ZeroSyncToken);
    }
    let dev = helios_device(h).ok_or(Wddm2Refusal::ForeignDevice)?;
    // ⛔ ADVISORY, NOT A GATE — measured 2026-08-24 on KMD 22.22.352.0. See
    // `DdiRefusals::sync_token_identity_unverified`: refusing the DDI on these
    // checks made DWM's very first `AcquireResource` fatal and it never
    // presented a frame. The callback below consumes only the sync token and
    // this device's broadcast context.
    if let Err(reason) = unsafe { sync_token_resource_identity(dev, resource) } {
        note_ddi_refusal(&DDI_REFUSALS.sync_token_identity_unverified);
        if SYNC_TOKEN_IDENTITY_LOG.first_n_then_every(16, 4096).is_some() {
            log_error!(
                "WDDM2.1 sync token identity unverified: {reason:?} priv={:p} slot=0x{:x} token={:p}",
                resource.pDrvPrivate,
                unsafe { state::resource_slot_word(resource) },
                sync_token
            );
        }
    }
    let _context = dev
        .outer
        .context
        .as_ref()
        .ok_or(Wddm2Refusal::MissingPhysicalContext)?;
    // The pointer is consumed synchronously by the callback. The caller keeps
    // the local handle word live across that call and replaces this temporary
    // pointer immediately before invoking it.
    Ok((
        dev,
        ddi::D3DDDICB_SYNCTOKEN {
            hSyncToken: sync_token,
            BroadcastContextCount: 1,
            BroadcastContextArray: core::ptr::null(),
        },
    ))
}

unsafe fn run_sync_token(
    h: Hdevice,
    resource: ddi::D3D10DDI_HRESOURCE,
    sync_token: ddi::HANDLE,
    release: bool,
) {
    let (dev, mut arg) = match exact_sync_token_context(h, resource, sync_token) {
        Ok(value) => value,
        Err(reason) => {
            refuse_void(
                h,
                if release {
                    "ReleaseResource"
                } else {
                    "AcquireResource"
                },
                reason,
                E_INVALIDARG,
            );
            return;
        }
    };
    let Some(context) = dev.outer.context.as_ref() else {
        refuse_void(
            h,
            if release {
                "ReleaseResource"
            } else {
                "AcquireResource"
            },
            Wddm2Refusal::MissingPhysicalContext,
            E_FAIL,
        );
        return;
    };
    let context_handle = context.handle.as_ptr();
    arg.BroadcastContextArray = &context_handle;
    if SYNC_TOKEN_TRACE.first_n(96).is_some() {
        log_error!(
            "DDI {} t={} hContext={:p} res={:p} token={:p}",
            if release { "ReleaseResource" } else { "AcquireResource" },
            crate::forward::trace_us(),
            context_handle,
            resource.pDrvPrivate,
            sync_token
        );
    }
    if release && !dev.dxvk.flush_submitted() {
        refuse_void(h, "ReleaseResource", Wddm2Refusal::SubmitFailed, E_FAIL);
        return;
    }
    if dev.kt_callbacks.is_null() {
        refuse_void(
            h,
            if release {
                "ReleaseResource"
            } else {
                "AcquireResource"
            },
            Wddm2Refusal::MissingRuntimeCallback,
            E_FAIL,
        );
        return;
    }
    let callback = if release {
        (*dev.kt_callbacks).pfnReleaseResourceCb
    } else {
        (*dev.kt_callbacks).pfnAcquireResourceCb
    };
    let Some(callback) = callback else {
        refuse_void(
            h,
            if release {
                "ReleaseResource"
            } else {
                "AcquireResource"
            },
            Wddm2Refusal::MissingRuntimeCallback,
            E_FAIL,
        );
        return;
    };
    let hr = callback(dev.h_rt_device, &arg);
    if hr != 0 {
        refuse_void(
            h,
            if release {
                "ReleaseResource"
            } else {
                "AcquireResource"
            },
            Wddm2Refusal::RuntimeCallback(hr),
            hr,
        );
    }
}

pub(crate) unsafe extern "C" fn acquire_resource_wddm2_1(
    h: Hdevice,
    resource: ddi::D3D10DDI_HRESOURCE,
    sync_token: ddi::HANDLE,
) {
    run_sync_token(h, resource, sync_token, false);
}

pub(crate) unsafe extern "C" fn release_resource_wddm2_1(
    h: Hdevice,
    resource: ddi::D3D10DDI_HRESOURCE,
    sync_token: ddi::HANDLE,
) {
    run_sync_token(h, resource, sync_token, true);
}
