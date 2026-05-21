# 2026-05-21 hyoui ブートストラップ

shimux の PTY プロキシ PoC（poc3）をベースに、`kawaz/hyoui` を新規リポとして起こした記録。

## 経緯

poc3 は `$SHELL` 固定起動の PTY プロキシだった。これを次の性格のツールに作り直した:

- `hyoui -- cmd [args...]` で任意コマンドを PTY 内実行
- interactive（実 tty 透過プロキシ）/ headless（実 tty なし・仮想サイズ）の 2 モード
- 「子に憑依して一体で動く」= 親子が一蓮托生で生き死にする

## 設計判断（要点）

### bg/fg ジョブ制御 — invariant「親 fg ⇒ 子 fg」

子は `POSIX_SPAWN_SETSID` で独立セッションのリーダー（PTY を制御端末に持つため必須）。
よってジョブ制御は「外側（hyoui 自身）」「内側（PTY 内の子）」の 2 層に分かれる。

不変条件を **「親 hyoui が走行中なら子も走行中」** と定義すると、設定軸が 2 本に整理できる:

- `--on-child-suspend=follow|auto-resume` — 子が自分を suspend したとき
  - `auto-resume`: 親が即 SIGCONT（poc3 時代の `nosuspend` 相当。headless 既定）
  - `follow`: 親も自分に SIGSTOP を raise（外側シェルに制御が戻る。interactive 既定）
- `--on-parent-suspend=transparent|decouple` — 親が suspend されたとき
  - `transparent`: 子 pgrp にも SIGSTOP（interactive 既定）
  - `decouple`: 子はそのまま（headless 既定）

invariant 回復は SIGCONT ハンドラに集約: 親再開時に子が STOPPED なら必ず SIGCONT。
これで「親 fg・子 stopped」の禁則状態が論理的に発生しない。

### nosuspend の教訓

poc3 時代、`exec claude` で PTY 内にシェルがいないと、claude が自分を suspend したとき
誰も fg できず詰む問題があり、別ツール `nosuspend`（`waitpid(WUNTRACED)` → 即 SIGCONT）で
対処していた。hyoui ではこれを `on-child-suspend=auto-resume` として内蔵。

## ハマり所 → 解決策

| 詰まり | 解決 |
|---|---|
| SIGCHLD 中の `waitpid` が EINTR で偽の失敗 → exit 1 | `proc_waitpid` を EINTR リトライに |
| poll が EINTR でループ即抜け（シグナルで即終了） | `io_poll` が EINTR を -2 で区別、ループ側で非致命扱い |
| `--socket /tmp/foo.sock` が `sock_listen failed` | 仕様。`hyoui_sock_listen` は親ディレクトリが 0700 かつ自分所有を要求（W15 セキュリティチェック）。/tmp（1777）は不可。0700 ディレクトリを使う |
| `moon build`（パッケージ指定なし）が `undefined _main` | lib パッケージの `link` ブロックを moon が単独実行ファイル化しようとする。lib のテストが FFI をリンクするため link 設定は残さざるを得ず、`moon build --package kawaz/hyoui/cmd/agent` で運用。各 moon.pkg.json に `_comment` で理由を記録 |

## 仕様の限界

- stdin EOF → PTY へ `^D`(0x04) 送出。canonical モードの子には EOF 相当だが、raw 入力で
  動く子には literal 0x04 として渡る（コードに Design rationale コメント済み）。

## ビルド・テスト

```
(cd ffi && cargo build --release)
moon build --target native --package kawaz/hyoui/cmd/agent
moon test --target native   # 64/64 passed
```

動作確認済み: echo / headless cat（パイプ stdin・EOF→^D）/ --timeout 124 / --idle-timeout 124 /
--until 0 / socket 注入 / on-child-suspend=auto-resume / on-parent-suspend=transparent（TSTP で
子も停止・CONT で再開）。ゾンビ残存なし。
