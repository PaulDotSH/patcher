//! LEB128 varints, shared by the bsdiff encoder (creator) and decoder (stub).
//!
//! bsdiff payload layout (before compression):
//! ```text
//! n_ctrl, ctrl_len, diff_len, extra_len      (varints)
//! ctrl:  n_ctrl x (add_len, copy_len, zigzag seek)
//! diff:  bytes added to the base bytes (wrapping)
//! extra: literal bytes
//! ```

pub fn put_u64(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn put_i64(out: &mut Vec<u8>, v: i64) {
    put_u64(out, ((v << 1) ^ (v >> 63)) as u64);
}

pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf }
    }

    pub fn u64(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for (i, &b) in self.buf.iter().enumerate().take(10) {
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                self.buf = &self.buf[i + 1..];
                return Some(v);
            }
        }
        None
    }

    pub fn usize(&mut self) -> Option<usize> {
        self.u64().and_then(|v| usize::try_from(v).ok())
    }

    pub fn i64(&mut self) -> Option<i64> {
        self.u64().map(|v| ((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    pub fn rest(self) -> &'a [u8] {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        let us = [0u64, 1, 127, 128, 300, u64::MAX];
        let is = [0i64, -1, 1, -64, 64, i64::MIN, i64::MAX];
        us.iter().for_each(|&v| put_u64(&mut buf, v));
        is.iter().for_each(|&v| put_i64(&mut buf, v));
        let mut r = Reader::new(&buf);
        for &v in &us {
            assert_eq!(r.u64(), Some(v));
        }
        for &v in &is {
            assert_eq!(r.i64(), Some(v));
        }
        assert!(r.rest().is_empty());
    }
}
