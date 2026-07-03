use dashmap::DashMap;
use fxhash::FxBuildHasher;
use memmap2::Mmap;
use rayon::prelude::*;
use roaring::RoaringBitmap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::Arc;

use std::mem::size_of_val;

mod index;

// [u8; 3] keys eliminate character decoding overhead entirely.
// RoaringBitmap compresses identical or dense line IDs down to minimal bit ranges.
type CompressedTrigramIndex = Arc<DashMap<[u8; 3], RoaringBitmap, FxBuildHasher>>;

/// PASS 1: Scans the raw memory map in parallel to harvest the starting byte position of every line.
fn collect_line_offsets(mmap: &Mmap) -> Vec<u64> {
    let file_size = mmap.len();
    if file_size == 0 { return vec![]; }

    let num_cores = rayon::current_num_threads();
    let chunk_size = file_size / num_cores;
    
    let mut byte_chunks = Vec::new();
    let mut start = 0;
    for i in 1..num_cores {
        let mut end = i * chunk_size;
        while end < file_size && mmap[end] != b'\n' { end += 1; }
        if end < file_size { end += 1; }
        byte_chunks.push((start, end));
        start = end;
    }
    if start < file_size { byte_chunks.push((start, file_size)); }

    byte_chunks.into_par_iter().flat_map(|(start, end)| {
        let mut local_offsets = Vec::new();
        let mut pos = start;
        if pos < end {
            local_offsets.push(pos as u64); 
        }
        while pos < end {
            if mmap[pos] == b'\n' && pos + 1 < end {
                local_offsets.push((pos + 1) as u64);
            }
            pos += 1;
        }
        local_offsets
    }).collect()
}

/// Zero-allocation sliding window over a raw line slice.
#[inline(always)]
fn extract_trigrams_bytes(line_bytes: &[u8], mut feed_trigram: impl FnMut([u8; 3])) {
    if line_bytes.len() < 3 { return; }
    for window in line_bytes.windows(3) {
        feed_trigram([window[0], window[1], window[2]]);
    }
}

/// PASS 2: Indexes the file by processing chunks of pre-calculated Line IDs concurrently.
fn build_compressed_index(mmap: &Mmap, line_offsets: &[u64]) -> CompressedTrigramIndex {
    let global_index: CompressedTrigramIndex = Arc::new(DashMap::with_hasher(FxBuildHasher::default()));
    let total_lines = line_offsets.len();
    
    // Chunk size of 100k lines strikes a perfect balance for thread-local map performance
    let chunk_size = 100_000;
    
    line_offsets.par_chunks(chunk_size).enumerate().for_each(|(chunk_idx, item_chunk)| {
        let base_line_id = (chunk_idx * chunk_size) as u32;
        let mut local_index: fxhash::FxHashMap<[u8; 3], Vec<u32>> = fxhash::FxHashMap::default();
        let mut seen_in_line = fxhash::FxHashSet::default();

        for (local_idx, &start_offset) in item_chunk.iter().enumerate() {
            let global_line_id = base_line_id + local_idx as u32;
            
            let end_offset = if (global_line_id as usize) + 1 < total_lines {
                line_offsets[(global_line_id as usize) + 1]
            } else {
                mmap.len() as u64
            };

            let line_bytes = &mmap[start_offset as usize..end_offset as usize];
            seen_in_line.clear();

            extract_trigrams_bytes(line_bytes, |trigram| {
                if seen_in_line.insert(trigram) {
                    local_index.entry(trigram).or_default().push(global_line_id);
                }
            });
        }

        // Merge local sorted batches directly into the global DashMap Roaring Bitmaps
        for (trigram, line_ids) in local_index {
            global_index.entry(trigram).or_default().extend(line_ids);
        }
    });
    
    println!("Optimizing index bitmap structures...");
    global_index.iter_mut().for_each(|mut entry| {
        entry.value_mut().optimize();
    });
    global_index
}

/// Extracts static literal byte sequences out of a regex pattern string.
fn extract_literal_trigrams_bytes_from_regex(pattern: &str) -> Vec<[u8; 3]> {
    let mut trigrams = Vec::new();
    let mut current_literal = Vec::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    
    while i < chars.len() {
        let c = chars[i];
        if ['\\', '.', '+', '*', '?', '(', ')', '[', ']', '{', '}', '^', '$', '|'].contains(&c) {
            if current_literal.len() >= 3 {
                for window in current_literal.windows(3) {
                    trigrams.push([window[0], window[1], window[2]]);
                }
            }
            current_literal.clear();
            if c == '\\' && i + 1 < chars.len() { i += 1; } 
        } else {
            let mut buf = [0; 4];
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                current_literal.push(b);
            }
        }
        i += 1;
    }
    if current_literal.len() >= 3 {
        for window in current_literal.windows(3) {
            trigrams.push([window[0], window[1], window[2]]);
        }
    }
    trigrams
}

/// Safely extracts a raw byte slice corresponding to a specific global Line ID.
#[inline(always)]
fn get_line_bytes<'a>(mmap: &'a Mmap, line_offsets: &[u64], line_id: u32) -> &'a [u8] {
    let idx = line_id as usize;
    let start = line_offsets[idx] as usize;
    let end = if idx + 1 < line_offsets.len() {
        line_offsets[idx + 1] as usize
    } else {
        mmap.len()
    };
    &mmap[start..end]
}

fn get_or_build_cache(
    mmap: &Mmap,
    cache_path: &Path
) -> io::Result<(Vec<u64>, CompressedTrigramIndex)>{
    if cache_path.exists() {
        println!("cache path exists using the path!");
        let total_start = std::time::Instant::now();
        let (offsets, idx) = index::load_index_from_cache(cache_path)?;
        Ok((offsets, idx))
    } else {
        println!("Pass 1: Identifying line boundaries...");
        let p1_start = std::time::Instant::now();
        let line_offsets = collect_line_offsets(mmap);
        println!("Found {} lines in {:?}", line_offsets.len(), p1_start.elapsed());

        println!("Pass 2: Building Compressed Trigram Index...");
        let p2_start = std::time::Instant::now();
        let index = build_compressed_index(mmap, &line_offsets);
        println!("Index completed successfully in {:?}", p2_start.elapsed());
        
        // Persist the built index to disk for next time
        index::save_index_to_cache(cache_path, line_offsets.clone(), Arc::clone(&index))?;

        Ok((line_offsets, index))
    }
}

fn main() -> io::Result<()> {
    let file_path = "/home/kdo/instagrep/bench/data.log"; 
    
    let cache_path = Path::new("/home/kdo/instagrep/bench/.index/data.log");


    println!("Opening file and memory mapping...");
    let file = File::open(Path::new(file_path))?;
    let mmap = unsafe { Mmap::map(&file)? };

    let Ok((line_offsets, index)) = get_or_build_cache(&mmap, cache_path) else {
        println!("Failed to get cache at {} and could not build", cache_path.display());
        panic!();
    };
    let stdin = io::stdin();
    println!("\nEnter a search Regex pattern (or 'exit' to quit):");
    for line in stdin.lock().lines() {
        let query = line?;
        if query == "exit" { break; }

        let query_cleaned = query.trim();
        let query_start = std::time::Instant::now();

        // Using regex::bytes::Regex completely avoids UTF-8 string casting validation during matches
        let regex = match regex::bytes::Regex::new(query_cleaned) {
            Ok(re) => re,
            Err(e) => {
                println!("Invalid Regex syntax: {}", e);
                continue;
            }
        };

        let required_trigrams = extract_literal_trigrams_bytes_from_regex(query_cleaned);
        let mut candidate_line_ids: Option<RoaringBitmap> = None;

        if !required_trigrams.is_empty() {
            for tg in required_trigrams {
                if let Some(bitmap) = index.get(&tg) {
                    match candidate_line_ids {
                        None => candidate_line_ids = Some(bitmap.value().clone()),
                        Some(ref mut current) => {
                            // Bitwise AND intersection directly on the compressed data structures
                            *current &= bitmap.value();
                        }
                    }
                } else {
                    // A mandatory literal trigram isn't in the index -> 0 absolute matches possible
                    candidate_line_ids = Some(RoaringBitmap::new());
                    break;
                }
            }
        }

        let mut match_count = 0;
        let mut matches_to_show = Vec::new();

        if let Some(candidates) = candidate_line_ids {
            // High-speed evaluation: Scan only lines passed by the roaring bitmap intersection filter
            for line_id in candidates.iter() {
                let line_bytes = get_line_bytes(&mmap, &line_offsets, line_id);
                if regex.is_match(line_bytes) {
                    match_count += 1;
                    if matches_to_show.len() < 5 {
                        let lossy_str = String::from_utf8_lossy(line_bytes).trim_end().to_string();
                        matches_to_show.push((line_id, lossy_str));
                    }
                }
            }
        } else {
            println!("Warning: No literal trigrams found. Falling back to parallel full-file scan...");
            // Fallback: Parallel scan over the lines array using Rayon if searching wildcards like ".*"
            let matching_ids: Vec<u32> = line_offsets
                .par_iter()
                .enumerate()
                .filter_map(|(line_id, _)| {
                    let line_bytes = get_line_bytes(&mmap, &line_offsets, line_id as u32);
                    if regex.is_match(line_bytes) {
                        Some(line_id as u32)
                    } else {
                        None
                    }
                })
                .collect();

            match_count = matching_ids.len();
            for &line_id in matching_ids.iter().take(5) {
                let line_bytes = get_line_bytes(&mmap, &line_offsets, line_id);
                let lossy_str = String::from_utf8_lossy(line_bytes).trim_end().to_string();
                matches_to_show.push((line_id, lossy_str));
            }
        }

        println!("Found {} matches in {:?}", match_count, query_start.elapsed());
        if match_count > 0 {
            println!("--- Showing first {} matches ---", std::cmp::min(5, match_count));
            for (line_id, line_str) in matches_to_show {
                println!("[Line {}]: {}", line_id, line_str);
            }
        }
        println!("\nEnter a search Regex pattern:");
    }

    Ok(())
}
