# DR-0022: `hyoui input` invocation 全体での auto-lock

- Status: Active
- Date: 2026-06-16
- Related: DR-0006 (CLI ground rules、§7 lock primitive / §8.5 spec prefix 内 lock 不採用方針の自然な続き), DR-0008 (protocol、本 DR は新 frame type 追加なし = 既存 `LockAcquire` / `LockRelease` の consumer), DR-0014 (透過原則 + 検証主義、本 DR の self-check), DR-0021 (PTY drain ack、本 DR が consume する completion 点)
- Origin: kawaz の design セッション (2026-06-16) — DR-0021 で「同一 client 内の bytes 順序」は保証したが、**他 client の割込み race** (= 2 つの `hyoui input` が同時実行されると bytes が混線する) は未解決として残っていた

## Context

`hyoui input <session> <specs...>` は **1 invocation = 1 ClientConnection** で daemon に
attach し、specs を順に dispatch する。DR-0021 で同一接続内の bytes は drain ack によって
順序保証されたが、**別 client が同時に input を打つと bytes が混線する**:

```
client A: input "text:hello\n"   (= bytes "hello\n")
client B: input "text:world\n"   (= bytes "world\n")
```

両方が同じ session に rw mode で attach すると、master fd への write 順序は daemon の
受信順 (= 非決定) で決まる。`hellworlold\n\n` のような混線が発生しうる。これは bytes
レベルの race であり DR-0021 の per-connection ack では救えない。

### 既存の lock primitive

DR-0006 §7 で確立された lock primitive (= `hyoui lock acquire` / `hyoui lock release`)
は session 全体への排他を提供しており、lock 中は daemon が **「holder client のみ
raw_data を受理」** で他 rw client の bytes を `client.lock-not-held` ack で reject する
(`crates/hyoui/src/daemon/control.rs:165-183`)。この primitive を `hyoui input` の
invocation 全体に自動適用すれば、上記 race を構造的に解消できる。

### 既存運用との関係

- 手動で `hyoui lock acquire` → `hyoui input --lock-token=<T>` → `hyoui lock release` の
  3 段運用は理論上可能だが、kawaz の dogfooding では誰も使っていない (= 煩雑、忘れる)
- DR-0006 §8.5 は spec の inline prefix としての lock を不採用とし、subcommand 境界に
  排他境界を揃える方針を確立したが、「subcommand invocation 単位での自動 lock」までは
  踏み込んでいなかった。本 DR がその空白を埋める

## Decision

### 1. `hyoui input` invocation 全体で 1 lock を auto-acquire / release

`hyoui input` 起動時に内部 lock を 1 本 auto-acquire し、process exit (success / error /
signal) で必ず release する (= 案 B「invocation 全体で 1 lock」)。spec 単位の short-lock
ではなく invocation 全体で 1 lock。理由:

- spec 単位 short-lock は wait 中の lock 解放が必要になり、wait 直後に他 client が
  入ると wait 後段の bytes が race する。invocation 全体で持つほうが意味論が単純で安全
- short-lock の取得/解放 overhead が spec 数に比例して増える

### 2. wait 中も lock を保持する

`wait:` / `wait-idle:` は子プロセスの画面描画を待つだけで、lock を開放しなくても
wait 条件は満たされる (= lock は **他 client の input 排他**、子の output には影響しない)。
wait 中も保持することで invocation 全体の意味論が「他 client から見て atomic な一連の
input」になる。

### 3. 外側 token 継承時は auto-acquire skip (= D 変形)

`--lock-token=<T>` flag か `HYOUI_LOCK_TOKEN` env がある場合、`hyoui input` は外側の
token を継承するだけで自分は acquire しない (= release もしない)。これは将来の
`hyoui lock tx` (DR-0006 §7、未実装) のような外側 wrapper との合成を予約する設計:

```
hyoui lock tx <session> -- hyoui input <session> "text:..." "wait:..."
                          (env: HYOUI_LOCK_TOKEN=<T> が継承)
```

外側で lock を取った接続が holder なので、inner input が再 acquire するのは
無意味 (= 同 token なら daemon は idempotent で Acquired を返すが、acquire/release を
ペアでやると外側の lock を release してしまう恐れがある)。skip が正解。

### 4. opt-out flag を作らない

`--no-lock` 等の opt-out flag は導入しない (= 常に auto-lock 有効)。理由:

- 透過原則を破る介入 (= client 間排他) ではあるが、これは子プロセスへの介入ではなく
  **client 間の race 防止**であり、子からは観測不能 (= 透過原則の対象外)
- opt-out を許すと「lock を取らない input」と「取る input」の挙動差を運用が記憶する
  必要があり、デフォルトの予測可能性を損なう
- どうしても lock 不要なら外側 token 継承経路 (= 何らかの形で `HYOUI_LOCK_TOKEN` を
  set してから input を呼ぶ) で skip させる

### 5. breaking change の扱い

v1.0 前なので protocol 拡張 / 既存挙動変更は許容 (= `feedback_v1_0_breaking_change_ok`)。
本 DR は新 protocol message 追加なし (= 案 X 採用、既存 `LockAcquire` / `LockRelease` を
client 内部で発行)。既存挙動の変更は:

- 並列 `hyoui input` が「混線する」→「先着優先で後着が待つ」に変わる
- 既存 `hyoui input` が他 client の lock を踏むと `LockAcquire` 段で Denied を待つ
  (= acquire timeout default 30s)

これらは既存挙動の改善であり、信頼できる sequencing が必要な test suite / 自動化が
むしろ恩恵を受ける。

### 6. acquire timeout の制御

`--auto-lock-timeout-acquire DUR` flag (default 30s) を `hyoui input` に追加。lock を
持っている他 client が長時間離さないとき、30s で acquire 失敗 → CLI exit 1。
default 30s の根拠: 短すぎると瞬間的な他 input と競合して flaky になる、長すぎると
hang したまま気付けない、の中庸点。

## Rejected alternatives

### 案 A: spec 単位の short-lock (= kawaz 初案)

各 spec を「acquire → send → release」で wrap する。

- ✗ wait 中の lock 解放が必要 (= wait 直後の race を救えない)
- ✗ spec 数に比例した acquire/release overhead
- ✗ invocation 全体としての atomic 性が失われる

### 案 C: 現状維持 (= 自動 lock しない)

- ✗ 並列 `hyoui input` の race が放置される (= dogfooding で実害確認済)
- ✗ kawaz が手動 lock を運用していない事実が「現状維持の不採用」を裏付け

### 案 D 変形: 外側 token を見たら inner auto-disable する代わりに inner で nested acquire

外側 token がある場合に「同 token で nested acquire」する案。

- ✗ daemon の idempotent 経路 (`state.lock_holder == Some(ch_id)` then return Acquired)
  に乗らない (= 別接続なので ch_id 違い、Denied で詰まる)
- ✗ 仮に通っても、release のタイミングが「最初の release で全体が消える」(= LIFO/
  refcount の管理が daemon に必要) になり protocol 拡張が必要
- ✗ 案採用版の「外側 token があれば skip」のほうが圧倒的に単純

### spec prefix 内 lock の復活 (DR-0006 §8.5 で却下済)

`hyoui input "lock:acquire" "text:..." "lock:release"` のような spec prefix 内で
lock を扱う案。

- ✗ DR-0006 §8.5 で「排他境界は subcommand 境界に揃える」方針として却下済、本 DR は
  その方針を覆さず、subcommand 境界を auto-lock の境界として活用する

### 案 Y: 新 `InputBeginTransaction` / `InputEndTransaction` message

daemon が input 専用の short-lock を内部管理する新 protocol message。

- ✗ 既存 lock primitive で十分 (= holder 判定 + raw_data reject が既に実装済)
- ✗ DR-0014 self-check の「最小介入」「kernel/PTY/shell 標準機能の再発明」「新 protocol
  message 必然性」を満たさない
- ✗ 案 X (= 既存 `LockAcquire` を client 内部で発行) なら protocol 追加 0、daemon 変更 0

## Consequences

### 実装影響

- **client 変更のみ**: `crates/hyoui-cli/src/main.rs::input_command` で
  `AutoLockGuard { conn_ref, token, owned, released }` 構造体を導入、`Drop::drop` で
  `owned && !released` なら release frame を best-effort 送信
- **daemon 変更 0**: 既存 `handle_lock_acquire` / `handle_lock_release` がそのまま機能。
  process-bound GC (= client disconnect 時の auto-release) も実装済
  (`crates/hyoui/src/daemon/session.rs:1639-1681`)
- **CLI struct 追加 1**: `InputCommand::auto_lock_timeout_acquire: Duration` (default 30s)

### Forward-compat

- 将来 `hyoui lock tx` を実装したとき、本 DR の外側 token 継承経路が自然に効く
- 将来 daemon に lock wait queue を実装したとき、本 DR の acquire は `wait: true` を
  送るので queue に入る経路に乗る (= 現状は MVP daemon が Denied を返すので CLI 側
  polling で擬似 wait)

### 既存挙動 breaking

- 並列 `hyoui input` の race 消失 → 既存 test で「同時実行で適当な順序を期待」していた
  ものは「直列化」に変わる。e2e test 修正が必要なら本 DR 実装と同 PR で
- daemon に `expected_token` が設定されている環境では既存挙動と同じ (= flag/env 経由で
  token 提示が必要、auto-acquire 段では 1 個目 LockAcquire は通る)

## self-check (DR-0014 §self-check)

- [x] **既存 DR で justify されているか** — DR-0006 §7 で確立された lock primitive を
      新規 API なしで consume する。DR-0006 §8.5 の subcommand 境界方針の自然な延長
- [x] **透過原則を破るが、その理由は「必然」か** — 子プロセスへの介入はゼロ
      (= lock は client 間排他、子からは観測不能)、透過原則の対象外
- [x] **最小介入か** — 既存 lock primitive 流用、新 protocol 0 個、daemon 変更 0
- [x] **kernel / PTY / shell 標準機能を再発明していないか** — lock 自体は既存 primitive、
      Drop guard も Rust RAII 標準パターン
- [x] **新 protocol message / cap flag 追加なら、必然性を DR に書けるか** — 追加なし
- [x] **既存 DR で justify された機能のうち未実装はないか確認** — DR-0006 §7 の
      `process-bound GC` は既に実装済 (`session.rs:1639-1681`)、本 DR が新規に必要に
      するものはない

## 実装方針 (案 X 採用)

`hyoui input` の `ClientConnection` 内部で既存 `LockAcquire` / `LockRelease` を発行:

```rust
struct AutoLockGuard<'a> {
    conn: &'a mut ClientConnection,
    token: String,
    released: bool,
}

impl Drop for AutoLockGuard<'_> {
    fn drop(&mut self) {
        if !self.released {
            // best-effort: send LockRelease, ignore errors
            let _ = self.conn.send_control(&ControlMessage::LockRelease(
                LockRelease { token: self.token.clone() }
            ));
        }
        // daemon の process-bound GC が 2 重保険 (= 接続切断時に auto-release)
    }
}
```

dispatch loop の前で auto-acquire、最後に明示 `release()` を呼ぶ。途中で error / panic /
signal が来ても Drop で release が走り、それも失敗しても daemon の process-bound GC で
回収される (= 2 重保険)。

### ack 経路との順序整合 (DR-0021 §pending_frames との関係)

`LockRelease` は control message (= ack 不要)。DR-0021 の `recv_raw_ack_inner` 経路 (=
raw_data の ack を同期待ち) と独立に send できる。`send_control` は ack を待たず即 return
するので、`pending_frames` buffer の順序を壊さない (= ack 待ち中の pending frames は
当該 ack 完了後に処理される)。

### lock reject ack の扱い (DR-0021 §4 との相互作用)

万一 auto-acquire 後に何らかの理由で `holder != ch_id` 状態に陥った場合 (= 通常は起き
ない、daemon バグ等)、raw_data は `client.lock-not-held` で ack:Error が返り、CLI は
exit 1 する (DR-0021 §4 の挙動)。これは正しい防御。

## 関連

- DR-0006 §7 (lock primitive、本 DR の基盤)
- DR-0006 §8.5 (subcommand 境界の排他、本 DR が活用)
- DR-0021 §4 (raw_data ack の意味論、lock reject ack との整合)
- DR-0014 §self-check (本 DR で 6 項目クリア確認済)
