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

## 監査結果 (2026-06-11 二次追記)

Fable + Codex の 2 系統監査を実施。本 issue の範囲を超える no-op オプション群
(--on-child-suspend / --timeout / --idle-timeout / --exclusive / --detach-others) と
シグナル系の bug 候補 (SIGWINCH 未配線、daemon SIGTSTP 吸い込み、daemon kill -9 →
child SIGHUP 巻き添え疑い等) が見つかった。全容と整理提案は
**[[2026-06-11-signal-suspend-interaction-audit]] (docs/findings/)** を正本とする。
本 issue は `--mode` 削除の実施票として継続。

## 調査メモ (2026-06-11 追記)

DR / コードを精読した結果、期待仕様の確定に効く事実:

- **DR-0001 の `--mode=headless` 定義は「suspend 系 preset」であって「attach しない」ではない**。
  preset 表は 軸1 = `auto-resume` / 軸2 = `decouple`。「tty に attach しない」という意味は
  DR-0001 には無い (fg で attach されること自体は DR-0015 の run = fork daemon + exec attach
  仕様通り。detach は `--detached` の担当)。
- **その preset 自体も DR-0017 §柱2 で意図的に廃止済み** (cli.rs:2285-2289 のコメント:
  「どの mode でも default にはしない (= headless でも勝手に resume しない)」)。
  つまり DR-0001 が headless に与えていた意味は現方針では空になっており、
  「未配線」ではなく「**配線すべき仕様が現存しない**」状態の可能性がある。
  → 期待仕様の復元は DR-0001 からではなく、「headless に今どんな意味を持たせたいか」の
  再定義 (= DR 起票) が先。
- **pipe-through pattern も未配線**: attach.rs のコメント / テスト (R5-FB2) に
  `echo "1+2" | hyoui run --mode=headless -- bc` 用の `StdinEofAction::SendEof` が
  実装済みだが、production 経路から `with_stdin_eof_action(SendEof)` を呼ぶ箇所が 0 件。
  headless の意味候補の 1 つとして「stdin EOF で EOT 送出 (pipe-through)」がここに眠っている。
- **run → exec attach で mode が伝搬されない**: run_command (main.rs:500-520) が組む
  `hyoui attach` 引数は session / socket / namespace / debug-dump-client のみ。
  attach 側に mode を解釈させる設計にするなら伝搬の追加が必要。

## 関連

- [[DR-0006]] / [[DR-0010]]
- [[DR-0001]] (headless preset の原典) / [[DR-0017]] (preset 廃止) / [[DR-0015]] (run = fork + exec attach)
- 同型の前例: record redaction no-op (docs/issue/2026-06-10-feature-record-redaction-phase5.md)
