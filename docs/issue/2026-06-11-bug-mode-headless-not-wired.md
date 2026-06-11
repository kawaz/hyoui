# bug: run --mode=headless が未配線 (parse されるが何もしない)

- Date: 2026-06-11
- Status: open
- Priority: 高 (= usage に載っている公開機能が no-op。worker 用途の起動形に直結)
- 報告者: kawaz dogfooding 初日 (2026-06-11)

## 現象

`hyoui run --mode=headless -- claude` が headless にならず、普通に tty 上に attach
された claude が起動する。

## 原因

`Mode::Headless` は cli.rs で parse され (`--mode=interactive|headless`)、usage にも
--size/--cols/--rows (仮想画面サイズ、headless 用) や suspend/exit policy の headless
default まで記載されているが、**main.rs / daemonize.rs のランタイムで Mode を一切
消費していない** (grep で利用箇所 0 件)。= 設計済み・未実装の典型 (B 方向チェック違反)。

## 期待仕様 (usage / DR から復元して確定させる)

- headless = 外側 tty に attach しない (stdin/stdout が tty でなくても動く)
- 画面サイズは --size/--cols/--rows の仮想値 (外側 tty から取らない)
- policy default が変わる: on-child-suspend = auto-resume / exit = decouple (usage 記載)
- `--detached` との関係の整理が必要 (= detached は「fork して即戻る」、headless は
  「tty 非依存の動作モード」— 直交か統合か、DR-0006/0010 を読んで判断)

## TODO

- [ ] DR-0006 / DR-0010 等から headless の正式仕様を確認
- [ ] 配線実装 (winsize の仮想値、attach 抑止、policy default 切替)
- [ ] 仕様が固まるまでの暫定: 未配線のまま受理するのは誤解を招くため、
  実装完了までは `--mode=headless` を「未実装」エラーにする選択肢も検討
- [ ] 当面の回避: worker 用途は `--detached` + `--size` (実装されていれば) で代替

## 関連

- [[DR-0006]] / [[DR-0010]]
- 同型の前例: record redaction no-op (docs/issue/2026-06-10-feature-record-redaction-phase5.md)
