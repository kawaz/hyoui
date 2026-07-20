# Vendored third-party assets

- `xterm.js` — xterm.js v5.3.0 UMD build (obtained 2026-07-20 from
  https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js). MIT License.
- `xterm.css` — xterm.js v5.3.0 stylesheet (same source). MIT License.

Do not fetch from CDN at runtime (DR-0027 §4: bundler なし、vendored copy を
crate に埋め込み)。バージョン更新時は上記 URL から取り直して同 file 名で上書き。
