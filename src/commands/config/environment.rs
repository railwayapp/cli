use anyhow::{Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedEnvironment {
    pub id: String,
    pub name: String,
}

/// Match an environment id, or an exact name. Ids win when a name collides.
pub fn resolve_environment<'a>(
    requested: &str,
    environments: &'a [NamedEnvironment],
) -> Result<&'a NamedEnvironment> {
    if let Some(env) = environments.iter().find(|env| env.id == requested) {
        return Ok(env);
    }
    let mut name_matches = environments
        .iter()
        .filter(|env| env.name == requested)
        .collect::<Vec<_>>();
    match name_matches.len() {
        1 => return Ok(name_matches[0]),
        n if n > 1 => {
            name_matches.sort_by(|left, right| left.id.cmp(&right.id));
            let ids = name_matches
                .iter()
                .map(|env| env.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Environment name \"{requested}\" matches more than one environment. Use an ID: {ids}"
            );
        }
        _ => {}
    }
    bail!(
        "Environment \"{requested}\" not found. Environments in this project: {}",
        environment_names(environments)
    )
}

fn environment_names(environments: &[NamedEnvironment]) -> String {
    let mut names = environments
        .iter()
        .map(|env| env.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

pub fn token_targets_other(token_id: &str, token_name: Option<&str>, requested: &str) -> bool {
    requested != token_id && token_name != Some(requested)
}

pub fn token_scope_error(scoped_to: &str, requested: &str) -> String {
    format!("This token is scoped to {scoped_to}; it cannot target {requested}.")
}

pub fn scoped_environment_label<'a>(token_id: &'a str, token_name: Option<&'a str>) -> &'a str {
    token_name
        .filter(|name| !name.is_empty())
        .unwrap_or(token_id)
}

pub fn ensure_pinned_plan_environment(
    plan_environment_id: &str,
    requested: &str,
    resolved_id: &str,
) -> Result<()> {
    if resolved_id == plan_environment_id {
        return Ok(());
    }
    bail!(
        "--environment {requested} does not match the pinned plan's environment {plan_environment_id}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, name: &str) -> NamedEnvironment {
        NamedEnvironment {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn resolves_by_id_or_exact_name() {
        let environments = vec![env("env_prod", "production"), env("env_stage", "staging")];
        assert_eq!(
            resolve_environment("env_stage", &environments)
                .unwrap()
                .name,
            "staging"
        );
        assert_eq!(
            resolve_environment("production", &environments).unwrap().id,
            "env_prod"
        );
        assert!(resolve_environment("Production", &environments).is_err());

        let collided = vec![env("staging", "production"), env("env_stage", "staging")];
        assert_eq!(
            resolve_environment("staging", &collided).unwrap().id,
            "staging"
        );
    }

    #[test]
    fn not_found_lists_environment_names() {
        let environments = vec![env("env_stage", "staging"), env("env_prod", "production")];
        let err = resolve_environment("pr-12", &environments)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "Environment \"pr-12\" not found. Environments in this project: production, staging"
        );
    }

    #[test]
    fn token_scope_error_rejects_a_different_environment() {
        assert!(!token_targets_other(
            "env_prod",
            Some("production"),
            "env_prod"
        ));
        assert!(!token_targets_other(
            "env_prod",
            Some("production"),
            "production"
        ));
        assert!(token_targets_other(
            "env_prod",
            Some("production"),
            "staging"
        ));
        assert_eq!(
            token_scope_error("production", "staging"),
            "This token is scoped to production; it cannot target staging."
        );
        assert_eq!(
            scoped_environment_label("env_prod", Some("production")),
            "production"
        );
        assert_eq!(scoped_environment_label("env_prod", None), "env_prod");
    }

    #[test]
    fn pinned_plan_environment_mismatch() {
        assert!(ensure_pinned_plan_environment("env_prod", "production", "env_prod").is_ok());
        let err = ensure_pinned_plan_environment("env_prod", "staging", "env_stage")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "--environment staging does not match the pinned plan's environment env_prod."
        );
    }
}
