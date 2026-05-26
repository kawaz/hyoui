# DR-0008: protocol 設計 — wire format / message kinds / handshake / transport 抽象化

- Status: Active
- Date: 2026-05-26
- Related: DR-0005 (思想)、DR-0006 (CLI ground rules)、DR-0007 (MVP scope)、PoC 01-08 findings (特に [[2026-05-26-fd-passing-vs-stream]])

## Context

[[DR-0006]] で「protocol は transport (Unix socket / TCP / WebSocket) から独立」と宣言、PoC 04 で SCM_RIGHTS 不採用 = stream 中継一本化を確定。本 DR で具体的な **wire format / message kinds / handshake / transport trait** を設計。

PoC 知見 ([[2026-05-26-poc-summary]]) を反映:
- daemon は子 pty 出力を broadcast、各 client stdin を multiplex (PoC 02)
- bracketed paste / alternate screen 自動検出は daemon の internal state (PoC 03)
- HYOUI_LOCK_TOKEN env 継承で client が自動 token 提示 (PoC 06)
- scrollback ring buffer + last_evicted_ts (PoC 07)
- 装飾除去 ANSI strip (PoC 08)

## Decision

### 1. Wire format: length-prefixed binary frame

```
Frame layout:
  +---------+--------+--------+--------------------+
  | u32 LE  | u8     | u8     | payload bytes      |
  | size    | type   | flags  |    ...             |
  +---------+--------+--------+--------------------+
  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  size = type(1) + flags(1) + payload (含 type/flags)
```

- **size**: u32 LE、`type` 以降の総 byte 数 (`type` と `flags` 含む)。1-byte type + 1-byte flags + payload = `size`
- **type**: u8、`MessageKind` enum (下記参照)
- **flags**: u8、拡張用 (現状全 message で 0)
- **payload**: type ごとに format 定義 (length-prefixed strings、u32/u16 LE 数値、binary bytes)

Max frame size: 16 MiB (`size < 16 * 1024 * 1024`)、超過は protocol error。

理由:
- 既存 `crates/hyoui/src/protocol.rs` の length-prefixed (u32 LE size + bytes) を拡張、後方互換
- 自前 encode/decode で軽量、依存追加なし
- bincode/msgpack/protobuf は将来 schema 安定化で乗り換え可能 (wire format は変えない)
- WebSocket binary frame でもそのまま流せる

### 2. Protocol version

```
PROTOCOL_VERSION = 1   // MVP
```

HANDSHAKE で交換、不一致なら ERROR で disconnect。

後方互換性ある変更 (= 新 message kind 追加、optional field 追加) は version 据え置き。breaking change (既存 message の format 変更、required field 追加) は version up。

### 3. Message kinds

```rust
#[repr(u8)]
pub enum MessageKind {
    // === Handshake (0x00..) ===
    HandshakeRequest  = 0x00,
    HandshakeResponse = 0x01,
    Error             = 0x02,

    // === Data stream (0x10..) ===
    DataFromChild     = 0x10,   // daemon → client (broadcast)、子 pty stdout
    DataFromClient    = 0x11,   // client → daemon (multiplex)、子 pty stdin

    // === Terminal control (0x20..) ===
    Resize            = 0x20,   // leader → daemon (TIOCSWINSZ)
    Signal            = 0x21,   // client → daemon → 子 (送信 sig 番号、文字コード代替)

    // === Lock / leader (0x30..) ===
    LockAcquire       = 0x30,
    LockResponse      = 0x31,
    LockRelease       = 0x32,
    LeaderRequest     = 0x33,   // v0.3.0 で解放
    LeaderNotify      = 0x34,   // daemon → client broadcast (leader 変更通知)
    ModeChange        = 0x35,   // daemon → client broadcast (rw/ro/locked 状態変更)

    // === Status / discovery (0x40..) ===
    StatusQuery       = 0x40,
    StatusResponse    = 0x41,

    // === Tail / wait (0x50..) ===
    TailRequest       = 0x50,
    TailData          = 0x51,
    TailEnd           = 0x52,
    WaitRequest       = 0x53,
    WaitResult        = 0x54,

    // === Lifecycle (0x60..) ===
    Detach            = 0x60,   // client が自分を detach (or --all/--others)
    Kill              = 0x61,   // daemon に kill 要求 (= 子に SIGTERM → daemon exit)

    // === 予約 (0x70..) ===
    // 将来追加用、未使用
}
```

### 4. Payload format (主要 message)

#### `HandshakeRequest` (0x00)

```
+-------------------+-----+-----+----------------+--------------------+
| protocol_version  | mode| ... | u16 LE token_len | token bytes (UTF8) |
| u16 LE            | u8  |     |                  |                    |
+-------------------+-----+-----+----------------+--------------------+
  mode:
    0 = rw (default)
    1 = ro (read-only、winsize 計算除外)
    2 = rw_no_leader (= rw だが leader 取らない)
  flags (byte after mode):
    bit 0: exclusive (= 起動時占有要求)
    bit 1: detach_others (= attach 時奪取)
  token_len: 0 なら token 無し
```

#### `HandshakeResponse` (0x01)

```
+-------------------+----------+---------+-----+-----+----------------+
| protocol_version  | client_id| leader  | mode| ... | server_caps    |
| u16 LE            | u32 LE   | u8 bool | u8  |     | u32 LE         |
+-------------------+----------+---------+-----+-----+----------------+
  client_id: daemon が割り当て
  leader:    true なら leader 取れた
  mode:      daemon が認証した実 mode (リクエスト時から変更されうる)
  server_caps: bit 0 = supports lock, bit 1 = supports snapshot, ...
```

#### `DataFromChild` / `DataFromClient` (0x10, 0x11)

payload = bytes (生)。length は frame の `size - 2` (= type + flags 除く)。

#### `Resize` (0x20)

```
+--------+--------+
| u16 LE | u16 LE |
| cols   | rows   |
+--------+--------+
```

leader 以外が送信した場合 daemon は無視 (or `Error` で notify)。

#### `Signal` (0x21)

```
+----+
| u8 |
| sig|
+----+
  sig: POSIX signal number (SIGINT=2, SIGTERM=15, SIGTSTP=20, etc.)
```

daemon が `kill(child_pgid, sig)` で子に送る。
通常は `DataFromClient` に Ctrl-C (0x03) 等を含めれば pty の line discipline が ISIG flag 有効 (cooked mode) なら SIGINT 発火する。
このメッセージは raw mode 中 (= ISIG disable) や明示的に signal を送りたいケース用。

#### `LockAcquire` (0x30)

```
+--------------+-----+-----+------------+------------+------------+
| timeout_abs  | ... | bit | abs DUR ms | idle DUR ms| bool process|
| flags u8     |     |     | u32 LE     | u32 LE     | u8         |
+--------------+-----+-----+------------+------------+------------+
  flags: bit 0 = wait mode (= 0 なら fail mode)
         bit 1 = absolute timeout set
         bit 2 = idle timeout set
         bit 3 = process bound
```

#### `LockResponse` (0x31)

```
+------+----------------+--------------+
| u8   | u16 LE token_len| token bytes  |
| ok   |                 |              |
+------+----------------+--------------+
  ok: 0 = success, 1 = wait queue full, 2 = held by other, 3+ = error
```

#### `StatusResponse` (0x41) / `TailRequest` (0x50) / `WaitRequest` (0x53)

詳細は MVP 実装フェーズで詰める。本 DR では「枠」のみ確定、payload format は実装段階で最終確定。

#### `Error` (0x02)

```
+------------+-------------+--------------+
| u16 LE code| u16 LE msglen| msg UTF8     |
+------------+-------------+--------------+
  code:
    0x0001 = unknown message kind
    0x0002 = protocol version mismatch
    0x0003 = malformed payload
    0x0004 = lock denied
    0x0005 = not leader (= Resize sent by non-leader)
    0x0006 = name conflict
    0xffff = generic
```

### 5. Transport trait

```rust
pub trait Transport: Send {
    fn send_frame(&mut self, frame: &Frame) -> Result<()>;
    fn recv_frame(&mut self) -> Result<Frame>;
    fn close(self: Box<Self>);
}

pub struct Frame {
    pub kind: MessageKind,
    pub flags: u8,
    pub payload: Vec<u8>,
}
```

実装:
- **MVP**: `UnixStreamTransport` (= `UnixStream` on top of `UnixSock`)
- v0.2.0: `TcpStreamTransport` (= `TcpStream`、`hyoui serve` 内部)
- v0.2.0: `WebSocketTransport` (= `tokio-tungstenite` 等、xterm.js 経由)

各 Transport は length-prefixed 同じ wire format を流す、上層は kind/payload で抽象的に扱う。

### 6. Encode / Decode

MVP は手書き encode/decode (= 各 MessageKind ごとの serialize/deserialize 関数)。

```rust
impl Frame {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let total_size = 2 + self.payload.len();  // type + flags + payload
        out.extend_from_slice(&(total_size as u32).to_le_bytes());
        out.push(self.kind as u8);
        out.push(self.flags);
        out.extend_from_slice(&self.payload);
    }

    pub fn decode<R: Read>(r: &mut R) -> Result<Frame> {
        let mut size_buf = [0u8; 4];
        r.read_exact(&mut size_buf)?;
        let size = u32::from_le_bytes(size_buf) as usize;
        if size < 2 || size > MAX_FRAME_SIZE { return Err(...); }
        let mut frame_buf = vec![0u8; size];
        r.read_exact(&mut frame_buf)?;
        let kind = MessageKind::try_from(frame_buf[0])?;
        let flags = frame_buf[1];
        let payload = frame_buf[2..].to_vec();
        Ok(Frame { kind, flags, payload })
    }
}
```

各 payload は専用 struct (`HandshakeRequest`, `LockAcquire` etc.) で encode/decode 関数 を提供。

### 7. Authentication

- **Socket permission 0600 + parent dir 0700** で同 UID 限定 (= sys/socket.rs 既存実装)
- **Lock token** (HYOUI_LOCK_TOKEN env から client 自動取得) で daemon が照合
- 同 UID = 信頼領域、別 UID は socket permission で完全遮断
- 詳細は [[2026-05-26-lock-token-env]] の "Security 注意点"

### 8. Capability negotiation

`server_caps` / `client_caps` で feature flag を相互通知:

| bit | feature | MVP | v0.2.0 | v0.3.0 |
|---|---|---|---|---|
| 0 | basic data stream (handshake/data/resize/signal) | ⭕ | ⭕ | ⭕ |
| 1 | lock/tx | ⭕ | ⭕ | ⭕ |
| 2 | tail (--since/--follow) | ⭕ | ⭕ | ⭕ |
| 3 | wait L0 (--idle/--text/--pattern) | ⭕ | ⭕ | ⭕ |
| 4 | snapshot (画面 dump) | ❌ | ⭕ | ⭕ |
| 5 | wait L1 (--rect/--cursor) | ❌ | ⭕ | ⭕ |
| 6 | wait L2 (--area/--predicate) | ❌ | ❌ | ⭕ |
| 7 | leader CLI 操作 | ❌ | ❌ | ⭕ |
| 8 | sink (永続出力先) | ❌ | ❌ | ⭕ |
| 9 | record/play | ❌ | ❌ | ⭕ |

client は不足 capability を見て「未対応機能」とエラーを返せる。

### 9. Lifecycle / Error handling

#### Connection lifecycle

```
1. client が Unix socket connect (or TCP/WebSocket)
2. client が HandshakeRequest 送信
3. daemon が HandshakeResponse or Error 返信
4. 以降 Frame の交換 (双方向)
5. client が detach (Detach Frame) or 切断 (EOF / connection error)
6. daemon が cleanup (broadcast list から除外、leader cascade)
```

#### Error の流れ

- daemon が malformed frame を受信 → Error frame で返信 → 接続維持 (回復可能)
- protocol version mismatch → Error → 即 disconnect
- daemon の内部 error → Error frame で notify、状況により disconnect
- client 側で Error 受信 → 出力 + 適切な exit code

### 10. Concurrency / Ordering

- 1 client connection 内では **send/recv は順序保証** (= TCP/Unix socket の stream 性質)
- daemon が複数 client に broadcast する時 **client 間で順序保証なし** (= ある client が他より先に出力を受け取る可能性)
- DataFromChild の bytes は順序保証 (= 子の出力 bytes order を維持)
- DataFromClient の bytes は **client 間で interleave 可能** (= 多 client 同時入力時、bytes が混ざる)。lock/tx で防ぐのがユーザの責務

## Rejected alternatives

### bincode / msgpack を MVP から採用

- bincode は schema 安定性が version 依存 (= bincode 2.0 で breaking change あり)、wire format 互換性管理が複雑
- msgpack は cross-language 互換性高いが overhead あり (= タグ + length 各 field 毎)
- MVP では手書き、後で乗り換え可能 (= wire format = length-prefixed binary frame は変えない)

### JSON wire format

- text なので debug は容易
- size が binary より 2-3 倍、bytes 透過 (DataFromChild) で escape 必要
- 性能 (parse cost) も binary より遅い
- → 不採用

### gRPC / protobuf

- schema 定義の厳密性、cross-language、コードジェネレータ
- 重い (依存大、ビルド時間増、binary サイズ)
- hyoui の MVP には overkill
- 将来検討余地はあるが、今は不要

### 1 メッセージ = 1 socket message (= datagram 型)

- Unix datagram socket で frame 境界が自然
- ただし TCP / WebSocket では使えない (stream のみ)
- transport 統一のため stream + length-prefix を採用

## Consequences

### 実装への波及

- **新 module**: `crates/hyoui/src/protocol/` (mod.rs + message kind ごとの sub-module)
  - `mod.rs`: `Frame`, `MessageKind`, `Transport` trait
  - `messages/handshake.rs`, `messages/data.rs`, `messages/lock.rs`, `messages/tail.rs`, `messages/wait.rs`, etc.
  - `transports/unix.rs` (MVP)、将来 `transports/tcp.rs`, `transports/websocket.rs`
- 既存 `crates/hyoui/src/protocol.rs` (= 単純 length-prefixed) は本 DR の `Frame::decode` のベース、`messages/` で kind 毎の payload encode/decode を追加
- **新 module**: `crates/hyoui/src/daemon/` (socket bind + multi-attach + 永続 PTY 管理)
  - 既存 `crates/hyoui/src/agent.rs` (v0.0.0 の単純 PTY ラッパー、697 行) は意味的に別物 (daemon = socket bind + 永続、agent = 単発 fork + 中継) なので**廃止**
  - ただし参考実装として残す価値はあるので `crates/hyoui/examples/00-pty-wrapper.rs` に移植 (依存を解消した standalone 版に書き換え)。PoC 01-08 と並ぶ「PoC 00」扱い
  - `cli.rs::run` は新 daemon module を呼ぶ形に置換
- daemon の event loop は Frame 単位で dispatch (= 既存 PoC 02 の bytes 中継から拡張)

### Release タイミング (v0.0.x → v0.1.0)

- **中間 v0.0.x release は打たない**。protocol module だけ release しても client/daemon が無く動作しないため
- run / attach / detach / list / status / kill が全動作する状態で初めて **v0.1.0** を打つ (= DR-0007 の MVP scope と整合)
- それまでは main は green を保ちつつ progress、tag は v0.1.0 まで打たない
- release flow は kawaz/* 標準 (`pkf run bump-version --level=minor` → main push → CI が tag + GH Release 自動生成、`release-flow-awareness.md` 参照)

### テスト戦略

- 各 message kind ごとに encode/decode の round-trip test
- Frame::decode の malformed input に対する error handling test
- end-to-end test: client が HandshakeRequest 送る → daemon が HandshakeResponse 返す
- Transport trait の mock 実装で daemon ロジックを test (= 実 socket 不要)

### 未確定事項 (実装フェーズで詰める)

- `StatusResponse` の payload schema (= scrollback info, clients list, etc.)
- `TailRequest`/`TailData`/`TailEnd` の細部
- `WaitRequest`/`WaitResult` の rich result (regex captures, position, etc.)
- `LeaderRequest` の semantics (v0.3.0 で確定)
- backpressure / flow control (= daemon が遅い client にどう対処するか、初期は drop or buffer)

これらは MVP 実装の中で TaskCreate で個別に詰める。

## 関連

- [[DR-0005]] — 思想 (外側自動操作主軸)
- [[DR-0006]] — CLI ground rules (本 DR の API を呼び出す client 側)
- [[DR-0007]] — MVP scope (v0.0.1 で protocol を実装)
- [[2026-05-26-fd-passing-vs-stream]] — SCM_RIGHTS 不採用、stream 中継一本化の根拠
- [[2026-05-26-multi-attach]] — daemon の broadcast/multiplex の poll パターン
- [[2026-05-26-poc-summary]] — PoC 全体まとめ
