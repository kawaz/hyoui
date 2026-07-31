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
  - [9. Group sessions with namespaces](#9-group-sessions-with-namespaces)
  - [10. Stop leaking parent env into the child (env scrub)](#10-stop-leaking-parent-env-into-the-child-env-scrub)
  - [11. Operate from a browser (`web`)](#11-operate-from-a-browser-web)
- [Troubleshooting](#troubleshooting)
- [See also](#see-also)

## Core flow

### 1. Start a detached session and attach from another terminal

```sh
# Terminal A: launch detached; the session id is printed on stdout
hyoui run --detached -- claude
# → run-<pid>-<rand>  (example)

# Terminal B: list, then attach
hyoui list
hyoui attach run-<pid>-<rand>
# A single Ctrl+Z suspends the client (back to the shell; `fg` to return)
# To close the connection: hyoui detach run-<pid>-<rand>
```

### 2. Observe in read-only mode

```sh
hyoui attach --observer run-<pid>-<rand>
# observer attach forwards no input; output is read-only
```

### 3. Stop a session

```sh
hyoui kill run-<pid>-<rand>            # SIGTERM
hyoui kill --signal KILL run-<pid>-<rand>  # SIGKILL
```

## Automation

These recipes assume `SESS` holds a session id (e.g. `SESS=$(hyoui run --detached -- bash)`).

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

### 9. Group sessions with namespaces

Use **namespaces** ([DR-0018](./decisions/DR-0018-session-namespace.md)) when
unrelated session groups (e.g. your day-to-day `claude` and a temporary worker
fleet) should not mix in `hyoui list`. Resolution is
`--namespace` flag > env `HYOUI_NAMESPACE` > `default`, shared by every session
command. The `default` namespace keeps the traditional socket layout, so
existing sessions are unaffected.

```sh
# isolate a worker fleet
hyoui run --detached --namespace=workers --session=w1 -- worker-cmd
hyoui run --detached --namespace=workers --session=w2 -- worker-cmd

hyoui list                            # default only — workers don't show up
hyoui list --namespace=workers        # the fleet only
hyoui list --all-namespaces           # everything, with a leading NS column
hyoui list --all-namespaces --prune-stale  # sweep stale sockets in every namespace

# every selector is namespace-scoped (session id, --index, kill --all, ...)
hyoui attach w1 --namespace=workers
hyoui input --namespace=workers w1 "text:ls" "key:Enter"
hyoui kill --all --namespace=workers
```

**direnv recipe** — put this in a project `.envrc`:

```sh
export HYOUI_NAMESPACE=myproj
```

Every `hyoui run` / `list` / `attach` executed inside the project directory is
then isolated automatically, with no flags.

**Inheritance** — `hyoui run` always injects the resolved namespace into the
child's environment as `HYOUI_NAMESPACE` (even `default`), the same convention
as tmux's `TMUX` / screen's `STY`. A hyoui launched from inside a namespaced
session therefore stays in the same namespace by default; pass
`--namespace=<other>` (e.g. `--namespace=default`) to escape. The variable also
lets a process detect "am I running under hyoui, and in which namespace?".

Namespace names share the session-id character set (`[A-Za-z0-9._-]`, max 64
bytes); `/` is rejected today and reserved for possible future hierarchical
namespaces. `default` is a reserved name that maps to the base socket dir.

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
`kill_glob` matches them (= hyoui itself injects `HYOUI_NAMESPACE` /
`HYOUI_SESSION_ID` etc. on purpose).

If the config has a parse error (= invalid TOML / type mismatch) hyoui refuses
to start (= booting with an unintended config risks leaking the parent's
Internal Context). Use `--no-scrub-env` if you need to bypass it temporarily.

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
hyoui web --listen=127.0.0.1:43690
# Open http://127.0.0.1:43690/ in a browser.
```

Open the keyboard FAB on a session page and select the Information tab to see the
attach mode and leader state. If another browser is leader, click “Become leader”
to move control without disconnecting either client; the PTY is resized to the new
browser's viewport. Failures are shown in the same Attach section.

## Troubleshooting

| Symptom | What to try |
|---|---|
| `hyoui list` shows nothing | Stale socket in `$XDG_RUNTIME_DIR/hyoui` / `${XDG_STATE_HOME:-$HOME/.local/state}/hyoui` (`docs/runbooks/2026-05-27-stale-socket-detection.md`) |
| Attach is closed immediately | The daemon may have rejected cap negotiation (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| Child process died but the daemon lingers | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

The full runbook index is `docs/runbooks/INDEX.md`.

## See also

- [README.md](../README.md) — Install, concepts, the first hello world
- [DESIGN.md](./DESIGN.md) — Internal architecture
- [ROADMAP.md](./ROADMAP.md) — When v0.2.0+ recipes will land
- [docs/runbooks/](./runbooks/) — Incident response procedures
