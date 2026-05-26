# Feature idea: tty I/O の dump / record / play subcommand

- Status: Open (Idea, 採用未確定)
- Date: 2026-05-26
- Priority: Low (v0.3.0 以降検討)
- 発見元: hyoui paste API 設計議論で `--spool-append` (継続的に file に追記) のユースケースを検討した際、
  「tty dump 用途は paste の責務ではなく、独立 subcommand が筋」と判明したため派生

## ファイル分類

本ファイルは **未採用の feature idea** を集積する `docs/issue/` の `feature-` prefix 慣習の初回ファイル。
規約上 `docs/issue/` は「自リポ TODO + 外部から受けた依頼」だが、未確定 idea も TODO の上流として
ここに置き、具体化したら DR/research に昇格、却下時に削除という運用にする (DR/journal だけだと埋もれる)。

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

## 実装上の検討点

- daemon は既に pty 出力を全 attach client に broadcast している → dump/record は client の一種 (= file sink)
  として実装可能、protocol 拡張は最小限
- `record` の cast format は asciinema 互換が筋 (= 既存 viewer/converter 資産を活用)
- `play` の入力再生は lock + tx 機構と統合 (= play 中は他 client の rw を一時降格)
- `play --speed` で時間軸を変更する場合、output だけは無視できるが input は子の応答待ちと整合させる必要 (= input only モードは無条件タイミング、output 反映モードは prompt 待ちと連動)

## 段階

優先度低、v0.3.0 以降で本格検討。MVP (v0.1.0) には入れない。
v0.2.0 (`hyoui serve` + HTTP gateway) と並行して record/play は CI 自動化観点で先行する可能性あり。

## 関連

- [[DR-0005]] — 外側自動操作主軸の思想 (record/play は思想ど真ん中)
- [[DR-0006]] — `--spool-append` 廃止判断の延長
- [[DR-0007]] — v0.3.0 以降の機能候補リストへの追加候補
- `docs/journal/2026-05-26-cli-design-discussion.md` — Phase 9.5 で派生
