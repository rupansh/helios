//! helios_probe — Phase 3 IOCTL channel smoke test (guest user mode).
//!
//! Opens the Helios device interface (`GUID_DEVINTERFACE_HELIOS`) via
//! SetupDi + CreateFile and round-trips the Venus control verbs over
//! `DeviceIoControl`:
//!   IOCTL_HELIOS_CTX_CREATE(capset = VENUS)  -> expects an out_ctx_id
//!   IOCTL_HELIOS_CTX_DESTROY(ctx_id)
//!
//! A successful CTX_CREATE proves the full path EXE -> IOCTL -> KMD -> virtio-gpu
//! control queue -> host virglrenderer (the venus context is acked on the used
//! ring; the KMD only returns success when the device reports OK).
//!
//! SUBMIT_VENUS with a real Vulkan command stream is intentionally NOT exercised
//! here — that needs the Mesa-venus encoder (the Vulkan ICD, Phase 5); feeding
//! the host venus decoder hand-rolled bytes would just wedge the context.

use std::ffi::c_void;
use std::io::Write;
use std::mem::{size_of, offset_of};
use std::time::Duration;

/// println! + immediate flush (so output survives a hang / forced exit).
macro_rules! say {
    ($($a:tt)*) => {{ println!($($a)*); let _ = std::io::stdout().flush(); }};
}

use helios_protocol::{
    HeliosEscapeAllocBlob, HeliosEscapeCtxCreate, HeliosEscapeCtxDestroy, HeliosEscapeHeader,
    GUID_DEVINTERFACE_HELIOS_DATA1, GUID_DEVINTERFACE_HELIOS_DATA2, GUID_DEVINTERFACE_HELIOS_DATA3,
    GUID_DEVINTERFACE_HELIOS_DATA4, HELIOS_ESCAPE_ALLOC_BLOB, HELIOS_ESCAPE_CTX_CREATE,
    HELIOS_ESCAPE_CTX_DESTROY, IOCTL_HELIOS_ALLOC_BLOB, IOCTL_HELIOS_CTX_CREATE,
    IOCTL_HELIOS_CTX_DESTROY, VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE, VIRTIO_GPU_BLOB_MEM_HOST3D,
    VIRTIO_GPU_CAPSET_VENUS,
};

// ── Minimal Win32 FFI (no windows-sys dependency) ───────────────────────────

type Handle = *mut c_void;
type Bool = i32;
const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const GUID_DEVINTERFACE_HELIOS: Guid = Guid {
    data1: GUID_DEVINTERFACE_HELIOS_DATA1,
    data2: GUID_DEVINTERFACE_HELIOS_DATA2,
    data3: GUID_DEVINTERFACE_HELIOS_DATA3,
    data4: GUID_DEVINTERFACE_HELIOS_DATA4,
};

#[repr(C)]
struct SpDeviceInterfaceData {
    cb_size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: usize,
}

#[repr(C)]
struct SpDeviceInterfaceDetailDataW {
    cb_size: u32,
    device_path: [u16; 1], // variable-length; we over-allocate the buffer
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        hwnd_parent: *mut c_void,
        flags: u32,
    ) -> Handle;
    fn SetupDiEnumDeviceInterfaces(
        device_info_set: Handle,
        device_info_data: *const c_void,
        interface_class_guid: *const Guid,
        member_index: u32,
        device_interface_data: *mut SpDeviceInterfaceData,
    ) -> Bool;
    fn SetupDiGetDeviceInterfaceDetailW(
        device_info_set: Handle,
        device_interface_data: *const SpDeviceInterfaceData,
        device_interface_detail_data: *mut c_void,
        device_interface_detail_data_size: u32,
        required_size: *mut u32,
        device_info_data: *mut c_void,
    ) -> Bool;
    fn SetupDiDestroyDeviceInfoList(device_info_set: Handle) -> Bool;
}

extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;
    fn DeviceIoControl(
        device: Handle,
        io_control_code: u32,
        in_buffer: *const c_void,
        in_buffer_size: u32,
        out_buffer: *mut c_void,
        out_buffer_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> Bool;
    fn CloseHandle(object: Handle) -> Bool;
    fn GetLastError() -> u32;
}

/// Discover + open the Helios device interface, returning a handle.
fn open_helios() -> Result<Handle, String> {
    // SAFETY: standard SetupDi enumeration; all pointers are valid for the call.
    unsafe {
        let dev_info = SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_HELIOS,
            core::ptr::null(),
            core::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if dev_info == INVALID_HANDLE_VALUE {
            return Err(format!("SetupDiGetClassDevs failed (err {})", GetLastError()));
        }

        let mut ifd = SpDeviceInterfaceData {
            cb_size: size_of::<SpDeviceInterfaceData>() as u32,
            interface_class_guid: GUID_DEVINTERFACE_HELIOS,
            flags: 0,
            reserved: 0,
        };
        if SetupDiEnumDeviceInterfaces(
            dev_info,
            core::ptr::null(),
            &GUID_DEVINTERFACE_HELIOS,
            0,
            &mut ifd,
        ) == 0
        {
            let e = GetLastError();
            SetupDiDestroyDeviceInfoList(dev_info);
            return Err(format!(
                "no GUID_DEVINTERFACE_HELIOS instance present (SetupDiEnumDeviceInterfaces err {e})"
            ));
        }

        // First call: get the required detail buffer size.
        let mut required: u32 = 0;
        SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            &ifd,
            core::ptr::null_mut(),
            0,
            &mut required,
            core::ptr::null_mut(),
        );
        if required == 0 {
            SetupDiDestroyDeviceInfoList(dev_info);
            return Err(format!("detail size query failed (err {})", GetLastError()));
        }

        // Second call: fill the detail (cbSize is the FIXED header size, not the
        // buffer size — 8 on x64).
        let mut buf = vec![0u8; required as usize];
        let detail = buf.as_mut_ptr() as *mut SpDeviceInterfaceDetailDataW;
        (*detail).cb_size = size_of::<SpDeviceInterfaceDetailDataW>() as u32;
        if SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            &ifd,
            detail as *mut c_void,
            required,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        ) == 0
        {
            let e = GetLastError();
            SetupDiDestroyDeviceInfoList(dev_info);
            return Err(format!("SetupDiGetDeviceInterfaceDetail failed (err {e})"));
        }

        // The device path is the wide string at offset_of(device_path).
        let path_ptr =
            (buf.as_ptr() as usize + offset_of!(SpDeviceInterfaceDetailDataW, device_path))
                as *const u16;
        // Print the path for confirmation.
        let path = wide_to_string(path_ptr);
        say!("  device path: {path}");
        say!("  opening (CreateFileW) ...");

        let h = CreateFileW(
            path_ptr,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            core::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            core::ptr::null_mut(),
        );
        // Capture the error BEFORE any other Win32 call (SetupDi... clears it).
        let create_err = if h == INVALID_HANDLE_VALUE { GetLastError() } else { 0 };
        SetupDiDestroyDeviceInfoList(dev_info);
        if h == INVALID_HANDLE_VALUE {
            return Err(format!("CreateFile failed (err {create_err})"));
        }
        Ok(h)
    }
}

fn wide_to_string(p: *const u16) -> String {
    // SAFETY: `p` points at a NUL-terminated wide string from SetupAPI.
    unsafe {
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(core::slice::from_raw_parts(p, len))
    }
}

/// `IOCTL_HELIOS_CTX_CREATE` (METHOD_BUFFERED): in/out share the buffer.
fn ctx_create(h: Handle) -> Result<u32, String> {
    let sz = size_of::<HeliosEscapeCtxCreate>();
    let req = HeliosEscapeCtxCreate {
        hdr: HeliosEscapeHeader::new(HELIOS_ESCAPE_CTX_CREATE, sz as u32),
        capset_id: VIRTIO_GPU_CAPSET_VENUS,
        out_ctx_id: 0,
    };
    let mut out = req;
    let mut returned: u32 = 0;
    // SAFETY: `h` is our device handle; in/out buffers are valid for `sz` bytes.
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_HELIOS_CTX_CREATE,
            &req as *const _ as *const c_void,
            sz as u32,
            &mut out as *mut _ as *mut c_void,
            sz as u32,
            &mut returned,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "IOCTL_HELIOS_CTX_CREATE failed (err {})",
            unsafe { GetLastError() }
        ));
    }
    Ok(out.out_ctx_id)
}

/// `IOCTL_HELIOS_ALLOC_BLOB` (METHOD_BUFFERED): in/out share the buffer. Creates
/// a host-visible mappable HOST3D blob and returns its resource id.
fn alloc_blob(h: Handle, ctx_id: u32, size: u64) -> Result<u32, String> {
    let sz = size_of::<HeliosEscapeAllocBlob>();
    let req = HeliosEscapeAllocBlob {
        hdr: HeliosEscapeHeader::new(HELIOS_ESCAPE_ALLOC_BLOB, sz as u32),
        size,
        blob_flags: VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE,
        blob_mem: VIRTIO_GPU_BLOB_MEM_HOST3D,
        ctx_id,
        out_resource_id: 0,
    };
    let mut out = req;
    let mut returned: u32 = 0;
    // SAFETY: `h` is our device handle; in/out buffers are valid for `sz` bytes.
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_HELIOS_ALLOC_BLOB,
            &req as *const _ as *const c_void,
            sz as u32,
            &mut out as *mut _ as *mut c_void,
            sz as u32,
            &mut returned,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!("IOCTL_HELIOS_ALLOC_BLOB failed (err {})", unsafe {
            GetLastError()
        }));
    }
    Ok(out.out_resource_id)
}

/// `IOCTL_HELIOS_CTX_DESTROY` (METHOD_BUFFERED).
fn ctx_destroy(h: Handle, ctx_id: u32) -> Result<(), String> {
    let sz = size_of::<HeliosEscapeCtxDestroy>();
    let req = HeliosEscapeCtxDestroy {
        hdr: HeliosEscapeHeader::new(HELIOS_ESCAPE_CTX_DESTROY, sz as u32),
        ctx_id,
        padding: 0,
    };
    let mut returned: u32 = 0;
    // SAFETY: `h` is our device handle; the input buffer is valid for `sz` bytes.
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_HELIOS_CTX_DESTROY,
            &req as *const _ as *const c_void,
            sz as u32,
            core::ptr::null_mut(),
            0,
            &mut returned,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "IOCTL_HELIOS_CTX_DESTROY failed (err {})",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

fn main() {
    // Watchdog: never hang the test harness. If any blocking call wedges, dump
    // what we've printed so far and force-exit after 10s.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        eprintln!("WATCHDOG: a step blocked >10s — exiting (see last [N/4] line above).");
        let _ = std::io::stdout().flush();
        std::process::exit(99);
    });

    say!("helios_probe: Phase 3/4 IOCTL smoke test");
    say!("[1/5] opening GUID_DEVINTERFACE_HELIOS ...");
    let h = match open_helios() {
        Ok(h) => {
            say!("  OK: device opened");
            h
        }
        Err(e) => {
            eprintln!("  FAIL: {e}");
            std::process::exit(1);
        }
    };

    say!("[2/5] IOCTL_HELIOS_CTX_CREATE (capset = VENUS={VIRTIO_GPU_CAPSET_VENUS}) ...");
    let ctx_id = match ctx_create(h) {
        Ok(id) => {
            say!("  OK: host created venus context, out_ctx_id = {id}");
            id
        }
        Err(e) => {
            eprintln!("  FAIL: {e}");
            unsafe { CloseHandle(h) };
            std::process::exit(2);
        }
    };

    // 64 KiB host-visible mappable blob.
    let blob_size: u64 = 64 * 1024;
    say!("[3/5] IOCTL_HELIOS_ALLOC_BLOB (HOST3D|MAPPABLE, {blob_size} bytes) ...");
    let resource_id = match alloc_blob(h, ctx_id, blob_size) {
        Ok(rid) => {
            say!("  OK: host created blob, out_resource_id = {rid}");
            rid
        }
        Err(e) => {
            eprintln!("  FAIL: {e}");
            unsafe { CloseHandle(h) };
            std::process::exit(3);
        }
    };
    let _ = resource_id; // MAP_BLOB (Phase 4c) will map this id.

    say!("[4/5] IOCTL_HELIOS_CTX_DESTROY (ctx_id = {ctx_id}) ...");
    match ctx_destroy(h, ctx_id) {
        Ok(()) => say!("  OK: context destroyed"),
        Err(e) => {
            eprintln!("  FAIL: {e}");
            unsafe { CloseHandle(h) };
            std::process::exit(4);
        }
    }

    say!("[5/5] closing handle ...");
    unsafe { CloseHandle(h) };
    say!("PASS: IOCTL channel round-trips end-to-end (EXE -> KMD -> virtio-gpu -> host venus).");
}
