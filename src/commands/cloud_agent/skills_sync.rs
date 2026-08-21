//! Syncing this machine's personal skills onto a cloud agent.
//!
//! What ships is deliberately narrow: directories under one chosen skills
//! directory that contain a `SKILL.md`, minus anything Railway installed
//! itself. Everything else about the agent's harness config — MCP servers,
//! hooks, trust, autonomy posture, the shared instruction file — is
//! express-agent's to own and is reconciled on every boot, so the CLI stays out
//! of it. Skills are the one piece of a user's setup the VM cannot know about.
//!
//! Landing shape on the VM (see [`provision_script`]): every skill goes into a
//! Railway-owned directory, `~/.railway-skills/<name>`, and is symlinked into
//! each harness's own skills directory. Nothing is ever written *through* a
//! harness path, which matters because `cloud-agent-base` builds those
//! directories with `npx skills add --global` and the per-tool entries may
//! themselves be symlinks into `~/.agents/skills`. It also means one Railway
//! directory holds everything we put there, so an update replaces content in
//! place and every harness's link follows it.
//!
//! Sync is add-only: a link is created only when nothing already occupies that
//! name, and turning the preference off later leaves whatever is already on the
//! agent alone. That keeps this path from ever deleting a skill a user
//! installed on the VM by hand, at the cost of a stale skill outliving the
//! preference that put it there.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::prefs::AgentPrefs;

/// Compressed-payload ceilings. The tarball rides one ssh stdin through the
/// relay, and `ssh_plumbing` re-sends stdin on every retry attempt, so a fat
/// payload is paid for repeatedly. The warn threshold is where a launch starts
/// to feel slow; the hard cap is where something is clearly not a skills
/// directory any more (a checked-in repo, a model file) and the user needs to
/// know rather than wait.
const WARN_BYTES: usize = 2 * 1024 * 1024;
const MAX_BYTES: usize = 10 * 1024 * 1024;

/// Per-skill file-count ceiling — a cheap guard against a skill directory that
/// has grown a `node_modules` under a name we do not exclude.
const MAX_FILES_PER_SKILL: usize = 500;

/// Never packed, at any depth. `.git` and `node_modules` are bulk with no value
/// on the agent; `.env` files are the reason this list is not just about size.
const EXCLUDED_DIRS: &[&str] = &[".git", "node_modules", ".venv", "__pycache__", "target"];

/// Where a machine's personal skills can live. Slugs are stable — they are
/// persisted in `agent-prefs.json`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SkillSource {
    pub slug: &'static str,
    pub label: &'static str,
    /// Path relative to `$HOME`, as components.
    dir: &'static [&'static str],
}

pub const SKILL_SOURCES: &[SkillSource] = &[
    SkillSource {
        slug: "claude",
        label: "Claude Code",
        dir: &[".claude", "skills"],
    },
    SkillSource {
        slug: "codex",
        label: "Codex",
        dir: &[".codex", "skills"],
    },
    SkillSource {
        slug: "grok",
        label: "Grok",
        dir: &[".grok", "skills"],
    },
    SkillSource {
        slug: "universal",
        label: "Universal (.agents)",
        dir: &[".agents", "skills"],
    },
];

impl SkillSource {
    pub fn path_in(&self, home: &Path) -> PathBuf {
        self.dir
            .iter()
            .fold(home.to_path_buf(), |acc, c| acc.join(c))
    }

    pub fn from_slug(slug: &str) -> Option<&'static SkillSource> {
        SKILL_SOURCES.iter().find(|s| s.slug == slug)
    }
}

/// The harness skills directories a synced skill is linked into, relative to
/// `$HOME` on the VM. All three regardless of the chosen agent: switching your
/// default later should not require a re-sync, and the add-only link rule makes
/// an extra link harmless.
const REMOTE_LINK_DIRS: &[&str] = &[".claude/skills", ".codex/skills", ".grok/skills"];

/// One skill, flattened to the files that will be packed.
struct LocalSkill {
    name: String,
    /// `(path relative to the skills dir, bytes)`, sorted by path.
    files: Vec<(String, Vec<u8>)>,
}

#[derive(Debug)]
pub struct PackedSkills {
    pub tarball: Vec<u8>,
    /// Content hash of everything packed. Compared against the copy recorded on
    /// the agent so an unchanged set costs no upload.
    pub hash: String,
    pub names: Vec<String>,
    pub source_dir: PathBuf,
}

/// Skills present in `dir`, in name order: a subdirectory is a skill when it
/// holds a `SKILL.md`, which is the same rule `railway skills` uses.
pub fn discover_skill_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir() && e.path().join("SKILL.md").is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Sources that hold at least one skill, for the setup prompt.
pub fn populated_sources(home: &Path) -> Vec<(&'static SkillSource, Vec<String>)> {
    SKILL_SOURCES
        .iter()
        .filter_map(|source| {
            let names = discover_skill_names(&source.path_in(home));
            (!names.is_empty()).then_some((source, names))
        })
        .collect()
}

/// Railway's own skills, by name. The manifest below is the accurate source,
/// but it only covers installs this CLI performed — plenty of people installed
/// `use-railway` with `npx skills add` or an older CLI, which is why the
/// "unmanaged Railway skills" nag exists at all. Names are the belt to the
/// manifest's braces.
const RAILWAY_SKILL_NAMES: &[&str] = &["use-railway"];

/// Skill names never synced: Railway's own. The agent image already bakes them
/// (`npx skills add railwayapp/railway-skills --global` at build time), so a
/// local copy could only ever let an older checkout win.
pub fn railway_managed_names(home: &Path) -> BTreeSet<String> {
    let mut names = crate::commands::skills::railway_managed_skill_names(home);
    names.extend(RAILWAY_SKILL_NAMES.iter().map(|n| n.to_string()));
    names
}

/// Pack the skills a launch should carry, or `None` when there is nothing to
/// send (sync off, no source, or every skill excluded).
pub fn pack(prefs: &AgentPrefs, home: &Path) -> Result<Option<PackedSkills>> {
    if !prefs.skills.enabled {
        return Ok(None);
    }

    // An explicitly recorded source that has since disappeared is worth saying
    // out loud — silently falling back to a different directory would change
    // what the agent can do without telling anyone.
    let source = match prefs.skills.source.as_deref() {
        Some(slug) => SkillSource::from_slug(slug)
            .with_context(|| format!("Unknown skills source `{slug}` in agent-prefs.json"))?,
        None => return Ok(None),
    };
    let source_dir = source.path_in(home);
    if !source_dir.is_dir() {
        eprintln!(
            "Skipping skills sync: {} is gone (re-run `railway ca setup`).",
            source_dir.display()
        );
        return Ok(None);
    }

    let mut excluded: BTreeSet<String> = railway_managed_names(home);
    excluded.extend(prefs.skills.exclude.iter().cloned());

    let mut skills = Vec::new();
    for name in discover_skill_names(&source_dir) {
        if excluded.contains(&name) {
            continue;
        }
        let skill = collect_skill(&source_dir, &name)?;
        // Nothing survived the walk — every file was a symlink or an exclusion.
        // Shipping it would create an empty directory on the agent and claim a
        // name no later sync can use, since linking is add-only.
        if skill.files.is_empty() {
            continue;
        }
        skills.push(skill);
    }
    if skills.is_empty() {
        return Ok(None);
    }

    let hash = hash_skills(&skills);
    let tarball = build_tarball(&skills)?;

    if tarball.len() > MAX_BYTES {
        let mut biggest: Vec<(usize, &str)> = skills
            .iter()
            .map(|s| (s.files.iter().map(|(_, b)| b.len()).sum(), s.name.as_str()))
            .collect();
        biggest.sort_unstable_by_key(|(size, _)| std::cmp::Reverse(*size));
        let worst: Vec<String> = biggest
            .iter()
            .take(3)
            .map(|(bytes, name)| format!("{name} ({})", human_bytes(*bytes)))
            .collect();
        bail!(
            "Your skills are too large to sync ({} compressed, limit {}).\n\
             Largest: {}.\n\
             Trim them, or turn skills off with `railway ca setup`.",
            human_bytes(tarball.len()),
            human_bytes(MAX_BYTES),
            worst.join(", ")
        );
    }
    if tarball.len() > WARN_BYTES {
        eprintln!(
            "Note: syncing {} of skills — this adds time to every launch.",
            human_bytes(tarball.len())
        );
    }

    Ok(Some(PackedSkills {
        tarball,
        hash,
        names: skills.into_iter().map(|s| s.name).collect(),
        source_dir,
    }))
}

/// Read one skill's files. Symlinks are skipped rather than followed: a link
/// out of the skills directory would pull in content the user never meant to
/// send, and a link to a path that only exists on their laptop is dead weight
/// on the VM either way.
fn collect_skill(source_dir: &Path, name: &str) -> Result<LocalSkill> {
    let root = source_dir.join(name);
    let mut files = Vec::new();
    walk(&root, &root, &mut files, name)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(LocalSkill {
        name: name.to_string(),
        files,
    })
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>, skill: &str) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read {}", dir.display()))?
        .flatten()
    {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // `symlink_metadata` so a symlink is classified as a symlink rather
        // than as whatever it points at.
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if EXCLUDED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk(root, &path, out, skill)?;
            continue;
        }
        if name.starts_with(".env") {
            continue;
        }
        if out.len() >= MAX_FILES_PER_SKILL {
            bail!(
                "Skill `{skill}` has more than {MAX_FILES_PER_SKILL} files — that looks like a \
                 checked-out project rather than a skill. Trim it, or exclude it in \
                 `~/.railway/agent-prefs.json`."
            );
        }
        let rel = path
            .strip_prefix(root)
            .expect("walk stays under root")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        out.push((rel, std::fs::read(&path)?));
    }
    Ok(())
}

/// Content hash over the packed set: skill name, then each file's path and
/// bytes, all length-prefixed so no concatenation of names can collide with a
/// different set. Hashing the *content* rather than the tarball keeps the hash
/// stable across gzip output differences between CLI versions.
fn hash_skills(skills: &[LocalSkill]) -> String {
    let mut hasher = Sha256::new();
    for skill in skills {
        hasher.update((skill.name.len() as u64).to_le_bytes());
        hasher.update(skill.name.as_bytes());
        for (path, bytes) in &skill.files {
            hasher.update((path.len() as u64).to_le_bytes());
            hasher.update(path.as_bytes());
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
    format!("{:x}", hasher.finalize())
}

/// One `<skill>/<path>` entry per file, built in memory — the payload is
/// bounded by [`MAX_BYTES`] and rides ssh stdin, so there is nothing to stream
/// to. Modes are set explicitly rather than copied from the local file: what
/// matters on the agent is that a skill is readable, not that it carries a
/// laptop's umask.
fn build_tarball(skills: &[LocalSkill]) -> Result<Vec<u8>> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for skill in skills {
        for (path, bytes) in &skill.files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(
                &mut header,
                format!("{}/{}", skill.name, path),
                bytes.as_slice(),
            )?;
        }
    }
    let encoder = builder.into_inner()?;
    let mut out = encoder.finish()?;
    out.flush().ok();
    Ok(std::mem::take(&mut out))
}

fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{}KB", (bytes / 1024).max(1))
    }
}

/// Marker prefix the launcher greps out of the main provision script's stdout
/// to decide whether the agent already holds this exact skill set.
pub const REMOTE_HASH_MARKER: &str = "SKILLS-HASH:";

/// Where the hash of the last synced set lives on the agent.
pub const REMOTE_HASH_FILE: &str = "$HOME/.railway-skills-hash";

/// Reads the hash the agent recorded, out of the provision script's stdout.
pub fn parse_remote_hash(provision_output: &str) -> Option<String> {
    provision_output
        .lines()
        .find_map(|line| line.trim().strip_prefix(REMOTE_HASH_MARKER))
        .map(str::to_string)
        .filter(|h| !h.is_empty())
}

/// The VM-side sync. The tarball arrives on stdin; extraction goes to a scratch
/// directory first so a truncated transfer cannot leave a half-written skill in
/// place, and the hash file is written last so a failure anywhere above it
/// means the next launch retries rather than believing itself current.
///
/// Every step degrades instead of aborting the launch: a missing `tar`, a bad
/// payload, or an unwritable harness directory costs the user their skills, not
/// their session. The launcher reports which marker came back.
pub fn provision_script(hash: &str) -> String {
    format!(
        r#"umask 077
# Read stdin FIRST, before anything that can bail: a script that exits without
# draining the pipe leaves the CLI's `write_all` on a broken pipe, turning every
# graceful degradation below into a hard launch failure.
payload="$HOME/.railway-skills-payload.tgz"
cat > "$payload"
{}"#,
        sync_body(hash)
    )
}

/// The sync steps from a saved `$payload` onward, for scripts that drained
/// stdin into that file themselves — the combined provision on a fresh agent,
/// where the tarball rides the same connection as the credential. Failure
/// paths `exit 0`, so a caller embedding this must print everything else it
/// needs (AGENT-READY et al) *before* this block.
pub fn sync_body(hash: &str) -> String {
    let link_dirs = REMOTE_LINK_DIRS
        .iter()
        .map(|d| format!("\"$HOME/{d}\""))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"command -v tar >/dev/null 2>&1 || {{ rm -f "$payload"; echo SKILLS-NO-TAR; exit 0; }}
staging="$HOME/.railway-skills-incoming"
rm -rf "$staging" && mkdir -p "$staging" || {{ rm -f "$payload"; echo SKILLS-STAGING-FAILED; exit 0; }}
tar -xzf "$payload" -C "$staging" --no-same-owner 2>/dev/null || {{ rm -rf "$staging" "$payload"; echo SKILLS-EXTRACT-FAILED; exit 0; }}
rm -f "$payload"
mkdir -p "$HOME/.railway-skills"
for skill in "$staging"/*/; do
  [ -d "$skill" ] || continue
  name="$(basename "$skill")"
  rm -rf "$HOME/.railway-skills/$name"
  mv "$skill" "$HOME/.railway-skills/$name" || {{ echo SKILLS-MOVE-FAILED; exit 0; }}
done
rm -rf "$staging"
# Add-only linking: an existing name belongs to the image or to the user, and
# is never replaced. Our own link from a previous sync already points at the
# directory we just refreshed in place, so updates need no relink.
for dir in {link_dirs}; do
  mkdir -p "$dir" 2>/dev/null || continue
  for skill in "$HOME"/.railway-skills/*/; do
    [ -d "$skill" ] || continue
    name="$(basename "$skill")"
    if [ ! -e "$dir/$name" ]; then
      ln -s "$skill" "$dir/$name" 2>/dev/null || true
    fi
  done
done
printf '%s\n' '{hash}' > "{REMOTE_HASH_FILE}"
echo SKILLS-OK"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plant_skill(dir: &Path, name: &str, body: &str) {
        let skill = dir.join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), body).unwrap();
    }

    fn prefs_with(source: &str) -> AgentPrefs {
        AgentPrefs {
            skills: super::super::prefs::SkillsPrefs {
                enabled: true,
                source: Some(source.to_string()),
                exclude: Vec::new(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn discovers_only_dirs_holding_skill_md() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "real-skill", "---\nname: real\n---\n");
        std::fs::create_dir_all(dir.join("not-a-skill")).unwrap();
        std::fs::write(dir.join("loose.md"), "x").unwrap();

        assert_eq!(discover_skill_names(&dir), vec!["real-skill".to_string()]);
    }

    #[test]
    fn packs_selected_source_and_reports_names() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "alpha", "a");
        plant_skill(&dir, "beta", "b");

        let packed = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        assert_eq!(packed.names, vec!["alpha".to_string(), "beta".to_string()]);
        assert!(!packed.tarball.is_empty());
        assert_eq!(packed.hash.len(), 64);
    }

    #[test]
    fn disabled_or_sourceless_prefs_pack_nothing() {
        let home = tempfile::tempdir().unwrap();
        plant_skill(&home.path().join(".claude").join("skills"), "alpha", "a");

        let mut off = prefs_with("claude");
        off.skills.enabled = false;
        assert!(pack(&off, home.path()).unwrap().is_none());

        let mut sourceless = prefs_with("claude");
        sourceless.skills.source = None;
        assert!(pack(&sourceless, home.path()).unwrap().is_none());
    }

    #[test]
    fn excluded_names_are_not_packed() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "alpha", "a");
        plant_skill(&dir, "use-railway", "railway");

        let mut prefs = prefs_with("claude");
        prefs.skills.exclude = vec!["use-railway".into()];
        let packed = pack(&prefs, home.path()).unwrap().unwrap();
        assert_eq!(packed.names, vec!["alpha".to_string()]);
    }

    /// Railway's skills ship in the image. Excluding them must not depend on
    /// the CLI having been the thing that installed them locally.
    #[test]
    fn railway_skills_are_excluded_without_a_manifest() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "use-railway", "railway");
        plant_skill(&dir, "mine", "mine");

        assert!(!AgentPrefs::path_in(home.path()).exists());
        let packed = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        assert_eq!(packed.names, vec!["mine".to_string()]);
    }

    /// A skill directory whose every entry is a symlink packs no files; sending
    /// it would plant an empty directory and burn the name, because linking on
    /// the agent never replaces an existing entry.
    #[cfg(unix)]
    #[test]
    fn skills_that_pack_no_files_are_dropped() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "real", "real");
        let hollow = dir.join("hollow");
        std::fs::create_dir_all(&hollow).unwrap();
        std::os::unix::fs::symlink(dir.join("real").join("SKILL.md"), hollow.join("SKILL.md"))
            .unwrap();

        let packed = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        assert_eq!(packed.names, vec!["real".to_string()]);
    }

    #[test]
    fn everything_excluded_packs_nothing() {
        let home = tempfile::tempdir().unwrap();
        plant_skill(
            &home.path().join(".claude").join("skills"),
            "use-railway",
            "r",
        );
        let mut prefs = prefs_with("claude");
        prefs.skills.exclude = vec!["use-railway".into()];
        assert!(pack(&prefs, home.path()).unwrap().is_none());
    }

    #[test]
    fn dotenv_and_excluded_dirs_are_skipped() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "alpha", "a");
        std::fs::write(dir.join("alpha").join(".env"), "SECRET=1").unwrap();
        std::fs::create_dir_all(dir.join("alpha").join(".git")).unwrap();
        std::fs::write(dir.join("alpha").join(".git").join("config"), "x").unwrap();

        let collected = collect_skill(&dir, "alpha").unwrap();
        let paths: Vec<&str> = collected.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["SKILL.md"]);
    }

    /// Content-addressed, not tarball-addressed: identical content must produce
    /// an identical hash so an unchanged set skips the upload.
    #[test]
    fn hash_tracks_content_not_packing() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "alpha", "a");

        let first = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        let again = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        assert_eq!(first.hash, again.hash);

        std::fs::write(dir.join("alpha").join("SKILL.md"), "changed").unwrap();
        let changed = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        assert_ne!(first.hash, changed.hash);
    }

    #[test]
    fn unknown_source_slug_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        let err = pack(&prefs_with("nonsense"), home.path()).unwrap_err();
        assert!(err.to_string().contains("Unknown skills source"), "{err}");
    }

    #[test]
    fn parses_the_remote_hash_marker() {
        assert_eq!(
            parse_remote_hash("AGENT-READY\nSKILLS-HASH:abc123\n").as_deref(),
            Some("abc123")
        );
        assert!(parse_remote_hash("AGENT-READY\nSKILLS-HASH:\n").is_none());
        assert!(parse_remote_hash("AGENT-READY\n").is_none());
    }

    /// The provision script's contract with the launcher: land in a Railway
    /// directory, never replace an existing harness entry, and record the hash
    /// only after the work is done.
    #[test]
    fn provision_script_is_add_only_and_records_the_hash_last() {
        let script = provision_script("deadbeef");
        assert!(script.contains("$HOME/.railway-skills/$name"));
        assert!(script.contains(r#"if [ ! -e "$dir/$name" ]"#));
        assert!(script.contains("SKILLS-NO-TAR"));
        assert!(script.contains("SKILLS-OK"));

        let hash_at = script.find("deadbeef").unwrap();
        let ok_at = script.find("echo SKILLS-OK").unwrap();
        let move_at = script.find("mv \"$skill\"").unwrap();
        assert!(move_at < hash_at && hash_at < ok_at);

        // Never a write through a harness path — those may be symlinks into
        // ~/.agents/skills on the image.
        assert!(!script.contains("-C \"$HOME/.claude"));
    }

    /// Every early exit must come after the `cat`. Bailing with the pipe still
    /// full breaks the CLI's write instead of the sync, which would turn "this
    /// agent has no tar" into a failed launch.
    #[test]
    fn provision_script_drains_stdin_before_it_can_bail() {
        let script = provision_script("deadbeef");
        let cat_at = script.find("cat > \"$payload\"").unwrap();
        for marker in [
            "SKILLS-NO-TAR",
            "SKILLS-STAGING-FAILED",
            "SKILLS-EXTRACT-FAILED",
        ] {
            assert!(
                script.find(marker).unwrap() > cat_at,
                "`{marker}` can be reached before stdin is drained"
            );
        }
    }

    /// The script actually runs. Everything else here asserts on its text; this
    /// executes it against a stand-in `$HOME` with the real tarball on stdin,
    /// which is the only local way to catch a shell error before a VM does.
    #[cfg(unix)]
    #[test]
    fn provision_script_lands_skills_and_leaves_existing_names_alone() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "mine", "mine-body");
        let packed = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();

        // Stand-in for the agent: a harness skills dir that already holds a
        // skill of the same name, plus one it has never seen.
        let vm = tempfile::tempdir().unwrap();
        let claude_skills = vm.path().join(".claude").join("skills");
        std::fs::create_dir_all(claude_skills.join("mine")).unwrap();
        std::fs::write(claude_skills.join("mine").join("SKILL.md"), "theirs").unwrap();

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(provision_script(&packed.hash))
            .env("HOME", vm.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&packed.tarball)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("SKILLS-OK"),
            "stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Content landed in Railway's own directory...
        let landed = vm
            .path()
            .join(".railway-skills")
            .join("mine")
            .join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "mine-body");

        // ...the name already taken on the agent was not touched...
        assert_eq!(
            std::fs::read_to_string(claude_skills.join("mine").join("SKILL.md")).unwrap(),
            "theirs"
        );

        // ...a harness that had no entry got a link to ours...
        let codex_link = vm.path().join(".codex").join("skills").join("mine");
        assert!(
            std::fs::symlink_metadata(&codex_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(codex_link.join("SKILL.md")).unwrap(),
            "mine-body"
        );

        // ...and the hash was recorded, so the next launch skips the upload.
        let recorded = std::fs::read_to_string(vm.path().join(".railway-skills-hash")).unwrap();
        assert_eq!(recorded.trim(), packed.hash);

        // No scratch files left behind.
        assert!(!vm.path().join(".railway-skills-payload.tgz").exists());
        assert!(!vm.path().join(".railway-skills-incoming").exists());
    }

    /// What lands on the agent is `<skill>/<file>` and nothing else — no
    /// absolute paths, no `..`, no laptop directory structure.
    #[test]
    fn tarball_entries_are_skill_relative() {
        use std::io::Read;

        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("skills");
        plant_skill(&dir, "alpha", "a");
        std::fs::create_dir_all(dir.join("alpha").join("scripts")).unwrap();
        std::fs::write(
            dir.join("alpha").join("scripts").join("run.sh"),
            "#!/bin/sh",
        )
        .unwrap();

        let packed = pack(&prefs_with("claude"), home.path()).unwrap().unwrap();
        let decoder = flate2::read::GzDecoder::new(packed.tarball.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let mut paths: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["alpha/SKILL.md", "alpha/scripts/run.sh"]);

        // And the content survives the round trip.
        let decoder = flate2::read::GzDecoder::new(packed.tarball.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let mut body = String::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().ends_with("SKILL.md") {
                entry.read_to_string(&mut body).unwrap();
            }
        }
        assert_eq!(body, "a");
    }
}
