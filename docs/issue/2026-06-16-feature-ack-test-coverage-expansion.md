---
title: "feature: DR-0021 ack 機構の test cover 拡張"
status: open
category: task
created: 2026-06-16T00:00:00+09:00
last_read:
open_entered: 2026-06-16T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: DR-0021 codex adversarial review minor finding 由来
---

# feature: DR-0021 ack 機構の test cover 拡張

- Severity: 低 (= 主要経路は cover 済、防御的観点で追加が望ましい)
- Related: DR-0021, `docs/issue/2026-06-16-bug-input-text-key-enter-not-sent.md`

## 背景

DR-0021 統合時の codex adversarial review で minor finding として挙がった test cover の不足。修正済の M1/M2/m1 で守られている経路は unit/e2e で cover 済だが、以下の観点が未カバー。

## 未カバー観点

### 1. 実機 TUI matrix の自動化

DR-0014 マトリクス検証主義に従い、3 category (= TUI alt screen / line-oriented / interactive REPL) で実機検証は手動で実施した (DR-0021 § 検証マトリクス)。これを **CI で自動回帰** にすると、将来の protocol 変更で同様 regression を踏まない:

- vim alt screen で 2000 B text + Escape + `:wq` の完走
- python -i で 900 B text + Enter の `print` 評価到達
- bash -i で `echo $((1+1))` 等の REPL 評価

### 2. timeout 後の接続再利用 (= poison 検証) を実環境で

`send_raw_bytes_after_timeout_is_poisoned_and_rejects_stale_ack` は socketpair mock。これを **実 daemon に対して**:
- daemon を意図的に slow (= sleep 7s) させて RAW_ACK_TIMEOUT を踏ませる
- 同一 ClientConnection で `send_raw_bytes` を 2 回目 → poison error を確認

### 3. 旧 daemon × 新 client skew

新 client が旧 daemon (= v0.6.5 以前) に attach した場合、ack を 5s 待って timeout する想定。これを **実旧バイナリ** (= brew の v0.6.5) との組合せで verify:

```bash
$brew_hyoui run --detached -- bash  # 旧 daemon
$new_hyoui input <sess> "text:..."   # 新 client から ack 待ち
# → 5s 後に timeout error で exit 1 が確認できるか
```

### 4. 並列 client からの ack 衝突 (= 防御的検証)

同 daemon に複数 client が attach し、それぞれ並列に `send_raw_bytes` を発火するシナリオ。各 client の ack が他 client に混ざらないことを確認 (= 設計上は per-client socket なので混ざらないはずだが、防御的に test)。

### 5. m1 silent skip の負荷耐性

`recv_control` が大量の unsolicited RAW_ACK を skip するシナリオで他 frame の取りこぼしが無いか確認。

## 推奨

短期: 1 (matrix CI) と 3 (旧 daemon skew) を優先。残りは次フェーズ。

## TODO

- [ ] 案を kawaz と詰める (= どこまで CI 自動化するか、cargo test の重さとのトレードオフ)
- [ ] 実装担当
- [ ] 解決時は本 issue file を delete + journal/DR に昇華
