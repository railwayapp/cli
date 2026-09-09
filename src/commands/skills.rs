use super::*;
use crate::consts::get_user_agent;
use crate::util::progress::{create_spinner, fail_spinner, success_spinner};
use crate::util::update_status::{self, SkillsOutcome};
use crate::util::write_atomic;
use chrono::Utc;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

const TARBALL_URL: &str =
    "https://github.com/railwayapp/railway-skills/archive/refs/heads/main.tar.gz";
const SKILLS_PATH_PREFIX: &str = "plugins/railway/skills/";
/// Returns the bare 40-char commit SHA of the skills repo's default branch.
const SKILLS_SHA_URL: &str = "https://api.github.com/repos/railwayapp/railway-skills/commits/main";
/// How often the background task re-checks upstream for a newer skills commit.
const SKILLS_CHECK_INTERVAL_HOURS: i64 = 1;

/// Install Railway agent skills for AI coding tools (Claude Code, Cursor, Codex, OpenCode, GitHub Copilot, Factory Droid, and all tools that support .agents/skills)
///
/// Always installs to ~/.agents/skills. Additionally installs to any detected tool directories (e.g. ~/.claude/skills, ~/.cursor/skills). Use --agent to target specific tools instead of auto-detection.
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Target specific agent(s) instead of all detected (e.g. --agent claude-code)
    #[clap(long, global = true)]
    agent: Vec<String>,

    /// Overwrite skills you've modified locally instead of skipping them
    #[clap(long, global = true)]
    force: bool,
}

#[derive(Parser)]
enum Commands {
    /// Install Railway agent skills for AI coding tools (Claude Code, Cursor, Codex, OpenCode, GitHub Copilot, Factory Droid, and all tools that support .agents/skills)
    ///
    /// Always installs to ~/.agents/skills. Additionally installs to any detected tool directories (e.g. ~/.claude/skills, ~/.cursor/skills). Use --agent to target specific tools instead of auto-detection.
    #[clap(visible_alias = "update", visible_alias = "add")]
    Install,
    /// Remove Railway skills from all tools
    #[clap(visible_alias = "rm", visible_alias = "uninstall")]
    Remove,
}

#[derive(Clone)]
pub(super) struct CodingTool {
    pub slug: &'static str,
    pub name: &'static str,
    pub global_parent: PathBuf,
    skills_dir_name: &'static str,
}

struct InstallTarget {
    tool_name: String,
    skills_dir: PathBuf,
}

#[derive(Clone, Copy)]
enum InstallMode<'a> {
    Explicit { force: bool, quiet: bool },
    Background,
    Upgrade { cli_version: &'a str },
}

impl<'a> InstallMode<'a> {
    fn managed_only(self) -> bool {
        !matches!(self, Self::Explicit { .. })
    }

    fn cli_version(self) -> &'a str {
        match self {
            Self::Upgrade { cli_version } => cli_version,
            _ => env!("CARGO_PKG_VERSION"),
        }
    }
}

type SkillFiles = HashMap<String, Vec<(PathBuf, Vec<u8>)>>;

// ---------------------------------------------------------------------------
// Install manifest + local-modification detection
//
// We record a per-target, per-skill manifest of the content hashes we wrote at
// install time. On a later upgrade this baseline lets us tell "the user edited
// this skill" apart from "upstream moved on" — the two are indistinguishable
// from on-disk vs new content alone. Stored next to the CLI's other state at
// ~/.railway/skills.json so the skill directories themselves stay pristine
// (coding tools enumerate them and a stray file could be misread as content).
// ---------------------------------------------------------------------------

/// File hashes for one installed skill, keyed by forward-slash relative path.
#[derive(Serialize, Deserialize, Clone, Default)]
struct SkillRecord {
    installed_at: String,
    files: BTreeMap<String, String>,
}

/// Persisted record of what we last wrote, keyed by skills-dir path then skill.
#[derive(Serialize, Deserialize, Default)]
struct SkillsManifest {
    /// Commit SHA of the railway-skills repo we last installed from.
    #[serde(default)]
    source_sha: Option<String>,
    /// Latest upstream SHA seen by the background staleness check.
    #[serde(default)]
    latest_sha: Option<String>,
    /// When the background staleness check last hit the API (RFC3339).
    #[serde(default)]
    last_checked: Option<String>,
    /// The upstream SHA a background auto-apply last attempted. Lets us avoid
    /// re-downloading every invocation when the only thing still "pending" is a
    /// user-modified skill the background apply can't touch.
    #[serde(default)]
    auto_applied_sha: Option<String>,
    /// CLI version that last synced installed skills. A new binary gets one
    /// immediate sync, regardless of the hourly upstream-check gate.
    #[serde(default)]
    cli_version: Option<String>,
    #[serde(default)]
    targets: BTreeMap<String, BTreeMap<String, SkillRecord>>,
}

impl SkillsManifest {
    fn path(home: &Path) -> PathBuf {
        home.join(".railway").join("skills.json")
    }

    /// Reads the manifest, treating a missing or unparseable file as empty so a
    /// corrupt manifest degrades to the no-baseline path rather than erroring.
    fn read(home: &Path) -> Self {
        std::fs::read_to_string(Self::path(home))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, home: &Path) -> Result<()> {
        let contents = serde_json::to_string_pretty(self)?;
        write_atomic(&Self::path(home), &contents)
    }

    fn record(&self, target_key: &str, skill: &str) -> Option<&SkillRecord> {
        self.targets.get(target_key)?.get(skill)
    }

    fn set_record(&mut self, target_key: &str, skill: &str, record: SkillRecord) {
        self.targets
            .entry(target_key.to_string())
            .or_default()
            .insert(skill.to_string(), record);
    }

    fn has_installed_skills(&self) -> bool {
        self.targets.values().any(|skills| !skills.is_empty())
    }

    /// True when we know both what we installed and what's upstream, and they
    /// differ. Conservative: an unknown source SHA never reports an update.
    fn update_pending(&self) -> bool {
        match (&self.source_sha, &self.latest_sha) {
            (Some(installed), Some(latest)) => installed != latest,
            _ => false,
        }
    }

    /// Sync once per CLI version (including legacy manifests), and whenever
    /// upstream changes. An attempt that skips modified skills is still recorded
    /// so we don't download the same revision on every command.
    fn should_auto_apply(&self) -> bool {
        self.has_installed_skills()
            && (self.cli_version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
                || (self.update_pending() && self.latest_sha != self.auto_applied_sha))
    }

    fn update_requires_attention(&self) -> bool {
        self.has_installed_skills() && self.update_pending() && !self.should_auto_apply()
    }
}

/// Serialize manifest reads/writes and skill installs across CLI processes.
/// Background callers quietly retry on their next invocation if the lock is busy.
fn lock_manifest(home: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;

    let dir = home.join(".railway");
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::File::create(dir.join("skills.lock"))?;
    file.try_lock_exclusive()
        .context("Another skills update is in progress. Please try again shortly.")?;
    Ok(file)
}

/// How an on-disk skill compares to the baseline we recorded and the new
/// upstream content. Drives whether we upgrade, skip, or warn.
#[derive(Debug, PartialEq, Eq)]
enum SkillState {
    /// Not present on disk — a fresh install.
    NotInstalled,
    /// On-disk content already equals the new upstream — nothing to do.
    UpToDate,
    /// Unmodified since our last install and upstream changed — safe to upgrade.
    CleanUpgrade,
    /// The user edited/deleted files we own, or added a file upstream now wants
    /// to write — skip unless forced.
    Modified,
    /// Present but we have no baseline and it differs from upstream — we can't
    /// prove it's untouched, so treat it like a modification.
    Unverifiable,
}

/// Hash of file contents with line endings normalized, so a CRLF rewrite or a
/// added/stripped trailing CR doesn't read as a user modification.
fn hash_normalized(bytes: &[u8]) -> String {
    let normalized: Vec<u8> = bytes.iter().copied().filter(|&b| b != b'\r').collect();
    let digest = Sha256::digest(&normalized);
    let mut hex = String::with_capacity(64);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn rel_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn join_rel(dir: &Path, rel: &str) -> PathBuf {
    let mut path = dir.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
    }
    path
}

fn new_file_hashes(files: &[(PathBuf, Vec<u8>)]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(path, contents)| (rel_key(path), hash_normalized(contents)))
        .collect()
}

fn hash_disk_file(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| hash_normalized(&bytes))
}

/// Classifies a single skill in a single target directory. `record` is the
/// baseline we wrote last time (None if we've never tracked this skill here).
fn classify_skill(
    skill_dir: &Path,
    new_hashes: &BTreeMap<String, String>,
    record: Option<&SkillRecord>,
) -> SkillState {
    if !skill_dir.exists() {
        return SkillState::NotInstalled;
    }

    // The files we consider "ours": what we recorded, else the incoming set.
    let owned: BTreeSet<String> = match record {
        Some(r) => r.files.keys().cloned().collect(),
        None => new_hashes.keys().cloned().collect(),
    };

    // Hash every relevant on-disk file once (owned ∪ new).
    let mut disk: BTreeMap<String, Option<String>> = BTreeMap::new();
    for rel in owned.iter().chain(new_hashes.keys()) {
        disk.entry(rel.clone())
            .or_insert_with(|| hash_disk_file(&join_rel(skill_dir, rel)));
    }

    // Already current? Every new file present & matching, and no owned file that
    // upstream dropped is still lingering. If so there's nothing to do — this
    // also covers the case where the user happened to make the same edit.
    let matches_new = new_hashes
        .iter()
        .all(|(rel, h)| disk.get(rel).and_then(Option::as_ref) == Some(h));
    let lingering = owned
        .iter()
        .any(|rel| !new_hashes.contains_key(rel) && disk.get(rel).is_some_and(Option::is_some));
    if matches_new && !lingering {
        return SkillState::UpToDate;
    }

    match record {
        // With a baseline: modified iff any owned file differs from what we
        // wrote (a missing file counts as a deletion).
        Some(r) => {
            let modified = r.files.iter().any(|(rel, recorded)| {
                disk.get(rel).and_then(Option::clone).as_ref() != Some(recorded)
            }) || new_hashes.iter().any(|(rel, new_hash)| {
                !r.files.contains_key(rel)
                    && disk
                        .get(rel)
                        .and_then(Option::as_ref)
                        .is_some_and(|hash| hash != new_hash)
            });
            if modified {
                SkillState::Modified
            } else {
                SkillState::CleanUpgrade
            }
        }
        // No baseline and it isn't already current — we can't prove it's
        // untouched. (A known-good historical-hash set could rescue some of
        // these in future; for now treat conservatively.)
        None => SkillState::Unverifiable,
    }
}

/// Names of the files we own that differ from the recorded baseline, for use in
/// the "you've modified …" warning. Only meaningful when a record exists.
fn modified_files(skill_dir: &Path, record: &SkillRecord) -> Vec<String> {
    record
        .files
        .iter()
        .filter(|(rel, recorded)| {
            hash_disk_file(&join_rel(skill_dir, rel)).as_ref() != Some(*recorded)
        })
        .map(|(rel, _)| rel.clone())
        .collect()
}

/// Surgically writes one skill into `skill_dir`: overwrites/creates every file
/// in `files`, and deletes files we previously owned that upstream has dropped.
/// Files we never wrote (foreign additions by the user) are left untouched.
fn apply_skill(
    skill_dir: &Path,
    files: &[(PathBuf, Vec<u8>)],
    record: Option<&SkillRecord>,
) -> Result<SkillRecord> {
    for (rel, contents) in files {
        let file_path = join_rel(skill_dir, &rel_key(rel));
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        std::fs::write(&file_path, contents)
            .with_context(|| format!("Failed to write {}", file_path.display()))?;
    }

    let new_keys: BTreeSet<String> = files.iter().map(|(p, _)| rel_key(p)).collect();
    if let Some(record) = record {
        for rel in record.files.keys() {
            if !new_keys.contains(rel) {
                let _ = std::fs::remove_file(join_rel(skill_dir, rel));
            }
        }
    }

    Ok(SkillRecord {
        installed_at: Utc::now().to_rfc3339(),
        files: new_file_hashes(files),
    })
}

/// Fetches the bare commit SHA of the skills repo's default branch. Best-effort:
/// any network/parse failure returns None so callers degrade gracefully.
async fn fetch_latest_sha() -> Option<String> {
    let response = reqwest::Client::new()
        .get(SKILLS_SHA_URL)
        .header("User-Agent", get_user_agent())
        .header("Accept", "application/vnd.github.sha")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let sha = response.text().await.ok()?.trim().to_string();
    (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// Explicit status reports local edits without interrupting normal commands.
pub(crate) fn print_update_details() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let manifest = SkillsManifest::read(&home);
    if manifest.update_requires_attention() {
        println!(
            "Agent skills preserved: local changes detected. Run `railway skills update` for details."
        );
    }
    let unmanaged = unmanaged_skill_tools(&home, &manifest);
    if !unmanaged.is_empty() {
        println!(
            "Unmanaged Railway skills: {}. Run `railway skills update` to adopt unmodified copies.",
            unmanaged.join(", ")
        );
    }
}

/// No-network read: true when a background auto-apply should be spawned (a
/// pending upstream update or the first sync with a new CLI version).
pub(crate) fn cached_skill_auto_apply_due() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let manifest = SkillsManifest::read(&home);
    manifest.should_auto_apply()
}

pub(crate) fn managed_install_info() -> (bool, Option<String>) {
    let Some(home) = dirs::home_dir() else {
        return (false, None);
    };
    let manifest = SkillsManifest::read(&home);
    (manifest.has_installed_skills(), manifest.cli_version)
}

/// Background staleness check, run from the same task as the CLI version check.
/// Hourly-gated and best-effort: refreshes the cached upstream SHA for automatic
/// synchronization and explicit status. Skips when no managed skills are installed.
pub(crate) async fn refresh_skill_update_state() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let Ok(_lock) = lock_manifest(&home) else {
        return;
    };
    let mut manifest = SkillsManifest::read(&home);
    // Only managed installs participate in automatic updates.
    if !manifest.has_installed_skills() {
        return;
    }

    if let Some(last) = manifest
        .last_checked
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    {
        let age = Utc::now().signed_duration_since(last.with_timezone(&Utc));
        if age < chrono::Duration::hours(SKILLS_CHECK_INTERVAL_HOURS) {
            return;
        }
    }

    // Stamp `last_checked` regardless of the fetch outcome: a failing
    // GitHub API (rate limit, network) would otherwise re-fire the
    // request on every CLI invocation until one succeeded. Worst case
    // a transient failure just delays discovery by one interval.
    manifest.last_checked = Some(Utc::now().to_rfc3339());
    if let Some(sha) = fetch_latest_sha().await {
        manifest.latest_sha = Some(sha);
    }
    let _ = manifest.save(&home);
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        None | Some(Commands::Install) => install_skills(&args.agent, args.force, false).await,
        Some(Commands::Remove) => remove_skills(&args.agent).await,
    }
}

pub(super) fn coding_tools(home: &Path) -> Vec<CodingTool> {
    vec![
        CodingTool {
            slug: "universal",
            name: "Universal (.agents)",
            global_parent: home.join(".agents"),
            skills_dir_name: "skills",
        },
        CodingTool {
            slug: "claude-code",
            name: "Claude Code",
            global_parent: home.join(".claude"),
            skills_dir_name: "skills",
        },
        CodingTool {
            slug: "codex",
            name: "OpenAI Codex",
            global_parent: home.join(".codex"),
            skills_dir_name: "skills",
        },
        CodingTool {
            slug: "opencode",
            name: "OpenCode",
            global_parent: home.join(".config").join("opencode"),
            skills_dir_name: "skills",
        },
        CodingTool {
            slug: "copilot",
            name: "GitHub Copilot",
            global_parent: home.join(".copilot"),
            skills_dir_name: "skills",
        },
        CodingTool {
            slug: "factory-droid",
            name: "Factory Droid",
            global_parent: home.join(".factory"),
            skills_dir_name: "skills",
        },
        CodingTool {
            slug: "cursor",
            name: "Cursor",
            global_parent: home.join(".cursor"),
            skills_dir_name: "skills",
        },
    ]
}

pub(super) fn resolve_tools(home: &Path, agent_filter: &[String]) -> Result<Vec<CodingTool>> {
    let all_tools = coding_tools(home);

    if agent_filter.is_empty() {
        // "agents" (universal) is always included; others require their config dir to exist.
        Ok(all_tools
            .into_iter()
            .filter(|tool| tool.slug == "universal" || tool.global_parent.is_dir())
            .collect())
    } else {
        let mut selected = Vec::new();
        for slug in agent_filter {
            match all_tools.iter().find(|t| t.slug == slug.as_str()) {
                Some(t) => selected.push(t.clone()),
                None => {
                    let valid = all_tools
                        .iter()
                        .map(|t| t.slug)
                        .collect::<Vec<_>>()
                        .join(", ");
                    bail!("Unknown agent: '{}'\n\nValid agents: {}", slug, valid);
                }
            }
        }
        Ok(selected)
    }
}

/// Skills this CLI distributes, by directory name. Used to recognize
/// railway skill installs that pre-date the manifest (or were synced
/// externally) without claiming unrelated skills that live in the same
/// shared skills directories.
const RAILWAY_SKILL_NAMES: &[&str] = &["use-railway"];

/// Coding tools that have a railway skill on disk with no manifest record
/// for that target — installs made before the CLI tracked skills, or
/// external syncs. Explicit update status points at `railway skills update`,
/// which adopts up-to-date copies and safely skips modified ones.
fn unmanaged_skill_tools(home: &Path, manifest: &SkillsManifest) -> Vec<&'static str> {
    coding_tools(home)
        .into_iter()
        .filter(|tool| {
            let skills_dir = tool.global_parent.join(tool.skills_dir_name);
            let manifested = manifest
                .targets
                .get(&rel_key(&skills_dir))
                .is_some_and(|skills| !skills.is_empty());
            if manifested {
                return false;
            }
            RAILWAY_SKILL_NAMES
                .iter()
                .any(|skill| skills_dir.join(skill).join("SKILL.md").is_file())
        })
        .map(|tool| tool.name)
        .collect()
}

/// What the background staleness check knows about an install, distinguishing
/// "verified current" from "never checked" so the health check doesn't claim
/// freshness it has no evidence for.
pub(super) enum SkillsStaleness {
    /// The last upstream check matched the installed commit.
    UpToDate,
    /// Upstream has moved past the install (short SHA of the newer commit).
    UpdateAvailable(String),
    /// A newer revision will be applied automatically on a normal invocation.
    UpdatePending(String),
    /// No upstream check result yet — staleness unknown.
    Unknown,
}

/// Skills install/staleness info for the help-screen health check: the short
/// commit SHA we last installed from, plus what the background staleness
/// check knows about it. Reads only what the manifest already tracks — no
/// network. None when there's no manifest-tracked install (e.g.
/// pre-manifest/orphan copies).
pub(super) fn installed_skills_revision(home: &Path) -> Option<(String, SkillsStaleness)> {
    let manifest = SkillsManifest::read(home);
    if !manifest.has_installed_skills() {
        return None;
    }
    let installed = manifest.source_sha.as_deref()?;
    let short = |sha: &str| sha.chars().take(7).collect::<String>();
    let staleness = match manifest.latest_sha.as_deref() {
        Some(latest) if latest == installed => SkillsStaleness::UpToDate,
        Some(latest) if manifest.should_auto_apply() => {
            SkillsStaleness::UpdatePending(short(latest))
        }
        Some(latest) => SkillsStaleness::UpdateAvailable(short(latest)),
        None => SkillsStaleness::Unknown,
    };
    Some((short(installed), staleness))
}

pub(super) fn skills_configured_for_slug(home: &Path, slug: &str) -> bool {
    coding_tools(home)
        .into_iter()
        .find(|tool| tool.slug == slug)
        .map(|tool| {
            tool.global_parent
                .join(tool.skills_dir_name)
                .join("use-railway")
        })
        .is_some_and(|path| path.is_dir())
}

/// Every skill name this CLI has installed, across every target it manages.
///
/// The cloud agent skills sync uses this as an exclusion list: Railway's own
/// skills are baked into `cloud-agent-base`, so re-uploading the local copy
/// could only ever let an older checkout win. Reads the manifest rather than
/// matching on a hardcoded name, so a second Railway skill is covered the day
/// it ships. A missing or corrupt manifest yields an empty set — the sync then
/// falls back to shipping a duplicate, which is untidy but harmless.
pub(super) fn railway_managed_skill_names(home: &Path) -> BTreeSet<String> {
    SkillsManifest::read(home)
        .targets
        .values()
        .flat_map(|skills| skills.keys().cloned())
        .collect()
}

fn build_targets(tools: &[CodingTool]) -> Vec<InstallTarget> {
    tools
        .iter()
        .map(|tool| InstallTarget {
            tool_name: tool.name.to_string(),
            skills_dir: tool.global_parent.join(tool.skills_dir_name),
        })
        .collect()
}

fn print_target_summary(action: &str, targets: &[InstallTarget]) {
    let target_names = targets
        .iter()
        .map(|target| target.tool_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    println!("{} {}\n", action.bold(), target_names);
}

async fn download_tarball(source_sha: Option<&str>) -> Result<Vec<u8>> {
    // Pin to the SHA we record: main can move between the API and tarball requests.
    let url = source_sha
        .map(|sha| format!("https://github.com/railwayapp/railway-skills/archive/{sha}.tar.gz"))
        .unwrap_or_else(|| TARBALL_URL.to_string());
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", get_user_agent())
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .context("Failed to download Railway skills")?;

    if !response.status().is_success() {
        bail!(
            "Failed to download Railway skills: HTTP {}",
            response.status()
        );
    }

    Ok(response
        .bytes()
        .await
        .context("Failed to read response body")?
        .to_vec())
}

/// Extract all skills from the tarball, grouped by skill name.
/// Returns a map of skill_name -> Vec<(relative_path, file_contents)>.
fn extract_skill_files(tarball_bytes: &[u8]) -> Result<SkillFiles> {
    let decoder = GzDecoder::new(Cursor::new(tarball_bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut skills: SkillFiles = HashMap::new();

    for entry in archive
        .entries()
        .context("Failed to read tarball entries")?
    {
        let mut entry = entry.context("Failed to read tarball entry")?;
        let path_str = entry
            .path()
            .context("Failed to read entry path")?
            .to_string_lossy()
            .into_owned();

        if let Some(pos) = path_str.find(SKILLS_PATH_PREFIX) {
            let after_prefix = &path_str[pos + SKILLS_PATH_PREFIX.len()..];

            // Split into skill_name/relative_path
            let Some(slash_pos) = after_prefix.find('/') else {
                continue;
            };
            let skill_name = &after_prefix[..slash_pos];
            let relative = &after_prefix[slash_pos + 1..];

            if skill_name.is_empty() || relative.is_empty() || entry.header().entry_type().is_dir()
            {
                continue;
            }

            let mut contents = Vec::new();
            entry
                .read_to_end(&mut contents)
                .context("Failed to read file from tarball")?;

            skills
                .entry(skill_name.to_string())
                .or_default()
                .push((PathBuf::from(relative), contents));
        }
    }

    if skills.is_empty() {
        bail!("No skills found in downloaded repository");
    }

    Ok(skills)
}

pub(super) async fn install_skills(
    agent_filter: &[String],
    force: bool,
    quiet: bool,
) -> Result<()> {
    run_install(agent_filter, InstallMode::Explicit { force, quiet })
        .await
        .map(|_| ())
}

/// Headless skills refresh, spawned as a detached process (mirrors the binary's
/// background self-update). Only updates manifest-managed skills, never forces,
/// and prints nothing. Re-check preferences in the child, before any writes.
pub(crate) async fn apply_update_in_background() -> Result<()> {
    run_install(&[], InstallMode::Background).await.map(|_| ())
}

/// Explicit CLI upgrades include managed skills, even when automatic checks are
/// disabled. This never forces local edits or installs skills for new users.
pub(crate) async fn sync_for_upgrade(cli_version: &str) -> Result<SkillsOutcome> {
    run_install(&[], InstallMode::Upgrade { cli_version })
        .await?
        .context("Skills synchronization did not complete")
}

async fn run_install(
    agent_filter: &[String],
    mode: InstallMode<'_>,
) -> Result<Option<SkillsOutcome>> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let _lock = lock_manifest(&home)?;
    let manifest = SkillsManifest::read(&home);
    let (targets, quiet) = match mode {
        InstallMode::Explicit { quiet, .. } => {
            (build_targets(&resolve_tools(&home, agent_filter)?), quiet)
        }
        InstallMode::Background | InstallMode::Upgrade { .. } => {
            if matches!(mode, InstallMode::Background)
                && (crate::telemetry::is_auto_update_disabled() || !manifest.should_auto_apply())
            {
                return Ok(None);
            }
            if !manifest.has_installed_skills() {
                update_status::record_skills(mode.cli_version(), SkillsOutcome::NotInstalled);
                return Ok(Some(SkillsOutcome::NotInstalled));
            }
            let targets = manifest
                .targets
                .keys()
                .map(|path| InstallTarget {
                    tool_name: path.clone(),
                    skills_dir: PathBuf::from(path),
                })
                .collect();
            (targets, true)
        }
    };

    if !quiet {
        println!("\n{}\n", "Railway Skills".bold());
        print_target_summary("Installing to:", &targets);
    }

    let report_outcome = mode.managed_only() || agent_filter.is_empty();
    if report_outcome {
        update_status::record_skills(mode.cli_version(), SkillsOutcome::Pending);
    }
    let result = async {
        let source_sha = fetch_latest_sha().await;
        if mode.managed_only() && source_sha.is_none() {
            bail!("Failed to determine the latest Railway skills revision");
        }
        let tarball_bytes = if quiet {
            download_tarball(source_sha.as_deref()).await?
        } else {
            let mut spinner = create_spinner("Downloading skills...".to_string());
            match download_tarball(source_sha.as_deref()).await {
                Ok(bytes) => {
                    success_spinner(&mut spinner, "Downloaded skills".to_string());
                    bytes
                }
                Err(e) => {
                    fail_spinner(&mut spinner, "Failed to download skills".to_string());
                    return Err(e);
                }
            }
        };

        let skills = extract_skill_files(&tarball_bytes)?;

        // The user may have disabled updates while the download was in flight.
        if matches!(mode, InstallMode::Background) && crate::telemetry::is_auto_update_disabled() {
            return Ok(None);
        }
        install_downloaded_skills(&home, &targets, &skills, source_sha, manifest, mode).map(Some)
    }
    .await;
    if report_outcome {
        match &result {
            Ok(Some(outcome)) => update_status::record_skills(mode.cli_version(), outcome.clone()),
            Err(error) => update_status::record_skills(
                mode.cli_version(),
                SkillsOutcome::Failed {
                    message: error.to_string(),
                },
            ),
            Ok(None) => {}
        }
    }
    result
}

fn install_downloaded_skills(
    home: &Path,
    targets: &[InstallTarget],
    skills: &SkillFiles,
    source_sha: Option<String>,
    mut manifest: SkillsManifest,
    mode: InstallMode<'_>,
) -> Result<SkillsOutcome> {
    let (force, quiet) = match mode {
        InstallMode::Explicit { force, quiet } => (force, quiet),
        InstallMode::Background | InstallMode::Upgrade { .. } => (false, true),
    };
    let mut skill_names: Vec<&String> = skills.keys().collect();
    skill_names.sort();

    if !quiet {
        println!();
    }

    let mut blocked = 0u32;
    let mut installed = 0u32;

    for target in targets {
        let target_key = rel_key(&target.skills_dir);

        for skill_name in &skill_names {
            let files = &skills[*skill_name];
            let new_hashes = new_file_hashes(files);
            let skill_dir = target.skills_dir.join(skill_name);
            let record = manifest.record(&target_key, skill_name).cloned();
            if mode.managed_only() && record.is_none() {
                continue;
            }
            let state = match classify_skill(&skill_dir, &new_hashes, record.as_ref()) {
                // A removed managed skill is a local deletion. Only an explicit
                // install may recreate it; automatic sync must leave it alone.
                SkillState::NotInstalled if mode.managed_only() => SkillState::Modified,
                state => state,
            };

            let (label, action) = match state {
                SkillState::NotInstalled => ("installed", true),
                SkillState::CleanUpgrade => ("updated", true),
                SkillState::UpToDate => {
                    // Adopt current content as the baseline, including when a
                    // user independently applied the same upstream changes.
                    manifest.set_record(
                        &target_key,
                        skill_name,
                        SkillRecord {
                            installed_at: Utc::now().to_rfc3339(),
                            files: new_hashes,
                        },
                    );
                    if !quiet {
                        println!(
                            "{} {}: {} already up to date",
                            "-".dimmed(),
                            target.tool_name,
                            skill_name
                        );
                    }
                    continue;
                }
                SkillState::Modified | SkillState::Unverifiable if !force => {
                    if !quiet {
                        let detail = match (&state, &record) {
                            (SkillState::Modified, Some(r)) => {
                                let files = modified_files(&skill_dir, r);
                                if files.is_empty() {
                                    "an incoming file conflicts with a local addition".to_string()
                                } else {
                                    format!("you've modified {}", files.join(", "))
                                }
                            }
                            _ => "can't verify it's unmodified".to_string(),
                        };
                        println!(
                            "{} {}: skipped {} — {}. Re-run with {} to overwrite.",
                            "\u{26a0}".yellow(),
                            target.tool_name.bold(),
                            skill_name.yellow(),
                            detail,
                            "--force".cyan()
                        );
                    }
                    blocked += 1;
                    continue;
                }
                // Forced overwrite of a modified/unverifiable skill.
                SkillState::Modified | SkillState::Unverifiable => ("overwrote", true),
            };

            if action {
                let new_record = apply_skill(&skill_dir, files, record.as_ref())?;
                manifest.set_record(&target_key, skill_name, new_record);
                installed += 1;
                if !quiet {
                    println!(
                        "{} {}: {} {} \u{2192} {}",
                        "\u{2713}".green(),
                        target.tool_name.bold(),
                        label,
                        skill_name.green(),
                        skill_dir.display().to_string().cyan()
                    );
                }
            }
        }
    }

    // Only advance source_sha when we applied the new content everywhere. If
    // some skills were preserved, retain the prior revision for explicit status.
    // `auto_applied_sha` is stamped regardless so a background apply that only
    // skips modified skills doesn't re-download the same commit every run.
    if let Some(sha) = source_sha.as_ref() {
        if blocked == 0 {
            manifest.source_sha = Some(sha.clone());
        }
        manifest.latest_sha = Some(sha.clone());
        manifest.last_checked = Some(Utc::now().to_rfc3339());
        manifest.auto_applied_sha = Some(sha.clone());
        manifest.cli_version = Some(mode.cli_version().to_string());
    }

    manifest.save(home)?;

    if !quiet {
        if blocked > 0 {
            println!(
                "\n{} {} skill(s) skipped because of local changes. Re-run with {} to overwrite them.",
                "!".yellow().bold(),
                blocked,
                "railway skills update --force".cyan()
            );
        }

        // Summarize what actually happened: claiming success after a run
        // that skipped everything misleads both humans and the agents
        // parsing this output into "skills are current".
        if installed > 0 {
            if blocked > 0 {
                println!(
                    "\n{}",
                    format!("Installed {installed} skill(s); {blocked} skipped.")
                        .green()
                        .bold()
                );
            } else {
                println!("\n{}", "Skills installed successfully!".green().bold());
            }
            println!(
                "{} You may need to restart your tool(s) to load skills.\n",
                "!".yellow().bold()
            );
        } else if blocked > 0 {
            println!(
                "\n{}\n",
                "No skills were installed — every pending change was skipped."
                    .yellow()
                    .bold()
            );
        } else {
            println!("\n{}\n", "Skills already up to date.".green().bold());
        }
    }

    Ok(match source_sha {
        Some(revision) if blocked > 0 => SkillsOutcome::Preserved {
            revision,
            count: blocked,
        },
        Some(revision) => SkillsOutcome::Synced { revision },
        None => SkillsOutcome::Failed {
            message: "Installed skills, but their upstream revision could not be verified".into(),
        },
    })
}

/// Re-execs this binary detached with `_RAILWAY_UPDATE_SKILLS` set so it
/// downloads and applies the latest skills out of band, exactly like the
/// binary self-updater's background download. Best-effort and never blocks.
pub(crate) fn spawn_background_skill_update() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Ok(log_path) = crate::util::self_update::auto_update_log_path() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.env(crate::consts::RAILWAY_UPDATE_SKILLS_ENV, "1");
    if let Ok(child) = crate::util::spawn_detached(&mut cmd, &log_path) {
        // Detached: never waited on. Mirrors spawn_background_download.
        std::mem::forget(child);
    }
}

// Remove fetches the skill list from the upstream repo rather than keeping a
// local manifest. The skills/ directory is shared with other providers, so we
// can't blindly delete everything — we need to know which subdirectories are
// ours. Using the repo as the source of truth avoids stale manifests when
// skills are renamed upstream.
async fn remove_skills(agent_filter: &[String]) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let _lock = lock_manifest(&home)?;
    let tools = resolve_tools(&home, agent_filter)?;
    let targets = build_targets(&tools);

    println!("\n{}\n", "Railway Skills".bold());
    print_target_summary("Removing from:", &targets);

    let mut spinner = create_spinner("Fetching skill list...".to_string());
    let tarball_bytes = match download_tarball(None).await {
        Ok(bytes) => {
            success_spinner(&mut spinner, "Fetched skill list".to_string());
            bytes
        }
        Err(e) => {
            fail_spinner(&mut spinner, "Failed to fetch skill list".to_string());
            return Err(e);
        }
    };

    let skills = extract_skill_files(&tarball_bytes)?;
    let mut skill_names: Vec<&String> = skills.keys().collect();
    skill_names.sort();

    println!();

    let mut removed_any = false;

    for target in &targets {
        for skill_name in &skill_names {
            let skill_dir = target.skills_dir.join(skill_name);
            match std::fs::remove_dir_all(&skill_dir) {
                Ok(()) => {
                    println!(
                        "{} {}: removed {}",
                        "\u{2713}".green(),
                        target.tool_name.bold(),
                        skill_name.red()
                    );
                    removed_any = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!(
                        "{} {}: {} not installed, skipping",
                        "-".dimmed(),
                        target.tool_name,
                        skill_name
                    );
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to remove skill at {}", skill_dir.display())
                    });
                }
            }
        }
    }

    if removed_any {
        println!("\n{}\n", "Skills removed successfully.".green().bold());
    } else {
        println!("\n{}\n", "No skills were installed.".dimmed());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_existing_use_railway_skill() {
        let home = tempfile::tempdir().unwrap();
        let path = home
            .path()
            .join(".agents")
            .join("skills")
            .join("use-railway");
        std::fs::create_dir_all(&path).unwrap();

        assert!(skills_configured_for_slug(home.path(), "universal"));
        assert!(!skills_configured_for_slug(home.path(), "cursor"));
    }

    // --- unmanaged (pre-manifest) skill detection --------------------------

    /// Lay down a railway skill on disk for the universal target, the way a
    /// pre-manifest install (or an external sync) would have.
    fn plant_orphan_skill(home: &Path) -> PathBuf {
        let skills_dir = home.join(".agents").join("skills");
        let skill = skills_dir.join("use-railway");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "content").unwrap();
        skills_dir
    }

    #[test]
    fn unmanaged_tools_found_when_skill_on_disk_without_manifest_record() {
        let home = tempfile::tempdir().unwrap();
        plant_orphan_skill(home.path());

        let manifest = SkillsManifest::default();
        let tools = unmanaged_skill_tools(home.path(), &manifest);
        assert_eq!(tools, vec!["Universal (.agents)"]);
    }

    #[test]
    fn unmanaged_ignores_manifested_targets_and_bare_dirs() {
        let home = tempfile::tempdir().unwrap();
        let skills_dir = plant_orphan_skill(home.path());

        // A manifest record for the target means it's managed — not an orphan.
        let mut manifest = SkillsManifest::default();
        manifest.set_record(&rel_key(&skills_dir), "use-railway", SkillRecord::default());
        assert!(unmanaged_skill_tools(home.path(), &manifest).is_empty());

        // A bare directory without SKILL.md (leftover scaffolding) is not a
        // skill install and must not nag.
        let home2 = tempfile::tempdir().unwrap();
        let bare = home2
            .path()
            .join(".cursor")
            .join("skills")
            .join("use-railway");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(unmanaged_skill_tools(home2.path(), &SkillsManifest::default()).is_empty());
    }

    // --- modification detection -------------------------------------------

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = join_rel(dir, rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn record_of(files: &[(&str, &str)]) -> SkillRecord {
        SkillRecord {
            installed_at: "t".to_string(),
            files: files
                .iter()
                .map(|(rel, c)| (rel.to_string(), hash_normalized(c.as_bytes())))
                .collect(),
        }
    }

    fn new_files(files: &[(&str, &str)]) -> Vec<(PathBuf, Vec<u8>)> {
        files
            .iter()
            .map(|(rel, c)| (PathBuf::from(rel), c.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn classify_not_installed_when_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        let new = new_file_hashes(&new_files(&[("SKILL.md", "hello")]));
        assert_eq!(classify_skill(&dir, &new, None), SkillState::NotInstalled);
    }

    #[test]
    fn classify_up_to_date_when_disk_matches_new() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "hello");
        let new = new_file_hashes(&new_files(&[("SKILL.md", "hello")]));
        let record = record_of(&[("SKILL.md", "hello")]);
        // Up to date regardless of whether we have a baseline.
        assert_eq!(
            classify_skill(&dir, &new, Some(&record)),
            SkillState::UpToDate
        );
        assert_eq!(classify_skill(&dir, &new, None), SkillState::UpToDate);
    }

    #[test]
    fn classify_clean_upgrade_when_unmodified_and_upstream_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "v1");
        let record = record_of(&[("SKILL.md", "v1")]); // disk == baseline
        let new = new_file_hashes(&new_files(&[("SKILL.md", "v2")])); // upstream moved
        assert_eq!(
            classify_skill(&dir, &new, Some(&record)),
            SkillState::CleanUpgrade
        );
    }

    #[test]
    fn classify_modified_when_user_edited_owned_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "user-edit");
        let record = record_of(&[("SKILL.md", "v1")]); // we shipped v1
        let new = new_file_hashes(&new_files(&[("SKILL.md", "v2")]));
        assert_eq!(
            classify_skill(&dir, &new, Some(&record)),
            SkillState::Modified
        );
    }

    #[test]
    fn classify_modified_when_user_deleted_owned_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "v1");
        // helper.sh was shipped but the user removed it from disk.
        let record = record_of(&[("SKILL.md", "v1"), ("helper.sh", "echo hi")]);
        let new = new_file_hashes(&new_files(&[("SKILL.md", "v2"), ("helper.sh", "echo hi")]));
        assert_eq!(
            classify_skill(&dir, &new, Some(&record)),
            SkillState::Modified
        );
    }

    #[test]
    fn classify_unverifiable_without_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "something-old");
        let new = new_file_hashes(&new_files(&[("SKILL.md", "v2")]));
        assert_eq!(classify_skill(&dir, &new, None), SkillState::Unverifiable);
    }

    #[test]
    fn line_ending_differences_are_not_modifications() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "line1\r\nline2\r\n"); // CRLF on disk
        let record = record_of(&[("SKILL.md", "line1\nline2\n")]); // recorded LF
        let new = new_file_hashes(&new_files(&[("SKILL.md", "line1\nline2\n")]));
        assert_eq!(
            classify_skill(&dir, &new, Some(&record)),
            SkillState::UpToDate
        );
    }

    #[test]
    fn apply_skill_preserves_foreign_files_and_removes_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("use-railway");
        write(&dir, "SKILL.md", "v1");
        write(&dir, "old.sh", "old"); // previously owned, dropped upstream
        write(&dir, "notes.md", "my notes"); // foreign, user-added

        let record = record_of(&[("SKILL.md", "v1"), ("old.sh", "old")]);
        let files = new_files(&[("SKILL.md", "v2")]);
        apply_skill(&dir, &files, Some(&record)).unwrap();

        assert_eq!(
            std::fs::read_to_string(join_rel(&dir, "SKILL.md")).unwrap(),
            "v2"
        );
        assert!(
            !join_rel(&dir, "old.sh").exists(),
            "dropped file should be removed"
        );
        assert_eq!(
            std::fs::read_to_string(join_rel(&dir, "notes.md")).unwrap(),
            "my notes",
            "foreign file should be preserved"
        );
    }

    #[test]
    fn update_pending_only_when_both_shas_known_and_differ() {
        let mut m = SkillsManifest::default();
        m.set_record("/skills", "use-railway", SkillRecord::default());

        // Unknown source SHA → never pending (conservative).
        m.latest_sha = Some("aaa".to_string());
        assert!(!m.update_pending());

        // Same SHA → up to date.
        m.source_sha = Some("aaa".to_string());
        assert!(!m.update_pending());

        // Differ → pending.
        m.latest_sha = Some("bbb".to_string());
        assert!(m.update_pending());
    }

    #[test]
    fn should_auto_apply_skips_already_attempted_sha() {
        let mut m = SkillsManifest::default();
        m.set_record("/skills", "use-railway", SkillRecord::default());
        m.cli_version = Some(env!("CARGO_PKG_VERSION").to_string());
        m.source_sha = Some("old".to_string());
        m.latest_sha = Some("new".to_string());

        // Pending and never attempted → auto-apply is due.
        assert!(m.should_auto_apply());
        assert!(!m.update_requires_attention());

        // We attempted "new" but it only skipped a modified skill (source_sha
        // stayed "old"). Still pending for the banner, but don't re-download.
        m.auto_applied_sha = Some("new".to_string());
        assert!(m.update_pending());
        assert!(!m.should_auto_apply());
        assert!(m.update_requires_attention());

        // Upstream moves again → due once more.
        m.latest_sha = Some("newer".to_string());
        assert!(m.should_auto_apply());
        assert!(!m.update_requires_attention());
    }

    #[test]
    fn cli_version_change_syncs_skills_even_with_a_fresh_upstream_cache() {
        let mut manifest = SkillsManifest {
            source_sha: Some("current".to_string()),
            latest_sha: Some("current".to_string()),
            auto_applied_sha: Some("current".to_string()),
            last_checked: Some(Utc::now().to_rfc3339()),
            cli_version: Some("0.0.1".to_string()),
            ..Default::default()
        };
        assert!(
            !manifest.should_auto_apply(),
            "never install skills for new users"
        );
        manifest.set_record("/skills", "use-railway", SkillRecord::default());
        assert!(manifest.should_auto_apply());
        manifest.cli_version = Some(env!("CARGO_PKG_VERSION").to_string());
        assert!(!manifest.should_auto_apply());

        // Pre-version-tracking manifests get the same one-time migration.
        manifest.cli_version = None;
        assert!(manifest.should_auto_apply());
    }

    #[test]
    fn background_sync_updates_clean_skills_and_preserves_local_changes() {
        assert_managed_sync_preserves_local_changes(InstallMode::Background);
    }

    #[test]
    fn explicit_upgrade_sync_preserves_local_changes_and_records_the_installed_version() {
        assert_managed_sync_preserves_local_changes(InstallMode::Upgrade {
            cli_version: "255.255.254",
        });
    }

    fn assert_managed_sync_preserves_local_changes(mode: InstallMode<'_>) {
        let home = tempfile::tempdir().unwrap();
        let mut manifest = SkillsManifest {
            source_sha: Some("old".to_string()),
            ..Default::default()
        };
        let cases = [
            "clean",
            "edited",
            "deleted-file",
            "deleted-skill",
            "collision",
            "unmanaged",
        ];
        let targets: Vec<_> = cases
            .iter()
            .map(|name| InstallTarget {
                tool_name: name.to_string(),
                skills_dir: home.path().join(name),
            })
            .collect();
        for target in &targets {
            let dir = target.skills_dir.join("use-railway");
            write(&dir, "SKILL.md", "v1");
            write(&dir, "old.md", "old");
            write(&dir, "notes.md", "my notes");
            if target.tool_name != "unmanaged" {
                manifest.set_record(
                    &rel_key(&target.skills_dir),
                    "use-railway",
                    record_of(&[("SKILL.md", "v1"), ("old.md", "old")]),
                );
            }
        }
        write(
            &home.path().join("edited/use-railway"),
            "SKILL.md",
            "my edit",
        );
        std::fs::remove_file(home.path().join("deleted-file/use-railway/old.md")).unwrap();
        std::fs::remove_dir_all(home.path().join("deleted-skill/use-railway")).unwrap();
        write(
            &home.path().join("collision/use-railway"),
            "new.md",
            "my addition",
        );
        let skills = HashMap::from([
            (
                "use-railway".to_string(),
                new_files(&[("SKILL.md", "v2"), ("new.md", "upstream")]),
            ),
            (
                "new-skill".to_string(),
                new_files(&[("SKILL.md", "new skill")]),
            ),
        ]);

        let outcome = install_downloaded_skills(
            home.path(),
            &targets,
            &skills,
            Some("new".to_string()),
            manifest,
            mode,
        )
        .unwrap();
        assert_eq!(
            outcome,
            SkillsOutcome::Preserved {
                revision: "new".into(),
                count: 4
            }
        );

        let read = |path: &str| std::fs::read_to_string(home.path().join(path)).unwrap();
        assert_eq!(read("clean/use-railway/SKILL.md"), "v2");
        assert_eq!(read("clean/use-railway/new.md"), "upstream");
        assert_eq!(read("clean/use-railway/notes.md"), "my notes");
        assert!(!home.path().join("clean/use-railway/old.md").exists());
        assert_eq!(read("edited/use-railway/SKILL.md"), "my edit");
        assert_eq!(read("deleted-file/use-railway/SKILL.md"), "v1");
        assert!(!home.path().join("deleted-file/use-railway/old.md").exists());
        assert!(!home.path().join("deleted-skill/use-railway").exists());
        assert_eq!(read("collision/use-railway/SKILL.md"), "v1");
        assert_eq!(read("collision/use-railway/new.md"), "my addition");
        assert_eq!(read("unmanaged/use-railway/SKILL.md"), "v1");
        assert!(
            targets
                .iter()
                .all(|target| !target.skills_dir.join("new-skill").exists())
        );

        let manifest = SkillsManifest::read(home.path());
        assert_eq!(manifest.source_sha.as_deref(), Some("old"));
        assert_eq!(manifest.latest_sha.as_deref(), Some("new"));
        assert_eq!(manifest.cli_version.as_deref(), Some(mode.cli_version()));
        if matches!(mode, InstallMode::Background) {
            assert!(
                !manifest.should_auto_apply(),
                "do not repeatedly retry modified skills"
            );
            assert!(manifest.update_requires_attention());
        }
    }

    #[test]
    fn successful_background_sync_records_revision_and_cli_version() {
        let home = tempfile::tempdir().unwrap();
        let target = InstallTarget {
            tool_name: "test".to_string(),
            skills_dir: home.path().join("skills"),
        };
        let dir = target.skills_dir.join("use-railway");
        // The user independently applied exactly the upstream change. Adopt
        // that content as the baseline so the next release can update it too.
        write(&dir, "SKILL.md", "v2");
        let mut manifest = SkillsManifest::default();
        manifest.set_record(
            &rel_key(&target.skills_dir),
            "use-railway",
            record_of(&[("SKILL.md", "v1")]),
        );
        let skills = HashMap::from([("use-railway".to_string(), new_files(&[("SKILL.md", "v2")]))]);
        install_downloaded_skills(
            home.path(),
            &[target],
            &skills,
            Some("new".to_string()),
            manifest,
            InstallMode::Background,
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), "v2");
        let manifest = SkillsManifest::read(home.path());
        assert_eq!(manifest.source_sha.as_deref(), Some("new"));
        assert_eq!(
            manifest.cli_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(!manifest.should_auto_apply());
        assert!(!manifest.update_requires_attention());
        assert_eq!(
            classify_skill(
                &dir,
                &new_file_hashes(&new_files(&[("SKILL.md", "v3")])),
                manifest.record(&rel_key(&home.path().join("skills")), "use-railway"),
            ),
            SkillState::CleanUpgrade
        );
    }

    #[test]
    fn has_installed_skills_reflects_records() {
        let mut m = SkillsManifest::default();
        assert!(!m.has_installed_skills());
        m.set_record("/skills", "use-railway", SkillRecord::default());
        assert!(m.has_installed_skills());
    }

    #[test]
    fn manifest_with_only_targets_still_parses() {
        // Back-compat: a manifest written before SHA tracking has no sha fields.
        let json = r#"{"targets":{"/skills":{"use-railway":{"installed_at":"t","files":{}}}}}"#;
        let m: SkillsManifest = serde_json::from_str(json).unwrap();
        assert!(m.has_installed_skills());
        assert!(m.source_sha.is_none());
        assert!(!m.update_pending());
    }

    #[test]
    fn manifest_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let mut manifest = SkillsManifest::default();
        manifest.set_record(
            "/skills",
            "use-railway",
            SkillRecord {
                installed_at: "t".to_string(),
                files: BTreeMap::from([("SKILL.md".to_string(), "abc".to_string())]),
            },
        );
        manifest.save(home.path()).unwrap();
        let read = SkillsManifest::read(home.path());
        assert!(read.record("/skills", "use-railway").is_some());
        assert!(read.record("/missing", "x").is_none());
    }

    #[test]
    fn detects_copilot_and_factory_droid_skills() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(
            home.path()
                .join(".copilot")
                .join("skills")
                .join("use-railway"),
        )
        .unwrap();
        std::fs::create_dir_all(
            home.path()
                .join(".factory")
                .join("skills")
                .join("use-railway"),
        )
        .unwrap();

        assert!(skills_configured_for_slug(home.path(), "copilot"));
        assert!(skills_configured_for_slug(home.path(), "factory-droid"));
    }
}
