# Railway CLI

[![Crates.io](https://img.shields.io/crates/v/railwayapp)](https://crates.io/crates/railwayapp)
[![CI](https://github.com/railwayapp/cli/actions/workflows/ci.yml/badge.svg)](https://github.com/railwayapp/cliv3/actions/workflows/ci.yml)
[![cargo audit](https://github.com/railwayapp/cli/actions/workflows/cargo-audit.yml/badge.svg)](https://github.com/railwayapp/cli/actions/workflows/cargo-audit.yml)

## Overview

This is the command line interface for [Railway](https://railway.com). Use it to connect your code to Railway's infrastructure without needing to worry about environment variables or configuration.

The Railway command line interface (CLI) connects your code to your Railway project from the command line.

The Railway CLI allows you to:

- Create new Railway projects from the terminal
- Link to an existing Railway project
- Pull down environment variables for your project locally to run
- Create services and databases right from the comfort of your fingertips

And more.

## Documentation

[View the CLI guide](https://docs.railway.com/guides/cli)

[View the CLI API reference](https://docs.railway.com/reference/cli-api)

## Quick start

Follow the [CLI guide](https://docs.railway.com/guides/cli) to install the CLI and run your first command.

## Authentication

For non-interactive authentication details, see the [CLI guide](https://docs.railway.com/guides/cli#tokens).

## Installation

### Package managers

#### Cargo

```bash
cargo install railwayapp --locked
```

#### Homebrew

```bash
brew install railway
```

#### NPM

```bash
npm install -g @railway/cli
```

#### Bash

```bash
# Install
bash <(curl -fsSL cli.new)

# Uninstall
bash <(curl -fsSL cli.new) -r
```

#### Scoop

```ps1
scoop install railway
```

#### Arch Linux AUR

Install with Paru

```bash
paru -S railwayapp-cli
```

Install with Yay

```bash
yay -S railwayapp-cli
```

### Docker

#### Install from the command line

```bash
docker pull ghcr.io/railwayapp/cli:latest
```

#### Use in GitHub Actions

For GitHub Actions setup, see the blog post at [blog.railway.com/p/github-actions](https://blog.railway.com/p/github-actions).

#### Use in GitLab CI/CD

For GitLab CI/CD setup, see the blog post at [blog.railway.com/p/gitlab-ci-cd](https://blog.railway.com/p/gitlab-ci-cd).

### Contributing

See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repository locally.

## Feedback

We would love to hear your feedback or suggestions. The best way to reach us is on [Central Station](https://station.railway.com/feedback).

We also welcome pull requests into this repository. See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repository locally.
