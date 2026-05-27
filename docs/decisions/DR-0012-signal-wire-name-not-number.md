# DR-0012: signal wire を u8 number から signal name string に変更

- Status: Active
- Date: 2026-05-27
- Related: [[DR-0007]] (MVP / 段階リリース), [[DR-0008]] (protocol 設計、部分上書き), [[DR-0010]] (v0.2.0 scope), R5-POSIX-C1, R5-C4

## Update (2026-05-27): version 区切り廃止

[[DR-0013]] 起票に伴い、本 DR で言及している version 区切り (= v0.1.x / v0.2.0 等) は廃止。scope の正本は [`docs/ROADMAP.md`](../ROADMAP.md) (= 4 層列挙型) を参照。signal wire name 化の方針自体は維持、ROADMAP `追加予定` に登録。

## Context

R5 (POSIX 委員) レビュー指摘 R5-POSIX-C1 / R5-C4:

- 現状の wire protocol は signal を **生 number (`u8`)** で送る。
  - `protocol/messages/control.rs::Signal { signum: u8 }`
  - `protocol/messages/lifecycle.rs::Kill { signum: Option<u8> }`
  - [[DR-0008]] §protocol L205 で `"signum": <uint> // POSIX signal number (SIGINT=2, SIGTERM=15, etc.)` と明文化済み
- **POSIX.1-2008 は signal number の数値を規定していない**。`<signal.h>` で macro 名
  (`SIGINT` / `SIGTERM` 等) が定義されることのみが MT。数値は OS により異なる。
  代表的な差:

  | signal     | Linux | macOS / *BSD | Solaris / illumos |
  |------------|-------|--------------|-------------------|
  | `SIGUSR1`  | 10    | 30           | 16                |
  | `SIGUSR2`  | 12    | 31           | 17                |
  | `SIGCHLD`  | 17    | 20           | 18                |
  | `SIGSTOP`  | 19    | 17           | 23                |
  | `SIGTSTP`  | 20    | 18           | 24                |
  | `SIGINFO`  | 該当無し | 29        | 該当無し          |
  | `SIGHUP=1` / `SIGINT=2` / `SIGQUIT=3` / `SIGKILL=9` / `SIGTERM=15` は概ね同じ |

- 現在は local Unix domain socket = 同一ホスト = 同一 OS = 同一 numbering なので
  実害ゼロ。だが [[DR-0010]] §2 の **v0.2.0 serve gateway**
  (`kawaz/hyoui-serve` = HTTP/WebSocket remote 制御) が入った瞬間、cross-OS の
  client が daemon を叩く構図が現実化する:

  > `hyoui send-signal --remote linux-host --signum 30`
  > → macOS daemon が `SIGUSR2` ではなく `SIGCHLD` を解釈してしまう

- [[DR-0008]] §protocol L205 のコメント `SIGINT=2, SIGTERM=15` は「POSIX が決めている」
  と読み取れる文面になっているのは厳密には誤り。**de facto に portable なのは
  低位 1〜15 のみ**で、それ以上は OS 依存。
- protocol breaking なので **v0.2.0 serve gateway 着手前** にやるのが筋。
  serve gateway 後だと client/server pair が増えて固定が難しくなる。

## Decision

### 1. wire 上は **signal name string** を送る

```cbor
{
  "kind": "signal",
  "signal": "SIGTERM"          // 正式 SIG-prefix 大文字表記
}
```

```cbor
{
  "kind": "kill",
  "signal": "SIGKILL"          // 省略可 (= null) なら daemon 側で SIGTERM default
}
```

- field 名は `signum` → **`signal`** (u8 number ではなく name を扱うことを名前で表現)
- 値域は POSIX (`SIGHUP` / `SIGINT` / `SIGQUIT` / `SIGABRT` / `SIGKILL` / `SIGTERM` /
  `SIGUSR1` / `SIGUSR2` / `SIGCHLD` / `SIGCONT` / `SIGSTOP` / `SIGTSTP` 等) +
  daemon の running OS が解釈可能な signal 名すべて
- **正規表記は `SIG` prefix + 大文字** ("SIGTERM")。小文字 ("sigterm") や略名
  ("TERM") は wire では受理しない。CLI 側で対応する場合は CLI 入口で大文字化する
  (= wire の前で正規化)
- 数値表現 ("15") も wire では受理しない (= 旧 signum との曖昧さ排除、未来の混乱回避)

### 2. 受信側は OS native value に解決

daemon は受信した signal name を libc の signal value に解決してから `kill(2)` を発行:

```rust
// crates/hyoui/src/daemon/control.rs (新設 helper)
pub(super) fn signal_name_to_nix_signal(name: &str) -> Option<Signal> {
    // "SIGTERM" → nix::sys::signal::Signal::SIGTERM
    // 内部実装は nix::sys::signal::Signal::from_str を呼ぶか、
    // 主要 signal を網羅した手書き match で対応する。
}
```

`nix::sys::signal::Signal` は OS 依存の `Signal::SIGUSR1` 等 variant を持つので、
**daemon の running OS の数値に自動解決** される。

### 3. 未知 signal name は `signal.invalid` で reject

- 未知 name (typo `SIGTREM`、別 OS 固有 `SIGINFO` を Linux daemon に送る、等) は
  既存 [`ErrorCode::SignalInvalid`](#) で reject
- BSD-specific (`SIGINFO`, `SIGEMT`) や Linux-specific (`SIGPWR`) は **daemon の OS
  で nix が `Signal` variant を提供していなければ自動的に reject** される

### 4. CLI 入口の更新

`hyoui kill --signum N` → **`hyoui kill --signal NAME`**:

- 旧 `--signum 15` → 新 `--signal SIGTERM`
- 旧 `--signum 9` → 新 `--signal SIGKILL`
- **数値は v0.2.0 以降一切受理しない** (= deprecation period を設けない、v0.2.0 が
  major breaking なので)。シンプル方針を優先

### 5. [[DR-0008]] §protocol L181 / L202-208 を本 DR で部分上書き

DR-0008 の Signal / Kill message 記述 (`signum: <uint>` 形式) を本 DR で
`signal: <text>` 形式に書き換える。DR-0008 側には本 DR への annotate を残す
(旧 schema は v0.1.x まで有効、v0.2.0 で本 DR の schema に切替)。

### 6. protocol breaking とリリース時の扱い

- 既存 v0.1.x peer (= 旧 `signum` u8 形式) との **wire 互換性なし**。新旧混在不可
- v0.2.0 リリース時に CHANGELOG / README で breaking change として明示
- DR-0008 §3 (= "Schema evolution = cap flags 一本") の forward-compat policy
  は **field 追加** には適用されるが、本件は **field rename + 型変更 (u8 → string)**
  なので forward-compat 範疇外。新名 (`signal`) を使うことで旧 field
  (`signum`) との衝突なく "未知 field は ignore" policy で旧 client が新 daemon
  に当たれば silent drop されるが、daemon は signal を解釈できないため事実上 reject
  と等価

## Rejected alternatives

### (a) u8 のまま「signal number 表」を spec で固定 (Linux 値 etc.)

- BSD / Solaris の lib level macro と値が衝突するので **物理的に不可能**
  (= "SIGUSR1=10" を強制すると macOS の `<signal.h>` を書き換えることになる)
- gateway 側で「Linux→macOS の signum 変換表」を持つ案もあるが、SIGINFO のような
  対応関係が無い signal で穴が空く
- → 不採用

### (b) u16 + namespace tag

`{ "ns": "linux", "signum": 10 }` のような namespace 構造で送り、受信側で OS 値に解決:

- name string ("SIGUSR1") のほうが読みやすい・debug しやすい・spec に書きやすい
- namespace 維持のコストが解決のコストより高い (= "ns=linux で signum=10" を
  daemon 側で持つ全 OS 用 mapping table が必要)
- → 不採用

### (c) signal name + signum 両方持つ

```cbor
{ "kind": "signal", "signal": "SIGTERM", "signum": 15 }
```

- 冗長 (= name から signum が一意に決まる、wire bytes の無駄)
- 矛盾時のルール (`signal` vs `signum` どちらが prior か) を定める必要が出る
- → 不採用

### (d) 旧 `signum` u8 を deprecation period 付きで残す (v0.2.0 で warn、v0.3.0 で削除)

- v0.2.0 は major breaking release ([[DR-0007]])。後方互換ペナルティは v0.2.0 で
  一括清算する方針と整合
- u8 と string の両方を受理する parser は schema を複雑にする (= `signum` が
  Option<u8> として残り、`signal` が新規 Option<String> として追加され、両者
  null チェックと矛盾検査が必要)
- → 不採用、シンプル方針 (v0.2.0 で完全切替) を選択

## Acceptance criteria

- [x] `Signal { signal: String }` / `Kill { signal: Option<String> }` に schema 変更
- [x] daemon の `handle_signal` / `handle_kill` が signal name を解決して `kill(2)` 発行
- [x] 未知 signal name は `ErrorCode::SignalInvalid` で reject
- [x] CLI `hyoui kill --signal NAME` (旧 `--signum N` は reject)
- [x] BSD-specific signal name (`SIGINFO`) を Linux で送ると reject される (= daemon
      OS で `nix::Signal` variant がなければ unknown 扱い)
- [x] [[DR-0008]] §protocol §181 / §202-208 に本 DR への annotate

## Why now (= v0.2.0 着手前)

[[DR-0010]] §2 で serve gateway を別 repo `kawaz/hyoui-serve` に切り出すと決めた。
serve gateway は wire protocol を素通しで relay する設計なので、**gateway 着手前に
wire schema を確定**しないと、後から signal field を変更すると gateway 側 (websocket
client) の breaking も発生する (= 影響範囲が広がる)。v0.2.0 が初の breaking release
なので、breaking を 1 回で清算する。
