# Decision Records (DR) Index

hyoui の設計判断記録一覧。ファイル名は `DR-NNNN-title.md`（4 桁ゼロパディング）。
`docs-structure.md` ルールに従い `## Active` / `## Archived` / `## Moved to research/` で区分する。

## Active

Status 列は **ラベル + 最終判定日** だけを載せる。実装範囲・残 Phase・裁定の内訳は各 DR 本文の Status 行と本体を見る。

- **✅ 実装済**: DR の Decision / Implementation phases が完了している
- **🟡 部分実装**: 一部のみ実装、他は ROADMAP / 別 task に
- **⬜ 未実装**: 設計のみ、実装エビデンスなし
- **🚧 Active**: 実装進行中 (Phase 途中)
- **🔁 Superseded**: 後続 DR が判断を置き換えた
- **N/A**: 実装対象でない (= 命名 / 思想 / ROADMAP / プロセス)
- **❌ 撤退**: 撤退判断済
- **💭 提案**: Proposed。裁定待ちの論点が残っている

| DR | Status | 説明 |
|---|---|---|
| [DR-0001](./DR-0001-bgfg-jobcontrol-two-axis.md) | 🟡 部分実装 (2026-06-11) | bg/fg ジョブ制御の 2 軸 (= 現存は軸 1 のみ) |
| [DR-0002](./DR-0002-project-naming.md) | N/A (= 命名) | プロジェクト名 "hyoui" (憑依) の決定 |
| [DR-0003](./DR-0003-rust-only-and-forkpty-login_tty.md) | ✅ 実装済 | Rust 一本化 (MoonBit 却下) と forkpty + login_tty 採用 |
| [DR-0004](./DR-0004-cli-subcommand-design.md) | ✅ 実装済 | CLI サブコマンド構成の決定 |
| [DR-0005](./DR-0005-design-philosophy-external-automation.md) | N/A (= 思想) | hyoui の思想 (= 外側自動操作主軸、TUI multiplexer ではない、透明性最優先) |
| [DR-0006](./DR-0006-cli-ground-rules.md) | 🟡 部分実装 | CLI 設計の地盤ルール (= 動作モデル / 自動操作 API / 排他制御) |
| [DR-0007](./DR-0007-mvp-scope-and-staged-release.md) | N/A (= ROADMAP) | MVP scope と段階リリースの区切り |
| [DR-0008](./DR-0008-protocol-design.md) | ✅ 実装済 | protocol 設計 (= CBOR ハイブリッド framing、cap flags) |
| [DR-0009](./DR-0009-session-module-split.md) | ✅ 実装済 (2026-05-27) | `daemon/session.rs` の責務分割 |
| [DR-0010](./DR-0010-v020-scope-and-serve-placement.md) | N/A (= ROADMAP) | v0.2.0 scope re-scope + serve gateway の配置判断 |
| [DR-0011](./DR-0011-observability-strategy.md) | ⬜ 未実装 | observability 戦略 (= tracing 採用) |
| [DR-0012](./DR-0012-signal-wire-name-not-number.md) | ✅ 実装済 | signal wire を u8 number から signal name string に変更 |
| [DR-0013](./DR-0013-screen-emulator-and-attach-stability.md) | 🟡 部分実装 | screen emulator + attach/detach 安定化 + データモデル統一 (= daemon を screen state の正本にする) |
| [DR-0014](./DR-0014-transparency-and-empirical-verification.md) | N/A (= プロセス) | 透過原則の徹底と検証主義 (= self-check / マトリクス検証 / ドッグフーディング) |
| [DR-0015](./DR-0015-run-as-fork-plus-attach.md) | ✅ 実装済 (2026-05-28) | `hyoui run` を fork daemon + attach client の合成に再定義し client/server 同居を廃止 |
| [DR-0016](./DR-0016-tty-io-record.md) | 🟡 部分実装 | `hyoui record` — tty I/O timeline の永続録画 subcommand (= bug 解析の観測道具) |
| [DR-0017](./DR-0017-session-anchor-and-suspend-policy.md) | ✅ 実装済 (2026-06-11) | session anchor 化 + suspend policy 改訂 (= TUI の Ctrl-Z を本来の意味論で動かす) |
| [DR-0019](./DR-0019-run-option-cleanup-and-suspend-policy-placement.md) | ✅ 実装済 (2026-06-12)、§5 は 🔁 Superseded by DR-0042 (2026-10-06) | run オプション棚卸し + suspend policy の daemon 配線。§5 (非 tty stdin の pipe-through と `--stdin-eof`) は DR-0042 が置き換え |
| [DR-0020](./DR-0020-self-session-reference.md) | ✅ 実装済 (2026-06-12) | self-session 参照 (= 子へ `HYOUI_SESSION_ID` 注入 + session 引数の省略時解決規則) |
| [DR-0021](./DR-0021-pty-drain-ack-for-bytes-input.md) | ✅ 実装済 (2026-06-16) | bytes 系 input spec の完了点を「PTY drain ack」に強化 |
| [DR-0022](./DR-0022-input-invocation-auto-lock.md) | ✅ 実装済 (2026-06-16) | `hyoui input` invocation 全体で 1 lock を auto-acquire / release |
| [DR-0023](./DR-0023-child-env-scrub.md) | 🔁 Superseded by DR-0024 (2026-06-22) | 子 PTY env scrub の初版 (= target-aware scrub + CLI flag 方式) |
| [DR-0024](./DR-0024-env-scrub-config-file.md) | ✅ 実装済 (2026-06-22) | 子 PTY env scrub の config ファイル化と CLI flag 最小化 (= hyoui 初の config ファイル機構) |
| [DR-0025](./DR-0025-daemon-reducer-and-domain-formalization.md) | 🚧 Active (2026-07-03) | Daemon Reducer 化と全ドメイン event の形式化 (= 6 domain reducer + 10 Phase migration) |
| [DR-0026](./DR-0026-attach-ctrl-z-intercept-and-reattach-resume.md) | 🔁 Superseded by DR-0029 (2026-07-25) | attach UX 拡張 (= Ctrl+Z 折衷 intercept + 再 attach 時の stopped child auto-resume) |
| [DR-0027](./DR-0027-web-gateway-in-repo.md) | ✅ 実装済 (2026-08-01) | Web UI gateway を同 repo `crates/hyoui-web` に置く (= DR-0010 §2 の別 repo 方針を supersede) |
| [DR-0028](./DR-0028-daemon-graceful-upgrade-self-exec.md) | 🟡 部分実装 (2026-07-21) | daemon graceful upgrade — self-exec で fd/PID を引き継ぐ (= Phase 1〜3 実装済、検証マトリクスは未整備) |
| [DR-0029](./DR-0029-attach-is-a-viewport-ctrl-z-guard.md) | ✅ 実装済 (2026-07-30) | **attach は覗き窓であり client 操作で子を止めない**の明文化と、これに反する既存判断の撤回 |
| [DR-0030](./DR-0030-rw-attach-keeps-child-running.md) | ✅ 実装済 (2026-07-29) | **rw attach client が居る間、子を停止させたままにしない**の確定 (= DR-0029 の対偶) |
| [DR-0031](./DR-0031-web-service-subcommand.md) | ✅ 実装済 (2026-07-29) | `hyoui web service register\|unregister\|status` で HTTP gateway の OS 自動起動を製品機能化 |
| [DR-0032](./DR-0032-child-suspend-unified-enum-and-action-menu.md) | ✅ 実装済 (2026-07-30) | 子 suspend 時動作の統合 enum (`[session] on_child_suspend`) + child action menu |
| [DR-0033](./DR-0033-leader-request-takeover.md) | ✅ 実装済 (2026-08-01) | `leader.request` — rw client による leader 奪取 (takeover) |
| [DR-0034](./DR-0034-service-multi-unit-and-stable-unstable-ha.md) | 🟡 部分実装 (2026-09-15)、部分 🔁 Superseded by DR-0038 (2026-10-04) | `hyoui web daemon` / `hyoui web service` の 2 系統化 (= 監督者 1 つを OS に載せる) と stable / unstable 2 インスタンスの HA。unit の中身・置き場・service の環境は DR-0038 が置き換え |
| [DR-0035](./DR-0035-web-contract-and-protocol-generation.md) | 🟡 部分実装 (2026-09-16) | web 境界の契約を `contract.rs` の serde 型で正本化し、世代 version (`WEB_PROTOCOL_VERSION`) で stale なページを検出する (= 伝達は WS hello frame と `/version` のみ、帯で reload 誘導)。W1-1〜W1-6 実装済、gate 1(b) は canddy の 3 endpoint 待ち / W1-5 の cap 不足 daemon への 501 は実機未検証 |
| [DR-0036](./DR-0036-passkey-auth-for-web-endpoints.md) | ✅ 実装済 (2026-10-04) | web endpoint を passkey で守る (= 登録は CLI 発行の招待 URL、gateway は自分の endpoint を知らず record の endpoint で検証、無認証 mode を持たない)。W2-1〜W2-6 と決定 5 の WS 側 (期限での切断 / `auth.extend` の family 照合 / 再利用検知時の WS 切断 / close code 4401) まで実装済。残りは実装でなく、kawaz の本番 endpoint 登録と gate 3 の Safari / iOS 確認 |
| [DR-0037](./DR-0037-daemon-nonblocking-event-loop.md) | 💭 提案 (2026-09-29) — 🟡 段階 1 実装済 | daemon のイベントループは外部 (client / 子 / fs / 標準エラー) の応答を待たない (= 不変条件、runtime 比較、kernel 同期 API の扱い、段階移行、固まった daemon の検出)。Q1〜Q3 / Q8 と段階 1 の着手は裁定済み、Q4〜Q7 は裁定待ち |
| [DR-0038](./DR-0038-web-unit-is-a-config-file.md) | 🚧 Active (2026-10-04) | web の unit = 任意 path の config ファイル 1 つ (登録簿は `{config, binary_path, enabled}`、`extends` で土台を共有)、foreground 起動は `daemon run` 1 本、置き場を `hyoui/web/` に揃える、service に場所の env を固定し差分で止まる。unit の config は `<unit>.toml` で `state_dir` (面) を必須に持ち、`daemon add <unit>` が生成する。unit 名に既定値は持たせない |
| [DR-0039](./DR-0039-webui-terminal-app-rework.md) | ⬜ 未実装 | web UI をブラウザ上のターミナルアプリとして作り直す (新規セッション作成、タブグループ / タブ / pane、web 専用アクション、構造は共有・配置は端末ごと、マウスモード切り替え、入力経路は WS 1 本、xterm.js 6.0.0) |
| [DR-0040](./DR-0040-daemon-own-cell-model-and-layer-composition.md) | ⬜ 未実装 | daemon の仮想スクリーンを vt100 から自前のセルモデル crate にし、rect 単位の層合成でオーバーレイを重ね、attach 出力は常に合成画面から作る (tmux 型) |
| [DR-0041](./DR-0041-session-id-uuid-and-tags.md) | ✅ 実装済 (2026-10-07) | session id を UUID にし、namespace を廃止して tag (`key=value`、`run --tag` / `list --tag`) にし、socket を `hyoui/sessions/` にフラットに置く。面は状態の root を決める環境変数 1 つ (`HYOUI_STATE_DIR`) で決まる。廃止した namespace の option は 2026-11 まで受け付けて捨てる |
| [DR-0042](./DR-0042-non-tty-stdin-is-the-childs-fd.md) | 🚧 Active (2026-10-06) | 非 tty の stdin は子の fd 0 にそのまま渡し、PTY は制御端末と出力にだけ使う (DR-0019 §5 を置き換え)。子の stdin も PTY にする指定は `run --pty-stdin`、attach は stdin を子に流さずキーは入力端末 (stdin か `/dev/tty`) から読む。配線に失敗したら起動・接続を拒否する |
| [DR-0043](./DR-0043-child-starts-with-default-signals.md) | ✅ 実装済 (2026-10-09) | 子は前景の job として exec する: SIGINT / SIGQUIT / SIGTSTP / SIGTTIN / SIGTTOU / SIGPIPE を既定に戻し、signal mask を空にする (呼び出し元や hyoui 自身の無視・block を子に引き継がない)。一覧の外の無視は引き継ぐ |
## Archived

後続の DR に置き換えられて退避した DR は [archive/INDEX.md](./archive/INDEX.md)。

## Moved to research/

(なし)
