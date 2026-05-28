# feature: CLI 設計大改修議論 (screen view 改名 / dump top-level 化 / screen write overlay / format 整理)

- Date: 2026-05-28
- Priority: 中 (= 結論未確定、別 session で本格議論 → DR 起票)
- Status: 議論段階 (= 採用方針未確定)

## 背景

kawaz と本 session 末で議論した CLI 設計大改修案。範囲が広く設計判断が多いので、別 session で
腰を据えて議論 → DR 起票 → 段階実装の流れ。

## 議論トピック一覧

| トピック | 議論内容 |
|---|---|
| `screen view` 改名 | `screen snapshot` 廃止、`screen view` に統一 (= state 取得寄りの命名) |
| `dump` を screen 枠から外す | raw byte stream は state レイヤより下、`hyoui dump` (top-level) として独立 |
| `screen write` (= overlay) | pos/rect 指定でセル overlay、`--persist` でスクロール時継続、設計の根幹判断 |
| `screen watch` / `screen wait` | rect/line 単位の細粒度監視、既存 `hyoui wait` (= L1) と L2 として共存 |
| layer × buffer 直交化 | `--buffer={primary\|alternate\|current}` + `--region={visible\|scrollback\|all}` 案 |
| format 整理 | `text` only / `binary` 廃止 (= vt100 前ゴミ) / `raw-bytes` 新設 / `json` + `jsonl` 追加 / `cborl` 不要 |
| `--head N` / `--tail N` / `±N` | POSIX `tail -n +N` semantics、両指定 AND 結合 = intersect range filter |
| 仮想サイズ snapshot | `--virtual-size COLS,ROWS` で vt100 state clone + resize + dump、試験再現性向上 |
| format 相互変換 utility | `hyoui screen convert --from <fmt> --to <fmt>`、raw-bytes は best-effort (= size 既知時のみ成功) |

## 変換 matrix (= 確定済み)

```
| from \ to | text | ascii | json | jsonl | cbor | cborl | raw-bytes |
| text      | id   | ✗    | ✗   | ✗    | ✗   | ✗    | ✗        |
| ascii     | ✓   | id   | ✓   | ✓    | ✓   | ✓    | ✗        |
| json      | ✓   | ✓    | id  | ✓    | ✓   | ✓    | ✗        |
| jsonl     | ✓   | ✓    | ✓   | id   | ✓   | ✓    | ✗        |
| cbor      | ✓   | ✓    | ✓   | ✓    | id  | ✓    | ✗        |
| cborl     | ✓   | ✓    | ✓   | ✓    | ✓   | id   | ✗        |
| raw-bytes | ◐¹  | ◐¹   | ◐¹  | ◐¹   | ◐¹  | ◐¹   | id        |
```

¹ ring 容量内 + best-effort (= size 既知なら成功、不明なら `--default-size` で fallback or 失敗)

## 採用順 (= 軽量 → 重い、kawaz 提示順)

1. `text` only / 他 alias 廃止 (= 軽量、parser + help)
2. `binary` 廃止 + `raw-bytes` 新設 (= 中規模、protocol breaking)
3. `--strip-trailing-spaces` flag (= text format sub-option)
4. `--head N` / `--tail N` (= 既存 ROADMAP `--last-rows N` と統合、AND 結合 intersect semantics)
5. JSON 実装 (= 中規模、serde_json 追加 + cell → JSON 変換)
6. JSONL 実装 (= 軽量、JSON 実装後 +α)
7. `--virtual-size=COLS,ROWS` (= 中規模、Phase B input log 応用)
8. `hyoui screen convert` utility
9. `screen view` 改名 + `dump` を screen 枠から外す (= subcommand 再編、設計の根幹)
10. layer × buffer 直交化 (= 設計の根幹、別 DR)
11. `screen write` overlay (= 新 DR、protocol message + state augment 機構、大規模)
12. `screen watch` / `screen wait` (= 新 DR、既存 wait との関係整理)

## POSIX tail -n +N semantics (= 採用予定)

```
--tail N    = 末尾 N 行
--tail +N   = N 行目以降全部 (= 先頭 N-1 行 skip)
--head N    = 先頭 N 行
--head -N   = 末尾 N 行を除いた全部
両指定      = AND 結合 (= intersect range filter)
```

## 次のステップ (= 新 session で着手する場合)

1. 本 issue を読んで議論前提を共有
2. **採用順 1-3 (= text only / binary 廃止 / strip-trailing-spaces)** を先行 PR 化 (= 軽量、breaking 含む)
3. 採用順 4 (= `--head` / `--tail` POSIX semantics) で wait/tail/dump の semantics 統一
4. 採用順 9-12 は **新 DR 起票** が必要 (= DR-0006 改訂 or 新 DR、screen write は別 DR)

## 注意

- `screen write` overlay は **透過原則の例外** になるので DR-0014 §self-check で justify 必要
- 「overlay は外部から見える表示を変えるが、子プロセスの state には影響しない」設計が成立するか要検討
- 子プロセスが redraw した瞬間 overlay は上書きされる、`--persist` でどう保つかが論点

## 関連

- DR-0006 §8-§11 (= 現行 CLI ground rules、本議論で改訂候補)
- DR-0013 (= screen state 正本化、format / layer の前提)
- DR-0014 (= 透過原則、`screen write` overlay は例外 justify が必要)
- ROADMAP (= --last-rows N が `--tail N` に統合される)
