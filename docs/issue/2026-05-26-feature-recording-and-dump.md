---
title: "Feature idea: tty I/O の dump / record / play subcommand"
status: wip
category: request
created: 2026-05-26T00:00:00+09:00
last_read: 2026-06-22T19:38:42+09:00
open_entered: 2026-05-26T00:00:00+09:00
wip_entered: 2026-06-01T00:00:00+09:00
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: hyoui paste API 設計議論で --spool-append のユースケースを検討した際に派生
---

# Feature idea: tty I/O の dump / record / play subcommand

- Priority: Low (v0.3.0 以降検討)

継続録画部分のみ [DR-0016](../decisions/DR-0016-tty-io-record.md) で MVP scope 切り出し、ctrl-z 系 bug 解析の観測道具として優先実装済 (= `hyoui record` 命名、`hyoui screen dump` 静止画と分離)。`hyoui play` (= I/O 注入再生) / `hyoui sink` 抽象化 / `--rotate` / asciinema cast format は **本 issue に残置**、v0.3.0 以降検討。

## 関連する 3 つの feature

### 1. `hyoui dump` — live I/O 流し続け

```
hyoui dump start <name> [--file PATH] [--stdin | --stdout | --both] [--rotate ...]
hyoui dump stop <name>
hyoui dump status <name>
hyoui dump list <name>
```

- 子の I/O (stdin/stdout/両方) を file に live で書き続ける
- `tail` の永続化版、`tee` を hyoui daemon 内で持つイメージ
- `--rotate` で size/time ベースのローテーション (運用ログ的に)
- 用途: 障害解析、操作履歴の永続化、CI 中の子プロセス挙動完全記録

### 2. `hyoui record` — completed session の cast 形式記録

```
hyoui record <name> --file PATH [--format=asciinema|raw]
hyoui record stop <name>
```

- 子の I/O を **asciinema cast format** (.cast 0.2 形式) で記録、タイミング情報込み
- asciinema 互換なら `asciinema play recording.cast` で再生可能 (= ターミナル録画として汎用)
- `--format=raw` なら bytes そのまま + 時刻メタ (自前 format)

### 3. `hyoui play` — 記録した session を別 daemon に再生

```
hyoui play <name> --file PATH [--speed 1.0] [--input-only] [--output-only]
```

- 記録した session を別 hyoui daemon に再生 (= ユーザの手作業を record して後で play で自動化)
- `--speed` で再生速度調整 (デモ/CI 用途)
- `--input-only`: 入力 (stdin) だけ再生 (= 自動操作シーケンスとしての利用)
- `--output-only`: 出力だけ再生 (= 「過去のセッションを画面で見直す」)

**自動操作主軸 (DR-0005) の思想にズバ刺さる**:
「手で 1 回 record → 後は play で何度でも再現」が CLI で完結する。
従来 `expect` script や CI script で書く工程を、record 1 発で雛形化できる可能性。

## dump vs record の使い分け

| | dump | record |
|---|---|---|
| 形式 | live stream (継続書き込み) | completed session (タイミング込み) |
| 再生 | 想定外 (= log) | play で再生可能 |
| 用途 | 障害解析、運用ログ | 自動化雛形作成、デモ |
| ローテーション | あり (`--rotate`) | なし (1 session = 1 file) |

両者共存して別 subcommand の方が UX 明快。

## 統合設計: sink 概念

dump/record/play は本質的に近い (= daemon の I/O を file に書く/file から戻す)。
更に tail (ad-hoc bytes stream client) とも「出力先の違いだけ」に見える。整理:

| | tail | dump (= sink) |
|---|---|---|
| 誰が動かす? | CLI client プロセス (別プロセス) | daemon 内の sink (daemon プロセス内) |
| ライフサイクル | ad-hoc、CLI 終了で止まる | 永続、CLI client と独立 |
| 複数持てる? | プロセス起動分だけ複数 | daemon 内に複数 sink |
| 出力先 | stdout / `--output FILE` | file |
| format | raw bytes | raw / cast |
| client 切断耐性 | 切断で止まる | 切断しても継続 |

**sink 概念で dump/record/play を統合**する設計案 (v0.3.0):

```bash
# daemon 内永続 sink (= dump 相当、daemon の寿命と一致)
hyoui sink add <name> --output FILE [--format=raw|cast] [--rotate=size:10MB,age:1h]
hyoui sink remove <name> <sink-id>
hyoui sink list <name>

# record = sink add の cast format alias
hyoui record <name> --output FILE         # = hyoui sink add <name> --output FILE --format=cast

# play は別物 (sink ではなく source、file → daemon に注入)
hyoui play <name> --input FILE [--speed 1.0] [--input-only|--output-only]
```

tail との関係:
- tail = ad-hoc、CLI client が daemon に「broadcast を私に流して」と要求、stdout 出力
- sink = daemon 内、client 独立、file 出力 (record は cast format)
- `hyoui tail --output FILE` (v0.2.0 後付け候補) は **ad-hoc 簡易 sink** に位置付け (tail プロセス生きてる間だけ file に書く)

## 段階整理

| 機能 | 段階 |
|---|---|
| tail (ad-hoc stdout) | v0.1.0 MVP |
| tail --output FILE | v0.2.0 後付け (実装は client 側完結、protocol 変更なし) |
| sink concept (永続) | v0.3.0+ |
| record (cast format sink) | v0.3.0+ |
| play (file → daemon 注入) | v0.3.0+ |

## 実装上の検討点

- daemon は既に pty 出力を全 attach client に broadcast している → dump/record は client の一種 (= file sink)
  として実装可能、protocol 拡張は最小限
- `record` の cast format は asciinema 互換が筋 (= 既存 viewer/converter 資産を活用)
- `play` の入力再生は lock + tx 機構と統合 (= play 中は他 client の rw を一時降格)
- `play --speed` で時間軸を変更する場合、output だけは無視できるが input は子の応答待ちと整合させる必要 (= input only モードは無条件タイミング、output 反映モードは prompt 待ちと連動)

## 関連

- [[DR-0005]] — 外側自動操作主軸の思想 (record/play は思想ど真ん中)
- [[DR-0006]] — `--spool-append` 廃止判断の延長
- [[DR-0007]] — v0.3.0 以降の機能候補リストへの追加候補
- `docs/journal/2026-05-26-cli-design-discussion.md` — Phase 9.5 で派生
