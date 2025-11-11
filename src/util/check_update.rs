use std::cmp::Ordering;

use anyhow::{Context, bail};
use dirs::home_dir;

use super::compare_semver::compare_semver;

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct UpdateCheck {
    pub last_update_check: Option<chrono::DateTime<chrono::Utc>>,
    pub latest_version: Option<String>,
}
impl UpdateCheck {
    pub fn write(&self) -> anyhow::Result<()> {
        let home = home_dir().context("Failed to get home directory")?;
        let path = home.join(".railway/version.json");
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap();
        let pid = std::process::id();
        // almost guaranteed no collision- can be upgraded to uuid if necessary.
        let tmp_path = path.with_extension(format!("tmp.{}-{}.json", pid, nanos));
        let contents = serde_json::to_string_pretty(&self)?;
        std::fs::write(&tmp_path, contents)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    pub fn read() -> anyhow::Result<Self> {
        let home = home_dir().context("Failed to get home directory")?;
        let path = home.join(".railway/version.json");
        let contents =
            std::fs::read_to_string(&path).context("Failed to read update check file")?;
        serde_json::from_str::<Self>(&contents).context("Failed to parse update check file")
    }
}
#[derive(serde::Deserialize)]
struct GithubApiRelease {
    tag_name: String,
}

const GITHUB_API_RELEASE_URL: &str = "https://api.github.com/repos/railwayapp/cli/releases/latest";
pub async fn check_update(force: bool) -> anyhow::Result<Option<String>> {
    let update = UpdateCheck::read().unwrap_or_default();

    if let Some(last_update_check) = update.last_update_check {
        if chrono::Utc::now().date_naive() == last_update_check.date_naive() && !force {
            bail!("Update check already ran today");
        }
    }

    let client = reqwest::Client::new();
    let response = client
        .get(GITHUB_API_RELEASE_URL)
        .header("User-Agent", "railwayapp")
        .send()
        .await?;
    let response = response.json::<GithubApiRelease>().await?;
    let latest_version = response.tag_name.trim_start_matches('v');

    match compare_semver(env!("CARGO_PKG_VERSION"), latest_version) {
        Ordering::Less => {
            let update = UpdateCheck {
                last_update_check: Some(chrono::Utc::now()),
                latest_version: Some(latest_version.to_owned()),
            };
            update.write()?;
            Ok(Some(latest_version.to_string()))
        }
        _ => Ok(None),
    }
}
