// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Persistent KeyStore and Bonding Keys for Bluetooth Low Energy Pairing & SMP.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::types::SimbleError;

/// Hex-serialized key representation for a JSON-backed keystore (see
/// [`KeyStore`]'s `to_json`/`from_json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingKey {
    #[serde(
        serialize_with = "serialize_hex_16",
        deserialize_with = "deserialize_hex_16"
    )]
    pub value: [u8; 16],
    #[serde(default)]
    pub authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ediv: Option<u16>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_opt_hex_8",
        deserialize_with = "deserialize_opt_hex_8",
        default
    )]
    pub rand: Option<[u8; 8]>,
}

impl PairingKey {
    pub fn new(value: [u8; 16]) -> Self {
        Self {
            value,
            authenticated: false,
            ediv: None,
            rand: None,
        }
    }
}

fn serialize_hex_16<S>(val: &[u8; 16], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&hex_encode(val))
}

fn deserialize_hex_16<'de, D>(deserializer: D) -> Result<[u8; 16], D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let bytes = hex_decode(&s).map_err(serde::de::Error::custom)?;
    if bytes.len() != 16 {
        return Err(serde::de::Error::custom("Expected 16 bytes for key"));
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

fn serialize_opt_hex_8<S>(val: &Option<[u8; 8]>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match val {
        Some(b) => serializer.serialize_some(&hex_encode(b)),
        None => serializer.serialize_none(),
    }
}

fn deserialize_opt_hex_8<'de, D>(deserializer: D) -> Result<Option<[u8; 8]>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    match opt {
        Some(s) => {
            let bytes = hex_decode(&s).map_err(serde::de::Error::custom)?;
            if bytes.len() != 8 {
                return Err(serde::de::Error::custom("Expected 8 bytes for rand"));
            }
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&bytes);
            Ok(Some(arr))
        }
        None => Ok(None),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let clean = s.trim();
    if !clean.len().is_multiple_of(2) {
        return Err("Odd length hex string".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Set of security and bonding keys for a connected peer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingKeys {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_type: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ltk: Option<PairingKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ltk_central: Option<PairingKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ltk_peripheral: Option<PairingKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub irk: Option<PairingKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrk: Option<PairingKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_key: Option<PairingKey>,
}

/// Persistent and namespaced KeyStore for managing bonded devices.
#[derive(Debug, Default, Clone)]
pub struct KeyStore {
    default_namespace: String,
    store: Arc<RwLock<HashMap<String, HashMap<String, PairingKeys>>>>,
}

impl KeyStore {
    pub const DEFAULT_NAMESPACE: &'static str = "__DEFAULT__";

    pub fn new(default_namespace: Option<&str>) -> Self {
        Self {
            default_namespace: default_namespace
                .unwrap_or(Self::DEFAULT_NAMESPACE)
                .to_string(),
            store: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Loads a KeyStore from a JSON formatted string.
    pub fn from_json(json_str: &str, default_namespace: Option<&str>) -> Result<Self, SimbleError> {
        let parsed: HashMap<String, HashMap<String, PairingKeys>> =
            serde_json::from_str(json_str).map_err(|e| SimbleError::SmpError(e.to_string()))?;

        Ok(Self {
            default_namespace: default_namespace
                .unwrap_or(Self::DEFAULT_NAMESPACE)
                .to_string(),
            store: Arc::new(RwLock::new(parsed)),
        })
    }

    /// Serializes the KeyStore to a formatted JSON string.
    pub fn to_json(&self) -> Result<String, SimbleError> {
        let store = self.store.read().unwrap();
        serde_json::to_string_pretty(&*store).map_err(|e| SimbleError::SmpError(e.to_string()))
    }

    /// Retrieves keys for a given peer name in a namespace.
    pub fn get(&self, name: &str) -> Option<PairingKeys> {
        let store = self.store.read().unwrap();
        // 1. Try default namespace
        if let Some(ns_map) = store.get(&self.default_namespace)
            && let Some(keys) = ns_map.get(name)
        {
            return Some(keys.clone());
        }
        // 2. Search all namespaces
        for ns_map in store.values() {
            if let Some(keys) = ns_map.get(name) {
                return Some(keys.clone());
            }
        }
        None
    }

    /// Updates or inserts pairing keys for a peer.
    pub fn update(&self, name: &str, keys: PairingKeys) {
        let mut store = self.store.write().unwrap();
        store
            .entry(self.default_namespace.clone())
            .or_default()
            .insert(name.to_string(), keys);
    }

    /// Retrieves all keys in the store.
    pub fn get_all(&self) -> Vec<(String, PairingKeys)> {
        let store = self.store.read().unwrap();
        let mut res = Vec::new();
        if let Some(ns_map) = store.get(&self.default_namespace) {
            for (k, v) in ns_map {
                res.push((k.clone(), v.clone()));
            }
        } else {
            for ns_map in store.values() {
                for (k, v) in ns_map {
                    res.push((k.clone(), v.clone()));
                }
            }
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keystore_in_memory_basic_flow() {
        let ks = KeyStore::new(Some("my_namespace"));
        assert_eq!(ks.get_all().len(), 0);

        let mut keys = PairingKeys::default();
        let ltk_bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        keys.ltk = Some(PairingKey::new(ltk_bytes));
        ks.update("peer1", keys);

        let retrieved = ks.get("peer1").expect("keys found");
        assert_eq!(retrieved.ltk.unwrap().value, ltk_bytes);
    }
}
