use std::collections::HashMap as Map;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum OSCServiceType {
    OSC,
    Query,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum OSCAddressAdType {
    /// External applications can write all values in this address tree.
    /// (e.g. /avatar would accept writes for it and anything under it, like /avatar/parameters/VRCEmote)
    WriteAll,
    /// External applications can only write to this value
    WriteValue,
    /// External applications can only read this value
    ReadValue,
    /// External applications can both read from- and write to this value
    ReadWriteValue,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum OSCAddressValueType {
    Bool,
    Int,
    Float,
}

impl OSCAddressValueType {
    pub fn osc_type(&self) -> &str {
        match *self {
            OSCAddressValueType::Bool => "F",
            OSCAddressValueType::Int => "i",
            OSCAddressValueType::Float => "f",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OSCAddressAd {
    pub address: String,
    pub ad_type: OSCAddressAdType,
    /// Only required for "Read" advertisement types.
    pub value_type: Option<OSCAddressValueType>,
    /// Only required for "Read" advertisement types. (Serialized)
    pub value: Option<String>,
    //, Optional human readable description
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct OSCQueryNode {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub full_path: String,
    pub access: u8,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub contents: Map<String, OSCQueryNode>,
    #[serde(alias = "TYPE", skip_serializing_if = "Option::is_none")]
    pub value_type: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub value: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum OSCQueryInitError {
    AlreadyInitialized,
    OSCQueryinitFailed,
    MDNSDaemonInitFailed,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct OSCQueryHostInfo {
    pub name: String,
    pub osc_transport: String,
    pub osc_ip: String,
    pub osc_port: u16,
    pub extensions: Map<String, bool>,
}
