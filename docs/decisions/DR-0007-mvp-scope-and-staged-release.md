# DR-0007: MVP scope と段階リリース (v0.1.0 / v0.2.0 / v0.3.0)

- Status: Active
- Date: 2026-05-26（v0.1.0 リリース後に re-scope: 2026-05-27）
- Related: DR-0005 (思想), DR-0006 (CLI ground rules), DR-0008 (protocol)

## Context

[[DR-0006]] で確定した API surface は大きい。一気に v0.1.0 で全部出すと開発工数が膨大、
かつ「動くもの」を出すまでの時間が伸びる。段階分割が必要。

優先度判断基準:

- **MVP (v0.1.0)**: daemon ライフサイクル + multi-attach + protocol 基盤までを実装し、
  「思想 (DR-0005) のコアが成立するための土台」を最短で release する
- **v0.2.0**: 外側自動操作 API (`send` / `keys` / `paste` / `wait` / `tail` / `lock` / `tx`)
  の本実装と `serve` gateway。MVP 土台の上に乗る機能群
- **v0.3.0+**: 高度な制御 (leader 操作、画面状態述語、record/replay) と pair-programming
  用機能

### Re-scope の経緯 (2026-05-27)

初版 (2026-05-26) では v0.1.0 に `send` / `keys` / `paste` / `detach` / `lock` / `unlock` /
`tx` / `status` / `tail` / `wait` まで全部入れる計画だった。実装フェーズで判明したこと:

- protocol 基盤 ([[DR-0008]]) のゼロベース再設計 + multi-attach + cap negotiation だけで
  既に大きな工数になった
- 自動操作 API は protocol 上の control message 集合として綺麗に積めるが、それぞれ
  独立した実装フェーズが必要 (= 1 リリースに詰め込むより小刻みに出すほうが回しやすい)
- 「土台が動く」状態を先に release しておけば、後続の自動操作 API は破壊的変更なしで
  cap flag 追加だけで足せる (= DR-0008 の schema evolution が活きる)

→ v0.1.0 を「daemon + multi-attach + protocol cap negotiation までの土台」に絞り、
自動操作 API を v0.2.0 に押し出す形に変更。実装と DR の同期を取る。

### 命名規則の確定: `--session` / `--size`

初版では `--name` / `--window-size` と書いていたが、実装は `--session` / `--size` を採用。
理由:

- **`--session`**: 「daemon が抱える単位」を industry 標準語彙 (= DR-0008 §4 で確定した
  `session` 用語) と揃える。`--name` は意味が広すぎる (= 何の名前?)
- **`--size`**: PTY の window size は context から自明 (CLI 全体が PTY のための tool)。
  短い flag のほうがタイプ量も少なく ergonomics が良い

実装 (`--session` / `--size`) を正にして、本 DR の表記もそれに合わせて統一する。

## Decision

### v0.1.0 (MVP — 確定 = 2026-05-27 release 済)

**daemon ライフサイクル + multi-attach + protocol cap negotiation 土台**:

```
hyoui run [--session=ID] [--detached] [--exclusive] [--size=COLSxROWS] [--socket=PATH] -- cmd args...
hyoui attach <session> | --socket=PATH [--mode=rw|ro|rw-no-leader] [--exclusive] [--detach-others]
hyoui list
hyoui kill <session> | --socket=PATH [--signum=N]
```

内部実装:
- daemon: pty fork、socket bind、multi-attach (= Session::serve、broadcast + multiplex)
- protocol: CBOR ハイブリッド framing ([[DR-0008]])、cap flags 一本の schema evolution
- attach client (`hyoui attach`): handshake + raw bytes 中継、`Ctrl-A D` detach prefix、
  `Ctrl-A Ctrl-A` で literal escape
- leader 内部メカニズム (winsize 主体、cascade `latest`)
- lock state machine (acquire / release / token verify、ただし wait queue は v0.2.0)
- subscription 切替 (Raw / TailFollow)、scrollback ring buffer
- bounded queue 厳密化 (byte-level cap + backpressure.disconnect)
- 統計: 208 tests pass (v0.1.0 release 時点)

### v0.2.0 (外側自動操作 API + serve gateway、実装中)

**自動操作 core を本実装**:

```
hyoui send <session> [--file PATH | --text T]
hyoui keys <session> <spec>...                        # text:/key:/wait:/wait-idle: prefix
hyoui paste <session> [--text|--file|--spool|--max-size|...]
hyoui detach <session> [--all | --others]
hyoui status <session> [--format text|json]
hyoui tail <session> [--follow]
hyoui wait <session> [--idle|--text|--pattern|--then-idle] [--timeout]
hyoui lock <session> [--timeout-* ...] [--mode wait|fail]
hyoui unlock <session> [--token T | --force]
hyoui tx <session> [--timeout-* ...] -- cmd args...
hyoui completion <shell>
```

> 注: `status` / `tail` / `wait` の daemon 側 handler は v0.1.0 直前で実装済 (Phase 11)、
> CLI subcommand は v0.2.0 で proper に提供する。protocol の cap 名 (`tail-v1` / `wait-l0`)
> も予約済。残作業は parser、help text、CLI 引数のフル展開。

`serve` gateway 追加:

```
hyoui serve [--bind 127.0.0.1] [--port 6978] [--auth none|token|file:PATH]
            [--tls cert,key] [--static-dir PATH] [--allow-spawn]
```

- 別 crate (`crates/hyoui-serve` + `crates/hyoui-serve-cli`)、core は HTTP 依存を持たない
- xterm.js + WebSocket binary、protocol は v0.1.0 の wire format を流す
- default port `6978` (QWERTY 物理配置: h y o u i ≒ 6 9 7 8)

実装内訳:
- bracketed paste 自動検出 + in-flight state best-effort end 保証
- HYOUI_LOCK_TOKEN env 自動継承
- nest 起動検知 ($HYOUI_NAME / $HYOUI_SOCK 注入)
- wait queue (= `lock.acquire wait=true` の queue 対応)
- tail.data の chunk 境界保持
- TailEnd(ChildExited) を child exit 時に tail subscriber へ broadcast
- detach prefix の customize (`--detach-prefix` env / option)
- ANSI escape strip の chunk 境界跨ぎ正規化

### v0.2.0 候補 (= 仕様再検討中、ROADMAP 参照)

```
hyoui snapshot <session> [--rect X,Y,W,H] [--format text|ansi|json]   # 画面 dump (要 vte)
hyoui wait <session> --rect X,Y,W,H --pattern R                       # L1: 画面 rect 指定
hyoui wait <session> --cursor X,Y                                     # cursor 位置確認
hyoui wait <session> --child-exit                                     # 子 exit 待ち
```

screen emulator (= vte ベース cell grid 管理) を入れるかどうかは v0.2.0 の中盤で判断
([[docs/ROADMAP.md]] 参照)。

### v0.3.0+ (高度な自動化 + 永続記録)

```
hyoui wait <session> --area input-line --pattern R   # L2: named area
hyoui leader show/take/give <session>                # leader 露出
hyoui attach <session> --as-leader
hyoui tx <session> --buffered                        # 他 client の入力を蓄積、tx 後 flush

# sink 概念 (dump/record/play 統合)
hyoui sink add <session> --output FILE [--format=raw|cast] [--rotate=size:10MB,age:1h]
hyoui record <session> --output FILE                 # sink add の cast format alias
hyoui play <session> --input FILE [--speed] [--input-only|--output-only]
```

実装:
- config-driven area alias (`input-line`, `status-bar` 等の semantic 名)
- leader CLI 露出 (v0.1.0 で内部実装済、解放するだけ)
- tx buffered mode (他 client の入力ロストを防ぐ、複雑度高)
- sink: daemon 内永続出力先、tail (ad-hoc) と区別

## Rejected alternatives

### v0.1.0 に自動操作 API (`send` / `keys` / `paste` / `wait` / `tail` / `lock` / `tx`) を全部入れる

- 初版で計画したが re-scope (上記 Context 参照)
- 土台 (protocol + multi-attach + cap negotiation) を先に release することで、後続の
  自動操作 API を **schema evolution の枠組み内で破壊変更なしに足せる**
- 「動く土台」を release できる時期が大きく前倒しになる
- 自動操作 API は cap flag (= `tail-v1`, `wait-l0`, `lock`) で個別に enable できるため、
  小刻みに段階リリースしても矛盾しない

### v0.1.0 に serve を入れる

- 思想 (外側自動操作主軸) のためには serve が無くても CLI 経由で十分価値あり
- HTTP/WebSocket 依存を core に持ち込むと crate 分離設計が後手になる
- v0.2.0 で別 crate として綺麗に被せる

### v0.1.0 に leader CLI を露出

- ユーザ (kawaz) の主用途は 1 人運用、leader 手動操作は不要
- 内部実装は v0.1.0 から持つ (HTTP gateway 展開時に解放、CLI 追加だけで動く)
- API surface を最小に保つ

### v0.1.0 に画面 emulator を入れる

- pty screen emulator は実装重 (vt100 ステート、escape sequence 完全解釈)
- L0 (stream regex) で 8 割の自動化はカバーできる、L1 は v0.2.0 で

### `--name` / `--window-size` 表記を残す

- 初版の DR では `--name` / `--window-size` と書いたが、実装は `--session` / `--size` で揃えた
- 既に v0.1.0 で release 済の表記を正にして DR を追従させる
- 命名理由: `session` は DR-0008 の用語と整合、`size` は context 自明で短い側を採用

## Consequences

### v0.1.0 の実装範囲 (= 確定済、release 済)

- protocol design ([[DR-0008]] 確定)、CBOR ハイブリッド framing、cap negotiation
- daemon: pty 起動、socket bind、multi-attach、leader、lock 基本、subscription
- CLI: `run` / `attach` / `list` / `kill` の 4 コマンド
- attach client: `Ctrl-A D` detach、`Ctrl-A Ctrl-A` literal escape
- 統計: 208 tests pass

### v0.2.0 への準備 (v0.1.0 時点で済んでいる)

- protocol を transport から完全に独立 (= `UnixStreamTransport` + 抽象 `Transport` trait)
- `hyoui::client::AttachClient` 相当を library API として pub export
- daemon discovery (`hyoui list` 相当) を library で叩ける形に
- multi-attach + leader を v0.1.0 で完成 (= ブラウザ + ローカル端末の同時 attach がそのまま動く)
- subscription 切替の仕組み (= `tail` / `wait` の daemon 側 handler は実装済、CLI は v0.2.0 で)

### 後付け順序 (v0.3.0 以降)

順序は需要次第で柔軟、ただし依存関係:

- leader CLI 露出は v0.1.0 内部実装に依存、独立に追加可能
- L2 (named area) は L1 (画面 emulator) に依存
- buffered tx mode は base lock 機構 (v0.1.0) に依存

## 関連

- [[DR-0005]] — 思想再定義
- [[DR-0006]] — CLI ground rules (本 DR で段階分割)
- [[DR-0008]] — protocol design (cap flags ベースの schema evolution が本 re-scope の前提)
- `docs/journal/2026-05-26-cli-design-discussion.md` — 議論の経緯
- `docs/journal/2026-05-26-night-phase10-11-release.md` — v0.1.0 release 時点の実装状態
- `docs/ROADMAP.md` — 未実装機能の振り分け先
