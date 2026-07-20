# Vendored third-party assets

- `xterm.js` — xterm.js v5.3.0 UMD build (obtained 2026-07-20 from
  https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js). MIT License.
- `xterm.css` — xterm.js v5.3.0 stylesheet (same source). MIT License.
- `fonts/HackGenConsoleNF-Regular.woff2` / `fonts/HackGenConsoleNF-Bold.woff2`
  — HackGen Console NF v2.10.0 (obtained 2026-07-21 from
  https://github.com/yuru7/HackGen/releases/download/v2.10.0/HackGen_NF_v2.10.0.zip、
  同梱 `HackGenConsoleNF-{Regular,Bold}.ttf` を `woff2_compress` v1.0.2 で
  woff2 化)。SIL Open Font License 1.1 (詳細は `fonts/LICENSE.md`)。
  半角:全角=1:2、Nerd Font グリフ + 日本語 JIS 第 1-2 水準を内包し、罫線 /
  半ブロック / Powerline のメトリクスが安定する。サブセット化は今回未実施
  (= tailnet 内利用のためサイズ許容、後日 fonttools が入ったら圧縮を検討)。
- `fonts/LICENSE.md` — HackGen v2.10.0 の LICENSE (SIL OFL 1.1、
  https://raw.githubusercontent.com/yuru7/HackGen/v2.10.0/LICENSE より取得)。
- `addon-unicode11.js` — @xterm/addon-unicode11 v0.8.0 UMD build (obtained
  2026-07-21 from
  https://cdn.jsdelivr.net/npm/@xterm/addon-unicode11@0.8.0/lib/addon-unicode11.js).
  MIT License. Unicode 11 の絵文字幅を xterm.js に反映するアドオン (= daemon 側
  vt100 emulator の width 2 判定と一致させる)。

Do not fetch from CDN at runtime (DR-0027 §4: bundler なし、vendored copy を
crate に埋め込み)。バージョン更新時は上記 URL から取り直して同 file 名で上書き。
