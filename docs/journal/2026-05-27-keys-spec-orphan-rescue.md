# Orphan DR-0009 (= keys subcommand 仕様) の救出 log

- Date: 2026-05-27
- 対象 orphan commit: `xmwzuslx f95dd097 docs(DR-0009): hyoui keys subcommand 仕様確定`

## 来歴

本セッション (c7988b6b、2026-05-27) で v0.2.0 の最初の自動操作コマンド `hyoui keys` を実装する
Agent を `v020-keys` workspace で起動 (ac2a17910e0ef559d)。Agent は DR-0009 (= keys 単独 subcommand
仕様) を起票する commit を作った段階で、ユーザの「v0.2.0 は既存 DR や議論を見返して進め方からいきますかね」
の指示で TaskStop された。

その後:

- 別 Agent が **同じ DR-0009 番号で `session.rs` 責務分割の DR** を起票 (= 現 main に乗っている
  `vyryvozl 0796249f docs(DR-0009): session.rs 責務分割の設計を起票`)
- keys 単独 subcommand の仕様は **DR-0010** (= v0.2.0 scope re-scope) で `hyoui input keys`
  (= `input` family の 1 子) に統合する方針に変更
- 結果として orphan の `xmwzuslx` は (1) DR-0009 番号衝突 (2) 単独 subcommand 前提が
  DR-0010 で覆された、の 2 重で main 統合不可能な状態に

## 本ファイルの位置づけ

orphan を abandon する前に内容を journal に救出。v0.2.0 input keys 実装時に再活用する。

DR-0006 §9 (CLI ground rules) でも keys spec の概要は確定済だが、orphan DR-0009 にはより
詳細な記述 (= key name table の完全版、Rejected alternatives、protocol 非拡張方針の論拠) が含まれる。
これらは v0.2.0 input keys 実装での DR/コードコメントの素材になる。

## 救出した内容 (= orphan DR-0009 の原文をそのまま貼り付け)

> 注: 以下は **orphan であり main 仕様ではない**。v0.2.0 input keys 実装時に DR-0006 §9 と
> マージして input family の DR に再構成する想定。「DR-0009」の表記は historical reference として
> 残すが、現 main の DR-0009 (= session.rs 責務分割) とは別物。

---

# Keys subcommand spec (= orphan DR-0009 救出版)


- Status: Active
- Date: 2026-05-27
- Related: DR-0005 (思想), DR-0006 (CLI ground rules), DR-0007 (MVP scope と段階リリース),
  DR-0008 (protocol)

## Context

[[DR-0007]] の v0.2.0 自動操作 API として `hyoui keys <session> <spec>...` が確定済。
v0.1.0 で土台 (daemon + multi-attach + protocol cap negotiation + raw PTY write) は
release 済。本 DR は v0.2.0 最初の自動操作コマンド `keys` の **仕様と実装方針** を
確定する。

`keys` の目的は「外部スクリプトから session の PTY に **構造化されたキー列** を流す」
こと。素朴な raw byte 送信 (= `send`) と違って:

- `key:Enter` / `key:Ctrl-C` のような **意味のあるキー名** を escape sequence に
  自動変換する (= ユーザが xterm 表を覚えなくて良い)
- `wait:1s` / `wait-idle:500ms` を spec 列の中に挟める (= 「Enter 押して 500ms 静止
  待ち」のような一連の操作を 1 コマンドで書ける)
- `text:<string>` / `key:<name>` / `wait:<dur>` / `wait-idle:<dur>` の 4 prefix が
  spec syntax の正本

## Decision

### Spec syntax

`hyoui keys <session> <spec>...` の各 spec は **prefix で 1 種類に確定** する:

| Prefix | 引数 | 動作 |
|---|---|---|
| `text:<string>` | 任意の UTF-8 文字列 | bytes (UTF-8) として PTY に raw write |
| `key:<name>` | 後述の key 名 | escape sequence にマッピングして raw write |
| `wait:<duration>` | `5s` / `1m30s` 等 (DR-0007 duration format) | 固定 sleep |
| `wait-idle:<duration>` | 同上 | PTY 出力が `duration` 期間来ない (= idle) まで待機 |

spec 列は **左から順に execute**。途中で wait/wait-idle が来たら待機、完了後に次の
spec へ進む。全 spec 完了で client は socket close + exit 0。

#### Key 名一覧 (v0.2.0 で対応)

xterm 互換 escape sequence。**case-insensitive** (`key:enter` も `key:Enter` も OK)。

| Key | Bytes | Comment |
|---|---|---|
| `Enter` / `Return` | `\r` (0x0d) | CR (PTY 入力慣例) |
| `Tab` | `\t` (0x09) | |
| `Esc` | `\x1b` | |
| `Space` | ` ` (0x20) | |
| `Backspace` | `\x7f` (DEL) | terminal 慣例 |
| `Up` | `\x1b[A` | CSI A |
| `Down` | `\x1b[B` | |
| `Right` | `\x1b[C` | |
| `Left` | `\x1b[D` | |
| `Home` | `\x1b[H` | |
| `End` | `\x1b[F` | |
| `PgUp` | `\x1b[5~` | |
| `PgDn` | `\x1b[6~` | |
| `F1`..`F4` | `\x1bOP` / `\x1bOQ` / `\x1bOR` / `\x1bOS` | SS3 (xterm) |
| `F5` | `\x1b[15~` | CSI ~ |
| `F6`..`F12` | `\x1b[17~`..`\x1b[24~` | |
| `Ctrl-<X>` (X = a..z) | `(X - 'a' + 1)` (= 0x01..0x1a) | C0 制御文字 |
| `Alt-<X>` | `\x1b` + raw byte (X) | xterm meta as ESC prefix |

「Ctrl- 大文字」「Ctrl-小文字」「ctrl-X」「^X」表記すべて同義 (case-insensitive)。

不正な key 名 (例: `key:Banana`) は **parse-time error**。spec 列を 1 つでも parse
失敗したら何も送信せず exit 2 (= `hyoui: keys: ...` を stderr に書く)。

### Protocol

v0.1.0 の attach 経路 (= Mode::Rw + raw_data frame) を **そのまま流用**。`keys` 専用
の新しい protocol message は **作らない**。

```
client → daemon: HandshakeRequest (mode=Rw)
daemon → client: HandshakeResponse
[for each spec]
  Text(s)        → Frame::raw_data(s.as_bytes())
  Key(k)         → Frame::raw_data(key_to_bytes(k))
  Wait(d)        → client-side sleep(d) (= no frame sent)
  WaitIdle(d)    → client-side polling: PTY 出力 (= raw_data frame from daemon)
                   が duration `d` 来なければ idle として次へ進む
[end]
client closes socket → daemon は client detach として cleanup
```

`wait` / `wait-idle` は **すべて client 側のロジック**。daemon に新規 handler を
足さない。これにより:

- protocol の cap flag を増やさない (= v0.1.0 cap で動く)
- daemon の attack surface を増やさない
- v0.2.0 scope を「CLI client の追加」だけに抑える

`wait-idle` の polling は「最後に raw_data frame を受信してから duration 経過」を
判定。`recv_frame` を non-blocking で読み、`poll()` の timeout に duration を渡す
(= attach の poll loop と同パターン)。

### CLI 引数

```
hyoui keys <session-id> [--socket=<path>] <spec>...
```

- 位置引数: 先頭 1 つが session-id、それ以降が spec 列
- `--socket=<path>` で session-id を bypass (= 既存 subcommand と整合)
- `--help` で keys 専用 help を表示 (R4-H1 規約)
- spec 列に何も書かない場合 → error (= 何もしないなら本コマンドを使う意味がない)

### Lock 連携

本 DR では `--lock` 等の lock 連携 option は **追加しない**。v0.2.0 後半で `lock` /
`tx` を実装するときに、`keys` 内部の lock acquire/release を別 DR で追加する。
現状は raw write を素直に流すだけ。

### Exit code

| Code | Reason |
|---|---|
| 0 | 全 spec 送信完了 (= 正常終了) |
| 1 | connect / send 失敗 (= daemon が応答しない / socket I/O error) |
| 2 | spec parse error / 引数不足 |
| 3 | wait-idle timeout (= 将来 `--idle-timeout` 等で hard limit を入れる場合用に予約。
       v0.2.0 では未使用、wait-idle は **必ず成立する待機** として動く) |

## Rejected alternatives

### Daemon 側に `keys` 専用の構造化 control message を追加する

- 案: `ControlMessage::Keys(KeysRequest)` を新設し、spec 列を daemon に送って daemon
  側で逐次 execute する
- 却下理由:
  - protocol 拡張が必要 (= cap flag + message kind 追加、DR-0008 schema evolution の負担)
  - daemon に「待機・スリープ・key→bytes 変換」のロジックが増える → attack surface 拡大
  - v0.2.0 scope を CLI 側だけで完結させる方針に反する
  - raw write の素直な利用で同等の挙動が出せる (= `keys` は **client orchestration**
    だけで十分)
  - 利点 (= 「複数 client の interleaving 防止」) は `lock` で別途解決される

### `xdotool` 風の長いキー名表記 (`XK_Return` 等)

- xterm/screen/tmux 慣例の短い名前 (`Enter` / `Return`) を採用
- X11 keysym 名前空間は CLI 用途には冗長

### `key:Ctrl-Shift-A` / `key:Meta-A` のような multi-modifier

- 当面 Ctrl / Alt のみ。Shift / Super は使用頻度が低く、シフト表記は raw text で
  `text:` を使えば足りる
- 必要になった時点で別 DR で追加

### `wait:` を「fixed sleep」と「idle wait」の両義にする (alias 化)

- DR-0007 §v0.2.0 に書いた表は alias 表記だが、本 DR では **`wait:` = fixed sleep**、
  **`wait-idle:` = idle 待機** として明確に分離する
- 既存 `hyoui wait` subcommand の `wait:<dur>` (= idle のエイリアス) との混同を避ける
  ため、`keys` 内では別 prefix にする
- DR-0007 の表記は本 DR 確定後に「`keys` 内の `wait:` は fixed sleep」と注記する

## Consequences

### 実装

- `crates/hyoui/src/cli.rs`:
  - `Command::Keys(KeysConfig)` variant 追加
  - `KeyName` / `KeySpec` enum、`KeysConfig` struct
  - `parse_keys` + `parse_key_spec` + `parse_key_name`
  - `HelpTopic::Keys` + `usage_keys()`
  - top-level help にも `keys` を載せる
- `crates/hyoui/src/keys.rs` (新規 module):
  - `pub fn key_to_bytes(KeyName) -> Vec<u8>`
  - test: 全 key 名 → escape sequence のテーブル検証
- `crates/hyoui-cli/src/main.rs`:
  - `keys_command(cfg: KeysConfig) -> ExitCode` を追加
  - client connect + handshake (Mode::Rw) → spec 列を逐次 send
  - `wait-idle` は recv loop で「duration 内に raw_data 来なかったら次へ」

### test

- unit: `cli.rs` の parse_keys / parse_key_spec / parse_key_name の round trip
- unit: `keys.rs` の key_to_bytes table check
- unit: help routing (`keys --help` → Keys topic)
- integration (`tests/keys.rs`): daemon を `cat` で起動 → `keys` client が `text:hello key:Enter`
  を送る → cat 出力に "hello\r\n" 相当が現れる
  - 実態としては `cat` は input echo + read input なので `tail` で input echo を確認

### top help

```
SUBCOMMANDS:
    run, attach, list, kill, status, tail, wait, keys, completion
```

`keys` を 8 つ目として追加。

## 関連

- [[DR-0005]] — 思想 (= 外側自動操作主軸、daemon は raw write の通り道)
- [[DR-0006]] — CLI ground rules、`keys` の API surface 規約
- [[DR-0007]] — v0.2.0 自動操作 API の段階リリース計画
- [[DR-0008]] — protocol 設計、本 DR は protocol を**拡張しない**方針
- 出典: Round 4 / v0.1.0 release / R4-C2 の DR-0007 re-scope (= keys を v0.2.0 に押し出した経緯)
- [DR-0009](./DR-0009-keys-command-spec.md) — `hyoui keys` subcommand 仕様 (text/key/wait/wait-idle prefix、raw write 経路再利用、protocol 非拡張)
