//! Candidate encodings and the selection rule.
//!
//! Every changed file is encoded several ways (full compression, zstd/LZMA2 with the
//! base file as history, bsdiff + compression, each optionally behind the x86 branch
//! filter) and the best candidate wins: the smallest, unless a candidate that is
//! cheaper to decode is within the profile's tolerance of it.

use std::collections::HashMap;
use std::io::Write;
use std::sync::OnceLock;

use lzma_rust2::{Lzma2Options, Lzma2Writer, LzmaOptions};
use patcher_core::bcj;
use patcher_core::format::{Codec, Comp, Transform};
use patcher_core::sys::Budget;
use rayon::prelude::*;
use zstd::zstd_safe::{self, CCtx, CParameter};

use crate::bsdiff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// zstd only, moderate level: quick iteration while building a mod.
    Fast,
    /// Default: every strategy, prefers faster decoders within 5 %.
    Balanced,
    /// Every strategy at maximum effort, always the smallest.
    Max,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub zstd_level: i32,
    /// LZMA2 preset (0-9); `None` disables LZMA2 candidates.
    pub lzma_preset: Option<u32>,
    /// Largest output for which full/bsdiff LZMA2 candidates are tried.
    pub lzma_max: u64,
    /// Largest base + target for which the LZMA2 preset-dictionary candidate is tried.
    pub lzma_ref_max: u64,
    /// Largest file for which bsdiff is tried (suffix array costs ~5x the base size).
    pub bsdiff_max: u64,
    /// Try the x86 branch filter on executables.
    pub bcj: bool,
    /// Accept a cheaper-to-decode candidate up to this fraction larger than the smallest.
    pub tolerance: f64,
}

impl Preset {
    pub fn profile(self) -> Profile {
        match self {
            Preset::Fast => Profile {
                zstd_level: 9,
                lzma_preset: None,
                lzma_max: 0,
                lzma_ref_max: 0,
                bsdiff_max: 0,
                bcj: false,
                tolerance: 0.05,
            },
            Preset::Balanced => Profile {
                zstd_level: 19,
                lzma_preset: Some(9),
                lzma_max: 64 << 20,
                lzma_ref_max: 64 << 20,
                bsdiff_max: 256 << 20,
                bcj: true,
                tolerance: 0.05,
            },
            Preset::Max => Profile {
                zstd_level: 22,
                lzma_preset: Some(9),
                lzma_max: 512 << 20,
                lzma_ref_max: 512 << 20,
                bsdiff_max: 512 << 20,
                bcj: true,
                tolerance: 0.0,
            },
        }
    }
}

/// Files above this size are split into independently encoded chunks.
pub const LARGE_FILE: u64 = 512 << 20;
pub const CHUNK: usize = 64 << 20;
/// zstd level cap for the plain and reference candidates of large-file chunks: above
/// this the optimal parser costs ~40 s per chunk and bsdiff beats it anyway.
const LARGE_LEVEL: i32 = 12;
/// Extra base data around a chunk's matched region, to catch shifted content.
const CHUNK_MARGIN: usize = 32 << 20;

/// One encoded piece of output.
#[derive(Debug)]
pub struct Encoded {
    pub codec: Codec,
    pub transform: Transform,
    pub data: Vec<u8>,
}

/// An encoded segment plus the candidates that were tried, for reporting.
#[derive(Debug)]
pub struct EncodedSegment {
    pub len: u64,
    /// (offset, len) of the base region used as reference.
    pub base: Option<(u64, u64)>,
    pub chosen: Encoded,
    pub tried: Vec<(String, u64)>,
}

pub fn label(codec: Codec, t: Transform) -> String {
    match t {
        Transform::None => codec.name().to_string(),
        Transform::BcjX86 => format!("bcj+{}", codec.name()),
    }
}

/// Encodes `target`, optionally against `base`, as one or more segments.
pub fn encode_file(base: Option<&[u8]>, target: &[u8], prof: &Profile, budget: &Budget) -> Vec<EncodedSegment> {
    let big = target.len() as u64 > LARGE_FILE || base.is_some_and(|b| b.len() as u64 > LARGE_FILE);
    if !big {
        return vec![encode_whole(base, target, prof, budget)];
    }
    let anchors = base.map(Anchors::build);
    let chunks: Vec<(usize, &[u8])> = target.chunks(CHUNK).scan(0, |off, c| {
        let o = *off;
        *off += c.len();
        Some((o, c))
    }).collect();
    chunks
        .par_iter()
        .map(|&(off, chunk)| {
            let region = base.map(|b| {
                let (s, e) = anchors.as_ref().unwrap().region(b.len(), target.len(), off, chunk);
                (s, &b[s..e])
            });
            let mut cands = vec![raw(chunk)];
            let level = prof.zstd_level.min(LARGE_LEVEL);
            {
                let _p = budget.acquire(zstd_mem(chunk.len(), 0));
                cands.push(Encoded { codec: Codec::Full(Comp::Zstd), transform: Transform::None, data: zstd(chunk, None, level) });
            }
            if let Some((_, r)) = region.filter(|(_, r)| !r.is_empty()) {
                {
                    let _p = budget.acquire(zstd_mem(chunk.len(), r.len()));
                    cands.push(Encoded { codec: Codec::Ref(Comp::Zstd), transform: Transform::None, data: zstd(chunk, Some(r), level) });
                }
                if prof.bsdiff_max > 0 {
                    let payload = {
                        let _p = budget.acquire(r.len() as u64 * 5 + chunk.len() as u64 * 3);
                        bsdiff::diff(r, chunk)
                    };
                    let _p = budget.acquire(zstd_mem(payload.len(), 0));
                    let data = zstd(&payload, None, prof.zstd_level.min(19));
                    cands.push(Encoded { codec: Codec::Bsdiff(Comp::Zstd), transform: Transform::None, data });
                }
            }
            let tried = tried(&cands);
            let chosen = pick(cands, prof.tolerance);
            let base = (chosen.codec.needs_base()).then(|| {
                let (s, r) = region.unwrap();
                (s as u64, r.len() as u64)
            });
            EncodedSegment { len: chunk.len() as u64, base, chosen, tried }
        })
        .collect()
}

/// Tries every applicable strategy on the whole file.
fn encode_whole(base: Option<&[u8]>, target: &[u8], prof: &Profile, budget: &Budget) -> EncodedSegment {
    let mut transforms = vec![Transform::None];
    if prof.bcj && is_x86_executable(target) {
        transforms.push(Transform::BcjX86);
    }
    let mut cands: Vec<Encoded> = transforms
        .par_iter()
        .flat_map(|&t| encode_with_transform(base, target, t, prof, budget))
        .collect();
    cands.push(raw(target));
    let tried = tried(&cands);
    let chosen = pick(cands, prof.tolerance);
    let base = chosen.codec.needs_base().then(|| (0, base.unwrap().len() as u64));
    EncodedSegment { len: target.len() as u64, base, chosen, tried }
}

fn encode_with_transform(base: Option<&[u8]>, target: &[u8], t: Transform, prof: &Profile, budget: &Budget) -> Vec<Encoded> {
    let filter = |d: &[u8]| {
        let mut v = d.to_vec();
        bcj::encode(&mut v);
        v
    };
    let (tb, tt);
    let (base, target) = match t {
        Transform::None => (base, target),
        Transform::BcjX86 => {
            tb = base.map(filter);
            tt = filter(target);
            (tb.as_deref(), &tt[..])
        }
    };
    let n = target.len() as u64;
    let b = base.map_or(0, |b| b.len() as u64);
    let lzma_dict = |len: u64| -> u32 { len.clamp(4096, 1536 << 20).next_multiple_of(4096) as u32 };

    type Job<'a> = Box<dyn Fn() -> Option<Encoded> + Send + Sync + 'a>;
    let mut jobs: Vec<Job> = Vec::new();
    jobs.push(Box::new(move || {
        let _p = budget.acquire(zstd_mem(target.len(), 0));
        Some(Encoded { codec: Codec::Full(Comp::Zstd), transform: t, data: zstd(target, None, prof.zstd_level) })
    }));
    if let Some(preset) = prof.lzma_preset.filter(|_| n <= prof.lzma_max) {
        jobs.push(Box::new(move || {
            let d = lzma_dict(n);
            let _p = budget.acquire(lzma_mem(d, n));
            Some(Encoded { codec: Codec::Full(Comp::Lzma2 { dict_size: d }), transform: t, data: lzma(target, None, preset, d) })
        }));
    }
    if let Some(base) = base {
        jobs.push(Box::new(move || {
            let _p = budget.acquire(zstd_mem(target.len(), base.len()));
            Some(Encoded { codec: Codec::Ref(Comp::Zstd), transform: t, data: zstd(target, Some(base), prof.zstd_level) })
        }));
        if let Some(preset) = prof.lzma_preset.filter(|_| n + b <= prof.lzma_ref_max) {
            jobs.push(Box::new(move || {
                let d = lzma_dict(n + b);
                let _p = budget.acquire(lzma_mem(d, n + b));
                Some(Encoded { codec: Codec::Ref(Comp::Lzma2 { dict_size: d }), transform: t, data: lzma(target, Some(base), preset, d) })
            }));
        }
    }
    let mut out: Vec<Encoded> = jobs.par_iter().filter_map(|j| j()).collect();

    // bsdiff produces one payload compressed two ways, so it is a job of its own.
    if let Some(base) = base.filter(|_| n.max(b) <= prof.bsdiff_max) {
        let payload = {
            let _p = budget.acquire(b * 5 + n * 3);
            bsdiff::diff(base, target)
        };
        let p = &payload[..];
        let pl = p.len() as u64;
        let (z, l) = rayon::join(
            || {
                let _p = budget.acquire(zstd_mem(p.len(), 0));
                Encoded { codec: Codec::Bsdiff(Comp::Zstd), transform: t, data: zstd(p, None, prof.zstd_level) }
            },
            || {
                let preset = prof.lzma_preset.filter(|_| pl <= prof.lzma_max)?;
                let d = lzma_dict(pl);
                let _p = budget.acquire(lzma_mem(d, pl));
                Some(Encoded { codec: Codec::Bsdiff(Comp::Lzma2 { dict_size: d }), transform: t, data: lzma(p, None, preset, d) })
            },
        );
        out.push(z);
        out.extend(l);
    }
    out
}

/// Picks the candidate with the lowest decode cost among those within `tolerance` of the
/// smallest (ties broken by size).
pub fn pick(cands: Vec<Encoded>, tolerance: f64) -> Encoded {
    let min = cands.iter().map(|c| c.data.len()).min().expect("at least one candidate");
    let limit = ((min as f64) * (1.0 + tolerance)).floor() as usize;
    cands
        .into_iter()
        .filter(|c| c.data.len() <= limit.max(min))
        .min_by_key(|c| (c.codec.decode_cost() + c.transform.decode_cost(), c.data.len()))
        .unwrap()
}

fn tried(cands: &[Encoded]) -> Vec<(String, u64)> {
    let mut v: Vec<_> = cands.iter().map(|c| (label(c.codec, c.transform), c.data.len() as u64)).collect();
    v.sort_by_key(|x| x.1);
    v
}

pub fn raw(data: &[u8]) -> Encoded {
    Encoded { codec: Codec::Raw, transform: Transform::None, data: data.to_vec() }
}

/// PE ("MZ") or ELF images for x86/x86-64.
pub fn is_x86_executable(d: &[u8]) -> bool {
    if d.len() >= 0x40 && d.starts_with(b"MZ") {
        let pe = u32::from_le_bytes(d[0x3c..0x40].try_into().unwrap()) as usize;
        return d.get(pe..pe + 6).is_some_and(|h| {
            h.starts_with(b"PE\0\0") && matches!(u16::from_le_bytes([h[4], h[5]]), 0x14c | 0x8664)
        });
    }
    d.len() >= 20 && d.starts_with(b"\x7fELF") && matches!(u16::from_le_bytes([d[18], d[19]]), 3 | 62)
}

// ---- compressors -------------------------------------------------------------------

fn ceil_log2(n: usize) -> u32 {
    usize::BITS - n.saturating_sub(1).leading_zeros()
}

/// zstd frame; with `prefix` this is `zstd --patch-from`.
pub fn zstd(data: &[u8], prefix: Option<&[u8]>, level: i32) -> Vec<u8> {
    let mut cctx = CCtx::create();
    let total = data.len() + prefix.map_or(0, <[u8]>::len);
    let set = |c: &mut CCtx, p| c.set_parameter(p).expect("valid zstd parameter");
    set(&mut cctx, CParameter::CompressionLevel(level));
    set(&mut cctx, CParameter::ChecksumFlag(false));
    set(&mut cctx, CParameter::DictIdFlag(false));
    set(&mut cctx, CParameter::ContentSizeFlag(true));
    if prefix.is_some() || total > 8 << 20 {
        set(&mut cctx, CParameter::WindowLog(ceil_log2(total).clamp(10, 31)));
    }
    if total > 8 << 20 {
        set(&mut cctx, CParameter::EnableLongDistanceMatching(true));
    }
    if let Some(p) = prefix {
        // libzstd only indexes the last 2^max(hashLog+3, chainLog+1) bytes of a prefix;
        // the level defaults cover ~32 MiB, so larger references need larger tables.
        let need = ceil_log2(p.len());
        if need > 25 {
            let chain = (need - 1).min(29);
            set(&mut cctx, CParameter::ChainLog(chain));
            set(&mut cctx, CParameter::HashLog(chain - 1));
        }
        cctx.ref_prefix(p).expect("zstd prefix");
    }
    let mut out = Vec::with_capacity(zstd_safe::compress_bound(data.len()));
    cctx.compress2(&mut out, data).expect("zstd compression");
    out
}

/// Raw LZMA2 stream; with `prefix` the base is a preset dictionary.
pub fn lzma(data: &[u8], prefix: Option<&[u8]>, preset: u32, dict_size: u32) -> Vec<u8> {
    let mut opts = LzmaOptions::with_preset(preset);
    opts.dict_size = dict_size;
    opts.preset_dict = prefix.map(<[u8]>::to_vec);
    let mut w = Lzma2Writer::new(Vec::with_capacity(data.len() / 2), Lzma2Options { lzma_options: opts, ..Default::default() });
    w.write_all(data).expect("lzma2 write");
    w.finish().expect("lzma2 finish")
}

pub fn zstd_mem(len: usize, prefix: usize) -> u64 {
    ((len + prefix) as u64) * 6 + (64 << 20)
}

fn lzma_mem(dict: u32, len: u64) -> u64 {
    u64::from(dict) * 12 + len * 2
}

// ---- large-file chunk alignment ----------------------------------------------------

/// Content-defined sample of base positions, used to find where a target chunk's data
/// lives in the base so each chunk gets a small, relevant reference window.
struct Anchors {
    map: HashMap<u64, u64>,
}

const ANCHOR_WIN: usize = 64;
const ANCHOR_MASK: u64 = (1 << 12) - 1; // ~ one anchor per 4 KiB

fn gear() -> &'static [u64; 256] {
    static T: OnceLock<[u64; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut s = 0x243F6A8885A308D3u64;
        std::array::from_fn(|_| {
            s = s.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        })
    })
}

/// Calls `f(position, hash)` at content-defined cut points of `data`.
fn for_each_anchor(data: &[u8], mut f: impl FnMut(usize, u64)) {
    let g = gear();
    let mut h = 0u64;
    for (i, &b) in data.iter().enumerate() {
        h = (h << 1).wrapping_add(g[b as usize]);
        if i >= ANCHOR_WIN && h & ANCHOR_MASK == 0 {
            let start = i + 1 - ANCHOR_WIN;
            f(start, xxhash_rust::xxh3::xxh3_64(&data[start..=i]));
        }
    }
}

impl Anchors {
    fn build(base: &[u8]) -> Anchors {
        let mut map = HashMap::new();
        for_each_anchor(base, |pos, h| {
            map.entry(h).or_insert(pos as u64);
        });
        Anchors { map }
    }

    /// Base range to use as reference for `chunk`, found at `off` in a target of
    /// `target_len` bytes: the chunk's span shifted by the median anchor displacement,
    /// widened by a margin. Falls back to the proportional position.
    fn region(&self, base_len: usize, target_len: usize, off: usize, chunk: &[u8]) -> (usize, usize) {
        let mut deltas = Vec::new();
        for_each_anchor(chunk, |pos, h| {
            if let Some(&b) = self.map.get(&h) {
                deltas.push(b as i64 - (off + pos) as i64);
            }
        });
        let start = if deltas.is_empty() {
            (off as u128 * base_len as u128 / target_len.max(1) as u128) as i64
        } else {
            deltas.sort_unstable();
            off as i64 + deltas[deltas.len() / 2]
        };
        let lo = (start - CHUNK_MARGIN as i64).clamp(0, base_len as i64) as usize;
        let hi = (start + (chunk.len() + CHUNK_MARGIN) as i64).clamp(0, base_len as i64) as usize;
        (lo, hi.max(lo))
    }
}
