# Railway CLI

![Build](https://github.com/railwayapp/cli/workflows/Build/badge.svg)

This is the command line interface for [Railway](https://railway.app). Use it to connect your code to Railways infrastructure without needing to worry about environment variables or configuration.

[View the docs](https://docs.railway.app/develop/cli)

## Installation

The Railway CLI is available through [Homebrew](https://brew.sh/), [NPM](https://www.npmjs.com/package/@railway/cli), curl, or as a [Nixpkg](https://nixos.org).

### Brew

```shell
brew install railway
```

### NPM

```shell
npm i -g @railway/cli
```

### Yarn

```shell
yarn global add @railway/cli
```

### curl

```shell
curl -fsSL https://railway.app/install.sh | sh
```

### Nixpkg
Note: This installation method is not supported by Railway and is maintained by the community.
```shell
# On NixOS
nix-env -iA nixos.railway
# On non-NixOS
nix-env -iA nixpkgs.railway
```

### From source
See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repo locally.

## Documentation

[View the full documentation](https://docs.railway.app)

## Feedback

We would love to hear your feedback or suggestions. The best way to reach us is on [Discord](https://discord.gg/xAm2w6g).

We also welcome pull requests into this repo. See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repo locally.
