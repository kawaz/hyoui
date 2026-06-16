# DR-0021: bytes 系 input spec の完了点を「PTY drain ack」に強化する

- Status: Active
- Date: 2026-06-16
- Related: DR-0006 (CLI ground rules、§8 input family / §8.6 sequencing 完了点を本 DR で明文化), DR-0008 (protocol、新 frame type `TYPE_RAW_ACK = 0x02` を追加 / wire 外枠は不変), DR-0014 (透過原則 + 検証主義、本 DR の self-check), DR-0016 (record / `MASTER_WRITE_IDLE_TIMEOUT_MS` の per-chunk timeout の周辺仕様)
- Origin: docs/issue/2026-06-16-bug-input-text-key-enter-not-sent.md (= `hyoui input "text:<長文>" "key:Enter"` で Enter が落ちる race)

## Context

`hyoui input` の bytes 系 spec (= `text:` / `paste:` / `hex:` / `file:` / `key:`) を 1 invocation で連続指定したとき、後段の spec (典型: `key:Enter`) が「効かない」と観測される事案があった (issue
2026-06-16-bug-input-text-key-enter-not-sent.md)。

### root cause (= 確定)

- client (`ClientConnection::send_raw_bytes` 旧実装) は `Frame::raw_data(bytes)` を socket に write_all + flush した瞬間に `Ok(())` を返し、CLI の `dispatch_spec` ループは即座に **次** の spec の bytes 化 → send_raw_bytes に進む。
- daemon (`daemon/control.rs::handle_client_frame` の `TYPE_RAW_DATA` 分岐) は受け取った body を `master_fd().write_all_with_idle_timeout` で master PTY fd に書き込むが、N_TTY line discipline の input buffer (典型 4–8 KiB) が満杯になると `EAGAIN` → `POLLOUT` 待ちで chunked 進行する。
- 結果、**長文 text の master fd への書き込みが完了する前に、後続の `\r` を含む frame が同じ master fd に書き込まれる** ケースが起こる。Unix socket は順序保証だが、line discipline は per-byte の状態機械であり、`text` の最終 byte と `\r` の到達順が逆転すると Enter が text の途中バイトとして解釈される (= キー bind が外れる) / line discipline buffer overflow で drop されることがある。

加えて、TUI (= claude / vim) は起動直後に cap negotiation シーケンスを stdout に流すため、`\r` がそのレスポンス受信タイミングと race して読み捨てられる現象も観測されていた (issue 仮説 B)。

### bug の本質

bytes 系 spec の「完了点」が **socket flush** にあり、**PTY drain (= line discipline への書き込み完了)** が保証されていない。spec sequencing の意味論として、後段 spec は前段 spec の bytes が「子の入力ストリーム末尾に確実に届いた」状態を前提にしないと安全に書けない。これは DR-0006 §8.6 で sequencing を語ったときに完了点を明文化していなかった欠陥。

## Decision

### 1. 完了点を「PTY drain ack」に変更

daemon は `TYPE_RAW_DATA` frame の master fd write が return した時点で、当該 client に **`TYPE_RAW_ACK` frame** を返す。client (`send_raw_bytes`) は ack を **同期で待ってから** 次の bytes を送る。これにより:

- 後段 spec の bytes は前段の master fd write 完了後に line discipline に届く (= 順序保証)
- daemon 側で `EAGAIN` chunked 進行 / `MASTER_WRITE_IDLE_TIMEOUT_MS` で部分書き込みになっても、client は明示エラー (= ack の `result: Error`) で受け取れる
- spec の意味論として「ack 受信 = 子の入力 stream にこの spec の bytes 全部が到達した」が成立

### 2. 新 frame type `TYPE_RAW_ACK = 0x02`

DR-0008 §1.2 で `0x02..0xff` は予約とされていた。新 type tag 追加は §3.3 forward-compatible (= 別 socket fork 不要)。本 DR で `0x02` を `TYPE_RAW_ACK` として確保する。

frame layout は変えない (= `[u32 LE size][u8 type][body]`、wire 外枠は永久固定の §1.1 と整合)。body は CBOR encoded `RawAck` struct:

```cbor
{
  "result": "ok" | "error",
  "code": <text> | omitted,      ; error 時のみ、機械可読 code (例: master.write-timeout)
  "message": <text> | omitted    ; error 時のみ、人間可読 description
}
```

- forward-compatible: 未知 field は ignore (= DR-0008 §3.2 と同じ)、新 field は後付け追加可能
- 明示 seq id は **持たない** (= connection-level の同期、`client → daemon` の raw_data 1 個に対し `daemon → client` の ack 1 個が 1:1 で対応)

### 3. cap flag を導入しない (= breaking、v1.0 前 OK)

v1.0 前なので protocol breaking 変更を許容 (= `feedback_v1_0_breaking_change_ok` 方針)。本 DR は cap negotiation を経由しない wire 強化として実装する:

- **新 daemon + 新 client**: 正常動作 (= ack で同期)
- **新 daemon + 旧 client**: daemon は ack を送るが旧 client は `TYPE_RAW_ACK` を「未知 type」として `decode_from` で reject する。実用上、旧 client は connect すらできないと考えてよい (= hyoui は単一バイナリ配布で daemon と client の version skew は基本起きない)
- **旧 daemon + 新 client**: 新 client は ack を待つが旧 daemon は送らない → `RAW_ACK_TIMEOUT` (= 5 秒) で `Error::Invalid("raw_ack timeout")` を返して CLI exit 1。この場合 daemon が古いことを示すエラーメッセージで誘導する余地は残るが、MVP では timeout エラーで明示失敗させる

### 4. ack 値の意味論 (改訂 2026-06-16)

**改訂理由**: 旧表は Ro / lock 不一致を `Ok` ack に倒していたが、ack:Ok が
「子の input stream に到達した」と「daemon が受理だけした (write は行わなかった)」
の 2 意味を持つ嘘応答になっていた。ack の意味論を「**bytes が子の input stream に
確実に到達した**」に統一する。

| `RawAck.result` | 送る条件 | `code` |
|---|---|---|
| `Ok` | `write_all_with_idle_timeout` が `is_complete() == true` で return (= 全 byte が master fd に到達) | — |
| `Error` | master PTY write が `IdleTimeout` | `master.write-timeout` |
| `Error` | master PTY write が I/O error | `master.write-error` |
| `Error` | master PTY write が partial で error なし (defense-in-depth) | `master.write-partial` |
| `Error` | client が Ro mode のため reject (master fd に未書込) | `client.ro-rejected` |
| `Error` | lock holder と異なる client のため reject (master fd に未書込) | `client.lock-not-held` |

`master.*` 系 Error は ack 送信後に当該 client を disconnect (= 旧仕様維持)。
`client.*` 系 Error (= Ro / lock reject) は接続を切らない (= client は次の操作
へ進める)。

client 側は `code` を見て semantic 判断する: `master.*` は daemon disconnect も
伴うため abort、`client.*` は権限不足通知として CLI exit 1 (= 既存の `Err(Error::Remote)`
経路で wrap される)。

**[[DR-0022]] (= `hyoui input` invocation auto-lock) との相互作用**: DR-0022 で
`hyoui input` は自接続で auto-acquire するため、通常運用では `client.lock-not-held` ack
は発生しない (= 自分が holder)。発生し得るのは (a) DR-0022 の auto-acquire が timeout
で失敗した直後 (= acquire 不成立で input を進めない経路に乗るので raw_data 自体送らない)、
または (b) 万一 daemon 側の `lock_holder` 状態が想定外に変化 (= バグ等) した場合のみ。
auto-lock 経路は `LockAcquire` / `LockRelease` (control message、ack 不要) を使うため、
本 §4 の raw_data ack 経路と独立して send/recv され、`pending_frames` の FIFO 順序を
壊さない。

### 5. client 側の挙動 (改訂 2026-06-16)

`send_raw_bytes(bytes)`:

1. **poison check**: 過去に ack 失敗で poison 済なら即 `Error::Invalid("connection poisoned...")` (= M2)
2. `Frame::raw_data(bytes)` を socket に write_all + flush
3. ack 待ち loop (`recv_raw_ack_inner`):
   - **`poll(reader_fd, POLLIN, remaining_deadline)`** で次 frame の readiness を待つ
     (= socket `read_timeout` は変更せず、frame body の読み出しは blocking で完走させる、後述「改訂理由」)
   - ready なら `Frame::decode_from` を blocking で 1 frame 完走読了
   - `TYPE_RAW_ACK` → `RawAck` decode して `Ok` / `Err(Error::Remote)` を返す
   - 他 frame (`TYPE_RAW_DATA` / `TYPE_CBOR_CONTROL`) → `pending_frames: VecDeque<Frame>` に push して loop 継続
   - `TYPE_RAW_ACK` 以外で deadline 超過 → `Error::Invalid("raw_ack timeout")`
4. ack 待ちが `Err(Error::Remote(_))` 以外で失敗した場合 (= timeout / I/O error / protocol error) は
   `poison()` を呼び reader/writer socket を `shutdown(Both)` する (= M2 stale ack 防止)

`pending_frames` は FIFO buffer。後続の `recv_frame` / `recv_control` がここから先に取り出す
(= broadcast 経由の raw_data や ModeChange/LeaderNotify を取りこぼさない)。input 1-shot
接続では使われないが、attach 経由の `send_raw_bytes` 呼び出しでも安全に動く設計。

#### 改訂理由 (poll-based readiness + blocking decode、2026-06-16)

旧実装は `set_read_timeout(Some(RAW_ACK_TIMEOUT))` で socket の `read(2)` 自体に
timeout を仕掛けていたが、`Frame::decode_from` は内部で `read_exact_eof` を 3 連続
(size 4B → type 1B → body N B) で呼ぶ。`read_exact` は途中で `TimedOut` を踏むと
**既に読まれた partial bytes を破棄して即 return** する仕様 (Rust 標準)。これにより
body 読み出し途中で deadline が短縮された場合に socket に body 残骸が居残り、
次 iteration で「残骸の先頭 4B を size として誤解読」する **partial-byte race** が
成立した。実機で `python -i` に 1038 B 以上の text + Enter を 1 invocation で送ると
`frame decode failed while waiting raw_ack` (= `Error::Invalid`) で exit 1 となる事故が
発生し (= 行 buffer ≤ 1024 の echo タイミングと一致)、vim alt screen では echo パターンが
異なるため 2000 B でも踏まない、という挙動分散が観測されていた。

修正後は **frame 境界でのみ deadline 判定が発火**する不変条件を満たす:
- `poll(2)` で次 frame の readiness を待つ (= timeout はここでだけ発火)
- ready 後は blocking `decode_from` が 1 frame を完走読了 (= partial-byte 残骸を作らない)

#### `recv_control` の unsolicited RAW_ACK 受信 (m1)

`recv_control` / `recv_frame` で **ack 待ちでない時に** `TYPE_RAW_ACK` を受信した
場合は silent skip する (= broadcast 由来の RAW_DATA を skip するのと同じ扱い)。
旧実装は `unexpected frame type` で hard error にしていたが、ack の所有者は
`send_raw_bytes` のみなので、他経路では ignore するのが防御的に安全
(= M2 poison が塞ぐ stale ack に加え、防御深層化)。

#### connection poisoning (M2)

ack 待ちが timeout / I/O error / protocol error で失敗した場合、同 connection への
次の `send_raw_bytes` は遅れて届いた前回 ack を次回 ack として誤受理する race が
理論上残る (= seq id を持たない 1:1 semantics の代償)。これを物理的に塞ぐため、
失敗時に socket を `shutdown(Both)` し `poisoned` フラグを立てる。以降の
`send_raw_bytes` は `Error::Invalid("connection poisoned after raw_ack failure")` を
即返す。`Err(Error::Remote(_))` (= daemon が ack:Error を返した、protocol 上の正常受信)
は poison しない (= semantic レベルの失敗なので caller が継続判断する)。

CLI 一発呼びでは ack 失敗時に exit するため影響なし。library で attach 経路から
send_raw_bytes を使う場合にこの保護が効く。

### 6. `RAW_ACK_TIMEOUT` = 5 秒

根拠: daemon の `MASTER_WRITE_IDLE_TIMEOUT_MS` は per-chunk **500 ms** (= R5-C3、子が完全に読まなくなったことを検出する閾値)。`write_all_with_idle_timeout` 全体は chunk × N 経過するので、複数 chunk が必要な大きな bytes 列でも実用上 1 秒以内に ack が返るのが想定。5 秒は十分余裕。

これを超えて ack が来ない場合 (= daemon dead-lock / 旧 daemon / 通信障害) は永遠に hang するより明示エラーで上に伝える。

### 7. `ClientHandle::Drop` の queue flush 保証 (M1)

daemon は失敗 ack (= `master.*` 系 Error) を `send_raw_ack` で enqueue した直後に
当該 client を `DropClient` で disconnect する。旧 `ClientHandle::Drop` は最初に
`reader.shutdown(Both)` を呼んで socket を即 close していたため、writer_pump が
未送信の ack frame を flush する前に socket が閉じ、client は理由 unknown な EOF
(= decode error) を観測していた。

修正後の Drop 順序:

1. `writer_tx` を closed dummy へ `mem::replace` (= channel close 状態)。
   `std::sync::mpsc::Receiver::recv()` は queue に積まれている frame を順次 Ok で返し、
   queue が空になった時点で初めて Err を返すので、writer_pump は **pending frame を
   flush し切ってから** loop を抜ける
2. `reader.set_write_timeout(Some(DROP_FLUSH_TIMEOUT))` で flush を bound
   (= client が socket を読まない / 死んでいる場合に write_all が無限 block しないよう、
   500 ms の upper bound。同 socket fd の duplicate なので writer 側にも timeout 適用)
3. `writer_thread.join()` で writer_pump 終了を reap
4. 最終 `reader.shutdown(Both)` で念押し close (= flush 後なので失う frame なし)

これにより `master.*` 系失敗 ack が client に到達することを保証する。

## Rejected alternatives

### 暗黙の `wait-idle` を自動挿入

「`text:` の後に `wait-idle:200ms` を自動挿入する」案。bytes 系 spec の意味論を歪める (= 透過原則違反、DR-0014)。「子が一定時間出力しなかった」は drain 完了の十分条件ではない (= cap negotiation 中の echo 待ち等で false negative)。

### socket flush で完了扱い (= 現状の旧実装)

race の root cause そのもの。不採用。

### cap flag 経由で gating (= `raw-ack-v1` cap)

v1.0 前で breaking OK のため cap を導入せず固定で振る舞いを変える。cap を入れると「ack を送らない接続」を許容するロジックを daemon と client 両方に維持する必要があり、複雑化に対する利得が薄い (= 新 daemon と新 client は同じバイナリから出るのが通常)。

### 明示 seq id を ack body に持つ

複数 raw_data を pipeline で送って ack を並行で返す semantics (= 高 throughput) を将来検討するなら必要だが、本 DR では「1 invocation の input spec を順次送る」semantics で十分。pipeline は forward-compat に「`raw-pipeline-v1` cap + seq id」として後で導入可能 (= `pending_frames` buffer の延長で実装可)。

### 新 daemon が旧 client (= `TYPE_RAW_ACK` を不明 type として disconnect する) を検出して ack 送信を抑止

handshake で client version を probe する余裕がない (= cap negotiation 後の type tag は事前 negotiation 不能の領域)。daemon は常に ack を送る、旧 client が困るのは「version skew が起きた場合のみ」で稀。

### `set_read_timeout(RAW_ACK_TIMEOUT)` で deadline 管理 (= 当初の DR-0021 採用、改訂で撤回)

ack 待ちを socket の `read_timeout` で時間制限する案。当初の DR-0021 で採用していたが、
`Frame::decode_from` の 3 連続 `read_exact` が partial-byte discard で frame 整合性を
壊す事故 (= python -i 1038 B 境界で再現) が判明し撤回。代替として poll-based readiness +
blocking decode で frame 境界での deadline 判定に切り替え (= §5 「改訂理由」)。

### M2 stale ack を seq id で識別

ack body に明示 seq id を持たせ、stale ack (= ack.seq != current_seq) を skip する案。
将来 pipeline (= 高 throughput) を導入する際は seq id が必須になるので、その時点で
forward-compatible に「`raw-pipeline-v1` cap + seq id」として追加する余地はある。
本 DR の範囲では「1 invocation で input spec を順次送る」semantics に限るため、
poison + 1:1 ack で十分。implementation cost (= 構造体 1 field 追加 + 全 ack site の seq
管理) より poison + shutdown のシンプルさが上回ると判断 (= 採用「案 a」)。

### Drop で writer_tx を後回しにし shutdown 先行 (旧 DR-0021 実装、改訂で撤回)

旧実装は writer_pump が write_all で block 中の時に shutdown(Both) で即 unblock
できるメリットがあったが、queue に積まれた失敗 ack frame まで巻き込んで捨てて
しまう副作用が判明し、§7 の順序に変更。bound 越えで block する稀ケースは
`DROP_FLUSH_TIMEOUT` で打ち切る。

## Consequences

### 実装への波及

- **protocol**:
  - `crates/hyoui/src/protocol/frame.rs`:
    - `TYPE_RAW_ACK = 0x02` constant 追加
    - `Frame::raw_ack(body: Vec<u8>) -> Frame` constructor 追加
    - `Frame::decode_from` の type 検証を 3 種 (data/control/ack) に拡張
    - 既存 unit test `decode_rejects_unknown_type` は予約 type を `0x03` に変更
  - `crates/hyoui/src/protocol/messages/raw_ack.rs` (新規):
    - `RawAck { result, code, message }` struct + serde derive
    - `RawAckResult { Ok, Error }` enum
    - `CODE_MASTER_WRITE_TIMEOUT` / `CODE_MASTER_WRITE_ERROR` / `CODE_MASTER_WRITE_PARTIAL` constants
    - **改訂追加**: `CODE_CLIENT_RO_REJECTED` (= `client.ro-rejected`) / `CODE_CLIENT_LOCK_NOT_HELD` (= `client.lock-not-held`) constants
    - `encode_to_vec` / `decode_from` + round-trip test
  - `crates/hyoui/src/protocol/messages/mod.rs`: re-export 追加
  - `crates/hyoui/src/protocol/mod.rs`: re-export 追加
- **daemon**:
  - `crates/hyoui/src/daemon/broadcast.rs`:
    - `send_raw_ack(ch, ack) -> bool` helper 追加 (= 既存 `send_control` の `TYPE_RAW_ACK` 版)
    - **改訂追加**: `ClientHandle::Drop` の順序を変更 (= writer_tx 先 drop → set_write_timeout(`DROP_FLUSH_TIMEOUT` = 500 ms) → join → 最終 shutdown)。`DROP_FLUSH_TIMEOUT` const 追加。失敗 ack の queue flush 保証 (= §7)
  - `crates/hyoui/src/daemon/control.rs`:
    - **改訂**: `TYPE_RAW_DATA` handler の Ro reject 経路で `RawAck::err(CODE_CLIENT_RO_REJECTED, ...)` を送る (= 旧 `RawAck::ok()` から変更)
    - **改訂**: lock 不一致 reject 経路で `RawAck::err(CODE_CLIENT_LOCK_NOT_HELD, ...)` を送る
    - 成功経路 (`outcome.is_complete()`) で `RawAck::ok()` を送る
    - 失敗経路 (`IdleTimeout` / `Io` / partial) で `RawAck::err(code, message)` を送ってから drop
- **client**:
  - `crates/hyoui/src/client/attach.rs`:
    - `RAW_ACK_TIMEOUT: Duration = 5 sec` constant 追加
    - `ClientConnection.pending_frames: VecDeque<Frame>` field 追加
    - `send_raw_bytes` を「raw_data 送信 → ack 待ち」semantics に変更 (= `recv_raw_ack_inner` helper 追加)
    - **改訂**: `recv_raw_ack_inner` を **poll-based readiness + blocking decode** に書き換え (= partial-byte race 撲滅、§5「改訂理由」)
    - **改訂追加**: `ClientConnection.poisoned: bool` field + `poison()` helper 追加 (= M2 stale ack 防止、§5「connection poisoning」)
    - **改訂追加**: `recv_control` で `TYPE_RAW_ACK` を silent skip (= m1 防御深層化)
    - `recv_frame` は `pending_frames` を先に消費するよう変更
- **CLI**:
  - `crates/hyoui-cli/src/main.rs::dispatch_spec` は **変更不要** (= 既存呼び出し `conn.send_raw_bytes(&bytes)` が同期 ack 待ちに変わる)
  - 新 e2e test `send_raw_bytes_text_then_enter_arrives_in_order` を追加 (= `/bin/cat` を子に MARK<i>\r を 30 回連続送信、各反復で `screen.dump` の visible bytes に label が現れることを確認)

### テスト戦略

- protocol frame layer: `raw_ack_empty_roundtrip` / `raw_ack_with_cbor_body_roundtrip` を追加
- `RawAck` body schema: `ok_roundtrip` / `err_timeout_roundtrip` / `err_io_roundtrip` / `ok_wire_omits_optional_fields` / `err_wire_includes_code_and_message`
- **改訂追加** `RawAck` 新 code: `err_ro_rejected_roundtrip` / `err_lock_not_held_roundtrip`
- e2e: `send_raw_bytes_text_then_enter_arrives_in_order` (= 30 回連続 text + Enter で順序保証)
- 既存 e2e: `send_raw_bytes_does_not_disconnect_daemon` (= ack 機構を追加しても regression なし)
- **改訂追加 e2e**:
  - `send_raw_bytes_partial_byte_race_regression` (= 2000 B × 20 反復で partial-byte race の不在を確認)
  - `send_raw_bytes_ro_client_receives_error_ack` (= Ro client が `Err(Error::Remote)` を受け取る)
  - `send_raw_bytes_lock_not_held_receives_error_ack` (= 非 lock holder が `Err(Error::Remote)` を受け取る)
- **改訂追加 unit (mock daemon, socketpair 直結)**:
  - `send_raw_bytes_handles_large_non_ack_frame_then_ack` (= 4 KiB non-ack frame → ack の順を pending_frames に積みつつ Ok を返す)
  - `send_raw_bytes_after_timeout_is_poisoned_and_rejects_stale_ack` (= 失敗後の poison + 2 回目の即時 fail)
  - `send_raw_bytes_remote_error_does_not_poison` (= ack:Error は poison しない)
  - `recv_control_silently_skips_unsolicited_raw_ack` (= m1 防御深層化)

### 旧 daemon との互換性

新 client が旧 daemon (= v0.6.5 以前) に attach した場合、`send_raw_bytes` は ack を 5 秒待って timeout し `Error::Invalid` を返す。CLI は exit 1 で abort する。これは v1.0 前の意図的な breaking change (= `feedback_v1_0_breaking_change_ok`)。

将来 cap negotiation で「`raw-ack-v1` を持たない peer は旧 protocol semantics で動く」モードを追加することは可能 (= `pending_frames` buffer の延長で実装容易)。MVP では不要。

### self-check (DR-0014)

- [x] **既存 DR で justify されているか?** — DR-0006 §8.6 (spec sequencing) の完了点を明文化、DR-0008 §3.3 (新 type tag 追加は forward-compatible) に整合
- [x] **透過原則を破るが、その理由は「必然」か?** — bytes 系 spec sequencing の意味論 (= 「ack 受信 = 子 input stream に到達」) は spec semantic 完成の必然
- [x] **最小介入か?** — 新 type tag 1 個 + 新 helper 1 個 + client 側 buffer 1 個。protocol cap や handshake 拡張は導入しない
- [x] **kernel / PTY / shell の標準機能を再発明していないか?** — `write_all_with_idle_timeout` の return 値を ack 点にするだけ (= 既存の kernel `write(2)` return を素直に使う)
- [x] **新 protocol message / cap flag 追加なら、必然性を DR に書けるか?** — 本 DR §1-§6 で必然性を記述
- [x] **既存 DR で justify された機能のうち、未実装のものはないか?** — DR-0006 §8.6 spec sequencing 完了点が未明文化だった (= 本 DR で完成)

### 検証マトリクス (TODO、kawaz 主導の実機検証で完成させる)

DR-0014 §検証主義に従い、3 category の app で順序保証を確認:

| app category | 期待 | 実機検証 |
|---|---|---|
| TUI alt screen (claude / vim) | text 末尾の Enter が確実に line discipline に届き、prompt 送信される | TODO |
| line-oriented (cat / less) | text + Enter で 1 行ずつ echo / pager 操作 | ✅ ack-race test で 30 回連続成功 (`/bin/cat`) |
| interactive REPL (python / bash) | text + Enter で式評価 / 行実行が確実に発火 | TODO |

unit test レベルでは 30 回連続成功で ack 機構の安定性は確認済。実機マトリクス検証は本 DR 完了後、別タスクで実施する。

## 関連

- DR-0006 §8.6 — spec sequencing 完了点 (本 DR で「PTY drain ack 受信」を完了点として明文化)
- DR-0008 §1.2 / §3.3 — 新 type tag 追加の forward-compat 経路
- DR-0014 — 透過原則 + 検証主義 (本 DR の self-check + マトリクス検証 TODO)
- DR-0016 — `MASTER_WRITE_IDLE_TIMEOUT_MS` (500 ms per-chunk) の周辺仕様
- docs/issue/2026-06-16-bug-input-text-key-enter-not-sent.md — root cause 調査と修正方針合意の経緯
