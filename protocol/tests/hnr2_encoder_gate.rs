//! A2's encoder gate, validator half.
//!
//! `tools/hnr2_encode_probe.c` links the ICD's own HNR2 encoder
//! (`icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c`), encodes a corpus of
//! batches, and writes every emitted command buffer to a file. This replays that
//! file through the validator the KMD will run — [`HeliosNativeRenderV2::validate`]
//! and [`validate_commit_tables`] — with the same per-context state machine, plus
//! the checks a validator cannot make because it never sees the encoder's input:
//! that the use table is the manifest, that the patch runs are a permutation of
//! the caller's patches, and that the fragments reassemble to the exact payload.
//!
//! ⭐ This is the ONLY place a Helios ICD source file is executed by a test.
//! `tools/hnr2-encoder-gate.sh` is the runner; without `HELIOS_HNR2_CORPUS` the
//! test skips, and the runner fails if the counts it prints are too small to be
//! a real corpus.

use helios_protocol::native_render::*;
use helios_protocol::wddm::crc64_ecma;
use helios_protocol::HELIOS_PACKAGE_GENERATION;

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        let end = self.offset + n;
        assert!(end <= self.bytes.len(), "corpus truncated at {}", self.offset);
        let out = &self.bytes[self.offset..end];
        self.offset = end;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PatchInput {
    payload_offset: u32,
    allocation_index: u32,
    operand_kind: u16,
}

const CORPUS_MAGIC: u32 = 0x5052_4E48;
const CORPUS_VERSION: u32 = 1;

#[test]
fn hnr2_encoder_corpus_validates() {
    let Ok(path) = std::env::var("HELIOS_HNR2_CORPUS") else {
        println!("HNR2 corpus: SKIPPED (HELIOS_HNR2_CORPUS unset; run tools/hnr2-encoder-gate.sh)");
        return;
    };
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut r = Reader::new(&data);

    assert_eq!(r.u32(), CORPUS_MAGIC, "corpus magic");
    assert_eq!(r.u32(), CORPUS_VERSION, "corpus version");
    let batch_count = r.u32();

    // One virtual context for the whole corpus: the probe hands out strictly
    // increasing tokens, so `last_batch_token` threads across batches exactly as
    // it would on a real context.
    let mut last_batch_token = 0u64;
    let mut fragments_validated = 0u32;

    for batch in 0..batch_count {
        let payload_bytes = r.u64();
        let allocation_count = r.u32();
        let patch_count = r.u32();
        let fragment_count = r.u32();
        let has_reply = r.u32() != 0;
        let reply_allocation_index = r.u32();
        let reply_offset = r.u64();
        let reply_capacity_bytes = r.u64();
        let reply_slot_generation = r.u64();
        let batch_token = r.u64();
        let payload = r.take(payload_bytes as usize).to_vec();

        let mut allocations = Vec::with_capacity(allocation_count as usize);
        for _ in 0..allocation_count {
            let handle = r.u32();
            let access = r.u32();
            let generation = r.u64();
            allocations.push((handle, access, generation));
        }
        let mut patch_inputs = Vec::with_capacity(patch_count as usize);
        for _ in 0..patch_count {
            let payload_offset = r.u32();
            let allocation_index = r.u32();
            let operand_kind = r.u16();
            assert_eq!(r.u16(), 0, "batch {batch}: patch padding must be zero");
            patch_inputs.push(PatchInput {
                payload_offset,
                allocation_index,
                operand_kind,
            });
        }

        let ctx = format!("batch {batch} (token {batch_token})");
        assert!(fragment_count >= 1, "{ctx}: no fragments");
        assert!(
            fragment_count <= HELIOS_HNR2_MAX_FRAGMENTS as u32,
            "{ctx}: {fragment_count} fragments is above the §10.7 bound"
        );

        let mut open: Option<Hnr2OpenBatch> = None;
        let mut reassembled: Vec<u8> = Vec::with_capacity(payload_bytes as usize);

        for f in 0..fragment_count {
            let command_length = r.u32();
            let render_allocation_count = r.u32();
            let command = r.take(command_length as usize);

            let want_begin = f == 0;
            let want_commit = f + 1 == fragment_count;
            assert_eq!(
                render_allocation_count,
                if want_commit { allocation_count } else { 0 },
                "{ctx} fragment {f}: D3DKMT_RENDER::AllocationCount"
            );

            let header: HeliosNativeRenderV2 =
                bytemuck::pod_read_unaligned(&command[..HELIOS_HNR2_HEADER_SIZE as usize]);

            let expect = Hnr2Expect {
                package_generation: HELIOS_PACKAGE_GENERATION,
                allocation_list_count: render_allocation_count,
                command_length,
                command_buffer_bytes: HELIOS_HVC1_DMA_BUFFER_BYTES,
                patch_location_list_in_size: 0,
                open,
                last_batch_token,
            };
            let accept = header
                .validate(&expect)
                .unwrap_or_else(|e| panic!("{ctx} fragment {f}: validate rejected {e:?}"));

            assert_eq!(header.batch_token, batch_token, "{ctx} fragment {f}: token");
            assert_eq!(accept.class.is_begin(), want_begin, "{ctx} fragment {f}: BEGIN");
            assert_eq!(accept.class.is_commit(), want_commit, "{ctx} fragment {f}: COMMIT");
            assert_eq!(
                accept.has_reply,
                has_reply && want_commit,
                "{ctx} fragment {f}: HAS_REPLY belongs to the COMMIT alone"
            );
            assert_eq!(
                header.total_payload_bytes, payload_bytes,
                "{ctx} fragment {f}: total payload"
            );

            let layout = accept.layout;
            let start = layout.payload_offset as usize;
            let slice = &command[start..start + layout.payload_bytes as usize];
            assert_eq!(
                header.fragment_crc64,
                crc64_ecma(slice),
                "{ctx} fragment {f}: fragment CRC is this fragment's payload slice"
            );
            assert_eq!(
                header.fragment_payload_offset,
                reassembled.len() as u64,
                "{ctx} fragment {f}: payload offset"
            );
            reassembled.extend_from_slice(slice);

            if want_commit {
                assert_eq!(
                    header.full_payload_crc64,
                    crc64_ecma(&payload),
                    "{ctx}: full payload CRC"
                );
                assert_eq!(
                    header.use_record_count, allocation_count,
                    "{ctx}: use record count"
                );
                assert_eq!(header.patch_record_count, patch_count, "{ctx}: patch count");

                let mut uses = Vec::with_capacity(allocation_count as usize);
                for i in 0..allocation_count as usize {
                    let at = layout.use_record_offset as usize
                        + i * HELIOS_HNR2_USE_RECORD_SIZE as usize;
                    uses.push(bytemuck::pod_read_unaligned::<HeliosNativeRenderUse>(
                        &command[at..at + HELIOS_HNR2_USE_RECORD_SIZE as usize],
                    ));
                }
                let mut patches = Vec::with_capacity(patch_count as usize);
                for i in 0..patch_count as usize {
                    let at = layout.patch_record_offset as usize
                        + i * HELIOS_HNR2_PATCH_RECORD_SIZE as usize;
                    patches.push(bytemuck::pod_read_unaligned::<HeliosNativeRenderPatch>(
                        &command[at..at + HELIOS_HNR2_PATCH_RECORD_SIZE as usize],
                    ));
                }

                validate_commit_tables(&header, &uses, &patches, render_allocation_count)
                    .unwrap_or_else(|e| panic!("{ctx}: commit tables rejected {e:?}"));

                // What the validator cannot know: the tables must be the
                // caller's manifest, not merely a self-consistent one.
                for (i, (_handle, access, generation)) in allocations.iter().enumerate() {
                    let u = uses[i];
                    assert_eq!(u.allocation_list_index, i as u32, "{ctx}: use {i} index");
                    assert_eq!(u.access_flags, *access, "{ctx}: use {i} access");
                    assert_eq!(
                        u.expected_allocation_generation, *generation,
                        "{ctx}: use {i} generation"
                    );
                    // The ICD builds the D3DDDI_ALLOCATIONLIST entry's
                    // WriteOperation from the same access bit.
                    validate_use_write_operation(&u, access & HELIOS_HNR2_ACCESS_WRITE != 0)
                        .unwrap_or_else(|e| panic!("{ctx}: use {i} write operation {e:?}"));
                }

                let mut want: Vec<PatchInput> = patch_inputs.clone();
                let mut got: Vec<PatchInput> = patches
                    .iter()
                    .map(|p| {
                        assert_eq!(
                            p.encoded_width,
                            hnr2_operand_width(p.operand_kind).unwrap(),
                            "{ctx}: patch width is derived from the kind"
                        );
                        PatchInput {
                            payload_offset: p.payload_offset,
                            allocation_index: p.allocation_list_index,
                            operand_kind: p.operand_kind,
                        }
                    })
                    .collect();
                want.sort();
                got.sort();
                assert_eq!(want, got, "{ctx}: patch table is not the caller's patches");

                if has_reply {
                    assert_eq!(
                        header.reply_allocation_list_index, reply_allocation_index,
                        "{ctx}: reply index"
                    );
                    assert_eq!(header.reply_offset, reply_offset, "{ctx}: reply offset");
                    assert_eq!(
                        header.reply_capacity_bytes, reply_capacity_bytes,
                        "{ctx}: reply capacity"
                    );
                    assert_eq!(
                        header.reply_slot_generation, reply_slot_generation,
                        "{ctx}: reply slot generation"
                    );
                }
            }

            open = accept.next_open;
            fragments_validated += 1;
        }

        assert!(open.is_none(), "{ctx}: batch never closed");
        assert_eq!(reassembled, payload, "{ctx}: fragments do not reassemble");
        last_batch_token = batch_token;
    }

    assert_eq!(r.offset, data.len(), "corpus has trailing bytes");
    println!(
        "HNR2 corpus: {batch_count} batches, {fragments_validated} fragments validated"
    );
}
