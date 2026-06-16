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
# Any extra arguments are forwarded to install.sh, e.g. to opt into the
# remote MCP server:
#
#     curl -fsSL railway.com/agents.sh | sh -s -- --remote
#
set -eu

curl -fsSL https://railway.com/install.sh | sh -s -- --agents -y "$@"
