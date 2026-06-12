# feature: attach --exclusive / --detach-others の実装

- Date: 2026-06-12
- Status: done (= DR-0020 §4 で実装済、2026-06-12)
- Priority: 低 (= 未実装エラー化済みで silent no-op は解消済。実装は需要が出てから)
- Origin: DR-0019 §6 (= 「実装自体は別 issue に切り出す」の実施票)

## 実装結果 (= DR-0020 §4 と統合)

両方実装済:

- `--exclusive`: daemon の handshake 統合 (`accept.rs::finalize_accepted_client`) で、
  他に rw client (= `Mode::Rw` / `RwNoLeader`) が居れば error response を返して
  attach を拒否する (= push せず drop)。error code は `mode.not-allowed` を流用。
- `--detach-others`: handshake 成立 (= push) 後に `process_pending_handshakes` で
  新 client 以外の全 client を drop し、leader cascade で奪取側を leader 昇格させる。
  `Detach{Others}` の daemon 機構 (= `ClientFrameOutcome::DropClients`) と同じ
  drop/cascade ロジック。
- CLI 側: `cli.rs::parse_attach` の未実装エラーを bool flag set に置換。
- `HandshakeRequest` の dead field (`exclusive` / `detach_others`) を daemon が読む
  ようになり、dead field 解消。
- e2e: `crates/hyoui-cli/tests/detach_cli.rs` の `attach_detach_others_steals_leadership`
  / `attach_exclusive_denied_when_rw_client_present`。

leader cascade との相互作用 (= 論点「奪取側が leader を取るか」) は「取る」で確定
(= detach-others 後に唯一の client = 奪取側が `elevate_next_leader` で leader 昇格)。

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
