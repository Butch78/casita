//! Content-defined chunk slicing against exact chunk deduplication.
//!
//! Exact deduplication saves bytes only when a whole FastCDC chunk recurs.
//! Slicing indexes sampled small chunks as a discovery cache, verifies every
//! hit by byte comparison, and extends each match in both directions, so a
//! rebuilt blob that differs from its base only in scattered store hashes
//! encodes as a few copies plus tiny literals. Every strategy reconstructs each
//! encoded blob through its compressed representation and checks the BLAKE3
//! identity before any sample is reported.
//!
//! Synthetic fixtures run under Criterion. Set `CASITA_CDCS_BASE` and
//! `CASITA_CDCS_REBUILT` to two directory trees, for example two rebuilds of one
//! Nix store path, to measure a real corpus instead; `CASITA_CDCS_REPORT`
//! writes the complete JSON report.

mod bench_util;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fastcdc::v2020::FastCDC;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One manifest entry of the current chunked backend: a chunk id and a size.
const MANIFEST_ENTRY_BYTES: u64 = 32 + 8;
/// A copy source is named once per blob by its 32-byte identity; tokens then
/// refer to that table by a small index.
const SOURCE_TABLE_ENTRY_BYTES: u64 = 32;
/// Verified range reads decode whole bao-tree groups.
const GROUP_BYTES: usize = 16 * 1024;
/// Discovery hits with more candidates than this (zero runs, padding) only
/// try the most recent ones.
const MAX_CANDIDATES: usize = 8;
const ZSTD_LEVEL: i32 = zstd::DEFAULT_COMPRESSION_LEVEL;
const SCHEMA: &str = "casita.cdcs.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    /// The current backend: whole-chunk identity, one zstd frame per chunk.
    Exact { avg: usize },
    /// Sampled discovery chunks, byte-verified and extended matches, literals
    /// in one zstd frame per blob.
    Slices { discovery: usize, sample: u64 },
}

impl Strategy {
    fn label(self) -> String {
        match self {
            Strategy::Exact { avg } => format!("exact/{}", human(avg)),
            Strategy::Slices { discovery, sample } => {
                format!("slices/{}/{sample}", human(discovery))
            }
        }
    }

    fn describe(self) -> Value {
        match self {
            Strategy::Exact { avg } => json!({"kind": "exact", "avg_chunk_bytes": avg}),
            Strategy::Slices { discovery, sample } => {
                json!({"kind": "slices", "discovery_bytes": discovery, "sample": sample})
            }
        }
    }
}

/// The strategy matrix. Exact sizes bracket the production default from both
/// sides down to the discovery sizes themselves, so the slicing rows can be
/// separated from the effect of merely cutting smaller chunks. The 1 KiB rows
/// sweep the sampling rate from 1 in 16 through 1 in 4 to unsampled, because
/// a gap between edits with no sampled chunk is lost whole.
const STRATEGIES: [Strategy; 9] = [
    Strategy::Exact { avg: 256 * 1024 },
    Strategy::Exact { avg: 64 * 1024 },
    Strategy::Exact { avg: 8 * 1024 },
    Strategy::Exact { avg: 1024 },
    Strategy::Slices {
        discovery: 8 * 1024,
        sample: 16,
    },
    Strategy::Slices {
        discovery: 2 * 1024,
        sample: 16,
    },
    Strategy::Slices {
        discovery: 1024,
        sample: 16,
    },
    Strategy::Slices {
        discovery: 1024,
        sample: 4,
    },
    Strategy::Slices {
        discovery: 1024,
        sample: 1,
    },
];

fn human(bytes: usize) -> String {
    if bytes.is_multiple_of(1024 * 1024) {
        format!("{}MiB", bytes / (1024 * 1024))
    } else if bytes.is_multiple_of(1024) {
        format!("{}KiB", bytes / 1024)
    } else {
        format!("{bytes}B")
    }
}

struct Blob {
    path: String,
    bytes: Vec<u8>,
}

struct Corpus {
    label: String,
    description: Value,
    base: Vec<Blob>,
    rebuilt: Vec<Blob>,
}

impl Corpus {
    fn rebuilt_bytes(&self) -> u64 {
        self.rebuilt
            .iter()
            .map(|blob| blob.bytes.len() as u64)
            .sum()
    }

    fn base_bytes(&self) -> u64 {
        self.base.iter().map(|blob| blob.bytes.len() as u64).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Source {
    Base(usize),
    Rebuilt(usize),
}

#[derive(Clone, Copy, Debug)]
struct Location {
    source: Source,
    offset: usize,
    len: usize,
}

#[derive(Clone, Copy, Debug)]
enum Token {
    Literal { len: usize },
    Copy(Location),
}

impl Token {
    fn len(self) -> usize {
        match self {
            Token::Literal { len } => len,
            Token::Copy(location) => location.len,
        }
    }
}

/// One encoded rebuilt blob: its tokens plus every stored frame it needs.
struct Encoded {
    tokens: Vec<Token>,
    /// Slices: the single zstd frame holding all literals in order.
    literal_frame: Vec<u8>,
    /// Exact: the ids of chunks in order; frames live in the shared table.
    chunks: Vec<[u8; 32]>,
}

fn cut(bytes: &[u8], avg: usize) -> Vec<(usize, usize)> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let min = (avg / 2).max(fastcdc::v2020::MINIMUM_MIN);
    let max = (avg * 2).max(fastcdc::v2020::MAXIMUM_MIN);
    FastCDC::new(bytes, min, avg, max)
        .map(|chunk| (chunk.offset, chunk.length))
        .collect()
}

fn fingerprint(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn sample_key(id: &[u8; 32]) -> u64 {
    u64::from_le_bytes(id[..8].try_into().unwrap())
}

#[derive(Clone, Default)]
struct Index {
    /// Exact: every stored chunk, by content id. Slices: sampled discovery
    /// chunks, by fingerprint; candidates are verified by byte comparison.
    entries: HashMap<[u8; 32], Vec<Location>>,
    /// Exact: compressed chunk frames by id.
    frames: HashMap<[u8; 32], Vec<u8>>,
}

impl Index {
    fn insert(&mut self, id: [u8; 32], location: Location) {
        let candidates = self.entries.entry(id).or_default();
        if candidates.len() == MAX_CANDIDATES {
            candidates.remove(0);
        }
        candidates.push(location);
    }
}

#[derive(Default, Debug, Clone)]
struct Outcome {
    logical_bytes: u64,
    payload_bytes: u64,
    metadata_bytes: u64,
    copy_bytes: u64,
    literal_bytes: u64,
    copies: u64,
    literals: u64,
    new_chunks: u64,
    reused_chunks: u64,
    base_index_entries: u64,
    index_entries: u64,
    max_depth: u64,
    max_sources_per_group: u64,
    sources_per_group_sum: u64,
    groups: u64,
    discovery: Duration,
    matching: Duration,
    compression: Duration,
}

impl Outcome {
    fn physical_bytes(&self) -> u64 {
        self.payload_bytes + self.metadata_bytes
    }

    fn to_json(&self) -> Value {
        let logical = self.logical_bytes.max(1) as f64;
        json!({
            "logical_bytes": self.logical_bytes,
            "physical_bytes": self.physical_bytes(),
            "physical_percent": self.physical_bytes() as f64 * 100.0 / logical,
            "payload_bytes": self.payload_bytes,
            "metadata_bytes": self.metadata_bytes,
            "copy_bytes": self.copy_bytes,
            "literal_bytes": self.literal_bytes,
            "copies": self.copies,
            "literals": self.literals,
            "new_chunks": self.new_chunks,
            "reused_chunks": self.reused_chunks,
            "base_index_entries": self.base_index_entries,
            "index_entries": self.index_entries,
            "max_depth": self.max_depth,
            "max_sources_per_group": self.max_sources_per_group,
            "mean_sources_per_group": self.sources_per_group_sum as f64 / self.groups.max(1) as f64,
            "discovery_seconds": self.discovery.as_secs_f64(),
            "matching_seconds": self.matching.as_secs_f64(),
            "compression_seconds": self.compression.as_secs_f64(),
            "encode_seconds": (self.discovery + self.matching + self.compression).as_secs_f64(),
        })
    }
}

/// Index every base blob. Exact strategies also store every base chunk frame
/// so reconstruction can round-trip through compression.
fn build_index(corpus: &Corpus, strategy: Strategy) -> Index {
    let mut index = Index::default();
    for (blob_index, blob) in corpus.base.iter().enumerate() {
        index_blob(
            &mut index,
            Source::Base(blob_index),
            &blob.bytes,
            strategy,
            true,
        );
    }
    index
}

fn index_blob(index: &mut Index, source: Source, bytes: &[u8], strategy: Strategy, store: bool) {
    match strategy {
        Strategy::Exact { avg } => {
            for (offset, len) in cut(bytes, avg) {
                let id = fingerprint(&bytes[offset..offset + len]);
                if store && !index.frames.contains_key(&id) {
                    let frame =
                        zstd::bulk::compress(&bytes[offset..offset + len], ZSTD_LEVEL).unwrap();
                    index.frames.insert(id, frame);
                    index.insert(
                        id,
                        Location {
                            source,
                            offset,
                            len,
                        },
                    );
                }
            }
        }
        Strategy::Slices { discovery, sample } => {
            for (offset, len) in cut(bytes, discovery) {
                let id = fingerprint(&bytes[offset..offset + len]);
                if sample_key(&id).is_multiple_of(sample) {
                    index.insert(
                        id,
                        Location {
                            source,
                            offset,
                            len,
                        },
                    );
                }
            }
        }
    }
}

fn source_bytes(corpus: &Corpus, source: Source) -> &[u8] {
    match source {
        Source::Base(index) => &corpus.base[index].bytes,
        Source::Rebuilt(index) => &corpus.rebuilt[index].bytes,
    }
}

fn encode_exact(
    corpus: &Corpus,
    blob_index: usize,
    avg: usize,
    index: &mut Index,
    outcome: &mut Outcome,
) -> Encoded {
    let bytes = &corpus.rebuilt[blob_index].bytes;
    let started = Instant::now();
    let chunks = cut(bytes, avg);
    let ids: Vec<[u8; 32]> = chunks
        .iter()
        .map(|&(offset, len)| fingerprint(&bytes[offset..offset + len]))
        .collect();
    outcome.discovery += started.elapsed();
    let mut tokens = Vec::with_capacity(chunks.len());
    for (&(offset, len), id) in chunks.iter().zip(&ids) {
        let started = Instant::now();
        let known = index
            .entries
            .get(id)
            .and_then(|candidates| candidates.first().copied());
        outcome.matching += started.elapsed();
        match known {
            Some(location) => {
                outcome.reused_chunks += 1;
                outcome.copy_bytes += len as u64;
                tokens.push(Token::Copy(location));
            }
            None => {
                let started = Instant::now();
                let frame = zstd::bulk::compress(&bytes[offset..offset + len], ZSTD_LEVEL).unwrap();
                outcome.compression += started.elapsed();
                outcome.payload_bytes += frame.len() as u64;
                outcome.new_chunks += 1;
                outcome.literal_bytes += len as u64;
                let location = Location {
                    source: Source::Rebuilt(blob_index),
                    offset,
                    len,
                };
                index.frames.insert(*id, frame);
                index.insert(*id, location);
                tokens.push(Token::Copy(location));
            }
        }
    }
    if chunks.len() > 1 {
        outcome.metadata_bytes += chunks.len() as u64 * MANIFEST_ENTRY_BYTES;
    }
    outcome.copies += chunks.len() as u64;
    Encoded {
        tokens,
        literal_frame: Vec::new(),
        chunks: ids,
    }
}

fn extend(
    target: &[u8],
    target_start: usize,
    target_end: usize,
    floor: usize,
    source: &[u8],
    source_start: usize,
) -> (usize, usize, usize) {
    let mut back = 0;
    while target_start - back > floor
        && source_start - back > 0
        && target[target_start - back - 1] == source[source_start - back - 1]
    {
        back += 1;
    }
    let mut end = target_end;
    let mut source_end = source_start + (target_end - target_start);
    while end < target.len() && source_end < source.len() && target[end] == source[source_end] {
        end += 1;
        source_end += 1;
    }
    (target_start - back, end, source_start - back)
}

fn encode_slices(
    corpus: &Corpus,
    blob_index: usize,
    discovery: usize,
    sample: u64,
    index: &mut Index,
    outcome: &mut Outcome,
) -> Encoded {
    let bytes = &corpus.rebuilt[blob_index].bytes;
    let started = Instant::now();
    let chunks = cut(bytes, discovery);
    let ids: Vec<Option<[u8; 32]>> = chunks
        .iter()
        .map(|&(offset, len)| {
            let id = fingerprint(&bytes[offset..offset + len]);
            sample_key(&id).is_multiple_of(sample).then_some(id)
        })
        .collect();
    outcome.discovery += started.elapsed();

    let started = Instant::now();
    let mut tokens = Vec::new();
    let mut literal = Vec::new();
    let mut covered = 0;
    for (&(offset, len), id) in chunks.iter().zip(&ids) {
        let Some(id) = id else { continue };
        if offset < covered {
            continue;
        }
        let Some(candidates) = index.entries.get(id) else {
            continue;
        };
        let mut best: Option<(usize, usize, Location)> = None;
        for candidate in candidates.iter().rev() {
            let source = source_bytes(corpus, candidate.source);
            if candidate.len != len
                || source[candidate.offset..candidate.offset + len] != bytes[offset..offset + len]
            {
                continue;
            }
            let (start, end, source_start) = extend(
                bytes,
                offset,
                offset + len,
                covered,
                source,
                candidate.offset,
            );
            if best.is_none_or(|(s, e, _)| end - start > e - s) {
                best = Some((
                    start,
                    end,
                    Location {
                        source: candidate.source,
                        offset: source_start,
                        len: end - start,
                    },
                ));
            }
        }
        let Some((start, end, location)) = best else {
            continue;
        };
        if start > covered {
            tokens.push(Token::Literal {
                len: start - covered,
            });
            literal.extend_from_slice(&bytes[covered..start]);
        }
        tokens.push(Token::Copy(location));
        covered = end;
    }
    if covered < bytes.len() {
        tokens.push(Token::Literal {
            len: bytes.len() - covered,
        });
        literal.extend_from_slice(&bytes[covered..]);
    }
    outcome.matching += started.elapsed();

    let started = Instant::now();
    let literal_frame = if literal.is_empty() {
        Vec::new()
    } else {
        zstd::bulk::compress(&literal, ZSTD_LEVEL).unwrap()
    };
    outcome.compression += started.elapsed();

    let mut sources = HashSet::new();
    let mut token_bytes = 0;
    for token in &tokens {
        match token {
            Token::Literal { len } => {
                outcome.literals += 1;
                outcome.literal_bytes += *len as u64;
                token_bytes += 1 + varint_len(*len as u64);
            }
            Token::Copy(location) => {
                outcome.copies += 1;
                outcome.copy_bytes += location.len as u64;
                sources.insert(location.source);
                token_bytes +=
                    1 + varint_len(location.offset as u64) + varint_len(location.len as u64) + 2;
            }
        }
    }
    outcome.payload_bytes += literal_frame.len() as u64;
    outcome.metadata_bytes += token_bytes + sources.len() as u64 * SOURCE_TABLE_ENTRY_BYTES;

    let started = Instant::now();
    index_blob(
        index,
        Source::Rebuilt(blob_index),
        bytes,
        Strategy::Slices { discovery, sample },
        false,
    );
    outcome.discovery += started.elapsed();
    Encoded {
        tokens,
        literal_frame,
        chunks: Vec::new(),
    }
}

fn varint_len(value: u64) -> u64 {
    (64 - value.leading_zeros() as u64).max(1).div_ceil(7)
}

/// Decode an encoded blob from stored frames and source bytes, then verify its
/// identity. Rebuilt sources resolve through their own verified decodes, so a
/// wrong chain fails here rather than being trusted from the corpus.
fn verify(
    corpus: &Corpus,
    blob_index: usize,
    encoded: &Encoded,
    index: &Index,
    decoded: &[Vec<u8>],
) -> Vec<u8> {
    let expected = &corpus.rebuilt[blob_index].bytes;
    let mut output = Vec::with_capacity(expected.len());
    if encoded.chunks.is_empty() {
        let literal = if encoded.literal_frame.is_empty() {
            Vec::new()
        } else {
            zstd::bulk::decompress(&encoded.literal_frame, expected.len()).unwrap()
        };
        let mut consumed = 0;
        for token in &encoded.tokens {
            match *token {
                Token::Literal { len } => {
                    output.extend_from_slice(&literal[consumed..consumed + len]);
                    consumed += len;
                }
                Token::Copy(location) => {
                    let source: &[u8] = match location.source {
                        Source::Base(base) => &corpus.base[base].bytes,
                        Source::Rebuilt(earlier) => {
                            assert!(earlier < blob_index, "copy from a later blob");
                            &decoded[earlier]
                        }
                    };
                    output.extend_from_slice(
                        &source[location.offset..location.offset + location.len],
                    );
                }
            }
        }
        assert_eq!(consumed, literal.len(), "literal frame not fully consumed");
    } else {
        for (id, token) in encoded.chunks.iter().zip(&encoded.tokens) {
            let frame = &index.frames[id];
            let chunk = zstd::bulk::decompress(frame, token.len()).unwrap();
            assert_eq!(chunk.len(), token.len());
            assert_eq!(fingerprint(&chunk), *id);
            output.extend_from_slice(&chunk);
        }
    }
    assert_eq!(output.len(), expected.len(), "decoded length mismatch");
    assert_eq!(
        blake3::hash(&output),
        blake3::hash(expected),
        "decoded identity mismatch for {}",
        corpus.rebuilt[blob_index].path
    );
    output
}

fn read_cost(encoded: &Encoded, len: usize, outcome: &mut Outcome) {
    if len == 0 {
        return;
    }
    let mut position = 0;
    let mut spans: Vec<(usize, usize, Option<Source>)> = Vec::with_capacity(encoded.tokens.len());
    for token in &encoded.tokens {
        let source = match token {
            Token::Literal { .. } => None,
            Token::Copy(location) => Some(location.source),
        };
        spans.push((position, position + token.len(), source));
        position += token.len();
    }
    let mut cursor = 0;
    for group in 0..len.div_ceil(GROUP_BYTES) {
        let start = group * GROUP_BYTES;
        let end = (start + GROUP_BYTES).min(len);
        while cursor < spans.len() && spans[cursor].1 <= start {
            cursor += 1;
        }
        let mut sources: HashSet<Option<Source>> = HashSet::new();
        let mut objects = 0u64;
        let mut probe = cursor;
        while probe < spans.len() && spans[probe].0 < end {
            if encoded.chunks.is_empty() {
                sources.insert(spans[probe].2);
            } else {
                objects += 1;
            }
            probe += 1;
        }
        let count = if encoded.chunks.is_empty() {
            sources.len() as u64
        } else {
            objects
        };
        outcome.max_sources_per_group = outcome.max_sources_per_group.max(count);
        outcome.sources_per_group_sum += count;
        outcome.groups += 1;
    }
}

/// Encode every rebuilt blob in order against a base index, verify each one,
/// and return the accounting. The index is consumed so a timed iteration owns
/// its own copy.
fn encode_corpus(corpus: &Corpus, strategy: Strategy, mut index: Index) -> Outcome {
    let mut outcome = Outcome {
        logical_bytes: corpus.rebuilt_bytes(),
        base_index_entries: index.entries.len() as u64,
        ..Outcome::default()
    };
    let mut decoded: Vec<Vec<u8>> = Vec::with_capacity(corpus.rebuilt.len());
    let mut depths: Vec<u64> = Vec::with_capacity(corpus.rebuilt.len());
    for blob_index in 0..corpus.rebuilt.len() {
        let encoded = match strategy {
            Strategy::Exact { avg } => {
                encode_exact(corpus, blob_index, avg, &mut index, &mut outcome)
            }
            Strategy::Slices { discovery, sample } => encode_slices(
                corpus,
                blob_index,
                discovery,
                sample,
                &mut index,
                &mut outcome,
            ),
        };
        // Exact chunks are raw objects, so only sliced blobs form chains.
        let depth = if encoded.chunks.is_empty() {
            encoded
                .tokens
                .iter()
                .filter_map(|token| match token {
                    Token::Copy(Location {
                        source: Source::Base(_),
                        ..
                    }) => Some(1),
                    Token::Copy(Location {
                        source: Source::Rebuilt(earlier),
                        ..
                    }) => Some(depths[*earlier] + 1),
                    Token::Literal { .. } => None,
                })
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        outcome.max_depth = outcome.max_depth.max(depth);
        read_cost(
            &encoded,
            corpus.rebuilt[blob_index].bytes.len(),
            &mut outcome,
        );
        let bytes = verify(corpus, blob_index, &encoded, &index, &decoded);
        decoded.push(bytes);
        depths.push(depth);
    }
    outcome.index_entries = index.entries.len() as u64;
    outcome
}

/// Nix base-32 alphabet, so fixtures look like real store hashes.
const NIX_BASE32: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

fn store_hash(seed: u64) -> String {
    bench_util::random_bytes(seed, 32)
        .iter()
        .map(|byte| NIX_BASE32[(byte % 32) as usize] as char)
        .collect()
}

/// Compressible but aperiodic text: random words from a per-file vocabulary,
/// so no 1 KiB window recurs by construction the way a cycled sentence would.
fn text_bytes(seed: u64, len: usize) -> Vec<u8> {
    const VOCABULARY: usize = 4096;
    let entropy = bench_util::random_bytes(seed, len * 2 + VOCABULARY * 8);
    let words: Vec<&[u8]> = entropy[..VOCABULARY * 8]
        .chunks(8)
        .map(|word| &word[..3 + (word[7] % 6) as usize])
        .collect();
    let mut output = Vec::with_capacity(len + 16);
    let mut picks = entropy[VOCABULARY * 8..].chunks_exact(2);
    while output.len() < len {
        let pick = picks.next().unwrap();
        let word = words[u16::from_le_bytes([pick[0], pick[1]]) as usize % VOCABULARY];
        output.extend(word.iter().map(|byte| b'a' + byte % 26));
        output.push(b' ');
    }
    output.truncate(len);
    output
}

/// Alternating 64 KiB text and random blocks with a file-unique seed, so
/// files never share content with each other and every saving below comes
/// from the base tree.
fn file_bytes(file: u64, len: usize) -> Vec<u8> {
    const BLOCK: usize = 64 * 1024;
    let mut output = Vec::with_capacity(len);
    let mut block = 0u64;
    while output.len() < len {
        let take = (len - output.len()).min(BLOCK);
        let seed = file * 1_000_003 + block;
        output.extend(if block.is_multiple_of(2) {
            text_bytes(seed, take)
        } else {
            bench_util::random_bytes(seed, take)
        });
        block += 1;
    }
    output
}

/// A rebuilt tree: the same files with every embedded store path pointing at
/// a rebuilt dependency. Rebuilding maps each of the eight dependencies to a
/// new hash consistently, as a real rebuild does.
fn synthetic_corpus(spacing: usize, files: usize, file_len: usize) -> Corpus {
    const DEPENDENCIES: u64 = 8;
    let path = |dependency: u64, generation: u64| {
        format!(
            "/nix/store/{}-dep{dependency}-1.0",
            store_hash(0xC0FFEE + dependency + generation * 1000)
        )
    };
    let tree = |generation: u64| -> Vec<Blob> {
        (0..files)
            .map(|file| {
                let mut bytes = file_bytes(file as u64, file_len);
                let mut position = spacing / 2;
                let mut counter = 0u64;
                while position + 64 <= bytes.len() {
                    let token = path(counter % DEPENDENCIES, generation);
                    bytes[position..position + token.len()].copy_from_slice(token.as_bytes());
                    position += spacing;
                    counter += 1;
                }
                Blob {
                    path: format!("file-{file}"),
                    bytes,
                }
            })
            .collect()
    };
    Corpus {
        label: format!("synthetic/spacing-{}", human(spacing)),
        description: json!({"kind": "synthetic", "edit_spacing_bytes": spacing, "files": files, "file_bytes": file_len}),
        base: tree(0),
        rebuilt: tree(1),
    }
}

fn walk(root: &Path, directory: &Path, blobs: &mut Vec<Blob>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for entry in entries {
        let metadata = std::fs::symlink_metadata(&entry).unwrap();
        if metadata.is_dir() {
            walk(root, &entry, blobs);
        } else if metadata.is_file() {
            blobs.push(Blob {
                path: entry
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                bytes: std::fs::read(&entry).unwrap(),
            });
        }
    }
}

/// A tree root is normally a directory; a Nix store path may also be one
/// regular file, which is then the only blob.
fn load_tree(root: &Path) -> Vec<Blob> {
    let mut blobs = Vec::new();
    if std::fs::symlink_metadata(root).unwrap().is_file() {
        blobs.push(Blob {
            path: root.file_name().unwrap().to_string_lossy().into_owned(),
            bytes: std::fs::read(root).unwrap(),
        });
    } else {
        walk(root, root, &mut blobs);
    }
    blobs
}

fn directory_corpus(base: &Path, rebuilt: &Path) -> Corpus {
    let base_blobs = load_tree(base);
    let rebuilt_blobs = load_tree(rebuilt);
    Corpus {
        label: "directories".to_string(),
        description: json!({
            "kind": "directories",
            "base": base.display().to_string(),
            "rebuilt": rebuilt.display().to_string(),
            "base_files": base_blobs.len(),
            "rebuilt_files": rebuilt_blobs.len(),
        }),
        base: base_blobs,
        rebuilt: rebuilt_blobs,
    }
}

fn sample(corpus: &Corpus, strategy: Strategy, outcome: &Outcome) -> Value {
    json!({
        "schema": SCHEMA,
        "corpus": corpus.label,
        "corpus_description": corpus.description,
        "base_bytes": corpus.base_bytes(),
        "strategy": strategy.label(),
        "strategy_description": strategy.describe(),
        "outcome": outcome.to_json(),
        "correctness": "passed",
    })
}

fn chunk_slicing(c: &mut Criterion) {
    let base = std::env::var_os("CASITA_CDCS_BASE");
    let rebuilt = std::env::var_os("CASITA_CDCS_REBUILT");
    let mut report = Vec::new();
    match (base, rebuilt) {
        (Some(base), Some(rebuilt)) => {
            let corpus = directory_corpus(Path::new(&base), Path::new(&rebuilt));
            assert!(
                !corpus.rebuilt.is_empty(),
                "rebuilt tree has no regular files"
            );
            for strategy in STRATEGIES {
                let index = build_index(&corpus, strategy);
                let outcome = encode_corpus(&corpus, strategy, index);
                let row = sample(&corpus, strategy, &outcome);
                eprintln!("{row}");
                report.push(row);
            }
        }
        (None, None) => {
            let mut group = c.benchmark_group("chunk_slicing");
            group.sample_size(10);
            // Edit spacing brackets both cliffs: below the 1 KiB discovery
            // chunk no discovery chunk survives an edit, and below the 256 KiB
            // production chunk no whole chunk survives one.
            for spacing in [512, 4096, 65536, 1_048_576] {
                let corpus = synthetic_corpus(spacing, 4, 1024 * 1024);
                for strategy in STRATEGIES {
                    let index = build_index(&corpus, strategy);
                    let outcome = encode_corpus(&corpus, strategy, index.clone());
                    let row = sample(&corpus, strategy, &outcome);
                    eprintln!("{row}");
                    report.push(row);
                    group.throughput(Throughput::Bytes(corpus.rebuilt_bytes()));
                    group.bench_function(BenchmarkId::new(strategy.label(), human(spacing)), |b| {
                        b.iter_batched(
                            || index.clone(),
                            |index| black_box(encode_corpus(&corpus, strategy, index)),
                            criterion::BatchSize::LargeInput,
                        )
                    });
                }
            }
            group.finish();
        }
        _ => panic!("set both CASITA_CDCS_BASE and CASITA_CDCS_REBUILT, or neither"),
    }
    if let Some(path) = std::env::var_os("CASITA_CDCS_REPORT") {
        // Every row above passed its reconstruction gate before it was pushed.
        let document = json!({"schema": SCHEMA, "samples": report});
        std::fs::write(path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }
}

criterion_group!(benches, chunk_slicing);
criterion_main!(benches);
