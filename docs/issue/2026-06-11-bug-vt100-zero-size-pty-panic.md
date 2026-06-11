# bug: PTY サイズ 0 のとき vt100 grid が subtract overflow で panic する

- Date: 2026-06-11
- Status: open
- Priority: 低 (= 通常の端末経由では発生しない、特殊な起動形態のみ)

## 現象

`script -q <file> hyoui run --socket=... -- /bin/sleep 30 </dev/null` のように
**サイズ 0 の PTY / 非標準の端末サイズ**で `hyoui run` を起動すると、daemon
(screen emulator) が panic する:

```
thread 'main' panicked at .../vt100-0.16.2/src/grid.rs:26:28:
attempt to subtract with overflow
```

jobcontrol_follow ハング調査 (2026-06-11) 中に `script(1)` 経由の再現を試みた際に
副産物として観測。

## 再現

```bash
script -q /tmp/typescript.out target/debug/hyoui run --socket=/tmp/x.sock -- /bin/sleep 30 </dev/null
```

(stdin が /dev/null で即 EOF + script の与える端末サイズが 0 になる組合せ)

## 推測される原因

端末サイズ取得が 0 行 / 0 列を返したとき、それをそのまま vt100 の
`Parser::new(rows, cols, ..)` 等に渡しており、vt100 側の grid 計算
(`size - 1` 系) が underflow する。

## 対処案

- サイズ取得結果が 0 の場合に既定値 (24x80) へ clamp してから screen emulator に渡す
- `--cols` / `--rows` の validation にも 0 を弾く確認を入れる

## 関連

- docs/issue/2026-06-11-bug-jobcontrol-follow-test-hangs.md (調査中の副産物として発見)
