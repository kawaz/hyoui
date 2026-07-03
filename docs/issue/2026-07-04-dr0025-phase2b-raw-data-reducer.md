---
title: "DR-0025 Phase 2-β — raw_data hot path の reducer→Effect→execute 化"
status: open
category: task
created: 2026-07-04T01:11:28+09:00
last_read:
open_entered: 2026-07-04T01:11:28+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# DR-0025 Phase 2-β — raw_data hot path の reducer→Effect→execute 化

## 概要

raw_data (`TYPE_RAW_DATA` frame) の認可判定〜PTY write〜DR-0021 ack を reducer 経路に移す。
DR-0025 の pending state パターンの初適用箇所。

## 背景

Phase 2-α で execute layer / `EffectResult` feedback (`execute_with_feedback`、
`MAX_FEEDBACK_ROUNDS=8`) / lock state の `DaemonState` 移設 / Client reducer の
`Detached` arm が稼働済み (v0.9.8)。次段として、hot path で最も複雑な raw_data
frame (認可判定・write・ack が同居) を reducer 経路に移す。

## スコープ

1. `ClientMsg::FrameReceived` の payload 実体化 (現状 client_id のみ → frame type +
   bytes を運ぶ。`translate::client_frame_received` の拡張)
2. Client reducer が認可判定 (mode/cap は `ClientRegistry` 側の情報が必要 = 最小限の
   client メタ (id/mode/caps) の pure ミラー導入を検討、lock holder は
   `DomainViews.lock` read 済みの仕組みを利用) → 可なら `Effect::TtyWrite` / 否なら
   `Effect::ClientReply(RawAck err)` + `in_rejected` record
3. execute に `TtyWrite` 配線: `pty.master_fd().write_all_with_idle_timeout` の移設。
   execute が `Pty` への参照を要する (シグネチャ拡張)。`WriteOutcome`
   (complete/partial) → `EffectOutcome` への写像、EAGAIN は
   `EffectErrorKind::WriteEagain`
4. `EffectResult::Ok` を受けた Client reducer が `RawAck(ok)` の `ClientReply`
   Effect を発行 (= DR-0021 drain ack 意味論の保存、DR-0025 §DR-0021 の記載どおり)。
   pending state: bytes 送信中の client state を Pending にして `EffectResult`
   で確定
5. record hook (in / in-write-error / in_rejected) の既存位置・順序の保存 (record
   の Effect 化は 2-γ なので、当面は execute 側で `record_registry` を呼ぶ形も可 =
   要設計判断)

## 注意点

- 該当既存コード: `control.rs` の `handle_client_frame` の `TYPE_RAW_DATA` arm
  (mode check / lock gate / `write_all_with_idle_timeout` / `RawAck` / record hook
  が同居)
- DR-0021 の ack 完了点 (master fd write の return) を変えない。ack の FIFO 順序
  保証 (`pending_frames`) との整合
- `MASTER_WRITE_IDLE_TIMEOUT_MS` の idle timeout 失敗時の disconnect 挙動
  (`master.write-timeout`) の保存
- 挙動不変が絶対条件。回帰基準は `input_auto_lock_cli` e2e 14 件 + `serve_*` 系
- 負荷依存 flaky: `outer_token_inheritance_skips_auto_acquire` は
  `docs/issue/2026-07-04-bug-flaky-outer-token-e2e-deadline.md` 参照 (単独でも稀に
  deadline fail、既知族)

## 参照

- DR-0025 §Effect layer / §read-only view / §DR-0008 protocol との接続 (raw_data 行) /
  §DR-0021
- Phase 1b-β agent の引き継ぎ (translate 挿入点)
- Phase 2-α の commit 3 件 (`0b785386` / `3414f6d0` / `2adbb0b0`)

## 受け入れ条件

- [ ] raw_data frame の認可判定〜PTY write〜ack が reducer→Effect→execute 経路を通る
- [ ] DR-0021 の ack 完了点・FIFO 順序保証が保存されている
- [ ] `input_auto_lock_cli` e2e 14 件 + `serve_*` 系が全 pass (挙動不変)
