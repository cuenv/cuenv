//! Hand-written prost bindings for the subset of the Terraform plugin
//! protocol (tfplugin5 and tfplugin6) that cuenv drives.
//!
//! The upstream `.proto` files live in `hashicorp/terraform` under
//! `docs/plugin-protocol/`. Field tags below mirror those files exactly;
//! anything cuenv does not read is left out, which protobuf decoding
//! tolerates by skipping unknown fields.
//!
//! Protocols 5 and 6 are wire-identical for every managed-resource RPC
//! used here. They differ in RPC names (see [`crate::plugin`]) and in
//! `Schema.Attribute`, where tag 10 is `write_only` in protocol 5 but the
//! nested attribute type in protocol 6, so schema messages exist once per
//! protocol.
//!
//! The `required` attribute flag is not decoded: required is implied by
//! neither optional nor computed. The attribute flags added after
//! `sensitive` (`deprecated`, `write_only`) are decoded as `Option<bool>`:
//! absent and `false` mean the same. `write_only`, descriptions, deprecation and
//! nested block item bounds are decoded for the generated CUE types (see
//! [`crate::cue_types`]); cuenv advertises no write-only support. Data source,
//! function and ephemeral resource schemas are not decoded. Of the provider's
//! `ServerCapabilities` only `plan_destroy` is decoded; it makes cuenv plan
//! every destroy.

use std::collections::HashMap;

/// Opaque encoding of a Terraform value. cuenv always sends MessagePack.
#[derive(Clone, PartialEq, prost::Message)]
pub struct DynamicValue {
    #[prost(bytes = "vec", tag = "1")]
    pub message_pack: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub json: Vec<u8>,
}

/// Diagnostic severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum Severity {
    Invalid = 0,
    Error = 1,
    Warning = 2,
}

/// A warning or error reported by a provider.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Diagnostic {
    #[prost(enumeration = "Severity", tag = "1")]
    pub severity: i32,
    #[prost(string, tag = "2")]
    pub summary: String,
    #[prost(string, tag = "3")]
    pub detail: String,
    #[prost(message, optional, tag = "4")]
    pub attribute: Option<AttributePath>,
}

/// Path to an attribute within a value.
#[derive(Clone, PartialEq, prost::Message)]
pub struct AttributePath {
    #[prost(message, repeated, tag = "1")]
    pub steps: Vec<AttributePathStep>,
}

/// One step of an [`AttributePath`].
#[derive(Clone, PartialEq, prost::Message)]
pub struct AttributePathStep {
    #[prost(oneof = "Selector", tags = "1, 2, 3")]
    pub selector: Option<Selector>,
}

/// Selector of an [`AttributePathStep`].
#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Selector {
    #[prost(string, tag = "1")]
    AttributeName(String),
    #[prost(string, tag = "2")]
    ElementKeyString(String),
    #[prost(int64, tag = "3")]
    ElementKeyInt(i64),
}

/// Stored state handed to `UpgradeResourceState`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct RawState {
    #[prost(bytes = "vec", tag = "1")]
    pub json: Vec<u8>,
    #[prost(map = "string, string", tag = "2")]
    pub flatmap: HashMap<String, String>,
}

/// Features the client (cuenv) supports.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ClientCapabilities {
    #[prost(bool, tag = "1")]
    pub deferral_allowed: bool,
    #[prost(bool, tag = "2")]
    pub write_only_attributes_allowed: bool,
}

/// Optional protocol features a provider supports, reported with its
/// schema (tag 6 of `GetProviderSchema.Response` in both protocols).
#[derive(Clone, PartialEq, prost::Message)]
pub struct ServerCapabilities {
    /// The provider expects `PlanResourceChange` for every destroy.
    #[prost(bool, tag = "1")]
    pub plan_destroy: bool,
}

/// Nesting mode shared by nested blocks (protocols 5 and 6) and nested
/// attributes (protocol 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum NestingMode {
    Invalid = 0,
    Single = 1,
    List = 2,
    Set = 3,
    Map = 4,
    Group = 5,
}

/// Empty request message (`GetProviderSchema`, `StopProvider`).
#[derive(Clone, PartialEq, prost::Message)]
pub struct Empty {}

/// `StopProvider.Response` / `Stop.Response`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct StopResponse {
    #[prost(string, tag = "1")]
    pub error: String,
}

/// `ValidateProviderConfig.Request` (protocol 6) / `PrepareProviderConfig.Request` (protocol 5).
#[derive(Clone, PartialEq, prost::Message)]
pub struct ValidateProviderConfigurationRequest {
    #[prost(message, optional, tag = "1")]
    pub configuration: Option<DynamicValue>,
}

/// `ValidateProviderConfig.Response` (protocol 6) / `PrepareProviderConfig.Response` (protocol 5).
///
/// Protocol 5 additionally returns `prepared_config` at tag 1, which cuenv ignores.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ValidateProviderConfigurationResponse {
    #[prost(message, repeated, tag = "2")]
    pub diagnostics: Vec<Diagnostic>,
}

/// `ConfigureProvider.Request` (protocol 6) / `Configure.Request` (protocol 5).
#[derive(Clone, PartialEq, prost::Message)]
pub struct ConfigureProviderRequest {
    #[prost(string, tag = "1")]
    pub terraform_version: String,
    #[prost(message, optional, tag = "2")]
    pub configuration: Option<DynamicValue>,
    #[prost(message, optional, tag = "3")]
    pub client_capabilities: Option<ClientCapabilities>,
}

/// Response carrying only diagnostics at tag 1.
#[derive(Clone, PartialEq, prost::Message)]
pub struct DiagnosticsResponse {
    #[prost(message, repeated, tag = "1")]
    pub diagnostics: Vec<Diagnostic>,
}

/// `ValidateResourceConfig.Request` (protocol 6) / `ValidateResourceTypeConfig.Request` (protocol 5).
#[derive(Clone, PartialEq, prost::Message)]
pub struct ValidateResourceConfigurationRequest {
    #[prost(string, tag = "1")]
    pub type_name: String,
    #[prost(message, optional, tag = "2")]
    pub configuration: Option<DynamicValue>,
    #[prost(message, optional, tag = "3")]
    pub client_capabilities: Option<ClientCapabilities>,
}

/// `UpgradeResourceState.Request`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct UpgradeResourceStateRequest {
    #[prost(string, tag = "1")]
    pub type_name: String,
    #[prost(int64, tag = "2")]
    pub version: i64,
    #[prost(message, optional, tag = "3")]
    pub raw_state: Option<RawState>,
}

/// `UpgradeResourceState.Response`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct UpgradeResourceStateResponse {
    #[prost(message, optional, tag = "1")]
    pub upgraded_state: Option<DynamicValue>,
    #[prost(message, repeated, tag = "2")]
    pub diagnostics: Vec<Diagnostic>,
}

/// `ReadResource.Request`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ReadResourceRequest {
    #[prost(string, tag = "1")]
    pub type_name: String,
    #[prost(message, optional, tag = "2")]
    pub current_state: Option<DynamicValue>,
    #[prost(bytes = "vec", tag = "3")]
    pub private: Vec<u8>,
    #[prost(message, optional, tag = "5")]
    pub client_capabilities: Option<ClientCapabilities>,
}

/// `Deferred`: the provider could not act yet and asks the client to retry
/// later. cuenv advertises no deferral support, so any deferral is an error.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Deferred {
    #[prost(int32, tag = "1")]
    pub reason: i32,
}

/// `ReadResource.Response`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ReadResourceResponse {
    #[prost(message, optional, tag = "1")]
    pub new_state: Option<DynamicValue>,
    #[prost(message, repeated, tag = "2")]
    pub diagnostics: Vec<Diagnostic>,
    #[prost(bytes = "vec", tag = "3")]
    pub private: Vec<u8>,
    #[prost(message, optional, tag = "4")]
    pub deferred: Option<Deferred>,
}

/// `PlanResourceChange.Request`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PlanResourceChangeRequest {
    #[prost(string, tag = "1")]
    pub type_name: String,
    #[prost(message, optional, tag = "2")]
    pub prior_state: Option<DynamicValue>,
    #[prost(message, optional, tag = "3")]
    pub proposed_new_state: Option<DynamicValue>,
    #[prost(message, optional, tag = "4")]
    pub configuration: Option<DynamicValue>,
    #[prost(bytes = "vec", tag = "5")]
    pub prior_private: Vec<u8>,
    #[prost(message, optional, tag = "7")]
    pub client_capabilities: Option<ClientCapabilities>,
}

/// `PlanResourceChange.Response`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct PlanResourceChangeResponse {
    #[prost(message, optional, tag = "1")]
    pub planned_state: Option<DynamicValue>,
    #[prost(message, repeated, tag = "2")]
    pub requires_replace: Vec<AttributePath>,
    #[prost(bytes = "vec", tag = "3")]
    pub planned_private: Vec<u8>,
    #[prost(message, repeated, tag = "4")]
    pub diagnostics: Vec<Diagnostic>,
    #[prost(bool, tag = "5")]
    pub legacy_type_system: bool,
    #[prost(message, optional, tag = "6")]
    pub deferred: Option<Deferred>,
}

/// `ApplyResourceChange.Request`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ApplyResourceChangeRequest {
    #[prost(string, tag = "1")]
    pub type_name: String,
    #[prost(message, optional, tag = "2")]
    pub prior_state: Option<DynamicValue>,
    #[prost(message, optional, tag = "3")]
    pub planned_state: Option<DynamicValue>,
    #[prost(message, optional, tag = "4")]
    pub configuration: Option<DynamicValue>,
    #[prost(bytes = "vec", tag = "5")]
    pub planned_private: Vec<u8>,
}

/// `ApplyResourceChange.Response`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ApplyResourceChangeResponse {
    #[prost(message, optional, tag = "1")]
    pub new_state: Option<DynamicValue>,
    #[prost(bytes = "vec", tag = "2")]
    pub private: Vec<u8>,
    #[prost(message, repeated, tag = "3")]
    pub diagnostics: Vec<Diagnostic>,
    #[prost(bool, tag = "4")]
    pub legacy_type_system: bool,
}

/// Schema messages for protocol 6.
pub mod version6 {
    use super::{Diagnostic, NestingMode, ServerCapabilities};
    use std::collections::HashMap;

    /// `GetProviderSchema.Response`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct GetProviderSchemaResponse {
        #[prost(message, optional, tag = "1")]
        pub provider: Option<Schema>,
        #[prost(map = "string, message", tag = "2")]
        pub resource_schemas: HashMap<String, Schema>,
        #[prost(message, repeated, tag = "4")]
        pub diagnostics: Vec<Diagnostic>,
        #[prost(message, optional, tag = "6")]
        pub server_capabilities: Option<ServerCapabilities>,
    }

    /// `Schema`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Schema {
        #[prost(int64, tag = "1")]
        pub version: i64,
        #[prost(message, optional, tag = "2")]
        pub block: Option<Block>,
    }

    /// `Schema.Block`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Block {
        #[prost(message, repeated, tag = "2")]
        pub attributes: Vec<Attribute>,
        #[prost(message, repeated, tag = "3")]
        pub nested_blocks: Vec<NestedBlock>,
        #[prost(string, tag = "4")]
        pub description: String,
        #[prost(bool, tag = "6")]
        pub deprecated: bool,
        #[prost(string, tag = "7")]
        pub deprecation_message: String,
    }

    /// `Schema.Attribute`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Attribute {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(bytes = "vec", tag = "2")]
        pub r#type: Vec<u8>,
        #[prost(message, optional, tag = "10")]
        pub nested_type: Option<Object>,
        #[prost(bool, tag = "5")]
        pub optional: bool,
        #[prost(bool, tag = "6")]
        pub computed: bool,
        #[prost(bool, tag = "7")]
        pub sensitive: bool,
        #[prost(string, tag = "3")]
        pub description: String,
        #[prost(bool, optional, tag = "9")]
        pub deprecated: Option<bool>,
        #[prost(bool, optional, tag = "11")]
        pub write_only: Option<bool>,
        #[prost(string, tag = "12")]
        pub deprecation_message: String,
    }

    /// `Schema.NestedBlock`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct NestedBlock {
        #[prost(string, tag = "1")]
        pub type_name: String,
        #[prost(message, optional, tag = "2")]
        pub block: Option<Block>,
        #[prost(enumeration = "NestingMode", tag = "3")]
        pub nesting: i32,
        #[prost(int64, tag = "4")]
        pub min_items: i64,
        #[prost(int64, tag = "5")]
        pub max_items: i64,
    }

    /// `Schema.Object` (nested attribute type).
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Object {
        #[prost(message, repeated, tag = "1")]
        pub attributes: Vec<Attribute>,
        #[prost(enumeration = "NestingMode", tag = "3")]
        pub nesting: i32,
    }
}

/// Schema messages for protocol 5.
pub mod version5 {
    use super::{Diagnostic, NestingMode, ServerCapabilities};
    use std::collections::HashMap;

    /// `GetProviderSchema.Response`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct GetProviderSchemaResponse {
        #[prost(message, optional, tag = "1")]
        pub provider: Option<Schema>,
        #[prost(map = "string, message", tag = "2")]
        pub resource_schemas: HashMap<String, Schema>,
        #[prost(message, repeated, tag = "4")]
        pub diagnostics: Vec<Diagnostic>,
        #[prost(message, optional, tag = "6")]
        pub server_capabilities: Option<ServerCapabilities>,
    }

    /// `Schema`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Schema {
        #[prost(int64, tag = "1")]
        pub version: i64,
        #[prost(message, optional, tag = "2")]
        pub block: Option<Block>,
    }

    /// `Schema.Block`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Block {
        #[prost(message, repeated, tag = "2")]
        pub attributes: Vec<Attribute>,
        #[prost(message, repeated, tag = "3")]
        pub nested_blocks: Vec<NestedBlock>,
        #[prost(string, tag = "4")]
        pub description: String,
        #[prost(bool, tag = "6")]
        pub deprecated: bool,
        #[prost(string, tag = "7")]
        pub deprecation_message: String,
    }

    /// `Schema.Attribute`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Attribute {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(bytes = "vec", tag = "2")]
        pub r#type: Vec<u8>,
        #[prost(bool, tag = "5")]
        pub optional: bool,
        #[prost(bool, tag = "6")]
        pub computed: bool,
        #[prost(bool, tag = "7")]
        pub sensitive: bool,
        #[prost(string, tag = "3")]
        pub description: String,
        #[prost(bool, optional, tag = "9")]
        pub deprecated: Option<bool>,
        #[prost(bool, optional, tag = "10")]
        pub write_only: Option<bool>,
        #[prost(string, tag = "11")]
        pub deprecation_message: String,
    }

    /// `Schema.NestedBlock`.
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct NestedBlock {
        #[prost(string, tag = "1")]
        pub type_name: String,
        #[prost(message, optional, tag = "2")]
        pub block: Option<Block>,
        #[prost(enumeration = "NestingMode", tag = "3")]
        pub nesting: i32,
        #[prost(int64, tag = "4")]
        pub min_items: i64,
        #[prost(int64, tag = "5")]
        pub max_items: i64,
    }
}
