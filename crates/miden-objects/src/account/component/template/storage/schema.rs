use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use miden_core::utils::{ByteReader, ByteWriter, Deserializable, Serializable};
use miden_processor::DeserializationError;

use super::InitStorageData;
use super::entry_content::{MapSchema, WordSchema};
use super::placeholder::{PlaceholderTypeRequirement, StorageValueName};
use super::schema_type::SchemaType;
use crate::Word;
use crate::account::{AccountStorage, StorageMap, StorageSlot, StorageSlotName};
use crate::errors::AccountComponentTemplateError;

/// Alias used for iterators that collect all placeholders and their types within a component
/// template.
pub type TemplateRequirementsIter<'a> =
    Box<dyn Iterator<Item = (StorageValueName, PlaceholderTypeRequirement)> + 'a>;

/// Describes the storage layout of an account component in terms of named storage slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountStorageSchema {
    fields: BTreeMap<StorageSlotName, StorageSlotSchema>,
}

impl AccountStorageSchema {
    /// Creates a new [`AccountStorageSchema`].
    ///
    /// # Errors
    /// - If `fields` contains duplicate slot names.
    /// - If `fields` contains the protocol-reserved faucet metadata slot name.
    pub fn new(
        fields: impl IntoIterator<Item = (StorageSlotName, StorageSlotSchema)>,
    ) -> Result<Self, AccountComponentTemplateError> {
        let mut map = BTreeMap::new();
        for (slot_name, schema) in fields {
            if slot_name.id() == AccountStorage::faucet_metadata_slot().id() {
                return Err(AccountComponentTemplateError::ReservedSlotName(slot_name));
            }

            if map.insert(slot_name.clone(), schema).is_some() {
                return Err(AccountComponentTemplateError::DuplicateSlotName(slot_name));
            }
        }

        Ok(Self { fields: map })
    }

    /// Returns an iterator over `(slot_name, schema)` pairs in slot-id order.
    pub fn iter(&self) -> impl Iterator<Item = (&StorageSlotName, &StorageSlotSchema)> {
        self.fields.iter()
    }

    /// Returns a reference to the underlying fields map.
    pub fn fields(&self) -> &BTreeMap<StorageSlotName, StorageSlotSchema> {
        &self.fields
    }

    /// Builds the initial [`StorageSlot`]s for this schema using the provided initialization data.
    pub fn build_storage_slots(
        &self,
        init_storage_data: &InitStorageData,
    ) -> Result<Vec<StorageSlot>, AccountComponentTemplateError> {
        self.fields
            .iter()
            .map(|(slot_name, schema)| schema.try_build_storage_slot(slot_name, init_storage_data))
            .collect()
    }

    /// Returns an iterator over placeholder requirements for the entire schema.
    pub fn template_requirements(&self) -> TemplateRequirementsIter<'_> {
        Box::new(
            self.fields
                .iter()
                .flat_map(|(slot_name, schema)| schema.template_requirements(slot_name)),
        )
    }

    pub(crate) fn validate(&self) -> Result<(), AccountComponentTemplateError> {
        for (slot_name, schema) in self.fields.iter() {
            if slot_name.id() == AccountStorage::faucet_metadata_slot().id() {
                return Err(AccountComponentTemplateError::ReservedSlotName(slot_name.clone()));
            }

            schema.validate(slot_name)?;
        }

        Ok(())
    }
}

impl Serializable for AccountStorageSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        target.write_u16(self.fields.len() as u16);
        for (slot_name, schema) in self.fields.iter() {
            target.write(slot_name);
            target.write(schema);
        }
    }
}

impl Deserializable for AccountStorageSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let num_entries = source.read_u16()? as usize;
        let mut fields = BTreeMap::new();

        for _ in 0..num_entries {
            let slot_name = StorageSlotName::read_from(source)?;
            let schema = StorageSlotSchema::read_from(source)?;

            if fields.insert(slot_name.clone(), schema).is_some() {
                return Err(DeserializationError::InvalidValue(format!(
                    "duplicate slot name in storage schema: {slot_name}",
                )));
            }
        }

        Ok(Self { fields })
    }
}

/// Describes the schema for a storage value slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueSlotSchema {
    description: Option<String>,
    word: WordSchema,
}

impl ValueSlotSchema {
    pub fn new(description: Option<String>, word: WordSchema) -> Self {
        Self { description, word }
    }

    pub fn description(&self) -> Option<&String> {
        self.description.as_ref()
    }

    pub fn schema_type(&self) -> SchemaType {
        self.word.schema_type()
    }

    pub fn word(&self) -> &WordSchema {
        &self.word
    }

    pub fn template_requirements(
        &self,
        slot_prefix: StorageValueName,
    ) -> TemplateRequirementsIter<'_> {
        self.word.template_requirements(slot_prefix)
    }

    pub fn try_build_word(
        &self,
        init_storage_data: &InitStorageData,
        placeholder_prefix: StorageValueName,
    ) -> Result<Word, AccountComponentTemplateError> {
        self.word.try_build_word(init_storage_data, placeholder_prefix)
    }

    pub fn default_value(&self) -> Option<Word> {
        if self.word.value().is_none() {
            return None;
        }

        if self.word.template_requirements(StorageValueName::empty()).next().is_some() {
            return None;
        }

        self.word
            .try_build_word(&InitStorageData::default(), StorageValueName::empty())
            .ok()
    }

    pub(crate) fn validate(
        &self,
        slot_name: &StorageSlotName,
    ) -> Result<(), AccountComponentTemplateError> {
        self.word.validate()?;
        if let WordSchema::Template { identifier, .. } = &self.word {
            if !identifier.name.as_str().is_empty() {
                return Err(AccountComponentTemplateError::InvalidSchema(format!(
                    "slot '{slot_name}' is a template; its placeholder name must be omitted (it is derived from the slot name)"
                )));
            }
        }

        Ok(())
    }
}

impl Serializable for ValueSlotSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        target.write(&self.description);
        target.write(&self.word);
    }
}

impl Deserializable for ValueSlotSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let description = Option::<String>::read_from(source)?;
        let word = WordSchema::read_from(source)?;
        Ok(ValueSlotSchema::new(description, word))
    }
}

/// Describes the schema for a storage map slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapSlotSchema {
    description: Option<String>,
    map: MapSchema,
    key_type: SchemaType,
    value_type: SchemaType,
}

impl MapSlotSchema {
    pub fn new(
        description: Option<String>,
        map: MapSchema,
        key_type: Option<SchemaType>,
        value_type: Option<SchemaType>,
    ) -> Self {
        let key_type = key_type.unwrap_or_else(|| map.key_schema_type());
        let value_type = value_type.unwrap_or_else(|| map.value_schema_type());
        Self { description, map, key_type, value_type }
    }

    pub fn description(&self) -> Option<&String> {
        self.description.as_ref()
    }

    pub fn map(&self) -> &MapSchema {
        &self.map
    }

    pub fn template_requirements(&self) -> TemplateRequirementsIter<'_> {
        self.map.template_requirements()
    }

    pub fn try_build_map(
        &self,
        init_storage_data: &InitStorageData,
    ) -> Result<StorageMap, AccountComponentTemplateError> {
        self.map.try_build_map(init_storage_data)
    }

    pub fn key_type(&self) -> &SchemaType {
        &self.key_type
    }

    pub fn value_type(&self) -> &SchemaType {
        &self.value_type
    }

    pub fn default_values(&self) -> Option<BTreeMap<Word, Word>> {
        self.map.default_values()
    }

    pub(crate) fn validate(&self) -> Result<(), AccountComponentTemplateError> {
        self.map.validate()
    }
}

impl Serializable for MapSlotSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        target.write(&self.description);
        target.write(&self.map);
        target.write(&self.key_type);
        target.write(&self.value_type);
    }
}

impl Deserializable for MapSlotSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let description = Option::<String>::read_from(source)?;
        let map = MapSchema::read_from(source)?;
        let key_type = SchemaType::read_from(source)?;
        let value_type = SchemaType::read_from(source)?;
        Ok(MapSlotSchema::new(description, map, Some(key_type), Some(value_type)))
    }
}

/// Describes the schema for a storage slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageSlotSchema {
    Value(ValueSlotSchema),
    Map(MapSlotSchema),
}

impl StorageSlotSchema {
    /// Returns placeholder requirements for this slot schema.
    pub fn template_requirements(
        &self,
        slot_name: &StorageSlotName,
    ) -> TemplateRequirementsIter<'_> {
        let slot_prefix = slot_name_to_placeholder_prefix(slot_name);
        match self {
            StorageSlotSchema::Value(slot) => slot.template_requirements(slot_prefix),
            StorageSlotSchema::Map(slot) => slot.template_requirements(),
        }
    }

    /// Builds a [`StorageSlot`] for the specified `slot_name` using the provided initialization
    /// data.
    pub fn try_build_storage_slot(
        &self,
        slot_name: &StorageSlotName,
        init_storage_data: &InitStorageData,
    ) -> Result<StorageSlot, AccountComponentTemplateError> {
        let slot_prefix = slot_name_to_placeholder_prefix(slot_name);
        match self {
            StorageSlotSchema::Value(slot) => {
                let word = slot.try_build_word(init_storage_data, slot_prefix)?;
                Ok(StorageSlot::with_value(slot_name.clone(), word))
            },
            StorageSlotSchema::Map(slot) => {
                let storage_map = slot.try_build_map(init_storage_data)?;
                Ok(StorageSlot::with_map(slot_name.clone(), storage_map))
            },
        }
    }

    pub(crate) fn validate(
        &self,
        slot_name: &StorageSlotName,
    ) -> Result<(), AccountComponentTemplateError> {
        match self {
            StorageSlotSchema::Value(slot) => slot.validate(slot_name)?,
            StorageSlotSchema::Map(slot) => slot.validate()?,
        }

        Ok(())
    }
}

impl Serializable for StorageSlotSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        match self {
            StorageSlotSchema::Value(slot) => {
                target.write_u8(0u8);
                slot.write_into(target);
            },
            StorageSlotSchema::Map(slot) => {
                target.write_u8(1u8);
                slot.write_into(target);
            },
        }
    }
}

impl Deserializable for StorageSlotSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let variant_tag = source.read_u8()?;
        match variant_tag {
            0 => Ok(StorageSlotSchema::Value(ValueSlotSchema::read_from(source)?)),
            1 => Ok(StorageSlotSchema::Map(MapSlotSchema::read_from(source)?)),
            _ => Err(DeserializationError::InvalidValue(format!(
                "unknown variant tag '{variant_tag}' for StorageSlotSchema"
            ))),
        }
    }
}

// HELPERS
// ================================================================================================

pub(crate) fn slot_name_to_placeholder_prefix(slot_name: &StorageSlotName) -> StorageValueName {
    let mapped = slot_name.as_str().replace("::", ".");
    StorageValueName::new(mapped)
        .expect("storage slot name components should be valid StorageValueName segments")
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;

    use super::*;
    use crate::account::component::template::storage::placeholder::{
        StorageValueName,
        TemplateTypeIdentifier,
    };
    use crate::account::component::template::storage::{
        FeltSchema,
        FieldIdentifier,
        MapEntrySchema,
    };
    use crate::{Felt, Word};

    #[test]
    fn value_slot_schema_default_value_returns_const_word() {
        let slot = ValueSlotSchema::new(
            Some("const value".into()),
            WordSchema::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]),
        );
        let expected = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
        assert_eq!(slot.default_value(), Some(expected));
    }

    #[test]
    fn map_slot_schema_default_values_returns_map() {
        let entry = MapEntrySchema::new(
            WordSchema::from([Felt::new(1), Felt::new(0), Felt::new(0), Felt::new(0)]),
            WordSchema::from([Felt::new(10), Felt::new(11), Felt::new(12), Felt::new(13)]),
        );
        let map_schema = MapSchema::new_value(vec![entry], StorageValueName::new("map").unwrap());
        let slot = MapSlotSchema::new(Some("static map".into()), map_schema, None, None);

        let mut expected = BTreeMap::new();
        expected.insert(
            Word::from([Felt::new(1), Felt::new(0), Felt::new(0), Felt::new(0)]),
            Word::from([Felt::new(10), Felt::new(11), Felt::new(12), Felt::new(13)]),
        );

        assert_eq!(slot.default_values(), Some(expected));
    }

    #[test]
    fn value_slot_schema_schema_type_returns_felt_types() {
        let felt_names = [
            StorageValueName::new("a").unwrap(),
            StorageValueName::new("b").unwrap(),
            StorageValueName::new("c").unwrap(),
            StorageValueName::new("d").unwrap(),
        ];

        let felt_values = [
            FeltSchema::new_template(
                TemplateTypeIdentifier::new("u8").unwrap(),
                felt_names[0].clone(),
            ),
            FeltSchema::new_template(
                TemplateTypeIdentifier::new("u16").unwrap(),
                felt_names[1].clone(),
            ),
            FeltSchema::new_template(
                TemplateTypeIdentifier::new("u32").unwrap(),
                felt_names[2].clone(),
            ),
            FeltSchema::new_template(
                TemplateTypeIdentifier::new("felt").unwrap(),
                felt_names[3].clone(),
            ),
        ];

        let slot = ValueSlotSchema::new(None, WordSchema::new_value(felt_values, None));
        let expected = SchemaType::Felts([
            TemplateTypeIdentifier::new("u8").unwrap(),
            TemplateTypeIdentifier::new("u16").unwrap(),
            TemplateTypeIdentifier::new("u32").unwrap(),
            TemplateTypeIdentifier::new("felt").unwrap(),
        ]);

        assert_eq!(slot.schema_type(), expected);
    }

    #[test]
    fn map_slot_schema_key_and_value_types() {
        let key_identifier = FieldIdentifier::with_name(StorageValueName::new("key").unwrap());
        let value_identifier = FieldIdentifier::with_name(StorageValueName::new("value").unwrap());

        let key_schema = WordSchema::Template {
            r#type: TemplateTypeIdentifier::new("sampling::Key").unwrap(),
            identifier: key_identifier,
        };

        let value_schema = WordSchema::new_value(
            [
                FeltSchema::new_value(Felt::new(100), None),
                FeltSchema::new_value(Felt::new(101), None),
                FeltSchema::new_value(Felt::new(102), None),
                FeltSchema::new_value(Felt::new(103), None),
            ],
            Some(value_identifier),
        );

        let entry = MapEntrySchema::new(key_schema, value_schema);
        let map_schema = MapSchema::new_value(vec![entry], StorageValueName::new("map").unwrap());
        let slot = MapSlotSchema::new(None, map_schema, None, None);

        assert_eq!(
            slot.key_type(),
            &SchemaType::Word(TemplateTypeIdentifier::new("sampling::Key").unwrap())
        );
        assert_eq!(
            slot.value_type(),
            &SchemaType::Felts([
                TemplateTypeIdentifier::new("felt").unwrap(),
                TemplateTypeIdentifier::new("felt").unwrap(),
                TemplateTypeIdentifier::new("felt").unwrap(),
                TemplateTypeIdentifier::new("felt").unwrap(),
            ])
        );
    }
}
