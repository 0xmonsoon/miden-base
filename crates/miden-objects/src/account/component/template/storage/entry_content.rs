use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::array::from_fn;
use core::iter;

use super::placeholder::{PlaceholderTypeRequirement, TEMPLATE_REGISTRY, TemplateTypeIdentifier};
use super::schema_type::SchemaType;
use super::{FieldIdentifier, InitStorageData, StorageValueName, TemplateRequirementsIter};
use crate::account::StorageMap;
use crate::account::component::template::AccountComponentTemplateError;
use crate::utils::{ByteReader, ByteWriter, Deserializable, DeserializationError, Serializable};
use crate::{Felt, FieldElement, Word};

// WORDS
// ================================================================================================

/// Defines how a word is represented within the component's storage description.
///
/// Each word representation can be:
/// - A template that defines a type but does not carry a value.
/// - A predefined value that may contain a hardcoded word or a mix of fixed and templated felts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum WordSchema {
    /// A templated value that serves as a placeholder for instantiation.
    ///
    /// This variant defines a type but does not store a value. The actual value is provided at the
    /// time of instantiation. The name is required to identify this template externally.
    Template {
        /// The type associated with this templated word.
        r#type: TemplateTypeIdentifier,
        identifier: FieldIdentifier,
    },

    /// A predefined value that can be used directly within storage.
    ///
    /// This variant may contain either a fully hardcoded word or a structured set of felts, some
    /// of which may themselves be templates.
    Value {
        identifier: Option<FieldIdentifier>,
        /// The 4-felt representation of the stored word.
        value: [FeltSchema; 4],
    },
}

impl WordSchema {
    /// Constructs a new `Template` variant.
    pub fn new_template(r#type: TemplateTypeIdentifier, identifier: FieldIdentifier) -> Self {
        WordSchema::Template { r#type, identifier }
    }

    /// Constructs a new `Value` variant.
    pub fn new_value(
        value: impl Into<[FeltSchema; 4]>,
        identifier: Option<FieldIdentifier>,
    ) -> Self {
        WordSchema::Value { identifier, value: value.into() }
    }

    /// Sets the description of the [`WordSchema`] and returns `self`.
    pub fn with_description(self, description: impl Into<String>) -> Self {
        match self {
            WordSchema::Template { r#type, identifier } => WordSchema::Template {
                r#type,
                identifier: FieldIdentifier {
                    name: identifier.name,
                    description: Some(description.into()),
                },
            },
            WordSchema::Value { identifier, value } => WordSchema::Value {
                identifier: identifier.map(|id| FieldIdentifier {
                    name: id.name,
                    description: Some(description.into()),
                }),
                value,
            },
        }
    }

    /// Returns the name associated with the word representation.
    /// - For the `Template` variant, it always returns a reference to the name.
    /// - For the `Value` variant, it returns `Some` if a name is present, or `None` otherwise.
    pub fn name(&self) -> Option<&StorageValueName> {
        match self {
            WordSchema::Template { identifier, .. } => Some(&identifier.name),
            WordSchema::Value { identifier, .. } => identifier.as_ref().map(|id| &id.name),
        }
    }

    /// Returns the description associated with the word representation.
    /// Both variants store an `Option<String>`, which is converted to an `Option<&str>`.
    pub fn description(&self) -> Option<&str> {
        match self {
            WordSchema::Template { identifier, .. } => identifier.description.as_deref(),
            WordSchema::Value { identifier, .. } => {
                identifier.as_ref().and_then(|id| id.description.as_deref())
            },
        }
    }

    /// Returns the type name.
    pub fn word_type(&self) -> TemplateTypeIdentifier {
        match self {
            WordSchema::Template { r#type, .. } => r#type.clone(),
            WordSchema::Value { .. } => TemplateTypeIdentifier::native_word(),
        }
    }

    /// Returns the schema type that describes how this word should be instantiated.
    pub fn schema_type(&self) -> SchemaType {
        match self {
            WordSchema::Template { r#type, .. } => SchemaType::Word(r#type.clone()),
            WordSchema::Value { value, .. } => {
                let types = from_fn(|index| value[index].felt_type());
                SchemaType::Felts(types)
            },
        }
    }

    /// Returns the value (an array of 4 `FeltSchema`s) if this is a `Value`
    /// variant; otherwise, returns `None`.
    pub fn value(&self) -> Option<&[FeltSchema; 4]> {
        match self {
            WordSchema::Value { value, .. } => Some(value),
            WordSchema::Template { .. } => None,
        }
    }

    /// Returns an iterator over the word's placeholders.
    ///
    /// For [`WordSchema::Value`], it corresponds to the inner iterators (since inner
    /// elements can be templated as well).
    /// For [`WordSchema::Template`] it returns the words's placeholder requirements
    /// as defined.
    pub fn template_requirements(
        &self,
        placeholder_prefix: StorageValueName,
    ) -> TemplateRequirementsIter<'_> {
        let placeholder_key =
            placeholder_prefix.with_suffix(self.name().unwrap_or(&StorageValueName::empty()));
        match self {
            WordSchema::Template { identifier, r#type } => Box::new(iter::once((
                placeholder_key,
                PlaceholderTypeRequirement {
                    description: identifier.description.clone(),
                    r#type: r#type.clone(),
                },
            ))),
            WordSchema::Value { value, .. } => Box::new(
                value
                    .iter()
                    .flat_map(move |felt| felt.template_requirements(placeholder_key.clone())),
            ),
        }
    }

    /// Attempts to convert the [WordSchema] into a [Word].
    ///
    /// If the representation is a template, the value is retrieved from
    /// `init_storage_data`, identified by its key. If any of the inner elements
    /// within the value are a template, they are retrieved in the same way.
    pub(crate) fn try_build_word(
        &self,
        init_storage_data: &InitStorageData,
        placeholder_prefix: StorageValueName,
    ) -> Result<Word, AccountComponentTemplateError> {
        match self {
            WordSchema::Template { identifier, r#type } => {
                let placeholder_path = placeholder_prefix.with_suffix(&identifier.name);
                let maybe_value = init_storage_data.get(&placeholder_path);
                if let Some(value) = maybe_value {
                    let parsed_value = TEMPLATE_REGISTRY
                        .try_parse_word(r#type, value)
                        .map_err(AccountComponentTemplateError::StorageValueParsingError)?;

                    Ok(parsed_value)
                } else {
                    Err(AccountComponentTemplateError::PlaceholderValueNotProvided(
                        placeholder_path,
                    ))
                }
            },
            WordSchema::Value { value, identifier } => {
                let mut result = [Felt::ZERO; 4];

                for (index, felt_repr) in value.iter().enumerate() {
                    let placeholder = placeholder_prefix.clone().with_suffix(
                        identifier
                            .as_ref()
                            .map(|id| &id.name)
                            .unwrap_or(&StorageValueName::empty()),
                    );
                    result[index] = felt_repr.try_build_felt(init_storage_data, placeholder)?;
                }
                // SAFETY: result is guaranteed to have all its 4 indices rewritten
                Ok(Word::from(result))
            },
        }
    }

    /// Validates that the defined type exists and all the inner felt types exist as well
    pub(crate) fn validate(&self) -> Result<(), AccountComponentTemplateError> {
        // Check that type exists in registry
        let type_exists = TEMPLATE_REGISTRY.contains_word_type(&self.word_type());
        if !type_exists {
            return Err(AccountComponentTemplateError::InvalidType(
                self.word_type().to_string(),
                "Word".into(),
            ));
        }

        if let Some(felts) = self.value() {
            for felt in felts {
                felt.validate()?;
            }
        }

        Ok(())
    }
}

impl Serializable for WordSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        match self {
            WordSchema::Template { identifier, r#type } => {
                target.write_u8(0);
                target.write(identifier);
                target.write(r#type);
            },
            WordSchema::Value { identifier, value } => {
                target.write_u8(1);
                target.write(identifier);
                target.write(value);
            },
        }
    }
}

impl Deserializable for WordSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let tag = source.read_u8()?;
        match tag {
            0 => {
                let identifier = FieldIdentifier::read_from(source)?;
                let r#type = TemplateTypeIdentifier::read_from(source)?;
                Ok(WordSchema::Template { identifier, r#type })
            },
            1 => {
                let identifier = Option::<FieldIdentifier>::read_from(source)?;
                let value = <[FeltSchema; 4]>::read_from(source)?;
                Ok(WordSchema::Value { identifier, value })
            },
            other => Err(DeserializationError::InvalidValue(format!(
                "unknown tag '{other}' for WordSchema"
            ))),
        }
    }
}

impl From<[FeltSchema; 4]> for WordSchema {
    fn from(value: [FeltSchema; 4]) -> Self {
        WordSchema::new_value(value, Option::<FieldIdentifier>::None)
    }
}

impl From<[Felt; 4]> for WordSchema {
    fn from(value: [Felt; 4]) -> Self {
        WordSchema::new_value(value.map(FeltSchema::from), Option::<FieldIdentifier>::None)
    }
}

// FELTS
// ================================================================================================

/// Supported element representations for a component's storage entries.
///
/// Each felt element in a storage entry can either be:
/// - A concrete value that holds a predefined felt.
/// - A template that specifies the type of felt expected, with the actual value to be provided
///   later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeltSchema {
    /// A concrete felt value.
    ///
    /// This variant holds a felt that is part of the component's storage.
    /// The optional name allows for identification, and the description offers additional context.
    Value {
        /// An optional identifier for this felt value.
        /// An optional explanation of the felt's purpose.
        identifier: Option<FieldIdentifier>,
        /// The actual felt value.
        value: Felt,
    },

    /// A templated felt element.
    ///
    /// This variant specifies the expected type of the felt without providing a concrete value.
    /// The name is required to uniquely identify the template, and an optional description can
    /// further clarify its intended use.
    Template {
        /// The expected type for this felt element.
        r#type: TemplateTypeIdentifier,
        /// A unique name for the felt template.
        /// An optional description that explains the purpose of this template.
        identifier: FieldIdentifier,
    },
}

impl FeltSchema {
    /// Creates a new [`FeltSchema::Value`] variant.
    pub fn new_value(value: impl Into<Felt>, name: Option<StorageValueName>) -> Self {
        FeltSchema::Value {
            value: value.into(),
            identifier: name.map(FieldIdentifier::with_name),
        }
    }

    /// Creates a new [`FeltSchema::Template`] variant.
    ///
    /// The name will be used for identification at the moment of instantiating the componentn.
    pub fn new_template(r#type: TemplateTypeIdentifier, name: StorageValueName) -> Self {
        FeltSchema::Template {
            r#type,
            identifier: FieldIdentifier::with_name(name),
        }
    }

    /// Sets the description of the [`FeltSchema`] and returns `self`.
    pub fn with_description(self, description: impl Into<String>) -> Self {
        match self {
            FeltSchema::Template { r#type, identifier } => FeltSchema::Template {
                r#type,
                identifier: FieldIdentifier {
                    name: identifier.name,
                    description: Some(description.into()),
                },
            },
            FeltSchema::Value { identifier, value } => FeltSchema::Value {
                identifier: identifier.map(|id| FieldIdentifier {
                    name: id.name,
                    description: Some(description.into()),
                }),
                value,
            },
        }
    }

    /// Returns the felt type.
    pub fn felt_type(&self) -> TemplateTypeIdentifier {
        match self {
            FeltSchema::Template { r#type, .. } => r#type.clone(),
            FeltSchema::Value { .. } => TemplateTypeIdentifier::native_felt(),
        }
    }

    /// Attempts to convert the [FeltSchema] into a [Felt].
    ///
    /// If the representation is a template, the value is retrieved from `init_storage_data`,
    /// identified by its key. Otherwise, the returned value is just the inner element.
    pub(crate) fn try_build_felt(
        &self,
        init_storage_data: &InitStorageData,
        placeholder_prefix: StorageValueName,
    ) -> Result<Felt, AccountComponentTemplateError> {
        match self {
            FeltSchema::Template { identifier, r#type } => {
                let placeholder_key = placeholder_prefix.with_suffix(&identifier.name);
                let raw_value = init_storage_data.get(&placeholder_key).ok_or(
                    AccountComponentTemplateError::PlaceholderValueNotProvided(placeholder_key),
                )?;

                Ok(TEMPLATE_REGISTRY
                    .try_parse_felt(r#type, raw_value)
                    .map_err(AccountComponentTemplateError::StorageValueParsingError)?)
            },
            FeltSchema::Value { value, .. } => Ok(*value),
        }
    }

    /// Returns an iterator over the felt's template.
    ///
    /// For [`FeltSchema::Value`], these is an empty set; for
    /// [`FeltSchema::Template`] it returns the felt's placeholder key based on the
    /// felt's name within the component description.
    pub fn template_requirements(
        &self,
        placeholder_prefix: StorageValueName,
    ) -> TemplateRequirementsIter<'_> {
        match self {
            FeltSchema::Template { identifier, r#type } => Box::new(iter::once((
                placeholder_prefix.with_suffix(&identifier.name),
                PlaceholderTypeRequirement {
                    description: identifier.description.clone(),
                    r#type: r#type.clone(),
                },
            ))),
            _ => Box::new(iter::empty()),
        }
    }

    /// Validates that the defined Felt type exists
    pub(crate) fn validate(&self) -> Result<(), AccountComponentTemplateError> {
        // Check that type exists in registry
        let type_exists = TEMPLATE_REGISTRY.contains_felt_type(&self.felt_type());
        if !type_exists {
            return Err(AccountComponentTemplateError::InvalidType(
                self.felt_type().to_string(),
                "Felt".into(),
            ));
        }
        Ok(())
    }
}

impl From<Felt> for FeltSchema {
    fn from(value: Felt) -> Self {
        FeltSchema::new_value(value, Option::<StorageValueName>::None)
    }
}

impl Default for FeltSchema {
    fn default() -> Self {
        FeltSchema::new_value(Felt::default(), Option::<StorageValueName>::None)
    }
}

impl Serializable for FeltSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        match self {
            FeltSchema::Value { identifier, value } => {
                target.write_u8(0);
                target.write(identifier);
                target.write(value);
            },
            FeltSchema::Template { identifier, r#type } => {
                target.write_u8(1);
                target.write(identifier);
                target.write(r#type);
            },
        }
    }
}

impl Deserializable for FeltSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let tag = source.read_u8()?;
        match tag {
            0 => {
                let identifier = Option::<FieldIdentifier>::read_from(source)?;
                let value = Felt::read_from(source)?;
                Ok(FeltSchema::Value { value, identifier })
            },
            1 => {
                let identifier = FieldIdentifier::read_from(source)?;
                let r#type = TemplateTypeIdentifier::read_from(source)?;
                Ok(FeltSchema::Template { r#type, identifier })
            },
            other => Err(DeserializationError::InvalidValue(format!(
                "unknown tag '{other}' for FeltSchema"
            ))),
        }
    }
}

// MAP ENTRY
// ================================================================================================

/// Key-value entry for storage maps.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(::serde::Deserialize, ::serde::Serialize))]
pub struct MapEntrySchema {
    key: WordSchema,
    value: WordSchema,
}

impl MapEntrySchema {
    /// Creates a new [`MapEntrySchema`] with the given key and value.
    pub fn new(key: impl Into<WordSchema>, value: impl Into<WordSchema>) -> Self {
        Self { key: key.into(), value: value.into() }
    }

    /// Returns a reference to the entry's key.
    pub fn key(&self) -> &WordSchema {
        &self.key
    }

    /// Returns a reference to the entry's value.
    pub fn value(&self) -> &WordSchema {
        &self.value
    }

    /// Deconstructs the entry into its key and value representations.
    pub fn into_parts(self) -> (WordSchema, WordSchema) {
        let MapEntrySchema { key, value } = self;
        (key, value)
    }

    /// Returns the placeholder requirements of the entry.
    pub fn template_requirements(
        &self,
        placeholder_prefix: StorageValueName,
    ) -> TemplateRequirementsIter<'_> {
        let key_iter = self.key.template_requirements(placeholder_prefix.clone());
        let value_iter = self.value.template_requirements(placeholder_prefix);

        Box::new(key_iter.chain(value_iter))
    }

    /// Returns `true` if the entry contains any templated placeholders.
    pub fn contains_template(&self) -> bool {
        self.key().template_requirements(StorageValueName::empty()).next().is_some()
            || self.value().template_requirements(StorageValueName::empty()).next().is_some()
    }
}

impl Serializable for MapEntrySchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        self.key.write_into(target);
        self.value.write_into(target);
    }
}

impl Deserializable for MapEntrySchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let key = WordSchema::read_from(source)?;
        let value = WordSchema::read_from(source)?;
        Ok(MapEntrySchema { key, value })
    }
}

// MAP REPRESENTATION
// ================================================================================================

/// Supported map representations for a component's storage entries.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(::serde::Deserialize, ::serde::Serialize))]
pub enum MapSchema {
    /// A map whose contents are provided during instantiation via placeholders.
    Template {
        /// The human-readable identifier of the map slot.
        identifier: FieldIdentifier,
    },
    /// A map with statically defined key/value pairs.
    Value {
        /// The human-readable identifier of the map slot.
        identifier: FieldIdentifier,
        /// Storage map entries, consisting of a list of keys associated with their values.
        entries: Vec<MapEntrySchema>,
    },
}

impl MapSchema {
    /// Creates a new `MapSchema` from a vector of map entries.
    pub fn new_value(entries: Vec<MapEntrySchema>, name: impl Into<StorageValueName>) -> Self {
        MapSchema::Value {
            entries,
            identifier: FieldIdentifier::with_name(name.into()),
        }
    }

    /// Creates a new templated map representation.
    pub fn new_template(name: impl Into<StorageValueName>) -> Self {
        MapSchema::Template {
            identifier: FieldIdentifier::with_name(name.into()),
        }
    }

    /// Sets the description of the [`MapSchema`] and returns `self`.
    pub fn with_description(self, description: impl Into<String>) -> Self {
        match self {
            MapSchema::Template { identifier } => MapSchema::Template {
                identifier: FieldIdentifier {
                    name: identifier.name,
                    description: Some(description.into()),
                },
            },
            MapSchema::Value { identifier, entries } => MapSchema::Value {
                entries,
                identifier: FieldIdentifier {
                    name: identifier.name,
                    description: Some(description.into()),
                },
            },
        }
    }

    fn infer_schema_type<F>(&self, selector: F) -> SchemaType
    where
        F: Fn(&MapEntrySchema) -> &WordSchema,
    {
        match self {
            MapSchema::Template { .. } => SchemaType::default_word(),
            MapSchema::Value { entries, .. } => entries
                .first()
                .map(selector)
                .map(|schema| schema.schema_type())
                .unwrap_or_else(SchemaType::default_word),
        }
    }

    /// Attempts to collect the statically defined entries as a map of words.
    pub fn default_values(&self) -> Option<BTreeMap<Word, Word>> {
        let (identifier, entries) = match self {
            MapSchema::Value { identifier, entries } => (identifier, entries),
            MapSchema::Template { .. } => return None,
        };

        if entries.iter().any(MapEntrySchema::contains_template) {
            return None;
        }

        let resolved_entries = entries
            .iter()
            .map(|entry| {
                let key = entry
                    .key()
                    .try_build_word(&InitStorageData::default(), identifier.name.clone())?;
                let value = entry
                    .value()
                    .try_build_word(&InitStorageData::default(), identifier.name.clone())?;
                Ok((key, value))
            })
            .collect::<Result<Vec<_>, AccountComponentTemplateError>>()
            .ok()?;

        if StorageMap::with_entries(resolved_entries.clone()).is_err() {
            return None;
        }

        let mut map = BTreeMap::new();
        for (key, value) in resolved_entries {
            map.insert(key, value);
        }

        Some(map)
    }

    /// Returns the schema type describing map keys.
    pub fn key_schema_type(&self) -> SchemaType {
        self.infer_schema_type(|entry| entry.key())
    }

    /// Returns the schema type describing map values.
    pub fn value_schema_type(&self) -> SchemaType {
        self.infer_schema_type(|entry| entry.value())
    }

    /// Returns an iterator over all of the storage entries' placeholder keys, alongside their
    /// expected type.
    pub fn template_requirements(&self) -> TemplateRequirementsIter<'_> {
        match self {
            MapSchema::Template { identifier } => Box::new(iter::once((
                identifier.name.clone(),
                PlaceholderTypeRequirement {
                    description: identifier.description.clone(),
                    r#type: TemplateTypeIdentifier::storage_map(),
                },
            ))),
            MapSchema::Value { identifier, entries } => Box::new(
                entries
                    .iter()
                    .flat_map(move |entry| entry.template_requirements(identifier.name.clone())),
            ),
        }
    }

    /// Returns a reference to map entries.
    pub fn entries(&self) -> &[MapEntrySchema] {
        match self {
            MapSchema::Value { entries, .. } => entries,
            MapSchema::Template { .. } => &[],
        }
    }

    /// Returns a reference to the map's name within the storage metadata.
    pub fn name(&self) -> &StorageValueName {
        match self {
            MapSchema::Template { identifier } | MapSchema::Value { identifier, .. } => {
                &identifier.name
            },
        }
    }

    /// Returns a reference to the field's description.
    pub fn description(&self) -> Option<&String> {
        match self {
            MapSchema::Template { identifier } | MapSchema::Value { identifier, .. } => {
                identifier.description.as_ref()
            },
        }
    }

    /// Returns the number of statically defined key-value pairs in the map.
    pub fn len(&self) -> usize {
        match self {
            MapSchema::Value { entries, .. } => entries.len(),
            MapSchema::Template { .. } => 0,
        }
    }

    /// Returns `true` if there are no statically defined entries in the map.
    pub fn is_empty(&self) -> bool {
        match self {
            MapSchema::Value { entries, .. } => entries.is_empty(),
            MapSchema::Template { .. } => true,
        }
    }

    /// Attempts to convert the [MapSchema] into a [StorageMap].
    ///
    /// If any of the inner elements are templates, their values are retrieved from
    /// `init_storage_data`, identified by their key.
    pub fn try_build_map(
        &self,
        init_storage_data: &InitStorageData,
    ) -> Result<StorageMap, AccountComponentTemplateError> {
        match self {
            MapSchema::Value { identifier, entries } => {
                let entries = entries
                    .iter()
                    .map(|map_entry| {
                        let key = map_entry
                            .key()
                            .try_build_word(init_storage_data, identifier.name.clone())?;
                        let value = map_entry
                            .value()
                            .try_build_word(init_storage_data, identifier.name.clone())?;
                        Ok((key, value))
                    })
                    .collect::<Result<Vec<(Word, Word)>, _>>()?;

                StorageMap::with_entries(entries).map_err(|err| {
                    AccountComponentTemplateError::StorageMapHasDuplicateKeys(Box::new(err))
                })
            },
            MapSchema::Template { identifier } => {
                if let Some(entries) = init_storage_data.map_entries(&identifier.name) {
                    return StorageMap::with_entries(entries.clone()).map_err(|err| {
                        AccountComponentTemplateError::StorageMapHasDuplicateKeys(Box::new(err))
                    });
                }

                Err(AccountComponentTemplateError::PlaceholderValueNotProvided(
                    identifier.name.clone(),
                ))
            },
        }
    }

    /// Validates the map representation by checking for duplicate keys and placeholder validity.
    pub(crate) fn validate(&self) -> Result<(), AccountComponentTemplateError> {
        match self {
            MapSchema::Template { .. } => Ok(()),
            MapSchema::Value { entries, .. } => {
                let mut seen_keys = BTreeSet::new();
                for entry in entries.iter() {
                    entry.key().validate()?;
                    entry.value().validate()?;
                    if let Ok(key) = entry
                        .key()
                        .try_build_word(&InitStorageData::default(), StorageValueName::empty())
                        && !seen_keys.insert(key)
                    {
                        return Err(AccountComponentTemplateError::StorageMapHasDuplicateKeys(
                            Box::from(format!("key `{key}` is duplicated")),
                        ));
                    }
                }

                Ok(())
            },
        }
    }
}

impl Serializable for MapSchema {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        match self {
            MapSchema::Value { identifier, entries } => {
                target.write_u8(0u8);
                target.write(identifier);
                target.write(entries);
            },
            MapSchema::Template { identifier } => {
                target.write_u8(1u8);
                target.write(identifier);
            },
        }
    }
}

impl Deserializable for MapSchema {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        let tag = source.read_u8()?;
        match tag {
            0 => {
                let identifier = FieldIdentifier::read_from(source)?;
                let entries = Vec::<MapEntrySchema>::read_from(source)?;
                Ok(MapSchema::Value { entries, identifier })
            },
            1 => {
                let identifier = FieldIdentifier::read_from(source)?;
                Ok(MapSchema::Template { identifier })
            },
            other => Err(DeserializationError::InvalidValue(format!(
                "unknown tag '{other}' for MapSchema"
            ))),
        }
    }
}
