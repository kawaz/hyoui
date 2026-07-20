---
title: daemon の graceful upgrade (self-exec による fd/pid 引き継ぎ)
status: open
category: request
created: 2026-07-21T02:46:19+09:00
last_read:
open_entered: 2026-07-21T02:46:19+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: 自リポ TODO
---

# daemon の graceful upgrade (self-exec による fd/pid 引き継ぎ)

## 概要

daemon を再起動せずに新バイナリへ切り替える graceful upgrade 機構を設計・実装したい。
kawaz 要望 (2026-07-20): 「再起動したくない。fd も pid も引き継いで新バイナリに exec する感じ」。

方式候補: self-exec。PTY master fd / unix socket listener の `CLOEXEC` を外して新バイナリへ
`exec(2)` する。PID が不変なので子プロセスとの親子関係と SIGCHLD 経路がそのまま保たれる
(fork+handoff 方式より hyoui のプロセスモデルに合う)。

メモリ状態は exec 前に一時ファイルへシリアライズし、新プロセス起動直後に復元する。
screen state は完全シリアライズでなく、scrollback bytes の再 feed による再構築で足りる
可能性が高い (要検証)。

attach client は upgrade 時に一旦切断 → 再接続とする。v1.0 未満で wire protocol が
まだ動いている前提のため、client 接続を維持したままの upgrade は現時点では過剰と判断。

## 背景

長時間 attach したまま daemon を更新したいという運用要望。現状は daemon 再起動 = 子プロセスの
再アタッチや session 状態の消失を伴うため、upgrade のたびに運用コストが発生している。

## 設計論点

- **state format の版間互換**: breaking 期はフォーマットのミスマッチが起き得るため、
  ミスマッチ検出時は tail 再 feed へのフォールバックを持たせる
- **exec 失敗時の安全性**: exec(2) 失敗時は旧プロセスがそのまま継続するため、比較的安全な
  失敗モードになる想定 (要検証)
- **トリガー方式**: `hyoui upgrade <session>` のような明示 subcommand にするか、新バイナリの
  自動検知にするか
- **DR-0025 reducer 化との関係**: state が message log として形式化されるほど handoff は
  単純化する方向であり、reducer 化と同方向の設計。DR-0025 の進捗と合わせて設計するのが望ましい

## 参考事例

kawaz 個人ツールの中に同種の graceful な fd/pid 引き継ぎを実装済みのものがある (実装パターンの
参照先として当たる価値あり、裏取り未了)。

## 受け入れ条件

- [ ] self-exec 方式での upgrade 設計を DR として起草する (DR-0025 message 駆動原則に従い、
      upgrade を protocol message として形式化してから実装に入る)
- [ ] state のシリアライズ/復元方式 (完全 vs scrollback 再 feed) を実機検証で決定する
- [ ] exec 失敗時のフォールバック挙動を明記する
- [ ] トリガー方式 (subcommand か自動検知か) を決定する
