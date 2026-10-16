//! bsdiff (Colin Percival's algorithm) over a divsufsort suffix array.
//!
//! Produces the uncompressed payload described in `patcher_core::varint`; the caller
//! compresses it. Unlike classic bsdiff the three streams share one compressed frame.

use patcher_core::varint::{put_i64, put_u64};

/// Inputs must be below 2 GiB (32-bit suffix array).
pub fn diff(old: &[u8], new: &[u8]) -> Vec<u8> {
    assert!(old.len() < i32::MAX as usize && new.len() < i32::MAX as usize);
    let mut ctrl = Vec::new();
    let mut db = Vec::with_capacity(new.len());
    let mut eb = Vec::new();
    let mut n_ctrl = 0u64;

    if old.is_empty() {
        if !new.is_empty() {
            put_u64(&mut ctrl, 0);
            put_u64(&mut ctrl, new.len() as u64);
            put_i64(&mut ctrl, 0);
            eb.extend_from_slice(new);
            n_ctrl = 1;
        }
        return assemble(n_ctrl, &ctrl, &db, &eb);
    }

    let mut sa = vec![0i32; old.len()];
    divsufsort::sort_in_place(old, &mut sa);

    let oldsize = old.len() as i64;
    let newsize = new.len() as i64;
    let at_old = |i: i64| old[i as usize];
    let at_new = |i: i64| new[i as usize];
    let (mut scan, mut len, mut pos) = (0i64, 0i64, 0i64);
    let (mut lastscan, mut lastpos, mut lastoffset) = (0i64, 0i64, 0i64);

    while scan < newsize {
        let mut oldscore = 0i64;
        scan += len;
        let mut scsc = scan;
        while scan < newsize {
            let (p, l) = search(&sa, old, &new[scan as usize..]);
            pos = p as i64;
            len = l as i64;
            while scsc < scan + len {
                if scsc + lastoffset < oldsize && at_old(scsc + lastoffset) == at_new(scsc) {
                    oldscore += 1;
                }
                scsc += 1;
            }
            if (len == oldscore && len != 0) || len > oldscore + 8 {
                break;
            }
            if scan + lastoffset < oldsize && at_old(scan + lastoffset) == at_new(scan) {
                oldscore -= 1;
            }
            scan += 1;
        }

        if len != oldscore || scan == newsize {
            // Extend the previous match forward...
            let (mut s, mut sf, mut lenf) = (0i64, 0i64, 0i64);
            let mut i = 0i64;
            while lastscan + i < scan && lastpos + i < oldsize {
                if at_old(lastpos + i) == at_new(lastscan + i) {
                    s += 1;
                }
                i += 1;
                if s * 2 - i > sf * 2 - lenf {
                    sf = s;
                    lenf = i;
                }
            }
            // ...and the new match backward.
            let mut lenb = 0i64;
            if scan < newsize {
                let (mut s, mut sb) = (0i64, 0i64);
                let mut i = 1i64;
                while scan >= lastscan + i && pos >= i {
                    if at_old(pos - i) == at_new(scan - i) {
                        s += 1;
                    }
                    if s * 2 - i > sb * 2 - lenb {
                        sb = s;
                        lenb = i;
                    }
                    i += 1;
                }
            }
            // Resolve overlap between the two extensions.
            if lastscan + lenf > scan - lenb {
                let overlap = (lastscan + lenf) - (scan - lenb);
                let (mut s, mut ss, mut lens) = (0i64, 0i64, 0i64);
                for i in 0..overlap {
                    if at_new(lastscan + lenf - overlap + i) == at_old(lastpos + lenf - overlap + i) {
                        s += 1;
                    }
                    if at_new(scan - lenb + i) == at_old(pos - lenb + i) {
                        s -= 1;
                    }
                    if s > ss {
                        ss = s;
                        lens = i + 1;
                    }
                }
                lenf += lens - overlap;
                lenb -= lens;
            }

            let (ls, lp, lf) = (lastscan as usize, lastpos as usize, lenf as usize);
            db.extend(new[ls..ls + lf].iter().zip(&old[lp..lp + lf]).map(|(n, o)| n.wrapping_sub(*o)));
            let extra_start = ls + lf;
            let extra_end = (scan - lenb) as usize;
            eb.extend_from_slice(&new[extra_start..extra_end]);
            put_u64(&mut ctrl, lf as u64);
            put_u64(&mut ctrl, (extra_end - extra_start) as u64);
            put_i64(&mut ctrl, (pos - lenb) - (lastpos + lenf));
            n_ctrl += 1;

            lastscan = scan - lenb;
            lastpos = pos - lenb;
            lastoffset = pos - scan;
        }
    }
    assemble(n_ctrl, &ctrl, &db, &eb)
}

fn assemble(n_ctrl: u64, ctrl: &[u8], db: &[u8], eb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ctrl.len() + db.len() + eb.len() + 32);
    put_u64(&mut out, n_ctrl);
    put_u64(&mut out, ctrl.len() as u64);
    put_u64(&mut out, db.len() as u64);
    put_u64(&mut out, eb.len() as u64);
    out.extend_from_slice(ctrl);
    out.extend_from_slice(db);
    out.extend_from_slice(eb);
    out
}

/// Longest match of a prefix of `new` among the suffixes of `old`.
fn search(sa: &[i32], old: &[u8], new: &[u8]) -> (usize, usize) {
    let (mut st, mut en) = (0usize, sa.len() - 1);
    while en - st >= 2 {
        let x = st + (en - st) / 2;
        let suf = &old[sa[x] as usize..];
        let n = suf.len().min(new.len());
        if suf[..n] < new[..n] {
            st = x;
        } else {
            en = x;
        }
    }
    let x = matchlen(&old[sa[st] as usize..], new);
    let y = matchlen(&old[sa[en] as usize..], new);
    if x > y { (sa[st] as usize, x) } else { (sa[en] as usize, y) }
}

fn matchlen(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().unwrap());
        let y = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if x != y {
            return i + ((x ^ y).trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use patcher_core::codec::bsdiff_apply;

    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn roundtrips() {
        let mut s = 0x9E3779B97F4A7C15;
        let base: Vec<u8> = (0..200_000).map(|i| if i % 7 == 0 { rng(&mut s) as u8 } else { (i / 13) as u8 }).collect();
        let mut new = base.clone();
        new.splice(5000..5000, (0..3000).map(|_| rng(&mut s) as u8));
        new.drain(100_000..120_000);
        for i in (0..new.len()).step_by(997) {
            new[i] = new[i].wrapping_add(1);
        }
        for (old, new) in [(&base[..], &new[..]), (&[][..], &new[..]), (&base[..], &[][..]), (&b"a"[..], &b"a"[..])] {
            let p = diff(old, new);
            assert_eq!(bsdiff_apply(old, &p, new.len()).unwrap(), new);
        }
    }
}
