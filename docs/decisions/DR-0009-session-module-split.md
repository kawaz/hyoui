# DR-0009: `daemon/session.rs` の責務分割 — module 化と段階移行

- Status: Active
- Date: 2026-05-27
- Related: [[DR-0005]] (思想), [[DR-0006]] (CLI ground rules), [[DR-0007]] (MVP scope), [[DR-0008]] (protocol)
- Backlog 解消: R4-H6 (session.rs 責務集約)、R4-M2 (handle_control_message 311 行) を本 DR で扱う

## Context

`crates/hyoui/src/daemon/session.rs` は v0.1.4 リリース時点 **4364 行 (非 test 部分 2262 行 / test 部分 2102 行)** に達した。Phase 6→11 を経て PTY 子プロセス管理、socket accept/handshake、broadcast writer pump、frame dispatch、control message handler、lock state machine、wait/tail predicate、子 lifecycle 追跡まで全責務が単一ファイルに集約されている。

v0.2.0 で次の handler を追加する予定:

- `keys` / `send` / `paste` (input 注入系、現在は `TYPE_RAW_DATA` 経由のみ)
- `detach` の `Others` / `All` ターゲット本実装 (現在は `not-implemented` error 返却)
- `status` / `tail` / `wait` の機能拡張
- `lock` / `unlock` / `tx` の wait queue 化
- `completion` / `serve` gateway

これら handler を現状の `handle_control_message` (311 行の単一 `match`) に直接追加すると **5500-6000 行越え** になる見込みで、(a) 認知負荷、(b) handler の追加位置不明確、(c) test 責務範囲が曖昧、(d) cap check / mode check が分散して横断的観点を見落とす、といった問題が深刻化する。

R4 quality round の `R4-H6` (session.rs 責務集約) は「大規模 refactor のため別 DR を起票してから着手」と deferred 扱いだった。本 DR でその「別 DR」を起票する。

### 現状の責務分類 (= 何を分割するか)

`session.rs` を頭から末尾まで読み、項目を責務別に分類:

| 責務カテゴリ | 主要 item | 概算 LOC | 依存 |
|---|---|---|---|
| **Session orchestrator** | `Session` struct、`Session::start` / `serve`、`Session::into_parts`、`impl Drop for Session` | 約 240 行 | 下記全てを呼ぶ最上位 |
| **PTY child lifecycle** | `ChildLifecycle` struct / impl、`ChildState` enum、`ALIVE_RETRY_INTERVAL` / `STOPPED_POLL_INTERVAL` const、`finalize_child` | 約 110 行 | nix waitpid のみ |
| **Socket accept / handshake** | `PendingHandshake`、`AcceptedClient`、`HandshakeStageOk` type alias、`spawn_handshake_worker`、`do_handshake_stage`、`finalize_accepted_client`、`process_pending_handshakes`、`unix_stream_from_owned_fd`、`HANDSHAKE_TIMEOUT` / `MAX_PENDING_HANDSHAKES` const、`constant_time_eq` | 約 290 行 | `UnixSock`、`UnixStreamTransport`、protocol |
| **Broadcast / writer / backpressure** | `ClientHandle` struct、`Subscription` enum、`EnqueueOutcome` enum、`writer_pump`、`enqueue_for_client`、`send_backpressure_error`、`send_control`、`broadcast_bytes`、`broadcast_control`、`broadcast_master_bytes`、`instant_to_epoch_ms`、`MAX_CLIENTS_PER_DAEMON` const | 約 290 行 | protocol Frame、Subscription |
| **Control message handler** | `handle_client_frame`、`handle_control_message` (311 行の単一 match)、`ClientFrameOutcome` enum、`FrameOrError` enum、`handle_detach_target`、`nix_signal_from_signum` | 約 420 行 | 全コンポーネントに副作用、cap check が散在 |
| **Lock state machine** | `SessionState` struct / impl、`generate_lock_token`、`should_assign_leader`、`elevate_next_leader`、leader cascade / lock auto-release (= serve_loop 内 inline) | 約 80 行 (+ serve_loop の cascade 部分) | clients + send_control |
| **Wait predicate** | `PendingWait` struct、`WAIT_ACCUMULATED_LIMIT` / `MAX_WAITS_PER_CLIENT` const、`handle_wait_request`、`update_waits_on_master_bytes`、`compute_wait_poll_timeout`、`check_wait_timeouts` | 約 280 行 | `crate::strip::StripAnsiCarry`、send_control |
| **Tail subscription** | `handle_tail_request`、`Subscription::TailFollow` 経路 (= broadcast_master_bytes 内 inline)、`TailEndReason` 経路 (= Session::serve cleanup 内 inline) | 約 70 行 | `Scrollback`、send_control |
| **Serve loop (event loop)** | `serve_loop`、`RelayOutcome` enum | 約 305 行 | 上記すべてを駆動 |
| **Test module** | `mod tests` | 約 2102 行 | super::* を参照 |

`crate::scrollback`、`crate::strip` (StripAnsiCarry / strip_ansi / normalize_lf)、`crate::sys::Pty` / `UnixSock`、`crate::protocol::*` は既に外部 module として独立済 — 本 DR の分割対象外。

## Decision

**`daemon/session.rs` を以下の module 構成に分割する**。`session.rs` は state 構造体 + `Session::start` / `serve` + 全 module を駆動する `serve_loop` の orchestrator のみを残す。

### 新 module 構成

```text
crates/hyoui/src/daemon/
  mod.rs            # 既存。pub use に新 module の公開 API を追加 (= Session のみ pub、内部は pub(super) で隠す)
  config.rs         # 既存。変更なし
  session.rs        # Session struct + start / serve + serve_loop (orchestrator のみ、目標 600-800 行)
  pty.rs            # ChildLifecycle, ChildState, finalize_child, ALIVE_RETRY_INTERVAL, STOPPED_POLL_INTERVAL
  accept.rs         # PendingHandshake, AcceptedClient, HandshakeStageOk, spawn_handshake_worker,
                    #   do_handshake_stage, finalize_accepted_client, process_pending_handshakes,
                    #   unix_stream_from_owned_fd, HANDSHAKE_TIMEOUT, MAX_PENDING_HANDSHAKES, constant_time_eq
  broadcast.rs      # ClientHandle, Subscription, EnqueueOutcome, writer_pump, enqueue_for_client,
                    #   send_backpressure_error, send_control, broadcast_bytes, broadcast_control,
                    #   broadcast_master_bytes, instant_to_epoch_ms, MAX_CLIENTS_PER_DAEMON
  control.rs        # handle_client_frame, handle_control_message (= kind 別 dispatcher 化を併せて行う),
                    #   ClientFrameOutcome, FrameOrError, handle_detach_target, nix_signal_from_signum
  lock.rs           # SessionState, generate_lock_token, should_assign_leader, elevate_next_leader
  wait.rs           # PendingWait, handle_wait_request, update_waits_on_master_bytes,
                    #   compute_wait_poll_timeout, check_wait_timeouts,
                    #   WAIT_ACCUMULATED_LIMIT, MAX_WAITS_PER_CLIENT
  tail.rs           # handle_tail_request、tail_end_reason 用 helper (= Session::serve の cleanup 用)
```

### 公開 API 規約

- **`pub` (crate 外)**: `Session` のみ (現状維持)。`DaemonConfig` は `config.rs` の現状維持
- **`pub(super)` (daemon module 内に閉じる)**: 上記 module の struct / enum / fn の大半
- **`pub(crate)` 禁止**: daemon の内部実装を `crate::*` に漏らさない。test も super::* で参照する

### module DAG (= 依存方向、tree 構造、循環なし)

```text
                       session.rs (Session, serve_loop)
                       /     |        |         |       \
                   pty.rs  accept.rs broadcast.rs control.rs ...
                              |        ^           |
                              |        |           v
                              +---> broadcast.rs <-+
                                       ^
                              wait.rs / tail.rs / lock.rs
                                       |
                              (broadcast.rs::send_control を呼ぶ)
```

- `broadcast.rs` が hub: `ClientHandle` / `send_control` / `broadcast_*` を提供
- `accept.rs` / `control.rs` / `wait.rs` / `tail.rs` / `lock.rs` は **broadcast.rs に片方向依存**
- `session.rs` は最上位、全 module を import
- 逆参照は禁止 (= `broadcast.rs` から `control.rs` を import しない)

### Shared types の置き場

- `ClientHandle`、`Subscription`、`EnqueueOutcome` → `broadcast.rs` (= 全 module が touch する core type)
- `SessionState` → `lock.rs` (= lock 由来の state のみ保持。将来 wait queue を入れるなら拡張)
- `ClientFrameOutcome`、`FrameOrError`、`RelayOutcome` → frame 処理結果は `control.rs`、loop 終了結果は `session.rs` に置く (= 跨ぐ enum は最上位呼出元側で定義)

> **note**: `daemon/types.rs` 集約案 (= shared struct を 1 file にまとめる) は検討したが、`ClientHandle` を broadcast 以外から書き換える場面が無いので broadcast.rs に置いた方が読み手がたどりやすい。types.rs は採用しない (Rejected alternatives 参照)。

## 移行プラン (Phase A-E、段階分割)

「一度に全部分割すると test 壊しまくる」を避けるため、副作用の少ない部分から段階的に切る。各 Phase は **1 commit (もしくは関心事ごとに 2-3 commit)** とし、Phase 間で必ず `cargo test --workspace` が green になる状態を維持する。

### Phase A: 純関数ヘルパ系の切り出し (PTY / lock / strip-carry 周辺)

**目的**: 副作用無し or 副作用の閉じた純粋ヘルパを先に外に出す。serve_loop の構造は触らない。

- `pty.rs` 新設: `ChildLifecycle`、`ChildState`、`ALIVE_RETRY_INTERVAL`、`STOPPED_POLL_INTERVAL`、`finalize_child` を移動
- `lock.rs` 新設: `SessionState`、`generate_lock_token`、`should_assign_leader`、`elevate_next_leader`、`constant_time_eq` を移動 (`constant_time_eq` は本来 accept で使うが、token 比較系として lock.rs に集約してもよい → Phase B で再配置可)
- `session.rs` 側は `use super::{pty::*, lock::*}` で吸う
- 期待 LOC 削減: **session.rs から約 190 行が移動** (約 4364 → 約 4170 行)
- test の影響: `child_lifecycle_tracks_stopped_continued_transitions` 等を `pty.rs` の `#[cfg(test)] mod tests` に移動 (super::* → super::pty::* に書き直し不要、再 export で吸収)
- breaking change: なし (= 公開 API は `Session` のみ)

### Phase B: control.rs と handle_control_message の dispatcher 化 (R4-M2 解消)

**目的**: 311 行の単一 `match` を解体し、handler を kind 単位で関数化する。同時に cap check / mode check を共通 helper に抽出する。

- `control.rs` 新設: `handle_client_frame`、`handle_control_message`、`ClientFrameOutcome`、`FrameOrError`、`handle_detach_target`、`nix_signal_from_signum` を移動
- `handle_control_message` を以下のように分解:

  ```rust
  fn handle_control_message(...) -> ClientFrameOutcome {
      match msg {
          ControlMessage::Kill(k)         => handle_kill(...),
          ControlMessage::Signal(s)       => handle_signal(...),
          ControlMessage::Resize(r)       => handle_resize(...),
          ControlMessage::LockAcquire(r)  => handle_lock_acquire(...),
          ControlMessage::LockRelease(r)  => handle_lock_release(...),
          ControlMessage::TailRequest(r)  => handle_tail_request_with_cap(...),  // wait/cap check + handle_tail_request 呼び出し
          ControlMessage::WaitRequest(r)  => handle_wait_request_with_cap(...),
          ControlMessage::StatusQuery(_)  => handle_status_query(...),
          ControlMessage::Detach(d)       => handle_detach_target(...),
          // daemon→client only な kind は protocol error
          _                               => reject_unexpected_kind(...),
      }
  }
  ```

- cap check は `require_cap(ch: &ClientHandle, cap: &str) -> Result<(), ()>` のような共通ヘルパに集約
- mode check は `require_mode(ch: &ClientHandle, allowed: &[Mode]) -> Result<(), ()>` で共通化
- 期待 LOC 削減: **handle_control_message は 305 行 → 約 60 行のディスパッチャ + 各 handler 平均 30-50 行**。session.rs から約 420 行が移動
- test の影響: control 系の test は control.rs の test module へ移動。共通 helper の単体 test も追加可
- breaking change: なし

### Phase C: broadcast.rs 切り出し (writer pump + backpressure)

**目的**: ClientHandle + writer thread + queued_bytes 機構を 1 module に集約。

- `broadcast.rs` 新設: `ClientHandle`、`Subscription`、`EnqueueOutcome`、`writer_pump`、`enqueue_for_client`、`send_backpressure_error`、`send_control`、`broadcast_bytes`、`broadcast_control`、`broadcast_master_bytes`、`instant_to_epoch_ms`、`MAX_CLIENTS_PER_DAEMON` を移動
- session.rs / accept.rs / control.rs / wait.rs / tail.rs / lock.rs は `use super::broadcast::*;` で吸う
- 期待 LOC 削減: session.rs から約 290 行が移動
- test の影響: backpressure / queued_bytes 系の test を broadcast.rs に再配置
- breaking change: なし

### Phase D: accept.rs 切り出し (handshake worker pool)

**目的**: handshake 周りの slow-loris 対策ロジック (= worker thread + timeout + token 検証) を 1 module に閉じ込める。

- `accept.rs` 新設: `PendingHandshake`、`AcceptedClient`、`HandshakeStageOk`、`spawn_handshake_worker`、`do_handshake_stage`、`finalize_accepted_client`、`process_pending_handshakes`、`unix_stream_from_owned_fd`、`HANDSHAKE_TIMEOUT`、`MAX_PENDING_HANDSHAKES` を移動
- `constant_time_eq` は Phase A で lock.rs に置いた場合 accept.rs に再配置 (= 用途が handshake token 検証なので accept の方が自然)
- session.rs / control.rs から `use super::accept::*;` で吸う
- 期待 LOC 削減: session.rs から約 290 行が移動
- test の影響: handshake 系の test (token mismatch / timeout / cap intersect) を accept.rs に再配置
- breaking change: なし

### Phase E: wait.rs / tail.rs 切り出し

**目的**: predicate / subscription 系の logic を独立 module に。

- `wait.rs` 新設: `PendingWait`、`handle_wait_request`、`update_waits_on_master_bytes`、`compute_wait_poll_timeout`、`check_wait_timeouts`、`WAIT_ACCUMULATED_LIMIT`、`MAX_WAITS_PER_CLIENT` を移動
- `tail.rs` 新設: `handle_tail_request`、`tail_end_reason_from_outcome` 等のヘルパを移動 (= Session::serve の cleanup で TailEnd を投げる経路を関数化)
- 期待 LOC 削減: session.rs から約 350 行が移動
- test の影響: wait predicate / tail subscribe / strip carry 連携の test を wait.rs / tail.rs に再配置
- breaking change: なし

### Phase F (任意 / 別 task): `serve_loop` の inline 構造解体

**目的**: Phase A-E 完了後、`serve_loop` は依然 ~300 行残る (= leader cascade、lock auto-release、drop drain 等の inline 処理)。さらに切り出す価値はあるが scope が拡大するので別 task として扱う。

- 候補: `handle_drop_cascade(clients, state, indices_to_drop)` のような関数化
- 「leader cascade と lock auto-release を `Drop` for `ClientHandle` に寄せる」案も検討余地あり (= R4-M3 周辺の panic safety と合流)
- **本 DR では Phase F 着手を保証しない**。Phase A-E 完了で session.rs が約 600-800 行に収まれば、追加投資は cost-benefit を見て判断する

### 完了後の到達目標

| ファイル | 目標 LOC | 主責務 |
|---|---|---|
| `session.rs` | 約 600-800 行 | Session struct + start / serve + serve_loop orchestrator |
| `pty.rs` | 約 110 行 | child lifecycle + waitpid |
| `accept.rs` | 約 290 行 | handshake worker pool |
| `broadcast.rs` | 約 290 行 | writer pump + backpressure |
| `control.rs` | 約 420 行 | control message dispatcher + kind 別 handler |
| `lock.rs` | 約 80 行 | lock state + leader cascade helper |
| `wait.rs` | 約 280 行 | predicate / accumulated bytes / timeout |
| `tail.rs` | 約 70 行 | tail subscribe / tail_end |
| (test module) | 各 module 内 `#[cfg(test)] mod tests` | 責務に対応する test を同居 |

合計は若干増える (= 各 module の use 文 / 構造重複) が、**1 ファイルの max LOC が 4364 → 800 程度に圧縮**される。

## Rejected alternatives

### A. 分割しない (= 巨大ファイルでも問題ない)

「rust-analyzer が読めるならそれでいい」「grep で十分」案。却下理由:

- 4364 行は **新規 handler 追加時の場所探索コスト** が大きい (= v0.2.0 で keys/send/paste/wait queue/serve を追加すると 5500-6000 行に到達)
- test 責務範囲が曖昧 (= ある test がどの component を検証しているかが命名と context 依存)
- R4 review で `R4-H6` として明示的に問題視されており、deferred 扱いの理由は「DR で方針を決めるべきだから」(= 「問題なし」ではない)

### B. 行数 1000 行ずつ機械的に 4 分割

責務無視で grep 効率も悪化。「session_part1.rs / session_part2.rs」型は **何があるかを名前で示せない** ため認知負荷を増やすだけ。却下。

### C. `daemon` を別 crate に切り出す

例: `crates/hyoui-daemon/`。crate 境界変更は scope が大きく、`Cargo.toml` / lib 公開 API / 内部 visibility を全面見直しになる。

- 本 DR の主目的は **同一 crate 内での module 分割** に絞る
- 別 crate 化が必要になる場合は別 DR を起票 (= 例えば `Transport` 抽象を再設計するときに合流)

### D. `daemon/types.rs` に共通 struct を集約

`ClientHandle`、`PendingWait`、`SessionState` 等を 1 file にまとめる案。却下理由:

- 「struct がどこにあるかと、その struct を最も触る関数がどこにあるか」がズレると追跡コストが増える
- `ClientHandle` は broadcast/writer thread/queued_bytes と一体で意味を持つ。broadcast.rs に置く方が自然
- types.rs は「どこに置くか迷ったときの逃げ場」になりがちで、結果的に各 module の責務が薄くなる

### E. v0.2.0 の handler 追加と同時に分割

「分割は v0.2.0 で実 handler を書きながらやればよい」案。却下理由:

- 機能追加 + 構造変更を 1 commit / 1 PR でやると **review 困難** + **regression 切り分け不能**
- 先に Phase A-E で構造を整えてから v0.2.0 handler を追加する方が安全 (= 「容器を作ってから中身を入れる」)

### F. handle_control_message を残したまま module だけ切り出す

Phase B で dispatcher 化せず、handler 内部の巨大 match を維持する案。却下理由:

- Phase B の主目的は R4-M2 (= 311 行の単一 match + cap check 分散) 解消。dispatcher 化しないと R4-M2 が残る
- 巨大 match は handler 追加時に **kind 順 / cap check 漏れ / mode check 漏れ** を見落とす危険が高い
- dispatcher 化 + 共通 cap_check helper 抽出をセットで行うことで、handler 追加時の必須チェックを型で強制できる

## Consequences

### Positive

- session.rs が認知可能サイズ (約 600-800 行) に縮小、新 handler 追加位置が明確
- R4-H6 (session.rs 責務集約)、R4-M2 (handle_control_message 311 行) が両方解消
- test が責務単位で配置され、何を検証している test なのかが module 名で分かる
- v0.2.0 で keys/send/paste/serve を追加する際の認知負荷が大幅減
- 共通 cap_check / mode_check helper で「cap negotiation 漏れ」「mode 権限漏れ」を型で防止しやすくなる

### Negative

- module 数増加 (現 2 → 9)。`daemon/mod.rs` で submodule 宣言が増える
- module 境界跨ぎの `use super::broadcast::*;` 等が増え、import 行が増える (= 重大な認知負荷ではないが boilerplate)
- Phase A-E は 5 つの commit 系列になるため、その間 backlog の他項目 (R4-M2 等) の追跡に注意が必要
- 短期的に **diff 量が膨らむ** (= module 跨ぎ移動なので blame 履歴が浅くなる)。git/jj の `--follow` で追える、jj move semantics で大きく救われる

### Neutral

- **Transport abstraction (R4-H7) との接続は本 DR では扱わない**。R4-H7 (= UnixStream 前提のコードが daemon に散在) は DR-0008 改訂 + 後続 DR (DR-0010 想定) で「Transport 境界の再定義」を扱う。本 DR の分割完了後に Transport 抽象を被せる経路を残す: 具体的には `accept.rs` の `spawn_handshake_worker` / `do_handshake_stage` / `finalize_accepted_client` の I/O 部分、`broadcast.rs` の `writer_pump` の I/O 部分が抽象化点になる予定
- R4-M3 (Session::serve cleanup Drop 不在) は R4-H4 で `Session` に `Drop` 追加済 + `into_parts` で正常 path をバイパス済 (= panic safety は確保済)。本 DR の分割では Drop 構造そのものは触らず、現状を維持する。`serve_loop` 内の drop cascade を `Drop` for `ClientHandle` に寄せる案は Phase F の検討事項とする
- session.rs 内の `unsafe` (= `Session::into_parts` の `ptr::read` ×4) は分割後も残る。Option-based 化 (= 各 field を `Option<T>` にして `.take()` で消費) は別 task として切り出す (本 DR の scope 外)

### 関連項目の更新

- backlog `R4-H6 session.rs 3879 行責務集約`: **本 DR (DR-0009) 起票で解消方針確定**。実装着手は Phase A から
- backlog `R4-M2 handle_control_message 311 行の単一 match + cap check が分散`: **本 DR の Phase B で解消**
- backlog `R4-H7 Transport abstraction が daemon に届いていない`: 本 DR では扱わない、別 DR で扱う旨を本 DR の Neutral に明示
- backlog `R4-M3 Session::serve cleanup が Drop でない panic safety 欠如`: R4-H4 で対処済、本 DR では現状維持 (Neutral 節参照)

## 次の TODO

- [ ] **Phase A 着手**: `pty.rs` + `lock.rs` の新設、`Session::start` / `serve` から該当 fn / struct を移動、`cargo test --workspace` green を確認
- [ ] Phase B 着手 (= R4-M2 解消): `control.rs` 新設 + `handle_control_message` dispatcher 化 + cap_check / mode_check helper
- [ ] Phase C 着手 (= broadcast.rs 切り出し)
- [ ] Phase D 着手 (= accept.rs 切り出し)
- [ ] Phase E 着手 (= wait.rs / tail.rs 切り出し)
- [ ] Phase 完了ごとに `docs/journal/2026-MM-DD-session-split-phase-X.md` でハマり所を記録
- [ ] Phase E 完了後、R4-H7 (Transport abstraction) のための DR-0010 起票検討

## 関連

- [[DR-0005]] — 思想 (= 透明性最優先、daemon 化 default)
- [[DR-0006]] — CLI ground rules (= v0.2.0 で追加される handler の前提)
- [[DR-0007]] — MVP scope と v0.2.0 計画 (= 分割の必要性の根拠)
- [[DR-0008]] — protocol 設計 (= cap negotiation / mode 権限の検査ポイント)
- backlog `/tmp/itumono-backlog-hyoui.md` の R4-H6 / R4-H7 / R4-M2 / R4-M3
- `crates/hyoui/src/daemon/session.rs` (= 分割対象、4364 行)
- `crates/hyoui/src/strip.rs` (= R4-H3 で追加された StripAnsiCarry、wait.rs から参照)
- `crates/hyoui/src/scrollback.rs` (= 既存の別 module、tail.rs から参照)
