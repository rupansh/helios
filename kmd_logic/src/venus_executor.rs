//! Generated-Venus command admission for the bounded HNR2 executor.
//!
//! This is intentionally not a general Venus decoder.  It recognizes only the
//! command and typed-resource subset needed by Mesa A3/A4, consumes every byte,
//! and reports the exact resource operands the KMD must cross-check against the
//! HNR2 typed-patch table.  The numeric constants are checked against Mesa's
//! pinned generated headers by `tools/venus-executor-schema-gate.py`.

use helios_protocol::native_render::HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32;

mod a7_schema;

pub use a7_schema::{A7CommandKind, SchemaScratch};

// Generated command opcodes (`vn_protocol_driver_defines.h`).
pub const OP_QUEUE_SUBMIT: u32 = 18;
pub const OP_ALLOCATE_MEMORY: u32 = 21;
pub const OP_FREE_MEMORY: u32 = 22;
pub const OP_QUEUE_BIND_SPARSE: u32 = 34;
const OP_CREATE_BUFFER: u32 = 50;
const OP_DESTROY_BUFFER: u32 = 51;
const OP_CREATE_IMAGE: u32 = 54;
const OP_DESTROY_IMAGE: u32 = 55;
const OP_BIND_BUFFER_MEMORY2: u32 = 138;
const OP_BIND_IMAGE_MEMORY2: u32 = 139;
const OP_GET_IMAGE_MEMORY_REQUIREMENTS2: u32 = 144;
const OP_GET_BUFFER_MEMORY_REQUIREMENTS2: u32 = 145;
pub const OP_SET_REPLY: u32 = 178;
pub const OP_QUEUE_SUBMIT2: u32 = 206;

// Generated structure tags (`vulkan_core.h` and the Venus generated define).
const ST_SUBMIT_INFO: u32 = 4;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_BIND_SPARSE_INFO: u32 = 7;
const ST_MEMORY_ALLOCATE_FLAGS_INFO: u32 = 1_000_060_000;
const ST_DEVICE_GROUP_SUBMIT_INFO: u32 = 1_000_060_005;
const ST_DEVICE_GROUP_BIND_SPARSE_INFO: u32 = 1_000_060_006;
const ST_EXPORT_MEMORY_ALLOCATE_INFO: u32 = 1_000_072_002;
const ST_MEMORY_DEDICATED_ALLOCATE_INFO: u32 = 1_000_127_001;
const ST_PROTECTED_SUBMIT_INFO: u32 = 1_000_145_000;
const ST_TIMELINE_SEMAPHORE_SUBMIT_INFO: u32 = 1_000_207_003;
const ST_MEMORY_OPAQUE_CAPTURE_ADDRESS_ALLOCATE_INFO: u32 = 1_000_257_003;
const ST_SUBMIT_INFO2: u32 = 1_000_314_004;
const ST_SEMAPHORE_SUBMIT_INFO: u32 = 1_000_314_005;
const ST_COMMAND_BUFFER_SUBMIT_INFO: u32 = 1_000_314_006;
const ST_IMPORT_MEMORY_RESOURCE_INFO_MESA: u32 = 1_000_384_002;

const COMMAND_GENERATE_REPLY: u32 = 1;
const MAX_NESTING: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenusCommandClass {
    AllocateMemory,
    FreeMemory,
    QueueSubmit,
    QueueSubmit2,
    QueueBindSparse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VenusOperand {
    pub payload_offset: u32,
    pub operand_kind: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VenusAdmission {
    pub class: VenusCommandClass,
    pub operand_count: u32,
    /// Exact raw Venus reply range encoded by SetReply. Zero for reply-less
    /// classes; the KMD cross-checks these against HNR2 before patching.
    pub reply_offset: u64,
    pub reply_size: u64,
    /// Exact `VkMemoryAllocateInfo` fields for the sole allocation class.
    /// Zero for every other class.
    pub allocation_size: u64,
    pub memory_type_index: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VenusA7Admission {
    pub command_count: u32,
    pub allocation_command_count: u32,
    /// Object-materialization commands (view create/destroy, descriptor-set
    /// update): dereference an outer allocation on the host, so the ICD
    /// defers them out of the unordered control lane into the batch, after
    /// the allocate/bind region and before the recordings.
    pub object_command_count: u32,
    pub command_buffer_count: u32,
    pub queue_command_count: u32,
    pub operand_count: u32,
    /// An exact allocation-lifetime terminal: zero or more object destroys
    /// followed by one FreeMemory, after the owning queue progress was joined.
    /// It contains no queue command and must carry one outer allocation use.
    pub terminal_teardown: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VenusControlAdmission {
    pub command_count: u32,
    pub opcode: u32,
    pub operand_count: u32,
    pub reply_offset: u64,
    pub reply_size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenusReject {
    Truncated,
    TrailingBytes,
    UnknownOpcode,
    BadFlags,
    BadPointer,
    BadArrayCount,
    BadStructureType,
    UnsupportedChain,
    MissingImport,
    DuplicateImport,
    NonZeroResourceOperand,
    OperandCapacity,
    CountOverflow,
    InvalidSequence,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn offset_u32(&self) -> Result<u32, VenusReject> {
        u32::try_from(self.offset).map_err(|_| VenusReject::CountOverflow)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], VenusReject> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or(VenusReject::CountOverflow)?;
        let out = self
            .bytes
            .get(self.offset..end)
            .ok_or(VenusReject::Truncated)?;
        self.offset = end;
        Ok(out)
    }

    fn array_count(&mut self) -> Result<u64, VenusReject> {
        self.u64()
    }

    fn bound_loop_count(&self, count: u64) -> Result<(), VenusReject> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        if count > (remaining / 4).saturating_add(1) as u64 {
            return Err(VenusReject::BadArrayCount);
        }
        Ok(())
    }

    fn skip_array(&mut self, count: u64, width: usize) -> Result<(), VenusReject> {
        let count = usize::try_from(count).map_err(|_| VenusReject::CountOverflow)?;
        let bytes = count.checked_mul(width).ok_or(VenusReject::CountOverflow)?;
        let padded = if width < 4 {
            bytes.checked_add(3).ok_or(VenusReject::CountOverflow)? & !3
        } else {
            bytes
        };
        self.skip(padded)
    }

    fn take_padded(&mut self, count: u64) -> Result<&'a [u8], VenusReject> {
        let bytes = usize::try_from(count).map_err(|_| VenusReject::CountOverflow)?;
        let padded = bytes.checked_add(3).ok_or(VenusReject::CountOverflow)? & !3;
        let out = self.take(padded)?;
        Ok(&out[..bytes])
    }

    fn u32(&mut self) -> Result<u32, VenusReject> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, VenusReject> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn skip(&mut self, n: usize) -> Result<(), VenusReject> {
        self.take(n).map(|_| ())
    }

    fn pointer(&mut self) -> Result<bool, VenusReject> {
        match self.u64()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(VenusReject::BadPointer),
        }
    }

    fn array(&mut self, expected: u32) -> Result<bool, VenusReject> {
        match self.u64()? {
            0 if expected == 0 => Ok(false),
            0 => Ok(false),
            n if n == expected as u64 => Ok(true),
            _ => Err(VenusReject::BadArrayCount),
        }
    }

    fn repeat_skip(&mut self, count: u32, stride: usize) -> Result<(), VenusReject> {
        let bytes = (count as usize)
            .checked_mul(stride)
            .ok_or(VenusReject::CountOverflow)?;
        self.skip(bytes)
    }

    fn bounded_count(&mut self) -> Result<u32, VenusReject> {
        let count = self.u32()?;
        // Every generated array element consumes at least four bytes.  This
        // precludes attacker-controlled billion-iteration loops even before a
        // shape-specific stride check runs.
        if count as usize > self.bytes.len().saturating_sub(self.offset) / 4 + 1 {
            return Err(VenusReject::BadArrayCount);
        }
        Ok(count)
    }
}

struct OperandWriter<'a> {
    out: &'a mut [VenusOperand],
    count: usize,
}

impl OperandWriter<'_> {
    fn push(&mut self, offset: u32, kind: u16) -> Result<(), VenusReject> {
        let slot = self
            .out
            .get_mut(self.count)
            .ok_or(VenusReject::OperandCapacity)?;
        *slot = VenusOperand {
            payload_offset: offset,
            operand_kind: kind,
        };
        self.count += 1;
        Ok(())
    }

    fn push_resource(&mut self, offset: u32, width: usize) -> Result<(), VenusReject> {
        match width {
            4 => self.push(offset, HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32),
            _ => Err(VenusReject::OperandCapacity),
        }
    }
}

fn command_header(c: &mut Cursor<'_>) -> Result<(u32, u32), VenusReject> {
    Ok((c.u32()?, c.u32()?))
}

fn expect_stype(c: &mut Cursor<'_>, expected: u32) -> Result<(), VenusReject> {
    if c.u32()? != expected {
        return Err(VenusReject::BadStructureType);
    }
    Ok(())
}

fn parse_leaf_pnext(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    if c.pointer()? {
        return Err(VenusReject::UnsupportedChain);
    }
    Ok(())
}

fn parse_timeline_body(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    let waits = c.bounded_count()?;
    if c.array(waits)? {
        c.repeat_skip(waits, 8)?;
    } else if waits != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    let signals = c.bounded_count()?;
    if c.array(signals)? {
        c.repeat_skip(signals, 8)?;
    } else if signals != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    Ok(())
}

fn parse_submit_pnext(c: &mut Cursor<'_>, depth: u32) -> Result<(), VenusReject> {
    if depth >= MAX_NESTING {
        return Err(VenusReject::UnsupportedChain);
    }
    if !c.pointer()? {
        return Ok(());
    }
    match c.u32()? {
        ST_DEVICE_GROUP_SUBMIT_INFO => {
            parse_submit_pnext(c, depth + 1)?;
            for stride in [4usize, 4, 4] {
                let count = c.bounded_count()?;
                if c.array(count)? {
                    c.repeat_skip(count, stride)?;
                } else if count != 0 {
                    return Err(VenusReject::BadArrayCount);
                }
            }
            Ok(())
        }
        ST_PROTECTED_SUBMIT_INFO => {
            parse_submit_pnext(c, depth + 1)?;
            c.skip(4)
        }
        ST_TIMELINE_SEMAPHORE_SUBMIT_INFO => {
            parse_submit_pnext(c, depth + 1)?;
            parse_timeline_body(c)
        }
        _ => Err(VenusReject::UnsupportedChain),
    }
}

fn parse_submit_info(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    expect_stype(c, ST_SUBMIT_INFO)?;
    parse_submit_pnext(c, 0)?;

    let waits = c.bounded_count()?;
    if c.array(waits)? {
        c.repeat_skip(waits, 8)?;
    } else if waits != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    if c.array(waits)? {
        c.repeat_skip(waits, 4)?;
    } else if waits != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let commands = c.bounded_count()?;
    if c.array(commands)? {
        c.repeat_skip(commands, 8)?;
    } else if commands != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let signals = c.bounded_count()?;
    if c.array(signals)? {
        c.repeat_skip(signals, 8)?;
    } else if signals != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    Ok(())
}

fn parse_semaphore_submit_info(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    expect_stype(c, ST_SEMAPHORE_SUBMIT_INFO)?;
    parse_leaf_pnext(c)?;
    c.skip(8 + 8 + 8 + 4)
}

fn parse_command_buffer_submit_info(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    expect_stype(c, ST_COMMAND_BUFFER_SUBMIT_INFO)?;
    parse_leaf_pnext(c)?;
    c.skip(8 + 4)
}

fn parse_submit_info2(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    expect_stype(c, ST_SUBMIT_INFO2)?;
    parse_leaf_pnext(c)?;
    c.skip(4)?; // flags

    let waits = c.bounded_count()?;
    if c.array(waits)? {
        for _ in 0..waits {
            parse_semaphore_submit_info(c)?;
        }
    } else if waits != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let commands = c.bounded_count()?;
    if c.array(commands)? {
        for _ in 0..commands {
            parse_command_buffer_submit_info(c)?;
        }
    } else if commands != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let signals = c.bounded_count()?;
    if c.array(signals)? {
        for _ in 0..signals {
            parse_semaphore_submit_info(c)?;
        }
    } else if signals != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    Ok(())
}

fn parse_bind_pnext(c: &mut Cursor<'_>, depth: u32) -> Result<(), VenusReject> {
    if depth >= MAX_NESTING {
        return Err(VenusReject::UnsupportedChain);
    }
    if !c.pointer()? {
        return Ok(());
    }
    match c.u32()? {
        ST_DEVICE_GROUP_BIND_SPARSE_INFO => {
            parse_bind_pnext(c, depth + 1)?;
            c.skip(8)
        }
        ST_TIMELINE_SEMAPHORE_SUBMIT_INFO => {
            parse_bind_pnext(c, depth + 1)?;
            parse_timeline_body(c)
        }
        _ => Err(VenusReject::UnsupportedChain),
    }
}

fn parse_sparse_memory_binds(c: &mut Cursor<'_>, objects: u32) -> Result<(), VenusReject> {
    for _ in 0..objects {
        c.skip(8)?; // buffer/image handle
        let binds = c.bounded_count()?;
        if c.array(binds)? {
            c.repeat_skip(binds, 36)?;
        } else if binds != 0 {
            return Err(VenusReject::BadArrayCount);
        }
    }
    Ok(())
}

fn parse_sparse_image_binds(c: &mut Cursor<'_>, objects: u32) -> Result<(), VenusReject> {
    for _ in 0..objects {
        c.skip(8)?;
        let binds = c.bounded_count()?;
        if c.array(binds)? {
            c.repeat_skip(binds, 56)?;
        } else if binds != 0 {
            return Err(VenusReject::BadArrayCount);
        }
    }
    Ok(())
}

fn parse_bind_sparse_info(c: &mut Cursor<'_>) -> Result<(), VenusReject> {
    expect_stype(c, ST_BIND_SPARSE_INFO)?;
    parse_bind_pnext(c, 0)?;

    let waits = c.bounded_count()?;
    if c.array(waits)? {
        c.repeat_skip(waits, 8)?;
    } else if waits != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let buffers = c.bounded_count()?;
    if c.array(buffers)? {
        parse_sparse_memory_binds(c, buffers)?;
    } else if buffers != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let opaque_images = c.bounded_count()?;
    if c.array(opaque_images)? {
        parse_sparse_memory_binds(c, opaque_images)?;
    } else if opaque_images != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let images = c.bounded_count()?;
    if c.array(images)? {
        parse_sparse_image_binds(c, images)?;
    } else if images != 0 {
        return Err(VenusReject::BadArrayCount);
    }

    let signals = c.bounded_count()?;
    if c.array(signals)? {
        c.repeat_skip(signals, 8)?;
    } else if signals != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    Ok(())
}

fn parse_queue(c: &mut Cursor<'_>, opcode: u32) -> Result<VenusCommandClass, VenusReject> {
    c.skip(8)?; // queue
    let count = c.bounded_count()?;
    let present = c.array(count)?;
    if !present && count != 0 {
        return Err(VenusReject::BadArrayCount);
    }
    if present {
        for _ in 0..count {
            match opcode {
                OP_QUEUE_SUBMIT => parse_submit_info(c)?,
                OP_QUEUE_SUBMIT2 => parse_submit_info2(c)?,
                OP_QUEUE_BIND_SPARSE => parse_bind_sparse_info(c)?,
                _ => return Err(VenusReject::UnknownOpcode),
            }
        }
    }
    c.skip(8)?; // fence
    Ok(match opcode {
        OP_QUEUE_SUBMIT => VenusCommandClass::QueueSubmit,
        OP_QUEUE_SUBMIT2 => VenusCommandClass::QueueSubmit2,
        OP_QUEUE_BIND_SPARSE => VenusCommandClass::QueueBindSparse,
        _ => return Err(VenusReject::UnknownOpcode),
    })
}

fn parse_memory_pnext(
    c: &mut Cursor<'_>,
    operands: &mut OperandWriter<'_>,
    depth: u32,
    imports: &mut u32,
) -> Result<(), VenusReject> {
    if depth >= MAX_NESTING {
        return Err(VenusReject::UnsupportedChain);
    }
    if !c.pointer()? {
        return Ok(());
    }
    let stype = c.u32()?;
    parse_memory_pnext(c, operands, depth + 1, imports)?;
    match stype {
        ST_EXPORT_MEMORY_ALLOCATE_INFO => c.skip(4),
        ST_MEMORY_ALLOCATE_FLAGS_INFO => c.skip(8),
        ST_MEMORY_DEDICATED_ALLOCATE_INFO => c.skip(16),
        ST_MEMORY_OPAQUE_CAPTURE_ADDRESS_ALLOCATE_INFO => c.skip(8),
        ST_IMPORT_MEMORY_RESOURCE_INFO_MESA => {
            if *imports != 0 {
                return Err(VenusReject::DuplicateImport);
            }
            *imports = 1;
            let offset = c.offset_u32()?;
            if c.u32()? != 0 {
                return Err(VenusReject::NonZeroResourceOperand);
            }
            operands.push(offset, HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32)
        }
        _ => Err(VenusReject::UnsupportedChain),
    }
}

fn parse_allocate(
    c: &mut Cursor<'_>,
    operands: &mut OperandWriter<'_>,
) -> Result<(VenusCommandClass, u64, u32), VenusReject> {
    c.skip(8)?; // device
    if !c.pointer()? {
        return Err(VenusReject::BadPointer);
    }
    expect_stype(c, ST_MEMORY_ALLOCATE_INFO)?;
    let mut imports = 0;
    parse_memory_pnext(c, operands, 0, &mut imports)?;
    let allocation_size = c.u64()?;
    let memory_type_index = c.u32()?;
    if allocation_size == 0 {
        return Err(VenusReject::BadArrayCount);
    }
    if c.pointer()? {
        return Err(VenusReject::BadPointer); // pAllocator unsupported by generator
    }
    if !c.pointer()? {
        return Err(VenusReject::BadPointer);
    }
    c.skip(8)?; // output VkDeviceMemory object id
    if imports != 1 {
        return Err(VenusReject::MissingImport);
    }
    Ok((
        VenusCommandClass::AllocateMemory,
        allocation_size,
        memory_type_index,
    ))
}

/// Validate one complete A3/A4 Venus stream and return its generated resource
/// operands in payload order.  Every reported operand is required to contain
/// zero; only the caller's host-private copy may later be patched.
pub fn validate_venus_stream(
    bytes: &[u8],
    operand_storage: &mut [VenusOperand],
) -> Result<VenusAdmission, VenusReject> {
    let mut c = Cursor::new(bytes);
    let mut operands = OperandWriter {
        out: operand_storage,
        count: 0,
    };
    let (opcode, flags) = command_header(&mut c)?;
    let mut reply_offset = 0;
    let mut reply_size = 0;
    let mut allocation_size = 0;
    let mut memory_type_index = 0;
    let class = match opcode {
        OP_SET_REPLY => {
            if flags != 0 || !c.pointer()? {
                return Err(VenusReject::BadFlags);
            }
            let offset = c.offset_u32()?;
            if c.u32()? != 0 {
                return Err(VenusReject::NonZeroResourceOperand);
            }
            operands.push(offset, HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32)?;
            reply_offset = c.u64()?;
            reply_size = c.u64()?;
            if reply_size == 0 {
                return Err(VenusReject::BadArrayCount);
            }
            let (next, next_flags) = command_header(&mut c)?;
            if next != OP_ALLOCATE_MEMORY || next_flags != COMMAND_GENERATE_REPLY {
                return Err(VenusReject::BadFlags);
            }
            let parsed = parse_allocate(&mut c, &mut operands)?;
            allocation_size = parsed.1;
            memory_type_index = parsed.2;
            parsed.0
        }
        OP_FREE_MEMORY => {
            if flags != 0 {
                return Err(VenusReject::BadFlags);
            }
            c.skip(8 + 8)?;
            if c.pointer()? {
                return Err(VenusReject::BadPointer);
            }
            VenusCommandClass::FreeMemory
        }
        OP_QUEUE_SUBMIT | OP_QUEUE_SUBMIT2 | OP_QUEUE_BIND_SPARSE => {
            if flags != 0 {
                return Err(VenusReject::BadFlags);
            }
            parse_queue(&mut c, opcode)?
        }
        _ => return Err(VenusReject::UnknownOpcode),
    };
    if c.offset != bytes.len() {
        return Err(VenusReject::TrailingBytes);
    }
    Ok(VenusAdmission {
        class,
        operand_count: operands.count as u32,
        reply_offset,
        reply_size,
        allocation_size,
        memory_type_index,
    })
}

/// Validate one complete record-only outer stream.
///
/// Unlike the legacy HNR2 helper above, an A7 outer operation is deliberately
/// a sequence: zero or more allocation materialization/bind commands, complete
/// `BeginCommandBuffer .. EndCommandBuffer` recordings, and exactly one final
/// queue operation.  Every command is walked by the generated package schema;
/// no trailing byte can hide an unclassified opcode.
pub fn validate_venus_a7_outer_stream(
    bytes: &[u8],
    operand_storage: &mut [VenusOperand],
    geometry_storage: &mut [u32],
) -> Result<VenusA7Admission, VenusReject> {
    const OP_BEGIN_COMMAND_BUFFER: u32 = 90;
    const OP_END_COMMAND_BUFFER: u32 = 91;
    const OP_CREATE_BUFFER_VIEW: u32 = 52;
    const OP_DESTROY_BUFFER_VIEW: u32 = 53;
    const OP_CREATE_IMAGE_VIEW: u32 = 57;
    const OP_DESTROY_IMAGE_VIEW: u32 = 58;
    const OP_UPDATE_DESCRIPTOR_SETS: u32 = 79;

    let mut c = Cursor::new(bytes);
    let mut operands = OperandWriter {
        out: operand_storage,
        count: 0,
    };
    let mut scratch = SchemaScratch::new(geometry_storage);
    let mut admission = VenusA7Admission::default();
    let mut recording = false;
    let mut recorded_any = false;
    let mut materializing = false;
    let mut object_phase = false;
    let mut tearing_down = false;
    let mut terminal_free_seen = false;

    while c.offset != bytes.len() {
        let (opcode, flags) = command_header(&mut c)?;
        if flags != 0 {
            return Err(VenusReject::BadFlags);
        }
        let operands_before = operands.count;
        let facts =
            a7_schema::parse_a7_command(opcode, flags, &mut c, &mut operands, &mut scratch)?;
        admission.command_count = admission
            .command_count
            .checked_add(1)
            .ok_or(VenusReject::CountOverflow)?;

        match facts.kind {
            A7CommandKind::Allocation => {
                match opcode {
                    OP_ALLOCATE_MEMORY | OP_BIND_BUFFER_MEMORY2 | OP_BIND_IMAGE_MEMORY2 => {
                        if tearing_down
                            || recording
                            || recorded_any
                            || object_phase
                            || admission.queue_command_count != 0
                        {
                            return Err(VenusReject::InvalidSequence);
                        }
                        materializing = true;
                        let added = operands.count - operands_before;
                        if opcode == OP_ALLOCATE_MEMORY {
                            if added != 1
                                || facts.allocation_size == 0
                                || facts.memory_type_index == u32::MAX
                            {
                                return Err(VenusReject::MissingImport);
                            }
                        } else if added != 0 {
                            return Err(VenusReject::InvalidSequence);
                        }
                    }
                    OP_DESTROY_BUFFER | OP_DESTROY_IMAGE | OP_FREE_MEMORY => {
                        if materializing
                            || recording
                            || recorded_any
                            || object_phase
                            || admission.queue_command_count != 0
                            || terminal_free_seen
                            || operands.count != operands_before
                        {
                            return Err(VenusReject::InvalidSequence);
                        }
                        tearing_down = true;
                        terminal_free_seen = opcode == OP_FREE_MEMORY;
                    }
                    _ => {
                        return Err(VenusReject::InvalidSequence);
                    }
                }
                admission.allocation_command_count = admission
                    .allocation_command_count
                    .checked_add(1)
                    .ok_or(VenusReject::CountOverflow)?;
            }
            A7CommandKind::CommandRecord => {
                if tearing_down || admission.queue_command_count != 0 {
                    return Err(VenusReject::InvalidSequence);
                }
                match opcode {
                    OP_BEGIN_COMMAND_BUFFER if !recording => {
                        recording = true;
                    }
                    OP_END_COMMAND_BUFFER if recording => {
                        recording = false;
                        recorded_any = true;
                        admission.command_buffer_count = admission
                            .command_buffer_count
                            .checked_add(1)
                            .ok_or(VenusReject::CountOverflow)?;
                    }
                    OP_BEGIN_COMMAND_BUFFER | OP_END_COMMAND_BUFFER => {
                        return Err(VenusReject::InvalidSequence);
                    }
                    _ if !recording => return Err(VenusReject::InvalidSequence),
                    _ => {}
                }
            }
            A7CommandKind::Queue => {
                if tearing_down || recording || admission.queue_command_count != 0 {
                    return Err(VenusReject::InvalidSequence);
                }
                admission.queue_command_count = 1;
                if c.offset != bytes.len() {
                    return Err(VenusReject::TrailingBytes);
                }
            }
            A7CommandKind::PureControl => {
                // Object-materialization region: view creates/destroys and
                // descriptor updates dereference an outer allocation on the
                // host, so the ICD orders them into the batch behind its
                // allocate/bind records (HVC1 is unordered against pending
                // batches — measured 2026-08-24 as host-side view creates on
                // unbound images).  They parse to zero resource operands;
                // any other pure-control opcode stays refused here.
                let object_materialization = matches!(
                    opcode,
                    OP_CREATE_BUFFER_VIEW
                        | OP_DESTROY_BUFFER_VIEW
                        | OP_CREATE_IMAGE_VIEW
                        | OP_DESTROY_IMAGE_VIEW
                        | OP_UPDATE_DESCRIPTOR_SETS
                );
                if !object_materialization
                    || tearing_down
                    || recording
                    || recorded_any
                    || admission.queue_command_count != 0
                    || operands.count != operands_before
                {
                    return Err(VenusReject::InvalidSequence);
                }
                object_phase = true;
                admission.object_command_count = admission
                    .object_command_count
                    .checked_add(1)
                    .ok_or(VenusReject::CountOverflow)?;
            }
        }
    }

    if recording
        || (tearing_down && (!terminal_free_seen || admission.queue_command_count != 0))
        || (!tearing_down && admission.queue_command_count != 1)
    {
        return Err(VenusReject::InvalidSequence);
    }
    admission.terminal_teardown = tearing_down;
    admission.operand_count =
        u32::try_from(operands.count).map_err(|_| VenusReject::CountOverflow)?;
    Ok(admission)
}

/// Validate one finite HVC1 generated-control transaction.  A reply-bearing
/// transaction is normally exactly SetReply plus one GENERATE_REPLY command.
/// The bounded multi-command exceptions are a generated-parser-proven
/// CreateBuffer/GetBufferMemoryRequirements2/DestroyBuffer triplet naming one
/// never-bound private buffer, and a CreateImage/GetImageMemoryRequirements2
/// pair naming one still-live image.  Every command in either sequence names
/// the same nonzero device/object.  A reply-less transaction is one or more
/// zero-flag commands.  SetReply's zero placeholder is the only generated
/// resource operand permitted.
pub fn validate_venus_control_stream(
    bytes: &[u8],
    has_reply: bool,
    operand_storage: &mut [VenusOperand],
    geometry_storage: &mut [u32],
) -> Result<VenusControlAdmission, VenusReject> {
    let mut c = Cursor::new(bytes);
    let mut operands = OperandWriter {
        out: operand_storage,
        count: 0,
    };
    let mut scratch = SchemaScratch::new(geometry_storage);
    let mut admission = VenusControlAdmission::default();

    if has_reply {
        let (opcode, flags) = command_header(&mut c)?;
        if opcode != OP_SET_REPLY || flags != 0 || !c.pointer()? {
            return Err(VenusReject::InvalidSequence);
        }
        let operand_offset = c.offset_u32()?;
        if c.u32()? != 0 {
            return Err(VenusReject::NonZeroResourceOperand);
        }
        operands.push(operand_offset, HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32)?;
        admission.reply_offset = c.u64()?;
        admission.reply_size = c.u64()?;
        if admission.reply_size == 0 {
            return Err(VenusReject::BadArrayCount);
        }
        scratch.set_reply_size(admission.reply_size);

        let (opcode, flags) = command_header(&mut c)?;
        let facts =
            a7_schema::parse_a7_command(opcode, flags, &mut c, &mut operands, &mut scratch)?;
        if opcode == OP_CREATE_BUFFER {
            let identity = scratch.command_identity();
            if flags != 0
                || facts.kind != A7CommandKind::PureControl
                || identity.0 == 0
                || identity.1 == 0
                || operands.count != 1
            {
                return Err(VenusReject::InvalidSequence);
            }

            let (query_opcode, query_flags) = command_header(&mut c)?;
            let query_facts = a7_schema::parse_a7_command(
                query_opcode,
                query_flags,
                &mut c,
                &mut operands,
                &mut scratch,
            )?;
            if query_opcode != OP_GET_BUFFER_MEMORY_REQUIREMENTS2
                || query_flags != COMMAND_GENERATE_REPLY
                || query_facts.kind != A7CommandKind::PureControl
                || scratch.command_identity() != identity
                || operands.count != 1
            {
                return Err(VenusReject::InvalidSequence);
            }

            let (destroy_opcode, destroy_flags) = command_header(&mut c)?;
            let destroy_facts = a7_schema::parse_a7_command(
                destroy_opcode,
                destroy_flags,
                &mut c,
                &mut operands,
                &mut scratch,
            )?;
            if destroy_opcode != OP_DESTROY_BUFFER
                || destroy_flags != 0
                || destroy_facts.kind != A7CommandKind::Allocation
                || scratch.command_identity() != identity
                || operands.count != 1
            {
                return Err(VenusReject::InvalidSequence);
            }
            admission.command_count = 3;
            admission.opcode = query_facts.opcode;
        } else if opcode == OP_CREATE_IMAGE {
            let identity = scratch.command_identity();
            if flags != 0
                || facts.kind != A7CommandKind::PureControl
                || identity.0 == 0
                || identity.1 == 0
                || operands.count != 1
            {
                return Err(VenusReject::InvalidSequence);
            }

            let (query_opcode, query_flags) = command_header(&mut c)?;
            let query_facts = a7_schema::parse_a7_command(
                query_opcode,
                query_flags,
                &mut c,
                &mut operands,
                &mut scratch,
            )?;
            if query_opcode != OP_GET_IMAGE_MEMORY_REQUIREMENTS2
                || query_flags != COMMAND_GENERATE_REPLY
                || query_facts.kind != A7CommandKind::PureControl
                || scratch.command_identity() != identity
                || operands.count != 1
            {
                return Err(VenusReject::InvalidSequence);
            }
            admission.command_count = 2;
            admission.opcode = query_facts.opcode;
        } else {
            if flags != COMMAND_GENERATE_REPLY
                || facts.kind != A7CommandKind::PureControl
                || operands.count != 1
            {
                return Err(VenusReject::InvalidSequence);
            }
            admission.command_count = 1;
            admission.opcode = facts.opcode;
        }
    } else {
        while c.offset != bytes.len() {
            let (opcode, flags) = command_header(&mut c)?;
            if opcode == OP_SET_REPLY || flags != 0 {
                return Err(VenusReject::BadFlags);
            }
            let facts =
                a7_schema::parse_a7_command(opcode, flags, &mut c, &mut operands, &mut scratch)?;
            // The ICD's C60 classifier rings a destroy only for an UNBOUND
            // buffer/image (a bound one defers into an outer batch), so a bare
            // destroy here touches no allocation. Measured 2026-08-24: every
            // D3D11 process died at its first transient-buffer destroy
            // (Nr2NoSchWho=0x60E). Allocate/free/bind stay refused.
            let ring_legal_destroy = facts.kind == A7CommandKind::Allocation
                && matches!(opcode, OP_DESTROY_BUFFER | OP_DESTROY_IMAGE);
            if (facts.kind != A7CommandKind::PureControl && !ring_legal_destroy)
                || operands.count != 0
            {
                return Err(VenusReject::InvalidSequence);
            }
            admission.command_count = admission
                .command_count
                .checked_add(1)
                .ok_or(VenusReject::CountOverflow)?;
            admission.opcode = facts.opcode;
        }
        if admission.command_count == 0 {
            return Err(VenusReject::InvalidSequence);
        }
    }
    if c.offset != bytes.len() {
        return Err(VenusReject::TrailingBytes);
    }
    admission.operand_count =
        u32::try_from(operands.count).map_err(|_| VenusReject::CountOverflow)?;
    Ok(admission)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn put32(out: &mut Vec<u8>, n: u32) {
        out.extend_from_slice(&n.to_le_bytes());
    }
    fn put64(out: &mut Vec<u8>, n: u64) {
        out.extend_from_slice(&n.to_le_bytes());
    }

    fn zeros(out: &mut Vec<u8>, n: usize) {
        out.resize(out.len() + n, 0);
    }

    fn queue_prefix(opcode: u32, count: u32) -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, opcode);
        put32(&mut b, 0);
        put64(&mut b, 0x101); // queue
        put32(&mut b, count);
        put64(&mut b, count as u64);
        b
    }

    fn allocation_stream() -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1);
        put32(&mut b, 0);
        put64(&mut b, 0);
        put64(&mut b, 24);
        put32(&mut b, OP_ALLOCATE_MEMORY);
        put32(&mut b, COMMAND_GENERATE_REPLY);
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pAllocateInfo
        put32(&mut b, ST_MEMORY_ALLOCATE_INFO);
        put64(&mut b, 1); // pNext
        put32(&mut b, ST_IMPORT_MEMORY_RESOURCE_INFO_MESA);
        put64(&mut b, 0); // pNext
        put32(&mut b, 0); // resourceId
        put64(&mut b, 4096);
        put32(&mut b, 0);
        put64(&mut b, 0); // pAllocator
        put64(&mut b, 1); // pMemory
        put64(&mut b, 99);
        b
    }

    fn a7_allocate(out: &mut Vec<u8>) -> usize {
        put32(out, OP_ALLOCATE_MEMORY);
        put32(out, 0);
        put64(out, 7); // device
        put64(out, 1); // pAllocateInfo
        put32(out, ST_MEMORY_ALLOCATE_INFO);
        put64(out, 1); // pNext
        put32(out, ST_IMPORT_MEMORY_RESOURCE_INFO_MESA);
        put64(out, 0); // pNext
        let resource_offset = out.len();
        put32(out, 0); // KMD-private host resource patch
        put64(out, 4096);
        put32(out, 0);
        put64(out, 0); // pAllocator
        put64(out, 1); // pMemory
        put64(out, 99);
        resource_offset
    }

    fn a7_empty_recording(out: &mut Vec<u8>) {
        put32(out, 90); // vkBeginCommandBuffer
        put32(out, 0);
        put64(out, 0x301);
        put64(out, 1); // pBeginInfo
        put32(out, 42); // VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO
        put64(out, 0); // pNext
        put32(out, 0); // flags
        put64(out, 0); // pInheritanceInfo

        put32(out, 91); // vkEndCommandBuffer
        put32(out, 0);
        put64(out, 0x301);
    }

    fn a7_clear_color_recording(out: &mut Vec<u8>) -> usize {
        put32(out, 90); // vkBeginCommandBuffer
        put32(out, 0);
        put64(out, 0x301);
        put64(out, 1); // pBeginInfo
        put32(out, 42); // VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO
        put64(out, 0); // pNext
        put32(out, 0); // flags
        put64(out, 0); // pInheritanceInfo

        put32(out, 119); // vkCmdClearColorImage
        put32(out, 0);
        put64(out, 0x301); // commandBuffer
        put64(out, 0x501); // image
        put32(out, 1); // imageLayout
        put64(out, 1); // pColor
        put32(out, 2); // VkClearColorValue uint32 tag
        let color_count_offset = out.len();
        put64(out, 4); // uint32[4] array count
        put32(out, 0x1122_3344);
        put32(out, 0x5566_7788);
        put32(out, 0x99aa_bbcc);
        put32(out, 0xddee_ff00);
        put32(out, 1); // rangeCount
        put64(out, 1); // pRanges array count
        put32(out, 1); // aspectMask
        put32(out, 0); // baseMipLevel
        put32(out, 1); // levelCount
        put32(out, 0); // baseArrayLayer
        put32(out, 1); // layerCount

        put32(out, 91); // vkEndCommandBuffer
        put32(out, 0);
        put64(out, 0x301);
        color_count_offset
    }

    fn a7_empty_queue(out: &mut Vec<u8>) {
        put32(out, OP_QUEUE_SUBMIT);
        put32(out, 0);
        put64(out, 0x101);
        put32(out, 0);
        put64(out, 0); // pSubmits
        put64(out, 0); // fence
    }

    fn a7_destroy_allocation_object(out: &mut Vec<u8>, opcode: u32) {
        assert!(matches!(opcode, OP_DESTROY_BUFFER | OP_DESTROY_IMAGE));
        put32(out, opcode);
        put32(out, 0);
        put64(out, 7); // device
        put64(out, 0x501); // buffer or image
        put64(out, 0); // pAllocator
    }

    fn a7_free(out: &mut Vec<u8>) {
        put32(out, OP_FREE_MEMORY);
        put32(out, 0);
        put64(out, 7); // device
        put64(out, 99); // memory
        put64(out, 0); // pAllocator
    }

    fn unbound_buffer_query_stream() -> (Vec<u8>, [usize; 6]) {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1); // pStream
        put32(&mut b, 0); // private reply resource placeholder
        put64(&mut b, 80); // reply offset
        put64(&mut b, 44); // VkMemoryRequirements2 reply without pNext

        put32(&mut b, OP_CREATE_BUFFER);
        put32(&mut b, 0);
        let create_device = b.len();
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pCreateInfo
        put32(&mut b, 12); // VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO
        put64(&mut b, 0); // pNext
        put32(&mut b, 0); // flags
        put64(&mut b, 65_536); // size
        put32(&mut b, 3); // usage
        put32(&mut b, 0); // exclusive sharing
        put32(&mut b, 0); // queueFamilyIndexCount
        put64(&mut b, 0); // pQueueFamilyIndices
        put64(&mut b, 0); // pAllocator
        put64(&mut b, 1); // pBuffer
        let create_buffer = b.len();
        put64(&mut b, 0x501); // output buffer object id

        put32(&mut b, OP_GET_BUFFER_MEMORY_REQUIREMENTS2);
        put32(&mut b, COMMAND_GENERATE_REPLY);
        let query_device = b.len();
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pInfo
        put32(&mut b, 1_000_146_000); // VkBufferMemoryRequirementsInfo2
        put64(&mut b, 0); // pNext
        let query_buffer = b.len();
        put64(&mut b, 0x501); // buffer
        put64(&mut b, 1); // pMemoryRequirements
        put32(&mut b, 1_000_146_003); // VkMemoryRequirements2
        put64(&mut b, 0); // pNext

        put32(&mut b, OP_DESTROY_BUFFER);
        put32(&mut b, 0);
        let destroy_device = b.len();
        put64(&mut b, 7); // device
        let destroy_buffer = b.len();
        put64(&mut b, 0x501); // buffer
        put64(&mut b, 0); // pAllocator

        (
            b,
            [
                create_device,
                create_buffer,
                query_device,
                query_buffer,
                destroy_device,
                destroy_buffer,
            ],
        )
    }

    fn dxvk_unbound_buffer_query_stream() -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1); // pStream
        put32(&mut b, 0); // private reply resource placeholder
        put64(&mut b, 80); // reply offset
        put64(&mut b, 64); // VkMemoryRequirements2 + dedicated requirements

        put32(&mut b, OP_CREATE_BUFFER);
        put32(&mut b, 0);
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pCreateInfo
        put32(&mut b, 12); // VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO
        put64(&mut b, 1); // pNext
        put32(&mut b, 1_000_072_000); // external memory buffer create info
        put64(&mut b, 0); // pNext
        put32(&mut b, 0x200); // renderer external handle type
        put32(&mut b, 0); // flags
        put64(&mut b, 65_536); // size
        put32(&mut b, 7); // first DXVK usage probe plus transfer bits
        put32(&mut b, 1); // concurrent sharing
        put32(&mut b, 2); // queueFamilyIndexCount
        put64(&mut b, 2); // pQueueFamilyIndices array count
        put32(&mut b, 0); // graphics queue family
        put32(&mut b, 1); // transfer queue family
        put64(&mut b, 0); // pAllocator
        put64(&mut b, 1); // pBuffer
        put64(&mut b, 0x501); // output buffer object id

        put32(&mut b, OP_GET_BUFFER_MEMORY_REQUIREMENTS2);
        put32(&mut b, COMMAND_GENERATE_REPLY);
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pInfo
        put32(&mut b, 1_000_146_000); // VkBufferMemoryRequirementsInfo2
        put64(&mut b, 0); // pNext
        put64(&mut b, 0x501); // buffer
        put64(&mut b, 1); // pMemoryRequirements
        put32(&mut b, 1_000_146_003); // VkMemoryRequirements2
        put64(&mut b, 1); // pNext
        put32(&mut b, 1_000_127_000); // VkMemoryDedicatedRequirements
        put64(&mut b, 0); // pNext

        put32(&mut b, OP_DESTROY_BUFFER);
        put32(&mut b, 0);
        put64(&mut b, 7); // device
        put64(&mut b, 0x501); // buffer
        put64(&mut b, 0); // pAllocator
        b
    }

    fn dxvk_image_create_query_stream() -> (Vec<u8>, [usize; 4]) {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1); // pStream
        put32(&mut b, 0); // private reply resource placeholder
        put64(&mut b, 80); // reply offset
        put64(&mut b, 64); // VkMemoryRequirements2 + dedicated requirements

        put32(&mut b, OP_CREATE_IMAGE);
        put32(&mut b, 0);
        let create_device = b.len();
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pCreateInfo
        put32(&mut b, 14); // VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO
        put64(&mut b, 1); // pNext
        put32(&mut b, 1_000_147_000); // VkImageFormatListCreateInfo
        put64(&mut b, 0); // pNext
        put32(&mut b, 2); // viewFormatCount
        put64(&mut b, 2); // pViewFormats array count
        put32(&mut b, 44); // VK_FORMAT_B8G8R8A8_UNORM
        put32(&mut b, 50); // VK_FORMAT_B8G8R8A8_SRGB
        put32(&mut b, 8); // VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT
        put32(&mut b, 1); // VK_IMAGE_TYPE_2D
        put32(&mut b, 44); // format
        put32(&mut b, 1920); // extent.width
        put32(&mut b, 1080); // extent.height
        put32(&mut b, 1); // extent.depth
        put32(&mut b, 1); // mipLevels
        put32(&mut b, 1); // arrayLayers
        put32(&mut b, 1); // samples
        put32(&mut b, 0); // optimal tiling
        put32(&mut b, 0x14); // sampled + color attachment usage
        put32(&mut b, 0); // exclusive sharing
        put32(&mut b, 0); // queueFamilyIndexCount
        put64(&mut b, 0); // pQueueFamilyIndices
        put32(&mut b, 0); // initialLayout
        put64(&mut b, 0); // pAllocator
        put64(&mut b, 1); // pImage
        let create_image = b.len();
        put64(&mut b, 0x601); // output image object id

        put32(&mut b, OP_GET_IMAGE_MEMORY_REQUIREMENTS2);
        put32(&mut b, COMMAND_GENERATE_REPLY);
        let query_device = b.len();
        put64(&mut b, 7); // device
        put64(&mut b, 1); // pInfo
        put32(&mut b, 1_000_146_001); // VkImageMemoryRequirementsInfo2
        put64(&mut b, 0); // pNext
        let query_image = b.len();
        put64(&mut b, 0x601); // image
        put64(&mut b, 1); // pMemoryRequirements
        put32(&mut b, 1_000_146_003); // VkMemoryRequirements2
        put64(&mut b, 1); // pNext
        put32(&mut b, 1_000_127_000); // VkMemoryDedicatedRequirements
        put64(&mut b, 0); // pNext

        (b, [create_device, create_image, query_device, query_image])
    }

    #[test]
    fn control_admits_only_exact_self_contained_unbound_buffer_query() {
        let (stream, offsets) = unbound_buffer_query_stream();
        let admitted = validate_venus_control_stream(
            &stream,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("same-device same-buffer create/query/destroy transaction");
        assert_eq!(admitted.command_count, 3);
        assert_eq!(admitted.opcode, OP_GET_BUFFER_MEMORY_REQUIREMENTS2);
        assert_eq!(admitted.operand_count, 1);

        for offset in offsets {
            let mut mismatched = stream.clone();
            mismatched[offset..offset + 8].copy_from_slice(&0x777u64.to_le_bytes());
            assert_eq!(
                validate_venus_control_stream(
                    &mismatched,
                    true,
                    &mut [VenusOperand::default(); 1],
                    &mut [0u32; 8],
                ),
                Err(VenusReject::InvalidSequence)
            );
        }

        let mut missing_destroy = stream;
        missing_destroy.truncate(offsets[4] - 8);
        assert!(validate_venus_control_stream(
            &missing_destroy,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .is_err());
    }

    #[test]
    fn control_admits_dxvk_concurrent_external_buffer_query_shape() {
        let stream = dxvk_unbound_buffer_query_stream();
        let admitted = validate_venus_control_stream(
            &stream,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("DXVK concurrent external buffer query transaction");
        assert_eq!(admitted.command_count, 3);
        assert_eq!(admitted.opcode, OP_GET_BUFFER_MEMORY_REQUIREMENTS2);
        assert_eq!(admitted.operand_count, 1);
        assert_eq!(admitted.reply_size, 64);
    }

    #[test]
    fn control_admits_only_same_image_create_query_pair() {
        let (stream, offsets) = dxvk_image_create_query_stream();
        let admitted = validate_venus_control_stream(
            &stream,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("same-device same-image create/query transaction");
        assert_eq!(admitted.command_count, 2);
        assert_eq!(admitted.opcode, OP_GET_IMAGE_MEMORY_REQUIREMENTS2);
        assert_eq!(admitted.operand_count, 1);
        assert_eq!(admitted.reply_size, 64);

        for offset in offsets {
            let mut mismatched = stream.clone();
            mismatched[offset..offset + 8].copy_from_slice(&0x777u64.to_le_bytes());
            assert_eq!(
                validate_venus_control_stream(
                    &mismatched,
                    true,
                    &mut [VenusOperand::default(); 1],
                    &mut [0u32; 8],
                ),
                Err(VenusReject::InvalidSequence)
            );
        }

        // A standalone no-reply destroy is ADMITTED since 2026-08-24: the
        // ICD rings exactly the unbound destroys (bound ones defer into outer
        // batches), and refusing them killed every D3D11 process at its first
        // transient buffer (Nr2NoSchWho=0x60E).
        let mut standalone_teardown = Vec::new();
        a7_destroy_allocation_object(&mut standalone_teardown, OP_DESTROY_IMAGE);
        let teardown =
            validate_venus_control_stream(&standalone_teardown, false, &mut [], &mut [0u32; 8])
                .expect("standalone unbound teardown");
        assert_eq!(teardown.command_count, 1);
    }

    fn a7_create_image_view(out: &mut Vec<u8>) {
        put32(out, 57); // vkCreateImageView
        put32(out, 0);
        put64(out, 0x101); // device
        put64(out, 1); // pCreateInfo present
        put32(out, 15); // VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO
        put64(out, 0); // pNext
        put32(out, 0); // flags
        put64(out, 0x501); // image
        put32(out, 1); // viewType
        put32(out, 37); // format
        for _ in 0..4 {
            put32(out, 0); // components
        }
        put32(out, 1); // aspectMask
        put32(out, 0);
        put32(out, 1);
        put32(out, 0);
        put32(out, 1);
        put64(out, 0); // pAllocator
        put64(out, 1); // pView present
        put64(out, 0x601); // view handle
    }

    fn a7_destroy_image_view(out: &mut Vec<u8>) {
        put32(out, 58); // vkDestroyImageView
        put32(out, 0);
        put64(out, 0x101);
        put64(out, 0x601);
        put64(out, 0); // pAllocator
    }

    fn a7_update_descriptor_sets_empty(out: &mut Vec<u8>) {
        put32(out, 79); // vkUpdateDescriptorSets
        put32(out, 0);
        put64(out, 0x101);
        put32(out, 0); // descriptorWriteCount
        put64(out, 0); // writes array size
        put32(out, 0); // descriptorCopyCount
        put64(out, 0); // copies array size
    }

    #[test]
    fn a7_admits_object_materialization_between_records_and_recordings() {
        let mut b = Vec::new();
        let resource_offset = a7_allocate(&mut b);
        a7_create_image_view(&mut b);
        a7_update_descriptor_sets_empty(&mut b);
        a7_destroy_image_view(&mut b);
        a7_empty_recording(&mut b);
        a7_empty_queue(&mut b);

        let mut operands = [VenusOperand::default(); 1];
        let admitted =
            validate_venus_a7_outer_stream(&b, &mut operands, &mut [0u32; 8]).unwrap();
        assert_eq!(admitted.command_count, 7);
        assert_eq!(admitted.allocation_command_count, 1);
        assert_eq!(admitted.object_command_count, 3);
        assert_eq!(admitted.command_buffer_count, 1);
        assert_eq!(admitted.queue_command_count, 1);
        assert_eq!(admitted.operand_count, 1);
        assert_eq!(operands[0].payload_offset, resource_offset as u32);

        // The common no-allocation shape: object commands straight into a
        // recording batch.
        let mut plain = Vec::new();
        a7_create_image_view(&mut plain);
        a7_empty_recording(&mut plain);
        a7_empty_queue(&mut plain);
        let admitted =
            validate_venus_a7_outer_stream(&plain, &mut [], &mut [0u32; 8]).unwrap();
        assert_eq!(admitted.object_command_count, 1);
        assert_eq!(admitted.queue_command_count, 1);
    }

    #[test]
    fn a7_refuses_misplaced_or_foreign_object_materialization() {
        // Allocation region must be closed before the object region.
        let mut alloc_after = Vec::new();
        a7_create_image_view(&mut alloc_after);
        a7_allocate(&mut alloc_after);
        assert_eq!(
            validate_venus_a7_outer_stream(
                &alloc_after,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::InvalidSequence)
        );

        // Object commands may not follow a completed recording.
        let mut after_recording = Vec::new();
        a7_empty_recording(&mut after_recording);
        a7_create_image_view(&mut after_recording);
        assert_eq!(
            validate_venus_a7_outer_stream(&after_recording, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );

        // Nor ride a teardown stream.
        let mut in_teardown = Vec::new();
        a7_destroy_allocation_object(&mut in_teardown, OP_DESTROY_IMAGE);
        a7_create_image_view(&mut in_teardown);
        a7_free(&mut in_teardown);
        assert_eq!(
            validate_venus_a7_outer_stream(&in_teardown, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );

        // Every other pure-control opcode stays refused in an outer stream —
        // vkDestroySampler shares vkDestroyImageView's exact wire shape.
        let mut foreign = Vec::new();
        put32(&mut foreign, 71); // vkDestroySampler
        put32(&mut foreign, 0);
        put64(&mut foreign, 0x101);
        put64(&mut foreign, 0x601);
        put64(&mut foreign, 0);
        a7_empty_recording(&mut foreign);
        a7_empty_queue(&mut foreign);
        assert_eq!(
            validate_venus_a7_outer_stream(&foreign, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );
    }

    #[test]
    fn a7_admits_deferred_allocate_record_and_exact_final_queue() {
        let mut b = Vec::new();
        let resource_offset = a7_allocate(&mut b);
        a7_empty_recording(&mut b);
        a7_empty_queue(&mut b);

        let mut operands = [VenusOperand::default(); 1];
        let mut geometry = [0u32; 8];
        let admitted = validate_venus_a7_outer_stream(&b, &mut operands, &mut geometry).unwrap();
        assert_eq!(admitted.command_count, 4);
        assert_eq!(admitted.allocation_command_count, 1);
        assert_eq!(admitted.command_buffer_count, 1);
        assert_eq!(admitted.queue_command_count, 1);
        assert_eq!(admitted.operand_count, 1);
        assert_eq!(operands[0].payload_offset, resource_offset as u32);
        assert_eq!(
            operands[0].operand_kind,
            HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32
        );
    }

    #[test]
    fn a7_consumes_clear_color_union_array_exactly() {
        let mut b = Vec::new();
        let color_count_offset = a7_clear_color_recording(&mut b);
        a7_empty_queue(&mut b);

        let admitted = validate_venus_a7_outer_stream(&b, &mut [], &mut [0u32; 8]).unwrap();
        assert_eq!(admitted.command_count, 4);
        assert_eq!(admitted.command_buffer_count, 1);
        assert_eq!(admitted.queue_command_count, 1);

        b[color_count_offset..color_count_offset + 8].copy_from_slice(&3u64.to_le_bytes());
        assert_eq!(
            validate_venus_a7_outer_stream(&b, &mut [], &mut [0u32; 8]),
            Err(VenusReject::BadArrayCount)
        );
    }

    #[test]
    fn a7_admits_exact_joined_allocation_teardown_without_synthetic_queue() {
        for opcode in [OP_DESTROY_BUFFER, OP_DESTROY_IMAGE] {
            let mut b = Vec::new();
            a7_destroy_allocation_object(&mut b, opcode);
            a7_free(&mut b);

            let admitted = validate_venus_a7_outer_stream(&b, &mut [], &mut [0u32; 8]).unwrap();
            assert_eq!(admitted.command_count, 2);
            assert_eq!(admitted.allocation_command_count, 2);
            assert_eq!(admitted.command_buffer_count, 0);
            assert_eq!(admitted.queue_command_count, 0);
            assert_eq!(admitted.operand_count, 0);
            assert!(admitted.terminal_teardown);
        }

        let mut free_only = Vec::new();
        a7_free(&mut free_only);
        let admitted = validate_venus_a7_outer_stream(&free_only, &mut [], &mut [0u32; 8]).unwrap();
        assert_eq!(admitted.command_count, 1);
        assert!(admitted.terminal_teardown);
    }

    #[test]
    fn a7_refuses_incomplete_reordered_or_mixed_allocation_teardown() {
        let mut destroy_only = Vec::new();
        a7_destroy_allocation_object(&mut destroy_only, OP_DESTROY_IMAGE);
        assert_eq!(
            validate_venus_a7_outer_stream(&destroy_only, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );

        let mut after_free = Vec::new();
        a7_free(&mut after_free);
        a7_destroy_allocation_object(&mut after_free, OP_DESTROY_IMAGE);
        assert_eq!(
            validate_venus_a7_outer_stream(&after_free, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );

        let mut synthetic_queue = Vec::new();
        a7_destroy_allocation_object(&mut synthetic_queue, OP_DESTROY_IMAGE);
        a7_free(&mut synthetic_queue);
        a7_empty_queue(&mut synthetic_queue);
        assert_eq!(
            validate_venus_a7_outer_stream(&synthetic_queue, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );

        let mut allocate_then_free = Vec::new();
        a7_allocate(&mut allocate_then_free);
        a7_free(&mut allocate_then_free);
        assert_eq!(
            validate_venus_a7_outer_stream(
                &allocate_then_free,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::InvalidSequence)
        );
    }

    #[test]
    fn a7_refuses_nonzero_resource_and_incomplete_or_reordered_scope() {
        let mut nonzero = Vec::new();
        let resource_offset = a7_allocate(&mut nonzero);
        nonzero[resource_offset] = 1;
        a7_empty_queue(&mut nonzero);
        assert_eq!(
            validate_venus_a7_outer_stream(
                &nonzero,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::NonZeroResourceOperand)
        );

        let mut unterminated = Vec::new();
        put32(&mut unterminated, 90);
        put32(&mut unterminated, 0);
        put64(&mut unterminated, 0x301);
        put64(&mut unterminated, 1);
        put32(&mut unterminated, 42);
        put64(&mut unterminated, 0);
        put32(&mut unterminated, 0);
        put64(&mut unterminated, 0);
        a7_empty_queue(&mut unterminated);
        assert_eq!(
            validate_venus_a7_outer_stream(&unterminated, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );

        let mut allocation_after_record = Vec::new();
        a7_empty_recording(&mut allocation_after_record);
        a7_allocate(&mut allocation_after_record);
        a7_empty_queue(&mut allocation_after_record);
        assert_eq!(
            validate_venus_a7_outer_stream(
                &allocation_after_record,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::InvalidSequence)
        );
    }

    #[test]
    fn admits_exact_allocate_and_reports_both_zero_operands() {
        let b = allocation_stream();
        let mut out = [VenusOperand::default(); 2];
        let admitted = validate_venus_stream(&b, &mut out).unwrap();
        assert_eq!(admitted.class, VenusCommandClass::AllocateMemory);
        assert_eq!(admitted.operand_count, 2);
        assert_eq!(out[0].payload_offset, 16);
        assert_eq!(out[1].payload_offset, 84);
        assert_eq!(
            out[0].operand_kind,
            HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32
        );
    }

    #[test]
    fn refuses_nonzero_resource_operand_and_trailing_command() {
        let mut b = allocation_stream();
        b[16] = 1;
        assert_eq!(
            validate_venus_stream(&b, &mut [VenusOperand::default(); 2]),
            Err(VenusReject::NonZeroResourceOperand)
        );
        let mut b = allocation_stream();
        put32(&mut b, OP_FREE_MEMORY);
        assert_eq!(
            validate_venus_stream(&b, &mut [VenusOperand::default(); 2]),
            Err(VenusReject::TrailingBytes)
        );
    }

    #[test]
    fn admits_empty_queue_commands_and_free() {
        for opcode in [OP_QUEUE_SUBMIT, OP_QUEUE_SUBMIT2, OP_QUEUE_BIND_SPARSE] {
            let mut b = Vec::new();
            put32(&mut b, opcode);
            put32(&mut b, 0);
            put64(&mut b, 1); // queue
            put32(&mut b, 0);
            put64(&mut b, 0); // null array
            put64(&mut b, 0); // fence
            let admitted = validate_venus_stream(&b, &mut []).unwrap();
            assert_eq!(admitted.operand_count, 0);
        }

        let mut b = Vec::new();
        put32(&mut b, OP_FREE_MEMORY);
        put32(&mut b, 0);
        put64(&mut b, 1);
        put64(&mut b, 2);
        put64(&mut b, 0);
        assert_eq!(
            validate_venus_stream(&b, &mut []).unwrap().class,
            VenusCommandClass::FreeMemory
        );
    }

    #[test]
    fn admits_generated_submit_with_the_complete_supported_chain() {
        let mut b = queue_prefix(OP_QUEUE_SUBMIT, 1);
        put32(&mut b, ST_SUBMIT_INFO);
        // VkDeviceGroupSubmitInfo -> VkProtectedSubmitInfo ->
        // VkTimelineSemaphoreSubmitInfo.  The generator writes the recursive
        // pNext body before each containing structure's self fields.
        put64(&mut b, 1);
        put32(&mut b, ST_DEVICE_GROUP_SUBMIT_INFO);
        put64(&mut b, 1);
        put32(&mut b, ST_PROTECTED_SUBMIT_INFO);
        put64(&mut b, 1);
        put32(&mut b, ST_TIMELINE_SEMAPHORE_SUBMIT_INFO);
        put64(&mut b, 0);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 9);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 10);
        put32(&mut b, 1); // protectedSubmit
        for count in [1u32, 1, 1] {
            put32(&mut b, count);
            put64(&mut b, count as u64);
            put32(&mut b, 0);
        }
        // VkSubmitInfo self: one wait + stage, one command, one signal.
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 0x201);
        put64(&mut b, 1);
        put32(&mut b, 0x10);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 0x301);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 0x401);
        put64(&mut b, 0x501); // fence

        assert_eq!(
            validate_venus_stream(&b, &mut []).unwrap().class,
            VenusCommandClass::QueueSubmit
        );
    }

    fn semaphore_submit(out: &mut Vec<u8>, semaphore: u64) {
        put32(out, ST_SEMAPHORE_SUBMIT_INFO);
        put64(out, 0);
        put64(out, semaphore);
        put64(out, 7);
        put64(out, 0x100);
        put32(out, 0);
    }

    #[test]
    fn admits_generated_submit2_with_wait_command_and_signal_records() {
        let mut b = queue_prefix(OP_QUEUE_SUBMIT2, 1);
        put32(&mut b, ST_SUBMIT_INFO2);
        put64(&mut b, 0);
        put32(&mut b, 0); // flags
        put32(&mut b, 1);
        put64(&mut b, 1);
        semaphore_submit(&mut b, 0x601);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put32(&mut b, ST_COMMAND_BUFFER_SUBMIT_INFO);
        put64(&mut b, 0);
        put64(&mut b, 0x701);
        put32(&mut b, 1);
        put32(&mut b, 1);
        put64(&mut b, 1);
        semaphore_submit(&mut b, 0x801);
        put64(&mut b, 0x901); // fence

        assert_eq!(
            validate_venus_stream(&b, &mut []).unwrap().class,
            VenusCommandClass::QueueSubmit2
        );
    }

    fn sparse_object(out: &mut Vec<u8>, stride: usize) {
        put64(out, 0xA01);
        put32(out, 1);
        put64(out, 1);
        zeros(out, stride);
    }

    #[test]
    fn admits_generated_bind_sparse_with_all_three_binding_classes() {
        let mut b = queue_prefix(OP_QUEUE_BIND_SPARSE, 1);
        put32(&mut b, ST_BIND_SPARSE_INFO);
        put64(&mut b, 1);
        put32(&mut b, ST_DEVICE_GROUP_BIND_SPARSE_INFO);
        put64(&mut b, 1);
        put32(&mut b, ST_TIMELINE_SEMAPHORE_SUBMIT_INFO);
        put64(&mut b, 0);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 11);
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 12);
        put32(&mut b, 0); // resourceDeviceIndex
        put32(&mut b, 0); // memoryDeviceIndex
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 0xB01);
        for stride in [36usize, 36, 56] {
            put32(&mut b, 1);
            put64(&mut b, 1);
            sparse_object(&mut b, stride);
        }
        put32(&mut b, 1);
        put64(&mut b, 1);
        put64(&mut b, 0xC01);
        put64(&mut b, 0xD01); // fence

        assert_eq!(
            validate_venus_stream(&b, &mut []).unwrap().class,
            VenusCommandClass::QueueBindSparse
        );
    }

    #[test]
    fn allocation_chain_is_recursive_and_requires_one_zero_import() {
        let mut b = allocation_stream();
        // Replace Import's pNext=NULL with a second import.  The generated
        // recursive chain shape is valid bytes but must be rejected as
        // ambiguous resource ownership.
        b[76..84].copy_from_slice(&1u64.to_le_bytes());
        b.splice(84..84, {
            let mut nested = Vec::new();
            put32(&mut nested, ST_IMPORT_MEMORY_RESOURCE_INFO_MESA);
            put64(&mut nested, 0);
            put32(&mut nested, 0);
            nested
        });
        assert_eq!(
            validate_venus_stream(&b, &mut [VenusOperand::default(); 3]),
            Err(VenusReject::DuplicateImport)
        );

        let mut missing = allocation_stream();
        // A null top-level chain removes the only import and the stream is
        // then re-laid out exactly as the generated VkMemoryAllocateInfo body.
        missing[64..72].copy_from_slice(&0u64.to_le_bytes());
        missing.drain(72..88);
        assert_eq!(
            validate_venus_stream(&missing, &mut [VenusOperand::default(); 2]),
            Err(VenusReject::MissingImport)
        );
    }

    #[test]
    fn admits_device_extension_count_and_fill_with_null_layer() {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1); // pStream
        put32(&mut b, 0); // private reply resource placeholder
        put64(&mut b, 80); // reply offset
        put64(&mut b, 28); // generated reply bytes

        put32(&mut b, 14); // vkEnumerateDeviceExtensionProperties
        put32(&mut b, COMMAND_GENERATE_REPLY);
        put64(&mut b, 2); // physical device
        put64(&mut b, 0); // optional pLayerName = NULL
        put64(&mut b, 1); // pPropertyCount
        put32(&mut b, 0); // extension-property capacity query
        put64(&mut b, 0); // pProperties = NULL

        let mut operands = [VenusOperand::default(); 1];
        let mut geometry = [0u32; 8];
        let admitted = validate_venus_control_stream(&b, true, &mut operands, &mut geometry)
            .expect("vkEnumerateDeviceExtensionProperties NULL-layer count query");
        assert_eq!(admitted.opcode, 14);
        assert_eq!(admitted.operand_count, 1);
        assert_eq!(admitted.reply_offset, 80);
        assert_eq!(admitted.reply_size, 28);
        assert_eq!(operands[0].payload_offset, 16);

        let mut fill = b.clone();
        fill[28..36].copy_from_slice(&43_444u64.to_le_bytes());
        fill[68..72].copy_from_slice(&162u32.to_le_bytes());
        fill[72..80].copy_from_slice(&162u64.to_le_bytes());
        let admitted = validate_venus_control_stream(
            &fill,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("vkEnumerateDeviceExtensionProperties output-array fill");
        assert_eq!(admitted.opcode, 14);
        assert_eq!(admitted.reply_size, 43_444);

        fill[28..36].copy_from_slice(&43_443u64.to_le_bytes());
        assert_eq!(
            validate_venus_control_stream(
                &fill,
                true,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::BadArrayCount)
        );

        let mut unterminated = b;
        unterminated[52..60].copy_from_slice(&1u64.to_le_bytes());
        unterminated.splice(60..60, [b'x', 0, 0, 0]);
        assert_eq!(
            validate_venus_control_stream(
                &unterminated,
                true,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::BadArrayCount)
        );
    }

    fn physical_device_features2_stream(chain: &[u32]) -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1); // pStream
        put32(&mut b, 0); // private reply resource placeholder
        put64(&mut b, 80); // reply offset
        put64(&mut b, 2_036); // captured generated reply bytes

        put32(&mut b, 147); // vkGetPhysicalDeviceFeatures2
        put32(&mut b, COMMAND_GENERATE_REPLY);
        put64(&mut b, 2); // physical device
        put64(&mut b, 1); // pFeatures
        put32(&mut b, 1_000_059_000); // VkPhysicalDeviceFeatures2
        for &structure_type in chain {
            put64(&mut b, 1); // pNext
            put32(&mut b, structure_type);
        }
        put64(&mut b, 0); // terminal pNext
        b
    }

    fn physical_device_memory_properties2_stream(reply_size: u64, memory_budget: bool) -> Vec<u8> {
        let mut b = Vec::new();
        put32(&mut b, OP_SET_REPLY);
        put32(&mut b, 0);
        put64(&mut b, 1); // pStream
        put32(&mut b, 0); // private reply resource placeholder
        put64(&mut b, 80); // reply offset
        put64(&mut b, reply_size);

        put32(&mut b, 152); // vkGetPhysicalDeviceMemoryProperties2
        put32(&mut b, COMMAND_GENERATE_REPLY);
        put64(&mut b, 2); // physical device
        put64(&mut b, 1); // pMemoryProperties
        put32(&mut b, 1_000_059_006); // VkPhysicalDeviceMemoryProperties2
        if memory_budget {
            put64(&mut b, 1); // pNext
            put32(&mut b, 1_000_237_000); // VkPhysicalDeviceMemoryBudgetPropertiesEXT
            put64(&mut b, 0); // terminal pNext
        } else {
            put64(&mut b, 0); // terminal pNext
        }
        put64(&mut b, 32); // VK_MAX_MEMORY_TYPES output slots
        put64(&mut b, 16); // VK_MAX_MEMORY_HEAPS output slots
        b
    }

    #[test]
    fn admits_captured_dxvk_feature_chain_but_keeps_a_finite_depth_bound() {
        let chain = [
            1_000_252_000,
            1_000_352_000,
            1_000_028_000,
            1_000_642_000,
            1_000_564_000,
            1_000_234_000,
            1_000_567_000,
            1_000_260_000,
            1_000_254_000,
            1_000_382_000,
            1_000_356_000,
            1_000_498_000,
            1_000_422_000,
            1_000_451_000,
            1_000_351_000,
            1_000_392_000,
            1_000_328_000,
            1_000_495_000,
            1_000_391_000,
            1_000_418_000,
            1_000_393_000,
            1_000_320_000,
            1_000_251_000,
            1_000_455_000,
            1_000_499_000,
            1_000_102_000,
            1_000_355_000,
            1_000_582_000,
            1_000_283_000,
            1_000_287_002,
            1_000_081_001,
            1_000_381_000,
            1_000_244_000,
            1_000_411_000,
            1_000_148_000,
            1_000_339_000,
            1_000_524_000,
            1_000_336_000,
            1_000_387_000,
            1_000_235_000,
            1_000_323_000,
            1_000_558_000,
            1_000_434_000,
            1_000_181_000,
            1_000_141_000,
            1_000_286_000,
            1_000_481_000,
            1_000_347_000,
            1_000_386_000,
            1_000_348_013,
            1_000_562_000,
            1_000_226_003,
            1_000_203_000,
            1_000_421_000,
            1_000_506_000,
            1_000_201_000,
            1_000_150_013,
            1_000_232_000,
            55,
            1_000_330_000,
            1_000_281_000,
            1_000_267_000,
            1_000_377_000,
            1_000_340_000,
            53,
            51,
            49,
        ];
        let stream = physical_device_features2_stream(&chain);
        assert_eq!(stream.len(), 876);
        let admitted = validate_venus_control_stream(
            &stream,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("captured DXVK vkGetPhysicalDeviceFeatures2 chain");
        assert_eq!(admitted.opcode, 147);
        assert_eq!(admitted.reply_size, 2_036);

        let too_deep = physical_device_features2_stream(&std::vec![1_000_252_000; 129]);
        assert_eq!(
            validate_venus_control_stream(
                &too_deep,
                true,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::UnsupportedChain)
        );
    }

    #[test]
    fn admits_captured_memory_properties2_output_shape_and_exact_reply_sizes() {
        let captured = physical_device_memory_properties2_stream(496, false);
        assert_eq!(captured.len(), 88);
        let admitted = validate_venus_control_stream(
            &captured,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("captured vkGetPhysicalDeviceMemoryProperties2 request");
        assert_eq!(admitted.opcode, 152);
        assert_eq!(admitted.reply_size, 496);

        let wrong_null_reply = physical_device_memory_properties2_stream(495, false);
        assert_eq!(
            validate_venus_control_stream(
                &wrong_null_reply,
                true,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::BadArrayCount)
        );

        let with_budget = physical_device_memory_properties2_stream(780, true);
        assert_eq!(with_budget.len(), 100);
        let admitted = validate_venus_control_stream(
            &with_budget,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("memory-budget pNext reply shape");
        assert_eq!(admitted.opcode, 152);
        assert_eq!(admitted.reply_size, 780);

        let wrong_budget_reply = physical_device_memory_properties2_stream(496, true);
        assert_eq!(
            validate_venus_control_stream(
                &wrong_budget_reply,
                true,
                &mut [VenusOperand::default(); 1],
                &mut [0u32; 8],
            ),
            Err(VenusReject::BadArrayCount)
        );
    }

    #[test]
    fn admits_captured_unbound_destroy_buffer_control_batch() {
        // Exact 32-byte payload captured at the first rejected control Render
        // on KMD 22.22.364.0 (Nr2NoSchWho=0x60E, ~20 processes/boot): one
        // reply-less vkDestroyBuffer of an UNBOUND transient buffer, which the
        // ICD's C60 classifier deliberately sends down the ring.
        let mut captured = Vec::new();
        put32(&mut captured, OP_DESTROY_BUFFER);
        put32(&mut captured, 0);
        put64(&mut captured, 3); // device
        put64(&mut captured, 0x3a); // buffer
        put64(&mut captured, 0); // pAllocator
        assert_eq!(captured.len(), 32);
        let admitted = validate_venus_control_stream(&captured, false, &mut [], &mut [0u32; 8])
            .expect("captured unbound vkDestroyBuffer");
        assert_eq!(admitted.opcode, OP_DESTROY_BUFFER);
        assert_eq!(admitted.command_count, 1);
        assert_eq!(admitted.operand_count, 0);
    }

    #[test]
    fn admits_unbound_destroy_image_after_pure_control() {
        let mut b = Vec::new();
        // vkDestroyBufferView (53) is an ordinary pure-control destroy.
        put32(&mut b, 53);
        put32(&mut b, 0);
        put64(&mut b, 7); // device
        put64(&mut b, 0x77); // bufferView
        put64(&mut b, 0); // pAllocator
        a7_destroy_allocation_object(&mut b, OP_DESTROY_IMAGE);
        let admitted = validate_venus_control_stream(&b, false, &mut [], &mut [0u32; 8])
            .expect("pure control followed by unbound vkDestroyImage");
        assert_eq!(admitted.command_count, 2);
        assert_eq!(admitted.operand_count, 0);
    }

    #[test]
    fn control_no_reply_still_refuses_free_memory() {
        let mut b = Vec::new();
        a7_free(&mut b);
        assert_eq!(
            validate_venus_control_stream(&b, false, &mut [], &mut [0u32; 8]),
            Err(VenusReject::InvalidSequence)
        );
    }

    #[test]
    fn admits_captured_dxvk_update_descriptor_sets_control_batch() {
        // Exact 120-byte payload captured at the first rejected Render on
        // KMD 22.22.316.0.  It is one reply-less vkUpdateDescriptorSets from
        // the dxvk-cs thread (SHA-256 5d50933e16eabdb484515ee65ff7a934f4
        // 33ba1adcff3674e8e606671209dc07), not a reply-stream or allocation call.
        let words = [
            0x4f, 0, 3, 0, 1, 1, 0, 0x23, 0, 0, 0x7c, 0, 0, 0, 1, 0, 1, 0, 0x13, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let mut captured = Vec::new();
        for word in words {
            put32(&mut captured, word);
        }
        assert_eq!(captured.len(), 120);

        let admitted = validate_venus_control_stream(&captured, false, &mut [], &mut [0u32; 8])
            .expect("captured DXVK vkUpdateDescriptorSets batch");
        assert_eq!(admitted.opcode, 79);
        assert_eq!(admitted.command_count, 1);
        assert_eq!(admitted.operand_count, 0);

        // A present payload pointer must still carry exactly descriptorCount
        // elements; only the two unused semantic-union pointers may be zero.
        captured[64..72].copy_from_slice(&2u64.to_le_bytes());
        assert_eq!(
            validate_venus_control_stream(&captured, false, &mut [], &mut [0u32; 8],),
            Err(VenusReject::BadArrayCount)
        );
    }

    #[test]
    fn admits_captured_dxvk_image_format_query_control_batch() {
        // Exact 112-byte Venus payload captured at the first repeating
        // 264-byte HNR2 refusal on KMD 22.22.317.0 (SHA-256
        // 0b4455c6fc0ddbc23b01041622d1bd5ee8c8be2e33b9e927886ece385606cbda).
        // It is SetReplyCommandStreamMESA followed by one generated-reply
        // vkGetPhysicalDeviceImageFormatProperties2 call.
        let words = [
            0xb2,
            0,
            1,
            0,
            0,
            0x50,
            0,
            0x3c,
            0,
            0x96,
            1,
            2,
            0,
            1,
            0,
            0x3b9b_b07c,
            0,
            0,
            0x2c,
            1,
            0,
            3,
            8,
            1,
            0,
            0x3b9b_b07b,
            0,
            0,
        ];
        let mut captured = Vec::new();
        for word in words {
            put32(&mut captured, word);
        }
        assert_eq!(captured.len(), 112);

        let admitted = validate_venus_control_stream(
            &captured,
            true,
            &mut [VenusOperand::default(); 1],
            &mut [0u32; 8],
        )
        .expect("captured DXVK image-format query");
        assert_eq!(admitted.opcode, 150);
        assert_eq!(admitted.command_count, 1);
        assert_eq!(admitted.operand_count, 1);
        assert_eq!(admitted.reply_offset, 80);
        assert_eq!(admitted.reply_size, 60);
    }

    #[test]
    fn every_nested_shape_refuses_a_truncated_tail() {
        let mut streams = Vec::new();

        let mut submit2 = queue_prefix(OP_QUEUE_SUBMIT2, 1);
        put32(&mut submit2, ST_SUBMIT_INFO2);
        put64(&mut submit2, 0);
        put32(&mut submit2, 0);
        put32(&mut submit2, 1);
        put64(&mut submit2, 1);
        semaphore_submit(&mut submit2, 1);
        streams.push(submit2);

        let mut sparse = queue_prefix(OP_QUEUE_BIND_SPARSE, 1);
        put32(&mut sparse, ST_BIND_SPARSE_INFO);
        put64(&mut sparse, 0);
        put32(&mut sparse, 0);
        put64(&mut sparse, 0);
        put32(&mut sparse, 1);
        put64(&mut sparse, 1);
        sparse_object(&mut sparse, 36);
        streams.push(sparse);

        for mut stream in streams {
            stream.pop();
            assert_eq!(
                validate_venus_stream(&stream, &mut []),
                Err(VenusReject::Truncated)
            );
        }
    }

    #[test]
    fn rejects_count_driven_truncation_without_long_loop() {
        let mut b = Vec::new();
        put32(&mut b, OP_QUEUE_SUBMIT);
        put32(&mut b, 0);
        put64(&mut b, 1);
        put32(&mut b, u32::MAX);
        put64(&mut b, u32::MAX as u64);
        assert_eq!(
            validate_venus_stream(&b, &mut []),
            Err(VenusReject::BadArrayCount)
        );
    }

    #[test]
    fn resource_id64_kind_remains_schema_reserved_but_width_known() {
        assert_eq!(
            helios_protocol::native_render::HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64,
            2
        );
    }
}
