use std::collections::{HashMap, HashSet};

use crate::trigram::{self, Trigram};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    All(Vec<Trigram>),
    Any(Vec<Query>),
    None,
}

pub fn decompose(pattern: &str) -> Query {
    let branches = split_alternations(pattern.as_bytes());
    if branches.len() == 1 {
        return extract_from_branch(&branches[0]);
    }

    let trees: Vec<_> = branches
        .iter()
        .map(|branch| extract_from_branch(branch))
        .collect();
    if trees.iter().any(|tree| matches!(tree, Query::None)) {
        Query::None
    } else {
        Query::Any(trees)
    }
}

pub fn evaluate_masked<F>(query: &Query, mut lookup: F) -> CandidateSet
where
    F: FnMut(Trigram) -> HashMap<u32, (u8, u8)>,
{
    evaluate_masked_inner(query, &mut lookup)
}

fn evaluate_masked_inner<F>(query: &Query, lookup: &mut F) -> CandidateSet
where
    F: FnMut(Trigram) -> HashMap<u32, (u8, u8)>,
{
    match query {
        Query::None => CandidateSet::All,
        Query::All(trigrams) => evaluate_all(trigrams, lookup),
        Query::Any(branches) => {
            let mut union = HashSet::new();
            for branch in branches {
                match evaluate_masked_inner(branch, lookup) {
                    CandidateSet::All => return CandidateSet::All,
                    CandidateSet::Some(ids) => union.extend(ids),
                }
            }
            CandidateSet::Some(union)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateSet {
    All,
    Some(HashSet<u32>),
}

fn evaluate_all<F>(trigrams: &[Trigram], lookup: &mut F) -> CandidateSet
where
    F: FnMut(Trigram) -> HashMap<u32, (u8, u8)>,
{
    let Some((&first, rest)) = trigrams.split_first() else {
        return CandidateSet::Some(HashSet::new());
    };
    let mut candidates = lookup(first);

    for &trigram in rest {
        let last_byte = (trigram & 0xff) as u8;
        let next_bit = 1u8 << (last_byte & 7);
        let prefiltered: HashSet<u32> = candidates
            .iter()
            .filter_map(|(&id, &(next_mask, _))| (next_mask & next_bit != 0).then_some(id))
            .collect();

        candidates = lookup(trigram)
            .into_iter()
            .filter(|(id, _)| prefiltered.contains(id))
            .collect();

        if candidates.is_empty() {
            break;
        }
    }

    CandidateSet::Some(candidates.into_keys().collect())
}

pub fn extract_literals(pattern: &[u8]) -> Vec<Vec<u8>> {
    let mut literals = Vec::new();
    let mut current = Vec::new();
    let mut i = 0;

    while i < pattern.len() {
        match pattern[i] {
            b'\\' if i + 1 < pattern.len() => {
                let escaped = pattern[i + 1];
                if matches!(
                    escaped,
                    b'w' | b'W'
                        | b'd'
                        | b'D'
                        | b's'
                        | b'S'
                        | b't'
                        | b'b'
                        | b'n'
                        | b'r'
                        | b'f'
                        | b'v'
                ) {
                    finish_literal(&mut current, &mut literals);
                } else {
                    current.push(escaped);
                }
                i += 2;
            }
            b'[' => {
                finish_literal(&mut current, &mut literals);
                i = skip_char_class(pattern, i + 1);
            }
            b'*' | b'+' | b'?' => {
                current.pop();
                finish_literal(&mut current, &mut literals);
                i += 1;
            }
            b'.' | b'^' | b'$' | b'(' | b')' => {
                finish_literal(&mut current, &mut literals);
                i += 1;
            }
            b'{' => {
                current.pop();
                finish_literal(&mut current, &mut literals);
                i = skip_until(pattern, i + 1, b'}');
            }
            byte => {
                current.push(byte);
                i += 1;
            }
        }
    }

    finish_literal(&mut current, &mut literals);
    literals
}

fn extract_from_branch(branch: &[u8]) -> Query {
    let mut seen = HashSet::new();
    let mut trigrams = Vec::new();
    for literal in extract_literals(branch) {
        for trigram in trigram::extract_ordered(&literal) {
            if seen.insert(trigram) {
                trigrams.push(trigram);
            }
        }
    }

    if trigrams.is_empty() {
        Query::None
    } else {
        Query::All(trigrams)
    }
}

fn finish_literal(current: &mut Vec<u8>, literals: &mut Vec<Vec<u8>>) {
    if !current.is_empty() {
        literals.push(std::mem::take(current));
    }
}

fn split_alternations(pattern: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut paren = 0usize;
    let mut bracket = false;
    let mut i = 0;

    while i < pattern.len() {
        let byte = pattern[i];
        if byte == b'\\' && i + 1 < pattern.len() {
            current.push(byte);
            current.push(pattern[i + 1]);
            i += 2;
            continue;
        }

        match byte {
            b'(' if !bracket => paren += 1,
            b')' if !bracket => paren = paren.saturating_sub(1),
            b'[' => bracket = true,
            b']' => bracket = false,
            b'|' if paren == 0 && !bracket => {
                out.push(std::mem::take(&mut current));
                i += 1;
                continue;
            }
            _ => {}
        }

        current.push(byte);
        i += 1;
    }

    out.push(current);
    out
}

fn skip_char_class(pattern: &[u8], mut i: usize) -> usize {
    while i < pattern.len() {
        if pattern[i] == b'\\' {
            i += 2;
        } else if pattern[i] == b']' {
            return i + 1;
        } else {
            i += 1;
        }
    }
    i
}

fn skip_until(pattern: &[u8], mut i: usize, target: u8) -> usize {
    while i < pattern.len() {
        if pattern[i] == target {
            return i + 1;
        }
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigram::encode;

    #[test]
    fn literal_decomposes_to_all_trigrams() {
        assert_eq!(
            decompose("hello"),
            Query::All(vec![encode(b"hel"), encode(b"ell"), encode(b"llo")])
        );
    }

    #[test]
    fn alternation_decomposes_to_any() {
        assert_eq!(
            decompose("cat|dog"),
            Query::Any(vec![
                Query::All(vec![encode(b"cat")]),
                Query::All(vec![encode(b"dog")])
            ])
        );
    }

    #[test]
    fn wildcard_can_force_full_scan() {
        assert_eq!(decompose(".*"), Query::None);
        assert_eq!(decompose("ab"), Query::None);
    }

    #[test]
    fn escaped_literals_are_kept() {
        let literals = extract_literals(br"a\.b");
        assert!(literals.iter().any(|literal| literal == b"a.b"));
    }
}
