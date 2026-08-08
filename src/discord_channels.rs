//! Persistent allowlist for channels where TWLinter should reply.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const DEFAULT_CHANNELS_FILE: &str = "twlinter-channels.json";

#[derive(Debug)]
pub struct ChannelRegistry {
    path: PathBuf,
    channels: Mutex<BTreeSet<u64>>,
}

impl ChannelRegistry {
    pub fn from_env() -> anyhow::Result<Self> {
        let path = std::env::var_os("TWLINTER_CHANNELS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CHANNELS_FILE));
        Self::load(path)
    }

    pub fn load(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let channels = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content)?
        } else {
            BTreeSet::new()
        };
        Ok(Self {
            path,
            channels: Mutex::new(channels),
        })
    }

    pub fn is_enabled(&self, channel_id: u64) -> bool {
        self.channels
            .lock()
            .expect("channel registry mutex is not poisoned")
            .contains(&channel_id)
    }

    pub fn list(&self) -> Vec<u64> {
        self.channels
            .lock()
            .expect("channel registry mutex is not poisoned")
            .iter()
            .copied()
            .collect()
    }

    pub fn enable(&self, channel_id: u64) -> anyhow::Result<bool> {
        self.update(|channels| channels.insert(channel_id))
    }

    pub fn disable(&self, channel_id: u64) -> anyhow::Result<bool> {
        self.update(|channels| channels.remove(&channel_id))
    }

    fn update(&self, change: impl FnOnce(&mut BTreeSet<u64>) -> bool) -> anyhow::Result<bool> {
        let mut channels = self
            .channels
            .lock()
            .expect("channel registry mutex is not poisoned");
        let mut next = channels.clone();
        if !change(&mut next) {
            return Ok(false);
        }
        persist(&self.path, &next)?;
        *channels = next;
        Ok(true)
    }
}

fn persist(path: &Path, channels: &BTreeSet<u64>) -> anyhow::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(channels)?;
    let temporary_path = path.with_extension("tmp");
    fs::write(&temporary_path, format!("{content}\n"))?;
    fs::rename(temporary_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ChannelRegistry;

    #[test]
    fn empty_registry_ignores_unregistered_channels() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ChannelRegistry::load(dir.path().join("channels.json")).unwrap();

        assert!(!registry.is_enabled(42));
        assert!(registry.list().is_empty());
    }

    #[test]
    fn changes_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.json");
        let registry = ChannelRegistry::load(&path).unwrap();

        assert!(registry.enable(42).unwrap());
        assert!(!registry.enable(42).unwrap());
        assert!(registry.enable(7).unwrap());
        assert_eq!(registry.list(), vec![7, 42]);

        let reloaded = ChannelRegistry::load(&path).unwrap();
        assert!(reloaded.is_enabled(42));
        assert!(reloaded.is_enabled(7));
        assert!(reloaded.disable(42).unwrap());
        assert!(!reloaded.is_enabled(42));
    }
}
