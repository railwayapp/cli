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

## Cloud agents in desktop apps

Prepare a cloud agent for Claude Code Desktop, Codex, or OpenCode Desktop:

```bash
railway ca desktop --claude
railway ca desktop --codex
railway ca desktop --opencode --agent my-box
railway ca desktop --opencode --new
```

App flags can be combined to prepare the same agent for several apps. Setup
carries available local sign-ins, applies your skills/MCP sync preferences,
and writes the agent's SSH configuration. `--dry-run` previews the local files
without creating or waking an agent or starting a connection. Existing agents
are reused and woken as needed; if none exists in the target environment, setup
creates one. `--new` always creates a fresh agent, including when several app
flags are combined. It cannot be combined with `--agent` or `--remove`.

OpenCode Desktop connects to [`opencode serve`](https://opencode.ai/docs/server/)
over HTTP. The CLI starts OpenCode on the agent and forwards it over SSH,
binding both ends to loopback. Keep the command running while connected.
Once the server reports it is listening, add
`http://localhost.:14096` in Settings → Servers. The trailing dot is intentional:
OpenCode Desktop 1.18.29 treats `localhost` and `127.0.0.1` as local machines
and opens the computer's native folder picker, even for SSH tunnels. The dotted
hostname still connects to loopback but lets OpenCode browse the agent's files.

Press Cmd+B (Ctrl+B on Windows/Linux) to open Home. Under Projects, hover over
the added server and choose Add project. Select `/app` (or your `--dir`) on the
agent, then use that project's menu → New session. Setting a default server
does not move existing chats. If a chat immediately stops and the server logs
show `ENOENT` for a local computer path, create a new session under the remote
project. Connect a provider there if no local OpenCode sign-in was available.

Use `--port` for a different local port, including when connecting to multiple
agents at once, and `--remote-port` if port 4096 is occupied on the agent.
The connection honors `--ssh-config`. Ctrl-C stops the server and tunnel;
rerun `railway ca desktop --opencode --agent my-box` to reconnect. The CLI also
saves an optional reconnect script under `~/.railway/desktop/opencode/`.
An occupied local port fails before provisioning; stop the previous connection
or choose another `--port`.

`railway ca desktop --opencode --agent my-box --remove` removes the generated
script and shared SSH block. Stop any running connection and remove the saved URL
in OpenCode separately. The agent remains; `railway ca sleep my-box` stops its
compute bill.

## Contributing

See [CONTRIBUTING.md](https://github.com/railwayapp/cli/blob/master/CONTRIBUTING.md) for information on setting up this repository locally.

## Feedback

Share feedback and suggestions on [Central Station](https://station.railway.com/feedback).
