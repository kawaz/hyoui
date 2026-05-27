# 老舗 C 系 terminal multiplexer 実装研究 (tmux / abduco / screen / dvtm)

- Date: 2026-05-27
- Author: Claude (sub-agent, research mode)
- 目的: hyoui (Rust 製、daemon-side screen emulator pivot) の attach/detach 設計に
  老舗実装の知見を取り込む。DR-0013 起票・実装時に直接引用できる粒度で抽出する。

---

## 1. 要約 (結論先出し)

### 各 multiplexer の性格

| 実装 | 性格 | hyoui 観点での価値 |
|---|---|---|
| **tmux** | C で 30 年級。daemon-side に full screen emulator + grid + reflow + 複数 client 同時対応。OpenBSD imsg ベース IPC。**hyoui がベンチマークすべき正本**。 | 設計の正本。grid 構造 / reflow / redraw flag bitmap / IPC 経路すべて参考になる。 |
| **abduco** | 思想最近 (2013-)、**1300 行程度の minimal**。screen emulator を持たず raw bytes を MSG_CONTENT で broadcast。attach 時の redraw は子に任せる (SIGWINCH のみ)。 | 「scope を絞った時の minimal な protocol design」と「state を持たないがゆえに hyoui の課題に直撃する」反面教師の両面で価値。 |
| **GNU screen** | 1987-。Message + SIGCONT で client/server 通信。MSG_ATTACH を受けると `Redisplay()` → `RefreshAll()` で daemon-side grid から全画面を redraw する **state-restoring** design。 | hyoui がやろうとしている「attach 時に daemon が画面を再構築して送る」の老舗実装。`RefreshAll/RefreshLine/DisplayLine` の階層化が直接参考になる。 |
| **dvtm** | single-process (client/server 分離なし)。`vt.c` に **完全な screen state + 2 buffer (normal/alt) + ring buffer scrollback**。redraw は ncurses が担当。 | screen state の data model (Cell / Row / Buffer / ring buffer scrollback) が一番読みやすい。hyoui の grid struct 設計の出発点として最適。 |

### hyoui への教訓 top 10

1. **daemon に full screen emulator を持つ正解は tmux/screen/dvtm 側。abduco の "state を持たず stream する" は scope 限定 (= detach のみ、attach 復元なし) の選択であり、hyoui は前者をモデルにすべき。**
2. **attach 復元は「flag を立てて next render tick でやる」設計が定石** — tmux の `CLIENT_ALLREDRAWFLAGS` 一発 set がコア (`server-fn.c: server_redraw_client`)。set した時点では描画しない。client が次に sync する時にまとめて redraw する。
3. **redraw は「grid を頭から舐めて ANSI byte を吐く」だけで OK** — screen 系で「子に redraw を頼む」発想は abduco だけ。tmux/screen/dvtm はすべて daemon-side grid を正本として描き直す。子は無関係。
4. **alt screen は別 grid を保持する** (tmux の `saved_grid`、dvtm の `buffer_normal`/`buffer_alternate`、`buffer` ポインタで切替)。hyoui も同様に primary/alt の 2 grid 構造にすべき。
5. **入力 parser は DEC Williams 状態機械が正解** (tmux `input.c`)。部分 sequence は state machine 内部の buffer (`interm_buf[4]`, `param_buf[64]`, dynamic `input_buf`) に貯め、終端 byte (`ST`/`BEL`/final char) が来た時に dispatch する。**detach 時に in-flight bytes を flush する必要はなく、state machine が自然に貯めているのでそのまま attach 後に続けられる**。
6. **redraw 時の attribute SGR は前 cell との差分のみ吐く** (tmux `tty.c: tty_attributes` の "Ignore cell if it is the same as the last one" + `tty->last_cell` キャッシュ)。hyoui も `last_cell` キャッシュを持って差分 SGR を出すべき。
7. **resize 時の reflow は「wrap 済み行を logical line に再結合 → 新 width で再分割」**。GRID_LINE_WRAPPED flag が key (tmux `grid.c: grid_reflow_join` → `grid_reflow_split`)。cursor は `grid_wrap_position` で wrap 座標に変換しておき、reflow 後に `grid_unwrap_position` で戻す。
8. **複数 client の resize 戦略は config option で選ばせる** — tmux の `window-size` (`smallest`/`largest`/`manual`/`latest`)。default は smallest。hyoui もこの 4 モードを設定可能にすべき (まずは smallest 固定で OK)。
9. **IPC は imsg 等の framed binary が定石**。tmux は OpenBSD imsg (length-prefix + type + fd-passing)、abduco は固定 4KB Packet、GNU screen は Message + SIGCONT。**自前 framing は abduco の Packet(type/len/union) が最 minimal な reference**。
10. **socket permission で attach 認可を表現する** (tmux `server.c: server_update_socket` が sessions attached の有無で exec bit を出し入れ、screen は SOCKMODE macro で multi-user bit を制御)。hyoui も socket perm を access control の primary 手段にするのが Unix 流。

### 特に attach 復元 / detach flush で参考になる 3 つのパターン

- **Pattern A (tmux/screen): "flag set → 次の render tick で grid から ANSI 再構築"** ← hyoui 採用すべき本命
- **Pattern B (abduco client): "attach 開始時に `\033[?1049h\033[H` (alt screen on + cursor home) を client が emit、detach 時に `\033[?25h\033[?1049l` で off"** ← hyoui client 側でも採用すると後始末が綺麗
- **Pattern C (tmux input.c): "部分 sequence は state machine 内部 buffer で自然に貯める。child の出力を逐次 parser に流せば detach 時に明示的な flush は不要"** ← hyoui の「detach 時に in-flight bytes を flush」課題は state machine 化すれば消滅する

---

## 2. 各 multiplexer の architecture overview

### 2.1 tmux

- **プロセス構成**: 1 つの daemon (server) + N client process。stand-alone モードもあるが基本は client/server 分離。
- **IPC**: Unix socket + OpenBSD imsg ライブラリ (length-prefix + type + fd passing)。`proc.c: proc_send` が `imsg_compose(ibuf, type, PROTOCOL_VERSION, -1, fd, vp, len)` で送信。`imsgbuf_allow_fdpass(&peer->ibuf)` で fd 渡しを許可。
- **screen state の正本**: daemon 側に `struct grid` を pane ごとに持ち、child PTY からの byte は `input.c` (DEC parser) で parse して `screen-write.c` 経由で grid に書く。
- **client への出力**: `tty.c` が grid → ANSI byte 列を生成して socket 経由で client へ。
- **主要ファイル**:
  - `server.c` — daemon main loop, socket, signal
  - `server-client.c` — client lifecycle (attach/detach)
  - `server-fn.c` — server-side helpers (`server_redraw_client` 等)
  - `client.c` — client process side
  - `proc.c` — imsg-based IPC
  - `grid.c` — screen grid data + reflow
  - `screen.c` / `screen-write.c` — high-level screen ops (alt screen 切替含む)
  - `screen-redraw.c` — full client redraw
  - `tty.c` — grid → terminal byte stream
  - `input.c` — DEC parser
  - `resize.c` — multi-client size calc
  - `tmux-protocol.h` — `enum msgtype`, `PROTOCOL_VERSION`

### 2.2 abduco

- **プロセス構成**: 1 daemon (server) + N client。**screen emulator 一切なし**。
- **IPC**: 固定サイズ Packet (4KB) を Unix socket で送受信。
- **PTY data の扱い**: PTY からの byte を **そのまま** `MSG_CONTENT` packet で attach 中の全 client に broadcast。
- **attach 時の復元**: **何もしない**。新規 attach 後に流れてきた byte だけが client に届く。子に SIGWINCH を送って "再描画してね" と頼むのみ (再描画するかは子次第)。
- **client 側の terminal 初期化**: `client_setup_terminal` で `\033[?1049h\033[H` (alt screen on + cursor home) を出力。detach 時に `\033[?25h\033[?1049l` で復元。**raw bytes を流すだけだが、client 側で alt screen を被せておくことで本来の terminal を汚さない設計**。
- **主要ファイル**: `abduco.c` (main + Packet 定義 + 共有)、`server.c`、`client.c`。

### 2.3 GNU screen

- **プロセス構成**: 1 backend (=server) + N attacher (=client)。
- **IPC**: Unix socket + `struct Message` (type + union)。**SIGCONT による補助シグナリング**: attacher は `MSG_ATTACH` を送ったあと `pause()` し、backend が処理を終えると attacher に SIGCONT を送って起こす。
- **attach 復元**: backend 側に `struct mline` (各行) と `struct mchar` (各 cell) で screen state を保持。`MSG_ATTACH` 受信時に `ReceiveMsg` → `CreateTempDisplay` → `FinishAttach` で display を bring up し、`Redisplay()` → `RefreshAll(1)` → `RefreshLine` → `DisplayLine` で全画面を再描画する。
- **state restoration**: `FinishAttach` が「Forget all we knew about the old terminal, reread the termcap entries」する (= 新 client の terminfo を読み直して termcap-aware に書き出す)。
- **主要ファイル**: `attacher.c`, `screen.c`, `socket.c`, `display.c`, `ansi.c` (DEC parser), `image.c` (mline)。

### 2.4 dvtm

- **プロセス構成**: **single process**。client/server 分離なし。tiling WM + vt100 emulator が同一プロセス内に同居。
- **screen state**: `vt.c` に完全な VT100 emulator が独立した library として閉じている (`vt.h` API)。dvtm.c は vt_process / vt_draw を呼ぶだけ。
- **redraw**: ncurses 経由。`vt_draw` が dirty 行だけ ncurses window に書き出し、`doupdate` で flush。
- **主要ファイル**: `dvtm.c` (WM + main loop)、`vt.c` (emulator + 2 buffer + scrollback)、`*.c` (layouts: tile/grid/bstack 等)。
- **hyoui への意味**: client/server がないので IPC の参考にはならないが、「screen state struct はこう書く」の最 minimal な実装例 (~4000 LoC) として読める。**`vt.c` の Buffer/Cell/Row + ring buffer scrollback の design は hyoui grid struct の出発点として直輸入可能**。

---

## 3. attach 復元シーケンスの実装パターン

### 3.1 tmux: "flag を立てて次の render tick で grid から再構築"

**core mechanism**: `server-fn.c: server_redraw_client`:

```c
void
server_redraw_client(struct client *c)
{
	c->flags |= CLIENT_ALLREDRAWFLAGS;
}
```

これだけ。**bit set のみで描画はしない**。次の event loop tick で `screen-redraw.c: screen_redraw_screen` が flag を見て描く:

```c
flags = screen_redraw_update(&ctx, c->flags);
if ((flags & CLIENT_ALLREDRAWFLAGS) == 0)
    return;

tty_sync_start(&c->tty);
if (flags & (CLIENT_REDRAWWINDOW|CLIENT_REDRAWBORDERS)) {
    screen_redraw_draw_borders(&ctx);
    /* ... */
}
if (flags & CLIENT_REDRAWWINDOW) {
    screen_redraw_draw_panes(&ctx);
}
if (ctx.statuslines != 0 && (flags & CLIENT_REDRAWSTATUS))
    screen_redraw_draw_status(&ctx);
```

**CLIENT_ALLREDRAWFLAGS は複数 bit の合成定数** (`tmux.h` から抜粋):

```c
#define CLIENT_REDRAWWINDOW   0x8
#define CLIENT_REDRAWSTATUS   0x10
#define CLIENT_REDRAWBORDERS  0x400
#define CLIENT_REDRAWOVERLAY  0x2000000
#define CLIENT_REDRAWPANES    0x20000000
#define CLIENT_REDRAWSCROLLBARS 0x4000000000ULL
```

**attach 経路**: `cmd-attach-session.c: cmd_attach_session_exec` → `cmd_attach_session` (内部で `server_client_set_session`)。`server_client_set_session` 自体は session 紐付けと `MSG_READY` 送信だけ:

```c
if (~c->flags & CLIENT_CONTROL)
    proc_send(c->peer, MSG_READY, -1, NULL, 0);
```

実際の redraw flag は session 切替に伴う recalculate_sizes 経由などで set される (= attach 完了 = full redraw trigger という不変条件を仕組みで担保している)。

### 3.2 GNU screen: "Redisplay → RefreshAll → RefreshLine → DisplayLine"

階層化された redraw 関数群:

- `Redisplay()` — terminal modes をリセット (InsertMode, ChangeScrollRegion, KeypadMode) してから `RefreshAll(1)`
- `RefreshAll()` — canvases を iterate して `RefreshArea()`
- `RefreshArea()` — 各行に `RefreshLine()`
- `RefreshLine()` — 行内の viewport/layer 判定して layer redisplay を呼ぶ
- `DisplayLine()` — 旧 screen buffer と新 screen buffer を **比較**して、差分だけを `GotoPos()` + 属性適用 + `PUTCHAR()` で出す

attach 時は `MSG_ATTACH` → `ReceiveMsg` → `CreateTempDisplay` → `FinishAttach` → `Redisplay` で全画面再生成。`FinishAttach` 内で **terminfo の reread** をやっているのが特徴 (= attach する client ごとに terminal capability が違う前提で書き出す)。

### 3.3 abduco: "復元しない"

- attach 時の状態復元なし。
- `attach_session()` で socket 接続 → `client_setup_terminal()` で alt screen ON + 端末 raw mode → `client_mainloop()` で読み書きするだけ。
- 子 process には SIGWINCH を送って「再描画してね」と頼むのみ (`MSG_RESIZE` packet 経由)。
- **hyoui がやろうとしているもの (= daemon-side state 正本 + attach 時に復元) とは方向性が逆**。abduco を真似ると hyoui の現在の課題 (子が新 client を知らない、部分 redraw しか出ない) がそのまま再現する。

### 3.4 比較 + hyoui への寓意

| | state 保持 | 復元方法 | 子への通知 |
|---|---|---|---|
| tmux | daemon grid | flag set → next tick で screen-redraw | (なし、子は知らない) |
| GNU screen | daemon mline/mchar | Redisplay → RefreshAll | (なし) |
| dvtm | vt buffer (single proc) | vt_draw が dirty 行を描き直す | N/A |
| abduco | (なし) | (復元しない) | SIGWINCH のみ |

**hyoui がやるべきは tmux/screen pattern**: daemon が grid を持ち、attach 時に flag を set し、次の write tick で grid から ANSI byte を生成して socket に流す。「子に通知して redraw してもらう」発想 (= abduco) は捨てる。

---

## 4. screen state の data model 比較

### 4.1 tmux (`tmux.h`)

```c
struct grid_cell {
	struct utf8_data data;     /* UTF-8 char + width */
	u_short          attr;     /* bold/italic/underline 等 bitmask */
	u_char           flags;
	int              fg;       /* 32-bit (256 color or RGB) */
	int              bg;
	int              us;       /* underscore color */
	u_int            link;     /* hyperlink id */
};

struct grid_line {
	struct grid_cell_entry *celldata;
	u_int                   cellused;
	u_int                   cellsize;
	struct grid_extd_entry *extddata;  /* RGB/wide char overflow */
	u_int                   extdsize;
	int                     flags;     /* GRID_LINE_WRAPPED 等 */
	time_t                  time;
};

struct grid {
	int               flags;
	u_int             sx, sy;      /* visible dimensions */
	u_int             hscrolled;   /* scroll position */
	u_int             hsize;       /* history line count */
	u_int             hlimit;      /* history limit */
	struct grid_line *linedata;    /* 動的配列 (linked list ではない) */
};

struct screen {
	char             *title;
	struct grid      *grid;          /* primary */
	u_int             cx, cy;        /* cursor */
	u_int             rupper, rlower;/* scroll region */
	int               mode;
	struct grid      *saved_grid;    /* alt screen 用 */
	struct grid_cell  saved_cell;
	int               saved_flags;
	bitstr_t         *tabs;
	struct hyperlinks*hyperlinks;
};
```

**ポイント**:
- `linedata` は **動的配列**。`grid_collect_history()` が history 上限に達した時に古い 10% を free して `memmove` で詰める (ring buffer ではない)。
- cell は **2 種類のストレージ**: `grid_cell_entry` (compact、ASCII + 16色) と `grid_extd_entry` (UTF-8 wide + RGB + hyperlink)。`grid_need_extended_cell` が判定。**メモリ効率優先の二段構え**。
- alt screen は `saved_grid` ポインタで切替。primary に戻る時に grid を swap で復元。

### 4.2 GNU screen (`screen.h`, `image.h`)

```c
struct mchar {
	unsigned char image;   /* char */
	unsigned char mbcs;    /* multi-byte */
	unsigned char attr;    /* bold/underline etc */
	unsigned char font;    /* charset */
	uint32_t      colorfg;
	uint32_t      colorbg;
};

/* mline は per-column の parallel array:
   image[col], attr[col], font[col], colorfg[col], colorbg[col] */
```

**ポイント**:
- 各 cell の field が**別 array に置かれる** (parallel array)。color 比較等で SIMD-friendly。
- color は 32-bit で `0x01000000` (16色), `0x020000xx` (256色), `0x04xxxxxx` (truecolor) を encoding。

### 4.3 dvtm (`vt.c`)

```c
typedef struct {
	wchar_t text;
	attr_t  attr;     /* ncurses attributes */
	short   fg;
	short   bg;
} Cell;

typedef struct {
	Cell    *cells;
	unsigned dirty:1;
} Row;

typedef struct {
	Row     *lines;          /* viewport (visible) */
	Row     *scroll_buf;     /* scrollback ring buffer */
	int      scroll_size;    /* capacity */
	int      scroll_index;   /* current write position */
	int      scroll_above;
	int      scroll_below;
} Buffer;

struct Vt {
	Buffer  buffer_normal;
	Buffer  buffer_alternate;
	Buffer *buffer;          /* 現在のアクティブ */
};
```

**ポイント**:
- **scrollback は ring buffer**: `scroll_index` が循環し、上限到達後は最古行を上書き。tmux の "上限到達で memmove" より GC コストが低い。
- **`Row.dirty:1` ビット**で per-row dirty 管理。redraw 時は dirty 行のみ。
- **primary/alt は 2 つの独立 Buffer struct + ポインタ切替**。コードが極めて直接的。

### 4.4 hyoui 設計指針

| 観点 | 推奨 |
|---|---|
| cell 構造 | `(char, attr_bitmask, fg, bg)` の基本 4 field。UTF-8 grapheme cluster は `Vec<char>` で sub-allocation する。tmux 風 compact/extended の二段は最適化として後付け。 |
| line 構造 | 各 row に `cells: Vec<Cell>` + `wrapped: bool` flag。dvtm の `dirty: bool` も付ける (= redraw 時に diff を絞れる)。 |
| scrollback | **ring buffer** (dvtm style)。`VecDeque<Row>` + capacity 制限が Rust 的に最も自然。 |
| alt screen | **2 つの独立 Grid struct** (dvtm style)。tmux の `saved_grid` ポインタ swap よりも Rust の所有権モデルで直接的。 |
| 動的拡張 | resize 時は新サイズの grid を確保して reflow で詰める。`linedata` 直接 realloc は Rust 的でない。 |

---

## 5. IPC protocol 比較

### 5.1 tmux: OpenBSD imsg

- **framing**: imsg ライブラリが自動 (length-prefix + type + protocol version + peer id)
- **fd passing**: `imsgbuf_allow_fdpass` 有効化、`imsg_compose(ibuf, type, PROTOCOL_VERSION, -1, fd, vp, len)` で fd を渡せる
- **version check**: `version = imsg->hdr.peerid & 0xff;` で PROTOCOL_VERSION (=8) を確認、不一致は PEER_BAD
- **message 種別** (`tmux-protocol.h`):
  ```c
  enum msgtype {
      MSG_VERSION = 12,
      MSG_IDENTIFY_FLAGS = 100, MSG_IDENTIFY_TERM, MSG_IDENTIFY_TTYNAME, ...
      MSG_IDENTIFY_DONE,
      MSG_COMMAND = 200, MSG_DETACH, MSG_EXIT, MSG_EXITED, MSG_LOCK, MSG_READY,
      MSG_RESIZE, MSG_SHELL, MSG_SHUTDOWN, MSG_SUSPEND, MSG_UNLOCK, MSG_WAKEUP,
      MSG_READ_OPEN = 300, MSG_READ, MSG_READ_DONE, MSG_WRITE_OPEN, MSG_WRITE,
      MSG_WRITE_READY, MSG_WRITE_CLOSE, MSG_READ_CANCEL,
  };
  ```
- **attach flow**: client → server に `MSG_IDENTIFY_FLAGS` → `MSG_IDENTIFY_TERM` → ... → `MSG_IDENTIFY_DONE` の sequence で client 情報を送信。完了後 server から `MSG_READY` を返す。

### 5.2 abduco: 固定サイズ Packet

```c
typedef struct {
    uint32_t type;
    uint32_t len;
    union {
        char     msg[4096 - 2*sizeof(uint32_t)];
        struct { uint16_t rows, cols; } ws;
        uint32_t i;
        uint64_t l;
    } u;
} Packet;

enum PacketType {
    MSG_CONTENT = 0,  // PTY I/O
    MSG_ATTACH  = 1,
    MSG_DETACH  = 2,
    MSG_RESIZE  = 3,
    MSG_EXIT    = 4,
    MSG_PID     = 5,
};
```

- **packet size 固定 4KB**。PTY 出力は `MSG_CONTENT` で 4080 byte ずつ。
- helper は `read_all` / `write_all` (EAGAIN/EINTR ハンドリングのみ)。
- **fd passing なし**。
- 極めて minimal。**自前 framing reference として理想的なシンプルさ**。

### 5.3 GNU screen: Message + SIGCONT

- `struct Message` (type + union)、Unix socket で送受信
- `recvmsg()` で **fd passing** あり (client の TTY を server に渡す)
- **SIGCONT による補助シグナリング**: client が `MSG_ATTACH` を送って `pause()` → server 処理完了で `SIGCONT` 送信 → client が `pause()` から起きて attach 状態へ
- 古い (1987-) ので idiom が古いが、socket + signal の組合せ自体は今でも参考になる

### 5.4 認証 / 排他

| 実装 | 認可手段 |
|---|---|
| tmux | socket ファイルの permission (`server_create_socket` で umask 適用、`server_update_socket` で attach 数に応じて exec bit を出し入れ) + ACL (`server_acl_join`) |
| abduco | socket ファイルの permission のみ |
| GNU screen | SOCKMODE macro で multi-user bit を制御。`(mode & 0677) != 0601` で multi-attach socket と single-user socket を識別 |

### 5.5 hyoui への指針

- **framing**: 自前で書くなら abduco Packet 模倣がシンプルで安全 (固定サイズ or length-prefix)。Rust なら `bincode` + length-prefix で十分。`bytes` crate の `Bytes`/`BytesMut` で zero-copy。
- **fd passing**: PTY master を client に渡す必要がない hyoui 設計 (= raw bytes broadcast ではなく ANSI redraw stream) なら fd passing 不要。
- **version check**: 最初に `MSG_VERSION` 相当を交換して mismatch なら拒否 (tmux pattern)。
- **socket permission**: tmux 流に umask で制御。abduco の "permission のみ" で十分 (hyoui は個人利用想定)。
- **SIGCONT 流の補助 signaling は不要**: 現代の async runtime (tokio) なら socket だけで足りる。GNU screen design は signal-driven C 時代の制約。

---

## 6. resize + reflow 戦略の比較

### 6.1 multi-client 時の sx/sy 決定

tmux `resize.c: recalculate_size` (window-size option による分岐):

| mode | 動作 |
|---|---|
| `WINDOW_SIZE_SMALLEST` (default) | 全 client の最小 dimensions (`if (cx < *sx) *sx = cx`) |
| `WINDOW_SIZE_LARGEST` | 全 client の最大 (`if (cx > *sx) *sx = cx`) |
| `WINDOW_SIZE_MANUAL` | `w->manual_sx, w->manual_sy` 固定 |
| `WINDOW_SIZE_LATEST` | 最後に attach した client のサイズ |

**hyoui への指針**: まず `smallest` 固定で実装、後で config option として 4 モード化。

### 6.2 reflow algorithm (tmux `grid.c`)

**core**: wrap 済み行を **logical line に再結合** → 新 width で再分割。

```
grid_reflow_dead   - 処理済み source 行を GRID_LINE_DEAD で marking
grid_reflow_add    - 新行を destination grid に拡張
grid_reflow_move   - source 行を destination に移動 (split/join 不要時)
grid_reflow_split  - 1 行が新 width より広い → cell 単位で分割
grid_reflow_join   - GRID_LINE_WRAPPED が立っている → 次行と結合
```

**GRID_LINE_WRAPPED flag**: 「この行は次行に折り返している」を示す。reflow 時は join 起点になる。**未折り返し行は独立 logical line として扱う**。

**cursor 位置**: reflow 中は **追跡しない**。代わりに `grid_wrap_position` で reflow 前に「論理行番号 + 論理列番号」に変換しておき、reflow 後に `grid_unwrap_position` で新座標に戻す。

### 6.3 scrollback の reflow

tmux は **scrollback も reflow 対象**。visible 領域 + history を一体として再 wrap する (= ユーザが過去ログを scroll back した時にも新 width で表示できる)。

dvtm の ring buffer scrollback は reflow しない (= 過去行は元の wrap のまま) — 軽量実装の trade-off。

### 6.4 hyoui への指針

- reflow algorithm: **tmux pattern (wrap join → width split + GRID_LINE_WRAPPED flag)** を直輸入。
- cursor: 同じく `wrap_position` / `unwrap_position` 二段変換。
- scrollback reflow: 最初は **やらない** (= dvtm pattern)。後で要求が出たら tmux pattern に拡張。

---

## 7. failure path の対応

### 7.1 child PTY 死亡時

**tmux `window.c: window_pane_destroy`**:

```c
if (wp->fd != -1) {
    bufferevent_free(wp->event);
    close(wp->fd);
}
if (wp->ictx != NULL)
    input_free(wp->ictx);
```

destruction 前に `window_pane_destroy_ready` で **pending data check**:

```c
if (wp->pipe_fd != -1 && EVBUFFER_LENGTH(wp->pipe_event->output) != 0) return (0);
if (ioctl(wp->fd, FIONREAD, &n) != -1 && n > 0) return (0);
```

→ **in-flight bytes が残っている間は pane を破棄しない**。input parser state (`ictx`) は最終的に free される (= 残った partial sequence は捨てる)。

**abduco**: `server_pty_died_handler` で `waitpid(-1, &exit_status, WNOHANG)` ループ。`server.running = false` で main loop が exit packet 送信を経て shutdown。

### 7.2 client 異常切断

- tmux: socket EOF を bufferevent が検出 → `server_client_lost` で cleanup
- abduco: `STATE_DISCONNECTED` 状態にして次の sweep で linked list から外す
- GNU screen: SIGCONT 待ち中の client 切断は backend が定期的に検出

### 7.3 protocol version mismatch

- tmux: `version = imsg->hdr.peerid & 0xff; if (version != PROTOCOL_VERSION) /* PEER_BAD */`
- abduco: なし (= 同じ binary version 前提)
- GNU screen: Message struct 内に version field

### 7.4 hyoui への指針

- **child 死亡時の in-flight bytes**: tmux pattern で「FIONREAD で残量を確認、残っている間は破棄を遅延」する。最終的に partial sequence は捨てる (parser context free)。
- **protocol version**: tmux pattern で最初の handshake で確認。breaking change 時は decimal を bump。
- **client 異常切断**: tokio の socket close 検出で session には触らず該当 client struct のみ drop。

---

## 8. hyoui に応用すべきパターン (DR-0013 引用用、優先度順)

### P0 (= 設計の根幹、必須)

**Pattern 1: daemon が screen emulator 正本を持つ (tmux/screen/dvtm 全採用)**

- 子の出力を逐次 DEC parser に通して daemon 側 grid に書く。
- client は grid から生成された ANSI byte 列だけを受け取る。
- **「子に redraw を頼む」発想 (abduco style) は捨てる**。これが hyoui 現状の attach 課題の根本原因。

**Pattern 2: DEC Williams 状態機械で input parser を組む (tmux `input.c`)**

- state enum: `ground, esc_enter, esc_intermediate, csi_enter, csi_parameter, csi_intermediate, csi_ignore, dcs_*, osc_string, apc_string`
- 部分 sequence 用 buffer: `interm_buf[4]`, `param_buf[64]`, dynamic `input_buf`
- 終端 byte (`ST`/`BEL`/final char) が来るまで buffer に貯める
- timeout: 5 秒で stalled sequence を reset
- **これにより detach 時に in-flight bytes を flush する必要がなくなる**。state machine が自然に貯めて attach 後に続行する。Rust なら `vte` crate がそのまま使える。

**Pattern 3: attach 時の redraw は "flag set → 次の tick で grid から再構築"**

- attach 完了で `CLIENT_ALLREDRAWFLAGS` (全 bit OR) を立てる
- 次の render tick で grid を頭から舐めて ANSI byte を socket に流す
- **describe では `set_session` で flag は立てない (= tmux は recalculate_sizes 経由で set)**。hyoui は attach 完了時に明示的に立てる方が分かりやすい。

**Pattern 4: primary/alt の 2 つの独立 grid (dvtm style)**

- `Grid` struct を 2 つ持ち、`active: &mut Grid` で切替
- DECSET 1049 / 1047 / 47 で切替
- **tmux の `saved_grid` ポインタ swap よりも Rust の所有権モデルで直接的**

### P1 (= 主要機能、ほぼ必須)

**Pattern 5: cell の attribute SGR は前 cell との差分のみ出力 (tmux `tty.c`)**

- redraw ループで `last_cell` をキャッシュ
- 次 cell が同 attr なら SGR 省略、文字 byte だけ出す
- 大幅な byte 数削減 (full screen redraw でも数 KB)

**Pattern 6: scrollback は ring buffer (dvtm style)**

- `VecDeque<Row>` + capacity 制限
- 上限到達時は最古行を自動 drop (memmove 不要)
- tmux の "上限到達時 10% を memmove" よりも単純で Rust 的

**Pattern 7: multi-client resize は smallest 戦略 default (tmux `WINDOW_SIZE_SMALLEST`)**

- attach 中の全 client の min(sx, sy) を採用
- 設定で `smallest`/`largest`/`manual`/`latest` を選べるようにするのは後付け
- size 変更時は **全 client に同じ grid を送る** (= 個別 viewport は持たない)

**Pattern 8: reflow は GRID_LINE_WRAPPED flag + join→split (tmux `grid.c`)**

- 各 row に `wrapped: bool` flag
- resize 時に wrap chain を logical line に結合 → 新 width で再分割
- cursor は `wrap_position` / `unwrap_position` で論理座標経由
- scrollback の reflow は最初はスキップ (= dvtm pattern)

### P2 (= 安定性、好ましい)

**Pattern 9: client 側で alt screen を被せる (abduco pattern)**

- client 起動時に `\033[?1049h\033[H` を出力
- detach 時に `\033[?25h\033[?1049l` で復元
- **hyoui daemon が alt screen を grid として持つこととは独立した、client 側の "ユーザの本来の terminal を汚さない" 工夫**。両方やる価値がある。

**Pattern 10: child PTY 死亡時の delayed destroy (tmux `window_pane_destroy_ready`)**

- `FIONREAD` で残 byte をチェック、残っている間は destroy を遅延
- 最終的に partial sequence は捨てる
- session を即座に消さず last output を grid に確実に反映してから notify

**Pattern 11: protocol version check (tmux)**

- handshake で `MSG_VERSION` を交換
- mismatch なら client を拒否
- breaking change 時は version 番号 bump

**Pattern 12: socket permission で認可 (tmux `server_update_socket`)**

- session attached 数で exec bit を出し入れ (= "attached 中" を権限で表現)
- $XDG_RUNTIME_DIR 直下に socket を置けば自然に user 限定

### P3 (= 派生機能、優先度低)

**Pattern 13: scrollback の reflow (tmux full reflow)** — 後付け
**Pattern 14: cell の compact/extended 二段ストレージ (tmux `grid_cell_entry` + `grid_extd_entry`)** — メモリ最適化
**Pattern 15: fd passing で PTY master を client に渡す (tmux imsg)** — hyoui の設計では不要
**Pattern 16: GNU screen 流の SIGCONT 補助 signaling** — async runtime があれば不要
**Pattern 17: 行ごとの dirty flag (dvtm `Row.dirty`)** — diff redraw 最適化

---

## 9. 採るべきでない pattern (旧時代的・hyoui に合わない)

### 9.1 abduco の "state を持たない broadcast" モデル

- screen emulator なし、raw bytes を全 client に流すだけ
- attach 時に子に SIGWINCH を送って「再描画してね」と頼む
- → **hyoui 現状の attach 課題 (子が新 client を知らない、部分 redraw のみ) と同じ問題が再現**
- 採用するなら "detach のみで attach 復元不要" という scope に限定が必要。hyoui は逆方向。

### 9.2 GNU screen の SIGCONT 補助 signaling

- attacher が `MSG_ATTACH` を送って `pause()` → backend が SIGCONT で起こす
- signal-driven C 時代の idiom。tokio + async socket で済む現代では複雑化要因。
- 採用するならログ調査の複雑化、signal safety の neckache を抱える。

### 9.3 tmux の動的配列 + memmove scrollback (`grid_collect_history`)

- linedata は `realloc + memmove` で詰める
- C 時代の選択。Rust では `VecDeque` ring buffer の方が自然 (dvtm pattern)。

### 9.4 GNU screen の parallel array cell storage

- `image[col]`, `attr[col]`, `font[col]` 等を別 array に置く
- SIMD-friendly な反面、Rust の所有権モデルでは struct-of-arrays より array-of-structs (= `Vec<Cell>`) の方が borrow check が楽。

### 9.5 client 側に terminfo 解釈を寄せる (tmux `MSG_IDENTIFY_TERMINFO`)

- client が自分の terminal capability を string で送り、server がそれを使って ANSI を生成
- 高機能だが実装重い。**hyoui は xterm-256color 固定前提 (= ECMA-48 SGR + 256/RGB)** で始めて、必要になったら拡張で十分。

### 9.6 子 PTY からの byte を client に live forward する (abduco style)

- daemon が DEC parser に通さず、socket に流すだけ
- attach 時の状態復元が不可能になる
- hyoui の grid-based redraw 設計と根本的に非互換

---

## 10. 主要引用元 URL 一覧

- tmux:
  - https://github.com/tmux/tmux
  - https://raw.githubusercontent.com/tmux/tmux/master/tmux.h
  - https://raw.githubusercontent.com/tmux/tmux/master/tmux-protocol.h
  - https://raw.githubusercontent.com/tmux/tmux/master/server-client.c
  - https://raw.githubusercontent.com/tmux/tmux/master/server-fn.c
  - https://raw.githubusercontent.com/tmux/tmux/master/server.c
  - https://raw.githubusercontent.com/tmux/tmux/master/client.c
  - https://raw.githubusercontent.com/tmux/tmux/master/proc.c
  - https://raw.githubusercontent.com/tmux/tmux/master/grid.c
  - https://raw.githubusercontent.com/tmux/tmux/master/screen-redraw.c
  - https://raw.githubusercontent.com/tmux/tmux/master/screen-write.c
  - https://raw.githubusercontent.com/tmux/tmux/master/tty.c
  - https://raw.githubusercontent.com/tmux/tmux/master/input.c
  - https://raw.githubusercontent.com/tmux/tmux/master/resize.c
  - https://raw.githubusercontent.com/tmux/tmux/master/window.c
  - https://raw.githubusercontent.com/tmux/tmux/master/cmd-attach-session.c
- abduco:
  - https://github.com/martanne/abduco
  - https://raw.githubusercontent.com/martanne/abduco/master/abduco.c
  - https://raw.githubusercontent.com/martanne/abduco/master/server.c
  - https://raw.githubusercontent.com/martanne/abduco/master/client.c
- GNU screen:
  - https://cgit.git.savannah.gnu.org/cgit/screen.git/tree/src/attacher.c
  - https://cgit.git.savannah.gnu.org/cgit/screen.git/tree/src/screen.c
  - https://cgit.git.savannah.gnu.org/cgit/screen.git/tree/src/socket.c
  - https://cgit.git.savannah.gnu.org/cgit/screen.git/tree/src/display.c
- dvtm:
  - https://github.com/martanne/dvtm
  - https://raw.githubusercontent.com/martanne/dvtm/master/dvtm.c
  - https://raw.githubusercontent.com/martanne/dvtm/master/vt.c

---

## 補遺: hyoui の現状課題と本研究の対応

handoff doc (`docs/journal/2026-05-27-screen-emulator-pivot-handoff.md`) 記載の課題への対応マッピング:

| 課題 | 該当パターン |
|---|---|
| attach socket は通るが client terminal に画面が再現されない | Pattern 1 (daemon が grid 正本) + Pattern 3 (flag set → next tick で redraw) |
| 子 (claude TUI) は新 attach client を知らず redraw しない | **そもそも子に知らせる必要がない**。Pattern 1 で daemon 側 grid から ANSI を再生成して client に送る (= 子は無関係) |
| resize 通知のみでは部分 redraw しか起きない | Pattern 3 (full redraw flag set)。resize trigger に頼らない |
| client が入力すると部分メッセージだけが流入、画面崩壊 | Pattern 1+2 で grid と input parser が daemon に揃えば、client は grid 由来の完全 byte stream を受け取るので部分メッセージ問題は消える |
| wait pattern が誤マッチ (scrollback に過去描画が混じる) | Pattern 6 (ring buffer scrollback) で過去 frame は scrollback に押し出され、現在 viewport は分離される。wait pattern は viewport (= visible grid) だけにマッチさせる設計を取れば誤マッチしない |
| in-flight bytes flush 問題 | Pattern 2 (DEC state machine) で **flush 自体が不要になる**。state machine 内部 buffer に partial sequence が貯まり、次 byte が来るまで dispatch されないので、detach/attach をまたいでも自然に継続 |

