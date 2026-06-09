use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Top-level configuration, deserialised from a TOML file.
///
/// At least one messaging channel (`[telegram]` or `[twilio]`) must be present.
/// Both may be active simultaneously.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Presence enables the Telegram channel.
    pub telegram: Option<TelegramConfig>,
    /// Presence enables the Twilio cloud SMS channel.
    pub twilio: Option<TwilioConfig>,
    pub darwin: DarwinConfig,
    pub storage: StorageConfig,
    #[serde(default)]
    pub assets: AssetsConfig,
    #[serde(default)]
    pub polling: PollingConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub kill_switch: KillSwitchConfig,
}

// ---------------------------------------------------------------------------
// Channel configs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub token: String,
    /// When `true`, the user's Telegram display name and @username are stored
    /// in their profile and updated on each message.  Defaults to `false`.
    #[serde(default)]
    pub capture_user_info: bool,
}

/// Twilio cloud SMS channel configuration.
///
/// ```toml
/// [twilio]
/// account_sid        = "ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
/// auth_token         = "your_auth_token"
/// from_number        = "+14155551234"
/// poll_interval_secs = 10                 # optional, default 10
/// ```
///
/// May be used alongside `[telegram]` — both channels run concurrently.
#[derive(Debug, Clone, Deserialize)]
pub struct TwilioConfig {
    /// Twilio Account SID (starts with `AC`).
    pub account_sid: String,
    /// Twilio Auth Token.
    pub auth_token: String,
    /// The Twilio phone number to send from and receive on (E.164 format).
    pub from_number: String,
    /// How often to poll the Twilio API for new inbound messages (seconds).
    #[serde(default = "TwilioConfig::default_poll_interval_secs")]
    pub poll_interval_secs: u64,
}

impl TwilioConfig {
    fn default_poll_interval_secs() -> u64 {
        10
    }
}

// ---------------------------------------------------------------------------
// Core configs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DarwinConfig {
    /// Not required when `simulate = true`.
    #[serde(default)]
    pub token: String,
    #[serde(default = "DarwinConfig::default_endpoint")]
    pub endpoint: String,
    /// When true, use a built-in simulator instead of the real Darwin API.
    #[serde(default)]
    pub simulate: bool,
}

impl DarwinConfig {
    fn default_endpoint() -> String {
        "https://api1.raildata.org.uk/1010-live-departure-board-dep1_2/LDBWS/api/20220120"
            .to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    pub user_data_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetsConfig {
    #[serde(default = "AssetsConfig::default_crs_csv_path")]
    pub crs_csv_path: PathBuf,
}

impl AssetsConfig {
    fn default_crs_csv_path() -> PathBuf {
        PathBuf::from("assets/crs.csv")
    }
}

impl Default for AssetsConfig {
    fn default() -> Self {
        Self {
            crs_csv_path: Self::default_crs_csv_path(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PollingConfig {
    #[serde(default = "PollingConfig::default_interval_seconds")]
    pub interval_seconds: u64,
    /// Number of rows to request from Darwin for the live `/now` display.
    #[serde(default = "PollingConfig::default_departure_rows")]
    pub departure_rows: u32,
    /// Number of rows to request from Darwin per polling call.
    /// Should be larger than `departure_rows` to avoid missing destinations when
    /// filtering client-side.
    #[serde(default = "PollingConfig::default_poll_rows")]
    pub poll_rows: u32,
    /// When `true` (default), the destination filter is passed to the Darwin API
    /// as `filterCrs` so Darwin returns only trains calling at that station.
    /// This is reliable and consistent with the `/now` command.
    ///
    /// Set to `false` to fetch the full origin board once and filter client-side
    /// using `subsequentCallingPoints` data.  This reduces API calls when many
    /// users share the same origin station, but Darwin does not always populate
    /// calling-point data for unfiltered boards, so results may be incomplete.
    #[serde(default = "PollingConfig::default_filter_destination_at_api")]
    pub filter_destination_at_api: bool,
}

impl PollingConfig {
    fn default_interval_seconds() -> u64 {
        60
    }
    fn default_departure_rows() -> u32 {
        10
    }
    fn default_poll_rows() -> u32 {
        149
    }
    fn default_filter_destination_at_api() -> bool {
        true
    }
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            interval_seconds: Self::default_interval_seconds(),
            departure_rows: Self::default_departure_rows(),
            poll_rows: Self::default_poll_rows(),
            filter_destination_at_api: Self::default_filter_destination_at_api(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "LoggingConfig::default_level")]
    pub level: String,
    /// When set, logs are written to daily-rolling files in this directory
    /// instead of stdout.  The directory is created if it does not exist.
    pub log_dir: Option<PathBuf>,
    /// When `true`, ANSI colour codes are emitted.  Disable when piping to a
    /// file or a log aggregator that does not strip escape sequences.
    #[serde(default)]
    pub ansi: bool,
}

impl LoggingConfig {
    fn default_level() -> String {
        "info".to_string()
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: Self::default_level(),
            log_dir: None,
            ansi: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Kill switch config
// ---------------------------------------------------------------------------

/// Controls the `/kill` command that causes the service to exit.
///
/// ```toml
/// [kill_switch]
/// enabled = true   # set to false to disable the command entirely
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KillSwitchConfig {
    /// When `false`, the `/kill` command is disabled and returns "not available".
    #[serde(default = "KillSwitchConfig::default_enabled")]
    pub enabled: bool,
}

impl KillSwitchConfig {
    fn default_enabled() -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load() -> Result<Config> {
    let path = find_config_path()?;
    load_from(&path)
}

pub fn load_from(path: &Path) -> Result<Config> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read config file: {}", path.display()))?;

    let mut cfg: Config =
        toml::from_str(&raw).with_context(|| format!("Invalid TOML in {}", path.display()))?;

    if let Some(dir) = path.parent() {
        resolve_paths(&mut cfg, dir);
    }

    check_file_permissions(path);
    validate(&cfg)?;

    Ok(cfg)
}

fn find_config_path() -> Result<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--config")
        && let Some(p) = args.get(pos + 1)
    {
        return Ok(PathBuf::from(p));
    }

    if let Ok(val) = std::env::var("WAITING_FOR_A_SIGNAL_CONFIG") {
        return Ok(PathBuf::from(val));
    }

    let system = PathBuf::from("/etc/waiting-for-a-signal/config.toml");
    if system.exists() {
        return Ok(system);
    }

    let local = PathBuf::from("config.toml");
    if local.exists() {
        return Ok(local);
    }

    bail!(
        "No config file found. Provide one via --config <path>, \
         $WAITING_FOR_A_SIGNAL_CONFIG, /etc/waiting-for-a-signal/config.toml, or ./config.toml. \
         See config.example.toml for the expected format."
    )
}

fn resolve_paths(cfg: &mut Config, base: &Path) {
    cfg.storage.user_data_dir = resolve(base, &cfg.storage.user_data_dir);
    cfg.assets.crs_csv_path = resolve(base, &cfg.assets.crs_csv_path);
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn validate(cfg: &Config) -> Result<()> {
    if cfg.telegram.is_none() && cfg.twilio.is_none() {
        bail!(
            "At least one messaging channel must be configured. \
             Add a [telegram] section, a [twilio] section, or both."
        );
    }

    if let Some(tg) = &cfg.telegram
        && tg.token.is_empty()
    {
        bail!("[telegram] token must not be empty");
    }

    if let Some(tw) = &cfg.twilio {
        if tw.account_sid.is_empty() {
            bail!("[twilio] account_sid must not be empty");
        }
        if tw.auth_token.is_empty() {
            bail!("[twilio] auth_token must not be empty");
        }
        if tw.from_number.is_empty() {
            bail!("[twilio] from_number must not be empty");
        }
    }

    if !cfg.darwin.simulate && cfg.darwin.token.is_empty() {
        bail!(
            "[darwin] token must not be empty \
             (or set simulate = true for local testing without a Darwin account)"
        );
    }

    Ok(())
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn check_file_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use tracing::warn;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            if mode & 0o004 != 0 {
                warn!(
                    path = %path.display(),
                    "Config file is world-readable (mode {:o}). \
                     Run `chmod 600 {}` to protect your tokens.",
                    mode,
                    path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TG: &str = r#"
[telegram]
token = "123:ABC"

[darwin]
token = "00000000-0000-0000-0000-000000000000"

[storage]
user_data_dir = "/var/lib/waiting-for-a-signal/users"
"#;

    const MINIMAL_TWILIO: &str = r#"
[twilio]
account_sid = "ACtest"
auth_token  = "secret"
from_number = "+14155551234"

[darwin]
token = "00000000-0000-0000-0000-000000000000"

[storage]
user_data_dir = "/var/lib/waiting-for-a-signal/users"
"#;

    #[test]
    fn minimal_telegram_config_parses() {
        let cfg: Config = toml::from_str(MINIMAL_TG).expect("parse failed");
        assert!(cfg.telegram.is_some());
        assert_eq!(cfg.polling.interval_seconds, 60);
        assert_eq!(cfg.polling.departure_rows, 10);
        assert_eq!(cfg.polling.poll_rows, 149);
        assert!(cfg.polling.filter_destination_at_api);
    }

    #[test]
    fn minimal_twilio_config_parses() {
        let cfg: Config = toml::from_str(MINIMAL_TWILIO).expect("parse failed");
        let tw = cfg.twilio.expect("twilio section missing");
        assert_eq!(tw.account_sid, "ACtest");
        assert_eq!(tw.from_number, "+14155551234");
        assert_eq!(tw.poll_interval_secs, 10); // default
    }

    #[test]
    fn validation_rejects_no_channel() {
        let raw = r#"
[darwin]
token = "tok"
[storage]
user_data_dir = "/tmp"
"#;
        let cfg: Config = toml::from_str(raw).expect("parse failed");
        assert!(validate(&cfg).is_err());
    }
}
