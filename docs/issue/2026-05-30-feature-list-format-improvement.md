# feature: `hyoui list` の表示形式改善 (= 固定長 + cwd / argv 表示 + `--format=jsonl`)

- Date: 2026-05-30
- Priority: 中 (= UX 改善、多 session 運用時に効果大)
- Status: **Closed (2026-05-31)** — cwd / argv / clients 表示が完成。実装サマリ:
  - `DaemonConfig.cwd` を追加、`run_daemon_child` の `chdir("/")` 直前で `std::env::current_dir()` を capture
  - `StatusResponse` に `cwd` / `argv` を optional field で追加 (= cap flag 不要、backward compatible)
  - `hyoui list` が live socket に対し並列で status.query を投げて cwd / argv / clients を取得 (per-query 300ms / overall 500ms timeout、自前低レベル handshake + `UnixStream::set_read_timeout` で実装)
  - plain format は `SESSION STATUS DUR CLIENTS CWD ARGV` の 6 列、SOCKET 列は jsonl 側のみに残す (= kawaz の「socket 名だけ出されても分からん」要件、機械可読は jsonl 側を使う前提)
  - cwd shorten: `<...>/repos/<host>/<owner>/<repo>/<sub>` → `<owner>/<repo>/<sub>`、それ以外は `$HOME` 前カット (`~/...`) のみ適用
- 報告者: kawaz 発言 (2026-05-30)

## 背景

kawaz の発言:

> list が見にくい。固定長フィールドを左にしつつ、cwd や実行コマンドなどが見られると何のプロセスかわかる。
> ソケット名とかだけ出されても分からん。起動日時 / DUR / ステータス / 接続数 / cwd (repos/github.com/ なら前カット) / コマンド引数 を 1 行で、
> cwd 前あたりまでは固定長で出して欲しい。`--format=jsonl` option も。

## 現状

`crates/hyoui-cli/src/main.rs::list_command_with_dirs` (= 643-700 行) の出力:

```
<session-id>\t<status>\t<socket-path>
```

session メタデータ (= 起動日時 / cwd / argv 等) は持っていない。socket file path しか分からないので、何の process が動いているか UX 上判断つかない。

## 表示したい情報 + 取得経路

| 項目 | 取得経路 | 実装コスト |
|---|---|---|
| **起動日時** (start time) | socket file mtime (= `fs::metadata`) で代替可。daemon に SystemTime を持たせるなら正確 | 低 (mtime) / 中 (protocol 拡張) |
| **DUR** (経過時間) | start_time から `now()` 引き算 | 低 |
| **status** (live/stale) | 既存 `probe_socket_liveness` | 既存 |
| **接続数** | daemon の `StatusResponse.clients` で取得可。ただし list 時に各 socket に query は O(N) | 高 (or list 時は省略 → `status` 案内) |
| **cwd** | daemon が記録していない。`child_pid` から取得 (macOS = `libproc` / Linux = `/proc/<pid>/cwd`) | 中 |
| **コマンド引数** (argv) | daemon の `DaemonConfig.cmd` に保持済、protocol response に追加すれば取得可 | 低 (protocol 拡張) |

## 設計案

### Stage 1: client side のみ (= daemon protocol 不変)

`hyoui list` の中で:
- socket mtime から起動日時 / DUR 計算
- live/stale 既存通り
- cwd / argv は **取得しない** (= "?" 表示 or 省略、後段 `hyoui status <id>` で参照案内)
- `--format=jsonl` で機械可読出力

実装範囲: 50-80 lines。breaking なし。

### Stage 2: daemon protocol 拡張

`StatusResponse` に追加:
- `start_time: SystemTime`
- `cmd: Vec<String>` (= argv、`DaemonConfig.cmd` を露出)
- `child_pid: u32` (= cwd 取得用)

`hyoui list` 時に各 live socket に短時間 `status` query → 上記取得。
- O(N) 問題: 通常 N は小さい (= 〜10 session)、100ms timeout でも合計 <1s
- cwd は client 側で `child_pid` から `libproc` (macOS) / `/proc/<pid>/cwd` (Linux) で取得

実装範囲: 100-150 lines + protocol breaking (= cap negotiate 要)。

### 表示フォーマット案 (案 X、kawaz 確認用)

```
SESSION              STATUS   STARTED      DUR     CWD                          COMMAND
test-claude          live     17:23:45     1h2m    kawaz/hyoui/main             claude
test-vim             live     18:10:02     15m     kawaz/foo                    vim notes.md
stale-test           stale    -            -       -                            -
```

- `SESSION` 固定 20 chars (= overflow は `…` で truncate)
- `STATUS` 固定 8 chars
- `STARTED` 固定 12 chars (= HH:MM:SS、当日以外は MM-DD HH:MM)
- `DUR` 固定 8 chars (= human readable `1h2m` `15m` `3d`)
- `CWD` 可変 max 30 chars、`repos/github.com/` 自動 strip (= `~/.local/share/repos/github.com/kawaz/hyoui/main` → `kawaz/hyoui/main`)
- `COMMAND` 残り全幅、長すぎたら truncate

### `--format=jsonl` 出力例

```jsonl
{"session":"test-claude","status":"live","socket":"/tmp/...","started":"2026-05-30T17:23:45Z","dur_ms":3725000,"clients":2,"cwd":"/Users/.../kawaz/hyoui/main","argv":["claude"]}
```

完全機械可読、script 用。

## kawaz 確認ポイント

1. **Stage 1 で先行 (= cwd / argv なし版) → Stage 2 で完全版** で進めて OK か、それとも **Stage 2 を一気に** やるか
2. **接続数 (clients)** を list で表示するか、`status` 案内にして省略するか (= O(N) query の代償)
3. **CWD の前カット rule**: `repos/github.com/` 固定で OK か、それとも env (`HYOUI_LIST_CWD_STRIP_PREFIX=...`) で柔軟化するか
4. **表示フォーマット案 X** で良いか、列順 / 幅は別案あるか
5. **既存 plain format** との互換性: `--format=plain` を後方互換に残すか、表示変更で **breaking change** にするか (= `hyoui list | awk '$2=="live"'` のような script 使われていれば影響)
6. **Stage 2 protocol 拡張**: cap `list-metadata-v1` を作って negotiate するか、StatusResponse を素直に拡張するか

## 進行中議論との衝突

`docs/issue/2026-05-28-feature-cli-restructure-discussion.md` (= CLI 再編議論) は `screen dump --format=...` 等を扱うが、`list --format=jsonl` には言及なし。**衝突なし**、独立 feature として進められる。

## 関連

- `crates/hyoui-cli/src/main.rs::list_command_with_dirs` (= 643-700 行)
- `crates/hyoui/src/daemon/messages/` (= StatusResponse protocol)
- `crates/hyoui/src/cli.rs::usage_list` (= help 更新も要)
- `~/.claude-personal/rules/cli-design-preferences.md` — 固定長 + 機械可読の好み
- `docs/findings/2026-05-27-cmux-msg-hyoui-integration-feedback.md` §B6 (= 個別 --help 取りこぼし指摘、help 整備の隣接 task)
