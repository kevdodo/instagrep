//! Trigram primitives.
//!
//! A trigram is three bytes packed into a `u32` (big-endian byte order so that
//! sorting `u32` keys sorts trigrams lexicographically). The index stores, per
//! trigram, the set of file ids that contain it (see [`crate::index`]).

use rustc_hash::FxHashSet;

pub type Trigram = u32;

/// Pack three bytes into a trigram key.
#[inline]
pub fn encode(a: u8, b: u8, c: u8) -> Trigram {
    ((a as u32) << 16) | ((b as u32) << 8) | c as u32
}

#[inline]
pub fn encode_slice(bytes: &[u8]) -> Trigram {
    encode(bytes[0], bytes[1], bytes[2])
}

/// All overlapping trigrams of a byte slice, in order.
pub fn extract_ordered(bytes: &[u8]) -> Vec<Trigram> {
    if bytes.len() < 3 {
        return Vec::new();
    }
    bytes.windows(3).map(encode_slice).collect()
}

/// The distinct trigrams a byte slice contains. Used at index time: each file
/// contributes the union of its trigrams to the inverted index.
#[allow(dead_code)]
pub fn extract_distinct(bytes: &[u8]) -> FxHashSet<Trigram> {
    let mut out = FxHashSet::default();
    if bytes.len() >= 3 {
        for w in bytes.windows(3) {
            out.insert(encode_slice(w));
        }
    }
    out
}

/// Distinct trigrams plus their ASCII-lowercased counterparts, so a single
/// index can serve both case-sensitive and case-insensitive queries.
///
/// Lowercasing is ASCII-only: full Unicode case folding on raw bytes is not
/// sound, and the overwhelming majority of source-code trigrams are ASCII.
/// Matching itself is delegated to the (Unicode-aware) regex engine, so this
/// only affects candidate pruning and is always conservative.
pub fn extract_distinct_folded(bytes: &[u8]) -> FxHashSet<Trigram> {
    let mut out = FxHashSet::default();
    if bytes.len() < 3 {
        return out;
    }
    for w in bytes.windows(3) {
        let (a, b, c) = (w[0], w[1], w[2]);
        out.insert(encode(a, b, c));
        out.insert(encode(
            a.to_ascii_lowercase(),
            b.to_ascii_lowercase(),
            c.to_ascii_lowercase(),
        ));
    }
    out
}

/// Trigrams required by a literal of three or more bytes (empty if too short).
pub fn literal_trigrams(literal: &[u8]) -> Vec<Trigram> {
    extract_ordered(literal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_trigrams() {
        assert_eq!(
            extract_ordered(b"abcde"),
            vec![encode(b'a', b'b', b'c'), encode(b'b', b'c', b'd'), encode(b'c', b'd', b'e')]
        );
    }

    #[test]
    fn distinct_folded_includes_both_cases() {
        let set = extract_distinct_folded(b"ABC");
        assert!(set.contains(&encode(b'A', b'B', b'C')));
        assert!(set.contains(&encode(b'a', b'b', b'c')));
    }

    #[test]
    fn short_literals_have_no_trigrams() {
        assert!(extract_ordered(b"ab").is_empty());
        assert!(extract_distinct(b"ab").is_empty());
    }
}
