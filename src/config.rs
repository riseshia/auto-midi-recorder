#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub device_name: String,
    pub output_dir: std::path::PathBuf,
    #[serde(default = "default_idle_timeout_minutes")]
    pub idle_timeout_minutes: u64,
}

fn default_idle_timeout_minutes() -> u64 {
    30
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("invalid {}: {e}", path.display()))
    }

    fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if config.device_name.is_empty() {
            // An empty pattern would match every port on the machine.
            return Err("device_name must not be empty".to_string());
        }
        if config.idle_timeout_minutes == 0 {
            return Err("idle_timeout_minutes must be at least 1".to_string());
        }
        Ok(config)
    }

    pub fn idle_timeout_micros(&self) -> u64 {
        self.idle_timeout_minutes.saturating_mul(60_000_000)
    }

    pub fn matches_port(&self, port_name: &str) -> bool {
        port_name
            .to_lowercase()
            .contains(&self.device_name.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn idle_timeout_defaults_to_thirty_minutes() {
        let config =
            crate::config::Config::parse("device_name = 'FP-90X'\noutput_dir = 'x'").unwrap();
        assert_eq!(config.idle_timeout_minutes, 30);
        assert_eq!(config.idle_timeout_micros(), 1_800_000_000);
    }

    #[test]
    fn device_name_and_output_dir_are_required() {
        assert!(crate::config::Config::parse("output_dir = 'x'").is_err());
        assert!(crate::config::Config::parse("device_name = 'x'").is_err());
    }

    #[test]
    fn empty_device_name_is_rejected() {
        assert!(crate::config::Config::parse("device_name = ''\noutput_dir = 'x'").is_err());
    }

    #[test]
    fn zero_idle_timeout_is_rejected() {
        let text = "device_name = 'x'\noutput_dir = 'x'\nidle_timeout_minutes = 0";
        assert!(crate::config::Config::parse(text).is_err());
    }

    #[test]
    fn port_match_ignores_case_and_allows_substring() {
        let config =
            crate::config::Config::parse("device_name = 'fp-90x'\noutput_dir = 'x'").unwrap();
        assert!(config.matches_port("2- FP-90X MIDI 1"));
        assert!(!config.matches_port("FP-30X"));
    }
}
