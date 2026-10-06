# DR-0042: 非 tty の stdin は子の fd 0 にそのまま渡し、PTY は制御端末と出力にだけ使う

- Status: 🚧 Active (2026-10-06)。決定 1〜5 は裁定済み (STDIN-Q1 の α / β / γ とも a、kawaz 2026-10-06)
- Date: 2026-10-06
- Supersedes (部分): DR-0019 §5 (pipe-through: 非 tty stdin の EOF で EOT を送る、`--stdin-eof`、2026-10-05 の注記と「Update: `--detached` でも非 tty stdin を子に届ける」)
- Related: DR-0005 (透過原則), DR-0014 (介入 self-check と検証主義), DR-0015 (run = fork daemon + exec attach), DR-0017 (session anchor: daemon が制御端末を持ち、子を同 session の foreground にする), DR-0028 (upgrade の self-exec と継承 fd), DR-0029 (attach は覗き窓、Ctrl+Z ガード), DR-0016 (record の `in` event), DR-0039 決定 1 (gateway からログイン shell を作る)
- Origin: `docs/issue/2026-10-05-design-pipe-stdin-pass-fd-to-child.md` (PoC とシェルとの対応、STDIN-Q1 の裁定)

## Context

`claude <<<'プロンプト'` は直接実行だとプロンプトが送信されるが、`hyoui run` を通すと入力欄に残って送信されない。DR-0019 §5 は呼び出し元の非 tty stdin の bytes を子の PTY master に書き、終わりに EOT (0x04) を送っていた。子の stdin は PTY のままなので、同じ bytes が子からはキー入力に見える。

直接実行の claude は非 tty の stdin を最初のプロンプトとして読み、その後のキーは `/dev/tty` から読む。hyoui を通すと bytes が claude の raw 化より前にキーとして届き、末尾の CR は ICRNL で LF になって送信にならない。LF を CR に変えて送る、子が raw になるまで待ってから送る、のどちらも claude では直らない (実測、issue 参照)。

同じ構造から次のずれも出ていた。

- `printf 'a\003b\000c\n' | hyoui run -- od -c` は 0x03 が ISIG で SIGINT になり rc=130 (直接実行は `a 003 b \0 c \n`)。バイナリが運べない
- `printf 'hi\nthere' | hyoui run -- cat` の出力に PTY の echo と EOT の痕跡 (`^D\b\b`) が混ざる
- 改行で終わらない入力は EOT を 2 個、python の `input()` は 3 個要るなど、EOF を line discipline の VEOF で模す限り子ごとの差が残る
- raw mode の TUI には EOT がただの入力として刺さるので `--stdin-eof=detach` を併用させていた

対話の bash が `cmd | tui` を起動する時、bash は pipe を `tui` の fd 0 に dup2 し、fd 1 / 2 と制御端末は自分の tty のまま渡す。stdin の bytes を端末に流し込むことはない。`tui` は stdin が tty でないので `/dev/tty` を開いてキーを読む。hyoui の子に対して hyoui は bash の位置に立ち、hyoui の PTY が bash の tty に当たる。hyoui の spawn 構造 (DR-0017) で fd 0 だけを pipe にした PoC では、claude / fzf / less / cat / python3 -I / bash の全部が直接実行と同じに動いた (issue の表)。

bash との違いは 2 つある。

- **端末が 2 段になる。** 利用者の端末と hyoui の PTY の間を attach client が中継するので、bash で「キーが端末から子へ直接届く」部分を attach client が担う
- **呼び出し元に端末が無くても PTY を作れる。** bash には無い、`tmux new-session -d` の振る舞いである。tmux の detached session の子の stdin は呼び出し元の stdin ではなく pty である

## 介入判断 self-check (CLAUDE.md / DR-0014)

- **既存 DR で justify されているか**: 本 DR は介入を減らす。DR-0019 §5 が持っていた介入 (stdin の bytes を PTY master に書く、EOT を合成して送る、daemon の reader thread と内部 pipe、lock 中の停止) を全部取り除き、kernel の fd 継承に置き換える。fd の配線は DR-0015 の fork daemon と DR-0017 の spawn 構造の中で行い、新しい種類の介入は足さない
- **必然か**: 子から見た stdin を直接実行と揃えるのが目的で、直接実行で `echo x | prog` とした時 prog の stdin は pipe である。PTY に変えて流し込む方が非透過で、`claude <<<prompt` が送信されない、バイナリが運べない、の実害が出ている
- **最小介入か**: hyoui がするのは「子の fd 0 に何を dup2 するか」を選ぶことだけで、bytes に触れない。stdin が tty の時と `--pty-stdin` の時は今どおり PTY slave を dup2 する
- **kernel / PTY / shell の再発明でないか**: EOF は pipe / file の EOF そのもの、書き手への EPIPE は kernel が返す。bash の pipeline と同じ配線で、EOT による EOF の模倣をやめる
- **新 protocol message / cap flag**: 無し。daemon への受け渡しは既存の `HYOUI_DAEMONIZE_INIT` JSON の field と fd 0 の継承で、`--stdin-eof` を attach に伝える経路は消える
- **既存 DR の未実装**: 本 DR の対象範囲で DR が約束して未実装のものは無い。DR-0019 §5 は実装済みで、それを置き換える

## Decision

### 1. 呼び出し元の stdin が tty でない時、その fd を子の fd 0 にする

- `hyoui run` は呼び出し元の stdin が tty でない (pipe / file / socket / `/dev/null` 等) 時、その fd を子の fd 0 にそのまま渡す。子の fd 1 / 2 と制御端末は今どおり PTY slave である
- `--detached` の有無で同じ。どちらも daemon が fd を受け取って子に渡す (決定 2)
- stdin が tty の時は今どおり子の stdin も PTY slave にする
- `/dev/null` も他の非 tty と同じく子に渡す。子はすぐ EOF を読む (直接実行で `prog </dev/null` とした時と同じ)。`bash -i` などの対話 shell は EOF で終わる。端末から切り離した起動元 (agent の Bash ツール、web gateway など) で対話の子を残したい時は決定 3 の指定を使う
- `hyoui input` は PTY master に書くので、stdin を読む子 (`cat` 等) に pipe を渡した後は、その子の stdin には届かない (直接実行で pipe を渡した時と同じ)。`/dev/tty` からキーを読む子 (claude / fzf / less 等) には届く

理由: 子から見た stdin を直接実行と同じにする (DR-0005)。EOF・バイナリ・EPIPE の扱いが kernel のものになり、子ごとの差が消える。

### 2. fd は spawn の前に渡し、hyoui はどのプロセスも fd を持ち続けない

- daemon は受け取った fd を子の spawn の時に fd 0 へ dup2 し、spawn が返った直後に自分の分を閉じる。daemon 自身の fd 0 は spawn の前に `/dev/null` にする (upgrade の self-exec (DR-0028) は fd 0 を引き継ぐので、そこに pipe を残さない)
- daemon が持つ複製は CLOEXEC 付きで、子の exec と upgrade の exec のどちらにも漏れない (子には dup2 で作った fd 0 だけが渡る)
- 非 detached の `run` も daemon に stdin を継承させる (DR-0015 の fork daemon は `run --detached` と同じ経路なので、渡し方は 1 つ)。exec する attach client は stdin を読まない (決定 4)
- 結果として pipe の読み手は子だけになる。子が先に終われば書き手は EPIPE を受け、書き手が閉じれば子は EOF を読む (直接実行と同じ)

理由: hyoui のプロセスが fd を持ち続けると、子が stdin を閉じたり終わったりしても書き手に EPIPE が届かない。daemon が spawn の時点で手放せば、hyoui は配線するだけで pipe の寿命に関与しない。

### 3. 子の stdin を PTY にする指定: `run --pty-stdin`

- `hyoui run --pty-stdin` は呼び出し元の stdin を子に渡さず、子の stdin も PTY slave にする。呼び出し元の stdin が tty の時は指定しなくても同じ結果になる (指定は無害)
- 用途は、端末から切り離した起動元から外から `hyoui input` / attach で操作し続ける shell / REPL を起動すること (`hyoui run --detached --pty-stdin -- bash -i`、`tmux new-session -d` に当たる)。操作し続ける使い方は起動側が明示する (`bash` を継続操作で使うなら `-i` を付けるのと同じ考え方)
- `--detached` と組み合わせても、非 detached でも同じ意味である。非 detached では attach client のキーが子の stdin に届く
- 指定した時、呼び出し元の stdin は daemon にも渡さない (daemon の stdin は `/dev/null`)

名前の理由: 子の stdin が何になるかをそのまま言う。hyoui の利用者は子が PTY の中で動くことを知っており (`hyoui run — run a command inside a PTY`)、「stdin も PTY」で意味が閉じる。`--no-stdin` は「子に stdin が無い」と読めて逆の誤解を招き、`--interactive` は DR-0019 で消した `--mode=interactive` と、docker の `-i` (= stdin を渡す、本指定と逆) の両方とぶつかる。`--stdin=pty|inherit` の値付きは選べる値が実質 1 つで、`--stdin-eof` の 2 値と同じく値の組み合わせを背負うだけになる。

### 4. attach client は呼び出し元の stdin を子に流さない。キーは入力端末から読む

- attach client (単独の `hyoui attach` と、`hyoui run` が exec する attach) は stdin を子に流さない。単独の `hyoui attach S < file` は bash の `fg` と同じく子の fd を差し替えない。稼働中の子へ流し込む時は `hyoui input` を使う
- attach client の入力端末は「stdin が tty なら stdin、そうでなければ `/dev/tty`」とする。raw 化、SIGWINCH、外側端末のサイズ、キーの読み取り、Ctrl+Z ガード (DR-0029) は全部この入力端末を対象にする
- `/dev/tty` が開けない時 (制御端末が無い。Claude の Bash ツールでは ENXIO) は、キー入力なしで出力を中継する。エラーにしない。終わり方は子の exit (exit code を伝える) か `hyoui detach` か接続の喪失
- stdin が tty でない attach client は、自分の fd 0 を `/dev/null` に置き換えてから中継を始める (`hyoui run` が exec した attach が pipe の読み手として残ると、決定 2 の EPIPE が書き手に届かない)
- 入力端末の EOF / read error は今どおり自分から離脱する (detach)。子には何も送らない
- `hyoui run` が子の初期サイズを外側端末から取る時も同じ規則で入力端末を選ぶ (stdin が pipe でも、`/dev/tty` があればそのサイズで子を起動する)

理由: キーは端末から子へ届くもので、stdin のデータとは別物である (bash で pipe + キーボードのプログラムを使う時と同じ)。単独の attach が stdin を流すと、run と attach で非 tty stdin の意味が分かれる。

### 5. 消すもの

- `--stdin-eof` (run / attach)、EOF での EOT の送出 (改行で終わらない入力の 2 個を含む)、`hyoui::stdin_eof`、daemon の stdin 転送 (`daemon::stdin_forward`、reader thread と内部 pipe)、`DaemonizeInit.stdin_forward`、attach の `with_stdin_eof_action` / `send_stdin_eof`
- v1.0 前なので互換は持たない (`--stdin-eof` は未知の option としてエラーになる)

## Rejected alternatives

| 案 | 理由 |
|---|---|
| bytes を PTY master に流し、EOF を EOT で送る (DR-0019 §5) | 子からはキー入力に見え、`claude <<<prompt` が送信されない。ISIG / ICRNL / echo がかかりバイナリが運べず、EOF の模倣は子ごとに個数が違う |
| 流し込み時に LF を CR に変える / 子が raw になるまで待ってから送る | claude では直らない (実測)。直ったとしても子の内部状態を推測する介入になる |
| `/dev/null` を「入力なし」として特別扱いし子の stdin を PTY にする | 直接実行の `prog </dev/null` とずれる。種類で分けず、PTY にしたい起動元は決定 3 で明示する |
| `--pty-stdin` を既定にし、渡す方を指定にする | 直接実行と揃えるのが本 DR の目的で、既定が非透過になる。`tmux new -d` 相当は呼び出し元に端末が無い時の特殊な使い方 |
| 単独の attach で非 tty stdin を子の PTY に流す | 稼働中の子の fd 0 は後から差し替えられず、PTY に流すのは本 DR が捨てる形そのもの。run と attach で意味が分かれる。流し込みは `hyoui input` が担う |
| `/dev/tty` が無い attach をエラーにする | Claude の Bash ツールや CI から `hyoui run` / `attach` を使えなくなる。出力の中継と exit code の伝搬は端末が無くても成り立つ |
| fd を SCM_RIGHTS で daemon に送る | daemon は run が spawn する子プロセスなので継承で足りる。protocol と後始末が増える |
| daemon が fd を持ち続け、子の exec 後に閉じる | 子が先に stdin を閉じたり終わったりしても書き手に EPIPE が届かない。upgrade の self-exec にも漏れる |

## Consequences

- `printf 'a\003b\000c\n' | hyoui run -- od -c` が直接実行と同じ出力になり、`claude <<<prompt` が直接実行と同じく最初のプロンプトとして送信される
- 子の `isatty(0)` は stdin が非 tty の時に偽になる (直接実行と同じ)。python / bash は stdin が pipe なら非対話で動く
- 端末から切り離した起動元 (agent の Bash ツール、web gateway、`</dev/null` を付けた script) が `hyoui run --detached -- bash -i` のような対話の子を作ると、子は stdin の EOF で終わる。残したい時は `--pty-stdin` を付ける (DR-0039 決定 1 の gateway のログイン shell も同じ)
- `hyoui run -- cmd < file` を端末から実行すると、attach client のキーは `/dev/tty` から子の PTY に届く。`cmd` が stdin を読むプログラムならキーは `cmd` の stdin には届かない (直接実行と同じ)
- record (DR-0016): 非 tty stdin の bytes は hyoui を通らないので `in` event に載らない。attach client のキーは今どおり `in` event に載る
- upgrade (DR-0028): daemon が stdin の fd を持たないので、upgrade で転送が途切れる問題は無くなる
- 呼び出し元が stdin と同じ pipe の書き側を CLOEXEC 無しで持ったまま `hyoui run` を起動すると、daemon と子がその書き側も継承し、子に EOF が来ない。hyoui は継承した無関係の fd (3 以降) を閉じない (直接実行で子が同じ fd を継承した時と同じ)
- breaking change: `--stdin-eof` は消える。v1.0 前の方針の範囲

## 実装の当たり所

- `crates/hyoui/src/sys/raw.rs`: `openpty_fork_anchor_exec` / `forkpty_then_exec_legacy` の子側で、fd が渡されていれば fd 0 にそれを dup2 する (無ければ slave)
- `crates/hyoui/src/sys/pty.rs` / `crates/hyoui/src/daemon/session.rs`: spawn に子の stdin を渡す口 (`Session::start_with_child_stdin`)。spawn 後に fd を閉じる
- `crates/hyoui-cli/src/daemonize.rs`: `DaemonizeInit.child_stdin`、fd 0 の CLOEXEC 付き複製と `/dev/null` への置き換え
- `crates/hyoui-cli/src/main.rs`: run は非 detached でも stdin を daemon に継承させる。attach は入力端末の選択 (stdin か `/dev/tty`)、無い時の出力だけの中継、fd 0 の `/dev/null` 化
- `crates/hyoui/src/client/attach.rs`: 入力の無い中継 (`run_output_only`)、EOF の EOT 経路の削除
- `crates/hyoui/src/cli.rs` / `crates/hyoui-cli/src/completion.rs` / `docs/MANUAL*.md`: `--pty-stdin` の追加と `--stdin-eof` の削除を help / completion / manual で同時に

## 検証要件 (DR-0014 マトリクス)

直接実行と並べた期待 vs 実態を、line-oriented (`cat` / `od -c`) × REPL (`python3 -I` / `bash`) × TUI (`fzf` か `less`、claude) で埋める。加えて:

- バイナリ (0x03 / 0x00) が化けない、改行で終わらない入力でも子が EOF で終わる
- 子の `isatty(0)` が偽で `isatty(1)` が真、`--pty-stdin` では両方真で `hyoui input` が届く (`--detached` の有無の両方)
- daemon が fd を持ち続けない: 子の終了前に書き手が閉じれば子は EOF、子が先に終われば書き手に EPIPE
- `/dev/tty` の無い起動元からの `hyoui run` が出力を中継し、子の exit code を返す

## 関連

- [[DR-0019]] — run オプション棚卸し (§5 を本 DR が置き換える。§5 以外は有効)
- [[DR-0005]] / [[DR-0014]] — 透過原則と介入 self-check
- [[DR-0015]] — run = fork daemon + exec attach (stdin の継承経路)
- [[DR-0017]] — session anchor (子の制御端末は PTY のまま)
- [[DR-0028]] — upgrade の self-exec (fd 0 に pipe を残さない理由)
- [[DR-0029]] — attach は覗き窓、Ctrl+Z ガードは入力端末に効く
- [[DR-0039]] — gateway からログイン shell を作る時の `--pty-stdin`
- `docs/issue/2026-10-05-design-pipe-stdin-pass-fd-to-child.md` — PoC とシェルとの対応、STDIN-Q1 の裁定
