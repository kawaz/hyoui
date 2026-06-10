# feature: ControlMessage::Signal に成功 ack を追加する

- Date: 2026-06-10
- Status: idea
- Priority: 低

## 背景

`hyoui kill <session> --signal=CONT --no-terminate` (DR-0017 で追加した非 terminate signal
経路) は、daemon の `handle_signal` が成功時に ack を返さないため、CLI 側は 300ms の
read timeout で「Error frame が来なければ成功」と判定している。実用上は十分だが、
確実な成功確認が欲しい場合は protocol に `signal.ack` を足す余地がある。

## 対応案

- `ControlMessage::SignalAck { ok: bool }` を追加し、cap flag で旧 client 互換を維持
- CLI は ack 受信で即 return (= 300ms 待ちの解消にもなる)

## 関連

- [[DR-0017]] §柱 2 (resume API)
- [[DR-0008]] protocol 設計 (cap flags)
