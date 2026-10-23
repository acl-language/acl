//! Local agent registry (spec roadmap): discovery for same-machine agents
//! without a directory service. File-backed JSON at a caller-chosen path.

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
    fn persists_across_instances() {
        let path = tmp("c");
        Registry::open(&path).register(entry("balboa", &["run"])).unwrap();
        let r2 = Registry::open(&path);
        assert!(r2.lookup("balboa").is_some());
        let _ = std::fs::remove_file(&path);
    }
}
