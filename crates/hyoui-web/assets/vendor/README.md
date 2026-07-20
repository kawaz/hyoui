# Vendored third-party assets

- `xterm.js` — xterm.js v5.3.0 UMD build (obtained 2026-07-20 from
  https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js). MIT License.
- `xterm.css` — xterm.js v5.3.0 stylesheet (same source). MIT License.
- `addon-unicode11.js` — @xterm/addon-unicode11 v0.8.0 UMD build (obtained
  2026-07-21 from
  https://cdn.jsdelivr.net/npm/@xterm/addon-unicode11@0.8.0/lib/addon-unicode11.js).
  MIT License. Unicode 11 の絵文字幅を xterm.js に反映するアドオン (= daemon 側
  vt100 emulator の width 2 判定と一致させる)。

Do not fetch from CDN at runtime (DR-0027 §4: bundler なし、vendored copy を
crate に埋め込み)。バージョン更新時は上記 URL から取り直して同 file 名で上書き。
