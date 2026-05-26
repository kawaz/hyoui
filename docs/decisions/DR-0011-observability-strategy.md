# DR-0011: Observability 戦略 — log / metrics / trace の導入方針

- Status: Active (方針確定、実装は Phase A から別 task)
- Date: 2026-05-27
- Related: [[DR-0005]] (思想), [[DR-0006]] (CLI ground rules), [[DR-0007]] (MVP scope と段階リリース), [[DR-0008]] (protocol), [[DR-0009]] (session.rs 分割 = instrument 挿入位置)
- Backlog 解消: R5-H1 (= 観測手段ゼロ) を本 DR で扱う。R5-H8 (runbook) は本 DR の成果物を前提とする後続 task

## Context

`crates/hyoui/src/daemon/*` 全体に `tracing::` / `log::` の使用が**ゼロ**である。detach した daemon の stderr は `daemonize.rs:75` で `Stdio::null()` に捨てられ、handshake reject / backpressure / writer dead / panic stack のいずれも記録されない。これは v0.1.x の「最小核」方針 (= [[DR-0007]]) では許容できた状態だが、以下の三点で v0.2.0 着手前に解消すべき問題になっている:

1. **事故 post-mortem が物理的に不可能**: 本番運用で daemon が想定外挙動を見せたとき (例: backpressure で client drop / writer thread が静かに死ぬ / detach 後の subprocess が即時 exit する)、stderr が `/dev/null` のため痕跡が一切残らない。再現を待つしかない。
2. **silently drop 系の debugability が低い** (R4-M26 を上位レイヤ化したもの): R4-M26 で指摘された「symptom が出る前にエラーが消失する」系のパスは、構造化ログ基盤がない限り symptom 単位の対症療法に終始してしまう。観測基盤の整備を前段に置かないと根治できない。
3. **v0.2.0 serve gateway は観測前提**: HTTP/WebSocket gateway は外部公開面 = 認証失敗 / rate-limit / abuse 検知が観測可能であることが前提になる。observability ゼロのまま gateway を立てると運用事故が確実に発生する ([[DR-0007]] v0.2.0 計画への前提条件)。

加えて、[[DR-0009]] で `session.rs` が `pty/accept/broadcast/control/lock/wait/tail` に module 分離されたことで、log line を挿入すべき責務境界が明確になった (= Phase A の作業対象が物理的に区切れる)。本 DR は DR-0009 の分割完了を前提にした次層の整備にあたる。

### 現状の観測手段 (= 何が無いか)

| 観点 | 現状 | 問題 |
|---|---|---|
| log line | `tracing::` / `log::` / `eprintln!` 含めて daemon 側 0 件 | 何も残らない |
| 構造化フォーマット | 未導入 | grep ベース運用すらできない |
| log destination | (そもそも書いていない) daemon detach 後は stderr → `/dev/null` | 外部リダイレクトも不可能 |
| level 制御 | 未導入 | 「本番だけ info、開発時 debug」が不可能 |
| metrics | 未導入 | per-session queued_bytes / client count / broadcast lag が一切不明 |
| trace propagation | 未導入 | client_id / session_id / request_id の相関追跡不可 |
| detached child stderr | `Stdio::null()` 固定 | 起動直後に死んだ場合の原因特定不能 |
| log rotation | 未導入 | (そもそも書いていない) |

## Decision

**`tracing` crate を基盤として、Phase A-C の段階導入で observability を構築する**。crate 選定・出力先・level 規約・metrics 露出経路・detached child 出力先・rotation 方式を本 DR で確定し、実装は Phase A から別 task で進める。

### 1. log 基盤: `tracing` + `tracing-subscriber`

- **crate**: `tracing` (= 構造化フィールド + level + span 対応、Rust エコシステム事実上の標準)
  - `log` facade は採用しない (= span が表現できず、後で `tracing` に切り替える二度手間になる)
  - `env_logger` 単独は採用しない (= 軽いが span 不対応、構造化フィールドが弱い)
  - `log4rs` 単独は採用しない (= 旧 std 系、`tracing` と比べて現代的でない)
- **subscriber**: `tracing-subscriber` の `fmt` + `EnvFilter` (= level 制御を環境変数経由で可能にする標準構成)
- **依存追加位置**: `crates/hyoui/Cargo.toml` に `tracing` / `tracing-subscriber` を追加。`hyoui-cli` 側は不要 (= CLI は thin で、log は daemon 側責務)

### 2. log 出力先と format

- **daemon (foreground / `--no-daemonize`)**: stderr に直接出力 (= 開発時用)
- **daemon (detach 済)**: 既定で `$XDG_STATE_HOME/hyoui/<session>.log` (= XDG state spec 準拠、`~/.local/state/hyoui/<session>.log` がデフォルト)
  - `$XDG_STATE_HOME` 未定義時は `$HOME/.local/state` を fallback として使用 (XDG spec の標準動作)
  - 既存 `$XDG_RUNTIME_DIR/hyoui/` の socket 配置 (= [[DR-0008]]) と別ディレクトリ (= log は state、socket は runtime で寿命が違う)
- **明示指定**: `--log-file=PATH` で daemon 起動時の log 出力先を override 可能 (= test / 特殊運用向け、CLI ground rules [[DR-0006]] に従う long option)
  - 並行 `--log-file=-` で stderr 強制も検討 (= Phase A で確定)
- **format**:
  - 既定 = human readable (`tracing-subscriber` `fmt` の compact preset 程度)
  - `--log-format=json` で JSON Lines (= 機械処理 / 後段の集約用)
- **level**: `trace` / `debug` / `info` / `warn` / `error` の 5 段階
  - 既定 = `info`
  - `HYOUI_LOG` 環境変数で `EnvFilter` を override (= `RUST_LOG` 流儀、ただし `tracing` 標準は `RUST_LOG`。`HYOUI_LOG` を採用するのは hyoui 固有名前空間を明示するため)
  - `--log-level=debug` のような CLI flag は導入しない (= 環境変数経由が `tracing` エコシステムの慣習)

### 3. metrics: 軽量 pull 方式

- **方針**: 専用 metrics crate (`prometheus` / `metrics` 等) は v0.2.0 では導入しない (= 依存が重い、scope creep [[DR-0005]] に抵触)
- **代替**: daemon 内部に `SessionMetrics` 構造体を持ち、`hyoui status --metrics` で snapshot を取得 (= protocol 既存の status 拡張、新規 endpoint 増設なし)
  - 露出する metric: `queued_bytes_per_client` (backpressure 観測)、`active_client_count`、`pending_handshake_count`、`broadcast_lag_ms`、`scrollback_used_bytes`、`wait_queue_depth_per_client`、`master_bytes_total` (counter)、`writer_dead_total` (counter)
  - format: protocol 拡張は最小、CBOR map で 1 levels
- **/metrics HTTP endpoint**: v0.2.0 の `hyoui-serve` gateway 側で `hyoui status --metrics` を呼んで Prometheus format に変換して公開 (= core daemon は HTTP を知らない、責務分離 [[DR-0008]] / R5-H5 の別 binary 方針と整合)

### 4. trace propagation: 識別子の log line への必須付与

- **方針**: OpenTelemetry / `tracing-opentelemetry` は **v0.2.0 では導入しない** (= 観測対象が固まっていない段階で trace backbone を入れると後で抜けない、scope creep)
- **代替**: `tracing::span!` で以下を span field に付与し、全 log line に自動で乗せる:
  - `session_id` (= daemon プロセス単位の識別子)
  - `client_id` (= accept ごとに発番、broadcast.rs の `ClientHandle` 既存 field を流用)
  - `request_id` (= control message ごとに発番、protocol 既存の correlation id を流用)
- **OpenTelemetry 移行余地**: v0.3.0+ で外部観測スタック (Tempo / Jaeger 等) と接続する余地を残すため、span field 名は OTel semantic conventions と整合する (= `session.id` ではなく `session_id` の snake_case を採用しつつ、後で alias / OTel converter を被せられる粒度に保つ)

### 5. detached child の stderr: ring buffer + on-demand 取得

- **現状**: `daemonize.rs:75` で `Stdio::null()` 固定 = 起動直後 panic / runtime panic の stack trace が消失
- **改善**:
  - daemon プロセスの stderr 自体は **2. log 出力先**経由 (= `<session>.log` に書く)
  - PTY 子プロセス (= `hyoui run` で起動した user の子コマンド) の stderr は **既定では従来通り PTY 経由** (= [[DR-0005]] の透明性原則、ユーザの想定する出力経路を変えない)
  - ただし daemon 自身の panic stack / `std::panic::set_hook` で捕捉した backtrace は `<session>.log` に必ず吐く (= panic=abort の前に flush するための `tracing-subscriber` の `with_writer` 構成)
- **`hyoui logs <session>` subcommand**: `<session>.log` の末尾 N 行を表示する read-only subcommand。Phase C で導入を検討 (= protocol 拡張不要、CLI が直接 file を読めばよい)。詳細仕様は Phase C 着手時の subcommand DR (DR-0006 補遺) で確定

### 6. log rotation

- **方式**: size-based + generation count (= `tracing-appender` の `RollingFileAppender` か独自の軽量 rotation)
- **既定値**: 10 MiB × 3 generation (= 合計 30 MiB、開発用途で十分、ユーザ ops で `--log-rotate-size=N --log-rotate-generations=M` で override 可能)
- **rotate trigger**: daemon 内部の log 書き込み path で size を確認 (= cron / logrotate 等の外部依存を持たない、self-contained を [[DR-0005]] 思想に従う)

### 7. panic hook

- **方針**: daemon process 起動の最初期 (= `Session::start` 前、`main` の最初の行) で `std::panic::set_hook` を入れ、panic 時に `tracing::error!` で stack を吐いてから既定の panic=abort 動作に委ねる
- **flush**: `tracing-subscriber` の `BlockingWriter` 構成で flush を保証 (= panic=abort 時にもログが取れる)

## 実装 Phase 分割

### Phase A: `tracing` 導入 + daemon 全責務に log instrument

- 範囲: `crates/hyoui/Cargo.toml` への `tracing` / `tracing-subscriber` 追加、`main.rs` の subscriber 初期化、`daemon/{pty,accept,broadcast,control,lock,wait,tail,session}.rs` への log line 挿入
- 挿入ポイントの目安:
  - `accept.rs::spawn_handshake_worker` / `do_handshake_stage` (= handshake reject の info / warn)
  - `broadcast.rs::writer_pump` / `enqueue_for_client` (= backpressure threshold 到達の warn、writer thread 終了の info)
  - `broadcast.rs::send_backpressure_error` (= 既存の silently drop パス、R4-M26 由来 → warn 化)
  - `control.rs::handle_control_message` (= 各 handler entry/exit の debug、エラー path の warn)
  - `lock.rs::generate_lock_token` (= R5-H11 の panic 回避とセットで Result 化済前提、失敗時 error)
  - `pty.rs::ChildLifecycle::poll` (= 子 state transition の info、想定外 wait status の warn)
  - `session.rs::serve_loop` (= 起動完了 info、shutdown info、accept loop の cap reject warn)
  - `daemonize.rs` (= detach 直前後の info、log file open 失敗時 error → fallback to stderr)
- level 規約:
  - `error`: 復旧不能、daemon が継続できない (panic 前 / unrecoverable I/O failure)
  - `warn`: 復旧可能だがユーザに気付いてほしい (backpressure drop、handshake reject、cap reach)
  - `info`: 主要 lifecycle event (起動、accept、detach、shutdown)
  - `debug`: handler 単位の entry/exit、subscription 変化
  - `trace`: per-frame / per-byte 粒度 (= 開発時のみ、本番は `EnvFilter` で off)
- 完了条件: 既存 test が pass + 主要 path を手動 trigger して log line が出ること
- 見積: 3-5 h (backlog R5-H1 の見積と一致)

### Phase B: `hyoui status --metrics` 実装

- 範囲: `SessionMetrics` 構造体追加、protocol status response の cap flag 付き拡張 ([[DR-0008]] cap flags ベース schema evolution に従う)、CLI 側 `--metrics` flag
- 露出 metric は本 DR §3 のリストに準拠
- protocol breaking change なし (= cap flag による additive 拡張)
- 完了条件: `hyoui status --metrics --session foo` で CBOR map が返り、queued_bytes 等が読める

### Phase C: detached child の log file capture + `hyoui logs` subcommand

- 範囲: `daemonize.rs` の `Stdio::null()` → `<session>.log` への redirect (= log file との merge を許可、daemon process 自体の stderr も拾えるようにする)、`hyoui logs <session>` subcommand 追加
- subcommand 細部は CLI ground rules [[DR-0006]] に従う long option ベース、`--follow` / `--lines N` / `--since` 等の細目は別 DR (DR-0006 補遺) で確定
- v0.2.0 入り前提 = serve gateway 公開前に運用準備として必須

### Phase 順序の根拠

A は他 Phase の前提 (= 構造化ログがない状態で metrics や file capture を入れても観測できない)。B は protocol 拡張が小さく独立で進められる。C は detach 周りの実装変更が必要なため A 完了後にしか進められない (= log subscriber 構成が固まらないと file rotation の責務が分散する)。

## Rejected alternatives

| 案 | 不採用理由 |
|---|---|
| `env_logger` (= `log` facade + 環境変数 filter) | span 非対応 = client_id / session_id の構造化 propagation が表現できない、tracing への移行コストが後で発生 |
| `log` + `log4rs` | 旧 std 系、現代の Rust エコシステムでは tracing が事実上標準、新規プロジェクトで採用する理由なし |
| `prometheus` crate を daemon に直 import | 依存重い (lazy_static / parking_lot 系の transitive)、core daemon を lean に保つ [[DR-0005]] 思想に反する、metrics は serve gateway 側で format 変換する責務分離が筋 |
| `tracing-opentelemetry` を v0.2.0 で導入 | 観測対象 (どの span / どの metric を見たいか) が固まっていない段階で trace backbone を入れると後で抜けない、scope creep [[DR-0005]]。v0.3.0+ で外部観測スタックと接続する判断を別 DR で行う |
| log destination を `/tmp/hyoui-<session>.log` | XDG state spec から外れる、`/tmp` は OS 起動で消える前提 = post-mortem に使えない |
| log destination を `$XDG_RUNTIME_DIR/hyoui/<session>.log` (socket と同居) | runtime は寿命が短い (logout で消える)、post-mortem が成立しない |
| log rotation を logrotate(8) に任せる | 外部依存、Linux 専用の前提 (macOS 標準 ops と乖離)、self-contained 原則 [[DR-0005]] に反する |
| `--log-level=debug` の CLI flag を追加 | `tracing` エコシステムの慣習は `RUST_LOG` / `HYOUI_LOG` 環境変数、CLI flag は冗長、CLI ground rules [[DR-0006]] の「環境変数で済むものは CLI flag を増やさない」と整合 |
| daemon stderr を子コマンドの stderr と merge | [[DR-0005]] 透明性原則違反 = ユーザの想定する子コマンド出力経路を歪める |

## Consequences

### Positive

- Phase A 完了で post-mortem 可能化 = backpressure drop / handshake reject / writer dead の事故原因が log から特定できる
- Phase B 完了で v0.2.0 serve gateway の運用準備が整う (= /metrics 経由で外部監視と接続可能)
- Phase C 完了で detached daemon の起動直後 panic も追跡可能
- 構造化フィールド (session_id / client_id / request_id) で OpenTelemetry 移行余地を残しつつ、現段階は依存を増やさない
- R4-M26 (silently drop の debugability) が log line として symptom 単位で残るため、根治判断ができるようになる

### Negative / Neutral

- log 出力で per-byte path に log line が増えると performance に影響する可能性 (= `EnvFilter` の `info` 既定で `trace` level は off、性能影響は最小化されるが、benchmark との関係は別途検証が必要 → 別 DR / findings で扱う)
- `<session>.log` のディスク容量が運用上の懸念になる (= rotation で 30 MiB cap = 同時 100 session で 3 GiB 上限、許容範囲だが運用 doc に明記すべき)
- `tracing` / `tracing-subscriber` / `tracing-appender` の supply chain が増える (= R5-H13 の `cargo audit` / `cargo deny` CI と組み合わせて運用リスクを抑える)
- `HYOUI_LOG` 環境変数は `RUST_LOG` 慣習から外れるが、hyoui 固有名前空間を採用 (= 他 Rust tool との混在環境で誤 trigger を避ける、後方互換問題なし = 新規導入なので変更コスト 0)

### Neutral (= 本 DR では決めない事項)

- Phase C の `hyoui logs` subcommand 細目 (= `--follow` / `--lines N` / `--since` / output format) は CLI ground rules [[DR-0006]] の補遺 DR で確定
- log rotation の異常系 (= disk full / permission denied) 時の daemon 挙動は Phase C 着手時の findings で扱う
- benchmark への影響評価は Phase A 完了後に別 task で計測

## 関連

- backlog `R5-H1` (= 観測手段ゼロ): **本 DR 起票で解消方針確定**。実装着手は Phase A から、backlog 上は `[deferred → DR-0011 起票後 Phase A 実装]` でマーク
- backlog `R5-H8` (= runbook ゼロ): 本 DR の成果物 (log line / metrics) を前提とした後続 task。本 DR スコープ外
- backlog `R4-M26` (= silently drop の debugability): 本 DR Phase A の log instrument で symptom 単位 → 構造化ログ基盤に上位レイヤ化される
- [[DR-0005]] hyoui の思想 (= 透明性 / 外側自動操作主軸 / self-contained)
- [[DR-0006]] CLI ground rules (= log 関連 CLI flag の命名規約)
- [[DR-0007]] MVP scope と段階リリース (= v0.2.0 serve gateway 着手前の前提条件として本 DR を位置付け)
- [[DR-0008]] protocol design (= cap flag による status response 拡張、metrics 公開経路の責務分離)
- [[DR-0009]] session.rs module 分割 (= log instrument 挿入位置が module 境界として明確化済)
