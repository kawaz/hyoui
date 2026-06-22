# CLAUDE.md — hyoui プロジェクト Claude Code 必読ルール

> このファイルは Claude Code が **常時 context に読み込む**プロジェクトルール。
> hyoui の設計思想と実装判断の self-check を集約する。実装着手前に必ず該当 DR を読むこと。

## hyoui とは

PTY ラップした long-running process (claude code / vim 等) を **外側から透過的に**監視・自動操作する
Rust 製ツール。daemon が screen state を正本として持ち、CLI / 将来の HTTP gateway 経由で操作する。
Terminal multiplexer ではない (= tmux/screen の代替ではない)。

## 必読 DR (= 順番通り、設計判断前に毎回参照)

| DR | 内容 |
|---|---|
| [DR-0005](docs/decisions/DR-0005-design-philosophy-external-automation.md) | 思想 — 外側自動操作主軸、透明性最優先 |
| [DR-0001](docs/decisions/DR-0001-bgfg-jobcontrol-two-axis.md) | jobcontrol 2 軸 — 透過原則の例外として justify された介入の正本 |
| [DR-0013](docs/decisions/DR-0013-screen-emulator-and-attach-stability.md) | screen state 正本化 — ドッグフーディング道具の整備 |
| **[DR-0014](docs/decisions/DR-0014-transparency-and-empirical-verification.md)** | **本ルールの正本** — 透過原則の徹底 + 検証主義 + self-check |
| **[DR-0015](docs/decisions/DR-0015-run-as-fork-plus-attach.md)** | **`hyoui run` 構造変更** — fork daemon + attach client 合成、client/server 同居廃止 |
| [DR-0006](docs/decisions/DR-0006-cli-ground-rules.md) | CLI 設計 — input family / wait / screen / tail / lock |
| [DR-0008](docs/decisions/DR-0008-protocol-design.md) | protocol — CBOR framing / cap flags |
| [DR-0023](docs/decisions/DR-0023-child-env-scrub.md) | 子 PTY env scrub 初版 (= Superseded by DR-0024、CLI flag 過剰でredesign) |
| **[DR-0024](docs/decisions/DR-0024-env-scrub-config-file.md)** | **env scrub の config ファイル化 + CLI flag 最小化** — `--no-scrub-env` のみ残し、`~/.config/hyoui/config.toml` で target 別 `inherit_builtin` / `kill_glob` / `keep_glob` |

## 介入判断 self-check (= DR-0014 §self-check)

新規実装・修正で「介入する」コードを書く前に、**毎回**以下を確認する。1 つでも No なら設計を疑え:

- [ ] **この介入は既存 DR で justify されているか?** (= 該当 DR を引用できるか)
- [ ] **透過原則を破るが、その理由は「必然」か?** (= 「便利」「あった方が親切」では透過原則優先)
- [ ] **最小介入か?** (= 同じ効果をより少ない介入で実現する選択肢はないか)
- [ ] **kernel / PTY / shell の標準機能を再発明していないか?** (= SIGCHLD 受信 / PTY line discipline /
  shell job control 等)
- [ ] **新 protocol message / cap flag 追加なら、必然性を DR に書けるか?**
- [ ] **既存 DR で justify された機能のうち、未実装のものはないか?** = 新規介入より既存 DR の
  実装漏れ修復が優先 (= 撤退判断は最後の手段)

## 検証主義 (= DR-0014 §検証主義)

### 推測で実装しない

- サンプル 1 (= 例: claude TUI 1 つ) で結論を出さない
- **最低 3 種類の category で検証**: TUI alt screen 系 (vim/claude) / line-oriented 系 (cat/less) /
  interactive REPL 系 (python/bash)
- マトリクス検証: 関連する全組合せ (= app × mode × signal × 送信元) で「期待 vs 実態」を埋める

### 観測コマンド

| 観測対象 | コマンド |
|---|---|
| プロセス状態 | `ps -o pid,ppid,pgid,sid,stat,comm` |
| TTY 状態 | `stty -a < /dev/ttyXXX` |
| hyoui screen | `hyoui screen dump <session> --format=ansi` |
| hyoui state | `hyoui screen snapshot <session> --include=Cells,Cursor,Mode` |
| hyoui 出力履歴 | `hyoui tail <session> --last-bytes=N --since-strict` |
| shell jobs | `jobs -l` |

### partial state を扱う実装の規律

stalled / reset / 自動破棄系の実装 (= state を「壊れている」と判定して捨てる) は
特に慎重に扱う:
- default は **warn のみ + 手動操作** (= 自動破棄しない)
- 自動破棄が必要なら判定基準を DR に明示 + マトリクス検証で false-positive 検証
- 例: OSC52 巨大 paste / DCS sixel 部分送信 / ネスト sync update など、
  「子は正常だが時間がかかっている」ケースを false-positive で破棄しない

### コードと DR の双方向整合性

- A 方向 (= 新規介入 → DR justify 確認): 通常の self-check
- B 方向 (= DR → 実装エビデンス確認): INDEX を眺める、各 DR の機能を grep で実装箇所特定する
- 本リポでは過去に DR-0001 軸 1/2 が 6 日放置されたことあり、双方向整合性が重要

## Anti-patterns (= DR-0014 §Anti-patterns、繰り返し禁止)

実際にやらかした anti-pattern。今後 self-check で弾く:

1. **「daemon 監視 → 新 control message → client 操作」発明** = 親プロセスの kernel 標準機能
   (SIGCHLD 受信) の再発明、新 cap flag まで追加しようとしたケース
2. **claude TUI サンプル 1 で原因断定** = vim/less/cat/bash で検証せず推測で結論
3. **「マトリクス検証は別 task」と先送り** = 修正前にマトリクスを埋めず仮説を信じて実装に進む

## 道具揃った段階の運用 (= DR-0014 §ドッグフーディング)

DR-0013 完了で screen dump / snapshot / tail / wait 等の観測道具が揃った。
**以降は推測実装を禁止する**:

- 設計判断時: 既存実装を実機で動かし、状態取って判断材料にする
- bug 報告時: 報告者の cast / ログを Read で精読し、出力 bytes 単位で追う
- 修正実装後: 必ず実機で動作確認 + マトリクスの該当セル再検証
- 観測道具自体に bug があった場合: 道具を最優先で直す (= 道具が信用できないと判断が崩れる)

## jj-workflow

このリポは `.jj` あり、git bare + jj workspace 方式。`jj commit -m "msg"` 一発で確定 + 空 @ 前進。
`jj describe` 単体は過去 change の `-r <change>` 修正にだけ使う (= "commit したつもり"事故防止)。
詳細は `~/.claude-personal/rules/jj-workflow.md` / `jj-tips.md` を参照。

## push

`just push` を使う。直接 `git push` / `jj git push` 禁止 (= deps で check + test + 翻訳ペア
検証 + version bump 漏れ検出が走る)。

## 言語

日本語で応答 (= サブエージェントの指示応答・思考も)。kawaz の判断・指示も日本語。
