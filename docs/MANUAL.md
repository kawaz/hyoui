# hyoui User Manual

> English | [日本語](./MANUAL-ja.md)

A use-case-driven recipe book for end users (people driving `hyoui` from the
CLI).

- **Install / concept overview** → [`README.md`](../README.md)
- **Internal design / why it's built this way** → [`DESIGN.md`](./DESIGN.md)
- **This file**: "I want to do X" → "use this command sequence."

> Status: covers v0.9.x. The automation API (`input` family / `wait` / `screen` /
> `lock` / `record` / `tail`) and the web gateway are implemented. The `tx`
> wrapper is not yet shipped.

## Table of contents

- [Core flow](#core-flow)
  - [1. Start a detached session and attach from another terminal](#1-start-a-detached-session-and-attach-from-another-terminal)
  - [2. Observe in read-only mode](#2-observe-in-read-only-mode)
  - [3. Stop a session](#3-stop-a-session)
- [Automation](#automation)
  - [4. Inject input (`input` family)](#4-inject-input-input-family)
  - [5. Wait for the screen to reach a state](#5-wait-for-the-screen-to-reach-a-state)
  - [6. Read the screen (`screen dump` / `snapshot`)](#6-read-the-screen-screen-dump--snapshot)
  - [7. Exclusive automation (`lock`)](#7-exclusive-automation-lock)
  - [8. Record the tty I/O timeline (`record`)](#8-record-the-tty-io-timeline-record)
  - [9. Session ids and faces (state roots)](#9-session-ids-and-faces-state-roots)
  - [10. Stop leaking parent env into the child (env scrub)](#10-stop-leaking-parent-env-into-the-child-env-scrub)
  - [11. Operate from a browser (`web`)](#11-operate-from-a-browser-web)
- [Troubleshooting](#troubleshooting)
- [See also](#see-also)

## Core flow

### 1. Start a detached session and attach from another terminal

```sh
# Terminal A: launch detached; the session id (a UUID) is printed on stdout
hyoui run --detached -- claude
# → 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f  (example)

# Terminal B: list, then attach
hyoui list
hyoui attach 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f
# A single Ctrl+Z suspends the client (back to the shell; `fg` to return)
# To close the connection: hyoui detach 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f
```

When stdin is a pipe or a file, that fd becomes the child's stdin, with or without `--detached`. The child's stdout / stderr and controlling terminal stay the PTY ([DR-0042](./decisions/DR-0042-non-tty-stdin-is-the-childs-fd.md)). The child sees the same stdin as when run directly, so it reads the pipe and exits on the pipe's EOF, and binary data arrives unchanged. hyoui does not read the pipe, so `run --detached` returns right away even for endless input such as `tail -f`.

```sh
echo "1+2" | hyoui run -- bc                  # stays attached; bc prints 3 and exits
hyoui run --detached -- claude <<<"prompt"    # the prompt is submitted as the first input, as when run directly
printf 'a\003b\000c\n' | hyoui run -- od -c    # binary data is not altered
```

- A TUI that reads a pipe and still uses the keyboard (claude / fzf / less, ...) reads keys from `/dev/tty` (= hyoui's PTY), as when run directly. Keys from attach and `hyoui input` arrive there
- Once a pipe is given to a program that reads stdin (`cat`, ...), `hyoui input` does not reach that program's stdin (the same as giving it a pipe when run directly)
- `/dev/null` becomes the child's stdin like any other non-tty, and the child reads EOF right away; an interactive shell such as `bash -i` exits. To start a shell / REPL that you keep operating from outside, from a launcher without a terminal (an agent or a script), add `--pty-stdin` so the child's stdin is the PTY as well: `hyoui run --detached --pty-stdin -- bash -i`
- When stdin is a terminal, the child's stdin is the PTY (unchanged)
- The attach client never forwards stdin to the child. It reads keys from stdin when stdin is a terminal, otherwise from `/dev/tty`; with neither, it relays output only and ends when the child exits
- In a loop that shares stdin, such as `while read l; do hyoui run --detached -- x; done < list`, the child reads the rest of stdin (the same as running it directly). Add `</dev/null` to keep the rest unread

### 2. Observe in read-only mode

```sh
hyoui attach --observer 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f
# observer attach forwards no input; output is read-only
```

### 3. Stop a session

```sh
hyoui kill 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f                 # SIGTERM
hyoui kill --signal KILL 0f8b6c1e-3d2a-4c5b-9e7f-1a2b3c4d5e6f   # SIGKILL
```

## Automation

These recipes assume `SESS` holds a session id (e.g. `SESS=$(hyoui run --detached --pty-stdin -- bash)`; the shell is operated from outside, so `--pty-stdin` makes its stdin the PTY as well).

### 4. Inject input (`input` family)

`hyoui input` sends an ordered sequence of specs to the child. Each argument is
one spec; they are applied left to right.

```sh
# type a command and press Enter
hyoui input "$SESS" "text:ls -la" "key:Enter"

# raw control bytes (hex) — here ESC[A = Up arrow
hyoui input "$SESS" "hex:1b5b41"

# paste a multi-line block via bracketed paste (the child sees it as one paste)
hyoui input "$SESS" "paste:$(cat script.py)"

# read the payload from a file
hyoui input "$SESS" "file:./payload.txt"
```

Spec prefixes: `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:`.

#### 4.1 Sequencing guarantee via ack (DR-0021)

Bytes-type specs (`text:` / `paste:` / `hex:` / `file:` / `key:`) are safe to
chain in a single invocation — ordering is guaranteed. The daemon returns an ack
once it has finished writing each spec's bytes to the master PTY fd; the client
waits for that ack before sending the next spec (no race).

```sh
# text followed by key:Enter — ack ensures Enter arrives after all text bytes
hyoui input "$SESS" "text:ls -la" "key:Enter"
```

If the daemon returns ack:Error, the CLI exits with code 1. Common error codes:

| code | meaning |
|---|---|
| `master.write-timeout` | child did not consume input within 500 ms (ICANON buffer full / child stopped) |
| `master.write-error` | I/O error on the daemon side |
| `master.write-partial` | partial write (defense-in-depth) |
| `client.ro-rejected` | attempted input injection from a read-only (Ro) client |
| `client.lock-not-held` | attempted input injection from a client that does not hold the lock |

If no ack arrives within `RAW_ACK_TIMEOUT` (5 s), the connection is poisoned and
the CLI exits 1. Start a fresh invocation for the next operation.

#### 4.2 Large-byte-write limit for ICANON apps

Children running in **ICANON mode** (bash, python, sh, …) have a line discipline
input buffer of roughly 1024 B. Sending more than that in a single spec triggers
`master.write-timeout`. Work around it by:

- splitting text at newline boundaries into **multiple specs**, or
- keeping each spec under 1 KB.

```sh
# bad: sending >1024 B in one spec to bash may hit master.write-timeout
hyoui input "$SESS" "text:$(cat large_payload.txt)"

# good: split by newline
hyoui input "$SESS" "text:line1" "key:Enter" "text:line2" "key:Enter"
```

Alt-screen TUI children (vim, claude, …) disable ICANON, so large payloads are
fine.

> **`wait:` / `wait-idle:` serve a different purpose.** They wait for the child's
> *output* to reach a certain state (e.g. a prompt appears, output goes quiet).
> The ack mechanism only guarantees that bytes have been *delivered to the child's
> input stream*, not that the child has finished processing them. Use a `wait:`
> spec when you need to know the command has completed.

#### 4.3 Invocation auto-lock (DR-0022)

`hyoui input` **automatically acquires one lock for the entire invocation**.
Parallel `hyoui input` calls against the same session no longer interleave their
bytes — the second call waits until the first completes (= serialization).

```sh
# Parallel inputs against the same session are serialized
hyoui input "$SESS" "text:hello\n" &
hyoui input "$SESS" "text:world\n" &
wait
# → the screen echoes "hello" completely before "world"
```

- **The lock is held even during `wait:` / `wait-idle:`** so other clients are
  blocked through the entire wait. This makes the invocation atomic from other
  clients' viewpoint.
- **Outer token inheritance skips auto-acquire**: if `--lock-token=<T>` or
  `HYOUI_LOCK_TOKEN` env is present, the inner `input` only inherits the token
  and does not acquire/release (= it won't break the outer lock).
- **Acquire timeout**: default 30 s. Adjust with `--auto-lock-timeout-acquire DUR`
  if another client is expected to hold the lock for longer.
- **No opt-out flag**: auto-lock is always on. To skip, set
  `HYOUI_LOCK_TOKEN` in the env.

```sh
# Outer holds the lock; inner inherits the token and skips auto-acquire
TOKEN=$(hyoui lock acquire "$SESS" --timeout=10s &)
hyoui input --lock-token="$TOKEN" "$SESS" "text:..."
hyoui lock release "$SESS" --token="$TOKEN"

# Extend the timeout when long waits are expected
hyoui input --auto-lock-timeout-acquire=2m "$SESS" "text:..."
```

### 5. Wait for the screen to reach a state

`wait` matches a regex against the **current visible screen state**, so past
redraws don't cause false hits. It can stand alone or be embedded in an `input`
sequence as a `wait:` spec.

```sh
# standalone: wait until a shell prompt appears (regex against the visible state)
hyoui wait "$SESS" "^\\$" --timeout=10s

# embedded: wait for a confirmation prompt, then answer it
hyoui input "$SESS" "wait:^Continue\\?" "key:Enter"
```

### 6. Read the screen (`screen dump` / `snapshot`)

```sh
# ANSI byte dump — pipe to a terminal (cat) to reproduce the visual
hyoui screen dump "$SESS"
hyoui screen dump "$SESS" --layer=both --rect=0,0,80,5

# structured snapshot (daemon speaks CBOR on the wire; `--format=json` converts in the CLI)
hyoui screen snapshot "$SESS" --include=Cells,Cursor,Mode               # CBOR (default, machine processing)
hyoui screen snapshot "$SESS" --include=Cursor,Mode --format=json | jq .  # JSON (pipe straight into jq)
# Note: with `--format=json`, `cells` / `scrollback` bytes expand to number arrays and become
# bulky. Exclude them via `--include` when you only need to inspect via jq.
```

### 7. Exclusive automation (`lock`)

Acquire exclusivity so other clients can't inject input mid-sequence. The
acquirer becomes leader; others are forced read-only until release.

```sh
hyoui lock acquire "$SESS" --timeout=30s
hyoui input "$SESS" "text:deploy" "key:Enter"
hyoui lock release "$SESS"   # `hyoui unlock "$SESS" --token=<T>` is an alias
```

### 8. Record the tty I/O timeline (`record`)

Persist the bytes-level I/O timeline to a file for later analysis (bug repro,
asciinema-style export). `--both` records stdin + stdout; `--format` is `jsonl`
(timeline with timestamps + lifecycle events) or `raw` (single-direction stream).

```sh
hyoui record start "$SESS" --output session.jsonl --both
hyoui record list "$SESS"
hyoui record stop "$SESS" --all
```

> **stdin handling**: the default (`--input-secrecy=record-all`) records stdin
> verbatim. If you may type passphrases or tokens, use
> `--input-secrecy=never-record-stdin` — stdin-derived events are then never
> recorded at all. `redact-after-prompt` (redact only after a prompt is
> detected) is planned for Phase 5 and currently errors out
> ([DR-0016](./decisions/DR-0016-tty-io-record.md) §6a).

### 9. Session ids and faces (state roots)

A session id is a lowercase, hyphenated UUID and nothing else ([DR-0041](./decisions/DR-0041-session-id-uuid-and-tags.md)). `hyoui list` shows every session of the current face, ordered by start time.

```sh
# choose the id up front (no need to read it back from stdout)
SID=$(uuidgen | tr A-Z a-z)
hyoui run --detached --pty-stdin --session-id="$SID" -- bash
hyoui input "$SID" "text:ls" "key:Enter"
```

- Only the canonical form `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` is accepted. Uppercase, unhyphenated, braced, or prefix-only ids are errors and are never normalized silently (a different spelling of the same UUID would be a different socket). Any UUID version is fine
- If a socket with the same id already exists, `hyoui run` ends with an error without starting the child, whether that daemon is alive or dead; run does not judge liveness. For a running session use `hyoui kill <id>`; if only the socket of a dead daemon is left and its lock is still there, `hyoui list` cleans it up (it removes sockets that refuse connections while nobody holds the lock); if `hyoui list` shows it as stale (no lock), confirm that no daemon is behind it and remove it by hand. Then try again. The check happens when the socket is bound and the name lock is taken, so of several concurrent runs with one id exactly one starts
- `hyoui kill --wait` waits for the child and the session to end and also for the daemon to exit (removing its socket), so `run` with the same id works right after it returns. If the daemon ended without removing its socket, it exits with 1 and points to `hyoui list` for the cleanup
- `hyoui kill` without `--wait` returns once the signal is sent. After the child ends, the daemon stays for about 2 seconds for late attaches, and a `run` with the same id during that time fails with "socket already exists". Use `kill --wait` when you reuse the id right away

**Tags** — a session can carry `key=value` tags, and `list` can filter by them. Everything is shown by default; a filter only applies when given.

```sh
hyoui run --detached --tag env=prod --tag team=infra -- claude
hyoui run --detached --tag scratch -- bash          # `--tag scratch` is short for `--tag scratch=` (empty value)
hyoui list --tag env=prod                           # exact value match
hyoui list --tag team                               # matches when the key exists (any value)
hyoui list --tag env=prod --tag team                # repeated filters are ANDed
hyoui list --format=jsonl | jq 'select(.tags.team | startswith("in"))'   # finer conditions: filter the jsonl
```

- A key is `[A-Za-z0-9._-]{1,256}`; a value is any string (split at the first `=`, empty allowed). Repeating a key keeps the last value
- Keys starting with `hyoui.` are reserved by hyoui and cannot be set (`run` fails; use another prefix). The check is case-sensitive. `list --tag hyoui.x` is not an error; it matches no session
- `list --tag key` matches when the key exists, and `list --tag key=` matches an empty value exactly; they are different conditions. A session tagged `--tag key=` matches both, one tagged `--tag key=bar` matches only `--tag key`. There are no wildcards
- The daemon keeps the tags and returns them in `status` (the `tags:` line / `tags` in json), `list` (the TAGS column / `tags` in jsonl) and the web `/api/sessions`. They survive a daemon upgrade and cannot be changed after start
- With `--tag`, rows that do not respond (no-response / stale / error) are not shown, since their tags are unknown
- There is no environment variable that supplies default tags. `HYOUI_NAMESPACE` is not read
- `--namespace` / `--all-namespaces` are accepted and ignored until 2026-11 (stdout is the same as without them; one notice line goes to stderr). After that they become unknown options

**Faces** — sockets live at `<state root>/sessions/<id>.sock`. The state root is decided as follows, and everything hyoui keeps (session sockets, the web supervisor, units, registry, passkeys, logs) stays inside it.

1. `HYOUI_STATE_DIR` (used as is when non-empty; a relative path is an error)
2. `$XDG_STATE_HOME/hyoui` (only when absolute)
3. `$HOME/.local/state/hyoui` (an error when `HOME` is missing or relative; never relative to the current directory)

To keep a separate face, set only `HYOUI_STATE_DIR` in that face's `.envrc` (`XDG_STATE_HOME` is shared with other applications, so leave it alone). Sessions of another face do not appear in `list` and cannot be reached by id. There is no option that spans faces: run hyoui once per face with that face's variable. The config (`~/.config/hyoui/`) is shared by every face. `XDG_RUNTIME_DIR` is not used (its lifetime is tied to the login, while sessions outlive logins).

- The unix socket `sun_path` limit (104 bytes on macOS, 108 on Linux) is not checked against the full path. When the path does not fit, hyoui binds / connects with a name relative to an fd of the socket's directory, so deep roots work
- The only variable hyoui adds to the child's env is `HYOUI_SESSION_ID`. The face variable `HYOUI_STATE_DIR` reaches the child when the caller has it (`--login` included), so a hyoui started inside it uses the same face
- The following are not read as sessions, and `hyoui list` and `hyoui web ...` warn on stderr when any are present: sockets outside `sessions/` (directly under the state root, or in a directory other than `sessions/` / `web/`), sockets in `sessions/` whose id is not a UUID, sockets under `$XDG_RUNTIME_DIR/hyoui`, and symlinks left in those places. See `docs/runbooks/session-uuid-migration-dr-0041.md`

### 10. Stop leaking parent env into the child (env scrub)

When you call hyoui from inside an AI agent CLI like `claude`, the parent's
**Internal Context env** (e.g. `CLAUDE_CODE_SESSION_ID` / `CLAUDECODE` /
`AI_AGENT`) leaks into the child via plain POSIX fork→exec, and the child
session ends up misidentifying itself as a continuation of the parent. hyoui
strips those out before spawning the child
([DR-0024](./decisions/DR-0024-env-scrub-config-file.md)).

**For `claude` it just works** — the 9 env vars documented in the Claude Code
official env-vars docs are removed by the builtin defaults. No setup required.

| flag | purpose |
|---|---|
| `--no-scrub-env` | Disable scrub entirely (= debug / compatibility escape hatch) |

To strip env vars for an unregistered target (= AI agents other than `claude`,
or your own tools), or to keep some of the builtin-removed vars, edit
`~/.config/hyoui/config.toml`:

```toml
[scrub_env]
enabled = true                    # global on/off (default: true)

# Extend the builtin claude list
[scrub_env.targets.claude]
inherit_builtin = true            # default: true — concat builtin + user
kill_glob = ["CMUXMSG_*"]         # extra env names to remove
keep_glob = ["AI_AGENT"]          # env names to keep that builtin would remove

# Register a brand-new target (= a CLI hyoui doesn't know about)
[scrub_env.targets.my-tool]
inherit_builtin = false           # ignore builtin, user list only
kill_glob = ["MYTOOL_SECRET"]
```

The target key is the basename of `<cmd>` in `hyoui run -- <cmd>`. Wrappers
like `env` are not unwrapped — just write `hyoui run -- claude` directly
([DR-0024 §2](./decisions/DR-0024-env-scrub-config-file.md)).

Env vars whose names start with `HYOUI_` are never removed even if a user
`kill_glob` matches them (= hyoui itself passes `HYOUI_SESSION_ID` to the child
on purpose, and `HYOUI_STATE_DIR` keeps the child in the same face).

If the config has a parse error (= invalid TOML / type mismatch) hyoui refuses
to start (= booting with an unintended config risks leaking the parent's
Internal Context). Use `--no-scrub-env` if you need to bypass it temporarily.

**`--login` sessions have nothing for scrub to remove** (= the child env starts from a minimal set instead of inheriting the caller's).

#### Starting as a login shell (`--login`)

Starts the child like an ordinary terminal app does
([DR-0039](./decisions/DR-0039-webui-terminal-app-rework.md) decision 1).

```sh
hyoui run --login --detached --pty-stdin               # the passwd shell, as a login shell
hyoui run --login --detached --pty-stdin -- zsh -f     # explicit command (e.g. skip rc files)
```

- The shell is operated from outside, so `--pty-stdin` makes its stdin the PTY as well. Without it the caller's stdin becomes the child's stdin, and from a launcher without a terminal (a script or an agent) the shell exits right away on stdin EOF ([DR-0042](./decisions/DR-0042-non-tty-stdin-is-the-childs-fd.md))
- The shell comes from passwd (`getpwuid`); the caller's `$SHELL` is ignored
- argv[0] is `-<basename of the shell>` (e.g. `-zsh`); the shell reads its own rc files
- The child env starts minimal instead of inheriting the caller's: `HOME` / `USER` / `LOGNAME` / `SHELL` / an initial `PATH` / `LANG` (if the caller has it) / `TERM` (see below). The initial `PATH` is built from `/etc/paths` and `/etc/paths.d/*` on macOS, and is `/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin` elsewhere
- `HYOUI_SESSION_ID` stays in the child even though the env is minimal. `HYOUI_STATE_DIR` stays too when it is set (a hyoui started inside the child uses the parent's face, so self-reference works)
- With an explicit command, that command runs as-is (no `-` prefix on argv[0]) and only the env is minimal
- Only the child's env is minimized. Which face root hyoui itself uses (`HYOUI_STATE_DIR` / `XDG_*`) is still decided by the caller's env

The child's `TERM` is inherited from the caller, with or without `--login`. Only when the caller has none (unset / empty) does hyoui set `[session] term_fallback` from the config (default `xterm-256color`) ([DR-0039](./decisions/DR-0039-webui-terminal-app-rework.md) decision 1).

### 11. What happens when the child stops, and what Ctrl+Z does

Configured under `[session]` / `[attach]` in `~/.config/hyoui/config.toml`
([DR-0032](./decisions/DR-0032-child-suspend-unified-enum-and-action-menu.md)).

```toml
[session]
# What happens when the child suspends (stops). default: auto_resume_on_attached
on_child_suspend = "auto_resume_on_attached"
#   auto_resume_always      — the daemon always sends SIGCONT immediately
#   auto_resume_on_attached — resume only while an rw attach client is present
#   show_child_action_menu  — do not resume; the attach client shows an action menu
# TERM given to the child when the caller has none. default: xterm-256color
term_fallback = "xterm-256color"

[attach]
# What a settled single Ctrl+Z does. default: client_suspend
ctrlz_x1_action = "client_suspend"
#   client_suspend   — suspend the client itself (`fg` returns to the same window)
#   client_detach    — tear down the window (the child keeps running)
#   select_on_demand — show a prompt (^Z: suspend / ^C: quit client / Esc: back)
```

`on_child_suspend` is a single choice — "what happens when the child stops" —
that hyoui maps onto both the daemon policy and the attach client's behaviour.
`hyoui run --on-child-suspend=notify|auto-resume` overrides **only** the daemon
side.

**Child action menu** (when `show_child_action_menu` is selected): if the child
stops while an rw attach is open, a menu appears at the bottom of the screen so
you can act on the spot. Keystrokes are swallowed by hyoui while it is up, so
they never reach the child (= no burst of stale input flooding in on resume).

| Key | Action |
|---|---|
| `d` | Escape: detach (the client exits; the child stays stopped) |
| `z` | Escape: suspend the client (`fg` resumes the child too) |
| `c` / `Esc` | Child operation: resume it (SIGCONT). Esc acts as "undo this stop" |
| `i` / `h` | Child operation: SIGINT / SIGHUP (SIGCONT is sent alongside so it reaches a stopped child) |
| `k` | Child operation: SIGKILL |

Any key outside the table is ignored and discarded (there is no plain "close" action:
a stopped child cannot receive input, so leaving the menu has no meaning). The
menu goes away when you pick an action, or when the child is resumed externally
(e.g. `hyoui kill --signal=CONT` from another shell).

If the child stops while nobody is attached there is nowhere to draw the menu,
so it appears on the next `hyoui attach`. Pick `auto_resume_always` if you want
it resumed even with no client around.

The old keys `[session] auto_resume` / `[attach] resume_stopped_child` are gone.
Leaving them in place makes hyoui refuse to start and print what to write
instead (= a configured intent must not silently fall back to the default).

To see which file hyoui resolves and what it currently ends up with:

```bash
hyoui config path   # print the config file path (even if it does not exist yet)
hyoui config show   # print the effective configuration as TOML, defaults included
```

`config show` prints every key with its effective value, so it answers "how is
hyoui behaving right now" rather than "what did I write". Builtin scrub
defaults are appended as TOML comments (= they are not config keys).

### 11. Operate from a browser (`web`)

```sh
hyoui web daemon add stable     # write ~/.config/hyoui/web/stable.toml and register it
hyoui web daemon run stable
# Open http://127.0.0.1:43690/ in a browser.
```

`hyoui web daemon run` is the one way to start a gateway in the foreground, and it always names what to start (no arguments prints help). `hyoui web` itself only groups the `daemon` / `service` / `passkey` / `session` commands.

An instance (unit) is one config file; the registry only records which file each unit reads. `daemon add <name>` writes `${XDG_CONFIG_HOME:-~/.config}/hyoui/web/<name>.toml` when it does not exist and registers it. The file gets `extends` pointing at `base.toml` (when one sits next to it), `state_dir` (the state root of the current environment), `listen` (`--listen`, default `127.0.0.1:43690`), and `binary_path` (`--binary`, default: the executable running `daemon add`):

```toml
# ~/.config/hyoui/web/base.toml — shared by every unit (and every state root; no state_dir here)
[web]
assets_dir = "~/src/hyoui/crates/hyoui-web/assets"

# ~/.config/hyoui/web/unstable.toml — written by `daemon add unstable --listen 127.0.0.1:43691 --binary ~/src/hyoui/target/release/hyoui`
extends = "base.toml"            # resolved next to this file

[web]
state_dir = "/Users/me/.local/state/hyoui"
listen = "127.0.0.1:43691"
binary_path = "/Users/me/src/hyoui/target/release/hyoui"
```

```sh
hyoui web daemon add unstable --listen 127.0.0.1:43691 --binary ~/src/hyoui/target/release/hyoui
hyoui web daemon add mine --config ~/dotfiles/hyoui-web.toml   # register an existing file under this name
hyoui web service register   # load the supervisor that holds every unit
hyoui web daemon status
```

- When `<name>.toml` already exists it is registered as it is, never rewritten (`--listen` / `--binary` are then refused). `--config <path>` registers an existing file from anywhere
- `[web].state_dir` is required in the unit's config file itself (a value inherited from a base through `extends` is refused, since a base is shared by every state root). `daemon add` and `daemon run` compare it with the current state root (both resolved with realpath) and refuse a mismatch, saying which state root the config belongs to and which one is running. A config copied from another state root can only be caught here
- `daemon add` checks, writes, and registers under a lock on the state root's registry, and refuses without writing anything while another add is running. When the generated config cannot be read (a broken `base.toml`, for example), the unit is not registered and the file is removed
- `daemon add` refuses an address that another unit of the same state root uses, or a port some process is listening on (it tries to bind). It never picks a free port for you
- `daemon run <name>` reads the config the unit is registered with; `daemon run --config <path>` reads that file without the registry. Both read only that file and the files it reaches through `extends`, never `config.toml`. `daemon run --no-config [--listen <host:port>]` reads no config at all and starts from the built-in defaults and the command line (for tests)
- The supervisor starts each unit as `<binary_path> web daemon run <name>`

`extends` layers a file over another: tables merge key by key, other values replace. Relative paths are resolved next to the file that wrote them, and `~` expands to `$HOME`. `binary_path` is copied into the registry when the unit is added (to change it, `remove` and `add` again); `listen` and `assets_dir` are read from the file at every start. State (registry, logs, passkeys) lives in `<state root>/web/`. `service register` pins the location variables (`HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `HYOUI_STATE_DIR`) into the OS definition and refuses to change them later without `--force`. Registering from a shell where a face's `.envrc` is in effect sets up that face's own supervisor.

Open the keyboard FAB on a session page and select the Information tab to see the
attach mode and leader state. If another browser is leader, click “Become leader”
to move control without disconnecting either client; the PTY is resized to the new
browser's viewport. Failures are shown in the same Attach section.

## Troubleshooting

| Symptom | What to try |
|---|---|
| `hyoui list` shows nothing | Check that the face (`HYOUI_STATE_DIR` etc.) is the one the session was started with (sessions of another face are not visible). Sockets left in the old layout are reported on stderr by `hyoui list` (`docs/runbooks/session-uuid-migration-dr-0041.md`) |
| `hyoui run` refuses with "socket already exists" | If a session with that id is running, `hyoui kill --wait <id>` (it waits for the daemon to exit). If only the socket of a dead daemon is left, `hyoui list` cleans it up when the lock is still there; if it shows as stale, confirm that no daemon is behind it and remove it by hand. Or start with another id |
| Attach is closed immediately | The daemon may have rejected cap negotiation (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| Child process died but the daemon lingers | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

The full runbook index is `docs/runbooks/INDEX.md`.

## See also

- [README.md](../README.md) — Install, concepts, the first hello world
- [DESIGN.md](./DESIGN.md) — Internal architecture
- [ROADMAP.md](./ROADMAP.md) — When v0.2.0+ recipes will land
- [docs/runbooks/](./runbooks/) — Incident response procedures
