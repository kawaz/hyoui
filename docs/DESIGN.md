# hyoui Design

> English | [日本語](./DESIGN-ja.md)

This document describes the **current implementation** as of v0.1.0. The
background, alternatives, and reasoning behind each decision live in
`docs/decisions/`. This document focuses on "what is running" along the two
axes of domain and architecture.

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
| **scrollback** | A ring buffer of past output (data source for tail / wait) |

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
      cli.rs        # CLI parser (subcommand dispatch, argument parsing)
      daemon/       # daemon side (server holding one session)
        mod.rs
        config.rs   # session config (socket path, scrollback size, ...)
        session.rs  # Session::serve = multi-attach + broadcast + control plane
      client/       # client side (connects to the daemon)
        mod.rs
        attach.rs   # ClientConnection (handshake + raw I/O + detach prefix)
      protocol/     # wire protocol
        mod.rs
        frame.rs    # u32 size + u8 type + body framing
        caps.rs     # capability negotiation (MVP_CAPS, intersect)
        messages/   # CBOR control message types (handshake, lock, tail, wait, ...)
        transports/ # Transport trait + UnixStreamTransport
      scrollback.rs # ring buffer of timestamped chunks (tail/wait data source)
      strip.rs      # ANSI escape sequence stripper (for wait text match)
      observer.rs   # legacy interface (v0.0.0 vestige, removal candidate)
      sys/          # all unsafe is concentrated here
        raw.rs      # forkpty / login_tty (child process spawn)
        signal.rs   # sigaction, self-pipe
        pty.rs      # PTY abstraction
        socket.rs   # Unix socket bind (perm 0600 / dir 0700)
        clock.rs    # Instant ↔ epoch ms
        poll.rs     # poll(2) wrapper
        ...
  hyoui-cli/        # binary crate (the `hyoui` command)
    src/
      main.rs       # entry point, dispatches the Command enum from cli.rs
      daemonize.rs  # double fork + setsid (--detached)
      socket_path.rs # socket directory resolver (XDG / TMPDIR)
      completion.rs # shell completion stub
```

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
- v0.1.0 cap set: `["data", "lock", "tail-v1", "screen-dump-v1", "state-snapshot-v1"]`
- Wait is **state-based** (= no cap; CLI side polls `screen.snapshot.request`).
  The legacy `wait.request` / `wait.result` path (scrollback regex, `wait-l0`
  cap) was removed per the DR-0006 §9 revision.

### 2.3 Daemon (`crates/hyoui/src/daemon/session.rs`)

`Session::serve` is the main loop. Responsibilities:

- **PTY management**: master fd set to `set_nonblocking(true)`, raw bytes read out
- **Client management**: socket accept, handshake (cap negotiation + mode +
  token verification), maintains the list of `ClientHandle`
- **Broadcast**: master → each client, encoding branches on subscription
  (Raw / TailFollow), encoded payload cached per `strip_ansi` boolean to avoid
  re-encoding
- **Multiplex**: each client → master (rw only; ro is silently dropped)
- **Leader management**: a new rw client becomes leader only when none exists;
  on leader detach the role cascades to the next rw client
- **Lock state machine**: `SessionState { lock_holder, lock_token }`;
  release succeeds only when token + holder both match
- **Scrollback**: owns `Scrollback::new(config.scrollback_bytes)`; pushed
  immediately after each master read
- **Pending waits**: a `Vec<PendingWait>` held by the serve loop, scanned on
  every incoming master byte
- **Backpressure**: `Arc<AtomicUsize> queued_bytes` enforces a per-byte cap;
  on overflow an `error` with kind `backpressure.disconnect` is sent and the
  client is `shutdown(Both)` and marked for removal

Child process startup uses forkpty + login_tty ([[DR-0003]]); `posix_spawn` is
rejected because it cannot acquire a controlling terminal.

### 2.4 Client (`crates/hyoui/src/client/attach.rs`)

`ClientConnection::run` is the main loop. Responsibilities:

- Sends handshake.request (caps / mode / token / exclusive / detach-others)
- Receives handshake.response, settles `session_id` / `client_id` / `leader` / `mode`
- stdin → frame writer (`type=0x00` raw data)
- frame reader → stdout
- **Detach prefix state machine**: `Ctrl-A D` detaches the client itself,
  `Ctrl-A Ctrl-A` sends a literal Ctrl-A to the child, `Ctrl-A <other>` is
  discarded (screen convention)
- For one-shot CLIs (`status` / `tail` / `wait` / `kill` / `list`) the connection
  exposes `recv_frame()` / `recv_control(buffer_raw_data)`

### 2.5 Scrollback (`crates/hyoui/src/scrollback.rs`)

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

### 2.6 Strip (`crates/hyoui/src/strip.rs`)

State-machine-based stripper for ANSI escape sequences (CSI / OSC / DCS /
single-char). Used as preprocessing for wait's `--text` / `--pattern` matching.
See `docs/findings/2026-05-26-ansi-strip.md` for details.

- Decoration stripping and newline conversion are kept as **separate layers**
  ([[DR-0006]] §11)
- Stripping only removes ANSI escapes; BEL / BS / TAB / LF / CR are preserved
- Newline conversion is a separate flag (`--newline-convert=preserve|lf|crlf`),
  available on wait and tail independently

### 2.7 sys module

All `unsafe` is confined to `sys/raw.rs` (forkpty / login_tty / TIOCSWINSZ) and
`sys/signal.rs` (sigaction / self-pipe). `sys/socket.rs` enforces perm 0600 /
dir 0700, `sys/poll.rs` wraps poll(2) type-safely, `sys/clock.rs` provides
Instant ↔ epoch ms.

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
   ↑↓ master fd read/write
[forkpty master FD]
   ↑↓ PTY
[child process]
```

The control plane (lock / resize / signal / handshake / ...) is multiplexed
on the same socket as `type=0x01` CBOR frames.

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

- The single VERSION file plus `Cargo.toml [workspace.package].version` are kept
  in sync by `pkf run bump-version`
- A push to `main` triggers release.yml (detected by VERSION change)
- The workflow itself creates the tag and the GH Release (see the
  `release-flow-awareness` rule)
- v0.1.0 was released on 2026-05-27 with 208 tests passing

## 7. Related documents

| Category | Location | Content |
|---|---|---|
| Philosophy | [[DR-0005]] | hyoui's direction (outside-driven automation, not a multiplexer) |
| CLI spec | [[DR-0006]] | Operational model, automation APIs, exclusion |
| MVP scope | [[DR-0007]] | v0.1.0 / v0.2.0 / v0.3.0+ staged release |
| Protocol | [[DR-0008]] | Wire format, cap flags, transport abstraction |
| Job control | [[DR-0001]] | bg/fg two-axis, precise SIGTSTP/SIGCONT |
| Language | [[DR-0003]] | Rust-only, forkpty + login_tty |
| CLI shape | [[DR-0004]] | Subcommand approach |
| Naming | [[DR-0002]] | Project name "hyoui" |
| Dev log | `docs/journal/` | Implementation history, pitfalls and fixes |
| PoC findings | `docs/findings/` | PTY / signal / socket / scrollback / ANSI strip results |
| Future work | `docs/ROADMAP.md` | Planned items |
