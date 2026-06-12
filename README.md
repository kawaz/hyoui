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

The daemon parses the child PTY through a [`vt100`](https://docs.rs/vt100)-based
screen emulator and **owns the canonical screen state**. That foundation is
what makes attach restore (including observing alt-screen-resident TUIs),
match-against-current-visible-state `wait`, and structured snapshots work
reliably ([DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md)).

## Who is hyoui for?

hyoui is not a tool you "live inside" of — it is a tool that drives one from
the outside. You probably want it if:

- **You script `claude` / Claude Code from CI or shell scripts** and are tired
  of fighting `tmux send-keys` quoting, or don't want to write `expect` scripts
- **You need to re-attach to long-running LLM / REPL / TUI sessions**,
  e.g. reconnect from your phone via SSH to a `claude` session you left running
  overnight
- **You want to drive an interactive command from a test or ops script** with
  input injection and waiting against the current screen, all from a single CLI

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
  app) and attach/detach to them as many times as you like. On attach the
  daemon repaints the screen from its canonical state, so even alt-screen apps
  come back without redraw glitches
- Inject input and wait for output from CI / scripts via `hyoui input` with the
  `text:` / `key:` / `paste:` / `wait:` / `wait-idle:` spec family, plus
  `hyoui wait`, `hyoui screen dump` / `screen snapshot`, `hyoui record`, and
  `hyoui lock`
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

### Homebrew

```bash
brew install kawaz/tap/hyoui
```

> The formula is auto-published to [`kawaz/homebrew-tap`](https://github.com/kawaz/homebrew-tap)
> on each release by `release.yml`.

Supported platforms: Linux / macOS (Rust 1.88+, PTY and Unix sockets — Windows
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

Attach repaints the screen from the daemon's canonical state in a single
frame ([DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md) §4 Phase A), so alt-screen-resident apps like the `claude`
TUI come back exactly as they were before detach.

### Automation: input / wait / screen / lock

```bash
# direct text combined with a key
hyoui input "$SESS" "text:ls -la" "key:Enter"

# match against the current visible state (no false hits from past redraws)
hyoui input "$SESS" "wait:^Continue\\?" "key:Enter"

# binary control bytes (= ESC[A = Up arrow)
hyoui input "$SESS" "hex:1b5b41"

# multi-line script via bracketed paste
hyoui input "$SESS" "paste:$(cat script.py)"

# standalone wait (regex against the current visible state, with timeout)
hyoui wait "$SESS" "^\\$" --timeout=10s

# screen dump (ANSI bytes; reproduces in a terminal via cat)
hyoui screen dump "$SESS"
hyoui screen dump "$SESS" --layer=both --rect=0,0,80,5

# structured snapshot (CBOR-encoded StateSnapshotResponse on the wire)
# NOTE: --format=json is forward-compat and NOT wired yet; output is CBOR today,
#       so decode with a CBOR tool before piping to jq.
hyoui screen snapshot "$SESS" --include=Cursor
hyoui screen snapshot "$SESS" --include=Cells,Cursor,Mode

# exclusive acquire (other clients become forced-ro; you become leader)
hyoui lock acquire "$SESS" --timeout=30s
hyoui lock release "$SESS"

# record the tty I/O timeline to a file (jsonl)
# ⚠ stdin redaction is NOT wired yet — stdin is recorded verbatim regardless of
#   --input-secrecy. Limit to --stdout if you may type secrets.
hyoui record start "$SESS" --output session.jsonl --both
hyoui record list "$SESS"
hyoui record stop "$SESS" --all

# raw byte stream for grep / save (log / asciinema preprocessing)
hyoui tail "$SESS" --last-bytes=4096
# strict variant: fail if the requested window was already evicted from scrollback
hyoui tail "$SESS" --since=10s --since-strict
```

### Session namespaces

Sessions can be grouped into **namespaces** so that unrelated groups never mix
in `hyoui list` ([DR-0018](./docs/decisions/DR-0018-session-namespace.md)). The
namespace resolves as `--namespace` flag > env `HYOUI_NAMESPACE` > `default`,
and every session command (run / attach / list / kill / input / ...) shares the
same resolution. The `default` namespace keeps the traditional socket layout,
so existing sessions are untouched.

```bash
# day-to-day session (default namespace; behaves exactly as before)
hyoui run --detached -- claude

# a worker fleet isolated under its own namespace
hyoui run --detached --namespace=workers --session=w1 -- worker-cmd
hyoui run --detached --namespace=workers --session=w2 -- worker-cmd

hyoui list                          # default only — no worker noise
hyoui list --namespace=workers      # the fleet only
hyoui list --all-namespaces         # everything, with an NS column
hyoui attach w1 --namespace=workers # all selectors are namespace-scoped
hyoui kill --all --namespace=workers  # killall scoped to the fleet

# direnv-friendly: put `export HYOUI_NAMESPACE=myproj` in a project .envrc and
# every run/list/attach inside that project is isolated automatically.
```

`hyoui run` always injects the resolved namespace into the child's environment
as `HYOUI_NAMESPACE` (even for `default`), so a hyoui nested inside a namespace
inherits it — the same convention as tmux's `TMUX` / screen's `STY`. To launch
into a different namespace from inside one, pass `--namespace=<other>`
explicitly (e.g. `--namespace=default`).

### Main subcommands

| Command | Purpose |
|---|---|
| `hyoui run [--detached] [--session=ID] [--size=COLSxROWS] -- cmd args...` | Start a PTY and daemonize |
| `hyoui attach <session> [--mode=rw\|ro\|rw-no-leader]` | I/O bridge (repaints from screen state on attach) |
| `hyoui list [--namespace=NS\|--all-namespaces]` | Enumerate active sessions (namespace-scoped) |
| `hyoui kill <session> [--signal=NUM_OR_NAME]` | Send a signal to the child (default SIGTERM; name or number, e.g. `--signal KILL` / `--signal 9`) |
| `hyoui status <session>` | Print session status (clients / leader / lock / scrollback) |
| `hyoui set <session> <key>=<value>` | Change a runtime setting (e.g. `on-child-suspend=notify\|auto-resume`) |
| `hyoui input <session> <spec>...` | Inject input via `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:` specs |
| `hyoui wait <session> <pattern>` | Wait until a regex matches the current visible state |
| `hyoui screen dump <session>` | Dump the screen as ANSI bytes (terminal-replayable) |
| `hyoui screen snapshot <session>` | Structured screen-state snapshot (JSON / CBOR) |
| `hyoui lock acquire\|release <session>` | Exclusion for atomic automation (`unlock` is an alias for `lock release`; `tx` is not yet implemented) |
| `hyoui detach [session]` | Detach all attached clients (daemon and child keep running) |
| `hyoui record start\|stop\|list <session>` | Persist the tty I/O timeline (jsonl / raw). **⚠ stdin redaction is not yet wired** |
| `hyoui tail <session>` | Raw byte stream (logging / grep / asciinema preprocessing) |

> **self-session (DR-0020)**: child processes under `hyoui run -- cmd` always get
> `HYOUI_SESSION_ID` injected. Session-taking subcommands (status / set / wait /
> detach, etc.) resolve to the current session when the session argument is
> omitted (so a process can observe / control itself from inside). `attach` is the
> exception: attaching to your own session is rejected to prevent nesting.

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
| Daemon owns canonical screen state | **yes** (vt100-based) | no | no | no | no | no |
| Input injection from external CLI | **first-class** (`input` family) | no | no | library call | via browser | record-only |
| Wait against the current visible state | **first-class** (state-based `wait`) | no | no | `expect()` (child PTY stream regex) | no | no |
| Structured snapshot / dump | **first-class** (`screen dump` / `screen snapshot`) | no | no | no | no | no |
| Record / replay | **record shipped** (`record start/stop/list`, jsonl/raw timeline); replay planned | no | no | no | no | core feature |
| HTTP / browser gateway | planned (= `kawaz/hyoui-serve`) | no | no | no | core feature | replay only |
| Daemon lifecycle | starts with `run`, exits with the child | session manager | long-lived server | dies with the child | server | N/A |

In short: hyoui's unique position is **a 1-daemon-1-session transparent PTY
wrapper** whose daemon **owns the canonical screen state**, with a
**first-class CLI / HTTP API for external automation** on top. abduco and
dtach lack external automation, shpool is server-resident, ttyd assumes a
browser, and expect is a library. The goal is to put "the ergonomics of `expect`",
"the attach experience of `abduco`", and "the remote reach of `ttyd`" into one
CLI.

See [DR-0005](./docs/decisions/DR-0005-design-philosophy-external-automation.md)
for the full design philosophy, and
[DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md)
for the canonical-screen-state foundation behind attach restore and
state-based automation.

## About the name

*Hyoui* (憑依) means "spirit possession" — something inhabits a host and becomes
one with it; the host looks ordinary yet can be moved from within. It captures
this tool's character: it accompanies a child process, lives and dies together
with it, and from the outside becomes a control handle
([DR-0002](./docs/decisions/DR-0002-project-naming.md)).

## Status

v0.1.x = **external API stabilization phase**.

- `run` / `attach` / `list` / `kill` + multi-attach + protocol cap
  negotiation: stable since v0.1.0
- Screen emulator adoption + attach handshake redraw + state-based wait /
  snapshot / dump: completed in [DR-0013](./docs/decisions/DR-0013-screen-emulator-and-attach-stability.md) Phase A/B (= the **core machinery
  for observing claude TUI sessions and driving them from the outside is in
  place**)
- The input family (`text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:`
  / `wait-idle:` specs) and `lock` / `unlock` are implemented (`tx` is not yet)
- `hyoui record start/stop/list` (tty I/O timeline, jsonl/raw) ships since
  v0.2.2; **stdin redaction (`--input-secrecy`) is not yet wired**

**Production readiness:**

- Tested platforms: Linux x86_64 / aarch64, macOS Intel / Apple Silicon
- Breaking change policy: **during v0.x, minor bumps may include breaking changes**
  (we won't sell snake oil before the API solidifies)
- Production use: kawaz uses it daily to drive `claude`
  (eat-your-own-dogfood), but treat business-critical use as self-test
  territory for now
- **Production-stable target**: after the `serve` gateway (in a separate repo
  `kawaz/hyoui-serve`) ships and the remaining work (record redaction wiring,
  replay, observability, L2 wait, ...) lands

Roadmap details: [`docs/ROADMAP.md`](./docs/ROADMAP.md).

## Documentation

- [`docs/DESIGN.md`](./docs/DESIGN.md) — Current implementation (domain + architecture)
- [`docs/MANUAL.md`](./docs/MANUAL.md) — End-user recipe book
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
