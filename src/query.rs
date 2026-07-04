//! Regex → trigram query analysis.
//!
//! This is a faithful Rust port of Russ Cox's "trigram index" analysis from
//! google/codesearch (`index/regexp.go`), built on [`regex_syntax`]'s parsed
//! [`Hir`]. For each subexpression we track five facts — can-empty, the exact
//! set of matched strings (if known), the set of prefixes, the set of suffixes,
//! and a boolean trigram query — and combine them structurally. The result is a
//! *sound* trigram query: every file the regex matches is guaranteed to satisfy
//! the query, so candidate selection never produces false negatives, while
//! staying far more precise than scanning literal runs.
//!
//! Reference: <https://swtch.com/~rsc/regexp/regexp4.html>

use regex_syntax::hir::{Class, Hir, HirKind, Repetition};
use roaring::RoaringBitmap;

use crate::trigram::{self, Trigram};

/// Keep the exact set only while it has this few strings; beyond that it is
/// treated as unknown.
const MAX_EXACT: usize = 8;
/// Cap on the number of strings tracked in the prefix/suffix sets.
const MAX_SET: usize = 20;
/// A character class is enumerated only up to this many members.
const MAX_CLASS: u32 = 100;

/// A boolean query over trigrams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// No document can match (e.g. an empty character class).
    None,
    /// Every document is a candidate — the index cannot help.
    All,
    /// Conjunction: all trigrams and all sub-queries must hold.
    And { trigrams: Vec<Trigram>, sub: Vec<Query> },
    /// Disjunction: any trigram or sub-query holding is sufficient.
    Or { trigrams: Vec<Trigram>, sub: Vec<Query> },
}

impl Query {
    fn and(self, other: Query) -> Query {
        match (self, other) {
            (Query::None, _) | (_, Query::None) => Query::None,
            (Query::All, r) | (r, Query::All) => r,
            (Query::And { trigrams: mut t, sub: s }, Query::And { trigrams: t2, sub: s2 }) => {
                t.extend(t2);
                let mut s = s;
                s.extend(s2);
                dedup_trigrams(&mut t);
                Query::And { trigrams: t, sub: s }
            }
            (Query::And { trigrams: t, sub: mut s }, r) | (r, Query::And { trigrams: t, sub: mut s }) => {
                s.push(r);
                Query::And { trigrams: t, sub: s }
            }
            (l, r) => Query::And { trigrams: Vec::new(), sub: vec![l, r] },
        }
    }

    fn or(self, other: Query) -> Query {
        match (self, other) {
            (Query::None, r) | (r, Query::None) => r,
            (Query::All, _) | (_, Query::All) => Query::All,
            (Query::Or { trigrams: mut t, sub: s }, Query::Or { trigrams: t2, sub: s2 }) => {
                t.extend(t2);
                let mut s = s;
                s.extend(s2);
                dedup_trigrams(&mut t);
                Query::Or { trigrams: t, sub: s }
            }
            (Query::Or { trigrams: t, sub: mut s }, r) | (r, Query::Or { trigrams: t, sub: mut s }) => {
                s.push(r);
                Query::Or { trigrams: t, sub: s }
            }
            (l, r) => Query::Or { trigrams: Vec::new(), sub: vec![l, r] },
        }
    }

    /// Trigrams of a string set, OR'd across members (a string shorter than
    /// three bytes contributes no trigrams, i.e. it widens to `All`).
    fn from_string_set(set: &StringSet) -> Query {
        if set.strings.is_empty() {
            return Query::All;
        }
        let mut query = Query::None;
        for s in &set.strings {
            let tri = trigram::literal_trigrams(s);
            query = query.or(if tri.is_empty() {
                Query::All
            } else {
                Query::And { trigrams: tri, sub: Vec::new() }
            });
        }
        query
    }
}

fn dedup_trigrams(trigrams: &mut Vec<Trigram>) {
    if trigrams.len() > 1 {
        trigrams.sort_unstable();
        trigrams.dedup();
    }
}

/// A bounded set of byte strings, used to track exact / prefix / suffix sets.
#[derive(Clone, Default)]
struct StringSet {
    strings: Vec<Vec<u8>>,
}

impl StringSet {
    fn empty() -> Self {
        Self { strings: Vec::new() }
    }

    fn one(s: Vec<u8>) -> Self {
        Self { strings: vec![s] }
    }

    fn add(&mut self, s: Vec<u8>) {
        self.strings.push(s);
    }

    /// Cartesian product (each left concatenated with each right).
    fn cross(&self, other: &StringSet) -> StringSet {
        let mut out = StringSet::empty();
        for l in &self.strings {
            for r in &other.strings {
                let mut s = l.clone();
                s.extend_from_slice(r);
                out.strings.push(s);
            }
        }
        out
    }

    fn union(mut self, other: StringSet) -> StringSet {
        self.strings.extend(other.strings);
        self
    }

    fn truncate(&mut self) {
        // Drop any string that has another as a proper prefix.
        self.strings.sort();
        self.strings.dedup();
        if self.strings.len() <= 1 {
            return;
        }
        let mut keep: Vec<Vec<u8>> = Vec::with_capacity(self.strings.len());
        for s in &self.strings {
            let dominated = keep.last().is_some_and(|prev| s.starts_with(prev));
            if !dominated {
                keep.push(s.clone());
            }
        }
        self.strings = keep;

        // If still too large, drop the longest suffixes until it fits.
        while self.strings.len() > MAX_SET {
            let mut max_idx = 0;
            for (i, s) in self.strings.iter().enumerate() {
                if s.len() > self.strings[max_idx].len() {
                    max_idx = i;
                }
            }
            let s = &mut self.strings[max_idx];
            s.pop();
            if s.is_empty() {
                self.strings.swap_remove(max_idx);
            }
        }
    }
}

#[derive(Clone)]
struct Info {
    can_empty: bool,
    exact: Option<StringSet>, // None == unknown
    prefix: StringSet,
    suffix: StringSet,
    query: Query,
}

impl Info {
    fn any_match() -> Info {
        // Stars / option can match the empty string and impose no required
        // bytes, but they keep an empty-string prefix/suffix so concatenation
        // still propagates neighbours across them.
        Info {
            can_empty: true,
            exact: None,
            prefix: StringSet::one(Vec::new()),
            suffix: StringSet::one(Vec::new()),
            query: Query::All,
        }
    }
}

/// Parse a pattern and produce the sound trigram query that drives candidate
/// selection. Returns [`Query::None`] for patterns that can never match and
/// [`Query::All`] when the index cannot narrow the search.
pub fn analyze(pattern: &str) -> Query {
    let hir = match regex_syntax::Parser::new().parse(pattern) {
        Ok(hir) => hir,
        Err(_) => return Query::All,
    };
    let mut info = analyze_hir(&hir);
    simplify(&mut info);
    info.query
}

fn analyze_hir(hir: &Hir) -> Info {
    match hir.kind() {
        HirKind::Empty => Info {
            can_empty: true,
            exact: Some(StringSet::one(Vec::new())),
            prefix: StringSet::one(Vec::new()),
            suffix: StringSet::one(Vec::new()),
            query: Query::All,
        },
        HirKind::Literal(lit) => {
            let bytes = lit.0.to_vec();
            let set = StringSet::one(bytes.clone());
            Info {
                can_empty: false,
                exact: Some(set.clone()),
                prefix: set.clone(),
                suffix: set,
                query: Query::All,
            }
        }
        HirKind::Class(class) => analyze_class(class),
        HirKind::Look(_) => Info {
            can_empty: true,
            exact: None,
            prefix: StringSet::one(Vec::new()),
            suffix: StringSet::one(Vec::new()),
            query: Query::All,
        },
        HirKind::Repetition(rep) => analyze_repetition(rep),
        HirKind::Capture(cap) => analyze_hir(&cap.sub),
        HirKind::Concat(parts) => analyze_concat(parts),
        HirKind::Alternation(parts) => analyze_alternation(parts),
    }
}

fn analyze_class(class: &Class) -> Info {
    let ranges: Vec<(u32, u32)> = match class {
        Class::Unicode(u) => u.iter().map(|r| (r.start() as u32, r.end() as u32)).collect(),
        Class::Bytes(b) => b.iter().map(|r| (r.start() as u32, r.end() as u32)).collect(),
    };
    let mut count: u32 = 0;
    for &(lo, hi) in &ranges {
        count = count.saturating_add(hi - lo + 1);
    }
    if count == 0 {
        return Info { can_empty: false, exact: Some(StringSet::empty()), prefix: StringSet::empty(), suffix: StringSet::empty(), query: Query::None };
    }
    if count > MAX_CLASS {
        return Info::any_match();
    }
    let mut exact = StringSet::empty();
    for &(lo, hi) in &ranges {
        let mut ch = lo;
        loop {
            let mut buf = [0u8; 4];
            let s = match char::from_u32(ch) {
                Some(c) => c.encode_utf8(&mut buf).as_bytes().to_vec(),
                None => Vec::new(),
            };
            exact.add(s);
            if ch == hi {
                break;
            }
            ch += 1;
        }
    }
    Info {
        can_empty: false,
        exact: Some(exact.clone()),
        prefix: exact.clone(),
        suffix: exact,
        query: Query::All,
    }
}

fn analyze_repetition(rep: &Repetition) -> Info {
    let min = rep.min;
    if min == 0 {
        // *, ?, {0,n} — the body is optional, so nothing is required.
        return Info::any_match();
    }
    // +, {n,m} with n>=1 — the body must occur at least once, so its required
    // trigrams still apply, but we can no longer claim an exact match.
    let mut info = analyze_hir(&rep.sub);
    if let Some(exact) = info.exact.take() {
        info.prefix = exact.clone();
        info.suffix = exact;
    }
    info
}

fn analyze_concat(parts: &[Hir]) -> Info {
    let mut acc = analyze_hir(&parts[0]);
    for part in &parts[1..] {
        let next = analyze_hir(part);
        acc = concat(acc, next);
    }
    acc
}

fn analyze_alternation(parts: &[Hir]) -> Info {
    let mut acc = analyze_hir(&parts[0]);
    for part in &parts[1..] {
        let next = analyze_hir(part);
        let can_empty = acc.can_empty || next.can_empty;
        let exact = match (acc.exact, next.exact) {
            (Some(a), Some(b)) => Some(a.union(b)),
            _ => None,
        };
        let mut prefix = acc.prefix.union(next.prefix);
        let mut suffix = acc.suffix.union(next.suffix);
        prefix.truncate();
        suffix.truncate();
        let query = acc.query.or(next.query);
        acc = Info { can_empty, exact, prefix, suffix, query };
    }
    acc
}

fn concat(x: Info, y: Info) -> Info {
    let can_empty = x.can_empty && y.can_empty;

    let x_exact = x.exact;
    let y_exact = y.exact;

    let exact = match (&x_exact, &y_exact) {
        (Some(a), Some(b)) => {
            let crossed = a.cross(b);
            if crossed.strings.len() <= MAX_EXACT {
                Some(crossed)
            } else {
                None
            }
        }
        _ => None,
    };

    // Cross-boundary trigrams: required trigrams spanning the seam between the
    // two subexpressions (e.g. `ab[cd]ef` ⇒ abc/bce/cef | abd/bde/def).
    let mut query = x.query.clone().and(y.query.clone());
    if x_exact.is_none()
        && y_exact.is_none()
        && x.suffix.strings.len() <= MAX_SET
        && y.prefix.strings.len() <= MAX_SET
    {
        let mut seam_ok = false;
        for s in &x.suffix.strings {
            for p in &y.prefix.strings {
                if s.len() + p.len() >= 3 {
                    seam_ok = true;
                    break;
                }
            }
            if seam_ok {
                break;
            }
        }
        if seam_ok {
            let seam = x.suffix.cross(&y.prefix);
            query = query.and(Query::from_string_set(&seam));
        }
    }

    // Prefix of a concatenation (symmetric rule applies to suffix).
    let mut prefix = match x_exact {
        Some(xe) => xe.cross(&y.prefix),
        None if x.can_empty => x.prefix.union(y.prefix),
        None => x.prefix,
    };
    let mut suffix = match y_exact {
        Some(ye) => ye.cross(&x.suffix),
        None if y.can_empty => y.suffix.union(x.suffix),
        None => y.suffix,
    };
    prefix.truncate();
    suffix.truncate();

    Info { can_empty, exact, prefix, suffix, query }
}

/// Fold the exact / prefix / suffix trigrams into the query (the
/// "information-saving transformations" from the codesearch article). When the
/// exact set is known its trigrams already subsume the prefix/suffix ones, so
/// we fold only exact in that case to avoid redundant work.
fn simplify(info: &mut Info) {
    match &info.exact {
        Some(exact) if !exact.strings.is_empty() => {
            info.query = info.query.clone().and(Query::from_string_set(exact));
        }
        _ => {
            if !info.prefix.strings.is_empty() {
                info.query = info.query.clone().and(Query::from_string_set(&info.prefix));
            }
            if !info.suffix.strings.is_empty() {
                info.query = info.query.clone().and(Query::from_string_set(&info.suffix));
            }
        }
    }
}

/// Result of evaluating a [`Query`] against the index.
#[derive(Debug)]
pub enum CandidateSet {
    All,
    Some(RoaringBitmap),
}

/// Evaluate `query` by looking each trigram up in the index. `lookup` returns
/// the bitmap of file ids that contain the trigram, if any.
pub fn evaluate(query: &Query, mut lookup: impl FnMut(Trigram) -> Option<RoaringBitmap>) -> CandidateSet {
    eval_inner(query, &mut lookup)
}

fn eval_inner(query: &Query, lookup: &mut impl FnMut(Trigram) -> Option<RoaringBitmap>) -> CandidateSet {
    match query {
        Query::None => CandidateSet::Some(RoaringBitmap::new()),
        Query::All => CandidateSet::All,
        Query::And { trigrams, sub } => {
            let mut acc: Option<RoaringBitmap> = None;
            for &t in trigrams {
                match lookup(t) {
                    None => return CandidateSet::Some(RoaringBitmap::new()),
                    Some(bm) => acc = Some(match acc {
                        None => bm,
                        Some(cur) => {
                            let mut cur = cur;
                            cur &= bm;
                            cur
                        }
                    }),
                }
                if acc.as_ref().is_some_and(|b| b.is_empty()) {
                    return CandidateSet::Some(RoaringBitmap::new());
                }
            }
            for s in sub {
                match eval_inner(s, lookup) {
                    CandidateSet::All => {}
                    CandidateSet::Some(bm) => acc = Some(match acc {
                        None => bm,
                        Some(cur) => {
                            let mut cur = cur;
                            cur &= bm;
                            cur
                        }
                    }),
                }
                if acc.as_ref().is_some_and(|b| b.is_empty()) {
                    return CandidateSet::Some(RoaringBitmap::new());
                }
            }
            match acc {
                None => CandidateSet::All,
                Some(bm) => CandidateSet::Some(bm),
            }
        }
        Query::Or { trigrams, sub } => {
            let mut acc: Option<RoaringBitmap> = None;
            for &t in trigrams {
                if let Some(bm) = lookup(t) {
                    acc = Some(match acc {
                        None => bm,
                        Some(mut cur) => {
                            cur |= bm;
                            cur
                        }
                    });
                }
            }
            for s in sub {
                match eval_inner(s, lookup) {
                    CandidateSet::All => return CandidateSet::All,
                    CandidateSet::Some(bm) => acc = Some(match acc {
                        None => bm,
                        Some(mut cur) => {
                            cur |= bm;
                            cur
                        }
                    }),
                }
            }
            match acc {
                None => CandidateSet::Some(RoaringBitmap::new()),
                Some(bm) => CandidateSet::Some(bm),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigram::encode;

    #[test]
    fn literal_decomposes_to_and_of_trigrams() {
        // "hello" ⇒ exact folded in: AND of hel, ell, llo.
        let q = analyze("hello");
        match q {
            Query::And { trigrams, sub } => {
                let mut t = trigrams.clone();
                t.sort_unstable();
                let mut expected = vec![
                    encode(b'h', b'e', b'l'),
                    encode(b'e', b'l', b'l'),
                    encode(b'l', b'l', b'o'),
                ];
                expected.sort_unstable();
                assert_eq!(t, expected);
                assert!(sub.is_empty());
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn alternation_decomposes_to_or() {
        // cat|dog ⇒ OR of (cat) and (dog).
        let q = analyze("cat|dog");
        match q {
            Query::Or { sub, .. } => {
                assert_eq!(sub.len(), 2);
                for branch in &sub {
                    assert!(matches!(branch, Query::And { trigrams, .. } if trigrams.len() == 1), "{branch:?}");
                }
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    #[test]
    fn wildcard_forces_full_scan() {
        assert_eq!(analyze(".*"), Query::All);
        assert_eq!(analyze("ab"), Query::All); // too short for any trigram
        assert_eq!(analyze("."), Query::All);
    }

    #[test]
    fn char_class_across_boundary_is_precise() {
        // ab[cd]ef ⇒ OR of (abc,bce,cef) and (abd,bde,def).
        let q = analyze("ab[cd]ef");
        match q {
            Query::Or { sub, .. } => {
                assert_eq!(sub.len(), 2);
                for branch in &sub {
                    assert!(matches!(branch, Query::And { trigrams, .. } if trigrams.len() == 3), "{branch:?}");
                }
            }
            other => panic!("expected Or, got {other:?}"),
        }
    }

    #[test]
    fn anchored_literal_still_extracted() {
        // ^abc ⇒ prefix "abc" survives the anchor.
        let q = analyze("^abc");
        match q {
            Query::And { trigrams, .. } => assert_eq!(trigrams, vec![encode(b'a', b'b', b'c')]),
            other => panic!("expected And, got {other:?}"),
        }
    }
}
