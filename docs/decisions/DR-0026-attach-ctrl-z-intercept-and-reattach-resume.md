# DR-0026: attach UX 拡張 — Ctrl+Z 折衷 intercept + 再 attach 時 stopped child auto-resume

- Status: Active
- Date: 2026-07-19
- Related: DR-0005 (思想 — 透過原則の許容範囲を再検討), DR-0013 (screen state 正本 — 将来の overlay 枠組み参照先), DR-0017 (session anchor + suspend policy — 本 DR が UX 層で継承), DR-0019 (follow ハードコード / notify default — client 側の対称拡張), DR-0024 (config.toml 機構 — 本 DR が `[attach]` セクションで相乗り)
- Origin: docs/QUESTIONS.md (2026-07-19 裁定完了、TSTP-Q0..Q3 + 2a)

## Context

kawaz ドッグフーディングで 2 件の UX 課題が浮上:

1. **Ctrl+Z 反射押し問題**: DR-0017 §柱1 の session anchor 化により Ctrl+Z byte (0x1a) は
   PTY line discipline 経由で本来の SIGTSTP 経路で動くようになった (= 直接起動と同じ
   セマンティクス、透過原則の勝ち)。ただし kawaz の使用パターンでは「detach したいとき
   反射で Ctrl+Z を押してしまう」ため、期待に反して子が stopped 化する。既存 detach key
   (`Ctrl-A d`) を使えば回避できるが、
   - 反射で押すキーを付け替えるのは学習コストが高い
   - `Ctrl-A d` 自体も raw mode で効かない疑いあり (別 issue で調査、本 DR とは独立)

2. **stopped 状態からの復帰が発見不能**: DR-0017 §柱2 で「daemon の無条件 auto-resume
   fallback」を廃止したため、child が stopped で detach された状態は正当な状態として
   放置される。復帰手段は `hyoui kill --signal=CONT` だが**一般ユーザには発見不能**で、
   attach し直しても raw 画面が凍結表示のままに見える (client 側は follow 済 = 対称)。
   DR-0019 §3 で無人 worker 用途の daemon 側 auto-resume policy は入ったが、attach する
   人間が居る場面での「再 attach = 復帰意思」という自然な UX signal は未活用。

いずれも DR-0017/DR-0019 の設計を撤回する話ではなく、**client 側の attach UX 層**で
覆いを足す話。

## Decision

### 1. Ctrl+Z 折衷 intercept (case G: debounce state machine)

attach client の stdin 経路に Ctrl+Z (0x1a) の state machine を追加する。既存の
detach prefix state machine (`process_detach_prefix`) と独立して動作する。

```
STATE_IDLE
  Ctrl+Z 受信       → 保留、SHORT_DEBOUNCE 起動、overlay 表示 → STATE_ARMED
  他 byte 受信      → forward

STATE_ARMED (SHORT_DEBOUNCE 期間中、保留 Ctrl+Z 1 発あり)
  Ctrl+Z 再受信     → detach 発動 (保留破棄)
  他 byte 受信      → overlay 消去 → 保留 Ctrl+Z を forward → 当該 byte forward
                     → STATE_GRACE (LONG_GRACE 起動)
  SHORT_DEBOUNCE 満了 → overlay 消去 → 保留 Ctrl+Z を forward
                     → STATE_GRACE (LONG_GRACE 起動)

STATE_GRACE (LONG_GRACE 期間中、Ctrl+Z を素通し)
  Ctrl+Z 受信       → 即 forward、LONG_GRACE リセット (延長)
  他 byte 受信      → 即 forward、LONG_GRACE 継続
  LONG_GRACE 満了   → STATE_IDLE
```

**設計判断**:

- **STATE_ARMED の他 byte 受信時は overlay 消去 → forward の順**。kawaz 明示指示 (2026-07-19
  QUESTIONS 応答)。表示遷移を bytes 送出より先にすることで race を排除。
- **STATE_GRACE の存在意義**: Ctrl+Z を子に届ける正当ユースケース (vim `:!bash` の子
  shell を止める / python REPL に SIGTSTP を送る / etc) を殺さない。1 発通した直後の
  連投は「意図的な Ctrl+Z 使用」と看做して素通しする。
- **overlay 表示**: Phase 1 では実装しない (Q1c 別 issue 化)。「仮想 screen state のみに
  overlay を挿入する枠組み」自体が DR-0013 の延長として一般機構化されるべきで、それ
  ができてから Phase 2 で乗せる。Phase 1 は state machine のみ動かし、user 学習は
  README / help / dogfooding にゆだねる。

**時定数の default**:

- `SHORT_DEBOUNCE = 300ms`: 反射の 2 連打を拾うのに十分、通常押しの遅延も体感しにくい
- `LONG_GRACE = 1500ms`: 連投許容窓として妥当、間空けたら再 detach 検知に戻せる

いずれも config.toml で調整可能 (§3)。

### 2. 再 attach 時に stopped child を auto-resume (case A)

attach client が handshake 応答で `child_stopped = true` を観測した際、attach mode が
**rw の場合のみ** 即座に `SessionChildResumeRequest` (= 既存 protocol) を送信する。

- `ro` (read-only 覗き見) attach では resume を送らない。「見に来ただけの人間が意図せず
  子を起こす」事故を防ぐ。
- daemon 側の `--on-child-suspend=notify|auto-resume` policy (DR-0019 §3) とは独立に動作する。
  daemon 側 auto-resume は「無人 worker の self-suspend からの自動復帰」、本 §2 は
  「有人再 attach 時の意思表明としての復帰」で発火 trigger が異なる。両者は排他ではなく
  合流可能。
- fresh attach (= 過去 detach なしの初回) では通常 child_stopped=false なので発火せず
  透過原則侵食なし。stopped 状態からの再 attach でのみ resume が走る = ユーザ動作が
  trigger である一貫性。

**透過原則との整合**: 本 §2 が resume を発火するのは「人間が rw attach した」事実に
基づく。attach mode = rw = 操作意思表明である以上、raw 画面が凍結したまま見えるのは
むしろ透過性の喪失 (= 直接起動なら fg した瞬間に走る)。DR-0017 §柱2 が「勝手に起こさない」
を守ったのは daemon 単独判断による resume の禁止であり、attach client 動作を trigger と
した resume は本 §で新規追加する介入だが、trigger が明示的な user action である点で
justify される。

### 3. config.toml `[attach]` セクション (DR-0024 config 機構への相乗り)

DR-0024 で導入した `~/.config/hyoui/config.toml` に `[attach]` セクションを追加。CLI flag
は追加しない (kawaz 明示: 「この手の UI 上の設定値なんてコマンド毎に触るもんでも無いし
設定でしょ普通」)。

```toml
[attach.tstp]
# Ctrl+Z (SIGTSTP byte) の attach client 側 intercept 挙動
intercept = true                # false で state machine 完全 bypass = Ctrl+Z 素通し
short_debounce_ms = 300         # STATE_ARMED 保留期間
long_grace_ms = 1500            # STATE_GRACE 素通し期間

[attach.resume]
# 再 attach 時に child が stopped だった場合の auto-resume 挙動
on_reattach = true              # false で resume 送信抑止 (現行 DR-0017 準拠に戻す)
# ro attach では on_reattach 設定に関わらず常に resume 送信しない
```

**default 値の justify**:

- `intercept = true`: kawaz 提示の主目的 (反射押し対応)、escape hatch は同 file で off に
  倒せる
- `short_debounce_ms = 300` / `long_grace_ms = 1500`: §1 の根拠
- `on_reattach = true`: §2 の主目的、opt-out は同 file

**Config module 拡張**: `crates/hyoui/src/config/mod.rs` (DR-0024 で新設) に `AttachConfig`
struct を追加、`ScrubEnvConfig` と対称配置。読み込み経路 (`Config::load`) を共有し、
新規 IO 経路を作らない。field 未指定時は上記 default をコード内で解決。

## Rejected alternatives

### 案 E: Ctrl+Z 単発 intercept、常に自 SIGSTOP

「Ctrl+Z を押した瞬間 client が食って自 SIGSTOP、子は継続」。学習ゼロだが、Ctrl+Z を
子に届けたい正当ユースケース (vim `:!bash` / python REPL 等) で escape hatch を毎回
探す必要がある。§1 の折衷案 (STATE_GRACE) はこの弱点を持たない。

### 案 F: prefix + z (例 `Ctrl-A z`) に suspend attach 動詞

既存 detach prefix state machine の拡張として動詞追加。Ctrl+Z byte は完全素通し。純潔
だが「反射で押すキーは Ctrl+Z」という kawaz の使用パターンに応えない (= 目的から外れる)。

### 案 D: 既存 detach key (`Ctrl-A d`) の案内のみ

学習前提の運用。kawaz が「反射で Ctrl+Z を押すのを何とかしてくれ」と要望しているので
本要件を満たさない。Ctrl-A d 経路自体が効いてない疑いあり (別 issue 調査中)。

### CLI flag `--tstp-mode=intercept|passthrough` を用意する

wire するコストが run/attach 両側にかかる。kawaz 明示「設定でしょ普通」により却下。
将来 CLI 側にも露出する必要が出たら非破壊で config.toml に上乗せ可能。

### daemon 側で Ctrl+Z を intercept する

master fd への write chunk を読んで 0x1a を除去する案。透過原則侵食が大きい (= 他 client
にも影響する)、かつ「reattach してる時だけ intercept したい」という個別性を表現できない。
client 側配置が唯一の合理配置。

## Consequences

- **透過原則侵食は最小限に留まる**: §1 は client の stdin 経路のみに介入 (子から見た
  PTY 挙動は変わらない)、§2 は user action (rw attach) を trigger とした介入で自動
  発火しない。
- **breaking change なし** (v0.x): 既存 attach 経路は intercept off / on_reattach off で
  従来通り動く。config.toml 追加のみ、既存 field 変更なし。
- **DR-0017/DR-0019 は不変**: 本 DR は両者の Decision を撤回しない。DR-0017 §柱2 は
  「daemon 単独判断の resume 禁止」として維持、DR-0019 §3 は「無人 worker 用途の daemon
  auto-resume」として維持。本 DR は「有人 attach 場面の client 動作」として直交する
  レイヤに介入。
- **overlay 表示は別 issue**: DR-0013 の延長として「screen state 一時 overlay の一般機構」
  が整備されるまで overlay 表示は Phase 2。issue 起票必要 (本 DR の Implementation phase で
  同時起票)。
- **Ctrl-A d 効かない疑いは別 issue**: 本 DR の Q1 案 G と独立で bug 調査 + fix。
- **実装後の検証要件 (DR-0014 マトリクス)**:
  - Ctrl+Z 単発 → SHORT_DEBOUNCE 満了 → 子に届く (通常 suspend パス) を 3 category
    (vim / python REPL / bash) で確認
  - Ctrl+Z 連打 → detach 発火、子が走ったまま外側 shell に戻る、`fg` → attach 復帰の
    完全ラウンドトリップ
  - STATE_GRACE 中の Ctrl+Z 連投がすべて子に届く (vim `:!bash` を止めて再開の
    ワンショット等)
  - `intercept = false` で state machine 完全 bypass (= 現行動作に戻る)
  - rw attach 時の stopped child auto-resume、ro attach 時の抑止、fresh attach での no-op
  - config.toml 未存在 / field 部分欠落時に default で動く

## Implementation

- `crates/hyoui/src/config/mod.rs`: `AttachConfig` (tstp / resume) 追加、`Config` に統合
- `crates/hyoui/src/client/attach.rs`: 既存 `process_detach_prefix` と並列で `process_tstp`
  相当の state machine 追加、config 参照で on/off + 時定数解決
- `crates/hyoui-cli/src/main.rs` attach_command: handshake response の `child_stopped` を
  見て rw かつ config `on_reattach=true` なら `SessionChildResumeRequest` 送信 (既存 protocol
  message 利用、cap 追加なし)
- テスト: state machine 単体 test (chunk 境界跨ぎ / 時定数 / grace 挙動) + e2e (config
  未存在 / on/off / 時定数変更 / rw vs ro attach)

## 関連

- [[DR-0005]] — 思想 (= 本 DR は透過原則の許容範囲を「client 動作 trigger」で拡張)
- [[DR-0013]] — screen state 正本 (= 将来 overlay 表示枠組みの母体)
- [[DR-0017]] — session anchor + suspend policy (= 本 DR が UX 層で継承)
- [[DR-0019]] — follow ハードコード / daemon side auto-resume (= 本 DR は attach client 側の
  対称拡張)
- [[DR-0024]] — config.toml 機構 (= 本 DR が `[attach]` セクションで相乗り)
- docs/QUESTIONS.md 2026-07-19 完了分 (TSTP-Q0..Q3 + 2a)
