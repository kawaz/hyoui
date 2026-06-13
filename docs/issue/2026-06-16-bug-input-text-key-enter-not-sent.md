# bug: `hyoui input` で `text:` と `key:Enter` を 1 invocation で連続指定すると Enter が効かず text が input box に残る

- Date: 2026-06-16
- Status: wip (方針確定: PTY drain ack 採用、実装中)
- Severity: 中 (= 自動化の主要ユースケースで送信できない、AI agent からの claude TUI 自動操作で詰まる)
- Reporter: kawaz (cmux-msg → hyoui era 移行の動作練習で発見)
- バイナリ: hyoui 0.6.5 (brew)

## 現象

`hyoui input <session> "wait-idle:500ms" "text:<長文>" "key:Enter"` を 1 invocation で実行すると、
**text 部分は claude TUI の input box に正しく入力されるが、`key:Enter` が効かず prompt が送信されない**。
text が input box に残ったまま claude が待機状態になる。

## 再現環境

- 親: 既に hyoui 配下で動いている別の claude session (cmux-msg メイン session)
- 子: hyoui run で別 cwd に起動した claude session

```bash
CHILD_SID=$(uuidgen | tr A-Z a-z)
CMUX_WORKSPACE_ID=hyoui-era \
hyoui run --detached --namespace=cmuxmsg-test --session=cmsg-child-${CHILD_SID:0:8} -- \
  claude --session-id "$CHILD_SID" --dangerously-skip-permissions

# 起動後 5 秒程度待ってから注入
sleep 5

hyoui input "cmsg-child-${CHILD_SID:0:8}" --namespace=cmuxmsg-test \
  "wait-idle:500ms" \
  "text:cmux-msg list で受信箱を確認してから、最初のメッセージを read して指示通り作業を進めてください。subscribe は Monitor で張って継続してください。" \
  "key:Enter"
```

実行直後の hyoui status:

```
session-id: cmsg-child-dbb28eda
child-state: running
lock-holder: (none)        ← lock 競合は無い
clients: id=5 mode=Ro      ← attach 中の RO client (cmux-msg の入力注入とは別経路)
```

## 期待挙動

text spec の入力が完了したのち、続く `key:Enter` が確実に flush され、claude TUI の prompt が送信される。

## 実際の挙動

`hyoui tail --last-bytes=2000` の抜粋 (ANSI strip 後):

```
[<u[>1u[>4;2mcmux-msg list で受信箱を確認してから、最初のメッセージを read…
                                                              ⏵⏵ bypass permissions on
                                                              0 tokens
                                                              /rc active
```

- text 全文が input box に表示されている (= 入力自体は届いた)
- prompt は送信されていない (= Enter が落ちている、または無視された)
- claude は input 待ち状態のまま
- `[<u[>1u[>4;2m` は kitty keyboard protocol / mouse cap negotiation のシーケンス

## 推測される原因 (要 hyoui 内部調査)

仮説 A: **text spec の完了待ちと key spec の送出が同期していない**
- text 内部の flush タイミングと key 送出が race している
- 結果として key:Enter が text 完了前 or claude TUI の input buffer フラッシュ前に届き、捨てられている

仮説 B: **claude TUI 起動直後の cap negotiation との競合**
- 起動直後 claude TUI が kitty kbp / mouse mode の問い合わせ (`[<u`, `[>1u`, `[>4;2m`) を送出している最中
- この期間に流れた key:Enter が cap negotiation のレスポンスとして解釈される or 捨てられる
- (注: 子の起動から input 実行まで 5 秒程度待ったが、cap negotiation のタイミング保証は不明)

仮説 C: **bracketed paste と key 入力の混在**
- text spec が内部的に bracketed paste を使っているなら、paste 終了マーカー後に key:Enter が来るタイミングで挙動が変わるかも

## ワークアラウンド候補 (要検証)

1. text と key を別 invocation に分ける:
   ```bash
   hyoui input <sess> "text:..."
   sleep 0.5
   hyoui input <sess> "key:Enter"
   ```
2. text と key の間に `wait-idle:` を挟む:
   ```bash
   hyoui input <sess> "text:..." "wait-idle:200ms" "key:Enter"
   ```
3. text 部分を `paste:` に切り替える (text spec と paste spec で内部処理が違うなら別挙動の可能性)

## 影響範囲

- AI agent から claude TUI への自動 prompt 注入 (= 親 agent → 子 agent の指示渡し) の主要ユースケースで詰まる
- 特に複数 spec を 1 invocation にまとめて投げる用途 (= hyoui input の本来の表現力) を活かせなくなる

## 参考: cmux-msg 側の context

cmux-msg は hyoui era に移行中 (= 旧 cmux 依存を全廃し、tell 相当の操作を `hyoui input` に委譲する方針)。
cmux-msg の本文 (= ファイル受信箱を経由する send/subscribe) は本件の影響なく正常動作した。
本件は `hyoui input` での「最初の prompt 注入」(= 子 claude を起動して subscribe を張らせる用途) で詰まる。

## TODO

- [x] 仮説 A/B/C のどれが実態か、hyoui 内部の input spec 実装 (crates/hyoui-cli or hyoui daemon 側) を読んで判定
  - **結論: 仮説 A (sequencing race) が確定**。より正確には「bytes 系 spec の完了点が socket flush で、PTY drain を待っていない」という仕様の手抜き。
  - 根拠: `main.rs:3161-3174` dispatch_spec ループ → `attach.rs:944-950` send_raw_bytes は socket writer.flush 成功で即 return → 次 spec へ。daemon `control.rs:173-175` の `TYPE_RAW_DATA` handler は master PTY fd への write_all が並行処理、ack 機構ゼロ。長文時は line discipline buffer (4-8 KiB) の消費が間に合わず、`\r` が text の bytes consumption 完了前に届く。
  - `text:` spec は bracketed paste を使わないため (`input_handlers.rs:66`)、仮説 C は否定。
  - 仮説 B (cap negotiation) は `wait-idle:500ms` を先頭に置いているため起動直後 race は除外。
- [x] ワークアラウンド 1-3 を 1 件ずつ実機検証
  - v0.6.5 (`/opt/homebrew/bin/hyoui`) で control 含め 9 回試行、**全て成功** (= 再現せず)。kawaz 報告時の特定条件 (prompt 長 / cap negotiation タイミング / session 状態) で踏む flaky と推定。
  - 最堅牢: W1 (別 invocation + `sleep 0.5`)。次点: W2 (`wait-idle:200ms` を text と key の間に挟む、1 invocation で完結)。W3 (paste:) も成功。
  - `wait-idle:500ms` を**先頭**に置くと claude TUI のスピナーで常時 timeout (exit 1) するが、後続 text/key は実行される (= 仕様罠、別件 issue 化検討)
- [ ] **修正方針確定: PTY drain ack (kawaz と合意済、2026-06-16)**
  - bytes 系 spec (`text:` / `paste:` / `hex:` / `file:` / `key:` 等) の完了点を **daemon の master fd への write_all 完了 ack 受信** に変える
  - 暗黙 wait-idle 追加ではなく、spec 意味論の完成 (= DR-0014 透過原則と整合、DR-0006 §8.6 spec sequencing 完了点の明文化)
  - v1.0 前なので protocol breaking OK
- [ ] 実装後の e2e test: `text:長文 → key:Enter` 1 invocation で race なく Enter が `\r` として text 最終バイト後に届くこと
- [ ] DR-0006 §8.6 / DR-0008 に ack 機構を明文化
- [ ] MANUAL-ja.md / MANUAL.md の input family 章を ack 前提に書き換え (= 1 invocation で race しないことを明示)
- [ ] 修正完了後、本 issue file を delete + journal に経緯昇華 (= `docs-knowledge-flow.md`)

## 関連調査ログ

- 実装読み (2026-06-16): `input_handlers.rs:66` text → bracketed paste 不使用 / `:499` key:Enter → `\r` 1 byte / `main.rs:3161-3174` dispatch_spec ループ → ack 待ち無し / `wait_core.rs:342-382` wait-idle は明示 spec 時のみ
- 実機検証 (2026-06-16): v0.6.5 で再現せず、根本因は実装の race 素地で確定
