# Railway CLI

The Railway CLI lets you interact with your Railway projects from the command line. Read the [CLI documentation](https://docs.railway.com/cli).

## Installation

Install the Railway CLI with agent support configured in one step (macOS, Linux, Windows via [WSL](https://learn.microsoft.com/en-us/windows/wsl/install)):

```bash
bash <(curl -fsSL railway.com/install.sh) --agents -y
```

This installs the CLI to `~/.railway/bin` and runs [`railway setup agent`](https://docs.railway.com/cli/setup) to configure detected agent tools.

To install the CLI without agent configuration:

```bash
bash <(curl -fsSL railway.com/install.sh) -y
```

Uninstall the CLI:

```bash
bash <(curl -fsSL cli.new) -r
```

Other installation methods are available in the CLI documentation: [Homebrew](https://docs.railway.com/cli#homebrew-macos), [npm](https://docs.railway.com/cli#npm-macos-linux-windows), [Scoop](https://docs.railway.com/cli#scoop-windows), [pre-built binaries](https://docs.railway.com/cli#pre-built-binaries), and [source builds](https://docs.railway.com/cli#from-source).

## Authentication

Before using the CLI, authenticate with your Railway account:

```bash
railway login
```

For environments without a browser, such as SSH sessions, use browserless login:

```bash
railway login --browserless
```

### Tokens

For CI/CD pipelines, set environment variables instead of using interactive login:

- Project token: Set `RAILWAY_TOKEN` for project-level actions.
- Account or workspace token: Set `RAILWAY_API_TOKEN` for account-level or workspace-level actions.

```bash
RAILWAY_TOKEN=xxx railway up
```

See [Tokens](https://docs.railway.com/integrations/api#creating-a-token) for more information.

## Agent Setup

Configure Railway agent support for AI coding tools:

```bash
railway setup agent -y
```

This installs Railway skills and configures the remote Railway MCP server (`mcp.railway.com` via `railway mcp proxy`, authenticated with your CLI login) for detected tools such as Claude Code, Cursor, Codex, OpenCode, GitHub Copilot, and Factory Droid.

Use the focused commands when you only need one part of the setup:

```bash
railway mcp install --agent cursor
railway skills --agent claude-code
```

With automatic updates enabled, CLI-managed skills update silently in the background,
including on the first normal command after a CLI version change (whether upgraded
with `railway upgrade` or a package manager). Locally modified or deleted skills are
skipped, and user-added files are preserved. Restart your coding tool to load updated
skills. Use `railway skills update` to review skipped updates, or add `--force` to
overwrite local changes explicitly.

`railway autoupdate disable` (or `RAILWAY_NO_AUTO_UPDATE=1`) disables automatic CLI
and skill updates and their update notices. Explicit `railway upgrade`,
`railway check-updates`, and `railway skills update` commands remain available.

Opt into the local GraphQL-backed MCP server instead:

```bash
railway setup agent -y --local
# or
railway mcp install --local --agent cursor
```

## Update progress and status

`railway upgrade` presents CLI installation and managed skill synchronization as
one flow. Stages advance as work completes, and the final summary says whether
the new version is active or will be used on the next command. This explicit
command also synchronizes managed skills when automatic updates are disabled.

Automatic discovery and downloads run quietly. After the new CLI is active and
its skill sync has finished, an interactive command shows one completion receipt
per CLI version. Preserved local edits are reported neutrally; a skill-sync
failure is reported separately from a successful CLI install. Commands using
JSON, piped output, help, and version output do not show or consume receipts.
Install methods that require manual upgrades get one availability notice per
release instead.

Run `railway autoupdate status` to inspect the running and recorded installed
versions, staged/in-progress CLI updates, skill-sync results, and unmanaged
skills. Use `railway skills update` for detailed skill results or to retry a sync.

## OpenCode clients and remote servers

Prepare an authenticated server on a cloud agent and open your local client:

```bash
railway code --opencode
railway code --opencode2
railway code --opencode2 --new
```

When the matching OpenCode Desktop edition has an existing settings file or
desktop database, Railway automatically saves the server URL, credentials,
default server, and project in it. The final output confirms that configuration
was updated; open `Railway: <agent-name>` in Desktop's server picker. Both JSON
settings and SQLite renderer state are supported, with backups of previous settings.
You may need to restart OpenCode Desktop to load the updated configuration.
Configuration failures are non-fatal and reported alongside the connection
details; rerun the command to retry.

The CLI also prints the server URL, username, password, and project directory
for manual setup, plus a shell command to connect directly.
After successful setup in an interactive terminal, it clears the setup messages
and shows the connection details, with the Desktop update confirmation inside
the result panel. Failed setup keeps its diagnostic output visible.
Press Enter to launch the matching local terminal client. If that client is
missing, Railway offers to install it first. Declining, Esc, or Ctrl+C at
either prompt leaves the server running and prints the connection details.
Standard and Beta clients are detected and installed separately.

New OpenCode agents are named `oc-railg-3ed` (standard) or `oc2-railg-3ed`
(Beta): the first five letters/digits of the project name, lowercase, plus a
random three-character suffix. When using your default cloud agents project,
the label comes from the local repository or directory instead. Existing names
are checked before creation; `--name` overrides the generated name. The same
naming applies to `remote` and `railway ca desktop`.

Reconnect to an existing server using your local client:

```bash
railway code --opencode connect
railway code --opencode2 connect my-box
```

`connect` discovers running servers of the selected edition on agents you own.
One match connects directly; multiple matches show a `workspace/project/agent`
picker. A name or ID targets an agent directly and can wake a saved server.
Connecting also refreshes the detected Desktop edition's configuration.
Connecting never creates a new agent or installs a server on an unrelated box.
In noninteractive terminals, the CLI still attempts Desktop configuration and
prints connection details instead of prompting, installing software, or
launching a terminal client; multiple matches require a name or ID.

To run both client and server inside the cloud agent, in Railway CA:

```bash
railway code --opencode remote
railway code --opencode2 remote --new
```

`--new` creates a fresh VM; `--agent <name-or-id>` targets an existing one.
For the local-client setup, `--dir` selects the remote project directory
(default `/app`). Beta uses its server's startup directory, so switching it
requires a fresh agent. Setup uses the same generated credentials, HTTPS
checks, provider sign-in behavior, skills, and MCP sync as Desktop. It saves
settings in detected Desktop installations and prints them for manual entry.
A running server and its password are reused. Use `railway ca sleep <name>`
when finished.

Put harness-specific arguments after `--`, for example:
`railway code --opencode2 -- run --standalone "explain this project"`.

### Retrieve the last connection configuration

```bash
railway code get-config
railway code get-config --json
```

Each successful `railway code` setup saves its connection details locally,
including when you decline or cancel the local-client prompt. OpenCode
reconnects refresh this record too. `get-config` works from any directory and
prints the most recently saved connection in one results panel: the OpenCode
server URL, username, password, project directory, connection commands, and
Desktop configuration result, plus a direct SSH command and a copyable
`~/.ssh/config` block.
SSH-based harness launches save the SSH details.

The record is a snapshot; viewing it does not create, wake, or connect to an
agent. It is stored in `~/.railway/last-code-config.json` with owner-only
permissions on Unix, replaced after each successful setup, and removed by
`railway logout`. JSON output includes the saved connection credentials.
Launches made before this feature was installed have no record; run a setup
or reconnect once to save one.

## Cloud agents in desktop apps

Prepare a cloud agent for Claude Code Desktop, Codex, or OpenCode Desktop:

```bash
railway ca desktop --claude
railway ca desktop --codex
railway ca desktop --opencode --agent my-box
railway ca desktop --opencode --new
railway ca desktop --opencode2 --new
```

App flags can be combined to prepare the same agent for several apps. Setup
carries available local sign-ins, applies your skills/MCP sync preferences,
and writes the agent's SSH configuration. `--dry-run` previews the local files
without creating or waking an agent or starting a connection. Existing agents
are reused and woken as needed; if none exists in the target environment, setup
creates one. `--new` always creates a fresh agent, including when several app
flags are combined. It cannot be combined with `--agent` or `--remove`.

OpenCode Desktop connects directly to the agent's existing HTTPS address.
The CLI starts password-protected [`opencode serve`](https://opencode.ai/docs/server/)
in the background on port 8080, checks its public endpoint, and saves the URL,
username, password, default server, and remote project in Desktop's settings.
`--opencode` configures only standard OpenCode (`ai.opencode.desktop`).
`--opencode2` configures only [OpenCode2 Beta](https://github.com/anomalyco/opencode-beta)
(`ai.opencode.desktop.beta`), using its compatible server runtime. These two
flags cannot be combined. JSON server settings and SQLite renderer state are
supported; a drafts-only database does not change where settings are saved.

For Beta, the CLI seeds an `opencode2` shim on the agent. Each new process checks
the latest official Beta release, downloads the Linux Desktop package for the
agent's architecture, verifies its published SHA-256, and extracts just the CLI
executable. The first start can take several minutes. A verified cached version
is reused until the release changes; running sessions keep their executable.
A failed update reports an error and preserves the previous runtime.

Use `railway ca --opencode2` for a terminal session. In the new-session picker,
highlight OpenCode and press Tab to switch to **OpenCode2 [Beta]**. Tab also
switches editions in the prompt footer; Shift+Tab cycles harnesses.

Setup also prints the connection details and a reminder that you may need to
restart OpenCode Desktop to load the updated configuration.
Private `.railway-backup` files preserve the previous settings and server state.
You can close the terminal after setup; there is no local tunnel to keep running.

Press Cmd+B (Ctrl+B on Windows/Linux) to open Home. Under Projects, find
`Railway: <agent-name>` and `/app` (or your `--dir`), then use that project's
menu → New session. Setting a default server
does not move existing chats. Standard OpenCode provider sign-ins from `auth.json` are copied
when available. OpenCode2 imports the active account for each provider from your local
Beta credential database, preserving accounts already configured on the cloud agent.
Legacy `auth.json` is used only when no Beta credential store exists. Credentials are
sent over SSH and the temporary transfer file is removed after import. If there is
no local sign-in to copy, connect the provider in the remote server's settings.

OpenCode uses the agent's public app port (8080). Setup refuses to take over
an occupied port; stop the other process or use `--new` for a fresh agent.
Rerunning `railway ca desktop --opencode --agent my-box` reuses the running
server and its credentials, or starts it again after a sleep/wake or restart.
The remote credential and process state are stored privately under
`~/.railway/desktop/opencode/`; startup logs are in `server.log` there.

`railway ca desktop --opencode --agent my-box --remove` (or `--opencode2`
for Beta) stops the managed
OpenCode process on a running agent and removes the shared SSH block and the
Desktop connection/project saved by setup. It does not wake a sleeping agent.
The agent remains; `railway ca sleep my-box` stops its compute bill.

## Contributing

See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repository locally.

## Feedback

Share feedback and suggestions on [Central Station](https://station.railway.com/feedback).
