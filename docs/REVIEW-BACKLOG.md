# hyoui レビューバックログ (= 全体レビューの集約)

ラウンド毎の指摘を集約。各ラウンドの先頭で読み込み、dedup の対象に含める。
対応済の項目は `[done]` を付ける。

## 保管位置と参照

- **canonical**: 本ファイル `docs/REVIEW-BACKLOG.md` (= リポ内、永続化)
- **互換 symlink**: `/tmp/itumono-backlog-hyoui.md` → 本ファイル (= `itumono-full-review`
  / `itumono-nonstop` スキルが `/tmp/itumono-backlog-{repo}.md` 規約で参照するため)
- スキル本体 (`claude-rules-personal/itumono-skills`) の規約改修は別 PR 推奨
  (他リポへ波及する変更のため、本リポ単独で先行)

## 来歴

- **Round 4** は前セッション (b368f29e、2026-05-27 01:40〜02:09 JST) で
  8 personas + Codex + Gemini Pro 並列レビューとして実施。集約結果を `/tmp`
  に書き戻そうとした際に `Prompt is too long` で Write が落ち、backlog が
  空のままセッション終了。本セッション (c7988b6b) で csa の thinking ログから
  集約内容を抽出し、再構築した。Codex は jj リポを git として認識できず失敗、
  Gemini Pro は RATE_LIMIT_EXCEEDED で結果取れず。
- **Round 5** は本セッションで 8 ペルソナ (SRE / Kernel / Formal / Audit / Perf
  / POSIX / Sales / Classic) 並列レビュー → dedup 後 95 件集約 → CRITICAL/HIGH
  をバッチで消化。R5-FRM-C1 (Session::into_parts ManuallyDrop) は誤指摘
  (= v0.1.6 で Option<SessionInner> 化済) として除外。
- **2026-05-27**: 本ファイルを `/tmp/itumono-backlog-hyoui.md` から `docs/REVIEW-BACKLOG.md`
  に移管 (= リポ内永続化)。`/tmp` 側は symlink で互換維持。

## Round 4 (2026-05-27 全体レビュー — 8 personas)

ペルソナ: Architect / DR-docs / Test 戦略 / v0.2.0 Roadmap / 新人 (UX) /
Wild debugger / Competitive 分析 / Rust API 設計

### CRITICAL — 全件 [done] (2026-05-27、main = 5fe284dd、CI green)

- [done] **R4-C1** README 刷新 (= commit 240ab8d8、新人/DR-docs/Competitive)
- [done] **R4-C2** DR-0007 re-scope (= commit d5f4b1ff、DR-docs/Architect)
- [done] **R4-C3** spawn_handshake_worker で slow-loris DoS 対処 (= commit 6e7039c7、Wild debugger)
- [done] **R4-C4** static Mutex で signal test serialize (= commit cf2bbe50、Test 戦略)
- [done] **R4-C5** docs/DESIGN, ROADMAP, CHANGELOG 新設 (= commit eee8816c、DR-docs/Roadmap)
- [done] **R4-C6** WaitMatchOptions::default() を doc に合わせる (= commit 8bd84681、Rust API)
- [done] **R4-C7** Mode::Ro の LockAcquire を reject (= commit c2885058、Wild debugger)
- [done] **R4-C8** Instant+Duration overflow を checked_add に + nix PollTimeout bug 回避 (= commit 23b7c9b1、Wild debugger)
- [done] **R4-C9** 自己 LockAcquire idempotent (= commit 5fe284dd、Wild debugger)

### HIGH

- [ ] **R4-H1** `hyoui kill --help` が機能しない (= subcommand help 未配線)
  - 出典: 新人 / UX
- [ ] **R4-H2** error message に next-action hint なし (= 「次にどうすべきか」が読めない)
  - 出典: 新人 / UX
- [ ] **R4-H3** `wait.text` の chunk boundary で needle miss (= 跨ぐ位置で match 取れない)
  - 出典: Wild debugger
- [done] **R4-H4** `Session` に `Drop` がない → test panic 時 orphan child process 残留
  - 出典: Test 戦略 / Architect
  - 実施: 2026-05-27 / 初版で `Session::Drop` + `Session::into_parts` (unsafe) を追加 (commit 5d0eedadc1d7)
  - 追加対応: 2026-05-27 / `Session` を `{ config, inner: Option<SessionInner> }` に書き換え、`serve` で `inner.take()` 消費に変更。`into_parts` 撤去で `daemon/` 全体の unsafe 数 0 達成。lint:unsafe whitelist から `daemon/session.rs` を削除
- [ ] **R4-H5** timing tight な test threshold で CI flaky (= 5ms 系の threshold 群)
  - 出典: Test 戦略
- [deferred → DR起票後] **R4-H6** `session.rs` 3879 行責務集約 (= PTY / control / writer / backpressure / lock 全部入り)
  - 出典: Architect / 大規模 refactor のため別 DR を起票してから着手 (Round 5 検討)
- [deferred → DR起票後] **R4-H7** `Transport` abstraction が daemon に届いていない (= UnixStream 前提のコードが daemon 側に散在)
  - 出典: Architect / DR-0008 改訂で Transport 境界を再定義してから着手
- [ ] **R4-H8** `token` field の `Debug` derive 漏れ (= security: ログに token が漏れる)
  - 出典: Rust API
- [ ] **R4-H9** 全 enum に `#[non_exhaustive]` がない (= public API の前方互換が壊れる)
  - 出典: Rust API
- [deferred → v0.2.0] **R4-H10** wait queue 未実装 (= v0.2.0 で必須機能)
  - 出典: Roadmap / docs/ROADMAP.md の v0.2.0 セクション参照
- [done] **R4-H11** README に tmux send-keys / Pexpect 比較 (= R4-C1 で対応済、README に比較表)
  - 出典: Competitive
- [deferred → v0.2.0+] **R4-H12** snapshot 機能不在で TUI 自動化主軸の説得力が weaker
  - 出典: Competitive / docs/ROADMAP.md 参照
- [ ] **R4-H13** `Error` enum の `&'static str` sub-discriminator が弱い (= 構造化エラーになりきれていない)
  - 出典: Rust API
- [ ] **R4-H14** `child_actually_exited` で `Stopped` / `Continued` 未区別 → SIGTSTP 子で busy-wait 化リスク (= Round 3 D8 で部分対処だが再 review 必要)
  - 出典: Wild debugger

### MEDIUM

- [done] **R4-M1** `Session::run` (Phase 8 legacy) と `Session::serve` (Phase 9) の duplicate (= run 撤去)
  - 出典: Architect
  - 実施: 2026-05-27 / 撤去関数: `Session::run`, `Session::accept_handshake_once`, `do_handshake`, `relay_loop`, `frame_send_outcome`, helper `spawn_daemon_thread`
  - 撤去 test: `accept_handshake_once_completes`, `run_exits_when_client_sends_kill`, `run_exits_when_client_sends_detach`, `run_exits_when_client_disconnects`, `run_propagates_child_exit_code`, `run_handshake_token_mismatch_rejected`
  - LOC 削減: session.rs -534、合計 -532
  - 残り 276 件 test 全 pass、3 回連続安定
  - journal: docs/journal/2026-05-27-r4-m1-legacy-removal.md
- [x] **R4-M2** `handle_control_message` 311 行の単一 match + cap check が分散
  - 出典: Architect
  - 実施: 2026-05-27 / DR-0009 Phase B で解消
  - dispatcher 化: 311 行単一 match → 36 行薄い dispatcher + 10 個の handler fn (`handle_kill` / `handle_signal` / `handle_resize` / `handle_lock_acquire` / `handle_lock_release` / `handle_tail_request_dispatch` / `handle_wait_request_dispatch` / `handle_status_query` / `handle_detach_target` / `reject_unexpected_kind`)
  - cap / mode helper 集約: `ensure_cap` / `ensure_rw_mode` / `ensure_not_ro` / `ensure_leader` で cap check / mode check 分散を解消 (handler 入口の 1 行に)
  - module 分離: `crates/hyoui/src/daemon/control.rs` 新設 (626 行)、session.rs から -560 LOC
- [ ] **R4-M3** `Session::serve` cleanup が Drop でない panic safety 欠如
  - 出典: Architect
- [ ] **R4-M4** detach key と bash readline (`Ctrl-A`) 衝突警告が docs に無い
  - 出典: 新人 / UX
- [ ] **R4-M5** duration format の bare 数字 reject 時に hint が出ない
  - 出典: 新人 / UX
- [ ] **R4-M6** `hyoui run --help` の option 順序が一貫しない
  - 出典: 新人 / UX
- [ ] **R4-M7** session id 自動採番ルールの docs 不在
  - 出典: DR-docs
- [ ] **R4-M8** DR-0007 の `--name` vs `--session` 命名ズレ (R4-C2 の派生)
  - 出典: DR-docs
- [ ] **R4-M9** error code naming が flat (= `protocol.malformed`、`backpressure.disconnect` 等の階層化が不徹底)
  - 出典: DR-docs
- [ ] **R4-M10** `HYOUI_NAME` nest 起動検知未実装
  - 出典: DR-docs
- [ ] **R4-M11** DR-0008 に error code 一覧が未追記
  - 出典: DR-docs
- [ ] **R4-M12** `parse_duration_ms` overflow path のテスト不足 (= u64 wrap 等)
  - 出典: Test 戦略
- [ ] **R4-M13** regex DoS / `size_limit` 超過 の test 不在
  - 出典: Test 戦略
- [ ] **R4-M14** `hyoui-cli` の `main.rs` / `daemonize.rs` に test ゼロ
  - 出典: Test 戦略
- [ ] **R4-M15** `Resize` clamp が silent (= 上限超えを silently truncate)
  - 出典: Wild debugger
- [ ] **R4-M16** `process_detach_prefix` が literal `Ctrl-A` を飲み込む (= prefix 直後に取消したい場面でユーザ入力失う)
  - 出典: Wild debugger
- [ ] **R4-M17** `instant_to_epoch_ms` の clock jump race
  - 出典: Wild debugger
- [ ] **R4-M18** struct field が全 `pub` (= builder / invariant 不在で外部から壊せる)
  - 出典: Rust API
- [ ] **R4-M19** `Transport::split` の `Send + 'static` 強制が embed 利用を縛る
  - 出典: Rust API
- [ ] **R4-M20** `cli.rs` 2200+ 行手書き parser (= clap への migration 余地、ただし設計判断は別 DR で)
  - 出典: Rust API
- [ ] **R4-M21** `serve` 機能の v0.2.0 前倒し検討
  - 出典: Competitive
- [ ] **R4-M22** record / replay 機能の不在 (= 競合との差別化機会)
  - 出典: Competitive
- [ ] **R4-M23** Python / Node bindings の不在
  - 出典: Competitive
- [ ] **R4-M24** `wait --child-exit` / `--regex-on-screen` 機能不在
  - 出典: Competitive
- [ ] **R4-M25** packaging / Installation 計画欠落 (= brew tap 以外の経路)
  - 出典: Competitive / Roadmap
- [ ] **R4-M26** silently drop の debugability (= backpressure / disconnect ログ不足)
  - 出典: Roadmap
- [ ] **R4-M27** tail chunk / ANSI / ChildExited の正規化規約不在
  - 出典: Roadmap
- [ ] **R4-M28** default 値 (backpressure 8 MiB、queue cap 等) の measurement 未実施
  - 出典: Roadmap
- [ ] **R4-M29** multi-platform (linux / macOS / WSL) サポート明文化不在
  - 出典: Roadmap
- [ ] **R4-M30** breaking change の累積 (= migration guide 不在で v0.1.x → v0.2.0 移行困難)
  - 出典: Roadmap

### LOW / Info

- [ ] **R4-L1** `observer.rs` の dead surface (= 使われていない API)
- [ ] **R4-L2** `compute_wait_poll_timeout` の perf (= 計算重複)
- [ ] **R4-L3** journal の stale 注記 (= 旧 wait exit code 等)
- [ ] **R4-L4** `protocol.malformed` の例が実装に不在
- [ ] **R4-L5** shallow assertion 多用 (= `assert!(result.is_ok())` 系)
- [ ] **R4-L6** test がファイルレベルで偏在 (= `sys/raw.rs` に test なし)
- [ ] **R4-L7** `parse_unit` のエラー文言混乱
- [ ] **R4-L8** `enqueue_for_client` の check-and-add race (= Round 3 L4 でコメント済、実装は据置)
- [ ] **R4-L9** `generate_lock_token` の panic 文書化不足
- [ ] **R4-L10** screen 型 vs tmux 型 architecture の選択 (= 設計判断、保留)
- [ ] **R4-L11** PoC disclaimer が乗り換え検討を抑制
- [ ] **R4-L12** regex 依存 (= `regex` crate の必要性検討)
- [ ] **R4-L13** `idle=0` path のテスト不足
- [ ] **R4-L14** `compute_wait_poll_timeout` と `strip_ansi` の重複

## Round 5 (2026-05-27 全体レビュー — 8 ペルソナ)

ペルソナ: SRE / Kernel / Formal / Audit / Perf / POSIX / Sales / Classic

> dedup ルール:
> - R5-FRM-C1 (Session::into_parts ManuallyDrop) は **誤指摘として除外** (= v0.1.6 で Option<SessionInner> 化済、daemon/ 全体の unsafe 数 0)
> - 複数ペルソナが指摘した項目は 1 件に merge し「出典: X+Y+Z」と併記
> - 既存 R4-* と重複/補強する項目は末尾の dedup セクションでまとめる (新 ID は振らない)
>
> 全 raw 件数 = 約 144 件 (SRE 20 + Sales 21 + Perf 19 + Formal 18 + Kernel 14 + POSIX 19 + Audit 20 + Classic 13)

### CRITICAL

- [ ] **R5-C1** CBOR decode に recursion limit 無し → 認証前 remote daemon crash
  - 出典: Audit (R5-AUD-C1) / 該当: `crates/hyoui/src/protocol/messages/mod.rs:156`, `daemon/accept.rs:179`
  - 攻撃面: handshake は token 検証**前**に decode、socket 到達可能な同 UID プロセスが認証無しで daemon abort 可能 (panic=abort と相乗)
  - 提案: `ciborium::de::Deserializer::with_recursion_limit(32)` で wrap、または handshake frame size を 64 KiB 別 cap に絞る
- [ ] **R5-C2** `session_id` path traversal で任意 0700 dir の socket file unlink/上書き
  - 出典: Audit (R5-AUD-C2) / 該当: `crates/hyoui-cli/src/socket_path.rs:66`
  - 攻撃面: 同 UID 攻撃者が `session_id="../../.ssh/control"` を渡すと `UnixSock::listen` の `unlink(&path)` が `~/.ssh/control` を吹き飛ばす
  - 提案: `[A-Za-z0-9._-]{1,64}` whitelist で CLI 入口 + library API 双方で validate。R5-AUD-M4 (handshake response 経由の ANSI injection) も同時解決
- [ ] **R5-C3** master PTY への client write が EAGAIN で即 DropClient → 子の slow-reader 経由 client DoS
  - 出典: Kernel (R5-KER-C1) / 該当: `daemon/control.rs:105`, `sys/fd.rs:30`
  - 状況: master は nonblock、`FdExt::write_all` は EAGAIN を retry せず即 Error 返し、`handle_client_frame` は DropClient。Linux PTY buffer 4-8 KiB に対し client が 16 KiB paste で silent disconnect
  - 提案: write loop で `poll(master, POLLOUT)` 待ちか、master 向け bounded per-target write queue を 1 本足す
- [ ] **R5-C4** wire protocol が生 signal number (u8) を送る → cross-OS 不整合
  - 出典: POSIX (R5-POSIX-C1) / 該当: `protocol/messages/control.rs:25-26`, `lifecycle.rs:31-33`, DR-0008 §protocol
  - 状況: POSIX.1-2008 は signal 値を規定しない。SIGUSR1/USR2/CHLD/STOP/TSTP は Linux と macOS で値が異なる。v0.2.0 serve gateway (HTTP/WebSocket remote) で cross-OS client が叩いた瞬間に破綻
  - 提案: wire を **signal 名 string** に変更 (`"signal": "SIGTERM"`)、daemon 側で OS native 値に解決。**v0.2.0 serve gateway 着手前の breaking change で実施**

### HIGH

- [deferred → DR-0011 起票後 Phase A 実装] **R5-H1** daemon の観測手段ゼロ (= ログ・metrics・trace 完全不在)
  - 出典: SRE+Audit (R5-SRE-C1, R5-AUD-I3) / 該当: `daemon/*.rs` 全 module、`daemonize.rs:75` (`Stdio::null()`)
  - 状況: detached daemon は stderr を /dev/null。handshake reject / backpressure / writer dead / panic stack 全部消失。本番事故 post-mortem 物理的に不可能
  - 提案: `tracing` + `tracing-subscriber` 導入、`$XDG_STATE_HOME/hyoui/<session>.log` (rotate 付き) リダイレクト、`HYOUI_LOG=info/debug` で制御。R4-M26 (silently drop debugability) と統合
  - **v0.2.0 serve gateway 前に必須**
  - 解消方針: 2026-05-27 ws `r5-dr-obs` で **DR-0011 (observability 戦略)** 起票。tracing crate 採用、log/metrics/trace 戦略・Phase A-C 分割を確定。実装着手は Phase A から別 task
- [done] **R5-H2** `MAX_PENDING_HANDSHAKES = MAX_CLIENTS_PER_DAEMON = 64` 合算頭打ちで legit client 締め出し
  - 出典: SRE+Audit (R5-SRE-C2, R5-AUD-H6) / 該当: `daemon/accept.rs:58, :431`
  - 状況: 64 client attached 状態で新規接続は無条件 reject、handshake worker すら spawn しない。CI 並列 `hyoui status` で N 個目以降が静かに消える
  - 提案: `MAX_PENDING_HANDSHAKES = MAX_CLIENTS / 4 = 16` の別定数、accept 段の閾値は AND 条件 (`clients < MAX_CLIENTS && pending < MAX_PENDING`)
  - 実施: 2026-05-27 r5-staleskt ws / `MAX_PENDING_HANDSHAKES = 16` に分離、session.rs serve_loop accept 段を OR 条件 (`clients >= MAX_CLIENTS || pending >= MAX_PENDING` で reject) に変更。test `accept_loop_pending_cap_independent_from_clients_cap` 追加 (const block で MAX_PENDING < MAX_CLIENTS を compile-time 保証 + 実機 handshake 完了確認)
- [done] **R5-H3** daemon panic/SIGKILL で socket unlink されず stale socket 確定、`hyoui list` で live と区別不能
  - 出典: SRE (R5-SRE-C3) / 該当: `sys/socket.rs:144`, `hyoui-cli/src/main.rs:368-399`
  - 提案: `hyoui list` で各 socket に `connect` best-effort 投げて stale マーカー表示、`--prune` で unlink オプション
  - 実施: 2026-05-27 r5-staleskt ws / `ListConfig::prune_stale` CLI flag 追加 (`--prune-stale`)、`probe_socket_liveness` で connect best-effort 死活判定、出力 format を `<session>\t<live|stale>\t<path>` に拡張、stale 検出時は stderr に warn + hint、`--prune-stale` 指定時 unlink で除去。test: `list_marks_stale_socket_when_no_ping_response` / `list_prune_stale_removes_dead_sockets` / `list_without_prune_keeps_stale_sockets` / CLI parser tests 3 件
- [ ] **R5-H4** ROADMAP の v0.2.0+ subcommand 増殖 = scope creep の初期症状 (DR-0005 retrograde)
  - 出典: Classic (R5-CLS-H1)
  - 状況: v0.1.0 5 個 → v0.2.0 で 15 個 → v0.3.0+ で 23 個 (tmux と同 density)、DR-0005 で却下した「TUI multiplexer 路線」と外形が見分けつかなくなる
  - 提案: DR-0010 (re-scope) を v0.2.0 着手前に起票。`send/keys/paste` → `input` 統合、`lock/unlock/tx` → `lock acquire|release|tx` nested、`snapshot` v0.3.0 押し下げ、`detach` を `attach --detach-others` に集約 → v0.2.0 = 10 個で頭打ち
- [ ] **R5-H5** `serve` gateway は別 crate ではなく**別 repo / 別 binary** に切り出すべき
  - 出典: Classic (R5-CLS-H2)
  - 状況: HTTP/TLS/WebSocket dep (hyper/tokio/rustls) が `hyoui-cli` の supply chain に視認される。`websocketd hyoui attach $SESS` で 90% 代替可能
  - 提案: `kawaz/hyoui-serve` を別 repo として開始、core repo の Cargo.toml を `nix + serde + ciborium + regex` の lean な状態で v0.x 全期間維持
- [ ] **R5-H6** SIGCHLD self-pipe 不在 = 子 state transition 検出が polling 経路にのみ依存
  - 出典: Kernel+Perf (R5-KER-H3, R5-PERF-H3) / 該当: `daemon/pty.rs::ChildLifecycle::poll`, ALIVE_RETRY_INTERVAL=5ms
  - 状況: SIGCONT 検出 latency が最大 500ms (Ctrl-Z → fg の半秒 freeze)、master EOF 後 5ms busy spin
  - 提案: `sys/signal.rs` の self-pipe infra で `SIGCHLD` を register、serve_loop の poll_fds に追加。一行で latency ms オーダー、busy spin 撤廃。forkpty 子側で `SIGCHLD: SIG_DFL` リセット必要 (= R5-KER-M4)
- [ ] **R5-H7** `Session::Drop` / `finalize_child` の signal が child PID 単独 → 孫プロセス orphan 化
  - 出典: Kernel+POSIX (R5-KER-H2, R5-POSIX-M2) / 該当: `daemon/session.rs:263, 282, :627`
  - 状況: `kill(child, SIGTERM)` は session leader 1 つだけ。`hyoui run /bin/sh -c 'sleep 9999 &'` で sleep orphan 化、init/launchd に re-parent。tmux/screen/abduco は全て pgid 単位
  - 提案: `kill(Pid::from_raw(-child.as_raw()), SIGTERM)` (= killpg 相当) に変更、または `tcgetpgrp(master_fd)` で foreground pgid 経由
- [done] **R5-H8** runbook ゼロ (= 障害対応手順が言語化されてない)
  - 出典: SRE (R5-SRE-H4) / 該当: `docs/runbooks/` 自体存在せず
  - 解消 (2026-05-27): `docs/runbooks/` 新設 + 6 runbook + INDEX (stale-socket-detection / backpressure-disconnect / handshake-cap-rejection / daemon-crash-recovery / child-orphan-detection / deployment-checklist)
  - 提案: `docs/runbooks/` 新設、最低 5 件: stale socket / 100% CPU / orphan daemon / backpressure 頻発 / EIO 連発。各 runbook は (症状)(切り分け)(原因)(復旧)(再発防止) 5 節構造
- [done] **R5-H9** broadcast の `Vec::clone()` × N clients amplification (= O(N × frame_size) memory bandwidth)
  - 出典: Perf (R5-PERF-H1) / 該当: `daemon/broadcast.rs:240-263, :300-318`
  - 状況: 64 clients × 8 KiB chunk × 1 MB/s PTY = 64 MB/s memcpy
  - 提案: `Arc<Vec<u8>>` で共有、enqueue は atomic incr のみ、`writer_pump` で `write_all(&arc[..])` zero-copy。`bytes` crate 不要
  - 解消 (2026-05-27): `SharedBytes = Arc<Vec<u8>>` 型を導入。`writer_tx: Sender<Vec<u8>>` → `Sender<SharedBytes>` に変更。broadcast 系 (`broadcast_master_bytes` / `broadcast_bytes` / `broadcast_control`) は 1 度だけ `Arc::new` した payload を `Arc::clone` で N clients に配布、writer_pump は `&payload[..]` を `write_all` に渡す。64 clients × 8 KiB chunk = 512 KiB → 64 × 8 byte (Arc pointer) = 512 byte (1000× 削減)。全 test pass / clippy clean / lint:unsafe clean
- [done] **R5-H10** handshake `caps`/`token` 長さ無制限 → 1 GiB 級 transient peak が認証前に成立
  - 出典: Audit+Kernel (R5-AUD-H1, R5-KER-M5) / 該当: `protocol/messages/handshake.rs:30,38`, `daemon/accept.rs:212-213`, `frame.rs:10` (MAX_FRAME_SIZE=16MiB)
  - 提案: handshake CBOR decode 直後に `caps.len() <= 64 && each.len() <= 64 && token.len() <= 256` 検証、または `MAX_HANDSHAKE_FRAME_SIZE = 64 KiB` を別 const で
- [done] **R5-H11** `generate_lock_token` の `expect` panic = 攻撃で daemon abort 可能 (panic=abort と相乗)
  - 出典: Audit (R5-AUD-H2) / 該当: `daemon/lock.rs:59-62`
  - 状況: lock cap 持つ rw client が高頻度 lock.acquire で fd 枯渇 (EMFILE) を誘発、`/dev/urandom` open 失敗で daemon abort
  - 提案: `Result<String, std::io::Error>` 化、失敗時は `LockResponse{Denied}` + `error{code: "lock.token-gen-failed"}` で session 維持
- [done] **R5-H12** core dump 抑止無し = メモリ内 lock token / `HYOUI_LOCK_TOKEN` 環境変数が disk 漏洩
  - 出典: Audit (R5-AUD-H5) / 該当: `daemonize.rs:113-191`
  - 状況: SIGSEGV/SIGABRT (R5-H11 と相乗) で `/cores` / systemd-coredump に token plain text 流出
  - 提案: daemon 起動直後 (setsid 後) に `setrlimit(RLIMIT_CORE, 0)`
- [done] **R5-H13** CI で `cargo audit` / `cargo deny` 未実施 = yanked / RUSTSEC advisory 検知不能
  - 出典: Audit (R5-AUD-H3) / 該当: `.github/workflows/ci.yml`
  - 提案: `cargo install --locked cargo-audit cargo-deny` + `cargo audit` + `cargo deny check`、`deny.toml` で advisories/bans/licenses/sources 最小構成
  - 対処: ci.yml に audit job (rustsec/audit-check@v2.0.0) + deny job (EmbarkStudios/cargo-deny-action@v2) を追加、deny.toml 新規作成 (zl ci: cargo-audit job を追加)
- [done] **R5-H14** release artifact に SLSA attestation / SHA256SUMS 無し = MITM 検知不能
  - 出典: Audit (R5-AUD-H4) / 該当: `.github/workflows/release.yml`
  - 提案: `actions/attest-build-provenance@v2` + `sha256sum *.tar.gz > SHA256SUMS` を release.yml に追加
  - 対処: publish-release job に SHA256SUMS 生成 + actions/attest-build-provenance@v2 で SLSA Provenance v1 を生成、id-token/attestations permission 追加 (u ci(release): SHA256SUMS + SLSA attestation を生成)
- [ ] **R5-H15** README にロゴ/GIF/asciinema cast 一切無し = 「動いてる感」ゼロ
  - 出典: Sales (R5-SAL-C1)
  - 状況: 競合 (tmux/zellij/ttyd/asciinema) は必ず screenshot/GIF/asciinema embed を持つ。「外側 driven」差別化が文字だけでは伝わらない
  - 提案: asciinema cast embed か vhs (charm) / terminalizer での GIF を README 冒頭に必須配置
- [ ] **R5-H16** tagline が抽象的 (= "transparent PTY wrapper / possesses") で 5 秒説明として弱い
  - 出典: Sales (R5-SAL-C2)
  - 提案: 用途 × 差別化 1 行 (例: "Drive `claude`, REPLs, TUIs from the outside via CLI — no prefix keys, no in-band escape"). 「憑依」語源は **About the name** に後退、冒頭はユースケース取る
- [ ] **R5-H17** Primary install path (`brew install kawaz/tap/hyoui`) が "(planned)" のまま = 初見ユーザ離脱
  - 出典: Sales (R5-SAL-C3) / R4-M25 とは別観点で営業 priority 最上位
  - 提案: (a) brew tap 最優先で立てる (homebrew-tap-deploy-key.md パターン)、(b) tap 完成まで README Installation 順序を「Pre-built binaries → cargo install → Homebrew (Planned)」に並べ替え、(c) crates.io 公開検討
- [done] **R5-H18** `ClientHandle` に `Drop` 実装無し → `Session::serve` panic で writer_thread leak
  - 出典: Formal (R5-FRM-H2) / 該当: `daemon/broadcast.rs:46`
  - 状況: panic unwinding で `Vec<ClientHandle>` drop されるが writer_tx drop + writer_thread join なし。R4-M3 と一致だが panic safety invariant の axis
  - 提案: `ClientHandle::Drop` で writer_tx drop + reader shutdown + writer_thread join (bounded 200ms) 一括化
  - 対応: ws `r5-drop-inv` で `impl Drop for ClientHandle` を追加 (reader.shutdown(Both) → writer_tx を closed dummy へ mem::replace → writer_thread.take().map(|h| h.join()))。session.rs の 3 つの cleanup site (Session::serve 末尾 drain / overflow_ids drop cascade / generic drop cascade) で重複していた drop(writer_tx)/shutdown/join 3 行を `drop(ch)` に一本化。test: `client_handle_drop_closes_writer_channel` (本物の writer_pump thread を spawn → recv block 中に drop → 500ms 以内に完了)、`client_handle_drop_idempotent_with_no_writer_thread` (writer_thread が None でも panic しないこと)
- [done] **R5-H19** `WAIT_ACCUMULATED_LIMIT` trim 後の invariant (needle は最新 1MiB 内) が doc/debug_assert 不在
  - 出典: Formal (R5-FRM-C3) / 該当: `daemon/wait.rs:235-238`
  - 状況: 実害は無いが、将来「ms 単位の time-bound trim」で書き換えると silent に壊れる
  - 提案: trim 直後に `debug_assert!(w.accumulated.len() <= WAIT_ACCUMULATED_LIMIT)` + doc-comment で「needle 検出範囲は末尾 1 MiB」を明記
  - 対応: ws `r5-drop-inv` で `update_waits_on_master_bytes` の doc-comment に `## invariant: needle 検出範囲 (R5-H19 / R5-FRM-C3)` セクションを追加し、trim が末尾 limit バイトを保持すること・daemon が limit を超えた古い byte の needle を検出しないこと・将来 trim ロジックを変える際の注意点を明記。trim 直後に `debug_assert!(w.accumulated.len() <= WAIT_ACCUMULATED_LIMIT)` を追加。test: `update_waits_keeps_accumulated_within_limit` (3.25 MiB を投入し各 iter で debug_assert + 最終的に accumulated.len() == WAIT_ACCUMULATED_LIMIT に saturate)
- [ ] **R5-H20** README で対象ユーザ (target persona) 明示無し
  - 出典: Sales (R5-SAL-H1)
  - 提案: "Who is this for?" セクション (3-5 行) を `What it does` の直前/直後に追加。claude users / DevOps / remote work hobbyist の persona ラベル × 1 行説明
- [ ] **R5-H21** 競合比較表が tmux/screen/Pexpect の 3 つだけ = 弾切れ
  - 出典: Sales (R5-SAL-H3)
  - 提案: 比較表を 2 段化「人間が中で生活するツール (tmux/screen/zellij = 競合じゃない)」「外側から制御するツール (Pexpect/expect/abduco/dtach/shpool/ttyd = 本当の競合領域)」
- [ ] **R5-H22** Status disclaimer が "MVP" 一語 = production readiness 不明
  - 出典: Sales (R5-SAL-H4) / R4-L11 と関連
  - 提案: 動作確認済 platform / test 数 (276 tests pass) / breaking change policy / "eat your own dogfood" 一行を Status セクションに記載

### MEDIUM

- [ ] **R5-M1** 全 timeout / limit が hardcode、運用 tuning 不能
  - 出典: SRE (R5-SRE-H2) / 該当: HANDSHAKE_TIMEOUT, MAX_PENDING_HANDSHAKES, MAX_CLIENTS, DRAIN_BUDGET, STOPPED_POLL, ALIVE_RETRY, WAIT_ACCUMULATED, PATTERN_MAX_LEN, REGEX_SIZE_LIMIT, client_buffer_bytes, scrollback_bytes, kernel backlog
  - 提案: `DaemonConfig` に一括追加、`HYOUI_TUNABLES=<key>=<val>,...` の 1 env で渡す (flag 爆発回避)
- [ ] **R5-M2** disconnect/handshake_fail event を broadcast/status に出す経路無し
  - 出典: SRE (R5-SRE-H3) / 該当: `daemon/broadcast.rs:125`, `session.rs:540-565`
  - 提案: `status.response` に `disconnect_counters` 追加、event broadcast channel (`event-v1`) を v0.2.0 sink 概念と合流
- [ ] **R5-M3** graceful shutdown が Drop 経由のみ、SIGTERM を daemon に送る正規 path 無し
  - 出典: SRE (R5-SRE-H5) / 該当: `daemon/session.rs:155-217`
  - 状況: `kill -TERM <daemon-pid>` で daemon は SIGTERM default action で即 abort、子 PTY が orphan に。systemd/launchd standard shutdown sequence に乗らない
  - 提案: `install_selfpipe_for(Signal::SIGTERM)` を daemon child 初期化で install、serve_loop poll_fds に追加
- [ ] **R5-M4** CLOEXEC fragility: nix `pipe()` default が CLOEXEC 非設定であることに暗黙依存
  - 出典: Kernel (R5-KER-H1) / 該当: `hyoui-cli/src/daemonize.rs:41-67`
  - 状況: nix が将来 `pipe2(O_CLOEXEC)` 化したら `run --detached` が 100% hang。`Stdio::null()` で原因不明化
  - 提案: `pipe2(O_CLOEXEC)` 明示生成 → 子側で `dup2(wr, 3)` + `fcntl(3, F_SETFD, 0)` で明示的 inheritance (`Command::pre_exec` 経由)
- [ ] **R5-M5** socket bind の `UmaskGuard` 方式は将来 multithread 化で破綻
  - 出典: Kernel+Audit (R5-KER-H4, R5-AUD-I5) / 該当: `sys/socket.rs:5-11, 91-115`
  - 提案: 「daemon serve 開始前 listen 1 回限定」を `debug_assert!(thread_count == 1)` で強制、または `openat` ベースに移行
- [ ] **R5-M6** `enqueue_for_client` の atomic ordering が AcqRel × 3 = ARM macOS で実コスト
  - 出典: Perf+Formal (R5-PERF-H2, R5-FRM-C2) / 該当: `daemon/broadcast.rs:97-110`
  - 提案: serve_loop single-threaded 前提なら load/fetch_add → `Relaxed`、fetch_sub → `Release`。または `compare_exchange_weak` loop / Mutex で機械検証可能化、または `debug_assert!` で thread_id 一致を強制
- [ ] **R5-M7** `update_waits_on_master_bytes` の計算量 (= `extend_from_slice` + `drain` で O(acc_len)、`windows().any()` で O(acc × needle))
  - 出典: Perf (R5-PERF-H4) / 該当: `daemon/wait.rs:215-247`
  - 提案: `accumulated` を `VecDeque<u8>` 化 (trim amortized O(1))、text predicate を `memchr::memmem::find` で SIMD 4-16x、または「前回 scan 終端 + needle.len()-1 から再 scan」
- [ ] **R5-M8** `PendingHandshake._worker` JoinHandle detached → MAX_PENDING_HANDSHAKES invariant 瞬間的に破る
  - 出典: Formal+SRE (R5-FRM-H1, R5-SRE-M5) / 該当: `daemon/accept.rs:101-109, :353`
  - 状況: 実用上 microsecond window だが ghost-state 不整合。worker panic も静かに drop
  - 提案: `Option<JoinHandle>` 化、drop 時に bounded join (100ms)、worker entry を `catch_unwind` で wrap + log
- [done] **R5-M9** termios `cfmakeraw` 後に `IUTF8` (Linux) を明示 set してない
  - 出典: Kernel (R5-KER-M1) / 該当: `sys/tty.rs:79`
  - 提案: `enter_raw` の `raw_t.input_flags |= IUTF8` 1 行で日本語 BS 崩れ回避
  - 解消: `enter_raw` に `cfg(any(linux, android, apple))` ガード付きで `raw_t.input_flags |= IUTF8` を追加。回帰防止に「enter_raw 後の termios で IUTF8 が立っている」アサート test を追加。
- [ ] **R5-M10** `install_ignore(SIGPIPE)` が serve start path で呼ばれていない → broadcast 中の EPIPE で daemon abort リスク
  - 出典: Kernel (R5-KER-M2) / 該当: `sys/signal.rs:36-48`, `daemon::session::Session::serve`
  - 提案: `Session::start` か `serve` 冒頭で `install_ignore(SIGPIPE)` 呼ぶ
- [ ] **R5-M11** SO_PEERCRED / getpeereid による uid 一致 assert 不在 (defense-in-depth)
  - 出典: Kernel (R5-KER-M3) / 該当: `sys/socket.rs:64-87`
  - 提案: `UnixSock::accept` 戻り値で OS 別に `SO_PEERCRED` (linux) / `getpeereid` (BSD/macOS) → euid 不一致は accept 段階で close
- [ ] **R5-M12** `forkpty + execvp` child path で signal mask / disposition reset 無し
  - 出典: Kernel (R5-KER-M4) / 該当: `sys/raw.rs:99-108`
  - 状況: 親の signal handler が exec 直前まで子側でも動く未定義動作の余地。R5-H6 (SIGCHLD self-pipe) と必須セット
  - 提案: `forkpty_then_exec` child 冒頭で `sigprocmask(SIG_SETMASK, &empty_set, NULL)` + 全 signum `sigaction(SIG_DFL)`
- [ ] **R5-M13** socket parent dir 検査が symlink-follow (TOCTOU + symlink すり替え)
  - 出典: POSIX+Audit (R5-POSIX-H3, R5-AUD-M5) / 該当: `sys/socket.rs:66-87`, `socket_path.rs:95-125`
  - 提案: `openat(O_NOFOLLOW | O_DIRECTORY)` で dir fd 握ってから `fstat`、または最低 `symlink_metadata` で symlink reject
- [done] **R5-M14** forkpty (BSD 拡張) / login_tty / WCONTINUED (WSL1) の portability gap 明文化不在
  - 出典: POSIX (R5-POSIX-H1, R5-POSIX-H2) / R4-M29 と統合
  - 提案: README + `docs/DESIGN-ja.md` §portability で「Linux/macOS 範囲、WSL2 のみ、Solaris/illumos 別途検証要」明記、DR-0003 補強
  - 解消: `docs/DESIGN-ja.md` / `docs/DESIGN.md` に §5 「ポータビリティ」/「Portability」を新設、Tier 1/2/非サポート OS 一覧 + forkpty/login_tty・WCONTINUED・IUTF8・SO_PEERCRED・CLOCK_MONOTONIC・pipe2(O_CLOEXEC) の OS 差を一覧化。後段 §は番号 +1 リナンバ。
- [ ] **R5-M15** Scrollback `since` / `last_n_bytes` の O(N) / O(K²) 計算量
  - 出典: Perf (R5-PERF-M1) / 該当: `crates/hyoui/src/scrollback.rs:117-130, :140-156`
  - 提案: `since` を `partition_point` で二分探索 → O(log K)、`last_n_bytes` を逆算 1-pass O(n) に
- [ ] **R5-M16** `compute_wait_poll_timeout` で `Idle{ms: u64::MAX}` 単独 pending で serve_loop が無限 block
  - 出典: Formal (R5-FRM-M2) / 該当: `daemon/wait.rs:301-310`
  - 提案: overflow した Idle は `check_wait_timeouts` 側で `WaitOutcome::Error` で明示 reject (safety + liveness 両立)
- [ ] **R5-M17** `handle_lock_release` の token 比較が `&str::eq` (short-circuit) = constant_time_eq 非対称
  - 出典: Formal (R5-FRM-H5) / 該当: `daemon/control.rs:513-514`
  - 状況: 同 UID 信頼境界では strict 不要だが、handshake で constant_time 採用なら lock release も揃えるのが invariant 的に望ましい
  - 提案: `security_token_eq` helper を `lock.rs` に置き、module-level invariant「token 比較は全て constant-time」を文書化
- [ ] **R5-M18** `handle_*` handler の `broadcast_control` 戻り値 `overflow_ids` が捨てられている
  - 出典: Formal (R5-FRM-M4) / 該当: `daemon/control.rs::handle_lock_acquire` L491-498 等
  - 状況: 急激な burst (= 子 PTY 10 MB/s 出力) で「broadcast 失敗で当該 client を切るべきなのに切らない」が次 iteration まで遅延
  - 提案: handler 戻り値に `overflow_ids: Vec<u64>` 含めるか、serve_loop 側に集約する pattern を強制
- [ ] **R5-M19** `MAX_FRAME_SIZE = 16 MiB` は control frame に過大、用途別 cap 分け
  - 出典: Audit+Kernel (R5-AUD-I2, R5-KER-M5) / 該当: `protocol/frame.rs:10`
  - 提案: control frame `MAX_CBOR_FRAME_SIZE = 64 KiB`、raw data frame だけ 16 MiB に分離。R5-H10 解決過程に組み込み可
- [ ] **R5-M20** `XDG_RUNTIME_DIR` の mode 0700 + 所有者検証無し、stale dir で TMPDIR fallback が silent
  - 出典: POSIX (R5-POSIX-M1) / 該当: `hyoui-cli/src/socket_path.rs:73-89`
  - 提案: `pick_socket_dir` 内で mode 0700 + 所有者検証、NG なら fatal error
- [done] **R5-M21** suspend 中の CLOCK_MONOTONIC 挙動 (Linux/macOS: 止まる、FreeBSD: 進む) が docs と実装で曖昧
  - 出典: POSIX (R5-POSIX-M4) / 該当: `sys/clock.rs:10-13`
  - 提案: docs/DESIGN-ja に「wait の timeout は OS の CLOCK_MONOTONIC 挙動に準拠 (suspend 取扱は OS 依存)」明記
  - 解消: `sys/clock.rs::clock_monotonic` の rustdoc に「Suspend behavior (OS-dependent)」節を追加 (Linux/macOS は止まる、FreeBSD は進む、`wait` timeout への影響を明示)。DESIGN §5 Portability から相互参照。元の説明 (macOS が suspend-inclusive 等) は誤りだったので訂正。
- [ ] **R5-M22** shell completion 未実装 (kawaz/* CLI 慣習との乖離)
  - 出典: POSIX (R5-POSIX-M6) / 該当: completions/ や `*.plugin.zsh` 不在
  - 提案: `hyoui completion {bash,zsh,fish,elvish,nushell}` を 1 個追加。ROADMAP v0.2.0 と同期
- [ ] **R5-M23** `next_client_id` が monotonic increasing = ID 推測容易
  - 出典: Audit (R5-AUD-M1) / 該当: `daemon/accept.rs:312`
  - 提案: 内部 dense index は維持、wire 上の `client_id` を `rand u64` で 1 回振る。v0.2.0+ で `--target-client-id` 入れる前に
- [ ] **R5-M24** `auto_session_id` が PID 単独 → 衝突 + 攻撃予測可能
  - 出典: Audit+POSIX (R5-AUD-M2, R5-POSIX-I3) / 該当: `socket_path.rs:21-23`
  - 提案: `format!("run-{}-{}", pid, rand_suffix)` (8 hex chars)
- [ ] **R5-M25** `DaemonConfig.expected_token` の `Debug` derive で全文表示しうる (R4-H8 延長)
  - 出典: Audit (R5-AUD-M6) / 該当: `daemon/config.rs:43`
  - 提案: `expected_token` を `secrecy::SecretString` 風 newtype に包むか、`DaemonConfig` Debug を手書きで redact
- [ ] **R5-M26** `HYOUI_DETACH_PREFIX` env が printable byte (例: 'a') を受理 → ロックアウト DoS
  - 出典: Audit (R5-AUD-M7) / 該当: `client/attach.rs:94-127`
  - 提案: env validate で control character (0x20 未満 + 0x7f) のみ受理、printable byte は reject
- [ ] **R5-M27** `serve_loop` 単一関数 292 行 = 1-page rule 違反、責務 7 つが直書き
  - 出典: Classic (R5-CLS-H3) / R4-H6 とは別 axis (= 内部 loop の脱線)
  - 提案: `poll_once` / `dispatch_event` / `reap_dropped_clients` を抽出、本体 50 行に。R4-H6 と独立で着手可能
- [ ] **R5-M28** Error 型階層 3 種 (`sys::Error` / `ControlMessageError` / `ErrorCode`) が flatten 不足
  - 出典: Classic (R5-CLS-H4) / R4-H13 補強
  - 提案: `Error::Protocol(ControlMessageError)` + `Error::Remote(ErrorCode)` で flatten、library 利用者 (Python/Node bindings) 視点で「`hyoui::Error` 1 個」に統一
- [ ] **R5-M29** `cli.rs` 2484 行手書き parser = 自前 ad-hoc parser 膨張、R4-M20 格上げ
  - 出典: Classic (R5-CLS-M1) / R4-M20 補強
  - 提案: DR-0010 「CLI parser を clap (builder API) に移行」を v0.2.0 着手前に起票、800 行に削減見込み
- [done] **R5-M30** `observer.rs` (dead surface) 削除一択、保留する理由なし
  - 出典: Classic (R5-CLS-M2) / R4-L1 格上げ
  - 提案: R5 のうちに jj 1 change で削除、R4-L1 を done に
  - 解消: `crates/hyoui/src/observer.rs` を削除、`lib.rs` の `pub mod observer;` も除去。外部参照 0 件を grep で確認済み。R4-L1 も合わせて clean up。
- [done] **R5-M31** ライセンス badge / CI / release badge を README に置く
  - 出典: Sales (R5-SAL-M1, R5-SAL-M2)
  - 提案: README L1 直下に CI status / latest release / MIT license / (将来) crates.io / Homebrew badge
  - 解消: README.md / README-ja.md の言語切替リンク直下に CI status / Release / MIT license の 3 badge を追加。crates.io / Homebrew は未公開なので将来追加 (R4 backlog item)。
- [done] **R5-M32** `.github/ISSUE_TEMPLATE/` 不在 = 困った時の聞き先不明
  - 出典: Sales (R5-SAL-M3)
  - 提案: bug_report.md + feature_request.md 最小追加、README に "Questions / Issues" セクション 1 行
  - 解消: `.github/ISSUE_TEMPLATE/{bug_report.md,feature_request.md,config.yml}` を追加 (config.yml で Discussion に誘導)。README.md / README-ja.md に「Questions / Issues」セクションを追加。
- [ ] **R5-M33** エンドユーザ向け MANUAL.md / MANUAL-ja.md 不在 = docs 棲み分け曖昧
  - 出典: Sales (R5-SAL-M4) / docs-structure.md 規約準拠
  - 提案: `docs/MANUAL{-ja}.md` 骨組み追加 (ユースケース recipe 集)、README/DESIGN との棲み分け明示。v0.2.0 API 完成時に本格化
- [ ] **R5-M34** GitHub repo Topics タグ未設定 (SEO)
  - 出典: Sales (R5-SAL-M7)
  - 提案: `pty`/`terminal`/`automation`/`cli`/`rust`/`daemon`/`tmux`/`expect`/`claude`/`repl`/`interactive`/`session-manager`
- [ ] **R5-M35** `RunConfig` / `AttachConfig` 全 field pub = invariant 不在 (R4-M18 補強)
  - 出典: Classic (R5-CLS-M3) / R4-M18 補強
  - 提案: field を `pub(crate)` 閉じ、reader method (unsigned narrowing で invariant 表現) + `RunConfig::new(command)` + 必須 setter

### INFO

- [ ] **R5-I1** criterion / benchmark 完全不在 (R4-M28 measurement 前提として整備必須)
  - 出典: Perf (R5-PERF-INFO-1)
  - 提案: criterion harness 整備、対象 (strip_ansi / Scrollback / update_waits / Frame::encode_to / broadcast_master_bytes)
- [ ] **R5-I2** CPU profiling 体制 (samply / flamegraph) が Taskfile.pkl に無い
  - 出典: Perf (R5-PERF-INFO-2)
  - 提案: `task profile` で `samply record cargo run --release -- ...`、docs/runbooks/ に記載
- [ ] **R5-I3** writer_pump 終了時の queued_bytes 戻し不整合 (sentinel 未設定)
  - 出典: SRE (R5-SRE-M4) / 該当: `daemon/broadcast.rs:284-297`
  - 提案: writer_pump 終了時に `queued_bytes.store(usize::MAX, Release)` sentinel、または rx.iter().fold で正確に戻す
- [ ] **R5-I4** detached daemon `chdir("/")` で初期 cwd が子 PTY に伝わらない
  - 出典: SRE (R5-SRE-M1) / 該当: `daemonize.rs:158`
  - 提案: `daemonize.rs` で chdir("/") 前に `initial_cwd` 取得、子 PTY exec 直前に `chdir(initial_cwd)` 復元
- [ ] **R5-I5** `--detached` ready-pipe 失敗で daemon child leak
  - 出典: SRE (R5-SRE-M2) / 該当: `daemonize.rs:88-102`
  - 提案: ready 読み失敗時 `child.kill()` で cleanup
- [ ] **R5-I6** `finalize_child` の blocking `waitpid` で hang リスク (trap '' TERM な bash)
  - 出典: SRE (R5-SRE-M3) / 該当: `daemon/session.rs:632`
  - 提案: SIGTERM → 500ms poll → SIGKILL → 200ms poll → 諦め (Drop と共通化)
- [ ] **R5-I7** handshake token rotate 不能 (expected_token: Option<String> 単一)
  - 出典: SRE (R5-SRE-M7) / 該当: `daemon/config.rs:43`
  - 提案: `Vec<String>` 化、env `HYOUI_LOCK_TOKEN=primary,fallback` の comma 区切り
- [ ] **R5-I8** listen backlog = 5 で burst connect の SYN drop リスク
  - 出典: SRE (R5-SRE-I2) / 該当: `sys/socket.rs:117`
  - 提案: 64 に上げる (Unix-domain では cost ゼロ)
- [ ] **R5-I9** macOS / Linux 差分整理 (`docs/knowledge/os-syscall-differences.md`) 不在 (R4-M29 と統合)
  - 出典: SRE+Kernel (R5-SRE-I4, R5-KER-I2)
- [ ] **R5-I10** Drop の SIGKILL 後 200ms 諦めが zombie 容認、ログ無し
  - 出典: SRE (R5-SRE-I5) / 該当: `daemon/session.rs:283-296`
  - 提案: R5-H1 のログ基盤上で `tracing::warn!(?child, "drop gave up reap")`
- [ ] **R5-I11** `Frame::encode_to` の中間 buf alloc (writev / 3-piece vectored で 0 alloc / 1 memcpy 化可能)
  - 出典: Perf (R5-PERF-M3, R5-PERF-INFO-4)
- [ ] **R5-I12** `StripAnsiCarry::push` が carry 空でも `input.to_vec()` で 1 alloc/chunk
  - 出典: Perf (R5-PERF-M6) / 提案: `Cow<[u8]>` で borrow / iterator chain
- [ ] **R5-I13** `poll_fds` を毎 iteration `Vec::new` (= 1000 alloc/s for 1ms loop)
  - 出典: Perf (R5-PERF-M2) / 提案: serve_loop 外で保持、loop 先頭で clear + extend
- [ ] **R5-I14** `instant_to_epoch_ms` per-frame 呼び出し (2 syscall/frame)
  - 出典: Perf (R5-PERF-M5) / 提案: serve_loop 上位で 1 回取得、broadcast に渡す
- [ ] **R5-I15** `windows().any()` が std 実装 SIMD 無し
  - 出典: Perf (R5-PERF-INFO-8) / 提案: `memchr::memmem::find` (既 dep) で 4-16x
- [ ] **R5-I16** regex `size_limit` = 64 KB の measurement 不在 (R4-M28 と統合)
  - 出典: Perf (R5-PERF-INFO-3)
- [ ] **R5-I17** writer_pump の blocking `write_all` で queued_bytes 遅延発覚 (8 MiB まで隠れる)
  - 出典: Perf (R5-PERF-INFO-5) / v0.2.0 で adaptive 閾値 / slow-client warning hook 検討
- [ ] **R5-I18** `handshake_worker` 毎 accept で thread spawn (~50 μs/accept)
  - 出典: Perf (R5-PERF-INFO-6) / v0.2.0 で worker pool 検討
- [ ] **R5-I19** `STOPPED_POLL_INTERVAL = 500ms` measurement 無し
  - 出典: Perf (R5-PERF-INFO-7) / R5-H6 (SIGCHLD self-pipe) 解決で adaptive 不要に
- [ ] **R5-I20** `nix_signal_from_signum(0)` の "kill(pid, 0) existence probe" 非対応が wire error で kind 単一 (区別不能)
  - 出典: Formal (R5-FRM-I1)
- [ ] **R5-I21** `ChildState::Stopped/Continued` の monotonicity 違反可能性 (Continued → Stopped 同 iteration 取りこぼし)
  - 出典: Formal (R5-FRM-M1) / 提案: 1 iteration で `waitpid` 連続呼び (drain pattern)
- [ ] **R5-I22** `SessionState` の lock_holder vs lock_token 同期 invariant 表明無し
  - 出典: Formal (R5-FRM-H3) / 提案: `assert_lock_invariant()` を mutation site で呼ぶ
- [ ] **R5-I23** Idle wait の `last_activity = now` 更新が session-global = starvation の可能性
  - 出典: Formal (R5-FRM-H4) / 提案: doc-comment で「session-global master byte event で reset」明記
- [ ] **R5-I24** bounded resource 全集合の積算 invariant (16 × 64 × 1MiB ≈ 1GiB) 文書化無し
  - 出典: Formal (R5-FRM-M5) / R4-M28 と統合
- [ ] **R5-I25** WINCH handler の `compare_exchange` Ordering Relaxed (実害無し、定義上は Acquire/Release pair が portable)
  - 出典: POSIX (R5-POSIX-I1)
- [ ] **R5-I26** SLA / release.yml の actions tag pin → SHA pin 化 (supply chain 強化)
  - 出典: Audit (R5-AUD-I1)
- [ ] **R5-I27** docs に「同 UID 信頼境界」と core dump model を `docs/runbooks/security-model.md` で明示
  - 出典: Audit (R5-AUD-I4)
- [ ] **R5-I28** `IUTF8` 以外 raw mode で `ONLCR` 落ち → CR-only 出力で line break 見えなくなる
  - 出典: Kernel (R5-KER-I3) / 提案: README に "raw mode は ONLCR を落とす" 1 行
- [ ] **R5-I29** Ctrl-A D 一個だけの in-band escape が DR-0005「透明性最優先」と文言整合性 (子に対しては透過、ユーザ視点では 1 個)
  - 出典: Classic (R5-CLS-I5) / R4-M4 / R4-M16 と統合
  - 提案: DR-0005 文言を「子に対して in-band escape 無し」と狭める、または `Ctrl-A D` を廃止して `hyoui detach <session>` (out-of-band only)
- [ ] **R5-I30** `--` の必要性が subcommand 不揃い (Rule of Least Surprise)
  - 出典: Classic (R5-CLS-M5) / 提案: 「複数 positional 取る subcommand は `--` 必須」をルール明文化
- [ ] **R5-I31** launch announcement (HN/Reddit/lobste.rs) 準備不在 (v0.2.0 タイミングで)
  - 出典: Sales (R5-SAL-M6)
- [ ] **R5-I32** Cargo.toml の `keywords` / `categories` 確認 (crates.io 公開時)
  - 出典: Sales (R5-SAL-I4)
- [ ] **R5-I33** SIGINFO (BSD/macOS) 不使用 = 意図的なら docs 化
  - 出典: POSIX (R5-POSIX-I5)
- [ ] **R5-I34** `epoll`/`kqueue` 不使用 (= POSIX `poll(2)` のみ) は **古典派/POSIX 観点で正解** (positive 確認)
  - 出典: POSIX+Classic (R5-POSIX-I6, R5-CLS-I1, R5-CLS-I2, R5-CLS-I3, R5-CLS-I4, R5-CLS-I6, R5-CLS-I7)

### dedup された既存 R4-* との重複/補強

- **R4-M3** (`Session::serve` cleanup が Drop でない panic safety 欠如) → R5-FRM-H2 (= R5-H18) で「ClientHandle 自体に Drop 実装」具体策補強
- **R4-M17** (`instant_to_epoch_ms` clock jump race) → R5-FRM-M6 / R5-AUD-M3 / R5-POSIX-M4 (= R5-M21) で「forensic 不能/suspend 中挙動 OS 依存」を補強
- **R4-M18** (struct field 全 pub) → R5-CLS-M3 (= R5-M35) で「古典派 data hiding + invariant explicit」観点補強
- **R4-M20** (cli.rs 2200 行手書き parser、clap 移行) → R5-CLS-M1 (= R5-M29) で「ad-hoc parser 膨張、DR-0010 起票」格上げ
- **R4-M22** (record/replay 機能) → R5-SAL-H3 で「PR materials / asciinema embed の手動運用は今すぐ可能」補強
- **R4-M25** (packaging / brew tap) → R5-H17 で「営業 priority 最上位 + Installation 順序入れ替え」具体化
- **R4-M26** (silently drop の debugability) → R5-H1 で「symptom 単位 → 構造化ログ基盤」上位レイヤ化
- **R4-M28** (default 値 measurement 未実施) → R5-I1 / R5-I16 / R5-I24 で「criterion harness 整備が前提」具体化
- **R4-M29** (multi-platform 明文化) → R5-M14 / R5-I9 で「forkpty BSD 拡張 / WCONTINUED WSL1 / OS syscall 差異一覧」内容指定
- **R4-H1** (`hyoui kill --help` 未配線) → R5-M29 で「手書き parser のメンテ穴の代表例」補強
- **R4-H4** (Session::Drop) — **完了済 / R5-FRM-C1 は誤指摘として除外**
- **R4-H6** (session.rs 3879 行責務集約) → R5-CLS-H3 (= R5-M27) で「内部 serve_loop も別軸で分割」補強
- **R4-H13** (Error enum &'static str sub-discriminator が弱い) → R5-CLS-H4 (= R5-M28) で「3 階層 Error の flatten」補強
- **R4-L1** (observer.rs dead surface) → R5-CLS-M2 (= R5-M30) で「古典派観点で MEDIUM 格上げ、R5 のうちに削除」

### dedup 後の合計

- CRITICAL: 4 件
- HIGH: 22 件
- MEDIUM: 35 件
- INFO: 34 件
- **合計**: 95 件 (= raw 144 件 → 49 件削減)
- 既存 R4-* に統合: 13 件 (= 新 ID 振らず)

## Round 5 fix loop 着手順序の推奨

### 第 1 波: v0.2.0 serve gateway 公開前に必須 (= CRITICAL + observability/supply chain HIGH)

CRITICAL を Agent 並列 (5-10 ws) で消す:

1. **R5-C1** CBOR recursion limit (1-2h、handshake 経路の cap 化と統合)
2. **R5-C2** `session_id` whitelist validate (1h、CLI + library API 双方)
3. **R5-C3** master write EAGAIN handling (3-5h、bounded write queue 設計判断含む)
4. **R5-C4** signal wire を u8 → name string (= DR 起票必要、protocol breaking なので v0.2.0 着手前必須)

並行で HIGH の「serve gateway 公開前に固めるべき」を:

5. **R5-H1** tracing + structured log 導入 (3-5h)
6. **R5-H10** handshake CBOR length cap (1h)
7. **R5-H11** generate_lock_token Result 化 (1h)
8. **R5-H12** core dump 抑止 (30min)
9. **R5-H13** cargo audit / deny CI 追加 (2h)
10. **R5-H14** SLSA attestation (2h)

### 第 2 波: 営業準備 (= v0.2.0 リリースアナウンス前に)

11. **R5-H17** brew tap 立てる + Installation 順序入れ替え (営業 priority 最上位、半日)
12. **R5-H15** README に asciinema cast / GIF (1-2h)
13. **R5-H16** tagline 磨き直し (30min)
14. **R5-H20** target persona 追加 (30min)
15. **R5-H21** 競合比較表 2 段化 (1h)

### 第 3 波: v0.2.0 着手前の re-scope 判断

16. **R5-H4** DR-0010 (re-scope) 起票 — scope creep を v0.2.0 着手前に固める
17. **R5-H5** serve gateway を別 repo に切り出す判断 (= DR-0007 補強)
18. **R5-H6** SIGCHLD self-pipe (latency ms オーダー化 + busy spin 撤廃の一石二鳥)
19. **R5-H7** killpg 経由 signal 配信 (孫プロセス orphan 化対策)

### 第 4 波: MEDIUM / INFO は v0.2.0 着手と並行

- 計測系 (R5-I1 criterion / R5-I2 samply) を先に整備すれば、Perf 系 (R5-H9, R5-M7, R5-M15) の効果検証が可能になる
- runbook (R5-H8) は incident readiness のため、observability (R5-H1) と同時並行で書ける
- MEDIUM 35 件は単独で取り組まず、関連する R4-* (M3/M17/M18/M20/M22/M25/M26/M28/M29) と束ねて DR 単位で進める

## Round 5 の総合所感

8 ペルソナのレビュー結果から見える hyoui の「設計癖」と Round 6 の必要性判断:

1. **observability の構造的欠落**: SRE / Audit / Formal の 3 視点が独立に「daemon の中で何が起きているか分からない」を CRITICAL/HIGH で指摘。これは Round 4 までで R4-M26 (silently drop debugability) として認識されていたが、Round 5 で「ログ基盤そのものが不在」という上位レイヤ問題として再定義された。v0.2.0 serve gateway は observability 抜きで公開してはいけない。
2. **scope creep の警戒**: Classic (古典派) と Sales が独立に「v0.2.0 ロードマップは tmux density に達している」を警告。DR-0005 で却下した「TUI multiplexer 路線」と外形が見分けつかなくなるリスク。`hyoui send/keys/paste` → `input` 統合のような re-scope DR (DR-0010) を v0.2.0 着手前に書く価値がある。
3. **ghost-state invariant の多発**: Formal が C2/C3/H1/H3/H4/M1/M2/M4 で「型システム / debug_assert で機械検証されていない暗黙不変量」を集中指摘。「single-threaded だから安全」「needle は最新 1MiB 内」等は現コードでは正しいが、将来の改変で silent に壊れる fragile な構造。
4. **POSIX portability の楽観**: 生 signal number wire (= cross-OS で破綻)、forkpty BSD 拡張、WCONTINUED WSL1 差、CLOEXEC default 依存等、「Linux/macOS 同一 OS 前提」が wire protocol レベルで仕込まれている。v0.2.0 serve gateway (= remote 制御) で破綻する設計負債。
5. **supply chain の未整備**: Audit が cargo audit/deny 不在 + SLSA attestation 不在を HIGH で指摘。v0.1.6 まで quality 改善に注力した結果、release pipeline 側の防御が遅れている。

**Round 6 必要性の判断**: Round 4 → Round 5 の差分 (Round 4 = quality / refactor 中心、Round 5 = observability + scope + portability + supply chain) を見ると、Round 5 は **v0.2.0 着手前のチェックポイント** として効果的だった。Round 5 の CRITICAL/HIGH を 70%+ 解消した後で Round 6 を打つのが効率的 (= Round 5 残課題に発見済問題が散らばっている状態でレビュー再実施するのは ROI 低い)。

## Round 3-backlog (2026-05-27 duration + Round1/2 残置統合) — done

commit `e39179f6` で D1-D8 + H1-H7 + L1-L6 全件解消済。詳細は jj log 参照。

## Field findings (= 実機検証由来、cmux-msg 検証セッション 2026-05-27)

出典: `docs/issue/2026-05-27-cmux-msg-experiment-feedback-v020-refresh.md` + `docs/findings/2026-05-27-headless-claude-remote-control-leak.md`

### B1-B8

- [done] **R5-FB1** `hyoui run --until PATTERN` が機能していない (= 期待のパターン後も子が走り続ける)
  - 出典: 実機検証 / 解消 (2026-05-27): `DaemonConfig.until` + `UntilWatcher` (= sliding window matcher with carry buffer) を serve_loop に配線。master byte stream の chunk 境界を跨ぐ needle も検出。match 時に `kill_pgrp(child, SIGTERM)` → `ClientDetachedOrKilled` 経路で finalize
- [done] **R5-FB2** headless mode で stdin EOF が子に伝わらない (= `echo "1+2" | hyoui run -- bc` で hang)
  - 出典: 実機検証 / 解消 (2026-05-27): `StdinEofAction` enum (Detach / SendEof) を `hyoui::client` に新設、`ClientConnection::with_stdin_eof_action(SendEof)` で stdin EOF 時に EOT (0x04) を子 PTY に raw_data 送信。`hyoui-cli/main.rs::run_command` で stdin が TTY 以外の場合 (= pipe / file / heredoc) に SendEof を自動選択
- [ ] **R5-FB3** `hyoui run --detached` 未実装 (= attach --help の RELATED 節に記載あるが本体未実装、v0.2.0 scope に組み込み要)
- [done] **R5-FB4** `run` 直後の wait/status が ENOENT (= socket 作成 race)
  - 出典: 実機検証 / 解消 (2026-05-27): `connect_with_retry()` helper を `hyoui-cli/main.rs` に新設、socket 不存在系 errno (ENOENT / ECONNREFUSED) のみ 100ms × 20 attempts = 2s budget で retry。attach / kill / status / tail / wait の 5 subcommand に適用。認証エラー / protocol error は retry せず即 fail で hint 経路へ
- [done] **R5-FB5** `--socket` 親 dir mode エラー文言が不親切 (= hint 追記で改善)
  - 出典: 実機検証 / 解消 (2026-05-27): `sys/socket.rs::check_parent_dir` の Precondition 文言に next-action hint を追加 (= `$XDG_RUNTIME_DIR` / `$TMPDIR` 推奨、`chmod 700 <parent>` の直し方明示)。`Error::Precondition` の `&'static str` リテラルを更新するだけで ErrorCode 体系は触らず
- [done] **R5-FB6** `hyoui list/kill --help` 取りこぼし (= R4-H1 で list/kill は解消、completion --help だけ残)
  - 完全解消 (2026-05-27): `usage_completion()` を list/kill と同じ pattern (USAGE / OPTIONS / SHELLS / EXAMPLES / RELATED 5 節構造) で再実装。help 配線そのものは parse_completion で既に HelpTopic::Completion ルーティング済
- [done] **R5-FB7** v0.2.0 で `kill` subcommand 去就 (= 2026-05-27 kawaz 確認: アプリ固有問題は `input keys` で対応可、kill は v0.1.x 互換維持で v0.2.0 scope 7 個から除外、DR-0010 据え置き)
- [done] **R5-FB8** leader 死亡時 socket 残骸 (= R5-H3 で list stale 検出 + --prune-stale 実装済、commit `beb7dd05`)
- [ ] **R5-FB2b** `headless_stdin_eof_terminates_child_reading_bc` test を event-based に書き直し
  - 出典: v0.1.12/v0.1.13 CI Linux で flaky (elapsed>10s)、production code 本体 (R5-FB2, commit `a574e792`) は OK、test 設計のみ問題
  - 対処 (一時): `#[ignore]` で skip、v0.1.14 で CI green 化
  - 恒久: daemon が ChildExited を broadcast したら client が即終了する経路を直接観測する event-based test に置換

### Headless Remote Control finding

- [ ] **R5-FB9** `hyoui run --mode=headless -- claude` で起動した child claude が Remote Control を継承 (= スマホ Claude アプリから直接介入可能)
  - 出典: `docs/findings/2026-05-27-headless-claude-remote-control-leak.md`
  - hyoui 自体の責任ではない (= claude code user settings 由来) が、`--child-env` で env override する経路や README 注記が望ましい
  - v0.2.0 で `run --child-env KEY=VAL` 検討候補

## Round 1-2 (2026-05-26 night) — done

Round 1: 5 personas + gemini → critical 10 + warning 5 fix (commit `24e00ead`)
Round 2: recall-biased simplify → 10 件 fix (commit `ad9b85f7`)
