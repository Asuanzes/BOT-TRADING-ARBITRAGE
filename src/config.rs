use btcbot_core::{MarketConfig, RunMode};
use std::path::Path;

/// Top-level structure that mirrors the full config/markets.toml file.
#[derive(Debug, serde::Deserialize)]
pub struct BotConfig {
    pub mode:    RunMode,
    pub markets: Vec<MarketConfig>,
}

/// Loads the full bot config from a TOML file.
/// Panics at startup if the file is missing or malformed — fail-fast is
/// preferable to trading with a silently wrong configuration.
pub fn load_config(path: &Path) -> BotConfig {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    toml::from_str(&content)
        .unwrap_or_else(|e| panic!("invalid config {}: {e}", path.display()))
}

/// Returns the market entry with the given id.
/// Panics at startup if the id is not present in the loaded list.
pub fn find_market<'a>(markets: &'a [MarketConfig], id: &str) -> &'a MarketConfig {
    markets
        .iter()
        .find(|m| m.id == id)
        .unwrap_or_else(|| panic!("market '{id}' not found in config"))
}
