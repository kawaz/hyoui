# BUG: auto-resume / 外部 SIGCONT 後も `child-state: stopped` が恒久的に下りない

- Status: Open
- Date: 2026-06-12
- Priority: Middle (実プロセスは正常走行、表示だけが矛盾。SUSPEND 可視化 (DR-0019 Update) と組み合わさるとユーザを混乱させる)
- 発見元: DR-0019 Update (set/可視化) 実装の push 前 Fable レビュー (2026-06-12)。本 diff 起因ではなく v0.6.3 に既存

## 症状

子が SIGSTOP → SIGCONT で復帰した後も、`hyoui status` / `hyoui list` の
`child-state` / `child_stopped` が **stopped のまま下りない** ケースがある。

再現 (いずれも attach client 不在の detached 状況):

1. **auto-resume policy**: `--on-child-suspend=auto-resume` (or `hyoui set` で変更) の
   worker に `kill -STOP <child>` → daemon が即 SIGCONT、`ps` は `S+` (走行中) に戻るのに
   `child-state: stopped` のまま (3 秒待っても変わらず)
2. **notify policy + 外部 SIGCONT**: stopped の子を `kill -CONT <child>` で手動復帰
   させても同様に stopped 表示が残る

実プロセス状態 (`ps -o stat`) は正しく `S+` なので、**flag の clear 漏れ** (表示系のみ)。

## 推定原因

`SessionState::child_stopped` は `notify_child_stopped` で `true`、
`record_child_continued` で `false` にする設計 (DR-0017 §柱2)。
`pty.rs` の lifecycle poll は WCONTINUED を持つが、**detached 状況 (= client 不在 /
serve_loop が idle) で Continued transition を拾えていない**模様。

- auto-resume 経路 (`notify_child_stopped` 内 killpg(SIGCONT)) は「子の Continued は
  次回 poll の `record_child_continued` 経路で記録される」前提だが、その poll が
  発火していない
- 外部 SIGCONT も同様 (= daemon は SIGCHLD/WCONTINUED で観測するはずの transition を
  取りこぼしている)

該当コード: `crates/hyoui/src/daemon/session.rs` (`record_child_continued` 呼び出し経路、
`ChildTransition::Continued` の match arm)、`crates/hyoui/src/sys/pty.rs` 系の
WCONTINUED poll。

## 影響

- DR-0019 Update の SUSPEND 可視化と矛盾する表示: 「`on-child-suspend: auto-resume`
  なのに `child-state: stopped`」= ユーザは「auto-resume が効いていない」と誤認する
  (= 可視化機能の動機である誤運用検出を逆方向に汚染)
- DR-0017 柱 2 の可観測性要件 (stopped → resumed は record + list/status で観測可能)
  の実装漏れ側面

## 検証メモ (2026-06-12 実機)

| policy | 操作 | ps STAT | status child-state | 期待 |
|---|---|---|---|---|
| auto-resume | kill -STOP | S+ (復帰済) | stopped ❌ | running |
| notify | kill -STOP | T+ | stopped ✅ | stopped |
| notify | kill -STOP → kill -CONT | S+ | stopped ❌ | running |

## TODO

- [ ] detached (client 不在) で WCONTINUED transition が拾えない root cause 特定
  (self-pipe / poll timeout 経路の lifecycle poll 条件を確認)
- [ ] `record_child_continued` 発火の実機確認 (lifecycle event `child-continued-observed`
  が jsonl に出るか)
- [ ] 修正 + マトリクス再検証 (attached / detached × auto-resume / notify × STOP/CONT)
