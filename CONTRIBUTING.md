# Contribute to the Railway CLI

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install)
- [CMake](https://cmake.org/install/)

OR

- [Nix](https://nixos.org/download.html)

## Setting Up with Nix

Use `nix-shell` to enter an environment with all necessary dependencies.

## Running and Building

- Run the binary with: `cargo run -- <args>`
- Build the binary with: `cargo build --release`

## Generating the Schema

Install `graphql-client` with cargo:

```sh
cargo install graphql_client_cli
```

Then, run the following command to generate the schema:

```sh
graphql-client introspect-schema https://backboard.railway.com/graphql/v2 > src/gql/schema.json
```

## npm installer

The npm package requires Node.js 18 or newer for native fetch and tar 7.
Run `pnpm install --ignore-scripts` and `pnpm test:installer` to test the
installer against local HTTP fixtures without downloading a release binary.
