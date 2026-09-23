//! Wire compatibility is independent of the executable name and release channel.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Protocol {
    // Missing metadata belongs to records written before V2 was the default.
    #[default]
    V1,
    V2,
}

impl Protocol {
    pub fn is_v2(self) -> bool {
        self == Self::V2
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::V1 => "OpenCode 1 — legacy",
            Self::V2 => "OpenCode",
        }
    }

    /// The harness slug saved connections and client panes carry. One
    /// OpenCode harness now; the protocol travels beside it as its own field.
    /// Records written while V2 was a separate edition say `opencode2`, and
    /// every reader still accepts that spelling.
    pub fn harness(self) -> &'static str {
        "opencode"
    }

    pub fn from_version(version: &str) -> Option<Self> {
        let version = version
            .trim()
            .strip_prefix("opencode v")
            .unwrap_or(version.trim());
        if version.starts_with("0.0.0-beta-") {
            return Some(Self::V2);
        }
        match version.split('.').next()? {
            "1" => Some(Self::V1),
            "2" => Some(Self::V2),
            _ => None,
        }
    }
}
