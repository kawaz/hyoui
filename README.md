# hyoui

> English | [日本語](./README-ja.md)

[![CI](https://github.com/kawaz/hyoui/actions/workflows/ci.yml/badge.svg)](https://github.com/kawaz/hyoui/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/kawaz/hyoui?include_prereleases&sort=semver)](https://github.com/kawaz/hyoui/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

**hyoui** `/ˈhjoʊi/` (from Japanese 憑依, "spirit possession") — drive `claude`,
REPLs, and TUIs **from the outside** via CLI. A transparent PTY wrapper with no
prefix keys and no in-band escape.

<!-- TODO(R5-H15): asciinema cast / GIF goes here -->
<!-- See docs/issue/2026-05-27-readme-asciinema-cast.md for the recording/placement plan -->
<!--
[![asciicast](https://asciinema.org/a/PLACEHOLDER.svg)](https://asciinema.org/a/PLACEHOLDER)
-->

hyoui launches an arbitrary command inside a PTY, stays completely transparent
toward the child, and instead offers a **foothold from the outside** to observe,
inject input, and drive the process from CLI / scripts.

## Who is hyoui for?

hyoui is not a tool you "live inside" of — it is a tool that drives one from
the outside. You probably want it if:

- **You script `claude` / Claude Code from CI or shell scripts** and are tired
  of fighting `tmux send-keys` quoting, or don't want to write `expect` scripts
- **You need to re-attach to long-running LLM / REPL / TUI sessions**,
  e.g. reconnect from your phone via SSH to a `claude` session you left running
  overnight
- **You want to drive an interactive command from a test or ops script** with
  input injection (`send`) and output waiting (`wait`) via a single CLI

If your goal is "press `Ctrl-b` to split panes and live in a multiplexer", use
tmux or zellij. hyoui is designed to **run inside** them.

## What it does

`hyoui run -- <cmd>` starts a command inside a PTY and daemonizes it. From
another process you can `hyoui attach` / `hyoui list` / `hyoui kill` to control
the session. The child sees nothing unusual: no in-band escape, no prefix key,
no rewriting of input/output. Control happens **out of band** through CLI
subcommands (and a future HTTP gateway).

Primary use cases:

- Drive long-running interactive processes (e.g. `claude`, a REPL, `ssh`, a TUI
  app) and attach/detach to them as many times as you like
- Inject input and wait for output from CI / scripts (`send` / `wait`, expanded
  in v0.2.0+)
- Share one session across multiple clients (pair programming, observation,
  human-in-the-loop)

## Installation

### Pre-built binaries (GitHub Releases)

```bash
# Grab a binary for your platform from the latest release:
# https://github.com/kawaz/hyoui/releases/latest
```

Binaries are published for Linux x86_64 / aarch64 and macOS Intel / Apple Silicon.

### Cargo

```bash
cargo install --git https://github.com/kawaz/hyoui hyoui-cli
```

### Build from source

```bash
git clone https://github.com/kawaz/hyoui.git
cd hyoui
cargo build --release
# binary at target/release/hyoui
```

### Homebrew (planned)

```bash
brew install kawaz/tap/hyoui
```

> Formula publication to the tap is in progress (see
> [`docs/issue/2026-05-27-homebrew-tap-deploy-key.md`](./docs/issue/2026-05-27-homebrew-tap-deploy-key.md)).
> Until then, use one of the three install paths above.

Supported platforms: Linux / macOS (Rust 1.86+, PTY and Unix sockets — Windows
is not supported).

## Quickstart

### Start a session

```bash
# foreground (auto-attached)
hyoui run -- bash

# detached (daemon only; the session id is printed on stdout)
SESS=$(hyoui run --detached -- bash)
echo "started: $SESS"
```

### Attach / list / kill from another terminal

```bash
# list active sessions (id and socket path)
hyoui list

# attach to an existing session (I/O bridge; detach with Ctrl-A D)
hyoui attach "$SESS"

# read-only observer
hyoui attach "$SESS" --mode=ro

# terminate (SIGTERM to the child)
hyoui kill "$SESS"
```

### Subcommands (v0.1.0)

| Command | Purpose |
|---|---|
| `hyoui run [--detached] [--session=ID] [--size=COLSxROWS] -- cmd args...` | Start a PTY and daemonize |
| `hyoui attach <session> \| --socket=PATH [--mode=rw\|ro\|rw-no-leader] [--exclusive] [--detach-others]` | I/O bridge |
| `hyoui list` | Enumerate active sessions |
| `hyoui kill <session> [--signum=N]` | Send a signal to the child (default SIGTERM) |

See [`docs/DESIGN.md`](./docs/DESIGN.md) and
[`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) for the full spec.

### Detach key

While attached, `Ctrl-A D` detaches the client (screen-style; the child keeps
running). `Ctrl-A Ctrl-A` sends a literal Ctrl-A to the child.

## How it differs from existing tools

hyoui is **not a terminal multiplexer**. The landscape splits into two camps:
tools you "live inside" and tools you "drive from the outside".

### Tools you live inside (not competitors — compose with hyoui)

| | hyoui | tmux / screen | zellij |
|---|---|---|---|
| In-band prefix key | **none** (transparent) | required (C-b / C-a) | required (C-p / C-q etc.) |
| Windows / panes | none (1 session = 1 PTY) | core feature | core feature |
| Primary use case | driven from the outside | humans living inside | humans living inside |

→ "Run hyoui inside tmux" / "run `hyoui run` from a zellij pane" is the
intended composition.

### Tools you drive from the outside (this is hyoui's lane)

| | hyoui | abduco / dtach | shpool | Pexpect / Expect | ttyd / gotty | asciinema |
|---|---|---|---|---|---|---|
| 1-daemon-1-session model | yes | yes | yes | no | no | no |
| Input injection from external CLI | **first-class** (v0.2.0+) | no | no | library call | via browser | record-only |
| Output waiting from outside | **first-class** (`wait`/`tail`) | no | no | `expect()` | no | no |
| Record / replay | planned for v0.2.0+ | no | no | no | no | core feature |
| HTTP / browser gateway | planned for v0.2.0+ (`serve`) | no | no | no | core feature | replay only |
| Daemon lifecycle | starts with `run`, exits with the child | session manager | long-lived server | dies with the child | server | N/A |

In short: hyoui's unique position is **a 1-daemon-1-session transparent PTY
wrapper with a first-class CLI / HTTP API for external automation**. abduco
and dtach lack external automation, shpool is server-resident, ttyd assumes a
browser, and expect is a library. The goal is to put "the ergonomics of `expect`",
"the attach experience of `abduco`", and "the remote reach of `ttyd`" into one
CLI.

See [DR-0005](./docs/decisions/DR-0005-design-philosophy-external-automation.md)
for the full design philosophy.

## About the name

*Hyoui* (憑依) means "spirit possession" — something inhabits a host and becomes
one with it; the host looks ordinary yet can be moved from within. It captures
this tool's character: it accompanies a child process, lives and dies together
with it, and from the outside becomes a control handle
([DR-0002](./docs/decisions/DR-0002-project-naming.md)).

## Status

v0.1.x = **external API stabilization phase**. The four commands `run` /
`attach` / `list` / `kill`, plus multi-attach and protocol cap negotiation, are
usable.

**Production readiness:**

- Tested platforms: Linux x86_64 / aarch64, macOS Intel / Apple Silicon
- Breaking change policy: **during v0.x, minor bumps may include breaking changes**
  (we won't sell snake oil before the API solidifies)
- Production use: **not yet recommended for v0.1.x**. kawaz uses it daily to
  drive `claude` (eat-your-own-dogfood), but treat business-critical use as
  self-test territory
- **Production-stable target: v0.2.0+**, gated on the `serve` gateway and the
  automation API surface (`send` / `keys` / `paste` / `wait` / `tail` / `lock` /
  `tx`)

Roadmap details: [`docs/ROADMAP.md`](./docs/ROADMAP.md).

## Documentation

- [`docs/DESIGN.md`](./docs/DESIGN.md) — Current implementation (domain + architecture)
- [`docs/ROADMAP.md`](./docs/ROADMAP.md) — Future work
- [`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) — Decision Records (DR)
- [`docs/journal/`](./docs/journal/) — Development journal
- [`docs/findings/`](./docs/findings/) — PoC findings

## Questions / Issues

- Bug or unexpected behavior? File an issue with the
  [bug report template](./.github/ISSUE_TEMPLATE/bug_report.md).
- Want a feature or change? Use the
  [feature request template](./.github/ISSUE_TEMPLATE/feature_request.md).
- Just have a question? Open a
  [Discussion](https://github.com/kawaz/hyoui/discussions) first.

## License

MIT License — Yoshiaki Kawazu (@kawaz)
