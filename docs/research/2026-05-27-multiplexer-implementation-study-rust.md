# Rust 製 multiplexer / emulator の実装研究 — zellij / wezterm / alacritty

- Date: 2026-05-27
- 対象: hyoui の DR-0013 (screen emulator + attach/detach 安定化) に向けた実装パターン抽出
- 調査方法: 各リポジトリの主要 module を `gh api` でソース直読 (commit はその時点の `main`)。引用箇所は path + 行番号で示す。

> 用語: 「セル grid」「dirty seqno」「pull 型」「push 型」「stable index」は本ドキュメント内で繰り返し使う概念。
> hyoui は「daemon = state 正本、attach 復元は redraw シーケンスで決め打ち」が現在の設計方針。
> 本研究はその方針を支える実装パターンを抽出する。

---

## 1. 要約 (= 結論先出し)

### 各実装 1-2 行サマリ

- **alacritty (`alacritty_terminal` crate)**: シングル GUI クライアント向け **emulator 専業**。`Term<T: EventListener>` に grid を内包し、`Grid<Cell>` の active / inactive (= primary / alternate) を `mem::swap` で切替える素朴かつ堅牢な単一プロセスモデル。multiplexer 機能はない代わりに、`vte` parser + grid storage の実装は production-tested で hyoui の screen state core にそのまま流用候補。
- **wezterm (`term` + `mux` + `codec` + `wezterm-client` の 4 crate)**: GUI + multiplexer の二刀流。**`Pane` trait** を仮想化境界に、ローカル PTY / SSH / mux-server の domain 切替を実現。`SequenceNo` ベースの dirty 追跡 + **pull 型** render protocol (= client が `GetPaneRenderChanges` で diff を取り、`GetLines` で実 cell を fetch) で多クライアント / 高 RTT 環境にも耐える。**reconnect/attach 設計の参考価値は最高**。
- **zellij (`zellij-server` + `zellij-utils`)**: multiplexer 専業 + 独自 grid 実装。**push 型** で `ServerToClientMsg::Render { content: String }` として **ANSI 文字列を完成形で client に送る**。state 正本は server、client は dumb な writer。新規 attach 時は `OutputBuffer::update_all_lines()` で「全行 changed」flag を立てて全 viewport を 1 度 ANSI に serialize して送信。protocol は **protobuf**。hyoui の現方針 (= daemon = 正本、redraw シーケンス生成) に最も近い。

### hyoui への教訓 top 10

1. **state 正本は daemon でよい (= zellij 方式)** が、protocol は wezterm 方式の **pull + dirty seqno** が再接続時の冗長性と帯域効率で優位。hyoui には **pull の選択肢**を最初から protocol に持たせるべき。
2. **alternate buffer は `mem::swap(&mut primary, &mut alt)`** が標準パターン (alacritty `term/mod.rs:731`、wezterm `terminalstate/mod.rs:148`)。**buffer は常に 2 つ allocate しておく**。primary には scrollback、alt には無し (`max_scroll_limit=0` / `allow_scrollback=false`)。
3. **dirty 追跡には行単位の `SequenceNo` (monotonic u64)** を全行に持たせる (wezterm `screen.rs:909`)。「seqno X 以降に変わった行」が定数時間で算出できる。これが attach 復元の **diff push** と reconnect 後 **resync** の両方の基礎。
4. **cell の lean 化**: alacritty は `Cell { c: char, fg: Color, bg: Color, flags: Flags(u16), extra: Option<Arc<CellExtra>> }` で **24 bytes 固定** (`term/cell.rs:303`)。zerowidth / hyperlink / underline_color などレアな属性は `Arc<CellExtra>` に COW で逃がす。**hyoui もこのパターンを採用すべき** (= cell 表現は性能とメモリの中核)。
5. **scrollback は ring buffer**。alacritty は `Vec` + `zero` offset で modular rotate (`grid/storage.rs:33`)、zellij/wezterm は `VecDeque<Row>` で `push_back` + `pop_front`。**lean さでは VecDeque が勝つ**、性能では Vec+zero。hyoui は VecDeque で十分。
6. **resize で `rewrap_lines` (= 過去行の reflow) は primary screen のみ**、alt screen は単純 crop/extend (wezterm `screen.rs:233`)。**理由は full-screen app が再描画してくる前提**。hyoui の reflow 仕様もこれに従うべき。
7. **attach 時の cursor 行は常に bonus_lines に積む** (wezterm `sessionhandler.rs:119-122`): どんなアプリでも cursor 行が一番動くので、明示的に強制送信する。hyoui の redraw seq でも cursor 行は最後に必ず再描画する。
8. **redraw シーケンスの組み立て**: zellij は `OutputBuffer::serialize()` で viewport 全体を `CharacterChunk[]` に変換 → `serialize_chunks_with_newlines` で ANSI 列に展開 (`output/mod.rs:1183`)。style の差分計算 (= 前 cell との変化点だけ SGR 出す) も同 module 内で実施。hyoui の `attach 復元 bytes 生成` は同パターンが妥当。
9. **DEC sync update (`?2026h` 系) 対応**: alacritty は `state.parser.sync_bytes_count()` で **同期更新範囲内の bytes は redraw event を抑制** (`event_loop.rs:166`)。これで claude のような部分描画でも tear なし。hyoui も最初から `vte` parser の sync hook を考慮した emulator にする。
10. **Pane を `trait` で抽象化** (wezterm `mux/src/pane.rs:167`) = local PTY / remote mux / dead-pane を同一 interface で扱える。hyoui も serve gateway / observe mode / record-replay を見据えるなら **session の interface は trait** にしておく。

---

## 2. 各実装の architecture overview

### 2.1 alacritty (single-process emulator)

- workspace = `alacritty` (binary, GUI) + `alacritty_terminal` (core lib) + `alacritty_config` (`Cargo.toml`)。
- `alacritty_terminal` は `Term<T: EventListener>` を core 型として export。GUI 側 (`alacritty` crate) は `EventListener` を実装した event proxy を渡す。**emulator 自体は GUI / 描画から完全に独立** (no_std にはなっていないが思想的に分離)。
- スレッド 1 (PTY reader): `EventLoop::pty_read` が PTY bytes を読み、`vte` parser に流し、Mutex<Term> を lock して `Term` に書き込む (`event_loop.rs:104-171`)。
- スレッド 2 (GUI): winit の event loop。`Event::Wakeup` を受け取って `Term` を lock し、grid を描画。
- multiplexer 機能なし。**attach/detach 概念がない**。

### 2.2 wezterm (GUI + multiplexer 二刀流)

最も crate が多い (`Cargo.toml` workspace に約 30 crate)。本研究で核となる:

| crate | 役割 |
|---|---|
| `wezterm-term` (`term/`) | screen state core (= `TerminalState`, `Screen`)、`vtparse` + `termwiz::escape` で parse + perform |
| `mux/` | multiplexer の domain / pane / tab / window 抽象。`pub trait Pane` が境界 |
| `codec/` | client ↔ server の wire protocol (`Pdu` enum, leb128 framing, varbincode + zstd) |
| `wezterm-client/` | 別 mux-server に attach する client 実装。`RenderableInner` が LRU<StableRowIndex, LineEntry> で local cache |
| `wezterm-mux-server-impl/` | server 側 session handler。`SessionHandler::process_one` が Pdu dispatch |

- **GUI mode**: `wezterm-gui` が直接 `mux` を抱えて local pane を駆動 (= local domain)。
- **multiplexer mode**: `wezterm-mux-server` が daemon、`wezterm connect xxx` で client が unix socket / TLS で接続。
- **重要**: GUI 自体も `mux` 経由で pane を見るので、local pane と remote pane が `dyn Pane` で同じ interface (= attach 抽象化の鏡映)。

### 2.3 zellij (multiplexer + plugin host)

| crate | 役割 |
|---|---|
| `zellij-server/` | session daemon。`ServerInstruction` enum で `FirstClientConnected` / `AttachClient` 等を捌く |
| `zellij-client/` | client (= attach 側 TUI)。受信した `ServerToClientMsg::Render { content }` を stdout に書くだけ |
| `zellij-utils/` | ipc, types, layout, plugin API |
| `zellij-tile*/` | WASM plugin SDK |

- daemon = state 正本、client = **dumb terminal**。ANSI 文字列を server が組み立てて流す。
- transport = unix socket + **protobuf** (`zellij-utils/src/ipc.rs:311` `write_protobuf_message`)。
- 各 pane は `Grid` struct (`panes/grid.rs:594`) を持ち、cell 表現は `TerminalCharacter { character: char, styles: RcCharacterStyles, width: u8 }` で **16 bytes 固定** (`terminal_character.rs:929` の const assert)。

---

## 3. 採用 crate 観察

### 3.1 各実装の screen emulator スタック

| 実装 | parser | state | cell 表現 | reflow | scrollback |
|---|---|---|---|---|---|
| alacritty | 自前 `vte` (`alacritty/vte` crate、SoS で fork) | `alacritty_terminal::Term` (自前) | `Cell` 24 bytes + `Arc<CellExtra>` | wrapline flag のみ、簡素 | `Storage<Row>` (`Vec` + zero offset の ring) |
| wezterm | 自前 `vtparse` + `termwiz::escape::parser` | `wezterm-term::TerminalState`, `Screen` (自前) | `termwiz::cell::Cell` (`CellAttributes` を内包) | `Screen::rewrap_lines` で logical line を再分割 | `VecDeque<Line>` |
| zellij | **`vte` crate を依存に持つ** (`Cargo.toml [dev-dependencies] vte`)、production は自前の `Grid` で `vte` 上に組む | 自前 `Grid` | `TerminalCharacter` 16 bytes (style は `Rc<CharacterStyles>` でシェア) | wrap-only、reflow は限定的 | `VecDeque<Row>` (`lines_above` / `viewport` / `lines_below`) |

**結論**: **誰も外部 emulator crate は使っていない**。全員 vt parser (= `vte` 系) は外部、screen state は自前。
hyoui が `vte` parser を採用しつつ state は自前にする方針は **業界標準パターン**。`alacritty_terminal` を crate として再利用する選択肢もあるが、依存が大きい (= `bitflags`, `serde`, `regex`, `unicode-width` 等) + GUI 前提の event proxy が trait param に出ているのが面倒。

### 3.2 wezterm が `termwiz` (社内 lib) を分離している意味

`termwiz` は wezterm の workspace member だが、別 crate (`termwiz/`) として独立 publish もされている。**cell, line, surface, escape parser, color, hyperlink, image 等を re-usable な形で提供**。実際 zellij とは無関係に存在し得る独立 lib として作られている。
hyoui が将来「emulator core を別 crate で publish」したくなったら、`termwiz` の crate 分割粒度は良い参考になる (= screen state core / parser / cell types / surface diff)。

---

## 4. attach 復元シーケンスの実装パターン

### 4.1 zellij — push 型 + 全 viewport 再 serialize (= hyoui 現方針に最も近い)

`zellij-server/src/lib.rs:1084-1152` の `ServerInstruction::AttachClient` handler:

1. クライアントの `terminal_window_size` を含む `ClientAttributes` を作成。
2. `session_state.set_client_data(client_id, size, is_web_client)` で session 側 ledger 更新。
3. `ScreenInstruction::AddClient(client_id, is_web_client, size, tab_to_focus, pane_to_focus)` を screen thread に投入。
4. screen thread が `OutputBuffer::add_clients(&client_ids, ...)` で内部 chunk 領域を確保 (`output/mod.rs:385`)、続いて `OutputBuffer::update_all_lines()` で `should_update_all_lines = true` をセット (`output/mod.rs:1175`)。これで次の render tick で **全行 changed 扱い**になる。
5. 次の render: `Grid::render()` (`panes/grid.rs:1556`) が `read_changes` で `CharacterChunk[]` を生成 → `OutputBuffer::serialize()` で client ごとに `client_serialized_render_instructions: String` を組み立てる。chunk 毎に **前 cell の style と差分のみ SGR を出す**最適化が `serialize_chunks_with_newlines` (`output/mod.rs:1183`) 内で実施。
6. `ServerToClientMsg::Render { content }` として protobuf-encoded で client へ送信。**client は stdout に書くだけ**。

**hyoui への翻訳**:
- 「detach 時の state を再現する redraw seq」= zellij の `OutputBuffer::serialize()` 相当。
- 全 viewport を 1 度 `\x1b[2J\x1b[H` でクリアして、cell ごとに style 差分を出力、最後に cursor 位置を `\x1b[<y>;<x>H`。
- alternate screen mode なら冒頭に `\x1b[?1049h` を入れる (= zellij の `pre_vte_instructions` に該当)。
- mode 復元 (= `?7h`/`?7l` line wrap、`?25h`/`?25l` cursor visibility、`?1h`/`?1l` cursor key mode、`?2004h` bracketed paste 等) は **state から逆引き**して付与。

### 4.2 wezterm — pull 型 + LRU<StableRowIndex, LineEntry>

client 側は最初 state を持たない (`wezterm-client/src/pane/renderable.rs:56` `RenderableInner`):

```rust
pub struct RenderableInner {
    ...
    lines: LruCache<StableRowIndex, LineEntry>,
    pub seqno: SequenceNo,  // = 0 initially
    ...
}
```

接続後の流れ:

1. `SetClientId` PDU を送って自分の identity 登録 (`sessionhandler.rs:298-329`)。
2. `ListPanes` で pane 一覧取得 (`sessionhandler.rs:388`)。
3. 各 pane に対して poll loop (`renderable.rs:608` 付近) で `GetPaneRenderChanges { pane_id, seqno: current_local_seqno }` を投げる。
4. server side `compute_changes` (`sessionhandler.rs:52`) が:
   - `pane.get_changed_since(0..viewport_bottom, old_seqno)` で `RangeSet<StableRowIndex>` を取得
   - viewport 内の dirty lines は `bonus_lines: Vec<(StableRowIndex, Line)>` に詰めて即送り (= **prefetch**)
   - viewport 外で dirty なものは `dirty_lines: Vec<Range<StableRowIndex>>` に残す (= client が後で `GetLines` で取りに来る)
   - cursor 行は dirty かどうかに関わらず常に bonus_lines に push (`sessionhandler.rs:119-122`)
5. `GetPaneRenderChangesResponse` を client が受領 → `apply_changes_to_surface` (`renderable.rs:305`):
   - `bonus_lines` を `put_line` で LRU に直接挿入
   - 残った `dirty_lines` のうち viewport 内のものは `LineEntry::Fetching(now)` をマークして `schedule_fetch_lines` で `GetLines` 投げる
   - viewport 外は `make_stale` (= 後で fetch、cache hit でも stale 表示扱い)
6. 描画は GUI 側が `get_lines` を呼んで LRU から読み出す。LRU にない or `Fetching` のものは placeholder 描画 → fetch 完了後に redraw。

**hyoui への教訓**:
- 「**未取得 = placeholder で描画**」を許容する protocol になっている。これは網羅性より **応答性** を重視する設計。
- 「**seqno X 以降の dirty 行を出してくれ**」という pull の引数で **冪等性** を担保 (= 同じ seqno で問い合わせれば同じ答え)。これは network が落ちて再接続したときに「**何が変わったか覚えていなくても seqno さえ持っていれば resync できる**」という強力な性質。
- attach の **初回** は `seqno = 0` で問い合わせれば「全部 dirty」が返るので、特別な「初回 attach API」は不要。

### 4.3 hyoui に推奨する pattern (= zellij push + wezterm seqno のハイブリッド)

DR-0013 で hyoui が採るべき設計:

```
[Phase A: 必須]
attach handshake 完了 →
  daemon が「ScreenStateInit」CBOR を送る:
    - viewport_size: { rows, cols }
    - mode_flags: u32 (= alt_screen, line_wrap, cursor_key, bracketed_paste, ...)
    - redraw_bytes: Vec<u8> (= ANSI 列、上述 §4.1 の形式)
    - cursor: { x, y, shape, visible }
    - current_seqno: u64
  client は redraw_bytes を stdout に書くだけで detach 時の画面が復元される。

[Phase B: 優先]
seqno ベース incremental update:
  追加 message:
    - DirtyLinesNotify { since_seqno: u64, dirty: Vec<Range<u32>>, current_seqno: u64 }
    - GetLines { rows: Vec<u32> } → Lines(Vec<(row_idx, ansi_bytes)>)
  これで再接続時の resync が wezterm 並に効率的になる。
```

Phase A だけで **検証可能な MVP** になり、Phase B は後付け可能 (= seqno は最初から各行に持たせておけば protocol を拡張するだけ)。

---

## 5. screen state の data model 比較 (= Rust 所有権を含む)

### 5.1 alacritty `Cell` (24 bytes 固定)

`alacritty_terminal/src/term/cell.rs:134`:

```rust
pub struct Cell {
    pub c: char,         // 4 bytes
    pub fg: Color,       // ~6 bytes (enum)
    pub bg: Color,
    pub flags: Flags,    // u16
    pub extra: Option<Arc<CellExtra>>,  // 8 bytes (pointer)
}

// `CellExtra` is `Arc`-shared, COW via `Arc::make_mut`:
pub struct CellExtra {
    zerowidth: Vec<char>,
    underline_color: Option<Color>,
    hyperlink: Option<Hyperlink>,
}
```

- **`Arc<CellExtra>` の COW pattern** (`set_underline_color` で `Arc::make_mut`): 同一 hyperlink を持つ大量 cell が clone されても heap allocate は 1 個。
- size 制約: `const _: [(); 24] = [(); std::mem::size_of::<Cell>()];` で **コンパイル時 enforce** (`cell.rs:303`)。**24 bytes を超える変更を弾くガード**。
- `is_empty()` の判定で BG が default で attr が無いことを確認 → scrollback 削減 (`cell.rs:225`)。

### 5.2 wezterm `Line` (= `Vec<Cell>` + per-line seqno)

`termwiz::surface::line::Line` は cell の Vec に加えて:
- `last_change_seqno: SequenceNo` — 行が変わるたびに incremented
- `bidi_mode`, `appdata` 等

これにより `Line::changed_since(seqno) -> bool` が O(1) で `seqno > current_seqno_at_query_time` のチェックを実現 (`screen.rs:909-928`)。

`screen.rs:24` `lines: VecDeque<Line>` を持ち、`stable_row_index_offset: usize` で scrollback 蓄積による行番号ズレを吸収。**StableRowIndex は client が安全に持ち続けられる行 id**。

### 5.3 zellij `TerminalCharacter` (16 bytes、`Rc<Styles>` で共有)

`terminal_character.rs:921`:

```rust
pub struct TerminalCharacter {
    pub character: char,      // 4
    pub styles: RcCharacterStyles,  // 8 (Rc<...>)
    width: u8,                // 1 (+ 3 padding)
}
```

`RcCharacterStyles` は `enum { Reset, Some(Rc<CharacterStyles>) }` (`terminal_character.rs:160` 周辺、source 確認済)。**連続 cell が同一 style なら `Rc::clone` だけで済む** = 帯域メモリ両得。alacritty の Arc 版とほぼ同じ思想、ただし `Rc` (= single-thread) で `Send` を諦めて軽量化。

### 5.4 Rust 特有の所有権戦略まとめ

| 戦略 | 実装 | 適用先 |
|---|---|---|
| `Arc<T>` + COW via `Arc::make_mut` | alacritty `CellExtra` | thread 跨ぐデータ |
| `Rc<T>` 直シェア | zellij `RcCharacterStyles` | single-thread 高頻度コピー |
| `VecDeque<Row>` + offset | wezterm `Screen` / zellij `Grid` | scrollback ring |
| `Vec<Row>` + `zero: usize` modular index | alacritty `Storage` | 高速 rotate を可能にする ring |
| `bitflags!` + `u16`/`u32` | 全実装 | mode / cell flags |
| compile-time size assert | alacritty/zellij | cell サイズ regression 防止 |

**hyoui 推奨**:
- daemon は **multi-thread** 前提なので `Arc<CellExtra>` 戦略 (alacritty 版)。
- cell は **24 bytes 以内**に収める size assert を入れる (= 5000 lines × 200 cols = 100 万 cell で 24MB、十分軽い)。
- scrollback は `VecDeque<Row>` で素朴に。性能 hot path になったら storage を Vec+offset に切替。
- per-line seqno (`u64`) も持つ — Phase B の incremental sync の前提。

---

## 6. IPC protocol 比較

### 6.1 wezterm: leb128 framing + varbincode + zstd + serial 番号

`codec/src/lib.rs:60-108` `encode_raw`:

```
[leb128 tagged_len] [leb128 serial] [leb128 ident] [data bytes]
                ^ MSB が compressed flag (COMPRESSED_MASK = 1<<63)
```

- `serial: u64` — client が request に振った番号。response に echo されることで out-of-order 完了をハンドル。
- `ident: u64` — PDU type 番号 (`Pdu enum` の variant idx、`codec/src/lib.rs:464` 等)。**新 variant は新番号、削除も互換**。
- body は `varbincode::Serializer` (= bincode の variable-length integer 版) で serde 経由。32 bytes 超なら `zstd` 圧縮 (`codec/src/lib.rs:289`)。
- versioning: `pub const CODEC_VERSION: usize = 45` (`codec/src/lib.rs:444`)。client は接続時に `GetCodecVersion` で確認。

**hyoui への教訓**:
- すでに hyoui の TYPE_RAW_DATA / TYPE_CBOR_CONTROL の二層 framing は wezterm の `[len][ident][data]` と同型。
- **serial 番号は導入価値あり** (= out-of-order tolerant + RTT 計測 `last_input_rtt`)。CBOR map に `serial` field を入れる程度で済む。
- compression threshold 32 bytes は妥当 (= zstd の最小オーバヘッド超えライン)。

### 6.2 zellij: protobuf + length-delimited

`zellij-utils/src/ipc.rs:311` `send_client_msg`:

```rust
let proto_msg: ProtoClientToServerMsg = msg.into();
write_protobuf_message(&mut self.sender, &proto_msg)?;
```

- ClientToServerMsg / ServerToClientMsg は serde-friendly Rust enum (`ipc.rs:97`, `ipc.rs:178`) で **public API**。proto 型は via `.into()` で生成する一段挟みパターン。
- transport は `interprocess::local_socket::LocalSocketStream` (`Cargo.toml` workspace deps)、windows でも動く抽象 layer。
- 圧縮なし、versioning は CODEC_VERSION 相当のものはなさそう (= 同じ binary の client/server を前提)。

### 6.3 hyoui の選択

現状 hyoui は CBOR 採用 (`ciborium`)。これは:
- ✅ schema-less で前方互換に強い (= map に新 field 足し放題)
- ✅ Rust ecosystem で軽量、no_std OK
- ❌ varbincode より size 大 (= 数 % - 10% 程度)
- ❌ protobuf より型安全性が弱い

**結論: 現方針 (CBOR) を維持して問題なし**。wezterm の serial / ident 番号方式は採り入れて、新 PDU は番号管理で追加していくのが拡張性が高い。zstd 圧縮は **大きな `ScreenStateInit.redraw_bytes` を送るときだけ** 入れる価値あり (= 32 bytes 以下なら逆効果)。

---

## 7. resize + reflow 戦略

### 7.1 alacritty (簡素、primary/alt 同時 resize)

`term/mod.rs:655-705`:

```rust
let is_alt = self.mode.contains(TermMode::ALT_SCREEN);
self.grid.resize(!is_alt, num_lines, num_cols);
self.inactive_grid.resize(is_alt, num_lines, num_cols);
```

- 第 1 引数 `reflow: bool` = active grid (= scrollback あり側) のみ reflow、inactive (= alt) は単純 truncate/extend。
- `grid/resize.rs` で行を縦方向に拡縮、wrapline flag のある行は join + re-split。

### 7.2 wezterm (rewrap、cursor 補正、conpty フォールバック)

`screen.rs:193-326`:

- `if self.allow_scrollback`(= primary) → `rewrap_lines` で wrapline を unwrap して `physical_cols` で re-wrap (`screen.rs:100-190`)。
- alt screen は単純 prune or invalidate。
- cursor 位置は `rewrap` が返した `adjusted_cursor` に追従。
- `is_conpty` (Windows conpty 経由) なら scrollback-preserving モード。**Unix では mutable scrollback (= 縦に広げると history が増える)**、conpty では immutable (= 黒 padding)。

### 7.3 zellij (lines_above / viewport / lines_below 三層)

`panes/grid.rs:594` の `Grid` は `lines_above: VecDeque<Row>` / `viewport: VecDeque<Row>` / `lines_below: VecDeque<Row>` の三層。resize は viewport を伸ばすと lines_above から行が降りてくる。reflow は wezterm より粗い (= search で結合 / 分割は明示的に rewrap を呼ばないと不完全)。

### 7.4 hyoui への推奨

- **primary screen のみ reflow、alt screen は単純 crop/extend**。これは 3 実装共通。
- reflow アルゴリズムは alacritty の wrapline-only 方式が軽量で hyoui に合う (= `Cell::WRAPLINE` 相当の flag を per-row で持つ)。
- multi-client 異サイズ問題:
  - **leader 方式** (zellij 流): focused client のサイズで child PTY を WINCH、他 client は crop/padding。
  - **observe mode** (handoff 文書の §10A): resize 通知を子に飛ばさない attach mode を flag で別建て。
  - **hyoui MVP は leader 方式のみ実装**、observe mode は別 issue として記録 (= handoff 通り)。

---

## 8. failure path / panic safety

### 8.1 alacritty

- `panic = "unwind"` (default), GUI が thread を再 spawn して継続。PTY reader thread が panic しても terminal は dead-pane 化。
- `terminal.lock_unfair()` の使用は **Mutex の fairness を捨てて throughput 優先** (`event_loop.rs:140-145`)。

### 8.2 wezterm

- domain (= pane source) ごとに `is_dead()` を持ち (`mux/src/pane.rs:252`)、reconnect tolerance のため dead state を pane に保持。
- client 側 `is_tardy()` で遅延検知 → UI に "laggy connection" 表示 (`renderable.rs:127`)。
- async (`smol` + `async-channel`) で各 pane を独立 task として駆動、1 個落ちても他に影響しない設計。

### 8.3 zellij

- `panic = "abort"` ではなく recovery 志向 (= プラグイン runtime 含めて長寿セッションが前提)。
- `Result<T>` を多層で propagate、エラーは `ServerInstruction::Error` で session 全体停止。

### 8.4 hyoui への推奨

- 現方針 `panic = "abort"` (Cargo.toml `[profile.release]`) は維持。ただし screen emulator は **panic-safe な API** を意識:
  - cell index out-of-bounds は `Option` 返しで非 panic 化
  - 大量データ (`ScreenStateInit`) の serialize は `Result<Vec<u8>, Error>` で fallible
- **dead-pane 概念**: 子プロセスが exit した後の grid を保持しておき、最後の状態を見せる。これは zellij の `subscribed_pane_closed` 通知と同等。

---

## 9. hyoui に応用すべきパターン (= 10-20 件)

### 必ず採るべき

1. **alacritty 方式の `Cell` 構造** (24 bytes + `Arc<CellExtra>` COW)。compile-time size assert 付き。
2. **`mem::swap(&mut primary, &mut alt)` で alt screen 切替**。両方常に allocate しておく。alt は `max_scroll_limit=0`。
3. **per-line `SequenceNo` (u64 monotonic)** を最初から仕込む。Phase B の incremental sync の前提。
4. **scrollback は `VecDeque<Row>` + `stable_row_offset`** (wezterm 流)。`StableRowIndex` 型で client が安全に行を指せる。
5. **cursor 行は必ず attach redraw / dirty notify で送る** (wezterm `sessionhandler.rs:119`)。
6. **mode flags を `bitflags!` で `u32`** (alacritty `TermMode` `term/mod.rs:55`)。alt_screen / line_wrap / app_cursor / app_keypad / bracketed_paste / mouse_* / kitty_keyboard 等を 1 mask に。
7. **`Pane` 相当の trait 抽象**を最初から入れる (= local pty / observe / record-replay の interface 統一)。
8. **DEC sync update (`?2026h`)** を parser/emulator で hook、同期中は redraw event 抑制 (alacritty `event_loop.rs:166`)。
9. **primary のみ reflow、alt は単純 crop**。reflow は wrapline flag ベースの簡素実装で開始。
10. **attach 初回の redraw bytes 生成は state からの逆引き**: clear screen (`\x1b[2J`) → mode 復元 (`\x1b[?7h` 等) → cell ごとに style 差分 SGR → cursor 位置設定 (`\x1b[<y>;<x>H`) → cursor visibility (`\x1b[?25h/l`)。

### 採るとよい

11. **wezterm 流 pull 型 protocol を Phase B として用意**: `DirtyLinesNotify { since_seqno }` + `GetLines { rows }`。reconnect 後の resync が劇的に効率化。
12. **PDU serial 番号** (wezterm `codec/src/lib.rs:67`) を CBOR control message に入れる。RTT 計測 + out-of-order 受付。
13. **zstd 圧縮 (threshold 32 bytes)** を redraw bytes 送信時のみ適用。CBOR には乗せず raw bytes pdu を別建て。
14. **dead-pane 保持**: 子 exit 後も最終 grid を一定時間保持、attach 時に「死んだ pane の最後の画面」を見せる。
15. **`is_tardy()` 相当**: client 側で response が一定時間来なければ UI に表示 (= hyoui の場合 attach indicator stderr に出すなど)。
16. **logical line のテスト**: wezterm の `for_each_logical_line_in_stable_range_mut` (`screen.rs:991`) パターンで wrap 跨ぎの行検索を可能にする。これは wait pattern matching の正確性に効く (= claude のような長文行が wrap した状態でも 1 行として扱える)。

### 採ってもよい

17. **`RcCharacterStyles` (zellij 流) で連続同 style cell の共有** — multi-thread で `Rc` 不可なら `Arc` で同等。圧縮効果あり。
18. **per-pane palette change notification** (wezterm `Alert::PaletteChanged`) で config 変更時の client 同期。
19. **bonus_lines / dirty_lines 二段送り** (wezterm) — viewport 内は即送り、外は client が後で取りに来る分離。

---

## 10. 採るべきでない pattern

1. **wezterm の domain 抽象 (SSH / TLS multiplexer)** 全部実装は overkill。`Pane` trait の枠だけ採り、実装は local PTY のみで開始。
2. **zellij の plugin runtime (WASM)** は hyoui のスコープ外。
3. **wezterm の image (sixel / kitty graphics / iterm) 対応**: MVP 後回しで OK。`vte` parser の image hook 無効化で簡単に skip 可。
4. **zellij の layout / tab / pane multiplexing**: hyoui は 1 session 1 PTY 想定 (= attach は複数 client が同一 PTY を見るだけ)、pane 分割は不要。
5. **wezterm の bidi (`bidi` crate)**: アラビア語等の双方向テキスト処理。MVP 不要。
6. **wezterm の `kitty keyboard protocol` 完全対応**: MVP では disambiguate_esc_codes flag だけ拾えば十分。
7. **alacritty の `vi mode`**: hyoui の外側操作主軸思想に合わない。client 側機能でいい。
8. **`interprocess` crate (zellij)** の windows 抽象: hyoui は Unix 専用なので `nix` で直接書く現方針を維持。
9. **大量 dependencies**: wezterm の Cargo.toml は数百 dep。hyoui の lean 方針 (`nix + serde + ciborium + regex + thiserror`) は強い武器。emulator core 追加で `vte` (parser) + `unicode-width` + `unicode-segmentation` 程度に抑える。
10. **`Mux::get()` global singleton (wezterm)**: 単一 daemon 単一 session が原則なら singleton でもいいが、test 容易性のためには Arc<SessionData> を引数渡しが better。

---

## 11. 参考 path リスト (行数は 2026-05-27 時点の main)

### alacritty
- `alacritty_terminal/src/term/cell.rs:134` — `Cell` 構造体
- `alacritty_terminal/src/term/cell.rs:303` — size compile-time assert
- `alacritty_terminal/src/term/mod.rs:268` — `Term<T>` 構造体
- `alacritty_terminal/src/term/mod.rs:55` — `TermMode` bitflags
- `alacritty_terminal/src/term/mod.rs:713-735` — `swap_alt`
- `alacritty_terminal/src/term/mod.rs:655-705` — `resize`
- `alacritty_terminal/src/grid/mod.rs:110` — `Grid<T>`
- `alacritty_terminal/src/grid/storage.rs:33` — `Storage<T>` ring buffer
- `alacritty_terminal/src/event_loop.rs:104-171` — `pty_read` + vte parser drive
- `alacritty_terminal/src/event_loop.rs:166` — DEC sync hook

### wezterm
- `term/src/terminalstate/mod.rs:148` — `alt_screen: Screen` / `alt_screen_is_active: bool`
- `term/src/screen.rs:15` — `Screen` 構造体 (`VecDeque<Line>` + `stable_row_index_offset`)
- `term/src/screen.rs:100-190` — `rewrap_lines` (reflow primary only)
- `term/src/screen.rs:193-326` — `resize` + conpty 考慮
- `term/src/screen.rs:909-928` — `get_changed_stable_rows`
- `mux/src/pane.rs:167` — `pub trait Pane`
- `codec/src/lib.rs:60-108` — leb128 framing + serial
- `codec/src/lib.rs:289-318` — zstd compress threshold 32
- `codec/src/lib.rs:914-928` — `GetPaneRenderChangesResponse`
- `wezterm-client/src/pane/renderable.rs:56-81` — `RenderableInner` (LRU<StableRowIndex, LineEntry>)
- `wezterm-client/src/pane/renderable.rs:305-423` — `apply_changes_to_surface`
- `wezterm-mux-server-impl/src/sessionhandler.rs:52-143` — `compute_changes` (server side diff)

### zellij
- `zellij-server/src/lib.rs:918-1082` — `FirstClientConnected` handler
- `zellij-server/src/lib.rs:1084-1152` — `AttachClient` handler
- `zellij-server/src/panes/grid.rs:594` — `Grid` 構造体
- `zellij-server/src/panes/grid.rs:992` — `render_full_viewport`
- `zellij-server/src/panes/grid.rs:1556` — `Grid::render`
- `zellij-server/src/panes/terminal_character.rs:921` — `TerminalCharacter`
- `zellij-server/src/panes/terminal_character.rs:929` — size assert (16 bytes)
- `zellij-server/src/output/mod.rs:385` — `add_clients`
- `zellij-server/src/output/mod.rs:520-561` — `serialize` (= per-client ANSI 列生成)
- `zellij-server/src/output/mod.rs:1144-1206` — `OutputBuffer`
- `zellij-utils/src/ipc.rs:97-174` — `ClientToServerMsg`
- `zellij-utils/src/ipc.rs:178-226` — `ServerToClientMsg` (= `Render { content: String }`)
- `zellij-utils/src/ipc.rs:289-323` — `IpcSenderWithContext` + protobuf framing

---

## 12. 次のアクション (= DR-0013 に反映)

1. **§4.1-§4.3 の attach 復元 protocol を DR-0013 §5 に追記**。Phase A (push 型 redraw bytes) と Phase B (pull 型 seqno diff) の二段。
2. **§5 の cell data model を DR-0013 §4 に反映**。alacritty `Cell { c, fg, bg, flags: u16, extra: Option<Arc<CellExtra>> }` パターン + compile-time size assert。
3. **§6 の IPC を DR-0013 §11 (DR-0008 連動) に反映**。CBOR 維持 + serial 番号 + redraw bytes は raw frame で別 PDU。
4. **§7 の resize 戦略を DR-0013 §8 に反映**。primary のみ reflow、alt は crop。multi-client は leader 方式、observe mode は別 issue。
5. **§9 の必須項目 (1-10) は DR-0013 採用、推奨 (11-16) は別 task として ROADMAP の「優先」枠に**。
6. **`alacritty_terminal` crate の依存採用は要 PoC**: hyoui の `Cargo.toml` workspace で features を絞り込んで footprint を測る。`vte` parser のみ採用 + state 自前のほうが lean。

---

## 関連

- `docs/journal/2026-05-27-screen-emulator-pivot-handoff.md` — 本研究の起点 (= 方針大転換)
- `docs/decisions/DR-0013` — 本研究を踏まえて起票予定 (次セッション)
- `docs/decisions/DR-0008` — protocol 設計、本研究の §6 を反映
- 外部リポ:
  - https://github.com/alacritty/alacritty
  - https://github.com/wez/wezterm
  - https://github.com/zellij-org/zellij
