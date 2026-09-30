---
title: hyoui list の残骸 socket 自動掃除が backlog 満杯の生き daemon の socket を消す (v0.9.57 回帰)
status: wip
category: bug
created: 2026-09-30T10:12:00+09:00
last_read: 2026-09-30T10:12:00+09:00
open_entered: 2026-09-30T10:12:00+09:00
wip_entered: 2026-09-30T10:12:00+09:00
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

# hyoui list の残骸 socket 自動掃除が backlog 満杯の生き daemon の socket を消す (v0.9.57 回帰)

## 現象 (2026-09-30、kawaz 報告 + 統括の実機観測)

セッション `run-27890-f40b6f65` が webui から繋がらず `hyoui list` にも出ない。daemon (pid 27892、0.9.54 binary) と子 claude (27893) は生存しており、daemon は `~/.local/state/hyoui/run-27890-f40b6f65.sock` を fd で握ったままだが、socket ファイルはディレクトリから消えている (state dir の mtime は 09-30 10:06)。

## 真因

v0.9.57 の `discovery::query_status` は `connect()` が `ECONNREFUSED` なら daemon 不在とみなして unlink する。しかし macOS (BSD) の unix socket は listen backlog (hyoui は 5) が満杯だと `connect()` が `ECONNREFUSED` を返す (XNU `unp_connect` の `sonewconn` 失敗)。daemon が生きていて accept が追いついていない (固まっている / 一時的に詰まっている) だけで残骸と誤判定される。「ECONNREFUSED = 不在」は Linux でも backlog 満杯時は保証されない (SYN drop 相当で block / EAGAIN になるので直ちに誤爆はしないが、根拠として弱い)。

## 修正方針 (v0.9.58)

daemon が socket の隣に `<name>.lock` を作って生存中 `flock(LOCK_EX)` を保持する。list は ECONNREFUSED 時に `flock(LOCK_EX|LOCK_NB)` を試し、**取れた時だけ**残骸と確定して unlink。取れなければ `no-response` (生きているが接続を受けられない) として unlink しない。lock ファイルが無い旧 daemon は判定不能なので unlink しない。

## 復旧

unlink 済み socket は外から再生成できないので attach 経路は失われる。`kill 27892` → 子 claude の session (`150b0876-9327-49bc-bdcb-eb05e4826963`, name `main-ccmsg`) を `claude --resume` で再開する。

## 受け入れ条件

- [ ] backlog 満杯の生き daemon に対して list が socket を消さず no-response と表示する (macOS で ECONNREFUSED が実際に出ることをテストで確認)
- [ ] lock を誰も保持していない socket + lock は unlink される
- [ ] lock ファイルの無い旧 daemon の socket は unlink されない
- [ ] DR-0028 upgrade で lock が新プロセスに正しく引き継がれる (または再取得される)
