use std::collections::HashMap as Map;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum OSCServiceType {
    OSC,
    Query,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum OSCMethodAccessType {
    /// External applications can only write to this value
    Write,
    /// External applications can only read this value
    Read,
    /// External applications can both read from- and write to this value
    ReadWrite,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum OSCMethodValueType {
    Bool,
    Int,
    Float,
    String,
}

impl OSCMethodValueType {
    pub fn osc_type(&self) -> &str {
        match *self {
            OSCMethodValueType::Bool => "F",
            OSCMethodValueType::Int => "i",
            OSCMethodValueType::Float => "f",
            OSCMethodValueType::String => "s",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OSCMethod {
    pub address: String,
    pub ad_type: OSCMethodAccessType,
    /// Only required for "Read" advertisement types.
    pub value_type: Option<OSCMethodValueType>,
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

#[derive(Debug, Clone)]
pub enum OSCQueryInitError {
    AlreadyInitialized,
    OSCQueryinitFailed,
    MDNSDaemonInitFailed(mdns_sd::Error),
    NotYetInitialized,
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

#[derive(Debug)]
pub enum Error {
    IO(std::io::Error),
    LocalIpUnavailable(local_ip_address::Error),
    InitError(OSCQueryInitError),
    IPV4Unavailable(), 
}
