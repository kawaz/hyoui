# DR-0010: v0.2.0 scope re-scope + serve gateway 配置判断

- Status: Active
- Date: 2026-05-27
- Related: [[DR-0005]] (思想), [[DR-0006]] (CLI ground rules), [[DR-0007]] (MVP scope), R5-H4, R5-H5

## Update (2026-05-27): version 区切り廃止

[[DR-0013]] 起票に伴い、本 DR で言及している version 区切り (= v0.1.x / v0.2.0 等) は廃止。scope の正本は [`docs/ROADMAP.md`](../ROADMAP.md) (= 4 層列挙型) を参照。

### scope の status

- **input family 整理 (= text:/file:/hex:/paste: spec 統一)**: 正本維持。ROADMAP `優先` に登録
- **serve 別 repo**: 正本維持。ROADMAP `追加予定` に登録
- **version 区切り (= 旧 v0.2.0 scope)**: 廃止 (= 上記)

## Context

Round 5 (R5-Classic) で以下 2 点が指摘された:

- **R5-H4**: v0.2.0 ROADMAP に並ぶ subcommand 数 (= v0.1.0 確定 5 個 → v0.3.0+ 累計 23 個) が
  tmux と同 density に膨らんでおり、[[DR-0005]] で却下した「TUI multiplexer 化」の
  初期症状に見える。外形 (=`--help` の subcommand 一覧の長さ・密度) で「外側自動操作主軸」
  という思想と区別が付かなくなる懸念。
- **R5-H5**: serve gateway を `crates/hyoui-serve` (同 repo・別 crate) として切り出す
  [[DR-0007]] の方針は、Unix philosophy ("stdio-as-API") 観点では `websocketd hyoui attach $SESS`
  で実用 90% カバーでき、core が `nix + serde + ciborium + regex` の lean さを
  維持できなくなる点でコストに見合わない。別 repo / 別 binary の方が筋。

両者とも v0.2.0 着手前 (= scope 確定前) に判断しないと、着手後の変更工数が大きい。

## Decision

### 1. v0.2.0 subcommand を統合して 11 → 7 に圧縮 (R5-H4)

[[DR-0007]] v0.2.0 で予定していた 11 個 (`send` / `keys` / `paste` / `detach` / `status` /
`tail` / `wait` / `lock` / `unlock` / `tx` / `completion`) を以下の通り統合する:

- **`input` family** (= 旧 `send` / `keys` / `paste` を統合)
  - `hyoui input text <session> [--file PATH | --text T]`
  - `hyoui input keys <session> <spec>...` (`text:` / `key:` / `wait:` / `wait-idle:` prefix)
  - `hyoui input paste <session> [--text|--file|--spool|--max-size|...]`
  - 自動操作 API の主軸が「入力を送る」一系統であることを CLI 外形で表現する
- **`lock` family** (= 旧 `lock` / `unlock` / `tx` を統合)
  - `hyoui lock acquire <session> [--timeout-* ...] [--mode wait|fail]`
  - `hyoui lock release <session> [--token T | --force]`
  - `hyoui lock tx <session> [--timeout-* ...] -- cmd args...`
  - 排他制御という単一概念で 1 family にまとめる
- 残る v0.2.0 確定 subcommand: `input` / `detach` / `status` / `tail` / `wait` / `lock` / `completion`
  = **7 個**
- `snapshot` (元 v0.2.0 候補) は v0.3.0 に押下げ (詳細は §3)

#### Why 統合

- `hyoui --help` の縦長さで「TUI multiplexer 並みの subcommand 列」に見える状態を避ける
- nested family にすることで「概念単位の learning」になり、tmux のフラットな
  subcommand 群 (`new-session` / `attach-session` / `kill-session` / ...) と異質性を保てる
- 内部実装は変わらず (= dispatch を nested に変えるだけ)、後方互換は問題にならない (= v0.2.0 初出)

### 2. serve gateway は別 repo `kawaz/hyoui-serve` に切り出す (R5-H5)

[[DR-0007]] の「同 repo・別 crate (`crates/hyoui-serve` + `crates/hyoui-serve-cli`)」方針を
**本 DR で覆す**。新方針:

- **hyoui core repo** は同じく `kawaz/hyoui` (= `crates/hyoui`, `crates/hyoui-cli`) のまま
- **serve gateway** は別 repo `kawaz/hyoui-serve` として独立切り出し
- 暫定運用として `websocketd hyoui attach $SESS` パターンを README に
  "unofficial integration example" として掲載 (= 90% カバーの簡易解)

#### Why 別 repo

| 観点 | 同 repo (旧方針) | 別 repo (本決定) |
|---|---|---|
| core の dependency footprint | http/ws stack が混入 (axum/tokio-tungstenite 等) | `nix + serde + ciborium + regex` lean を維持 |
| release cycle | gateway 修正で core release を巻き込む | 独立 release、core の安定性を確保 |
| security audit boundary | gateway の attack surface が core repo に同居 | core (local socket only) と gateway (network exposed) の境界が明確 |
| CI 一本化 | ◎ | × (= 別 repo 分の workflow 整備が必要) |
| code 共有 ergonomics | ◎ (workspace 内 path dep) | △ (`hyoui` を crate dep として publish 経由で参照) |

「core が lean であることが思想 ([[DR-0005]]) の根幹」なので、別 repo の cost を払う。

#### 移行手順

1. v0.2.0 リリース時点では `hyoui-serve` 別 repo を作成済みにする (= 同時公開)
2. `kawaz/hyoui` README に `kawaz/hyoui-serve` へのリンクと、`websocketd hyoui attach`
   の unofficial 例を併記
3. `kawaz/hyoui-serve` は `hyoui` crate を依存に取り、`hyoui::client::AttachClient` 経由で
   セッションへ接続 (= protocol は v0.1.0 wire format をそのまま流す)

### 3. snapshot (画面 emulator) を v0.3.0 に押下げ

[[DR-0007]] v0.2.0 候補にあった `snapshot` / `wait --rect` / `wait --cursor` を v0.3.0 に押下げ。

#### Why

- Round 4 / Round 5 の自動化 PoC で L0 (= stream regex の `wait --text` / `wait --pattern`)
  が 8 割の自動化 use case をカバーしている (= journal 2026-05-26 + R4-H12 / R4-M21 の
  指摘で「snapshot 不在の影響は限定的」と確認)
- L1 (画面 emulator + rect 指定) は L2 (named area) との一体実装が筋であり、
  v0.2.0 に半端に L1 だけ入れるより v0.3.0 で L1+L2 まとめて出す方が設計上 cleaner
- v0.2.0 の scope を「外側自動操作の本実装」に絞ることで、リリース粒度を保てる

## Rejected alternatives

### (a) subcommand 統合せず 11 個のまま v0.2.0 リリース

- 主張: shell 補完が効くので 11 個でも UX 問題なし、`--help` を充実すれば learning curve も問題ない
- 却下理由:
  - [[DR-0005]] の「外形で TUI multiplexer と区別が付くこと」を満たせない
  - v0.3.0+ で更に膨らむ (= 累計 23 個) のが既定路線になり、scope creep が加速する
  - 統合は「内部実装を変えず dispatch を変えるだけ」で cost が低い (= 後でやる動機が薄れる)

### (b) serve gateway を同 repo `crates/hyoui-serve` のまま維持

- 主張: workspace 内 path dep で code 共有が ergonomic、CI 一本化、release tagging も単純
- 却下理由:
  - core の dependency footprint が `nix + serde + ciborium + regex` から膨らむ (= http/ws stack)
  - core release が gateway 修正に引きずられる (= 思想変更を伴わない PR で core が tag される)
  - security audit boundary が曖昧 (= network 露出する gateway と local-only core の混在)

### (c) snapshot を v0.2.0 に維持

- 主張: 「画面 emulator が無いと自動化 UX が tmux に劣る」という直感的な要望
- 却下理由:
  - L0 (stream regex) で 8 割カバーできているという観測事実
  - L1 単体だと L2 (named area) との設計接続が見えていない (= L1 だけ先行リリースすると
    後付け L2 で L1 API を破壊する risk が高い)
  - v0.2.0 scope を絞ってリリース粒度を保つ方が優先

## Consequences

- **[[DR-0007]] を部分的に上書き**: v0.2.0 subcommand 数、serve gateway 配置、snapshot 配置
  の 3 点。[[DR-0007]] 本文末尾にも annotate を追加する
- **`docs/ROADMAP.md` を再編集**: v0.2.0 セクションを `input` / `lock` family の nested 構成に
  書き換え、serve gateway を別 repo 化と annotate、snapshot を v0.3.0 に移動
- **v0.2.0 着手前に確定**: 着手後の subcommand リネーム / 別 repo 切り出しは工数が増えるため、
  本 DR を v0.2.0 着手の前提条件とする
- **`kawaz/hyoui-serve` 新 repo 作成タスク**が v0.2.0 リリース directly before に発生する
  (= 別 issue で起票予定)

## 関連

- [[DR-0005]] — 思想 (外側自動操作主軸、TUI multiplexer ではない)
- [[DR-0006]] — CLI ground rules (= 統合後の `input` / `lock` family 詳細仕様は本 DR ベースで update 予定)
- [[DR-0007]] — MVP scope (= 本 DR で部分上書き)
- [[DR-0008]] — protocol (= cap flags の前提は変わらず)
- `docs/ROADMAP.md` — v0.2.0 セクション (= 本 DR に従って再編集)
