//! A set of 128-bit keys that lives on disk.
//!
//! Per set `<name>` the index directory holds:
//! - `<name>.sorted`: every merged key, 16 bytes each, ascending. It is mapped
//!   read-only and binary searched, so what it costs in memory is file cache
//!   the kernel may drop, not anonymous memory.
//! - `<name>.tail`: keys added since the last merge, appended in arrival
//!   order and mirrored in a hash set. At `MERGE_AT` keys it is merged into a
//!   new sorted file, which replaces the old one by rename.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use memmap2::{Advice, Mmap};

use crate::error::ignore_not_found;

pub type Key = [u8; 16];

pub const MERGE_AT: usize = 65_536;

/// The key of a uuid. The protocol treats a uuid as an opaque string, so the
/// key is a hash of its exact text: dedupe stays exact-string like the Python
/// server's set, whatever the text looks like. At 128 bits a collision among
/// millions of uuids is out of reach (about 1e-25).
pub fn key_of(text: &str) -> Key {
    xxhash_rust::xxh3::xxh3_128(text.as_bytes()).to_be_bytes()
}

pub struct KeySet {
    dir: PathBuf,
    sorted_path: PathBuf,
    sorted: Option<Mmap>,
    tail: HashSet<Key>,
    tail_file: File,
    tail_bytes: u64,
    merge_at: usize,
}

impl KeySet {
    /// An empty set, replacing whatever the files held.
    pub fn create_empty(dir: &Path, name: &str) -> io::Result<KeySet> {
        let (sorted_path, tail_path) = paths(dir, name);
        ignore_not_found(fs::remove_file(partial_path(&sorted_path)))?;
        File::create(&sorted_path)?.sync_all()?;
        File::create(&tail_path)?.sync_all()?;
        sync_dir(dir)?;
        KeySet::open_with(dir, name, MERGE_AT)
    }

    pub fn open(dir: &Path, name: &str) -> io::Result<KeySet> {
        KeySet::open_with(dir, name, MERGE_AT)
    }

    fn open_with(dir: &Path, name: &str, merge_at: usize) -> io::Result<KeySet> {
        let (sorted_path, tail_path) = paths(dir, name);
        // A merge that stopped before its rename leaves a half-written file.
        ignore_not_found(fs::remove_file(partial_path(&sorted_path)))?;
        let sorted = map_sorted(&sorted_path)?;

        let mut tail_file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&tail_path)?;
        let mut bytes = Vec::new();
        tail_file.read_to_end(&mut bytes)?;
        let whole = bytes.len() / 16 * 16;
        if whole != bytes.len() {
            // A write cut short by a crash; its key was never acknowledged.
            tail_file.set_len(whole as u64)?;
            tail_file.sync_data()?;
        }
        let tail = bytes[..whole].as_chunks::<16>().0.iter().copied().collect();

        Ok(KeySet {
            dir: dir.to_owned(),
            sorted_path,
            sorted,
            tail,
            tail_file,
            tail_bytes: whole as u64,
            merge_at,
        })
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.tail.contains(key)
            || self
                .sorted
                .as_deref()
                .is_some_and(|bytes| keys(bytes).binary_search(key).is_ok())
    }

    /// Adds keys the caller has checked are absent and distinct. They are on
    /// disk when this returns.
    pub fn add_new(&mut self, new: &[Key]) -> io::Result<()> {
        if new.is_empty() {
            return Ok(());
        }
        let bytes = new.concat();
        if let Err(error) = self
            .tail_file
            .write_all(&bytes)
            .and_then(|()| self.tail_file.sync_data())
        {
            // Keep the tail a whole number of records for the next append.
            let _ = self.tail_file.set_len(self.tail_bytes);
            return Err(error);
        }
        self.tail_bytes += bytes.len() as u64;
        self.tail.extend(new.iter().copied());
        if self.tail.len() >= self.merge_at {
            self.merge()?;
        }
        Ok(())
    }

    /// Adds the keys that are not in the set yet. `candidates` may repeat keys.
    pub fn add(&mut self, mut candidates: Vec<Key>) -> io::Result<()> {
        if candidates.len() >= self.merge_at {
            // Sort in place and merge: a hash set of this many keys would cost tens of MiB.
            candidates.sort_unstable();
            candidates.dedup();
            return self.absorb_sorted(&candidates);
        }
        let mut seen = HashSet::new();
        let fresh: Vec<Key> = candidates
            .iter()
            .filter(|key| !self.contains(key) && seen.insert(**key))
            .copied()
            .collect();
        self.add_new(&fresh)
    }

    fn merge(&mut self) -> io::Result<()> {
        let mut keys: Vec<Key> = self.tail.iter().copied().collect();
        keys.sort_unstable();
        self.absorb_sorted(&keys)?;
        // Keys left in the tail after a crash here are in the sorted file too, which is harmless.
        self.tail_file.set_len(0)?;
        self.tail_file.sync_data()?;
        self.tail_bytes = 0;
        // Not `clear()`: that keeps the capacity a big add grew the table to.
        self.tail = HashSet::new();
        Ok(())
    }

    /// Merges ascending, distinct keys into the sorted file by writing a new
    /// file and renaming it into place.
    pub fn absorb_sorted(&mut self, add: &[Key]) -> io::Result<()> {
        let partial = partial_path(&self.sorted_path);
        let old: &[Key] = self.sorted.as_deref().map_or(&[], keys);
        if let Some(map) = &self.sorted {
            let _ = map.advise(Advice::Sequential);
        }

        let mut out = BufWriter::with_capacity(1 << 16, File::create(&partial)?);
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < add.len() {
            match old[i].cmp(&add[j]) {
                Ordering::Less => {
                    out.write_all(&old[i])?;
                    i += 1;
                }
                Ordering::Greater => {
                    out.write_all(&add[j])?;
                    j += 1;
                }
                Ordering::Equal => {
                    out.write_all(&old[i])?;
                    i += 1;
                    j += 1;
                }
            }
        }
        for key in &old[i..] {
            out.write_all(key)?;
        }
        for key in &add[j..] {
            out.write_all(key)?;
        }
        let file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        drop(file);

        fs::rename(&partial, &self.sorted_path)?;
        sync_dir(&self.dir)?;
        self.sorted = map_sorted(&self.sorted_path)?;
        Ok(())
    }

    /// Number of keys in the sorted file and the tail; a key present in both
    /// counts twice.
    pub fn len(&self) -> usize {
        self.sorted.as_deref().map_or(0, |bytes| bytes.len() / 16) + self.tail.len()
    }
}

fn keys(bytes: &[u8]) -> &[Key] {
    bytes.as_chunks::<16>().0
}

fn paths(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{name}.sorted")),
        dir.join(format!("{name}.tail")),
    )
}

fn partial_path(sorted_path: &Path) -> PathBuf {
    sorted_path.with_extension("sorted.new")
}

fn map_sorted(path: &Path) -> io::Result<Option<Mmap>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len % 16 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {len} bytes, which is not a whole number of 16-byte keys",
                path.display()
            ),
        ));
    }
    if len == 0 {
        return Ok(None);
    }
    // SAFETY: this process holds the index directory's lock, and sorted files
    // are only ever replaced by rename, never changed or truncated in place.
    let map = unsafe { Mmap::map(&file)? };
    // Lookups touch scattered pages; read-ahead would only fill the cache.
    let _ = map.advise(Advice::Random);
    Ok(Some(map))
}

/// Makes a rename or file creation in `dir` durable.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> Key {
        key_of(&format!("uuid-{n}"))
    }

    fn sorted_file(dir: &Path, name: &str) -> Vec<Key> {
        keys(&fs::read(dir.join(format!("{name}.sorted"))).unwrap()).to_vec()
    }

    #[test]
    fn keys_are_a_stable_hash_of_the_exact_text() {
        // XXH3-128 of the empty input, seed 0, from the xxHash reference.
        assert_eq!(
            key_of(""),
            0x99aa06d3014798d86001c324468d497f_u128.to_be_bytes()
        );
        assert_eq!(key_of("A"), key_of("A"));
        assert_ne!(key_of("A"), key_of("a"));
        assert_ne!(key_of("A"), key_of("A "));
    }

    #[test]
    fn a_new_set_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let set = KeySet::create_empty(dir.path(), "seen").unwrap();
        assert_eq!(set.len(), 0);
        assert!(!set.contains(&key(1)));
    }

    #[test]
    fn added_keys_are_found_and_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        set.add_new(&[key(1), key(2)]).unwrap();
        assert!(set.contains(&key(1)) && set.contains(&key(2)) && !set.contains(&key(3)));
        drop(set);

        let set = KeySet::open(dir.path(), "seen").unwrap();
        assert!(set.contains(&key(1)) && set.contains(&key(2)) && !set.contains(&key(3)));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn the_tail_merges_into_the_sorted_file_at_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        KeySet::create_empty(dir.path(), "seen").unwrap();
        let mut set = KeySet::open_with(dir.path(), "seen", 4).unwrap();

        set.add_new(&[key(1), key(2), key(3)]).unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("seen.tail")).unwrap().len(),
            48
        );
        assert!(sorted_file(dir.path(), "seen").is_empty());

        set.add_new(&[key(4)]).unwrap();
        assert_eq!(fs::metadata(dir.path().join("seen.tail")).unwrap().len(), 0);
        let merged = sorted_file(dir.path(), "seen");
        assert_eq!(merged.len(), 4);
        assert!(merged.windows(2).all(|pair| pair[0] < pair[1]));
        for n in 1..=4 {
            assert!(set.contains(&key(n)));
        }

        set.add_new(&[key(5), key(6), key(7), key(8)]).unwrap();
        assert_eq!(sorted_file(dir.path(), "seen").len(), 8);
        drop(set);
        let set = KeySet::open_with(dir.path(), "seen", 4).unwrap();
        assert!((1..=8).all(|n| set.contains(&key(n))) && !set.contains(&key(9)));
    }

    #[test]
    fn a_merge_gives_back_the_room_the_tail_grew_to() {
        let dir = tempfile::tempdir().unwrap();
        KeySet::create_empty(dir.path(), "seen").unwrap();
        let mut set = KeySet::open_with(dir.path(), "seen", 4).unwrap();
        let many: Vec<Key> = (0..1_000).map(key).collect();

        set.add_new(&many).unwrap();
        assert!(set.tail.is_empty());
        assert!(
            set.tail.capacity() < many.len(),
            "the tail kept room for {} keys",
            set.tail.capacity()
        );
        assert!(many.iter().all(|key| set.contains(key)));
    }

    #[test]
    fn add_skips_keys_that_are_already_there_and_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        set.add(vec![key(1), key(1), key(2)]).unwrap();
        set.add(vec![key(2), key(3), key(3)]).unwrap();
        assert_eq!(set.len(), 3);
        assert_eq!(
            fs::metadata(dir.path().join("seen.tail")).unwrap().len(),
            48
        );
    }

    #[test]
    fn a_bulk_add_goes_straight_into_the_sorted_file() {
        let dir = tempfile::tempdir().unwrap();
        KeySet::create_empty(dir.path(), "seen").unwrap();
        let mut set = KeySet::open_with(dir.path(), "seen", 4).unwrap();
        set.add_new(&[key(1)]).unwrap();

        // Repeats, and key 1, which the tail holds already.
        set.add([1, 2, 3, 3, 4, 5, 6, 7, 8, 8].map(key).to_vec())
            .unwrap();

        let merged = sorted_file(dir.path(), "seen");
        assert_eq!(merged.len(), 8);
        assert!(merged.windows(2).all(|pair| pair[0] < pair[1]));
        assert!((1..=8).all(|n| set.contains(&key(n))) && !set.contains(&key(9)));
        assert_eq!(set.tail.len(), 1, "the candidates must not enter the tail");
        assert_eq!(
            fs::metadata(dir.path().join("seen.tail")).unwrap().len(),
            16
        );
        drop(set);

        let set = KeySet::open_with(dir.path(), "seen", 4).unwrap();
        assert!((1..=8).all(|n| set.contains(&key(n))) && !set.contains(&key(9)));
    }

    #[test]
    fn a_torn_tail_record_is_dropped_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        set.add_new(&[key(1)]).unwrap();
        drop(set);
        OpenOptions::new()
            .append(true)
            .open(dir.path().join("seen.tail"))
            .unwrap()
            .write_all(&[7; 5])
            .unwrap();

        let mut set = KeySet::open(dir.path(), "seen").unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("seen.tail")).unwrap().len(),
            16
        );
        assert!(set.contains(&key(1)));
        set.add_new(&[key(2)]).unwrap();
        drop(set);
        let set = KeySet::open(dir.path(), "seen").unwrap();
        assert!(set.contains(&key(1)) && set.contains(&key(2)));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn a_sorted_file_of_the_wrong_length_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        KeySet::create_empty(dir.path(), "seen").unwrap();
        fs::write(dir.path().join("seen.sorted"), [0u8; 17]).unwrap();
        let error = KeySet::open(dir.path(), "seen").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_missing_sorted_file_is_an_error_not_an_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let error = KeySet::open(dir.path(), "seen").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_half_written_merge_is_discarded_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        set.add_new(&[key(1)]).unwrap();
        drop(set);
        fs::write(dir.path().join("seen.sorted.new"), [1u8; 40]).unwrap();

        let set = KeySet::open(dir.path(), "seen").unwrap();
        assert!(!dir.path().join("seen.sorted.new").exists());
        assert!(set.contains(&key(1)));
    }

    #[test]
    fn absorbing_overlapping_runs_keeps_one_copy_of_each_key_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        let mut first: Vec<Key> = (0..100).map(key).collect();
        first.sort_unstable();
        set.absorb_sorted(&first).unwrap();
        let mut second: Vec<Key> = (50..150).map(key).collect();
        second.sort_unstable();
        set.absorb_sorted(&second).unwrap();

        let merged = sorted_file(dir.path(), "seen");
        assert_eq!(merged.len(), 150);
        assert!(merged.windows(2).all(|pair| pair[0] < pair[1]));
        assert!((0..150).all(|n| set.contains(&key(n))));
    }

    #[test]
    fn lookups_are_exact_in_a_large_sorted_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        let mut present: Vec<Key> = (0..20_000).map(key).collect();
        present.sort_unstable();
        set.absorb_sorted(&present).unwrap();
        assert!((0..20_000).all(|n| set.contains(&key(n))));
        assert!((20_000..40_000).all(|n| !set.contains(&key(n))));
    }

    #[test]
    fn a_key_whose_write_failed_is_not_remembered() {
        let Ok(full) = OpenOptions::new().write(true).open("/dev/full") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut set = KeySet::create_empty(dir.path(), "seen").unwrap();
        set.add_new(&[key(1)]).unwrap();

        let real = std::mem::replace(&mut set.tail_file, full);
        assert!(set.add_new(&[key(2)]).is_err());
        assert!(!set.contains(&key(2)));
        set.tail_file = real;

        set.add_new(&[key(3)]).unwrap();
        drop(set);
        let set = KeySet::open(dir.path(), "seen").unwrap();
        assert!(set.contains(&key(1)) && !set.contains(&key(2)) && set.contains(&key(3)));
        assert_eq!(set.len(), 2);
    }
}
