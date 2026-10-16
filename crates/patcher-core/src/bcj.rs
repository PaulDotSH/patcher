//! Reversible x86 branch filter.
//!
//! `E8`/`E9` (CALL/JMP rel32) operands are relative to the instruction, so inserting code
//! changes every call that crosses the insertion point. Converting them to absolute
//! positions makes those operands identical again, which diff and LZ coders exploit.
//!
//! Every `E8`/`E9` byte consumes the following 4 operand bytes, so no conversion can
//! touch bytes another decision depends on. Only operands whose top byte is `00`/`FF`
//! (|rel| < 16 MiB, i.e. real near calls) are converted, and the result keeps that
//! property (25-bit arithmetic, sign-extended) while unconverted operands never have it,
//! so the decoder makes exactly the same decisions as the encoder without extra state.

pub fn encode(buf: &mut [u8]) {
    run(buf, true)
}

pub fn decode(buf: &mut [u8]) {
    run(buf, false)
}

fn run(buf: &mut [u8], encode: bool) {
    if buf.len() < 5 {
        return;
    }
    let limit = buf.len() - 5;
    let mut i = 0;
    while i <= limit {
        let b = buf[i];
        if b != 0xE8 && b != 0xE9 {
            i += 1;
            continue;
        }
        let top = buf[i + 4];
        if top == 0x00 || top == 0xFF {
            let v = u32::from_le_bytes(buf[i + 1..i + 5].try_into().unwrap());
            let pos = (i as u32).wrapping_add(5);
            let r = if encode { v.wrapping_add(pos) } else { v.wrapping_sub(pos) };
            let r = (((r << 7) as i32) >> 7) as u32;
            buf[i + 1..i + 5].copy_from_slice(&r.to_le_bytes());
        }
        i += 5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> u8 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*seed >> 33) as u8
    }

    #[test]
    fn roundtrip_random() {
        let mut s = 1;
        for len in [0, 1, 4, 5, 6, 100, 10_000] {
            let mut data: Vec<u8> = (0..len)
                .map(|_| match lcg(&mut s) % 4 {
                    0 => 0xE8,
                    1 => 0xFF,
                    2 => 0x00,
                    _ => lcg(&mut s),
                })
                .collect();
            let orig = data.clone();
            encode(&mut data);
            decode(&mut data);
            assert_eq!(data, orig, "len {len}");
        }
    }

    #[test]
    fn shifted_calls_become_identical() {
        // call to absolute target 0x1000 from position p
        let call_at = |p: usize| {
            let rel = (0x1000i64 - (p as i64 + 5)) as i32;
            let mut v = vec![0xE8];
            v.extend_from_slice(&rel.to_le_bytes());
            v
        };
        let mut a = vec![0x90; 16];
        a.extend(call_at(16));
        let mut b = vec![0x90; 20];
        b.extend(call_at(20));
        encode(&mut a);
        encode(&mut b);
        assert_eq!(a[16..], b[20..]);
    }
}
