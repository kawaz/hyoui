# hyoui

> English | [日本語](./README-ja.md)

**hyoui** `/ˈhjoʊi/` (from Japanese 憑依, "spirit possession") — a transparent
PTY wrapper CLI that "possesses" a child process and moves as one with it.

hyoui launches an arbitrary command inside a PTY, stays completely transparent
toward the child, and instead offers a **foothold from the outside** to observe,
inject input, and drive the process from CLI / scripts.

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

### Homebrew (planned)

```bash
brew install kawaz/tap/hyoui
```

> Not published to the tap yet as of v0.1.0. Use the GitHub Release archives or
> a source build until then.

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

## How it differs from tmux / screen / Pexpect

hyoui is **not a terminal multiplexer**.

| | hyoui | tmux / screen | Pexpect / Expect |
|---|---|---|---|
| In-band prefix key | **none** (transparent) | required (C-b / C-a) | none |
| Windows / panes | none (1 session = 1 PTY) | core feature | none |
| External input injection from CLI | **first-class** (`send`/`keys`/`paste`, v0.2.0+) | `send-keys` | library call |
| External output waiting | **first-class** (`wait`/`tail`) | `pipe-pane` (indirect) | `expect()` |
| Daemon lifecycle | starts with `run`, exits with the child | long-lived server, many sessions | dies with the child |
| Primary use case | scripts / external drivers controlling long-running processes | humans living inside a multiplexer | test automation libraries |

In short: hyoui is not a replacement for a multiplexer — you can run hyoui
inside tmux. It sits as the layer that **drives a shell or REPL from outside**
for scripts.

See [DR-0005](./docs/decisions/DR-0005-design-philosophy-external-automation.md)
for the full design philosophy.

## About the name

*Hyoui* (憑依) means "spirit possession" — something inhabits a host and becomes
one with it; the host looks ordinary yet can be moved from within. It captures
this tool's character: it accompanies a child process, lives and dies together
with it, and from the outside becomes a control handle
([DR-0002](./docs/decisions/DR-0002-project-naming.md)).

## Status

v0.1.0 = MVP. The four commands `run` / `attach` / `list` / `kill`, plus
multi-attach and protocol cap negotiation, are usable. Automation APIs such as
`send` / `keys` / `paste` / `wait` / `tail` / `lock` / `tx` will land
incrementally in v0.2.0+ ([`docs/ROADMAP.md`](./docs/ROADMAP.md)).

## Documentation

- [`docs/DESIGN.md`](./docs/DESIGN.md) — Current implementation (domain + architecture)
- [`docs/ROADMAP.md`](./docs/ROADMAP.md) — Future work
- [`docs/decisions/INDEX.md`](./docs/decisions/INDEX.md) — Decision Records (DR)
- [`docs/journal/`](./docs/journal/) — Development journal
- [`docs/findings/`](./docs/findings/) — PoC findings

## License

MIT License — Yoshiaki Kawazu (@kawaz)
