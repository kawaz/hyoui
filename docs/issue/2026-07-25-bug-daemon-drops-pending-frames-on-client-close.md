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

## 原因 (2026-07-26 確定、daemon serve_loop に計装を入れて直接観測)

**上記「推定原因」がそのまま正しかった。** `input_auto_lock_cli` の 30s hang
(= [[2026-07-03-bug-macos-ci-flaky-pty-tests]] / [[2026-07-04-bug-flaky-outer-token-e2e-deadline]])
が同一原因であることも判明し、そちらで再現させて観測した。

serve_loop / accept / broadcast に一時トレースを入れ、失敗回の daemon 側イベントを取得:

```
[TR ...] handshake:promoted id=2 mode=Rw          ← 短命 client が client 化
[TR ...] enqueue WRITER_DEAD id=2                 ← daemon→client の write が失敗
[TR ...] TIMEOUT-branch overflow_id id=2
[TR ...] TIMEOUT-branch client_drop id=2 idx=0    ← 受信済み frame を読まずに破棄
[TR ...] poll:enter nclients=0 ...                ← 以降 client 0 のまま無限 poll
```

確定した機構:

1. 短命 client が `connect` → `send_control(Kill)` → 即 `drop(conn)` する
2. daemon は handshake 完了後、当該 client へ attach redraw / LeaderNotify を
   enqueue しようとするが、peer は既に close 済なので writer thread が死んでいて
   `EnqueueOutcome::WriterDead` になる
3. `handle_enqueue_outcome` が **WriterDead を disconnect の根拠にして**
   `overflow_ids` に push → 同一 iteration 内で `clients` から除去
4. その client の **socket 受信バッファに残っている Kill frame は読まれずに消える**
5. client が 0 になり、session を畳む契機が永久に来ない → 呼び出し側は deadline で fail

`send_control` が `Ok` を返すのは「socket に bytes を書けた」だけで、daemon が
処理したことを意味しない。そのため呼び出し側からは無言の欠落に見える。

**負荷依存の理由**: 低負荷では daemon が client の close より先に Kill frame を
読むため顕在化しない。高負荷 (= CI runner 3〜4 core) では close が先行する確率が上がる。

## 修正 (2026-07-26)

`EnqueueOutcome::WriterDead` を **disconnect の根拠にしない**ように変更した。

- `crates/hyoui/src/daemon/broadcast.rs` `handle_enqueue_outcome`
- `crates/hyoui/src/daemon/accept.rs` `send_attach_redraw`

根拠: socket は全二重で、write 半分が死んでいることは「client が既に送ってきた
frame」の有効性と無関係。正しい disconnect 点は reader 側の EOF であり、EOF 経路は
`frames_to_process` で受信済み frame を全て処理した **後**に client を drop するため
順序が保たれる。`Overflow` (= backpressure による意図的な切断) は従来どおり即 disconnect。

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
