# hyoui ROADMAP

> 注: ここに version 区切り (= v0.1.x / v0.2.0 等) は記載しない。scope の正本はこの ROADMAP、version は release 時の便宜 (= bump-semver で打つ)。リリースに何が含まれるかは git log / CHANGELOG を参照。

[[DR-0013]] 起票 (2026-05-27) を機に、従来の version 区切り型 (= v0.1.x / v0.2.0 / v0.3.0+) ROADMAP を**廃止**。
固定 version への scope 紐付けは実態と乖離しやすく指標として弱いため、**4 層列挙型** (必須 / 優先 / 追加予定 / 過去 milestone) に再編した。
各 DR の Status は維持、scope の正本は本 ROADMAP。

## 必須 (= 基盤、これが終わるまで他に着手しない)

[[DR-0013]] Phase A: screen emulator 採用 + attach handshake redraw + alt mode hook

- vt100 crate 取り込み (= 0.16) — `Cargo.toml` に `vt100 = "0.16"` を追加
- `crates/hyoui/src/daemon/screen/` module 新設 (= `mod.rs` + `virtual_screen.rs`、wrapper 100-150 行)
- 子 PTY read loop で `vt100::Parser::process` を呼び、state 反映を統一
- attach handshake redraw 実装 (= `ScreenStateInit` control message、`Screen::state_formatted()` + alt mode prepend)
- alt screen hook (= `Screen::alternate_screen()` 判定 + 補完 sequence prepend)
- 既存 broadcast の attach 時動作変更 (= 生 byte broadcast → state 経由)
- DEC sync update (= `?2026h`) 抑制 hook
- stalled sequence 5s reset (= health check、parser internal buffer clear)

出典: [[DR-0013]] §1-6, Implementation Phase A

## 優先 (= 必須完了後、順次)

### [[DR-0013]] Phase B 残項目

完了済 (2026-05-27 nonstop session):

- [x] input bytes log 実装 (= primary buffer 用 bounded ring buffer、`daemon/screen/input_log.rs`) + resize replay
- [x] debug / inspection protocol (= `ScreenDumpRequest` / `ScreenDumpResponse` / `StateSnapshotRequest` / `StateSnapshotResponse`)
- [x] [[DR-0008]] cap flag 追加 (= `screen-dump-v1` / `state-snapshot-v1`)
- [x] structured snapshot 圧縮 wrapper (= `daemon/screen/snapshot.rs`、空 cell skip + 属性 bit pack + Color variant 整数化)
- [x] stalled sequence 5s reset (= health check、parser internal buffer clear)

未着手 (= 順次):

- 既存 `crates/hyoui/src/scrollback.rs` の vt100 wrapper 置換は **見送り** (= byte-base tail 専用層として責務分離した、[[DR-0013]] §8 Update)
- `last_evicted_age` 補完 counter (= 2026-05-28 で vt100 内蔵 ring は配線済 (default 1000 行) になったが、本 counter は未配線。incremental sync で「いつ scrollback から行が evict されたか」を caller が確認したくなった段階で実装)
- per-line SequenceNo + pull 型 protocol (= `DirtyLinesNotify` / `GetLinesRequest` / `GetLinesResponse`)
- PDU serial 番号導入 (= out-of-order tolerant + RTT 計測)

出典: [[DR-0013]] §3-§11, Implementation Phase B

### state-based 上位機能 (完了済 2026-05-27)

- [x] **wait** (= state-based、scrollback 誤マッチ解消、現在 visible に対する match。`cli/wait_core.rs` が `screen.snapshot.request` を polling して visible cells から text 構築 → regex match)
- [x] **snapshot** (= state-based、`hyoui screen dump` / `hyoui screen snapshot` の CLI 露出、[[DR-0006]] §10)
- tail (= byte-base 維持、state-based wait / snapshot との棲み分けを [[DR-0006]] §11.4 で明示)

### input family 整理 (完了済 2026-05-27)

- [x] spec syntax 統一: `text:` / `hex:` / `file:` / `paste:` / `key:` / `wait:` / `wait-idle:` prefix
- [x] `hyoui input <session> <spec>...` (= leaf 廃止、1 leaf に集約、[[DR-0006]] §8.1)
- [x] bracketed paste は prefix で明示 (= `text:` direct / `paste:` bracketed、[[DR-0006]] §8.3)
- [x] multi-line script を 1 paste block で送る経路 (= `paste:$(cat script.py)`)
- [x] `file:` 入力の size / type validation
- [x] key alias (= Unicode key alias + typo suggest、[[DR-0006]] §8.4)
- [x] spec prefix typo suggest (= edit-distance ベース)
- multi-modifier (= Ctrl-Shift-A 等) は terminal capability negotiation が必要、`追加予定` へ

### lock / tx (完了済 2026-05-27)

- [x] `hyoui lock acquire <session> [--timeout-* ...] [--mode wait|fail]`
- [x] `hyoui lock release <session> [--token T | --force]`
- `hyoui lock tx <session> [--timeout-* ...] -- cmd args...` は **subcommand 予約済 + 起票済**、本実装は別 task (= `docs/issue/` 参照)
- [x] `HYOUI_LOCK_TOKEN` の自動継承 (= `--lock-token` 未指定時に env から拾う)
- wait queue 実装 (= 旧 v0.1.x で「即 Denied 返却」だった部分の proper 化) は別 task

### completion / UX

- [x] shell completion (= `hyoui completion <shell>`、screen + input subcommands + spec prefix 補完)

### `wait` の chunk boundary 跨ぎ needle miss 修正

- screen state 経由 (= state-based wait) になり **自然解消** (= cell 単位で text 化されるので chunk 境界の概念が消える)
- tail 側の strip carry は R4-H3 として別 task に残る

## 追加予定 (= 順序定めず、必要が出たら検討)

### [[DR-0013]] Phase C (= 優先度低めだが記録)

完了済:

- [x] **scrollback layer dump** (2026-05-28、`hyoui screen dump --layer={scrollback,both}` 配線、`DaemonConfig.screen_vt100_scrollback_rows` default 1000 行、`--scrollback-rows` / `HYOUI_SCROLLBACK_ROWS` で override 可)

未着手:

- **observe mode** (= `--no-resize-propagate`、子に WINCH 送らず daemon screen state を native size で表示、観戦 / 複数 client 異サイズの reflow 戦争回避)
- **multi-client resize モード config 化** (= tmux pattern の `smallest` / `largest` / `manual` / `latest` 4 モード、MVP は `smallest` 固定)
- **scrollback の真の reflow 実装** (= vt100 内蔵以上の品質が必要になったら、tmux pattern に拡張)
- **scrollback dump の `--last-rows N` / `--rect` honor** (= 末尾 N 行だけ取る、矩形領域指定の honor、別 task)
- **scrollback ANSI dump の色保持** (= 現状 SGR bold/italic/underline/inverse のみ、色情報は落ちる。完全保持したい場合は Cbor 経路を使う)
- **zstd 圧縮** (= `redraw_bytes` 32 bytes 超で、Phase B 負荷測定後)
- **libghostty-vt swap 評価** (= C API stable + semver annotate + 標準 allocator 対応になれば swap 候補)
- **vt100 fork vendor 戦略** (= bus factor 対策、abandon 時に hyoui workspace に vendor する手順を準備)

### gateway / 配布 / 周辺

- **serve gateway** (= 別 repo `kawaz/hyoui-serve` に切り出し、独立 release cycle、xterm.js + WebSocket binary、[[DR-0010]] §2)
- **record-replay** (= asciinema 互換 cast format、`hyoui record` / `hyoui play`、sink 概念の前段)
- **Python / Node bindings** (= `hyoui::client::AttachClient` を pyo3 / napi-rs で expose、Pexpect 代替の library API)
- **packaging** (= homebrew tap + cargo install 以外の Linux 配布 path、npm / pypi 整理)
- **bounded queue / backpressure の measurement** (= v0.1.0 default queue cap 8 MiB の見積もり値を実 measurement で調整、R4-M28)

### 上位機能の発展

- **L2 wait** (= 高度な pattern match、AST レベル、named area `--area input-line --pattern R`、config-driven area alias)
- **wait `--child-exit` / `--regex-on-screen`** (= 子 process exit 待ち、screen grid 上の regex 検索、R4-M24)
- **multi-modifier 対応** (= xterm modifyOtherKeys / kitty keyboard protocol、terminal capability negotiation 要)
- **leader CLI 露出** (= `hyoui leader show|take|give` / `attach --as-leader`、内部実装は v0.1.0 完了済)
- **tx buffered mode** (= 他 client の入力を蓄積、tx 後 flush)
- **sink concept** (= daemon 内永続出力先、tail と区別、`docs/issue/2026-05-26-feature-recording-and-dump.md`)

### observability / 信号 / 規約

- **observability** (= [[DR-0011]] Phase A 以降、tracing instrument、status --metrics、detached child log file + hyoui logs)
- **signal wire name 化** ([[DR-0012]] 完了後の整理、cross-OS serve gateway 対応の前提)
- **itumono skill 改修** (= `/tmp` → `docs/REVIEW-BACKLOG.md` 規約への移管、別 PR で実施)
- ~~**[[DR-0006]] §8/§9 改訂**~~ (完了済 2026-05-27、§8 input family / §9 wait / §10 snapshot / §11 tail を state-based に書き直し済、旧仕様は Archive section に保全)
- **detach key sequence の customize** (= `--detach-prefix` env / option で `Ctrl-A D` 固定を解除)
- **attach subcommand 専用 `--help`** (= detach key 動作・mode 説明を専用 help に書く)

### 横断的な改善 (= 時期未定、必要が出たら拾う)

- `Transport` abstraction を daemon 側にも徹底 (= R4-H7、現状 UnixStream 前提のコードが散在)
- `Session::run` (= Phase 8 legacy) の撤去 (= `Session::serve` 一本化、R4-M1)
- `handle_control_message` 311 行の分割 (= R4-M2)
- `cli.rs` 2200+ 行手書き parser の整理 (= clap 移行検討は別 DR、R4-M20)
- `token` field の `Debug` derive 漏れ (= security: ログに token 漏出、R4-H8)
- 全 public enum に `#[non_exhaustive]` (= 前方互換、R4-H9)
- `Error` enum の `&'static str` sub-discriminator を構造化エラーに置換 (= R4-H13)
- struct field の `pub` → builder pattern (= invariant 保護、R4-M18)
- `Transport::split` の `Send + 'static` 緩和 (= embed 利用向け、R4-M19)
- error message に next-action hint (= R4-H2)
- duration format の bare 数字 reject 時の hint (= R4-M5)
- `hyoui run --help` option 順序の一貫性 (= R4-M6)
- detach key と bash readline (`Ctrl-A`) 衝突の docs 警告 (= R4-M4)
- session id 自動採番ルールの docs 化 (= R4-M7)
- [[DR-0008]] に error code 一覧追記 (= R4-M11)
- error code naming の階層化 (= R4-M9)
- linux / macOS / WSL サポート明文化 (= R4-M29)
- migration guide (= version 移行手順、R4-M30)
- signal handler test の process-global state leak 対処 (= R4-C4)
- timing tight な threshold の relax (= CI flaky 解消、R4-H5)
- `parse_duration_ms` overflow path のテスト追加 (= R4-M12)
- regex DoS / `size_limit` 超過の test (= R4-M13)
- `hyoui-cli` の `main.rs` / `daemonize.rs` に test (= R4-M14)
- `sys/raw.rs` の test 拡充 (= R4-L6)

## 過去 milestone (= reference のみ、削除はしない)

本 ROADMAP の前 version (= [[DR-0007]] / [[DR-0010]] / [[DR-0011]] / [[DR-0012]] の頃) では v0.1.x / v0.2.0 等の version 区切りで scope を切っていた。これは指標として弱く、固定化すると実態と乖離するため [[DR-0013]] 起票 (2026-05-27) と同時に廃止。各 DR の Status は維持、scope の正本はこちら。

実機検証 (= cmux-msg 連携 / claude TUI 観戦) で **attach がほぼ機能しない + wait pattern 誤マッチ多発** が判明し、根本原因が daemon に screen emulator が無いことだったため、ROADMAP を「screen emulator + attach/detach 安定化を最優先」に組み替えた。input / wait / tail / lock / tx 等は全部この基盤完了後の延長に降格。詳細経緯は `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md`。

旧 ROADMAP に存在し、本 ROADMAP に未掲載 / 統合済の項目 (= reference):

- v0.1.0 残 (Phase 10 完了 / detach key customize / attach --help / wait chunk boundary): wait chunk boundary は `優先` セクションに統合、他は `追加予定` に移行
- v0.2.0 自動操作 CLI 7 個 (input / detach / status / tail / wait / lock / completion): `優先` の state-based 上位機能 + input family + lock / tx に再分配
- v0.2.0 serve gateway: `追加予定` の gateway / 配布
- v0.2.0 wait queue: `優先` の lock / tx に統合
- v0.2.0 環境変数自動継承: `優先` の lock / tx に統合
- v0.2.0 候補 (wait --child-exit / record-replay / Python・Node bindings): `追加予定` に移行
- v0.3.0+ 画面 emulator + snapshot: [[DR-0013]] に置き換え (= 必須に昇格)
- v0.3.0+ 高度な TUI 自動化: `追加予定` の L2 wait
- v0.3.0+ leader CLI 露出: `追加予定` の上位機能の発展
- v0.3.0+ tx buffered mode: 同上
- v0.3.0+ sink concept: 同上

## 関連

- [[DR-0005]] — 思想
- [[DR-0006]] — CLI ground rules (v0.2.0+ 自動操作 API の正本、§8/§9 は state-based に書き直し要 = `追加予定`)
- [[DR-0007]] — MVP scope と段階リリース (version 区切りは廃止、本 ROADMAP が正本)
- [[DR-0008]] — protocol (= cap flags ベース schema evolution、[[DR-0013]] §10 で structured state access message 追加)
- [[DR-0010]] — input family 整理 + serve gateway 別 repo (= input family は `優先`、serve は `追加予定`)
- [[DR-0011]] — observability 戦略 (= `追加予定` の observability)
- [[DR-0012]] — signal wire name 化 (= `追加予定` の signal wire name 整理)
- [[DR-0013]] — screen emulator + attach/detach 安定化 (= 本 ROADMAP の `必須` / `優先` Phase B / `追加予定` Phase C の正本)
- `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md` — 方針大転換の議論経緯
- `docs/issue/2026-05-26-feature-recording-and-dump.md` — sink / record / dump の発想元
