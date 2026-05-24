# DR-0007: MVP scope と段階リリース (v0.1.0 / v0.2.0 / v0.3.0)

- Status: Active
- Date: 2026-05-26
- Related: DR-0005 (思想), DR-0006 (CLI ground rules)

## Context

[[DR-0006]] で確定した API surface は大きい。一気に v0.1.0 で全部出すと開発工数が膨大、
かつ「動くもの」を出すまでの時間が伸びる。段階分割が必要。

優先度判断基準:

- **MVP (v0.1.0)**: 思想 (DR-0005) のコア = 「外側からの自動操作主軸」が成立する最小セット
- **v0.2.0**: 発展ユースケース (remote attach、高度な TUI 自動化) を支える機能
- **v0.3.0+**: 高度な制御 (leader 操作、画面状態述語) と pair-programming 用機能

## Decision

### v0.1.0 (MVP)

**daemon ライフサイクル + 基本 attach + 自動操作 core + 排他**:

```
hyoui run [--name N] [--detached] [--exclusive] [--window-size=...] [--socket P] -- cmd args...
hyoui attach <name> [--read-only] [--no-leader] [--detach-others]
hyoui detach <name> [--all | --others]
hyoui list [--format text|json|jsonl]
hyoui status [name] [--format text|json]
hyoui kill <name> [--signal SIG]
hyoui send <name> [--file PATH | --text T]
hyoui keys <name> <spec>...                        # text:/key:/wait:/wait-idle: prefix
hyoui paste <name> [--text|--file|--spool|--max-size|...]
hyoui tail <name> [--follow]
hyoui wait <name> [--idle|--text|--pattern|--then-idle] [--timeout]
hyoui lock <name> [--timeout-*] [--mode wait|fail]
hyoui unlock <name> [--token T | --force]
hyoui tx <name> [--timeout-*] -- cmd args...
hyoui completion <shell>
```

内部実装:
- leader (winsize 主体、cascade `latest`)
- HYOUI_LOCK_TOKEN env 自動継承
- bracketed paste 自動検出 + in-flight state best-effort end 保証
- nest 起動検知 ($HYOUI_NAME 注入)
- daemon の wire format は transport 独立 (Unix socket 経由、後で TCP/WebSocket に被せられる構造)

### v0.2.0

**remote attach + TUI 自動化高度化**:

```
hyoui serve [--bind 127.0.0.1] [--port 6978] [--auth none|token|file:PATH]
            [--tls cert,key] [--static-dir PATH] [--allow-spawn]
hyoui snapshot <name> [--rect X,Y,W,H] [--format text|ansi|json]
hyoui wait <name> --rect X,Y,W,H --pattern R     # L1: 画面 rect 指定
hyoui wait <name> --cursor X,Y                    # cursor 位置確認
```

実装:
- 別 crate (`crates/hyoui-serve` + `crates/hyoui-serve-cli`)、core は HTTP 依存を持たない
- xterm.js + WebSocket binary、protocol は v0.1.0 の wire format を流す
- daemon に pty screen emulator (`vte` crate 等) を埋め込み、画面 grid を保持
- default port `6978` (QWERTY キーボード物理配置で hyoui = h(=6) y o u i = 6 9 7 8)

### v0.3.0

**高度な自動化 + pair-programming**:

```
hyoui wait <name> --area input-line --pattern R   # L2: named area
hyoui leader show <name>                           # leader 露出
hyoui leader take <name>
hyoui leader give <name> <client-id>
hyoui attach <name> --as-leader
hyoui tx <name> --buffered                          # 他 client の入力を蓄積、tx 後 flush
```

実装:
- config-driven area alias (`input-line`, `status-bar` 等の semantic 名)
- leader CLI 露出 (v0.1.0 で内部実装済、解放するだけ)
- tx buffered mode (他 client の入力ロストを防ぐ、複雑度高)

## Rejected alternatives

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

### v0.1.0 に lock/tx を入れない

- HTTP gateway を被せる前提で「複数 client での自動操作」が想定される
- v0.1.0 で lock 機構が無いと、HTTP gateway 追加時に競合制御が後付け困難
- 実装は意外と軽い (daemon 内に `lock_holder: Option<client_id>` + protocol message 2 種)
- → MVP に含める

## Consequences

### v0.1.0 の実装範囲 (見積もり)

- protocol design (wire format、transport 独立)
- daemon: pty 起動、socket bind、multi-attach、leader、lock、paste in-flight state
- CLI parser: text:/key:/wait:/wait-idle:/spool=/ など多数の prefix・enum、timeout 3 種同時、symbolic key alias
- 各 subcommand の dispatch (16 個 + completion)
- 自動操作系のテスト (mock pty、複数 client 同時 attach、lock 取得競合、bracketed paste cleanup)

### v0.2.0 への準備 (v0.1.0 時点で済ませる)

- protocol を transport から完全に独立 (Unix socket / TCP / WebSocket で同じ wire format)
- `hyoui::client::AttachClient` を library API として pub export
- daemon discovery (`hyoui list` 相当) を library で叩ける形に
- multi-attach + leader を v0.1.0 で完成させる (= ブラウザ + ローカル端末の同時 attach がそのまま動く)

### 後付け順序 (v0.3.0 以降)

順序は需要次第で柔軟、ただし依存関係:

- leader CLI 露出は v0.1.0 内部実装に依存、独立に追加可能
- L2 (named area) は L1 (画面 emulator) に依存
- buffered tx mode は base lock 機構 (v0.1.0) に依存

## 関連

- [[DR-0005]] — 思想再定義
- [[DR-0006]] — CLI ground rules (本 DR で段階分割)
- `docs/journal/2026-05-26-cli-design-discussion.md` — 議論の経緯
