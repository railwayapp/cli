//! The registry of database engines whose managed features the CLI drives.
//!
//! Everything engine-specific that is DATA -- the HA companion's template
//! code, the PITR archive variable contract, which Railway-built image
//! lineages carry each capability -- is declared here and nowhere else. The
//! commands and controllers resolve an [`DatabaseEngine`] and stay
//! engine-name-free, so adding an engine is an entry in this file rather than
//! a new branch in every flow.
//!
//! The entries mirror the platform's own registries -- `DATABASE_ENGINES`
//! (frontend `lib/databaseEngines.ts`) for the HA companion, `pitrEngineSpecs`
//! (`@railway/models` `src/pitr.ts`) for the archive variable contract -- but
//! only as far as the CLI actually reads them, so an unused field never sits
//! here going stale against its source.
//!
//! Anything a TEMPLATE declares about itself -- `haActiveVariable`,
//! `clusterWiring`, `haConversionConfig`, `adoptionImageEligibility` -- is
//! deliberately absent: those are read off the live config or template record
//! at runtime (see `database_plugins` and `adoption_eligibility`), so shipping
//! support for a new image major, or rewiring a cluster, stays a template
//! update rather than a CLI release.

/// How a cluster's data nodes are asked about their role, and driven to hand
/// the primary over -- the one part of HA that is a protocol, not a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchoverMechanism {
    /// The data nodes run a coordinator with a rich member API the CLI speaks
    /// (`clusterWiring.memberStatusApi.protocol == "patroni"`): member list,
    /// roles, lag, and a cluster-wide switchover endpoint. Postgres.
    Patroni,
    /// The data nodes expose the platform's generic role/switchover REST
    /// contract (`clusterWiring.dataNodeRoleCheck` / `dataNodeSwitchover`):
    /// GET role on a node says whether it is the primary, POST switchover asks
    /// that node's own colocated coordinator to make it one. Redis (Sentinel)
    /// and MySQL (Group Replication).
    DeclaredHttp,
}

/// The engine's HA companion template, when one ships.
#[derive(Debug, Clone, Copy)]
pub struct HaCompanion {
    /// Template deployed onto the existing service to convert it, and reverted
    /// to tear the cluster back down. Only a fallback: a service provisioned
    /// from a first-party template carries its own `haTemplateCode`, which
    /// wins (see [`DatabaseEngine::ha_template_code_for`]).
    pub template_code: &'static str,
    /// Variable the HA agent sets to "true" when the cluster is active, for
    /// clusters converted before templates declared `haActiveVariable`.
    /// `None` for engines whose HA companion always declared it.
    pub legacy_active_variable: Option<&'static str>,
    pub switchover: SwitchoverMechanism,
}

/// Which live-archive probe implementation backs an engine's PITR status.
///
/// Declared kind -> code registry, so the probe is selected by declaration
/// rather than by branching on the engine's name. An engine that declares
/// none simply reports no coverage detail: the archive's own restore is the
/// hard guard, and inventing a probe it has no tool for would be worse than
/// saying nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitrProbeKind {
    PgBackRest,
}

/// The engine's PITR contract -- the mirror of one `pitrEngineSpecs` entry.
#[derive(Debug, Clone, Copy)]
pub struct PitrSpec {
    /// Composable template overlaid onto the root service to enable archiving.
    pub template_code: &'static str,
    /// Prefix of the archive variable contract the template stamps. Every
    /// engine's contract is the same six variables, differing only in prefix.
    pub archive_var_prefix: &'static str,
    /// Live coverage probe backing `pitr status`, when one exists for this
    /// engine's archiver.
    pub probe_kind: Option<PitrProbeKind>,
    /// Whether PITR is supported on an HA cluster of this engine. MySQL's
    /// archiver refuses to run whenever the Group Replication seed list is
    /// set, so its PITR is standalone-only and the HA progress/cancel
    /// subcommands have nothing to drive.
    pub supports_ha: bool,
}

impl PitrSpec {
    /// The gate variable whose presence means archiving is configured. Its
    /// presence -- not its value -- is what the enable overlay stamps, and what
    /// fleet discovery keys on.
    pub fn archive_gate_variable(&self) -> String {
        format!("{}BUCKET", self.archive_var_prefix)
    }
}

/// Connection pooling, for engines that ship a pooler companion.
#[derive(Debug, Clone, Copy)]
pub struct PoolingSpec {
    pub template_code: &'static str,
    /// Substring identifying the pooler's image among a root's edge children.
    pub image_identifier: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct DatabaseEngine {
    /// The `railway <key>` command this engine's features live under, and the
    /// key used in the local ops log.
    pub key: &'static str,
    /// Engine name for user-facing copy, e.g. "Postgres".
    pub display_name: &'static str,
    pub ha: Option<HaCompanion>,
    pub pitr: Option<PitrSpec>,
    pub pooling: Option<PoolingSpec>,
}

pub const POSTGRES: DatabaseEngine = DatabaseEngine {
    key: "postgres",
    display_name: "Postgres",
    ha: Some(HaCompanion {
        template_code: "postgres-ha",
        // Clusters converted before templates declared haActiveVariable are
        // detected by the variable Patroni itself reads.
        legacy_active_variable: Some("PATRONI_ENABLED"),
        switchover: SwitchoverMechanism::Patroni,
    }),
    pitr: Some(PitrSpec {
        template_code: "postgres-pitr",
        archive_var_prefix: "WAL_ARCHIVE_",
        probe_kind: Some(PitrProbeKind::PgBackRest),
        supports_ha: true,
    }),
    pooling: Some(PoolingSpec {
        template_code: "postgres-with-pgbouncer",
        image_identifier: "pgbouncer",
    }),
};

pub const MYSQL: DatabaseEngine = DatabaseEngine {
    key: "mysql",
    display_name: "MySQL",
    ha: Some(HaCompanion {
        template_code: "mysql-ha",
        // mysql-ha has declared haActiveVariable since it shipped.
        legacy_active_variable: None,
        switchover: SwitchoverMechanism::DeclaredHttp,
    }),
    pitr: Some(PitrSpec {
        template_code: "mysql-pitr",
        archive_var_prefix: "BINLOG_ARCHIVE_",
        probe_kind: None,
        // The image's restore-on-boot runs in standalone mode only -- it is
        // refused outright whenever the cluster's seed list is set.
        supports_ha: false,
    }),
    pooling: None,
};

pub const REDIS: DatabaseEngine = DatabaseEngine {
    key: "redis",
    display_name: "Redis",
    ha: Some(HaCompanion {
        template_code: "redis-ha",
        legacy_active_variable: None,
        switchover: SwitchoverMechanism::DeclaredHttp,
    }),
    pitr: None,
    pooling: None,
};

impl DatabaseEngine {
    /// The HA companion template to deploy for `declared_ha_template_code` --
    /// the service's own `haTemplateCode` when its origin template declared
    /// one, else this engine's registry default. Services provisioned before
    /// templates carried the field (and legacy deploys with no template link
    /// at all) have no declaration, which is exactly what the fallback covers.
    pub fn ha_template_code_for(&self, declared: Option<&str>) -> Option<String> {
        declared
            .map(str::to_string)
            .or_else(|| self.ha.map(|ha| ha.template_code.to_string()))
    }
}

/// A parsed Docker image reference: registry host (when one is present) and
/// repository path, with any tag or digest stripped.
///
/// Mirrors `parseDockerImage` from `@railway/images`: a leading segment counts
/// as the registry only when it looks like a host (contains a dot or colon, or
/// is `localhost`), so `mysql:8` parses as the Docker-library path `mysql`
/// rather than a host named `mysql`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub domain: Option<String>,
    pub path: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

pub fn parse_image_ref(image: &str) -> Option<ImageRef> {
    let image = image.trim();
    if image.is_empty() {
        return None;
    }

    let (remainder, digest) = match image.split_once('@') {
        Some((remainder, digest)) if !digest.is_empty() => (remainder, Some(digest.to_string())),
        Some(_) => return None,
        None => (image, None),
    };

    let (mut domain, path_and_tag) = match remainder.split_once('/') {
        Some((first, rest)) if is_registry_host(first) => (Some(first.to_string()), rest),
        _ => (None, remainder),
    };

    // A tag can only live in the final path segment -- a colon anywhere
    // earlier belongs to a registry port, which the host split above already
    // consumed.
    let last_slash = path_and_tag.rfind('/').map(|i| i + 1).unwrap_or(0);
    let (path, tag) = match path_and_tag[last_slash..].split_once(':') {
        Some((_, tag)) => {
            let cut = last_slash + path_and_tag[last_slash..].find(':').unwrap_or(0);
            (
                path_and_tag[..cut].to_string(),
                (!tag.is_empty()).then(|| tag.to_string()),
            )
        }
        None => (path_and_tag.to_string(), None),
    };

    if path.is_empty() {
        return None;
    }
    // `docker.io` is the implicit registry; normalize it away so a reference
    // written either way compares equal.
    if domain.as_deref() == Some("docker.io") {
        domain = None;
    }

    Some(ImageRef {
        domain,
        path,
        tag,
        digest,
    })
}

fn is_registry_host(segment: &str) -> bool {
    segment == "localhost" || segment.contains('.') || segment.contains(':')
}

/// The `{major, minor}` an image TAG declares. `minor` is `None` for a bare
/// major tag (`:8`), and the whole result is `None` when the tag declares no
/// leading-digit version (`:latest`, a named tag, no tag at all, or a
/// digest-only reference -- whose `sha256:...` payload is not a version).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageTagVersion {
    pub major: i64,
    pub minor: Option<i64>,
}

pub fn image_tag_version(image: Option<&str>) -> Option<ImageTagVersion> {
    let tag = parse_image_ref(image?)?.tag?;
    let mut parts = tag.split('.');
    let major: i64 = parts.next()?.parse().ok()?;
    // Only a purely numeric second component is a minor: `8.2` is, the `2-alpine`
    // of `8.2-alpine` is not.
    let minor = parts.next().and_then(|part| part.parse::<i64>().ok());
    Some(ImageTagVersion { major, minor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_qualified_references() {
        let parsed = parse_image_ref("ghcr.io/railwayapp-templates/postgres-ssl:16").unwrap();
        assert_eq!(parsed.domain.as_deref(), Some("ghcr.io"));
        assert_eq!(parsed.path, "railwayapp-templates/postgres-ssl");
        assert_eq!(parsed.tag.as_deref(), Some("16"));
        assert!(parsed.digest.is_none());
    }

    #[test]
    fn parses_docker_library_references_without_a_host() {
        let parsed = parse_image_ref("mysql:8.4").unwrap();
        assert!(parsed.domain.is_none());
        assert_eq!(parsed.path, "mysql");
        assert_eq!(parsed.tag.as_deref(), Some("8.4"));

        // An explicit docker.io normalizes to the same shape.
        assert_eq!(
            parse_image_ref("docker.io/library/redis:8").unwrap().domain,
            None
        );
    }

    #[test]
    fn parses_ports_and_digests_without_mistaking_them_for_tags() {
        let parsed = parse_image_ref("localhost:5000/team/db:16").unwrap();
        assert_eq!(parsed.domain.as_deref(), Some("localhost:5000"));
        assert_eq!(parsed.path, "team/db");
        assert_eq!(parsed.tag.as_deref(), Some("16"));

        let parsed = parse_image_ref("ghcr.io/owner/img@sha256:abc123").unwrap();
        assert_eq!(parsed.path, "owner/img");
        assert!(parsed.tag.is_none());
        assert_eq!(parsed.digest.as_deref(), Some("sha256:abc123"));
    }

    #[test]
    fn image_tag_version_reads_major_and_optional_minor() {
        let v = image_tag_version(Some("ghcr.io/railwayapp-templates/mysql-ha/mysql:8.4")).unwrap();
        assert_eq!(v.major, 8);
        assert_eq!(v.minor, Some(4));

        let v = image_tag_version(Some("ghcr.io/railwayapp-templates/postgres-ssl:16")).unwrap();
        assert_eq!(v.major, 16);
        assert_eq!(v.minor, None);

        // A suffixed tag carries a major but no readable minor.
        let v = image_tag_version(Some("redis:8.2-alpine")).unwrap();
        assert_eq!(v.major, 8);
        assert_eq!(v.minor, None);

        assert!(image_tag_version(Some("ghcr.io/x/y:latest")).is_none());
        assert!(image_tag_version(Some("ghcr.io/x/y")).is_none());
        // A digest is not a version: `sha256:23a8...` must never read as 23.
        assert!(image_tag_version(Some("ghcr.io/x/y@sha256:23a8ff")).is_none());
        assert!(image_tag_version(None).is_none());
    }

    #[test]
    fn ha_template_code_prefers_the_services_own_declaration() {
        // A first-party service declares the companion its origin template
        // names; the registry default only covers services that declare none.
        assert_eq!(
            POSTGRES.ha_template_code_for(Some("postgres-ha-custom")),
            Some("postgres-ha-custom".to_string())
        );
        assert_eq!(
            POSTGRES.ha_template_code_for(None),
            Some("postgres-ha".to_string())
        );
        assert_eq!(
            MYSQL.ha_template_code_for(None),
            Some("mysql-ha".to_string())
        );
        assert_eq!(
            REDIS.ha_template_code_for(None),
            Some("redis-ha".to_string())
        );
    }

    #[test]
    fn pitr_gate_variable_follows_the_declared_prefix() {
        assert_eq!(
            POSTGRES.pitr.unwrap().archive_gate_variable(),
            "WAL_ARCHIVE_BUCKET"
        );
        assert_eq!(
            MYSQL.pitr.unwrap().archive_gate_variable(),
            "BINLOG_ARCHIVE_BUCKET"
        );
    }

    #[test]
    fn capability_declarations_match_what_ships() {
        // Redis has no PITR archiver and no pooler; MySQL PITR is
        // standalone-only. These are the gates the command tree reads to
        // decide which subcommands exist, so they are worth pinning.
        assert!(REDIS.pitr.is_none());
        assert!(REDIS.pooling.is_none());
        assert!(MYSQL.pooling.is_none());
        assert!(!MYSQL.pitr.unwrap().supports_ha);
        assert!(POSTGRES.pitr.unwrap().supports_ha);
        assert!(POSTGRES.pooling.is_some());
    }
}
