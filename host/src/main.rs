//! helios-host — host-side diagnostics for the Helios vGPU stack.
//!
//! Connects to a running QEMU's QMP monitor and reports the VM run state plus
//! whether the virtio-gpu device (the one the Helios KMD binds to) is present.
//!
//! Launch QEMU with a QMP socket, e.g.:
//!   -qmp tcp:127.0.0.1:55555,server,nowait
//!   -qmp unix:/tmp/helios-qmp.sock,server,nowait
//!
//! Usage:
//!   helios-host [ADDR]      ADDR = host:port (TCP) or /path/to.sock (Unix)
//!                           default: 127.0.0.1:55555

use std::error::Error;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;

use helios_protocol::{VIRTIO_GPU_PCI_DEVICE_ID, VIRTIO_PCI_VENDOR_ID};
use qapi::{qmp, Qmp};

const DEFAULT_ADDR: &str = "127.0.0.1:55555";

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_ADDR.into());
    if let Err(e) = run(&addr) {
        eprintln!("helios-host: {e}");
        std::process::exit(1);
    }
}

fn run(addr: &str) -> Result<(), Box<dyn Error>> {
    println!("Helios host diagnostics — connecting to QMP at {addr}");
    // A Unix socket address is a filesystem path; everything else is TCP.
    if addr.starts_with('/') || addr.starts_with('.') {
        diagnose(&UnixStream::connect(addr)?)
    } else {
        diagnose(&TcpStream::connect(addr)?)
    }
}

/// Run the diagnostic queries over any QMP-capable stream.
fn diagnose<S>(stream: S) -> Result<(), Box<dyn Error>>
where
    S: Read + Write + Clone,
{
    let mut qmp = Qmp::from_stream(stream);
    qmp.handshake()?;

    let version = qmp.execute(&qmp::query_version {})?;
    println!(
        "QEMU {}.{}.{} ({})",
        version.qemu.major, version.qemu.minor, version.qemu.micro, version.package,
    );

    let status = qmp.execute(&qmp::query_status {})?;
    println!("VM run state: {:?} (running={})", status.status, status.running);

    report_virtio_gpu(&mut qmp)?;
    Ok(())
}

/// Walk the PCI topology looking for the virtio-gpu device the KMD binds to
/// (VEN_1AF4 & DEV_1050).
fn report_virtio_gpu<S>(qmp: &mut Qmp<qapi::Stream<std::io::BufReader<S>, S>>) -> Result<(), Box<dyn Error>>
where
    S: Read + Write,
{
    let buses = qmp.execute(&qmp::query_pci {})?;
    let want_vendor = i64::from(VIRTIO_PCI_VENDOR_ID);
    let want_device = i64::from(VIRTIO_GPU_PCI_DEVICE_ID);

    let mut found = false;
    for bus in &buses {
        for dev in &bus.devices {
            if dev.id.vendor == want_vendor && dev.id.device == want_device {
                found = true;
                println!(
                    "virtio-gpu found: bus {} slot {} fn {} — {} (qom: {})",
                    bus.bus,
                    dev.slot,
                    dev.function,
                    dev.class_info.desc.as_deref().unwrap_or("GPU"),
                    dev.qdev_id,
                );
            }
        }
    }

    if !found {
        println!(
            "virtio-gpu (VEN_{:04X}&DEV_{:04X}) NOT present — \
             check the -device virtio-gpu-gl line in the QEMU command",
            VIRTIO_PCI_VENDOR_ID, VIRTIO_GPU_PCI_DEVICE_ID,
        );
    }
    Ok(())
}
