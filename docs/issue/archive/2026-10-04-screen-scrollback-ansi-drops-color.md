---
title: screen dump の scrollback / both layer (ANSI) が色を落とし、web の初期表示がモノクロになる
status: resolved
category: bug
created: 2026-10-04T11:00:00+09:00
last_read: 2026-10-04T11:00:00+09:00
open_entered: 2026-10-04T11:00:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered: 2026-10-04T10:57:07+09:00
discard_reason:
pending_reason:
close_reason: RowCellSnap に fg / bg / dim を持たせ rows_to_ansi が色 SGR を出すようにした
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

## 決着 (2026-10-04)

- `RowCellSnap` (`daemon/screen/state.rs`) に `fg` / `bg` (vt100 の `Color`) と `dim` を追加。CBOR の `CellSnapshot` と `attrs` の bit 配置は変えていない。vt100 0.16.2 の `Cell` は blink / strike を持たないため、持たせたのは `Cell` にある dim のみ。
- `rows_to_ansi` (`daemon/screen/snapshot.rs`) が前景・背景の SGR を出す (default は出さない、indexed 0-7 は `30-37` / `40-47`、8-15 は `90-97` / `100-107`、16-255 は `38;5;N` / `48;5;N`、RGB は `38;2;R;G;B` / `48;2;R;G;B`)。dim は `2`。style が直前 cell から変わった時だけ reset + SGR を吐く方式は維持。
- test: `both_ansi_matches_visible_cells_with_colors` (indexed 8 / 16 / 256、RGB、fg + bg、属性の組み合わせを visible と both で dump し、同じ vt100 parser で再生したセルの文字・fg・bg・属性が一致) と `both_ansi_keeps_color_in_scrollback` を追加。
