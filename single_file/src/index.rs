// src/index.rs
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::io::BufWriter; // FIXED: BufWriter is in std::io, not std::fs
use dashmap::DashMap;
use fxhash::FxBuildHasher;
use roaring::RoaringBitmap;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::CompressedTrigramIndex;

#[derive(Serialize, Deserialize)]
struct CachedIndex {
    line_offsets: Vec<u64>,
    trigrams: HashMap<[u8; 3], RoaringBitmap, FxBuildHasher>
}

// FIXED: Added 'pub' so main.rs can call it
pub fn save_index_to_cache(
    cache_path: &Path,
    line_offsets: Vec<u64>,
    global_index: CompressedTrigramIndex
) -> io::Result<()> {
    println!("Serializing index to cache file: {:?}", cache_path);
    
    // Deconstruct the Arc and collect the DashMap cleanly into a standard HashMap
    let dashmap = Arc::try_unwrap(global_index)
        .unwrap_or_else(|arc| (*arc).clone());
    
    // FIXED: Collect directly into a standard HashMap to resolve type mismatch
    let trigrams: HashMap<[u8; 3], RoaringBitmap, FxBuildHasher> = dashmap.into_iter().collect();

    let cache_data = CachedIndex { line_offsets, trigrams };
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent);
    };
    let file = File::create(cache_path)?;
    let writer = BufWriter::new(file);
    
    bincode::serialize_into(writer, &cache_data)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        
    println!("Cache saved successfully.");
    Ok(())
}

pub fn load_index_from_cache(
    cache_path: &Path
) -> io::Result<(Vec<u64>, CompressedTrigramIndex)> {
    println!("Loading index from cache: {:?}", cache_path);
    let file = File::open(cache_path)?;
    let reader = io::BufReader::new(file);

    let cache_data: CachedIndex = bincode::deserialize_from(reader)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let global_index = DashMap::from_iter(cache_data.trigrams);
    Ok((cache_data.line_offsets, Arc::new(global_index)))
}