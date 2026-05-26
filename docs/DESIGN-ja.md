# hyoui Design

> [English](./DESIGN.md) | 日本語

v0.1.0 時点の **現実装** の説明。設計判断の背景・経緯は `docs/decisions/` の DR
を参照。本ドキュメントは「いま動いているもの」をドメインとアーキテクチャの 2 軸で記す。

## 1. ドメイン

### 1.1 hyoui とは

PTY ラッパー CLI。任意のコマンドを PTY 内で起動し、子に対しては完全透過（in-band
escape なし）に振る舞いつつ、**外側から監視・自動操作するための足場**を提供する。
位置づけの詳細と「TUI multiplexer ではない」理由は [[DR-0005]]。

### 1.2 主要概念

| 用語 | 意味 |
|---|---|
| **session** | daemon + 子 PTY + scrollback の集合体。`run` で起動、`<session_id>` で識別 |
| **client** | session に接続するプロセス（CLI 単発 / 長期 attach / 将来は WebSocket） |
| **attach** | client が session に接続して入出力を中継開始する操作 |
| **detach** | client が session から切断する操作（子は生存） |
| **leader** | session 内で TIOCSWINSZ 計算対象になる代表 client（rw mode の 1 つに自動付与） |
| **mode** | client の動作 mode（`rw` / `ro` / `rw-no-leader`） |
| **lock** | 排他取得状態、token で識別（自動操作の atomic 性確保用） |
| **scrollback** | 過去出力の ring buffer（tail / wait のデータソース） |

語彙は DR-0008 §4 で「industry 標準」を採用。abduco / shpool に近い概念体系。

### 1.3 動作モデル

- **screen 型** (1 daemon 1 socket 1 子) を採用。tmux 型 (1 server 多 session) は不採用 ([[DR-0006]] §1)
- session の存在は **filesystem が source of truth** (`hyoui list` は socket dir 走査)
- daemon は子 exit で即終了、全 client detach 中でも生存
- socket 配置: Linux `$XDG_RUNTIME_DIR/hyoui/<session>.sock` / macOS `$TMPDIR/hyoui-$UID/<session>.sock`
- dir mode 0700 / sock mode 0600（同 UID 信頼境界）

## 2. アーキテクチャ

### 2.1 crate 構成

```
crates/
  hyoui/            # library crate (= 全コア機能)
    src/
      lib.rs        # re-export
      cli.rs        # CLI parser (subcommand 分岐、引数解析)
      daemon/       # daemon (= session 1 つを抱える server)
        mod.rs
        config.rs   # session config (socket path, scrollback size, ...)
        session.rs  # Session::serve = multi-attach + broadcast + control plane
      client/       # client (= daemon に attach する側)
        mod.rs
        attach.rs   # ClientConnection (handshake + raw I/O + detach prefix)
      protocol/     # wire protocol
        mod.rs
        frame.rs    # u32 size + u8 type + body の framing
        caps.rs     # capability negotiation (MVP_CAPS, intersect)
        messages/   # CBOR control message types (handshake, lock, tail, wait, ...)
        transports/ # Transport trait + UnixStreamTransport
      scrollback.rs # ring buffer (timestamped chunks、tail/wait のデータソース)
      strip.rs      # ANSI escape sequence strip (wait の text match 用)
      observer.rs   # legacy interface (v0.0.0 名残、削除候補)
      sys/          # unsafe を集約
        raw.rs      # forkpty / login_tty (子プロセス起動)
        signal.rs   # sigaction 登録、self-pipe
        pty.rs      # PTY abstraction
        socket.rs   # Unix socket bind (perm 0600 / dir 0700)
        clock.rs    # Instant ↔ epoch ms 変換
        poll.rs     # poll(2) wrapper
        ...
  hyoui-cli/        # binary crate (`hyoui` command)
    src/
      main.rs       # entry point、cli.rs の Command を dispatch
      daemonize.rs  # double fork + setsid (--detached)
      socket_path.rs # socket dir resolver (XDG / TMPDIR)
      completion.rs # shell completion stub
```

`#![forbid(unsafe_code)]` は `hyoui-cli` 全体に、`hyoui` lib では `sys/raw.rs` と
`sys/signal.rs` の 2 ファイルに `unsafe` を封じ込め（残部は nix 安全 API のみ）。
Rust 一本化の判断は [[DR-0003]]。

### 2.2 protocol（wire format）

[[DR-0008]] が正本。要点だけ:

```
Frame: [u32 LE size][u8 type][body]   size ≤ 16 MiB
  type=0x00: raw PTY data (= 生 bytes、透過)
  type=0x01: CBOR control message
  type=0x02..0xff: 予約 (受信時は protocol error → disconnect)

Control message body (type=0x01) = CBOR map { "kind": "<dotted.name>", ...payload }
  例: handshake.request / handshake.response / lock.acquire / lock.response
       resize / signal / tail.request / tail.data / tail.end
       wait.request / wait.result / status.query / status.response / error / kill
```

- **wire 外枠 (size + type + body) は永久固定**。breaking change は別 socket path で fork
- 制御メッセージは CBOR map で **未知 field は ignore**、cap flags で「相手が話せるか」交渉
- v0.1.0 cap 集合: `["data", "lock", "tail-v1", "wait-l0"]`

### 2.3 daemon (`crates/hyoui/src/daemon/session.rs`)

`Session::serve` がメインループ。責務:

- **PTY 管理**: master fd を `set_nonblocking(true)`、read で raw bytes を取り出す
- **client 管理**: socket accept、handshake (cap negotiation + mode + token 検証)、
  `ClientHandle` 群を保持
- **broadcast**: master → 各 client、subscription (Raw / TailFollow) に応じて encoding を分岐、
  strip_ansi の真偽でキャッシュを 2 個分け再 encode を回避
- **multiplex**: 各 client → master (rw のみ書き込み許可、ro は silently drop)
- **leader 管理**: rw 新 client に leader 不在時のみ自動委譲、leader detach 時は次の rw に cascade
- **lock state machine**: `SessionState { lock_holder, lock_token }`、token + holder 一致で release
- **scrollback**: `Scrollback::new(config.scrollback_bytes)` を所有、master read 直後に push
- **pending waits**: `Vec<PendingWait>` を serve_loop で保持、master bytes 着信ごとに scan
- **backpressure**: `Arc<AtomicUsize> queued_bytes` で byte 単位 cap、超過時 `backpressure.disconnect`
  を送って当該 client を `shutdown(Both)` で drop

子プロセス起動は forkpty + login_tty ([[DR-0003]])。`posix_spawn` は controlling terminal を
取れないため不採用。

### 2.4 client (`crates/hyoui/src/client/attach.rs`)

`ClientConnection::run` がメインループ。責務:

- handshake.request 送信 (caps / mode / token / exclusive / detach-others)
- handshake.response 受信、`session_id` / `client_id` / `leader` / `mode` を確定
- stdin → frame writer (`type=0x00 raw data`)
- frame reader → stdout
- **detach prefix state machine**: `Ctrl-A D` で client 自身を detach、
  `Ctrl-A Ctrl-A` で literal Ctrl-A を子に送る、`Ctrl-A <他>` は両捨て (screen 慣例)
- 1-shot CLI (`status` / `tail` / `wait` / `kill` / `list`) 用に `recv_frame()` /
  `recv_control(buffer_raw_data)` を提供

### 2.5 scrollback (`crates/hyoui/src/scrollback.rs`)

```rust
struct OutputChunk { timestamp: Instant, bytes: Vec<u8> }
VecDeque<OutputChunk>  // ring buffer
last_evicted_ts: Option<Instant>
```

- size 上限超過で古い chunk から pop_front、`last_evicted_ts` 更新
- `--since DUR` は内部フィルタ、`last_evicted_ts >= since_start` なら不完全
- `--since-strict` で不完全を非 0 exit に
- default 4 MiB（claude / TUI 主用途想定）

### 2.6 strip (`crates/hyoui/src/strip.rs`)

ANSI escape sequence (CSI / OSC / DCS / single char) state machine による strip。
wait の `--text` / `--pattern` で text match する際の前処理。詳細は
`docs/findings/2026-05-26-ansi-strip.md`。

- 装飾除去と改行変換は **別レイヤ** ([[DR-0006]] §11 で確定)
- 装飾除去は ANSI escape のみ、BEL / BS / TAB / LF / CR は残す
- 改行変換は別 flag (`--newline-convert=preserve|lf|crlf`) で wait / tail 個別指定

### 2.7 sys モジュール

unsafe を `sys/raw.rs` (forkpty / login_tty / TIOCSWINSZ) と `sys/signal.rs` (sigaction
/ self-pipe) の 2 ファイルに封じ込め。`sys/socket.rs` で perm 0600 / dir 0700 enforce、
`sys/poll.rs` で poll(2) を type-safe に wrap、`sys/clock.rs` で Instant ↔ epoch ms。

## 3. データフロー

### 3.1 attach 中の I/O

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

control plane (lock / resize / signal / handshake / ...) は同じ socket を type=0x01
CBOR frame で multiplex。

### 3.2 multi-attach の broadcast

```
[master read N bytes]
   ↓
[scrollback.push]
   ↓
[update_waits_on_master_bytes (pending waits に append + scan)]
   ↓
[broadcast_master_bytes]
   ├→ client A (Subscription::Raw)          → type=0x00 raw frame
   ├→ client B (Subscription::TailFollow{strip_ansi:true})  → type=0x01 tail.data CBOR
   └→ client C (Subscription::TailFollow{strip_ansi:false}) → type=0x01 tail.data CBOR
```

encoding キャッシュは strip_ansi 真偽で 2 個分ける（同じ subscription を持つ複数 client
で再 encode を回避）。

### 3.3 backpressure

```
[broadcast 時]
   ↓
[client.queued_bytes.fetch_add(N) > cap?]
   ├ yes → error{kind="backpressure.disconnect"} を当該 client に送り
   │       shutdown(Both)、ClientHandle を mark for removal
   └ no  → 通常 enqueue
```

queued_bytes は `Arc<AtomicUsize>`、ClientHandle drop 時に減算。

## 4. 信頼境界・認証

- **同 UID** が信頼境界 ([[DR-0008]] §7)
- socket perm 0600 + parent dir 0700 を `sys/socket.rs` で enforce
- `HYOUI_LOCK_TOKEN` env を handshake.request の `token` field で提示、daemon が holder 照合
- 暗号化なし（同 UID 領域、別 UID は perm で完全遮断）
- TCP / WebSocket transport を追加する v0.2.0+ では別途 token-based auth or TLS を DR 化

## 5. ポータビリティ

hyoui は POSIX 風 PTY/シグナル/Unix ドメインソケットを前提とした PTY 寄生ツール。
動作確認/サポート方針は以下:

| 区分 | OS | 状況 |
|---|---|---|
| Tier 1 (CI で常時検証) | Linux x86_64 / aarch64、macOS Intel / Apple Silicon | release blocking、`cargo test` 通過必須 |
| Tier 2 (互換性は意図、未自動検証) | WSL2 (Linux 互換 kernel) | best-effort、kawaz 手元で sanity 確認 |
| 非サポート | WSL1、Solaris/illumos、各種 BSD、Cygwin/MSYS、Windows native | 動かないか別 issue 化が必要 |

OS 機能に対する依存と既知のポータビリティギャップ:

- **`forkpty(3)` + `login_tty(3)` は POSIX ではなく BSD 拡張**。Linux (glibc/musl)、
  macOS、各種 BSD には存在する。Solaris/illumos には存在しない (`posix_openpt` +
  手動 controlling terminal setup の自作 wrapper が必要)。本プロジェクトは Solaris
  系を非サポートとする ([[DR-0003]] 補強)。
- **`waitpid(WCONTINUED)` は WSL1 で未実装** (`WCONTINUED` flag が ENOSYS 相当で
  失敗するケース)。WSL2 = 通常 Linux kernel なので問題なし。
- **`IUTF8` (termios input flag) は Linux/Android/Apple のみ**。それ以外の OS では
  cfg ガードで no-op。R5-M9 参照。
- **`SO_PEERCRED` (Linux) と `getpeereid(3)` (BSD/macOS) は名前と取得方法が違う**。
  defense-in-depth の uid 一致 assert を入れる場合は OS 別 path が必要 (R5-M11)。
- **`CLOCK_MONOTONIC` の suspend 中挙動は OS 依存**。Linux/macOS は止まる、FreeBSD
  は進む。`wait` の timeout は OS の挙動に依存する (R5-M21)。
- **`pipe2(O_CLOEXEC)` は Linux/FreeBSD のみ**。macOS は `pipe(2)` + `fcntl(F_SETFD,
  FD_CLOEXEC)` が必要。`nix::unistd::pipe` の将来の API 変更で挙動が変わる可能性が
  あるため、CLOEXEC 状態の暗黙依存を避けて明示制御することを推奨 (R5-M4)。

新規 OS 対応は DR-0003 を起点に検討する。

## 6. リリース管理

- VERSION file 1 つ + Cargo.toml workspace.package.version を `pkf run bump-version` で同期
- `main` への push が release.yml の trigger（VERSION 変更を検知）
- workflow が tag + GH Release を自動作成（[release-flow-awareness](../../) ルール参照）
- v0.1.0 = 2026-05-27 release 済、208 tests pass

## 7. 関連文書

| カテゴリ | 場所 | 内容 |
|---|---|---|
| 思想 | [[DR-0005]] | hyoui の方向性（外側自動操作主軸、TUI multiplexer ではない） |
| CLI 全体仕様 | [[DR-0006]] | 動作モデル、自動操作 API、排他制御 |
| MVP scope | [[DR-0007]] | v0.1.0 / v0.2.0 / v0.3.0+ の段階リリース |
| protocol | [[DR-0008]] | wire format、cap flags、transport 抽象 |
| jobcontrol | [[DR-0001]] | bg/fg 2 軸設計、SIGTSTP/SIGCONT 精密制御 |
| 言語選定 | [[DR-0003]] | Rust 一本化、forkpty + login_tty |
| CLI 形 | [[DR-0004]] | subcommand 採用判断 |
| naming | [[DR-0002]] | プロジェクト名 "hyoui" |
| 開発ログ | `docs/journal/` | 実装の経緯、ハマり所と解決策 |
| PoC 知見 | `docs/findings/` | PTY / signal / socket / scrollback / ANSI strip 等の検証結果 |
| 未実装 | `docs/ROADMAP.md` | 将来検討項目 |
