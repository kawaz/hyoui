# daemon が子の出力から取れる OSC の範囲 (vt100 0.16.2)

web UI 作り直し (`docs/issue/2026-10-04-design-webui-terminal-app-rework.md`) で、daemon が子の出力から OSC 0 / 1 / 2 / 7 / 133 / 9 / 777 / 9;4 / 52 を観測して session 属性にしたい。その前提として、daemon が使う vt100 がこれらをどう扱うかをソースで確認した。

記法: 「事実」はソース読解で確認したもの、「未確認」は実機・実行で見ていないもの。本調査は全て静的読解で、実行検証はしていない。行番号は調査時点 (2026-10-04、作業コピー) のもの。vt100 のパスは crate ルートからの相対 (`vt100-0.16.2/src/...`)。

## 版

- `Cargo.lock` の vt100 は 0.16.2、その下の vte は 0.15.0 (どちらも crates.io)。workspace は `Cargo.toml:35` で `vt100 = "0.16"`、使うのは `crates/hyoui/Cargo.toml:25` のみ

## 1. vt100 の OSC dispatch (事実)

OSC の dispatch は `vt100-0.16.2/src/perform.rs:198` の `osc_dispatch(params, _bel_terminated)` ただ 1 箇所。`params` は vte が `;` で分割した `&[&[u8]]` で、`match` は引数の個数まで含めて固定パターンで書かれている。`bel_terminated` 引数は受け取って捨てている (`_` 付き)。

| OSC | 保持 (Screen の API で取れる) | callback で通知 | 捨てる / unhandled | 根拠 |
|---|---|---|---|---|
| 0 (`0;<s>`) | しない | `set_window_icon_name` と `set_window_title` の両方 | | `perform.rs:200-203` |
| 1 (`1;<s>`) | しない | `set_window_icon_name` | | `perform.rs:204-206` |
| 2 (`2;<s>`) | しない | `set_window_title` | | `perform.rs:207-209` |
| 52 (`52;<ty>;<data>`) | しない | data が `?` なら `paste_from_clipboard(ty)`、base64 なら `copy_to_clipboard(ty, data)` (data は base64 のまま) | ty が `cpqs01234567` 以外の文字を含む / data が base64 でも `?` でもない場合は `unhandled_osc` | `perform.rs:210-232`、定数は `perform.rs:1-3` |
| 7 | しない | しない | `unhandled_osc` | `perform.rs:234-236` (`_` 腕) |
| 133 | しない | しない | `unhandled_osc` | 同上 |
| 9 / 777 / 9;4 | しない | しない | `unhandled_osc` | 同上 |
| 8 (参考) | しない | しない | `unhandled_osc` | 同上 |

- title / icon name を保持する field も取得 API も無い。`Screen` (`screen.rs`) と `lib.rs` に `title` / `icon` を含む識別子は 0 件 (grep で確認)。値を取る唯一の経路は callback
- callback は `vt100-0.16.2/src/callbacks.rs` の `Callbacks` trait。title は `set_window_title(&mut Screen, &[u8])` (`callbacks.rs:23`)、icon name は `set_window_icon_name(&mut Screen, &[u8])` (`callbacks.rs:15`)、クリップボードは `copy_to_clipboard(&mut Screen, ty: &[u8], data: &[u8])` / `paste_from_clipboard(&mut Screen, ty: &[u8])`、それ以外は `unhandled_osc(&mut Screen, params: &[&[u8]])` (`callbacks.rs:66`)。全メソッドに空の default 実装があり、`impl Callbacks for ()` がある (`callbacks.rs` 末尾)
- 値は生の `&[u8]` (UTF-8 とは限らない)
- callback を渡す入口は `Parser::new_with_callbacks` (`parser.rs:29`)、`Parser::new` は `()` を渡す。callback 側の状態は `Parser::callbacks()` 等で参照できる (`parser.rs:68`)
- 引数の個数に依存する点 (事実、vte の挙動と合わせた読み): vte は `;` で OSC を分割し、最大 16 params を超える分は読み捨てる (`vte-0.15.0/src/lib.rs:45` の `MAX_OSC_PARAMS = 16`、`:529-530`)。vt100 の `[b"0", s]` は 2 要素ちょうどにしか一致しないので、title 文字列自体に `;` が含まれると `0;a;b` は 3 要素になり title callback に行かず `unhandled_osc` に落ちる。**この落ち方は読解からの推論で、実行では未確認**
- `unhandled_osc` に来る OSC 7 / 133 / 9 / 777 / 8 は、`params` の形で受け取れる (例: `[b"7", b"file://host/path"]`、`[b"133", b"A"]`、`[b"9", b"4", b"1", b"50"]`)。URI 内の `;` もそのまま分割されるので、受け側で結合し直す必要がある。これも読解からの推論で未確認

## 2. hyoui daemon 側の現状 (事実)

- vt100 に渡す層は 1 つだけ: `crates/hyoui/src/daemon/screen/state.rs` の `ScreenState`。フィールド `parser: vt100::Parser` (`state.rs:42`)。型パラメータ省略なので callback は `()` で、**callback は使っていない**
- 生成は `vt100::Parser::new(rows, cols, scrollback_len)` が 2 箇所: 初期化 `state.rs:95`、resize 時の組み直し `state.rs:250`。`new_with_callbacks` の呼び出しは hyoui 内に 0 件
- bytes の流れ: `ScreenState::process` (`state.rs:112`) が `self.parser.process(bytes)` (`state.rs:116`) を呼ぶ。その直後に自前で見るのは DEC sync update (`\x1b[?2026h/l`) の chunk 跨ぎ走査 `update_sync_flag_with_carry` (`state.rs:119`、定義は `:450`、carry 長 `SYNC_SCAN_CARRY_LEN = 7` が `state.rs:33`) と alt screen の変化のみ。OSC を自前で観測する層は無い
- 自前 scan の前例: 上記の sync 走査は、vt100 が内部処理しない sequence を parser の外側で bytes から検出する実装になっている (`state.rs` のコメントにも「vt100 は本 mode を内部処理しないため」とある)。OSC 用の同種の層は未実装
- resize 時は新 Parser に `input_log` を replay する (`state.rs:246-258`、alt 中なら `\x1b[?1049h` を先に流す)。callback を導入すると、replay された bytes に含まれる OSC で callback が再発火する (読解からの推論、未確認)。`input_log` は primary buffer 中の bytes のみ (alt 中は push しない)
- 別の ANSI 処理として `crates/hyoui/src/strip.rs` (CSI / OSC / DCS を strip して plain text 化、OSC は BEL / ST 終端、chunk 跨ぎ carry あり) があり、tail (`daemon/tail.rs:84`) と broadcast (`daemon/broadcast.rs:397`) が使う。これは OSC を捨てる側で、取り出す側ではない
- `ScreenState` の `screen()` (`state.rs:375`) が `&vt100::Screen` を返し、外から使うのは cell / cursor / mode / scrollback など。title 系は存在しない (上記 API 不在)

## 3. 子の cwd の現状 (事実)

- session 一覧の `cwd` の出どころは **`hyoui run` を叩いた時点の cwd** (起動時 cwd)。`crates/hyoui-cli/src/daemonize.rs:479-486` が daemon の `chdir("/")` の直前に `std::env::current_dir()` を読み (失敗時は `/`)、`DaemonConfig::cwd` (`daemon/config.rs:172`) に格納
- 子 PTY は `Pty::spawn(..., config.cwd.as_deref())` (`daemon/session.rs:330`) でその cwd に chdir してから exec される (`session.rs:325-329` のコメント)。つまり起動時には「子の cwd = 一覧の cwd」
- `status.response` の `cwd` field (`protocol/messages/status.rs:156`、必須 String) は `daemon/control.rs:928-932` で `config.cwd` を文字列化して載せる (None なら `/`)。discovery 側は `crates/hyoui/src/discovery.rs:82` / `:383` で `sr.cwd` を `cwd` に写す。`hyoui list` の表示は `crates/hyoui-cli/src/main.rs:1551` の `shorten_cwd`、web 側は `crates/hyoui-web/src/lib.rs:216` で JSON に出す
- 起動後に子が `cd` した現在 cwd を OS から読む処理は **無い**: `proc_pidinfo` を使うのは `crates/hyoui/src/sys/procstate.rs:23-29` の `PROC_PIDTBSDINFO` (停止状態の判定 `is_stopped`) と、Linux の `/proc/<pid>/stat` (`procstate.rs:49`) のみ。`PROC_PIDVNODEPATHINFO` / `/proc/<pid>/cwd` / `getcwd` 等の現在 cwd 取得は grep で 0 件
- 未確認: 子が fork した孫 (shell が起動した前景ジョブ) の cwd を取りたい場合に pid をどう決めるか (pty の前景 pgrp を使う手段は `tcgetpgrp` が候補だが、hyoui 内で `tcgetpgrp` の使用は grep で見つからなかった)

## 4. OSC ごとに「取るには何が要るか」

分類: (A) vt100 の API だけで足りる / (B) `Callbacks` 実装が要る / (C) vt100 の前段で bytes を自前 parse する層が要る。7 / 133 / 9 / 777 / 9;4 は (B) の `unhandled_osc` でも受けられるので、(B) と (C) の両方を併記する。

| OSC | 区分 | 内容 |
|---|---|---|
| 0 / 1 / 2 (title, icon name) | B | `set_window_title` / `set_window_icon_name` の実装が要る。Screen から取る API は無い (A は不可)。値は `&[u8]`。`;` を含む title は `unhandled_osc` に落ちる (推論、未確認) ので、title の欠落を避けるなら `unhandled_osc` 側で `[b"0"\|b"1"\|b"2", rest...]` を結合し直す処理も要る。C でも取れる |
| 7 (cwd) | B または C | `unhandled_osc` で `[b"7", uri]` を受ける (B)。または自前 scan (C)。vt100 の既存 API では取れない。OS から現在 cwd を読む経路 (第 3 節) は別手段で、hyoui には未実装 |
| 133 (shell integration) | B または C | `unhandled_osc` で `[b"133", b"A"/b"B"/b"C"/b"D", ...]` を受ける (B)。`;` で追加 param が付く形 (`D;<exit>` 等) も params に分割されて届く。vt100 は cursor 位置 (行) と結び付けて保持しない。prompt の画面上の行を使いたい場合は callback 時点の `Screen::cursor_position()` を callback 内で読む必要がある (callback は `&mut Screen` を受け取る) |
| 9 / 777 (通知) | B または C | `unhandled_osc` で `[b"9", msg]` / `[b"777", b"notify", title, body]` を受ける (B)。vt100 は通知として解釈しない |
| 9;4 (進捗) | B または C | `unhandled_osc` で `[b"9", b"4", state, value]` を受ける (B)。OSC 9 の通知と同じ番号なので、第 2 param が `4` かで振り分ける必要がある (`[b"9", msg]` の msg がたまたま `4` の場合との区別は param 個数で決まる) |
| 52 (クリップボード) | B | `copy_to_clipboard(ty, data)` / `paste_from_clipboard(ty)` を実装する。data は base64 のまま渡る。不正な ty / data は `unhandled_osc` に来る。vt100 自身は clipboard を保持しない |
| 8 (参考、既知問題) | C (既存 issue 参照) | `unhandled_osc` で params は受けられるが、attach 復元の欠落 (`docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md`) は cell 側に urlId を持たせる必要があり、callback だけでは足りない。本調査の範囲外 |

共通の注意点 (事実または推論を明記):

- (B) を使うには `vt100::Parser::new` を `new_with_callbacks` に変える必要があり、`ScreenState.parser` の型が `Parser<()>` から変わる。`state.rs:42` / `:95` / `:250` と、型名 `vt100::Parser` が出る箇所 (grep では `state.rs` のみ) が影響範囲 (事実)
- callback は `ScreenState::process` の呼び出しの中で同期的に発火する。callback 内で得た値は callback 側の構造体に溜め、`process` の後で hyoui が取り出す形になる (`Parser::callbacks()` 経由、`parser.rs:68`、推論)
- resize の replay で再発火する点 (第 2 節) は、同じ OSC を二重に計上する可能性として設計側で考慮が要る (推論、未確認)
- (C) の自前 scan は、`state.rs` の sync scan と同様に chunk 跨ぎの carry が要る (OSC は BEL / ST 終端でどこでも分割され得る)。`strip.rs` に OSC の carry 付き走査の実装がある
- 実機での確認 (実際に vt100 へ各 OSC を食わせて callback が呼ばれる / 呼ばれない) は未実施。上記は全てソース読解

## 未確認 (まとめ)

- `;` を含む title が `unhandled_osc` に落ちること、ST (`ESC \`) 終端と BEL 終端での params の差 (vt100 は `bel_terminated` を無視する)、16 param を超える OSC の挙動は、実行で見ていない
- resize replay での callback 再発火は実行で見ていない
- 子の孫プロセス (前景ジョブ) の現在 cwd を OS から取る手段は hyoui に無く、候補 (`tcgetpgrp` + `proc_pidinfo(PROC_PIDVNODEPATHINFO)` / `/proc/<pid>/cwd`) は調べていない
