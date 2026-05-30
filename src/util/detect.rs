//! Conservative service-dependency detection for the guest-app flow.
//!
//! Scans the cwd for explicit data-store client libraries in known
//! manifest files. Today these hints are printed as local diagnostics
//! only; automatic provisioning is intentionally not wired until the
//! backend has a durable resource-creation flow.
//!
//! Conservative on purpose: only matches **explicit** client libraries
//! whose presence strongly implies a specific data store (e.g., `pg`
//! → Postgres). Ambiguous signals (`prisma`, `drizzle-orm` without
//! provider config) are deliberately skipped — false positives clutter
//! diagnostics now and would be dangerous if later wired to provision.
//!
//! Keep the slug allowlist aligned with backend-supported templates:
//!   - postgres
//!   - redis
//!   - mysql
//!   - mongo
//!   - valkey (no public client library yet; placeholder)
//!
//! Empty hint list is the right answer when we're not sure. The user
//! can still add services manually.

use std::collections::BTreeSet;
use std::path::Path;

/// Returns the sorted, deduplicated list of detected service slugs.
pub fn detect_services(cwd: &Path) -> Vec<String> {
    let mut found = BTreeSet::new();

    scan_package_json(cwd, &mut found);
    scan_requirements_txt(cwd, &mut found);
    scan_pyproject_toml(cwd, &mut found);
    scan_gemfile(cwd, &mut found);
    scan_go_mod(cwd, &mut found);
    scan_cargo_toml(cwd, &mut found);

    found.into_iter().collect()
}

fn read_file(cwd: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(cwd.join(name)).ok()
}

/// Match against the dependency-graph string of a manifest file. The
/// patterns are conservative — these libraries are unambiguously
/// associated with a specific data store (so `pg` matches Postgres,
/// but the more generic `prisma` doesn't match anything because
/// Prisma can target any of the four).
fn scan_package_json(cwd: &Path, out: &mut BTreeSet<String>) {
    let Some(contents) = read_file(cwd, "package.json") else {
        return;
    };
    // String search over the full file works fine here — any match
    // inside `dependencies` / `devDependencies` blocks shows up. Doing
    // a real JSON parse + key lookup would be more correct but adds
    // a json crate and complexity for marginal benefit.
    let pkg_match = |needle: &str| contents.contains(&format!("\"{}\"", needle));

    // Postgres clients
    if pkg_match("pg")
        || pkg_match("postgres")
        || pkg_match("@databases/pg")
        || pkg_match("node-postgres")
    {
        out.insert("postgres".to_owned());
    }
    // Redis clients
    if pkg_match("redis")
        || pkg_match("ioredis")
        || pkg_match("bullmq")
        || pkg_match("bull")
    {
        out.insert("redis".to_owned());
    }
    // MySQL clients
    if pkg_match("mysql") || pkg_match("mysql2") {
        out.insert("mysql".to_owned());
    }
    // MongoDB clients
    if pkg_match("mongodb") || pkg_match("mongoose") {
        out.insert("mongo".to_owned());
    }
}

fn scan_requirements_txt(cwd: &Path, out: &mut BTreeSet<String>) {
    let Some(contents) = read_file(cwd, "requirements.txt") else {
        return;
    };
    let lower = contents.to_lowercase();
    // requirements.txt lines look like `psycopg2-binary==2.9.9` or just
    // `psycopg2`. Match on a prefix of each line.
    let any = |needles: &[&str]| {
        lower.lines().any(|line| {
            let head = line
                .split_once(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .map(|(h, _)| h)
                .unwrap_or(line);
            needles.iter().any(|n| head == *n)
        })
    };
    if any(&["psycopg2", "psycopg2-binary", "psycopg", "asyncpg"]) {
        out.insert("postgres".to_owned());
    }
    if any(&["redis", "aioredis", "rq", "celery"]) {
        out.insert("redis".to_owned());
    }
    if any(&["mysqlclient", "pymysql", "aiomysql"]) {
        out.insert("mysql".to_owned());
    }
    if any(&["pymongo", "motor"]) {
        out.insert("mongo".to_owned());
    }
}

fn scan_pyproject_toml(cwd: &Path, out: &mut BTreeSet<String>) {
    let Some(contents) = read_file(cwd, "pyproject.toml") else {
        return;
    };
    let lower = contents.to_lowercase();
    // pyproject.toml uses quoted deps in tables. Substring check is OK
    // here because the names are distinctive.
    if lower.contains("\"psycopg2\"")
        || lower.contains("\"psycopg2-binary\"")
        || lower.contains("\"psycopg\"")
        || lower.contains("\"asyncpg\"")
    {
        out.insert("postgres".to_owned());
    }
    if lower.contains("\"redis\"") || lower.contains("\"aioredis\"") {
        out.insert("redis".to_owned());
    }
    if lower.contains("\"mysqlclient\"")
        || lower.contains("\"pymysql\"")
        || lower.contains("\"aiomysql\"")
    {
        out.insert("mysql".to_owned());
    }
    if lower.contains("\"pymongo\"") || lower.contains("\"motor\"") {
        out.insert("mongo".to_owned());
    }
}

fn scan_gemfile(cwd: &Path, out: &mut BTreeSet<String>) {
    let Some(contents) = read_file(cwd, "Gemfile") else {
        return;
    };
    // Gemfile lines look like `gem "pg"` or `gem 'redis'`.
    let has_gem = |name: &str| {
        let needle_double = format!("gem \"{}\"", name);
        let needle_single = format!("gem '{}'", name);
        contents.contains(&needle_double) || contents.contains(&needle_single)
    };
    if has_gem("pg") {
        out.insert("postgres".to_owned());
    }
    if has_gem("redis") || has_gem("sidekiq") {
        out.insert("redis".to_owned());
    }
    if has_gem("mysql2") {
        out.insert("mysql".to_owned());
    }
    if has_gem("mongoid") || has_gem("mongo") {
        out.insert("mongo".to_owned());
    }
}

fn scan_go_mod(cwd: &Path, out: &mut BTreeSet<String>) {
    let Some(contents) = read_file(cwd, "go.mod") else {
        return;
    };
    if contents.contains("github.com/jackc/pgx")
        || contents.contains("github.com/lib/pq")
    {
        out.insert("postgres".to_owned());
    }
    if contents.contains("github.com/redis/go-redis")
        || contents.contains("github.com/go-redis/redis")
    {
        out.insert("redis".to_owned());
    }
    if contents.contains("github.com/go-sql-driver/mysql") {
        out.insert("mysql".to_owned());
    }
    if contents.contains("go.mongodb.org/mongo-driver") {
        out.insert("mongo".to_owned());
    }
}

fn scan_cargo_toml(cwd: &Path, out: &mut BTreeSet<String>) {
    let Some(contents) = read_file(cwd, "Cargo.toml") else {
        return;
    };
    let has = |name: &str| {
        // `tokio-postgres = "..."` or `tokio-postgres = { ... }`
        contents.contains(&format!("{} =", name))
            || contents.contains(&format!("\"{}\"", name))
    };
    if has("tokio-postgres") || has("sqlx") && contents.contains("postgres") {
        out.insert("postgres".to_owned());
    }
    if has("redis") {
        out.insert("redis".to_owned());
    }
    if has("mysql_async") || has("mysql") && !has("tokio-postgres") {
        out.insert("mysql".to_owned());
    }
    if has("mongodb") {
        out.insert("mongo".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir() -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("railway-detect-{}", rand::random::<u32>()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detects_postgres_from_package_json() {
        let dir = tmp_dir();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"pg":"^8.0.0","express":"^4.0.0"}}"#,
        )
        .unwrap();
        let got = detect_services(&dir);
        assert_eq!(got, vec!["postgres".to_owned()]);
    }

    #[test]
    fn detects_postgres_and_redis() {
        let dir = tmp_dir();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"pg":"^8","ioredis":"^5"}}"#,
        )
        .unwrap();
        let got = detect_services(&dir);
        assert_eq!(got, vec!["postgres".to_owned(), "redis".to_owned()]);
    }

    #[test]
    fn detects_from_requirements_txt() {
        let dir = tmp_dir();
        fs::write(
            dir.join("requirements.txt"),
            "fastapi==0.110.0\npsycopg2-binary==2.9.9\nredis==5.0.1\n",
        )
        .unwrap();
        let got = detect_services(&dir);
        assert_eq!(got, vec!["postgres".to_owned(), "redis".to_owned()]);
    }

    #[test]
    fn no_match_returns_empty() {
        let dir = tmp_dir();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"express":"^4"}}"#,
        )
        .unwrap();
        let got = detect_services(&dir);
        assert!(got.is_empty());
    }

    #[test]
    fn prisma_alone_does_not_match_postgres() {
        // Conservative: prisma can target any DB, don't auto-detect.
        let dir = tmp_dir();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"@prisma/client":"^5","prisma":"^5"}}"#,
        )
        .unwrap();
        let got = detect_services(&dir);
        assert!(got.is_empty(), "expected no match, got {:?}", got);
    }

    #[test]
    fn detects_mongo_from_mongoose() {
        let dir = tmp_dir();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"mongoose":"^8"}}"#,
        )
        .unwrap();
        let got = detect_services(&dir);
        assert_eq!(got, vec!["mongo".to_owned()]);
    }

    #[test]
    fn deduplicates_across_files() {
        let dir = tmp_dir();
        fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"pg":"^8"}}"#,
        )
        .unwrap();
        fs::write(dir.join("requirements.txt"), "psycopg2-binary\n").unwrap();
        let got = detect_services(&dir);
        assert_eq!(got, vec!["postgres".to_owned()]);
    }
}
