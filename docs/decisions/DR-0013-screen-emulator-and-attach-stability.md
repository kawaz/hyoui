# DR-0013: screen emulator + attach/detach 安定化 + データモデル統一

- Status: Accepted
- Date: 2026-05-27
- Related: [[DR-0005]] (思想), [[DR-0006]] (CLI ground rules, §8/§9 は本 DR と整合性 annotate 必要), [[DR-0008]] (protocol、structured state access message 追加), [[DR-0009]] (session 分割、emulator 統合先), [[DR-0010]] (input family 整理), [[DR-0011]] (observability), [[DR-0012]] (signal wire name)

## Context

### 背景: 実機検証で発覚した 2 重大欠陥

本セッション (= cmux-msg 連携 / claude TUI 観戦) の実機検証で、現行 hyoui daemon に
以下 2 つの致命的欠陥が見つかった:

1. **attach がほぼ機能しない**
   - attach socket は通るが client terminal に画面が再現されない
   - 子 (= claude TUI 等の alternate screen 常駐アプリ) は新 attach client を知らず redraw しない
   - resize 通知のみでは部分 redraw しか起きない
   - client が入力すると `press once more to exit` 等の**部分メッセージだけが流入**、画面崩壊
   - ユーザ期待 (= detach 時の画面 + 入力連動が綺麗に再現される) と乖離

2. **wait pattern の誤マッチ多発**
   - claude TUI は alternate screen を持ち、bg/fg 切替や redraw で全画面 ANSI を再送
   - scrollback に過去描画分が混じる
   - `wait --pattern "Continue?"` で過去履歴に誤発火が頻発
   - 「現在 visible state に対する match」が無いと使い物にならない

### 根本原因

両方とも **daemon が screen state を持っていない** ことに起因する。生 TTY bytes を
単純 broadcast している現行設計 (= abduco 流) のままでは、いくら attach / wait の
表面 API を整えてもこの 2 欠陥は解消しない。

### 解決方針

**daemon = screen state の唯一の正本** にする基盤 (= screen emulator) を入れる。
これが attach 復元 / wait L1 / tail L1 / snapshot / debug inspection の共通土台に
なる。「生 TTY bytes を触る部分を最小化、統一してデータを正確に扱える基盤」を
hyoui のコア哲学に据える。

### 先行調査 (= Phase 0 で実施済)

本 DR 起票前に 5 件の研究を実施し、結論を確定した:

- `docs/research/2026-05-27-screen-emulator-crate-comparison.md` — vt100 採用根拠 + 不採用案 (vte / alacritty_terminal / wezterm-term / termwiz)
- `docs/research/2026-05-27-multiplexer-implementation-study-classic.md` — tmux / abduco / screen / dvtm の実装パターン (DEC Williams parser、SGR キャッシュ、ring buffer scrollback、attach 復元 Pattern A/B/C 等)
- `docs/research/2026-05-27-multiplexer-implementation-study-rust.md` — zellij / wezterm / alacritty の実装パターン (Render{content} push 型、SequenceNo pull 型、Arc<CellExtra> COW、mem::swap alt screen、DEC sync update 等)
- `docs/research/2026-05-27-ghostty-libghostty-study.md` — ghostty 補強 (= `formatter.vt` / `stream_readonly` が hyoui usecase 直系、libghostty-vt は将来再評価枠)
- `docs/research/2026-05-27-vt100-poc-report.md` — PoC 結果 (= 条件付き GO、reflow truncate 制約あり、wrapper で吸収可能)

handoff sketch (= `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md` §1-13) が
ベース。本 DR はその全項目を詳細化する。

## Decision

### 1. データモデル

- **daemon = screen state の唯一の正本**。生 TTY bytes は internal、client API は state 経由
- raw bytes layer (= TYPE_RAW_DATA) は維持 (= attach 復元の主送信路を兼ねる、§11 参照)
- screen state は **vt100 crate (= `Parser` + `Screen`)** で表現
- daemon process は session 寿命中ずっと alive にして state を保ち続ける (= cmux / ghostty
  surface object と同じアーキテクチャ判断)
- 子 PTY bytes は 1 度 vt100 Parser を通って state に反映され、client への送出は
  state を経由する形に統一する (= 生 byte の直接 broadcast はしない)

### 2. screen emulator crate 採用: **vt100 (0.16.2)**

採用根拠 (= `screen-emulator-crate-comparison.md` §6 から引用):

1. **目的が hyoui と一致**: README に「`screen` や `tmux` のような application を実装する」と
   名指し、daemon-as-screen-state の用途に直接設計されている
2. **`Screen::state_formatted() -> Vec<u8>`**: attach 復元の primitive がそのまま用意されて
   いる (= daemon が grid から ANSI を再構築する 200-500 行を書かなくて済む)
3. **依存 3 個** (= itoa / unicode-width / vte) で hyoui の lean 方針 (`nix + serde +
   ciborium + regex + thiserror`) と整合
4. **alternate screen / scrollback / cursor / wide char / grapheme cluster 全対応**
   (= `Cell::contents() -> &str` で combining char も保持)
5. **production 利用実績** 113 dependents / 約 80 万 downloads/month、active 維持
   (2025-07-12 commit、MSRV CI 整備済)
6. **license MIT** で hyoui の MIT と整合

採用判断は ghostty 調査 (= `ghostty-libghostty-study.md` §1.4) で補強される: ghostty 自身が
terminal core 分離 (= `lib_vt.zig` / `libghostty-vt`、`Format = .vt` で state→VT 再生成) を
進めており、vt100 と同じ思想に独立到達している。**業界 standard pattern**。

### 3. state 構造

vt100 の `Parser` / `Screen` をそのまま正本にし、hyoui 側は `crates/hyoui/src/daemon/screen/`
配下に wrapper module (= `VirtualScreen` 仮称) を新設して expose する。wrapper の責務:

| 項目 | 値 / API |
|---|---|
| cell grid | vt100 `Screen::cell(r, c) -> Option<&Cell>` / `Screen::rows(start, width)` を expose |
| cursor | vt100 `Screen::cursor_position() -> (u16, u16)` / `Screen::hide_cursor()` |
| mode | alt screen / app_keypad / app_cursor / bracketed_paste / mouse_protocol_{mode,encoding} を個別 getter で expose (vt100 既存 API) |
| buffer | `Primary | Alternate` enum で active 判定 (= `Screen::alternate_screen() -> bool`) |
| scrollback | vt100 内蔵 ring + size 上限は既存 hyoui 仕様を継承 (= `Parser::new(rows, cols, scrollback_len)` で指定、§8 参照) |
| window_size | `rows, cols` (= `Screen::size()`) |
| input bytes log | **primary buffer 用の bounded ring buffer** (= resize 救済策、§7 参照) |
| state line count | vt100 に `last_evicted_age` 相当が無いため自前 u64 counter で補完 (= PoC §5 で確認) |
| SequenceNo | per-line `u64` monotonic counter (= wezterm 流、Phase B incremental sync 前提、§4 参照) |

### 4. attach 復元 protocol (Phase A: push 型、Phase B: pull 型)

#### Phase A (必須): push 型 redraw bytes

attach handshake 完了直後、daemon は以下を 1 つの control message として client に送る:

```
ScreenStateInit {
    viewport_size: { rows: u16, cols: u16 },
    mode_flags: u32,            // alt_screen / line_wrap / cursor_key / bracketed_paste / ...
    cursor: { x: u16, y: u16, shape: u8, visible: bool },
    redraw_bytes: Vec<u8>,      // = state_formatted() + 補完 prepend (下記)
    current_seqno: u64,         // Phase B 用、Phase A では受信のみ
}
```

`redraw_bytes` の組み立て (= zellij `OutputBuffer::serialize` + ghostty `Format.vt` 相当):

1. alt screen mode (= `Screen::alternate_screen() == true`) なら冒頭に `\x1b[?1049h`
   (= PoC §2 で発覚した vt100 `state_formatted()` の alt フラグ復元欠如を補う、wrapper で 1 行)
2. `Screen::state_formatted()` の出力をそのまま結合 (= clear + cursor home + 各 cell の
   style 差分 SGR + cursor 位置設定 + cursor visibility + bracketed paste mode)
3. その他補完が必要な mode (= mouse_protocol_mode 等) を逆引きで prepend
4. cursor 行は最後に明示的に再描画する `\x1b[<y>;<x>H` を末尾に保証 (= wezterm
   `sessionhandler.rs:119` パターン)

client terminal は `redraw_bytes` を stdout に書くだけで detach 時の画面が復元される。

#### Phase B (優先): pull 型 incremental sync

reconnect / 高 RTT 環境での効率化のため、Phase B で wezterm 流 SequenceNo + pull
protocol を追加する:

```
DirtyLinesNotify { since_seqno: u64, dirty: Vec<Range<u32>>, current_seqno: u64 }
GetLinesRequest { rows: Vec<u32>, since_seqno: u64 }
GetLinesResponse { lines: Vec<(row_idx: u32, ansi_bytes: Vec<u8>, last_change_seqno: u64)> }
```

性質:

- **冪等性**: client が `since_seqno` を持っていれば「何が変わったか覚えていなくても resync
  できる」(wezterm pattern)
- **初回 attach も同じ API**: `since_seqno = 0` で問い合わせれば全行 dirty が返る
- **未取得 = placeholder 描画許容**: 網羅性より応答性 (wezterm `LineEntry::Fetching` パターン)

Phase A の `current_seqno` を最初から送っておくことで、Phase B 移行時に protocol を
拡張するだけで済む (= Phase A 単独でも完結する設計)。

#### Pattern 整理 (research 由来)

- Pattern A (tmux / GNU screen): flag set → 次 tick で grid から ANSI 再構築 → **Phase A で採用**
- Pattern B (abduco client): client 側で `\x1b[?1049h\x1b[H` を被せて detach 時に復元 → **client 側で並行採用** (hyoui daemon の Phase A redraw とは独立した「ユーザの本来の terminal を汚さない」工夫)
- Pattern C (tmux input.c の DEC Williams state machine): partial sequence は parser 内部 buffer に自然に貯まる → **§5 detach 時 flush の根拠**

### 5. detach 時の state flush

**結論: 明示的 flush 処理は不要**。

vt100 は内部に DEC Williams state machine を持つ (= `vte` crate ベース)。in-flight な
partial escape sequence は parser 内部 buffer (`interm_buf`, `param_buf`, dynamic
`input_buf`) に自然に貯まり、終端 byte (`ST` / `BEL` / final char) が来るまで dispatch
されない。detach / attach をまたいでも state machine が継続するので、明示的に
「flush」する必要がない (= classic study §3.3 Pattern C の発見)。

handoff sketch §6 で挙げていた「flush 課題」は emulator 採用で**消滅する**。

ただし health check として:

- **5 秒 timeout で stalled sequence を reset** (= tmux `input.c` 標準、broken byte
  stream で parser が永久に partial 状態に閉じ込められるのを防ぐ)
- timeout 発火時は warn ログ + parser internal buffer clear (= 過去 byte は捨てる)

#### Update (2026-05-27, post-DR-0014 audit): partial state 自動破棄の保守化

Phase B 実装で「stalled 5s × 3 連続 → 自動 reset」とした (= partial sequence を
vt100 state ごと捨てる強い介入)。DR-0014 制定後の self-audit
(= `docs/findings/2026-05-27-self-audit-after-dr-0014.md` Item 4) で
「partial state を裁量で破棄する介入」として識別、以下に補強:

- **判定基準の明示**: 3 連続検知は「typical SGR/CSI sequence は 1-30 bytes、5 秒で
  完結しないのは真に異常」という仮定に基づく。OSC52 (clipboard) の base64 巨大 paste
  / DCS sixel 部分送信 / ネスト sync update 等、子は正常だが時間がかかるケースで
  false-positive リスクあり
- **false-positive 対策**: `HYOUI_STALLED_AUTO_RESET=0` で default OFF 化を将来検討
  (= 別 task)、または warn のみ + 手動 reset CLI 提供
- **マトリクス検証要否**: 巨大 OSC52 paste / DCS sixel / ネスト sync update での
  false-positive 検証を DR-0014 マトリクス verification に登録 (= cell 候補リスト
  あり、audit findings 参照)

### 6. alternate screen hook

vt100 が `?1049h` / `?1049l` / `?1047h` / `?1047l` / `?47h` / `?47l` を内部処理する。
hyoui wrapper は以下を提供:

- `is_alternate_screen() -> bool` (= `Screen::alternate_screen()` をそのまま expose)
- attach 復元時の alt mode 復元 sequence prepend (§4 参照、PoC §2 で発覚した
  `state_formatted()` の欠落を補う、wrapper で 1 行)
- alt screen 切替時に **input bytes log の primary/alt 切替** (= §7 参照、primary
  log は alt 中も保持、alt screen 中の bytes は log に含めない)

### 7. resize + reflow 戦略

#### 制約

vt100 `Parser::set_size(rows, cols)` は **truncate のみで真の reflow なし** (= PoC §6 で
実証、80 cols → 40 cols → 80 cols で失われた文字は復元されない)。wezterm-term や
alacritty_terminal レベルの reflow 品質ではない。

#### 補完策

**primary buffer**: input bytes log を bounded ring buffer に保持し、resize 時に新
Parser を作って log を再 feed する形に倒す:

```
struct InputBytesLog {
    ring: VecDeque<u8>,         // primary buffer 専用の bounded ring
    capacity: usize,            // config 化 (default: 1 MB、tuning は別 task)
    line_count_offset: u64,     // ring 先頭が捨てた行数 (= last_evicted_age 補完用)
}
```

- 配置: `crates/hyoui/src/daemon/screen/input_log.rs` (新 module)
- 既存 `crates/hyoui/src/scrollback.rs` との関係: scrollback は vt100 内蔵 ring に統合
  (§8 参照)、`input_log` は **resize 救済専用の別 layer** として並存。両者の責務分離は:
  - scrollback (= vt100 内蔵): 過去 row へのアクセス + 描画用
  - input_log: resize 時の Parser 再構築 + 過去 bytes 再 feed 専用
- ring 上限到達時は最古 byte から drop、`line_count_offset` を進める
- alt screen 中の bytes は log に push しない (= alt は子側で再描画させる、下記参照)

**alt screen 中の resize**: 子に WINCH を送って子側で再描画させる (= 反映ロスは
気にしない、PTY 接続の自然な挙動)。`?1049l` で primary に戻った時点で primary log の
最後の状態が残っている。

#### multi-client 異サイズの戦略

tmux の `window-size` option (= `smallest` / `largest` / `manual` / `latest`) を参考に:

- **default: smallest** (= 全 client の `min(sx, sy)` を採用、tmux 既定)
- observe mode (= §12) client は計算から除外
- MVP は `smallest` 固定で実装、設定で 4 モード化は Phase C
- size 変更時は **全 client に同じ grid を送る** (= 個別 viewport は持たない、zellij pattern)

#### Update (2026-05-27, post-DR-0014 audit): resize race の spec

primary buffer 用 input bytes log で resize 時 replay する設計だが、以下 race を
spec として明示する (= `docs/findings/2026-05-27-self-audit-after-dr-0014.md` Item 5):

- **同時 attach race**: resize と同じタイミングで新 client が attach した場合、
  attach 復元 redraw は **resize 完了後の新 size** で生成される必要がある (= 旧 size
  で生成 → resize → 旧 cell が arrange 不能、を防ぐ)。実装上は resize completion を
  flag で gate
- **input log 上限到達 race**: log capacity 1 MiB 直前で resize → replay 時に log が
  既に古い byte を evict 済の場合、復元できる範囲は log 残存分のみ。これは仕様上の
  限界として明示 (= log size を増やすか、resize 後の画面が部分的に欠ける)
- **alt screen 中の resize**: alt 中は子に WINCH のみ、primary log への影響なし
  (= alt 中 push してないので)。alt → primary 復帰時は子側で再描画想定

これらは DR-0014 マトリクス verification の cell 候補。

### 8. scrollback 管理

- **vt100 内蔵 ring を主体**にする (= `Parser::new(rows, cols, scrollback_len)` で
  scrollback_len 指定、`Screen::set_scrollback(n)` / `Screen::scrollback() -> usize` で
  offset 制御)
- 既存 `crates/hyoui/src/scrollback.rs` は **vt100 wrapper に置換**する (= 二重管理を
  避ける)
- 過去 row への構造化アクセス API は vt100 wrapper で提供 (= `Screen::cell(r, c)` を
  scrollback offset 込みで expose)
- `last_evicted_age` は vt100 に public API がないため、wrapper で自前 `u64` counter を
  保持して補完 (= PoC §5 で確認、実装数十行)
- scrollback の reflow は **やらない** (= MVP は dvtm pattern、過去行は元の wrap のまま)。
  要求が出たら tmux pattern に拡張は別 task

#### Update (2026-05-27, Phase B 実装): byte-base / rows-base の責務分離方針に修正

Phase B 実装着手時に「既存 `scrollback.rs` を vt100 内蔵 ring に置換」を素直に実施すると、
**`hyoui tail` の `since_ms` / `since_strict` / `last_bytes` の byte-base timestamp 意味論が壊れる**
(= vt100 内蔵 ring は rows-base、timestamp は持たない) ことが判明。

修正方針:

- **byte-base 層 (= `scrollback.rs`)**: tail コマンド用に維持 (= timestamp filter / 受信時刻順)。
  そのまま残す
- **rows-base 層 (= vt100 内蔵 ring)**: cell 単位アクセス用、screen.dump / screen.snapshot の
  scrollback layer などで利用 (= Phase C で配線、現状は `Parser::new(_, _, 0)` で無効化)
- 両層を **責務分離** (= 同じ「過去履歴」概念を別レイヤーで持つ)、二重管理ではなく
  異なる用途の層と再定義

bytes ↔ rows の換算 (= §7 の旧記述「`scrollback_rows = scrollback_bytes / (cols * 4)`」) も
**廃止**。根拠が脆い (= cell byte 数は UTF-8 と style overhead で大きく揺れる) + tail 意味論を
保つために換算自体が不要。代わりに `screen_input_log_bytes` (default 1 MiB) を独立 config として
導入 (= Phase B 実装済)。

`last_evicted_age` counter (= 上記の自前 `u64`) は **Phase C で配線**。Phase B 時点で
vt100 内蔵 ring は無効化されているため、本 counter も未配線。

### 9. debug / inspection protocol (新規)

機械観察 / 自動テスト用に CBOR control message を 2 種類追加する:

```rust
// 1. raw bytes / formatted ANSI のダンプ
ScreenDumpRequest {
    format: enum { Binary, Ansi, Json, Cbor },
    layer: enum { Visible, Scrollback, Both },
    rect: Option<{ x: u16, y: u16, w: u16, h: u16 }>,
}
ScreenDumpResponse { payload: Vec<u8> }

// 2. 構造化 state snapshot
StateSnapshotRequest {
    include: Set<enum { Cells, Cursor, Mode, Style, Scrollback, WindowSize, Buffer }>,
}
StateSnapshotResponse {
    cells: Option<...>,
    cursor: Option<...>,
    mode: Option<...>,
    style: Option<...>,
    scrollback: Option<...>,
    window_size: Option<...>,
    buffer: Option<...>,
}
```

cap flag: `screen-dump-v1`, `state-snapshot-v1`

用途:

- **debug 目視**: daemon state が正しいか人間が確認 (= "現在 cell[3][5] は何"?)
- **自動 test**: 「特定操作後の visible に prompt がある」を predicate で書ける
- **自動操作の predicate primitive**: `wait` family の判定や hyoui 上位ツールの安定化
- **post-mortem**: 後から画面状態を再現

**本 DR では protocol だけ確定する**。CLI 露出 (= `hyoui screen dump <session>` 等) は
DR-0013 後の別 task。

### 10. DR-0008 連動

- 既存 raw bytes layer (= TYPE_RAW_DATA) は維持し、attach 復元の主送信路としても使う
  (= Phase A の `redraw_bytes` は raw frame で別 PDU、CBOR には乗せない、§11 参照)
- §9 の structured state access message を TYPE_CBOR_CONTROL 経由で追加
- cap flag negotiation の既存機構を活用 (= `screen-dump-v1` / `state-snapshot-v1` /
  Phase B 移行時に `dirty-lines-v1` を追加)
- breaking change なし (= 既存 client は新 message を見ない、cap flag で gating)
- PDU serial 番号 (= wezterm `codec/src/lib.rs:67` パターン) を CBOR control message に
  入れる検討 (= out-of-order tolerant + RTT 計測)。Phase B の実装で確定

### 11. CBOR serialization 方針

#### PoC で判明した制約

PoC §9 で確認: naive cell-level CBOR serialization は **283 倍に膨張** (= 24x80 grid で
`state_formatted()` 568 bytes に対して CBOR snapshot 161,066 bytes)。理由は cell ごとに
String + 2 Color enum + 5 bool + 2 wide flags を素直に encode しているため。

#### hybrid 戦略採用

| 用途 | serialization |
|---|---|
| 通常 attach 復元 (= Phase A `ScreenStateInit.redraw_bytes`) | **`state_formatted()` の raw bytes** (= TYPE_RAW_DATA、CBOR には載せない) |
| 構造化 snapshot 要求 (= §9 `StateSnapshotRequest`) | **圧縮 wrapper** (= 空 cell skip + 属性 bit pack + Color variant 整数化、TYPE_CBOR_CONTROL) |
| Phase B incremental (= `GetLinesResponse`) | per-line `ansi_bytes` を raw bytes、metadata だけ CBOR |

#### wrapper struct

`crates/hyoui/src/daemon/screen/snapshot.rs` (新 module、200-300 行想定):

- `CellSnap { content, fg, bg, attrs_packed, wide }` (`#[serde(skip_serializing_if =
  "is_default")]` 多用で空 cell を 0 byte 化)
- `ScreenSnap` で cells を `Vec<(row, col, CellSnap)>` の sparse 表現に
- Color は variant 整数化 (= `ColorSnap::Idx(u8)` / `Rgb(u8, u8, u8)`)
- attribute bits は `u16` に pack (= bold / italic / underline / inverse / dim / blink / strike / underline_style 2bit 等)
- RLE は MVP では入れない (= 実装量が増える、必要なら別 task)

#### zstd 圧縮

`redraw_bytes` (= raw bytes 層) で 32 bytes 超なら zstd 圧縮を載せる検討 (= wezterm
`codec/src/lib.rs:289` の閾値)。Phase B の負荷測定で必要と判断したら別 task で導入。
MVP では zstd 無しで開始。

### 12. 追加機能 (Phase C、優先度低め)

#### A. resize 無し ro 復元 ("observe mode")

attach 時に resize 通知を子に出さず、daemon screen state を **native size のまま**
表示する mode。

- 用途: 動いている claude セッションを別端末から触らず観戦 / 複数 client 異サイズの
  reflow 戦争回避
- 仕様: client terminal < daemon size = crop / > daemon size = padding (= 黒 fill or
  placeholder)
- flag: attach handshake に `--no-resize-propagate` (= 仮称) を追加
- 既存 leader の resize は引き続き効く、observe mode client は §7 の smallest 計算から
  除外
- Phase C で実装、本 DR では仕様だけ確定

### 13. 採用 pattern まとめ (research doc 由来の必須 10 件)

本 DR で hyoui に取り入れる pattern を 10 件抽出 (= classic / rust / ghostty study の
要点):

1. **redraw は flag set → 次 tick で grid から ANSI 再構築** (tmux `server-fn.c:
   server_redraw_client` + `screen-redraw.c`、Phase A の `ScreenStateInit` 送出に採用)
2. **alt screen は 2 つの独立 buffer** (= dvtm `buffer_normal` / `buffer_alternate`、
   alacritty `mem::swap(&mut primary, &mut alt)`、wezterm `Screen::alt_screen_is_active`)。
   vt100 内部で実装されているので hyoui wrapper は判定 API だけ用意
3. **SGR は前 cell との差分のみ吐く** (tmux `tty.c: tty_attributes` の `last_cell`
   キャッシュ)。vt100 `state_formatted()` は内部でこの最適化を行っているので便乗
4. **per-line `SequenceNo: u64` を最初から仕込む** (wezterm `screen.rs:909`)。Phase A の
   `current_seqno` で先取り、Phase B の `DirtyLinesNotify` で活用
5. **cell の lean 化 (= 24 bytes 程度の cell + COW で rare 属性を逃がす)** (alacritty
   `Cell { c, fg, bg, flags: u16, extra: Option<Arc<CellExtra>> }`、`const _: [(); 24] =
   [(); size_of::<Cell>()];` で size assert)。vt100 の `Cell` をそのまま使うので
   hyoui 側で追加実装は無いが、CBOR snapshot wrapper (§11) で同じ思想を採用
6. **DEC sync update (`?2026h`) を hook** (alacritty `event_loop.rs:166`、同期中は
   redraw event 抑制)。Phase A の実装で vt100 の sync hook を考慮 (= claude の
   `?2026h` 部分描画で tear なし)
7. **PDU serial 番号** (wezterm `codec/src/lib.rs:67`、out-of-order tolerant + RTT 計測)。
   Phase B の protocol 拡張で導入
8. **scrollback は ring buffer** (dvtm pattern、wezterm `VecDeque<Line>`)。vt100 内蔵
   ring を採用 (§8)
9. **readonly stream 分離** (ghostty `stream_readonly.zig` の design rationale = hyoui
   daemon の usecase 一致)。子 PTY からの bytes を state に流し込む層と、DSR/DA/cursor
   query 等の response が必要な action を判定する層を分離する hook point を MVP 時点で
   確保 (= 実装は Phase B 以降)
10. **state → VT 再生成は業界 standard** (vt100 `state_formatted()` / ghostty
    `Format.vt` / zellij `OutputBuffer::serialize` / tmux `tty.c`)。Phase A の
    `redraw_bytes` がこの pattern に乗っていることを設計の正当性として明記

## Rejected alternatives

### (a) vte 単体 (parser only)

- 主張: 軽量、screen state は自前で書けば lean
- 却下理由:
  - screen state を自前実装する規模が 2-3k LoC、alternate screen / scrollback / wide char /
    grapheme cluster / mode 管理 / state→ANSI 再生成すべて自前
  - `Perform` trait の dispatch (= CSI/OSC/DCS 各 method) を hyoui 内に書き下す行為が
    DR-0013 の scope 外まで膨らむ
  - vt100 が「vte + screen state」を完成形で提供しているのに car wheels を再発明する
    意味がない

### (b) alacritty_terminal

- 主張: production 実績最強 (= Alacritty 本体で daily 数万 dl)、grid モデルが堅牢
- 却下理由:
  - **attach 復元用の `state → ANSI sequence` 生成 API が無く**、vt100 採用の最大の便益が
    消える (= 自前で grid 走査 + ANSI 再構築を書くなら vt100 の優位性が逆転)
  - 依存 11 個 (= `home` / `polling` / `rustix-openpty` / `signal-hook` 等) が hyoui で
    既に nix 経由で実装済の領域と重複、binary size + build time に二重コスト
  - `Term::new` の signature が `D: Dimensions` + `EventListener` で trait 実装を強制、
    ergonomics が vt100 の `Parser::new(rows, cols, scrollback_len)` 1 行に劣る
  - 設計優先 (= `design-priority.md`) に従い「より正しい設計」を選ぶ

### (c) wezterm-term

- 主張: API ergonomics 最高 (= `Terminal::advance_bytes(impl AsRef<[u8]>)`)、grapheme
  cluster / bidi / image の完成度が高い
- 却下理由:
  - **crates.io に publish されていない** (= git dep 専用、`wezterm-term = { git =
    "...", rev = "..." }` 形式必須)。pkfire の `bump-semver` + Taskfile.pkl の version
    bump gate 思想と摩擦
  - 依存 23+ で `image` / `miniz_oxide` (= sixel/iTerm2 image) / `csscolorparser`
    (= CSS color) など hyoui MVP scope 外の機能向け dep が大量、binary size + compile
    time が桁違いに増える
  - `TerminalConfiguration` trait の数十 method 実装が必要、最小実装でも数十行の
    ボイラープレート

### (d) termwiz

- 主張: wezterm 系の cell + line + surface 操作 lib
- 却下理由:
  - **ドメインモデルが違う** (= `Surface` モデルで「自分が描画 driver」型、`Change` を
    流し込む形)。alternate screen / scrollback の概念が無く、hyoui の「PTY bytes を
    流し込んで screen state を作る」用途には**そのままでは使えない**
  - escape parser は別 module (`termwiz::escape`)、両者の合成は wezterm 本体ですら
    wezterm-term 側で実装している
  - 依存 30+ で `pest` / `pest_derive` / `fancy-regex` などの重量級が混入

### (e) libghostty-vt (= ghostty 由来の core library)

- 主張: ghostty 本家が library 化を進行中、production-grade な Zig 実装、`Format.vt`
  + `stream_readonly` の design rationale が hyoui と一致
- 却下理由 (= `ghostty-libghostty-study.md` §3 + §6 から):
  - **C API 未完成**: `include/ghostty/vt.h` は OSC / SGR / Key / Paste の parser のみ
    expose、Terminal / Screen / Stream 本体は C export されていない
  - Zig module 経由なら完全に揃うが Rust 採用は (a) cargo + Zig 連携、(b) Zig allocator
    と Rust GlobalAlloc の橋渡し shim、(c) API 不安定 (= 公式 warning「API は変わる」)、
    (d) build complexity の四重苦
  - libghostty (= big lib) は GUI + font + renderer 全部入りで lean 方針と完全衝突
- **将来再評価**: libghostty-vt が C API stable + semver annotate + 標準 allocator 対応に
  なれば swap 候補に上がる (= ROADMAP `追加予定` に記録)

## Consequences

### Positive

- **attach 復元が確実に動く** (= screen state 正本化、claude TUI 等 alt screen 常駐
  アプリの観戦が綺麗に再現される)
- **wait / tail / snapshot / input / lock / tx 等の上位機能が state ベースで自然に
  構築可能** (= 「現在の visible に対する match」「過去履歴の誤マッチ排除」が L1 で
  可能になる)
- **debug / test の信頼性が劇的に向上** (= screen.dump / state.snapshot で自動 test に
  predicate を書ける、post-mortem も再現可)
- **業界 standard pattern に乗る** (= tmux / zellij / wezterm / ghostty が独立到達して
  いる「daemon が state 正本 + state→VT 再生成」の系譜)

### Negative / リスク

- **input bytes log の memory cost** (= primary buffer の resize 救済策、§7)。MVP は
  1 MB default、tuning は別 task で確定。claude TUI は数時間で MB 級になりうるので、
  periodic に `state_formatted()` を取って古い byte を捨てる設計が必要 (= PoC §残懸念 2)
- **vt100 bus factor** (= 個人メンテ、abandon リスク)。対策: 万一の fork vendor 戦略を
  ROADMAP `追加予定` に記録 (= hyoui workspace に vt100 を vendor する手順)
- **CBOR snapshot wrapper の追加実装** (= §11、200-300 行)。MVP の実装ボリュームに
  加算
- **protocol breaking ではないが capability 追加** (= cap flag で互換維持、§10)
- **reflow truncate 制約** (= PoC §6)。input bytes log replay で吸収するが、ring 上限を
  超えた古い byte は失われる (= 完全 reflow ではない)

### 追加 dep の最終一覧

direct dep に追加:
- **vt100** (= 0.16.2)

vt100 経由で内部 transitive (= 既に hyoui に直接無いもの):
- itoa (= integer formatting)
- unicode-width
- vte (= 0.15.x、parser layer)

他 (= regex / thiserror / serde / ciborium 等) は hyoui 既存と共有。**direct dep 1 個追加 +
transitive 3 個程度**で済む (= alacritty_terminal の direct 11 / wezterm-term の direct 23+
と比較すると lean を維持)。

## Implementation Phases

### Phase A (必須最優先)

- `Cargo.toml` に `vt100 = "0.16"` を追加
- `crates/hyoui/src/daemon/screen/` module 新設
  - `mod.rs` — wrapper 公開 API
  - `virtual_screen.rs` — vt100 Parser 包む `VirtualScreen` struct (= 100-150 行)
  - 既存 `crates/hyoui/src/scrollback.rs` を vt100 wrapper に置換
- 子 PTY bytes feed 経路の変更 (= 既存 PTY read loop から `vt100::Parser::process` を
  呼ぶように差し替え)
- attach handshake redraw 実装
  - `Screen::state_formatted()` + alt mode prepend wrapper (= §4, §6)
  - `ScreenStateInit` control message 追加 (= raw bytes 層)
- alt screen hook (= `Screen::alternate_screen()` で判別、attach 復元時に補完)
- 既存 broadcast の attach 時動作変更 (= 生 byte broadcast → state 経由)
- DEC sync update (`?2026h`) 抑制 hook (= vt100 parser の同期 hook を考慮)
- 5 秒 stalled sequence reset (= §5 health check)

### Phase B (優先)

- input bytes log 実装 (= `crates/hyoui/src/daemon/screen/input_log.rs`、bounded ring、
  50-80 行)
- resize + replay 実装 (= §7、新 Parser 作成 + log 再 feed、`set_size()` を呼ばない設計)
- scrollback の vt100 統合 (= `scrollback.rs` 完全置換、`last_evicted_age` 補完 counter)
- debug / inspection protocol 実装 (= §9)
- DR-0008 cap flag negotiation 拡張 (= `screen-dump-v1` / `state-snapshot-v1` 追加)
- structured snapshot wrapper (= `crates/hyoui/src/daemon/screen/snapshot.rs`、§11、
  200-300 行)
- per-line SequenceNo + pull 型 protocol (= §4 Phase B、`DirtyLinesNotify` / `GetLines`)
- PDU serial 番号導入

### Phase C (優先度低)

- observe mode (= §12 A、`--no-resize-propagate`)
- multi-client resize 戦略の config 化 (= `smallest` / `largest` / `manual` / `latest`、
  tmux 4 モード)
- scrollback reflow (= 要求が出たら tmux pattern に拡張)
- zstd 圧縮 (= redraw_bytes 32 bytes 超で、Phase B 負荷測定後)
- 将来: libghostty-vt 安定化時の swap 評価

## 関連

- `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md` — 本 DR の起点 sketch
- `docs/research/2026-05-27-screen-emulator-crate-comparison.md` — crate 比較 (vt100 採用根拠)
- `docs/research/2026-05-27-multiplexer-implementation-study-classic.md` — tmux / abduco / screen / dvtm 教訓
- `docs/research/2026-05-27-multiplexer-implementation-study-rust.md` — zellij / wezterm / alacritty 教訓
- `docs/research/2026-05-27-ghostty-libghostty-study.md` — ghostty 補強 + libghostty-vt 将来枠
- `docs/research/2026-05-27-vt100-poc-report.md` — PoC 結果 (条件付き GO、reflow 制約)
- PoC code: `/Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/poc-vt100/crates/poc-vt100/`
- [[DR-0005]] — 思想 (外側自動操作主軸)
- [[DR-0006]] — CLI ground rules (§8 wait / §9 snapshot は本 DR と整合性を取る別 task で改訂)
- [[DR-0008]] — protocol (= 本 DR §10 で structured state access message 追加)
- [[DR-0009]] — session 分割 (= screen emulator 統合先は主に `daemon/` 配下)
- [[DR-0010]] — input family 整理 (= 本 DR の state-based 上位機能の前提)
- [[DR-0011]] — observability (= Phase A の log instrument を本 DR Phase A と並走)
- [[DR-0012]] — signal wire name (= protocol breaking change の前例)
- `docs/ROADMAP.md` — 本 DR 確定後、4 層列挙型 (必須 / 優先 / 追加予定) に再編集予定 (別 task)
