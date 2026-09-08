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

Opt into the local GraphQL-backed MCP server instead:

```bash
railway setup agent -y --local
# or
railway mcp install --local --agent cursor
```

## OpenCode clients and remote servers

Start an interactive OpenCode session on a cloud agent:

```bash
railway code --opencode
railway code --opencode2
```

To run the client on your computer, start its matching server on an agent:

```bash
railway code --opencode remote --new
railway code --opencode2 remote --new
railway code --opencode2 remote --agent my-box --dir /app
```

`remote` starts a detached server on the agent's authenticated HTTPS endpoint,
then prints a command you can paste locally. Standard uses `opencode attach`;
Beta uses `opencode2 --server` (or the installed Beta Desktop CLI on macOS).
The command includes the server credentials. Your client runs locally while
tools and project files stay on the agent. `--dir` selects the remote directory;
Beta uses the server's startup directory, so switching it requires a fresh agent.

Remote mode uses the same generated credentials, startup checks, provider
sign-in behavior, skills, and MCP sync as Desktop setup. It leaves Desktop
settings alone. A running server and its password are reused; you can close
the Railway terminal. Rerun after sleeping or restarting the agent. `--new`
creates a fresh VM. Use `railway ca sleep <name>` when finished.

Put harness-specific arguments after `--`, for example:
`railway code --opencode2 -- run --standalone "explain this project"`.

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

Setup also prints the connection details. On macOS, it gracefully restarts the selected
running edition to apply the settings. Quit the apps before setup on Windows/Linux.
Private `.railway-backup` files preserve the previous settings and server state.
You can close the terminal after setup; there is no local tunnel to keep running.

Press Cmd+B (Ctrl+B on Windows/Linux) to open Home. Under Projects, find
`Railway: <agent-name>` and `/app` (or your `--dir`), then use that project's
menu → New session. Setting a default server
does not move existing chats. Standard OpenCode provider sign-ins from `auth.json` are copied
when available. Beta uses its own sign-in store; connect providers there. OpenCode's newer chat mode uses a separate credential store;
if your provider is missing there, connect it in the remote server's settings.

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
