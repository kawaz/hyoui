# `hyoui run --pty-stdin`

**一言で言うと**: 子の stdin も hyoui の PTY につなぎ、呼び出し元の stdin は使わない。

仕様の正本は [DR-0042](../decisions/DR-0042-non-tty-stdin-is-the-childs-fd.md) の決定 3。この文書は使い方の説明。

## 子の stdin は何になるか

hyoui は子を起動する時、PTY を必ず新しく作る。子の stdout / stderr と制御端末は、いつもこの PTY。違いが出るのは子の stdin だけ。

| 起動のしかた | 子の stdin |
|---|---|
| 端末から `hyoui run -- cmd` | PTY |
| `printf … \| hyoui run -- cmd` (stdin が端末でない) | 呼び出し元の pipe をそのまま渡す。シェルの `\|` と同じ配線 |
| `hyoui run --pty-stdin -- cmd` | 呼び出し元の stdin に関係なく PTY |

## いつ使うか

端末を持たない起動元 (agent の Bash ツール、web gateway、script) から、後で外から操作し続ける shell や REPL を作る時に付ける。

```sh
SESS=$(hyoui run --detached --pty-stdin -- bash -i)
hyoui input "$SESS" 'text:ls' key:Enter
hyoui screen dump "$SESS"
```

付けないと、子の stdin は呼び出し元の `/dev/null` (または socket) になる。`bash -i` はそこから EOF を読んですぐ終わる。仮に終わらなくても、bash は fd 0 を読むので、PTY に書く `hyoui input` は届かない。`docker run … bash` を `-it` 無しで起動するとすぐ終わるのと同じ。

python の REPL や `cat` のように stdin を読むプログラムも、同じ理由で `--pty-stdin` が要る。

## 付けない方がよい時

claude / vim / fzf / less などの TUI は、stdin が何であってもキーを `/dev/tty` (= hyoui の PTY) から読む。付けなくても `hyoui input` や attach のキーは届く。

pipe で最初の入力を渡したい時は、付けてはいけない。付けると pipe の中身は使われない。

```sh
# 最初のプロンプトとして送信される (付けない)
printf '準備して待て' | hyoui run --detached -- claude
```

## 付けた時にキーが届く経路

呼び出し元の stdin は、子にも PTY にも流れない。子に届くのは次の 2 つだけ。

- `hyoui input` (どこからでも)
- attach (呼び出し元に端末がある時の打鍵)

ファイルの中身を PTY への入力として流したい時は `hyoui input <id> file:<path>` を使う。

## 背景: fd と制御端末

プロセスには 2 種類の経路がある。fd 0 / 1 はプログラム同士がデータを受け渡す横の流れで、`|` や `<` / `>` で付け替えられる。制御端末は人や操作する側とつながる縦の軸で、キー・画面・`^C` の signal が通る。`/dev/tty` を開くと、fd 0 がどこを指していても制御端末が開く。

hyoui は縦の軸 (PTY) だけを握り、横の流れはシェルと同じように呼び出し元の配線を子に渡す。`--pty-stdin` は、その横の入口 (fd 0) も縦の軸に寄せる指定。図と詳細は [DESIGN の「子の fd と制御端末」](../DESIGN-ja.md) を参照。
