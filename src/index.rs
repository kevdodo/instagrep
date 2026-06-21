use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Instant;

use crate::scanner;
use crate::trigram::{self, Trigram};

const INDEX_DIR: &str = ".instantgrep";
const INDEX_FILE: &str = "index.bin";
const MAGIC: &[u8; 8] = b"IGRUST\0\x01";

#[derive(Debug, Clone)]
pub struct Index {
    pub files: Vec<PathBuf>,
    pub postings: HashMap<Trigram, Vec<Posting>>,
    pub metas: HashMap<PathBuf, FileMeta>,
    pub build_time_us: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Posting {
    pub file_id: u32,
    pub next_mask: u8,
    pub loc_mask: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub file_id: u32,
    pub mtime_secs: i64,
    pub size: u64,
}

pub fn build(path: &Path) -> Index {
    let workers = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    build_with_worker_count(path, workers)
}

fn build_with_worker_count(path: &Path, worker_count: usize) -> Index {
    let started = Instant::now();
    let files = scanner::scan(path);
    let worker_count = worker_count.max(1).min(files.len().max(1));
    let chunk_size = files.len().div_ceil(worker_count).max(1);

    let worker_results = thread::scope(|scope| {
        files
            .chunks(chunk_size)
            .enumerate()
            .map(|(chunk_index, chunk)| {
                let base_id = (chunk_index * chunk_size) as u32;
                scope.spawn(move || index_file_chunk(chunk, base_id))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });

    let mut postings = HashMap::new();
    let mut metas = HashMap::new();
    for worker_result in worker_results {
        for (path, meta) in worker_result.metas {
            metas.insert(path, meta);
        }
        merge_postings(&mut postings, worker_result.postings);
    }

    Index {
        files,
        postings,
        metas,
        build_time_us: started.elapsed().as_micros(),
    }
}

#[derive(Debug, Default)]
struct WorkerIndex {
    postings: HashMap<Trigram, Vec<Posting>>,
    metas: Vec<(PathBuf, FileMeta)>,
}

pub fn update(base_dir: &Path) -> io::Result<Index> {
    let old = match load(base_dir)? {
        Some(index) => index,
        None => {
            eprintln!("No existing index found - performing full build...");
            let index = build(base_dir);
            save(&index, base_dir)?;
            return Ok(index);
        }
    };

    let current: HashSet<PathBuf> = scanner::scan(base_dir).into_iter().collect();
    let old_paths: HashSet<PathBuf> = old.metas.keys().cloned().collect();

    let added: HashSet<_> = current.difference(&old_paths).cloned().collect();
    let removed: HashSet<_> = old_paths.difference(&current).cloned().collect();
    let changed: HashSet<_> = current
        .intersection(&old_paths)
        .filter(|path| changed_since_index(path, &old.metas))
        .cloned()
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

    let index = build(base_dir);
    save(&index, base_dir)?;
    Ok(index)
}

pub fn save(index: &Index, base_dir: &Path) -> io::Result<()> {
    let dir = base_dir.join(INDEX_DIR);
    fs::create_dir_all(&dir)?;
    let file = File::create(dir.join(INDEX_FILE))?;
    let mut writer = BufWriter::new(file);

    writer.write_all(MAGIC)?;
    write_u64(&mut writer, index.files.len() as u64)?;
    write_u64(&mut writer, index.postings.len() as u64)?;
    write_u64(
        &mut writer,
        index.build_time_us.min(u64::MAX as u128) as u64,
    )?;

    for (id, path) in index.files.iter().enumerate() {
        let bytes = path.to_string_lossy();
        write_bytes(&mut writer, bytes.as_bytes())?;
        let meta = index.metas.get(path).copied().unwrap_or(FileMeta {
            file_id: id as u32,
            mtime_secs: 0,
            size: 0,
        });
        write_i64(&mut writer, meta.mtime_secs)?;
        write_u64(&mut writer, meta.size)?;
    }

    let mut keys: Vec<_> = index.postings.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        write_u32(&mut writer, key)?;
        let mut rows = index.postings[&key].clone();
        rows.sort_by_key(|row| row.file_id);
        write_u64(&mut writer, rows.len() as u64)?;
        for row in rows {
            write_u32(&mut writer, row.file_id)?;
            writer.write_all(&[row.next_mask, row.loc_mask])?;
        }
    }

    writer.flush()
}

pub fn load(base_dir: &Path) -> io::Result<Option<Index>> {
    let path = base_dir.join(INDEX_DIR).join(INDEX_FILE);
    if !path.is_file() {
        return Ok(None);
    }

    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Ok(None);
    }

    let file_count = read_u64(&mut reader)? as usize;
    let posting_key_count = read_u64(&mut reader)? as usize;
    let build_time_us = read_u64(&mut reader)? as u128;

    let mut files = Vec::with_capacity(file_count);
    let mut metas = HashMap::with_capacity(file_count);
    for id in 0..file_count {
        let path = PathBuf::from(String::from_utf8_lossy(&read_bytes(&mut reader)?).into_owned());
        let meta = FileMeta {
            file_id: id as u32,
            mtime_secs: read_i64(&mut reader)?,
            size: read_u64(&mut reader)?,
        };
        metas.insert(path.clone(), meta);
        files.push(path);
    }

    let mut postings = HashMap::with_capacity(posting_key_count);
    for _ in 0..posting_key_count {
        let key = read_u32(&mut reader)?;
        let len = read_u64(&mut reader)? as usize;
        let mut rows = Vec::with_capacity(len);
        for _ in 0..len {
            let file_id = read_u32(&mut reader)?;
            let mut masks = [0u8; 2];
            reader.read_exact(&mut masks)?;
            rows.push(Posting {
                file_id,
                next_mask: masks[0],
                loc_mask: masks[1],
            });
        }
        postings.insert(key, rows);
    }

    Ok(Some(Index {
        files,
        postings,
        metas,
        build_time_us,
    }))
}

impl Index {
    pub fn lookup_with_masks(&self, trigram: Trigram) -> HashMap<u32, (u8, u8)> {
        self.postings
            .get(&trigram)
            .into_iter()
            .flatten()
            .map(|row| (row.file_id, (row.next_mask, row.loc_mask)))
            .collect()
    }

    pub fn resolve_files(&self, ids: Option<&HashSet<u32>>) -> Vec<PathBuf> {
        let mut files = match ids {
            None => self.files.clone(),
            Some(ids) => ids
                .iter()
                .filter_map(|&id| self.files.get(id as usize).cloned())
                .collect(),
        };
        files.sort();
        files
    }

    pub fn print_stats(&self) {
        println!("Index Statistics:");
        println!("  Files indexed:   {}", self.files.len());
        println!("  Unique trigrams: {}", self.postings.len());
        println!("  Build time:      {}", format_time(self.build_time_us));
    }
}

fn index_file_chunk(files: &[PathBuf], base_id: u32) -> WorkerIndex {
    let mut worker = WorkerIndex::default();
    for (offset, file) in files.iter().enumerate() {
        let file_id = base_id + offset as u32;
        if let Some(meta) = read_meta(file, file_id) {
            worker.metas.push((file.clone(), meta));
        }
        index_file(file, file_id, &mut worker.postings);
    }
    worker
}

fn merge_postings(
    target: &mut HashMap<Trigram, Vec<Posting>>,
    source: HashMap<Trigram, Vec<Posting>>,
) {
    for (trigram, mut rows) in source {
        target.entry(trigram).or_default().append(&mut rows);
    }
}

fn index_file(file: &Path, file_id: u32, postings: &mut HashMap<Trigram, Vec<Posting>>) {
    let Ok(bytes) = fs::read(file) else {
        return;
    };
    let sample_len = bytes.len().min(512);
    if scanner::looks_binary(&bytes[..sample_len]) {
        return;
    }

    let mut file_trigrams = trigram::extract_with_masks(&bytes);
    let lower = bytes.to_ascii_lowercase();
    if lower != bytes {
        for (key, (next_mask, loc_mask)) in trigram::extract_with_masks(&lower) {
            file_trigrams
                .entry(key)
                .and_modify(|(next, loc)| {
                    *next |= next_mask;
                    *loc |= loc_mask;
                })
                .or_insert((next_mask, loc_mask));
        }
    }

    for (trigram, (next_mask, loc_mask)) in file_trigrams {
        postings.entry(trigram).or_default().push(Posting {
            file_id,
            next_mask,
            loc_mask,
        });
    }
}

fn read_meta(path: &Path, file_id: u32) -> Option<FileMeta> {
    let meta = fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    Some(FileMeta {
        file_id,
        mtime_secs,
        size: meta.len(),
    })
}

fn changed_since_index(path: &Path, metas: &HashMap<PathBuf, FileMeta>) -> bool {
    let Some(old) = metas.get(path) else {
        return true;
    };
    read_meta(path, old.file_id)
        .map(|new| new.mtime_secs != old.mtime_secs || new.size != old.size)
        .unwrap_or(true)
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

fn write_bytes(mut writer: impl Write, bytes: &[u8]) -> io::Result<()> {
    write_u64(&mut writer, bytes.len() as u64)?;
    writer.write_all(bytes)
}

fn read_bytes(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let len = read_u64(&mut reader)? as usize;
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn write_u32(mut writer: impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u32(mut reader: impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_u64(mut writer: impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u64(mut reader: impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_i64(mut writer: impl Write, value: i64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_i64(mut reader: impl Read) -> io::Result<i64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(i64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::trigram::encode;

    #[test]
    fn parallel_build_matches_single_worker_build() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("alpha.txt"),
            "hello world\nTODO: ship the thing\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("beta.txt"),
            "another hello\nworld of code\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(
            dir.path().join("nested/gamma.txt"),
            "grep fast grep faster\n",
        )
        .unwrap();

        let single = build_with_worker_count(dir.path(), 1);
        let parallel = build_with_worker_count(dir.path(), 4);

        assert_eq!(parallel.files, single.files);
        assert_eq!(parallel.metas.len(), single.metas.len());
        assert_postings_eq(&parallel.postings, &single.postings);
    }

    #[test]
    fn parallel_build_preserves_binary_skip_behavior() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("text.txt"), "abc searchable text\n").unwrap();
        fs::write(dir.path().join("binary.dat"), b"abc\0def searchable").unwrap();

        let index = build_with_worker_count(dir.path(), 4);
        let text_id = index
            .files
            .iter()
            .position(|path| path.file_name().unwrap() == "text.txt")
            .unwrap() as u32;
        let abc_files: Vec<_> = index
            .postings
            .get(&encode(b"abc"))
            .into_iter()
            .flatten()
            .map(|posting| posting.file_id)
            .collect();

        assert_eq!(index.files.len(), 2);
        assert_eq!(abc_files, vec![text_id]);
    }

    fn assert_postings_eq(
        left: &HashMap<Trigram, Vec<Posting>>,
        right: &HashMap<Trigram, Vec<Posting>>,
    ) {
        assert_eq!(left.len(), right.len());
        for (trigram, left_rows) in left {
            let mut left_rows = left_rows.clone();
            let mut right_rows = right.get(trigram).cloned().unwrap_or_default();
            left_rows.sort_by_key(|row| (row.file_id, row.next_mask, row.loc_mask));
            right_rows.sort_by_key(|row| (row.file_id, row.next_mask, row.loc_mask));
            assert_eq!(left_rows, right_rows, "trigram {trigram}");
        }
    }
}
