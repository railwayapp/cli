#!/usr/bin/env sh
#
# Railway agent setup installer.
#
# Installs (or reuses) the Railway CLI and configures agent support in a
# single step. This is a thin convenience wrapper so the published one-liner
# can stay clean:
#
#     curl -fsSL railway.com/agents.sh | sh
#
# It is exactly equivalent to:
#
#     curl -fsSL railway.com/install.sh | sh -s -- --agents -y
#
# By default this configures the remote MCP server via `railway mcp proxy`
# (CLI login auth). Extra arguments are forwarded to install.sh, e.g. to opt
# into the local MCP server instead:
#
#     curl -fsSL railway.com/agents.sh | sh -s -- --local
#
set -eu

curl -fsSL https://railway.com/install.sh | sh -s -- --agents -y "$@"
