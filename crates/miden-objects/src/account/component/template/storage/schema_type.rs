use alloc::fmt;

use miden_core::utils::{ByteReader, ByteWriter, Deserializable, Serializable};
use miden_processor::DeserializationError;

use super::placeholder::TemplateTypeIdentifier;

/// Describes the type of a storage schema entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaType {
    /// A whole-word schema type.
    Word(TemplateTypeIdentifier),
    /// A schema type that describes the type for each of the four felt slots.
    Felts([TemplateTypeIdentifier; 4]),
}

impl SchemaType {
    pub(crate) fn default_word() -> Self {
        SchemaType::Word(TemplateTypeIdentifier::native_word())
    }
}

impl Serializable for SchemaType {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        match self {
            SchemaType::Word(id) => {
                target.write_u8(0);
                target.write(id);
            },
            SchemaType::Felts(ids) => {
                target.write_u8(1);
                for id in ids.iter() {
                    target.write(id);
                }
            },
        }
    }
}

impl Deserializable for SchemaType {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let tag = source.read_u8()?;
        match tag {
            0 => {
                let id = TemplateTypeIdentifier::read_from(source)?;
                Ok(SchemaType::Word(id))
            },
            1 => {
                let mut ids = [
                    TemplateTypeIdentifier::native_felt(),
                    TemplateTypeIdentifier::native_felt(),
                    TemplateTypeIdentifier::native_felt(),
                    TemplateTypeIdentifier::native_felt(),
                ];
                for index in 0..4 {
                    ids[index] = TemplateTypeIdentifier::read_from(source)?;
                }
                Ok(SchemaType::Felts(ids))
            },
            other => Err(DeserializationError::InvalidValue(format!(
                "unknown schema type tag '{other}'"
            ))),
        }
    }
}

impl fmt::Display for SchemaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaType::Word(id) => write!(f, "word({id})"),
            SchemaType::Felts(ids) => {
                let mut iter = ids.iter();
                if let Some(first) = iter.next() {
                    write!(f, "felt({first}")?;
                    for next in iter {
                        write!(f, ", {next}")?;
                    }
                    write!(f, ")")
                } else {
                    f.write_str("felt()")
                }
            },
        }
    }
}
