//! Decoders for every `Codec`. Pure Rust so the stub cross-compiles anywhere.

use std::io::Read;

use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

use crate::format::{Codec, Comp, Segment, Transform};
use crate::{bcj, Error, Result};

/// Decodes one segment. `base` must be the referenced base slice for `Ref`/`Bsdiff` codecs.
pub fn decode_segment(seg: &Segment, blob: &[u8], base: Option<&[u8]>) -> Result<Vec<u8>> {
    let len = usize::try_from(seg.len).map_err(|_| Error::Corrupt("segment too large".into()))?;
    let filtered;
    let base = match (seg.codec.needs_base(), base, seg.transform) {
        (false, _, _) => None,
        (true, None, _) => return Err(Error::Corrupt("segment needs base data".into())),
        (true, Some(b), Transform::None) => Some(b),
        (true, Some(b), Transform::BcjX86) => {
            let mut v = b.to_vec();
            bcj::encode(&mut v);
            filtered = v;
            Some(&filtered[..])
        }
    };
    let mut out = match seg.codec {
        Codec::Raw => {
            if blob.len() != len {
                return Err(Error::Corrupt("raw segment length mismatch".into()));
            }
            blob.to_vec()
        }
        Codec::Full(c) => decompress(c, blob, None, len)?,
        Codec::Ref(c) => decompress(c, blob, base, len)?,
        Codec::Bsdiff(c) => {
            let limit = len.saturating_mul(2).saturating_add(64 << 20);
            let patch = decompress_unknown(c, blob, limit)?;
            bsdiff_apply(base.unwrap(), &patch, len)?
        }
    };
    if seg.transform == Transform::BcjX86 {
        bcj::decode(&mut out);
    }
    Ok(out)
}

/// Largest LZMA2 dictionary the creator emits.
const MAX_DICT: u32 = 1536 << 20;

/// Initial output capacity: never trust a length from the patch with a huge allocation.
fn cap(len: usize) -> usize {
    len.min(256 << 20)
}

/// Decompresses a stream whose decoded length is known.
pub fn decompress(comp: Comp, src: &[u8], prefix: Option<&[u8]>, len: usize) -> Result<Vec<u8>> {
    let out = match comp {
        Comp::Zstd => zstd_decode(src, prefix, len)?,
        Comp::Lzma2 { dict_size } => lzma2_decode(src, dict_size, prefix, len)?,
    };
    if out.len() != len {
        return Err(Error::Corrupt(format!("decoded {} bytes, expected {len}", out.len())));
    }
    Ok(out)
}

/// Like `decompress`, for streams that carry their own framing (bsdiff payloads); fails
/// beyond `limit` bytes.
fn decompress_unknown(comp: Comp, src: &[u8], limit: usize) -> Result<Vec<u8>> {
    match comp {
        Comp::Zstd => zstd_decode(src, None, limit),
        Comp::Lzma2 { dict_size } => lzma2_decode(src, dict_size, None, limit),
    }
}

/// Decodes at most `limit` bytes; more is an error.
fn lzma2_decode(src: &[u8], dict_size: u32, prefix: Option<&[u8]>, limit: usize) -> Result<Vec<u8>> {
    if dict_size > MAX_DICT {
        return Err(Error::Corrupt("lzma2 dictionary too large".into()));
    }
    let mut out = Vec::with_capacity(cap(limit));
    lzma_rust2::Lzma2Reader::new(src, dict_size, prefix)
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| Error::Corrupt(format!("lzma2: {e}")))?;
    if out.len() > limit {
        return Err(Error::Corrupt("lzma2: stream longer than expected".into()));
    }
    Ok(out)
}

/// Decodes at most `limit` bytes; more is an error.
fn zstd_decode(src: &[u8], prefix: Option<&[u8]>, limit: usize) -> Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Display| Error::Corrupt(format!("zstd: {e}"));
    let mut dec = FrameDecoder::new();
    // The creator's window never exceeds the next power of two of prefix + output.
    let span = (limit as u64).saturating_add(prefix.map_or(0, |p| p.len() as u64));
    dec.set_max_window_size(span.checked_next_power_of_two().unwrap_or(u64::MAX).clamp(1 << 24, 1 << 31));
    let mut input = src;
    dec.reset(&mut input).map_err(|e| err(&e))?;
    if let Some(p) = prefix {
        dec.set_raw_prefix(p).map_err(|e| err(&e))?;
    }
    let mut out = Vec::with_capacity(cap(limit));
    loop {
        let done = dec
            .decode_blocks(&mut input, BlockDecodingStrategy::UptoBytes(4 << 20))
            .map_err(|e| err(&e))?;
        dec.collect_to_writer(&mut out).map_err(|e| err(&e))?;
        if out.len() > limit {
            return Err(Error::Corrupt("zstd: stream longer than expected".into()));
        }
        if done {
            break;
        }
    }
    Ok(out)
}

/// Applies a decompressed bsdiff-style patch (see `crate::varint` for the layout).
pub fn bsdiff_apply(old: &[u8], patch: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let corrupt = || Error::Corrupt("bad bsdiff stream".into());
    let mut r = crate::varint::Reader::new(patch);
    let n_ctrl = r.u64().ok_or_else(corrupt)?;
    let ctrl_len = r.usize().ok_or_else(corrupt)?;
    let diff_len = r.usize().ok_or_else(corrupt)?;
    let extra_len = r.usize().ok_or_else(corrupt)?;
    let rest = r.rest();
    let total = ctrl_len.checked_add(diff_len).and_then(|t| t.checked_add(extra_len));
    if total != Some(rest.len()) {
        return Err(corrupt());
    }
    let (ctrl, rest) = rest.split_at(ctrl_len);
    let (mut diff, mut extra) = rest.split_at(diff_len);
    let mut ctrl = crate::varint::Reader::new(ctrl);

    let mut out = Vec::with_capacity(cap(out_len));
    let mut old_pos: i64 = 0;
    for _ in 0..n_ctrl {
        let add = ctrl.usize().ok_or_else(corrupt)?;
        let copy = ctrl.usize().ok_or_else(corrupt)?;
        let seek = ctrl.i64().ok_or_else(corrupt)?;

        let start = usize::try_from(old_pos).map_err(|_| corrupt())?;
        let src = old.get(start..start.checked_add(add).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        if diff.len() < add || extra.len() < copy || out.len() + add + copy > out_len {
            return Err(corrupt());
        }
        let (d, d_rest) = diff.split_at(add);
        out.extend(src.iter().zip(d).map(|(a, b)| a.wrapping_add(*b)));
        diff = d_rest;

        let (e, e_rest) = extra.split_at(copy);
        out.extend_from_slice(e);
        extra = e_rest;

        old_pos = old_pos
            .checked_add(add as i64)
            .and_then(|p| p.checked_add(seek))
            .ok_or_else(corrupt)?;
    }
    if out.len() != out_len {
        return Err(corrupt());
    }
    Ok(out)
}
