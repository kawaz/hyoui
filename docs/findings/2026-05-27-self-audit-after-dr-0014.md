# 2026-05-27 nonstop session self-audit (per DR-0014)

- Date: 2026-05-27
- Scope: `main@origin..@` (= 本 nonstop session で積まれた 50+ change)
- 根拠: [DR-0014](../decisions/DR-0014-transparency-and-empirical-verification.md) §self-check 5 項目
- 関連: [DR-0001](../decisions/DR-0001-bgfg-jobcontrol-two-axis.md), [DR-0005](../decisions/DR-0005-design-philosophy-external-automation.md), [DR-0006](../decisions/DR-0006-cli-ground-rules.md), [DR-0013](../decisions/DR-0013-screen-emulator-and-attach-stability.md)

## 要約

### 4 区分の内訳

| 区分 | 件数 | 主な内訳 |
|---|---|---|
| **A. 採用維持** (= self-check 全 ✓) | 9 | Phase A vt100 統合、wait_core 状態 polling、CLI 拡張 (screen dump / snapshot)、file: spec security、Unicode alias、旧 wait protocol 削除、DR-0006 改訂、docs 系 |
| **B. 軽微な改善余地** (= 大筋 OK、後追い調整可) | 3 | lock acquire の block 設計、Phase B DEC sync chunk carry、wait-idle SequenceNo polling |
| **C. 要再評価** (= justify 不十分、議論要) | 2 | Phase B stalled detect 3 連続自動 reset、Phase B input bytes log resize replay |
| **D. 削除候補 / 大幅修正** (= 透過原則違反明白) | 0 | — |

総監査項目 **14 件**。D 区分 0 件は **本 session の道具揃え順序が概ね正当だった**ことを示すが、C 区分 2 件は道具揃え過程で混入した「子 bytes に対する hyoui 裁量判定」の典型で、DR-0014 で言う **「監視 + 状態破棄」anti-pattern** に該当する可能性がある。

### 主要 finding top 5

1. **stalled 3 連続 reset (15s) は子の bytes を「壊れている」と hyoui が裁量で判定する強い介入**。DR-0013 §5 の「5 秒で warn → Phase A は detect-only、reset は Phase B で再検討」を Phase B で「3 連続なら自動 reset」に強化したが、ResetRequested = parser 内部 buffer clear (= partial sequence 破棄) であり、復旧不能な情報損失。マトリクス検証 (= 子が長い OSC52 / Sixel 等 15s 超の大型 sequence を送るケース) なしで導入されており、再評価が必要。
2. **input bytes log resize replay は primary buffer 専用の補完策として §7 で justify 済だが、replay 中に partial sequence + sync flag を新 Parser に持ち越さない設計**。 復旧過程で新たな sync detect が発火する semantics は spec として書かれているが、実機検証なしで投入されている。マトリクス (= claude TUI 中の resize, alt screen 切替直後の resize 等) で検証要。
3. **DEC sync chunk 跨ぎ 7 byte sliding window carry は「子の bytes を hyoui 側で needle 検索する」介入だが、vt100 0.16 が `?2026h/l` を内部 hook しないため wrapper で補完する必然がある**。DR-0013 §6 で明記、必然性は OK。ただし「7 byte 持ち越しが needle 完全網羅」の根拠は code comment レベルで、別 escape pattern (= `?2027h` 等 vt100 が後日 hook するかもしれない sync 変種) を見落とすリスクは残る。
4. **Phase A の alt mode prepend (`\x1b[?1049h/l`) は state → ANSI 再生成の業界 standard pattern (= wezterm `sessionhandler.rs:119`) に乗っており、PoC §2 で実証された vt100 `state_formatted()` の欠落補完として必然**。透過原則の延長線 (= state 正本からの再描画は介入とは異なる、read-only state から bytes を再構築しているだけ) と整理でき、self-check ✓。
5. **lock acquire の「block until signal/EOF」設計は client の挙動を変えるが daemon / 子には影響なし**。透過原則違反ではないが、SIGINT/SIGTERM/SIGHUP + stdin EOF の組合せが platform 依存挙動を持ちうるので **マトリクス検証要** (= daemon kill 中に SIGINT、stdin EOF と SIGTERM の同時発火、etc)。設計自体は採用維持。

## 監査対象一覧

`jj log -r 'main@origin..@'` の主要 commit (= 抜粋、doc-only は集約):

| change_id | subject | category |
|---|---|---|
| `lxxurvwz` | build(deps): add vt100 0.16 | A |
| `prztqxop` | feat(daemon/screen): introduce vt100 ScreenState wrapper (Phase A) | A |
| `yqroonkq` | feat(daemon): integrate ScreenState into serve loop + attach redraw | A |
| `oqxxzvwl` | docs: rewrite DR-0006 §8/§9 + input family spec to state-based | A |
| `qywspxmn` | **feat(daemon/screen): DR-0013 Phase B - input log + snapshot protocol + stalled reset** | B/C 混在、後述 |
| `kskwoqsk` | docs: annotate DR-0013 §8 byte/rows responsibility split | A |
| `nttsvtqo` | feat(cli): add `screen dump` subcommand | A |
| `lsurxnpp` | feat(cli): add 'screen snapshot' subcommand | A |
| `ytnrnzym` | feat(cli): add 'input' subcommand + spec parser | A |
| `tzuumruk` | feat(client): add `ClientConnection::send_raw_bytes` | A |
| `lkqyuumk` | feat(cli): wire input family handlers | A |
| `kquvykqu` | feat(cli): add wait_core (state-based polling) | A (Phase A wait-idle は B) |
| `rvszlymy` | feat(cli): rewrite `hyoui wait` to state-based polling | A |
| `nqxmnvqn` | feat(cli): wire input family wait:/wait-idle: to state-based core | A/B (wait-idle 部分は B) |
| `npvmqvnw` | feat(cli): align tail subcommand with DR-0006 §11 | A |
| `nnpptmxt` | feat(cli): wire --lock-token + env fallback | A |
| `ouyyysws` | chore(cli): reserve tx/lock/unlock subcommands | A |
| `tqzywvxk` | feat(cli): file size limit + type validation for input file: spec | A |
| `kvulpnqm` | feat(cli): edit-distance typo suggest (UX) | A |
| `yomyoyor` | feat(cli/input): Unicode key aliases + typo suggest | A |
| `ukwxrlln` | feat(cli/completion): wire screen + input subcommands | A |
| `rzzzmnop` / `vouwlqmk` / `lopyozlm` | feat(cli): lock/unlock subcommand parser/dispatcher/test | B (block 設計) |
| `qvtqvsll` | docs(issue): mark lock/unlock as done | A |
| `okqtrwnq` | test(client): stabilize run_returns_when_daemon_closes | A |
| `pnuxqrpl` | docs(findings): audit dependency licenses for MIT | A |
| `plpzlkku` / `ozprorry` / `rmlkokmy` / `zkpzxrvz` | test: edge cases QA | A |
| `rtkpmkto` / `xwpskuyo` / `zlvzktwr` | refactor(protocol): remove legacy wait | A |
| `uxvywowl` | docs: annotate DR-0007/0008/0009 with state-based wait migration | A |
| docs (DESIGN / README / journal / roadmap / DR-0014 制定) | doc only | A |
| harness / smoke 系 (= `nzsurttv` / `ntqlozrl` / `qmoqpzs` / `yuuslmzm` / `qqltnmmu` / `mmvqykxn` / `lwqzpryo`) | DR-0014 制定後の検証道具揃え | A |

## 各監査項目の self-check 結果

---

### Item 1: `prztqxop` + `yqroonkq` — vt100 ScreenState wrapper + serve_loop 統合 (Phase A)

**self-check**:
- [✓] **既存 DR で justify 済**: DR-0013 §1/§3/§4 Phase A、§6 alt screen hook。
- [✓] **透過原則を破る? 必然?**: read-only 観測 (= 子 PTY bytes を vt100 parser に通すだけ)。bytes 自体は従来通り raw_data broadcast にも流れる。state 正本化は **介入ではなく観測**。
- [✓] **最小介入**: vt100 crate を素直に通し、wrapper は 100-150 行で alt mode prepend / sync flag / stalled timer のみ追加。
- [✓] **kernel/PTY/shell 標準の再発明否**: terminal emulator state は kernel/PTY/shell には無い概念 (= GUI terminal の責務)。tmux/zellij/wezterm/ghostty が独立到達した業界 standard pattern。
- [✓] **新 protocol 不要**: Phase A は raw_data frame 内に redraw bytes を流す。新 message なし。

**判定**: **A. 採用維持**。Phase A 全体が DR-0013 の正規実装。

---

### Item 2: `yqroonkq` — attach redraw (alt mode prepend `\x1b[?1049h/l`)

**self-check**:
- [✓] **DR justify**: DR-0013 §4 Phase A + §6、PoC §2 で実証された vt100 `state_formatted()` の alt フラグ欠落の補完。
- [✓] **透過と必然**: state 正本 → ANSI 再生成は業界 standard。子に bytes を送るのではなく、新 attach client に既存画面を見せるための **state 投影**。bytes 透過 (= 子→client 経路) は別経路 (raw_data) で並行維持。
- [✓] **最小介入**: prepend 1 行 + `state_formatted()` をそのまま結合。vt100 出力を後加工しない。
- [✓] **再発明否**: wezterm `sessionhandler.rs:119` / ghostty `Format.vt` / zellij `OutputBuffer::serialize` と同パターン。
- [✓] **新 protocol 不要**: 既存 TYPE_RAW_DATA frame に乗せるだけ。

**判定**: **A. 採用維持**。

---

### Item 3: `qywspxmn` (Phase B) — **DEC sync chunk 跨ぎ 7 byte sliding window carry**

**self-check**:
- [✓] **DR justify**: DR-0013 §6 で「DEC sync update を hook」と明記。vt100 0.16 が内部 hook しないため wrapper で補完するのは仕様通り。Phase B commit log でも option c (sliding window carry) の選択理由を明記。
- [✓] **透過と必然**: 子の bytes を hyoui 側で needle 検索する介入だが、attach redraw を **tear free** にするために必須 (= sync 中に部分 state を流すと client が中途半端な画面を見る)。alacritty `event_loop.rs:166` の DEC sync hook と同思想。
- [✓] **最小介入**: 7 byte carry のみ、内容は変更しない (= read-only 走査)。
- [△] **再発明否**: vt100 が将来 sync mode を内部 hook するならその時点で wrapper を撤廃すべき (= 改善余地)。 同等 sync 変種 (= `?2027h` 等) が業界に出てきた場合の追従コストは残る。
- [✓] **新 protocol 不要**: 内部 flag のみ。

**判定**: **B. 軽微な改善余地**。マトリクス検証 (= 巨大 sequence 中の sync、ネスト sync) で必然性を裏付け、vt100 upstream 動向 watch を ROADMAP に追加するのが望ましい。

---

### Item 4: `qywspxmn` (Phase B) — **stalled detect 3 連続自動 reset (= 15s で partial sequence 捨てる)**

**self-check**:
- [△] **DR justify**: DR-0013 §5 で「5s timeout で stalled detect、Phase A は detect-only、reset は Phase B で再検討」と明記、本 commit が「3 連続なら自動 reset」を導入したのは **DR の再評価結果が DR に書かれていない**。 commit message に「保守的設定」とあるが、3 連続 = 15s の根拠は不明 (= サンプル数 0、broken stream の現実頻度不明)。
- [△] **透過と必然**: ResetRequested → `reset()` は **parser 内部 buffer clear (= partial sequence + sync flag + carry buffer 全破棄)**。子の bytes を hyoui が「壊れている」と判定して情報を捨てる強い介入。tmux `input.c` も 5s timeout 後の挙動は実装ごとに違う (= warn のみ / reset 等)、業界 standard とは言えない。「異常 stream の復旧」必然性自体は妥当だが、**自動 reset まで踏み込む必然は薄い** (= warn のみで人間が `screen reset` 相当 CLI を叩く方が透過原則的)。
- [△] **最小介入**: 5s warn → 15s reset の 2 段階だが、Phase A の「Phase B で再検討」は本来「warn + human-triggerable reset CLI」が筋。自動 reset は人間判断を奪う。
- [△] **再発明否**: kernel/PTY 標準には「stream 異常検出」概念は無いので新規概念だが、shell 等の上位プロセスが必要に応じて行うべき判断を hyoui daemon に持ち込んでいる。
- [✓] **新 protocol 不要**: 内部 only。

**判定**: **C. 要再評価**。
**改善案**: 
- 自動 reset を default OFF、`HYOUI_STALLED_AUTO_RESET=1` で opt-in、または `hyoui screen reset <session>` CLI 経由の手動 reset に移行する。
- 自動 reset を維持する場合、DR-0013 を update して「3 連続 = 15s」の根拠 (= 既存 multiplexer 調査 + 想定 broken stream パターン) を明記する。
- マトリクス検証要セル: (a) 巨大 OSC52 sequence (15s 超の large clipboard paste)、(b) 部分送信される DCS sixel 画像、(c) ネスト sync update、(d) 子プロセスが PTY を slow に書く SIGSTOP/SIGCONT 中。

**マトリクス検証要否**: **要**。上記 4 セル最小。

---

### Item 5: `qywspxmn` (Phase B) — **input bytes log + resize replay (primary buffer)**

**self-check**:
- [✓] **DR justify**: DR-0013 §7 で詳細仕様まで明記、PoC §6 で vt100 `set_size` の reflow 制約を実証済。
- [△] **透過と必然**: 子 bytes を ring buffer に貯めて resize 時に replay する = bytes に手は加えないが、**vt100 内部 state を resize 時に再構築する目的で再使用**している。alt screen 中は push を skip、resize の trigger は WINCH 経由で **子に伝わってから replay** という 2 経路同時駆動。必然性 (= reflow 不能を補う) は OK だが、「resize 中の race condition で半端な state が他 client に流れない」保証は spec で書かれていない。
- [△] **最小介入**: 1 MiB ring の memory cost は明示済だが、tuning 別 task。
- [✓] **再発明否**: tmux/screen の reflow も似た「過去 bytes を再 feed」pattern (= dvtm pattern)。
- [✓] **新 protocol 不要**: 内部 only。

**判定**: **C. 要再評価**。
**改善案**:
- spec として「resize 中は新 client への redraw を sync flag 同様に defer する」を明記。
- replay 中の sync detect は新 Parser 上で再発火する設計を test で証明。
- マトリクス検証要セル: (a) claude TUI 中の cols 80→40→80 resize、(b) alt screen 中の resize 直後に primary 復帰、(c) 1 MiB log 上限直前での resize (= log evict 後の replay の正しさ)、(d) resize と同時に新 client attach (= race condition)。

**マトリクス検証要否**: **要**。上記 4 セル最小。

---

### Item 6: `qywspxmn` (Phase B) — structured snapshot wrapper (sparse cells + bit pack)

**self-check**:
- [✓] **DR justify**: DR-0013 §9 + §11 で確定、PoC §9 の「283 倍膨張」regression test 込みで実装。
- [✓] **透過と必然**: read-only な state observation、子に bytes を送らない。
- [✓] **最小介入**: 空 cell skip + attribute bit pack + variant 整数化、RLE は MVP 見送り (= 過剰設計回避)。
- [✓] **再発明否**: CBOR serialize の hand-rolled compaction は debug protocol として hyoui 固有の必要、kernel/shell には対応概念なし。
- [✓] **新 protocol 必然性 DR 化済**: cap flag `screen-dump-v1` / `state-snapshot-v1` は DR-0013 §9 + §10 で明記、breaking change なし。

**判定**: **A. 採用維持**。

---

### Item 7: `kquvykqu` + `rvszlymy` + `nqxmnvqn` — state-based wait (`wait_core`, `hyoui wait`, `input wait:` spec)

**self-check**:
- [✓] **DR justify**: DR-0006 §9 改訂 + DR-0013 §9 (snapshot protocol を polling)。
- [✓] **透過と必然**: client 側 polling、daemon に新規負荷なし (= 既存 snapshot を読むだけ)。子 bytes には触らない。
- [△] **最小介入**: 100ms polling は MVP として妥当だが、 push 型通知 (= DR-0013 §4 Phase B の `DirtyLinesNotify`) で polling 不要化が可能。DR-0013 §4 で言及済。
- [✓] **再発明否**: shell 標準には「terminal visible match」概念なし、hyoui 固有の自動操作 API。
- [✓] **新 protocol 必然性 DR 化済**: 旧 wait protocol 削除 (= dead code 化解消)、新 snapshot protocol で代替。

**判定**: **A. 採用維持** (= Phase B push 型化は ROADMAP 既存項目)。

---

### Item 8: `nqxmnvqn` — wait-idle SequenceNo polling (= MVP Phase A1)

**self-check**:
- [✓] **DR justify**: DR-0013 §3 で SequenceNo を「Phase A から仕込む」と明記。
- [✓] **透過と必然**: client polling、子に影響なし。
- [△] **最小介入**: 「SequenceNo 観察で idle 判定」は近似 (= 真の idle = 子が input 待ち状態とは別概念)。本来は daemon-side `last_input_at` (= 親→子 入力が一定時間ない) の方が semantics として正しい。commit message にも「Phase A2 で daemon-side last_input_at に差し替え予定」と明記。
- [✓] **再発明否**: idle 判定 primitive は kernel/PTY に無い、hyoui 固有。
- [✓] **新 protocol 不要**: snapshot 既存。

**判定**: **B. 軽微な改善余地**。MVP として OK、Phase A2 (= daemon last_input_at) の DR 起票が望ましい。

---

### Item 9: `vouwlqmk` + `rzzzmnop` + `lopyozlm` — lock acquire の「block until signal/EOF」設計

**self-check**:
- [✓] **DR justify**: DR-0006 §7 改訂 + daemon-side wait queue が MVP 未実装の補完。
- [✓] **透過と必然**: client の挙動を変更 (= block する) だけ、daemon / 子には影響なし。
- [△] **最小介入**: socket EOF / SIGINT/SIGTERM/SIGHUP / stdin EOF の 4 路 poll は妥当だが、stdin EOF の解釈は **interactive 用 hyoui CLI で stdin が tty の場合に意図しない release** を引き起こす可能性 (= subshell 内の hyoui lock acquire を stdin redirect で起動するケース)。
- [✓] **再発明否**: shell 標準には atomic lock primitive なし、hyoui daemon 機能の正当な拡張。
- [✓] **新 protocol 不要**: 既存 lock 周りで完結。

**判定**: **B. 軽微な改善余地**。マトリクス検証 (= subshell stdin tty / pipe / closed の 3 ケース) で挙動を確認、必要なら `--no-stdin-eof` flag 追加。

**マトリクス検証要否**: 軽め。stdin 形態 3 ケース。

---

### Item 10: `tqzywvxk` — file: spec の size 上限 + type validation

**self-check**:
- [✓] **DR justify**: DR-0006 §8.6。
- [✓] **透過と必然**: client 側のみ、子に影響なし。security 観点 (= 巨大 file 誤指定、device file の無限 read) は正当。
- [✓] **最小介入**: `metadata.is_file()` + size flag/env、追加 dep なし。
- [✓] **再発明否**: file 安全性は OS の責務だが、device file の無限 read 等は CLI 側で守るのが妥当。
- [✓] **新 protocol 不要**: client 側 only。

**判定**: **A. 採用維持**。

---

### Item 11: `kvulpnqm` + `yomyoyor` — edit-distance typo suggest + Unicode key alias

**self-check**:
- [✓] **DR justify**: DR-0006 §8.4 (key alias) + cli-design-preferences 一般 UX。
- [✓] **透過と必然**: CLI 入力解釈のみ、子に届くのは bytes (= alias は parse 段で展開、最終的に既存 key 名と同じ bytes を生成)。透過原則の対象外。
- [✓] **最小介入**: Levenshtein 距離 1 のみ救う、crate 追加せず自前 30 行。
- [✓] **再発明否**: CLI UX 範疇、kernel/shell 無関係。
- [✓] **新 protocol 不要**: CLI 内完結。

**判定**: **A. 採用維持**。

---

### Item 12: `rtkpmkto` + `xwpskuyo` + `zlvzktwr` — 旧 wait protocol 層削除

**self-check**:
- [✓] **DR justify**: DR-0006 §9 改訂で state-based に移行済、layer 削除は dead code 整理。
- [✓] **透過と必然**: 削除なので過剰介入を減らす方向 (= good)。
- [✓] **最小介入**: 削除のみ、置換実装なし (= 既に state-based に移行済)。
- [✓] **再発明否**: N/A (削除)。
- [✓] **新 protocol 不要**: cap flag `wait-l0` も廃止、breaking ではない (= 旧 cap 受理側 client は単に新規則で動かないだけ、本 daemon は新 cap で動く新 client を相手にする)。

**判定**: **A. 採用維持**。

---

### Item 13: `oqxxzvwl` + `kskwoqsk` + `uxvywowl` 他 — DR-0006 §8-§11 改訂 + 旧 DR 整合 annotate

**self-check**:
- [✓] **DR justify**: 自己改訂。DR-0013 整合のための spec 更新。
- [✓] **透過と必然**: doc only、code 挙動は変えない。
- [✓] **最小介入**: archive section に旧仕様保全、breaking change なし。
- [✓] **再発明否**: N/A。
- [✓] **新 protocol 不要**: N/A。

**判定**: **A. 採用維持**。

---

### Item 14: `nttsvtqo` + `lsurxnpp` + `ytnrnzym` + `lkqyuumk` 他 — CLI 拡張群 (screen dump / snapshot, input family handlers, tail align)

**self-check**:
- [✓] **DR justify**: DR-0006 §8-§11 改訂、DR-0013 §9 protocol。
- [✓] **透過と必然**: CLI 層のみ、daemon / 子に影響なし。
- [✓] **最小介入**: subcommand 構造 (= screen / input)、forward-compat option (= `--layer=scrollback` 等 daemon 側未実装) で過剰設計を回避。
- [✓] **再発明否**: CLI UX 範疇。
- [✓] **新 protocol 必然性 DR 化済**: DR-0013 §9 / DR-0006 §10 で明示。

**判定**: **A. 採用維持**。

---

## 総合評価

### 透過原則の継続的遵守状況

**概ね遵守**。本 session で道具揃え (= Phase A vt100 統合、CLI 拡張、protocol 整理) を行ったが、**子 bytes に対する hyoui 側の裁量判定は最小限**に保たれている:

- **子→client 経路**: vt100 parser に通すだけ、bytes 自体は raw_data broadcast にも流す。 介入は alt mode prepend (= state 正本からの再描画必要) のみで、これは bytes 経路の補完。
- **client→daemon→子 経路**: 何も介入していない (= raw_data frame 素通し)。
- **新 protocol**: `screen-dump-v1` / `state-snapshot-v1` を追加したが、 cap flag で gating + 既存 client は無視可能、DR-0013 §9-§10 で必然性 justify 済。

### 例外 (= 介入として妥当性に疑義あり)

1. **stalled 3 連続自動 reset** (Item 4): 子 bytes を「壊れている」と判定して情報を捨てる強い介入。**C 区分**。
2. **input bytes log resize replay** (Item 5): bytes を貯めて再 feed する仕組み自体は §7 で justify 済だが、resize race 周りの spec 不足 + マトリクス検証なし。**C 区分**。

### 本 session 全体の品質判定

- **道具揃え順序 = 概ね正当**: DR-0014 §例外 (= 観測道具自体が未実装の場合は推測実装で先に道具を作る) に該当。Phase A / Phase B は DR-0013 で先行確定済の仕様に従って実装したため、推測ではなく design に基づく。
- **Phase B で混入した anti-pattern 候補が 2 件**: stalled 3 連続 reset と input log resize replay。DR-0014 制定前に実装されたため self-check を経ていない (= 制定後の retrospective audit が本 doc の目的)。
- **DR-0014 制定後の作業 (= harness 整備 + smoke test 起票)**: 全て A 区分、マトリクス検証道具の正当な準備。

## 次のアクション

### マトリクス検証で確認すべき cell

DR-0014 §マトリクス検証の要件に従い、以下 cell を優先検証:

| # | cell | 確認対象 | 結果用途 |
|---|---|---|---|
| 1 | **巨大 OSC52 paste (= 15s 超 large clipboard) を子が送信** | stalled auto-reset が誤発火しないか | Item 4 改善判断 |
| 2 | **DCS sixel 画像の部分送信 (= 5s で停止 → 8s で再開)** | warn は出るが reset しない、再開後に描画完成するか | Item 4 改善判断 |
| 3 | **ネスト sync update (`?2026h` ... `?2026h` ... `?2026l` ... `?2026l`)** | sync flag が正しく carry されるか、stalled として誤判定しないか | Item 3 + Item 4 |
| 4 | **claude TUI 中の resize 80→40→80**: input log replay 後の画面が detach 前と同等か | log replay の正しさ | Item 5 |
| 5 | **alt screen 中の resize → primary 復帰** (= claude `:resize` 経路 + 終了) | alt 中 push skip + primary 復帰時 log 状態の整合 | Item 5 |
| 6 | **input log 1 MiB 上限直前での resize** (= log evict 後の replay) | line_count_offset が正しく適用されるか | Item 5 |
| 7 | **resize と同時に新 client attach** (= race) | redraw が中途半端な state を送らないか | Item 5 |
| 8 | **lock acquire を subshell stdin redirect で起動** (= stdin EOF 即時) | 意図せぬ release が起きないか | Item 9 |
| 9 | **lock acquire 中に daemon kill** (= socket close path) | 適切に exit するか | Item 9 |
| 10 | **wait-idle on 子 = bash REPL** (= 子に input なし、子 self-output あり) | SequenceNo polling で誤 idle 判定しないか | Item 8 |

最低 **cell 1-7** は Phase B の挙動確認として必須。Task #32 (= DR-0001 マトリクス検証実装) と並走して回せる。

### 修正の優先順位

1. **【高】Item 4 (stalled 3 連続 reset)**: マトリクス cell 1-3 を回し、誤発火が確認できたら `HYOUI_STALLED_AUTO_RESET=0` で default OFF、または warn のみに退避。DR-0013 §5 を update。
2. **【高】Item 5 (input log resize replay)**: マトリクス cell 4-7 を回し、replay race condition の spec を補強。必要なら DR-0013 §7 を update。
3. **【中】Item 3 (sync chunk carry)**: vt100 upstream の動向 watch + 同等 sync 変種 (`?2027h` 等) の調査を ROADMAP に追加。
4. **【中】Item 8 (wait-idle SequenceNo)**: daemon-side `last_input_at` への移行 DR 起票。
5. **【低】Item 9 (lock acquire block)**: stdin 形態マトリクス検証、必要なら `--no-stdin-eof` flag 追加。

### docs/issue/ 追跡

修正項目は `docs/issue/` に YYYY-MM-DD-self-audit-* 形式で起票するのが docs-knowledge-flow に則った形 (= 本 audit は findings/、修正は issue/ で trackable)。

## 本 audit で明らかになった DR / CLAUDE.md の改善案

### DR-0013 の補強候補

- **§5 stalled reset** に「3 連続 = 15s」の根拠 (= 既存 multiplexer 調査結果 + 想定 broken stream パターン) を追記、または default OFF に方針修正。
- **§7 input log resize replay** に「resize 中の新 client attach は redraw を defer」「replay 中の sync detect は新 Parser で再発火」を spec として明記。
- **§6 DEC sync** に「vt100 upstream で内部 hook 化されたら wrapper 撤廃」の追従計画を追記。

### DR-0014 の補強候補

- **self-check リストに新項目追加検討**: 「観測対象 (= 子 bytes / state) を hyoui 側で『異常』と判定して情報を捨てる介入を含むか?」「含む場合、warn のみで人間判断に委ねる選択肢を検討したか?」(= Item 4 のような stalled reset 系を将来同じパターンで弾けるようにする)。
- **Anti-patterns 節に追加候補**: 「子の bytes を hyoui daemon が裁量で『broken』判定して partial state を捨てる介入 (= 5s timeout 系)。warn + 人間 trigger reset CLI が原則」。
- **道具揃った段階の運用** に「Phase B / Phase C で追加した介入は、Phase A 完了後の retrospective audit を必須化」を追記 (= 本 audit のような流れを ritual 化)。

### CLAUDE.md の補強候補

- **検証主義** 節に「stalled / reset 系の判断は必ずマトリクス検証 + warn を default」を明記。
- **Anti-patterns** に「partial state の自動破棄」を追加。

## 関連

- [DR-0014](../decisions/DR-0014-transparency-and-empirical-verification.md) — self-check 5 項目の正本
- [DR-0013](../decisions/DR-0013-screen-emulator-and-attach-stability.md) — 本 audit の主要対象 (Phase A/B 実装)
- [DR-0001](../decisions/DR-0001-bgfg-jobcontrol-two-axis.md) — Item 4 と同類の「介入を transparent に保つ」教訓の正本
- [DR-0006](../decisions/DR-0006-cli-ground-rules.md) — CLI 拡張の正本
- `docs/journal/2026-05-27-*.md` — 本 session の生記録
