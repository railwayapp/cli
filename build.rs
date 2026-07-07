// Rebuild when the GraphQL queries and mutations change
fn main() {
    println!("cargo:rerun-if-changed=src/gql/queries/strings");
    println!("cargo:rerun-if-changed=src/gql/mutations/strings");
    println!("cargo:rerun-if-changed=src/gql/subscriptions/strings");
    println!("cargo:rerun-if-changed=src/gql/schema.json");

    // Rebuild when git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");

    // Expose the compile-time target triple so the self-updater fetches the
    // correct release asset (respects ABI: gnu vs musl, msvc vs gnu, etc.).
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-env=BUILD_TARGET={target}");

    // Expose git commit hash
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={}", git_sha);

    // Expose build date (ISO 8601 format in UTC)
    let build_date = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_DATE={}", build_date);

    // Expose rustc version
    let rustc_version = std::process::Command::new("rustc")
        .args(["--version"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RUSTC_VERSION={}", rustc_version);
}
