// Rebuild when the GraphQL queries and mutations change
fn main() {
    println!("cargo:rerun-if-changed=src/gql/queries/strings");
    println!("cargo:rerun-if-changed=src/gql/mutations/strings");
    println!("cargo:rerun-if-changed=src/gql/subscriptions/strings");
    println!("cargo:rerun-if-changed=src/gql/schema.json");
}
