//! Feature toggles (`features:` in the config file).

use serde::{Deserialize, Serialize};

use crate::speech_config::SpeechConfig;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct FeaturesConfig {
    pub attention_feed: bool,
    /// Show the Claude usage chips (5h / 7d windows per profile) in the toolbar.
    pub claude_usage_bar: bool,
    pub speech: SpeechConfig,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            attention_feed: true,
            claude_usage_bar: true,
            speech: SpeechConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FeaturesConfig;
    use crate::Config;

    #[test]
    fn features_default_has_usage_bar_true_and_old_yaml_loads() {
        assert!(FeaturesConfig::default().claude_usage_bar);
        assert!(Config::default().features.claude_usage_bar);

        // A config written before the flag existed keeps loading with it on.
        let old: Config = serde_yaml::from_str("features:\n  attention_feed: false\n").expect("old yaml parses");
        assert!(old.features.claude_usage_bar);
        assert!(!old.features.attention_feed);
    }

    #[test]
    fn usage_bar_flag_can_be_switched_off_in_yaml() {
        let config: Config = serde_yaml::from_str("features:\n  claude_usage_bar: false\n").expect("yaml parses");
        assert!(!config.features.claude_usage_bar);
    }
}
