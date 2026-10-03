use super::*;
use crate::ingest;

const STAMP: &str = "2026-10-03T22:53:10+00:00";

const SAMPLES: &str = r#"[
  {"uuid":"AAAA-1111","type":"sleepAnalysis","start":"2026-07-06T01:12:00+01:00","end":"2026-07-06T02:40:00+01:00","value":"asleepREM","source":"Apple Watch","metadata":{"timeZone":"Europe/Lisbon"}},
  {"uuid":"BBBB-2222","type":"heartRate","start":"2026-07-06T08:00:00+01:00","end":"2026-07-06T08:00:00+01:00","value":58,"unit":"count/min","source":"Apple Watch"},
  {"deleted":["CCCC-3333","DDDD-4444"]}
]"#;

struct Fixture {
    _root: tempfile::TempDir,
    dirs: Dirs,
}

impl Fixture {
    fn new() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            data: root.path().join("data"),
            index: root.path().join("index"),
        };
        Fixture { _root: root, dirs }
    }

    fn open(&self) -> Store {
        Store::open(&self.dirs).unwrap()
    }

    fn file(&self, stem: &str) -> PathBuf {
        self.dirs.data.join(format!("{stem}.ndjson"))
    }

    fn text(&self, stem: &str) -> String {
        fs::read_to_string(self.file(stem)).unwrap_or_default()
    }

    fn lines(&self, stem: &str) -> Vec<Value> {
        self.text(stem)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Writes files the way the Python server did, without the index.
    fn seed(&self, stem: &str, text: &str) {
        fs::create_dir_all(&self.dirs.data).unwrap();
        fs::write(self.file(stem), text).unwrap();
    }
}

fn parsed(body: &str) -> Batch {
    ingest::parse(body.as_bytes(), STAMP).unwrap()
}

fn commit(store: &mut Store, body: &str) -> (usize, usize, usize) {
    let counts = store.commit(&parsed(body), STAMP).unwrap();
    (counts.received, counts.new, counts.deleted)
}

fn sample(uuid: &str, stem: &str, end: &str) -> String {
    format!(r#"{{"uuid":"{uuid}","type":"{stem}","start":"{end}","end":"{end}","value":1}}"#)
}

#[test]
fn a_new_store_creates_its_index_and_starts_empty() {
    let fx = Fixture::new();
    let store = fx.open();
    assert_eq!(store.latest_json(), "{}");
    for name in [
        "state.json",
        "seen.sorted",
        "seen.tail",
        "deleted.sorted",
        "deleted.tail",
        "lock",
    ] {
        assert!(fx.dirs.index.join(name).exists(), "{name}");
    }
    assert!(fx.dirs.data.is_dir());
    assert_eq!(fs::read_dir(&fx.dirs.data).unwrap().count(), 0);
}

#[test]
fn a_commit_appends_what_is_new_and_counts_it() {
    let fx = Fixture::new();
    let mut store = fx.open();
    assert_eq!(commit(&mut store, SAMPLES), (3, 2, 2));

    let sleep = fx.lines("sleepAnalysis");
    assert_eq!(sleep.len(), 1);
    assert_eq!(sleep[0]["uuid"], "AAAA-1111");
    assert_eq!(sleep[0]["receivedAt"], STAMP);
    assert_eq!(fx.lines("heartRate").len(), 1);
    let tombstones = fx.lines(TOMBSTONES);
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0]["deleted"], json!(["CCCC-3333", "DDDD-4444"]));
    assert_eq!(
        fx.text(TOMBSTONES),
        "{\"deleted\":[\"CCCC-3333\",\"DDDD-4444\"],\"receivedAt\":\"2026-10-03T22:53:10+00:00\"}\n"
    );
}

#[test]
fn sending_a_batch_again_changes_nothing() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    let before = (
        fx.text("sleepAnalysis"),
        fx.text("heartRate"),
        fx.text(TOMBSTONES),
    );
    let state = fs::read(fx.dirs.index.join("state.json")).unwrap();

    assert_eq!(commit(&mut store, SAMPLES), (3, 0, 0));
    assert_eq!(
        before,
        (
            fx.text("sleepAnalysis"),
            fx.text("heartRate"),
            fx.text(TOMBSTONES)
        )
    );
    assert_eq!(state, fs::read(fx.dirs.index.join("state.json")).unwrap());
}

#[test]
fn a_uuid_repeated_in_a_batch_or_under_another_type_is_stored_once() {
    let fx = Fixture::new();
    let mut store = fx.open();
    let body = format!(
        "[{},{},{},{}]",
        sample("U1", "a", "2026-07-06T08:00:00Z"),
        sample("U1", "a", "2026-07-06T09:00:00Z"),
        sample("U1", "b", "2026-07-06T09:00:00Z"),
        sample("U2", "b", "2026-07-06T09:00:00Z"),
    );
    assert_eq!(commit(&mut store, &body), (4, 2, 0));
    assert_eq!(fx.lines("a").len(), 1);
    assert_eq!(fx.lines("b").len(), 1);
    assert_eq!(fx.lines("b")[0]["uuid"], "U2");
}

#[test]
fn uuids_repeated_inside_one_tombstone_are_all_counted() {
    let fx = Fixture::new();
    let mut store = fx.open();
    assert_eq!(
        commit(&mut store, r#"[{"deleted":["a","a","b"]}]"#),
        (1, 0, 3)
    );
    assert_eq!(fx.lines(TOMBSTONES)[0]["deleted"], json!(["a", "a", "b"]));
    assert_eq!(commit(&mut store, r#"[{"deleted":["a","b"]}]"#), (1, 0, 0));
    // An earlier tombstone in the same batch makes later repeats stale.
    assert_eq!(
        commit(&mut store, r#"[{"deleted":["x"]},{"deleted":["x","y"]}]"#),
        (2, 0, 2)
    );
    assert_eq!(fx.lines(TOMBSTONES).len(), 3);
}

#[test]
fn dedupe_and_latest_survive_a_restart() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);

    let mut store = fx.open();
    assert_eq!(
        store.latest_json(),
        r#"{"heartRate": "2026-07-06T08:00:00+01:00", "sleepAnalysis": "2026-07-06T02:40:00+01:00"}"#
    );
    assert_eq!(commit(&mut store, SAMPLES), (3, 0, 0));
}

#[test]
fn latest_compares_absolute_time_and_the_first_of_equal_instants_stays() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(
        &mut store,
        &format!("[{}]", sample("A", "t", "2026-07-06T09:03:00+01:00")),
    );
    assert_eq!(store.latest_json(), r#"{"t": "2026-07-06T09:03:00+01:00"}"#);
    // 05:00-04:00 is 09:00Z: later, though it sorts earlier as text.
    commit(
        &mut store,
        &format!("[{}]", sample("B", "t", "2026-07-06T05:00:00-04:00")),
    );
    assert_eq!(store.latest_json(), r#"{"t": "2026-07-06T05:00:00-04:00"}"#);
    commit(
        &mut store,
        &format!("[{}]", sample("C", "t", "2026-07-06T10:00:00Z")),
    );
    assert_eq!(store.latest_json(), r#"{"t": "2026-07-06T10:00:00Z"}"#);
    // The same instant written another way does not replace it, and neither does an older one.
    commit(
        &mut store,
        &format!(
            "[{},{}]",
            sample("D", "t", "2026-07-06T11:00:00+01:00"),
            sample("E", "t", "2026-07-05T10:00:00Z")
        ),
    );
    assert_eq!(store.latest_json(), r#"{"t": "2026-07-06T10:00:00Z"}"#);
    // What is not a timestamp with a zone never counts, and tombstones carry none.
    commit(
        &mut store,
        r#"[{"uuid":"F","type":"u","end":"2026-07-06"},{"uuid":"G","type":"u"},{"deleted":["x"]}]"#,
    );
    assert_eq!(store.latest_json(), r#"{"t": "2026-07-06T10:00:00Z"}"#);
}

#[test]
fn a_sample_typed_like_the_tombstone_file_is_filed_under_unknown() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(
        &mut store,
        &format!("[{}]", sample("U", "_deleted", "2026-07-06T08:00:00Z")),
    );
    assert!(!fx.file(TOMBSTONES).exists());
    assert_eq!(fx.lines("unknown").len(), 1);
}

#[test]
fn files_without_an_index_need_a_reindex() {
    let fx = Fixture::new();
    fx.seed(
        "heartRate",
        &format!("{}\n", sample("A", "heartRate", "2026-07-06T08:00:00Z")),
    );
    let error = Store::open(&fx.dirs).err().unwrap();
    assert!(matches!(error, Error::NeedsReindex(_)), "{error}");
    assert!(error.to_string().contains("pulso-server reindex"));
    assert!(!fx.dirs.index.join("state.json").exists());
    assert!(!fx.dirs.index.join("seen.sorted").exists());
}

#[test]
fn empty_files_do_not_need_a_reindex() {
    let fx = Fixture::new();
    fx.seed("heartRate", "");
    assert_eq!(fx.open().latest_json(), "{}");
}

#[test]
fn an_index_that_was_deleted_is_not_rebuilt_by_starting_the_server() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    let before = fx.text("heartRate");

    fs::remove_dir_all(&fx.dirs.index).unwrap();
    assert!(matches!(
        Store::open(&fx.dirs).err().unwrap(),
        Error::NeedsReindex(_)
    ));
    assert!(!fx.dirs.index.join("seen.sorted").exists());
    assert_eq!(before, fx.text("heartRate"));

    let summary = reindex(&fx.dirs).unwrap();
    assert_eq!((summary.files, summary.uuids, summary.deleted), (3, 2, 2));
    let mut store = fx.open();
    assert_eq!(commit(&mut store, SAMPLES), (3, 0, 0));
}

fn python_written_files(fx: &Fixture) {
    fx.seed(
        "heartRate",
        &format!(
            "{}\n\n{}\nnot json\n[1,2]\n{{\"uuid\":5,\"end\":\"2027-01-01T00:00:00Z\"}}\n{}\n",
            r#"{"uuid":"H1","type":"heartRate","end":"2026-07-06T08:00:00+01:00","receivedAt":"r"}"#,
            r#"{"uuid":"H2","type":"heartRate","end":"2026-07-06T09:00:00+01:00","receivedAt":"r"}"#,
            r#"{"uuid":"H2","type":"heartRate","end":"2026-07-06T07:00:00+01:00","receivedAt":"r"}"#,
        ),
    );
    fx.seed(
        "sleepAnalysis",
        "{\"uuid\":\"S1\",\"end\":\"2026-07-06T09:00:00Z\"}\r\n{\"uuid\":\"S2\",\"end\":\"2026-07-06T10:00:00+01:00\"}",
    );
    fx.seed(
        TOMBSTONES,
        "{\"deleted\":[\"X1\",\"X2\"],\"receivedAt\":\"r\"}\n{\"deleted\":\"junk\"}\n{\"deleted\":[\"X2\",7,\"X3\"]}\n",
    );
}

#[test]
fn reindex_indexes_files_in_place_and_rebuilds_latest() {
    let fx = Fixture::new();
    python_written_files(&fx);
    let before: Vec<String> = ["heartRate", "sleepAnalysis", TOMBSTONES]
        .iter()
        .map(|s| fx.text(s))
        .collect();

    let summary = reindex(&fx.dirs).unwrap();
    assert_eq!((summary.files, summary.uuids, summary.deleted), (3, 4, 3));

    let mut store = fx.open();
    // The first instant seen wins a tie, and H2's second line (07:00, older) does not lower it.
    assert_eq!(
        store.latest_json(),
        r#"{"heartRate": "2026-07-06T09:00:00+01:00", "sleepAnalysis": "2026-07-06T09:00:00Z"}"#
    );
    let again = r#"[{"uuid":"H1","type":"heartRate"},{"uuid":"H2","type":"heartRate"},{"uuid":"S1","type":"sleepAnalysis"},{"uuid":"S2","type":"sleepAnalysis"},{"deleted":["X1","X2","X3"]}]"#;
    assert_eq!(commit(&mut store, again), (5, 0, 0));
    assert_eq!(
        commit(
            &mut store,
            r#"[{"uuid":"NEW","type":"heartRate"},{"deleted":["X4"]}]"#
        ),
        (2, 1, 1)
    );

    let after: Vec<String> = ["heartRate", "sleepAnalysis", TOMBSTONES]
        .iter()
        .map(|s| fx.text(s))
        .collect();
    for (old, new) in before.iter().zip(&after) {
        assert!(
            new.starts_with(old.as_str()),
            "pre-existing bytes must stay as they were"
        );
    }
}

#[test]
fn reindex_replaces_an_index_that_is_out_of_date() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    // A file edited by hand: the old index no longer matches it.
    fx.seed(
        "heartRate",
        &format!("{}\n", sample("ONLY", "heartRate", "2026-07-06T08:00:00Z")),
    );
    assert!(matches!(
        Store::open(&fx.dirs).err().unwrap(),
        Error::Mismatch(_)
    ));

    reindex(&fx.dirs).unwrap();
    let mut store = fx.open();
    assert_eq!(
        commit(
            &mut store,
            &format!(
                "[{}]",
                sample("BBBB-2222", "heartRate", "2026-07-06T08:00:00Z")
            )
        ),
        (1, 1, 0)
    );
    assert_eq!(
        commit(
            &mut store,
            &format!("[{}]", sample("ONLY", "heartRate", "2026-07-06T08:00:00Z"))
        ),
        (1, 0, 0)
    );
}

#[test]
fn reindex_refuses_a_missing_data_directory_and_a_busy_index() {
    let fx = Fixture::new();
    assert!(matches!(
        reindex(&fx.dirs).err().unwrap(),
        Error::Mismatch(_)
    ));
    let _store = fx.open();
    assert!(matches!(reindex(&fx.dirs).err().unwrap(), Error::Busy(_)));
}

#[test]
fn reindex_leaves_the_index_invalid_if_it_stops_half_way() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    // A data file that cannot be read stops the run after the old state is removed.
    fs::create_dir_all(&fx.dirs.data).unwrap();
    fs::set_permissions(
        fx.file("heartRate"),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    if File::open(fx.file("heartRate")).is_ok() {
        return; // running as a user that ignores file permissions
    }
    assert!(reindex(&fx.dirs).is_err());
    assert!(!fx.dirs.index.join("state.json").exists());
    fs::set_permissions(
        fx.file("heartRate"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    assert!(matches!(
        Store::open(&fx.dirs).err().unwrap(),
        Error::NeedsReindex(_)
    ));
}

#[test]
fn lines_a_crash_left_unindexed_are_found_on_the_next_start() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    // The data was synced but the keys and the state were not.
    let mut file = OpenOptions::new()
        .append(true)
        .open(fx.file("heartRate"))
        .unwrap();
    writeln!(
        file,
        "{}",
        sample("LOST", "heartRate", "2026-07-07T08:00:00+01:00")
    )
    .unwrap();
    let mut file = OpenOptions::new()
        .append(true)
        .open(fx.file(TOMBSTONES))
        .unwrap();
    writeln!(file, r#"{{"deleted":["T9"]}}"#).unwrap();
    drop(file);

    let mut store = fx.open();
    assert!(
        store
            .latest_json()
            .contains(r#""heartRate": "2026-07-07T08:00:00+01:00""#)
    );
    assert_eq!(
        commit(
            &mut store,
            &format!(
                "[{},{{\"deleted\":[\"T9\"]}}]",
                sample("LOST", "heartRate", "2026-07-07T08:00:00+01:00")
            )
        ),
        (2, 0, 0)
    );
    assert_eq!(fx.lines("heartRate").len(), 2);
}

#[test]
fn a_crash_between_the_keys_and_the_state_costs_nothing() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    let state_before = fs::read(fx.dirs.index.join("state.json")).unwrap();
    commit(
        &mut store,
        &format!("[{}]", sample("LATE", "heartRate", "2026-07-08T08:00:00Z")),
    );
    drop(store);
    // Keys are in the tail, but the state still describes the earlier commit.
    fs::write(fx.dirs.index.join("state.json"), state_before).unwrap();

    let mut store = fx.open();
    assert!(store.latest_json().contains("2026-07-08T08:00:00Z"));
    assert_eq!(
        commit(
            &mut store,
            &format!("[{}]", sample("LATE", "heartRate", "2026-07-08T08:00:00Z"))
        ),
        (1, 0, 0)
    );
    assert_eq!(fx.lines("heartRate").len(), 2);
}

#[test]
fn a_torn_last_line_does_not_swallow_the_next_record() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    OpenOptions::new()
        .append(true)
        .open(fx.file("heartRate"))
        .unwrap()
        .write_all(b"{\"uuid\":\"TORN\",\"ty")
        .unwrap();

    let mut store = fx.open();
    assert_eq!(
        commit(
            &mut store,
            &format!("[{}]", sample("NEXT", "heartRate", "2026-07-08T08:00:00Z"))
        ),
        (1, 1, 0)
    );
    let text = fx.text("heartRate");
    assert!(text.ends_with('\n'));
    let broken = text
        .lines()
        .filter(|l| serde_json::from_str::<Value>(l).is_err())
        .count();
    assert_eq!(broken, 1, "only the torn line is unreadable: {text}");
    assert_eq!(fx.lines_ok("heartRate").last().unwrap()["uuid"], "NEXT");
    drop(store);

    let mut store = fx.open();
    assert_eq!(
        commit(
            &mut store,
            &format!("[{}]", sample("NEXT", "heartRate", "2026-07-08T08:00:00Z"))
        ),
        (1, 0, 0)
    );
}

impl Fixture {
    fn lines_ok(&self, stem: &str) -> Vec<Value> {
        self.text(stem)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

#[test]
fn a_last_line_without_a_newline_that_is_complete_is_still_indexed() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    OpenOptions::new()
        .append(true)
        .open(fx.file("heartRate"))
        .unwrap()
        .write_all(sample("NONL", "heartRate", "2026-07-09T08:00:00Z").as_bytes())
        .unwrap();

    let mut store = fx.open();
    assert_eq!(
        commit(
            &mut store,
            &format!("[{}]", sample("NONL", "heartRate", "2026-07-09T08:00:00Z"))
        ),
        (1, 0, 0)
    );
    commit(
        &mut store,
        &format!("[{}]", sample("AFTER", "heartRate", "2026-07-10T08:00:00Z")),
    );
    assert_eq!(fx.lines("heartRate").len(), 3);
}

#[test]
fn a_data_file_shorter_than_the_index_covers_is_refused() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);

    let len = fs::metadata(fx.file("heartRate")).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(fx.file("heartRate"))
        .unwrap()
        .set_len(len - 10)
        .unwrap();
    let error = Store::open(&fx.dirs).err().unwrap();
    assert!(
        matches!(error, Error::Mismatch(_)) && error.to_string().contains("reindex"),
        "{error}"
    );

    fs::remove_file(fx.file("heartRate")).unwrap();
    assert!(matches!(
        Store::open(&fx.dirs).err().unwrap(),
        Error::Mismatch(_)
    ));
}

#[test]
fn a_shrunk_file_is_refused_while_running_too() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    fs::remove_file(fx.file("heartRate")).unwrap();
    let batch = parsed(&format!(
        "[{}]",
        sample("N", "other", "2026-07-08T08:00:00Z")
    ));
    assert!(matches!(
        store.commit(&batch, STAMP).err().unwrap(),
        Error::Mismatch(_)
    ));
    assert!(!fx.file("other").exists());
}

#[test]
fn a_file_added_by_hand_is_indexed_from_its_start_on_the_next_start() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(&mut store, SAMPLES);
    drop(store);
    fx.seed(
        "extra",
        &format!("{}\n", sample("EXTRA", "extra", "2026-07-06T08:00:00Z")),
    );

    let mut store = fx.open();
    assert!(store.latest_json().contains(r#""extra""#));
    assert_eq!(
        commit(
            &mut store,
            &format!("[{}]", sample("EXTRA", "other", "2026-07-06T08:00:00Z"))
        ),
        (1, 0, 0)
    );
}

#[test]
fn a_second_store_cannot_use_an_index_that_is_in_use() {
    let fx = Fixture::new();
    let first = fx.open();
    assert!(matches!(
        Store::open(&fx.dirs).err().unwrap(),
        Error::Busy(_)
    ));
    drop(first);
    let _ = fx.open();
}

#[test]
fn a_commit_that_failed_part_way_is_repaired_by_the_next_one() {
    let fx = Fixture::new();
    let mut store = fx.open();
    // A directory where a data file must go makes the second sample fail after the first was written.
    fs::create_dir(fx.file("bad")).unwrap();
    let body =
        r#"[{"uuid":"a","type":"good","end":"2026-07-06T08:00:00Z"},{"uuid":"b","type":"bad"}]"#;
    let batch = parsed(body);
    assert!(store.commit(&batch, STAMP).is_err());

    fs::remove_dir(fx.file("bad")).unwrap();
    let counts = store.commit(&batch, STAMP).unwrap();
    assert_eq!(
        (counts.new, counts.deleted),
        (1, 0),
        "a was found already stored; only b is new"
    );
    assert_eq!(fx.lines("good").len(), 1);
    assert_eq!(fx.lines("bad").len(), 1);
    assert_eq!(store.latest_json(), r#"{"good": "2026-07-06T08:00:00Z"}"#);
    drop(store);
    let mut store = fx.open();
    assert_eq!(commit(&mut store, body), (2, 0, 0));
}

#[test]
fn an_unreadable_or_foreign_state_is_refused() {
    let fx = Fixture::new();
    drop(fx.open());
    let state = fx.dirs.index.join("state.json");
    for bad in [
        "not json",
        r#"{"version":2,"files":{},"latest":{}}"#,
        r#"{"version":1,"latest":{}}"#,
        r#"{"version":1,"files":{"a":"x"},"latest":{}}"#,
        r#"{"version":1,"files":{},"latest":{"a":"not a time"}}"#,
        r#"{"version":1,"files":{},"latest":{"a":5}}"#,
    ] {
        fs::write(&state, bad).unwrap();
        let error = Store::open(&fx.dirs).err().unwrap();
        assert!(
            matches!(error, Error::Mismatch(_)) && error.to_string().contains("reindex"),
            "{bad}: {error}"
        );
    }
}

#[test]
fn a_missing_or_damaged_index_file_is_refused() {
    let fx = Fixture::new();
    drop(fx.open());
    fs::write(fx.dirs.index.join("seen.sorted"), [0u8; 5]).unwrap();
    assert!(matches!(
        Store::open(&fx.dirs).err().unwrap(),
        Error::Mismatch(_)
    ));
    fs::remove_file(fx.dirs.index.join("seen.sorted")).unwrap();
    let error = Store::open(&fx.dirs).err().unwrap();
    assert!(
        matches!(error, Error::Mismatch(_)) && error.to_string().contains("seen.sorted"),
        "{error}"
    );
}

#[test]
fn many_samples_merge_into_the_sorted_file_and_stay_deduplicated() {
    let fx = Fixture::new();
    let mut store = fx.open();
    let count = crate::index::MERGE_AT + 1_000;
    let body = format!(
        "[{}]",
        (0..count)
            .map(|n| format!(r#"{{"uuid":"U{n}","type":"bulk"}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_eq!(commit(&mut store, &body), (count, count, 0));
    assert_eq!(commit(&mut store, &body), (count, 0, 0));
    drop(store);

    // The merge ran: most keys are in the sorted file, and the tail is short.
    let sorted = fs::metadata(fx.dirs.index.join("seen.sorted"))
        .unwrap()
        .len();
    let tail = fs::metadata(fx.dirs.index.join("seen.tail")).unwrap().len();
    assert_eq!(sorted + tail, 16 * count as u64);
    assert!(sorted > 0 && tail < 16 * crate::index::MERGE_AT as u64);

    let mut store = fx.open();
    assert_eq!(commit(&mut store, &body), (count, 0, 0));
    assert_eq!(fx.lines("bulk").len(), count);
}

#[test]
fn only_what_is_new_is_appended_when_old_and_new_samples_mix() {
    let fx = Fixture::new();
    let mut store = fx.open();
    commit(
        &mut store,
        &format!("[{}]", sample("OLD", "t", "2026-07-06T08:00:00Z")),
    );
    let body = format!(
        "[{},{},{}]",
        sample("OLD", "t", "2026-07-06T08:00:00Z"),
        sample("NEW1", "t", "2026-07-06T09:00:00Z"),
        sample("NEW2", "u", "2026-07-06T09:00:00Z")
    );
    assert_eq!(commit(&mut store, &body), (3, 2, 0));
    let uuids: Vec<String> = fx
        .lines("t")
        .iter()
        .map(|l| l["uuid"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(uuids, ["OLD", "NEW1"]);
}
