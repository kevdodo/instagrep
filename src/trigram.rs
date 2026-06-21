use std::collections::HashMap;

pub type Trigram = u32;

#[inline]
pub fn encode(bytes: &[u8]) -> Trigram {
    ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32
}

pub fn extract_with_masks(bytes: &[u8]) -> HashMap<Trigram, (u8, u8)> {
    let mut out = HashMap::new();
    if bytes.len() < 3 {
        return out;
    }

    for pos in 0..=(bytes.len() - 3) {
        let key = encode(&bytes[pos..pos + 3]);
        let loc_bit = 1u8 << (pos & 7);
        let next_bit = bytes.get(pos + 3).map(|b| 1u8 << (b & 7)).unwrap_or(0);

        out.entry(key)
            .and_modify(|(next, loc)| {
                *next |= next_bit;
                *loc |= loc_bit;
            })
            .or_insert((next_bit, loc_bit));
    }

    out
}

pub fn extract_ordered(bytes: &[u8]) -> Vec<Trigram> {
    if bytes.len() < 3 {
        return Vec::new();
    }

    bytes.windows(3).map(encode).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_trigrams() {
        let got = extract_ordered(b"abcde");
        assert_eq!(got, vec![encode(b"abc"), encode(b"bcd"), encode(b"cde")]);
    }

    #[test]
    fn masks_encode_next_and_location() {
        let got = extract_with_masks(b"abcabc");
        let (next, loc) = got[&encode(b"abc")];
        assert_ne!(loc & 1, 0);
        assert_ne!(loc & 8, 0);
        assert_ne!(next & (1 << (b'a' & 7)), 0);
    }
}
