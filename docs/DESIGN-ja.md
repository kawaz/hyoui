# hyoui Design

> [English](./DESIGN.md) | 日本語

**現実装** の説明（v0.1.x 系 + [[DR-0013]] Phase A/B 反映後）。設計判断の背景・経緯は
`docs/decisions/` の DR を参照。本ドキュメントは「いま動いているもの」をドメインと
アーキテクチャの 2 軸で記す。

> [[DR-0013]] (2026-05-27) で daemon に screen emulator (vt100 crate ベース) を
> 導入し、wait / snapshot / dump / lock / input family が state-based 基盤に乗った。
> 本 DESIGN は state-based 仕様を正本として記述する。

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
- socket 配置: `$XDG_RUNTIME_DIR/hyoui/<session>.sock` を優先し、利用できなければ `${XDG_STATE_HOME:-$HOME/.local/state}/hyoui/<session>.sock`。`$TMPDIR` は参照せず、完成 path を platform の `sun_path` 上限に対して検査する
- dir mode 0700 / sock mode 0600（同 UID 信頼境界）

## 2. アーキテクチャ

### 2.1 crate 構成

```
crates/
  hyoui/            # library crate (= 全コア機能)
    src/
      lib.rs        # re-export
      cli.rs        # CLI parser + 各 subcommand 定義
      daemon/       # daemon (= session 1 つを抱える server)
        mod.rs
        config.rs   # session config (socket path, scrollback size, screen sizes)
        session.rs  # Session::serve = multi-attach + broadcast + control plane
        screen/     # vt100 ScreenState wrapper (DR-0013)
          mod.rs
          state.rs       # VirtualScreen (vt100::Parser を抱える正本)
          input_log.rs   # primary 用 bounded ring (resize replay)
          snapshot.rs    # 構造化 snapshot wrapper (CBOR 圧縮)
          redraw.rs      # attach 時の初期 redraw
          health.rs      # screen state health 判定
        control.rs  # control message dispatcher
        broadcast.rs # writer pump + backpressure + ClientHandle
        accept.rs   # handshake worker pool
        wait.rs     # state polling 補助 (= snapshot 発火 trigger / poll interval)
        tail.rs     # tail subscription
        lock.rs     # SessionState + leader cascade
        pty.rs      # child lifecycle
        record.rs   # tty I/O timeline 録画 (DR-0016)
      client/       # client (= daemon に attach する側)
        mod.rs
        attach.rs   # ClientConnection (handshake + raw I/O + detach prefix + raw bytes 送信)
      protocol/     # wire protocol
        mod.rs
        frame.rs    # u32 size + u8 type + body の framing
        caps.rs     # capability negotiation (MVP_CAPS, intersect)
        messages/   # CBOR control message types (handshake, lock, tail, screen.dump,
                    #   screen.snapshot, ...)
        transports/ # Transport trait + UnixStreamTransport
      scrollback.rs # byte-base ring buffer (= tail の since/last_bytes 用、
                    #   timestamp filter / 受信時刻順、DR-0013 §8 Update で責務分離)
      strip.rs      # ANSI escape sequence strip (= tail --strip 用、wait は state 経由で escape 不在)
      sys/          # unsafe を集約
        raw.rs      # forkpty / login_tty (子プロセス起動)
        signal.rs   # sigaction 登録、self-pipe
        pty.rs      # PTY abstraction
        socket.rs   # Unix socket bind (perm 0600 / dir 0700)
        clock.rs    # Instant ↔ epoch ms 変換
        poll.rs     # poll(2) wrapper
        wait.rs     # waitpid wrapper
        fd.rs / env.rs / tty.rs / error.rs
  hyoui-cli/        # binary crate (`hyoui` command)
    src/
      main.rs       # entry point、cli.rs の Command を dispatch
      daemonize.rs  # double fork + setsid (--detached)
      socket_path.rs # socket dir resolver (XDG runtime / state fallback)
      input_handlers.rs # input family の subcommand handler
      wait_core.rs  # state-based wait polling (= snapshot 発火 + cells → text 構築)
      completion.rs # shell completion 生成
```

daemon module の責務分割は [[DR-0009]]、screen 配下は [[DR-0013]] が正本。

`#![forbid(unsafe_code)]` は `hyoui-cli` 全体に、`hyoui` lib では `sys/` 配下の
whitelist (`raw.rs` / `signal.rs` / `env.rs` / `procstate.rs`、justfile の
`lint-unsafe` gate が正本) に `unsafe` を封じ込め（残部は nix 安全 API のみ）。
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
       screen.dump.request / screen.dump.response / screen.snapshot.request /
       screen.snapshot.response / status.query / status.response / error / kill
```

- **wire 外枠 (size + type + body) は永久固定**。breaking change は別 socket path で fork
- 制御メッセージは CBOR map で **未知 field は ignore**、cap flags で「相手が話せるか」交渉
- v0.1.x cap 集合: `["data", "lock", "tail-v1", "screen-dump-v1", "state-snapshot-v1"]`
- wait は **state-based** (= 専用 cap / kind なし、CLI 側 `hyoui-cli/src/wait_core.rs` が
  `screen.snapshot.request` を polling して visible cells から text を組み立て regex match)。
  旧 `wait.request` / `wait.result` kind と `wait-l0` cap、`wait.*` error code は
  [[DR-0006]] §9 + [[DR-0013]] §9 への移行で wire / 実装ともに削除済

### 2.3 daemon (`crates/hyoui/src/daemon/`)

`Session::serve` がメインループ。[[DR-0009]] で 9 module に責務分割済 (= `session.rs`
は orchestrator、`pty.rs` / `accept.rs` / `broadcast.rs` / `control.rs` / `lock.rs` /
`wait.rs` / `tail.rs` / `screen/`)。責務:

- **PTY 管理** (`pty.rs`): master fd を `set_nonblocking(true)`、read で raw bytes を取り出す
- **screen state 正本管理** (`screen/`、[[DR-0013]] Phase A/B):
  - 子 PTY bytes を `vt100::Parser::process` に feed (= byte broadcast 前段で正本化)
  - `VirtualScreen` wrapper が cell grid / cursor / mode / alt screen 切替 / scrollback
    (= rows-base ring) を保持
  - attach handshake 時に `state_formatted()` + alt mode prepend で **redraw bytes** を
    1 frame 送信 (= claude TUI 等の alt screen 常駐アプリの観戦が綺麗に再現される)
  - primary buffer 用 **input bytes log** (= bounded ring、default 1 MiB) で resize replay
  - DEC sync update (`?2026h`) hook + 5s stalled sequence reset (= health check)
- **client 管理** (`accept.rs`): socket accept、handshake (cap negotiation + mode + token 検証)、
  `ClientHandle` 群を保持
- **broadcast** (`broadcast.rs`): master → 各 client、subscription (Raw / TailFollow) に応じて
  encoding を分岐、strip_ansi の真偽でキャッシュを 2 個分け再 encode を回避
- **multiplex**: 各 client → master (rw のみ書き込み許可、ro は silently drop)
- **leader 管理** (`lock.rs`): rw 新 client に leader 不在時のみ自動委譲、leader detach 時は
  次の rw に cascade
- **lock state machine** (`lock.rs`): `SessionState { lock_holder, lock_token }`、
  token + holder 一致で release
- **scrollback** (`scrollback.rs`、byte-base): `Scrollback::new(config.scrollback_bytes)` を
  所有、master read 直後に push。tail コマンド (= `--since` / `--last-bytes`) 専用層
- **state-based wait 補助** (`wait.rs`): master bytes 着信を trigger にして snapshot 発火 /
  poll interval 算出。L0 wait protocol (= `wait.request`/`wait.result` kind) は廃止済、
  実体は CLI 側 (`hyoui-cli/src/wait_core.rs`) の polling
- **structured snapshot / dump** ([[DR-0013]] §9): `screen.dump.request` / `screen.snapshot.request`
  の handler、`screen/snapshot.rs` の CBOR 圧縮 wrapper を経由
- **backpressure** (`broadcast.rs`): `Arc<AtomicUsize> queued_bytes` で byte 単位 cap、
  超過時 `backpressure.disconnect` を送って当該 client を `shutdown(Both)` で drop

子プロセス起動は forkpty + login_tty ([[DR-0003]])。`posix_spawn` は controlling terminal を
取れないため不採用。

### 2.4 client (`crates/hyoui/src/client/attach.rs`)

`ClientConnection::run` がメインループ。責務:

- handshake.request 送信 (caps / mode / token / exclusive / detach-others)
- handshake.response 受信、`session_id` / `client_id` / `leader` / `mode` を確定
- attach handshake 直後に daemon から送られる **redraw bytes frame** ([[DR-0013]] §4) を
  stdout に書き出すだけで detach 時の画面を完全復元
- stdin → frame writer (`type=0x00 raw data`)
- frame reader → stdout
- **Ctrl+Z ガード state machine** ([[DR-0029]] §2): tty stdin で 2 発ごとに子へ
  Ctrl+Z を 1 発届け、余った 1 発が `ctrlz_guard_delay` 後に **client 自身を suspend**
  する (= 外側 shell に戻り、`fg` で同じ接続に復帰。接続は畳まない)。prefix キーは
  持たず、子には hyoui 由来の escape を一切足さない
- 1-shot CLI (`input` / `screen dump` / `screen snapshot` / `tail` / `wait` /
  `lock` / `kill` / `list` / `status`) 用に `recv_frame()` / `recv_control(buffer_raw_data)` /
  `send_raw_bytes()` を提供

### 2.5 byte-base scrollback (`crates/hyoui/src/scrollback.rs`)

```rust
struct OutputChunk { timestamp: Instant, bytes: Vec<u8> }
VecDeque<OutputChunk>  // ring buffer
last_evicted_ts: Option<Instant>
```

- size 上限超過で古い chunk から pop_front、`last_evicted_ts` 更新
- `--since DUR` は内部フィルタ、`last_evicted_ts >= since_start` なら不完全
- `--since-strict` で不完全を非 0 exit に
- default 4 MiB（claude / TUI 主用途想定）
- **用途**: `hyoui tail` 専用 (= timestamp filter / 受信時刻順)。`since_ms` / `--since-strict` /
  `--last-bytes` の byte-base 意味論を保つために維持
- vt100 内蔵 ring (= cell rows-base、§2.8 参照) とは責務分離 ([[DR-0013]] §8 Update)

### 2.6 rows-base screen state (`crates/hyoui/src/daemon/screen/`)

[[DR-0013]] Phase A/B で導入された **vt100 ScreenState wrapper** が daemon 側の正本。

- `state.rs::VirtualScreen` が `vt100::Parser` (+ `Screen`) を抱える
- 子 PTY bytes は **必ず Parser 経由**で state に反映 (= 生 byte の直接 broadcast はしない)
- API 提供: cell grid / cursor / mode flags (= alt screen / app_keypad / bracketed_paste 等) /
  scrollback offset / window_size / `state_formatted()`
- attach handshake 時に `state_formatted()` + alt mode prepend → 1 frame の redraw bytes として送出
- `input_log.rs`: primary buffer 用 bounded ring (= default 1 MiB)、resize 時に新 Parser を
  作り直して replay (= vt100 の `set_size` truncate-only 制約への補完策)
- `snapshot.rs`: 構造化 state を CBOR で送る圧縮 wrapper (= sparse cells / Color variant 整数化 /
  attribute bit pack)

### 2.7 strip (`crates/hyoui/src/strip.rs`)

ANSI escape sequence (CSI / OSC / DCS / single char) state machine による strip。

- **現用途**: `hyoui tail --strip` (= byte stream を grep / script で扱う時の前処理)
- 旧用途 (= wait の `--text` / `--pattern` 前処理) は state-based 移行で不要に。
  wait は cell 化後の text を見るので escape は元から不在 ([[DR-0006]] §9.1)
- 装飾除去と改行変換は **別レイヤ** ([[DR-0006]] §11.1 で確定)
- 装飾除去は ANSI escape のみ、BEL / BS / TAB / LF / CR は残す
- 改行変換は別 flag (`--newline-convert=preserve|lf|crlf`) で tail 個別指定

### 2.8 sys モジュール

unsafe を whitelist 4 ファイルに封じ込め: `sys/raw.rs` (forkpty / login_tty /
TIOCSWINSZ)、`sys/signal.rs` (sigaction / self-pipe)、`sys/env.rs` (環境変数の
非スレッドセーフ操作)、`sys/procstate.rs` (proc_pidinfo / procfs による子状態直読み)。`sys/socket.rs` で perm 0600 / dir 0700 enforce、
`sys/poll.rs` で poll(2) を type-safe に wrap、`sys/clock.rs` で Instant ↔ epoch ms。

### 2.9 Record (`crates/hyoui/src/daemon/record.rs`)

tty I/O timeline の永続録画
([DR-0016](./decisions/DR-0016-tty-io-record.md))。daemon は byte stream の正本
（broadcast 点で全 in/out byte を既に見ている）なので、録画は client broadcast
経路とは独立した **daemon 内の I/O sink** として配線される。

- `RecordRegistry` が session ごとの active record を保持。`record start/stop/list`
  control message が生成 / drain / 列挙する
- hot path は **bounded queue** に event を push し、専用の **writer task** が
  ファイルへ drain する。queue を bounded にすることで、遅いディスクが PTY
  read/write loop を止めたり観測している timing を歪めたりしないようにする
  (DR-0016 の「観測対象を歪めない」invariant)
- `RecordEvent` は in/out bytes、reject / write-error、lifecycle (start/stop、
  SIGTSTP/SIGCONT)、monotonic な `seq` を運ぶ
- format: `jsonl` (1 event 1 行、timestamp + lifecycle event つき。診断 timeline
  format) と `raw` (単一方向の生 byte stream、timestamp なし。export 専用、
  `--both` 不可)
- cap: `record-v1` optional capability で gate し、旧 client も動作継続する
- **secret redaction の state machine (`redact-after-prompt`) は Phase 5 に積み残し。**
  interim は正直化済 (DR-0016 §6a): default は `record-all`（stdin を素通し記録、
  loud warning つき）、`never-record-stdin` は stdin 由来 event を sink に配信しない
  実実装、`redact-after-prompt` は parse 段 / daemon 段の双方で reject される。
  `InSecretRedacted` / `push_in_secret_redacted` は Phase 5 まで dead code。

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
   ↑↓ master fd read → vt100::Parser::process → state 反映
   ↑↓ master fd write
[forkpty master FD]
   ↑↓ PTY
[child process]
```

control plane (lock / resize / signal / handshake / screen.dump / screen.snapshot / ...)
は同じ socket を type=0x01 CBOR frame で multiplex。

### 3.1.1 attach handshake の redraw 復元 ([[DR-0013]] §4 Phase A)

```
[client] handshake.request (caps, mode, token)
   ↓
[daemon] handshake.response (session_id, client_id, caps 確定)
   ↓
[daemon] alt mode 復元 sequence (= ?1049h 等) を prepend
   + VirtualScreen::state_formatted()  // = grid から ANSI 再構築
   + 末尾に cursor 位置の明示再描画 \x1b[<y>;<x>H
   ↓ 1 frame (type=0x00) として送出
[client] stdout に流すだけで detach 時の画面が完全復元
```

claude TUI 等の **alt screen 常駐アプリの観戦が綺麗に再現される** のはこの redraw 経路に
よる。子 PTY 側に再描画を要求しないので、子から見て attach は完全透過。

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
- v0.1.0 = 2026-05-27 release 済、208 tests pass。以降は [[DR-0013]] Phase A/B 反映に
  伴い incremental に minor bump。バージョン区切りで scope を切る運用は廃止 (= `docs/ROADMAP.md`
  が正本、[[DR-0007]] Update 参照)

## 7. 関連文書

| カテゴリ | 場所 | 内容 |
|---|---|---|
| 思想 | [[DR-0005]] | hyoui の方向性（外側自動操作主軸、TUI multiplexer ではない） |
| CLI 全体仕様 | [[DR-0006]] | 動作モデル、自動操作 API、排他制御 (§8-§11 state-based) |
| MVP scope | [[DR-0007]] | 段階リリース戦略 (version 区切りは廃止、ROADMAP が正本) |
| protocol | [[DR-0008]] | wire format、cap flags、transport 抽象 |
| screen emulator | [[DR-0013]] | daemon = screen state 正本、attach 復元、state-based 基盤 |
| session 分割 | [[DR-0009]] | daemon/session.rs の 9 module 化 |
| jobcontrol | [[DR-0001]] | bg/fg 2 軸設計、SIGTSTP/SIGCONT 精密制御 |
| 言語選定 | [[DR-0003]] | Rust 一本化、forkpty + login_tty |
| CLI 形 | [[DR-0004]] | subcommand 採用判断 |
| naming | [[DR-0002]] | プロジェクト名 "hyoui" |
| 開発ログ | `docs/journal/` | 実装の経緯、ハマり所と解決策 |
| PoC 知見 | `docs/findings/` | PTY / signal / socket / scrollback / ANSI strip 等の検証結果 |
| 未実装 | `docs/ROADMAP.md` | 将来検討項目 |
