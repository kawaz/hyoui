---
title: pipe_send_eof_default_terminates_bc が macos-latest CI で bc 互換性問題により失敗
status: resolved
category: bug
created: 2026-06-30T11:00:51+09:00
last_read:
open_entered: 2026-06-30T11:00:51+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-07-03T18:58:00+09:00
discard_reason:
pending_reason:
close_reason: "誤診断だった。真因は bc 互換性ではなく attach run loop の RawAck 未処理 (archive/2026-07-03-bug-attach-run-loop-drops-rawack.md) で、client が入力 forward 直後に死んで stdout が空になっていた。RawAck fix + test の bc 非依存化 (sh read-loop、pipe_send_eof_default_terminates_child に rename) で解消、pipe e2e 3 件 green"
blocked_by:
origin: 自リポ TODO
---

# pipe_send_eof_default_terminates_bc が macos-latest CI で bc 互換性問題により失敗

## 概要

`crates/hyoui-cli/tests/stdin_eof_pipe.rs::pipe_send_eof_default_terminates_bc` が
macos-latest CI 環境で deterministic に failure する。

## 背景

- CI run id: 28413666305 (= 2026-06-30 main commit ab46b529)
- 失敗 step: `Ignored tests (macos-latest / PTY+daemon)`
- 失敗位置: `stdin_eof_pipe.rs:120`
- 結果: `test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out`

ローカル ベースライン検証 (= 2026-06-29 workflow Implement agent 観測) によれば、
macOS 環境の bc (Howard bc v7.0.3) は GNU bc (= ubuntu) と異なり `>>>` プロンプトを
出す等の挙動差があり、test が期待する `stdout.contains('3')` が成立しない。

本 commit (= DR-0025 Draft land、docs/CI 変更のみ) による回帰ではなく、bc 種別固有の
deterministic 失敗。

## 受け入れ条件

- [ ] macos-latest CI で `pipe_send_eof_default_terminates_bc` が pass する (または明示的に skip/ignore で理由が記録される)
- [ ] test の本来意図 (= pipe stdin EOF で子が自然 exit する) が保持される
- [ ] test-failure-no-tampering 規約を満たす (assert 緩和による偽 green は禁止)

## 対応候補

1. **GNU bc 専用テスト化**: test に `#[cfg(target_os = "linux")]` を付与 (= macOS で skip)、
   GNU bc の振る舞いだけを仕様として固定
2. **bc 種別非依存化**: 入力 / assert を bc バージョン差異に robust な形に書き換え
   (= ただし test-failure-no-tampering 規約上、改変前後で検証意図が保たれることを明示する
   必要あり)
3. **bc を使わない別の chardev 子プロセス**: `/dev/null` や `/bin/cat` 等で同等の
   stdin EOF 動作を検証 (= test の本来意図 = pipe stdin EOF で子が自然 exit する、
   bc である必要性は副次的)

## 関連 DR

- DR-0019 §5 (`stdin-eof=detach|send-eof` policy)
- 旧実装の C-1 再現テスト (= pipe stdin で client が CPU spin せず、SendEof default で子が
  自然 exit する) の検証目的

## Close 時の訂正 (2026-07-03)

本 issue の「bc 互換性問題」という診断は誤りだった。実測での真因:

- client (attach run loop) が daemon の RawAck (DR-0021) を unknown frame 扱いして
  入力 forward 直後に exit 1 → 子の出力を中継する前に死ぬ → stdout 空 → assert fail
- bc の種別 (GNU / Howard / BSD) は無関係。sh read-loop 置換後も修正前 binary では
  同一 failure を再現、RawAck fix 後に green
- 「macos のみ」に見えたのは ubuntu ignored job が backpressure deadlock で先に
  fail し当該 test まで到達していなかったため

詳細: archive/2026-07-03-bug-attach-run-loop-drops-rawack.md
