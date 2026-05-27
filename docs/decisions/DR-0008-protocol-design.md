# DR-0008: protocol 設計 — CBOR ハイブリッド framing、cap flags ベースの schema evolution

- Status: Active
- Date: 2026-05-26
- Related: DR-0005 (思想)、DR-0006 (CLI ground rules)、DR-0007 (MVP scope)、PoC 01-08 findings (特に [[2026-05-26-fd-passing-vs-stream]]、[[2026-05-26-multi-attach]]、[[2026-05-26-lock-token-env]])

## Update (2026-05-27): [[DR-0013]] で structured state access message + cap flag を追加

[[DR-0013]] §10 で以下を追加:

- `ScreenDumpRequest` / `ScreenDumpResponse` (= cap flag `screen-dump-v1`)
- `StateSnapshotRequest` / `StateSnapshotResponse` (= cap flag `state-snapshot-v1`)
- Phase B 移行時に `dirty-lines-v1` (= per-line SequenceNo + pull 型 protocol、`DirtyLinesNotify` / `GetLinesRequest` / `GetLinesResponse`)

既存 raw bytes layer (= TYPE_RAW_DATA) は維持、TYPE_CBOR_CONTROL 経由で新 message 追加。cap flag negotiation は本 DR の既存機構を活用、breaking change なし (= 既存 client は新 message を見ない、cap flag で gating)。

PDU serial 番号 (= wezterm `codec/src/lib.rs:67` パターン、out-of-order tolerant + RTT 計測) を CBOR control message に入れる検討は Phase B の実装で確定。

## Context

[[DR-0006]] で「protocol は transport (Unix socket / TCP / WebSocket) から独立」と宣言。PoC 04 ([[2026-05-26-fd-passing-vs-stream]]) で SCM_RIGHTS 不採用 = stream 中継一本化を確定。本 DR は **wire format / 制御メッセージのモデル / handshake / capability negotiation / transport 抽象** をゼロベースで設計する。

### 要件 (思想・PoC 知見から導出)

| # | 要件 | 出所 |
|---|------|------|
| R1 | daemon は子 PTY を 1 つ持ち、複数 client が同時 attach できる (broadcast + multiplex) | DR-0005, PoC 02 |
| R2 | 子 PTY との data 流路は **raw bytes 透過** (ANSI escape / binary 含む)、変換しない | DR-0006, [[2026-05-26-ansi-strip]] |
| R3 | 制御メッセージ (handshake, resize, signal, lock, leader, mode change, status, tail, wait, detach, kill, error) が存在 | DR-0006 §3-8 |
| R4 | transport は Unix socket (MVP)、将来 TCP / WebSocket / SSH stdio。**stream 系のみ** | DR-0006, [[2026-05-26-fd-passing-vs-stream]] |
| R5 | 信頼境界は **同 UID** (socket perm 0600 + dir 0700 + lock token)、暗号化なし | [[2026-05-26-lock-token-env]] |
| R6 | client は env (`HYOUI_LOCK_TOKEN`) から token 自動継承、handshake で提示 | [[2026-05-26-lock-token-env]] |
| R7 | scrollback / status / wait は将来「画面 dump (cell grid)」「regex captures」等の rich payload を扱う | DR-0007 v0.2.0+ |
| R8 | 依存量は「正当な依存追加は受け入れる」(scrollback/strip は純 Rust だが、cross-lang/schema-evolution の価値がある依存は OK) | ユーザ確認 2026-05-26 |
| R9 | WebSocket 経由のブラウザ client (xterm.js / JS) を v0.2.0 で本気で目指す | DR-0007 v0.2.0、ユーザ確認 2026-05-26 |

### 別解の検討経緯

並行で外部議論 (artifact 3 件、2026-05-26) でも検証。要点:

- **bincode は RUSTSEC-2025-0141 で unmaintained**、wire format 候補から脱落
- **msgpack vs CBOR**: 機能はほぼ同等だが、CBOR は IETF RFC 8949 + 標準化された Diagnostic Notation (EDN) + Wireshark 標準 dissector + CBOR Sequences (RFC 8742) + indefinite-length encoding を持つ。debug tooling と standards-track な安定性で msgpack を上回る
- **protobuf / Cap'n Proto** は IDL toolchain (codegen + build.rs) が重く、terminal IPC には overkill
- **postcard** は Rust 専用 (R9 と矛盾)
- **JSON** は binary に base64 = 3x overhead (R2 の hot path で致命)
- **PtyMux Protocol** (Wez Furlong 提案、2024) は 2025-10 時点で stalled。仕様未公開、標準化挫折。互換は core で握りに行かず gateway 戦略 (後述)

## Decision

### 1. Wire format = CBOR ハイブリッド framing

#### 1.1 frame layout

```
+---------+--------+----------------+
| u32 LE  | u8     | body bytes     |
| size    | type   |                |
+---------+--------+----------------+
size = 1 (type byte) + body_len
```

- **size**: u32 LE、`type` byte + body の総 byte 数 (= `body_len + 1`)
- **type**: u8 demux tag、下記参照
- **body**: type に応じて raw bytes か CBOR item

最大 frame サイズ: **`size ≤ 16 MiB`** (= 16 * 1024 * 1024)。超過は protocol error → disconnect。

**wire 外枠 (= `u32 size` + `u8 type` + `body`) は永久固定**。breaking 変更はしない。理由は §3 schema evolution と一致 (= 「cap で wire 変更を交渉」が成立しないため、外枠は固定にする)。frame layout 自体が変わるレベルの将来要件が発生した場合は **別 protocol = 別 socket path** を使う (例: `~/.hyoui/sock/<name>.v2.sock`)。

理由:
- `size` 先頭で「あと何 byte 読めば 1 frame 取れるか」が即決定 = length-prefixed protocol の伝統、実装簡素
- hexdump で frame 境界が読みやすい (debug 性)
- 単一 frame layout (= type ごとに layout が変わらない) で decoder が綺麗

#### 1.2 type tag

| value | 意味 | body |
|-------|------|------|
| `0x00` | raw PTY data | `body` = 生 bytes (= ANSI escape 含む、透過)。length-prefix のみ |
| `0x01` | CBOR control message | `body` = 1 個の CBOR encoded item (典型は CBOR map) |
| `0x02..0xff` | **予約** | MVP では受信時 protocol error で disconnect |

理由 (ハイブリッド framing):
- raw PTY data は per-byte の hot path → CBOR で包むと overhead (map header / key / type) が無視できない
- 制御メッセージは秒に数回レベル → CBOR overhead は誤差、schema evolution / cross-lang / debug 性のメリットが大きい
- 「type byte → 残り body の解釈」だけの単純 demux で済む
- 将来 `0x02..` で ping/pong heartbeat、圧縮 data frame (例: LZ4)、CBOR Sequences ストリーム frame 等を追加余地

### 2. Control message vocabulary (= type `0x01` の body)

#### 2.1 形式: CBOR map with `kind` text key

```cbor
{
  "kind": "<dotted.kind>",
  ...payload fields...
}
```

- `kind` は text string、dotted naming (例: `"handshake.request"`, `"lock.acquire"`)
- payload は同じ map の他 key

理由:
- text key は debug 性最強 (`cbor-diag` でそのまま読める)
- enum 整数 tag (= DR-0008 旧案 0x00..) より cross-lang 親和性高い
- size 誤差は制御メッセージ頻度では無視できる
- dotted naming で sub-namespace を表現 (tmux/zellij 等の慣習に合う)

#### 2.2 kind 一覧 (MVP / v0.1.0)

| kind | 方向 | 用途 |
|------|------|------|
| `handshake.request` | client → daemon | 接続初回、cap negotiation + 認証 |
| `handshake.response` | daemon → client | cap 確定、session 情報返却 |
| `error` | 双方向 | 回復可能 error 通知 (protocol error 等) |
| `resize` | leader → daemon | TIOCSWINSZ |
| `signal` | client → daemon → 子 | 明示 signal (raw mode 中の Ctrl-C 代替等) |
| `lock.acquire` | client → daemon | lock 取得要求 |
| `lock.response` | daemon → client | lock 取得結果 |
| `lock.release` | client → daemon | lock 解放 |
| `leader.notify` | daemon → all clients | leader 変更通知 (broadcast) |
| `mode.change` | daemon → all clients | rw/ro/locked 状態変化 (broadcast) |
| `status.query` | client → daemon | session 状態問い合わせ |
| `status.response` | daemon → client | status 返却 |
| `tail.request` | client → daemon | scrollback の流し読み開始 |
| `tail.data` | daemon → client | tail chunk |
| `tail.end` | daemon → client | tail 終了 |
| `wait.request` | client → daemon | 出力条件待ち |
| `wait.result` | daemon → client | 条件成立 (or timeout) |
| `detach` | client → daemon | client 自身 (or `--all`/`--others`) を detach |
| `kill` | client → daemon | 子に SIGTERM → daemon exit |

v0.2.0+ 予約:
- `snapshot.request` / `snapshot.response` (画面 dump)
- `leader.request` (leader CLI 解放)
- `record.start` / `record.stop` / `play.start` (record/play)
- `sink.attach` / `sink.detach` (永続出力先)

#### 2.3 payload schema (主要)

**`handshake.request`**:
```cbor
{
  "kind": "handshake.request",
  "caps": ["data", "lock", "tail-v1", "wait-l0", ...],  // client が話せる capability の集合
  "mode": "rw" | "ro" | "rw-no-leader",
  "exclusive": <bool>,        // 起動時占有要求
  "detach-others": <bool>,    // attach 時に他を奪取
  "token": <text> | null      // HYOUI_LOCK_TOKEN env 由来、null なら未提示
}
```

**`handshake.response`**:
```cbor
{
  "kind": "handshake.response",
  "caps": ["data", "lock", "tail-v1", ...],  // daemon が話せる capability
  "session-id": "<name>",                    // session 名 (DR-0006 規定)
  "client-id": <uint>,                       // daemon が割り当てた client 番号
  "leader": <bool>,                          // leader 取得結果
  "mode": "rw" | "ro" | "rw-no-leader"       // 認証後の実 mode
}
```

cap negotiation: daemon は client の caps と自身の caps の intersection を「有効 capability」として扱う。client が unsupported feature を呼ぼうとしたら client 側で reject、daemon が unsupported request を受けたら `error` で kind=`"unsupported-capability"` を返す。

**`error`**:
```cbor
{
  "kind": "error",
  "code": "<error-code-string>",   // 例: "protocol.malformed", "lock.denied", "mode.not-leader"
  "message": "<human readable>",
  "details": { ... } | null        // 追加情報 (optional)
}
```

error code は dotted text string で人間可読性優先 (= 数値 enum よりデバッグしやすい)。**Rust API 上は [`ErrorCode`] enum (`crates/hyoui/src/protocol/messages/error.rs`) で構造化**、wire 上は引き続き dotted text で encode/decode する (= 手書き Serialize/Deserialize で 1:1 対応、R4-H13)。

**error code 一覧 (v0.1.0、R4-M11)** — 新 code を追加する際は本表と `ErrorCode` enum / `from_wire` を同期更新:

| wire code | `ErrorCode` variant | 回復性 | 用途 |
|---|---|---|---|
| `protocol.malformed` | `ProtocolMalformed` | 致命 | frame / CBOR が解釈不能。直後に disconnect |
| `protocol.unexpected-kind` | `ProtocolUnexpectedKind` | 回復可 | 受信した kind が方向 (client↔daemon) や状態的に不正 |
| `unsupported-capability` | `UnsupportedCapability` | 回復可 | cap negotiation 後に未対応 feature が呼ばれた |
| `handshake.timeout` | `HandshakeTimeout` | 致命 | handshake が制限時間内に完了しなかった |
| `auth.token-mismatch` | `AuthTokenMismatch` | 致命 | handshake の auth_token が一致しない |
| `backpressure.disconnect` | `BackpressureDisconnect` | 致命 | client 送信 queue が limit 超過、daemon が disconnect |
| `mode.not-allowed` | `ModeNotAllowed` | 回復可 | 現 mode (interactive / readonly 等) で許可されない操作 |
| `mode.not-leader` | `ModeNotLeader` | 回復可 | leader 専用操作 (resize 等) を non-leader が送った |
| `lock.denied` | `LockDenied` | 回復可 | lock acquire を既存 holder が拒否 |
| `lock.not-held` | `LockNotHeld` | 回復可 | lock 操作 (release 等) を保持していない client が送った |
| `signal.invalid` | `SignalInvalid` | 回復可 | signal 番号 / 名前が不正 |
| `wait.too-many` | `WaitTooMany` | 回復可 | wait 同時実行数の上限超過 |
| `wait.invalid-text` | `WaitInvalidText` | 回復可 | wait の text 引数が不正 |
| `wait.invalid-pattern` | `WaitInvalidPattern` | 回復可 | wait の pattern (正規表現) が不正 |
| `detach.target-partial` | `DetachTargetPartial` | 回復可 | detach 対象指定の一部が見つからない / 失敗 |
| `master.write-timeout` | `MasterWriteTimeout` | 致命 | client → master PTY への raw_data write が子の slow-reader で idle timeout 超過、daemon が disconnect (R5-C3) |
| (任意の未知 string) | `Unknown(String)` | — | 旧 binary が新 daemon から受け取った未知 code を drop せず保持 (前方互換) |

`ErrorCode` enum は `#[non_exhaustive]` のため、library user は match で必ず `_` arm を用意する (= 新 code 追加で既存 caller が壊れない)。未知 code を受信したら `ErrorCode::Unknown(String)` で wire 文字列を保持するので、log / debug 用途で原 string を失わない。

**`resize`**:
```cbor
{
  "kind": "resize",
  "cols": <uint>,
  "rows": <uint>
}
```

leader 以外が送ると daemon は `error` で kind=`"mode.not-leader"` を返す。

**`signal`**:
```cbor
{
  "kind": "signal",
  "signum": <uint>     // POSIX signal number (SIGINT=2, SIGTERM=15, etc.)
}
```

通常は raw PTY data の `0x00` frame に Ctrl-C (0x03) を含めれば pty の line discipline (ISIG flag 有効時) で SIGINT 発火する。`signal` は raw mode 中や明示送信ケース用。

**`lock.acquire`**:
```cbor
{
  "kind": "lock.acquire",
  "wait": <bool>,              // true = wait queue 参加 / false = fail mode
  "timeout-abs-ms": <uint> | null,  // 絶対 timeout (ms)、null なら無限
  "timeout-idle-ms": <uint> | null, // idle timeout (ms)、null なら無効
  "process-bound": <bool>      // process と寿命を紐付ける
}
```

**`lock.response`**:
```cbor
{
  "kind": "lock.response",
  "result": "acquired" | "queued" | "denied" | "timeout",
  "token": "<text>" | null,    // result="acquired" のみ token を返す
  "queue-position": <uint> | null  // result="queued" 時の順位
}
```

**`status.response`** / **`tail.request`** / **`wait.request`** の細部 schema は実装フェーズで詰める (cap flags で互換性は確保される)。

### 3. Schema evolution = cap flags 一本

#### 3.1 PROTOCOL_VERSION は廃止

CBOR map の self-describing 性 + cap flags の組み合わせで schema evolution を表現する。固定 version field は持たない。

#### 3.2 未知 field policy = ignore unknown

control message を decode する側は **未知 key を黙って無視する** (forward-compatible)。送信側は CBOR map に新 field を後付け追加できる。

ただし cap negotiation で「相手が話せない」と分かれば必須 field 欠落・format 不一致は `error` で notify する道筋を作る。

#### 3.3 wire 外枠は永久固定、extension は type tag 追加で forward-compat

frame layout 自体 (`u32 size` + `u8 type` + `body`) の変更は、handshake を decode する前段で起きるため cap flags では交渉不可能 (= cap を読むには CBOR control frame を decode する必要があり、それは frame 外枠が正しいことが前提)。これは設計上の循環なので **wire 外枠を永久固定** する方針 (§1.1) で解消する。

機能追加は以下の forward-compatible 経路のみ:

- **新 type tag 追加** (例: `0x02` = LZ4 圧縮 data frame、`0x03` = ping)。MVP は未知 type を protocol error で disconnect するが、cap 経由で「unknown type を ignore する」モードを将来追加できる (= 旧 daemon が新 client の `0x02` frame を黙って skip 可能になる)
- **新 control message kind 追加** (= CBOR map の `kind` 値)。未知 kind は decode 側で ignore (forward-compat)
- **既存 message の field 追加**。未知 field は ignore (§3.2)

これらは全て cap flags で「相手が話せるか」を確認した上で送る。

frame 外枠 (size + type + body) の変更が必要になった場合は **別 protocol = 別 socket path** で扱う (例: `~/.hyoui/sock/<name>.v2.sock`)。これは forward-compat ではなく fork (= 旧 v1 と新 v2 が同居)、運用負荷は大きいが circular dependency に陥らない。

#### 3.4 cap 命名規約

- 機能名は `noun-verb` or `noun-vN` (例: `"lock"`, `"tail-v1"`, `"snapshot-v1"`, `"wire-v1"`)
- 新 capability 追加は forward-compatible (= 未知 cap は ignore で良い)
- 既存 cap の semantics 変更は新 cap 名で表現 (= 旧名を残しつつ新名追加)

### 4. 用語 (industry 標準採用)

abduco/shpool に最も近い (= "minimal abduco + control infrastructure" 位置づけ) ので、語彙は industry 標準を採用:

| 用語 | 意味 (本 protocol 内) |
|------|---------------------|
| session | daemon + 子 PTY + scrollback の集合体 (= name で識別) |
| client | session に attach する process (CLI 単発、長期 attach、WS 等) |
| attach | client が session に接続して入出力を中継開始する操作 |
| detach | client が session から切断する操作 |
| leader | session 内で TIOCSWINSZ 計算対象になる代表 client (rw mode の 1 つに自動付与) |
| lock | 排他取得状態、token で識別 |
| mode | client の動作 mode (rw / ro / rw-no-leader) |
| scrollback | 過去出力の ring buffer (PoC 07 / src/scrollback.rs) |

`pane` は hyoui MVP では使わない (= 1 session = 1 PTY、pane 概念なし)。将来複数 pane を持つ session 形式を入れる場合は別 DR で扱う。

### 5. Transport 抽象 = 薄い Read + Write

```rust
// 概略 (確定形は実装時)
pub trait Transport: std::io::Read + std::io::Write + Send + 'static {
    fn close(self: Box<Self>) -> Result<()>;
}

pub struct FrameReader<T: Transport> { inner: T, ... }
pub struct FrameWriter<T: Transport> { inner: T, ... }
```

MVP: **`UnixStreamTransport`** (= `UnixStream` を `Transport` で wrap)

将来:
- `TcpStreamTransport` (= `hyoui serve` 内部)
- `WebSocketTransport` (= `tokio-tungstenite` 等、xterm.js 経由、binary frame で wire 透過)
- `StdioTransport` (= SSH stdio 経由、gateway 戦略の receiver)

全 transport は同じ frame layout (`[u32 size][u8 type][body]`) を流す。上層は `Frame { kind, body }` 単位で扱う。

### 6. PtyMux 互換 = gateway 戦略

PtyMux Protocol (stalled) との将来互換は以下の方針:

- **hyoui daemon 自身は独自 wire (本 DR の CBOR hybrid) のみ喋る**。マルチプロトコル化しない
- 将来 PtyMux 仕様が固まったら、**別 process (= `hyoui-ptymux-gateway` 等) を間に挟む**。hyoui daemon ⇔ gateway は hyoui wire、gateway ⇔ external client は PtyMux wire
- これによって core を複雑化せずに将来互換余地を残す
- 現時点 (2026-05-26) では PtyMux 仕様が公開されていないので、DR では「互換手段としての gateway を future-work として明記」のみ

### 7. 認証 / Authorization

- 信頼境界: **同 UID** (socket perm 0600 + parent dir 0700 = sys/socket.rs 既存実装)
- `handshake.request.token` で lock token 提示、daemon が照合 (= HYOUI_LOCK_TOKEN env 経由、[[2026-05-26-lock-token-env]])
- 暗号化なし (= 同 UID 信頼領域、別 UID は socket perm で完全遮断)
- TCP / WebSocket transport を追加する v0.2.0+ では別途 token-based authentication or TLS を DR 化

### 8. Concurrency / Ordering / Backpressure

#### 8.1 順序保証

- 1 client connection 内: send/recv は順序保証 (= TCP/Unix socket stream の性質)
- daemon が複数 client に broadcast: client 間で順序保証なし
- raw PTY data (= type `0x00`) の bytes は順序保証 (= 子の出力 bytes 順を維持)
- 複数 client の `0x00` frame (= stdin) は **client 間で interleave 可能**。lock/tx で防ぐのはユーザの責務

#### 8.2 Backpressure (= slow client 対策)

multi-attach broadcast の core 要件として、遅い client が全 session を止める事態を防ぐ必要がある。drop は terminal stream を破壊する (= ANSI escape 中断で画面崩壊)、無制限 buffer は OOM のリスク。よって以下の戦略を採用:

- **daemon は client ごとに bounded output queue を持つ**
  - 既定: `8 MiB` (= scrollback buffer の典型サイズと同程度)、daemon 起動時オプションで上書き可能 (= 実装フェーズで `--client-buffer-bytes` 等)
  - queue の単位は frame ではなく byte 数 (= 大 frame 1 個でも上限到達するため)
- **queue 超過時はその client を切る**
  1. daemon は当該 client に `error` (kind=`"backpressure.disconnect"`、`details = { "queued_bytes": ..., "limit": ... }`) を送る (queue 末尾に enqueue できる場合のみ、できなければ送らず即 close)
  2. 接続を close (= TCP RST / Unix socket close)
  3. 該当 client の `broadcast list` から除外、leader だった場合は cascade で次の rw client に移譲 (= leader 選出ロジックの一部)
- **drop はしない**: stream 破壊を避ける + 「黙って消える」より「明示的に disconnect」が運用上 debug しやすい
- **他 client は影響を受けない**: 遅い client を切ったあと、残りの client への broadcast は通常通り継続

実装上のヒント:
- daemon の broadcast 側は per-client mpsc channel (bounded、N bytes) + 専用 writer task で実装。enqueue 失敗 (= channel full) でその client を切る
- bound のサイズはユーザ要件次第 (= 一瞬の遅延を許容したいなら大きく、即切断したいなら小さく) なので runtime 設定可能にする

#### 8.3 client 側の対称ルール

client から daemon への送信側も同様の bound が daemon の receive 側にあると想定して、client は flush 失敗で abort する。実装は OS の socket buffer に任せる (= 明示 buffer は持たない) ことから始める。問題が出たら別途検討。

### 9. Encode / Decode の実装スタイル

- frame encode/decode は手書き (`Frame::encode_to<W: Write>` / `Frame::decode_from<R: Read>`)、依存ゼロ
- control message (= type `0x01` の body) は `#[derive(serde::Serialize, serde::Deserialize)]` + `ciborium::from_reader` / `ciborium::into_writer`
- 各 control message は struct で表現、`kind` field は struct の `kind: &'static str` (encode 時固定値) or enum 経由で処理
- ciborium は serde 経由なので derive で書ける → 手書き repetition なし、type safety 確保

### 10. テスト戦略

- **frame layer**: round-trip (encode → decode → 比較)、malformed input (= size 過大、未知 type tag、CBOR parse error) の error handling
- **control message**: 各 struct の round-trip、cap negotiation の組み合わせテスト
- **golden test fixtures**: 既知 message を CBOR encode した hex dump を `tests/fixtures/*.cbor.hex` に保存、wire 互換性 regression check
- **e2e**: client → daemon の handshake → data 中継 → detach フロー (= integration test)
- **mock transport**: `Transport` trait の in-memory 実装で daemon ロジックを単体 test

## Rejected alternatives

### bincode
RUSTSEC-2025-0141 unmaintained。主要 Rust プロジェクト (cargo, rustls 等) も移行進行中。wire format 候補としては排除。

### MessagePack (rmp-serde 等)
CBOR と機能ほぼ同等だが、debug tooling が劣る (annotated hex / web visualization / Wireshark dissector の差)。標準化観点で CBOR (IETF RFC 8949) が上。差は僅かだが CBOR を採用。

### protobuf / Cap'n Proto
IDL toolchain + codegen + build.rs が必要、binary size と build time の負担大。terminal IPC には overkill。schema evolution の堅さは魅力だが代償が大きい。

### postcard
Rust 専用 (= R9 cross-lang と矛盾)。serde 互換で軽量だが、ブラウザ client 想定が消える。

### JSON (serde_json)
debug 性は最高だが、raw PTY bytes を base64 化すると約 3x size overhead (R2 hot path で致命的)。text なのに binary を扱うミスマッチ。

### 自前 binary 手書き (= 議論初期 DR-0008 旧案)
依存ゼロは魅力だが:
- 各 message struct ごとに encode/decode 関数を手書き = メンテコスト
- cross-lang client (R9) で再実装する場合の負担大
- schema evolution の自由度が低い (= field 追加に各 message でロジック書き換え必要)
- bincode と比較する文脈で「wire format 完全制御」を理由にしていたが、CBOR でも wire layout は完全に決定論的 (= 同じ struct → 同じ bytes)、その利点はほぼ等価

### PROTOCOL_VERSION field
CBOR map の self-describing 性 + cap flags で表現力が足りる。固定 version は冗長。frame 外枠の breaking 変更は §3.3 のとおり別 socket path で fork する (= cap で交渉は循環するため不採用)。

### 先頭 magic + wire major version の prelude
案: socket connect 直後に `b"HYOUI\0"` + `u8 wire_major` を交換し、wire 外枠を将来 breaking 変更可能にする。これは frame layout 変更を許容する場合の正攻法。

不採用の理由: hyoui の MVP/v0.1.0 段階で wire 外枠を変える必要性が見えない (= type tag 追加 + control message field 追加で機能拡張は概ね足りる)。prelude を入れると最初の handshake までに 1 往復増える、エラーパス (= 古い client) で接続を切る判定箇所が増える。**「将来 wire を変えなければならなくなったら別 socket path に逃げる」** の方が外枠固定で済んで simple。
prelude の道を残すコストはあとから払うより、必要になってから初版 (= 別 socket path or 別 protocol) で導入する方が筋。

### MessageKind を enum 整数 tag で表現
DR 旧案では `MessageKind = 0x00..` の u8 enum を使っていた。CBOR では text string (例: `"handshake.request"`) の方が debug 性高く、size 差は誤差。整数 enum は採用しない。

### CBOR Tag (RFC 8949 §3.4) で kind 表現
CBOR Tag は user-defined range もあるが、汎用 tooling (cbor-diag / Wireshark) が tag 番号を見せても意味が分からない (= map key の方が読みやすい)。tag の overhead/可読性両面で中途半端、不採用。

### CBOR map で short key 採用 (= "k" 等で size 節約)
control message は秒に数回レベル、size 節約の必要性低い。debug 性 (`cbor-diag` でそのまま読める) が優先。

### CBOR Sequences (RFC 8742) を MVP で活用
scrollback の large dump や tail (--follow) で「区切りなしの CBOR item 連鎖」を使うと streaming に向く。ただし frame layer (= type `0x01` + length-prefix) と二重化、MVP では frame 単位で 1 CBOR item に統一。Sequences は v0.2.0+ の snapshot/tail-bulk で再評価。

### CBOR の crate 選定: serde_cbor / minicbor
- `serde_cbor` は unmaintained (= bincode と同じ運命)
- `minicbor` は no_std + serde 非依存、軽量だが独自 derive macro 必須、ecosystem 親和性が劣る
- **`ciborium` を採用** (active maintenance、serde 経由、ergonomic)

### hyoui daemon の PtyMux マルチプロトコル化
PtyMux 仕様自体が stalled (2025-10 時点で未公開 / 進展停止)。stalled な仕様への core 統合は本末転倒。gateway 戦略 (§6) で代用。

### data 流路と control 流路を別 socket
2 connection 構成は Unix socket では可能だが、WebSocket transport で multi-stream 化が必要 (subprotocol or stream multiplexing)。複雑化に対する利得 (= demux 簡素化) は frame 内の type byte demux で代用可能。**1 connection 内で frame type demux** を採用。

## Consequences

### 実装への波及

- **新 module**: `crates/hyoui/src/protocol/`
  - `mod.rs`: re-export
  - `frame.rs`: `Frame { type, body }`, `encode_to`, `decode_from`, type tag 定数, MAX_FRAME_SIZE
  - `messages/`: 各 control message struct (`handshake.rs`, `lock.rs`, `tail.rs`, `wait.rs`, `status.rs`, `lifecycle.rs`, `error.rs`, `control.rs` etc.) + serde derive
  - `transports/unix.rs`: `UnixStreamTransport` (MVP)
- **新 module**: `crates/hyoui/src/daemon/` (socket bind + multi-attach + 永続 PTY 管理)
  - 既存 `crates/hyoui/src/agent.rs` (v0.0.0 PoC、697 行) は本 DR の対象外、`crates/hyoui/examples/00-pty-wrapper.rs` に保全して削除 (PoC 00 扱い)
  - `cli.rs::run` は新 daemon module を呼ぶ形に置換
- **依存追加**: `ciborium`, `serde`, `serde_derive` (= `serde = { version = "1", features = ["derive"] }` + `ciborium`)

### Release タイミング (v0.0.x → v0.1.0)

- 中間 v0.0.x release は打たない (= protocol module だけ release しても client/daemon が無く動作しない)
- run / attach / detach / list / status / kill が全動作した時点で **v0.1.0** を打つ (DR-0007 MVP scope と整合)
- main は green を保ちながら progress、tag は v0.1.0 まで打たない
- release flow は kawaz/* 標準 (`pkf run bump-version --level=minor` → main push → CI が tag + GH Release 自動生成、`release-flow-awareness.md` 参照)

### テスト戦略

- 各 control message の round-trip (encode → decode → 比較)
- frame layer の malformed input (oversized, unknown type, truncated, CBOR parse error)
- golden test fixtures: 主要 message を CBOR encode した hex を `tests/fixtures/protocol/*.cbor.hex` に保存、wire 互換 regression 検出
- mock `Transport` 実装で daemon ロジックを単体 test
- e2e integration test: handshake → data 中継 → detach → cleanup

### 未確定事項 (実装フェーズで詰める)

- `status.response` の payload schema (= scrollback info, clients list, lock state, child pid 等)
- `tail.request` / `tail.data` / `tail.end` の細部 (= since cursor, follow, line vs byte モード)
- `wait.request` / `wait.result` の rich result (regex captures, position, timeout 結果)
- error code 一覧 (実装と並行で網羅、本 DR では枠のみ)
- cap flag 一覧の確定 (実装の進捗で追加)
- per-client buffer 既定値の妥当性 (= 8 MiB が現実的か、運用で観察)

これらは MVP 実装の中で個別 TaskCreate で詰める。

## 関連

- [[DR-0005]] — 思想 (外側自動操作主軸)
- [[DR-0006]] — CLI ground rules (本 protocol を呼び出す client 側)
- [[DR-0007]] — MVP scope (v0.1.0 で protocol + daemon + client)
- [[2026-05-26-fd-passing-vs-stream]] — SCM_RIGHTS 不採用、stream 中継一本化の根拠
- [[2026-05-26-multi-attach]] — daemon broadcast/multiplex の poll パターン
- [[2026-05-26-lock-token-env]] — env 経由 token 継承
- [[2026-05-26-poc-summary]] — PoC 全体まとめ
- 外部議論 artifacts (2026-05-26): CBOR 採用根拠、PtyMux 状況、industry 用語整理 ※ chat 内 ephemeral、URL は ハンドオフ参照
- RFC 8949 — CBOR (Concise Binary Object Representation)
- RFC 8742 — CBOR Sequences (将来活用)
