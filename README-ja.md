# hyoui

> [English](./README.md) | 日本語

**hyoui** `/ˈhjoʊi/`（ヒョーイ・憑依）— 子プロセスに「憑依」して一体で動く、透過的な PTY コンパニオン。

任意のコマンドを PTY の中で実行するラッパー。普段は何もせず透過的に振る舞うが、
入出力に「憑く」ことで観察・書き換え・外部制御の取っ掛かりを与える。

## できること（design）

- `hyoui -- cmd [args...]` — 任意コマンドを PTY 内で実行（argv は execvp で直接解決）
- **interactive モード**（default）— 実 tty を raw 化して透過プロキシ
- **headless モード** — 実 tty なしで動作。`--size COLSxROWS` で仮想スクリーンサイズを与え、
  `cat input | hyoui -- cmd` のように stdin をパイプから子へ渡せる
- Unix socket 経由で外部から PTY へ入力注入
- 停止条件: `--timeout` / `--idle-timeout` / `--until <pattern>`
- bg/fg の透過制御: 子の suspend / 親の suspend をそれぞれオプションで連動

## 名前について

「憑依」— 何かが宿って一体化し、宿主は一見ふつうに見えるが内側から動かしうる。
子プロセスに付き添い、一蓮托生で生き死にし、headless では外部からの操縦ハンドルにもなる、
というこのツールの性格を表す。

## 状態

PoC（個人用の小物ツール）。設計と実装は順次。

## License

MIT License — Yoshiaki Kawazu (@kawaz)
