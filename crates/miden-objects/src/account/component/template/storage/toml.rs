use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use miden_core::{Felt, Word};
use semver::Version;
use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Error, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::placeholder::TemplateTypeIdentifier;
use super::{
    AccountStorageSchema,
    FeltSchema,
    InitStorageData,
    MapEntrySchema,
    MapSchema,
    MapSlotSchema,
    SchemaType,
    StorageSlotSchema,
    StorageValueNameError,
    ValueSlotSchema,
    WordSchema,
};
use crate::account::component::template::storage::placeholder::{TEMPLATE_REGISTRY, TemplateFelt};
use crate::account::component::{AccountComponentMetadata, FieldIdentifier, StorageValueName};
use crate::account::{AccountType, StorageSlotName};
use crate::errors::AccountComponentTemplateError;

// ACCOUNT COMPONENT METADATA TOML FROM/TO
// ================================================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawAccountComponentMetadata {
    name: String,
    description: String,
    version: Version,
    supported_types: BTreeSet<AccountType>,
    #[serde(rename = "storage")]
    storage: Vec<RawStorageSlotSchema>,
}

impl AccountComponentMetadata {
    /// Deserializes `toml_string` and validates the resulting [AccountComponentMetadata]
    ///
    /// # Errors
    ///
    /// - If deserialization fails
    /// - If the schema specifies storage slots with duplicates.
    /// - If the schema contains invalid slot definitions.
    pub fn from_toml(toml_string: &str) -> Result<Self, AccountComponentTemplateError> {
        let raw: RawAccountComponentMetadata = toml::from_str(toml_string)
            .map_err(AccountComponentTemplateError::TomlDeserializationError)?;

        let mut fields = Vec::with_capacity(raw.storage.len());
        for slot in raw.storage {
            fields.push(slot.into_slot_schema()?);
        }

        let storage_schema = AccountStorageSchema::new(fields)?;
        Self::new(raw.name, raw.description, raw.version, raw.supported_types, storage_schema)
    }

    /// Serializes the account component template into a TOML string.
    pub fn to_toml(&self) -> Result<String, AccountComponentTemplateError> {
        let toml =
            toml::to_string(self).map_err(AccountComponentTemplateError::TomlSerializationError)?;
        Ok(toml)
    }
}

// WORD REPRESENTATION SERIALIZATION
// ================================================================================================

impl Serialize for WordSchema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            WordSchema::Template { identifier, r#type } => {
                let mut state = serializer.serialize_struct("WordSchema", 3)?;
                state.serialize_field("name", &identifier.name())?;
                state.serialize_field("description", &identifier.description())?;
                state.serialize_field("type", r#type)?;
                state.end()
            },
            WordSchema::Value { identifier, value } => {
                let mut state = serializer.serialize_struct("WordSchema", 3)?;

                state.serialize_field("name", &identifier.as_ref().map(|id| id.name()))?;
                state.serialize_field(
                    "description",
                    &identifier.as_ref().map(|id| id.description()),
                )?;
                state.serialize_field("value", value)?;
                state.end()
            },
        }
    }
}

impl<'de> Deserialize<'de> for WordSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct WordSchemaVisitor;

        impl<'de> Visitor<'de> for WordSchemaVisitor {
            type Value = WordSchema;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string or a map representing a WordSchema")
            }

            // A bare string is interpreted it as a Value variant.
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                let parsed_value = Word::parse(value).map_err(|_err| {
                    E::invalid_value(
                        serde::de::Unexpected::Str(value),
                        &"a valid hexadecimal string",
                    )
                })?;
                Ok(<[Felt; _]>::from(&parsed_value).into())
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_str(&value)
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                // Deserialize as a list of felt representations
                let elements: Vec<FeltSchema> =
                    Deserialize::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
                if elements.len() != 4 {
                    return Err(Error::invalid_length(
                        elements.len(),
                        &"expected an array of 4 elements",
                    ));
                }
                let value: [FeltSchema; 4] = elements.try_into().expect("length was checked");
                Ok(WordSchema::new_value(value, None))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                #[derive(Deserialize, Debug)]
                struct WordSchemaHelper {
                    name: Option<String>,
                    description: Option<String>,
                    // The "value" field (if present) must be an array of 4 FeltSchemas.
                    value: Option<[FeltSchema; 4]>,
                    #[serde(rename = "type")]
                    r#type: Option<TemplateTypeIdentifier>,
                }

                let helper = WordSchemaHelper::deserialize(MapAccessDeserializer::new(map))?;

                if let Some(value) = helper.value {
                    let identifier = helper
                        .name
                        .map(|n| parse_field_identifier::<M::Error>(n, helper.description.clone()))
                        .transpose()?;
                    Ok(WordSchema::Value { value, identifier })
                } else {
                    // Otherwise, we expect a Template variant (name is required for identification)
                    let identifier = expect_parse_field_identifier::<M::Error>(
                        helper.name,
                        helper.description,
                        "word template",
                    )?;
                    let r#type = helper.r#type.unwrap_or_else(TemplateTypeIdentifier::native_word);
                    Ok(WordSchema::Template { r#type, identifier })
                }
            }
        }

        deserializer.deserialize_any(WordSchemaVisitor)
    }
}

// FELT REPRESENTATION SERIALIZATION
// ================================================================================================

impl Serialize for FeltSchema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            FeltSchema::Value { identifier, value } => {
                let hex = value.to_string();
                if identifier.is_none() {
                    serializer.serialize_str(&hex)
                } else {
                    let mut state = serializer.serialize_struct("FeltSchema", 3)?;
                    if let Some(id) = identifier {
                        state.serialize_field("name", &id.name)?;
                        state.serialize_field("description", &id.description)?;
                    }
                    state.serialize_field("value", &hex)?;
                    state.end()
                }
            },
            FeltSchema::Template { identifier, r#type } => {
                let mut state = serializer.serialize_struct("FeltSchema", 3)?;
                state.serialize_field("name", &identifier.name)?;
                state.serialize_field("description", &identifier.description)?;
                state.serialize_field("type", r#type)?;
                state.end()
            },
        }
    }
}

impl<'de> Deserialize<'de> for FeltSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Felts can be deserialized as either:
        //
        // - Scalars (parsed from strings)
        // - A table object that can or cannot hardcode a value. If not present, this is a
        //   placeholder type
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Intermediate {
            Map {
                name: Option<String>,
                description: Option<String>,
                #[serde(default)]
                value: Option<String>,
                #[serde(rename = "type")]
                r#type: Option<TemplateTypeIdentifier>,
            },
            Scalar(String),
        }

        let intermediate = Intermediate::deserialize(deserializer)?;
        match intermediate {
            Intermediate::Scalar(s) => {
                let felt = Felt::parse_felt(&s)
                    .map_err(|e| D::Error::custom(format!("failed to parse Felt: {e}")))?;
                Ok(FeltSchema::Value { identifier: None, value: felt })
            },
            Intermediate::Map { name, description, value, r#type } => {
                // Get the defined type, or the default if it was not specified
                let felt_type = r#type.unwrap_or_else(TemplateTypeIdentifier::native_felt);
                if let Some(val_str) = value {
                    // Parse into felt from the input string
                    let felt =
                        TEMPLATE_REGISTRY.try_parse_felt(&felt_type, &val_str).map_err(|e| {
                            D::Error::custom(format!("failed to parse {felt_type} as Felt: {e}"))
                        })?;
                    let identifier = name
                        .map(|n| parse_field_identifier::<D::Error>(n, description.clone()))
                        .transpose()?;
                    Ok(FeltSchema::Value { identifier, value: felt })
                } else {
                    // No value provided, so this is a placeholder
                    let identifier = expect_parse_field_identifier::<D::Error>(
                        name,
                        description,
                        "map template",
                    )?;
                    Ok(FeltSchema::Template { r#type: felt_type, identifier })
                }
            },
        }
    }
}

// ACCOUNT STORAGE SCHEMA SERIALIZATION
// ================================================================================================

#[derive(Debug, Deserialize, Serialize)]
struct RawStorageSlotSchema {
    /// The name of the storage slot, in `StorageSlotName` format (e.g.
    /// `my_project::module::slot`).
    name: String,
    #[serde(default)]
    description: Option<String>,
    /// Slot type.
    ///
    /// - If `type = "map"`, this is a map slot.
    /// - Otherwise, if `type` is set and `value` is not, this is a templated word slot.
    #[serde(rename = "type")]
    #[serde(default)]
    r#type: Option<TemplateTypeIdentifier>,
    /// Word slot value representation (can contain nested templates).
    #[serde(default)]
    value: Option<WordSchema>,
    /// Map slot entries (can contain templates).
    #[serde(default)]
    values: Option<Vec<MapEntrySchema>>,
    #[serde(rename = "key-type")]
    #[serde(default)]
    key_type: Option<RawSchemaType>,
    #[serde(rename = "value-type")]
    #[serde(default)]
    value_type: Option<RawSchemaType>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum RawSchemaType {
    Word(TemplateTypeIdentifier),
    Felts([TemplateTypeIdentifier; 4]),
}

impl From<SchemaType> for RawSchemaType {
    fn from(schema_type: SchemaType) -> Self {
        match schema_type {
            SchemaType::Word(id) => RawSchemaType::Word(id),
            SchemaType::Felts(ids) => RawSchemaType::Felts(ids),
        }
    }
}

impl From<RawSchemaType> for SchemaType {
    fn from(raw: RawSchemaType) -> Self {
        match raw {
            RawSchemaType::Word(id) => SchemaType::Word(id),
            RawSchemaType::Felts(ids) => SchemaType::Felts(ids),
        }
    }
}

impl RawStorageSlotSchema {
    fn from_slot(slot_name: &StorageSlotName, schema: &StorageSlotSchema) -> Self {
        match schema {
            StorageSlotSchema::Value(slot) => {
                let word = slot.word();
                let (r#type, value) = match word {
                    WordSchema::Template { identifier, r#type }
                        if identifier.name.as_str().is_empty() =>
                    {
                        (Some(r#type.clone()), None)
                    },
                    other => (None, Some(other.clone())),
                };

                Self {
                    name: slot_name.as_str().to_string(),
                    description: slot.description().cloned(),
                    r#type,
                    value,
                    values: None,
                    key_type: None,
                    value_type: None,
                }
            },
            StorageSlotSchema::Map(slot) => {
                let map = slot.map();
                let (r#type, values) = match map {
                    MapSchema::Template { .. } => {
                        (Some(TemplateTypeIdentifier::storage_map()), None)
                    },
                    MapSchema::Value { entries, .. } => {
                        (Some(TemplateTypeIdentifier::storage_map()), Some(entries.clone()))
                    },
                };

                Self {
                    name: slot_name.as_str().to_string(),
                    description: slot.description().cloned(),
                    r#type,
                    value: None,
                    values,
                    key_type: Some(RawSchemaType::from(slot.key_type().clone())),
                    value_type: Some(RawSchemaType::from(slot.value_type().clone())),
                }
            },
        }
    }

    fn into_slot_schema(
        self,
    ) -> Result<(StorageSlotName, StorageSlotSchema), AccountComponentTemplateError> {
        let RawStorageSlotSchema {
            name,
            description,
            r#type,
            value,
            values,
            key_type,
            value_type,
        } = self;

        let slot_name_raw = name;
        let slot_name = StorageSlotName::new(slot_name_raw.clone()).map_err(|err| {
            AccountComponentTemplateError::InvalidSchema(format!(
                "invalid storage slot name `{slot_name_raw}`: {err}"
            ))
        })?;

        let description =
            description.and_then(|d| if d.trim().is_empty() { None } else { Some(d) });

        if value.is_some() && values.is_some() {
            return Err(AccountComponentTemplateError::InvalidSchema(
                "storage slot schema cannot define both `value` (word slot) and `values` (map slot)"
                    .into(),
            ));
        }

        let slot_prefix = super::slot_name_to_placeholder_prefix(&slot_name);
        let key_type = key_type.map(Into::into);
        let value_type = value_type.map(Into::into);

        match (r#type, value, values) {
            // Map slot with statically-defined entries (which may contain templates).
            (maybe_type, None, Some(entries)) => {
                if let Some(r#type) = maybe_type.clone()
                    && r#type != TemplateTypeIdentifier::storage_map()
                {
                    return Err(AccountComponentTemplateError::InvalidSchema(
                        "map storage slots with `values` must have `type = \"map\"`".into(),
                    ));
                }

                let identifier = FieldIdentifier {
                    name: slot_prefix.clone(),
                    description: description.clone(),
                };

                Ok((
                    slot_name,
                    StorageSlotSchema::Map(MapSlotSchema::new(
                        description.clone(),
                        MapSchema::Value { identifier, entries },
                        key_type.clone(),
                        value_type.clone(),
                    )),
                ))
            },

            // Map slot whose contents are provided at instantiation time.
            (Some(r#type), None, None) if r#type == TemplateTypeIdentifier::storage_map() => {
                let identifier = FieldIdentifier {
                    name: slot_prefix.clone(),
                    description: description.clone(),
                };

                Ok((
                    slot_name,
                    StorageSlotSchema::Map(MapSlotSchema::new(
                        description.clone(),
                        MapSchema::Template { identifier },
                        key_type.clone(),
                        value_type.clone(),
                    )),
                ))
            },

            // Word slot with explicit value representation (which may contain nested templates).
            (None, Some(value), None) => Ok((
                slot_name,
                StorageSlotSchema::Value(ValueSlotSchema::new(description.clone(), value)),
            )),

            // Templated word slot; placeholder key is derived from slot name.
            (Some(r#type), None, None) => {
                let identifier = FieldIdentifier {
                    name: StorageValueName::empty(),
                    description: description.clone(),
                };

                Ok((
                    slot_name,
                    StorageSlotSchema::Value(ValueSlotSchema::new(
                        description,
                        WordSchema::Template { identifier, r#type },
                    )),
                ))
            },

            (None, None, None) => Err(AccountComponentTemplateError::InvalidSchema(
                "storage slot schema must define either `value`, `values`, or `type`".into(),
            )),

            (Some(_), Some(_), _) => Err(AccountComponentTemplateError::InvalidSchema(
                "storage slot schema cannot define both `value` and `type`".into(),
            )),

            (None, _, Some(_)) => Err(AccountComponentTemplateError::InvalidSchema(
                "storage slot schema cannot define `values` without `type = \"map\"`".into(),
            )),
        }
    }

    fn try_into_slot_schema<E>(self) -> Result<(StorageSlotName, StorageSlotSchema), E>
    where
        E: serde::de::Error,
    {
        self.into_slot_schema().map_err(|err| E::custom(err.to_string()))
    }
}

impl Serialize for AccountStorageSchema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.fields().len()))?;
        for (slot_name, schema) in self.fields().iter() {
            seq.serialize_element(&RawStorageSlotSchema::from_slot(slot_name, schema))?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for AccountStorageSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw_schemas = Vec::<RawStorageSlotSchema>::deserialize(deserializer)?;
        let mut fields = Vec::with_capacity(raw_schemas.len());

        for raw in raw_schemas {
            let (slot_name, schema) = raw.try_into_slot_schema::<D::Error>()?;
            fields.push((slot_name, schema));
        }

        AccountStorageSchema::new(fields).map_err(D::Error::custom)
    }
}

// INIT STORAGE DATA
// ================================================================================================

impl InitStorageData {
    /// Creates an instance of [`InitStorageData`] from a TOML string.
    ///
    /// This method parses the provided TOML and flattens nested tables into
    /// dot‑separated keys using [`StorageValueName`] as keys. All values are converted to plain
    /// strings (so that, for example, `key = 10` and `key = "10"` both yield
    /// `String::from("10")` as the value).
    ///
    /// # Errors
    ///
    /// - If duplicate keys or empty tables are found in the string
    /// - If the TOML string includes arrays
    pub fn from_toml(toml_str: &str) -> Result<Self, InitStorageDataError> {
        let value: toml::Value = toml::from_str(toml_str)?;
        let mut value_entries = BTreeMap::new();
        let mut map_entries = BTreeMap::new();
        // Start with an empty prefix (i.e. the default, which is an empty string)
        Self::flatten_parse_toml_value(
            StorageValueName::empty(),
            value,
            &mut value_entries,
            &mut map_entries,
        )?;

        Ok(InitStorageData::new(value_entries, map_entries))
    }

    /// Recursively flattens a TOML `Value` into a flat mapping.
    ///
    /// When recursing into nested tables, keys are combined using
    /// [`StorageValueName::with_suffix`]. If an encountered table is empty (and not the top-level),
    /// an error is returned. Arrays are not supported.
    fn flatten_parse_toml_value(
        prefix: StorageValueName,
        value: toml::Value,
        value_entries: &mut BTreeMap<StorageValueName, String>,
        map_entries: &mut BTreeMap<StorageValueName, Vec<(Word, Word)>>,
    ) -> Result<(), InitStorageDataError> {
        match value {
            toml::Value::Table(table) => {
                // If this is not the root and the table is empty, error
                if !prefix.as_str().is_empty() && table.is_empty() {
                    return Err(InitStorageDataError::EmptyTable(prefix.as_str().into()));
                }
                for (key, val) in table {
                    // Create a new key and combine it with the current prefix.
                    let new_key = StorageValueName::new(key.to_string())
                        .map_err(InitStorageDataError::InvalidStorageValueName)?;
                    let new_prefix = prefix.clone().with_suffix(&new_key);
                    Self::flatten_parse_toml_value(new_prefix, val, value_entries, map_entries)?;
                }
            },
            toml::Value::Array(items) if items.is_empty() => {
                if prefix.as_str().is_empty() {
                    return Err(InitStorageDataError::ArraysNotSupported);
                }
                map_entries.insert(prefix, Vec::new());
            },
            toml::Value::Array(items) => {
                if prefix.as_str().is_empty()
                    || !items.iter().all(|item| matches!(item, toml::Value::Table(_)))
                {
                    return Err(InitStorageDataError::ArraysNotSupported);
                }

                let entries = items
                    .into_iter()
                    .map(parse_map_entry_value)
                    .collect::<Result<Vec<(Word, Word)>, _>>()?;
                map_entries.insert(prefix, entries);
            },
            toml_value => {
                // Get the string value, or convert to string if it's some other type
                let value = match toml_value {
                    toml::Value::String(s) => s.clone(),
                    _ => toml_value.to_string(),
                };
                value_entries.insert(prefix, value);
            },
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum InitStorageDataError {
    #[error("failed to parse TOML")]
    InvalidToml(#[from] toml::de::Error),

    #[error("empty table encountered for key `{0}`")]
    EmptyTable(String),

    #[error("invalid input: arrays are not supported")]
    ArraysNotSupported,

    #[error("invalid storage value name")]
    InvalidStorageValueName(#[source] StorageValueNameError),

    #[error("invalid map entry: {0}")]
    InvalidMapEntrySchema(String),
}

impl Serialize for FieldIdentifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("description", &self.description)?;
        map.end()
    }
}

struct FieldIdentifierVisitor;

impl<'de> Visitor<'de> for FieldIdentifierVisitor {
    type Value = FieldIdentifier;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map with 'name' and optionally 'description'")
    }

    fn visit_map<M>(self, mut map: M) -> Result<FieldIdentifier, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut name = None;
        let mut description = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "name" => {
                    name = Some(map.next_value()?);
                },
                "description" => {
                    let d: String = map.next_value()?;
                    // Normalize empty or whitespace-only strings into None
                    description = if d.trim().is_empty() { None } else { Some(d) };
                },
                _ => {
                    // Ignore other values as FieldIdentifiers are flattened within other structs
                    let _: de::IgnoredAny = map.next_value()?;
                },
            }
        }
        let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
        Ok(FieldIdentifier { name, description })
    }
}

impl<'de> Deserialize<'de> for FieldIdentifier {
    fn deserialize<D>(deserializer: D) -> Result<FieldIdentifier, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(FieldIdentifierVisitor)
    }
}

// UTILS / HELPERS
// ================================================================================================

fn missing_field_for<E: serde::de::Error>(field: &str, context: &str) -> E {
    E::custom(format!("missing '{field}' field for {context}"))
}

/// Checks than an optional (but expected) name field has been defined and is correct.
fn expect_parse_field_identifier<E: serde::de::Error>(
    n: Option<String>,
    description: Option<String>,
    context: &str,
) -> Result<FieldIdentifier, E> {
    let name = n.ok_or_else(|| missing_field_for("name", context))?;
    parse_field_identifier(name, description)
}

/// Tries to parse a string into a [FieldIdentifier].
fn parse_field_identifier<E: serde::de::Error>(
    n: String,
    description: Option<String>,
) -> Result<FieldIdentifier, E> {
    StorageValueName::new(n)
        .map_err(|err| E::custom(format!("invalid `name`: {err}")))
        .map(|storage_name| {
            if let Some(desc) = description {
                FieldIdentifier::with_description(storage_name, desc)
            } else {
                FieldIdentifier::with_name(storage_name)
            }
        })
}

/// Parses a `{ key, value }` TOML table into a `(Word, Word)` pair, rejecting templates.
fn parse_map_entry_value(item: toml::Value) -> Result<(Word, Word), InitStorageDataError> {
    // Try to deserialize the user input as a map entry
    let entry: MapEntrySchema = MapEntrySchema::deserialize(item)
        .map_err(|err| InitStorageDataError::InvalidMapEntrySchema(err.to_string()))?;

    // Make sure the entry does not contain templates, only static
    if entry.key().template_requirements(StorageValueName::empty()).next().is_some()
        || entry.value().template_requirements(StorageValueName::empty()).next().is_some()
    {
        return Err(InitStorageDataError::InvalidMapEntrySchema(
            "map entries cannot contain templates".into(),
        ));
    }

    // Interpret the user input as static words
    let key = entry
        .key()
        .try_build_word(&InitStorageData::default(), StorageValueName::empty())
        .map_err(|err| InitStorageDataError::InvalidMapEntrySchema(err.to_string()))?;
    let value = entry
        .value()
        .try_build_word(&InitStorageData::default(), StorageValueName::empty())
        .map_err(|err| InitStorageDataError::InvalidMapEntrySchema(err.to_string()))?;

    Ok((key, value))
}

// TESTS
// ================================================================================================

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use core::error::Error;

    use super::*;
    use crate::account::AccountStorage;
    use crate::account::component::toml::InitStorageDataError;

    #[test]
    fn from_toml_str_with_nested_table_and_flattened() {
        let toml_table = r#"
            [token_metadata]
            max_supply = "1000000000"
            symbol = "ETH"
            decimals = "9"
        "#;

        let toml_inline = r#"
            token_metadata.max_supply = "1000000000"
            token_metadata.symbol = "ETH"
            token_metadata.decimals = "9"
        "#;

        let storage_table = InitStorageData::from_toml(toml_table).unwrap();
        let storage_inline = InitStorageData::from_toml(toml_inline).unwrap();

        assert_eq!(storage_table.placeholders(), storage_inline.placeholders());
    }

    #[test]
    fn from_toml_str_with_deeply_nested_tables() {
        let toml_str = r#"
            [a]
            b = "0xb"

            [a.c]
            d = "0xd"

            [x.y.z]
            w = 42 # NOTE: This gets parsed as string
        "#;

        let storage = InitStorageData::from_toml(toml_str).expect("Failed to parse TOML");
        let key1 = StorageValueName::new("a.b".to_string()).unwrap();
        let key2 = StorageValueName::new("a.c.d".to_string()).unwrap();
        let key3 = StorageValueName::new("x.y.z.w".to_string()).unwrap();

        assert_eq!(storage.get(&key1).unwrap(), "0xb");
        assert_eq!(storage.get(&key2).unwrap(), "0xd");
        assert_eq!(storage.get(&key3).unwrap(), "42");
    }

    #[test]
    fn test_error_on_array() {
        let toml_str = r#"
            token_metadata.v = [1, 2, 3]
        "#;

        let result = InitStorageData::from_toml(toml_str);
        assert_matches::assert_matches!(
            result.unwrap_err(),
            InitStorageDataError::ArraysNotSupported
        );
    }

    #[test]
    fn parse_map_entries_from_array() {
        let toml_str = r#"
            my_map = [
                { key = "0x0000000000000000000000000000000000000000000000000000000000000001", value = "0x0000000000000000000000000000000000000000000000000000000000000010" },
                { key = "0x0000000000000000000000000000000000000000000000000000000000000002", value = ["1", "2", "3", "4"] }
            ]
        "#;

        let storage = InitStorageData::from_toml(toml_str).expect("Failed to parse map entries");
        let map_name = StorageValueName::new("my_map").unwrap();
        let entries = storage.map_entries(&map_name).expect("map entries missing");
        assert_eq!(entries.len(), 2);

        let first_key =
            Word::try_from("0x0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        assert_eq!(entries[0].0, first_key);

        let second_value =
            Word::from([Felt::new(1u64), Felt::new(2u64), Felt::new(3u64), Felt::new(4u64)]);
        assert_eq!(entries[1].1, second_value);
    }

    #[test]
    fn error_on_empty_subtable() {
        let toml_str = r#"
            [a]
            b = {}
        "#;

        let result = InitStorageData::from_toml(toml_str);
        assert_matches::assert_matches!(result.unwrap_err(), InitStorageDataError::EmptyTable(_));
    }

    #[test]
    fn error_on_duplicate_keys() {
        let toml_str = r#"
            token_metadata.max_supply = "1000000000"
            token_metadata.max_supply = "500000000"
        "#;

        let result = InitStorageData::from_toml(toml_str).unwrap_err();
        // TOML does not support duplicate keys
        assert_matches::assert_matches!(result, InitStorageDataError::InvalidToml(_));
        assert!(result.source().unwrap().to_string().contains("duplicate"));
    }

    #[test]
    fn metadata_from_toml_parses_named_storage_schema() {
        let toml_str = r#"
            name = "test component"
            description = "test description"
            version = "0.1.0"
            supported-types = []

            [[storage]]
            name = "demo::test_value"
            description = "a demo slot"
            type = "word"

            [[storage]]
            name = "demo::my_map"
            type = "map"
            values = [
                { key = "0x0000000000000000000000000000000000000000000000000000000000000001", value = { name = "val" } },
            ]
        "#;

        let metadata = AccountComponentMetadata::from_toml(toml_str).unwrap();
        let requirements = metadata.get_placeholder_requirements();

        assert!(requirements.contains_key(&StorageValueName::new("demo.test_value").unwrap()));
        assert!(requirements.contains_key(&StorageValueName::new("demo.my_map.val").unwrap()));
    }

    #[test]
    fn metadata_from_toml_rejects_reserved_slot_names() {
        let reserved_slot = AccountStorage::faucet_metadata_slot().as_str();

        let toml_str = format!(
            r#"
                name = "test component"
                description = "test description"
                version = "0.1.0"
                supported-types = []

                [[storage]]
                name = "{reserved_slot}"
                type = "word"
            "#
        );

        assert_matches::assert_matches!(
            AccountComponentMetadata::from_toml(&toml_str),
            Err(AccountComponentTemplateError::ReservedSlotName(_))
        );
    }

    #[test]
    fn metadata_toml_round_trip_value_and_map_slots() {
        let toml_str = r#"
            name = "round trip"
            description = "test round-trip"
            version = "0.1.0"
            supported-types = []

            [[storage]]
            name = "demo::scalar"
            description = "single word slot"
            value = "0x1"

            [[storage]]
            name = "demo::statemap"
            type = "map"
            values = [
                { key = "0x000000000000ed5d", value = "0x10" },
            ]
        "#;

        let original =
            AccountComponentMetadata::from_toml(toml_str).expect("original metadata should parse");
        let round_trip_toml = original.to_toml().expect("serialize to toml");
        let round_trip =
            AccountComponentMetadata::from_toml(&round_trip_toml).expect("round-trip parse");

        assert_eq!(original, round_trip);
    }

    #[test]
    fn metadata_toml_round_trip_typed_slots() {
        let toml_str = r#"
            name = "typed components"
            description = "test typed slots"
            version = "0.1.0"
            supported-types = []

            [[storage]]
            name = "demo::typed_value"
            type = "word"

            [[storage]]
            name = "demo::typed_map"
            type = "map"
            key-type = "word"
            value-type = ["u8", "u16", "u32", "felt"]
            values = [
                { key = { name = "key_word", type = "word" }, value = { name = "value_word", type = "word" } },
            ]
        "#;

        let metadata =
            AccountComponentMetadata::from_toml(toml_str).expect("typed metadata should parse");
        let schema = metadata.storage_schema();

        let value_slot = schema
            .fields()
            .get(&StorageSlotName::new("demo::typed_value").unwrap())
            .expect("value slot missing");
        let value_slot = match value_slot {
            StorageSlotSchema::Value(slot) => slot,
            _ => panic!("expected value slot"),
        };

        let typed_value = TemplateTypeIdentifier::native_word();
        assert_eq!(value_slot.schema_type(), SchemaType::Word(typed_value.clone()));

        let map_slot = schema
            .fields()
            .get(&StorageSlotName::new("demo::typed_map").unwrap())
            .expect("map slot missing");
        let map_slot = match map_slot {
            StorageSlotSchema::Map(slot) => slot,
            _ => panic!("expected map slot"),
        };

        assert_eq!(map_slot.key_type(), &SchemaType::Word(TemplateTypeIdentifier::native_word()));
        assert_eq!(
            map_slot.value_type(),
            &SchemaType::Felts([
                TemplateTypeIdentifier::new("u8").unwrap(),
                TemplateTypeIdentifier::new("u16").unwrap(),
                TemplateTypeIdentifier::new("u32").unwrap(),
                TemplateTypeIdentifier::new("felt").unwrap(),
            ])
        );

        let mut requirements = metadata.get_placeholder_requirements();
        assert_eq!(
            requirements
                .remove(&StorageValueName::new("demo.typed_value").unwrap())
                .unwrap()
                .r#type,
            typed_value
        );
        assert_eq!(
            requirements
                .remove(&StorageValueName::new("demo.typed_map.key_word").unwrap())
                .unwrap()
                .r#type,
            TemplateTypeIdentifier::native_word()
        );
        assert_eq!(
            requirements
                .remove(&StorageValueName::new("demo.typed_map.value_word").unwrap())
                .unwrap()
                .r#type,
            TemplateTypeIdentifier::native_word()
        );

        let round_trip = metadata.to_toml().expect("serialize");
        let parsed: toml::Value = toml::from_str(&round_trip).unwrap();
        let storage = parsed.get("storage").unwrap().as_array().unwrap();

        let typed_value_entry = storage
            .iter()
            .find(|entry| entry.get("name").unwrap().as_str().unwrap() == "demo::typed_value")
            .unwrap();
        assert_eq!(typed_value_entry.get("type").unwrap().as_str().unwrap(), "word");

        let typed_map_entry = storage
            .iter()
            .find(|entry| entry.get("name").unwrap().as_str().unwrap() == "demo::typed_map")
            .unwrap();
        assert_eq!(typed_map_entry.get("type").unwrap().as_str().unwrap(), "map");
        assert_eq!(typed_map_entry.get("key-type").unwrap().as_str().unwrap(), "word");
        let values = typed_map_entry.get("values").unwrap().as_array().unwrap();
        let value_type = typed_map_entry.get("value-type").unwrap().as_array().unwrap();
        assert_eq!(
            value_type.iter().map(|value| value.as_str().unwrap()).collect::<Vec<_>>(),
            vec!["u8", "u16", "u32", "felt"]
        );
        let first = values.get(0).unwrap().as_table().unwrap();
        let key = first.get("key").unwrap().as_table().unwrap();
        assert_eq!(key.get("type").unwrap().as_str().unwrap(), "word");
        let value = first.get("value").unwrap().as_table().unwrap();
        assert_eq!(value.get("type").unwrap().as_str().unwrap(), "word");
    }
}
