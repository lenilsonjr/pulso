//! Reading what is already on disk: NDJSON lines, and the three fields the
//! index and `/latest` need from each.

use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// A stored line seen the way the Python server read it: a repeated key keeps
/// its last value, and a field of the wrong type counts as absent.
pub struct Fields<'a> {
    pub uuid: Option<Cow<'a, str>>,
    pub end: Option<Cow<'a, str>>,
    /// The string members of `deleted`.
    pub deleted: Vec<Cow<'a, str>>,
}

/// `None` for anything that is not one JSON object on its own.
pub fn fields(line: &[u8]) -> Option<Fields<'_>> {
    serde_json::from_slice(line).ok()
}

/// Calls `each` with every line of `path` from byte `from` on, without its
/// newline, and returns the offset the scan reached. A last line without a
/// newline counts as a line.
pub fn each_line(
    path: &Path,
    from: u64,
    mut each: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut line = Vec::new();
    let mut offset = from;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(offset);
        }
        offset += read as u64;
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        each(&line)?;
    }
}

impl<'de> Deserialize<'de> for Fields<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(FieldsVisitor)
    }
}

struct FieldsVisitor;

impl<'de> Visitor<'de> for FieldsVisitor {
    type Value = Fields<'de>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fields<'de>, A::Error> {
        let mut fields = Fields {
            uuid: None,
            end: None,
            deleted: Vec::new(),
        };
        while let Some(Name(name)) = map.next_key()? {
            match &*name {
                "uuid" => fields.uuid = map.next_value::<Shape>()?.into_text(),
                "end" => fields.end = map.next_value::<Shape>()?.into_text(),
                "deleted" => fields.deleted = map.next_value::<Shape>()?.into_texts(),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(fields)
    }
}

struct Name<'de>(Cow<'de, str>);

impl<'de> Deserialize<'de> for Name<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NameVisitor;
        impl<'de> Visitor<'de> for NameVisitor {
            type Value = Name<'de>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_borrowed_str<E>(self, text: &'de str) -> Result<Name<'de>, E> {
                Ok(Name(Cow::Borrowed(text)))
            }

            fn visit_str<E>(self, text: &str) -> Result<Name<'de>, E> {
                Ok(Name(Cow::Owned(text.to_owned())))
            }
        }
        deserializer.deserialize_str(NameVisitor)
    }
}

/// What a value is, to the extent the fields above care.
enum Shape<'de> {
    Text(Cow<'de, str>),
    /// An array, reduced to its string members.
    List(Vec<Cow<'de, str>>),
    Other,
}

impl<'de> Shape<'de> {
    fn into_text(self) -> Option<Cow<'de, str>> {
        match self {
            Shape::Text(text) => Some(text),
            _ => None,
        }
    }

    fn into_texts(self) -> Vec<Cow<'de, str>> {
        match self {
            Shape::List(texts) => texts,
            _ => Vec::new(),
        }
    }
}

impl<'de> Deserialize<'de> for Shape<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ShapeVisitor)
    }
}

struct ShapeVisitor;

impl<'de> Visitor<'de> for ShapeVisitor {
    type Value = Shape<'de>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_borrowed_str<E>(self, text: &'de str) -> Result<Shape<'de>, E> {
        Ok(Shape::Text(Cow::Borrowed(text)))
    }

    fn visit_str<E>(self, text: &str) -> Result<Shape<'de>, E> {
        Ok(Shape::Text(Cow::Owned(text.to_owned())))
    }

    fn visit_bool<E>(self, _: bool) -> Result<Shape<'de>, E> {
        Ok(Shape::Other)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Shape<'de>, E> {
        Ok(Shape::Other)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Shape<'de>, E> {
        Ok(Shape::Other)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Shape<'de>, E> {
        Ok(Shape::Other)
    }

    fn visit_unit<E>(self) -> Result<Shape<'de>, E> {
        Ok(Shape::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape<'de>, A::Error> {
        let mut texts = Vec::new();
        while let Some(item) = seq.next_element::<Shape>()? {
            if let Shape::Text(text) = item {
                texts.push(text);
            }
        }
        Ok(Shape::List(texts))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape<'de>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Shape::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid_and_end(line: &str) -> Option<(Option<String>, Option<String>)> {
        fields(line.as_bytes()).map(|f| (f.uuid.map(Cow::into_owned), f.end.map(Cow::into_owned)))
    }

    #[test]
    fn reads_the_fields_of_a_stored_sample() {
        let line = r#"{"uuid":"AAAA-1111","type":"sleepAnalysis","start":"s","end":"2026-07-06T02:40:00+01:00","value":"asleepREM","metadata":{"timeZone":"Europe/Lisbon","list":[1,{"x":null}]},"receivedAt":"r"}"#;
        assert_eq!(
            uuid_and_end(line),
            Some((
                Some("AAAA-1111".into()),
                Some("2026-07-06T02:40:00+01:00".into())
            ))
        );
    }

    #[test]
    fn a_repeated_key_keeps_its_last_value() {
        assert_eq!(
            uuid_and_end(r#"{"uuid":"a","uuid":"b"}"#),
            Some((Some("b".into()), None))
        );
        assert_eq!(uuid_and_end(r#"{"uuid":"a","uuid":5}"#), Some((None, None)));
    }

    #[test]
    fn a_field_of_the_wrong_type_counts_as_absent() {
        assert_eq!(uuid_and_end(r#"{"uuid":5,"end":null}"#), Some((None, None)));
        assert_eq!(
            uuid_and_end(r#"{"uuid":["a"],"end":{"x":1}}"#),
            Some((None, None))
        );
        assert_eq!(
            uuid_and_end(r#"{"uuid":true,"end":1.5}"#),
            Some((None, None))
        );
    }

    #[test]
    fn escaped_key_names_and_values_are_decoded() {
        assert_eq!(
            uuid_and_end(r#"{"uuid":"aé\n","end":"x"}"#),
            Some((Some("aé\n".into()), Some("x".into())))
        );
    }

    #[test]
    fn deleted_keeps_only_the_string_members() {
        let f =
            fields(br#"{"deleted":["a",5,"b",["c"],{"d":1},null,"e"],"receivedAt":"r"}"#).unwrap();
        assert_eq!(f.deleted, ["a", "b", "e"]);
        assert!(fields(br#"{"deleted":"abc"}"#).unwrap().deleted.is_empty());
        assert!(
            fields(br#"{"deleted":{"a":"b"}}"#)
                .unwrap()
                .deleted
                .is_empty()
        );
        assert!(fields(br#"{"deleted":[]}"#).unwrap().deleted.is_empty());
    }

    #[test]
    fn what_is_not_one_json_object_is_skipped() {
        for line in [
            "",
            "   ",
            "[1,2]",
            "\"text\"",
            "5",
            "null",
            "{",
            "{\"uuid\":\"a\"",
            "{\"uuid\":\"a\"} x",
            "{} {}",
            "not json",
        ] {
            assert!(fields(line.as_bytes()).is_none(), "{line:?}");
        }
        assert!(fields(b"{\"uuid\":\"a\xff\"}").is_none());
    }

    #[test]
    fn whitespace_around_the_object_is_fine() {
        assert_eq!(
            uuid_and_end("  {\"uuid\":\"a\"}\r"),
            Some((Some("a".into()), None))
        );
    }

    #[test]
    fn each_line_reads_from_an_offset_and_reports_where_it_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.ndjson");
        std::fs::write(&path, "one\ntwo\r\n\nlast without newline").unwrap();

        let mut lines = Vec::new();
        let end = each_line(&path, 0, |line| {
            lines.push(String::from_utf8(line.to_vec()).unwrap());
            Ok(())
        })
        .unwrap();
        assert_eq!(lines, ["one", "two\r", "", "last without newline"]);
        assert_eq!(end, 30);

        let mut rest = Vec::new();
        let end = each_line(&path, 4, |line| {
            rest.push(line.to_vec());
            Ok(())
        })
        .unwrap();
        assert_eq!(rest.len(), 3);
        assert_eq!(end, 30);

        let end = each_line(&path, 30, |_| panic!("nothing left to read")).unwrap();
        assert_eq!(end, 30);
    }

    #[test]
    fn an_error_from_the_callback_stops_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.ndjson");
        std::fs::write(&path, "a\nb\n").unwrap();
        let mut calls = 0;
        let result = each_line(&path, 0, |_| {
            calls += 1;
            Err(io::Error::other("stop"))
        });
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn deeply_nested_values_do_not_crash_the_scan() {
        let nested = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        // A value the scan skips is not recursed into.
        assert!(fields(format!("{{\"metadata\":{}}}", nested(100_000)).as_bytes()).is_some());
        // A value the scan reads is held to the parser's depth limit, and the line is skipped.
        assert!(fields(format!("{{\"uuid\":{}}}", nested(1000)).as_bytes()).is_none());
        assert!(fields(format!("{{\"deleted\":{}}}", nested(50)).as_bytes()).is_some());
    }
}
