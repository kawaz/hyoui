# DR-0005: hyoui の思想再定義 — 外側からの自動操作主軸、TUI multiplexer ではない

- Status: Active
- Date: 2026-05-26
- Related: DR-0001 (jobcontrol 2 軸), DR-0002 (naming), DR-0003 (Rust 一本化), DR-0006 (CLI ground rules), DR-0007 (MVP scope), DR-0017 (session anchor 化 — daemon の役割を改訂)

> **📌 思想改訂注記 (2026-06-10、[[DR-0017]] により改訂)**: 本 DR の「daemon は外側の観測者」に、
> [[DR-0017]] で **「daemon は child の session の anchor を兼ねる」**を追記する。daemon が
> `TIOCSCTTY` で controlling tty を保持し、slave に `tcsetpgrp` / `tcsetattr` を行うことは、
> TUI の Ctrl-Z を本来のセマンティクスで動かすために [[DR-0017]] で justify された必要最小限の介入。

> **📌 現行 CLI 体系への注記 (2026-06-10 追記、本文は判断記録として保存)**:
> 本 DR が例示する `hyoui send / keys / paste / detach` は **草案段階の構想名**であり、
> 現行 CLI では以下の体系に統合・改名されている。本文中の `send/keys/paste/detach` の表記は
> 当時の思想説明として読むこと:
>
> | 当時の構想名 | 現行 CLI |
> |---|---|
> | `send` / `keys` / `paste` | `hyoui input` family に統合 (= `text:` / `hex:` / `file:` / `paste:` / `key:` spec を順序保証で送信、DR-0006 §8)。独立 subcommand 化せず |
> | `detach` (out-of-band) | `hyoui detach` CLI (= in-band キーは持たない。Ctrl+Z 単発は client suspend、[[DR-0029]] §2)。daemon は linger 継続 (DR-0015) |
> | `tx` | 未実装 (docs/issue/2026-05-27-tx-lock-unlock-cli-subcommands.md) |

> **📌 in-band escape 原則の改訂注記 (2026-07-25、[[DR-0029]] により)**: 本 DR の
> 「in-band escape (prefix キー等) を一切導入しない」は **「子の stdin には hyoui 由来の
> escape を一切足さない (= 子から見た完全透過)」** の意味に狭める。attach client の
> tty stdin では Ctrl+Z 1 キーだけが hyoui のローカル解釈対象になる (= 単発で client
> 自身を suspend、2 連打で子へ 1 発、[[DR-0029]] §2)。prefix キー体系 (tmux/screen 流の語彙) は依然
> 領域外で、子への入力経路 (`hyoui input`) には escape を一切持たない。

## Context

poc → Rust 一本化 (DR-0003) → v0.0.0 リリース後、本実装の機能設計フェーズに入った。
最初に決めるべきは「hyoui は何で、何でないか」の方向性。

考慮したルート:

1. **TUI multiplexer 路線**: tmux/screen のように、ユーザが日常的に「中で生活する」UI を充実させる
   (prefix キー、window/pane/copy-mode/scrollback)
2. **外側自動操作路線**: ユーザは普段 hyoui の中で生活しない。CLI/HTTP/script から外側で監視・自動操作する
   ツールとして振る舞う
3. **両取り路線**: 1 と 2 を併存

ユーザ (kawaz) の主用途は「PTY ラップした long-running process (claude code 等) を外側から
監視・自動操作したい」「外出先からリモート attach したい」。tmux/screen 風 UI は既存ツールに任せ、
hyoui の存在価値は「外側からの透明な制御」に集中させたい。

## Decision

**hyoui は外側からの監視/自動操作を主軸とするツールに振る**。Terminal multiplexer ではない。

### 思想の柱

- **透明性最優先**: 子プロセスへの入力は完全透過。in-band escape (prefix キー等) を一切導入しない
- **外側 driven**: 監視・操作は CLI (`hyoui send/keys/paste/wait/tail/lock/tx`) や将来の HTTP gateway 経由
- **副次的に対話的 attach 対応**: 人間が attach して中で生活する用途も許す。ただし主軸ではない、UI 拡張は最小
- **daemon 化 default**: 起動と同時に socket を持ち、detach/attach が標準フローとして成立

### 領域外と明示するもの

- prefix キーバインド (tmux: C-b, screen: C-a)
- window / pane / copy-mode / scrollback の UI
- session グループ / server モデル (= 1 process が複数 session を抱える tmux 型は採用しない、screen 型 = 1 daemon 1 session)
- 「中で生活する」ためのキーバインド設定機能

### 含めるもの

- `hyoui run` で起動 = daemon 化 + socket 常設 + 自動 attach
- `hyoui send/keys/paste` で外側から入力注入
- `hyoui tail/wait` で外側から出力監視
- `hyoui lock/tx` で複数 attach 状態での自動操作排他
- 将来 (v0.2.0) `hyoui serve` で HTTP gateway + xterm.js による remote attach

## Rejected alternatives

### TUI multiplexer 路線 (1)

- tmux が既に高品質に存在し、差別化困難
- ユーザ (kawaz) が prefix キーを覚えられない/嫌い
- hyoui を「中で生活する」UI に投資すると、外側自動操作という本来の差別化点が薄まる

### 単発 PTY ラッパー (daemon 化なし)

- 「中で実行して終わる」だけだと、自動操作主軸と矛盾 (= 外から操作する余地がない)
- 既存の `script(1)` や `expect(1)` で代用可、新規ツールにする価値が薄い

### 両取り路線 (3)

- 思想が拡散する。tmux 寄りの機能要求と「透明性最優先」が衝突する場面が必ず出る
- まずは外側自動操作に絞り、TUI 機能は別 layer (tmux の中で hyoui を動かす運用) で吸収する

## Consequences

### CLI design への波及

- in-band escape を持たないので、detach は out-of-band (`hyoui detach <name>`) のみ
- send/keys/paste/wait が first-class API、複雑な引数体系もこれを支える設計に振る
- attach は「ペアプロ・観戦・人手介入」のための補助機能。leader 概念は内部メカニズムとして持つが、
  MVP では CLI 露出しない (HTTP gateway 展開時に解放)
- nest 起動 (hyoui の中で hyoui) は許可 + warn (`$HYOUI_NAME`/`$HYOUI_SOCK` 環境変数で検知)
- prefix キーの設定オプションは存在しない (= 設定の選択肢自体を消す)

### 段階リリース

- v0.1.0: 外側自動操作の core (send/keys/paste/wait/lock/tx) と daemon ライフサイクル
- v0.2.0: HTTP gateway (`hyoui serve`) で remote attach、xterm.js ブラウザクライアント
- v0.3.0 以降: 高度な TUI 自動化 (画面 rect 指定 wait、named area)、leader CLI 露出

詳細は [[DR-0006]] (CLI ground rules)、[[DR-0007]] (MVP scope)。

### 発展シナリオ (kawaz が想定する代表的ユースケース)

- `hyoui run claude` で PC 上に常駐 → お出かけ → リモートから PC の `hyoui serve` HTTP endpoint
  にアクセス → ブラウザターミナル (xterm.js) で attach して作業継続
- CI/script から `hyoui send` で大量入力 → `hyoui wait --pattern "done"` で完了待ち → 結果取得
- `hyoui tx` で複数 client 環境でも atomic な自動操作シーケンス実行

## 関連

- [[DR-0001]] — bg/fg jobcontrol 2 軸 (透明性思想の起点)
- [[DR-0006]] — 本 DR の思想を具体化した CLI ground rules
- [[DR-0007]] — MVP scope と段階リリース計画
- `docs/journal/2026-05-26-cli-design-discussion.md` — 本 DR に至る議論の経緯
