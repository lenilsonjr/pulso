//! The NDJSON files, the uuid index beside them, and the newest `end` per type.
//!
//! The files are the record. The index holds one key per stored uuid so that
//! dedupe never reads them, and `state.json` in the index directory says how
//! many bytes of each file the index covers and what `/latest` answers.
//!
//! An ingest writes in this order: data files (synced), then new keys
//! (synced), then `state.json`. A crash between two steps leaves lines the
//! index does not cover yet. The next start, and the start of every commit,
//! finds them by comparing file sizes with `state.json` and indexes them, so
//! an acknowledged sample is never forgotten and never stored twice.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Map, Value, json};

use crate::config::Dirs;
use crate::error::{Context, Error, Result};
use crate::index::{Key, KeySet, key_of, sync_dir};
use crate::ingest::{Batch, Item};
use crate::isotime::parse_instant;
use crate::pyjson;
use crate::scan;

/// The stem of the file that holds tombstones.
pub const TOMBSTONES: &str = "_deleted";

const STATE_FILE: &str = "state.json";
const STATE_VERSION: u64 = 1;
const REINDEX_HINT: &str = "Run `pulso-server reindex` with the same PULSO_DATA and PULSO_INDEX, \
then start the server again.";

/// Keys held in memory at once while a file is indexed.
const CHUNK: usize = 1_000_000;

pub struct Counts {
    pub received: usize,
    pub new: usize,
    pub deleted: usize,
}

struct Newest {
    at: i64,
    text: String,
}

struct State {
    covered: BTreeMap<String, u64>,
    latest: BTreeMap<String, Newest>,
}

pub struct Store {
    data: PathBuf,
    index: PathBuf,
    _lock: File,
    seen: KeySet,
    deleted: KeySet,
    /// Per data file (by stem), the bytes from the start that the index covers.
    covered: BTreeMap<String, u64>,
    latest: BTreeMap<String, Newest>,
}

struct Appender {
    path: PathBuf,
    file: BufWriter<File>,
    /// Bytes of the file the index covered when the appender opened.
    start: u64,
    written: u64,
    existed: bool,
}

impl Appender {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.file
            .write_all(bytes)
            .context(|| format!("append to {}", self.path.display()))?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

impl Store {
    pub fn open(dirs: &Dirs) -> Result<Store> {
        fs::create_dir_all(&dirs.data).context(|| format!("create {}", dirs.data.display()))?;
        fs::create_dir_all(&dirs.index).context(|| format!("create {}", dirs.index.display()))?;
        let lock = lock_index(&dirs.index)?;
        let on_disk = list_data(&dirs.data)?;

        let Some(state) = read_state(&dirs.index)? else {
            if on_disk.values().any(|len| *len > 0) {
                return Err(Error::NeedsReindex(format!(
                    "the data directory {} holds files but there is no uuid index in {}. {REINDEX_HINT}",
                    dirs.data.display(),
                    dirs.index.display()
                )));
            }
            let create = |name| {
                KeySet::create_empty(&dirs.index, name)
                    .context(|| format!("create the uuid index in {}", dirs.index.display()))
            };
            let store = Store {
                data: dirs.data.clone(),
                index: dirs.index.clone(),
                _lock: lock,
                seen: create("seen")?,
                deleted: create("deleted")?,
                covered: BTreeMap::new(),
                latest: BTreeMap::new(),
            };
            store.save_state()?;
            return Ok(store);
        };

        let mut store = Store {
            data: dirs.data.clone(),
            index: dirs.index.clone(),
            _lock: lock,
            seen: open_set(&dirs.index, "seen")?,
            deleted: open_set(&dirs.index, "deleted")?,
            covered: state.covered,
            latest: state.latest,
        };
        store.sync_with_data()?;
        Ok(store)
    }

    /// The body of `GET /latest`: one flat object of type to timestamp, spaced
    /// as Python's `json.dumps` spaces it, which replies have always been.
    pub fn latest_json(&self) -> String {
        let mut out = vec![b'{'];
        for (i, (stem, newest)) in self.latest.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            pyjson::write_str(&mut out, stem);
            out.extend_from_slice(b": ");
            pyjson::write_str(&mut out, &newest.text);
        }
        out.push(b'}');
        String::from_utf8(out).expect("JSON text is UTF-8")
    }

    /// Appends the batch's samples and tombstones that are new, in order, and
    /// returns the counts the reply carries. The batch is already parsed and
    /// valid; an error here means the disk refused something, and the next
    /// call repairs whatever was left half done.
    pub fn commit(&mut self, batch: &Batch, stamp: &str) -> Result<Counts> {
        self.sync_with_data()?;

        let mut counts = Counts {
            received: batch.received,
            new: 0,
            deleted: 0,
        };
        let mut appenders: BTreeMap<String, Appender> = BTreeMap::new();
        let (mut new_seen, mut new_deleted): (Vec<Key>, Vec<Key>) = (Vec::new(), Vec::new());
        let (mut seen_now, mut deleted_now): (HashSet<Key>, HashSet<Key>) =
            (HashSet::new(), HashSet::new());
        let mut newest: Vec<(&str, i64, &str)> = Vec::new();
        let mut tombstone = Vec::new();

        for item in &batch.items {
            match item {
                Item::Sample {
                    key,
                    stem,
                    line,
                    end,
                } => {
                    if self.seen.contains(key) || !seen_now.insert(*key) {
                        continue;
                    }
                    self.appender(&mut appenders, stem)?
                        .write(&batch.lines[line.clone()])?;
                    new_seen.push(*key);
                    if let Some((at, text)) = end {
                        newest.push((stem, *at, text));
                    }
                    counts.new += 1;
                }
                Item::Tombstone(uuids) => {
                    let keys: Vec<Key> = uuids.iter().map(|uuid| key_of(uuid)).collect();
                    // Uuids repeated inside one tombstone are all fresh and all counted.
                    let fresh: Vec<usize> = (0..keys.len())
                        .filter(|&i| {
                            !self.deleted.contains(&keys[i]) && !deleted_now.contains(&keys[i])
                        })
                        .collect();
                    if fresh.is_empty() {
                        continue;
                    }
                    tombstone.clear();
                    write_tombstone(
                        &mut tombstone,
                        fresh.iter().map(|&i| uuids[i].as_str()),
                        stamp,
                    );
                    self.appender(&mut appenders, TOMBSTONES)?
                        .write(&tombstone)?;
                    for &i in &fresh {
                        if deleted_now.insert(keys[i]) {
                            new_deleted.push(keys[i]);
                        }
                    }
                    counts.deleted += fresh.len();
                }
            }
        }
        if appenders.is_empty() {
            return Ok(counts);
        }

        let mut covered = Vec::new();
        let mut created = false;
        for (stem, appender) in appenders {
            let Appender {
                path,
                file,
                start,
                written,
                existed,
            } = appender;
            let file = file
                .into_inner()
                .map_err(io::IntoInnerError::into_error)
                .context(|| format!("append to {}", path.display()))?;
            file.sync_data()
                .context(|| format!("sync {}", path.display()))?;
            created |= !existed;
            covered.push((stem, start + written));
        }
        if created {
            sync_dir(&self.data).context(|| format!("sync {}", self.data.display()))?;
        }
        let index_error = || format!("update the uuid index in {}", self.index.display());
        self.seen.add_new(&new_seen).context(index_error)?;
        self.deleted.add_new(&new_deleted).context(index_error)?;

        for (stem, at, text) in newest {
            note_instant(&mut self.latest, stem, at, text);
        }
        self.covered.extend(covered);
        self.save_state()?;
        Ok(counts)
    }

    /// Indexes whatever the data files hold beyond what `state.json` says is
    /// covered: lines a crash left unindexed, or files added by hand. A file
    /// that is shorter than the index covers has been changed or replaced, and
    /// its keys can no longer be trusted.
    fn sync_with_data(&mut self) -> Result<()> {
        let on_disk = list_data(&self.data)?;
        for (stem, covered) in &self.covered {
            let len = on_disk.get(stem).copied().unwrap_or(0);
            if len < *covered {
                return Err(Error::Mismatch(format!(
                    "{stem}.ndjson is {len} bytes but the index in {} covers {covered}. \
                     The data changed outside the server. {REINDEX_HINT}",
                    self.index.display()
                )));
            }
        }
        let mut changed = false;
        for (stem, len) in &on_disk {
            if self.covered.get(stem).copied().unwrap_or(0) != *len {
                self.catch_up(stem)?;
                changed = true;
            }
        }
        if changed {
            self.save_state()?;
        }
        Ok(())
    }

    fn catch_up(&mut self, stem: &str) -> Result<()> {
        let path = data_path(&self.data, stem);
        let from = self.covered.get(stem).copied().unwrap_or(0);
        let tombstones = stem == TOMBSTONES;
        let Store {
            seen,
            deleted,
            latest,
            ..
        } = self;
        let set = if tombstones { deleted } else { seen };
        let mut pending: Vec<Key> = Vec::new();
        let to = scan::each_line(&path, from, |line| {
            let Some(fields) = scan::fields(line) else {
                return Ok(());
            };
            if tombstones {
                pending.extend(fields.deleted.iter().map(|uuid| key_of(uuid)));
            } else if let Some(uuid) = &fields.uuid {
                pending.push(key_of(uuid));
                note_latest(latest, stem, fields.end.as_deref());
            }
            if pending.len() >= CHUNK {
                set.add(&pending)?;
                pending.clear();
            }
            Ok(())
        })
        .context(|| format!("index {}", path.display()))?;
        set.add(&pending)
            .context(|| format!("index {}", path.display()))?;
        self.covered.insert(stem.to_owned(), to);
        Ok(())
    }

    fn appender<'a>(
        &self,
        appenders: &'a mut BTreeMap<String, Appender>,
        stem: &str,
    ) -> Result<&'a mut Appender> {
        if !appenders.contains_key(stem) {
            let path = data_path(&self.data, stem);
            let existed = path.exists();
            let start = self.covered.get(stem).copied().unwrap_or(0);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .context(|| format!("open {}", path.display()))?;
            let mut writer = BufWriter::with_capacity(1 << 16, file);
            let mut written = 0;
            // A crash can leave a last line without its newline; the next record must not join it.
            if !ends_with_newline(&path, start).context(|| format!("read {}", path.display()))? {
                writer
                    .write_all(b"\n")
                    .context(|| format!("append to {}", path.display()))?;
                written = 1;
            }
            appenders.insert(
                stem.to_owned(),
                Appender {
                    path,
                    file: writer,
                    start,
                    written,
                    existed,
                },
            );
        }
        Ok(appenders.get_mut(stem).expect("inserted above"))
    }

    fn save_state(&self) -> Result<()> {
        write_state(&self.index, &self.covered, &self.latest)
    }
}

pub struct Summary {
    pub files: usize,
    pub uuids: usize,
    pub deleted: usize,
    pub seconds: f64,
}

/// Rebuilds the index from the NDJSON files, holding at most `CHUNK` keys in
/// memory. Until the last step the index is marked invalid, so a run that
/// stops half way leaves a server that refuses to start rather than one that
/// trusts a partial index.
pub fn reindex(dirs: &Dirs) -> Result<Summary> {
    let started = Instant::now();
    if !dirs.data.is_dir() {
        return Err(Error::Mismatch(format!(
            "the data directory {} does not exist",
            dirs.data.display()
        )));
    }
    fs::create_dir_all(&dirs.index).context(|| format!("create {}", dirs.index.display()))?;
    let _lock = lock_index(&dirs.index)?;
    let on_disk = list_data(&dirs.data)?;

    match fs::remove_file(dirs.index.join(STATE_FILE)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(Error::Io {
                context: "remove the old index state".into(),
                source: error,
            });
        }
        _ => {}
    }
    sync_dir(&dirs.index).context(|| format!("sync {}", dirs.index.display()))?;

    let work = dirs.index.join("rebuild");
    match fs::remove_dir_all(&work) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(Error::Io {
                context: format!("clear {}", work.display()),
                source: error,
            });
        }
        _ => {}
    }
    fs::create_dir(&work).context(|| format!("create {}", work.display()))?;

    let mut seen = Builder::new(&work, "seen")?;
    let mut deleted = Builder::new(&work, "deleted")?;
    let mut covered = BTreeMap::new();
    let mut latest = BTreeMap::new();
    for stem in on_disk.keys() {
        let path = data_path(&dirs.data, stem);
        let tombstones = stem == TOMBSTONES;
        let to = scan::each_line(&path, 0, |line| {
            let Some(fields) = scan::fields(line) else {
                return Ok(());
            };
            if tombstones {
                for uuid in &fields.deleted {
                    deleted.push(key_of(uuid))?;
                }
            } else if let Some(uuid) = &fields.uuid {
                seen.push(key_of(uuid))?;
                note_latest(&mut latest, stem, fields.end.as_deref());
            }
            Ok(())
        })
        .context(|| format!("index {}", path.display()))?;
        covered.insert(stem.clone(), to);
    }
    let (uuids, deleted_uuids) = (seen.finish()?, deleted.finish()?);

    for name in ["seen", "deleted"] {
        let (from, to) = (
            work.join(format!("{name}.sorted")),
            dirs.index.join(format!("{name}.sorted")),
        );
        fs::rename(&from, &to).context(|| format!("move {} into place", from.display()))?;
        File::create(dirs.index.join(format!("{name}.tail")))
            .and_then(|file| file.sync_all())
            .context(|| format!("empty the {name} tail"))?;
    }
    sync_dir(&dirs.index).context(|| format!("sync {}", dirs.index.display()))?;
    write_state(&dirs.index, &covered, &latest)?;
    // Best effort: what is left is an empty directory.
    let _ = fs::remove_dir_all(&work);

    Ok(Summary {
        files: on_disk.len(),
        uuids,
        deleted: deleted_uuids,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// Collects keys and merges them into a new set a chunk at a time.
struct Builder {
    set: KeySet,
    chunk: Vec<Key>,
}

impl Builder {
    fn new(work: &Path, name: &str) -> Result<Builder> {
        let set = KeySet::create_empty(work, name)
            .context(|| format!("create {name} in {}", work.display()))?;
        Ok(Builder {
            set,
            chunk: Vec::new(),
        })
    }

    fn push(&mut self, key: Key) -> io::Result<()> {
        self.chunk.push(key);
        if self.chunk.len() >= CHUNK {
            self.flush()
        } else {
            Ok(())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.chunk.sort_unstable();
        self.chunk.dedup();
        self.set.absorb_sorted(&self.chunk)?;
        self.chunk.clear();
        Ok(())
    }

    /// Number of distinct keys.
    fn finish(&mut self) -> Result<usize> {
        self.flush().context(|| "write the uuid index".to_owned())?;
        Ok(self.set.len())
    }
}

fn data_path(data: &Path, stem: &str) -> PathBuf {
    data.join(format!("{stem}.ndjson"))
}

/// Regular files named `<stem>.ndjson` and their sizes.
fn list_data(data: &Path) -> Result<BTreeMap<String, u64>> {
    let context = || format!("list {}", data.display());
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(data).context(context)? {
        let entry = entry.context(context)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".ndjson") else {
            continue;
        };
        let metadata =
            fs::metadata(entry.path()).context(|| format!("stat {}", entry.path().display()))?;
        if metadata.is_file() {
            files.insert(stem.to_owned(), metadata.len());
        }
    }
    Ok(files)
}

fn lock_index(index: &Path) -> Result<File> {
    let path = index.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .context(|| format!("open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::Busy(format!(
            "another pulso-server or reindex is using the index in {}",
            index.display()
        ))),
        Err(TryLockError::Error(source)) => Err(Error::Io {
            context: format!("lock {}", path.display()),
            source,
        }),
    }
}

fn open_set(index: &Path, name: &str) -> Result<KeySet> {
    KeySet::open(index, name).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Error::Mismatch(format!(
            "the uuid index in {} is incomplete: {name}.sorted is missing. {REINDEX_HINT}",
            index.display()
        )),
        io::ErrorKind::InvalidData => Error::Mismatch(format!("{error}. {REINDEX_HINT}")),
        _ => Error::Io {
            context: format!("open the uuid index in {}", index.display()),
            source: error,
        },
    })
}

fn ends_with_newline(path: &Path, len: u64) -> io::Result<bool> {
    if len == 0 {
        return Ok(true);
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

fn write_tombstone<'a>(out: &mut Vec<u8>, uuids: impl Iterator<Item = &'a str>, stamp: &str) {
    out.extend_from_slice(b"{\"deleted\":[");
    for (i, uuid) in uuids.enumerate() {
        if i > 0 {
            out.push(b',');
        }
        pyjson::write_str(out, uuid);
    }
    out.extend_from_slice(b"],\"receivedAt\":");
    pyjson::write_str(out, stamp);
    out.extend_from_slice(b"}\n");
}

/// Keeps the newest `end` per type. On a tie the first one seen stays.
fn note_latest(latest: &mut BTreeMap<String, Newest>, stem: &str, end: Option<&str>) {
    if let Some(text) = end
        && let Some(at) = parse_instant(text)
    {
        note_instant(latest, stem, at, text);
    }
}

fn note_instant(latest: &mut BTreeMap<String, Newest>, stem: &str, at: i64, text: &str) {
    match latest.get_mut(stem) {
        Some(current) if at <= current.at => {}
        Some(current) => {
            *current = Newest {
                at,
                text: text.to_owned(),
            }
        }
        None => {
            latest.insert(
                stem.to_owned(),
                Newest {
                    at,
                    text: text.to_owned(),
                },
            );
        }
    }
}

fn read_state(index: &Path) -> Result<Option<State>> {
    let path = index.join(STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Io {
                context: format!("read {}", path.display()),
                source,
            });
        }
    };
    let broken = |why: String| {
        Error::Mismatch(format!(
            "{} cannot be used: {why}. {REINDEX_HINT}",
            path.display()
        ))
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| broken(error.to_string()))?;
    if value["version"] != STATE_VERSION {
        return Err(broken(format!(
            "it is version {}, this server reads version {STATE_VERSION}",
            value["version"]
        )));
    }
    let mut covered = BTreeMap::new();
    for (stem, bytes) in value["files"]
        .as_object()
        .ok_or_else(|| broken("files is missing".to_owned()))?
    {
        let bytes = bytes
            .as_u64()
            .ok_or_else(|| broken(format!("files.{stem} is not a size")))?;
        covered.insert(stem.clone(), bytes);
    }
    let mut latest = BTreeMap::new();
    for (stem, text) in value["latest"]
        .as_object()
        .ok_or_else(|| broken("latest is missing".to_owned()))?
    {
        let text = text
            .as_str()
            .ok_or_else(|| broken(format!("latest.{stem} is not text")))?;
        let at = parse_instant(text)
            .ok_or_else(|| broken(format!("latest.{stem} is not a timestamp")))?;
        latest.insert(
            stem.clone(),
            Newest {
                at,
                text: text.to_owned(),
            },
        );
    }
    Ok(Some(State { covered, latest }))
}

fn write_state(
    index: &Path,
    covered: &BTreeMap<String, u64>,
    latest: &BTreeMap<String, Newest>,
) -> Result<()> {
    let files: Map<String, Value> = covered
        .iter()
        .map(|(stem, bytes)| (stem.clone(), Value::from(*bytes)))
        .collect();
    let newest: Map<String, Value> = latest
        .iter()
        .map(|(stem, n)| (stem.clone(), Value::String(n.text.clone())))
        .collect();
    let state = json!({ "version": STATE_VERSION, "files": files, "latest": newest });

    let (path, temp) = (index.join(STATE_FILE), index.join("state.json.tmp"));
    let write = || -> io::Result<()> {
        let mut file = File::create(&temp)?;
        file.write_all(&serde_json::to_vec(&state).expect("a JSON value serializes"))?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        sync_dir(index)
    };
    write().context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests;
