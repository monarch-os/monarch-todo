//! Optional desktop integration. Never accessed by CLI-only/store commands.
use anyhow::{bail, Context, Result};
use std::{env, fs, path::PathBuf, process::Command};

fn nonempty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

pub fn strings() -> &'static str {
    let preference = nonempty_env("MONARCH_LANGUAGE").or_else(|| {
        let config = nonempty_env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        fs::read_to_string(config.join("monarch/language"))
            .ok()
            .and_then(|text| text.lines().next().map(str::to_owned))
    });
    let language = preference
        .filter(|value| !value.is_empty() && value != "auto")
        .or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .find_map(|key| nonempty_env(key))
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    if language == "fr"
        || ["fr_", "fr-", "fr."]
            .iter()
            .any(|prefix| language.starts_with(prefix))
    {
        include_str!("../i18n/fr.json")
    } else {
        include_str!("../i18n/en.json")
    }
}

pub fn text(key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(strings())
        .ok()
        .and_then(|values| values[key].as_str().map(str::to_owned))
        .unwrap_or_else(|| key.to_owned())
}

pub fn run(action: &str) -> Result<()> {
    let root = nonempty_env("MONARCH_TODO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/monarch-todo"));
    if !root.is_absolute() {
        bail!("MONARCH_TODO_DATA_DIR must be absolute");
    }
    let status = Command::new("/bin/bash")
        .arg(root.join("desktop/integration.sh"))
        .arg(action)
        .arg(&root)
        .status()
        .context("Starting monarch-todo desktop integration")?;
    if !status.success() {
        bail!("Desktop integration failed ({status})");
    }
    Ok(())
}
