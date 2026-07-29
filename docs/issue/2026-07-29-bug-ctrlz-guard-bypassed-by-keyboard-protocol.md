---
title: "Ctrl+Z ガードが keyboard protocol 有効端末で完全に不発 (= 0x1a 単一 byte でなく CSI-u で届く)"
status: open
category: bug
created: 2026-07-29T00:00:00+09:00
last_read:
open_entered: 2026-07-29T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: docs/QUESTIONS.md RC-C1 (DR-0030 auto-resume の実機確認) から派生。kawaz 実機観測 2026-07-29 (brew 0.9.25、config 不在 = 全 default)
---

# 現象

Ghostty + `hyoui run -- claude ...` + attach 中に Ctrl+Z を押すと、DR-0029 §2 の
ガードが **一切効かない**:

- 単発 / 2 連打 / 3 連打のいずれでも **毎回**子 (claude) に届き、
  `Claude Code has been suspended. Run fg to bring Claude Code back.` が出る
- **単発での detach が起きない** (= 窓の中から抜けられない)
- DR-0030 の `resume_stopped_child` は効いており session は死なない

# 真因

ガードは **単一 byte `0x1a` だけ**を見る:

`crates/hyoui/src/client/attach.rs:31`
```rust
pub const CTRL_Z_BYTE: u8 = 0x1a;
```
`process_ctrlz_guard` (同 65 行) の match は `byte == CTRL_Z_BYTE` の比較のみ。

一方、TUI 子アプリが keyboard protocol を有効化すると、**外側端末は Ctrl+Z を
0x1a では送らなくなる**。claude code の起動時 raw 出力を実測 (0.9.26、`--debug-dump-server`):

```
\x1b[?2004h  \x1b[?1004h  \x1b[?2031h  \x1b[>1u  \x1b[>4;2m
```

- `\x1b[>1u` = kitty keyboard protocol push (disambiguate escape codes)
- `\x1b[>4;2m` = xterm modifyOtherKeys level 2

これらの escape は attach client を素通しして外側 Ghostty に届く (= 透過なので正しい)。
以後 Ghostty は Ctrl+Z を `\x1b[122;5u` (CSI-u、`z`=122 / ctrl=5) として送る。
ガードの byte 比較には 1 つも一致しないので:

1. detach タイマーが張られない → **単発 detach が起きない**
2. sequence は他キー扱いで丸ごと forward される → **連打数に関係なく毎回子へ貫通**
3. claude は自分が要求した符号化なので Ctrl+Z と解釈して self-suspend →
   DR-0030 の auto-resume が毎回起きる

DR-0029 Consequences の末尾が「keyboard protocol と Ctrl+Z の相互作用 (= CSI-u 有効端末で
0x1a が単一 byte で来るか) は残る観測課題」と書いていた懸念が、そのまま実機で顕在化した形。
DR-0029 の検証が `cat` / `bash -i` (= keyboard protocol を有効化しないアプリ) だけだったため
すり抜けた。

# 実測マトリクス (0.9.26、ネスト hyoui、config 不在 = default 500ms)

外側 `hyoui run --detached -- hyoui attach inner` の PTY に byte 列を注入し、
内側 `cat` セッションに何が届くか / 外側 attach client が detach するかを観測。

| 注入 | 期待 (DR-0029 §2) | 実測 inner (子) | 実測 outer (client) |
|---|---|---|---|
| `key:C-z` 単発 (= 0x1a) | 子へ 0 発 / detach する | `^Z` 無し ✅ | session 消滅 = detach ✅ |
| `key:C-z` 2 連打 | 子へ 1 発 / detach しない | `^Z` × 1 ✅ | live のまま ✅ |
| `hex:1b5b3132323b3575` 単発 (= CSI-u) | 子へ 0 発 / detach する | `^[[122;5u` がそのまま到達 ❌ | live のまま (detach せず) ❌ |
| 同 2 連打 | 子へ 1 発 / detach しない | `^[[122;5u` × 2 が到達 ❌ | live のまま (偶然一致) |

CSI-u を実際の claude セッションに注入すると `Claude Code has been suspended` + STATUS=stopped
になることも別途確認済 (= 子側の解釈も想定どおり)。

つまり **0x1a 経路のガードは正しく動いており、実装バグではない**。ガードの定義が
「byte 0x1a」に閉じていることが原因。

# 修正には設計判断が要る (= 実装を止めた理由)

局所修正で済まないため DR-0029 §2 の改訂が要る。論点:

1. **ガードの対象を「byte」から「キーイベント」へ広げるか**。広げるなら client の
   stdin を最低限パースすることになり、「入力は完全透過」(DR-0005) との距離が変わる
2. **どの符号化を Ctrl+Z と認めるか**: kitty CSI-u (`CSI 122;5u`、event type 付き
   `CSI 122;5:1u`、alternate key 付き `CSI 122:122;5u`)、xterm modifyOtherKeys
   (`CSI 27;5;122~`)、素の 0x1a
3. **「子へ 1 発届ける」時に何を送るか**。0x1a に正規化すると、CSI-u を要求した子が
   受け取れる保証がない (= 子が期待する符号化のまま送るのが筋)
4. **chunk 境界**: CSI sequence は `read(2)` を跨ぎうるので、未完 sequence の保留
   buffer が要る。現行 state (1 bit) では表現できない
5. **他キー割り込みルールとの整合**: sequence の途中 byte を「他キー」と見なして
   保留をキャンセルしてはいけない

# 暫定回避

現状 attach 中に窓から抜ける手段が無いので、別端末から `hyoui detach <session>`。
