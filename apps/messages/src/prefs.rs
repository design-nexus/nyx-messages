//! Local preferences for the Messages shell.

use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub mode: nyx_theme::ThemeMode,
    pub theme: String,
    pub glow: bool,
    pub reduce_motion: bool,
    pub filter: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { mode: nyx_theme::ThemeMode::Omarchy, theme: String::new(), glow: true, reduce_motion: false, filter: "all".into() }
    }
}

static PREFS: RwLock<Option<Prefs>> = RwLock::new(None);

fn path() -> std::path::PathBuf {
    nyx_core::paths::config_home().join("nyx-messages").join("messages.json")
}

pub fn get() -> Prefs {
    if let Ok(g) = PREFS.read()
        && let Some(p) = g.as_ref()
    {
        return p.clone();
    }
    let loaded: Prefs = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if let Ok(mut g) = PREFS.write() {
        *g = Some(loaded.clone());
    }
    loaded
}

pub fn update(f: impl FnOnce(&mut Prefs)) {
    let mut p = get();
    f(&mut p);
    if let Ok(mut g) = PREFS.write() {
        *g = Some(p.clone());
    }
    if let Some(parent) = path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(t) = serde_json::to_string_pretty(&p) {
        let _ = std::fs::write(path(), t);
    }
}
