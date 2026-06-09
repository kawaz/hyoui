# Feature: record secret redaction Phase 5 本実装 (DR-0016 §6)

- Status: Open
- Date: 2026-06-10
- Priority: High (= record は出荷済みだが stdin 素通し記録のため、secret 入力を録画すると passphrase / token がファイルに平文で残る安全性問題)
- 関連 DR: [DR-0016](../decisions/DR-0016-tty-io-record.md) §6 (redact-after-prompt 仕様正本)
- 関連実装: `crates/hyoui/src/daemon/record.rs` (冒頭 `⚠ redaction は未実装` 注記)

## 問題

`hyoui record` の本体 (start/stop/list、jsonl/raw sink、bounded queue + writer task、
lifecycle event) は実装・出荷済み (v0.2.2)。だが **§6 の secret redaction state machine が
未配線** (Phase 5 積み残し)。

現状の挙動:

- `--input-secrecy`（default `redact-after-prompt`）は CLI で受理され request に乗るが、
  `RecordRegistry::start` は `prompt_pattern` を storage に置くだけで触らない
  (`record.rs` の Phase 5 注記参照)
- **policy 値に関わらず stdin は素通しで記録される**
- `RecordEvent::InSecretRedacted` / `RecordRegistry::push_in_secret_redacted` は
  `#[allow(dead_code)]` で抑制された dead code

→ password / OTP / token を打つ可能性のあるセッションを録画すると、それらが
jsonl / raw にそのまま残る。README / MANUAL / DESIGN / DR-0016 に loud warning は
出してあるが、本実装で塞ぐべき。

## 求められる仕様 (= DR-0016 §6 から)

- `redact-after-prompt` (= default): 子 PTY 出力が password/OTP prompt pattern に
  match したら、以降の stdin を **redaction mode** に入れる。redaction mode 中の
  stdin bytes は捨て、`InSecretRedacted { byte_count, reason }` event のみ記録
- prompt pattern は request の `prompt_pattern` (= 未指定なら default pattern)。
  Phase 5 で **compile 検証 + state machine への組み込み**を行う (現状は storage のみ)
- redaction mode の解除条件 (= prompt 応答完了の検知) を DR-0016 §6 に沿って実装。
  Enter / 一定 idle / 次の non-secret prompt 等の判定基準を確定する
- `record-all` (= opt-in): redaction なし、全 stdin を hex 記録、loud warning 済
- `never-record-stdin` (= opt-in): 全 stdin を `in-redacted` 化、内容捨て

## 実装の足がかり

- `record.rs` line 410 付近: prompt pattern compile 検証 (現状 `if let Some(pat)` で
  受け取るだけ) を Phase 5 で本実装
- `record.rs` line 594-601: `push_in_secret_redacted` を hot path から呼ぶ配線
- `RecordEvent::InSecretRedacted` (line 208-211): redaction mode 中の stdin で発火
- hot path 側 (`daemon/accept.rs` / record sink 配線箇所): 子 PTY 出力を prompt
  pattern と照合する経路、stdin を redaction mode で振り分ける経路を追加
- `#[allow(dead_code)]` (line 50-52, 697) を外す

## 検証 (= CLAUDE.md 検証主義)

- マトリクス: prompt pattern × (password 入力 / 通常入力 / paste) × format(jsonl/raw)
  で「redact されるべき / されないべき」を埋める
- **false-positive 検証**: 通常の対話 (= prompt に見えるが secret ではない出力) を
  redaction mode に誤って入れないか。partial state 破棄系と同じ慎重さで
  (CLAUDE.md §partial state を扱う実装の規律)
- 完了後 DR-0016 §6 と DESIGN.md 2.9 / DR-0016 INDEX status の警告を更新する

## 完了時のフロー

本実装完了で以下を真実化:

- DR-0016 INDEX status の「⚠ redaction Phase 5 未配線」警告を解除
- DESIGN.md / DESIGN-ja.md 2.9 の「Secret redaction is not yet implemented」段落を更新
- README / MANUAL (ja/en) の「⚠ stdin redaction is NOT wired yet」警告を解除
- `docs/decisions/DR-0016-tty-io-record.md` の Status 直下 ⚠ ブロックを解除
