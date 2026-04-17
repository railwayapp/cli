use super::*;
use crate::consts::get_user_agent;
use crate::util::progress::{create_spinner, fail_spinner, success_spinner};
use flate2::read::GzDecoder;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

const TARBALL_URL: &str =
    "https://github.com/railwayapp/railway-skills/archive/refs/heads/main.tar.gz";
const SKILLS_PATH_PREFIX: &str = "plugins/railway/skills/";

/// Install Railway agent skills for AI coding tools (Claude Code, Cursor, Codex, OpenCode, and all tools that support .agents/skills)
///
/// Always installs to ~/.agents/skills. Additionally installs to any detected tool directories (e.g. ~/.claude/skills, ~/.cursor/skills). Use --agent to target specific tools instead of auto-detection.
#[derive(Parser)]
pub struct Args {
    #[clap(subcommand)]
    command: Option<Commands>,

    /// Target specific agent(s) instead of all detected (e.g. --agent claude-code)
    #[clap(long, global = true)]
    agent: Vec<String>,
}

#[derive(Parser)]
enum Commands {
    /// Install Railway agent skills for AI coding tools (Claude Code, Cursor, Codex, OpenCode, and all tools that support .agents/skills)
    ///
    /// Always installs to ~/.agents/skills. Additionally installs to any detected tool directories (e.g. ~/.claude/skills, ~/.cursor/skills). Use --agent to target specific tools instead of auto-detection.
    #[clap(alias = "update")]
    Install,
    /// Remove Railway skills from all tools
    Remove,
}

#[derive(Clone)]
struct CodingTool {
    slug: &'static str,
    name: &'static str,
    global_parent: PathBuf,
    skills_dir_name: &'static str,
}

struct InstallTarget {
    tool_name: String,
    skills_dir: PathBuf,
}

type SkillFiles = HashMap<String, Vec<(PathBuf, Vec<u8>)>>;

pub async fn command(args: Args) -> Result<()> {
    match args.command {
        None | Some(Commands::Install) => install_skills(&args.agent).await,
        Some(Commands::Remove) => remove_skills(&args.agent).await,
    }
}

fn coding_tools(home: &Path) -> Vec<CodingTool> {
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
            slug: "cursor",
            name: "Cursor",
            global_parent: home.join(".cursor"),
            skills_dir_name: "skills",
        },
    ]
}

fn resolve_tools(home: &Path, agent_filter: &[String]) -> Result<Vec<CodingTool>> {
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

fn write_skills_to_target(target: &InstallTarget, skills: &SkillFiles) -> Result<()> {
    for (skill_name, files) in skills {
        let dest = target.skills_dir.join(skill_name);

        if let Err(e) = std::fs::remove_dir_all(&dest) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(e).with_context(|| {
                    format!("Failed to remove existing skill at {}", dest.display())
                });
            }
        }

        for (relative_path, contents) in files {
            let file_path = dest.join(relative_path);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory {}", parent.display()))?;
            }
            std::fs::write(&file_path, contents)
                .with_context(|| format!("Failed to write {}", file_path.display()))?;
        }
    }

    Ok(())
}

async fn install_skills(agent_filter: &[String]) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let tools = resolve_tools(&home, agent_filter)?;
    let targets = build_targets(&tools);

    println!("\n{}\n", "Railway Skills".bold());
    print_target_summary("Installing to:", &targets);

    let mut spinner = create_spinner("Downloading skills...".to_string());
    let tarball_bytes = match download_tarball().await {
        Ok(bytes) => {
            success_spinner(&mut spinner, "Downloaded skills".to_string());
            bytes
        }
        Err(e) => {
            fail_spinner(&mut spinner, "Failed to download skills".to_string());
            return Err(e);
        }
    };

    let skills = extract_skill_files(&tarball_bytes)?;
    let mut skill_names: Vec<&String> = skills.keys().collect();
    skill_names.sort();

    println!();

    for target in &targets {
        std::fs::create_dir_all(&target.skills_dir).with_context(|| {
            format!(
                "Failed to create skills directory {}",
                target.skills_dir.display()
            )
        })?;

        write_skills_to_target(target, &skills)?;

        for skill_name in &skill_names {
            let skill_path = target.skills_dir.join(skill_name);
            println!(
                "{} {}: installed {} \u{2192} {}",
                "\u{2713}".green(),
                target.tool_name.bold(),
                skill_name.green(),
                skill_path.display().to_string().cyan()
            );
        }
    }

    println!("\n{}", "Skills installed successfully!".green().bold());
    println!(
        "{} You may need to restart your tool(s) to load skills.\n",
        "!".yellow().bold()
    );

    Ok(())
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
