---
title: "feature: DR-0014 で防ぎきれなかった盲点の補強"
status: wip
category: task
created: 2026-05-28T00:00:00+09:00
last_read:
open_entered: 2026-05-28T00:00:00+09:00
wip_entered: 2026-05-30T00:00:00+09:00
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

# feature: DR-0014 で防ぎきれなかった盲点の補強

- Priority: 中 (= process 改善、本格 fix で再発防止)

盲点 1〜3 は DR-0014 改訂 (commit `mqkrkxqnplqw`) で反映済 (= self-check 拡張、Anti-pattern 追加、§状態観測 / §検証主義 補強、CLAUDE.md 連動更新)。**残り作業**: harness 自動 test (`matrix_test.rs`) への `stty -a` 観測組み込み + real-world TUI 3 category 検証の CI 化 + 盲点 4 (DR Implementation Auditor persona) 実装。

## 背景

DR-0014 制定後にも Claude が **4 連続で anti-pattern を踏んだ**:

1. POSIX orphan group 誤読 (= 「TTY system 経由限定」を「全 SIGTSTP」と一般化)
2. CI 6h hang を「matrix test 由来」と推測 (= 真因は backpressure test)
3. スクショ context 読み違えて「別 session 混入」誤認 (= cwd / remote-control session ID の罠)
4. kawaz の active terminal プロセスに外部 `kill -TSTP` 送って cmux freeze 誘発

→ self-check 7 項目あっても **本当に走らせる機構が無い** = 盲点の存在を示している。

## 発見した盲点 (= DR-0014 で防げなかった項目)

### 1. harness test に tty mode 観測がない

- matrix tests (= 既存 15 cell) は `ps stat` のみ確認
- `stty -a < /dev/ttyXXX` で実際の TTY mode (= raw / cooked / line discipline state) 確認してれば
  Issue 1 (= termios 復元漏れ) を検出できた
- **DR-0014 §状態観測 に追加候補**: 「`stty -a` を harness で実装、CI test に含める」

### 2. 「観察操作と破壊的操作の区別」が DR にない

- 私が kawaz の active terminal プロセスに外部 `kill -TSTP` 送って cmux freeze 誘発
- これは「観察」と称して「破壊的操作」、active session 侵襲
- **Anti-pattern 6 件目候補**: 「観察と称して active session に破壊的 signal を送る」
- **self-check 8 項目目候補**: 「観察に見えて破壊的な副作用を持つ操作ではないか?」

### 3. real-world TUI app の harness 範囲

- 既存 harness は `/bin/sh -c 'kill -TSTP $$'` の simple 例のみ
- claude TUI / vim / less 等の **category 3 種類検証は未完**
- DR-0014 §検証主義は「最低 3 種類 category で検証」と書いてるが、harness 実装が追いついてない
- **DR-0014 §検証主義 に追加候補**: 「harness で 3 category 検証を CI 必須にする」

### 4. DR Implementation Auditor persona が未実装

- findings/2026-05-27-dr-0001-implementation-gap-analysis.md §6.2 で提案済
- 「DR を読んで実装エビデンスを grep / test で照合する専任 reviewer」
- **DR-0014 §双方向整合性 の実装手段として組み込み候補**

## DR-0014 への追加候補 (= 提案)

### self-check 8 項目目

```markdown
- [ ] **観察に見えて破壊的な副作用を持つ操作ではないか?**
  (= active session への signal 送信、root権限操作、グローバル state 変更等)
```

### Anti-pattern 6 件目

```markdown
6. **「観察」と称して active session に破壊的 signal を送る**
   = 例: kawaz の active terminal プロセスに `kill -TSTP` 送って cmux freeze 誘発、
   「ps で観察するだけのつもり」「signal が透過するかの実験」を装って active な
   ユーザの作業環境を破壊
```

### §状態観測 補強

```markdown
| 観測対象 | コマンド |
|---|---|
| TTY mode (line discipline) | `stty -a < /dev/ttyXXX` |
```

を追加、かつ「harness 自動 test で stty 観測を組み込む」を明示。

### §検証主義 補強

```markdown
- harness 自前 fake child だけでなく、real-world TUI app (= claude / vim / less 等
  最低 3 種類 category) を **CI で必須検証** に含める。simple 例だけでは
  漏れが出ることが本 session で実証された。
```

## TODO

- [x] DR-0014 改訂 (= self-check 7 → 8 項目、Anti-pattern 5 → 6 件、§状態観測 / §検証主義 補強)
- [ ] harness test (= matrix_test.rs) に `stty -a` 観測を組み込み (= Issue 1 検証の前提)
- [ ] real-world TUI 3 category の test runner 設計 + CI 化
- [ ] DR Implementation Auditor persona 実装 (盲点 4)

## 関連

- DR-0014 (= 改訂対象)
- findings/2026-05-27-self-audit-after-dr-0014.md (= 自監査結果、本 session 後)
- findings/2026-05-27-dr-0001-implementation-gap-analysis.md §6.2 (= DR Implementation Auditor 案)
