# tty / PTY 関連テスト戦略の先行プロダクト調査 — multiplexer / emulator / PTY library 横断

- Date: 2026-05-27
- 目的: hyoui の DR-0014 §検証主義に基づくマトリクス検証の前段として、**先行する production-grade プロダクトが tty / PTY 関連 (= signal 透過 / Ctrl-Z / job control / screen state / attach-detach) をどうテストしているか**を実装コードから抽出する。表面 API でなく **test directory の主要ファイルを実際に読み**、hyoui に直接 applicable な pattern を pattern 名付きで集約する。
- 対象: tmux (C 古典 multiplexer) / zellij (Rust multiplexer) / wezterm (Rust GUI + multiplexer) / alacritty (Rust emulator) / abduco (minimal detach) / pexpect (Python automation) / rexpect (Rust automation) / expectrl (Rust automation) / pty-process (Rust 低 level) / asciinema (recording)
- 引用方針: 各プロダクトの test ファイルパスを URL ベース (`github.com/<owner>/<repo>/blob/<branch>/<path>`) で明示。コードは「観察された pattern を抽象化」する形で要約する。

---

## 1. 要約 (= 結論先出し)

### 1.1 各プロダクトの test 戦略の性格 (1-2 行)

- **tmux**: `regress/*.sh` の **shell script ハーネス**。`-Ltest` で test-only socket に分離し、`new -d` で daemon 起動 → `command` / `send-keys` / `capturep` で操作・観察 → `cmp` / `diff` で期待値比較。実 PTY + shell 駆動の **black-box 検証**。`Makefile.NOTPARALLEL` で逐次実行 (= socket 共有衝突回避)。
- **zellij**: 三層構造。`src/unit/*_tests.rs` で **`MockOsApi` / `FakeInputOutput` trait 実装** による in-process unit test (= 実 PTY 不要)、`src/unit/screen_tests.rs` で **insta snapshot + 自前 vte parser で grid 再構成** による visual regression、`src/tests/e2e/` で **SSH + Docker container で実 zellij を回す** e2e。
- **wezterm**: `term/src/test/mod.rs` で **`TestTerm` wrapper** を作り、`term.advance_bytes(...)` で ANSI を流し込んで `assert_cursor_pos` / `assert_dirty_lines` で直接 assert。**実 PTY を一切使わず emulator core 単体テスト**。`k9::assert_equal` 採用。
- **alacritty**: `tests/ref.rs` で **「録画 ANSI byte 列 + 期待 grid (JSON) のペア」をリプレイして grid 一致を確認**する ref test pattern。`Mock` の `EventListener` を渡して emulator を実 GUI から切り離す。
- **abduco**: `testsuite.sh` で **stdin pipe で coordinated key input** を流し込み、attach / detach の output を `diff` する。100% shell + diff のシンプル e2e。
- **pexpect**: `tests/test_ctrl_chars.py` で **`getch.py` という helper を子で実行 → 256 byte 全部送信して `\d+<STOP>` で echo back を expect**。`sendintr` / `sendeof` / `sendcontrol` を中心に **PTY ctrl char の全 byte カバー**。
- **rexpect**: `examples/bash.rs` で **`spawn_bash(timeout) → send_control('z') → wait_for_prompt → execute("bg") / execute("fg")`** を 10 行で完結。bash の job control を実 PTY で完全に黒箱検証する最短経路を提示。
- **expectrl**: `tests/expect.rs` で `spawn("cat") + send_line + expect("...")` の典型 expect スタイル、`tests/interact.rs` で `set_input_action` / `set_output_action` の callback ベース interactive 検証。
- **pty-process**: `tests/winch.rs` で **`perl -E '$SIG{WINCH}=sub{say "WINCH"}'` で子に WINCH ハンドラを仕込み、親で `pty.resize()` → output から `"WINCH"` 行を見る**という in-band 検証。`tests/basic.rs` で `cat + write + read + echo back` の素朴 PTY round-trip。
- **asciinema**: `tests/integration.sh` で **`assert_exit_code` / `assert_file_exists` / `assert_file_not_empty` 等の sh 関数を自作 framework** として用意、`casts/` フィクスチャを使って record-replay-output cycle を検証。

### 1.2 hyoui への applicable pattern top 8

優先度順 (= hyoui の DR-0001 軸 1/2 + DR-0014 マトリクス検証で**今すぐ使える**もの)。

1. **`spawn_bash + send_control('z') + wait_for_prompt + bg/fg` パターン (rexpect)** — `hyoui run -- bash` の Ctrl-Z 軸 1/2 検証にそのまま転写可能。実 PTY 経由で job control を黒箱検証する最短経路。
2. **`MockOsApi` trait + in-process unit (zellij)** — `ServerOsApi` 相当の trait を hyoui daemon の OS 境界に置けば、signal 送信 / PTY write / kill を mock した状態で **unit test レベルで invariant (= DR-0001 §invariant) を assert** できる。
3. **「ANSI byte 録画 → 自前 emulator にリプレイ → 期待 grid と比較」ref test (alacritty)** — hyoui の screen state core (DR-0013) の regression test を **PTY を一切使わず** に回せる。録画を fixture として VCS に含める。
4. **`insta snapshot + 自前 vte parser で grid を再構成` (zellij screen_tests)** — hyoui の `hyoui screen dump --format=ansi` 出力 → vt100 で grid → `format!("{:?}", grid)` で文字列化 → `insta::assert_snapshot!` という流れ。**redraw / attach 復元の visual regression に最適**。
5. **`-Ltest` 等の test-only socket / instance 分離 (tmux)** — hyoui の daemon socket は既に session 名で分離されているが、**test ごとに `HYOUI_RUNTIME_DIR=$(mktemp -d)` を export して完全に独立した daemon を立てる**規約を確立する。
6. **race condition を regex 置換で snapshot 正規化 (zellij `account_for_races_in_snapshot`)** — 描画タイミング起因の不安定 cell (= cursor 残骸 / status bar の async load 等) を snapshot 比較前に regex で削る。hyoui の attach 復元 snapshot 比較で必須。
7. **`perl -E '$SIG{WINCH}=sub{say "WINCH"}'` (pty-process)** — 子側 in-band で signal 受信を確認する **helper-less** な技。SIGWINCH 配送マトリクスでまさにこの形で「親が resize → 子が WINCH 受信 → PTY 出力に "WINCH" 行」を 1 行で assert できる。同じ pattern で SIGTERM / SIGHUP 配送も検証可能 (= `$SIG{TERM} = sub{ say "TERM"; exit }`)。
8. **stdin pipe で coordinated key input + diff (abduco)** — TUI を起動して `printf 'c\nc\n \nqq' | sleep 1` を pipe で流し込み output を `diff` する手法。hyoui の lock acquire / wait コマンドの end-to-end smoke test に使える低コスト pattern。

### 1.3 採用推奨 Rust crate

- **`insta`** (snapshot) — zellij が screen / e2e 両方で全面採用。`cargo insta review` で diff を対話確認できる。hyoui の `screen dump` / `screen snapshot` 出力との相性が極めて良い。
- **`rexpect`** (PTY expect) — 軸 1/2 検証で `spawn_bash` + Ctrl-Z + `bg/fg` を **10 行以内**で書ける。CI 上で実 PTY を扱う最短経路。
- **`pty-process`** (低 level PTY) — rexpect では拾えない細部 (= 任意のコマンドを spawn して PTY master 直接読み、winch 配送タイミング測定) を素朴に書ける。`forkpty + fork-exec` の boilerplate を完全に省ける。
- **`vte`** (ANSI parser、既存依存) — zellij / wezterm / alacritty 全て使用。alacritty の ref test pattern (録画リプレイ) で grid 再構成器として使う。
- **k9** (= wezterm が採用、`assert_equal` 系の柔軟な diff 表示) — 任意。`pretty_assertions` で代替可。

### 1.4 CI で PTY が動くかの結論

- **GitHub Actions の `ubuntu-latest` で実 PTY は動く** (確認: zellij e2e / pty-process / rexpect / pexpect が全て GA で実 PTY テストを CI 通過させている)。
- **container 内** (`runs-on: ubuntu-latest` + `container: debian:12`) でも `/dev/ptmx` が見えれば動く (wezterm の `gen_debian12.yml` が同パターンで GUI 含む test を回している)。
- **macOS runner** でも基本動く (`pty-process` は macOS で multiple spawn の挙動が異なるため `cfg(not(target_os="macos"))` で一部 disable していたが、これは PTY が動かないわけではなく macos 固有の挙動差)。
- 注意: **SSH ベース e2e** (zellij が採用) はサービスコンテナを別途立てる必要があるため CI 設定がやや重い。hyoui 初期は不要、まず local + GA の native PTY で十分。

### 1.5 マトリクス検証実装の最初の 3 ステップ提案

1. **`tests/common/` に PTY harness を 1 個作る**: `pty_process` ベースで `spawn_hyoui(args: &[&str]) -> (PtyMaster, ChildHandle)` + `wait_for_pattern(re, timeout)` + `send_keys(bytes)` の最小 helper を整備。**helper 設計を rexpect でなく pty_process 直叩きにする理由**: rexpect は expect API が便利だがプロンプト前提 (= `spawn_bash` 等)、hyoui は任意の child を載せるため低 level の方が hyoui 制御フローに合う。
2. **`tests/matrix/jobcontrol_axis1.rs` を書く**: DR-0001 軸 1 (`follow` / `auto-resume`) のマトリクスを **app × signal × mode** で表現。`app` = `bash -c 'sleep 100'` (= 子 self-SIGTSTP) / `bash -i` (= 内側 shell でジョブ操作) / `python3 -c 'import signal,os; os.kill(os.getpid(), signal.SIGTSTP); print("RESUMED")'` (= 子 self-SIGTSTP + 復帰確認) の最低 3 種類で軸 1 の挙動を asserts。
3. **`tests/screen/` に snapshot test を仕込む**: `hyoui screen dump --format=ansi` の出力を `insta::assert_snapshot!` で保存、`hyoui run` → 一定の入力流し込み → snapshot 撮影、を最小単位として確立。**snapshot 比較前に `account_for_races` 相当の regex 正規化を 1 関数で噛ます** (cursor 行の dirty bit、async status の有無等)。

---

## 2. 各プロダクトの test 戦略 詳細

### 2.1 tmux — shell script `regress/` ハーネス

**ファイル**:
- `https://github.com/tmux/tmux/blob/master/regress/Makefile`
- `https://github.com/tmux/tmux/blob/master/regress/control-client-sanity.sh`
- `https://github.com/tmux/tmux/blob/master/regress/kill-session-process-exit.sh`
- `https://github.com/tmux/tmux/blob/master/regress/tty-keys.sh`
- `https://github.com/tmux/tmux/blob/master/regress/input-keys.sh`
- `https://github.com/tmux/tmux/blob/master/regress/capture-pane-sgr0.sh`
- `https://github.com/tmux/tmux/blob/master/regress/decrqm-sync.sh`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`-L<label>` socket 分離** | 全 test script の冒頭で `TMUX="$TEST_TMUX -Ltest"` と test-only socket label を設定し、`$TMUX kill-server 2>/dev/null` で前回残骸を掃除 |
| **`new -d` daemon spawn + sleep ベース sync** | `$TMUX -f/dev/null new -d -x80 -y24 \|\| exit 1; sleep 1` で daemon 起動 → 一定 sleep で安定化。pure sync mechanism (= file lock / FD signal) は使わない |
| **`send-keys + capturep` round trip** | `$TMUX send-keys -t$W "$key" 'EOL'; $TMUX capturep -pt$W \| head -1 \| sed 's/EOL.*//'` で送ったキーの実際の byte 列を `cat -tv` echo を介して取得。期待値 (例: `^A`) と shell 文字列比較 |
| **`respawnw -k` で子を任意 cmd に差し替え** | `$TMUX respawnw -k -t:0 -- sh -c "printf '\\033[?2026\$p'; dd bs=1 count=11 \| cat -v > $TMP"` で「DECRQM 投げて DECRPM を 11 byte 読む」専用ペインを動的に立てる。**任意の検証コマンドを実 PTY 内で走らせるため pane 再生成を使う** |
| **`kill -0 $P` で子 PID 生存確認** | session kill → `sleep 3` → `kill -0 $P 2>/dev/null && exit 1` で子が確実に死んだことを assert。GID / PGID 確認は明示的にはしない |
| **`$TMUX -C` control mode で client 駆動** | `cat <<EOF \| $TMUX -C a > $TMP` で control-mode client を heredoc 入力で動かし、出力を `cmp -s` で期待値と byte 一致比較 |
| **`Makefile .NOTPARALLEL`** | 全 test 直列実行。socket label を共有するため。test 1 個 → daemon kill → sleep 1 → 次 test の clean room 設計 |

**verdict**: 30 年級の C プロダクトとは思えないほど lean。shell + sleep + diff で **状態空間を一通り回す**。hyoui 初期にそのまま転用可能。

### 2.2 zellij — Rust 三層 (unit / screen / e2e)

**ファイル**:
- `https://github.com/zellij-org/zellij/blob/main/zellij-server/src/unit/os_input_output_tests.rs`
- `https://github.com/zellij-org/zellij/blob/main/zellij-server/src/unit/pty_tests.rs`
- `https://github.com/zellij-org/zellij/blob/main/zellij-server/src/unit/screen_tests.rs`
- `https://github.com/zellij-org/zellij/blob/main/src/tests/e2e/remote_runner.rs`
- `https://github.com/zellij-org/zellij/blob/main/src/tests/e2e/cases.rs`
- `https://github.com/zellij-org/zellij/blob/main/.github/workflows/e2e.yml`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`MockOsApi` / `FakeInputOutput` trait 実装** | `ServerOsApi` trait に対し test 用 mock 実装を提供。`spawn_terminal` / `write_to_tty_stdin` / `kill` / `force_kill` / `send_sigint` / `set_terminal_size_using_terminal_id` / `send_to_client` を全 mock 化。state (= cwd / cmd / sent messages) は `Arc<Mutex<HashMap>>` で観測 |
| **実 process + 実 signal で kill/sigint 検証 (`os_input_output_tests`)** | `long_running_cmd()` (= `Command::new("sleep").arg("60")`) を `spawn` し、`server.kill(pid)` / `server.force_kill(pid)` / `server.send_sigint(pid)` を実プロセスに送り、`thread::sleep(100ms)` で配送猶予を取る。**signal 配送の正確な検出はせず "panic しない / 後続が動く" 程度で済ます** |
| **cross-platform `cfg(not(windows))`** | 全 signal test を unix 限定でガード。windows では `long_running_cmd()` を `timeout /T 60` に差し替え、`CREATE_NO_WINDOW` flag を立てるなどの platform 分岐 |
| **`take_snapshot_and_cursor_coordinates(ansi, &mut grid)` (`screen_tests`)** | server が client に送る `ServerInstruction::Render` の ANSI byte 列を、test 側で `vte::Parser::new()` + `Grid::new(...)` に流し直して **screen state を再構成**。`format!("{:?}", grid)` + `cursor_coordinates()` を `insta::assert_snapshot!` で保存 |
| **SSH コンテナで実 zellij e2e (`remote_runner.rs`)** | `ssh2::Session` で `linuxserver/openssh-server` コンテナへ接続 → `channel.request_pty("xterm", None, Some((cols, rows, 0, 0)))` で PTY 要求 → `channel.shell()` → `channel.write_all("zellij --session e2e-test\n")` で起動 → channel 読み取り byte を `account_for_races_in_snapshot` で正規化 → `insta::assert_snapshot!` |
| **race condition 正規化 (`account_for_races_in_snapshot`)** | 描画 race 起因の不安定領域 (= `"Alt <[]>  BASE\s*\n"` 等の async load 表示) を regex で消す。**snapshot 安定化のための明示的な lossy normalization** |
| **e2e CI service container** | `.github/workflows/e2e.yml` で `services: ssh: image: ghcr.io/linuxserver/openssh-server` を立てて GA 上で SSH PTY を確保。`target/` を `-v` mount で共有 |

**verdict**: hyoui の参考価値が**最大**。daemon + screen state 正本 + protocol という構造が極めて近い。**unit + screen snapshot + e2e の三層構成** をそのまま採用すべき。

### 2.3 wezterm — emulator core 単体 + GUI 統合

**ファイル**:
- `https://github.com/wez/wezterm/blob/main/term/src/test/mod.rs`
- `https://github.com/wez/wezterm/blob/main/term/src/test/c0.rs` `c1.rs` `csi.rs`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`TestTerm` wrapper + `Deref<Target=Terminal>`** | `struct TestTerm { term: Terminal }` を作り `Deref` 実装で透明に `Terminal` 操作 + test 用 helper (`set_mode("?7", true)`, `cup(col, row)`, `erase_in_display(...)`) を生やす。**test fixture を type-safe に組み立てる** |
| **`assert_cursor_pos(x, y, reason, seqno)`** | `(x, y)` 座標 + 任意の `reason: Option<&str>` で **失敗時の debug info を carry**。`SequenceNo` 比較で「最新 seqno 時点でこの位置」を assert |
| **`assert_dirty_lines(seqno, expected, reason)`** | 「seqno X 以降に dirty な行 index 配列」を直接比較。**dirty 追跡の正確性を grid 直叩きで検証** |
| **`Mock` の `Clipboard`** | clipboard を `Mutex<Option<String>>` で carry する fake 実装。**外部依存 (= OS clipboard) を完全に断ち切る** |
| **PTY 不在の emulator core test** | `term.advance_bytes(b"\x1b[31mfoo")` のような **直接 byte 流し込み**。fork/spawn が一切ない。emulator の入出力契約だけを test する |

**verdict**: hyoui の **screen emulator core (DR-0013 vt100 ベース実装)** にそのまま転用。`TestTerm` 相当の wrapper を hyoui 側にも作り、test helper を多数生やすのが筋。

### 2.4 alacritty — ref test (録画リプレイ)

**ファイル**:
- `https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/tests/ref.rs`
- `https://github.com/alacritty/alacritty/tree/master/alacritty_terminal/tests/ref/<name>/`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **macro による test 大量生成** | `ref_tests! { alt_reset clear_underline ... }` という macro で `#[test] fn $name()` を一気に生やす。test 名 = fixture dir 名と一致 |
| **fixture 4 file セット** | `tests/ref/<name>/` に `alacritty.recording` (= ANSI byte 録画) / `size.json` (= TermSize) / `grid.json` (= 期待 grid serialized) / `config.json` の 4 ファイルを置く |
| **`parser.advance(&mut terminal, &recording)` でリプレイ** | recording を ansi parser に流し込み Term を駆動。実 PTY 不要 |
| **`Term` の `grid()` を JSON serialized 期待値と比較** | `serde::Deserialize` で `Grid<Cell>` を読み込み、`assert_eq!(grid, term_grid)`。**grid 全 cell 一致をリプレイで担保** |
| **`Mock: EventListener`** | `send_event(&self, _: Event) {}` の no-op 実装で event 配送を断ち切る |
| **fixture 生成は別 binary で実機操作録画** | `alacritty.recording` は実機 alacritty に `--ref-test` フラグ付与で起動 + 終了時に録画ダンプする (= 別途仕組みあり)。一度作れば永続 |

**verdict**: hyoui の **regression test の主力**にできる。`hyoui` に `--record-bytes <file>` フラグを足し、`hyoui run -- bash` 等で記録した byte 列をリプレイで grid 一致 assert する仕組みは alacritty 流儀で簡素に実装可能。fixture を VCS に含めるか .gitignore で生成するかは別判断。

### 2.5 abduco — minimal shell + diff

**ファイル**:
- `https://github.com/martanne/abduco/blob/master/testsuite.sh`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`check_environment` で前提クリーン** | `[ "\`$ABDUCO \| wc -l\`" -gt 1 ] && exit 1; pgrep abduco && exit 1;` で「test 前に session/process が残っていない」を assert。**clean room** |
| **`expected_*` 関数で期待値を function generate** | `expected_abduco_prolog()` (= `[?1049h[H` ANSI シーケンス) と `expected_abduco_epilog()` を組み合わせて期待 byte 列を build する。**期待値 = ANSI raw byte** で正面比較 |
| **`detach()` を stdin pipe で送る** | `detach() { sleep 1; printf ""; }` のような関数を `detach \| $ABDUCO ...` で pipe 入力。**入力タイミングを sleep で決定** |
| **`sed 's/.$//' \| diff -u` で末端の不安定 byte 除去** | 1 byte 削って `diff -u` する hack で 1 byte 単位の不安定性を吸収 |

**verdict**: hyoui の **bare minimum smoke test** に使える。「hyoui run hello → stdout に 'hello\r\n'」「detach → ls に session が見える」レベルを **shell + diff** で書くなら最速。

### 2.6 pexpect — PTY automation の元祖

**ファイル**:
- `https://github.com/pexpect/pexpect/blob/master/tests/test_ctrl_chars.py`
- `https://github.com/pexpect/pexpect/blob/master/tests/test_expect.py`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`getch.py` helper による echo back 検証** | `child = pexpect.spawn(self.PYTHONBIN + ' getch.py', echo=False, timeout=5)` で「1 byte 読んで `<byte値><STOP>` を出す」helper を子で動かす。親が `child.send(byte(i))` → `child.expect('%d<STOP>' % i)` を 256 回 ループ。**全 byte カバレッジを helper + ループで担保** |
| **`sendintr` / `sendeof` / `sendcontrol` の専用 API** | ctrl chars は名前ベース (`sendcontrol('c')`) で送信し、子から戻る ord 値で逆引き verify |
| **`echo=False` で line discipline echo を切る** | PTY の cooked mode の echo 機能で input が 2 重に出ないよう test 用に明示 disable |
| **`PexpectTestCase` 基底クラス** | `unittest.TestCase` を継承して共通 setUp (= PYTHONBIN 解決) を集約 |

**verdict**: hyoui の **input stream の全 byte パススルー** test (= DR-0014 透過原則の bytes 透過項目) はこの pattern で網羅できる。`hyoui run -- python3 helpers/echo_byte.py` を子で動かして 0..256 を全部送るだけ。

### 2.7 rexpect — Rust expect 系の最強短文

**ファイル**:
- `https://github.com/rust-cli/rexpect/blob/master/examples/bash.rs`
- `https://github.com/rust-cli/rexpect/blob/master/examples/repl.rs`
- `https://github.com/rust-cli/rexpect/blob/master/examples/exit_code.rs`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`spawn_bash(Some(1000))` 1 関数で bash + prompt 整備** | bash の PROMPT_COMMAND を tweak した bespoke prompt を仕込んだ `PtyReplSession` を返す。**「prompt 待ち = command 完了」を保証する糊** |
| **`send_control('z') + wait_for_prompt + execute("bg") + execute("fg")`** | bash の job control を **10 行で完全に駆動**。これが hyoui の Ctrl-Z 軸 1/2 検証に **直接転写可能** |
| **`execute(cmd, expected_marker)`** | 「cmd を send → expected_marker を expect」を 1 関数化。コマンド完了の同期 token として使う |
| **`PtyReplSession::echo_on(false) + quit_command(Some("Q"))`** | REPL ごとに echo 有無 / 終了コマンドを設定。**子 PTY の line discipline 挙動と整合させる** |
| **`p.process().wait()` で `WaitStatus::Exited(_, code)` 直アクセス** | exit code / signal exit を `nix::WaitStatus` で型安全に分岐 |

**verdict**: hyoui の **job control test 主力 crate 候補**。`hyoui run -- bash` を spawn して中身を rexpect で駆動する形 (= rexpect が hyoui を spawn、hyoui の中で bash を spawn、bash で Ctrl-Z) で軸 1/2 を実機検証できる。

### 2.8 expectrl — rexpect の async 強化版

**ファイル**:
- `https://github.com/zhiburt/expectrl/blob/master/tests/expect.rs`
- `https://github.com/zhiburt/expectrl/blob/master/tests/interact.rs`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`spawn("cat") + send_line + expect(...)`** | rexpect とほぼ同じ最短 API。`Regex` / `Eof` / `NBytes` の matcher が組み込み |
| **async / sync の同一 API** | `cfg(feature = "async")` で同じ test を async 化。`AsyncExpect` trait |
| **`set_input_action(callback)` / `set_output_action(callback)`** | input / output stream に callback を仕込んで条件反応。`Lookup::new()` で pattern matching を持続 |
| **windows サポート** | `pwsh -c "python ./tests/actions/cat/main.py"` で win 側 cat 相当を spawn。pexpect 系は windows 弱いが expectrl は ConPTY 経由で動く |

**verdict**: hyoui が windows サポートを将来検討するなら expectrl 優位、当面 unix 専用なら rexpect で十分。

### 2.9 pty-process — 最低 level の PTY 操作

**ファイル**:
- `https://github.com/doy/pty-process/blob/master/tests/basic.rs`
- `https://github.com/doy/pty-process/blob/master/tests/winch.rs`
- `https://github.com/doy/pty-process/blob/master/tests/behavior.rs`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **`(pty, pts) = open(); pty.resize(Size::new(24, 80))`** | PTY master / slave を 2 タプルで取得、resize は master に対する method。**所有権がはっきりした薄い API** |
| **`Command::new("cat").spawn(pts)`** | `std::process::Command` 互換、PTY slave を渡して spawn。slave の dup2 + setsid + ioctl(TIOCSCTTY) を crate 側が処理 |
| **`helpers::output(&pty)` で line iterator** | PTY master からの byte stream を `\r\n` 区切りで `Iterator<Item=String>` 化する helper。**iter.next() == "expected\r\n"** で 1 行ずつ assert |
| **`perl -E '$SIG{WINCH}=sub{say "WINCH"}'` で signal in-band 検出** | 子に perl で signal handler を仕込み、handler が標準出力に書き出した文字列を親 PTY で読む。**signal 配送の確定検出**を helper-less で実現する最良 pattern |
| **`pty.write_all(&[4u8])` で EOF (Ctrl-D) 送信** | byte 列 raw で送り PTY line discipline に EOF 解釈させる |
| **`cfg(not(target_os="macos"))` の platform 分岐** | macOS で PTY を spawn 後 再 spawn (`spawn_borrowed`) すると挙動が違うため macos test を一部 disable。**hyoui CI で macos runner を使う場合の参考** |

**verdict**: hyoui の **PTY harness 基盤 crate** として最有力候補。rexpect/expectrl より低 level で hyoui daemon の制御フローと整合する。

### 2.10 asciinema — record / replay 系の test framework

**ファイル**:
- `https://github.com/asciinema/asciinema/blob/main/tests/integration.sh`

**観察された pattern**:

| pattern 名 | 内容 |
|---|---|
| **自前 bash test framework** | `assert_exit_code` / `assert_file_exists` / `assert_file_not_empty` / `log_info` / `log_success` 等を sh 関数で定義し、`TESTS_RUN` / `TESTS_PASSED` / `TESTS_FAILED` カウンタを更新 |
| **color output (NO_COLOR 対応)** | `[[ ! -t 1 ]] \|\| [[ -n "${NO_COLOR:-}" ]]` で CI / 非 tty 時に色を切る |
| **`casts/` fixture** | record-replay cycle の正常性を fixture cast file に対して assert |

**verdict**: hyoui の **shell ベース smoke test** にコピペで持って来られる pattern。`tests/smoke.sh` で `hyoui run -- echo hello` → `hyoui screen dump session` の出力を `assert_contains "hello"` する程度なら一瞬で書ける。

---

## 3. 観点別 pattern 集約

### A. 実 PTY vs mock

| プロダクト | 実 PTY | mock | 理由 |
|---|---|---|---|
| tmux | ◎ | — | 黒箱 e2e のみ |
| zellij unit | — | ◎ | `MockOsApi` で trait 実装 |
| zellij screen | — | ◎ | server 出力 ANSI を test 側 grid で再 parse |
| zellij e2e | ◎ (SSH) | — | 完全 e2e |
| wezterm term test | — | ◎ | `Terminal::advance_bytes` 直叩き |
| alacritty ref | — | ◎ | recording を ansi parser に流し込み |
| abduco | ◎ | — | shell + diff のみ |
| pexpect | ◎ | — | 元祖 PTY automation |
| rexpect | ◎ | — | bash + job control |
| pty-process | ◎ | — | PTY 操作自身が target |

**hyoui への教訓**: **「core (= screen emulator) は mock テスト、daemon 統合は実 PTY」の二層が標準**。両方やる。

### B. signal テスト

**送信方法の pattern**:

- 親プロセス → 子: `killpg(child_pid, sig)` (= zellij `force_kill`) / `nix::sys::signal::kill(Pid, Signal)` / `Command::new("kill").args(["-TSTP", &pid])` (シェル経由)
- 子が自分に: 子側 `kill(getpid(), SIGTSTP)` / `os.kill(os.getpid(), signal.SIGTSTP)` (python) / `raise(SIGTSTP)` (C) / `$SIG{...}` (perl handler 経由)
- PTY 経由 (Ctrl-C / Ctrl-Z): `pty.write_all(b"\x03")` (= 0x03 = ^C) / `pty.write_all(b"\x1a")` (= 0x1a = ^Z) — line discipline が ISIG で解釈

**検証方法の pattern**:

- `kill -0 $pid` で生存確認 (tmux `kill-session-process-exit.sh`)
- 子側 in-band 検出: `$SIG{WINCH} = sub { say "WINCH" }` (pty-process)
- `WaitStatus::Exited(_, code)` / `WaitStatus::Signaled(_, sig, _)` (rexpect)
- `waitpid(WUNTRACED \| WNOHANG)` で stopped 判定 — hyoui の DR-0001 と一致
- thread sleep 100ms で配送猶予 (zellij `os_input_output_tests`)

**timing 問題**:

- 「signal 送信 → 効果検出」の race を **`thread::sleep(100ms)` 程度の決め打ち** で吸収 (zellij)
- もしくは「子 stdout に signal handler が "MARK" 書く → 親が read で待つ」で **同期 token** にする (pty-process)
- tmux は **`sleep 1`** ベース。30 年級でこれ。**過剰な precision を目指さない**

### C. screen state 検証

**取得方法の pattern**:

| プロダクト | 取り方 |
|---|---|
| zellij | server が client に送る `ServerInstruction::Render` ANSI 文字列を test 側で `vte::Parser + Grid` に再 parse |
| wezterm | `term.screen().for_each_phys_line(...)` で行直アクセス、`term.cursor_pos()` |
| alacritty | `terminal.grid()` clone → `truncate()` → JSON serialize して期待値と比較 |
| tmux | `tmux capturep -p` で出力 dump (= 自前 capture コマンド) |
| zellij e2e | SSH channel から raw ANSI byte を読み、test 側 emulator で再構成 |

**比較方法の pattern**:

- `format!("{:?}", grid)` 文字列化 + `insta::assert_snapshot!` (zellij、最も lean)
- `grid: Grid<Cell>` 構造体直比較 (alacritty)
- ANSI byte 同士 `cmp` バイナリ比較 (tmux `cmp -s`)
- 行 array 直比較 (wezterm `assert_dirty_lines`)

### D. attach / detach (multiplexer 系)

| プロダクト | pattern |
|---|---|
| tmux | `new -d`, `kill-session`, `attach`, `capturep` で全周検証 |
| zellij e2e | session 起動 → 1st client attach → key 入力 → detach → re-attach → snapshot で「復元後の screen state が一致」を assert |
| abduco | `$ABDUCO -c name cmd \| ...` で create-attached、`$ABDUCO -n name` で create-detached、`$ABDUCO -a` で attach、output diff |

**共通**: **detach 後 → 新 client attach → screen state 復元**を「snapshot 一致」で assert する。これは hyoui の `serve --attach` (DR-0013 関連) のテスト pattern として直転写。

### E. CI で PTY 利用

| プロダクト | CI | 実 PTY |
|---|---|---|
| zellij | GA `ubuntu-latest` + service container (openssh) | ◎ (SSH 経由) |
| wezterm | GA `ubuntu-latest` + `container: debian:12` | ◎ |
| tmux | (autotools test、`make check`) | ◎ (実 PTY) |
| rexpect | GA `ubuntu-latest` | ◎ (実 PTY) |
| pty-process | GA `ubuntu-latest` + macos | ◎ (実 PTY、macos 一部 disable) |
| pexpect | GA `ubuntu-latest` + macos + windows | ◎ |

**結論**: **GA ubuntu-latest で実 PTY は問題なく動く**。container 内も `/dev/ptmx` があれば OK。flaky 対策が必要なケースは zellij 流の **regex 正規化 + retry** で対処。

### F. テスト framework / library

| プロダクト | framework |
|---|---|
| tmux | sh + diff + cmp |
| zellij | `cargo test` + `insta` + `ssh2` (e2e) |
| wezterm | `cargo test` + `k9` |
| alacritty | `cargo test` + `serde_json` (fixture) |
| abduco | sh + diff |
| pexpect | `unittest` |
| rexpect | `cargo test` + `nix` |
| expectrl | `cargo test` + `futures_lite` (async) |
| pty-process | `cargo test` + `tokio` (async) |
| asciinema | sh (自前 framework) |

**hyoui 採用案**: **`cargo test` + `insta` + `pty-process` + (optionally) `rexpect` の 4 本立て**。

### G. 決定性 / flaky 対策

- **`sleep 1`** で諦める (tmux): 単純、十分に堅牢
- **`thread::sleep(100ms)`** + retry 内蔵 (zellij `RETRIES: usize = 10`)
- **regex 正規化で不安定領域を消す** (zellij `account_for_races_in_snapshot`)
- **「子から MARK 出力させる」同期 token** (pty-process WINCH)
- **同期トークン (= prompt 文字列)** + `wait_for_prompt` (rexpect)

### H. テスト粒度

| 粒度 | 採用プロダクト | 特徴 |
|---|---|---|
| **unit (= 型直叩き)** | wezterm term test, zellij screen_tests, alacritty ref | 実 PTY なし、core ロジックのみ。最速、最も大量に書ける |
| **integration (= 内部 boundary 越え)** | zellij os_input_output_tests, pty_tests | 実 process spawn、trait mock で boundary 制御 |
| **e2e (= 実 binary 全周)** | tmux regress, zellij e2e, abduco, asciinema | 実 binary + 実 PTY + 実 child。1 個あたり高コストだが価値高 |

**hyoui への教訓**: unit を **過剰に書き**、integration を **必要な daemon 境界だけ**、e2e は **マトリクスの cell 1 個ずつ** 1 個書く。

---

## 4. hyoui に直接 applicable な pattern (= 抽出)

優先度順。「どの project から学んだか」「hyoui のどこに適用するか」を併記。

| # | pattern 名 | 学んだ元 | hyoui での適用先 |
|---|---|---|---|
| 1 | **`spawn_bash + send_control('z') + bg/fg` 黒箱 job control** | rexpect | `tests/matrix/jobcontrol_axis1.rs`、DR-0001 軸 1 (`follow` / `auto-resume`) のマトリクス |
| 2 | **`MockOsApi` trait + in-process unit** | zellij | hyoui daemon の OS 境界に同等の trait を切り、signal / kill / pty 操作を mock 化して unit で invariant 検証 |
| 3 | **ANSI 録画 → ref test (alacritty)** | alacritty | `tests/fixtures/recordings/*.bin` を保管、`hyoui screen dump --format=ansi` の決定性 regression |
| 4 | **`insta snapshot + vte で grid 再構成`** | zellij screen_tests | `hyoui screen snapshot` 出力の visual regression、`tests/screen/*.rs` |
| 5 | **test-only socket / runtime dir 分離** | tmux `-Ltest` | `HYOUI_RUNTIME_DIR=$(mktemp -d)` を `tests/common/` で発行、test ごとに完全独立 daemon |
| 6 | **`account_for_races` 相当の regex 正規化** | zellij | snapshot 比較前に「不安定 cursor 行」「async load 表示」「dirty bit 残骸」を regex で削る共通関数 |
| 7 | **子 in-band signal 検出 (`$SIG{WINCH} = sub{say "WINCH"}`)** | pty-process | SIGWINCH / SIGHUP / SIGTERM 配送マトリクス検証、perl/python helper を `tests/fixtures/sig-echo.py` 等で用意 |
| 8 | **stdin pipe + diff の minimal smoke** | abduco | `tests/smoke.sh` で hyoui の基本機能 (= run / detach / attach / wait) を 30 行以内で検証 |
| 9 | **`TestTerm` wrapper + Deref + 大量 helper** | wezterm | hyoui screen emulator core (vt100 ラッパー) に `TestEmulator` を生やし、`erase_in_line(...)` `cup(col, row)` 等の test helper 集約 |
| 10 | **`Mock: EventListener` で event 配送を no-op** | alacritty | hyoui の `ServerInstruction` 配送、`OutputBuffer` 通知などを test 用 no-op 実装で断ち切る |
| 11 | **SSH 経由で実 client e2e** | zellij e2e | hyoui の `serve` gateway 完成後の e2e で SSH コンテナ + `ssh2::Session` パターン採用 (= 将来) |
| 12 | **同期 token = prompt / MARK 出力**  | rexpect / pty-process | sleep ベースの flaky 回避、必ず子から戻る確定 byte で同期 |
| 13 | **macro による test 大量生成** | alacritty `ref_tests!` macro | hyoui の `screen_tests!` macro でマトリクス cell を 1 test として展開 |
| 14 | **cross-platform `cfg(not(windows))`** | zellij | hyoui は unix 専用とはいえ、CI macos runner 追加時に `cfg(not(target_os="macos"))` のような細部分岐 |
| 15 | **自前 sh test framework (`assert_*` 関数群)** | asciinema | `tests/smoke.sh` 共通 helper、`assert_exit_code` / `assert_screen_contains` 等 |

---

## 5. 採用すべき rust crate / framework

hyoui の lean 方針 + 既存依存 (`vte`, `tokio`, `nix` 等) と整合する組み合わせ:

| crate | 役割 | 必須度 |
|---|---|---|
| **`insta`** | snapshot test、screen state regression の正本 | 必須 |
| **`pty-process`** | PTY harness 基盤、`tests/common/pty.rs` で wrap | 必須 |
| **`vte`** | 既存依存。test 側でも grid 再構成器として再利用 | 必須 |
| **`rexpect`** | bash job control 検証の最短経路。`tests/matrix/jobcontrol_*.rs` で使う | 推奨 |
| **`pretty_assertions`** or **`k9`** | failure 時の diff 見やすさ | 推奨 (任意) |
| **`nix`** | 既存依存。`WaitStatus` / signal / `setpgid` 等 test でも使う | 既に依存 |
| **`tempfile`** | `HYOUI_RUNTIME_DIR` 用 tempdir。test 間隔離 | 必須 |
| **`regex`** | `account_for_races` 相当の snapshot 正規化 | 推奨 |

**不要**:

- `expectrl` (= rexpect で十分、async / windows 必要時のみ検討)
- `ssh2` (= e2e SSH コンテナを後日採用するなら、現時点は不要)
- `serde_json` for ref test (= alacritty 方式の grid JSON シリアライズは hyoui には過剰。`insta` の `Debug` snapshot で同等)

---

## 6. 採用すべきでない pattern (= hyoui には過剰)

| pattern | 出元 | 不採用理由 |
|---|---|---|
| **SSH コンテナ e2e** | zellij | 初期スコープ外。CI コスト高。`serve` gateway 完成後に検討 |
| **fixture を JSON で full serialize** | alacritty | `insta` の `Debug` snapshot で代替可能、JSON は重い |
| **windows ConPTY サポート** | expectrl | hyoui は unix 専用、複雑性増 |
| **GUI 統合 e2e** | wezterm | hyoui は headless / daemon、GUI 統合は意図的に対象外 (DR-0005) |
| **30 sec を超える長 timeout** | pexpect の一部 test | flaky を sleep で吸収するのは反パターン。同期 token で 1-2 sec 上限 |
| **clipboard / GUI / sixel 等のリッチ機能 test** | wezterm | hyoui のスコープ外、bytes 透過原則の対象 |

---

## 7. CI 整備の推奨 (= GitHub Actions)

### 7.1 結論

**GA `ubuntu-latest` で実 PTY を使う test を回せる**。zellij / rexpect / pty-process / wezterm が全て GA で実 PTY テスト通過させている。

### 7.2 hyoui の最小 CI 構成案

```yaml
# .github/workflows/test.yml (概念図、実装は別 task)
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --workspace                      # unit + integration
      - run: cargo test --workspace --features pty-tests # rexpect / pty-process 系
      - run: cargo insta accept --check                  # snapshot mismatch を fail
```

### 7.3 shell 経由の signal 送信を CI で

GA runner 上で `kill -TSTP $PID` / `bash -c 'kill -STOP $$'` 等は問題なく動く。hyoui の external signal test は **shell 経由** (`Command::new("kill").args(["-TSTP", &pid_str]).status()`) で書く方が透過原則と整合する (= signal 送信は kernel 標準機能、hyoui が `kill(2)` API を直接呼ぶ test より shell 経由の方がユーザ目線に近い)。

### 7.4 flaky 対策

- snapshot 正規化 (`tests/common/normalize.rs` で `account_for_races` 相当)
- 同期 token (prompt / MARK 出力) を必ず噛ます
- timeout 上限 `Duration::from_secs(5)` を tests 標準にし、超えたら fail
- `RUST_LOG=hyoui=trace cargo test -- --nocapture` で local 再現

---

## 8. マトリクス検証への直接適用 (= DR-0014 §検証主義)

DR-0014 で要求された「interactive/headless × app × signal × 送信元」マトリクス検証の実装手順案。

### 8.1 マトリクスの軸

| 軸 | 値 |
|---|---|
| **app category** | TUI alt-screen (vim / claude / less +F) / line-oriented (cat / tail -f / yes) / interactive REPL (bash / python / node) |
| **mode** | `interactive` (= 既定) / `headless` (= `--mode=headless`) |
| **signal** | Ctrl-Z (子 self via PTY) / Ctrl-C / kill -TSTP (親外部) / kill -INT (親外部) / kill -HUP (親外部) / kill -WINCH (= resize) |
| **送信元** | 子 self (子の中で `raise(SIGSTOP)`) / 親 self (hyoui に外部から `kill -TSTP`) / 子 PTY (^Z byte stream で line discipline 経由) |

= 計 ~3 × 2 × 6 × 3 = 108 cell。**全部書く必要はない**、軸 1/2 の挙動が変わる cell + 透過性が重要な cell に絞る (= 想定 20-30 cell)。

### 8.2 実装手順

**Step 1: PTY harness を 1 個作る** (= `tests/common/pty.rs`)

```rust
// 概念。pty-process ベース。
pub struct HyouiTestRunner {
    pty: pty_process::blocking::Pty,
    child: pty_process::blocking::Child,
    runtime_dir: tempfile::TempDir,
}

impl HyouiTestRunner {
    pub fn spawn_hyoui(args: &[&str]) -> Self { ... }
    pub fn send_bytes(&mut self, b: &[u8]) -> std::io::Result<()> { ... }
    pub fn wait_for(&mut self, re: &Regex, timeout: Duration) -> Result<String> { ... }
    pub fn wait_for_exit(&mut self) -> WaitStatus { ... }
    pub fn screen_dump(&self, session: &str) -> String { /* hyoui screen dump 経由 */ }
    pub fn external_signal(&self, sig: Signal) { /* hyoui 自身に kill(sig) */ }
}
```

**Step 2: 軸 1 マトリクスを 1 ファイル**: `tests/matrix/jobcontrol_axis1.rs`

```rust
// 概念。3 app × 2 mode × 1 signal (Ctrl-Z) のみ最小 6 cell。
#[test] fn axis1_bash_interactive_ctrlz_follow_resumes_with_fg() { ... }
#[test] fn axis1_bash_headless_self_tstp_auto_resumes() { ... }
#[test] fn axis1_python_interactive_ctrlz_follow_resumes_with_fg() { ... }
#[test] fn axis1_python_headless_self_tstp_auto_resumes() { ... }
#[test] fn axis1_vim_interactive_ctrlz_follow_resumes_with_fg() { ... }
#[test] fn axis1_vim_headless_self_tstp_auto_resumes() { ... }
```

各 test は:

1. `HyouiTestRunner::spawn_hyoui(&["run", "--mode=...", "--", "bash", "-c", "..."])` で起動
2. `send_bytes(b"\x1a")` で Ctrl-Z 送信 (= 子 PTY line discipline 経由)
3. **expected**: follow なら親が STOPPED に、auto-resume なら子が即復帰 (= ps で確認 / output から "RESUMED" マーク確認)
4. `wait_for_exit` で WaitStatus 確認

**Step 3: screen snapshot test を 1 ファイル**: `tests/screen/attach_restore.rs`

```rust
// 概念。
#[test]
fn attach_restore_after_vim_input() {
    let runner = HyouiTestRunner::spawn_hyoui(&["run", "--detach", "--", "vim"]);
    runner.send_bytes(b":help\r");
    sleep_until_stable(Duration::from_secs(2));
    let snap1 = runner.screen_dump_normalized("session");
    // detach + re-attach
    let runner2 = HyouiTestRunner::attach("session");
    let snap2 = runner2.screen_dump_normalized("session");
    insta::assert_snapshot!("vim_after_help", snap1);
    assert_eq!(snap1, snap2, "screen state after re-attach must match");
}

fn normalize(s: &str) -> String {
    // account_for_races 相当。cursor 行末空白、async status 等を regex で消す。
    ...
}
```

これで **3 step で軸 1 マトリクス + attach 復元 snapshot の基盤** が立ち上がる。残り cell はこの形を ループ展開して追加する。

---

## 9. 参考 URL 一覧

### tmux
- `https://github.com/tmux/tmux/blob/master/regress/Makefile`
- `https://github.com/tmux/tmux/blob/master/regress/control-client-sanity.sh`
- `https://github.com/tmux/tmux/blob/master/regress/kill-session-process-exit.sh`
- `https://github.com/tmux/tmux/blob/master/regress/tty-keys.sh`
- `https://github.com/tmux/tmux/blob/master/regress/input-keys.sh`
- `https://github.com/tmux/tmux/blob/master/regress/capture-pane-sgr0.sh`
- `https://github.com/tmux/tmux/blob/master/regress/decrqm-sync.sh`

### zellij
- `https://github.com/zellij-org/zellij/blob/main/zellij-server/src/unit/os_input_output_tests.rs`
- `https://github.com/zellij-org/zellij/blob/main/zellij-server/src/unit/pty_tests.rs`
- `https://github.com/zellij-org/zellij/blob/main/zellij-server/src/unit/screen_tests.rs`
- `https://github.com/zellij-org/zellij/blob/main/src/tests/e2e/remote_runner.rs`
- `https://github.com/zellij-org/zellij/blob/main/src/tests/e2e/cases.rs`
- `https://github.com/zellij-org/zellij/blob/main/.github/workflows/e2e.yml`

### wezterm
- `https://github.com/wez/wezterm/blob/main/term/src/test/mod.rs`
- `https://github.com/wez/wezterm/blob/main/term/src/test/c0.rs`
- `https://github.com/wez/wezterm/blob/main/term/src/test/c1.rs`
- `https://github.com/wez/wezterm/blob/main/term/src/test/csi.rs`
- `https://github.com/wez/wezterm/blob/main/.github/workflows/gen_debian12.yml`

### alacritty
- `https://github.com/alacritty/alacritty/blob/master/alacritty_terminal/tests/ref.rs`
- `https://github.com/alacritty/alacritty/tree/master/alacritty_terminal/tests/ref`

### abduco
- `https://github.com/martanne/abduco/blob/master/testsuite.sh`

### pexpect
- `https://github.com/pexpect/pexpect/blob/master/tests/test_ctrl_chars.py`
- `https://github.com/pexpect/pexpect/blob/master/tests/test_expect.py`

### rexpect
- `https://github.com/rust-cli/rexpect/blob/master/examples/bash.rs`
- `https://github.com/rust-cli/rexpect/blob/master/examples/repl.rs`
- `https://github.com/rust-cli/rexpect/blob/master/examples/exit_code.rs`

### expectrl
- `https://github.com/zhiburt/expectrl/blob/master/tests/expect.rs`
- `https://github.com/zhiburt/expectrl/blob/master/tests/interact.rs`

### pty-process
- `https://github.com/doy/pty-process/blob/master/tests/basic.rs`
- `https://github.com/doy/pty-process/blob/master/tests/winch.rs`
- `https://github.com/doy/pty-process/blob/master/tests/behavior.rs`

### asciinema
- `https://github.com/asciinema/asciinema/blob/main/tests/integration.sh`

### hyoui 内部参照
- `docs/decisions/DR-0001-bgfg-jobcontrol-two-axis.md`
- `docs/decisions/DR-0013-screen-emulator-and-attach-stability.md`
- `docs/decisions/DR-0014-transparency-and-empirical-verification.md`
- `docs/research/2026-05-27-multiplexer-implementation-study-rust.md`
- `docs/research/2026-05-27-screen-emulator-crate-comparison.md`
