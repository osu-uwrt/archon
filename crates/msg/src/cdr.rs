//! Decodes CDR, the serialization every ROS 2 RMW uses (Fast DDS, Cyclone, zenoh): a 4-byte encapsulation header
//! naming the byte order, then each field in order, aligned to its own size counted from the end of the header.

use anyhow::{Context, anyhow, bail};

use crate::registry::Registry;
use crate::schema::{Array, Base, Field, Primitive, TypeName};
use crate::value::Value;

pub fn decode(registry: &Registry, name: &TypeName, bytes: &[u8]) -> anyhow::Result<Value> {
    let (header, payload) = bytes.split_at_checked(4).ok_or_else(|| anyhow!("only {} bytes", bytes.len()))?;
    let little = match header[..2] {
        [0, 0] => false,
        [0, 1] => true,
        _ => bail!("unsupported CDR encapsulation {:02x}{:02x}", header[0], header[1]),
    };
    let mut reader = Reader { data: payload, at: 0, little };
    reader.message(registry, name)
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    little: bool,
}

impl Reader<'_> {
    fn message(&mut self, registry: &Registry, name: &TypeName) -> anyhow::Result<Value> {
        let def = registry.get(name).ok_or_else(|| anyhow!("{name} isn't loaded"))?;
        let mut fields = Vec::with_capacity(def.fields.len());
        for field in &def.fields {
            let value = self.field(registry, field).with_context(|| format!("field `{}` of {name}", field.name))?;
            fields.push((field.name.clone(), value));
        }
        Ok(Value::Struct(fields))
    }

    fn field(&mut self, registry: &Registry, field: &Field) -> anyhow::Result<Value> {
        let count = match field.array {
            Array::Single => return self.single(registry, &field.base),
            Array::Fixed(n) => n,
            Array::Sequence => self.u32()? as usize,
        };
        // Each element takes at least a byte, so a count beyond what's left is corrupt data, not a huge allocation
        if count > self.data.len() - self.at {
            bail!("array of {count} in {} bytes", self.data.len() - self.at);
        }
        if let Base::Primitive(Primitive::UInt8 | Primitive::Byte | Primitive::Char) = field.base {
            return Ok(Value::Bytes(self.take(count)?.to_vec()));
        }
        (0..count).map(|_| self.single(registry, &field.base)).collect::<anyhow::Result<_>>().map(Value::Array)
    }

    fn single(&mut self, registry: &Registry, base: &Base) -> anyhow::Result<Value> {
        Ok(match base {
            Base::Primitive(p) => self.primitive(*p)?,
            Base::String => {
                let len = self.u32()? as usize;
                let bytes = self.take(len)?;
                // The length counts a terminating NUL
                let text = bytes.strip_suffix(&[0]).unwrap_or(bytes);
                Value::String(String::from_utf8_lossy(text).into_owned())
            }
            Base::WString => {
                // Fast DDS sends a character count, then 4 bytes per character
                let len = self.u32()? as usize;
                let text = (0..len)
                    .map(|_| self.u32().map(|c| char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER)))
                    .collect::<anyhow::Result<String>>()?;
                Value::String(text.trim_end_matches('\0').to_owned())
            }
            Base::Message(name) => self.message(registry, name)?,
        })
    }

    fn primitive(&mut self, p: Primitive) -> anyhow::Result<Value> {
        Ok(match p {
            Primitive::Bool => Value::Bool(self.bytes::<1>()?[0] != 0),
            Primitive::Byte | Primitive::Char | Primitive::UInt8 => Value::UInt(u64::from(self.bytes::<1>()?[0])),
            Primitive::Int8 => Value::Int(i64::from(i8::from_ne_bytes(self.bytes()?))),
            Primitive::Int16 => Value::Int(i64::from(self.num(i16::from_le_bytes, i16::from_be_bytes)?)),
            Primitive::UInt16 => Value::UInt(u64::from(self.num(u16::from_le_bytes, u16::from_be_bytes)?)),
            Primitive::Int32 => Value::Int(i64::from(self.num(i32::from_le_bytes, i32::from_be_bytes)?)),
            Primitive::UInt32 => Value::UInt(u64::from(self.u32()?)),
            Primitive::Int64 => Value::Int(self.num(i64::from_le_bytes, i64::from_be_bytes)?),
            Primitive::UInt64 => Value::UInt(self.num(u64::from_le_bytes, u64::from_be_bytes)?),
            Primitive::Float32 => Value::Float32(self.num(f32::from_le_bytes, f32::from_be_bytes)?),
            Primitive::Float64 => Value::Float64(self.num(f64::from_le_bytes, f64::from_be_bytes)?),
        })
    }

    fn u32(&mut self) -> anyhow::Result<u32> {
        self.num(u32::from_le_bytes, u32::from_be_bytes)
    }

    /// Reads an `N`-byte number aligned to `N`
    fn num<const N: usize, T>(&mut self, le: fn([u8; N]) -> T, be: fn([u8; N]) -> T) -> anyhow::Result<T> {
        self.at = self.at.next_multiple_of(N);
        let bytes = self.bytes::<N>()?;
        Ok(if self.little { le(bytes) } else { be(bytes) })
    }

    fn bytes<const N: usize>(&mut self) -> anyhow::Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("take returns N bytes"))
    }

    fn take(&mut self, n: usize) -> anyhow::Result<&[u8]> {
        let end = self.at.checked_add(n).filter(|&end| end <= self.data.len());
        let end = end.ok_or_else(|| anyhow!("message ends early, at byte {} of {}", self.data.len(), self.at + n))?;
        let bytes = &self.data[self.at..end];
        self.at = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::FakeShare;

    /// Writes little-endian CDR the way an RMW does, to check the decoder against
    #[derive(Default)]
    struct Writer(Vec<u8>);

    impl Writer {
        fn align(&mut self, n: usize) -> &mut Self {
            while !self.0.len().is_multiple_of(n) {
                self.0.push(0);
            }
            self
        }
        fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.align(4).0.extend(v.to_le_bytes());
            self
        }
        fn i32(&mut self, v: i32) -> &mut Self {
            self.align(4).0.extend(v.to_le_bytes());
            self
        }
        fn f32(&mut self, v: f32) -> &mut Self {
            self.align(4).0.extend(v.to_le_bytes());
            self
        }
        fn f64(&mut self, v: f64) -> &mut Self {
            self.align(8).0.extend(v.to_le_bytes());
            self
        }
        fn string(&mut self, s: &str) -> &mut Self {
            self.u32(s.len() as u32 + 1);
            self.0.extend(s.as_bytes());
            self.u8(0)
        }
        fn finish(&self) -> Vec<u8> {
            [vec![0, 1, 0, 0], self.0.clone()].concat()
        }
    }

    fn share() -> FakeShare {
        FakeShare::new(&[
            ("std_msgs/String", "string data\n"),
            ("std_msgs/Header", "builtin_interfaces/Time stamp\nstring frame_id\n"),
            ("builtin_interfaces/Time", "int32 sec\nuint32 nanosec\n"),
            ("demo/Reading", "Header header\nfloat64 value\nfloat32[3] axes\nuint8[] raw\nbool ok\n"),
            ("demo/Points", "demo/Point[] points\n"),
            ("demo/Point", "float32 x\nint8 y\n"),
        ])
    }

    fn decode_as(share: &FakeShare, ty: &str, bytes: &[u8]) -> anyhow::Result<Value> {
        let mut registry = share.registry();
        let name = TypeName::parse(ty).unwrap();
        registry.load(&name)?;
        decode(&registry, &name, bytes)
    }

    #[test]
    fn decodes_a_string() {
        let bytes = Writer::default().string("hi").finish();
        assert_eq!(bytes, [0, 1, 0, 0, 3, 0, 0, 0, b'h', b'i', 0]);
        let value = decode_as(&share(), "std_msgs/msg/String", &bytes).unwrap();
        assert_eq!(value.to_text(), "data: hi\n");
    }

    #[test]
    fn aligns_after_strings() {
        // "base" ends at byte 17 of the payload, so the float64 after it is padded to byte 24
        let bytes = Writer::default()
            .i32(12)
            .u32(34)
            .string("base")
            .f64(9.81)
            .f32(1.0)
            .f32(2.0)
            .f32(3.0)
            .u32(2)
            .u8(7)
            .u8(8)
            .u8(1)
            .finish();
        let value = decode_as(&share(), "demo/Reading", &bytes).unwrap();
        assert_eq!(
            value.to_text(),
            "header:\n  stamp:\n    sec: 12\n    nanosec: 34\n  frame_id: base\nvalue: 9.81\naxes: [1.0, 2.0, 3.0]\nraw: [7, 8]\nok: true\n"
        );
    }

    #[test]
    fn decodes_sequences_of_messages() {
        let bytes = Writer::default().u32(2).f32(1.5).u8(255).f32(2.5).u8(1).finish();
        let value = decode_as(&share(), "demo/Points", &bytes).unwrap();
        assert_eq!(value.to_text(), "points:\n- x: 1.5\n  y: -1\n- x: 2.5\n  y: 1\n");
    }

    #[test]
    fn big_endian() {
        let bytes = [0, 0, 0, 0, 0, 0, 0, 3, b'h', b'i', 0];
        assert_eq!(decode_as(&share(), "std_msgs/String", &bytes).unwrap().to_text(), "data: hi\n");
    }

    #[test]
    fn truncated_messages_are_errors_not_panics() {
        let bytes = Writer::default().string("hello").finish();
        let err = decode_as(&share(), "std_msgs/String", &bytes[..8]).unwrap_err();
        assert!(format!("{err:#}").contains("ends early"), "{err:#}");
        let huge_count = Writer::default().u32(u32::MAX).finish();
        assert!(decode_as(&share(), "demo/Points", &huge_count).is_err());
    }
}
