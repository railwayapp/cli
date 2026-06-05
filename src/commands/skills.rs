use super::*;
use crate::consts::get_user_agent;
use crate::util::progress::{create_spinner, fail_spinner, success_spinner};
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
const SKILLS_CHECK_INTERVAL_HOURS: i64 = 12;

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
    /// The upstream SHA we last nagged about unmanaged (pre-manifest /
    /// externally-synced) skill installs, so the banner fires once per
    /// upstream commit rather than on every command.
    #[serde(default)]
    orphan_nag_sha: Option<String>,
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

    /// True when a background auto-apply is worth spawning: there's a pending
    /// update we haven't already attempted for this exact upstream SHA. Once an
    /// attempt runs (even one that only skips modified skills), we don't retry
    /// the same SHA — that case falls back to the user-facing nag.
    fn should_auto_apply(&self) -> bool {
        self.update_pending() && self.latest_sha != self.auto_applied_sha
    }
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
    /// The user edited (or deleted) files we own — skip unless forced.
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
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let sha = response.text().await.ok()?.trim().to_string();
    (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

/// No-network read of cached state, for the startup banner: true when we have
/// skills installed and a previous background check found a newer commit.
pub(crate) fn cached_skill_update_available() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let manifest = SkillsManifest::read(&home);
    manifest.has_installed_skills() && manifest.update_pending()
}

/// No-network read: true when a background auto-apply should be spawned (a
/// pending update we haven't already attempted for this upstream SHA).
pub(crate) fn cached_skill_auto_apply_due() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let manifest = SkillsManifest::read(&home);
    manifest.has_installed_skills() && manifest.should_auto_apply()
}

/// Background staleness check, run from the same task as the CLI version check.
/// 12h-gated and best-effort: refreshes the cached upstream SHA so the next
/// invocation's banner is accurate. Skips entirely when no skills are installed.
pub(crate) async fn refresh_skill_update_state() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let mut manifest = SkillsManifest::read(&home);
    // Run the check for managed installs AND for unmanaged-but-present
    // railway skills (pre-manifest installs), so the orphan nag can key
    // off a real upstream SHA and re-arm when upstream moves. Machines
    // with no railway skills anywhere still never hit the network.
    if !manifest.has_installed_skills() && unmanaged_skill_tools(&home, &manifest).is_empty() {
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
    // a transient failure just delays the banner by one interval.
    manifest.last_checked = Some(Utc::now().to_rfc3339());
    if let Some(sha) = fetch_latest_sha().await {
        manifest.latest_sha = Some(sha);
    }
    let _ = manifest.save(&home);
}

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        None | Some(Commands::Install) => install_skills(&args.agent, args.force).await,
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
/// external syncs. We never write to these without the user asking;
/// `orphan_skills_nag_due` surfaces a banner pointing at
/// `railway skills update` instead (which adopts up-to-date copies and
/// safely skips modified ones).
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

/// Banner check for unmanaged railway skills: returns the affected tool
/// names when a nag is due and stamps the dedupe key so it fires once per
/// upstream SHA (re-arming when upstream moves), not on every command.
/// No network; a couple of stat calls per known tool.
fn orphan_skills_nag(home: &Path) -> Option<Vec<String>> {
    let mut manifest = SkillsManifest::read(home);
    let orphans = unmanaged_skill_tools(home, &manifest);
    if orphans.is_empty() {
        return None;
    }
    // Key the nag to the last-seen upstream SHA; before the first
    // background check lands there's no SHA yet, so a sentinel gives us
    // exactly one pre-check nag.
    let key = manifest
        .latest_sha
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    if manifest.orphan_nag_sha.as_deref() == Some(key.as_str()) {
        return None;
    }
    manifest.orphan_nag_sha = Some(key);
    let _ = manifest.save(home);
    Some(orphans.into_iter().map(str::to_string).collect())
}

/// No-network banner entry point for `main`: unmanaged railway skills
/// found on disk (see `unmanaged_skill_tools`), at most once per upstream
/// SHA.
pub(crate) fn orphan_skills_nag_due() -> Option<Vec<String>> {
    let home = dirs::home_dir()?;
    orphan_skills_nag(&home)
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

async fn download_tarball() -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let response = client
        .get(TARBALL_URL)
        .header("User-Agent", get_user_agent())
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

pub(super) async fn install_skills(agent_filter: &[String], force: bool) -> Result<()> {
    run_install(agent_filter, force, false).await
}

/// Headless skills refresh, spawned as a detached process (mirrors the binary's
/// background self-update). Auto-detects targets, never forces — so user-edited
/// skills are skipped and left to the nag — and prints nothing.
pub(crate) async fn apply_update_in_background() -> Result<()> {
    run_install(&[], false, true).await
}

async fn run_install(agent_filter: &[String], force: bool, quiet: bool) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let tools = resolve_tools(&home, agent_filter)?;
    let targets = build_targets(&tools);

    if !quiet {
        println!("\n{}\n", "Railway Skills".bold());
        print_target_summary("Installing to:", &targets);
    }

    let tarball_bytes = if quiet {
        download_tarball().await?
    } else {
        let mut spinner = create_spinner("Downloading skills...".to_string());
        match download_tarball().await {
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
    let mut skill_names: Vec<&String> = skills.keys().collect();
    skill_names.sort();

    // Best-effort: the commit we're installing. Lets the background staleness
    // check tell when this install has fallen behind upstream.
    let source_sha = fetch_latest_sha().await;

    if !quiet {
        println!();
    }

    let mut manifest = SkillsManifest::read(&home);
    let mut blocked = 0u32;
    let mut installed = 0u32;

    for target in &targets {
        std::fs::create_dir_all(&target.skills_dir).with_context(|| {
            format!(
                "Failed to create skills directory {}",
                target.skills_dir.display()
            )
        })?;
        let target_key = rel_key(&target.skills_dir);

        for skill_name in &skill_names {
            let files = &skills[*skill_name];
            let new_hashes = new_file_hashes(files);
            let skill_dir = target.skills_dir.join(skill_name);
            let record = manifest.record(&target_key, skill_name).cloned();
            let state = classify_skill(&skill_dir, &new_hashes, record.as_ref());

            let (label, action) = match state {
                SkillState::NotInstalled => ("installed", true),
                SkillState::CleanUpgrade => ("updated", true),
                SkillState::UpToDate => {
                    // Adopt an untracked-but-current skill so future upgrades
                    // have a baseline; otherwise nothing to write.
                    if record.is_none() {
                        manifest.set_record(
                            &target_key,
                            skill_name,
                            SkillRecord {
                                installed_at: Utc::now().to_rfc3339(),
                                files: new_hashes,
                            },
                        );
                    }
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
                                format!("you've modified {}", files.join(", "))
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

    // Record the installed commit and reset the staleness cache so the "update
    // available" banner clears immediately. We only advance source_sha when we
    // applied the new content everywhere; if some skills were skipped as
    // modified, leaving the old (or absent) SHA keeps the banner honest.
    // `auto_applied_sha` is stamped regardless so a background apply that only
    // skips modified skills doesn't re-download the same commit every run.
    if let Some(sha) = source_sha {
        if blocked == 0 {
            manifest.source_sha = Some(sha.clone());
        }
        manifest.latest_sha = Some(sha.clone());
        manifest.last_checked = Some(Utc::now().to_rfc3339());
        manifest.auto_applied_sha = Some(sha);
    }

    manifest.save(&home)?;

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

    Ok(())
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
    let tools = resolve_tools(&home, agent_filter)?;
    let targets = build_targets(&tools);

    println!("\n{}\n", "Railway Skills".bold());
    print_target_summary("Removing from:", &targets);

    let mut spinner = create_spinner("Fetching skill list...".to_string());
    let tarball_bytes = match download_tarball().await {
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
        manifest.set_record(
            &rel_key(&skills_dir),
            "use-railway",
            SkillRecord::default(),
        );
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

    #[test]
    fn orphan_nag_fires_once_per_upstream_sha() {
        let home = tempfile::tempdir().unwrap();
        plant_orphan_skill(home.path());

        // First sighting nags (pre-check sentinel key) and stamps the manifest.
        assert!(orphan_skills_nag(home.path()).is_some());
        // Same key again: deduped.
        assert!(orphan_skills_nag(home.path()).is_none());

        // Upstream moved (background check recorded a new SHA): re-arms once.
        let mut manifest = SkillsManifest::read(home.path());
        manifest.latest_sha = Some("a".repeat(40));
        manifest.save(home.path()).unwrap();
        assert!(orphan_skills_nag(home.path()).is_some());
        assert!(orphan_skills_nag(home.path()).is_none());
    }

    #[test]
    fn orphan_nag_silent_when_no_skills_anywhere() {
        let home = tempfile::tempdir().unwrap();
        assert!(orphan_skills_nag(home.path()).is_none());
        // And it must not create a manifest as a side effect.
        assert!(!SkillsManifest::path(home.path()).exists());
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
        m.source_sha = Some("old".to_string());
        m.latest_sha = Some("new".to_string());

        // Pending and never attempted → auto-apply is due.
        assert!(m.should_auto_apply());

        // We attempted "new" but it only skipped a modified skill (source_sha
        // stayed "old"). Still pending for the banner, but don't re-download.
        m.auto_applied_sha = Some("new".to_string());
        assert!(m.update_pending());
        assert!(!m.should_auto_apply());

        // Upstream moves again → due once more.
        m.latest_sha = Some("newer".to_string());
        assert!(m.should_auto_apply());
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
