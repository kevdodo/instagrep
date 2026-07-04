//! The persisted trigram inverted index.
//!
//! Each distinct trigram maps to a [`RoaringBitmap`] of file ids that contain
//! it. Roaring bitmaps compress sorted integer sets aggressively (a trigram
//! present in every file of a 5k-file repo is a single bit-range, not 5k ids)
//! and intersect with SIMD-accelerated bitwise operations, which is exactly the
//! hot path of candidate selection. The on-disk format is a compact,
//! version-tagged binary blob.
//!
//! Paths are stored **relative to the index root** so an index is portable and
//! its output is clean.

use std::fs::{self, File};
use std::io::{self, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::*;
use roaring::RoaringBitmap;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::hash_map::Entry;

use crate::scanner::{self, WalkOptions};
use crate::trigram::{self, Trigram};

const INDEX_DIR: &str = ".instantgrep";
const INDEX_FILE: &str = "index.bin";
const MAGIC: &[u8; 7] = b"IGRUST\0";
const VERSION: u8 = 3;

#[derive(Debug, Clone)]
pub struct Index {
    /// File paths relative to the index root, sorted. The array index is the
    /// file id used in posting bitmaps.
    pub files: Vec<PathBuf>,
    /// Parallel to [`files`]; metadata used to detect changes for updates.
    pub metas: Vec<FileMeta>,
    pub postings: FxHashMap<Trigram, RoaringBitmap>,
    /// Ids whose files have been removed by an incremental update. Kept (not
    /// compacted) so the file ids in postings stay stable; resolved/iterated
    /// results skip them, and a later add may reuse the slot.
    pub removed_ids: RoaringBitmap,
    pub build_time_us: u128,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileMeta {
    pub mtime_secs: i64,
    pub size: u64,
}

/// Build a fresh index for `base` using the given walk options.
pub fn build(base: &Path, opts: &WalkOptions) -> Index {
    let started = Instant::now();
    let files = scanner::scan(base, opts);
    let n = files.len();
    let chunk = chunk_size(n);

    let partials: Vec<ChunkResult> = files
        .par_chunks(chunk)
        .enumerate()
        .map(|(chunk_index, chunk_files)| {
            let base_id = (chunk_index * chunk) as u32;
            let mut map: FxHashMap<Trigram, RoaringBitmap> = FxHashMap::default();
            let mut metas: Vec<(u32, FileMeta)> = Vec::with_capacity(chunk_files.len());
            for (offset, rel) in chunk_files.iter().enumerate() {
                let id = base_id + offset as u32;
                let abs = base.join(rel);
                if let Some(meta) = index_file(&abs, id, &mut map) {
                    metas.push((id, meta));
                }
            }
            ChunkResult { map, metas }
        })
        .collect();

    let mut postings: FxHashMap<Trigram, RoaringBitmap> = FxHashMap::default();
    let mut metas = vec![FileMeta::default(); n];
    for partial in partials {
        for (id, meta) in partial.metas {
            metas[id as usize] = meta;
        }
        for (trigram, bitmap) in partial.map {
            match postings.entry(trigram) {
                Entry::Occupied(mut o) => *o.get_mut() |= &bitmap,
                Entry::Vacant(v) => {
                    v.insert(bitmap);
                }
            }
        }
    }

    Index {
        files,
        metas,
        postings,
        removed_ids: RoaringBitmap::new(),
        build_time_us: started.elapsed().as_micros(),
    }
}

struct ChunkResult {
    map: FxHashMap<Trigram, RoaringBitmap>,
    metas: Vec<(u32, FileMeta)>,
}

fn chunk_size(n: usize) -> usize {
    let workers = std::thread::available_parallelism().map(usize::from).unwrap_or(1);
    (n / workers).max(64).min(n.max(1))
}

/// Incrementally refresh the index. Added, changed, *and* removed files are
/// all applied surgically: each affected file's previously-indexed trigram set
/// is reconstructed straight from the posting bitmaps (a trigram `t` was indexed
/// for file `id` iff `postings[t]` contains `id`), so the add/remove delta is
/// exact — no git, no per-file trigram storage, no content history. A full
/// rebuild only happens when the change set is large enough that scanning the
/// whole tree would be cheaper.
pub fn update(base: &Path, opts: &WalkOptions) -> io::Result<Index> {
    let Some(mut old) = load(base)? else {
        eprintln!("No existing index found - performing full build...");
        let index = build(base, opts);
        save(&index, base)?;
        return Ok(index);
    };

    let current = scanner::scan(base, opts);
    let old_ids: FxHashMap<&Path, u32> = old
        .files
        .iter()
        .enumerate()
        .filter(|(id, _)| !old.removed_ids.contains(*id as u32))
        .map(|(i, p)| (p.as_path(), i as u32))
        .collect();
    let current_set: FxHashSet<&Path> = current.iter().map(|p| p.as_path()).collect();

    let mut added: Vec<PathBuf> = Vec::new();
    let mut changed: Vec<(u32, PathBuf)> = Vec::new();
    for rel in &current {
        match old_ids.get(rel.as_path()) {
            None => added.push(rel.clone()),
            Some(&id) => {
                if changed_since(base, rel, &old.metas[id as usize]) {
                    changed.push((id, rel.clone()));
                }
            }
        }
    }
    let removed: Vec<(u32, PathBuf)> = old
        .files
        .iter()
        .enumerate()
        .filter(|(id, p)| !old.removed_ids.contains(*id as u32) && !current_set.contains(p.as_path()))
        .map(|(i, p)| (i as u32, p.clone()))
        .collect();

    let unchanged = current.len().saturating_sub(added.len() + changed.len());
    println!(
        "  {} added, {} changed, {} removed, {} unchanged",
        added.len(),
        changed.len(),
        removed.len(),
        unchanged
    );

    if added.is_empty() && changed.is_empty() && removed.is_empty() {
        println!("Index is already up to date.");
        return Ok(old);
    }

    // Surgical work is O(trigrams × affected files). It is always exact, so for
    // small indexes we just do it; only on a large index with a large change set
    // does a full rebuild become cheaper, and we fall back then.
    let live = old.files.len() - old.removed_ids.len() as usize;
    let surgical_ok = live <= 1000 || (changed.len() + removed.len()) * 4 <= live;

    if surgical_ok {
        for (id, _rel) in &removed {
            remove_file(&mut old, *id);
        }
        for (id, rel) in &changed {
            let abs = base.join(rel);
            if let Some(new_meta) = patch_changed(&mut old, *id, &abs) {
                old.metas[*id as usize] = new_meta;
            }
        }
        for rel in &added {
            add_file(&mut old, base, rel);
        }
        save(&old, base)?;
        return Ok(old);
    }

    eprintln!("Large change set - performing full rebuild...");
    let index = build(base, opts);
    save(&index, base)?;
    Ok(index)
}

/// The trigrams currently indexed for `id`, reconstructed from the posting
/// bitmaps. This is exactly the file's indexed folded-trigram set.
fn trigrams_of(index: &Index, id: u32) -> Vec<Trigram> {
    index.postings.iter().filter(|(_, bm)| bm.contains(id)).map(|(t, _)| *t).collect()
}

/// A file's distinct folded trigram set (empty for binary or unreadable files,
/// matching build-time behaviour).
fn read_trigram_set(abs: &Path) -> FxHashSet<Trigram> {
    let Ok(bytes) = fs::read(abs) else {
        return FxHashSet::default();
    };
    if scanner::looks_binary(&bytes[..bytes.len().min(512)]) {
        return FxHashSet::default();
    }
    trigram::extract_distinct_folded(&bytes)
}

/// Remove a file's contribution from every posting and mark its id dead.
fn remove_file(index: &mut Index, id: u32) {
    for t in trigrams_of(index, id) {
        if let Some(bm) = index.postings.get_mut(&t) {
            bm.remove(id);
        }
    }
    index.removed_ids.insert(id);
}

/// Patch a changed file: remove trigrams it no longer has, add new ones. Keeps
/// its id stable. Returns refreshed metadata if the file is readable.
fn patch_changed(index: &mut Index, id: u32, abs: &Path) -> Option<FileMeta> {
    let old_set: FxHashSet<Trigram> = trigrams_of(index, id).into_iter().collect();
    let new_set = read_trigram_set(abs);
    for t in old_set.iter().filter(|t| !new_set.contains(t)) {
        if let Some(bm) = index.postings.get_mut(t) {
            bm.remove(id);
        }
    }
    for t in new_set.iter().filter(|t| !old_set.contains(t)) {
        index.postings.entry(*t).or_default().insert(id);
    }
    read_meta(abs)
}

/// Index a brand-new file, reusing a dead id if one is available.
fn add_file(index: &mut Index, base: &Path, rel: &Path) {
    let id = if let Some(dead) = index.removed_ids.iter().next() {
        index.removed_ids.remove(dead);
        index.files[dead as usize] = rel.to_path_buf();
        dead
    } else {
        let id = index.files.len() as u32;
        index.files.push(rel.to_path_buf());
        id
    };
    while index.metas.len() <= id as usize {
        index.metas.push(FileMeta::default());
    }
    let abs = base.join(rel);
    index.metas[id as usize] = read_meta(&abs).unwrap_or_default();
    for t in read_trigram_set(&abs) {
        index.postings.entry(t).or_default().insert(id);
    }
}

fn changed_since(base: &Path, rel: &Path, old: &FileMeta) -> bool {
    let Ok(meta) = fs::metadata(base.join(rel)) else {
        return true;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    mtime != old.mtime_secs || meta.len() != old.size
}

/// Read `abs`, index its (case-folded) trigrams into `map` under file `id`, and
/// return its metadata. Binary files are recorded but not indexed.
fn index_file(abs: &Path, id: u32, map: &mut FxHashMap<Trigram, RoaringBitmap>) -> Option<FileMeta> {
    let meta = read_meta(abs)?;
    let bytes = fs::read(abs).ok()?;
    let sample = &bytes[..bytes.len().min(512)];
    if !scanner::looks_binary(sample) {
        for trigram in trigram::extract_distinct_folded(&bytes) {
            map.entry(trigram).or_default().insert(id);
        }
    }
    Some(meta)
}

fn read_meta(path: &Path) -> Option<FileMeta> {
    let meta = fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(FileMeta {
        mtime_secs,
        size: meta.len(),
    })
}

pub fn save(index: &Index, base: &Path) -> io::Result<()> {
    let dir = base.join(INDEX_DIR);
    fs::create_dir_all(&dir)?;
    let file = File::create(dir.join(INDEX_FILE))?;
    let mut w = BufWriter::new(file);

    w.write_all(MAGIC)?;
    w.write_all(&[VERSION])?;
    write_u32(&mut w, index.files.len() as u32)?;
    write_u32(&mut w, index.postings.len() as u32)?;
    write_u64(&mut w, index.build_time_us.min(u64::MAX as u128) as u64)?;

    for (id, rel) in index.files.iter().enumerate() {
        write_bytes(&mut w, rel.to_string_lossy().as_bytes())?;
        let meta = index.metas.get(id).copied().unwrap_or_default();
        write_i64(&mut w, meta.mtime_secs)?;
        write_u64(&mut w, meta.size)?;
    }

    let mut keys: Vec<u32> = index.postings.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        write_u32(&mut w, key)?;
        // Roaring's portable format is self-delimiting, so no length prefix is
        // needed: the reader consumes exactly as many bytes as the writer emits.
        index.postings[&key].serialize_into(&mut w)?;
    }

    index.removed_ids.serialize_into(&mut w)?;

    w.flush()
}

pub fn load(base: &Path) -> io::Result<Option<Index>> {
    let path = base.join(INDEX_DIR).join(INDEX_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let data = fs::read(path)?;
    if data.len() < 8 || &data[0..7] != MAGIC || data[7] != VERSION {
        // Unknown or stale format: rebuild from scratch.
        return Ok(None);
    }

    let mut rdr = Cursor::new(data);
    {
        let mut magic = [0u8; 8];
        rdr.read_exact(&mut magic)?;
    }
    let file_count = read_u32(&mut rdr)? as usize;
    let trigram_count = read_u32(&mut rdr)? as usize;
    let build_time_us = read_u64(&mut rdr)? as u128;

    let mut files = Vec::with_capacity(file_count);
    let mut metas = Vec::with_capacity(file_count);
    for _ in 0..file_count {
        let rel = read_bytes(&mut rdr)?;
        let mtime_secs = read_i64(&mut rdr)?;
        let size = read_u64(&mut rdr)?;
        files.push(PathBuf::from(String::from_utf8_lossy(&rel).into_owned()));
        metas.push(FileMeta { mtime_secs, size });
    }

    let mut postings = FxHashMap::with_capacity_and_hasher(trigram_count, Default::default());
    for _ in 0..trigram_count {
        let key = read_u32(&mut rdr)?;
        let bitmap = RoaringBitmap::deserialize_from(&mut rdr)?;
        postings.insert(key, bitmap);
    }

    // removed_ids is always written in v3; tolerate an older/truncated tail by
    // defaulting to an empty set.
    let removed_ids = RoaringBitmap::deserialize_from(&mut rdr).unwrap_or_default();

    Ok(Some(Index {
        files,
        metas,
        postings,
        removed_ids,
        build_time_us,
    }))
}

impl Index {
    /// Resolve a candidate id set to concrete paths. `None` means all live files.
    pub fn resolve_files(&self, ids: Option<&RoaringBitmap>) -> Vec<PathBuf> {
        match ids {
            None => self
                .files
                .iter()
                .enumerate()
                .filter(|(id, _)| !self.removed_ids.contains(*id as u32))
                .map(|(_, p)| p.clone())
                .collect(),
            Some(bm) => bm
                .iter()
                .filter(|id| !self.removed_ids.contains(*id))
                .filter_map(|id| self.files.get(id as usize).cloned())
                .collect(),
        }
    }

    pub fn print_stats(&self, base: &Path) {
        let index_bytes = base
            .join(INDEX_DIR)
            .join(INDEX_FILE)
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        let total_postings: u64 = self.postings.values().map(|b| b.len()).sum();
        let live = self.files.len() - self.removed_ids.len() as usize;
        println!("Index Statistics:");
        println!("  Files indexed:    {}", live);
        println!("  Unique trigrams:  {}", self.postings.len());
        println!("  Posting entries:   {}", total_postings);
        println!("  Index on disk:    {}", human_bytes(index_bytes));
        println!("  Build time:       {}", format_time(self.build_time_us));
    }
}

fn human_bytes(n: u64) -> String {
    let f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", f / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", f / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", f / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_time(us: u128) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}

fn write_bytes(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    write_u32(w, bytes.len() as u32)?;
    w.write_all(bytes)
}

fn read_bytes(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let len = read_u32(r)? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_u32(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn write_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn write_i64(w: &mut impl Write, v: i64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn read_i64(r: &mut impl Read) -> io::Result<i64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(i64::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigram::encode;

    #[test]
    fn build_indexes_folded_trigrams() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "Hello World\n").unwrap();

        let index = build(dir.path(), &WalkOptions::default());
        assert!(index.postings.contains_key(&encode(b'H', b'e', b'l')));
        assert!(index.postings.contains_key(&encode(b'h', b'e', b'l'))); // folded
        assert_eq!(index.files, vec![PathBuf::from("a.txt")]);
    }

    #[test]
    fn save_load_roundtrip_preserves_postings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.txt"), "alpha beta gamma\n").unwrap();
        std::fs::write(dir.path().join("y.txt"), "delta beta gamma\n").unwrap();

        let index = build(dir.path(), &WalkOptions::default());
        save(&index, dir.path()).unwrap();
        let loaded = load(dir.path()).unwrap().unwrap();

        assert_eq!(loaded.files, index.files);
        assert_eq!(loaded.postings.len(), index.postings.len());
        for key in index.postings.keys() {
            assert_eq!(loaded.postings[key], index.postings[key], "trigram {key}");
        }
    }

    #[test]
    fn update_appends_new_files_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one two three\n").unwrap();
        let index = build(dir.path(), &WalkOptions::default());
        save(&index, dir.path()).unwrap();
        let before = load(dir.path()).unwrap().unwrap();

        std::fs::write(dir.path().join("b.txt"), "four five six\n").unwrap();
        let after = update(dir.path(), &WalkOptions::default()).unwrap();

        assert!(after.files.contains(&PathBuf::from("b.txt")));
        // Existing file kept its id.
        let a_id = before.files.iter().position(|p| p == "a.txt").unwrap();
        assert_eq!(after.files[a_id], PathBuf::from("a.txt"));
    }

    #[test]
    fn update_surgically_patches_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha beta gamma\n").unwrap();
        let index = build(dir.path(), &WalkOptions::default());
        save(&index, dir.path()).unwrap();
        let a_id = load(dir.path()).unwrap().unwrap().files.iter().position(|p| p == "a.txt").unwrap() as u32;

        // Rewrite the file so "alpha"/"beta" disappear and "delta"/"epsilon" appear.
        std::fs::write(dir.path().join("a.txt"), "delta epsilon gamma\n").unwrap();
        let after = update(dir.path(), &WalkOptions::default()).unwrap();

        let beta = encode(b'b', b'e', b't');
        let delt = encode(b'd', b'e', b'l');
        // Old trigram removed for this file ...
        assert!(
            !after.postings.get(&beta).is_some_and(|bm| bm.contains(a_id)),
            "stale trigram 'bet' still indexed for a.txt"
        );
        // ... new trigram added, same id, no new file slot.
        assert!(after.postings.get(&delt).is_some_and(|bm| bm.contains(a_id)));
        assert_eq!(after.files.len(), 1, "changed file should not add a slot");
    }

    #[test]
    fn update_surgically_removes_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "shared content\n").unwrap();
        std::fs::write(dir.path().join("gone.txt"), "unique to gone\n").unwrap();
        let index = build(dir.path(), &WalkOptions::default());
        save(&index, dir.path()).unwrap();
        let gone_id = load(dir.path())
            .unwrap()
            .unwrap()
            .files
            .iter()
            .position(|p| p == "gone.txt")
            .unwrap() as u32;

        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let after = update(dir.path(), &WalkOptions::default()).unwrap();

        // gone's id is marked removed and carries no postings.
        assert!(after.removed_ids.contains(gone_id));
        let uni = encode(b'u', b'n', b'i');
        assert!(
            after.postings.get(&uni).is_none_or(|bm| !bm.contains(gone_id)),
            "removed file still referenced in postings"
        );
        // Resolving all files excludes the removed one.
        assert!(!after.resolve_files(None).iter().any(|p| p == "gone.txt"));
    }
}
