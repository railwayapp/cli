// Rebuild when the GraphQL queries and mutations change
fn main() {
    println!("cargo:rerun-if-changed=src/gql/queries/strings");
    println!("cargo:rerun-if-changed=src/gql/mutations/strings");
    println!("cargo:rerun-if-changed=src/gql/subscriptions/strings");
    println!("cargo:rerun-if-changed=src/gql/schema.json");

    // Expose the compile-time target triple so the self-updater fetches the
    // correct release asset (respects ABI: gnu vs musl, msvc vs gnu, etc.).
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-env=BUILD_TARGET={target}");
}
