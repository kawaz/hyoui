# 2026-05-21 hyoui ブートストラップ

shimux の PTY プロキシ PoC（poc3）をベースに、`kawaz/hyoui` を新規リポとして起こした記録。

## 経緯

poc3 は `$SHELL` 固定起動の PTY プロキシだった。「SHELL 固定でなく
`hyoui -- cmd [args...]` で任意コマンドを実行したい」「外部から stdin を与えたい」
「tty を元の tty に反映しない headless 動作」という要望から再設計を始めた。

長い設計議論を経て、ツールの性格と主要な判断が固まった:

- **2 モード** — interactive（実 tty 透過プロキシ、既定）/ headless（実 tty なし・
  仮想スクリーンサイズ）
- **bg/fg ジョブ制御の 2 軸設計** — 「子が自分を suspend」「親が外部から suspend」を
  独立した軸とし、invariant「親が走行中なら子も走行中」で整理。→ **DR-0001** に詳述
- **プロジェクト名 "hyoui"（憑依）** — 命名は多くの候補を経て決定。→ **DR-0002** に詳述

実装は機能ごとにサブエージェントへ委譲して進めた:
1. リポ作成（jj bare 方式）
2. poc3 のコード取り込み（FFI シンボル `shimux_*` → `hyoui_*` リネーム）
3. コア実装（argv 渡し / モード / CLI パーサ / stdin EOF→^D / bg-fg シグナル / 停止条件）
4. canonical Taskfile.pkl（pkfire 0.10 / pkf-tasks 3.0.3）整備
5. ビルド・動作確認・初回 push

## 設計判断

設計判断は DR に記録した:

- **DR-0001** — bg/fg ジョブ制御の 2 軸設計と invariant「親 fg ⇒ 子 fg」。
  軸 1 `--on-child-suspend=follow|auto-resume`、軸 2 `--on-parent-suspend=transparent|decouple`、
  モード別デフォルトとその根拠、poc3 時代の `nosuspend` の教訓の取り込み。
- **DR-0002** — プロジェクト名 "hyoui" の決定。命名議論で検討した多数の不採用候補と理由。

## ハマり所 → 解決策

| 詰まり | 解決 |
|---|---|
| SIGCHLD 中の `waitpid` が EINTR で偽の失敗 → exit 1 | `proc_waitpid` を EINTR リトライに |
| poll が EINTR でループ即抜け（シグナルで即終了） | `io_poll` が EINTR を -2 で区別、ループ側で非致命扱い |
| `--socket /tmp/foo.sock` が `sock_listen failed` | 仕様。`hyoui_sock_listen` は親ディレクトリが 0700 かつ自分所有を要求（W15 セキュリティチェック）。/tmp（1777）は不可。0700 ディレクトリを使う |
| `moon build`（パッケージ指定なし）が `undefined _main` | lib パッケージの `link` ブロックを moon が単独実行ファイル化しようとする。lib のテストが FFI をリンクするため link 設定は残さざるを得ず、`moon build --package kawaz/hyoui/cmd/agent` で運用。各 moon.pkg.json に `_comment` で理由を記録 |
| 初回 push で `semver:check-version-bumped` が落ちる | pkf-tasks の `kawaz.semver.checkBumped` が `main@origin` 不在（初回 push リポ）+ `set -e` + 複合コマンドの command substitution で落ちる。ローカル task に差し替えて回避。pkf-tasks 側に issue 起票済み |
| `moon fmt` と `moon check` の `moon.pkg` スキーマ不一致 | `moon fmt` が新形式へマイグレートする `supported_targets` を `moon check` が拒否。lint から `moon fmt` を外し `moon check` のみに。moon 最新化で解消するか要確認（issue 起票済み） |

## 仕様の限界

- stdin EOF → PTY へ `^D`(0x04) 送出。canonical モードの子には EOF 相当だが、raw 入力で
  動く子には literal 0x04 として渡る（コードに Design rationale コメント済み）。

## ビルド・テスト

```
(cd ffi && cargo build --release)
moon build --target native --package kawaz/hyoui/cmd/agent
moon test --target native   # 64/64 passed
pkf run ci                  # lint + test + build
```

動作確認済み: echo / headless cat（パイプ stdin・EOF→^D）/ --timeout 124 / --idle-timeout 124 /
--until 0 / socket 注入 / on-child-suspend=auto-resume / on-parent-suspend=transparent（TSTP で
子も停止・CONT で再開）。ゾンビ残存なし。

## 残課題

`docs/issue/` に起票済み:
- CI / release ワークフロー整備
- lint に `moon fmt` を復活（moon toolchain 最新化）
