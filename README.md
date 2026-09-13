<p align="center">
  <img src="docs/brand/heycode-mascot.png" width="180" alt="HeyCode mascot" />
</p>
<h1 align="center">HeyCode</h1>
<p align="center">Your code. Your models. Your terminal.</p>
<p align="center">
  <a href="https://github.com/Naresh084/heycode/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/Naresh084/heycode" /></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-blue" /></a>
</p>

HeyCode is an open-source coding agent for your terminal. Ask about a codebase, plan a feature, edit files, and run commands without leaving the conversation.

![HeyCode in action](docs/images/terminal-conversation.gif)

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/Naresh084/heycode/main/install.sh | sh
```

Then open your project and run `heycode`.

Available for **macOS** (Apple Silicon and Intel) and **Linux** (x86-64). No Rust toolchain or source checkout required. The installer uses `~/.local/bin`; add it to your `PATH` if needed.

[Download a binary](https://github.com/Naresh084/heycode/releases) · [Getting started](docs/guides/getting-started.md)

## Use the models you like

Connect a provider with `/provider` and choose a model with `/model`.

- **Existing CLI accounts.** Use your signed-in Claude Code, Codex, or OpenCode installation through its runtime integration.
- **Model APIs.** Bring credentials for OpenAI, Anthropic, Google, OpenRouter, DeepSeek, and other supported providers.
- **Local models.** Connect LM Studio or another supported OpenAI-compatible endpoint.

CLI integrations use the installed CLI's account access and limits. API connections use your provider credentials.

![Choose a provider in HeyCode](docs/images/terminal-providers.png)

## Built for everyday coding

- **Understand a project.** Explore files and ask questions about how things fit together.
- **Make changes.** Plan work, edit code, and run checks with permission controls.
- **Keep the conversation.** Resume sessions and return to work where you left off.
- **Make it yours.** Add MCP servers, skills, and plugins for your workflow.
- **A little company.** An animated terminal cat reacts as you work. Click it to say hello.

```sh
heycode                            # Start a conversation
heycode -c                         # Continue your last session
heycode run "Explain this project" # Ask from the command line
```

![Say hello to the HeyCode mascot](docs/images/terminal-mascot.gif)

## Explore

[Providers](docs/guides/providers.md) · [MCP](docs/guides/mcp.md) · [Plugins](docs/guides/plugins.md) · [Contributing](CONTRIBUTING.md)

HeyCode is under active development. Bug reports and contributions are welcome.

[MIT licensed](LICENSE).
