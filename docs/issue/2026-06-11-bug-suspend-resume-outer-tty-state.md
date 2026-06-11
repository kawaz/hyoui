# bug: attach の suspend/resume で外側端末の状態を管理していない

- Date: 2026-06-11
- Status: wip
- Priority: **最高** (= DR-0017 で ^Z 自体は効くようになったが、suspend 後の shell が壊れ、
  fg 後は操作不能になる。^Z 機能の実用性がこれで決まる)
- 報告者: kawaz 実機検証 2026-06-11 (claude TUI、ghostty)

## 現象 (v0.4.0)

`hyoui run -- claude` で ^Z → suspend → 外側 shell に戻る (= DR-0017 の follow は機能) が:

1. **shell に戻った後、カーソル位置が変・`ls` で画面がぐちゃぐちゃ**
   → attach が `raise(SIGSTOP)` する前に外側端末の termios を復元していない
   (= raw mode のまま shell に返している。echo 無し・ONLCR 無し)
2. **fg 後、claude の入力欄に `^[[99;5u^[[100;5u^[[122;5u...^[[?62;22;52c` が出て操作不能**
   → 複合要因:
   - 外側端末 (ghostty) の kitty keyboard protocol が有効なまま suspend を跨いでおり、
     fg 後に ctrl+c/d/z が CSI u シーケンス (`99;5u` = ctrl+c 等) として流れ、
     子 PTY の line discipline が制御文字として認識しない (= **ctrl+c/d で殺せない**)
   - DA 応答 (`^[[?62;22;52c`) 等の端末問い合わせ応答が入力に化けて混入
   - resume 後に attach が外側 termios の raw 再設定・daemon への redraw 要求をしていない

## TODO

- [ ] suspend 前: 外側端末の termios を保存値に復元 (= detach 時と同じ処理) してから
      `raise(SIGSTOP)`。端末モード (alt screen / kitty protocol 等) のリセット escape も
      daemon の screen state (vt100 mode 追跡) から導出して吐く (= detach 経路に同等処理が
      あればそれを流用)
- [ ] resume 後 (= SIGCONT 復帰): 外側 termios を raw に再設定 → daemon に redraw 要求
      (= attach handshake redraw (DR-0013 Phase A) と同じ機構の再実行。モード復元
      (alt screen / kitty protocol 再有効化) が redraw に含まれるかを確認、含まれなければ追加)
- [ ] ResumeRequest 送信は redraw 要求と順序を整理 (= 子を起こす前に画面を整えるか、後か)
- [ ] 実機マトリクス: claude / vim / less で suspend → shell 操作 → fg → 入力可能、を確認

## 観察手がかり

- kawaz の cast: /tmp/hyoui-claude-test1.cast (旧バイナリ分) + 2026-06-11 の新 cast
- attach の既存 detach 処理 (Ctrl-A D) は termios 復元をしているはず → suspend にも同じ処理が必要
- attach handshake redraw (DR-0013) が再 attach 時の画面復元をしている → resume 後の再描画に流用可能

## 関連

- [[DR-0017]] — ^Z 本体の修正 (本 issue はその仕上げ)
- [[DR-0013]] — screen state 正本化 / attach redraw (resume 後の再描画に使う機構)
