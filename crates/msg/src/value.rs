//! A decoded message, and its text form in the style of `ros2 topic echo`.

use std::fmt::Write;

/// Arrays of numbers longer than this are cut short in the text, as an image or point cloud is unreadable anyway
const MAX_INLINE: usize = 32;

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float32(f32),
    Float64(f64),
    String(String),
    /// `uint8`, `byte` and `char` arrays, kept compact since they can be megabytes (images)
    Bytes(Vec<u8>),
    Array(Vec<Value>),
    Struct(Vec<(String, Value)>),
}

impl Value {
    /// The message as YAML-like text, one field per line, without the `---` that separates messages
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        match self {
            Value::Struct(fields) => write_fields(&mut out, fields, 0),
            scalar => out.push_str(&scalar_text(scalar).unwrap_or_default()),
        }
        out
    }

    /// The field at a path like `linear_acceleration.z` or `force[3]`
    pub fn get(&self, path: &str) -> Option<&Value> {
        path.split('.').try_fold(self, |value, part| {
            let (name, index) = match part.split_once('[') {
                Some((name, rest)) => (name, Some(rest.strip_suffix(']')?.parse::<usize>().ok()?)),
                None => (part, None),
            };
            let value = match value {
                Value::Struct(fields) => &fields.iter().find(|(n, _)| n == name)?.1,
                _ => return None,
            };
            match (index, value) {
                (None, value) => Some(value),
                (Some(i), Value::Array(items)) => items.get(i),
                (Some(_), _) => None,
            }
        })
    }

    pub fn as_f64(&self) -> Option<f64> {
        Some(match *self {
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Int(i) => i as f64,
            Value::UInt(u) => u as f64,
            Value::Float32(f) => f64::from(f),
            Value::Float64(f) => f,
            _ => return None,
        })
    }
}

fn write_fields(out: &mut String, fields: &[(String, Value)], indent: usize) {
    let pad = " ".repeat(indent);
    for (name, value) in fields {
        match value {
            Value::Struct(inner) if inner.is_empty() => _ = writeln!(out, "{pad}{name}: {{}}"),
            Value::Struct(inner) => {
                _ = writeln!(out, "{pad}{name}:");
                write_fields(out, inner, indent + 2);
            }
            Value::Array(items) if items.iter().any(|v| matches!(v, Value::Struct(_))) => {
                _ = writeln!(out, "{pad}{name}:");
                for item in items {
                    // A list of maps: the first field shares the line with the `-`
                    let mut block = String::new();
                    match item {
                        Value::Struct(inner) => write_fields(&mut block, inner, indent + 2),
                        other => _ = writeln!(block, "{pad}  {}", inline(other)),
                    }
                    out.push_str(&pad);
                    out.push_str("- ");
                    out.push_str(&block[indent + 2..]);
                }
            }
            other => _ = writeln!(out, "{pad}{name}: {}", inline(other)),
        }
    }
}

/// A value on one line: a scalar, or a list of scalars
fn inline(value: &Value) -> String {
    let list = |items: Vec<String>, total: usize| {
        let more = total.saturating_sub(MAX_INLINE);
        let more = if more > 0 { format!(", … {more} more") } else { String::new() };
        format!("[{}{more}]", items.join(", "))
    };
    match value {
        Value::Bytes(bytes) => list(bytes.iter().take(MAX_INLINE).map(u8::to_string).collect(), bytes.len()),
        Value::Array(items) => list(items.iter().take(MAX_INLINE).map(inline).collect(), items.len()),
        Value::Struct(_) => "{…}".to_owned(),
        scalar => scalar_text(scalar).unwrap_or_default(),
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    Some(match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        // Debug formatting keeps the `.0` of whole numbers and prints an f32 as the f32 it is
        Value::Float32(f) => format!("{f:?}"),
        Value::Float64(f) => format!("{f:?}"),
        Value::String(s) => quote(s),
        _ => return None,
    })
}

/// Plain when that reads unambiguously, quoted otherwise
fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.starts_with(|c: char| c.is_whitespace() || "'\"[]{}#&*!|>%@`-:,?".contains(c))
        && !s.ends_with(char::is_whitespace)
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.chars().any(char::is_control);
    if plain {
        s.to_owned()
    } else if s.chars().any(char::is_control) {
        format!("{s:?}")
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(fields: &[(&str, Value)]) -> Value {
        Value::Struct(fields.iter().map(|(n, v)| ((*n).to_owned(), v.clone())).collect())
    }

    #[test]
    fn formats_like_topic_echo() {
        let msg = s(&[
            (
                "header",
                s(&[
                    ("stamp", s(&[("sec", Value::Int(1)), ("nanosec", Value::UInt(5))])),
                    ("frame_id", Value::String("base".into())),
                ]),
            ),
            ("force", Value::Array(vec![Value::Float32(1.0), Value::Float32(0.1)])),
            ("ok", Value::Bool(true)),
            ("label", Value::String(String::new())),
        ]);
        assert_eq!(
            msg.to_text(),
            "header:\n  stamp:\n    sec: 1\n    nanosec: 5\n  frame_id: base\nforce: [1.0, 0.1]\nok: true\nlabel: ''\n"
        );
    }

    #[test]
    fn lists_of_structs_use_dashes() {
        let point = |x| s(&[("x", Value::Float64(x)), ("y", Value::Float64(0.0))]);
        let msg = s(&[("points", Value::Array(vec![point(1.0), point(2.0)]))]);
        assert_eq!(msg.to_text(), "points:\n- x: 1.0\n  y: 0.0\n- x: 2.0\n  y: 0.0\n");
    }

    #[test]
    fn long_arrays_are_cut_short() {
        let msg = s(&[("data", Value::Bytes(vec![7; 100]))]);
        assert!(msg.to_text().ends_with(", 7, … 68 more]\n"), "{}", msg.to_text());
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(quote("hello world"), "hello world");
        assert_eq!(quote("- dash"), "'- dash'");
        assert_eq!(quote("it's: here"), "'it''s: here'");
        assert_eq!(quote("two\nlines"), "\"two\\nlines\"");
    }

    #[test]
    fn gets_fields_by_path() {
        let msg = s(&[("accel", s(&[("z", Value::Float64(9.8))])), ("force", Value::Array(vec![Value::Float32(2.0)]))]);
        assert_eq!(msg.get("accel.z").and_then(Value::as_f64), Some(9.8));
        assert_eq!(msg.get("force[0]").and_then(Value::as_f64), Some(2.0));
        assert!(msg.get("accel.w").is_none());
    }
}
