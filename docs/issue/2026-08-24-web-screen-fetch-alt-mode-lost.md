---
title: web UI の初期 /screen fetch が alternate screen mode を復元しない
status: open
category: bug
created: 2026-08-24T09:11:44+09:00
last_read:
open_entered: 2026-08-24T09:11:44+09:00
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

# web UI の初期 /screen fetch が alternate screen mode を復元しない

## 概要

web UI を開く / reload したときの初期復元 (`fetchScreen()` → HTTP `/screen`) で、子が
alternate screen にいても xterm.js の activeType が `normal` のままになる。daemon 側は
alt を正しく認識している。

## 背景

2026-08-24、OSC 8 調査の副産物として実測。

- 検証セッション `osc8-probe-alt2` で子を alt screen に入れた状態で観測
- daemon: `hyoui screen snapshot --include=Mode` → `alternate-screen: true`
- browser 初期復元後の xterm: `activeType` が `normal`。alt buffer として復元されない
- 一方、attach 後に子から `?1049h + clear + OSC 8` を live 送信すると xterm の
  `activeType` は `alternate` になる (= live 経路は正常)
- 観測者の見立て: `fetchScreen()` の reset + `/screen` payload 側が alt mode を
  再現していない

### 対比 (CLI 経路との差)

CLI attach 側は DR-0013 §4 Phase A で alt flag を保つ設計になっており、
`serve_attach_redraw_preserves_alt_screen_flag` (crates/hyoui/src/daemon/session.rs の
テスト) が「redraw の冒頭が `\x1b[?1049h` で始まる」ことを固定している。つまり web の
HTTP `/screen` fetch 経路だけが alt mode を落としている可能性が高い。build_attach_redraw
が alt flag を付けるのに対し、`/screen` (screen dump) 経路が同じ扱いをしていないのでは
ないか (未確認、要調査)。

### 影響

web を reload すると、子が TUI (alt screen) を表示中でも normal buffer として描画される。
scrollback や画面消去の扱いが実際の子の状態とズレるため、表示の整合が崩れうる。DR-0013 が
screen state を正本にしている以上、CLI と web で復元結果が食い違うのは設計上の不整合。

### 関連

- DR-0013 §4 Phase A (attach 復元 protocol)
- DR-0013 §6 (alternate screen hook)
- DR-0027 (web gateway)
- docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md (同じ「screen dump 復元層で
  情報が落ちる」系統の別件)

## 受け入れ条件

- [ ] `/screen` fetch 経路が alt mode 中の子に対して、xterm 側で `activeType: alternate`
      になるよう復元 payload を修正する
- [ ] CLI attach 経路 (`serve_attach_redraw_preserves_alt_screen_flag`) と同様の固定テスト
      を web 経路にも用意する

## TODO

## 修正 (2026-09-15)

### 原因

Observed:

- release build (`hyoui 0.9.41`) と mode 0700 の専用 `XDG_RUNTIME_DIR`、専用 port `127.0.0.1:43701` で alt screen 中の session を起動した。
- daemon の `screen snapshot --include=Mode --format=json` は `alternate-screen: true` を返した。
- 修正前の `GET /api/sessions/:id/screen?layer=both` は HTTP 200 と画面 marker を返したが、payload 先頭は `ESC[H` で `ESC[?1049h` を含まなかった。
- CLI attach は `build_attach_redraw()` で active buffer に応じた `ESC[?1049h` / `ESC[?1049l` を prepend していたが、`ScreenDumpLayer::Both + ScreenDumpFormat::Ansi` は rows を ANSI 化するだけだった。

Inferred:

- web handler 自体は daemon の screen dump payload をそのまま返すため、alt mode の欠落箇所は HTTP 層ではなく `Both + Ansi` の復元 bytes 生成だった。

### 修正箇所

- `crates/hyoui/src/daemon/screen/redraw.rs`: active buffer の mode sequence 生成を `buffer_mode_sequence()` に抽出し、CLI attach と screen dump で共有した。
- `crates/hyoui/src/daemon/screen/snapshot.rs`: `Both + Ansi` payload の先頭へ共有 mode sequence を prepend した。scrollback + visible の連結順は維持した。
- 新 protocol message / cap flag は追加していない。

### テスト

- unit: `dump_both_ansi_preserves_active_buffer_mode`
- e2e: `e2e_screen_both_preserves_alternate_screen_mode`

### 実機観測

修正後の同条件では daemon が `alternate-screen: true`、HTTP status が 200、payload 先頭 8 bytes が `1b5b3f3130343968` (`ESC[?1049h`)、画面 marker も保持された。検証用 web gateway と session を停止し、専用 runtime directory と対象 process が残っていないことを確認した。
