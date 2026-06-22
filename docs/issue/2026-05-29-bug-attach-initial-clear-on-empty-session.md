---
title: "bug: hyoui run / attach の初期 redraw で画面が clear される"
status: open
category: bug
created: 2026-05-29T00:00:00+09:00
last_read: 2026-06-22T21:40:45+09:00
open_entered: 2026-05-29T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 実機検証 2026-05-29
---

# bug: hyoui run / attach の初期 redraw で画面が clear される

- Priority: 中 (= UX 悪化、動作には影響なし)

## 現象

```bash
# 期待: prompt 直後に hyoui run コマンドだけ残って sleep 待機
$ ./target/release/hyoui run --session=test2 -- /bin/sleep 6000

# 実態: 上記実行と同時に画面が clear (= cursor home + clear screen) される
```

`/bin/sleep` のような **何も出力しない子コマンド** でも attach 直後に外側
shell の画面 history が消える。

## 原因 (= ほぼ確定)

DR-0013 Phase A の attach handshake redraw:

1. attach client が daemon に handshake.request 送信
2. daemon が `build_attach_redraw` で `redraw_bytes` 構築 (= `state_formatted()` ベース)
3. `ScreenStateInit` control message に redraw_bytes を含めて返信
4. attach client が redraw_bytes を stdout に書く

`state_formatted()` は vt100 の **clear screen + cursor home + 各 cell の SGR + cursor 位置**
を bytes として吐く (= 既存 alt screen 復元用 protocol)。新規 session で **子が 1 byte も
出力していない** 場合でも、空 grid の redraw として「clear + home」が出る。

外側 shell の TTY = client の stdout なので、その bytes がそのまま流れて画面 clear。

## 期待挙動

- 子が **何も出力していない** (= screen state が初期状態のまま) なら redraw_bytes を送らない
- 子が出力済 (= alt screen app 起動済 etc) なら従来通り redraw を送る

## 修正方針 (= 候補)

### 案 A: daemon 側で空 state 判定

`build_attach_redraw` の中で screen が「初期状態 (= 全 cell empty + cursor (0,0) +
alt screen OFF + mode 変更なし)」と判定したら `redraw_bytes = vec![]` (= 空) を返す。
client は空なら stdout に何も書かない。

実装場所: `crates/hyoui/src/daemon/screen/virtual_screen.rs` の `build_attach_redraw`
相当 + 空判定 helper。

### 案 B: client 側で「screen mode 系 bytes だけ」を skip

client が redraw_bytes を受信した時、内容を解析して「clear / home / SGR reset / mode 切替」
のみで visible char 0 なら stdout に書かない。

→ 複雑、案 A の方が筋。

### 案 C: 子が 1 byte でも出力したか daemon 側で track

`crate::daemon::session.rs::serve_loop` で master_bytes_read counter を持ち、
> 0 なら redraw、= 0 なら skip。

→ 子の output 経路が増えれば不要かも、案 A と組み合わせ可。

## 推奨

**案 A**: vt100 state を判定する helper を追加、空なら redraw_bytes を空に。
副次的に「子の出力 = 純改行のみ」「子が clear した直後」等の corner case でも
画面 history を保護する効果あり。

## 再現

```bash
# どの短命/長期子でも同じ症状 (= 子が出力する前に attach 完了 → 空 state を redraw)
./target/release/hyoui run -- /bin/sleep 30
# → 外側 shell の画面 history が消える
```

claude TUI のような alt screen 常駐 app は **alt screen mode に入ってから出力する**ので
従来通り redraw が必要 (= 画面復元の主目的)。判定 helper は alt screen 状態でも
正しく動かす必要あり。

## 関連 file

- `crates/hyoui/src/daemon/screen/virtual_screen.rs` (= state_formatted, build_attach_redraw)
- `crates/hyoui/src/daemon/accept.rs::finalize_accepted_client` (= redraw 送信経路)
- DR-0013 §4 attach 復元 protocol
