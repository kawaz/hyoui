---
title: screen dump の scrollback / both layer (ANSI) が色を落とし、web の初期表示がモノクロになる
status: open
category: bug
created: 2026-10-04T11:00:00+09:00
last_read: 2026-10-04T11:00:00+09:00
open_entered: 2026-10-04T11:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# screen dump の scrollback / both layer (ANSI) が色を落とし、web の初期表示がモノクロになる

## 現象

web の session ページは初期表示と WS 再接続直後に `GET /api/sessions/{id}/screen?layer=both` を取り (`crates/hyoui-web/assets/session.js` の `fetchScreen`)、gateway は `ScreenDumpFormat::Ansi` で daemon に dump を要求する。daemon は scrollback / both layer の ANSI を `crates/hyoui/src/daemon/screen/snapshot.rs` の `rows_to_ansi` で組み直すが、出すのは bold / italic / underline / inverse の 4 属性だけで、色は `RowCellSnap` が保持していないので落とす (同関数の doc comment に「MVP scope」と明記)。結果として初期表示と再接続直後の画面がモノクロになる。`hyoui screen dump --layer=both --format=ansi` も同じ。

vt100 の `Cell` は前景色・背景色を持っているので、vt100 の制約ではない。

## 直し方の方向

`RowCellSnap` に前景色・背景色 (と dim / blink / strike 等、vt100 の `Cell` が持つ属性) を持たせ、`rows_to_ansi` で色の SGR (indexed / RGB) を出す。変更は `daemon/screen/` に閉じる。visible のみの layer (`state_formatted()` 経由) との出力差が無くなることを test で固定する (同じ画面を visible と both で dump し、visible 部分の SGR が一致する)。

## 関連

- `docs/issue/2026-10-04-design-webui-terminal-app-rework.md` (daemon 側の自前セルモデルは別 track。本件はそれを待たずに直せる)
