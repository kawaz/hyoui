---
title: hyoui-web が session 単位の操作・状態取得のたびに全 session 全 daemon へ status.query を投げる (list_sessions 全走査) のをやめる
status: resolved
category: bug
created: 2026-10-02T23:35:25+09:00
last_read:
open_entered: 2026-10-02T23:35:25+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-03T00:49:42+09:00
discard_reason:
pending_reason:
close_reason: v0.9.62 で実装 (session 単位要求は find_session で同名 socket だけ解決、GET /api/sessions/{id} 追加、一覧と 1 件解決を single-flight で束ねる。TTL キャッシュは入れない)
blocked_by:
origin: 自リポ TODO
---

# hyoui-web が session 単位の操作・状態取得のたびに全 session 全 daemon へ status.query を投げる (list_sessions 全走査) のをやめる

## 概要

hyoui-web が session 単位の操作・状態取得のたびに全 session 全 daemon へ status.query を投げる (list_sessions 全走査) のをやめる。

事実:

- `crates/hyoui-web/src/lib.rs` の `resolve_socket` が id→socket path 解決のために `hyoui::discovery::list_sessions` を呼ぶ。そのため `/api/sessions/:id/{screen,input,resize,resume,attach}` の各リクエストが host 上の全 daemon に connect + status.query する
- `session.js` は `refreshSessionStatus` を 5 秒ごとに `/api/sessions` (全走査) で回し、自分の 1 件だけ find している (コメント「専用エンドポイントを増やさず既存 API を再利用」)
- `index.js` は一覧を 3 秒ごとに全走査
- WS 未接続時は `fetchScreen` が 2 秒ごと (これも `resolve_socket` 経由で全走査)
- タブ数 × web daemon 本数 (stable/unstable) で掛け算になり、どれか 1 daemon が数秒 accept できないだけで listen backlog (5) が埋まる
- v0.9.57 ではそこで ECONNREFUSED → socket unlink となり生きた session が切断された (2026-10-02 run-24993-2b8fcf16、関連 issue `2026-09-30-list-prune-deletes-live-daemon-socket-on-backlog-full`)

端末描画自体は WS の raw stream で差分なので問題ない。問題は状態/メタ情報の取得経路。

## 背景

方針案:

1. `resolve_socket` は DR-0018 の配置規則から id を直接 path 解決し、その 1 socket だけ確認する
2. session 画面の状態 (child_stopped / clients / child_pid) は WS attach 中なら daemon からの通知で受ける。無ければ当該 1 件だけ query する専用経路にする
3. 一覧画面の全走査は頻度・同時実行 (多重 in-flight の抑止、web daemon 内の結果共有) を見直す

## 受け入れ条件

- [x] session 単位のリクエスト (`/api/sessions/:id/*`) が他 session の daemon に接続しない
- [x] session 画面の状態更新が全走査に依存しない
- [x] 一覧の全走査が多重 in-flight にならず、タブ数に比例して daemon への接続数が増えない
