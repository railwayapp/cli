use crate::queries::project::{ProjectProject, ProjectProjectEnvironmentsEdges};
use anyhow::bail;
use base64::prelude::*;
use similar::{ChangeTag, TextDiff};

use super::*;

pub async fn pull(
    environment: &ProjectProjectEnvironmentsEdges,
    project: ProjectProject,
    args: Pull,
) -> Result<()> {
    let (id, path) = common::get_function_from_path(args.path)?;
    let functions = common::get_functions_in_environment(&project, environment);
    let function = functions.iter().find(|f| f.node.service_id == id);
    if let Some(function) = function {
        if let Some(cmd) = &function.node.start_command {
            let encoded = cmd.split(' ').next_back();
            if let Some(encoded) = encoded {
                let decoded = String::from_utf8(BASE64_STANDARD.decode(encoded)?)?;
                let current = String::from_utf8(std::fs::read(&path)?)?;
                let diff = TextDiff::from_lines(&current, &decoded);
                let mut insertions = 0;
                let mut deletions = 0;
                let mut changes = 0;

                for group in diff.grouped_ops(0) {
                    for op in group {
                        for change in diff.iter_changes(&op) {
                            match change.tag() {
                                ChangeTag::Delete => deletions += 1,
                                ChangeTag::Insert => insertions += 1,
                                ChangeTag::Equal => {}
                            }
                        }

                        // Count it as a "change" if it's a Replace (both Delete and Insert)
                        if op.tag() == similar::DiffTag::Replace {
                            changes += 1;
                        }
                    }
                }
                std::fs::write(path, decoded)?;
                println!("Function updated ({insertions} insertions, {deletions} deletions, {changes} changes)");
            } else {
                bail!("Function no longer uses the correct start command format")
            }
        }
    } else {
        bail!(
            "The function linked to the path specified no longer exists in the current environment"
        );
    }
    Ok(())
}
