//! Direct ownership of one Mesa A5 record-only translator instance.
//!
//! Both package UMDs construct this object before their DXVK/vkd3d engine. The
//! A5 entry point is a normal package import from the lower ICD; there is no
//! loader search, module enumeration, `GetProcAddress`, or second `VkInstance`.

use core::ffi::c_void;

use helios_protocol::{
    crc64_ecma, hob1_record_crc64, validate_batch_record, HeliosOuterBatchExpectation,
    HeliosOuterBatchOperandV1, HeliosOuterBatchRejection, HeliosOuterBatchUseV1,
    HeliosOuterBatchV1, HeliosOuterContextAttachV1, HeliosOuterScopeBeginV1,
    HeliosOuterScopeCloseV1, HeliosOuterSubmitV1, HeliosQueueAttachRequestV1, HeliosQueueAttachV1,
    HeliosSealedBatchCopyV1, HeliosSealedBatchV1, HeliosSealedOperandV1, HeliosSealedResourceUseV1,
    HeliosTranslationEndpointV1, HeliosTranslatorCreateInfoV1, HeliosTranslatorDispatchV1,
    HeliosTranslatorHostCallbacksV1, HeliosTranslatorInstanceV1, HeliosTranslatorScope,
    HeliosTranslatorStatus, HeliosTranslatorStatusCode, HELIOS_HOB1_ABI_VERSION,
    HELIOS_HOB1_HEADER_BYTES, HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX,
    HELIOS_HOB1_IDENTITY_D3D12_GPUVA, HELIOS_HOB1_MAGIC, HELIOS_HOB1_MAX_BYTES,
    HELIOS_HOB1_OFFSET_ALIGNMENT, HELIOS_HOB1_OPERAND_RECORD_BYTES, HELIOS_HOB1_USE_RECORD_BYTES,
    HELIOS_HOS1_ABI_VERSION, HELIOS_HOS1_BYTES, HELIOS_HOS1_MAGIC, HELIOS_PACKAGE_GENERATION,
    HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES, HELIOS_TRANSLATOR_CREATE_INFO_BYTES,
    HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION, HELIOS_TRANSLATOR_ENDPOINT_BYTES,
    HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES, HELIOS_TRANSLATOR_HQA1_BYTES,
    HELIOS_TRANSLATOR_INSTANCE_BYTES, HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES,
    HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES, HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES,
    HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED, HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED,
    HELIOS_TRANSLATOR_SEALED_BATCH_BYTES, HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES,
    HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY,
};

const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x0000_0004;
const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x0000_0002;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut *mut c_void) -> i32;
}

unsafe extern "C" {
    fn helios_icd_create_translator_v1(
        create_info: *const HeliosTranslatorCreateInfoV1,
        out_instance: *mut HeliosTranslatorInstanceV1,
    ) -> HeliosTranslatorStatusCode;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectTranslatorError {
    Refused(HeliosTranslatorStatus),
    UnknownStatus(HeliosTranslatorStatusCode),
    EntryProvenance,
    ProcedureProvenance,
    EndpointCapacity,
}

/// One complete immutable A5 seal copied out of the lower ICD.  The vectors
/// own their bytes; no pointer into Mesa survives the copy call or crosses the
/// subsequent runtime callback.
pub struct DirectSealedBatch {
    pub descriptor: HeliosSealedBatchV1,
    pub payload: Vec<u8>,
    pub uses: Vec<HeliosSealedResourceUseV1>,
    pub operands: Vec<HeliosSealedOperandV1>,
}

/// The exact outer-context facts against which a copied A5 batch is encoded.
/// These values come from the live runtime context object; none is recovered
/// from a process-global lookup or from the sealed allocation tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectHobContext {
    pub context_generation: u64,
    pub endpoint_id: u32,
    pub context_flags: u32,
    pub max_command_bytes: u64,
    pub last_batch_id: u64,
    pub allocation_list_count: u32,
}

/// The two WDDM identity fields the owning UMD resolves for one exact token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectResolvedUse {
    pub address_or_index: u64,
    pub allocation_generation: u64,
}

/// Named failures from the UMD-owned token-to-WDDM conversion.  Keeping these
/// distinct is what lets both D3D11 and D3D12 refuse stale, foreign and missing
/// associations without reclassifying them as an encoder-size failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectIdentityRefusal {
    ZeroToken,
    ForeignDevice,
    MissingToken,
    StaleGeneration,
    DuplicateAssociation,
    D3D11ByteOffset,
    RangeOverflow,
    RangeOutOfBounds,
    UseAfterDestroy,
    AddressZero,
    AllocationIndexOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectHobError {
    Translator(HeliosTranslatorStatus),
    PayloadLength,
    PayloadCrc,
    UseTableLength,
    OperandTableLength,
    LayoutOverflow,
    Identity {
        use_index: u32,
        reason: DirectIdentityRefusal,
    },
    Record(HeliosOuterBatchRejection),
}

/// One 8-byte-aligned, immutable complete HOB1 record.  `Vec<u8>` cannot
/// promise the alignment the protocol parser requires, so storage is in words
/// while `byte_len` preserves the exact (possibly non-word-sized) record.
pub struct DirectHob1 {
    words: Vec<u64>,
    byte_len: usize,
    header: HeliosOuterBatchV1,
}

impl DirectHob1 {
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `words` owns at least `byte_len` initialized bytes and is
        // naturally 8-byte aligned for the whole returned borrow.
        unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.byte_len) }
    }

    pub fn header(&self) -> &HeliosOuterBatchV1 {
        &self.header
    }

    /// Build the exact 64-byte HOS1 metadata for this immutable HOB1.
    pub fn outer_submit(&self) -> HeliosOuterSubmitV1 {
        HeliosOuterSubmitV1 {
            magic: HELIOS_HOS1_MAGIC,
            abi_version: HELIOS_HOS1_ABI_VERSION,
            struct_size: HELIOS_HOS1_BYTES,
            package_generation: self.header.package_generation,
            session_generation: self.header.session_generation,
            context_generation: self.header.context_generation,
            endpoint_id: self.header.endpoint_id,
            hob1_bytes: self.header.total_bytes as u32,
            batch_id: self.header.batch_id,
            hob1_crc64: self.header.crc64,
            reserved: 0,
        }
    }
}

pub struct DirectTranslator {
    instance: HeliosTranslatorInstanceV1,
    host_callbacks: Box<HeliosTranslatorHostCallbacksV1>,
}

impl DirectTranslator {
    pub fn create(
        luid: i64,
        requested_endpoint_capacity: u32,
        sync_progress_join: helios_protocol::PfnHeliosTranslatorSyncProgressJoin,
        sync_progress_query: helios_protocol::PfnHeliosTranslatorSyncProgressQuery,
    ) -> Result<Self, DirectTranslatorError> {
        let host_callbacks = Box::new(HeliosTranslatorHostCallbacksV1 {
            struct_bytes: HELIOS_TRANSLATOR_HOST_CALLBACKS_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            package_generation: HELIOS_PACKAGE_GENERATION,
            sync_progress_join,
            sync_progress_query,
        });
        host_callbacks
            .validate(HELIOS_PACKAGE_GENERATION)
            .map_err(DirectTranslatorError::Refused)?;

        let split = luid as u64;
        let create_info = HeliosTranslatorCreateInfoV1 {
            struct_bytes: HELIOS_TRANSLATOR_CREATE_INFO_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            package_generation: HELIOS_PACKAGE_GENERATION,
            submission_mode: HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY,
            requested_endpoint_capacity,
            adapter_luid_low: split as u32,
            adapter_luid_high: (split >> 32) as u32 as i32,
            host_callbacks: host_callbacks.as_ref(),
        };
        create_info
            .validate(HELIOS_PACKAGE_GENERATION)
            .map_err(DirectTranslatorError::Refused)?;

        let mut instance = HeliosTranslatorInstanceV1 {
            struct_bytes: HELIOS_TRANSLATOR_INSTANCE_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            handle: core::ptr::null_mut(),
            dispatch: core::ptr::null(),
            vk_instance: core::ptr::null_mut(),
            session_generation: 0,
            endpoint_capacity: 0,
            submission_mode: 0,
        };
        // SAFETY: both exact protocol records remain live across this
        // synchronous call. The entry point is a package import, not a lookup.
        let status = unsafe { helios_icd_create_translator_v1(&create_info, &mut instance) };
        let status = HeliosTranslatorStatus::from_wire(status)
            .map_err(|unknown| DirectTranslatorError::UnknownStatus(unknown.code))?;
        if status != HeliosTranslatorStatus::Ok {
            return Err(DirectTranslatorError::Refused(status));
        }
        instance
            .validate()
            .map_err(DirectTranslatorError::Refused)?;
        // SAFETY: successful instance validation established the non-null table
        // pointer and A5 owns it for this instance's lifetime.
        let dispatch = unsafe { &*instance.dispatch };
        if let Err(refusal) = dispatch.validate(HELIOS_PACKAGE_GENERATION) {
            // A malformed table is not callable, including its apparent final
            // slot. Refuse and deliberately leave cleanup to process rundown.
            return Err(DirectTranslatorError::Refused(refusal));
        }
        if instance.endpoint_capacity > requested_endpoint_capacity {
            Self::destroy_validated(&instance);
            return Err(DirectTranslatorError::EndpointCapacity);
        }
        // SAFETY: address-only module queries do not dereference either pointer.
        let entry_module = unsafe {
            module_for_address(helios_icd_create_translator_v1 as *const () as *const c_void)
        };
        if entry_module.is_null() || entry_module != dispatch.icd_module_base.cast_mut() {
            Self::destroy_validated(&instance);
            return Err(DirectTranslatorError::EntryProvenance);
        }
        // Every callable down edge must resolve to the same imported ICD module.
        if !dispatch_provenance_is_exact(dispatch, entry_module) {
            Self::destroy_validated(&instance);
            return Err(DirectTranslatorError::ProcedureProvenance);
        }

        Ok(Self {
            instance,
            host_callbacks,
        })
    }

    pub fn vk_instance(&self) -> *mut c_void {
        self.instance.vk_instance
    }

    pub fn dispatch(&self) -> &HeliosTranslatorDispatchV1 {
        // SAFETY: construction validated this pointer and `self` owns the A5
        // instance whose lifetime keeps the table live.
        unsafe { &*self.instance.dispatch }
    }

    pub fn module_base(&self) -> *mut c_void {
        self.dispatch().icd_module_base.cast_mut()
    }

    pub fn get_instance_proc_addr(&self) -> *const c_void {
        self.dispatch()
            .get_instance_proc_addr
            .map_or(core::ptr::null(), |proc| proc as *const () as *const c_void)
    }

    pub fn session_generation(&self) -> u64 {
        self.instance.session_generation
    }

    pub fn endpoint_capacity(&self) -> u32 {
        self.instance.endpoint_capacity
    }

    pub fn handle(&self) -> helios_protocol::HeliosTranslatorHandle {
        self.instance.handle
    }

    fn decode_status(status: HeliosTranslatorStatusCode) -> Result<(), DirectTranslatorError> {
        let status = HeliosTranslatorStatus::from_wire(status)
            .map_err(|unknown| DirectTranslatorError::UnknownStatus(unknown.code))?;
        if status == HeliosTranslatorStatus::Ok {
            Ok(())
        } else {
            Err(DirectTranslatorError::Refused(status))
        }
    }

    /// Snapshot the exact immutable endpoint descriptors owned by this A5
    /// instance.  There is no loader enumeration or retry against another
    /// module: a changing count is a refusal of this construction edge.
    pub fn endpoints(&self) -> Result<Vec<HeliosTranslationEndpointV1>, DirectTranslatorError> {
        let Some(enumerate) = self.dispatch().enumerate_endpoints else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let mut count = 0u32;
        Self::decode_status(enumerate(
            self.instance.handle,
            &mut count,
            core::ptr::null_mut(),
            HELIOS_TRANSLATOR_ENDPOINT_BYTES,
        ))?;
        if count > self.instance.endpoint_capacity {
            return Err(DirectTranslatorError::EndpointCapacity);
        }
        let mut endpoints = vec![HeliosTranslationEndpointV1::default(); count as usize];
        if count != 0 {
            let mut written = count;
            Self::decode_status(enumerate(
                self.instance.handle,
                &mut written,
                endpoints.as_mut_ptr(),
                HELIOS_TRANSLATOR_ENDPOINT_BYTES,
            ))?;
            if written != count {
                return Err(DirectTranslatorError::EndpointCapacity);
            }
        }
        Ok(endpoints)
    }

    /// Build the complete HQA1 before the runtime context callback.  The
    /// caller owns `context_generation` and must never reuse it.
    pub fn build_queue_attach(
        &self,
        context_generation: u64,
        endpoint_id: u32,
        engine_class: u32,
        context_flags: u32,
    ) -> Result<HeliosQueueAttachV1, DirectTranslatorError> {
        let Some(build) = self.dispatch().build_queue_attach else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let request = HeliosQueueAttachRequestV1 {
            struct_bytes: HELIOS_TRANSLATOR_QUEUE_ATTACH_REQUEST_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            context_generation,
            endpoint_id,
            engine_class,
            context_flags,
            reserved: 0,
        };
        let mut attach = HeliosQueueAttachV1::default();
        Self::decode_status(build(
            self.instance.handle,
            &request,
            &mut attach,
            HELIOS_TRANSLATOR_HQA1_BYTES,
        ))?;
        Ok(attach)
    }

    pub fn attach_outer_context(
        &self,
        context_generation: u64,
        endpoint_id: u32,
        context_flags: u32,
        host_context_cookie: *mut c_void,
    ) -> Result<(), DirectTranslatorError> {
        let Some(attach) = self.dispatch().attach_outer_context else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let record = HeliosOuterContextAttachV1 {
            struct_bytes: HELIOS_TRANSLATOR_CONTEXT_ATTACH_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            context_generation,
            endpoint_id,
            context_flags,
            host_context_cookie,
        };
        Self::decode_status(attach(self.instance.handle, &record))
    }

    pub fn detach_outer_context(
        &self,
        context_generation: u64,
    ) -> Result<(), DirectTranslatorError> {
        let Some(detach) = self.dispatch().detach_outer_context else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        Self::decode_status(detach(self.instance.handle, context_generation))
    }

    pub fn open_outer_scope(
        &self,
        context_generation: u64,
        endpoint_id: u32,
    ) -> Result<HeliosTranslatorScope, DirectTranslatorError> {
        let Some(open) = self.dispatch().open_outer_scope else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let begin = HeliosOuterScopeBeginV1 {
            struct_bytes: HELIOS_TRANSLATOR_SCOPE_BEGIN_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            context_generation,
            endpoint_id,
            reserved: 0,
        };
        let mut scope = core::ptr::null_mut();
        Self::decode_status(open(self.instance.handle, &begin, &mut scope))?;
        if scope.is_null() {
            return Err(DirectTranslatorError::Refused(
                HeliosTranslatorStatus::NullArgument,
            ));
        }
        Ok(scope)
    }

    pub fn seal_and_copy(
        &self,
        scope: HeliosTranslatorScope,
    ) -> Result<DirectSealedBatch, DirectTranslatorError> {
        let Some(seal) = self.dispatch().seal_outer_scope else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let Some(copy) = self.dispatch().copy_sealed_batch else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let mut descriptor = HeliosSealedBatchV1 {
            struct_bytes: HELIOS_TRANSLATOR_SEALED_BATCH_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            ..Default::default()
        };
        Self::decode_status(seal(scope, &mut descriptor))?;
        let payload_len = usize::try_from(descriptor.payload_bytes)
            .map_err(|_| DirectTranslatorError::EndpointCapacity)?;
        let mut payload = vec![0u8; payload_len];
        let mut uses = vec![HeliosSealedResourceUseV1::default(); descriptor.use_count as usize];
        let mut operands =
            vec![HeliosSealedOperandV1::default(); descriptor.operand_count as usize];
        let destination = HeliosSealedBatchCopyV1 {
            struct_bytes: HELIOS_TRANSLATOR_SEALED_BATCH_COPY_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            payload: payload.as_mut_ptr().cast(),
            payload_capacity: descriptor.payload_bytes,
            uses: uses.as_mut_ptr(),
            use_capacity: descriptor.use_count,
            reserved0: 0,
            operands: operands.as_mut_ptr(),
            operand_capacity: descriptor.operand_count,
            reserved1: 0,
        };
        Self::decode_status(copy(scope, &destination))?;
        Ok(DirectSealedBatch {
            descriptor,
            payload,
            uses,
            operands,
        })
    }

    /// Convert one immutable A5 seal into one complete HOB1.  The resolver is
    /// invoked exactly once per unique sealed token and supplies only the two
    /// WDDM identity fields that differ between the D3D11 and D3D12 arms.
    /// Every other field is written explicitly from the seal.
    pub fn encode_hob1<F>(
        &self,
        batch: &DirectSealedBatch,
        context: DirectHobContext,
        mut resolve: F,
    ) -> Result<DirectHob1, DirectHobError>
    where
        F: FnMut(
            u32,
            &HeliosSealedResourceUseV1,
        ) -> Result<DirectResolvedUse, DirectIdentityRefusal>,
    {
        batch
            .descriptor
            .validate(
                HELIOS_PACKAGE_GENERATION,
                self.session_generation(),
                context.context_generation,
                context.endpoint_id,
                context.context_flags,
            )
            .map_err(DirectHobError::Translator)?;

        if batch.payload.len() as u64 != batch.descriptor.payload_bytes
            || u32::try_from(batch.payload.len()).is_err()
        {
            return Err(DirectHobError::PayloadLength);
        }
        if batch.uses.len() != batch.descriptor.use_count as usize {
            return Err(DirectHobError::UseTableLength);
        }
        if batch.operands.len() != batch.descriptor.operand_count as usize {
            return Err(DirectHobError::OperandTableLength);
        }
        if crc64_ecma(&batch.payload) != batch.descriptor.payload_crc64 {
            return Err(DirectHobError::PayloadCrc);
        }

        let header_bytes = HELIOS_HOB1_HEADER_BYTES as u64;
        let use_bytes = (batch.descriptor.use_count as u64)
            .checked_mul(HELIOS_HOB1_USE_RECORD_BYTES as u64)
            .ok_or(DirectHobError::LayoutOverflow)?;
        let operand_bytes = (batch.descriptor.operand_count as u64)
            .checked_mul(HELIOS_HOB1_OPERAND_RECORD_BYTES as u64)
            .ok_or(DirectHobError::LayoutOverflow)?;
        let use_offset = (batch.descriptor.use_count != 0).then_some(header_bytes);
        let after_uses = header_bytes
            .checked_add(use_bytes)
            .ok_or(DirectHobError::LayoutOverflow)?;
        let operand_offset = (batch.descriptor.operand_count != 0).then_some(after_uses);
        let after_operands = after_uses
            .checked_add(operand_bytes)
            .ok_or(DirectHobError::LayoutOverflow)?;
        let payload_offset = align_up(after_operands, HELIOS_HOB1_OFFSET_ALIGNMENT)
            .ok_or(DirectHobError::LayoutOverflow)?;
        let total_bytes = payload_offset
            .checked_add(batch.descriptor.payload_bytes)
            .ok_or(DirectHobError::LayoutOverflow)?;
        if total_bytes > HELIOS_HOB1_MAX_BYTES
            || total_bytes > context.max_command_bytes
            || total_bytes > u32::MAX as u64
        {
            return Err(DirectHobError::LayoutOverflow);
        }

        let identity_kind = match context.context_flags {
            helios_protocol::HELIOS_HQA1_FLAG_D3D11_PHYSICAL => {
                HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX
            }
            helios_protocol::HELIOS_HQA1_FLAG_D3D12_VIRTUAL => HELIOS_HOB1_IDENTITY_D3D12_GPUVA,
            _ => {
                return Err(DirectHobError::Translator(
                    HeliosTranslatorStatus::ContextFlags,
                ))
            }
        };

        let mut uses = Vec::with_capacity(batch.uses.len());
        for (index, sealed) in batch.uses.iter().enumerate() {
            let index = index as u32;
            if sealed.outer_allocation_token == 0 {
                return Err(DirectHobError::Identity {
                    use_index: index,
                    reason: DirectIdentityRefusal::ZeroToken,
                });
            }
            let identity = resolve(index, sealed).map_err(|reason| DirectHobError::Identity {
                use_index: index,
                reason,
            })?;
            if identity_kind == HELIOS_HOB1_IDENTITY_D3D12_GPUVA && identity.address_or_index == 0 {
                return Err(DirectHobError::Identity {
                    use_index: index,
                    reason: DirectIdentityRefusal::AddressZero,
                });
            }
            if identity.allocation_generation == 0 {
                return Err(DirectHobError::Identity {
                    use_index: index,
                    reason: DirectIdentityRefusal::StaleGeneration,
                });
            }
            uses.push(HeliosOuterBatchUseV1 {
                address_or_index: identity.address_or_index,
                byte_length: sealed.byte_length,
                expected_allocation_generation: identity.allocation_generation,
                access_flags: sealed.access_flags,
                identity_kind,
                operand_count: sealed.operand_count,
                first_operand: sealed.first_operand,
                reserved: 0,
            });
        }

        let mut operands = Vec::with_capacity(batch.operands.len());
        for sealed in &batch.operands {
            let absolute = u64::from(sealed.payload_relative_offset)
                .checked_add(payload_offset)
                .and_then(|offset| u32::try_from(offset).ok())
                .ok_or(DirectHobError::LayoutOverflow)?;
            operands.push(HeliosOuterBatchOperandV1 {
                payload_offset: absolute,
                use_index: sealed.use_index,
                operand_kind: sealed.operand_kind,
                encoded_width: sealed.encoded_width,
                reserved: 0,
            });
        }

        let mut header = HeliosOuterBatchV1 {
            magic: HELIOS_HOB1_MAGIC,
            abi_version: HELIOS_HOB1_ABI_VERSION,
            header_size: HELIOS_HOB1_HEADER_BYTES,
            package_generation: batch.descriptor.package_generation,
            session_generation: batch.descriptor.session_generation,
            context_generation: batch.descriptor.context_generation,
            batch_id: batch.descriptor.batch_id,
            endpoint_id: batch.descriptor.endpoint_id,
            flags: batch.descriptor.context_flags,
            total_bytes,
            payload_offset: payload_offset as u32,
            payload_bytes: batch.payload.len() as u32,
            use_offset: use_offset.unwrap_or(0) as u32,
            use_count: batch.descriptor.use_count,
            operand_offset: operand_offset.unwrap_or(0) as u32,
            operand_count: batch.descriptor.operand_count,
            crc64: 0,
            reserved: [0; 24],
        };

        let byte_len = usize::try_from(total_bytes).map_err(|_| DirectHobError::LayoutOverflow)?;
        let word_len = byte_len
            .checked_add(core::mem::size_of::<u64>() - 1)
            .ok_or(DirectHobError::LayoutOverflow)?
            / core::mem::size_of::<u64>();
        let mut words = vec![0u64; word_len];
        // SAFETY: the checked layout bounds every destination wholly inside the
        // word storage. Sources are live, non-overlapping POD records/bytes.
        unsafe {
            let base = words.as_mut_ptr().cast::<u8>();
            core::ptr::copy_nonoverlapping(
                (&header as *const HeliosOuterBatchV1).cast::<u8>(),
                base,
                core::mem::size_of::<HeliosOuterBatchV1>(),
            );
            if let Some(offset) = use_offset {
                core::ptr::copy_nonoverlapping(
                    uses.as_ptr().cast::<u8>(),
                    base.add(offset as usize),
                    use_bytes as usize,
                );
            }
            if let Some(offset) = operand_offset {
                core::ptr::copy_nonoverlapping(
                    operands.as_ptr().cast::<u8>(),
                    base.add(offset as usize),
                    operand_bytes as usize,
                );
            }
            core::ptr::copy_nonoverlapping(
                batch.payload.as_ptr(),
                base.add(payload_offset as usize),
                batch.payload.len(),
            );
        }
        let record = unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), byte_len) };
        header.crc64 = hob1_record_crc64(record).map_err(DirectHobError::Record)?;
        // SAFETY: same exact header-sized destination as the first copy.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&header as *const HeliosOuterBatchV1).cast::<u8>(),
                words.as_mut_ptr().cast::<u8>(),
                core::mem::size_of::<HeliosOuterBatchV1>(),
            );
        }

        let expectation = HeliosOuterBatchExpectation {
            package_generation: HELIOS_PACKAGE_GENERATION,
            session_generation: self.session_generation(),
            context_generation: context.context_generation,
            endpoint_id: context.endpoint_id,
            flags: context.context_flags,
            max_command_bytes: context.max_command_bytes,
            last_batch_id: context.last_batch_id,
            allocation_list_count: context.allocation_list_count,
        };
        let record = unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), byte_len) };
        validate_batch_record(record, &expectation).map_err(DirectHobError::Record)?;

        Ok(DirectHob1 {
            words,
            byte_len,
            header,
        })
    }

    pub fn close_outer_scope(
        &self,
        scope: HeliosTranslatorScope,
        committed_progress: Option<u64>,
    ) -> Result<(), DirectTranslatorError> {
        let Some(close) = self.dispatch().close_outer_scope else {
            return Err(DirectTranslatorError::ProcedureProvenance);
        };
        let record = HeliosOuterScopeCloseV1 {
            struct_bytes: HELIOS_TRANSLATOR_SCOPE_CLOSE_BYTES,
            abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
            disposition: if committed_progress.is_some() {
                HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED
            } else {
                HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED
            },
            reserved: 0,
            progress_value: committed_progress.unwrap_or(0),
        };
        Self::decode_status(close(scope, &record))
    }

    fn destroy_validated(instance: &HeliosTranslatorInstanceV1) {
        if instance.handle.is_null() || instance.dispatch.is_null() {
            return;
        }
        // SAFETY: every caller has validated the complete fixed dispatch table.
        if let Some(destroy) = unsafe { (*instance.dispatch).destroy_instance } {
            let _ = destroy(instance.handle);
        }
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return None;
    }
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
}

impl Drop for DirectTranslator {
    fn drop(&mut self) {
        let _keep_callbacks_live = &self.host_callbacks;
        if let Some(destroy) = self.dispatch().destroy_instance {
            let _ = destroy(self.instance.handle);
        }
        self.instance.handle = core::ptr::null_mut();
        self.instance.dispatch = core::ptr::null();
        self.instance.vk_instance = core::ptr::null_mut();
    }
}

unsafe fn module_for_address(address: *const c_void) -> *mut c_void {
    let mut module = core::ptr::null_mut();
    // SAFETY: FROM_ADDRESS interprets the second argument as an address only;
    // UNCHANGED_REFCOUNT makes this a compare-only query.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address.cast(),
            &mut module,
        )
    };
    if ok == 0 {
        core::ptr::null_mut()
    } else {
        module
    }
}

fn dispatch_provenance_is_exact(
    dispatch: &HeliosTranslatorDispatchV1,
    expected_module: *mut c_void,
) -> bool {
    macro_rules! exact {
        ($slot:expr) => {{
            let Some(proc) = $slot else { return false };
            // SAFETY: this is an address-only module query.
            unsafe { module_for_address(proc as *const () as *const c_void) == expected_module }
        }};
    }

    exact!(dispatch.get_instance_proc_addr)
        && exact!(dispatch.enumerate_endpoints)
        && exact!(dispatch.build_queue_attach)
        && exact!(dispatch.attach_outer_context)
        && exact!(dispatch.detach_outer_context)
        && exact!(dispatch.open_outer_scope)
        && exact!(dispatch.seal_outer_scope)
        && exact!(dispatch.copy_sealed_batch)
        && exact!(dispatch.close_outer_scope)
        && exact!(dispatch.query_refusal_counters)
        && exact!(dispatch.destroy_instance)
}
