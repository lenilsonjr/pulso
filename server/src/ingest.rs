//! A request body, turned into the work for one commit.

use std::fmt;
use std::ops::Range;

use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_json::Value;

use crate::index::{Key, key_of};
use crate::isotime::parse_instant;
use crate::pyjson;
use crate::store::TOMBSTONES;

const UNKNOWN: &str = "unknown";

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    BadJson,
    NotAnArray,
}

#[derive(Debug, Default)]
pub struct Batch {
    /// Elements in the body, including those that are neither samples nor tombstones.
    pub received: usize,
    pub items: Vec<Item>,
    /// The stored form of every sample, one line each, back to back.
    pub lines: Vec<u8>,
}

#[derive(Debug)]
pub enum Item {
    Sample {
        key: Key,
        /// The data file the sample goes to, without `.ndjson`.
        stem: String,
        line: Range<usize>,
        /// The sample's `end` as an instant and as written, when it is a valid timestamp.
        end: Option<(i64, String)>,
    },
    /// The uuids of a tombstone, in order and with repeats.
    Tombstone(Vec<String>),
}

/// Nothing is stored unless the whole body parses, so a bad body has no effect.
pub fn parse(body: &[u8], stamp: &str) -> Result<Batch, ParseError> {
    let first = body
        .iter()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'));
    if first != Some(&b'[') {
        let valid = serde_json::from_slice::<IgnoredAny>(body).is_ok();
        return Err(if valid {
            ParseError::NotAnArray
        } else {
            ParseError::BadJson
        });
    }
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let batch = deserializer
        .deserialize_seq(Elements { stamp })
        .map_err(|_| ParseError::BadJson)?;
    deserializer.end().map_err(|_| ParseError::BadJson)?;
    Ok(batch)
}

struct Elements<'a> {
    stamp: &'a str,
}

impl<'de> Visitor<'de> for Elements<'_> {
    type Value = Batch;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Batch, A::Error> {
        let mut batch = Batch::default();
        while let Some(element) = seq.next_element::<Value>()? {
            batch.received += 1;
            batch.add(element, self.stamp);
        }
        Ok(batch)
    }
}

impl Batch {
    fn add(&mut self, element: Value, stamp: &str) {
        let Value::Object(mut object) = element else {
            return;
        };
        if let Some(deleted) = object.get("deleted") {
            let uuids = match deleted {
                Value::Array(items) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            self.items.push(Item::Tombstone(uuids));
            return;
        }
        let Some(uuid) = object.get("uuid").and_then(Value::as_str) else {
            return;
        };
        let key = key_of(uuid);
        let stem = stem_of(object.get("type"));
        let end = object
            .get("end")
            .and_then(Value::as_str)
            .and_then(|text| Some((parse_instant(text)?, text.to_owned())));

        // Replaces the value in place when the sender already used the name.
        object.insert("receivedAt".to_owned(), Value::String(stamp.to_owned()));
        let from = self.lines.len();
        pyjson::write_object(&mut self.lines, &object);
        self.lines.push(b'\n');
        self.items.push(Item::Sample {
            key,
            stem,
            line: from..self.lines.len(),
            end,
        });
    }
}

/// The file a sample type is stored in. Types become file names, so anything
/// outside `[A-Za-z0-9_-]` turns into `_`. The tombstone file's name is
/// reserved: a sample there would be read back as a tombstone.
fn stem_of(sample_type: Option<&Value>) -> String {
    let Some(Value::String(text)) = sample_type else {
        return UNKNOWN.to_owned();
    };
    let stem: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() || stem == TOMBSTONES {
        UNKNOWN.to_owned()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: &str = "2026-10-03T22:53:10+00:00";

    fn parsed(body: &str) -> Batch {
        parse(body.as_bytes(), STAMP).unwrap()
    }

    fn line(batch: &Batch, n: usize) -> &str {
        let samples: Vec<&Range<usize>> = batch
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Sample { line, .. } => Some(line),
                Item::Tombstone(_) => None,
            })
            .collect();
        std::str::from_utf8(&batch.lines[samples[n].clone()]).unwrap()
    }

    #[test]
    fn a_sample_is_stored_as_given_with_received_at_appended() {
        let batch = parsed(
            r#"[{"uuid":"AAAA-1111","type":"heartRate","start":"s","end":"2026-07-06T08:00:00+01:00","value":58,"unit":"count/min"}]"#,
        );
        assert_eq!(batch.received, 1);
        assert_eq!(
            line(&batch, 0),
            "{\"uuid\":\"AAAA-1111\",\"type\":\"heartRate\",\"start\":\"s\",\"end\":\"2026-07-06T08:00:00+01:00\",\"value\":58,\"unit\":\"count/min\",\"receivedAt\":\"2026-10-03T22:53:10+00:00\"}\n"
        );
        let Item::Sample { stem, end, .. } = &batch.items[0] else {
            panic!()
        };
        assert_eq!(stem, "heartRate");
        assert_eq!(end.as_ref().unwrap().1, "2026-07-06T08:00:00+01:00");
    }

    #[test]
    fn a_received_at_sent_by_the_client_is_replaced_in_place() {
        let batch = parsed(r#"[{"uuid":"u","receivedAt":"old","type":"t"}]"#);
        assert_eq!(
            line(&batch, 0),
            "{\"uuid\":\"u\",\"receivedAt\":\"2026-10-03T22:53:10+00:00\",\"type\":\"t\"}\n"
        );
    }

    #[test]
    fn elements_that_are_not_samples_or_tombstones_are_counted_and_skipped() {
        let batch = parsed(
            r#"[1,"x",null,true,[1],{},{"type":"t"},{"uuid":5},{"uuid":null},{"uuid":"ok","type":"t"}]"#,
        );
        assert_eq!(batch.received, 10);
        assert_eq!(batch.items.len(), 1);
    }

    #[test]
    fn a_tombstone_keeps_its_string_uuids_in_order_with_repeats() {
        let batch = parsed(r#"[{"deleted":["a","a",5,"b",null]}]"#);
        let Item::Tombstone(uuids) = &batch.items[0] else {
            panic!()
        };
        assert_eq!(uuids, &["a", "a", "b"]);
    }

    #[test]
    fn a_tombstone_wins_over_a_uuid_in_the_same_element() {
        let batch = parsed(r#"[{"uuid":"u","deleted":["x"]}]"#);
        assert!(matches!(&batch.items[0], Item::Tombstone(uuids) if uuids == &["x"]));
    }

    #[test]
    fn a_deleted_that_is_not_a_list_is_a_tombstone_of_nothing() {
        let batch =
            parsed(r#"[{"deleted":"abc"},{"deleted":null},{"deleted":{"a":1}},{"deleted":[]}]"#);
        assert_eq!(batch.received, 4);
        assert!(
            batch
                .items
                .iter()
                .all(|item| matches!(item, Item::Tombstone(u) if u.is_empty()))
        );
    }

    #[test]
    fn types_become_safe_file_names() {
        let stem = |t: &str| stem_of(Some(&Value::String(t.to_owned())));
        assert_eq!(stem("sleepAnalysis"), "sleepAnalysis");
        assert_eq!(stem("a-b_c9"), "a-b_c9");
        assert_eq!(stem("../../etc/passwd"), "______etc_passwd");
        assert_eq!(stem("a b.c/d\\e\0f"), "a_b_c_d_e_f");
        assert_eq!(stem("é日本😀"), "____");
        assert_eq!(stem(""), "unknown");
        assert_eq!(stem("_deleted"), "unknown");
        assert_eq!(stem("_deleted2"), "_deleted2");
        assert_eq!(stem_of(None), "unknown");
        assert_eq!(stem_of(Some(&Value::Null)), "unknown");
        assert_eq!(stem_of(Some(&serde_json::json!(5))), "unknown");
        assert_eq!(stem_of(Some(&serde_json::json!(["a"]))), "unknown");
    }

    #[test]
    fn an_end_that_is_not_a_timestamp_with_a_zone_is_not_kept() {
        for end in [
            "null",
            "5",
            "\"x\"",
            "\"2026-07-06\"",
            "\"2026-07-06T09:00:00\"",
            "[]",
        ] {
            let batch = parsed(&format!(r#"[{{"uuid":"u","type":"t","end":{end}}}]"#));
            let Item::Sample { end, .. } = &batch.items[0] else {
                panic!()
            };
            assert!(end.is_none());
        }
        let batch = parsed(r#"[{"uuid":"u","type":"t"}]"#);
        assert!(matches!(&batch.items[0], Item::Sample { end: None, .. }));
    }

    #[test]
    fn a_bad_body_is_rejected_as_a_whole() {
        let body = r#"[{"uuid":"a","type":"t"},{"uuid":"b","type":"t"}, nope]"#;
        assert_eq!(
            parse(body.as_bytes(), STAMP).unwrap_err(),
            ParseError::BadJson
        );
        for bad in [
            "",
            "   ",
            "not json",
            "[1,2",
            "[1,2]x",
            "[1,2] [3]",
            "{\"a\":",
            "\u{feff}[]",
            "[\"\\ud800\"]",
        ] {
            assert_eq!(
                parse(bad.as_bytes(), STAMP).unwrap_err(),
                ParseError::BadJson,
                "{bad:?}"
            );
        }
        assert_eq!(
            parse(b"[\"\xff\"]", STAMP).unwrap_err(),
            ParseError::BadJson
        );
    }

    #[test]
    fn a_body_that_is_valid_json_but_not_an_array_is_told_apart() {
        for other in [
            r#"{"not":"array"}"#,
            "5",
            "\"text\"",
            "null",
            "true",
            "  {} ",
        ] {
            assert_eq!(
                parse(other.as_bytes(), STAMP).unwrap_err(),
                ParseError::NotAnArray,
                "{other}"
            );
        }
    }

    #[test]
    fn an_empty_array_is_a_valid_empty_batch() {
        let batch = parsed(" [ ] ");
        assert_eq!(
            (batch.received, batch.items.len(), batch.lines.len()),
            (0, 0, 0)
        );
    }

    #[test]
    fn nesting_beyond_the_parser_limit_is_a_bad_body() {
        let deep = format!("[{}{}]", "[".repeat(200), "]".repeat(200));
        assert_eq!(
            parse(deep.as_bytes(), STAMP).unwrap_err(),
            ParseError::BadJson
        );
        let fine = format!(
            "[{{\"uuid\":\"u\",\"metadata\":{}{}}}]",
            "[".repeat(100),
            "]".repeat(100)
        );
        assert_eq!(parsed(&fine).items.len(), 1);
    }

    #[test]
    fn a_deeply_nested_body_that_is_not_an_array_is_judged_without_recursion() {
        let nested = format!(
            "{{\"a\":{}{}}}",
            "[".repeat(1_000_000),
            "]".repeat(1_000_000)
        );
        assert_eq!(
            parse(nested.as_bytes(), STAMP).unwrap_err(),
            ParseError::NotAnArray
        );
        let broken = format!("{{\"a\":{}", "[".repeat(1_000_000));
        assert_eq!(
            parse(broken.as_bytes(), STAMP).unwrap_err(),
            ParseError::BadJson
        );
    }

    #[test]
    fn numbers_and_text_keep_the_shape_python_gives_them() {
        let batch = parsed(r#"[{"uuid":"u","a":1e5,"b":2.50,"c":-0.0,"d":"é\n","e":[1,2.0]}]"#);
        assert_eq!(
            line(&batch, 0),
            "{\"uuid\":\"u\",\"a\":100000.0,\"b\":2.5,\"c\":-0.0,\"d\":\"é\\n\",\"e\":[1,2.0],\"receivedAt\":\"2026-10-03T22:53:10+00:00\"}\n"
        );
    }
}
