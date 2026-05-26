# hyoui User Manual

> English | [日本語](./MANUAL-ja.md)

A use-case-driven recipe book for end users (people driving `hyoui` from the
CLI).

- **Install / concept overview** → [`README.md`](../README.md)
- **Internal design / why it's built this way** → [`DESIGN.md`](./DESIGN.md)
- **This file**: "I want to do X" → "use this command sequence."

> Status: scaffold for v0.1.x. Real depth comes once v0.2.0 lands the `serve`
> family and the automation API (`send` / `keys` / `paste` / `wait` / `tail` /
> `lock` / `tx`). Today only v0.1 recipes are included.

## Table of contents

- [Core flow](#core-flow)
  - [1. Start a detached session and attach from another terminal](#1-start-a-detached-session-and-attach-from-another-terminal)
  - [2. Observe in read-only mode](#2-observe-in-read-only-mode)
  - [3. Stop a session](#3-stop-a-session)
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

## Troubleshooting

| Symptom | What to try |
|---|---|
| `hyoui list` shows nothing | Stale socket in `XDG_RUNTIME_DIR` / `TMPDIR` (`docs/runbooks/2026-05-27-stale-socket-detection.md`) |
| Attach is closed immediately | The daemon may have rejected cap negotiation (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| Child process died but the daemon lingers | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

The full runbook index is `docs/runbooks/INDEX.md`.

## See also

- [README.md](../README.md) — Install, concepts, the first hello world
- [DESIGN.md](./DESIGN.md) — Internal architecture
- [ROADMAP.md](./ROADMAP.md) — When v0.2.0+ recipes will land
- [docs/runbooks/](./runbooks/) — Incident response procedures
