//! K5's session gate, kernel half: replay the corpus `tools/hts1_attach_probe.c`
//! produced through the KMD's own HTS1 session state machine and require every
//! verdict to be the one the guest half declared.
//!
//! Run by `tools/hts1-attach-gate.sh`, which compiles the probe against
//! `protocol/include/helios_translation_session.h` and sets `HELIOS_HTS1_CORPUS`.
//! ⛔ Without that variable this test SKIPS, so the runner checks the case count
//! it prints — a gate that can pass on an empty corpus is not a gate.

use helios_kmd_logic::translation_session::{
    SessionPhase, SessionRefusal, TranslationSession,
};
use helios_protocol::native_render::{Hvm1Role, HELIOS_HVM1_REPLY_POOL_BYTES};
use helios_protocol::translation_session::{
    HeliosQueueAttachV1, HeliosSessionCapability, HeliosTranslationSessionInitV1,
};

const CASE_INIT: u32 = 1;
const CASE_ATTACH: u32 = 2;
const CASE_RESET: u32 = 1;
const CASE_ADMIT: u32 = 2;

struct Corpus {
    package_generation: u64,
    capset: u32,
    session_generation: u64,
    capability: HeliosSessionCapability,
    endpoint_capacity: u32,
    cases: Vec<Case>,
}

struct Case {
    kind: u32,
    flags: u32,
    name: String,
    bytes: Vec<u8>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> u32 {
        let end = self.at + 4;
        let value = u32::from_le_bytes(
            self.bytes[self.at..end]
                .try_into()
                .expect("4 bytes for a u32"),
        );
        self.at = end;
        value
    }

    fn u64(&mut self) -> u64 {
        let low = self.u32() as u64;
        let high = self.u32() as u64;
        low | (high << 32)
    }

    fn take(&mut self, len: usize) -> &'a [u8] {
        let end = self.at + len;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        slice
    }
}

fn parse(bytes: &[u8]) -> Corpus {
    assert_eq!(&bytes[..8], b"HTS1CORP", "not an HTS1 corpus");
    let mut r = Reader { bytes, at: 8 };
    assert_eq!(r.u32(), 1, "corpus format version");
    let package_generation = r.u64();
    let capset = r.u32();
    let session_generation = r.u64();
    let capability = HeliosSessionCapability::from_halves(r.u64(), r.u64());
    let endpoint_capacity = r.u32();
    let count = r.u32();
    let mut cases = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let kind = r.u32();
        let flags = r.u32();
        let name_len = r.u32() as usize;
        let name = String::from_utf8(r.take(name_len).to_vec()).expect("case name is utf-8");
        let len = r.u32() as usize;
        cases.push(Case {
            kind,
            flags,
            name,
            bytes: r.take(len).to_vec(),
        });
    }
    assert_eq!(r.at, bytes.len(), "trailing bytes in the corpus");
    Corpus {
        package_generation,
        capset,
        session_generation,
        capability,
        endpoint_capacity,
        cases,
    }
}

// ── Wire decoding, by explicit offset ────────────────────────────────────────
//
// Deliberately not `bytemuck`: `kmd_logic/Cargo.toml` records that bytemuck is
// not nameable from this crate, and decoding the two records from the §10.4
// offset tables by hand means the gate does not inherit the same derive both
// halves use. A field that moved in one language shows up here as a wrong value,
// not as two structs that agree because they share a macro.

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().expect("2 bytes"))
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().expect("8 bytes"))
}

fn decode_init(b: &[u8]) -> HeliosTranslationSessionInitV1 {
    assert_eq!(b.len(), 32, "HTS1 INIT is 32 bytes");
    HeliosTranslationSessionInitV1 {
        magic: u32_at(b, 0),
        abi_version: u16_at(b, 4),
        struct_size: u16_at(b, 6),
        package_generation: u64_at(b, 8),
        capset: u32_at(b, 16),
        requested_endpoint_capacity: u32_at(b, 20),
        reserved: u64_at(b, 24),
    }
}

fn decode_attach(b: &[u8]) -> HeliosQueueAttachV1 {
    assert_eq!(b.len(), 72, "HQA1 is 72 bytes");
    HeliosQueueAttachV1 {
        magic: u32_at(b, 0),
        abi_version: u16_at(b, 4),
        struct_size: u16_at(b, 6),
        package_generation: u64_at(b, 8),
        session_generation: u64_at(b, 16),
        capability_low: u64_at(b, 24),
        capability_high: u64_at(b, 32),
        endpoint_id: u32_at(b, 40),
        engine_class: u32_at(b, 44),
        queue_family: u32_at(b, 48),
        queue_index: u32_at(b, 52),
        context_generation: u64_at(b, 56),
        flags: u32_at(b, 64),
        reserved: u32_at(b, 68),
    }
}

fn live_session(c: &Corpus) -> TranslationSession {
    let mut s = TranslationSession::new_provisional(c.package_generation, c.capset);
    s.bind_reply_pool(
        Hvm1Role::ReplyPool,
        HELIOS_HVM1_REPLY_POOL_BYTES,
        0x5EED_0001,
    )
    .expect("the corpus's session binds its reply pool");
    s.complete_init(
        c.endpoint_capacity,
        c.endpoint_capacity,
        c.session_generation,
        c.capability,
    )
    .expect("the corpus's session completes INIT");
    assert_eq!(s.phase(), SessionPhase::Live);
    s
}

/// A provisional session, for the INIT cases: `admit_init` is the receiving-side
/// validator and it only runs before a session is live.
fn provisional_session(c: &Corpus) -> TranslationSession {
    let mut s = TranslationSession::new_provisional(c.package_generation, c.capset);
    s.bind_reply_pool(
        Hvm1Role::ReplyPool,
        HELIOS_HVM1_REPLY_POOL_BYTES,
        0x5EED_0001,
    )
    .expect("binds");
    s
}

#[test]
fn the_kmd_session_agrees_with_the_guest_on_every_corpus_case() {
    let Ok(path) = std::env::var("HELIOS_HTS1_CORPUS") else {
        eprintln!("HELIOS_HTS1_CORPUS unset - skipping (run tools/hts1-attach-gate.sh)");
        return;
    };
    let bytes = std::fs::read(&path).expect("read the corpus");
    let corpus = parse(&bytes);

    let mut session = live_session(&corpus);
    let mut admitted = 0usize;
    let mut refused = 0usize;

    for case in &corpus.cases {
        let expect_admit = case.flags & CASE_ADMIT != 0;
        if case.flags & CASE_RESET != 0 {
            session = if case.kind == CASE_INIT {
                provisional_session(&corpus)
            } else {
                live_session(&corpus)
            };
        }
        let before = (
            session.highest_context_generation(),
            session.attached_contexts(),
        );

        let outcome: Result<(), SessionRefusal> = match case.kind {
            CASE_INIT => session.admit_init(&decode_init(&case.bytes)).map(|_| ()),
            CASE_ATTACH => session.attach(&decode_attach(&case.bytes)).map(|_| ()),
            other => panic!("unknown corpus case kind {other} in {}", case.name),
        };

        match (expect_admit, &outcome) {
            (true, Ok(())) => admitted += 1,
            (false, Err(_)) => {
                refused += 1;
                // A refusal must leave nothing behind for the next packet to
                // trip over — that is what makes the sequenced cases meaningful.
                if case.kind == CASE_ATTACH {
                    assert_eq!(
                        (
                            session.highest_context_generation(),
                            session.attached_contexts()
                        ),
                        before,
                        "{}: a refused attach mutated the session",
                        case.name
                    );
                }
            }
            (true, Err(reason)) => panic!(
                "{}: the guest expects this to be admitted; the KMD refused it with {reason:?}",
                case.name
            ),
            (false, Ok(())) => panic!(
                "{}: the guest expects this to be REFUSED; the KMD admitted it",
                case.name
            ),
        }
    }

    println!(
        "HTS1 corpus: {} cases replayed, {admitted} admitted, {refused} refused",
        corpus.cases.len()
    );
    assert!(admitted > 0 && refused > 0, "a one-sided corpus is not a gate");
}
