use std::path::Path;

/// Authoring language for `.railway/railway.{ts,py,go}`.
///
/// Never inferred from the application repo (`package.json`, `go.mod`, …).
/// Once one of those files exists, every command that writes config must use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthoringLang {
    TypeScript,
    Python,
    Go,
}

impl AuthoringLang {
    pub(super) fn from_path(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => Some(Self::TypeScript),
            "py" => Some(Self::Python),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    pub(super) fn file_name(self) -> &'static str {
        match self {
            Self::TypeScript => "railway.ts",
            Self::Python => "railway.py",
            Self::Go => "railway.go",
        }
    }

    pub(super) fn helper(self, name: &str) -> String {
        match self {
            Self::Go => format!("iac.{}", go_helper_ident(name)),
            _ => name.to_string(),
        }
    }

    pub(super) fn config_field(self, key: &str, value: &str) -> String {
        match self {
            Self::TypeScript => format!("    {key}: {value},"),
            Self::Python => format!("        {key}={value},"),
            Self::Go => format!("\t\t\"{key}\": {value},"),
        }
    }
}

impl std::fmt::Display for AuthoringLang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.file_name())
    }
}

fn go_helper_ident(name: &str) -> &'static str {
    match name {
        "github" => "Github",
        "image" => "Image",
        "bucket" => "Bucket",
        "volume" => "Volume",
        "group" => "Group",
        "postgres" => "Postgres",
        "mysql" => "Mysql",
        "redis" => "Redis",
        "mongo" => "Mongo",
        "preserve" => "Preserve",
        "service" => "ServiceNamed",
        "project" => "ProjectNamed",
        _ => "ServiceNamed",
    }
}
