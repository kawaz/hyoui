# DR-0043: 子は前景の job として、既定の signal の扱いと空の mask で exec する

- Status: ✅ 実装済 (2026-10-09)。SIG-Q1 = a (kawaz 裁定 2026-10-09)
- Date: 2026-10-09
- Related: DR-0042 (hyoui は子に対して bash の位置に立つ), DR-0017 (session anchor: 子を daemon と同じ session の foreground pgrp にする), DR-0014 (介入 self-check と検証主義), DR-0005 (透過原則), DR-0015 (run = fork daemon + attach)
- Origin: `docs/issue/2026-10-09-design-child-inherits-ignored-signals.md` (観測と SIG-Q1 の裁定)

## Context

`execve(2)` は、呼び出し元で無視 (SIG_IGN) になっている signal を無視のまま、signal mask もそのまま新しいプログラムに引き継ぐ (handler を張った signal だけが既定に戻る)。hyoui の子は「呼び出し元 → `hyoui run` → daemon → 子」と fork / exec を重ねて起動されるので、途中のどこかで無視・block された設定が子まで届く。

- 対話の bash のコマンド置換 `$(...)` の中は、job control の signal (SIGTSTP / SIGTTIN / SIGTTOU) が無視になる。`I=$(hyoui run --detached -- cat)` で起動した子は ^Z も `kill -TSTP` も効かない
- 非対話の shell で `cmd &` とすると SIGINT / SIGQUIT が無視になる。`sh -c 'hyoui run --detached -- cat &'` の子は SIGINT で終わらない
- hyoui 自身も Rust の runtime が起動時に SIGPIPE を無視にする。呼び出し元が何も変えていなくても、子は SIGPIPE を無視した状態で始まる
- 呼び出し元が block した signal は daemon の mask に残り、子の mask にも残る (SIGTERM を block した呼び出し元から起動すると、子は SIGTERM で終わらない)

子は PTY を制御端末に持つ前景の job として動く (DR-0017)。対話の bash は前景の job を exec する前に、自分が無視にしていた job control とキーボードの signal を既定に戻す。tmux も pane の子の exec の前に、自分が無視・捕捉していた signal (SIGPIPE / SIGTSTP / SIGINT / SIGQUIT / SIGHUP / SIGTERM 等) を既定に戻してから mask を戻す (`proc.c` の `proc_clear_signals` と `spawn.c`)。

## 介入判断 self-check (CLAUDE.md / DR-0014)

- **既存 DR で justify されているか**: DR-0042 で「hyoui は子に対して bash の位置に立ち、bash と同じように配線する」と決めた。本 DR はその配線の対象を signal の扱いと mask に広げる。spawn の構造 (DR-0017) は変えない
- **必然か**: 透過の基準を「端末で起動された時と同じ」に置く。子は PTY を制御端末に持つ対話のプロセスとして起動されるので、端末から起動された前景の job と同じ signal の扱いで始まるのが透過になる。今のままだと ^Z / ^C が効かない、`kill -TERM` で終わらない、の実害が出ている。直接実行で `$(cmd)` とした時に cmd が無視を引き継ぐのは、cmd が端末の前景の job ではないからで、hyoui の子とは立場が違う
- **最小介入か**: 戻すのは、端末の前景の job が既定で受け取る signal (SIGINT / SIGQUIT / SIGTSTP / SIGTTIN / SIGTTOU) と、hyoui 自身が無視にする SIGPIPE の 6 つと mask だけ。一覧の外の無視 (`nohup` の SIGHUP 等) は `execve` の規定どおり子に引き継ぐ (決定 3)
- **kernel / PTY / shell の再発明でないか**: signal の配送と既定の動作 (停止・終了) は kernel のもの。hyoui は exec の前に扱いを既定にするだけで、signal を中継・合成しない
- **partial state を hyoui の裁量で破棄する介入か**: 該当しない。子の状態を読んで判定するものが無い
- **新 protocol message / cap flag**: 無し
- **既存 DR の未実装**: DR-0042 の対象範囲 (fd と制御端末) は実装済みで、本 DR とは独立

## Decision

### 1. 6 つの signal を既定に戻す

子は exec の前に SIGINT / SIGQUIT / SIGTSTP / SIGTTIN / SIGTTOU / SIGPIPE の扱いを既定 (SIG_DFL) にする。anchor 経路 (`openpty_fork_anchor_exec`) とテスト用の legacy 経路 (`forkpty_then_exec_legacy`) の両方で行う。

### 2. signal mask を空にする

子は exec の前に signal mask を空にする。fork の前に保存した mask (= daemon の mask で、呼び出し元から引き継いだもの) には戻さない。

### 3. 一覧の外の signal は引き継ぐ

SIGHUP / SIGTERM / SIGALRM 等の無視は、`execve` の規定どおり子に引き継ぐ。`nohup hyoui run -- cmd` の cmd が SIGHUP を無視するのは、直接実行の `nohup cmd` と同じ。daemon が handler を張っている signal は exec で既定に戻る (`execve` の規定)。子はその handler を継承していれば exec の前に既定に戻す (決定 4 の 3。exec 後の状態は変わらない)。

### 4. 子側の順序

fork から exec までの子の手順は次の順にする。どれも async-signal-safe な呼び出しだけで行う。

1. 親から継承した self-pipe の handler を無効にする (`disarm_self_pipe_in_child`)。fork の前に `block_handled_signals` で block した signal は、まだ block のまま
2. (anchor 経路のみ) `setpgid(0, 0)` で新しい pgrp になり、SIGTTOU を一時的に無視して `tcsetpgrp` で自分を foreground にする
3. 6 つの signal と、親から継承した hyoui の handler (self-pipe / SIGWINCH の handler) が張られた signal を既定に戻してから、mask を空にする (`reset_signals_for_exec`)
4. fd の dup2、chdir、exec

理由:

- 1 を mask を外す前に行う: 外した時点で、block 中に届いていた signal が配送される。disarm の前だと、継承した handler が親の self-pipe に signal 番号を書き、親が自分宛ての signal と誤認する
- 3 で扱いを既定にしてから mask を外す: block 中に届いていた SIGINT / SIGTSTP 等を、exec 後と同じ既定の動作で受ける (無効化した handler に飲み込ませない)
- 継承した hyoui の handler も既定に戻す: 実運用の経路では `Session::start` が serve (handler を張る) より先に子を fork するので継承は起きないが、同じ process で serve を動かした後に spawn する経路 (test 等) では SIGTERM / SIGHUP / SIGUSR1 等の handler が残る。今の handler が hyoui のものかを `sigaction` で確かめて既定にするだけで、exec 後の状態は変わらず、呼び出し元から引き継いだ無視には触れない。前提を文書に残して経路ごとに守らせるより、子側で常に成り立たせる方が単純
- 3 を 2 の後に行う: SIGTTOU が既定のまま `tcsetpgrp` を呼ぶと、親より先に来た子は background pgrp なので SIGTTOU で止まり、exec に届かない (DR-0017)

## 責務外

daemon 自身の signal の扱いと mask は本 DR の対象外 (子が exec の時点で持つ状態だけを扱う)。

## Rejected alternatives

| 案 | 理由 |
|---|---|
| 今のまま引き継ぐ (透過を「直接実行と同じ」と読む) | 子は端末の前景の job として起動されるのに、^Z / ^C / SIGTERM が効かない。直接実行の `$(cmd)` の cmd とは端末に対する立場が違う |
| 全 signal (1〜NSIG) を既定に戻す | `nohup` の SIGHUP など、呼び出し元が子まで含めて意図した無視を消す。直接実行の `nohup cmd` とずれる |
| mask を呼び出し元のものに戻す | block した呼び出し元から起動すると、子が SIGTERM 等を受け取れない。対話の bash も前景の job を空の mask で起動する |

## Consequences

- `I=$(hyoui run --detached -- cat)` の子が ^Z / `kill -TSTP` で止まり、`sh -c 'hyoui run --detached -- cat &'` の子が SIGINT で終わる
- 子は SIGPIPE が既定で始まる。読み手の去った pipe に書いた子は、端末で直接実行した時と同じく SIGPIPE で終わる
- 呼び出し元が block していた SIGTERM 等は子の mask に残らない

## 検証

- unit (legacy 経路): `sys::signal::tests::spawned_child_starts_with_default_signals_and_empty_mask`。test process で 6 つ + SIGHUP を無視、SIGTERM / SIGUSR2 を block してから `Pty::spawn` し、子の perl に `%SIG` と `sigprocmask` を報告させる
- e2e (anchor 経路、daemon 経由): `crates/hyoui-cli/tests/child_signal_defaults.rs`。無視・block した呼び出し元と、何も変えていない呼び出し元 (SIGPIPE が既定になることを見る) の 2 セル
- unit (継承した handler): `sys::signal::tests::reset_drops_inherited_own_handlers_but_keeps_inherited_ignores`。SIGUSR1 に self-pipe の handler、SIGHUP に無視を張って fork し、子で `disarm_self_pipe_in_child` → `reset_signals_for_exec` の後の扱いを見る (SIGUSR1 は既定、SIGHUP は無視のまま)
- 実機: 対話の bash の `$(...)` から起動した子が `kill -TSTP` で `T+` になる、非対話 sh の `cmd &` から起動した子が SIGINT で終わる (本 DR の変更を外した 0.14.0 ではどちらも `S+` のまま)

### 3 category のマトリクス (DR-0014、実機 2026-10-09、macOS)

呼び出し元は 6 つの signal を無視にして hyoui を exec する (perl で `$SIG{...}='IGNORE'` してから `hyoui run --detached --pty-stdin -- <app>`、`$(...)` / `cmd &` の代わり)。前景の ^Z は `hyoui input <id> key:C-z` (PTY の line discipline か app 自身が SIGTSTP にする)、外からの signal は子の pid への `kill`。値は 2 秒以内に観測した `ps -o stat` (`gone` は終わった)。0.14.0 は本 DR の変更を含まない版。

| app (category) | 期待 | 0.14.0: ^Z / kill -TSTP / kill -INT | 本 DR: ^Z / kill -TSTP / kill -INT |
|---|---|---|---|
| `vim -u NONE -N` (TUI、alt screen) | ^Z と TSTP で止まる。INT は vim が捕捉して続く | `S+` / `S+` / `S+` | `T+` / `T+` / `S+` |
| `cat` (line-oriented) | ^Z と TSTP で止まる。INT で終わる | `S+` / `S+` / `S+` | `T+` / `T+` / `gone` |
| `python3 -I -q` (REPL) | ^Z と TSTP で止まる。INT は python が捕捉して続く | `S+` / `S+` / `S+` | `T+` / `T+` / `S+` |

python の INT は捕捉されたかも見た: `key:C-c` で 0.14.0 は 2 秒以内に `KeyboardInterrupt` が出ず (無視されている)、本 DR では出る (python が起動時に既定の SIGINT を見て handler を張った)。

## 関連

- [[DR-0042]] — hyoui は子に対して bash の位置に立つ (fd と制御端末)
- [[DR-0017]] — session anchor (子を同じ session の foreground pgrp にする手順)
- [[DR-0005]] / [[DR-0014]] — 透過原則と介入 self-check
