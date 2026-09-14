# Railway terminal history patch

This is the crates.io `vt100` 0.16.2 source, under its original MIT license.
It is compiled as an internal module so the scrollback fix ships in both release
binaries and the published `railwayapp` crate. Cargo strips `[patch]` overrides
when publishing, so a local dependency patch would only fix repository builds.

For module integration, `lib.rs` becomes `mod.rs`, internal `crate::` paths become
`super::`, and upstream crate lint settings are replaced with module-local
allowances for unused library APIs and Clippy. Rustfmt applies the CLI's format.
The original manifest is retained as `Cargo.toml.upstream`; its three runtime
dependencies are declared in Railway's manifest.

`Grid::scroll_up` retains primary-screen lines whenever the scrolling region
starts at row zero, including a region that ends above a fixed composer.
Upstream only retains lines when the scrolling region spans the whole screen.
Codex uses a shorter region, so its output previously disappeared instead of
entering the Railway terminal pane's scrollback.

Regions below a fixed header still discard their scrolled rows. Alternate-screen
grids still have zero history capacity. History limits and cell formatting are
unchanged. The now-unused `scroll_region_active` helper was removed.

Regression coverage lives in `src/commands/cloud_agent/tui/session.rs`, including
an opt-in test against an installed Codex binary (`RAILWAY_TEST_CODEX_BIN`).
