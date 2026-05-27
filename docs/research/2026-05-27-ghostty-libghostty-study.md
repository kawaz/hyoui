# ghostty / libghostty-vt / cmux 実装研究 (DR-0013 補足調査)

- Date: 2026-05-27
- 目的: 先行調査 (= `2026-05-27-screen-emulator-crate-comparison.md` 推奨: vt100) に対し、kawaz が日常メインで使う **cmux** が内部で使う **libghostty (= ghostty 由来の terminal core library)** を独立に深掘り。vt100 推奨を覆す要素があるか、補強する要素があるか、ghostty の方針を vt100 ベース実装にどう取り入れるかの判断材料を提供する。
- 対象: ghostty (Zig 製 terminal emulator) / libghostty-vt (= ghostty core を library 化した派生) / cmux (= manaflow-ai/cmux、libghostty ベース macOS terminal)。
- 出典の重み付け: ghostty / cmux 共にローカル clone が存在 (`~/.local/share/repos/github.com/ghostty-org/ghostty`、cmux は GitHub API + WebFetch)。本文中の引用はローカル read を主、Web は subordinate。

## 1. 要約 (= 結論先出し)

### 1.1 ghostty / libghostty の性格

**ghostty は GUI terminal emulator (= Zig、Mitchell Hashimoto 発)、最近 `libghostty-vt` という形で terminal core (= parser + screen + state) を独立 library に分離する作業が進行中**。

- libghostty-vt は `include/ghostty/vt.h` を public header に持つ **C ABI library**。ただし**現状 publish された C API は OSC / SGR / Key encoding / Paste utilities までで、`Terminal` / `Screen` / `Stream` 本体は C export されていない** (= Zig 側 module `ghostty-vt` 経由なら全 API 利用可)。
- Zig module `ghostty-vt` (= `src/lib_vt.zig`) は **Terminal/Screen/PageList/Parser/Stream/ScreenSet/Style/Cursor 全部を export しており、Zig からは production-grade な terminal core にフルアクセスできる**。
- 公式 warning は明示的:「The API is not yet stable. Breaking changes are expected.」 (= `include/ghostty/vt.h`)、「The API itself (functions, types, etc.) may change without warning.」 (= `src/lib_vt.zig`)。**library 化は WIP**。
- ghostty 本体は GUI 用途に向いており、`Surface` + `App` + apprt (= application runtime、GLFW/GTK/AppKit 抽象) を統合した stack だが、`src/lib_vt.zig` の存在から **terminal state core だけを切り出して別 application で使う方向性を本家が意図して進めている**。これが先行調査時点で vt100 が「screen/tmux 用途のために設計された」と書いたのと**まさに同じ意図**で ghostty 側でも公式化が進んでいる。

### 1.2 cmux の実体特定結果 + ghostty 依存

**cmux = `manaflow-ai/cmux` (公式 site: cmux.com、kawaz の README リンク `nichochar/cmux` は redirect/old)**。Swift / AppKit 製 macOS native app で、ghostty を fork (= `manaflow-ai/ghostty`) して **submodule** として組み込み、`libghostty` を直接呼ぶ。terminal の core (= parser / screen / pty 統合) は **完全に libghostty に委譲**しており、cmux 自身は terminal emulator を実装していない。

- `Sources/GhosttyTerminalView.swift` で `ghostty_init`、`ghostty_config_new/load_*/finalize`、`ghostty_app_new/set_focus`、`ghostty_surface_*` (= `clear_selection_compat`、`select_cursor_cell_compat`、`complete_clipboard_request`) などを呼ぶ。
- cmux の "multiplexing" は **AppKit の native window/tab/split** で実装、各 split に 1 つの `ghostty surface` が紐づき、surface 1 つに 1 PTY が紐づく構成。
- **cmux に「attach/detach の screen state 復元」概念は存在しない**。multiplexer ではなく terminal emulator + window manager だから、ghostty surface object を保持し続けることで state が保たれる。プロセス再起動で session 復元は **`SessionRestoredTerminalCommandStore`** 経由でコマンド (= 起動時の引数) を保存・再実行する形 (= state replay ではない)。
- ghostty fork (= manaflow-ai/ghostty) で適用している patches:
  - **manual IO mode** (`GHOSTTY_SURFACE_IO_MANUAL`、`io_write_cb`、`ghostty_surface_process_output`、`ghostty_surface_text_input`、`ghostty_surface_render_now`) を追加 (= iOS 系向け、ghostty 内蔵 PTY を経由せず外側から bytes を流し込む経路)
  - OSC 99 (kitty 通知) parser 追加、APC graphics handler 統合、theme picker、DECRPM mode 2031、URL/path regex 修正、キーボード copy mode 用に上流が削除した `ghostty_surface_select_cursor_cell` / `ghostty_surface_clear_selection` を再 expose。
- **cmux が hyoui の direct competitor ではない**: cmux は terminal emulator + native macOS UI、hyoui は **headless multiplexer / daemon** (= terminal は ssh / iTerm / ghostty / cmux 等 client 側、daemon は screen state 正本)。レイヤーが違う。

### 1.3 hyoui への教訓 top 5

1. **libghostty-vt の Zig module は production-grade だが、Rust から直接使うのはコスト過大**。bindgen + custom shim + Zig submodule + allocator 仲介 + API instability の四重苦。**vt100 採用判断は揺るがない**。
2. **ghostty の `terminal/formatter.zig` (= `Format = .vt`) は vt100 の `state_formatted()` と同じ「state → VT escape sequence 再生成」 API を提供しており、両者が独立に同じ結論に到達している = hyoui の DR-0013 (attach 復元のために state→ANSI 再エンコードを daemon 側で行う) は terminal emulator 業界の standard pattern**。
3. **ghostty の `stream_readonly` module (= `src/terminal/stream_readonly.zig`) の design rationale は hyoui daemon の usecase と完全一致**: 「terminal emulator that only needs to render output and doesn't need to respond (since it maybe isn't running the actual program)... ideal for replay tooling, CI logs, PaaS builder output, etc.」 hyoui daemon もまさにこの形 (= 子 PTY からの bytes を流し込んで state を作るが、DSR 等の query 応答を出すのは別 layer)。vt100 採用後の hyoui 実装で**「query 応答は別 layer」というアーキテクチャ判断**を直接根拠付ける。
4. **page-based scrollback (= ghostty の `PageList` + `Page` の linked list of memory pages) は大規模 scrollback の memory efficiency に効く**。vt100 採用後、hyoui が scrollback を大きくする場合 (= 10000 行+) は、vt100 内蔵 ring buffer から page-based に切替を検討。ただし MVP では vt100 ring で十分。
5. **cmux 流の "manual IO" pattern (= `ghostty_surface_process_output` で外側から PTY bytes を流し込む)** は hyoui の daemon ↔ vt100 連携と概念的に同じ。**つまり cmux 側の Zig module fork パターンは、hyoui の Rust + vt100 アーキテクチャの妥当性を独立に検証している**。

### 1.4 採用判断: vt100 推奨は維持 (= 補強)

| 観点 | 判定 | 理由 |
|---|---|---|
| vt100 推奨を覆すか | **No** | libghostty-vt は WIP + Zig module + C API 未完成。Rust 採用は非現実的 |
| vt100 推奨を補強するか | **Yes** | ghostty 自身が「core を library 化」を進めており、設計思想 (state→VT 再生成、readonly stream) が vt100 と一致 |
| ghostty 方針を vt100 ベース実装にどう取り入れるか | **3 点** | (a) attach 復元 (= state_formatted()) は ghostty `Format.vt` と同じ思想で安心して採用、(b) hyoui daemon は ghostty `stream_readonly` 同様「query 応答を分離」する設計を取る、(c) scrollback の page-based 化は将来オプション (= MVP は vt100 内蔵 ring) |

### 1.5 1 行サマリ

ghostty は GUI emulator + 進行中の library 化 (= libghostty-vt、API 未安定)、cmux は libghostty を直接呼ぶ Swift native multi-tab terminal で multiplexer 状態管理は持たない。vt100 推奨は **揺るがない** どころか ghostty 側の動きで **思想的に補強される**。

---

## 2. ghostty の architecture overview

### 2.1 全体構造

```
ghostty/
├── src/
│   ├── main_ghostty.zig         // entry point (GUI)
│   ├── main_c.zig               // entry point (C lib build for libghostty)
│   ├── lib_vt.zig               // entry point (libghostty-vt Zig module + C API)
│   ├── Surface.zig              // terminal surface (= 1 tab/pane に対応する高位 object)
│   ├── App.zig                  // app-wide singleton
│   ├── apprt/                   // application runtime 抽象 (GLFW/GTK/AppKit/iOS)
│   ├── pty.zig                  // PTY 統合
│   ├── termio/                  // terminal IO loop (PTY ↔ Terminal)
│   ├── terminal/                // ← core: parser + state + screen
│   │   ├── Terminal.zig         // 11856 行: トップ level terminal state (modes, screens, scrolling, ...)
│   │   ├── Screen.zig           // 9506 行: 1 つの screen (primary or alternate) state
│   │   ├── ScreenSet.zig        // primary/alt の切替
│   │   ├── PageList.zig         // 12806 行: scrollback の page-based linked list
│   │   ├── page.zig             // 3839 行: 1 page (= row 群 + style table)
│   │   ├── Parser.zig           // 1072 行: VT/ANSI parser (Paul Williams state machine)
│   │   ├── stream.zig           // 3387 行: parser → action dispatcher (full handler)
│   │   ├── stream_readonly.zig  // readonly handler (response 不要な usecase 用)
│   │   ├── formatter.zig        // state → text/vt/html serialize
│   │   ├── modes.zig            // DECSET/DECRST mode 一覧
│   │   ├── osc.zig + osc/       // OSC parser (title, hyperlink, clipboard, etc.)
│   │   ├── kitty.zig + kitty/   // kitty protocol (keyboard, graphics, ...)
│   │   ├── sgr.zig              // SGR (= text color/style) parser
│   │   ├── style.zig            // Style (color + attrs)
│   │   ├── Selection.zig        // text selection
│   │   ├── search.zig + search/ // grid search
│   │   └── ...
│   ├── input/                   // key encoding (kitty keyboard protocol 等)
│   ├── renderer/                // GPU 描画 (Metal / OpenGL)
│   ├── font/                    // font shaping (harfbuzz)
│   └── ...
└── include/ghostty/             // public C headers
    ├── ghostty.h                // libghostty (= big lib) header
    └── vt.h + vt/               // libghostty-vt header
```

### 2.2 terminal state core の design

`Terminal.zig` (= 11856 行) の構造:

```zig
const Terminal = @This();

screens: ScreenSet,            // primary + alternate
status_display: ansi.StatusDisplay = .main,
tabstops: Tabstops,
rows: size.CellCountInt,
cols: size.CellCountInt,
width_px: u32 = 0,
height_px: u32 = 0,
scrolling_region: ScrollingRegion,
pwd: std.ArrayList(u8),
colors: Colors,
previous_char: ?u21 = null,
modes: modespkg.ModeState = .{},  // DECSET/DECRST 全 mode の bit field
mouse_shape: mouse_shape_pkg.MouseShape = .text,
// ...
```

screen は `ScreenSet` で primary/alternate を持ち、`active` で current を返す (= alt screen 切替は ScreenSet 内部)。

`Screen.zig` (= 9506 行) の構造:

```zig
alloc: Allocator,
pages: PageList,           // 1 screen は複数 page の linked list (page = 行群の memory pool)
no_scrollback: bool = false,
cursor: Cursor,
saved_cursor: ?SavedCursor = null,
selection: ?Selection = null,
charset: CharsetState = .{},
// ...
```

注目: **scrollback は `PageList` (linked list of memory page) で管理**。vt100 の単純 ring buffer と比べると memory efficiency が高い (= 古い page は free できる、style table は page 内 share)。10k 行+ の scrollback が現実的になる。

### 2.3 parser + stream

```
PTY bytes
  → Parser (= Paul Williams ANSI state machine)
  → Stream<Handler> (= Action を Handler dispatch、Action は print/CSI/OSC/DCS/APC など 30 種類+)
  → Handler.??? (= terminal.zig の `vtHandler`、もしくは stream_readonly.zig の readonly Handler)
  → Terminal state 更新
```

- `Parser.zig` (= 1072 行): SIMD 最適化された state machine。byte → Action 変換のみ責務 (= vte crate と同じ層)。
- `stream.zig` (= 3387 行): Action dispatch + Handler trait 風。SIMD optimization あり (= `simd/` ディレクトリ参照)。
- `stream_readonly.zig`: **「terminal that only needs to render output and doesn't need to respond」のための readonly handler**。design rationale が hyoui daemon と一致 (= §1.3.3 参照)。

### 2.4 formatter — attach 復元の primitive

`src/terminal/formatter.zig` の `Format` enum:

```zig
pub const Format = enum {
    plain,   // plain text
    vt,      // VT escape sequence 付き (= 色 + style + URL 等を再生成)
    html,    // HTML (= inline CSS)
};
```

`Format.vt` の役割は **screen state → VT bytes 再生成** で、vt100 の `Screen::state_formatted()` と同じ思想。`PageFormatter` / `ScreenFormatter` / `TerminalFormatter` の 3 階層で、page 単位 / screen 単位 / terminal 全体 (= palette 付き) を切り替えられる。

```zig
// 概念例
var fmt: ScreenFormatter = .init(terminal.screens.active, .vt);
try fmt.format(writer, options);  // → writer に VT 復元 bytes が流れる
```

これは hyoui DR-0013 §5 (= attach 復元 protocol) の正解パターンが ghostty 側でも採用されている証拠。

### 2.5 PTY 統合

- `src/pty.zig` + `src/termio/` が PTY 管理を担当 (= `read_thread.zig`、`write_thread.zig`、`subprocess.zig` 等)。
- 通常モード: ghostty が PTY を fork して child 起動、`read_thread` が PTY から bytes 取得 → `Stream` に流し込み → `Terminal` 更新 → `renderer` が描画。
- **manual IO mode** (cmux 用 patch): `GHOSTTY_SURFACE_IO_MANUAL` flag で PTY 統合を bypass、外側 (= cmux 側) が `ghostty_surface_process_output` で bytes を直接流し込む。**hyoui daemon が vt100 で行う構造とまったく同じ概念**。

---

## 3. libghostty-vt (= core library 化) の現状

### 3.1 build target

`build.zig` の関連 step:

```zig
const libvt_step = b.step("lib-vt", "Build libghostty-vt");
const test_lib_vt_step = b.step("test-lib-vt", "Run libghostty-vt tests");
// ...
const libghostty_vt_shared = shared: {
    // libghostty-vt の shared lib build
};
libghostty_vt_shared.install(libvt_step);
libghostty_vt_shared.install(b.getInstallStep());
```

= **libghostty-vt は libghostty (= big lib、`include/ghostty/ghostty.h`) と独立した build target**。`zig build lib-vt` で libghostty-vt.{so,dylib,a} が出る。

### 3.2 public surface — Zig module 経由 (= 完全に揃っている)

`src/lib_vt.zig` の export:

```zig
pub const apc = terminal.apc;
pub const dcs = terminal.dcs;
pub const osc = terminal.osc;
pub const point = terminal.point;
pub const color = terminal.color;
pub const device_status = terminal.device_status;
pub const formatter = terminal.formatter;
pub const highlight = terminal.highlight;
pub const kitty = terminal.kitty;
pub const modes = terminal.modes;
pub const page = terminal.page;
pub const parse_table = terminal.parse_table;
pub const search = terminal.search;
pub const size = terminal.size;

pub const Cell = page.Cell;
pub const Charset = terminal.Charset;
pub const Coordinate = point.Coordinate;
pub const CSI = Parser.Action.CSI;
pub const Page = page.Page;
pub const PageList = terminal.PageList;
pub const Parser = terminal.Parser;
pub const Pin = PageList.Pin;
pub const Point = point.Point;
pub const Screen = terminal.Screen;
pub const ScreenSet = terminal.ScreenSet;
pub const Selection = terminal.Selection;
pub const Style = terminal.Style;
pub const Terminal = terminal.Terminal;
pub const Stream = terminal.Stream;
pub const Cursor = Screen.Cursor;
pub const CursorStyle = Screen.CursorStyle;
pub const Mode = modes.Mode;
pub const Attribute = terminal.Attribute;

pub const input = struct {
    pub const PasteError = paste.Error;
    pub const isSafePaste = paste.isSafe;
    pub const encodePaste = paste.encode;
    pub const Key = key.Key;
    pub const KeyEvent = key.KeyEvent;
    pub const KeyEncodeOptions = key_encode.Options;
    pub const encodeKey = key_encode.encode;
};
```

= **Terminal / Screen / Stream / Parser / PageList / Page / Cell / Style / Cursor / Mode / Selection / 全部公開**。Zig からは完全に library として使える。

example (= `example/zig-vt-stream/src/main.zig`):

```zig
const ghostty_vt = @import("ghostty-vt");

var t: ghostty_vt.Terminal = try .init(alloc, .{ .cols = 80, .rows = 24 });
defer t.deinit(alloc);

var stream = t.vtStream();
defer stream.deinit();

try stream.nextSlice("Hello, World!\r\n");
try stream.nextSlice("\x1b[1;32mGreen Text\x1b[0m\r\n");
try stream.nextSlice("\x1b[1;1HTop-left corner\r\n");
// ...

const str = try t.plainString(alloc);
defer alloc.free(str);
```

= **vt100 の `Parser::process` / `Screen::contents()` と同じ ergonomics**。Zig 側では完全に library として動く。

### 3.3 public surface — C ABI 経由 (= 限定的、WIP)

`include/ghostty/vt.h` が含む header:

```c
#include <ghostty/vt/result.h>      // result type
#include <ghostty/vt/allocator.h>   // memory management
#include <ghostty/vt/osc.h>         // OSC parser
#include <ghostty/vt/sgr.h>         // SGR parser
#include <ghostty/vt/key.h>         // key encoding
#include <ghostty/vt/paste.h>       // paste safety check
#include <ghostty/vt/wasm.h>        // wasm utilities
```

= **OSC / SGR / Key / Paste の parser のみ**。**`Terminal` / `Screen` / `Stream` / `Cell` の C API は未公開**。`lib_vt.zig` の `comptime` block を見ると、export しているのは:

```
ghostty_key_event_*
ghostty_key_encoder_*
ghostty_osc_*
ghostty_sgr_*
ghostty_paste_is_safe
ghostty_color_rgb_get
ghostty_wasm_*
```

の系列のみで、**Terminal/Screen/Stream の C export は無い**。

公式 warning:

> `vt.h`: This is an incomplete, work-in-progress API. It is not yet stable and is definitely going to change.

> `lib_vt.zig`: WARNING: The API is not guaranteed to be stable. The functionality is extremely stable, since it is extracted directly from Ghostty which has been used in real world scenarios by thousands of users for years. However, the API itself (functions, types, etc.) may change without warning. We're working on stabilizing this in the future.

### 3.4 ghostty を Rust から使うときの選択肢

| 選択肢 | 評価 | 詳細 |
|---|---|---|
| (A) libghostty-vt C API (= 現状の expose 範囲) | **不可能** | Terminal/Screen の C export が無いので screen state 用途には未対応 |
| (B) libghostty (= big lib) | **不適合** | GUI + font + renderer + apprt 全部入りで巨大、hyoui の lean 方針と完全衝突 |
| (C) Zig module + cargo build script で Zig 同梱 | **理論上可能、実用上非推奨** | (1) hyoui の build system に Zig toolchain を追加、(2) Cargo + Zig の build 連携、(3) Zig allocator と Rust GlobalAlloc の橋渡し (= libghostty-vt は allocator を引数で受け取る設計なので可能性はある)、(4) API 不安定で breaking change を継続的に追従、の四重コスト |
| (D) ghostty repo を vendor + 自前 C shim 書く | **不採用** | (C) の上に C ABI shim 自作も乗る。投資対効果がない |
| (E) 待つ (= libghostty-vt が Terminal/Screen を C export するまで) | **将来オプション** | API 安定後の swap は容易になる可能性。hyoui の DR で「将来 ghostty-vt が安定したら再評価」と annotate |

→ **現時点で Rust 採用は (A)~(D) 全て非現実的**。vt100 採用が確定。

---

## 4. cmux の実装

### 4.1 実体特定

- **公式名: cmux (https://cmux.com/、`manaflow-ai/cmux`)**。
- License: AGPL-3.0、Swift / AppKit native macOS app (2026-02 リリース、HN 上位)。
- repo: `manaflow-ai/cmux` (= kawaz の README の `nichochar/cmux` は古い URL、redirect/migration したと推定。`nichochar` は cmux 創業者の個人 GitHub だった可能性)。
- 公式説明: 「Ghostty-based macOS terminal with vertical tabs and notifications for AI coding agents」。
- 重要: **cmux はファイルベースの fork 関係ではなく、ghostty を submodule + library 利用** (= `WebKit` を埋め込む native app と同じ関係)。

### 4.2 directory 構造 (= GitHub API 経由)

```
manaflow-ai/cmux/
├── .gitmodules                  // → manaflow-ai/ghostty + manaflow-ai/bonsplit + manaflow-ai/homebrew-cmux
├── Sources/                     // Swift code (= main app)
│   ├── App/                     // AppDelegate 関連
│   ├── AgentHibernation/        // session restore
│   ├── Auth/
│   ├── Cloud/
│   ├── AppDelegate.swift
│   ├── GhosttyApp+SurfaceConfigurationReload.swift
│   ├── GhosttyConfig.swift
│   ├── GhosttyTerminalView.swift          // ← ghostty 連携の中核
│   ├── GhosttyTerminalAppearance.swift
│   ├── GhosttyNSView+IMEComposition.swift
│   ├── TerminalController.swift
│   ├── TerminalNotificationQueue.swift
│   ├── SessionRestoredTerminalCommandStore.swift  // ← 「state replay」ではなく「command replay」
│   ├── WorkspaceSurfaceConfig.swift
│   └── ... (数百ファイル)
├── Packages/                    // Swift Package (= 内部分割)
│   ├── CMUXAgentLaunch/
│   ├── CMUXAgentVault/
│   ├── CMUXAuthCore/
│   ├── CMUXDebugLog/
│   ├── CMUXPasteboardFidelity/
│   ├── CMUXSocketPathDomain/    // ← Unix socket API
│   ├── CMUXWorkstream/
│   └── CmuxExtensionKit/
├── Native/                      // native helper (= Swift 以外)
├── CLI/                         // cmux CLI tool (= cmux command for users)
├── ghostty/ (submodule)         // manaflow-ai/ghostty
├── homebrew-cmux/ (submodule)
└── vendor/bonsplit/ (submodule)
```

### 4.3 ghostty fork (= manaflow-ai/ghostty) の patches

cmux 側の `docs/ghostty-fork.md` から:

- **目的**: 「ローカルパッチを上流に取り込む前のための fork。定期的に上流と rebase。」 (= 一時的 fork、upstream に merge を目指す)
- **ピン**: May 2026 時点で `176bd550f` (= upstream に対する具体的 rev)

主な patch:

| Patch | 内容 | 上流への意図 |
|---|---|---|
| Theme picker integration | `cli-helper` バイナリ + theme override file + cmux reload 通知 | 上流に merge 目指す |
| DECRPM mode 2031 fix | mode 2031 enable で DSR 997 即送 | 上流に merge 目指す |
| Manual IO mode | `GHOSTTY_SURFACE_IO_MANUAL`、`io_write_cb`、`ghostty_surface_process_output`、`ghostty_surface_text_input`、`ghostty_surface_render_now` | **iOS 系向け**、上流に merge 目指す |
| Re-expose selection API | `ghostty_surface_select_cursor_cell`、`ghostty_surface_clear_selection` を再 expose | cmux 専用 (= keyboard copy mode 用) |
| OSC 99 + APC | kitty notification parser + kitty graphics APC handler | 上流に merge 目指す |
| Surface rendering | CVDisplayLink 再起動、resize 中の stale frame 再描画、IME / preedit 改善 | 上流に merge 目指す |
| URL/path regex | 空白を含むパス対応 | 上流に merge 目指す |

= **大半が cmux 専用ではなく upstream 候補**。ghostty 本家への寄稿が前提の運用。

### 4.4 libghostty API 呼び出し (= cmux の Swift 側)

`Sources/GhosttyTerminalView.swift` から (= WebFetch 経由で取得):

**初期化・設定**:
```
ghostty_init()
ghostty_config_new()
ghostty_config_load_default_files()
ghostty_config_load_file()
ghostty_config_load_string()                // ← cmux 専用 patch (= メモリ内文字列から設定ロード)
ghostty_config_load_recursive_files()
ghostty_config_finalize()
ghostty_config_free()
ghostty_config_get()
ghostty_config_diagnostics_count()
ghostty_config_get_diagnostic()
```

**App / runtime**:
```
ghostty_app_new(config, &runtime_callbacks)
ghostty_app_set_focus()
```

**Surface (= 1 tab/pane に対応)**:
```
ghostty_surface_clear_selection_compat()
ghostty_surface_select_cursor_cell_compat()
ghostty_surface_complete_clipboard_request()
```

**runtime callbacks**:
```
wakeup_cb                  // I/O thread → main thread への wake up
action_cb                  // terminal action (= bell, title 変更, etc.)
read_clipboard_cb
confirm_read_clipboard_cb
write_clipboard_cb
close_surface_cb
```

= cmux 側は **GUI 層 (= Swift / AppKit)** に専念し、terminal state / parsing / scrollback / cell grid 等は全部 ghostty に委譲。**Swift code 中に Cell や Row や Screen を扱う型は出てこない** (= ghostty surface object の不透明 handle として保持するだけ)。

### 4.5 terminal state は cmux 側に無い (= 重要)

cmux source の検索結果 (= `gh api search/code repo:manaflow-ai/cmux ...`) から:

- "terminal" を含むファイルは多数あるが、すべて **TerminalController** (= window/pane 管理)、**TerminalNotification\*** (= 通知)、**TerminalImageTransfer** (= image)、**GhosttyTerminalView** (= ghostty 統合 view) 等で、**Cell / Row / Screen / ScrollBack を Swift 側で扱うファイルは無い**。
- session 復元は `SessionRestoredTerminalCommandStore` で実装されており、**「保存していた command line」を再実行する** (= state replay ではない)。プロセスが終わると terminal state は失われる。
- **multiplexing = AppKit window + tab + split + ghostty surface の組み合わせで実現**。各 split が独立した ghostty surface (= 独立した PTY + Terminal state) を持つ。

### 4.6 cmux の attach / detach

**hyoui 的な attach/detach (= 同じ daemon の screen state を別端末から見る) は cmux にはない**。理由:

- cmux は terminal emulator + window manager (= GUI app)、headless daemon ではない。
- 「detach」概念は **Unix socket API** 経由で外側 (= `cmux` CLI、AppleScript、Web 自動化) から命令を投げる形でのみ存在 (= GUI 操作の remote、画面復元ではない)。
- 1 つの surface に複数 client が接続する想定は無い (= GUI に直接 attach されている macOS window 1 つ)。

→ **cmux の実装パターンから hyoui の attach 復元の参考になる箇所は少ない**。代わりに「state を保持する側 (= ghostty surface object) を**プロセス生存中ずっと alive にする**」という保証だけが教訓 (= hyoui daemon が screen state を保つ前提と一致)。

---

## 5. terminal state data model (= ghostty 側、cmux 側それぞれ)

### 5.1 ghostty 側

| 項目 | ghostty | 備考 |
|---|---|---|
| 構造体トップ | `Terminal` | 11856 行 |
| screen 切替 | `ScreenSet { primary: Screen, alternate: Screen, active: *Screen }` | alt screen は `?1049h`/`?1049l` で切替 |
| 1 screen の cell 列 | `Screen { pages: PageList, cursor, selection, ... }` | scrollback も `PageList` |
| scrollback | `PageList` (= linked list of `Page`、各 page は memory pool) | vt100 の単純 ring より efficient |
| 1 page | `Page { rows, cells, styles (= style table), graphemes }` | style は table で share、unicode は dedup |
| 1 cell | `Cell { content_tag (codepoint or pin or wide), style_id, ... }` | wide char / grapheme cluster / hyperlink 対応 |
| cursor | `Screen.Cursor { x, y, style: CursorStyle, ... }` | shape, blink, visible 全部 |
| mode | `modespkg.ModeState` (= DECSET/DECRST 全 mode の packed bit field) | ratuit に検索可能 |
| color | `Colors` (= 256 palette + default fg/bg) | OSC 4/10/11 で動的変更可 |
| size | `rows, cols` + `width_px, height_px` (= 画素単位) | pixel size は image / mouse 用 |
| selection | `Selection` (= start/end pin、rectangle 対応) | text 選択 |
| hyperlink | `hyperlink.zig` | OSC 8 |
| kitty graphics | `kitty.zig + kitty/` | image 対応 |

### 5.2 cmux 側

= **terminal state は Swift 側に持たない**。`GhosttySurface` (= ghostty 側の opaque handle) を unmanaged で保持し、cmux Swift 側は **window / tab / pane / notification / pasteboard / browser 等の GUI 状態だけ管理**。

cmux 固有の高位 state:
- `WorkspaceSurfaceConfig` — 各 split の論理位置・cwd・git branch
- `TerminalNotificationQueue` — bell + OSC 9/99 通知
- `SessionRestoredTerminalCommandStore` — 起動時の引数 / cwd を JSON で永続化
- `ClosedItemHistory` — 閉じた tab の履歴
- `CmuxEventBus` / `CmuxEventStream` — Unix socket API への event broadcast

= **cmux の「multiplexer 状態」は terminal state ではなく workspace state**。terminal state が再生成不要なら command replay で十分、というアーキテクチャ判断。

---

## 6. Rust からの利用可能性

### 6.1 libghostty-vt + Rust の組み合わせ

**現状: 実用不可**。理由:

1. **C API 未完成**: Terminal/Screen/Stream の C export が無い (= §3.3)。
2. **Zig module 経由のみ完全**: でもこれは Rust から直接呼べない。Zig を Rust の build に組み込む必要 (= `cargo build` の中で `zig build lib-vt` を呼ぶ shell out)。
3. **allocator**: libghostty-vt は `std.mem.Allocator` を引数で受け取る設計 (= Zig 慣習)。Rust 側で `GlobalAlloc` を Zig 互換に wrap する shim が必要。
4. **API instability**: 公式 warning が「API は変わる」と明言。hyoui の release 安定性に直結。
5. **build complexity**: Cargo + Zig + Cmake (= ghostty 依存) の三層連携、cross compile も難。

### 6.2 仮に Rust + libghostty-vt をやる場合の架空 shim

```rust
// 概念例 — 実装するなら
unsafe extern "C" {
    fn ghostty_terminal_new(rows: u32, cols: u32) -> *mut GhosttyTerminal;
    fn ghostty_terminal_free(t: *mut GhosttyTerminal);
    fn ghostty_terminal_stream_bytes(t: *mut GhosttyTerminal, bytes: *const u8, len: usize);
    fn ghostty_terminal_plain_string(t: *mut GhosttyTerminal, out: *mut *const u8, out_len: *mut usize) -> i32;
    fn ghostty_terminal_format_vt(t: *mut GhosttyTerminal, out: *mut *const u8, out_len: *mut usize) -> i32;
    fn ghostty_terminal_cursor(t: *mut GhosttyTerminal, x: *mut u32, y: *mut u32);
    fn ghostty_terminal_is_alt_screen(t: *mut GhosttyTerminal) -> bool;
    // ... 数十個 ...
}
```

= **これらの C API は現状 ghostty 側に存在しない**。hyoui が ghostty 本家に提案して PR を通すか、自前で `src/terminal/c_api.zig` を fork で増やす必要。投資対効果が著しく低い。

### 6.3 結論

**vt100 採用が現実解、ghostty 側の動きは将来 (= 1-2 年後の libghostty-vt 安定化) の swap option として記録**。

---

## 7. attach / detach 機構

### 7.1 cmux にはない (= 該当機能なし)

§4.6 で述べた通り。cmux は GUI 専用なので multiplexer 型の attach/detach は無い。

### 7.2 ghostty にもない

ghostty 本体も terminal emulator (= GUI) なので detach/attach 概念は無い。

### 7.3 hyoui への教訓

= **ghostty / cmux からは attach 復元 protocol そのものは学べない**。先行調査 (= tmux/abduco/zellij/wezterm 系) の方が直接参考になる。

ただし**間接的教訓**:

1. **state を保持する process (= cmux の場合は cmux.app 本体、hyoui の場合は daemon) を生かし続ける**ことで multi-client / replay を実現するアーキテクチャは ghostty/cmux 系も同じ。
2. **state を VT bytes に serialize する API (= ghostty `Format.vt` / vt100 `state_formatted()`) は両者が独立に到達した standard pattern**。

---

## 8. hyoui に応用すべきパターン

ghostty / cmux 由来で hyoui に取り入れるべき設計教訓:

### 8.1 必ず取り入れる (= MVP 内)

1. **readonly stream 分離** (= ghostty `stream_readonly.zig`)。hyoui daemon は子 PTY からの bytes を流し込んで state を作るが、**DSR / DA / cursor query 等の "response が必要" な action を別 layer で処理する**。vt100 採用後の hyoui 実装で、`vt100::Parser::process` を直接呼ぶ部分とは別に、**bytes を 1 度 pre-scan して response が必要な escape sequence を検出し、daemon が代行応答するか子に転送するか判断する layer** を入れる。これは DR-0013 §4 (= state 構造) に明記すべき。

2. **state→VT 再生成 = attach 復元の標準パターン** (= ghostty `Format.vt` + vt100 `state_formatted()`)。両者の API 名は違うが「visible cells + cursor + mode を VT bytes に再生成」という同じ思想で実装されている。hyoui の DR-0013 §5 (= attach 復元 protocol) で「業界 standard pattern」と明記し、設計の正当性を補強する。

3. **terminal state を保持する process を 1 つ alive にする** (= cmux も hyoui も同じ)。multi-client (hyoui) / multi-window (cmux) は **同じ state を複数 viewer に投げる**形で実現する、replay ではなく live mirror。

### 8.2 将来取り入れる (= MVP 後の延長線)

4. **page-based scrollback** (= ghostty `PageList`)。MVP は vt100 の ring buffer で十分だが、scrollback を 10k 行+ に増やす場合は vt100 → 自前 page-list ベースに置換を検討。**ghostty `PageList` の Zig コードは Rust に port するときの参考実装として活用可能** (= MIT license)。

5. **style table での dedup** (= ghostty `style.zig` + `page.zig` の style table)。同じ style (fg/bg/attr 組合せ) は table 内で share、cell には id だけ持つ。large scrollback の memory に効く。

6. **manual IO pattern** (= cmux の `GHOSTTY_SURFACE_IO_MANUAL`)。**hyoui daemon が vt100 に PTY bytes を流し込む構造と概念的に同じ**。「外側 (= daemon) が PTY 統合の責任を負い、terminal state library は純粋に bytes-in / state-out のみ」という分離が両者で一致。これは hyoui の lean 方針 + nix 直接利用と整合する。

7. **OSC handler の独立 callback** (= vt100 `Parser::new_with_callbacks` / ghostty `osc.zig`)。OSC 8 hyperlink / OSC 9 notification / OSC 52 clipboard / title change を hyoui の structured event (= protocol message) に変換するための hook point を MVP 時点で確保しておく。

### 8.3 採用しない (= hyoui スコープ外)

8. **kitty graphics / sixel / iTerm2 inline images**: ghostty/cmux 両方サポートしているが、hyoui の MVP scope 外。記録のみ。

9. **font shaping / GPU 描画**: ghostty 中心の機能、hyoui は client terminal に任せる。

10. **bidi / complex script**: vt100 でも最小限の対応、ghostty は finl_unicode + wezterm-bidi。hyoui は MVP では西欧 + CJK + grapheme cluster (= vt100 範囲) で十分。

---

## 9. 採用判断の影響

### 9.1 vt100 推奨を覆すか

**No**。理由:

1. libghostty-vt の C API は OSC/SGR/Key/Paste の parser のみ。Terminal/Screen 本体は未 expose。
2. Zig module は production-grade だが Rust から使うコストが過大 (= cargo + Zig + allocator shim + API instability の四重苦)。
3. ghostty 本体 (= big lib) は GUI + font + renderer 全部入りで lean 方針と完全衝突。

### 9.2 vt100 推奨を補強するか

**Yes**。理由:

1. ghostty 自身が **terminal core を library 化する作業を本格的に進めている** (= `src/lib_vt.zig`、`libghostty-vt`、`zig build lib-vt` step)。**「terminal core を別 application で使う」というドメインの正当性そのものを ghostty 本家が認証している**。vt100 が「個人開発で abandoned 説」と書かれていた件が完全に払拭される (= ghostty が同じ方向を向いている = 業界 standard)。
2. ghostty の `formatter.zig` (= `Format = .vt`) と vt100 の `Screen::state_formatted()` は**独立に同じ「state→VT 再生成」 API を提供**。これは hyoui の DR-0013 §5 (= attach 復元 protocol) の正解 pattern が業界 standard である証拠。
3. ghostty の `stream_readonly.zig` の design rationale (= 「terminal that only needs to render output and doesn't need to respond... ideal for replay tooling, CI logs, PaaS builder output, etc.」) が hyoui daemon の usecase と完全一致。**hyoui の責務分離 (= state 維持 / response 代行) のアーキテクチャ判断を直接根拠付ける**。

### 9.3 ghostty の方針を vt100 ベース実装にどう取り入れるか

§8 の「必ず取り入れる」3 項目:

1. **readonly stream 分離** = DR-0013 §4 に追記
2. **state→VT 再生成 = 業界 standard** = DR-0013 §5 に補強
3. **state 保持 process の長期 alive 保証** = DR-0013 §1 (= データモデル) に追記

§8 の「将来取り入れる」4 項目は ROADMAP `追加予定` に記録。

### 9.4 ghostty が将来採用候補になる条件

将来 (= 1-2 年後) に hyoui が libghostty-vt に swap する場合の前提条件:

1. `include/ghostty/vt.h` に `Terminal` / `Screen` / `Stream` の C API が export されている (= 現状未対応)
2. C API が semver で stable と annotate されている (= 現状 WIP)
3. allocator が C 標準の `malloc/free` を default で使う form がある (= 現状 Zig allocator 必須)
4. vt100 で実装した hyoui MVP に「ghostty なら解決する」明確な不足が生じている (= 現状不明)

→ hyoui の DR-0013 に「**将来 libghostty-vt が C API stable になったら再評価する**」と annotate する。MVP は vt100 で確定。

---

## 10. 不明点 / 続調査項目

### 10.1 確証取れた事項

- ghostty repo / build system / lib_vt module の存在
- libghostty-vt の C API expose 範囲 (= OSC/SGR/Key/Paste のみ)
- ghostty `Terminal` / `Screen` / `PageList` の Zig 実装サイズ + design
- cmux = manaflow-ai/cmux の実体 + Swift + ghostty submodule 関係
- cmux が呼ぶ libghostty API 名 (= GhosttyTerminalView.swift より)
- cmux の "manual IO mode" patch

### 10.2 不明点 / 続調査が必要なら

1. **libghostty-vt の C API roadmap**: ghostty discord / GitHub discussions で Terminal/Screen の C export 計画があるか。本調査では未確認、必要なら別タスク。
2. **manaflow-ai/ghostty fork の具体的 patch 内容**: `docs/ghostty-fork.md` の summary は読んだが、各 patch の diff は読んでいない。`manual IO mode` の具体的 hook 点 (= Zig 側でどこに insert したか) は将来 hyoui が同様の pattern を採用するときの参考になる可能性。
3. **ghostty `PageList` の reflow 品質**: resize 時の line-wrap 保持の挙動。MVP では vt100 を使うので不要だが、将来 page-based に置換する際に重要。
4. **ghostty `stream_readonly` の query 検出 list**: どの escape sequence を「response が必要 = stream_readonly で ignore」と判定しているかの具体 list。hyoui daemon が「daemon が代行応答するか子に転送するか」の判断 list を組むときの参考になる。
5. **cmux の Unix socket API protocol**: cmux CLI から GUI app に投げる socket protocol。本調査では `CMUXSocketPathDomain` package が存在することだけ確認、詳細未読。hyoui の protocol design の比較対象になり得る。

### 10.3 本調査で確証取れなかった主張 (= 慎重に扱うべき)

- **kawaz が日常使う cmux が「nichochar/cmux」かどうか**: 本人 README にそう書かれているが、実際の repo は manaflow-ai/cmux で nichochar アカウントは見つからない (404)。可能性: (a) nichochar 個人が manaflow-ai に migrate した、(b) kawaz の README リンクが間違いで実際は manaflow-ai を使っている。**kawaz に確認した方が良い**。
- **hyoui の DR-0013 が「screen emulator + attach/detach 安定化を必須」とする方針**: 本調査では揺らがず補強される、と判定したが、これは現先行調査結論との合算 (= vt100 推奨) を前提とする。万一 vt100 PoC で深刻な障害が出た場合は ghostty-vt 待ちも改めて検討候補に上がる。

---

## 参考 URL / ファイル

- ghostty repo: https://github.com/ghostty-org/ghostty
- ghostty 公式 site: https://ghostty.org
- libghostty-vt header: https://github.com/ghostty-org/ghostty/blob/main/include/ghostty/vt.h
- libghostty-vt Zig module: https://github.com/ghostty-org/ghostty/blob/main/src/lib_vt.zig
- ghostty Terminal.zig: https://github.com/ghostty-org/ghostty/blob/main/src/terminal/Terminal.zig
- ghostty Screen.zig: https://github.com/ghostty-org/ghostty/blob/main/src/terminal/Screen.zig
- ghostty PageList.zig: https://github.com/ghostty-org/ghostty/blob/main/src/terminal/PageList.zig
- ghostty formatter.zig: https://github.com/ghostty-org/ghostty/blob/main/src/terminal/formatter.zig
- ghostty stream_readonly.zig: https://github.com/ghostty-org/ghostty/blob/main/src/terminal/stream_readonly.zig
- ghostty example zig-vt-stream: https://github.com/ghostty-org/ghostty/blob/main/example/zig-vt-stream/src/main.zig
- cmux 公式: https://cmux.com/
- cmux repo: https://github.com/manaflow-ai/cmux
- cmux ghostty fork doc: https://github.com/manaflow-ai/cmux/blob/main/docs/ghostty-fork.md
- cmux GhosttyTerminalView.swift: https://github.com/manaflow-ai/cmux/blob/main/Sources/GhosttyTerminalView.swift
- awesome-libghostty: https://github.com/Uzaaft/awesome-libghostty
- 関連先行調査: `/Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main/docs/research/2026-05-27-screen-emulator-crate-comparison.md`
- 関連先行調査: `/Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main/docs/research/2026-05-27-multiplexer-implementation-study-classic.md`
- 関連先行調査: `/Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main/docs/research/2026-05-27-multiplexer-implementation-study-rust.md`
- handoff: `/Users/kawaz/.local/share/repos/github.com/kawaz/hyoui/main/docs/journal/2026-05-27-screen-emulator-pivot-handoff.md`
