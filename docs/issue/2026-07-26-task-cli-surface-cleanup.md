---
title: "CLI surface 棚卸し — 初期からの残骸・3 者不整合・幽霊 subcommand の整理"
status: open
category: task
created: 2026-07-26T00:00:00+09:00
last_read: 2026-07-26T23:03:40+09:00
open_entered: 2026-07-26T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: kawaz 指示「引数とか初期の頃からのゴミが大量に残ってるから一度整理するべき」(2026-07-26) を受けた全 subcommand / flag / env / help / completion の棚卸し
---

# CLI surface 棚卸し

## 概要

hyoui v0.9.22 の CLI 全面 (subcommand / flag / env / help / completion) を、
DR 群 (DR-0004 / 0006 / 0010 / 0012 / 0015 / 0016 / 0019 / 0020 / 0022 / 0023 /
0024 / 0026 / 0027 / 0029) と実装の実態に突き合わせて棚卸しした結果。

**本 issue は調査結果の記録であり、削除・変更は未実施。** 判定が「要裁定」の
項目は kawaz の裁定を待つ。

## 検証方法 (= 実測ベース、ソース読みのみで判定していない)

- `cargo build` した `target/debug/hyoui` (v0.9.22) で全 subcommand の `--help` を取得
- 実 session (`hyoui run --detached`) を立てて flag を実際に渡し、exit code と
  daemon 応答を観測 (`exit=2` = parse 拒否、`exit=0/1` = parse 受理)
- `hyoui completion bash|zsh|fish` の実出力を取得して help / 実装と 3 者比較
- 実装到達の有無は daemon 側までの grep で追跡

---

## A. 最優先 — 発見不能 / 誤誘導 (影響度: 高)

| 項目 | 現状 | 根拠 DR | 判定 | 理由 |
|---|---|---|---|---|
| **`hyoui web`** | `cli.rs:1049` で dispatch され `hyoui web --help` は完全な help を返す。だが `TOP_LEVEL_SUBCOMMANDS` / `IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS` / `hyoui --help` / bash・zsh・fish completion の **すべてに存在しない** | DR-0027 (web gateway を同 repo に配置) | **直す** | 実装済み機能が `--help` から発見できず typo suggest にも出ない。DR-0027 で正式採用された機能なので露出すべき。SSOT 定数 2 つ + `--help` + completion 3 shell に追加 |
| **`hyoui upgrade`** | 実装済・`IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS` 入り・bash/zsh は補完するが、`hyoui --help` の SUBCOMMANDS 節に **無い**。fish completion にも **無い** | DR-0028 | **直す** | 同上。`--binary` / `--skip-version-check` を持つ完全な help があるのに一覧から辿れない |
| **`set` が `TOP_LEVEL_SUBCOMMANDS` に無い** | `IMPLEMENTED_*` と `--help` と completion にはあるが、typo suggest 用の全集合定数にだけ欠落 | DR-0019 Update | **直す** | `hyoui ste` のような typo で `set` が候補に出ない。1 行追加で済む |
| **`tail --help` が `hyoui attach --read-only` を案内** | `--read-only` は実測 **exit=2 (unknown attach option)**。正しくは `--mode=ro` | — | **直す** | 存在しないフラグへの誘導。help テキスト 1 行修正 |
| **`run --help` の `SHELL` fallback 記載** | help ENVIRONMENT 節に `SHELL  Fallback command when none is given (legacy)` とあるが、**`"SHELL"` の grep が実装に 0 件**。実測でも `hyoui run` (引数なし) は `no command given` で exit=2 | 該当 DR なし | **消す** | 実装が存在しない機能の help 記載。「(legacy)」と自称している通りの初期残骸。**明らかなゴミ** |

---

## B. DR で「定義済みだが未実装」— help にも completion にも無いもの

実装が無いこと自体は DR で「MVP 後回し」等と明記されているものが多く、
**CLI surface としては露出していないので実害なし**。棚卸しの記録として残す。

| 項目 | 実測 | 根拠 DR | 判定 |
|---|---|---|---|
| `run --name` / `HYOUI_NAME` / `HYOUI_SOCK` | exit=2 / grep 0 件 | DR-0006 §3, §12 | **残す (未実装のまま)** — DR-0018 の namespace + `--session` が実質代替。DR-0006 §12 の env 定義は死んでいるので **DR 側に「不採用」を追記すべき** |
| `attach --no-leader` / `--window-size` | exit=2 | DR-0006 §5, §6 | **残す** — `--mode=rw-no-leader` が `--no-leader` の実質代替として実装済み。window-size は未着手 |
| `wait --scope=visible\|scrollback\|both` | exit=2 | DR-0006 §9.3 (L2 ROADMAP) | **残す** — 未実装。関連 issue: `2026-06-22-wait-scrollback-snapshot-coverage` |
| `tail --newline-convert` | exit=2 | DR-0006 §11 | **残す** |
| `input --spool` / `--max-size` / `--line-ending` / `--trailing-newline` | 全て exit=2 | DR-0006 §8.6-8.7 | **残す** |
| `lock acquire --process-bound` / `--timeout-absolute` / `--timeout-idle` | 全て exit=2 | DR-0006 §7 | **残す** |
| `hyoui leader take/give/show` | 未実装 | DR-0006 §5 (v0.3.0 で解放予定) | **残す** |
| `tx` (予約) | `parse_args` が「予約だが未実装」error | DR-0006 §7 / DR-0010 §1 | **残す** — 関連 issue: `2026-05-27-tx-lock-unlock-cli-subcommands` |
| `send` (予約) | 同上 | DR-0004 Reserved | **要裁定** — 下記 D-1 |

---

## C. 受理されるが効かない / 中途半端 (影響度: 中)

| 項目 | 現状 (実測) | 根拠 DR | 判定 | 理由 |
|---|---|---|---|---|
| **`attach --exclusive` / `--detach-others`** | **parse を通過して daemon まで到達する** (実測 exit=1 = connect 失敗まで進む)。`daemon/accept.rs:323` に `detach_others && Ro` の分岐があり部分的に生きている | DR-0020 §4 で定義、**DR-0019 が「parse 段で未実装エラー化」と決定** | **要裁定** — 下記 D-2 | DR-0019 の決定と実装が食い違っている。help にも堂々と載っている |
| **`screen dump --rect`** | parse は構文 validate するが **daemon が受信して無視**。実測: `--rect=0,0,5,5` でも全画面が返る | DR-0006 §10.2 (Phase B は無視仕様、forward-compat) | **残す** | help に「forward-compat: daemon 現状無視」と明記済みなので誤誘導ではない。ただし **効かない flag を露出し続ける是非**は D-3 で裁定 |
| **`screen dump/snapshot --timeout`** | 実測 `--timeout=1ms` でも即座に成功 (= no-op)。help に注記なし | DR-0019 Consequences が「射程外・未修正」と明記 | **直す** | help に「現状 no-op」注記を足すか、実際に response 待ちに配線する。**注記なしの no-op は誤誘導** |
| **`screen snapshot --include=style`** | parse は受理するが「protocol で送信可能な component が 1 つも含まれていません」エラー | DR-0006 §10.3 (forward-compat slot) | **直す** | 単独指定で必ずエラーになる値を受理する意味がない。parse 段で明示 reject するか help に「未実装」を明記 |
| **`screen snapshot --include=scrollback`** | daemon が `ProtocolMalformed: not implemented in MVP` を返す | DR-0006 §10.3 | **直す** | 同上。CLI 段で先に弾く方が親切 |
| **`attach --debug-dump-client`** | **実装は受理するが help にも completion にも無い** (run の help にのみ記載) | — | **要裁定** — 下記 D-4 | 隠しオプション状態。debug 用途として意図的に隠しているのか、単なる記載漏れか |
| **`hyoui --help` の screen 行** | `screen  Dump / inspect virtual screen state (subcommands: dump)` — 実際は `dump` と `snapshot` の 2 つ | — | **直す** | 単純な記載漏れ。snapshot は DR-0013 で正式実装済み |

---

## D. 要裁定

### D-1: `send` 予約を残すか

- **現状**: `hyoui send` は「予約だが未実装」エラー。DR-0004 の Reserved 由来。
  同じく予約だった `attach` / `status` / `detach` はすべて実装済みになっている
- **選択肢**:
  - (a) 予約のまま残す
  - (b) 予約を撤廃し、`send` を unknown subcommand 扱いにする (typo suggest 候補からも外す)
- **推し: (b)**。`input` family が DR-0006 §8.1 で 1 本化を完了しており、
  `send` に割り当てる責務が現状ない。予約が「いつか使う」の札のまま 2 ヶ月以上
  動いていない。撤廃しても後で必要になれば再追加は容易 (v1.0 未満で breaking OK)。
  ただし `tx` は DR-0006 §7 で仕様が具体的に定義済み + 専用 issue もあるので **残す**

### D-2: `attach --exclusive` / `--detach-others` の扱い

- **現状**: DR-0019 は「parse 段で未実装エラー化 (実装自体は別 issue)」と決定したが、
  **実際は parse を通過して daemon まで到達**している。daemon 側にも部分的な分岐がある
  (`accept.rs:323`)。help には DR-0020 §4 を根拠に堂々と載っている
- **選択肢**:
  - (a) DR-0019 の決定通り parse 段でエラー化し、help からも消す
  - (b) 実装を完成させる (daemon 側の占有判定 / 奪取処理を作り込む)
  - (c) 現状維持 (中途半端に通る) + DR-0019 の記述を実態に合わせて修正
- **推し: (b) か (a)**。(c) は「動くように見えて動かない」最悪の形なので却下したい。
  `hyoui kill --no-terminate` が `detach_others: true` を送って全 client を蹴る
  バグ (issue `2026-07-21-sigcont-alive-child-session-vanish`) が既に出ており、
  この経路が中途半端に生きていること自体がリスク。**まず (a) で塞ぎ、需要が出たら
  (b) で作り直す**のが安全と見ています。ただし DR-0020 §4 が正式定義した機能を
  消す判断なので裁定が必要

### D-3: 「効かないが forward-compat」flag を露出し続けるか

- **対象**: `screen dump --rect` (daemon 無視)、`screen snapshot --include=style` /
  `=scrollback` (エラーになる)
- **選択肢**:
  - (a) 現状維持 (help に「未実装」注記があるものは残す)
  - (b) 実装されるまで CLI から隠す (parse は受理し続けるが help / completion から外す)
  - (c) parse 段で明示エラー化
- **推し: (c) の部分適用**。`--include=style` / `=scrollback` は **必ず失敗する**ので
  CLI 段で分かりやすくエラーにすべき。`--rect` は「無視される」だけで害がなく
  help に明記済みなので (a) 維持でよい。判断が分かれるので裁定を仰ぎます

### D-4: `attach --debug-dump-client` を help に載せるか

- **現状**: 実装は受理するが help / completion に無い (run にはある)
- **選択肢**: (a) help + completion に追加 / (b) 意図的な隠しオプションとして明記なしを維持
- **推し: (a)**。`--debug-dump-server` は attach では正しく reject されている
  (run 専用) のに対し、client 側は attach でこそ使いたい flag。
  CLAUDE.md の「観測道具を最優先で直す」方針とも整合する

---

## E. completion の構造的欠陥 (= 同種の乖離が再発する原因)

`completion.rs` の既存テスト 14 件を検査した結果、以下が **原理的に検出できない**:

1. **subcommand 文脈を見ていない** — 全 assert が「スクリプト全体の substring / token 検索」。
   どの subcommand のブロックに出るかを問わないため、**zsh が `run` に存在しない
   `--index` を補完している**バグを検出できていない (実測: `run --index` は parse 拒否)
2. **fish の `upgrade` 欠落がすり抜ける決定的理由** — fish のヘルパ関数
   `__hyoui_using_subcommand` 内の `case run attach ... upgrade ...` が token 一致するため、
   **実際の候補行 (`complete -n ... -a upgrade`) が無くてもテストが green になる**。
   「テストが緑なのに壊れている」実例
3. **逆方向 (completion → 実装) の検証がゼロ** — 「completion が出す flag を実装が
   受理するか」の assert が 1 つも無い (`self-written-rule-blind-spots` の片面ルール)
4. **flag の網羅性を検証しない** — 名指しした一部 flag のみ検証。下記の補完漏れが素通り
5. **help テキストとの照合が無い** — 検証は「SSOT 定数 ↔ completion」の 2 者のみ

### completion の flag 補完漏れ (実測)

| subcommand | 漏れている flag | bash | zsh | fish |
|---|---|---|---|---|
| run | `--detached` (DR-0015 の主要 flag) | ✗ | ✗ | ✗ |
| run | `--session` | ✗ | ✗ | ✗ |
| run | `--no-scrub-env` (DR-0024 唯一の flag) | ✗ | ✗ | ✗ |
| run | `--debug-dump-server` / `--debug-dump-client` | ✗ | ✗ | ✗ |
| run | `--socket` | ✓ | **✗** | ✓ |
| run | `--index` を **誤って補完** (実装は非対応) | ✗ | **✓ 誤** | ✗ |
| input | `--auto-lock-timeout-acquire` (DR-0022) | ✗ | ✗ | ✗ |
| web | `--listen` / `--web-assets-dir` | ✗ | ✗ | ✗ |
| (top) | `upgrade` | ✓ | ✓ | **✗** |

### alias の 3 者不一致

| alias ペア | 実装 | help | completion 3 shell |
|---|---|---|---|
| `--strip` / `--strip-ansi` (tail) | **両方受理** | `--strip` のみ記載 | `--strip-ansi` のみ |
| `--last` / `--last-bytes` (tail) | **両方受理** | `--last` のみ記載 | `--last-bytes` のみ |
| `screen dump --format=text` / `plain` / `text/plain` | 3 形すべて受理 | `text/plain` のみ | `text/plain` のみ |

**help が推す名前を completion が一切補完しない**状態。どちらを primary とするかの
合意が 3 者で取れていない。→ **要裁定** (下記 D-5)

### D-5: tail の `--strip` / `--last` はどちらを primary にするか

- **選択肢**: (a) 短形 (`--strip` / `--last`) を primary にして completion を合わせる /
  (b) 長形 (`--strip-ansi` / `--last-bytes`) を primary にして help を合わせる
- **推し: (b)**。`cli-design-preferences` rule が「ロングオプションを基本とする
  (補完前提)」と規定しており、`--strip` は「何を strip するか」が不明瞭。
  `--last` も単位 (bytes/lines) が不明瞭。alias としては両方受理を維持

---

## F. env の棚卸し

| env | help 記載 | 実装 | 判定 |
|---|---|---|---|
| `HYOUI_NAMESPACE` | ✓ (19 箇所) | ✓ | 残す |
| `HYOUI_SESSION_ID` | ✓ (15) | ✓ | 残す |
| `HYOUI_LOCK_TOKEN` | ✓ (14) | ✓ | 残す |
| `HYOUI_MAX_FILE_BYTES` | ✓ (2) | ✓ (`cli.rs:5409`) | 残す |
| `HYOUI_WAIT_POLL_MS` | ✓ (1) | ✓ (`wait_core.rs:66`) | 残す |
| `HYOUI_SCROLLBACK_ROWS` | ✓ (1) | ✓ (`main.rs:457`) | 残す |
| `HYOUI_ALLOW_CORE` | **✗ 記載なし** | ✓ (`session.rs:291`) | **要検討** — user 向けか内部専用か。内部なら現状維持でよい |
| `HYOUI_DAEMONIZE_INIT` | ✗ | ✓ (内部 IPC、起動時に `remove_var`) | 残す (内部専用、露出不要) |
| `HYOUI_UPGRADE_*` (9 個) | ✗ | ✓ (DR-0028 の self-exec 引き継ぎ用、内部専用) | 残す (内部専用) |
| `HYOUI_NAME` / `HYOUI_SOCK` | ✗ | **✗ 実装なし** | DR-0006 §12 の死んだ定義。**DR 側に不採用を追記** |
| `HYOUI_DETACH_PREFIX` | ✗ | **✗ 全廃済み** | DR-0029 で正しく撤去済み。**残骸なし (確認済み)** |

---

## G. 廃止済み機能の残骸チェック — 結果は良好

DR-0029 / DR-0019 / DR-0015 / DR-0012 / DR-0023 で廃止した機能の残骸を
help 全文 grep で検査した結果、**残骸は見つからなかった**:

- `Ctrl-A d` detach prefix / `HYOUI_DETACH_PREFIX` (DR-0029 で全廃) → help に一切なし。
  attach help は DR-0029 の新仕様 (Ctrl+Z ガード) を正しく記載
- `--mode=interactive|headless` (DR-0019 §1 で削除) → help になし。
  実測で `--mode` は migration hint 付きエラーを返す (= 正しい扱い)
- `--on-parent-suspend` (DR-0015 で削除) → help になし、明示エラー返却
- `--signum` (DR-0012 で `--signal` に置換) → 実測で
  `--signum is removed in v0.2.0 (DR-0012); use --signal` と正しく案内
- `--scrub-env-target` / `--add` / `--keep` (DR-0023→0024 で削除) → 痕跡なし
- `serve` subcommand (DR-0010 §2 → DR-0027 で覆り `web` に) → `serve` の露出なし

**この層は歴代の整理がきちんと効いている。** 今回問題なのは「廃止残骸」ではなく
**「実装したのに露出していない」「定義したのに実装していない」の双方向漏れ**。

---

## 段階実施案

### Phase 1 — help テキストのみ (低リスク・即効)

実装に触れず help 文字列だけを直す。作業量小、影響度は中〜高。

1. `run --help` から `SHELL` fallback 記載を削除 (A-5、明らかなゴミ)
2. `tail --help` の `--read-only` → `--mode=ro` に訂正 (A-4)
3. `hyoui --help` の SUBCOMMANDS 節に `upgrade` を追加 (A-2)
4. `hyoui --help` の screen 行を `(subcommands: dump, snapshot)` に訂正 (C-7)
5. `screen dump/snapshot --timeout` に「現状 no-op」注記を追加 (C-3、恒久対応は Phase 3)

### Phase 2 — SSOT 定数 + completion の整合 (中リスク)

1. `TOP_LEVEL_SUBCOMMANDS` に `set` を追加 (A-3)
2. `web` を SSOT 定数 2 つ + `--help` + completion 3 shell に追加 (A-1)
3. fish completion に `upgrade` 候補行を追加 (A-2)
4. completion の flag 補完漏れ 8 件を補充 (E)
5. zsh の `run --index` 誤提示を削除 (E-1)
6. **completion テストの構造改善**: 全文検索 → subcommand ブロック単位の検査、
   逆方向 (completion → parse 受理) の検証を追加 (E-1〜E-3)。
   これをやらないと同種の乖離が再発する

### Phase 3 — 実装判断が要るもの (要裁定後)

1. D-2: `attach --exclusive` / `--detach-others` の扱い確定 → 実装 or 削除
2. D-1: `send` 予約の撤廃可否
3. D-3: `--include=style` / `=scrollback` の parse 段 reject 化
4. D-4: `attach --debug-dump-client` の help 露出
5. D-5: `--strip` / `--last` の primary 名確定 → help / completion を揃える
6. `screen dump/snapshot --timeout` の実配線 (Phase 1 の注記を恒久対応に置換)
7. DR-0006 §12 の `HYOUI_NAME` / `HYOUI_SOCK` に「不採用」を追記 (DR 側の整合)

---

## 関連

- CLAUDE.md「介入判断 self-check」「コードと DR の双方向整合性」
- `cli-design-preferences` rule (実装 ↔ `--help` ↔ completion の 3 者同期保守)
- `2026-05-28-feature-cli-restructure-discussion` (CLI 設計大改修議論、idea 状態)
- `2026-07-21-sigcont-alive-child-session-vanish` (`detach_others: true` 誤送信バグ、D-2 と関連)
- `2026-05-27-tx-lock-unlock-cli-subcommands` (`tx` 未実装、B)
