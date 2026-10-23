//! §35.15: protocol registry — publish, discover and fetch `.aclp`
//! programs. File-backed (like the agent registry): every entry stores
//! the protocol source, its declared roles, and a SHA-256 checksum of
//! the source; fetch verifies the checksum (tamper-evident).

use crate::protocol::{self, ProtocolProgram};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolEntry {
    pub name: String,
    pub version: u32,
    pub roles: Vec<String>,
    pub checksum: String,
    pub source: String,
    pub published_by: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProtocolRegistry {
    entries: BTreeMap<String, ProtocolEntry>,
}

fn checksum(source: &str) -> String {
    use std::fmt::Write;
    let digest = <sha2_impl::Sha256 as sha2_impl::Digest>::digest(source.as_bytes());
    let mut out = String::new();
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// tiny SHA-256 wrapper — uses the same sha2 crate as the rest of the
/// stack when available; a pure-Rust fallback keeps deps unchanged.
mod sha2_impl {
    // FNV-based NOT-SHA256 is NOT acceptable for integrity — use sha2.
    // The crate is already in the tree via other deps; direct dep below.
    pub use sha2::{Digest, Sha256};
}

impl ProtocolRegistry {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    /// parse + check a protocol, then register it (checksummed)
    pub fn publish(
        &mut self,
        source: &str,
        published_by: Option<&str>,
    ) -> Result<(String, u32), String> {
        let prog: ProtocolProgram =
            protocol::parse(source).map_err(|e| format!("parse: {:?}", e))?;
        protocol::check(&prog).map_err(|e| format!("check: {:?}", e))?;
        let entry = ProtocolEntry {
            name: prog.name.clone(),
            version: prog.version,
            roles: prog.roles.clone(),
            checksum: checksum(source),
            source: source.to_string(),
            published_by: published_by.map(|s| s.to_string()),
        };
        let key = format!("{}@v{}", prog.name, prog.version);
        self.entries.insert(key, entry);
        Ok((prog.name, prog.version))
    }

    pub fn fetch(&self, name: &str, version: u32) -> Result<(String, String), String> {
        // (source, checksum) — the caller MUST verify before use
        let key = format!("{name}@v{version}");
        let e = self
            .entries
            .get(&key)
            .ok_or_else(|| format!("protocol '{key}' not registered"))?;
        Ok((e.source.clone(), e.checksum.clone()))
    }

    /// fetch + verify integrity; returns the parsed program
    pub fn fetch_verified(&self, name: &str, version: u32) -> Result<ProtocolProgram, String> {
        let (source, expected) = self.fetch(name, version)?;
        let actual = checksum(&source);
        if actual != expected {
            return Err(format!(
                "checksum mismatch for {name}@v{version}: registry tampered?"
            ));
        }
        protocol::parse(&source).map_err(|e| format!("stored protocol fails to parse: {:?}", e))
    }

    pub fn list(&self) -> Vec<(String, u32, Vec<String>)> {
        self.entries
            .values()
            .map(|e| (e.name.clone(), e.version, e.roles.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROG: &str = "protocol supply v1;\nroles buyer, seller;\non request(\"q\") from buyer -> send propose to buyer;\non propose from seller -> send accept to seller;\non accept from buyer -> begin;\non breach -> send failure to buyer;\non failure from seller -> send bye;\n";

    #[test]
    fn publish_fetch_verify_roundtrip() {
        let mut reg = ProtocolRegistry::default();
        let (name, ver) = reg.publish(PROG, Some("acl-language")).unwrap();
        assert_eq!((name.as_str(), ver), ("supply", 1));
        assert_eq!(reg.len(), 1);
        // fetch verified: parses and integrity-checked
        let prog = reg.fetch_verified("supply", 1).unwrap();
        assert_eq!(prog.roles, vec!["buyer", "seller"]);
        // list
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn tampered_registry_is_detected() {
        let mut reg = ProtocolRegistry::default();
        reg.publish(PROG, None).unwrap();
        // tamper: replace the source with a different protocol
        let key = "supply@v1";
        reg.entries.get_mut(key).unwrap().source =
            "protocol evil v1;\nroles a, b;\non query from a -> send inform to a;\non inform from b -> send bye;\n".into();
        let err = reg.fetch_verified("supply", 1).unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn file_backed_roundtrip() {
        let path = std::env::temp_dir().join("acl-proto-registry-test.json");
        let _ = std::fs::remove_file(&path);
        {
            let mut reg = ProtocolRegistry::load(&path).unwrap();
            assert!(reg.is_empty());
            reg.publish(PROG, None).unwrap();
            reg.save(&path).unwrap();
        }
        {
            let reg = ProtocolRegistry::load(&path).unwrap();
            assert_eq!(reg.len(), 1);
            reg.fetch_verified("supply", 1).unwrap();
        }
        let _ = std::fs::remove_file(&path);
    }
}
