# Feature: tx / lock / unlock CLI subcommand 実装 (DR-0006 §7)

- Status: Open (= 実装待ち、本 nonstop session の task #20 から切り出し)
- Date: 2026-05-27
- Priority: Mid (= MVP の自動操作排他は env fallback で機能しているが、外側 wrapper の標準入口が無いと UX が固い)
- 関連 DR: [[DR-0006]] §7 (Lock + tx 仕様正本)、[[DR-0006]] §8.5 (input family との関係)
- 関連 task: #19 (= `--lock-token` flag + env fallback 配線済) の後続

## 背景

DR-0006 §7 で「自動操作排他」の CLI として 3 subcommand が確定している:

```bash
hyoui tx <name> [--timeout-* ...] -- cmd args...
  # 起動時 lock 取得 → 子 env に HYOUI_LOCK_TOKEN 注入 → 子 exit で自動 unlock

hyoui lock <name> [--timeout-* ...] [--mode wait|fail]
hyoui unlock <name> [--token T | --force]
```

現状 (= task #19 完了時点) で実装されているのは下記まで:

- **Protocol 層**: `LockAcquire` / `LockResponse` / `LockRelease` message と daemon
  handler は完備 (`crates/hyoui/src/protocol/messages/lock.rs`、
  `crates/hyoui/src/daemon/control.rs` の `handle_lock_acquire` / `handle_lock_release`)。
  cap `"lock"` も MVP_CAPS に含まれている (`protocol/caps.rs`)
- **`ErrorCode::LockDenied` / `LockNotHeld`**: protocol 上の error 表現も既にある
  (`protocol/messages/error.rs`)
- **`--lock-token` flag + `HYOUI_LOCK_TOKEN` env fallback**: `attach` / `tail` /
  `input` / `kill` などの subcommand が handshake.token に流す配線済 (= task #19)

未実装 (= 本 issue で扱う):

- `Command` enum に `Tx` / `Lock` / `Unlock` variant が **無い**
- `parse_args` の match arm に `"tx"` / `"lock"` / `"unlock"` が **無い**。
  `"send"` / `"detach"` は "reserved but not yet implemented" として明示されているが、
  tx/lock/unlock は予約すらされていない
- `hyoui-cli/src/main.rs` 側 dispatch も対応する handler 関数なし

つまり「子側で `--lock-token`/env を使えば lock 配下で動ける」ところまでは出来ているが、
**外側で lock を取って子を起こす wrapper 入口** がまだ無い。MVP では tx の代わりに

```bash
HYOUI_LOCK_TOKEN=$(uuidgen) hyoui input <session> "text:..." "key:Enter" ...
```

のような env 手動セットで凌げるが、それは DR-0006 §7 の自動操作排他としては不完全 (= 取得・
解放を CLI が透過的に保証しない、refcount や cascade policy が効かない)。

## 求められる仕様 (= DR-0006 §7 から再掲)

### `hyoui tx <name> -- cmd args...`

子 process 起動時に lock 取得 → 子の env に `HYOUI_LOCK_TOKEN` 注入 → 子 exit で
自動 unlock。default timeout:

| flag | default |
|---|---|
| `--timeout-absolute` | 5min (safety net) |
| `--timeout-idle` | 30s |
| `--process-bound` | ⭕ (子プロセス bound、tx 固有) |

### `hyoui lock <name>` (低レベル)

token を生成して取得、stdout に token を出して exit。後続コマンドに env / flag で
渡す前提。default timeout:

| flag | default |
|---|---|
| `--timeout-absolute` | 5min |
| `--timeout-idle` | 30s |
| `--process-bound` | ❌ |

`--mode wait|fail` (= default wait?) で「他 owner が居る時に block するか即 fail するか」。

### `hyoui unlock <name>`

`--token T` で自分の取得した lock を解放、または `--force` で他 owner の lock も
剥がす (= 救済用、stderr に warn 出す)。

### 共通 semantics

- lock owner ⇒ leader 強制昇格 (winsize 主体)、他 rw は ro 一時降格
- 終了で leader cascade policy 発動 (元 leader 残存ならそこに戻す)
- 全 send/keys/paste/wait は `--token T` 受け、未指定なら env `HYOUI_LOCK_TOKEN` (= 配線済)
- nested lock: 同 token なら no-op 成功 (refcount)、別 owner は wait/fail
- 他 client は **強制 ro** (= バッファ・ブロックは将来 opt-in)

## 実装の見積もり (= 別 task 切り出し時の目安)

優先順:

1. **`hyoui lock <name>` 単体** (= 最小): protocol `LockAcquire` を送って `LockResponse(Granted)`
   を待ち、token を stdout に書く。client が socket を保持しなくても lock は daemon 側で
   token + timeout 管理される設計なら、CLI 自身は短命で良い。`--timeout-idle` 反映には
   daemon 側 timer 管理の追加が要るかもしれない (= 要確認)
2. **`hyoui unlock <name>`** (= 解放): `LockRelease` を送るだけ。`--force` は daemon 側で
   別 path が必要か要確認
3. **`hyoui tx <name> -- cmd...`** (= 1+2 の上に乗る wrapper): token 生成 → lock 取得
   (= 1) → `Command::spawn` で子起動 + env 注入 + waitpid → 子 exit 後 unlock (= 2)。
   `--process-bound` は子 PID を lock に紐付ける daemon side の機能が必要

daemon 側の timeout / refcount / process-bound 管理が現状どこまで実装されているか別途
調査が要る (= `daemon/control.rs` の `handle_lock_acquire` 周辺と LockState 構造を読む)。

## 別 task として切り出す判断

- 本 nonstop session の task #20 「tx CLI 実装」では、protocol も daemon-side timer も
  含む大規模な実装が必要なため、本 session 内には収めない判断 (= task #19 の判断と同じ)
- 次セッションで本 issue を起点に再開する

## 確認すべき open question

- daemon 側 `LockState` で `--timeout-absolute` / `--timeout-idle` / refcount /
  `--process-bound` は既に管理されているか? (= `control.rs` の handler は granted/denied を
  返すだけに見える、timer 動作は別 thread が要るかも)
- `hyoui lock <name>` を「短命 client」として実装する場合、socket disconnect で lock が
  即解放されないことを担保しているか? (= 普通の attach client が落ちると lock が外れる
  semantics になっているかも、要調査)
- `--mode wait|fail` の default は? DR-0006 §7 では明示していないので決め要。

## 参考実装

- task #19 commit (= `--lock-token` 配線): `feat(cli): wire --lock-token to input family + env HYOUI_LOCK_TOKEN fallback`
- protocol Lock message 定義: `crates/hyoui/src/protocol/messages/lock.rs`
- daemon Lock handler: `crates/hyoui/src/daemon/control.rs` の `handle_lock_acquire` /
  `handle_lock_release`
