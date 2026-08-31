//! Pre-flight for the rules that decide whether an existing service may adopt
//! a managed feature -- HA conversion, or a PITR enable overlay.
//!
//! Every rule here is TEMPLATE-DECLARED and read off the fetched template
//! record: `adoptionImageEligibility` on the root slot (which image
//! repositories carry the capability, and whether a floating major tag or the
//! image's own entrypoint is required), plus `haConversionConfig` (supported
//! image majors, minor pinning, and the per-role count selectors) for
//! conversions. The CLI compiles in none of it, so extending an engine's
//! supported majors, widening its eligible images or offering a new cluster
//! size stays a template update.
//!
//! These are read from the COMPANION being applied, never from the service
//! being adopted -- the same record the server-side gate reads. A standalone
//! service may carry a partial copy of its companion's conversion config (or,
//! like the redis template, none at all), so trusting the service's copy would
//! silently skip checks for exactly the engines that need them.
//!
//! This is a pre-flight, not a boundary: `templateDeployV2` enforces the same
//! declarations server-side before creating anything. Checking locally only
//! buys a fast, specific error with the remedy attached, instead of a refusal
//! that arrives after the confirmation prompt.

use std::collections::BTreeMap;

use serde_json::Value;

use super::database_engines::{image_tag_version, parse_image_ref};

/// The declarations a template makes about adopting an existing service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdoptionRules {
    /// How refusal copy names the feature, e.g. "Point-in-time recovery".
    pub label: Option<String>,
    /// Registry-qualified repositories whose builds carry the capability.
    /// Empty means the template declares no image requirement.
    pub repositories: Vec<String>,
    /// Refuse an eligible image pinned to a `major.minor` tag or by digest,
    /// offering the floating `:major` tag as the remedy.
    pub require_floating_major_tag: bool,
    /// Refuse while the target carries a start command -- the capability is
    /// switched on by the image's own entrypoint, which a custom command
    /// overrides.
    pub require_image_entrypoint: bool,
    /// Image majors the HA companion publishes data-node images for. Empty
    /// means the template declares none (not applicable outside conversion).
    pub supported_image_major_versions: Vec<i64>,
    /// Conversion pins every node to the source's exact `major.minor`, so a
    /// source tag that declares no minor cannot be converted.
    pub pin_to_minor_version: bool,
    /// The per-role count selectors the companion declares, keyed by cluster
    /// role. A role absent from this map does not exist in the topology at
    /// all; a role present with an empty option list accepts any count.
    pub role_options: BTreeMap<String, Vec<i64>>,
}

/// Reads the declarations off a fetched template's `serializedConfig`, from
/// its root slot -- the slot the existing service is adopted into.
pub fn rules_from_template(serialized_config: &Value) -> AdoptionRules {
    let Some(root) = serialized_config
        .get("services")
        .and_then(Value::as_object)
        .and_then(|services| {
            services
                .values()
                .find(|s| s.get("clusterRole").and_then(Value::as_str) == Some("root"))
        })
    else {
        return AdoptionRules::default();
    };

    let eligibility = root.get("adoptionImageEligibility");
    let conversion = root.get("haConversionConfig");

    let flag = |parent: Option<&Value>, key: &str| {
        parent
            .and_then(|v| v.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };

    AdoptionRules {
        label: eligibility
            .and_then(|v| v.get("label"))
            .and_then(Value::as_str)
            .map(str::to_string),
        repositories: eligibility
            .and_then(|v| v.get("repositories"))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        require_floating_major_tag: flag(eligibility, "requireFloatingMajorTag"),
        require_image_entrypoint: flag(eligibility, "requireImageEntrypoint"),
        supported_image_major_versions: conversion
            .and_then(|v| v.get("supportedImageMajorVersions"))
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
        pin_to_minor_version: flag(conversion, "pinToMinorVersion"),
        role_options: ["replica", "internal", "edge"]
            .into_iter()
            .filter_map(|role| {
                let selector = conversion?.get(role).filter(|v| !v.is_null())?;
                let options = selector
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|entries| entries.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                Some((role.to_string(), options))
            })
            .collect(),
    }
}

/// What the service brings to the check.
pub struct AdoptionTarget<'a> {
    pub image: Option<&'a str>,
    pub has_start_command: bool,
}

impl AdoptionRules {
    /// Every declared rule this target fails, each phrased with its remedy.
    /// An empty result means the pre-flight found nothing -- the server-side
    /// gate remains the authority.
    pub fn blockers(&self, target: &AdoptionTarget<'_>) -> Vec<String> {
        let mut blockers = Vec::new();
        let feature = self.label.as_deref().unwrap_or("This feature");

        if !self.repositories.is_empty() && !self.image_repository_is_eligible(target.image) {
            blockers.push(format!(
                "{feature} runs in Railway's own database images, and \"{}\" is not one of them. Supported images: {}.",
                target.image.unwrap_or("(no image)"),
                self.repositories.join(", ")
            ));
        }

        if self.require_image_entrypoint && target.has_start_command {
            blockers.push(format!(
                "{feature} is switched on by the image's entrypoint, which a custom start command overrides. Clear the service's start command first."
            ));
        }

        // The pin rules only make sense for an image the capability actually
        // ships in: an ineligible image already failed above, and piling a
        // tag complaint on top of it only buries the real problem.
        if self.repositories.is_empty() || self.image_repository_is_eligible(target.image) {
            blockers.extend(self.tag_blockers(feature, target.image));
        }

        blockers
    }

    /// Whether the image's registry host and repository path match a declared
    /// repository exactly. Exact, never by prefix: sidecars share repository
    /// prefixes (`mysql-ha/haproxy` sits next to `mysql-ha/mysql`), and a
    /// substring match would also accept a lookalike registry.
    fn image_repository_is_eligible(&self, image: Option<&str>) -> bool {
        let Some(parsed) = image.and_then(parse_image_ref) else {
            return false;
        };
        let qualified = match &parsed.domain {
            Some(domain) => format!("{domain}/{}", parsed.path),
            None => parsed.path.clone(),
        };
        self.repositories.contains(&qualified)
    }

    fn tag_blockers(&self, feature: &str, image: Option<&str>) -> Vec<String> {
        let mut blockers = Vec::new();
        let Some(image) = image else {
            return blockers;
        };
        let parsed = parse_image_ref(image);
        let version = image_tag_version(Some(image));

        if self.require_floating_major_tag {
            let digest_pinned = parsed.as_ref().is_some_and(|p| p.digest.is_some());
            if digest_pinned {
                blockers.push(format!(
                    "{feature} ships its fixes by republishing the major tag, which a digest pin freezes out. Move \"{image}\" to a floating major tag (e.g. \":16\") first."
                ));
            } else if let Some(version) = version
                && version.minor.is_some()
            {
                blockers.push(format!(
                    "{feature} ships its fixes by republishing the major tag, which a minor pin freezes out. Move \"{image}\" to the major tag \":{}\" first.",
                    version.major
                ));
            }
        }

        if self.pin_to_minor_version {
            match version {
                // A bare-major source leaves the minor undeterminable, and
                // pinning the bare major would put every node on a floating
                // tag -- uniform now, mixed after the next scale-up, which is
                // the exact break the opt-in exists to prevent.
                Some(v) if v.minor.is_none() => blockers.push(format!(
                    "This cluster pins every node to the source image's exact major.minor version, but \"{image}\" declares only a major. Retag it to the minor your database is actually running (e.g. \":{}.2\") first.",
                    v.major
                )),
                None => blockers.push(format!(
                    "This cluster pins every node to the source image's exact major.minor version, but no version can be read from \"{image}\". Retag it to the minor your database is actually running first."
                )),
                _ => {}
            }
        }

        if !self.supported_image_major_versions.is_empty() {
            let supported = self
                .supported_image_major_versions
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            match version {
                Some(v) if self.supported_image_major_versions.contains(&v.major) => {}
                Some(v) => blockers.push(format!(
                    "No high-availability image is published for major version {}. Supported majors: {supported}.",
                    v.major
                )),
                None => blockers.push(format!(
                    "No version can be read from \"{image}\", so the cluster's node images cannot be pinned to it. Retag the service to a versioned tag first. Supported majors: {supported}."
                )),
            }
        }

        blockers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target<'a>(image: &'a str, has_start_command: bool) -> AdoptionTarget<'a> {
        AdoptionTarget {
            image: Some(image),
            has_start_command,
        }
    }

    /// The postgres-pitr root slot's real declaration.
    fn postgres_pitr_rules() -> AdoptionRules {
        rules_from_template(&json!({
            "services": {
                "root": {
                    "clusterRole": "root",
                    "adoptionImageEligibility": {
                        "label": "Point-in-time recovery",
                        "repositories": [
                            "ghcr.io/railwayapp-templates/postgres-ssl",
                            "ghcr.io/railwayapp-templates/postgres-ha/postgres-patroni"
                        ],
                        "requireImageEntrypoint": true,
                        "requireFloatingMajorTag": true
                    }
                }
            }
        }))
    }

    /// A declaration that names its eligible repositories but does NOT
    /// require a floating major tag -- the shape used by the engines whose
    /// image matrix publishes exact minors, where a minor tag is the normal
    /// case rather than a blocker.
    fn minor_tagged_lineage_rules() -> AdoptionRules {
        rules_from_template(&json!({
            "services": {
                "root": {
                    "clusterRole": "root",
                    "adoptionImageEligibility": {
                        "label": "High availability conversion",
                        "repositories": [
                            "ghcr.io/railwayapp-templates/mysql-ha/mysql",
                            "ghcr.io/railwayapp-templates/mysql-ha/mysql-wrapper"
                        ],
                        "requireImageEntrypoint": true
                    }
                }
            }
        }))
    }

    #[test]
    fn reads_the_declarations_off_the_root_slot() {
        let rules = postgres_pitr_rules();
        assert_eq!(rules.label.as_deref(), Some("Point-in-time recovery"));
        assert_eq!(rules.repositories.len(), 2);
        assert!(rules.require_floating_major_tag);
        assert!(rules.require_image_entrypoint);

        let rules = minor_tagged_lineage_rules();
        assert!(rules.require_image_entrypoint);
        assert!(!rules.require_floating_major_tag);
    }

    #[test]
    fn a_template_declaring_nothing_blocks_nothing() {
        let rules = rules_from_template(&json!({"services": {"root": {"clusterRole": "root"}}}));
        assert_eq!(rules, AdoptionRules::default());
        assert!(
            rules
                .blockers(&target("anything/at-all:latest", true))
                .is_empty()
        );

        // Same for a config with no root slot at all.
        assert_eq!(rules_from_template(&json!({})), AdoptionRules::default());
    }

    #[test]
    fn eligible_postgres_image_on_a_major_tag_passes() {
        let rules = postgres_pitr_rules();
        assert!(
            rules
                .blockers(&target(
                    "ghcr.io/railwayapp-templates/postgres-ssl:16",
                    false
                ))
                .is_empty()
        );
    }

    #[test]
    fn postgres_minor_and_digest_pins_are_refused_with_the_remedy() {
        let rules = postgres_pitr_rules();

        let blockers = rules.blockers(&target(
            "ghcr.io/railwayapp-templates/postgres-ssl:16.10",
            false,
        ));
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("minor pin"));
        assert!(blockers[0].contains("\":16\""));

        let blockers = rules.blockers(&target(
            "ghcr.io/railwayapp-templates/postgres-ssl@sha256:abc123",
            false,
        ));
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("digest pin"));
    }

    #[test]
    fn minor_pins_are_accepted_where_the_template_does_not_forbid_them() {
        // The exact divergence a hardcoded "minor pins are bad" rule would
        // have caused: mysql-ha/mysql only ever publishes major.minor, so
        // refusing every minor-pinned image would refuse every one of them.
        let rules = minor_tagged_lineage_rules();
        assert!(
            rules
                .blockers(&target(
                    "ghcr.io/railwayapp-templates/mysql-ha/mysql:8.4",
                    false
                ))
                .is_empty()
        );
    }

    #[test]
    fn a_start_command_blocks_only_where_the_entrypoint_is_required() {
        let rules = postgres_pitr_rules();
        let blockers = rules.blockers(&target(
            "ghcr.io/railwayapp-templates/postgres-ssl:16",
            true,
        ));
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("start command"));

        let mut relaxed = rules.clone();
        relaxed.require_image_entrypoint = false;
        assert!(
            relaxed
                .blockers(&target(
                    "ghcr.io/railwayapp-templates/postgres-ssl:16",
                    true
                ))
                .is_empty()
        );
    }

    #[test]
    fn ineligible_images_are_matched_exactly_not_by_substring() {
        let rules = postgres_pitr_rules();

        // A sibling repository under the same prefix must not pass.
        let blockers = rules.blockers(&target(
            "ghcr.io/railwayapp-templates/postgres-ha/haproxy:3",
            false,
        ));
        assert!(blockers.iter().any(|b| b.contains("not one of them")));

        // Nor a lookalike registry serving the same path.
        let blockers = rules.blockers(&target(
            "evil.example.com/railwayapp-templates/postgres-ssl:16",
            false,
        ));
        assert!(blockers.iter().any(|b| b.contains("not one of them")));

        // An ineligible image reports THAT, and is not also nagged about its
        // tag -- one clear problem beats two, one of which is noise.
        let blockers = rules.blockers(&target("postgis/postgis:16.1", false));
        assert_eq!(blockers.len(), 1);
    }

    #[test]
    fn conversion_gates_on_the_templates_declared_majors() {
        // redis-ha's real declaration: majors 7 and 8, pinned to minor, and
        // no adoptionImageEligibility at all.
        let rules = rules_from_template(&json!({
            "services": {
                "root": {
                    "clusterRole": "root",
                    "haConversionConfig": {
                        "supportedImageMajorVersions": [7, 8],
                        "pinToMinorVersion": true
                    }
                }
            }
        }));
        assert!(rules.repositories.is_empty());

        // The supported, minor-pinned case converts.
        assert!(rules.blockers(&target("redis:8.2", false)).is_empty());

        // A bare major is refused: pinning it would leave every node on a
        // floating tag.
        let blockers = rules.blockers(&target("redis:8", false));
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("major.minor"));

        // An unsupported major is refused with the list.
        let blockers = rules.blockers(&target("redis:6.2", false));
        assert!(blockers.iter().any(|b| b.contains("major version 6")));
        assert!(blockers.iter().any(|b| b.contains("7, 8")));

        // :latest fails both the pin rule and the major gate, and says so.
        let blockers = rules.blockers(&target("redis:latest", false));
        assert_eq!(blockers.len(), 2);
    }

    #[test]
    fn mysql_ha_conversion_declares_its_own_majors() {
        let rules = rules_from_template(&json!({
            "services": {
                "root": {
                    "clusterRole": "root",
                    "haConversionConfig": {
                        "supportedImageMajorVersions": [8, 9],
                        "pinToMinorVersion": true
                    }
                }
            }
        }));
        assert!(rules.blockers(&target("mysql:8.4", false)).is_empty());
        assert!(!rules.blockers(&target("mysql:5.7", false)).is_empty());
    }

    #[test]
    fn postgres_ha_conversion_accepts_a_bare_major_because_it_does_not_pin_minors() {
        let rules = rules_from_template(&json!({
            "services": {
                "root": {
                    "clusterRole": "root",
                    "haConversionConfig": { "supportedImageMajorVersions": [14, 15, 16, 17, 18] },
                    "adoptionImageEligibility": {
                        "label": "High availability conversion",
                        "repositories": ["ghcr.io/railwayapp-templates/postgres-ssl"]
                    }
                }
            }
        }));
        assert!(
            rules
                .blockers(&target(
                    "ghcr.io/railwayapp-templates/postgres-ssl:16",
                    false
                ))
                .is_empty()
        );
        assert!(
            !rules
                .blockers(&target(
                    "ghcr.io/railwayapp-templates/postgres-ssl:13",
                    false
                ))
                .is_empty()
        );
    }
}
