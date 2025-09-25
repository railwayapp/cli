# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Commands

- `cargo run -- <args>` - Run CLI during development
- `cargo test` - Run tests
- `cargo lint-fix` - Fix linting issues automatically (run after making changes)
- `cargo fmt` - Format code (run after making changes)
- `cargo clippy` - Check for linting issues
- `nix-shell` - Enter dev environment with dependencies

## Architecture

- **Commands**: `src/commands/` - CLI commands using clap derives, each with `exec()` function
- **Controllers**: `src/controllers/` - Business logic for Railway entities (project, service, deployment)  
- **GraphQL**: `src/gql/` - Generated type-safe queries/mutations for Railway API
- **Config**: `src/config.rs` - Authentication and project settings
- **Workspace**: `src/workspace.rs` - Multi-project context handling

### Command System
Commands use a macro system in `main.rs`. The `commands!` macro generates routing for modules in `src/commands/`.

### Authentication
- Project tokens via `RAILWAY_TOKEN` environment variable
- User tokens via OAuth flow stored in config directory
