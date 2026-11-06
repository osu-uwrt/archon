//! The `.msg` format: one field per line as `type name`, with optional array suffixes, bounds and default values, plus
//! constants (`type NAME=value`) and `#` comments.

use std::fmt;

use anyhow::{Context, anyhow, bail};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeName {
    pub package: String,
    pub name: String,
}

impl TypeName {
    /// Parses `pkg/msg/Type`, as the ROS graph reports types, or the shorter `pkg/Type` that `.msg` files use
    pub fn parse(text: &str) -> Option<TypeName> {
        let parts: Vec<&str> = text.split('/').collect();
        let (package, name) = match parts.as_slice() {
            [package, "msg", name] | [package, name] => (*package, *name),
            _ => return None,
        };
        let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        (valid(package) && valid(name)).then(|| TypeName { package: package.to_owned(), name: name.to_owned() })
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/msg/{}", self.package, self.name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Primitive {
    Bool,
    Byte,
    Char,
    Float32,
    Float64,
    Int8,
    UInt8,
    Int16,
    UInt16,
    Int32,
    UInt32,
    Int64,
    UInt64,
}

impl Primitive {
    fn parse(text: &str) -> Option<Primitive> {
        Some(match text {
            "bool" => Primitive::Bool,
            "byte" => Primitive::Byte,
            "char" => Primitive::Char,
            "float32" => Primitive::Float32,
            "float64" => Primitive::Float64,
            "int8" => Primitive::Int8,
            "uint8" => Primitive::UInt8,
            "int16" => Primitive::Int16,
            "uint16" => Primitive::UInt16,
            "int32" => Primitive::Int32,
            "uint32" => Primitive::UInt32,
            "int64" => Primitive::Int64,
            "uint64" => Primitive::UInt64,
            _ => return None,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Base {
    Primitive(Primitive),
    String,
    WString,
    Message(TypeName),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Array {
    Single,
    Fixed(usize),
    /// Bounded or not; a bound doesn't change how it's serialized
    Sequence,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Field {
    pub name: String,
    pub base: Base,
    pub array: Array,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MessageDef {
    pub name: TypeName,
    pub fields: Vec<Field>,
}

/// Parses the text of `<name.package>/msg/<name.name>.msg`
pub fn parse(name: &TypeName, text: &str) -> anyhow::Result<MessageDef> {
    let mut fields = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let field = parse_line(&name.package, line).with_context(|| format!("{name} line {}", number + 1))?;
        fields.extend(field);
    }
    Ok(MessageDef { name: name.clone(), fields })
}

/// Cuts a `#` comment, unless the `#` is inside a quoted default value
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (at, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') => return &line[..at],
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            _ => {}
        }
    }
    line
}

/// A field, or `None` for a constant
fn parse_line(package: &str, line: &str) -> anyhow::Result<Option<Field>> {
    let (ty, rest) = line.split_once(char::is_whitespace).ok_or_else(|| anyhow!("expected `type name` in {line:?}"))?;
    let rest = rest.trim_start();
    // A constant has `=` straight after its name; a default value is separated by whitespace
    let name_end = rest.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(rest.len());
    if rest[name_end..].trim_start().starts_with('=') {
        return Ok(None);
    }
    let name = &rest[..name_end];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("bad field name {name:?}");
    }

    let (base, array) = match ty.split_once('[') {
        None => (ty, Array::Single),
        Some((base, suffix)) => {
            let inside = suffix.strip_suffix(']').ok_or_else(|| anyhow!("unclosed `[` in {ty:?}"))?;
            let array = if inside.is_empty() || inside.starts_with("<=") {
                Array::Sequence
            } else {
                Array::Fixed(inside.parse().with_context(|| format!("array size in {ty:?}"))?)
            };
            (base, array)
        }
    };
    let base = if let Some(primitive) = Primitive::parse(base) {
        Base::Primitive(primitive)
    } else if base == "string" || base.starts_with("string<=") {
        Base::String
    } else if base == "wstring" || base.starts_with("wstring<=") {
        Base::WString
    } else if base == "Header" {
        Base::Message(TypeName { package: "std_msgs".to_owned(), name: "Header".to_owned() })
    } else if base.contains('/') {
        Base::Message(TypeName::parse(base).ok_or_else(|| anyhow!("bad type {base:?}"))?)
    } else {
        Base::Message(TypeName::parse(&format!("{package}/{base}")).ok_or_else(|| anyhow!("bad type {base:?}"))?)
    };
    Ok(Some(Field { name: name.to_owned(), base, array }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> TypeName {
        TypeName::parse(text).unwrap()
    }

    #[test]
    fn parses_type_names() {
        assert_eq!(name("std_msgs/msg/String"), name("std_msgs/String"));
        assert_eq!(name("std_msgs/String").to_string(), "std_msgs/msg/String");
        assert!(TypeName::parse("std_msgs/srv/Foo/Bar").is_none());
        assert!(TypeName::parse("nope").is_none());
    }

    #[test]
    fn parses_fields_arrays_and_nested_types() {
        let def = parse(
            &name("riptide_msgs2/Thrusters"),
            "# A comment line\n\
             Header header\n\
             float32[8] force   # per thruster\n\
             uint16[] pwm\n\
             int8[<=4] small\n\
             string<=16 label\n\
             ElectricalReadings readings\n\
             geometry_msgs/Vector3 offset\n\
             bool enabled true\n\
             int32 MODE_IDLE=0\n\
             int32 MODE_RUN = 1\n\
             string NAME=\"a # b\"\n",
        )
        .unwrap();
        let summary: Vec<(&str, &Base, Array)> =
            def.fields.iter().map(|f| (f.name.as_str(), &f.base, f.array)).collect();
        let msg = |t: &str| Base::Message(name(t));
        assert_eq!(
            summary,
            [
                ("header", &msg("std_msgs/Header"), Array::Single),
                ("force", &Base::Primitive(Primitive::Float32), Array::Fixed(8)),
                ("pwm", &Base::Primitive(Primitive::UInt16), Array::Sequence),
                ("small", &Base::Primitive(Primitive::Int8), Array::Sequence),
                ("label", &Base::String, Array::Single),
                ("readings", &msg("riptide_msgs2/ElectricalReadings"), Array::Single),
                ("offset", &msg("geometry_msgs/Vector3"), Array::Single),
                ("enabled", &Base::Primitive(Primitive::Bool), Array::Single),
            ]
        );
    }

    #[test]
    fn default_strings_may_contain_hashes() {
        let def = parse(&name("p/T"), "string s \"a#b\" # comment\n").unwrap();
        assert_eq!(def.fields.len(), 1);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&name("p/T"), "int32\n").is_err());
        assert!(parse(&name("p/T"), "int32[x] a\n").is_err());
    }
}
