use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Provider;

    #[test]
    fn provider_names_are_closed_lowercase_values_with_fixed_executables() {
        assert_eq!(
            serde_json::to_string(&Provider::Claude).unwrap(),
            "\"claude\""
        );
        assert_eq!(
            serde_json::to_string(&Provider::Codex).unwrap(),
            "\"codex\""
        );
        assert_eq!(Provider::Claude.executable(), "claude");
        assert_eq!(Provider::Codex.executable(), "codex");
        assert!(serde_json::from_str::<Provider>("\"gemini\"").is_err());
    }
}
