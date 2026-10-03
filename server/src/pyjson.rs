//! JSON text as Python's `json.dumps(value, separators=(",", ":"),
//! ensure_ascii=False)` writes it. Lines already on disk were written that
//! way; matching it keeps new lines the same shape.

use std::io::Write;

use serde_json::{Map, Number, Value};

pub fn write_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => write_number(out, number),
        Value::String(text) => write_str(out, text),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => write_object(out, map),
    }
}

pub fn write_object(out: &mut Vec<u8>, map: &Map<String, Value>) {
    out.push(b'{');
    for (i, (key, value)) in map.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_str(out, key);
        out.push(b':');
        write_value(out, value);
    }
    out.push(b'}');
}

pub fn write_str(out: &mut Vec<u8>, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'"');
    let bytes = text.as_bytes();
    let mut copied = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        let short: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x08 => b"\\b",
            0x0c => b"\\f",
            0x00..=0x1f => &[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 15)],
            ],
            _ => continue,
        };
        out.extend_from_slice(&bytes[copied..i]);
        out.extend_from_slice(short);
        copied = i + 1;
    }
    out.extend_from_slice(&bytes[copied..]);
    out.push(b'"');
}

fn write_number(out: &mut Vec<u8>, number: &Number) {
    let written = if let Some(int) = number.as_i64() {
        write!(out, "{int}")
    } else if let Some(int) = number.as_u64() {
        write!(out, "{int}")
    } else {
        write_f64(out, number.as_f64().unwrap_or_default());
        Ok(())
    };
    written.expect("writing to a Vec cannot fail");
}

/// Python's `repr(float)`: the shortest digits that round-trip, in positional
/// notation unless the decimal exponent is below -4 or above 15.
fn write_f64(out: &mut Vec<u8>, value: f64) {
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("{:e} always has an exponent");
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |m| (true, m));
    let digits: Vec<u8> = mantissa.bytes().filter(|b| *b != b'.').collect();
    let point = exponent
        .parse::<i32>()
        .expect("{:e} exponent is an integer")
        + 1;
    let count = digits.len() as i32;

    if negative {
        out.push(b'-');
    }
    if point > 16 || point <= -4 {
        out.push(digits[0]);
        if count > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        write!(
            out,
            "e{}{:02}",
            if point < 1 { '-' } else { '+' },
            (point - 1).abs()
        )
        .expect("writing to a Vec cannot fail");
    } else if point <= 0 {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', point.unsigned_abs() as usize));
        out.extend_from_slice(&digits);
    } else if point >= count {
        out.extend_from_slice(&digits);
        out.extend(std::iter::repeat_n(b'0', (point - count) as usize));
        out.extend_from_slice(b".0");
    } else {
        out.extend_from_slice(&digits[..point as usize]);
        out.push(b'.');
        out.extend_from_slice(&digits[point as usize..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(json: &str) -> String {
        let value: Value = serde_json::from_str(json).unwrap();
        let mut out = Vec::new();
        write_value(&mut out, &value);
        String::from_utf8(out).unwrap()
    }

    /// Pairs of (JSON text in, text Python 3 writes for it), produced with
    /// `json.dumps(json.loads(text), separators=(",", ":"), ensure_ascii=False)`.
    #[test]
    fn matches_what_python_writes() {
        let cases: &[(&str, &str)] = &[
            ("58", "58"),
            ("-58", "-58"),
            ("0", "0"),
            ("58.0", "58.0"),
            ("58.5", "58.5"),
            ("-0.0", "-0.0"),
            ("0.1", "0.1"),
            ("0.30000000000000004", "0.30000000000000004"),
            ("1e5", "100000.0"),
            ("1E5", "100000.0"),
            ("1e-5", "1e-05"),
            ("1e-4", "0.0001"),
            ("0.0001", "0.0001"),
            ("0.00001", "1e-05"),
            ("5e-324", "5e-324"),
            ("1.7976931348623157e308", "1.7976931348623157e+308"),
            ("1e15", "1000000000000000.0"),
            ("1e16", "1e+16"),
            ("1e22", "1e+22"),
            ("123456789.123456789", "123456789.12345679"),
            ("2.50", "2.5"),
            ("1.5E+3", "1500.0"),
            ("4.35", "4.35"),
            ("100.0", "100.0"),
            ("1234567890123456.0", "1234567890123456.0"),
            ("12345678901234567.0", "1.2345678901234568e+16"),
            ("9007199254740993", "9007199254740993"),
            ("123456789012345678", "123456789012345678"),
            ("18446744073709551615", "18446744073709551615"),
            ("-9223372036854775808", "-9223372036854775808"),
            ("0.5", "0.5"),
            ("72.123456789012345", "72.12345678901235"),
            ("1.0e-10", "1e-10"),
            ("3.14159", "3.14159"),
            ("2.2250738585072014e-308", "2.2250738585072014e-308"),
            ("1e100", "1e+100"),
            ("1.5e300", "1.5e+300"),
            ("0.000123", "0.000123"),
            ("0.00012345678901234567", "0.00012345678901234567"),
            (
                "[1,2.5,\"a\",null,true,false]",
                "[1,2.5,\"a\",null,true,false]",
            ),
            (r#"{"a":{"b":[]},"c":{}}"#, r#"{"a":{"b":[]},"c":{}}"#),
            (
                r#"{"uuid":"A","receivedAt":"x","type":"t"}"#,
                r#"{"uuid":"A","receivedAt":"x","type":"t"}"#,
            ),
            (r#""plain""#, r#""plain""#),
            (
                r#""quote\" backslash\\ slash\/""#,
                r#""quote\" backslash\\ slash/""#,
            ),
            (
                r#""tab\t nl\n cr\r bs\b ff\f""#,
                r#""tab\t nl\n cr\r bs\b ff\f""#,
            ),
            (r#""\u0001\u001f\u007f""#, "\"\\u0001\\u001f\u{7f}\""),
            (r#""é日本😀""#, "\"é日本😀\""),
            (r#""\u00e9\u65e5\u672c\ud83d\ude00""#, "\"é日本😀\""),
            (r#""\u2028\u2029""#, "\"\u{2028}\u{2029}\""),
            ("\"\u{2028}\u{2029}\"", "\"\u{2028}\u{2029}\""),
        ];
        for (input, expected) in cases {
            assert_eq!(rewrite(input), *expected, "input {input}");
        }
    }

    #[test]
    fn keeps_key_order_and_replaces_a_repeated_key_in_place() {
        assert_eq!(rewrite(r#"{"z":1,"a":2,"m":3}"#), r#"{"z":1,"a":2,"m":3}"#);
        assert_eq!(rewrite(r#"{"a":1,"b":2,"a":3}"#), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn escapes_every_control_character() {
        let mut out = Vec::new();
        write_str(
            &mut out,
            "\u{0}\u{1}\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{1e}\u{1f} ~",
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#""\u0000\u0001\b\t\n\u000b\f\r\u001e\u001f ~""#
        );
    }
}
