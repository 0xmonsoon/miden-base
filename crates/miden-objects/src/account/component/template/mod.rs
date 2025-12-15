use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use core::str::FromStr;

use miden_core::utils::{ByteReader, ByteWriter, Deserializable, Serializable};
use miden_mast_package::{Package, SectionId};
use miden_processor::DeserializationError;
use semver::Version;

use super::AccountType;
use crate::AccountError;
use crate::errors::AccountComponentTemplateError;

mod storage;
pub use storage::*;

// ACCOUNT COMPONENT METADATA
// ================================================================================================

/// Represents the full component metadata configuration.
///
/// An account component metadata describes the component alongside its storage layout.
/// On the storage layout, placeholders can be utilized to identify values that should be provided
/// at the moment of instantiation.
///
/// When the `std` feature is enabled, this struct allows for serialization and deserialization to
/// and from a TOML file.
///
/// # Guarantees
///
/// - The metadata's storage schema does not contain duplicate slot names.
/// - The schema cannot contain protocol-reserved slot names.
/// - Each placeholder represents a single value. The expected placeholders can be retrieved with
///   [AccountComponentMetadata::get_placeholder_requirements()], which returns a map from keys to
///   [PlaceholderTypeRequirement] (which, in turn, indicates the expected value type for the
///   placeholder).
///
/// # Example
///
/// ```
/// use std::collections::{BTreeMap, BTreeSet};
///
/// use miden_objects::Felt;
/// use miden_objects::account::StorageSlotName;
/// use miden_objects::account::component::{
///     AccountComponentMetadata,
///     AccountStorageSchema,
///     FeltRepresentation,
///     InitStorageData,
///     StorageSlotSchema,
///     StorageValueName,
///     TemplateTypeIdentifier,
///     WordRepresentation,
/// };
/// use semver::Version;
///
/// let slot_name = StorageSlotName::new("demo::test_value")?;
///
/// let word = WordRepresentation::new_value(
///     [
///         FeltRepresentation::from(Felt::new(0u64)),
///         FeltRepresentation::from(Felt::new(1u64)),
///         FeltRepresentation::from(Felt::new(2u64)),
///         FeltRepresentation::new_template(
///             TemplateTypeIdentifier::native_felt(),
///             StorageValueName::new("foo")?,
///         ),
///     ],
///     None,
/// );
///
/// let storage_schema = AccountStorageSchema::new([(
///     slot_name,
///     StorageSlotSchema::Value {
///         description: Some("demo slot".into()),
///         value: word,
///     },
/// )])?;
///
/// let metadata = AccountComponentMetadata::new(
///     "test name".into(),
///     "description of the component".into(),
///     Version::parse("0.1.0")?,
///     BTreeSet::new(),
///     storage_schema,
/// )?;
///
/// // Placeholder keys are derived from slot name (`::` -> `.`): `demo.test_value.foo`.
/// let init_storage_data = InitStorageData::new(
///     [(StorageValueName::new("demo.test_value.foo")?, "300".to_string())],
///     BTreeMap::new(),
/// );
///
/// let storage_slots = metadata.storage_schema().build_storage_slots(&init_storage_data)?;
/// assert_eq!(storage_slots.len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "std", serde(rename_all = "kebab-case"))]
pub struct AccountComponentMetadata {
    /// The human-readable name of the component.
    name: String,

    /// A brief description of what this component is and how it works.
    description: String,

    /// The version of the component using semantic versioning.
    /// This can be used to track and manage component upgrades.
    version: Version,

    /// A set of supported target account types for this component.
    supported_types: BTreeSet<AccountType>,

    /// Storage schema defining the component's storage layout and defaults/templates.
    #[cfg_attr(feature = "std", serde(rename = "storage"))]
    storage_schema: AccountStorageSchema,
}

impl AccountComponentMetadata {
    /// Create a new [AccountComponentMetadata].
    ///
    /// # Errors
    ///
    /// - If the schema contains invalid slot definitions.
    /// - If the schema contains duplicate slot names.
    /// - If the schema contains duplicate placeholder names.
    pub fn new(
        name: String,
        description: String,
        version: Version,
        targets: BTreeSet<AccountType>,
        storage_schema: AccountStorageSchema,
    ) -> Result<Self, AccountComponentTemplateError> {
        let component = Self {
            name,
            description,
            version,
            supported_types: targets,
            storage_schema,
        };
        component.validate()?;
        Ok(component)
    }

    /// Retrieves a map of unique storage placeholder names mapped to their expected type that
    /// require a value at the moment of component instantiation.
    ///
    /// These values will be used for initializing storage slot values, or storage map entries.
    /// For a full example on how a placeholder may be utilized, please refer to the docs for
    /// [AccountComponentMetadata].
    ///
    /// Types for the returned storage placeholders are inferred based on their location in the
    /// storage layout structure.
    pub fn get_placeholder_requirements(
        &self,
    ) -> BTreeMap<StorageValueName, PlaceholderTypeRequirement> {
        let mut templates = BTreeMap::new();
        for (name, requirement) in self.storage_schema.template_requirements() {
            templates.insert(name, requirement);
        }

        templates
    }

    /// Returns the name of the account component.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the description of the account component.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the semantic version of the account component.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// Returns the account types supported by the component.
    pub fn supported_types(&self) -> &BTreeSet<AccountType> {
        &self.supported_types
    }

    /// Returns the storage schema of the component.
    pub fn storage_schema(&self) -> &AccountStorageSchema {
        &self.storage_schema
    }

    /// Validate the [AccountComponentMetadata].
    ///
    /// # Errors
    ///
    /// - If the schema contains invalid slot definitions.
    /// - If the schema contains duplicate placeholder names.
    fn validate(&self) -> Result<(), AccountComponentTemplateError> {
        self.storage_schema.validate()?;

        // Check for duplicate storage placeholder names
        let mut seen_placeholder_names = BTreeSet::new();
        for (name, _) in self.storage_schema.template_requirements() {
            if !seen_placeholder_names.insert(name.clone()) {
                return Err(AccountComponentTemplateError::DuplicatePlaceholderName(name));
            }
        }

        Ok(())
    }
}

impl TryFrom<&Package> for AccountComponentMetadata {
    type Error = AccountError;

    fn try_from(package: &Package) -> Result<Self, Self::Error> {
        package
            .sections
            .iter()
            .find_map(|section| {
                (section.id == SectionId::ACCOUNT_COMPONENT_METADATA).then(|| {
                    AccountComponentMetadata::read_from_bytes(&section.data).map_err(|err| {
                        AccountError::other_with_source(
                            "failed to deserialize account component metadata",
                            err,
                        )
                    })
                })
            })
            .transpose()?
            .ok_or_else(|| {
                AccountError::other(
                    "package does not contain account component metadata section - packages without explicit metadata may be intended for other purposes (e.g., note scripts, transaction scripts)",
                )
            })
    }
}

// SERIALIZATION
// ================================================================================================

impl Serializable for AccountComponentMetadata {
    fn write_into<W: ByteWriter>(&self, target: &mut W) {
        self.name.write_into(target);
        self.description.write_into(target);
        self.version.to_string().write_into(target);
        self.supported_types.write_into(target);
        self.storage_schema.write_into(target);
    }
}

impl Deserializable for AccountComponentMetadata {
    fn read_from<R: ByteReader>(source: &mut R) -> Result<Self, DeserializationError> {
        Ok(Self {
            name: String::read_from(source)?,
            description: String::read_from(source)?,
            version: semver::Version::from_str(&String::read_from(source)?).map_err(
                |err: semver::Error| DeserializationError::InvalidValue(err.to_string()),
            )?,
            supported_types: BTreeSet::<AccountType>::read_from(source)?,
            storage_schema: AccountStorageSchema::read_from(source)?,
        })
    }
}

// TESTS
// ================================================================================================

// #[cfg(test)]
// mod tests {
//     use std::collections::{BTreeMap, BTreeSet};
//     use std::string::ToString;

//     use assert_matches::assert_matches;
//     use miden_assembly::Assembler;
//     use miden_core::utils::{Deserializable, Serializable};
//     use miden_core::{Felt, FieldElement};
//     use semver::Version;

//     use super::{FeltRepresentation, MapRepresentation, test_package_with_metadata};
//     use crate::AccountError;
//     use crate::account::component::template::storage::StorageEntry;
//     use crate::account::component::template::{AccountComponentMetadata, InitStorageData};
//     use crate::account::{AccountComponent, StorageValueName};
//     use crate::errors::AccountComponentTemplateError;
//     use crate::testing::account_code::CODE;

//     fn default_felt_array() -> [FeltRepresentation; 4] {
//         [
//             FeltRepresentation::from(Felt::ZERO),
//             FeltRepresentation::from(Felt::ZERO),
//             FeltRepresentation::from(Felt::ZERO),
//             FeltRepresentation::from(Felt::ZERO),
//         ]
//     }

//     #[test]
//     fn contiguous_value_slots() {
//         let storage = vec![
//             StorageEntry::new_value(0, default_felt_array()),
//             StorageEntry::new_value(1, default_felt_array()),
//         ];

//         let original_config = AccountComponentMetadata {
//             name: "test".into(),
//             description: "desc".into(),
//             version: Version::parse("0.1.0").unwrap(),
//             supported_types: BTreeSet::new(),
//             storage,
//         };

//         let serialized = original_config.to_toml().unwrap();
//         let deserialized = AccountComponentMetadata::from_toml(&serialized).unwrap();
//         assert_eq!(deserialized, original_config);
//     }

//     #[test]
//     fn new_non_contiguous_value_slots() {
//         let storage = vec![
//             StorageEntry::new_value(0, default_felt_array()),
//             StorageEntry::new_value(2, default_felt_array()),
//         ];

//         let result = AccountComponentMetadata::new(
//             "test".into(),
//             "desc".into(),
//             Version::parse("0.1.0").unwrap(),
//             BTreeSet::new(),
//             storage,
//         );
//         assert_matches!(result, Err(AccountComponentTemplateError::NonContiguousSlots(0, 2)));
//     }

//     #[test]
//     fn metadata_binary_serde_roundtrip() {
//         let storage = vec![
//             StorageEntry::new_value(0, default_felt_array()),
//             StorageEntry::new_map(
//                 1,
//                 MapRepresentation::new_template(StorageValueName::new("thresholds").unwrap()),
//             ),
//         ];

//         let component_metadata = AccountComponentMetadata {
//             name: "test".into(),
//             description: "desc".into(),
//             version: Version::parse("0.1.0").unwrap(),
//             supported_types: BTreeSet::new(),
//             storage,
//         };

//         let library = Assembler::default().assemble_library([CODE]).unwrap();
//         let package = test_package_with_metadata("test_package", &library, &component_metadata);
//         let _ = AccountComponent::from_package(&package, &InitStorageData::default()).unwrap();

//         let serialized = component_metadata.to_bytes();
//         let deserialized = AccountComponentMetadata::read_from_bytes(&serialized).unwrap();

//         assert_eq!(deserialized, component_metadata);
//     }

//     #[test]
//     pub fn fail_on_duplicate_key() {
//         let toml_text = r#"
//             name = "Test Component"
//             description = "This is a test component"
//             version = "1.0.1"
//             supported-types = ["FungibleFaucet"]

//             [[storage]]
//             name = "map"
//             description = "A storage map entry"
//             slot = 0
//             values = [
//                 { key = "0x1", value = ["0x3", "0x1", "0x2", "0x3"] },
//                 { key = "0x1", value = ["0x1", "0x2", "0x3", "0x10"] }
//             ]
//         "#;

//         let result = AccountComponentMetadata::from_toml(toml_text);
//         assert_matches!(result,
// Err(AccountComponentTemplateError::StorageMapHasDuplicateKeys(_)));     }

//     #[test]
//     pub fn fail_on_duplicate_placeholder_name() {
//         let toml_text = r#"
//             name = "Test Component"
//             description = "tests for two duplicate placeholders"
//             version = "1.0.1"
//             supported-types = ["FungibleFaucet"]

//             [[storage]]
//             name = "map"
//             slot = 0
//             values = [
//                 { key = "0x1", value = [{type = "felt", name = "test"}, "0x1", "0x2", "0x3"] },
//                 { key = "0x2", value = ["0x1", "0x2", "0x3", {type = "token_symbol", name =
// "test"}] }             ]
//         "#;

//         let result = AccountComponentMetadata::from_toml(toml_text).unwrap_err();
//         assert_matches::assert_matches!(
//             result,
//             AccountComponentTemplateError::DuplicatePlaceholderName(_)
//         );
//     }

//     #[test]
//     pub fn fail_duplicate_key_instance() {
//         let _ = color_eyre::install();

//         let toml_text = r#"
//             name = "Test Component"
//             description = "This is a test component"
//             version = "1.0.1"
//             supported-types = ["FungibleFaucet"]

//             [[storage]]
//             name = "map"
//             description = "A storage map entry"
//             slot = 0
//             values = [
//                 { key = ["0", "0", "0", "1"], value = ["0x9", "0x12", "0x31", "0x18"] },
//                 { key = { name="duplicate_key" }, value = ["0x1", "0x2", "0x3", "0x4"] }
//             ]
//         "#;

//         let metadata = AccountComponentMetadata::from_toml(toml_text).unwrap();
//         let library = Assembler::default().assemble_library([CODE]).unwrap();

//         let package = test_package_with_metadata("test_package", &library, &metadata);

//         // Fail to instantiate on a duplicate key

//         let init_storage_data = InitStorageData::new(
//             [(
//                 StorageValueName::new("map.duplicate_key").unwrap(),
//                 "0x0000000000000000000000000000000000000000000000000100000000000000".to_string(),
//             )],
//             BTreeMap::new(),
//         );
//         let account_component = AccountComponent::from_package(&package, &init_storage_data);
//         assert_matches!(
//             account_component,
//             Err(AccountError::AccountComponentTemplateInstantiationError(
//                 AccountComponentTemplateError::StorageMapHasDuplicateKeys(_)
//             ))
//         );

//         // Successfully instantiate a map (keys are not duplicate)
//         let valid_init_storage_data = InitStorageData::new(
//             [(StorageValueName::new("map.duplicate_key").unwrap(), "0x30".to_string())],
//             BTreeMap::new(),
//         );
//         AccountComponent::from_package(&package, &valid_init_storage_data).unwrap();
//     }
// }
