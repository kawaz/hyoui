# hyoui Design

> English | [日本語](./DESIGN-ja.md)

This document describes the **current implementation** (v0.1.x line, with
[[DR-0013]] Phase A/B reflected). Background, alternatives, and reasoning for
each decision live in `docs/decisions/`. This document focuses on "what is
running" along the two axes of domain and architecture.

> [[DR-0013]] (2026-05-27) introduced a screen emulator (based on the `vt100`
> crate) inside the daemon, putting wait / snapshot / dump / lock / the input
> family onto a state-based foundation. This DESIGN treats the state-based
> spec as canonical.

## 1. Domain

### 1.1 What hyoui is

A PTY wrapper CLI. It launches an arbitrary command inside a PTY, behaves
completely transparently toward the child (no in-band escape), and offers a
**foothold for the outside world** to observe and drive the process. See
[[DR-0005]] for the design philosophy and why hyoui is explicitly *not* a TUI
multiplexer.

### 1.2 Key concepts

| Term | Meaning |
|---|---|
| **session** | The bundle of daemon + child PTY + scrollback. Created by `run`, identified by `<session_id>` |
| **client** | A process that connects to a session (one-shot CLI, long-running attach, WebSocket in the future) |
| **attach** | A client connects to a session and starts relaying I/O |
| **detach** | A client disconnects from a session (the child keeps running) |
| **leader** | The representative client used for TIOCSWINSZ in a session (auto-granted to one of the rw clients) |
| **mode** | Client operating mode (`rw` / `ro` / `rw-no-leader`) |
| **lock** | Exclusive-acquisition state, identified by a token (for atomic automation) |
| **scrollback** | A byte-base ring buffer of past output (data source for `tail`; see §2.5 for how it now coexists with the row-base screen state) |

Vocabulary follows the "industry standard" choice in DR-0008 §4. The concept
model is close to abduco / shpool.

### 1.3 Operational model

- **Screen-style** (one daemon, one socket, one child). The tmux-style model
  (one server, many sessions) is rejected ([[DR-0006]] §1)
- The **filesystem is the source of truth** (`hyoui list` walks the socket
  directory)
- The daemon exits as soon as the child exits, even while all clients are
  detached
- Socket placement: Linux `$XDG_RUNTIME_DIR/hyoui/<session>.sock`, macOS
  `$TMPDIR/hyoui-$UID/<session>.sock`
- Directory mode 0700, socket mode 0600 (same-UID trust boundary)

## 2. Architecture

### 2.1 Crate layout

```
crates/
  hyoui/            # library crate (all core functionality)
    src/
      lib.rs        # re-exports
      cli.rs        # CLI parser + subcommand definitions
      daemon/       # daemon side (server holding one session)
        mod.rs
        config.rs   # session config (socket path, scrollback size, screen sizes)
        session.rs  # Session::serve = multi-attach + broadcast + control plane
        screen/     # vt100 ScreenState wrapper (DR-0013)
          mod.rs
          state.rs       # VirtualScreen (owns vt100::Parser as the canonical state)
          input_log.rs   # bounded ring for primary buffer (resize replay)
          snapshot.rs    # structured snapshot wrapper (CBOR compression)
          redraw.rs      # initial redraw on attach
          health.rs      # screen state health checks
        control.rs  # control message dispatcher
        broadcast.rs # writer pump + backpressure + ClientHandle
        accept.rs   # handshake worker pool
        wait.rs     # state polling helpers (snapshot trigger / poll interval)
        tail.rs     # tail subscription
        lock.rs     # SessionState + leader cascade
        pty.rs      # child lifecycle
        record.rs   # tty I/O timeline recording (DR-0016)
      client/       # client side (connects to the daemon)
        mod.rs
        attach.rs   # ClientConnection (handshake + raw I/O + detach prefix + raw bytes send)
      protocol/     # wire protocol
        mod.rs
        frame.rs    # u32 size + u8 type + body framing
        caps.rs     # capability negotiation (MVP_CAPS, intersect)
        messages/   # CBOR control message types (handshake, lock, tail,
                    #   screen.dump, screen.snapshot, ...)
        transports/ # Transport trait + UnixStreamTransport
      scrollback.rs # byte-base ring buffer (`hyoui tail` only: since / last_bytes
                    #   semantics with receive-time ordering; see DR-0013 §8 Update)
      strip.rs      # ANSI escape sequence stripper (used by `tail --strip`; wait
                    #   no longer needs it since the state already holds cell text)
      sys/          # all unsafe is concentrated here
        raw.rs      # forkpty / login_tty (child process spawn)
        signal.rs   # sigaction, self-pipe
        pty.rs      # PTY abstraction
        socket.rs   # Unix socket bind (perm 0600 / dir 0700)
        clock.rs    # Instant ↔ epoch ms
        poll.rs     # poll(2) wrapper
        wait.rs     # waitpid wrapper
        fd.rs / env.rs / tty.rs / error.rs
  hyoui-cli/        # binary crate (the `hyoui` command)
    src/
      main.rs       # entry point, dispatches the Command enum from cli.rs
      daemonize.rs  # double fork + setsid (--detached)
      socket_path.rs # socket directory resolver (XDG / TMPDIR)
      input_handlers.rs # subcommand handlers for the input family
      wait_core.rs  # state-based wait polling (snapshot trigger + cells → text)
      completion.rs # shell completion generation
```

The daemon module split is canonical in [[DR-0009]]; the `screen/` subtree is
canonical in [[DR-0013]].

`#![forbid(unsafe_code)]` is applied to the entire `hyoui-cli` crate. In the
`hyoui` library, `unsafe` is confined to `sys/raw.rs` and `sys/signal.rs`
only — the rest uses safe `nix` APIs. The Rust-only choice is detailed in
[[DR-0003]].

### 2.2 Protocol (wire format)

[[DR-0008]] is the canonical reference. Summary:

```
Frame: [u32 LE size][u8 type][body]   size ≤ 16 MiB
  type=0x00: raw PTY data (raw bytes, transparent)
  type=0x01: CBOR control message
  type=0x02..0xff: reserved (treat as protocol error, disconnect)

Control message body (type=0x01) = CBOR map { "kind": "<dotted.name>", ...payload }
  e.g. handshake.request / handshake.response / lock.acquire / lock.response
       resize / signal / tail.request / tail.data / tail.end
       screen.dump.request / screen.dump.response / screen.snapshot.request /
       screen.snapshot.response / status.query / status.response / error / kill
```

- The **outer wire envelope (size + type + body) is permanently fixed**;
  breaking changes use a separate socket path (fork)
- Control messages are CBOR maps where **unknown fields are ignored**;
  cap flags negotiate "what the peer can speak"
- v0.1.x cap set: `["data", "lock", "tail-v1", "screen-dump-v1", "state-snapshot-v1"]`
- Wait is **state-based** (no dedicated cap or kind; the CLI side
  `hyoui-cli/src/wait_core.rs` polls `screen.snapshot.request`, rebuilds text
  from the visible cells, and runs the regex). The legacy `wait.request` /
  `wait.result` kinds, the `wait-l0` cap, and the `wait.*` error codes were
  removed from both wire and implementation when DR-0006 §9 and DR-0013 §9
  landed.

### 2.3 Daemon (`crates/hyoui/src/daemon/`)

`Session::serve` is the main loop. [[DR-0009]] split the responsibilities
across nine modules — `session.rs` is now the orchestrator, with `pty.rs` /
`accept.rs` / `broadcast.rs` / `control.rs` / `lock.rs` / `wait.rs` /
`tail.rs` / `screen/` underneath. Responsibilities:

- **PTY management** (`pty.rs`): the master fd is `set_nonblocking(true)`;
  read pulls out raw bytes
- **Screen state as the canonical source** (`screen/`, [[DR-0013]] Phase A/B):
  - child PTY bytes are fed through `vt100::Parser::process` *before* any
    broadcast (the state is the source of truth, not the raw stream)
  - the `VirtualScreen` wrapper holds the cell grid / cursor / mode flags /
    alt-screen switching / scrollback (row-base ring)
  - on attach handshake, the daemon sends a single frame of **redraw bytes**
    built from `state_formatted()` + alt-mode prepend, so the client's screen
    is fully restored even for alt-screen-resident apps like `claude` TUI
  - a bounded **input bytes log** (default 1 MiB) backs the primary buffer so
    that a resize can rebuild a fresh parser and replay
  - DEC synchronized-update (`?2026h`) hook + a 5 s stalled-sequence reset
    serve as health checks
- **Client management** (`accept.rs`): socket accept + handshake (cap
  negotiation + mode + token verification), and the list of `ClientHandle`s
- **Broadcast** (`broadcast.rs`): master → each client; encoding branches on
  subscription (`Raw` / `TailFollow`); two encoded-payload caches keyed by
  `strip_ansi` avoid re-encoding for clients sharing a subscription
- **Multiplex**: each client → master (rw only; ro is silently dropped)
- **Leader management** (`lock.rs`): a new rw client becomes leader only when
  none exists; on leader detach the role cascades to the next rw client
- **Lock state machine** (`lock.rs`): `SessionState { lock_holder, lock_token }`;
  release succeeds only when token + holder both match
- **Byte-base scrollback** (`scrollback.rs`): owns
  `Scrollback::new(config.scrollback_bytes)`; pushed immediately after each
  master read. Dedicated to the `hyoui tail` command (`--since` /
  `--last-bytes`)
- **State-based wait helpers** (`wait.rs`): incoming master bytes trigger
  snapshot polling and compute the poll interval. The legacy L0 wait protocol
  (`wait.request` / `wait.result` kinds) was removed; the actual matching
  lives in the CLI (`hyoui-cli/src/wait_core.rs`)
- **Structured snapshot / dump** ([[DR-0013]] §9): handlers for
  `screen.dump.request` / `screen.snapshot.request`, routed through the CBOR
  compression wrapper in `screen/snapshot.rs`
- **Backpressure** (`broadcast.rs`): `Arc<AtomicUsize> queued_bytes` enforces
  a per-byte cap; on overflow an `error` with kind `backpressure.disconnect`
  is sent and the client is `shutdown(Both)` and marked for removal

Child process startup uses forkpty + login_tty ([[DR-0003]]); `posix_spawn` is
rejected because it cannot acquire a controlling terminal.

### 2.4 Client (`crates/hyoui/src/client/attach.rs`)

`ClientConnection::run` is the main loop. Responsibilities:

- Sends `handshake.request` (caps / mode / token / exclusive / detach-others)
- Receives `handshake.response`, settles `session_id` / `client_id` /
  `leader` / `mode`
- Right after the handshake, writes the daemon's **redraw bytes frame**
  ([[DR-0013]] §4) directly to stdout — that single frame fully restores the
  pre-detach screen
- stdin → frame writer (`type=0x00` raw data)
- frame reader → stdout
- **Detach prefix state machine**: `Ctrl-A D` detaches the client itself,
  `Ctrl-A Ctrl-A` sends a literal Ctrl-A to the child, `Ctrl-A <other>` is
  discarded (screen convention)
- For one-shot CLIs (`input` / `screen dump` / `screen snapshot` / `tail` /
  `wait` / `lock` / `kill` / `list` / `status`) the connection exposes
  `recv_frame()` / `recv_control(buffer_raw_data)` / `send_raw_bytes()`

### 2.5 Byte-base scrollback (`crates/hyoui/src/scrollback.rs`)

```rust
struct OutputChunk { timestamp: Instant, bytes: Vec<u8> }
VecDeque<OutputChunk>  // ring buffer
last_evicted_ts: Option<Instant>
```

- When the byte limit is exceeded, the oldest chunk is `pop_front`-ed and
  `last_evicted_ts` is updated
- `--since DUR` filters internally; if `last_evicted_ts >= since_start`,
  the result is incomplete
- `--since-strict` turns incomplete results into a non-zero exit
- Default: 4 MiB (chosen for claude / TUI workloads)
- **Purpose**: this is the data source for `hyoui tail` only — its
  receive-time ordering and timestamp filter make `since_ms` /
  `--since-strict` / `--last-bytes` meaningful
- Responsibility-separated from the vt100 internal ring (the row-base cell
  layer in §2.6); see [[DR-0013]] §8 Update

### 2.6 Row-base screen state (`crates/hyoui/src/daemon/screen/`)

The **vt100 ScreenState wrapper** introduced in [[DR-0013]] Phase A/B is the
daemon's canonical screen representation.

- `state.rs::VirtualScreen` owns a `vt100::Parser` (+ `Screen`)
- Child PTY bytes are **always routed through the Parser** before any
  broadcast (no direct raw-byte broadcast)
- Exposed API: cell grid / cursor / mode flags (alt screen / app_keypad /
  bracketed_paste / ...) / scrollback offset / window size / `state_formatted()`
- On attach handshake, `state_formatted()` plus an alt-mode prepend become a
  single redraw frame sent to the client
- `input_log.rs`: bounded ring for the primary buffer (default 1 MiB); on
  resize, a fresh parser is created and the log is replayed (this works
  around vt100's `set_size` being truncate-only)
- `snapshot.rs`: structured-state CBOR wrapper with sparse cells, Color
  variant packed as an integer, and attribute bit packing for size

### 2.7 Strip (`crates/hyoui/src/strip.rs`)

State-machine ANSI escape sequence stripper (CSI / OSC / DCS / single-char).

- **Current use**: `hyoui tail --strip` — preprocessing for piping the byte
  stream into `grep` / scripts
- The old use (preprocessing for wait's `--text` / `--pattern`) is gone:
  state-based wait operates on text already extracted from cells, where
  escapes simply do not exist ([[DR-0006]] §9.1)
- Decoration stripping and newline conversion stay as **separate layers**
  ([[DR-0006]] §11.1)
- Stripping only removes ANSI escapes; BEL / BS / TAB / LF / CR are preserved
- Newline conversion is a separate flag (`--newline-convert=preserve|lf|crlf`)
  on the tail side

### 2.8 sys module

All `unsafe` is confined to `sys/raw.rs` (forkpty / login_tty / TIOCSWINSZ)
and `sys/signal.rs` (sigaction / self-pipe). `sys/socket.rs` enforces perm
0600 / dir 0700, `sys/poll.rs` wraps poll(2) type-safely, `sys/clock.rs`
provides Instant ↔ epoch ms.

### 2.9 Record (`crates/hyoui/src/daemon/record.rs`)

Persistent tty I/O timeline recording
([DR-0016](./decisions/DR-0016-tty-io-record.md)). The daemon is the canonical
owner of the byte stream (it already sees every in/out byte at the broadcast
point), so recording is wired as an **independent I/O sink in the daemon** —
separate from the client broadcast path.

- `RecordRegistry` holds active records per session; `record start/stop/list`
  control messages create / drain / enumerate them
- The hot path pushes events into a **bounded queue**; a dedicated **writer
  task** drains it to the file. The queue is bounded so a slow disk can't stall
  the PTY read/write loop or distort the timing it observes (DR-0016's
  "must not distort the observed target" invariant)
- `RecordEvent` carries in/out bytes, reject / write-error, lifecycle
  (start/stop, SIGTSTP/SIGCONT) and a monotonic `seq`
- Formats: `jsonl` (one event per line, with timestamps + lifecycle events;
  the diagnostic timeline format) and `raw` (single-direction byte stream, no
  timestamps — export only, `--both` not allowed)
- Caps: gated behind the `record-v1` optional capability so older clients keep
  working
- **⚠ Secret redaction is not yet implemented.** `--input-secrecy`
  (`redact-after-prompt` default) is accepted and stored, but the redaction
  state machine is parked in Phase 5 — stdin is recorded verbatim regardless of
  the policy. `InSecretRedacted` / `push_in_secret_redacted` are dead code until
  then.

## 3. Data flow

### 3.1 I/O while attached

```
[user terminal]
   ↑↓ stdin/stdout (raw bytes)
[hyoui attach (client)]
   ↑↓ frame: [u32 size][u8 type=0x00][raw bytes]
[Unix socket]
   ↑↓
[hyoui daemon (Session::serve)]
   ↑↓ master fd read → vt100::Parser::process → state update
   ↑↓ master fd write
[forkpty master FD]
   ↑↓ PTY
[child process]
```

The control plane (lock / resize / signal / handshake / screen.dump /
screen.snapshot / ...) is multiplexed on the same socket as `type=0x01` CBOR
frames.

### 3.1.1 Attach handshake redraw restore ([[DR-0013]] §4 Phase A)

```
[client] handshake.request (caps, mode, token)
   ↓
[daemon] handshake.response (session_id, client_id, settled caps)
   ↓
[daemon] alt-mode restore sequence (?1049h, etc.) prepended
   + VirtualScreen::state_formatted()  // ANSI rebuilt from the grid
   + explicit cursor reposition \x1b[<y>;<x>H appended
   ↓ sent as one frame (type=0x00)
[client] writes the frame to stdout — the pre-detach screen is fully restored
```

This is why **observing alt-screen-resident apps like `claude` TUI works
cleanly after attach** — the child PTY is never asked to redraw, so the child
sees attach as fully transparent.

### 3.2 Multi-attach broadcast

```
[master read N bytes]
   ↓
[scrollback.push]
   ↓
[update_waits_on_master_bytes (append to pending waits + scan)]
   ↓
[broadcast_master_bytes]
   ├→ client A (Subscription::Raw)                              → type=0x00 raw frame
   ├→ client B (Subscription::TailFollow{strip_ansi:true})      → type=0x01 tail.data CBOR
   └→ client C (Subscription::TailFollow{strip_ansi:false})     → type=0x01 tail.data CBOR
```

Encoded-payload caches are split by `strip_ansi` (true/false) so multiple
clients with the same subscription do not pay the encoding cost twice.

### 3.3 Backpressure

```
[during broadcast]
   ↓
[client.queued_bytes.fetch_add(N) > cap ?]
   ├ yes → send error{kind="backpressure.disconnect"} to the client,
   │       shutdown(Both), mark the ClientHandle for removal
   └ no  → enqueue normally
```

`queued_bytes` is `Arc<AtomicUsize>` and decremented when a ClientHandle is
dropped.

## 4. Trust boundary and authentication

- The **same UID** is the trust boundary ([[DR-0008]] §7)
- `sys/socket.rs` enforces socket perm 0600 + parent dir 0700
- The `HYOUI_LOCK_TOKEN` env var is sent in the handshake's `token` field; the
  daemon checks it against the holder
- No encryption (we are inside the same-UID trust region; other UIDs are
  blocked at the permission layer)
- TCP / WebSocket transports added in v0.2.0+ will require token-based auth or
  TLS via a separate DR

## 5. Portability

hyoui is a PTY-piggyback tool that assumes POSIX-style PTYs, signals, and Unix
domain sockets. Support tiers and known gaps:

| Tier | OS | Status |
|---|---|---|
| Tier 1 (CI-verified) | Linux x86_64 / aarch64, macOS Intel / Apple Silicon | Release-blocking; `cargo test` must pass |
| Tier 2 (intended but not auto-verified) | WSL2 (Linux-compatible kernel) | Best-effort; sanity-checked by kawaz |
| Unsupported | WSL1, Solaris/illumos, the BSDs, Cygwin/MSYS, native Windows | Does not work or requires a separate issue |

Dependencies on OS features and known portability gaps:

- **`forkpty(3)` + `login_tty(3)` are BSD extensions, not POSIX.** They exist
  on Linux (glibc/musl), macOS, and the BSDs; they do not exist on
  Solaris/illumos (which would need a hand-rolled wrapper using `posix_openpt`
  plus manual controlling-terminal setup). Solaris-family systems are out of
  scope ([[DR-0003]] reinforcement).
- **`waitpid(WCONTINUED)` is not implemented on WSL1** (the `WCONTINUED` flag
  fails with an ENOSYS-equivalent). WSL2 uses a real Linux kernel and is fine.
- **`IUTF8` (termios input flag) only exists on Linux/Android and Apple
  targets.** Other OSes get a cfg-gated no-op (see R5-M9).
- **`SO_PEERCRED` (Linux) and `getpeereid(3)` (BSD/macOS) differ in name and
  retrieval path.** A defense-in-depth UID-match assert needs per-OS branches
  (R5-M11).
- **`CLOCK_MONOTONIC` behavior across suspend is OS-dependent.** Linux/macOS
  pause it; FreeBSD advances. `wait` timeouts inherit the OS behavior (R5-M21).
- **`pipe2(O_CLOEXEC)` only exists on Linux/FreeBSD.** macOS needs `pipe(2)`
  plus `fcntl(F_SETFD, FD_CLOEXEC)`. Since `nix::unistd::pipe`'s CLOEXEC
  semantics could change in a future version, prefer explicit control over
  implicit dependence on the default (R5-M4).

Adding a new OS starts from a fresh DR-0003-style decision record.

## 6. Release management

- The single VERSION file plus `Cargo.toml [workspace.package].version` are
  kept in sync by `pkf run bump-version`
- A push to `main` triggers `release.yml` (detected by VERSION change)
- The workflow itself creates the tag and the GH Release (see the
  `release-flow-awareness` rule)
- v0.1.0 was released on 2026-05-27 with 208 tests passing. Subsequent
  releases are incremental minor bumps as [[DR-0013]] Phase A/B and the
  state-based revisions land. The version-segmented scope model has been
  retired; `docs/ROADMAP.md` is canonical for scope (see [[DR-0007]] Update).

## 7. Related documents

| Category | Location | Content |
|---|---|---|
| Philosophy | [[DR-0005]] | hyoui's direction (outside-driven automation, not a multiplexer) |
| CLI spec | [[DR-0006]] | Operational model, automation APIs, exclusion (§8-§11 state-based) |
| MVP scope | [[DR-0007]] | Staged release strategy (version segments retired; ROADMAP is canonical) |
| Protocol | [[DR-0008]] | Wire format, cap flags, transport abstraction |
| Screen emulator | [[DR-0013]] | Daemon = canonical screen state, attach restore, state-based foundation |
| Session split | [[DR-0009]] | daemon/session.rs split into nine modules |
| Job control | [[DR-0001]] | bg/fg two-axis, precise SIGTSTP/SIGCONT |
| Language | [[DR-0003]] | Rust-only, forkpty + login_tty |
| CLI shape | [[DR-0004]] | Subcommand approach |
| Naming | [[DR-0002]] | Project name "hyoui" |
| Dev log | `docs/journal/` | Implementation history, pitfalls and fixes |
| PoC findings | `docs/findings/` | PTY / signal / socket / scrollback / ANSI strip results |
| Future work | `docs/ROADMAP.md` | Planned items |
