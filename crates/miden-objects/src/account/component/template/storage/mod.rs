use alloc::string::String;

use miden_core::utils::{ByteReader, ByteWriter, Deserializable, Serializable};
use miden_processor::DeserializationError;

mod schema_type;
pub use schema_type::SchemaType;

mod entry_content;
pub use entry_content::*;

mod schema;
pub(crate) use schema::slot_name_to_placeholder_prefix;
pub use schema::*;

mod placeholder;
pub use placeholder::{
    PlaceholderTypeRequirement,
    StorageValueName,
    StorageValueNameError,
    TemplateTypeError,
    TemplateTypeIdentifier,
};

mod init_storage_data;
pub use init_storage_data::InitStorageData;

#[cfg(feature = "std")]
pub mod toml;

// IDENTIFIER
// ================================================================================================

/// An identifier for a storage entry field.
///
/// An identifier consists of a name that identifies the field, and an optional description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldIdentifier {
    /// A human-readable identifier for the template.
    pub name: StorageValueName,
    /// An optional description explaining the purpose of this template.
    pub description: Option<String>,
}

impl FieldIdentifier {
    /// Creates a new `FieldIdentifier` with the given name and no description.
    pub fn with_name(name: StorageValueName) -> Self {
        Self { name, description: None }
    }

    /// Creates a new `FieldIdentifier` with the given name and description.
    pub fn with_description(name: StorageValueName, description: impl Into<String>) -> Self {
        Self {
            name,
            description: Some(description.into()),
        }
    }

    /// Returns the identifier name.
    pub fn name(&self) -> &StorageValueName {
        &self.name
    }

    /// Returns the identifier description.
    pub fn description(&self) -> Option<&String> {
        self.description.as_ref()
    }
}

impl From<StorageValueName> for FieldIdentifier {
    fn from(value: StorageValueName) -> Self {
        FieldIdentifier::with_name(value)
    }
}

impl Serializable for FieldIdentifier {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        target.write(&self.name);
        target.write(&self.description);
    }
}

impl Deserializable for FieldIdentifier {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let name = StorageValueName::read_from(source)?;
        let description = Option::<String>::read_from(source)?;
        Ok(FieldIdentifier { name, description })
    }
}
