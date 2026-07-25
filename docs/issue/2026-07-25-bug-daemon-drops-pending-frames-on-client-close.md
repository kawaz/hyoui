---
title: "BUG: 送信直後に切断した client の control frame が daemon で無言で捨てられる"
status: open
category: bug
created: 2026-07-25T00:00:00+09:00
last_read:
open_entered: 2026-07-25T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: web_e2e_api::e2e_resize_endpoint の flaky 調査 (2026-07-25)。web の resize endpoint 側は往復確認で回避済だが、daemon 側の一般則として残る
---

# BUG: 送信直後に切断した client の control frame が daemon で無言で捨てられる

## 症状

短命 client が「connect → handshake → control message を 1 発送信 → 即 close」した場合、
その message が **daemon に処理されないことがある** (= 無言で捨てられる)。

実測は `hyoui web` の `POST /api/sessions/:id/resize` (= `resize_blocking` が
まさにこの形だった) で発生し、HTTP は 204 を返すのに PTY / screen state の
window_size が変わらない、という形で observable だった。

## 実測 (2026-07-25、macOS、debug build)

`e2e_resize_endpoint` と同一手順を probe で再現 (fresh runtime dir / `run --detached` /
`hyoui web` / 404 → 400 → valid resize → snapshot 2s poll):

| 条件 | 結果 |
|---|---|
| そのまま (送信して即 close) | **ok=2/8** |
| 先行の 404 / 400 request を省く | ok=6/6 |
| 400 の後に 1s 空ける | ok=5/6 |
| 送信後に `StatusQuery` を 1 往復させてから close | **ok=8/8** |

- 失敗時の snapshot は初期値のまま (`{"window-size":{"rows":24,"cols":80},"serial":1}`)
  で、2s / 12 回 poll しても変わらない = **遅延ではなく欠落**
- **DR-0029 起因ではない**: 変更前 revision (`7c15f5a1`) の jj workspace を作って
  `e2e_resize_endpoint` を交互に 8 回ずつ実行した結果、どちらも同程度に落ちる
  (`resize_blocking` は DR-0029 の変更対象外)

  ```
  after (DR-0029) : F F F F P P P F   (3/8 pass)
  before(7c15f5a1): F F F F F P F P   (2/8 pass)
  ```
- `ClientConnection::send_control` は `flush()` 済なので、bytes は socket に出ている
- 直前に別 connection の accept / close があると失敗率が上がる = daemon 側の
  scheduling 依存

## 推定原因 (未確定)

daemon は client ごとに writer thread を持ち、write 失敗 (= EPIPE) を検知した client を
`overflow_ids` → `indices_to_drop` 経由で除去する。短命 client は control message を
書いた直後に close するため、**daemon が reader 側でその frame を読む前に
「writer が死んだ client」として drop される**経路があるとフレームが失われる。
`crates/hyoui/src/daemon/session.rs` の serve_loop (client revents 処理 → 
`frames_to_process` → `indices_to_drop` 適用) と `broadcast.rs` の writer thread 死活
判定の順序を確認すること。

## 回避済みの箇所

- `crates/hyoui-web/src/lib.rs` の `resize_blocking`: 送信後に `StatusQuery` を
  1 往復させてから close する (= frame は FIFO 処理なので、応答が返れば resize は
  処理済み)。2026-07-25 修正済

## 受け入れ条件

- [ ] drop 前に pending 受信 frame を drain する (or writer 死と reader 処理の順序を
      入れ替える) 形にできるか検討し、daemon 側で根治する
- [ ] 根治後、`resize_blocking` の往復確認が不要になるか判定 (= 不要なら戻す。
      ただし「204 を返す前に適用を確認する」こと自体は endpoint の正しさとして
      残す価値がある)
- [ ] 同じ形 (= 送って即 close) の経路が他にないか棚卸し (`hyoui detach` / `kill`
      の非 ack 経路 / `input` の一部)
