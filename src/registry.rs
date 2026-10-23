//! Local agent registry (spec roadmap): discovery for same-machine agents
//! without a directory service. File-backed JSON at a caller-chosen path.

use crate::AgentCard;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryEntry {
    pub name: String,
    pub aid: String,
    /// hex-encoded Ed25519 public key (32 bytes = 64 hex chars)
    pub public_key: String,
    pub capabilities: Vec<String>,
    pub registered_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    agents: Vec<RegistryEntry>,
}

#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn open(path: impl AsRef<Path>) -> Self {
        Registry { path: path.as_ref().to_path_buf() }
    }

    fn load(&self) -> RegistryFile {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, f: &RegistryFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec_pretty(f).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }

    pub fn register(&self, entry: RegistryEntry) -> Result<(), String> {
        let mut f = self.load();
        f.agents.retain(|a| a.name != entry.name);
        f.agents.push(entry);
        self.save(&f)
    }

    pub fn lookup(&self, name: &str) -> Option<RegistryEntry> {
        self.load().agents.into_iter().find(|a| a.name == name)
    }

    pub fn by_capability(&self, cap: &str) -> Vec<RegistryEntry> {
        self.load()
            .agents
            .into_iter()
            .filter(|a| a.capabilities.iter().any(|c| c == cap))
            .collect()
    }

    pub fn list(&self) -> Vec<RegistryEntry> {
        self.load().agents
    }

    pub fn unregister(&self, name: &str) -> Result<bool, String> {
        let mut f = self.load();
        let before = f.agents.len();
        f.agents.retain(|a| a.name != name);
        let removed = f.agents.len() < before;
        if removed {
            self.save(&f)?;
        }
        Ok(removed)
    }

    // ------------------------------------------------ AgentCard integration

    /// Build a registry entry from a signed AgentCard (§7). The card is the
    /// source of truth: name, endpoint (as aid), capabilities, public key.
    pub fn entry_from_card(card: &AgentCard) -> RegistryEntry {
        RegistryEntry {
            name: card.name.clone(),
            aid: card.endpoint.0.clone(),
            public_key: card
                .public_key
                .as_ref()
                .map(|pk| pk.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .unwrap_or_default(),
            capabilities: card.capabilities.iter().map(|c| c.name.clone()).collect(),
            registered_at_ms: 0,
        }
    }

    /// Register an agent directly from its AgentCard (replaces by name).
    pub fn register_card(&self, card: &AgentCard) -> Result<(), String> {
        self.register(Self::entry_from_card(card))
    }

    /// Discovery: agents with a capability, returned as best-effort
    /// AgentCards reconstructed from the registry (public key restored
    /// when present; signature NOT carried — verify at contact time).
    pub fn discover_cards(&self, cap: &str) -> Vec<AgentCard> {
        self.by_capability(cap)
            .into_iter()
            .map(|e| {
                let pk = (0..e.public_key.len())
                    .step_by(2)
                    .filter_map(|i| e.public_key.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
                    .collect::<Vec<u8>>();
                AgentCard {
                    name: e.name.clone(),
                    version: "1".into(),
                    description: format!("discovered via registry ({})", e.aid),
                    endpoint: crate::Aid(e.aid.clone()),
                    capabilities: e
                        .capabilities
                        .iter()
                        .map(|c| crate::Capability {
                            name: c.clone(),
                            input_schema: None,
                            output_schema: None,
                            examples: None,
                        })
                        .collect(),
                    auth_scheme: "ed25519".into(),
                    key_id: None,
                    public_key: if pk.len() == 32 { Some(pk) } else { None },
                    rate_hint: None,
                    valid_until: None,
                    sig: None,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("acl-registry-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn entry(name: &str, caps: &[&str]) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            aid: format!("local:1:{name}"),
            public_key: "ab".repeat(32),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            registered_at_ms: 0,
        }
    }

    #[test]
    fn register_lookup_list_unregister() {
        let path = tmp("a");
        let r = Registry::open(&path);
        r.register(entry("argus", &["log.watch"])).unwrap();
        r.register(entry("kenji", &["code"])).unwrap();
        assert_eq!(r.lookup("argus").unwrap().aid, "local:1:argus");
        assert!(r.lookup("nope").is_none());
        assert_eq!(r.list().len(), 2);
        assert_eq!(r.by_capability("code").len(), 1);
        assert!(r.unregister("argus").unwrap());
        assert_eq!(r.list().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn re_register_replaces() {
        let path = tmp("b");
        let r = Registry::open(&path);
        r.register(entry("kenji", &["code"])).unwrap();
        let mut e2 = entry("kenji", &["code", "review"]);
        e2.aid = "local:2:kenji".into();
        r.register(e2).unwrap();
        assert_eq!(r.list().len(), 1);
        assert_eq!(r.lookup("kenji").unwrap().aid, "local:2:kenji");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn register_card_and_discover() {
        use crate::Capability;
        let path = tmp("d");
        let r = Registry::open(&path);
        let card = AgentCard {
            name: "argus".into(),
            version: "1.0".into(),
            description: "watcher".into(),
            endpoint: crate::Aid::new("http://127.0.0.1:9200"),
            capabilities: vec![Capability {
                name: "log.watch".into(),
                input_schema: Some(serde_json::json!({"type": "object"})),
                output_schema: None,
                examples: None,
            }],
            auth_scheme: "ed25519".into(),
            key_id: None,
            public_key: Some((0u8..32).collect()),
            rate_hint: None,
            valid_until: None,
            sig: None,
        };
        r.register_card(&card).unwrap();
        assert_eq!(r.lookup("argus").unwrap().capabilities, vec!["log.watch"]);
        // discovery by capability returns reconstructed cards
        let found = r.discover_cards("log.watch");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "argus");
        assert_eq!(found[0].capabilities[0].name, "log.watch");
        assert!(found[0].public_key.is_some());
        // no capability match -> empty
        assert!(r.discover_cards("nope").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn persists_across_instances() {
        let path = tmp("c");
        Registry::open(&path).register(entry("balboa", &["run"])).unwrap();
        let r2 = Registry::open(&path);
        assert!(r2.lookup("balboa").is_some());
        let _ = std::fs::remove_file(&path);
    }
}
