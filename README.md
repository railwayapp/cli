# Railway CLI

[![Crates.io](https://img.shields.io/crates/v/railwayapp)](https://crates.io/crates/railwayapp)
[![CI](https://github.com/railwayapp/cli/actions/workflows/ci.yml/badge.svg)](https://github.com/railwayapp/cliv3/actions/workflows/ci.yml)
[![cargo audit](https://github.com/railwayapp/cli/actions/workflows/cargo-audit.yml/badge.svg)](https://github.com/railwayapp/cli/actions/workflows/cargo-audit.yml)

This is the command line interface for [Railway](https://railway.app). Use it to connect your code to Railway's infrastructure without needing to worry about environment variables or configuration.

[View the docs](https://docs.railway.app/develop/cli)

The Railway command line interface (CLI) connects your code to your Railway project from the command line.

The Railway CLI allows you to

- Create new Railway projects from the terminal
- Link to an existing Railway project
- Pull down environment variables for your project locally to run
- Create services and databases right from the comfort of your fingertips
## Status
Currently pre-release. We are looking for feedback and suggestions. Please join our [Discord](https://discord.gg/railway) to provide feedback.

## Installation

### Cargo
```bash
cargo install railwayapp --locked
```

### Homebrew

```bash 
brew tap railwayapp/tap
brew install rlwy
```

### NPM
```bash
npm install -g @railway/cli
```

### Bash
```bash
# Install 
bash <(curl -fsSL cli.new)

# Uninstall
bash <(curl -fsSL cli.new) -r
```

### Scoop
```ps1
scoop install railway
```

### From source
See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repo locally.

## Documentation
[View the full documentation](https://docs.railway.app)

## Feedback

We would love to hear your feedback or suggestions. The best way to reach us is on [Discord](https://discord.gg/railway).

We also welcome pull requests into this repo. See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repo locally.
