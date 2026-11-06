//! Building messages to publish: a type's default message, setting a field from the text someone typed, and
//! encoding to CDR.

use anyhow::{Context, anyhow, bail};

use crate::registry::Registry;
use crate::schema::{Array, Base, Field, Primitive, TypeName};
use crate::value::Value;

/// A message of type `name` with every field zero, empty or false, as a new ROS message is
pub fn default_value(registry: &Registry, name: &TypeName) -> anyhow::Result<Value> {
    let def = registry.get(name).ok_or_else(|| anyhow!("{name} isn't loaded"))?;
    let fields = def
        .fields
        .iter()
        .map(|field| Ok((field.name.clone(), default_field(registry, field)?)))
        .collect::<anyhow::Result<_>>()?;
    Ok(Value::Struct(fields))
}

fn default_field(registry: &Registry, field: &Field) -> anyhow::Result<Value> {
    let count = match field.array {
        Array::Single => return default_single(registry, &field.base),
        Array::Fixed(n) => n,
        Array::Sequence => 0,
    };
    if is_bytes(&field.base) {
        return Ok(Value::Bytes(vec![0; count]));
    }
    (0..count).map(|_| default_single(registry, &field.base)).collect::<anyhow::Result<_>>().map(Value::Array)
}

fn default_single(registry: &Registry, base: &Base) -> anyhow::Result<Value> {
    Ok(match base {
        Base::Primitive(p) => match p {
            Primitive::Bool => Value::Bool(false),
            Primitive::Float32 => Value::Float32(0.0),
            Primitive::Float64 => Value::Float64(0.0),
            Primitive::Int8 | Primitive::Int16 | Primitive::Int32 | Primitive::Int64 => Value::Int(0),
            _ => Value::UInt(0),
        },
        Base::String | Base::WString => Value::String(String::new()),
        Base::Message(name) => default_value(registry, name)?,
    })
}

fn is_bytes(base: &Base) -> bool {
    matches!(base, Base::Primitive(Primitive::UInt8 | Primitive::Byte | Primitive::Char))
}

/// Sets the field at `path` (like `linear.x` or `force[2]`) of a message of type `name` from text: a number, `true`,
/// a string, or `[1, 2, 3]` for a whole array of numbers
pub fn set_field(registry: &Registry, name: &TypeName, root: &mut Value, path: &str, text: &str) -> anyhow::Result<()> {
    let mut type_name = name.clone();
    let mut value = root;
    let parts: Vec<&str> = path.split('.').collect();
    for (i, part) in parts.iter().enumerate() {
        let (field_name, index) = match part.split_once('[') {
            Some((n, rest)) => {
                let index = rest.strip_suffix(']').and_then(|i| i.parse::<usize>().ok());
                (n, Some(index.ok_or_else(|| anyhow!("bad index in {part:?}"))?))
            }
            None => (*part, None),
        };
        let def = registry.get(&type_name).ok_or_else(|| anyhow!("{type_name} isn't loaded"))?;
        let field = def
            .fields
            .iter()
            .find(|f| f.name == field_name)
            .ok_or_else(|| anyhow!("{type_name} has no field {field_name:?}"))?;
        let Value::Struct(fields) = value else { bail!("{path}: not a message") };
        let slot = &mut fields.iter_mut().find(|(n, _)| n == field_name).expect("the value matches its type").1;
        let last = i == parts.len() - 1;
        match (index, last) {
            (None, true) => {
                *slot = parse_field(field, text).with_context(|| format!("{path}: {text:?}"))?;
                return Ok(());
            }
            (Some(at), _) => {
                let element = match slot {
                    Value::Array(items) => items.get_mut(at),
                    Value::Bytes(bytes) if last => {
                        let byte = bytes.get_mut(at).ok_or_else(|| anyhow!("{path}: no element {at}"))?;
                        *byte = parse_int(text, 0, 255).with_context(|| format!("{path}: {text:?}"))? as u8;
                        return Ok(());
                    }
                    _ => None,
                };
                let element = element.ok_or_else(|| anyhow!("{path}: no element {at}"))?;
                if last {
                    *element = parse_single(&field.base, text).with_context(|| format!("{path}: {text:?}"))?;
                    return Ok(());
                }
                value = element;
            }
            (None, false) => value = slot,
        }
        if let Base::Message(nested) = &field.base {
            type_name = nested.clone();
        }
    }
    Ok(())
}

/// A whole field from text: `[1, 2]` for an array, else one value
fn parse_field(field: &Field, text: &str) -> anyhow::Result<Value> {
    if field.array == Array::Single {
        return parse_single(&field.base, text);
    }
    let inner = text.trim().strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or(text.trim());
    let items: Vec<&str> = if inner.trim().is_empty() { Vec::new() } else { inner.split(',').map(str::trim).collect() };
    if let Array::Fixed(n) = field.array
        && items.len() != n
    {
        bail!("needs exactly {n} values, got {}", items.len());
    }
    if is_bytes(&field.base) {
        return items
            .iter()
            .map(|t| parse_int(t, 0, 255).map(|v| v as u8))
            .collect::<anyhow::Result<_>>()
            .map(Value::Bytes);
    }
    items.iter().map(|t| parse_single(&field.base, t)).collect::<anyhow::Result<_>>().map(Value::Array)
}

fn parse_single(base: &Base, text: &str) -> anyhow::Result<Value> {
    let text = text.trim();
    Ok(match base {
        Base::Primitive(p) => parse_primitive(*p, text)?,
        Base::String | Base::WString => {
            // Quotes are optional, for when the text came from a quoted display
            let unquoted = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(text);
            Value::String(unquoted.to_owned())
        }
        Base::Message(name) => bail!("{name} is a message; set its fields instead"),
    })
}

fn parse_int(text: &str, min: i128, max: i128) -> anyhow::Result<i128> {
    let text = text.trim();
    let n = match text.strip_prefix("0x") {
        Some(hex) => i128::from_str_radix(hex, 16)?,
        None => text.parse::<i128>().map_err(|_| anyhow!("not a whole number"))?,
    };
    if n < min || n > max {
        bail!("out of range {min}..={max}");
    }
    Ok(n)
}

pub fn parse_primitive(p: Primitive, text: &str) -> anyhow::Result<Value> {
    Ok(match p {
        Primitive::Bool => match text.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Value::Bool(true),
            "false" | "0" | "off" | "no" => Value::Bool(false),
            _ => bail!("not true or false"),
        },
        Primitive::Float32 => Value::Float32(text.parse().map_err(|_| anyhow!("not a number"))?),
        Primitive::Float64 => Value::Float64(text.parse().map_err(|_| anyhow!("not a number"))?),
        Primitive::Int8 => Value::Int(parse_int(text, i8::MIN.into(), i8::MAX.into())? as i64),
        Primitive::Int16 => Value::Int(parse_int(text, i16::MIN.into(), i16::MAX.into())? as i64),
        Primitive::Int32 => Value::Int(parse_int(text, i32::MIN.into(), i32::MAX.into())? as i64),
        Primitive::Int64 => Value::Int(parse_int(text, i64::MIN.into(), i64::MAX.into())? as i64),
        Primitive::UInt8 | Primitive::Byte | Primitive::Char => Value::UInt(parse_int(text, 0, u8::MAX.into())? as u64),
        Primitive::UInt16 => Value::UInt(parse_int(text, 0, u16::MAX.into())? as u64),
        Primitive::UInt32 => Value::UInt(parse_int(text, 0, u32::MAX.into())? as u64),
        Primitive::UInt64 => Value::UInt(parse_int(text, 0, u64::MAX.into())? as u64),
    })
}

/// Encodes a message as little-endian CDR, the way an RMW sends it
pub fn encode(registry: &Registry, name: &TypeName, value: &Value) -> anyhow::Result<Vec<u8>> {
    let mut writer = Writer { out: Vec::new() };
    writer.message(registry, name, value)?;
    Ok([vec![0, 1, 0, 0], writer.out].concat())
}

struct Writer {
    /// The payload, after the encapsulation header; alignment counts from its start
    out: Vec<u8>,
}

impl Writer {
    fn align(&mut self, n: usize) {
        let padded = self.out.len().next_multiple_of(n);
        self.out.resize(padded, 0);
    }

    fn put(&mut self, bytes: &[u8]) {
        self.align(bytes.len());
        self.out.extend_from_slice(bytes);
    }

    fn message(&mut self, registry: &Registry, name: &TypeName, value: &Value) -> anyhow::Result<()> {
        let def = registry.get(name).ok_or_else(|| anyhow!("{name} isn't loaded"))?;
        let Value::Struct(fields) = value else { bail!("{name} needs a message value") };
        for field in &def.fields {
            let v = fields
                .iter()
                .find(|(n, _)| *n == field.name)
                .map(|(_, v)| v)
                .ok_or_else(|| anyhow!("missing field {}", field.name))?;
            self.field(registry, field, v).with_context(|| format!("field `{}` of {name}", field.name))?;
        }
        Ok(())
    }

    fn field(&mut self, registry: &Registry, field: &Field, value: &Value) -> anyhow::Result<()> {
        if field.array == Array::Single {
            return self.single(registry, &field.base, value);
        }
        let count = match value {
            Value::Bytes(b) => b.len(),
            Value::Array(items) => items.len(),
            _ => bail!("needs an array"),
        };
        match field.array {
            Array::Fixed(n) if n != count => bail!("needs exactly {n} elements, has {count}"),
            Array::Sequence => self.put(&(count as u32).to_le_bytes()),
            _ => {}
        }
        match value {
            Value::Bytes(bytes) => self.out.extend_from_slice(bytes),
            Value::Array(items) => items.iter().try_for_each(|v| self.single(registry, &field.base, v))?,
            _ => unreachable!("checked above"),
        }
        Ok(())
    }

    fn single(&mut self, registry: &Registry, base: &Base, value: &Value) -> anyhow::Result<()> {
        match (base, value) {
            (Base::Message(name), v) => self.message(registry, name, v)?,
            (Base::String, Value::String(s)) => {
                self.put(&(s.len() as u32 + 1).to_le_bytes());
                self.out.extend_from_slice(s.as_bytes());
                self.out.push(0);
            }
            (Base::WString, Value::String(s)) => {
                let chars: Vec<char> = s.chars().collect();
                self.put(&(chars.len() as u32).to_le_bytes());
                chars.iter().for_each(|c| self.put(&(*c as u32).to_le_bytes()));
            }
            (Base::Primitive(p), v) => {
                let number = v.as_f64().ok_or_else(|| anyhow!("needs a number"))?;
                let int = |v: &Value| match *v {
                    Value::Int(i) => i,
                    Value::UInt(u) => u as i64,
                    Value::Bool(b) => i64::from(b),
                    _ => number as i64,
                };
                match p {
                    Primitive::Bool => self.out.push(u8::from(number != 0.0)),
                    Primitive::Byte | Primitive::Char | Primitive::UInt8 | Primitive::Int8 => {
                        self.out.push(int(v) as u8)
                    }
                    Primitive::Int16 | Primitive::UInt16 => self.put(&(int(v) as u16).to_le_bytes()),
                    Primitive::Int32 | Primitive::UInt32 => self.put(&(int(v) as u32).to_le_bytes()),
                    Primitive::Int64 => self.put(&int(v).to_le_bytes()),
                    Primitive::UInt64 => self.put(
                        &(match v {
                            Value::UInt(u) => *u,
                            _ => int(v) as u64,
                        })
                        .to_le_bytes(),
                    ),
                    Primitive::Float32 => self.put(&(number as f32).to_le_bytes()),
                    Primitive::Float64 => self.put(&number.to_le_bytes()),
                }
            }
            (base, _) => bail!("value doesn't fit a {base:?}"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr::decode;
    use crate::registry::tests::FakeShare;

    fn share() -> FakeShare {
        FakeShare::new(&[
            ("std_msgs/Header", "builtin_interfaces/Time stamp\nstring frame_id\n"),
            ("builtin_interfaces/Time", "int32 sec\nuint32 nanosec\n"),
            (
                "demo/Command",
                "Header header\nfloat64 speed\nint8 mode\nbool armed\nfloat32[3] axes\nuint8[] raw\ndemo/Point[] points\n",
            ),
            ("demo/Point", "float32 x\nint16 y\n"),
        ])
    }

    fn setup(share: &FakeShare) -> (Registry, TypeName) {
        let mut registry = share.registry();
        let name = TypeName::parse("demo/Command").unwrap();
        registry.load(&name).unwrap();
        (registry, name)
    }

    #[test]
    fn defaults_are_zero_and_empty() {
        let share = share();
        let (registry, name) = setup(&share);
        let value = default_value(&registry, &name).unwrap();
        assert_eq!(value.get("axes"), Some(&Value::Array(vec![Value::Float32(0.0); 3])));
        assert_eq!(value.get("raw"), Some(&Value::Bytes(Vec::new())));
        assert_eq!(value.get("header.frame_id"), Some(&Value::String(String::new())));
    }

    #[test]
    fn sets_fields_from_text_and_round_trips() {
        let share = share();
        let (registry, name) = setup(&share);
        let mut value = default_value(&registry, &name).unwrap();
        for (path, text) in [
            ("header.stamp.sec", "12"),
            ("header.frame_id", "\"base\""),
            ("speed", "0.5"),
            ("mode", "-3"),
            ("armed", "true"),
            ("axes", "[1, 2.5, -1]"),
            ("axes[1]", "4"),
            ("raw", "[1, 0xff]"),
        ] {
            set_field(&registry, &name, &mut value, path, text).unwrap();
        }
        let bytes = encode(&registry, &name, &value).unwrap();
        let back = decode(&registry, &name, &bytes).unwrap();
        assert_eq!(back, value);
        assert_eq!(back.get("axes[1]").and_then(Value::as_f64), Some(4.0));
        assert_eq!(back.get("header.frame_id"), Some(&Value::String("base".into())));
    }

    #[test]
    fn rejects_text_that_doesnt_fit() {
        let share = share();
        let (registry, name) = setup(&share);
        let mut value = default_value(&registry, &name).unwrap();
        let err = |path, text| set_field(&registry, &name, &mut value.clone(), path, text).unwrap_err().to_string();
        assert!(err("mode", "200").contains("mode"));
        assert!(err("armed", "maybe").contains("armed"));
        assert!(err("axes", "[1, 2]").contains("axes"));
        assert!(err("nope", "1").contains("no field"));
        assert!(set_field(&registry, &name, &mut value, "mode", "-128").is_ok());
    }

    #[test]
    fn encodes_sequences_of_messages() {
        let share = share();
        let (registry, name) = setup(&share);
        let mut value = default_value(&registry, &name).unwrap();
        let point = |x, y| Value::Struct(vec![("x".into(), Value::Float32(x)), ("y".into(), Value::Int(y))]);
        if let Value::Struct(fields) = &mut value {
            fields.iter_mut().find(|(n, _)| n == "points").unwrap().1 =
                Value::Array(vec![point(1.5, -2), point(3.0, 7)]);
        }
        let bytes = encode(&registry, &name, &value).unwrap();
        assert_eq!(decode(&registry, &name, &bytes).unwrap(), value);
    }
}
