# Cloud-agent command guide

Use `railway ca` to browse and manage VMs, `railway code` to launch a coding
session, and `railway ca desktop` to configure a desktop app. Cloud Agents access
is required. Run any command with `--help` for its options.

## Launch and reconnect

```sh
railway code --codex
railway code --opencode
railway code --claude
railway code --codex connect my-box
```

An explicit agent flag on `railway code` creates a new VM unless `connect` or
`--agent` selects an existing one. `connect [agent]` connects to an existing
Codex/OpenCode server; `--agent <name-or-id>` selects a VM for server setup.

Codex, OpenCode, and OpenCode2 normally run a local client connected to the VM.
Their clients run inside the Railway CA interface with command approvals disabled.
Codex matches the local client to its server version and trusts the remote project.
Missing OpenCode clients can be installed after confirmation. OpenCode2 downloads
its latest Beta runtime onto the VM at startup.

Use `remote` to run the client on the VM. Use `--` to pass arguments to the agent:

```sh
railway code --codex remote
railway code --codex -- exec "explain this codebase"
```

`railway ca --codex` and `railway ca start --codex` launch on the VM and do not
use `code`'s local-client actions. `ca start` also skips the management interface.
These CA paths do not automatically request a fresh VM; use `--new` when needed.

## Defaults, targeting, and lifecycle

`railway ca setup` saves the default agent, project, and skills. Explicit flags
win over preferences. `RAILWAY_CA_AGENT` overrides the saved agent for one run.
A directory's linked project takes precedence over the saved default project.
Preferences live in `~/.railway/agent-prefs.json`.

Agent selectors accept names or IDs. Lifecycle commands without a selector use
this directory's agent or your only agent, otherwise show candidates.

Disconnecting leaves the VM running and compute billing continues.
`railway ca sleep <agent>` stops compute and ends running processes while keeping
the disk. `railway ca delete <agent>` deletes the VM and its disk.
The older `railway code --rm` deletes this environment's agent and disk.

Use `railway ca ssh <agent>` for a shell, `--session [name]` to attach to or start
a terminal session, and `--resume` to resume the most recent Claude conversation
in a fresh terminal session after sleep or restart.

In the management interface, Option+F toggles the tree and Option+N starts another
session. Piped sessions and agent arguments after `--` use the terminal directly.

## Authentication

Local sign-ins are optional: without one, sign in using the agent's login flow
on the VM. When available, Codex copies `~/.codex/auth.json`, Grok copies
`~/.grok/auth.json`, and OpenCode carries local provider sign-ins to the VM.

Claude uses a setup token from `claude setup-token`, or the supplied
`CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY`. The CLI can mint the setup token
and caches it locally in `~/.railway/claude-code-token`. It reuses a credential
already on the VM. `--refresh-auth` replaces the cached credentials with a newly
minted token; use it after revoking a token or when authentication fails.
`railway logout` clears the local token cache.

Railway Agent uses credentials already on the VM and needs no separate model
provider sign-in.

## Desktop setup

```sh
railway ca desktop --claude
railway ca desktop --codex
railway ca desktop --opencode --agent my-box
railway code --codex desktop-only --agent my-box --dir /app
```

Desktop setup uses an existing VM when available, creating one if needed.
`--new` requests a fresh VM; `--agent` selects an existing one.
`railway ca desktop --codex` alone is an alias for Codex `desktop-only` setup.
Setup writes configuration without opening the desktop app. Restart the app to
load the connection. The VM remains awake; rerun setup after sleep or restart.

Normal `railway code` Codex setup and connect also configure Desktop in the
background. OpenCode setup and connect configure the matching Desktop edition
when its settings are detected. The command reports configuration success or
failure.

Claude and Codex use generated SSH configuration. `--ssh-config` selects the file.
Codex also saves its connection and remote project in
`$CODEX_HOME/codex-app/config.json` (default `~/.codex/codex-app/config.json`).
After restarting Codex, find `Railway: <agent-name>` in the project sidebar.

OpenCode uses a public HTTPS endpoint. New VMs request a code endpoint on port
4096; existing VMs use their configured endpoint or app port 8080. Setup saves
the URL, credentials, default server, and project in the selected Desktop edition.
Open Home → Projects → Railway: <agent-name> → /app (or the selected `--dir`) →
New session. Changing the default server does not move existing chats.
An occupied server port causes setup to fail without stopping its process.

`--dry-run` previews changes without changing local configuration or the VM.
`--remove` removes local Desktop configuration and stops the managed OpenCode
server on a running VM. For Codex it removes the import declaration; remove
already-imported hosts and projects in Settings → Connections and the sidebar.

## Saved connection details and JSON

```sh
railway code get-config
railway code get-config my-box --json
railway code --codex connect my-box --connection-json
```

`get-config` shows local snapshots without login, network access, or waking a VM.
It does not verify that a saved connection still works. Omit the selector for the
latest snapshot; use an ID when names are ambiguous. Snapshots contain credentials
and are removed by `railway logout`.

`--connection-json` returns verified connection details including credentials for
Codex or OpenCode2, without opening a local client. Progress goes to stderr.
It works with setup, `connect`, and Codex `desktop-only`.

## Reusable VMs and variables

```sh
railway ca bootstrap save dev --agent my-box --default
railway code --codex --bootstrap dev
railway ca create my-box --from-checkpoint <checkpoint-id>
```

Bootstraps capture running VMs for reuse. Saving the same name creates a new
version. `railway ca bootstrap default dev` selects a ready bootstrap as this
machine's default for the environment. `--no-bootstrap` skips the default.
`--from-checkpoint` on `ca create` accepts a cloud-agent checkpoint ID and bypasses
the default bootstrap.

`--variable` sets creation-time variables and supports comma-separated values,
repeated flags, and service references such as `DB_URL=postgres.DATABASE_URL` or
`DB_URL=${{postgres.DATABASE_URL}}`. Quote the latter in a shell. Repeat
`--env-file` to load files; `--variable` overrides matching file entries.

Local clients request their code endpoint automatically. When configuring one
explicitly on `railway code` or `ca start`, `--code-endpoint` and `--code-port`
require an explicit `--new`, even when the launcher would create a VM by default.
