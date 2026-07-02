use dashmap::DashMap;
use fxhash::FxBuildHasher;
use memmap2::Mmap;
use rayon::prelude::*;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::Arc;

// Type alias for our shared, highly concurrent inverted index.
// Maps a unique token (String) to a list of file byte offsets where it occurs.
type InvertedIndex = Arc<DashMap<String, Vec<u64>, FxBuildHasher>>;

/// Splits a huge file into roughly equal byte chunks, ensuring boundaries fall exactly on newlines.
fn calculate_chunk_boundaries(mmap: &Mmap, num_chunks: usize) -> Vec<(usize, usize)> {
    let file_size = mmap.len();
    if file_size == 0 { return vec![]; }
    
    let chunk_size = file_size / num_chunks;
    let mut boundaries = Vec::new();
    let mut start = 0;

    for i in 1..num_chunks {
        let mut end = i * chunk_size;
        // Scan forward to find the next newline so we don't slice a log line in half
        while end < file_size && mmap[end] != b'\n' {
            end += 1;
        }
        if end < file_size {
            end += 1; // Include the newline in the current chunk
        }
        if start < file_size {
            boundaries.push((start, std::cmp::min(end, file_size)));
        }
        start = end;
    }
    
    if start < file_size {
        boundaries.push((start, file_size));
    }
    boundaries
}

/// Tokenizes a raw byte slice into alphanumeric words.
/// Zero allocations during tokenization by working directly with primitive byte slices.
fn tokenize<'a>(bytes: &'a [u8]) -> Vec<&'a str> {
    std::str::from_utf8(bytes)
        .unwrap_or("")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Linearly scans a single log line starting at `line_offset` and updates the thread-local map.
fn process_line(line_bytes: &[u8], line_offset: u64, local_index: &mut fxhash::FxHashMap<String, Vec<u64>>) {
    for token in tokenize(line_bytes) {
        // Only allocate the String key if it's the first time this thread has encountered it
        local_index
            .entry(token.to_string())
            .or_default()
            .push(line_offset);
    }
}

fn build_index(mmap: &Mmap) -> InvertedIndex {
    let num_cores = rayon::current_num_threads();
    let chunks = calculate_chunk_boundaries(mmap, num_cores * 4); // Oversample to keep CPU queues full
    
    let global_index: InvertedIndex = Arc::new(DashMap::with_hasher(FxBuildHasher::default()));

    // Parallel processing pipeline using Rayon's work-stealing pool
    chunks.into_par_iter().for_each(|(chunk_start, chunk_end)| {
        // Thread-local hashmap entirely eliminates lock contention during tokenization
        let mut local_index = fxhash::FxHashMap::default();
        
        let chunk_bytes = &mmap[chunk_start..chunk_end];
        let mut current_pos = 0;

        // Manual line scanning loop optimized for sequential memory access
        while current_pos < chunk_bytes.len() {
            let line_remainder = &chunk_bytes[current_pos..];
            let next_nl = line_remainder.iter().position(|&b| b == b'\n');
            
            let line_len = match next_nl {
                Some(idx) => idx + 1,
                None => line_remainder.len(),
            };

            let global_line_offset = (chunk_start + current_pos) as u64;
            let line_bytes = &line_remainder[..line_len];
            
            process_line(line_bytes, global_line_offset, &mut local_index);
            current_pos += line_len;
        }

        // Drain the thread-local index back into the shared concurrent DashMap
        for (token, mut offsets) in local_index {
            global_index.entry(token).or_default().append(&mut offsets);
        }
    });

    // Sort the posting lists so intersection queries are lightning fast
    global_index.iter_mut().for_each(|mut entry| {
        entry.value_mut().sort_unstable();
    });

    global_index
}

/// Given an absolute byte offset, prints out the entire log line cleanly.
fn print_line_at_offset(mmap: &Mmap, offset: u64) {
    let idx = offset as usize;
    if idx >= mmap.len() { return; }

    let remainder = &mmap[idx..];
    let line_len = remainder.iter().position(|&b| b == b'\n').unwrap_or(remainder.len());
    
    if let Ok(line_str) = std::str::from_utf8(&remainder[..line_len]) {
        println!("[Offset {}]: {}", offset, line_str);
    }
}

fn main() -> io::Result<()> {
    let file_path = "/home/kdo/instagrep/bench/data.log"; // Change this to your target file
    
    println!("Opening file and memory mapping...");
    let file = File::open(Path::new(file_path))?;
    let mmap = unsafe { Mmap::map(&file)? };
    // let mmap = unsafe { 
    //     memmap2::MmapOptions::new()
    //         .populate() 
    //         .map(&file)? 
    // };
    println!("Indexing {} file in parallel... (Sit back, this scales with your CPU cores)", file_path);
    let start_time = std::time::Instant::now();
    let index = build_index(&mmap);
    println!("Index built successfully in {:?}", start_time.elapsed());
    println!("Distinct tokens indexed: {}", index.len());

    // Simple interactive query loop
    let stdin = io::stdin();
    println!("\nEnter a search token (or 'exit' to quit):");
    for line in stdin.lock().lines() {
        let query = line?;
        if query == "exit" { break; }

        let query_cleaned = query.trim();
        let query_start = std::time::Instant::now();
        
        // Sub-millisecond lookup phase
        if let Some(offsets) = index.get(query_cleaned) {
            println!("Found {} matches in {:?}", offsets.len(), query_start.elapsed());
            println!("--- Showing first 5 matches ---");
            for &offset in offsets.iter().take(5) {
                print_line_at_offset(&mmap, offset);
            }
        } else {
            println!("Token '{}' not found ({:?})", query_cleaned, query_start.elapsed());
        }
        println!("\nEnter a search token:");
    }

    Ok(())
}
