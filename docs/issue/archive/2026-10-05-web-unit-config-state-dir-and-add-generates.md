---
title: web の unit config に state_dir を必須で持たせ、daemon add が config を生成する (DR-0038 の追補)
status: resolved
category: task
created: 2026-10-05T11:30:00+09:00
last_read: 2026-10-06T12:50:00+09:00
open_entered: 2026-10-05T11:30:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-06T12:50:00+09:00
discard_reason:
pending_reason:
close_reason: DR-0038 決定 9 として実装 (add の生成と state_dir の照合、run の 3 形態、add は登録簿の排他 lock の中、state_dir は unit の config ファイル自身に要求)。reference cli-daemon-subcommands の修正と実機 toml への state_dir 追記も済み
blocked_by:
---

# web の unit config に state_dir を必須で持たせ、daemon add が config を生成する (DR-0038 の追補)

kawaz と合意済み (2026-10-05)。実装と DR-0038 への追記が残っている。面と置き場の前提は DR-0041 決定 6 (面は `HYOUI_STATE_DIR` 1 つで決まる、config は面で分けず共有)。

## 合意した仕様

- **unit の config は `<unit>.toml`**。ファイル名に面の key 等を入れない (面は中の `state_dir` で分かる。ファイル名にも入れると二重管理になる)
- **unit の config の `[web]` に `state_dir` を必須で書く**。`daemon add` / `daemon run` は config の `state_dir` と今の面の状態の root を realpath で正規化して比べ、食い違えば「この config は面 X のもので、今は面 Y で実行している」というエラー。面同士は互いの登録簿を見られないので、別の面の config を (コピー等で) 登録・起動した事故に気付ける場所は config 自身しかない。`state_dir` は面をまたいで共有する土台 (`base.toml`) には書かない
- **`daemon add <name> [--listen ...] [--binary ...]` が config を生成する**: `~/.config/hyoui/web/<name>.toml` に `extends = "base.toml"` (土台があれば)、`state_dir` (今の面の root)、`listen`、`binary_path` を書いて登録する。既にそのファイルがあれば生成せず、中の `state_dir` が今の面と食い違えばエラー (別の面が同じ名前を使っている。黙って上書きしない)。パスを明示した add は既存ファイルを登録するだけ (同じく `state_dir` を確かめる)
- **name に既定値は持たせない** (`default` という名前は既定値を管理しているように見えるため)。name を省いた add は help。**listen の既定値 (`127.0.0.1:43690`) は残す**。add の時点で、同じ面の登録簿に同じポートの unit が無いか、そのポートを今ほかのプロセスが listen していないか (実際に bind を試す) を確かめ、当たれば「使用中、`--listen` で指定する」とエラー (空いているポートを自動で選ばない)
- **`daemon run`**: `daemon run <unit>` (登録簿 → config) / `daemon run --config <path>` (登録簿を通さずその config、`state_dir` は確かめる) / `daemon run --no-config` (config を読まず組み込みの既定値と CLI 引数だけ。テスト向け、例 `HYOUI_STATE_DIR=<一時dir> hyoui web daemon run --no-config --listen 127.0.0.1:0`)。何も付けない `daemon run` は help。**名前を省いた時に `config.toml` の `[web]` で起動する経路 (`crates/hyoui-cli/src/web_daemon/mod.rs` 722 行付近) を消す**。web の gateway が `config.toml` を読む経路は無くなり、`[web]` 節は不要になる
- **何を読むか**: `run <unit>` も `--config` も、その config ファイルと `extends` でたどれるファイルだけを読む。共通の `config.toml` は暗黙に読まない (現行の実装どおり、確認済み)
- **監督者**: plist に載るのは `hyoui web daemon supervise` だけ。supervise は自分の面の登録簿を読み、unit ごとに `<binary_path> web daemon run <unit>` を子として起動する (子に `--config` は渡さない。config のパスの正本を登録簿 1 か所に保ち、ps で unit 名が読める)

## 関連して直すもの

- reference `cli-daemon-subcommands` の `run [unit]` の「未指定の場合はデフォルト」を「unit の名前に既定値を持たせるかは案件が決める」に直す (`default` という名前が誤解を招く事情は hyoui に限らない)
- 移行済みの実機 `~/.config/hyoui/web/stable.toml` / `unstable.toml` に `state_dir = "/Users/kawaz/.local/state/hyoui"` を足す (実装が入る前に足しても無害)
- `--help` / completion / MANUAL (ja / en) を同時に追従
