//! The hub's settings: which apps are set up, on which port, and how custom apps are started.

use crate::{files, paths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// When true, AI clients are configured to launch `<app>-cli mcp` directly over stdio
    /// without running a background `crafthub serve` TCP daemon.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
    /// The AI clients apps are registered with, by id. Not set: Claude Code, as before clients
    /// could be chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clients: Option<Vec<String>>,
    /// AI clients the hub doesn't know by itself, as the user (or their AI) described them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_clients: BTreeMap<String, CustomClient>,
    #[serde(default)]
    pub apps: BTreeMap<String, AppConfig>,
}

/// Where an AI client keeps its MCP servers, for a client the hub has no built-in knowledge of.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomClient {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The settings file.
    pub file: String,
    /// `json` (the default) or `toml`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    /// The object (JSON) or table (TOML) holding the servers. Default: `mcpServers`, `mcp_servers`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// JSON only: whether each server entry carries `"type": "stdio"`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub typed: bool,
    /// Whether the client reads its servers only when it starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restart: bool,
}

/// One app that has been set up. Everything but the port is optional: an app from the catalog is
/// found on disk and started the way its built-in profile says.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub port: u16,
    /// Display name, for an app the catalog doesn't know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How to run the app's command-line tool, for a custom app (or to override the one found).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cli: Vec<String>,
    /// Arguments that start the tool as an MCP engine, instead of the built-in profile's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Whether the app works on photo libraries the hub opens for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<bool>,
    /// The app's settings folders, where its recent libraries are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Vec<String>>,
    /// Flags that each take the folder the engine may read and write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_flags: Option<Vec<String>>,
    /// Whether those flags may be repeated, for several folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_root: Option<bool>,
    /// Whether the engine has no file access without a folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_required: Option<bool>,
    /// The folders the engine may read and write. Not set: the home folder. Empty: any file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Vec<String>>,
    /// One folder, as it was saved before an engine could have several. Read, never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub custom: bool,
}

impl Config {
    /// The saved settings; empty when there are none yet or the file can't be read.
    pub fn load() -> Config {
        fs::read_to_string(paths::config_file()).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let file = paths::config_file();
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        files::write_atomic(&file, text.as_bytes()).map_err(|e| format!("cannot write {}: {e}", file.display()))
    }
}
