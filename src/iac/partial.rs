use std::collections::BTreeMap;

pub const PROJECT_PARTIAL: &str = "*";
const PARTIAL_NAME: &str = r"^[a-zA-Z0-9._-]{1,64}$";

pub type IacPartials = BTreeMap<String, String>;

pub fn parse_partial_name(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value == PROJECT_PARTIAL || !regex_is_match(value) {
        return Err(format!(
            "Invalid partial export: {value:?}. Use a 1–64 character name matching [a-zA-Z0-9._-]+."
        ));
    }
    Ok(Some(value.to_string()))
}

fn regex_is_match(value: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(PARTIAL_NAME).expect("partial name regex"))
        .is_match(value)
}

pub fn has_named_partials(owners: Option<&IacPartials>) -> bool {
    owners
        .map(|owners| owners.values().any(|owner| owner != PROJECT_PARTIAL))
        .unwrap_or(false)
}

pub fn owner_of<'a>(owners: Option<&'a IacPartials>, address: &str) -> Option<&'a str> {
    owners
        .and_then(|owners| owners.get(address))
        .map(String::as_str)
}

pub fn effective_partial(partial: Option<&str>) -> &str {
    partial.unwrap_or(PROJECT_PARTIAL)
}

pub fn foreign_resource_message(address: &str, owner: &str) -> String {
    let (kind, name) = address
        .split_once('.')
        .map(|(kind, name)| (kind, name))
        .unwrap_or(("resource", address));
    format!("Cannot manage {kind} \"{name}\": already managed by partial \"{owner}\".")
}

pub fn nameless_file_message() -> String {
    "This environment already has named IaC partials. Export `const partial = \"<name>\"` from this file instead of managing the whole project.".to_string()
}

pub fn needs_partial_claim_apply(
    declared: &[String],
    owners: Option<&IacPartials>,
    partial: Option<&str>,
) -> bool {
    let p = effective_partial(partial);
    declared
        .iter()
        .any(|address| owner_of(owners, address) != Some(p))
}
