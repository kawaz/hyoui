---
title: attach 復元経路で OSC 8 (hyperlink) の metadata が失われる
status: open
category: bug
created: 2026-08-24T00:00:00+09:00
last_read:
open_entered: 2026-08-24T00:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
origin: hyoui 本体 (kawaz 要望「webui でリンクを実際に機能させたい」由来)
---

# attach 復元経路で OSC 8 (hyperlink) の metadata が失われる

## 概要

attach **前**に画面へ出力された OSC 8 hyperlink は、attach 後の web/CLI 画面では文字列だけが復元され、リンクとして機能しない。attach **後**に出力された OSC 8 は正常に機能する (xterm.js で urlId が付き hover underline / click activate が動く)。つまり web を reload したり別デバイスから attach し直すと、それまで画面にあったリンクが全部死ぬ。

## 背景

kawaz 要望「webui でリンクを実際に機能させたい」(2026-08-24) の一部。同要望のうち live 経路 / iframe sandbox / touch / 素 URL は別途対応中で、本 issue は screen state 側の残課題。

### 実測 (2026-08-24、codex-sol-worker の probe)

- 子 → daemon の raw dump には完全な OSC 8 bytes が存在する (`1b 5d 38 3b 3b 68 74 74 70 73 ... 1b 5c`)
- attach 後の初期画面で `PRE-OSC8-LINK` は xterm cell が `extended.urlId=0`、hover underline なし
- 同セッションに attach 後 live で出した `POST-OSC8-LINK` は `extended.urlId=1` で hover/confirm/click すべて成立
- `hyoui screen dump --format=ansi` の出力に OSC 8 sequence が含まれない (`\x1b[?25h\x1b[m\x1b[H\x1b[JPRE-OSC8-LINK\r\n...`)
- primary / alt screen の両方で同じ。alt screen 下端配置や `CSI H + CSI 2J + OSC 8` の 20 回連続再描画でも live 経路は正常 (urlId が付く) なので、境界は「attach 前か後か」の一点

### 原因

vt100-0.16.2 の `osc_dispatch` (cargo registry `vt100-0.16.2/src/perform.rs:198-236`) は OSC 0/1/2/52 のみ処理し、OSC 8 は `unhandled_osc` に落ちる。既定 callback は no-op。したがって daemon の screen state に hyperlink metadata が保持されず、`state_formatted()` / `build_attach_redraw` (crates/hyoui/src/daemon/screen/redraw.rs:29-43) にも現れない。

### 影響範囲

DR-0013 で screen state を正本にしている以上、attach 復元・screen dump・snapshot のすべてで hyperlink 情報が欠落する。web UI に限らず CLI attach でも同じ。

### 対処案 (未裁定、いずれも小さくない)

- 案 A: screen emulator を OSC 8 対応のものに置換または vt100 を fork/拡張して urlId を cell attribute として保持する。DR-0013 の Rejected alternatives (vte / alacritty_terminal / wezterm-term / termwiz / libghostty-vt) を hyperlink 保持の観点で再評価する必要がある
- 案 B: hyoui 側に sidecar の hyperlink state を持つ。ただし cursor 移動 / erase / scroll / resize との同期コストが大きく、安易なフィールド追加は設計を歪める (worker も非推奨と評価)。採るなら DR で機構を設計してから
- 案 C: 現状維持 (live で出たリンクだけ機能する)。reload/再 attach で死ぬ制約を仕様として明記する

## 受け入れ条件

- [ ] 案 A/B/C のいずれかを DR で裁定する
- [ ] 裁定した案を実装し、attach 前に出力された OSC 8 hyperlink が attach 後も機能することを実機検証する (primary/alt screen 両方)

## 関連

DR-0013 (screen state 正本 / attach 復元 protocol)
