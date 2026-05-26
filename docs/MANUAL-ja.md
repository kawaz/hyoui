# hyoui ユーザマニュアル

> [English](./MANUAL.md) | 日本語

エンドユーザ (CLI から hyoui を使う人) 向けのユースケース別レシピ集。

- **インストール / 概念紹介** → [`README-ja.md`](../README-ja.md)
- **内部設計・なぜそうなっているか** → [`DESIGN-ja.md`](./DESIGN-ja.md)
- **このファイル**: 「○○ をやりたい」→ 「このコマンド列で実現」のレシピ

> Status: v0.1.x scaffold。v0.2.0 の `serve` 系 / 自動操作 API
> (`send` / `keys` / `paste` / `wait` / `tail` / `lock` / `tx`) 完成後に本格化する。
> 現状は v0.1 系で利用可能なレシピだけ収録。

## 目次

- [基本フロー](#基本フロー)
  - [1. detached でセッションを起動して別端末から attach](#1-detached-でセッションを起動して別端末から-attach)
  - [2. read-only で観察する](#2-read-only-で観察する)
  - [3. 終了させる](#3-終了させる)
- [トラブルシューティング](#トラブルシューティング)
- [関連リンク](#関連リンク)

## 基本フロー

### 1. detached でセッションを起動して別端末から attach

```sh
# 端末 A: detached でセッション起動 (session id が stdout に出る)
hyoui run --detached -- claude
# → run-<pid>-<rand>  (例)

# 端末 B: list で確認 → attach
hyoui list
hyoui attach run-<pid>-<rand>
# detach は Ctrl-A D
```

### 2. read-only で観察する

```sh
hyoui attach --observer run-<pid>-<rand>
# observer は入力を送らない読み取り専用 attach
```

### 3. 終了させる

```sh
hyoui kill run-<pid>-<rand>            # SIGTERM
hyoui kill --signal KILL run-<pid>-<rand>  # SIGKILL
```

## トラブルシューティング

| 症状 | 対処 |
|---|---|
| `hyoui list` に session が出ない | `XDG_RUNTIME_DIR` / `TMPDIR` の socket dir に stale socket が残っていないか確認 (`docs/runbooks/2026-05-27-stale-socket-detection.md`) |
| attach 直後に切られる | daemon が cap negotiation で reject した可能性 (`docs/runbooks/2026-05-27-handshake-cap-rejection.md`) |
| 子プロセスが死んで daemon だけ残る | `docs/runbooks/2026-05-27-child-orphan-detection.md` |

詳細な runbook は `docs/runbooks/INDEX.md` を参照。

## 関連リンク

- [README-ja.md](../README-ja.md) — インストール、コンセプト、最初の hello world
- [DESIGN-ja.md](./DESIGN-ja.md) — 内部アーキテクチャ
- [ROADMAP.md](./ROADMAP.md) — v0.2.0+ のレシピが追加されるタイミング
- [docs/runbooks/](./runbooks/) — 障害対応手順
