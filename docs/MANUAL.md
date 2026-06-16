# hyoui User Manual

> English | [日本語](./MANUAL-ja.md)

A use-case-driven recipe book for end users (people driving `hyoui` from the
CLI).

- **Install / concept overview** → [`README.md`](../README.md)
- **Internal design / why it's built this way** → [`DESIGN.md`](./DESIGN.md)
- **This file**: "I want to do X" → "use this command sequence."

> Status: covers v0.2.x. The automation API (`input` family / `wait` / `screen` /
> `lock` / `record` / `tail`) is implemented. The `serve` HTTP gateway and `tx`
> wrapper are not yet shipped.

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
# Detach with Ctrl-A D
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

> **⚠ stdin redaction is NOT wired yet.** Regardless of `--input-secrecy`
> (`redact-after-prompt` is the default), stdin is recorded verbatim — the
> redaction state machine is parked in Phase 5
> ([DR-0016](./decisions/DR-0016-tty-io-record.md)). If you may type passphrases
> or tokens, record only `--stdout`, or avoid recording stdin during secret entry.

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

## Troubleshooting

| Symptom | What to try |
|---|---|
| `hyoui list` shows nothing | Stale socket in `$XDG_RUNTIME_DIR/hyoui` / `/tmp/hyoui-<uid>` (`docs/runbooks/2026-05-27-stale-socket-detection.md`) |
| Attach is closed immediately | The daemon may have rejected cap negotiation (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| Child process died but the daemon lingers | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

The full runbook index is `docs/runbooks/INDEX.md`.

## See also

- [README.md](../README.md) — Install, concepts, the first hello world
- [DESIGN.md](./DESIGN.md) — Internal architecture
- [ROADMAP.md](./ROADMAP.md) — When v0.2.0+ recipes will land
- [docs/runbooks/](./runbooks/) — Incident response procedures
