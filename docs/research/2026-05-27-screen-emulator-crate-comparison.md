# screen emulator crate 比較調査 (DR-0013 採用判断用)

- Date: 2026-05-27
- 目的: hyoui daemon に screen state 機能を組み込む際の Rust crate 選定。
- 対象: vte / alacritty_terminal / wezterm-term / termwiz **+ 調査中に浮上した強候補 vt100**。
- 判断基準: hyoui の lean 方針 (`nix + serde + ciborium + regex + thiserror` のみが direct dep)。

## 1. 要約 (結論先出し)

**推奨: `vt100` (= doy/vt100-rust v0.16.2、依存 3 つの軽量 crate)**。理由:

1. **目的が hyoui と完全一致**: README に「`screen` や `tmux` のような application を実装するための機能」と明記、daemon が parser + screen state を保持する用途に直接設計されている。
2. **attach 復元の primitive がそのまま用意されている**: `Screen::state_formatted()` が「現在の cell 内容 + 描画 attr + input mode」を escape sequence として吐く API として既に存在。これは hyoui 必須要件 (= attach handshake 後の redraw sequence 生成) の **コア機能**。
3. **依存が極小**: `itoa`, `unicode-width`, `vte` の 3 つだけ。hyoui lean 方針との衝突がない (alacritty_terminal の 16 dep / termwiz の 30+ dep と比較)。
4. **alternate screen / scrollback / cursor / wide char 全部対応**。`Cell::contents() -> &str` で grapheme cluster (combining char) も保持。
5. **active 維持中** (2025-07-12 commit、MSRV CI 追加 + 0.16.2 release)。世間で "abandoned" と書かれる古い記事があるが、現状は doy 本人が現役メンテ + 過去 30 release の実績。

次点: **alacritty_terminal** (= production 実績最強だが Term への入力経路が `vte::ansi::Handler` 経由で迂遠、依存 16 個と重め)。

不採用: **vte 単体** (= parser のみ、screen state は自前で書く必要があり最大の課題を未解決)、**wezterm-term** (= crates.io 未 publish の git dep 専用、依存ツリーが 20+ で巨大)、**termwiz** (= alternate screen / scrollback の概念がそもそも `Surface` モデルに無く、用途違い)。

## 2. 4 (+1) crate の概要

### 2.1 vte (alacritty 系)

- 起源: Alacritty workspace の parser-only crate。
- 目的: Paul Williams' ANSI parser state machine の実装。**バイトを「action」に変換するまで** が責務。
- 主要 type: `Parser` / `Perform` trait / `Params`。
- バージョン: 0.15.0 (2025-02-02)、active。
- 採用実績: zellij (= バージョン 0.11 で固定)、alacritty_terminal の内部、wezterm 系の wezterm-escape-parser はこれを利用しない (vtparse 独自) など。
- ライセンス: Apache-2.0 OR MIT。
- 公式 doc: https://docs.rs/vte/0.15.0/vte/

### 2.2 alacritty_terminal (alacritty 系)

- 起源: Alacritty 本体から terminal emulator core を分離した crate。
- 目的: full な「Term + Grid + Cell」モデル + event loop + tty (= PTY) 統合。
- 主要 type: `term::Term<T>` / `grid::Grid<Cell>` / `term::cell::Cell` / `term::TermMode` / `event_loop::EventLoop`。
- バージョン: 0.26.0 (2026-04-06)、極めて active (= Alacritty 本体 v0.17.0 と同期した release)。
- 採用実績: Alacritty 本体、ezno-terminal や他の Rust 製 emulator。"helper library for building terminal emulators, broken out from alacritty and inspired by libvte" と公式に位置付け。
- ライセンス: Apache-2.0。
- 公式 doc: https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/

### 2.3 wezterm-term (wezterm 系)

- 起源: wezterm workspace の terminal emulator core (workspace member、`term/` directory)。
- 目的: full な「Terminal + Screen + Line + Cell」モデル + input encoding。`Terminal::advance_bytes(&[u8])` で bytes を直接 feed できる API 設計。
- 主要 type: `Terminal` / `terminalstate::TerminalState` / `screen::Screen` / `screen::ScreenOrAlt` / `CursorPosition` / `TerminalSize`。`PhysRowIndex` / `StableRowIndex` 等の typed index で誤代入を予防。
- バージョン: workspace 内では `0.1.0` のまま据え置き、**crates.io には publish されていない** (= `crates.io/api/v1/crates/wezterm-term` が 404、wezterm-cell / wezterm-surface / wezterm-escape-parser も同様)。
- 採用実績: wezterm 本体 + tattoy-wezterm-term (= 同名 fork が別途 publish されている、コアは wezterm 由来)。
- ライセンス: MIT。
- repo: https://github.com/wezterm/wezterm/tree/main/term

### 2.4 termwiz (wezterm 系)

- 起源: wezterm workspace 内の terminfo + surface 操作系ライブラリ。
- 目的: 「自分が描画 driver になる」モデル (= `Surface` への `Change` を積んで delta 描画)。escape parser (`escape` module、vtparse 経由) + cell 表現 + line editor + caps 検出も内蔵。
- 主要 type: `surface::Surface` / `cell::Cell` / `escape::parser::Parser` / `terminal::Terminal` (= 自分が制御する terminal device の trait)。
- バージョン: 0.23.3 (2025-03-20)、wezterm 本体に追随する形で更新。
- 採用実績: wezterm 本体、ratatui (= 一部の backend)、200 crate が依存。
- ライセンス: MIT。
- 公式 doc: https://docs.rs/termwiz/0.23.3/termwiz/

### 2.5 vt100 (= 調査中に浮上した最有力候補、doy/vt100-rust)

- 起源: 個人 (doy) の単独 crate、graphical terminal emulator から「parser + screen state」だけ抜き出したもの。
- 目的: README 引用「Although you can use this crate to build a graphical terminal emulator, it also contains functionality necessary for implementing terminal applications that want to run other terminal applications - programs like `screen` or `tmux` for example.」
- 主要 type: `Parser` (= `Parser::new(rows, cols, scrollback_len)`) / `Screen` / `Cell` / `Color`。`Parser::process(&[u8])` で bytes を直接 feed。
- バージョン: 0.16.2 (2025-07-12)、active (= MSRV CI 整備、Cargo.lock 維持に切替)。
- 採用実績: 113 stars / 113 直接 dependents / 約 80 万 downloads/month。tmux/screen 系の用途に明示的に設計。
- ライセンス: MIT。
- repo: https://github.com/doy/vt100-rust 、doc: https://docs.rs/vt100/0.16.2/vt100/

## 3. 比較表

| 観点 | vte | alacritty_terminal | wezterm-term | termwiz | **vt100** |
|---|---|---|---|---|---|
| Parser+State 両方 | ✗ (parser のみ) | ✓ | ✓ | △ (Surface だが alt screen 無し) | **✓** |
| `advance_bytes(&[u8])` 直接 API | ✗ (Perform 実装必要) | △ (vte::Handler trait 経由) | ✓ (`advance_bytes`) | ✗ (Change 流し込み) | **✓ (`process`)** |
| alternate screen | n/a | ✓ (`TermMode::ALT_SCREEN`) | ✓ (`ScreenOrAlt`, `is_alt_screen_active()`) | ✗ | **✓ (`alternate_screen()`)** |
| scrollback API | n/a | ✓ (`Grid` 内の history) | ✓ (`StableRowIndex` 経由) | ✗ | **✓ (`Parser::new(..,..,scrollback_len)`, `Screen::set_scrollback()`)** |
| cell grid 読出し | n/a | ✓ (`grid[Point]`, `display_iter`) | ✓ (`screen().lines`) | ✓ (`screen_cells()`) | **✓ (`Screen::cell(r, c) -> Option<&Cell>`, `rows()`)** |
| cursor 位置/形 | n/a | ✓ (`grid.cursor`) | ✓ (`cursor_pos()`) | ✓ (`cursor_position()`) | **✓ (`cursor_position()`)** |
| mode 群 (app_keypad, app_cursor, bracketed_paste, mouse_*) | n/a | ✓ (`TermMode` bitflags) | ✓ | △ | **✓ (個別 method: `application_keypad()`, `bracketed_paste()`, `mouse_protocol_mode()`)** |
| resize + reflow | n/a | ✓ (`resize::<S: Dimensions>`) | ✓ (`resize(TerminalSize)`) | ✓ (`resize`) | **△ (`set_size` あり、reflow の品質は要検証)** |
| attach 用 redraw sequence 生成 | n/a | ✗ (= 自前で grid → ANSI 再エンコード必要) | ✗ (= 同様) | △ (`get_changes` で diff) | **✓ (`state_formatted()` で一発、`contents_diff()` で delta も)** |
| serde 連携 | optional (`features=["serde"]`) | optional (default-enabled、`bitflags/serde` + `vte/serde` 連動) | optional (`use_serde` で wezterm-* 連動) | optional (`features=["serde"]`) | **dev-only (= 本体 Cell/Screen は impl 無し、自前 wrapper 必要)** |
| wide char | n/a | ✓ (`unicode-width` 経由) | ✓ (`finl_unicode` + `unicode-normalization`) | ✓ (`finl_unicode`) | **✓ (`is_wide()`, `is_wide_continuation()`, `unicode-width`)** |
| grapheme cluster (combining char) | n/a | △ (char 単位) | ✓ | ✓ | **✓ (`Cell::contents() -> &str` で combining char 保持)** |
| direct dependencies 数 | **2-6** (= arrayvec, memchr + optional 4) | 11 (+ Windows 3 / Unix 3) | 23+ (= wezterm-cell, -surface, -escape-parser, -bidi, -dynamic, termwiz 等を取り込む) | 30+ (= pest, fancy-regex, wezterm-bidi など) | **3** (= itoa, unicode-width, vte) |
| crates.io publish 状態 | ✓ | ✓ | **✗ (git dep 専用)** | ✓ | ✓ |
| 最新 release | 0.15.0 (2025-02-02) | 0.26.0 (2026-04-06) | git only (= 0.1.0 のまま据置) | 0.23.3 (2025-03-20) | 0.16.2 (2025-07-12) |
| repo 最新 commit | 2026-02-28 (alacritty/vte) | 継続 (Alacritty 本体と同期) | 2026-03-31 (wezterm) | 2026-03-31 (wezterm) | 2025-07-12 |
| ライセンス | Apache-2.0 OR MIT | Apache-2.0 | MIT | MIT | MIT |
| no_std | △ (= default feature off 可、`std` feature あり) | ✗ | ✗ | ✗ | ✗ |
| production 採用 | Alacritty, zellij, alacritty_terminal | Alacritty 本体 | wezterm 本体 | wezterm, ratatui (一部) | 113 直接 dep (= 名指し用途多数) |

## 4. API surface 詳細

### 4.1 vte

```rust
use vte::{Parser, Perform};

struct MyEmu;
impl Perform for MyEmu {
    fn print(&mut self, c: char) { /* ... */ }
    fn execute(&mut self, byte: u8) { /* ... */ }
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) { /* ... */ }
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) { /* ... */ }
    // + esc_dispatch, hook, put, unhook, ...
}

let mut parser = Parser::new();
let mut emu = MyEmu;
parser.advance(&mut emu, b"\x1b[31mhello");
```

→ **screen state は全部自前**。alternate screen / scrollback / cursor 位置 / wide char / grapheme cluster の管理を自分で書くのは 2-3k LoC 規模で、これが本調査の本質的なコスト。

### 4.2 alacritty_terminal

```rust
use alacritty_terminal::{Term, term::Config, vte::ansi::Processor, event::EventListener};
use alacritty_terminal::term::test::TermSize;  // または独自の Dimensions impl

struct NoopListener;
impl EventListener for NoopListener {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {}
}

let config = Config::default();
let size = TermSize::new(80, 24);  // cols, rows
let mut term = Term::new(config, &size, NoopListener);
let mut processor = Processor::new();
processor.advance(&mut term, b"\x1b[31mhello".as_slice());

let grid: &Grid<Cell> = term.grid();
let mode: &TermMode = term.mode();
let alt = mode.contains(TermMode::ALT_SCREEN);
let cursor_point = grid.cursor.point;
```

主要 method:
- `Term::new(config, &dims, event_proxy)` / `Term::resize(size)`
- `Term::grid()` / `Term::grid_mut()` / `Term::mode()`
- `Term::scroll_display(Scroll::Lines(n))` / `Term::reset_state()`
- `Term::damage()` / `Term::reset_damage()` (= 部分 redraw 用)
- `Term::renderable_content() -> RenderableContent` (= 描画用 visible cell iter)

input は `vte::ansi::Handler` trait 経由 (= `Term` がこれを implement、`vte::ansi::Processor` から自動呼び出し)。**直接 `&[u8]` を `Term` に流す 1 行 API は無い**。

### 4.3 wezterm-term

```rust
use wezterm_term::{Terminal, TerminalSize, TerminalConfiguration};
use std::sync::Arc;

struct MyCfg;
impl TerminalConfiguration for MyCfg { /* 多数の method を実装 */ }

let size = TerminalSize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0, dpi: 96 };
let cfg = Arc::new(MyCfg);
let writer: Box<dyn std::io::Write + Send> = Box::new(std::io::sink());
let mut term = Terminal::new(size, cfg, "hyoui", "0.1", writer);

term.advance_bytes(b"\x1b[31mhello");
let screen = term.screen();
let cursor = term.cursor_pos();
let is_alt = term.is_alt_screen_active();
term.resize(TerminalSize { rows: 30, cols: 100, ..size });
term.send_paste("multi\nline").ok();
```

主要 method:
- `Terminal::new(size, config, program, version, writer)` (= signature 重い、`Arc<dyn TerminalConfiguration + Send + Sync>` と `Box<dyn Write + Send>` を要求)
- `Terminal::advance_bytes(impl AsRef<[u8]>)` — シグネチャ良好
- `terminalstate::TerminalState` 経由で `screen() -> &Screen` / `screen_mut() -> &mut Screen` / `cursor_pos() -> CursorPosition` / `resize(TerminalSize)` / `send_paste(&str)` / `focus_changed(bool)` / `is_mouse_grabbed()`
- `Screen` は `VecDeque<Line>` で primary + scrollback を一体管理、`ScreenOrAlt` で primary/alt の切替を抽象化

### 4.4 termwiz

```rust
use termwiz::surface::{Surface, Change};
use termwiz::cell::CellAttributes;

let mut surface = Surface::new(80, 24);
surface.add_change("Hello");
surface.add_change(Change::Attribute(termwiz::cell::AttributeChange::Foreground(...)));
let cells = surface.screen_cells();
let (cx, cy) = surface.cursor_position();
let diff: Vec<Change> = surface.get_changes(seq);
```

**Surface は「自分が描画 driver になる」モデル**。alternate screen / scrollback の概念は無く、`add_change` で組み立てて `get_changes` で diff を取り出す。escape parser は別 module (`termwiz::escape`) にあり、両者の合成は自分で書く必要 (= wezterm 本体ですらこの合成は wezterm-term 側で行っている)。hyoui の「PTY bytes を流し込んで screen state を作る」用途には**そのままでは使えない**。

### 4.5 vt100

```rust
use vt100::Parser;

let mut parser = Parser::new(24, 80, 1000);  // rows, cols, scrollback_len
parser.process(b"\x1b[31mhello\x1b[?1049h\x1b[2J");  // 直接 feed

let screen = parser.screen();
let (rows, cols) = screen.size();
let (cy, cx) = screen.cursor_position();
let is_alt: bool = screen.alternate_screen();
let bracketed: bool = screen.bracketed_paste();
let mouse_mode = screen.mouse_protocol_mode();

// セル単位アクセス
if let Some(cell) = screen.cell(0, 0) {
    let s: &str = cell.contents();          // grapheme cluster 対応の &str
    let wide: bool = cell.is_wide();
    let fg = cell.fgcolor();
    let bold = cell.bold();
}

// 行単位の plain text
for row_str in screen.rows(0, 80) { /* String */ }

// hyoui の attach 復元シーケンス生成 (= これがコア)
let redraw_bytes: Vec<u8> = screen.state_formatted();
// → client terminal に書けば、現在の visible cell + input mode が完全再現

// delta 描画
let diff: Vec<u8> = screen.contents_diff(&prev_screen);
```

主要 method (= Screen):
- `Parser::process(&[u8])` / `Parser::screen() -> &Screen` / `Parser::screen_mut() -> &mut Screen`
- `Screen::set_size(rows, cols)` / `Screen::size()` / `Screen::set_scrollback(n)` / `Screen::scrollback()`
- `Screen::cursor_position()` / `Screen::cell(r, c) -> Option<&Cell>` / `Screen::rows(start, width)`
- `Screen::contents() -> String` / `Screen::contents_formatted() -> Vec<u8>` / `Screen::state_formatted() -> Vec<u8>` / `Screen::contents_diff(&prev) -> Vec<u8>` / `Screen::input_mode_formatted() -> Vec<u8>` / `Screen::attributes_formatted() -> Vec<u8>`
- `Screen::alternate_screen() -> bool` / `Screen::application_keypad() -> bool` / `Screen::application_cursor() -> bool` / `Screen::bracketed_paste() -> bool` / `Screen::mouse_protocol_mode()` / `Screen::mouse_protocol_encoding()`

主要 method (= Cell):
- `contents() -> &str` (= grapheme cluster 含む) / `has_contents()` / `is_wide()` / `is_wide_continuation()`
- `fgcolor()` / `bgcolor()` / `bold()` / `dim()` / `italic()` / `underline()` / `inverse()`

`Parser::new_with_callbacks` で OSC 等の event を独自に hook 可能 (= title 変更や clipboard 通知をキャッチ)。

## 5. 依存ツリー (= 重量比較)

### 5.1 vte (0.15.0)

- arrayvec
- memchr
- (optional) bitflags / cursor-icon / log / serde

**direct 2 + optional 4**。最軽量、hyoui に追加するなら no_default で `features = ["serde"]` だけが妥当。

### 5.2 alacritty_terminal (0.26.0)

| dep | 役割 | 備考 |
|---|---|---|
| base64 | OSC 52 clipboard | |
| bitflags | TermMode, Flags | |
| home | config path | hyoui には不要 |
| libc | tty | |
| log | logging | |
| parking_lot | sync | |
| polling | event_loop | hyoui は自前 epoll/kqueue |
| regex-automata | search | |
| unicode-width | wide char | |
| vte | parser | |
| serde (optional, default-enabled) | | |
| rustix / rustix-openpty / signal-hook | Unix | hyoui は nix なので二重 |
| miow / piper / windows-sys | Windows | hyoui は Unix only |

**direct 11 + Unix 3 / Windows 3**。`home` / `polling` / `rustix-openpty` / `signal-hook` は hyoui で既に nix を使っているので二重投資。tty / event_loop module を切り離して `term` + `grid` だけ使う path は技術的に可能だが、hyoui の lean 方針には不一致が大きい。

### 5.3 wezterm-term (workspace 内 0.1.0)

direct deps (= 抜粋): anyhow, bitflags, csscolorparser, downcast-rs, finl_unicode, hex, humansize, image, lazy_static, log, lru, miniz_oxide, num-traits, ordered-float, serde, terminfo, unicode-normalization, url, **wezterm-bidi, wezterm-cell, wezterm-dynamic, wezterm-escape-parser, wezterm-surface, termwiz**。

**direct 23+、しかも wezterm-* の git dep 連鎖**。`image` / `miniz_oxide` (= sixel/iTerm2 image) や `csscolorparser` (= CSS color string) など hyoui には不要なものが多い。**何より crates.io に publish されていない** ため、`Cargo.toml` で `wezterm-term = { git = "https://github.com/wezterm/wezterm", rev = "..." }` 形式で固定する必要があり、hyoui の release 安定性に直結する。

### 5.4 termwiz (0.23.3)

direct deps: anyhow, base64, bitflags, fancy-regex, filedescriptor, finl_unicode, fixedbitset, hex, lazy_static, libc, log, memmem, num-derive, num-traits, ordered-float, pest, pest_derive, phf, sha2, signal-hook, siphasher, terminfo, thiserror, ucd-trie, unicode-segmentation, vtparse, wezterm-bidi, wezterm-blob-leases, wezterm-color-types, wezterm-dynamic, wezterm-input-types, + (Unix) nix 0.29 / termios。

**direct 30+**。`pest` / `pest_derive` (= parser generator) や `fancy-regex` など重量級が混入。

### 5.5 vt100 (0.16.2)

- itoa (= integer formatting)
- unicode-width
- vte 0.15

**direct 3 のみ**。dev-deps に nix / quickcheck / rand / serde / serde_json / terminal_size があるが、本番ビルドには出ない。

## 6. 推奨と理由 (= hyoui 適合度ランキング)

### 1 位 (推奨): **vt100**

- hyoui の必須要件 (= attach 復元 redraw sequence、alternate screen 切替検出、cell 直接読出し、scrollback、wide char) を**全て直接 API で提供**。
- `Screen::state_formatted()` は DR-0013 の核 (= attach handshake 後の redraw sequence 生成) と完全一致する設計思想で、自前で「grid を走査して ANSI を再構築」する 200-500 行のコードが不要になる。
- 依存 3 で hyoui の lean 方針と完全整合。`thiserror` / `regex` / `nix` / `serde` / `ciborium` を直接 dep に持つ hyoui に vt100 を追加しても direct dep が 6 個 (= vte / itoa / unicode-width が追加)、ツリー全体でも 10 以下に収まる見込み。
- 公式 README が `screen` / `tmux` 実装用途を**名指し**、hyoui の daemon-as-screen-state 思想とドメインが完全一致。
- `serde` 連携は本体に impl が無いが、hyoui は **state を CBOR で送るときに `Screen` 全体をそのまま serialize する必要が無い** (= API としては `cell(r, c)` で行/列を CBOR struct に詰める方が安定。`state_formatted()` で得た bytes を transport する方が後方互換性も高い)。よって serde impl 不足は実質的な障害にならない。

### 2 位: **alacritty_terminal**

- production 実績は最強 (= Alacritty 本体で daily 数万 download)、grid モデルが堅牢、Damage tracking が attach 復元用途にも転用可能。
- 不採用理由: (a) 入力経路が `vte::ansi::Processor` 経由で 1 行で書けない (= ergonomics 低下、hyoui session 内の hot loop に小さなオーバーヘッド)、(b) 依存 11 個の中に `polling` / `rustix-openpty` / `signal-hook` / `home` など hyoui で nix 経由で実装済 / 不要のものが混入、(c) **attach 復元用の「state → ANSI sequence」生成 API が無い**ため結局自前実装が必要 (= vt100 採用の最大の利益が消える)。

### 3 位: **wezterm-term**

- API ergonomics は最高 (= `Terminal::advance_bytes(impl AsRef<[u8]>)` が直接)、grapheme cluster / bidi / image 等の完成度が高い。
- 不採用理由: (a) **crates.io 未 publish** で git dep 専用、hyoui release で git rev 固定するのは pkfire/Taskfile.pkl の version bump gate 思想と摩擦、(b) 依存 23+ で `image` / `miniz_oxide` / `csscolorparser` など hyoui に不要なものが大量、(c) `TerminalConfiguration` trait の実装が重い (= 数十 method の default impl を書く必要)、(d) attach 復元用の「state → ANSI 再生成」も自前。

### 4 位: **termwiz**

- 不採用理由: そもそも**ドメインが違う** (= 「自分が描画 driver」型、`Surface` モデルに alternate screen も scrollback も無い)。hyoui のユースケース (= PTY bytes を流し込んで screen state を作る) には合わない。

### 不採用: **vte 単体**

- 不採用理由: parser のみで screen state は自前。hyoui の **本質的なコスト** (= alternate screen / scrollback / wide char / grapheme cluster / mode 管理 / state→ANSI 再生成) を全部自分で書く前提になる。これを書くなら vt100 をそのまま使う方が桁違いに低コスト + 高品質。`Perform` trait の各 method (= `csi_dispatch` の派遣等) を hyoui 内に書き下す行為自体が DR-0013 の scope 外まで膨らむリスクが高い。

## 7. 採用後の懸念 / 検証すべき点 (= vt100 採用前提)

1. **reflow の品質**: `Screen::set_size(rows, cols)` で reflow が起きるが、wezterm-term / alacritty_terminal の reflow ロジックと同等の品質かは要 PoC。特に**行折返しを保持したまま resize したときの cursor 位置のずれ**は実機で claude TUI を流し込んで確認すべき。
2. **`state_formatted()` の cursor visibility / shape**: cursor visible / shape / blink まで含めて再現してくれるか doc では曖昧。`Screen::hide_cursor()` 系の getter があるか source 確認、不足ならパッチ提案 or 自前 補完層を入れる。
3. **OSC (= title / clipboard / hyperlink) の扱い**: `Parser::new_with_callbacks` で hook できるが、callback の呼び順や順序保証は要検証。hyoui の structured event (= OSC 8 hyperlink を client に転送) で必要になる。
4. **mouse protocol mode の covered 範囲**: `mouse_protocol_mode()` / `mouse_protocol_encoding()` は getter があるが、bracketed paste + extended mouse 等の組み合わせの実装網羅性は要検証 (= claude TUI の mouse 操作で fuzz してみる)。
5. **doy/vt100-rust の bus factor**: 個人メンテ。"abandoned" と書かれた古い記事 (= ChrisTitusTech fork が存在) があったが、現状の commit log (2025-07-12) では active。万一 abandoned された場合の fork 戦略 (= hyoui workspace に vendor する or fork 維持) を頭に入れておく。
6. **scrollback の memory budget**: hyoui は既に `scrollback.rs` を持つので二重管理にならないよう統合設計が必要。vt100 の `Parser::new(rows, cols, scrollback_len)` で scrollback_len を持つ場合、既存 ring buffer を捨てて vt100 内蔵に統合する方が単純。
7. **serde 不在の影響**: `Cell` / `Screen` は Serialize impl を持たない。hyoui の CBOR protocol で state を送るときは「`state_formatted()` を raw bytes 層で送る」設計に倒し、cell-level の structured snapshot は hyoui 側で wrapper struct (= 1 layer の hyoui::CellSnapshot で `cell.contents().to_string()` + 各 attr を持つ) を作って serialize する。vt100 本体に変更不要。
8. **OSC 8 hyperlink / sixel / iTerm2 image**: vt100 はこれらを cell に保持しない。hyoui の MVP では不要だが、将来 image 対応する場合は wezterm-term への乗り換えが必要になる可能性 (= ROADMAP 追加予定に記録)。

## 8. 不採用案の選定理由 (= DR-0013 引用用)

### vte 単体を不採用とする理由

- parser-only であり、screen state を自前で実装する必要がある。これは hyoui に「軽量 crate 1 つ追加すれば screen emulator 完備」という DR-0013 設計目標と整合せず、新規 module 2-3k LoC + bug 温床を hyoui 内に抱え込む形になる。
- `Perform` trait の dispatch を自前で実装するコストは無視できない。alternate screen 切替 (`?1049h`/`?1049l`) や mouse mode bit (`?1000h`〜`?1006h`) など state machine の bug が出やすい箇所を自前で持つことは、hyoui の lean 方針 (= 既存 dep をきちんと使う) と反する。

### alacritty_terminal を不採用とする理由

- attach 復元用の「state → ANSI sequence 再生成」 API が無く、vt100 採用の最大の便益 (= `state_formatted()` で 1 行) を hyoui 側で再実装する必要がある。
- 依存 11 個のうち `home` / `polling` / `rustix-openpty` / `signal-hook` は hyoui で既に nix + 自前 epoll 経由で実装済の領域と重複し、binary size と build time に二重コストが乗る。
- `Term::new` の signature が `D: Dimensions` + `EventListener` で trait 実装を強制し、ergonomics として vt100 の `Parser::new(rows, cols, scrollback_len)` に比べて hot loop の入口が複雑。
- production 実績は最強だが、**hyoui の用途 (= daemon が screen state を持って attach 復元する) に直接機能 fit するのは vt100 の方が上**。設計優先 (= `design-priority.md`) に従い「より正しい設計」を選ぶ。

### wezterm-term を不採用とする理由

- **crates.io に publish されていない**。hyoui の `Cargo.toml` で `wezterm-term = { git = "...", rev = "..." }` 形式 (= revision 固定) を採用すると、pkfire の `bump-semver` + Taskfile.pkl 配下の `versions` task 設計と摩擦が生じる (= バージョン整合性チェックの対象外)。これは公開 OSS 配布物で予期せぬ事故を呼び込む。
- 依存 23+ で `image` / `miniz_oxide` (= sixel/iTerm2 image)、`csscolorparser` (= CSS color string) など hyoui の MVP scope 外の機能向け dep が大量。binary size と compile time が桁違いに増える。
- `TerminalConfiguration` trait は数十の default-impl 付き method を持ち、最小実装でも数十行のボイラープレートが必要。これは vt100 の `Parser::new` の 1 行と比べて開発負荷が高い。
- API ergonomics と grapheme cluster / bidi / image の完成度は最高だが、hyoui MVP では grapheme + alt screen + scrollback + wide char が押さえられれば十分で、wezterm-term の overspec を支払う理由が薄い。

### termwiz を不採用とする理由

- ドメインモデルが「自分が描画 driver になる」型 (= `Surface` への `Change` 流し込み)。hyoui の「PTY bytes を流し込んで screen state を構築する」用途に**そのままでは使えない** (= escape parser は別 module の `termwiz::escape`、両者の合成は wezterm 本体ですら wezterm-term 側で実装している)。
- alternate screen / scrollback の概念が `Surface` モデルに無い。hyoui の必須要件と決定的に合わない。
- 依存 30+ で `pest` / `pest_derive` / `fancy-regex` などの重量級 dep が混入。

## 参考 URL

- vt100 crate: https://docs.rs/vt100/0.16.2/vt100/ , https://github.com/doy/vt100-rust , https://crates.io/crates/vt100
- vte crate: https://docs.rs/vte/0.15.0/vte/ , https://github.com/alacritty/vte
- alacritty_terminal: https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/ , https://github.com/alacritty/alacritty/tree/master/alacritty_terminal
- wezterm-term: https://github.com/wezterm/wezterm/tree/main/term , https://docs.rs/wezterm-term
- termwiz: https://docs.rs/termwiz/0.23.3/termwiz/
- zellij の vte 利用: https://github.com/zellij-org/zellij/blob/main/zellij-server/src/panes/terminal_pane.rs
- vt100 メンテ状況: https://github.com/doy/vt100-rust/commits/main (最新 2025-07-12)
- crates.io API による wezterm-term の未 publish 確認: `curl https://crates.io/api/v1/crates/wezterm-term` → 404
