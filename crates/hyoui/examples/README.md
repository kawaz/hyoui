# hyoui PoC examples

DR-0005〜DR-0008 を確定させるために実施した技術検証 PoC 群。各 PoC は単独
`cargo run --example <name>` で実行でき、対応する `docs/findings/` に結論を残す。

## 一覧

| # | example | 検証内容 | 対応 finding |
|---|---------|---------|------------|
| 01 | `01-daemon-fork` | double-fork + setsid で完全 daemon 化、親 shell prompt 即復帰、孫 process が orphan で生存し続けるか | `docs/findings/2026-05-26-daemon-fork.md` |
| 01b | `01b-daemon-fork-detached` | 01 の改良: daemon が stdin/stdout/stderr を /dev/null にリダイレクト、`pipe` が即解放されるか | 同上 |
| 02 | `02-multi-attach` | 子 PTY を 1 つ持つ daemon が複数 client から接続を受け、broadcast (子→clients) + multiplex (clients→子) する `poll` パターン | `docs/findings/2026-05-26-multi-attach.md` |
| 03 | `03-paste-and-alt-screen` | bracketed paste / alternate screen / ANSI escape の自動検出、出力 byte rate (= scrollback サイズの判断材料) | `docs/findings/2026-05-26-paste-and-alt-screen.md` |
| 04 | `04-fd-passing` | SCM_RIGHTS による fd passing と stream 中継の比較。**stream 中継一本化** に確定 (DR-0008) | `docs/findings/2026-05-26-fd-passing-vs-stream.md` |
| 05 | `05-resize-propagation` | leader からの `TIOCSWINSZ` で子 (bash) が SIGWINCH を受けて `$COLUMNS` / `$LINES` を更新できるか | `docs/findings/2026-05-26-resize-propagation.md` |
| 06 | `06-lock-token-env` | `HYOUI_LOCK_TOKEN` env を子 process / 孫 process が `std::env::var` で受け取れるか (lock token の自動継承) | `docs/findings/2026-05-26-lock-token-env.md` |
| 07 | `07-scrollback-ring` | `VecDeque<OutputChunk{ts, bytes}>` + size 制約 + `last_evicted_ts` 更新 + `--since-strict` 判定の synthetic test | `docs/findings/2026-05-26-scrollback-ring-buffer.md` |
| 08 | `08-ansi-strip` | CSI / OSC / DCS / single char escape を strip して raw text を取り出す。03 の生 sample を入力にして動作確認 | `docs/findings/2026-05-26-ansi-strip.md` |

## 実行

```sh
# 1 つだけ実行
cargo run --example 02-multi-attach -- test

# 全 example を build (= CI が走らせている健全性チェック)
cargo build --examples
```

例の中には引数を取るもの (`02-multi-attach <session-name>`) や、別 PoC が生成した
ファイル (`03-*.raw`) を入力にするもの (`08-ansi-strip`) がある。詳細は各 file の
doc コメント冒頭を参照。

## 位置づけ

- これらは **PoC = 設計検証用の使い捨て産物**。MVP の正規実装は `crates/hyoui/src/`
  にあり、PoC の知見は `src/scrollback.rs` / `src/strip.rs` 等として取り込まれている。
- PoC の中身が陳腐化したら remove する。長期保管が目的ではない。
- DR-0008 確定後の旧 `agent.rs` (= v0.0.0 PoC PTY ラッパー、697 行) は git history に
  保存されている。必要に応じて `git show 24a6181e^:crates/hyoui/src/agent.rs` で参照可能。

## CI

`cargo build --examples` が `.github/workflows/ci.yml` の `cargo build --workspace`
で実行される (= 全 example が compile に通ることのみ保証)。実行テストは未自動化、
手動 invoke を前提。

## 関連

- `docs/decisions/DR-0005`〜`DR-0008` — 設計判断
- `docs/findings/2026-05-26-poc-summary.md` — PoC 全体まとめ
