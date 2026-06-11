# feature: attach --exclusive / --detach-others の実装

- Date: 2026-06-12
- Status: open
- Priority: 低 (= 未実装エラー化済みで silent no-op は解消済。実装は需要が出てから)
- Origin: DR-0019 §6 (= 「実装自体は別 issue に切り出す」の実施票)

## 内容

DR-0006 §5 で構想された attach 時の排他制御:

- `--exclusive`: 自分以外の rw client が居る場合に attach を拒否
- `--detach-others`: attach 成立時に他 client を detach させて奪取

現状は parse 段で「未実装」エラーを返す (DR-0019 §6、DR-0004 の予約エラー流儀)。
HandshakeRequest には dead field として `exclusive` / `detach_others` が wire に乗っている
(daemon 側に読むコードが無い)。

## 実装時の論点

- daemon 側: `Detach{target: Others}` の部分実装 (`DetachTargetPartial` エラー) の完成が前提
- leader cascade との相互作用 (奪取側が leader を取るか)
- 既存 dead field を使うか handshake を作り直すか (v1.0 未満 breaking OK)

## 関連

- [[DR-0006]] §5 (原典) / [[DR-0019]] §6 (未実装エラー化の判断)
