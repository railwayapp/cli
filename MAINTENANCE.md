# Maintenance

## Background

Maintained fork: `0xble/railway-cli` of `railwayapp/cli`; owned branch `master`,
upstream `origin/master`. Canonical source: `/Users/brianle/Repos/railway-cli`.
Working implementation: `/Users/brianle/Repos/railway-cli-multiaccount`.
Accepted upstream baseline: `17c8e2d` (5.52.0); publish only to `fork`.

## Preserve

- Named-account selection precedes refresh, telemetry, updates, HTTP, WebSocket,
  SSH, MCP, and subprocess authentication. Zero profiles retains legacy;
  one auto-selects; multiple require `--account`. Unknown explicit names fail.
- `~/.railway/accounts` profiles never use ambient Railway tokens. Imports do
  not change legacy config. Listing is alphabetical metadata, never secrets.
- Source publication and installation are separate. Installation requires
  authorization, rollback artifact, SHA proof, and unchanged live auth bytes.

## Active patches

### MULTIACCOUNT-001

- Named accounts, local list/import, account-scoped saved connections, Claude
  cache and agent preferences, MCP child argv pinning, private no-clobber import.
- Upstream issue: https://github.com/railwayapp/cli/issues/688
- Tests: `tests/multiaccount.rs`, `tests/multiaccount_auth.rs` (loopback HTTP),
  `tests/account_errors.rs` (portable JSON/path/ordering contract).
- The upstream 5.52.0 code-connection archive was reconciled using
  `Configs::account_data_dir_in(home)` for every archive/snapshot path.
- Do not automatically migrate ancillary state, SSH keys, or editor credentials.
- Retire when upstream supplies equivalent isolated named profiles.

### ACCOUNT-HARDENING-002

- Fork identity and upstream replacement policy are compile-time Cargo metadata.
  Both detached self-update and explicit package-manager upgrade are blocked.
  `railway upgrade --check` exposes fork identity and source commit. Removing
  the fork metadata restores upstream policy; no runtime bypass exists.
- Config reads fail closed on corruption and I/O failures without overwriting
  original bytes. Diagnostics omit deserializer values. Restore a known-good
  private backup rather than deleting the damaged store automatically.
- Session lock failures block refresh. Separate short write locks serialize
  credential merge and atomic publication without recursively locking refresh.
- Local MCP reloads its pinned config and discards the bearer-bearing client
  after logout, file removal, or unreadable config. In-flight requests already
  sent cannot be recalled.
- JSON account-selection errors use ACCOUNT_REQUIRED, ACCOUNT_UNKNOWN and
  ACCOUNT_INVALID. The parsed --json flag controls output, not child argv.
- Existing Linux/macOS/Windows hosted matrix explicitly runs portable account
  and store regressions. A missing/billing-blocked run is never called passing.
- Windows MSVC/GNU binaries reserve an 8 MiB main stack for the async dispatcher;
  the debug executable otherwise overflows before even parsing `--help`.
  Portable subprocess tests exercise the actual debug binary without skipping.
- Debug-only `RAILWAY_TEST_HOME` isolates subprocess credential fixtures on
  Windows, whose known-folder API ignores HOME/USERPROFILE. It must be absolute
  and is compiled out of release builds; release credential routing is unchanged.
  The mutating Windows subprocess fixtures run only in debug mode; release-mode
  tests must never fall back to a developer's real known-folder credentials.
- Tests: config lock/contention/redaction unit tests; config_store subprocess
  tests; actual loopback MCP no-Authorization regression; fork updater unit and
  package-manager subprocess refusal tests.
- Retire each behavior when upstream has equivalent tested protections.

## Update and verify

Fetch `origin/master` and `fork/master`; reconcile upstream before publication.
Run `cargo fmt --all --check`, `git diff --check`, `cargo test --locked`,
`cargo check --locked`, `cargo clippy --locked`, and release build. Review the
exact candidate once with independent-context evidence; disclose provider
fallbacks. Verify PR state and remote SHA after landing. Build the landed source
for installation so its embedded source commit identifies the installed tree.

Install the fork release with a preserved package-entry rollback and immutable
SHA-addressed executable, without editing auth. Never use upstream npm/brew
upgrade as the fork maintenance path. Runtime update protection cannot prevent
a human explicitly reinstalling an upstream package.
