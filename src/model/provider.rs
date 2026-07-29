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

    pub fn other(self) -> Self {
        match self {
            Self::Claude => Self::Codex,
            Self::Codex => Self::Claude,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    #[default]
    Auto,
    Cli,
    Desktop,
}

#[cfg(test)]
mod tests {
    use super::{Provider, Surface};

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

    #[test]
    fn other_returns_the_opposite_variant() {
        assert_eq!(Provider::Claude.other(), Provider::Codex);
        assert_eq!(Provider::Codex.other(), Provider::Claude);
    }

    #[test]
    fn surface_names_are_closed_lowercase_values() {
        assert_eq!(serde_json::to_string(&Surface::Auto).unwrap(), "\"auto\"");
        assert_eq!(serde_json::to_string(&Surface::Cli).unwrap(), "\"cli\"");
        assert_eq!(
            serde_json::to_string(&Surface::Desktop).unwrap(),
            "\"desktop\""
        );
        assert_eq!(Surface::default(), Surface::Auto);
        assert!(serde_json::from_str::<Surface>("\"tui\"").is_err());
    }
}
