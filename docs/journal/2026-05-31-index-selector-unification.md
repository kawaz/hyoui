# 2026-05-31 — session selector `--index=N` の共通化

session-targeted subcommand 全体で session 指定の流儀を統一した経緯のメモ。

## ゴール

multi-session 運用で「session-id を逐一コピペするのが面倒」という kawaz UX 課題 (= `docs/issue/2026-05-30-feature-attach-index-shortcut.md`) を解消する。

「list の mtime 順で N 番目」を指定する短縮記法を全 subcommand で共通の流儀にする。

## 設計選択と決定

| 観点 | 当初実装 (= 撤回) | 最終仕様 |
|---|---|---|
| 短縮指定の文法 | 位置引数の整数 = index (= `attach 1` で 1 番古い) | **位置引数の整数 = session-id 名 (= `attach 1` は session-id `"1"`)** |
| index 指定 | (位置引数の数字経由) | **`--index=N` 専用** (= `1` = 1 番古い、`-1` = 1 番新しい、`0` reject) |
| `kill -N signal` | 維持 | 維持 (= POSIX 慣習) |
| `--all` | kill のみ実装 | **kill 専用維持** (= 他 subcommand はケースバイケース、現状追加なし) |
| 適用範囲 | attach, kill のみ | **attach / kill / status / tail / wait / screen dump / screen snapshot / lock acquire / lock release / unlock / input** (= 全 session-targeted subcommand) |

### なぜ「位置引数の整数 = index」を撤回したか

kawaz の最初の発言例 (`attach 1` `attach -1`) を「位置引数の数字 = index」と解釈して実装したが、kawaz の意図は「index 指定用オプションが欲しい」であって「位置引数を index 扱いしてくれ」ではなかった (kawaz 2026-05-30 訂正)。

位置引数 = session-id (数字でも) / `--index=N` = index、で **流儀を直交させる**方が:
- session-id が数字始まりのケースを曖昧さなく扱える
- `--` セパレータが attach では不要になり、parser simple
- 全 subcommand で同じ流儀を共通展開しやすい (= helper `parse_session_targeted` を 3-tuple 化して横展開)

## 実装 commit 列

| commit | 内容 |
|---|---|
| `997b0a2b` | attach に `--index=N` + 位置引数の数字 index 対応 (= 撤回前) |
| `f569ddd7` | kill に `-N` POSIX short flag / 略名 / 数字 normalize / `--all` / `--index` |
| `a21bf67a` | **位置引数の整数→index 解釈を撤回** (= attach + kill 両方)。`--index=N` 専用に |
| `0deeac56` | helper `parse_session_targeted` を `(socket, session_id, index)` 3-tuple に拡張、status / tail / wait / screen dump / screen snapshot / lock acquire / lock release / unlock に共通展開。`parse_wait` のみ独自 parser のため別途実装 |
| `f2778c65` | usage 8 個に `--index` 説明追加 + input family にも `--index` 展開 |

## ハマり所と解決策

### 1. Edit が rustfmt 後に invalidate される

cargo fmt が走った後の main.rs は Read を再実行しないと Edit が「File has not been read yet」で失敗する。**fmt 走った直後の連続 Edit は Read を挟む**。

### 2. Config literal の `index: None` 追加が 8〜9 箇所

`StatusConfig` / `TailConfig` / `WaitConfig` / `ScreenDumpConfig` / `ScreenSnapshotConfig` / `LockAcquireConfig` / `LockRelaseConfig` / `InputCommand` の test 内 literal 全てに `index: None` 追加が必要。`session_id:` の後に挿入するパターンを各箇所で identifier (= `ScreenDumpCliFormat::Ansi` 等) を hint に個別 Edit する。replace_all は InputCommand の literal を巻き込むため不可。

### 3. `parse_session_targeted` の戻り型変更で clippy `type_complexity` 警告

3-tuple が 3 種類の型なので "very complex type" 扱い。helper は `pub fn` ではないので type alias 化せず `#[allow(clippy::type_complexity)]` で抑止。

### 4. `parse_wait` だけ helper を使えない

wait は positional 2 個 (session-id + predicate) で他の subcommand (positional 0〜1 個) と signature が違う。`--index=N` 指定時は positional 1 個 (predicate のみ) に切り替える `selector_present` 判定を `parse_wait` 内で個別実装。

## 実機検証 (= matrix)

3 session 立てて以下を確認 (commit `0deeac56` 時点 + commit `f2778c65` 時点):

| subcommand | `--index=1` (= 最古) | `--index=-1` (= 最新) | `--index=0` (= reject) |
|---|---|---|---|
| attach | OK | OK | error |
| kill | OK | OK | error |
| status | OK | OK | error |
| tail | OK | OK | error |
| wait | OK (timeout) | OK | error |
| screen dump | OK | OK | error |
| input | OK (= `text:hello key:Enter` → tail で echo back 確認) | OK | error |

cleanup: `kill --all` で 3/3 terminated を確認。

## 残作業

- `docs/issue/2026-05-30-feature-list-format-improvement.md`: cwd / argv 表示 (= protocol 拡張要、kawaz 相談待ち)
- `--all` の他 subcommand 展開 (= kawaz「ケースバイケース」、現状追加なし)

## 関連

- DR-0006 (CLI ground rules) — session selector の流儀統一はこの DR の範疇、新 DR 不要
- `docs/issue/2026-05-30-feature-attach-index-shortcut.md` — 起点 issue (Closed)
