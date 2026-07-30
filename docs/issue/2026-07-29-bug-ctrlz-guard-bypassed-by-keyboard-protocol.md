---
title: "Ctrl+Z ガードが keyboard protocol 有効端末で完全に不発 (= 0x1a 単一 byte でなく CSI-u で届く)"
status: wip
category: bug
created: 2026-07-29T00:00:00+09:00
last_read: 2026-07-30T00:00:00+09:00
open_entered: 2026-07-29T00:00:00+09:00
wip_entered: 2026-07-29T00:00:00+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: docs/QUESTIONS.md RC-C1 (DR-0030 auto-resume の実機確認) から派生。kawaz 実機観測 2026-07-29 (brew 0.9.25、config 不在 = 全 default)
---

# 現象

Ghostty + `hyoui run -- claude ...` + attach 中に Ctrl+Z を押すと、DR-0029 §2 の
ガードが **一切効かない**:

- 単発 / 2 連打 / 3 連打のいずれでも **毎回**子 (claude) に届き、
  `Claude Code has been suspended. Run fg to bring Claude Code back.` が出る
- **単発での detach が起きない** (= 窓の中から抜けられない)
- DR-0030 の `resume_stopped_child` は効いており session は死なない

# 真因

ガードは **単一 byte `0x1a` だけ**を見る:

`crates/hyoui/src/client/attach.rs:31`
```rust
pub const CTRL_Z_BYTE: u8 = 0x1a;
```
`process_ctrlz_guard` (同 65 行) の match は `byte == CTRL_Z_BYTE` の比較のみ。

一方、TUI 子アプリが keyboard protocol を有効化すると、**外側端末は Ctrl+Z を
0x1a では送らなくなる**。claude code の起動時 raw 出力を実測 (0.9.26、`--debug-dump-server`):

```
\x1b[?2004h  \x1b[?1004h  \x1b[?2031h  \x1b[>1u  \x1b[>4;2m
```

- `\x1b[>1u` = kitty keyboard protocol push (disambiguate escape codes)
- `\x1b[>4;2m` = xterm modifyOtherKeys level 2

これらの escape は attach client を素通しして外側 Ghostty に届く (= 透過なので正しい)。
以後 Ghostty は Ctrl+Z を `\x1b[122;5u` (CSI-u、`z`=122 / ctrl=5) として送る。
ガードの byte 比較には 1 つも一致しないので:

1. detach タイマーが張られない → **単発 detach が起きない**
2. sequence は他キー扱いで丸ごと forward される → **連打数に関係なく毎回子へ貫通**
3. claude は自分が要求した符号化なので Ctrl+Z と解釈して self-suspend →
   DR-0030 の auto-resume が毎回起きる

DR-0029 Consequences の末尾が「keyboard protocol と Ctrl+Z の相互作用 (= CSI-u 有効端末で
0x1a が単一 byte で来るか) は残る観測課題」と書いていた懸念が、そのまま実機で顕在化した形。
DR-0029 の検証が `cat` / `bash -i` (= keyboard protocol を有効化しないアプリ) だけだったため
すり抜けた。

# 実測マトリクス (0.9.26、ネスト hyoui、config 不在 = default 500ms)

外側 `hyoui run --detached -- hyoui attach inner` の PTY に byte 列を注入し、
内側 `cat` セッションに何が届くか / 外側 attach client が detach するかを観測。

| 注入 | 期待 (DR-0029 §2) | 実測 inner (子) | 実測 outer (client) |
|---|---|---|---|
| `key:C-z` 単発 (= 0x1a) | 子へ 0 発 / detach する | `^Z` 無し ✅ | session 消滅 = detach ✅ |
| `key:C-z` 2 連打 | 子へ 1 発 / detach しない | `^Z` × 1 ✅ | live のまま ✅ |
| `hex:1b5b3132323b3575` 単発 (= CSI-u) | 子へ 0 発 / detach する | `^[[122;5u` がそのまま到達 ❌ | live のまま (detach せず) ❌ |
| 同 2 連打 | 子へ 1 発 / detach しない | `^[[122;5u` × 2 が到達 ❌ | live のまま (偶然一致) |

CSI-u を実際の claude セッションに注入すると `Claude Code has been suspended` + STATUS=stopped
になることも別途確認済 (= 子側の解釈も想定どおり)。

つまり **0x1a 経路のガードは正しく動いており、実装バグではない**。ガードの定義が
「byte 0x1a」に閉じていることが原因。

# 修正 (2026-07-29、DR-0029 §2 の範囲内で改訂)

ガードの認識対象を「byte `0x1a`」から「Ctrl+Z **キー押下**」へ広げ、あわせて
**符号化の知識を decode 層に隔離**した。判定表の正本は DR-0029 §2。

## 層構造

| 層 | 場所 | 責務 |
|---|---|---|
| decode | `crates/hyoui/src/client/key_decoder.rs` (新設) | byte stream → `KeyToken` 列。符号化差 / `read(2)` 境界の再組立 / 外れた prefix の flush を閉じ込める |
| policy | `process_ctrlz_guard` (`client/attach.rs`) | DR-0029 §2 の状態機械。トークンだけを消費し、**符号化を一切知らない** |

`KeyToken` は `CtrlZ { bytes }` と `Passthrough(bytes)` の 2 つだけ。`CtrlZ` が原 byte 列を
持つので、policy が「子へ 1 発届ける」判断をした時はそれをそのまま流せる (= `0x1a` へ
正規化しない。子は自分が要求した protocol の符号化を期待している)。

汎用 keymap 機構にはしていない (= 他キーを扱う予定が無い、DR-0029 §3 で prefix key 体系を
全廃済み)。キーを増やす必要が出たら符号化テーブルと `KeyToken` の variant を足すだけで済む。

## decode 層の規則

- 対象: `0x1a` / kitty CSI-u `CSI 122;5u` (event type `:1` `:2`、alternate key
  `122:122` の変種込み) / xterm modifyOtherKeys `CSI 27;5;122~`
- key release (`:3`) は押下ではないので素通し
- テーブルに無い符号化は素通し = ガード無効に倒れる (= 誤検出で無関係なキーを握り潰さない)
- `read(2)` 境界で割れた sequence は、符号化の途中まで一致している間だけ 20ms 保持して
  判定する。`ESC` 単打もこの保持に入るが、子アプリ側の `ESC` 曖昧性解決 timeout
  (通常 25ms 以上) より短いので Esc の解釈は変わらない
  (`lone_esc_is_released_within_child_disambiguation_budget` が定数として固定)
- 保持が満了した byte 列は捨てずに `Passthrough` として出す。run loop の poll timeout は
  detach 保留と保持期限の早い方に合わせる。20ms 以内に続きが来なかった分割はガードが
  効かず素通しになる (= 実端末は 1 キー = 1 write なので通常起きない、下記マトリクス末尾で実測)

DR-0029 §2 の意味論 (= 2 発ごとに子へ 1 発、余り 1 発で detach タイマー、他キー割り込みで
保留破棄、`delay=0` で即 detach、`guard=false` で完全 bypass) は不変。

## 分離作業中に自前で踏んだ bug (test で検出、修正済)

保持期限を `feed` のたびに張り直していたため、**入力が無い呼び出し (= poll 満了での
`feed(&[], now)`) でも期限が再武装され、未確定 byte を永久に握り続ける**状態だった。
期限を延ばすのは「続きの byte が実際に届いた時」だけに限定して解消
(`idle_feed_does_not_extend_hold` / `arriving_continuation_extends_hold` で固定)。

## repeat (`:2`) を press 扱いにした判断

指示は press (`:1`) と release (`:3`) の扱いのみだったが、repeat (= キー長押し) は
press 扱いで握る側にした。素通しにすると「Ctrl+Z を長押しした 1 回」で子が止まり、
DR-0029 の目的 (= 反射的な Ctrl+Z で子を止めない) の真逆になるため。

## 修正後の実機マトリクス (0.9.26 自ビルド `target/release/hyoui`、ネスト hyoui)

内側 = `sh -c 'stty raw -echo; exec cat -v'` (= 届いた byte を可視形でそのまま画面に出す)、
外側 = `hyoui run --detached -- hyoui attach <inner>`。外側 PTY に `hyoui input` で注入し、
内側は `screen dump --format=plain`、detach 有無は外側 `hyoui status` の生死で判定。

| 注入 | inner に届いた入力 | attach client | inner |
|---|---|---|---|
| 0x1a 単発 | (到達なし) | detach | live |
| 0x1a 2 連打 | `^Z` | 繋がったまま | live |
| 0x1a 3 連打 | `^Z` | detach | live |
| 0x1a 4 連打 | `^Z^Z` | 繋がったまま | live |
| CSI-u 単発 | (到達なし) | detach | live |
| CSI-u 2 連打 | `^[[122;5u` | 繋がったまま | live |
| CSI-u 3 連打 | `^[[122;5u` | detach | live |
| CSI-u release (`:3`) | `^[[122;5:3u` | 繋がったまま | live |
| modifyOtherKeys 単発 | (到達なし) | detach | live |
| modifyOtherKeys 2 連打 | `^[[27;5;122~` | 繋がったまま | live |
| CSI-u 単発を 1 byte ずつ連続 write | (到達なし) | detach | live |
| CSI-u 単発を 65ms 間隔で分割 | `^[[122;5u` | 繋がったまま | live |
| `ESC` 単打 + `a` | `^[a` | 繋がったまま | live |
| 他キー (`hi`) | `hi` | 繋がったまま | live |
| Ctrl+Z 単発 + 他キー (`x`) | `x` | 繋がったまま | live |

DR-0029 §2 の連打表 (1→0 / 2→1 / 3→1 / 4→2 発) が 3 符号化すべてで一致。detach した回でも
inner は live のまま (= 覗き窓を閉じても子は走り続ける)。最終行は「他キー割り込みで保留
Ctrl+Z を破棄」が効いていること (= `x` だけ届き `^Z` は消える) の確認。

### 分割着信の 2 行について

最初は分割注入を `hyoui input` の別プロセス呼び出しで組んだところ素通しになった。
計測すると **`hyoui input` 1 回あたり約 65ms** かかっており、保持期限 20ms を超えて
partial が満了していただけで、decoder の bug ではなかった。同一プロセス内で spec を
並べた連続 write (= 実端末の 1 キー = 1 write に相当) では正しく 1 キーへ再組立される。
両方を行として残し、fail-open の境界が実測で見えるようにしてある。

## 残作業 (= status を wip のままにしている理由)

**kawaz の実端末 (Ghostty × claude code) での最終確認が未了**。ネスト hyoui は外側端末の
符号化を byte 注入で模しているだけなので、実キーボード経由の確認は代替できない。
確認したいのは「Ghostty が実際に送る Ctrl+Z の byte 列が上表の 3 符号化のどれかに
収まっているか」で、外れていれば `CTRLZ_ENCODINGS` に 1 行足すだけで済む。

# 追加改訂 (2026-07-30 kawaz 裁定): 単発 = detach → **client suspend**、窓 1000ms

3 符号化対応を実機に載せたうえで、kawaz が単発時のアクションと窓の長さを裁定した:

> a 500ms じゃなく 1000ms に変更しといて。
> b ok
> ただし、ctrlz 1 回のときはデタッチじゃなく client の suspend にしてください。fg で
> すぐに戻れないのは不便すぎる。バックグラウンドのままの client は kill で始末なりしても
> 子プロセスには影響ないよう。また親シェルがいなくなったら suspend 中の client は
> 親なしのゴミプロセスとして残らず勝手に死んでほしい

反映内容 (DR-0029 §2 を改訂、正本は DR):

- **単発 = attach client 自身の suspend**。`OUTER_TTY_RESET` → `TtyGuard::suspend` →
  `sys::signal::suspend_self()` (= `SIGTSTP` を `SIG_DFL` に戻して `raise`) → `SIGCONT`
  復帰後に `TtyGuard::resume` + redraw 要求。外部 SIGTSTP を受けたときの既存経路
  (DR-0015 §2.3 の signal thread) と同じ helper を共有しており、新機構は足していない
- 再描画は独立 message が無いので既存 `SessionChildResumeRequest` (= daemon が redraw →
  SIGCONT の順で処理する) に相乗り。送る条件は DR-0030 の resume 条件と同じ
- 接続は畳まないので `RunOutcome::Detached` の発生源は stdin EOF / read error だけになった
- `ctrlz_guard_delay` default: 500ms → **1000ms**
- 連打規則 (2 発ごとに子へ 1 発 / 他キー割り込みで保留破棄 / 子へは受信符号化のまま) は不変

## 実機確認 (2026-07-30、macOS、debug 0.9.29、default 1000ms)

停止の観測には job control を持つ親 shell が要る (= test process 直下の client は pgrp が
orphan なので POSIX により SIGTSTP が破棄される)。入れ子 hyoui で測った:

| 操作 | attach client | 子 (`/bin/cat`) |
|---|---|---|
| `0x1a` 単発 | `T` (= stopped) | `S+` |
| 単発 → `fg` | `S+` (= 同接続で復帰、以後の打鍵が子へ届く) | `S+` |
| `0x1a` 2 連打 | `S+` (= suspend しない) | `T+` (= 0x1a が届いた) |
| CSI-u 単発 | `T` | `S+` |
| CSI-u 2 連打 | `S+` | `S+` + echo に `^[[122;5u` (= 符号化そのまま 1 発) |
| suspend 中に client を `kill -9` | 消滅 | `S+` / daemon も `child-state: running` |
| suspend 中に親 shell を `kill -9` | **500ms 以内に消滅** | `S+` |

親シェル消滅時の自滅は **POSIX の orphaned process group 規則**に乗っている
(= stopped メンバーを含む pgrp が orphan 化すると kernel が SIGHUP + SIGCONT を配送、
client は SIGHUP に handler を張っていないので default で終了)。`getppid()` 監視のような
保険コードは実測で不要と確認したため入れていない。`hyoui run -- <cmd>` 形 (= fork daemon +
exec attach) でも同結果。この 3 ケースは e2e
`crates/hyoui-cli/tests/ctrlz_suspend_client.rs` (`#[ignore]`、入れ子 PTY + signal) に固定。

# 修正前に検討した論点 (= DR 改訂の要否)

DR-0029 §2 の範囲内で解けると判断したが、以下は判断が要った点:

1. **ガードの対象を「byte」から「キーイベント」へ広げるか**。広げるなら client の
   stdin を最低限パースすることになり、「入力は完全透過」(DR-0005) との距離が変わる
2. **どの符号化を Ctrl+Z と認めるか**: kitty CSI-u (`CSI 122;5u`、event type 付き
   `CSI 122;5:1u`、alternate key 付き `CSI 122:122;5u`)、xterm modifyOtherKeys
   (`CSI 27;5;122~`)、素の 0x1a
3. **「子へ 1 発届ける」時に何を送るか**。0x1a に正規化すると、CSI-u を要求した子が
   受け取れる保証がない (= 子が期待する符号化のまま送るのが筋)
4. **chunk 境界**: CSI sequence は `read(2)` を跨ぎうるので、未完 sequence の保留
   buffer が要る。現行 state (1 bit) では表現できない
5. **他キー割り込みルールとの整合**: sequence の途中 byte を「他キー」と見なして
   保留をキャンセルしてはいけない

# 暫定回避

不要になった (= Ctrl+Z 単発で shell に戻れる)。ガードを切っている場合や
未知の符号化に落ちた場合は、別端末から `hyoui detach <session>`。
