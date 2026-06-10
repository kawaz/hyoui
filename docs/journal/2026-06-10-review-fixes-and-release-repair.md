# review-fixes-and-release-repair: 全体レビュー → 指摘一括対応 → release pipeline 修復

- Date: 2026-06-10

## 何をしていたか

プロジェクト全体レビュー → 指摘の一括並列修正 → push 後に発覚した CI / release pipeline の
連鎖故障の修復、までを一気通貫で実施した日。Ctrl-Z バグの根本原因確定と session anchor 方針の
PoC もこの日に決着。状況復元用の要約 + ポインタとして残す (詳細は各参照先)。

## 1. プロジェクト全体レビュー (3 観点並列)

daemon コア / CLI 層・テスト / docs 整合性 の 3 観点を並列レビュー。主要発見:

- **redact-after-prompt が no-op** (Critical) — `--input-secrecy` が未配線で stdin 素通し記録
- completion と実装の体系的乖離 (subcommand / flag のズレ)
- CI で主機能の統合テストが全て `#[ignore]` され実質未検証
- DR INDEX / CHANGELOG の status 腐敗 (実態と記述のズレ)
- Ctrl-Z バグ 3 件未着手 (issue 起票済みだが原因未確定)

**false positive だった指摘**: 「SIGPIPE で daemon 自爆」説はレビュー指摘だったが、実機検証で
棄却。Rust ランタイムが pre-main で SIGPIPE を SIG_IGN にしており、client を `kill -9` した後も
daemon は生存し続けることを確認した。

## 2. 指摘一括対応 (11 agent 並列修正)

ファイル排他割当で conflict を回避しつつ 11 agent で並列修正:

- completion 同期 + SSOT ガード / broadcast backpressure 統一 / SIGCHLD fallback
- record path traversal 防御 / `HYOUI_LOCK_TOKEN` 漏洩防止
- ready-pipe・wait・lock の hang 防止 / `run --detached` を session 名出力に変更
- `now_unix_ms` 統合 / docs 真実化 / CI 強化 / Ctrl-Z 調査

並列編集の縫合ミス (clippy 3 件、README の実装乖離 4 件) は検証 agent が後追いで修繕。

## ハマり所 → 解決策

### jj リポは worktree isolation 不可、ファイル排他で並列

- 現象: jj 管理リポで agent ごとの worktree 分離ができず、並列編集が同一ファイルで衝突しうる
- 解決: agent ごとに**編集対象ファイルを排他割当**して conflict を構造的に回避

### pgrep -f が daemon 自身を誤検出

- 現象: `pgrep -f 'sleep 6000'` が daemon プロセス (コマンドラインに `sleep 6000` を含む) を拾い、
  子プロセスの stat 観測を誤る
- 解決: PID 起点で子を辿る
  ```
  pgrep -P <daemon_pid>   # daemon の子だけを列挙
  ```

### kill -STOP が「効かない」ように見える

- 現象: 子に `kill -STOP` しても止まらない
- 原因: daemon の auto-resume が即座に CONT を送り返していた
- 解決: daemon を先に止めてから子の挙動を観測する分離検証に切り替え

### Bash sandbox の signal 疑い

- 現象: Claude Code の Bash sandbox が外部プロセスへの signal を握り潰している疑い
- 解決: 対照実験 (自前で起動した子は `kill -STOP` で止まる) + `dangerouslyDisableSandbox` で
  sandbox 原因説を棄却

### rust-version を上げると clippy MSRV-aware lint が新発火

- 現象: `rust-version` を 1.88 に上げると、それまで通っていた clippy が新たな lint を発火
  (`collapsible_if` → let-chains 化を要求する等)
- 解決:
  ```
  cargo clippy --fix --allow-no-vcs   # jj リポは VCS 未検出扱いになるため --allow-no-vcs が必要
  ```

### run --detached の stdout 受けで Bash ツールがハング誤判定

- 現象: `run --detached` の stdout (socket path) を `$(...)` で受けると、daemon が stderr fd を
  握り続け、Bash ツールが「コマンド未終了」と誤判定してハング扱いになる
- 解決: fd を逃がす
  ```
  socket=$(hyoui run ... --detached 2>/dev/null </dev/null)
  ```

## 3. Ctrl-Z バグ: 根本原因確定 + session anchor 方針

二層構造の原因を確定:

1. orphan pgrp に対する SIGTSTP が discard される (kernel の job control 仕様)
2. daemon の auto-resume が即 CONT を送る

kawaz 実機 + 分離実験 (daemon を SIGSTOP で止めると子が止まったままになる) で証明。tmux も同一の
制限を持つことを実証した。

対策方針として **session anchor 案** (forkpty 廃止、daemon が `TIOCSCTTY` で制御端末を握る) を
macOS / glibc / musl の 3 platform PoC で実証し、本命方針に確定。

詳細: [findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md](../findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md)

## 4. release pipeline 修復 (push 後に連鎖発覚)

push 後に発覚した問題の連鎖と解決:

- **(a) rustfmt 未適用で lint-rust 失敗** → `cargo fmt` + squash
- **(b) MSRV 1.86 宣言が let-chains (1.88 stable) と矛盾** → 新設 MSRV ジョブが初回検出。
  rust-version を 1.88 に統一 + clippy の MSRV-aware lint 19 件を `--fix`
- **(c) Release workflow が v0.2.6 以降ずっと壊れていた** — `bump-semver get` が CI 環境限定で
  失敗。sha256 一致 binary + 一致入力でも GitHub Actions のみ失敗すると特定 (Docker エミュレータ
  では成功)。release.yml に診断 + perl 直読み fallback + `workflow_dispatch` を追加して
  v0.3.1 リリースを成立させた (4 platform バイナリ)。bump-semver 側には docs/issue/ 起票済み

## 5. docs-structure 準拠

監査の結果ほぼ準拠 (サブディレクトリ命名違反ゼロ)。本日の追加対応:

- CHANGELOG.md 削除 (canonical = kawaz/bump-semver の方針に統一、kawaz 決定)
- docs/STRUCTURE.md 新設
- 本 journal 補完

## 議論の要点

- **撤退判断より検証優先**: SIGPIPE 自爆説 / Bash sandbox signal 説はいずれも「機能を疑う」前に
  実機で棄却した (false positive を確定させてから次へ)。CLAUDE.md の検証主義に沿った進め方。
- **Ctrl-Z は kernel 仕様の壁**: tmux も同じ制限を持つことを実証した上で、forkpty を廃して daemon
  が制御端末を握る session anchor 案を採用。便利さでなく「必然」で介入する DR-0014 の透過原則に
  整合する方針。
- **release pipeline の故障は外部依存 (bump-semver) の CI 限定挙動が原因**だった。binary / 入力が
  bit-identical でも Actions のみ失敗 = 環境差。fallback 経路を持たせて自リポの release を先に
  通し、根治は bump-semver 側 issue に分離した。

## 次にやること

- [ ] session anchor 案 (forkpty 廃止 + daemon TIOCSCTTY) の本実装 (DR 化 → 実装)
- [ ] `--input-secrecy` (redact-after-prompt) の配線 (現状 no-op)
- [ ] CI の `#[ignore]` 統合テストの常時実行化の検討
- [ ] bump-semver 側 `get` の CI 限定失敗の根治 (docs/issue/ 起票済み)

## 関連

- [findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md](../findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md) — Ctrl-Z 二層原因 + session anchor PoC
- [issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md](../issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md) / [issue/2026-05-29-bug-ctrl-z-second-time-noop.md](../issue/2026-05-29-bug-ctrl-z-second-time-noop.md) / [issue/2026-05-28-bug-claude-tui-ctrl-z-followup.md](../issue/2026-05-28-bug-claude-tui-ctrl-z-followup.md) — Ctrl-Z 関連 issue
- [decisions/DR-0014-transparency-and-empirical-verification.md](../decisions/DR-0014-transparency-and-empirical-verification.md) — 透過原則 + 検証主義 (本日の判断軸)
- [REVIEW-BACKLOG.md](../REVIEW-BACKLOG.md) — レビュー指摘 backlog

## 追記: DR-0017 session anchor 実装完了 (同日夜)

DR-0017 起票 → 実装まで同日に完走。forkpty を廃止して `openpty` + 親側 `TIOCSCTTY` +
手動 fork (子: `setpgid`/`tcsetpgrp`/`dup2`/`execvp`) に変更し、auto-resume fallback を削除。
実機マトリクス (cat/sleep/python/bash/vim) で「^Z 1 回目から停止 → `list` に stopped 表示 →
`kill --signal=CONT --no-terminate` で session 継続のまま復帰 → 2 回目の ^Z も停止」を確認。
vim (raw mode TUI) も自前 handler の re-raise が anchor により機能して停止 = claude TUI と
同構造の本丸ケースが解決。

実装中の発見: `hyoui kill --signal=CONT` は signal 後に必ず session を terminate する仕様で、
DR-0017 の前提「外側 API で起こせる」が不成立だった → `--no-terminate` フラグを追加して
非 terminate の `ControlMessage::Signal` 経路を CLI に露出 (対極チェックの成果)。

残: 起動直後の SIGTTIN 一過性 (issue 起票済・実害なし)、signal.ack (issue 起票済)、
kawaz の実端末での attach ^Z 最終確認。
