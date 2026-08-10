//! K6's decode gate: replay A2's REAL encoder output through the KMD's own
//! per-context assembler.
//!
//! `tools/hnr2_encode_probe.c` links the ICD's own encoder
//! (`icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c`) and writes every command
//! buffer it emits to a corpus file. `protocol/tests/hnr2_encoder_gate.rs`
//! already replays that corpus through `protocol`'s validators — but it hand-rolls
//! the per-context state machine as test locals, so it proves the WIRE is right
//! and proves nothing about the state machine the KMD will actually run.
//!
//! This test drives `helios_kmd_logic::native_render::RenderContext` instead, and
//! adds the three things only the KMD side can check: that the staging pool
//! admits the batch, that the output patch plan is one slot per use and is
//! repeatable, and that a use record's access agrees with the
//! `DXGK_ALLOCATIONLIST` entry it names — the check `protocol` structurally
//! cannot make.
//!
//! ⛔ Every record below is decoded **by explicit §10.7 offset**, never through a
//! shared derive. That is deliberate, and it is K5's precedent
//! (`hts1_attach_gate.rs`): a gate that decodes with the same `bytemuck` impl
//! both halves share inherits the agreement it is supposed to be testing.
//!
//! Run by `tools/hnr2-decode-gate.sh`. ⛔ Without `HELIOS_HNR2_CORPUS` this test
//! SKIPS, so the runner checks the counts it prints — a gate that can pass on an
//! empty corpus is not a gate.
//!
//! # The two defeats a single-pass corpus replay cannot catch, and how they are
//!
//! Both were measured succeeding against the first version of this gate, and
//! neither needed a change to the producer:
//!
//! * **Substituting the advertised `HELIOS_HVC1_DMA_BUFFER_BYTES` constant for
//!   the `DmaSize` dxgkrnl actually returned** is invisible when every corpus
//!   batch is encoded into exactly that constant. Closed by replaying each
//!   batch's first fragment against a buffer one byte short and requiring the
//!   refusal — a real Render can be handed a smaller buffer, and §10.7:1821
//!   makes that a refusal.
//! * **Two contexts sharing one assembler** is invisible when every batch closes
//!   before the next opens. Closed by interleaving the two DEEPEST multi-fragment
//!   batches across two `RenderContext`s: the second BEGIN then lands while the
//!   first batch is still open, which a shared assembler refuses with
//!   `BatchAlreadyOpen`. This is the isolation K6's per-context design exists
//!   for, and a single-context replay never touches it.

use helios_kmd_logic::native_render::{
    admit_commit_tables, admit_render_fragment, plan_output_patch_slots, RenderContext, RenderEnv,
};
use helios_protocol::native_render::{
    HeliosNativeRenderPatch, HeliosNativeRenderUse, HeliosNativeRenderV2, HELIOS_HNR2_ACCESS_WRITE,
    HELIOS_HNR2_HEADER_SIZE, HELIOS_HNR2_PATCH_RECORD_SIZE, HELIOS_HNR2_USE_RECORD_SIZE,
    HELIOS_HVC1_DMA_BUFFER_BYTES,
};
use helios_protocol::HELIOS_PACKAGE_GENERATION;

const CORPUS_MAGIC: u32 = 0x5052_4E48;
const CORPUS_VERSION: u32 = 1;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        let end = self.at + n;
        assert!(end <= self.bytes.len(), "corpus truncated at {}", self.at);
        let out = &self.bytes[self.at..end];
        self.at = end;
        out
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take(2).try_into().unwrap())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }
}

/// Read a little-endian scalar at an explicit byte offset. The whole point is
/// that these offsets are transcribed from the §10.7 field table, not shared
/// with the producer.
fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

/// §10.7's HNR2 header, by offset.
fn decode_header(b: &[u8]) -> HeliosNativeRenderV2 {
    assert!(b.len() >= HELIOS_HNR2_HEADER_SIZE as usize, "short header");
    HeliosNativeRenderV2 {
        magic: u32_at(b, 0),
        abi_version: u16_at(b, 4),
        header_size: u16_at(b, 6),
        package_generation: u64_at(b, 8),
        batch_token: u64_at(b, 16),
        total_payload_bytes: u64_at(b, 24),
        fragment_payload_offset: u64_at(b, 32),
        fragment_payload_bytes: u32_at(b, 40),
        fragment_index: u16_at(b, 44),
        fragment_count: u16_at(b, 46),
        use_record_offset: u32_at(b, 48),
        use_record_count: u32_at(b, 52),
        patch_record_offset: u32_at(b, 56),
        patch_record_count: u32_at(b, 60),
        reply_allocation_list_index: u32_at(b, 64),
        flags: u32_at(b, 68),
        reply_offset: u64_at(b, 72),
        reply_capacity_bytes: u64_at(b, 80),
        fragment_crc64: u64_at(b, 88),
        full_payload_crc64: u64_at(b, 96),
        reply_slot_generation: u64_at(b, 104),
    }
}

/// §10.7's 24-byte use record, by offset.
fn decode_use(b: &[u8]) -> HeliosNativeRenderUse {
    HeliosNativeRenderUse {
        allocation_list_index: u32_at(b, 0),
        access_flags: u32_at(b, 4),
        expected_allocation_generation: u64_at(b, 8),
        first_patch: u32_at(b, 16),
        patch_count: u32_at(b, 20),
    }
}

/// §10.7's 16-byte typed-operand record, by offset.
fn decode_patch(b: &[u8]) -> HeliosNativeRenderPatch {
    HeliosNativeRenderPatch {
        payload_offset: u32_at(b, 0),
        allocation_list_index: u32_at(b, 4),
        operand_kind: u16_at(b, 8),
        encoded_width: u16_at(b, 10),
        reserved: u32_at(b, 12),
    }
}

#[test]
fn the_kmd_assembler_admits_every_batch_the_icd_encoder_emits() {
    let Ok(path) = std::env::var("HELIOS_HNR2_CORPUS") else {
        println!("HNR2 decode: SKIPPED (HELIOS_HNR2_CORPUS unset; run tools/hnr2-decode-gate.sh)");
        return;
    };
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut r = Reader::new(&data);

    assert_eq!(r.u32(), CORPUS_MAGIC, "corpus magic");
    assert_eq!(r.u32(), CORPUS_VERSION, "corpus version");
    let batch_count = r.u32();

    // ONE context for the whole corpus, exactly as a real HVC1 control context
    // would be: the token watermark and the staging pool thread across batches,
    // which is the state `protocol`'s own gate keeps in test locals instead.
    let mut ctx = RenderContext::new();
    let mut fragments = 0u32;
    let mut commits = 0u32;
    let mut patch_slots = 0u32;
    let mut write_mutations = 0u32;
    let mut short_buffer_cases = 0u32;
    let mut multi_fragment: Vec<(u64, Vec<(u32, Vec<u8>)>)> = Vec::new();

    for batch in 0..batch_count {
        let payload_bytes = r.u64();
        let allocation_count = r.u32();
        let patch_count = r.u32();
        let fragment_count = r.u32();
        let _has_reply = r.u32() != 0;
        let _reply_allocation_index = r.u32();
        let _reply_offset = r.u64();
        let _reply_capacity_bytes = r.u64();
        let _reply_slot_generation = r.u64();
        let batch_token = r.u64();
        let _payload = r.take(payload_bytes as usize);

        // The allocation list the runtime would hand `DxgkDdiRender`: the access
        // bits are the corpus's own, so the `WriteOperation` slice below is the
        // producer's declared intent rather than something this test invents.
        let mut list_write = Vec::with_capacity(allocation_count as usize);
        for _ in 0..allocation_count {
            let _handle = r.u32();
            let access = r.u32();
            let _generation = r.u64();
            list_write.push(access & HELIOS_HNR2_ACCESS_WRITE != 0);
        }
        for _ in 0..patch_count {
            let _payload_offset = r.u32();
            let _allocation_index = r.u32();
            let _operand_kind = r.u16();
            assert_eq!(r.u16(), 0, "batch {batch}: patch padding");
        }

        let ctx_name = format!("batch {batch} (token {batch_token})");
        let mut collected: Vec<(u32, Vec<u8>)> = Vec::with_capacity(fragment_count as usize);
        let staged_before = ctx.staging().staged_bytes();
        ctx.staging_mut()
            .checkout(payload_bytes)
            .unwrap_or_else(|e| panic!("{ctx_name}: staging refused {e:?}"));

        for f in 0..fragment_count {
            let command_length = r.u32();
            let render_allocation_count = r.u32();
            let command = r.take(command_length as usize);
            collected.push((render_allocation_count, command.to_vec()));
            let header = decode_header(command);

            let env = RenderEnv {
                package_generation: HELIOS_PACKAGE_GENERATION,
                allocation_list_count: render_allocation_count,
                command_length,
                command_buffer_bytes: HELIOS_HVC1_DMA_BUFFER_BYTES,
                patch_location_list_in_size: 0,
            };
            let accept = admit_render_fragment(&mut ctx, &header, &env)
                .unwrap_or_else(|e| panic!("{ctx_name} fragment {f}: refused {e:?}"));
            fragments += 1;

            assert_eq!(
                accept.class.is_commit(),
                f + 1 == fragment_count,
                "{ctx_name} fragment {f}: COMMIT position"
            );
            assert_eq!(
                ctx.open_batch().is_some(),
                f + 1 != fragment_count,
                "{ctx_name} fragment {f}: the batch closes exactly at COMMIT"
            );
            assert_eq!(
                ctx.last_batch_token(),
                batch_token,
                "{ctx_name} fragment {f}: watermark"
            );

            if !accept.class.is_commit() {
                // A non-COMMIT fragment plans nothing: it carries no use table.
                assert_eq!(
                    plan_output_patch_slots(&[], 4096).unwrap().count,
                    0,
                    "{ctx_name} fragment {f}: a non-COMMIT fragment owns no patch slots"
                );
                continue;
            }
            commits += 1;

            let layout = accept.layout;
            let mut uses = Vec::with_capacity(header.use_record_count as usize);
            for i in 0..header.use_record_count as usize {
                let at =
                    layout.use_record_offset as usize + i * HELIOS_HNR2_USE_RECORD_SIZE as usize;
                uses.push(decode_use(&command[at..at + HELIOS_HNR2_USE_RECORD_SIZE as usize]));
            }
            let mut patches = Vec::with_capacity(header.patch_record_count as usize);
            for i in 0..header.patch_record_count as usize {
                let at = layout.patch_record_offset as usize
                    + i * HELIOS_HNR2_PATCH_RECORD_SIZE as usize;
                patches.push(decode_patch(
                    &command[at..at + HELIOS_HNR2_PATCH_RECORD_SIZE as usize],
                ));
            }

            admit_commit_tables(
                &header,
                &uses,
                &patches,
                render_allocation_count,
                &list_write,
            )
            .unwrap_or_else(|e| panic!("{ctx_name}: COMMIT tables refused {e:?}"));

            // ⛔ WITHOUT THIS THE WRITE-OPERATION CHECK IS TAUTOLOGICAL. The
            // `list_write` slice above is derived from the corpus's own access
            // words, so a gate that only asserts "it was admitted" would pass
            // just as happily with `validate_use_write_operation` deleted. Flip
            // one entry and require the refusal — per entry, because a loop that
            // stops early is exactly the defect this rule exists to catch.
            for i in 0..list_write.len() {
                let mut mutated = list_write.clone();
                mutated[i] = !mutated[i];
                let refused = admit_commit_tables(
                    &header,
                    &uses,
                    &patches,
                    render_allocation_count,
                    &mutated,
                );
                assert!(
                    refused.is_err(),
                    "{ctx_name}: allocation-list entry {i} disagreeing with its use record \
                     was admitted"
                );
                write_mutations += 1;
            }

            // §18.2 gate 3: "Patch invoked twice" must produce the same record.
            let plan = plan_output_patch_slots(&uses, 4096)
                .unwrap_or_else(|e| panic!("{ctx_name}: patch plan refused {e:?}"));
            assert_eq!(
                plan,
                plan_output_patch_slots(&uses, 4096).unwrap(),
                "{ctx_name}: the patch plan must be repeatable"
            );
            assert_eq!(
                plan.count,
                uses.len() as u32,
                "{ctx_name}: one output patch slot per USE record, not per typed operand \
                 (this batch has {} operands)",
                patches.len()
            );
            for i in 0..plan.count {
                let (index, ordinal) = plan.entry(&uses, i).expect("plan entry in range");
                assert_eq!(
                    index, uses[i as usize].allocation_list_index,
                    "{ctx_name}: plan entry {i} names its use record's allocation"
                );
                assert_eq!(ordinal, i, "{ctx_name}: capability ordinal is the slot index");
            }
            patch_slots += plan.count;

            // A short patch-location list is a refusal, never a truncated plan.
            if plan.count > 0 {
                assert!(
                    plan_output_patch_slots(&uses, plan.count - 1).is_err(),
                    "{ctx_name}: a patch list one entry short must be refused"
                );
            }
        }

        // ⛔ THE `DmaSize` ARM, which the corpus alone cannot reach: every batch is
        // encoded into exactly `HELIOS_HVC1_DMA_BUFFER_BYTES`, so substituting
        // that constant for the buffer dxgkrnl actually returned is invisible. A
        // real Render can be handed a SMALLER buffer, and §10.7:1821 makes that a
        // refusal. Replay the first fragment against a buffer one byte short.
        {
            let (alloc_count, command) = &collected[0];
            let header = decode_header(command);
            let mut fresh = RenderContext::new();
            let short = RenderEnv {
                package_generation: HELIOS_PACKAGE_GENERATION,
                allocation_list_count: *alloc_count,
                command_length: command.len() as u32,
                command_buffer_bytes: command.len() as u32 - 1,
                patch_location_list_in_size: 0,
            };
            assert!(
                admit_render_fragment(&mut fresh, &header, &short).is_err(),
                "{ctx_name}: a command longer than the returned DMA buffer was admitted"
            );
            short_buffer_cases += 1;
        }

        if fragment_count >= 2 {
            multi_fragment.push((batch_token, collected));
        }

        ctx.staging_mut()
            .retire(payload_bytes)
            .unwrap_or_else(|e| panic!("{ctx_name}: staging retire refused {e:?}"));
        assert_eq!(
            ctx.staging().staged_bytes(),
            staged_before,
            "{ctx_name}: the staging pool must return to its prior level"
        );
    }

    // ⛔ TWO CONTEXTS, FRAGMENTS INTERLEAVED. A single-context corpus cannot tell
    // a per-context assembler from a process-global one: every batch closes before
    // the next opens, so a shared assembler works. Interleaving two multi-fragment
    // batches makes the difference observable — the second BEGIN lands while the
    // first batch is still open, which a shared assembler refuses with
    // `BatchAlreadyOpen`. This is the isolation K6's whole per-context design
    // exists for, and it was untested until this pass.
    assert!(
        multi_fragment.len() >= 2,
        "the corpus must carry at least two multi-fragment batches for the \
         interleave pass; found {}",
        multi_fragment.len()
    );
    // The two DEEPEST, so the interleave spans as many fragments as the corpus
    // offers rather than whichever two happened to come first.
    multi_fragment.sort_by_key(|(_, frags)| core::cmp::Reverse(frags.len()));
    multi_fragment.truncate(2);
    let mut ctx_a = RenderContext::new();
    let mut ctx_b = RenderContext::new();
    let deepest = multi_fragment[0].1.len().max(multi_fragment[1].1.len());
    let mut interleaved = 0u32;
    for step in 0..deepest {
        for (which, ctx) in [(0usize, &mut ctx_a), (1usize, &mut ctx_b)] {
            let Some((alloc_count, command)) = multi_fragment[which].1.get(step) else {
                continue;
            };
            let header = decode_header(command);
            let env = RenderEnv {
                package_generation: HELIOS_PACKAGE_GENERATION,
                allocation_list_count: *alloc_count,
                command_length: command.len() as u32,
                command_buffer_bytes: HELIOS_HVC1_DMA_BUFFER_BYTES,
                patch_location_list_in_size: 0,
            };
            admit_render_fragment(ctx, &header, &env).unwrap_or_else(|e| {
                panic!(
                    "interleave: context {which} fragment {step} refused {e:?} — two \
                     contexts are sharing one assembler"
                )
            });
            interleaved += 1;
        }
    }
    assert_eq!(ctx_a.open_batch(), None, "interleave: context A left a batch open");
    assert_eq!(ctx_b.open_batch(), None, "interleave: context B left a batch open");
    assert_eq!(
        ctx_a.last_batch_token(),
        multi_fragment[0].0,
        "interleave: context A's watermark is its own batch's token"
    );
    assert_eq!(
        ctx_b.last_batch_token(),
        multi_fragment[1].0,
        "interleave: context B's watermark is its own batch's token"
    );

    assert_eq!(r.at, data.len(), "corpus has trailing bytes");
    assert_eq!(ctx.open_batch(), None, "a batch was left open at the end");
    assert_eq!(ctx.staging().outstanding(), 0, "a staging slot leaked");

    println!(
        "HNR2 decode: {batch_count} batches, {fragments} fragments, {commits} commits, \
         {patch_slots} output patch slots planned, {write_mutations} write-bit mutations refused, \
         {short_buffer_cases} short-DMA-buffer refusals, {interleaved} interleaved fragments"
    );
}
