# DR-0016: `hyoui record` — tty I/O timeline の永続録画 subcommand

- Status: Active
- Date: 2026-06-01
- Related: DR-0005 (= 外側自動操作主軸、本 DR の思想根拠), DR-0008 (= protocol cap flag), DR-0013 (= daemon = screen state 正本、本 DR は I/O stream 正本化), DR-0014 (= 透過原則 + 検証主義、本 DR は観測道具の整備), DR-0015 (= jobcontrol notify / resume protocol、本 DR の lifecycle event 経路)
- Supersedes / Superseded by: なし
- Issue: [docs/issue/2026-05-26-feature-recording-and-dump.md](../issue/2026-05-26-feature-recording-and-dump.md) (= 元 idea、本 DR は record 部分のみ MVP 切り出し)

## Context

### 観測道具の不足

現状 hyoui に **I/O timeline を完全記録する手段が無い**:

| 既存道具 | 性質 | 限界 |
|---|---|---|
| `hyoui tail` | ad-hoc stdout stream | CLI 終了で止まる、永続化されない、停止前の history 取れない |
| `hyoui screen dump` | screen state 静止画 | 1 時点の cells のみ、bytes-level timeline 取れない |
| `hyoui screen snapshot` | cells + cursor + mode の構造化 dump | 同上、history なし |

→ **bug 再現時の bytes-level timeline / signal-level event 順序**を後から解析できない。

### 直近の動機: ctrl-z bug 解析

`docs/issue/2026-05-28-bug-claude-tui-ctrl-z-followup.md` 系の 3 件は「TUI app に ctrl-z を送ったとき何が起きるか」を bytes + signal 単位で観察する必要がある (= `empirical-verification` + CLAUDE.md §道具揃った段階の運用)。観測道具が無いと推測実装に逆戻り → DR-0014 §Anti-patterns の再演リスク。

### 思想根拠

- DR-0005: hyoui は「外側自動操作主軸 + 透明性最優先」。**自動操作の前提として観測可能性**が必要
- DR-0013: daemon は screen state 正本。**bytes-level I/O も daemon が正本** (= 子 PTY と client の間に daemon が居て全 bytes を broadcast 済)
- DR-0014: 観測道具を使う、推測ベース実装禁止
- **観測道具が観測対象を歪めない**ことが必須 (= 録画が PTY read/write loop を止める / 観測点の race で順序を保証できない 等は不可)

## Decision

### MVP scope (= 本 DR で実装)

#### 1. 命名: `record` (= `dump` ではない)

- **継続録画** = `hyoui record start/stop/list` (= 本 DR、timeline)
- **静止画** = 既存 `hyoui screen dump` (= 1 時点 cells)
- protocol kind も `record.start.request` 等の dotted naming で `screen.dump.*` と分離

`dump` を「永続録画」と「静止画」両方に使うと意味が衝突する (= 既存 `hyoui screen dump` / `screen.dump.*` との混同)。v1.0 までは breaking 自由なので、ここで分離を確定する。

#### 2. CLI: `hyoui record` subcommand 3 種

```bash
hyoui record start <session> --output PATH [--stdin|--stdout|--both] [--format=jsonl|raw] \
    [--max-bytes <N>] [--max-duration <DUR>] [--input-secrecy <POLICY>]
hyoui record stop <session> [--id <ID>] [--all]
hyoui record list <session> [--format=table|jsonl]
```

- session selector は既存 `parse_session_targeted` helper 共通化 (= `--socket PATH` / `--index N` / 位置引数 全部使える)
- `--stdin` (= client → daemon → 子 PTY、認可済 write 成功 bytes のみ)、`--stdout` (= 子 PTY → daemon、加工前生 bytes)、`--both` (= default、両方記録)
- **format default は `jsonl`**。`raw` は単一 direction 限定 (= `--both` 不可)、timestamp / lifecycle event なし (= stream export 専用、診断 timeline 用ではない)
- `--max-bytes` default 100MB / `--max-duration` default 1h、到達で自動 stop。明示 `0` で disable 可 (= disable 時は loud warning)
- `--input-secrecy` default `redact-after-prompt` (= §6 詳述)、`record-all` / `never-record-stdin` を opt-in
- `--id` は **複数 active record がある時のみ必須**、single record なら省略可。`--all` で全停止 (= 同 session の全 record)
- `--output PATH` は **絶対 path 必須** (= relative は CLI 側で reject、daemon の cwd と client の cwd 不一致を避ける)

`hyoui record start` 実行時、stderr に warning を必ず出す:

```
WARNING: record file contains ALL bytes including potential secrets
  (passwords typed at prompts, OTP, API tokens).
  Output: /tmp/foo.jsonl (mode 0600, only readable by your user).
  Default input redaction: --input-secrecy=redact-after-prompt is active.
  Do NOT share record files outside your authentication boundary.
```

#### 3. format spec — `jsonl`

**1 行目 (header、必須)**:

```jsonl
{"v":1,"type":"hyoui-record-jsonl/1","session":"foo","daemon_pid":1234,"daemon_boot_id":"abc123","started_unix_ms":1717000000000,"argv":["claude"],"cwd":"/Users/.../kawaz/hyoui/main","direction":"both","input_secrecy":"redact-after-prompt"}
```

field:
- `v`: format version, integer。**forward-compat 規約**: field 追加のみは同 v 維持 (= parser は unknown field を ignore する MUST)。enum 新値追加は同 v 維持 (= parser は `"unknown:<value>"` 形式で保持する MUST)。field 削除 / 型変更は v increment
- `type`: file type marker `"hyoui-record-jsonl/1"` (= 外部 tool 識別用 magic)
- `session`: session id
- `daemon_pid`: daemon プロセス pid (= debug metadata)
- `daemon_boot_id`: daemon プロセス起動 ID (= UUID or random hex)。**daemon restart 跨ぎの真の識別子** (= pid は再利用される)
- `started_unix_ms`: record 開始時刻 (= body 行 `ts_unix_ms` の基準)
- `argv`: 子 PTY 起動時の argv (= header 段階で sensitive pattern を `<REDACTED>` 化、§Security)
- `cwd`: daemon 起動時の cwd (= sensitive path は `<REDACTED>` 化)
- `direction`: `"stdin"` | `"stdout"` | `"both"`
- `input_secrecy`: `"redact-after-prompt"` | `"record-all"` | `"never-record-stdin"`

**2 行目以降 (body)**:

bytes event:
```jsonl
{"ts_unix_ms":1717000000123,"seq":1,"dir":"out","bytes":"68656c6c6f"}
{"ts_unix_ms":1717000000125,"seq":2,"dir":"in","client_id":42,"bytes":"1b5b41"}
```

field:
- `ts_unix_ms`: epoch ms (絶対値、header `started_unix_ms` との差分が delta)
- `seq`: record_id 単位で monotonically increasing、event 順序の正本 (= timestamp は補助、複数 producer から push される event の **真の順序**)
- `dir`: `"in"` (= 子 PTY に write 成功した bytes、§4 詳述) | `"out"` (= 子 PTY から read した bytes、screen 加工前)
- `client_id`: `in` のみ、送信元 client (= daemon 内 monotonic、daemon restart で reset、跨 daemon は `daemon_boot_id` と組合せで unique)
- `bytes`: hex string (lowercase、no separator)

reject / failure event:
```jsonl
{"ts_unix_ms":...,"seq":3,"ev":"in-rejected","client_id":42,"client_mode":"ro","lock_holder_client_id":7,"reason":"ro-client","bytes":"1b5b41"}
{"ts_unix_ms":...,"seq":4,"ev":"in-write-error","client_id":42,"requested_len":150,"written_len":100,"error":"timeout","unwritten_bytes":"..."}
{"ts_unix_ms":...,"seq":5,"ev":"in-secret-redacted","client_id":42,"byte_count":12,"reason":"password-prompt-detected"}
```

field:
- `ev`: event 種類 (= bytes event 以外)
- `client_id` / `client_mode`: 送信元 (= `in-rejected` で必須、bug 解析時の「誰が」を満たす)
- `lock_holder_client_id`: lock 持ってる client (= `lock-not-held` 時の context)
- `reason`: `"ro-client"` | `"lock-not-held"` (= policy reject 専用、§5 詳述)
- `in-write-error` の `requested_len` / `written_len` / `error` / `unwritten_bytes`: partial write 結果の正確な記録 (= §4 詳述)
- `in-secret-redacted` の `byte_count` / `reason`: redaction 統計のみ、内容は捨てる (= §6)

lifecycle event:
```jsonl
{"ts_unix_ms":...,"seq":6,"ev":"child-stopped-observed","sig_name":"SIGTSTP","sig_num":20,"pid":1234}
{"ts_unix_ms":...,"seq":7,"ev":"resume-request-received","client_id":42}
{"ts_unix_ms":...,"seq":8,"ev":"sigcont-sent","pid":1234}
{"ts_unix_ms":...,"seq":9,"ev":"child-continued-observed","sig_name":"SIGCONT","sig_num":18,"pid":1234}
{"ts_unix_ms":...,"seq":10,"ev":"client-attached","client_id":43,"mode":"rw"}
{"ts_unix_ms":...,"seq":11,"ev":"client-detached","client_id":43}
{"ts_unix_ms":...,"seq":12,"ev":"lock-acquired","client_id":42}
{"ts_unix_ms":...,"seq":13,"ev":"lock-released","client_id":42}
{"ts_unix_ms":...,"seq":14,"ev":"record-aborted","dump_id":1,"reason":"io-error","detail":"ENOSPC"}
```

ctrl-z 解析で重要な **4 段階分離** (= DR-0015 jobcontrol protocol の各段階を区別):
1. `child-stopped-observed`: WUNTRACED で stopped を観測した瞬間 (= daemon 側)
2. `resume-request-received`: client から `session.child.resume.request` 受信 (= daemon 側)
3. `sigcont-sent`: daemon が `killpg(SIGCONT)` を実行した瞬間
4. `child-continued-observed`: WCONTINUED で continued を観測した瞬間

これらを区別しないと「resume request 送信は届いたが SIGCONT が effective でなかった」「SIGCONT 送ったが kernel が continued 観測してない」のような bug を切り分けられない。

#### 4. `in` event の意味論 (= partial write 対応)

daemon の内部 helper `write_with_idle_timeout` (= 現 `write_all_with_idle_timeout` の改修版) は以下の戻り型に統一:

```rust
pub struct WriteOutcome {
    pub requested_len: usize,
    pub written_len: usize,
    pub error: Option<WriteError>,
}
pub enum WriteError {
    IdleTimeout,
    IoError(io::Error),
}
```

`write_with_idle_timeout` を呼んだ瞬間に record sink に push する event:
- `written_len > 0`: `in` event (= write 成功 prefix `bytes[0..written_len]` を bytes として記録、`client_id` 付き)
- `error.is_some()`: `in-write-error` event (= `requested_len` / `written_len` / `error` / `unwritten_bytes = bytes[written_len..]` を記録)

両方発火する可能性あり (= partial write + timeout、`in` で 100 bytes + `in-write-error` で 50 bytes、別 `seq` で順序保証)。

`in-rejected` (= policy reject、認可前) と `in-write-error` (= transport failure、認可後) は別 event 種類で意味論を分離。

**注**: `write_with_idle_timeout` の改修は本 DR scope に含めるが、内部 helper の signature 変更なので protocol breaking なし。

#### 5. format spec — `raw`

- bytes そのまま file に write (= `cat` 互換、stream export 専用)
- direction 識別なし → `--stdin` または `--stdout` 単一 direction 限定、`--both` は raw 形式 invalid
- timestamp / sequence / lifecycle event / reject event なし
- header なし (= 1 byte 目から raw bytes、`type` 識別不能)
- 用途: 「子 PTY 出力を別 process に流す」「`cat` で再生する」等の stream export
- **診断 timeline には使えない** (= `jsonl` を使え)、`--help` / docs で明示

#### 6. secret redaction (= `--input-secrecy=redact-after-prompt` default)

子 PTY が password prompt 等の pattern を出した直後の stdin を hex で record file に永続化するのは forensic risk として深刻 (= ssh / sudo / gh auth / 1Password CLI の password 入力)。default で redaction を効かせる:

`--input-secrecy=redact-after-prompt` (default):
- `out` event 内に prompt pattern (default regex: `(?i)(password|passphrase|secret|token|otp|verification\s*code)[\s:]*$`) を観測した瞬間に redaction mode ON
- redaction mode 中の `in` event は `in-secret-redacted` event に置き換え (= `byte_count` + `reason` のみ、bytes は捨てる)
- 次の改行 (`\n` / `\r`) を `in` または `out` で観測した瞬間に redaction mode OFF

`--input-secrecy=record-all` (opt-in):
- redaction なし、全 stdin bytes を hex で記録
- `record start` 時に loud warning を 3 行 stderr に出す (= 「全 secret が永続化される」明示)

`--input-secrecy=never-record-stdin` (opt-in):
- 全 stdin を `in-redacted` event 化 (= bytes 捨てる、byte_count のみ)
- bug 解析で stdin 内容が見えない代わりに secret 完全保護

custom prompt regex は `--prompt-pattern <regex>` で override 可。

#### 7. protocol message + cap flag

`crates/hyoui/src/protocol/messages/` に追加:

```rust
// kind: "record.start.request"
pub struct RecordStartRequest {
    pub direction: RecordDirection,  // Stdin | Stdout | Both
    pub format: RecordFormat,        // Raw | Jsonl
    pub output_path: String,         // absolute path required
    pub max_bytes: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub input_secrecy: InputSecrecy,
    pub prompt_pattern: Option<String>,
}
// kind: "record.start.response"
pub struct RecordStartResponse {
    pub record_id: u32,  // session-scope monotonic, u32 で十分
}
// kind: "record.stop.request"
pub struct RecordStopRequest {
    pub record_id: u32,
}
// kind: "record.stop.all.request" (= --all 用、別 message)
pub struct RecordStopAllRequest {}
// kind: "record.stop.response" (= stop / stop.all 双方の成功 ACK)
pub struct RecordStopResponse {
    pub stopped: u32,  // 停止した record 数 (= single なら 1、--all なら N)
}
// kind: "record.list.request"
pub struct RecordListRequest {}
// kind: "record.list.response"
pub struct RecordListResponse {
    pub records: Vec<RecordInfo>,
}
pub struct RecordInfo {
    pub record_id: u32,
    pub direction: RecordDirection,
    pub format: RecordFormat,
    pub output_path: String,
    pub started_unix_ms: u64,
    pub started_by_client_id: u64,
    pub raw_bytes_recorded: u64,    // 子 PTY 生 bytes 累計
    pub file_bytes_written: u64,     // file 上の bytes (= jsonl overhead 含む)
    pub last_flushed_unix_ms: u64,
}
```

CBOR encoding:
- 全 struct は **empty braces** で定義 (= `RecordStopAllRequest {}` / `RecordListRequest {}`、既存 `StatusQuery {}` 互換、unit struct `;` は CBOR null になり kind dispatch 破綻)
- field naming は serde の `#[serde(rename_all = "kebab-case")]` で kebab-case (= `record_id` → `record-id`)
- kind は dotted: `record.start.request` / `record.start.response` / `record.stop.request` / `record.stop.all.request` / `record.stop.response` / `record.list.request` / `record.list.response`
- **stop / stop.all は成功時も `record.stop.response` を返す** (= 成功を無音にすると client が recv で永久 hang する。失敗 `record-not-found` error と同じ recv 経路で受ける)
- enum は `#[non_exhaustive]` 付与 (= 新 variant 追加で caller match が壊れない)
- error code: `RecordError::PathNotAbsolute` / `OutputAlreadyExists` / `OutputPermissionDenied` / `UnsupportedDirectionForFormat` / `InvalidPromptPattern` / `RecordLimitExceeded`

cap flag: `"record-v1"` を **optional cap** で追加。

- daemon は cap negotiate で client から `record-v1` が来なければ record-related message を **reject** (= `unsupported-capability` error)
- record 未対応の old client が attach / status / tail 等他機能を使うのは無影響
- v1.0 まで breaking OK なので新 daemon は `record-v1` を必ず advertise、新 client は negotiate で得たら使う。**required cap 化は protocol evolution と衝突する** (= dump 不要 client が attach 不能になる) ので採用しない

#### 8. daemon 内部実装 (= I/O event sink、broadcast 経路と独立 + bounded queue)

既存 broadcast 機構には乗せない。理由:
- `broadcast_master_bytes` (= `daemon/session.rs:1142` 周辺) は子 PTY 出力 → client 向け protocol frame 経路、stdin が通らない
- stdin は `daemon/control.rs:133` で `master_fd().write_*` 別経路
- `Subscription::Raw` 経由だと `Frame::raw_data(...)` 済み protocol frame が書かれて raw 仕様破綻

→ daemon 内に **record 専用 I/O event sink** を別経路で持つ:

| event 種類 | 観測ポイント | 観測内容 |
|---|---|---|
| `out` bytes | `daemon/session.rs:1142` 周辺 (= PTY read 直後、scrollback/screen_state 加工 / broadcast の **直前**) | PTY 生 bytes 全部 |
| `in` bytes | `daemon/control.rs:133` 周辺 (= `write_with_idle_timeout` の return 直後、`written_len > 0` の prefix) | 子 PTY に write 成功した bytes + `client_id` |
| `in-rejected` | `daemon/control.rs:111` 周辺 (= 認可前、Ro/lock 不所持で reject 確定の瞬間) | 却下 input + `client_id` / `client_mode` / `lock_holder_client_id` / `reason` |
| `in-write-error` | `write_with_idle_timeout` の return 直後 (= `error.is_some()`) | `requested_len` / `written_len` / `error` / `unwritten_bytes` |
| `in-secret-redacted` | `in` push 直前 (= redaction mode 中の場合) | bytes は捨てる、`byte_count` + `reason` |
| lifecycle | `notify_child_stopped` / `accept` / lock 処理点 / WCONTINUED 観測点 / SIGCONT 送信点 | 4 段階 child stop/resume + client attach/detach + lock acq/rel + record-aborted |

##### 8a. bounded queue + writer task 隔離 (= 観測対象を歪めない設計)

dump 経路を PTY read/write loop と同 thread で flush すると、slow filesystem / NFS / disk full / permission error で **観測対象 (= 子 PTY) が止まる**。DR-0008 §8.2 は slow client 切り離し設計を持つので、record sink も同等の隔離を入れる:

```
[PTY read/write thread] → push event to bounded queue (= 1024 events) → [record writer thread] → file write + flush
```

- queue は `crossbeam-channel` 等の bounded queue (= 容量 1024 events、超過時 push がブロック)
- writer thread は record_id ごとに 1 つ (= 並列 record も独立)
- queue full 時の policy:
  - **push 側で 100ms timeout** で諦め、`record-aborted reason: queue-full` event を **次の隙間** で書き込み (= 子 PTY 経路は止まらない、record は止まる)
  - PTY thread は record をスキップして本来の I/O 処理を継続
- write error 時の policy:
  - 該当 record を **個別に自動 stop** + `record-aborted` を書こうとして失敗したら諦め
  - 他の record / 子 PTY / 他 client への影響なし
  - daemon stderr / log に出力

これで「観測道具が観測対象を止める」事故を構造的に防ぐ。

##### 8b. seq の生成

各 record_id 内で monotonically increasing。bounded queue に push する瞬間に確定 (= queue push 順 = seq 順 = file 行順)。queue full で event が落ちた場合は seq が **欠番**になる (= parser 側で「seq 連続性チェック」で脱落検出可能)。

#### 9. file open semantics

dump file open 時:

```rust
OpenOptions::new()
    .create_new(true)        // 既存 file への append/上書きを EEXIST で防ぐ
    .write(true)
    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
    .mode(0o600)             // owner-only、umask 非依存
    .open(path)
```

- `create_new(true)`: 既存 file は EEXIST で error。jsonl header 二重問題回避
- `O_NOFOLLOW`: symlink 拒否 (= ELOOP)。`/tmp/foo.jsonl -> /etc/passwd` 等の TOCTOU 攻撃面除去
- `O_CLOEXEC`: 子 PTY や grandchild プロセスに fd を継承させない (= fd leak 経路除去)
- mode `0o600`: socket / dir 運用と整合、owner 以外読めない

`--output PATH` validation (= request 受信時、daemon side):
- 絶対 path 必須 (= relative は client 側で reject、defensive に daemon でも check)
- 拡張子 allowlist: `.jsonl` / `.raw` / `.bin` / `.log` 限定。`.sh` / `.zshrc` / `.ssh/*` / `.config/*` / `.gnupg/*` 等の shell-interpreted / sensitive 拡張子は拒否
- 親 dir が exists かつ daemon process が write 可能であること (= 親 dir 自動作成しない)
- HOME 配下の sensitive path prefix (= `~/.ssh/`, `~/.config/`, `~/.gnupg/`, `~/.aws/`, `~/.docker/`) は拒否
- opt-out `--allow-unsafe-output` flag は MVP に含めない (= validation 厳格)

### Out of scope (= 別 task / 別 DR)

| 項目 | 移送先 |
|---|---|
| `--rotate` (= size/age-based file rotation) | 別 task |
| asciinema cast format | 別 task |
| `hyoui record` (= I/O 注入再生) / `hyoui sink` 抽象化 | [docs/issue/2026-05-26-feature-recording-and-dump.md](../issue/2026-05-26-feature-recording-and-dump.md) の v0.3.0 以降 |
| zstd 圧縮 `jsonl.zst` (= 自分ドメイン辞書付き) | [docs/issue/2026-06-01-advanced-feature-jsonl-zstd-domain-dict.md](../issue/2026-06-01-advanced-feature-jsonl-zstd-domain-dict.md) |
| stream output (= `--output -` で client stdout に流す) | 別 task (= 別 protocol message `record.stream.request` + tail 系経路) |
| fsync 戦略 (= disk full / kernel panic 時の byte 損失粒度) | 別 task |
| forensic 用 signature / hash chain | 別 task (= MVP は trustworthy daemon 前提、§Security disclaimer) |
| `hyoui record scan <dir>` (= 過去 record file の発見) | 別 task |
| signed_pid / token rotation 等 token 漏洩対応 | 別 task (= dump file 経由の secret leak は §Security 注意点) |

### Security 考慮

- **dump file は機密情報を含む可能性** (= 子 PTY の全 I/O、redaction を通り抜けた secret、TUI app 出力中の sensitive 情報)。`record start` 時の stderr loud warning + `--input-secrecy` default redaction で防護
- **file permission**: mode `0o600` + `O_NOFOLLOW` + `O_CLOEXEC` (= §9)
- **path validation**: 絶対 path / 拡張子 allowlist / sensitive path deny (= §9)
- **dump file の意図せぬ共有**: 拡張子 `.hyoui-record.jsonl` を `.gitignore` template に追加することを usage docs で推奨
- **header sanitize**: `argv` / `cwd` に sensitive pattern (= `--api-key=...` / `--token=...` / `password=...` / `~/.ssh/...` / `~/.gnupg/...`) を含む場合は `<REDACTED>` 化
- **token 漏洩 → record file 経由の session 全 I/O 漏洩**: dump file は token 認可境界内の全データを含む。token rotation を usage docs で推奨
- **trustworthy daemon 前提**: dump file は daemon が任意 bytes を書ける。**forensic evidence / 監査証跡として使用してはならない** (= 改ざんされた daemon は valid に見える dump file を生成可能)。signature / hash chain は別 task

## Implementation 概算

| 部分 | 規模 |
|---|---|
| protocol message + cap flag (record-v1 optional) | ~50 行 |
| daemon: I/O event sink (5 event 種類 + 4 段階 lifecycle) | ~250 行 |
| daemon: bounded queue + writer task (= record_id ごと 1 thread) | ~120 行 |
| daemon: write_with_idle_timeout を `Result<WriteOutcome, _>` 化 | ~30 行 |
| daemon: secret redaction state machine (= prompt pattern detection) | ~80 行 |
| daemon: safety limit (max-bytes/max-duration auto-stop) | ~50 行 |
| daemon: path validation + file open (mode 0600 + O_NOFOLLOW + O_CLOEXEC) | ~50 行 |
| daemon: header sanitize (argv/cwd) | ~30 行 |
| CLI: `hyoui record` 3 subcommand parse (+ `--id` 省略可ロジック / `--all`) | ~150 行 |
| CLI: main exec + helpers (= hex encode + path canonicalize 等) | ~80 行 |
| usage docs (`usage_record_start` 等) | ~80 行 |
| test (parser unit + protocol roundtrip + redaction + integration) | ~250 行 |
| acceptance matrix 実機検証 + `docs/findings/` 記録 | ~200 行 (docs) |
| **合計** | ~1420 行 |

## Acceptance matrix (= CLAUDE.md §検証主義、最低 3 カテゴリ + failure / 並行 / security)

実装完了の判定 matrix。`docs/findings/2026-MM-DD-dr-0016-acceptance-matrix.md` に実機結果を記録。

### app カテゴリ × format × direction (= primary 検証、`claude` TUI は secondary)

| カテゴリ | primary app | `raw --stdout` | `raw --stdin` | `jsonl --both` |
|---|---|---|---|---|
| TUI alt screen | `vim /tmp/x` | bytes 一致 | input bytes 一致 | bytes 順序 + lifecycle event 一致 |
| TUI alt screen | `tmux new-session -d` | 同上 | 同上 | ネスト PTY 経由でも記録 |
| TUI alt screen | `htop` | 同上 (= 高頻度更新) | 同上 | seq 連続性 |
| line-oriented | `cat` | bytes 一致 | input bytes 一致 | 同上 |
| line-oriented | `less /tmp/big.txt` | 同上 | 同上 | 同上 |
| interactive REPL | `python3` | bytes 一致 | input bytes 一致 | 同上 |
| interactive REPL | `bash -l` | 同上 | 同上 | 同上 |

`claude` TUI は **secondary** (= 本 DR の解析対象であり、循環依存を避けるため primary 全 pass 後に確認する応用例)。

### signal / lifecycle event との組合せ

| シナリオ | 期待 |
|---|---|
| ctrl-z 送信 (= Rw client から `\x1a`) | jsonl に `in: 1a` + `child-stopped-observed sig: SIGTSTP` の順 (= seq で順序保証) |
| client から `session.child.resume.request` 送信 | `resume-request-received` + `sigcont-sent` + `child-continued-observed` の 4 段階完全記録 |
| Ro client から input 送信 | `in-rejected reason: ro-client client_id: N` 記録 |
| lock 非保持で input 送信 | `in-rejected reason: lock-not-held client_id: N lock_holder_client_id: M` 記録 |
| write_with_idle_timeout が partial 失敗 | `in: <written prefix>` + `in-write-error requested_len: X written_len: Y` の別 line で記録 |
| client attach / detach | `client-attached client_id: N mode: rw` / `client-detached` 記録 |
| lock acquire / release | `lock-acquired` / `lock-released` 記録 |

### secret redaction

| シナリオ | 期待 |
|---|---|
| `sudo` → `[sudo] password for kawaz:` prompt → password 入力 | prompt 後の stdin が `in-secret-redacted byte_count: N` 化、`\n` で redaction OFF |
| `ssh user@host` → `Password:` prompt → 入力 | 同上 |
| `gh auth login` → `Verification code:` prompt → OTP 入力 | 同上 (= regex hit) |
| `--input-secrecy=record-all` + password 入力 | redaction なし、raw bytes 記録 (= loud warning 出てるはず) |
| `--input-secrecy=never-record-stdin` + 任意 input | 全 stdin が `in-redacted` 化 |

### file open / 並列 / safety

| シナリオ | 期待 |
|---|---|
| 既存 file への `record start` | EEXIST で abort |
| symlink (`/tmp/foo.jsonl -> /etc/passwd`) | ELOOP で abort |
| `.sh` / `.zshrc` / `~/.ssh/key.dump` への `record start` | path validation で reject |
| relative path | client 側で reject |
| 親 dir 不在 | reject (= 自動作成しない) |
| `--max-bytes 10MB` 到達 | 自動 stop + `record-aborted reason: max-bytes` 最終行 |
| `--max-duration 5s` 到達 | 同上 (`reason: max-duration`) |
| 同 session に複数 record (= 異なる `--id`) 並列 | 各 file 独立、互いに干渉なし、各 record_id で seq 独立 |
| `record stop` で `--id` 省略 + single active | 該当 record stop |
| `record stop` で `--id` 省略 + 複数 active | error 「multiple active records, use `--id <N>` or `--all`」 |
| `record stop --all` | 全 record 一括 stop |
| `--format=raw --both` | error (= raw 単一 direction 限定) |

### failure / 並行 / 観測歪曲防止

| シナリオ | 期待 |
|---|---|
| 録画中 disk full | 該当 record 個別自動 stop + 他 record / 子 PTY / 他 client への影響なし、`record-aborted reason: io-error detail: ENOSPC` 記録 |
| 録画中 daemon SIGKILL | 各 record の最後の 1 行損失で済む (= jsonl 構造維持、parser は partial line skip で残り全部読める)。kernel panic は別 (= fsync 戦略は別 task) |
| 録画中 file permission 変更 (= chmod 000) | 個別自動 stop + 他無影響 |
| bounded queue full (= 子 PTY 出力が writer 処理を超える) | push 側 100ms timeout → record スキップ、`record-aborted reason: queue-full` 記録、子 PTY は止まらない |
| multi-client interleave (= 複数 Rw client が同時に input) | 各 `in` event に `client_id`、seq で順序保証、混在しても parse 可能 |
| same-ms timestamp の event | seq で順序が一意 (= timestamp 単独では決まらない場合の正本) |
| non-UTF-8 / binary 出力 (= `cat /bin/ls`) | hex encoding で問題なし、parse 可能 |
| daemon restart 中の record 混入 | `daemon_boot_id` で識別 (= pid 再利用に強い) |

## Why not (= 不採用案の理由)

### a. `dump-v1` を required cap にする

protocol evolution と衝突。dump 不要な old client が attach すら不能になる。optional cap で `hyoui record` CLI が negotiate 失敗時に `unsupported-capability` で fail する方が筋。

### b. sequence number を入れない

bytes event と lifecycle event は別 producer (= PTY read thread / signal handler thread / control thread) から push される。kernel pipe order は同 thread 内のみで効く。**複数 producer 越しの順序保証は seq でのみ可能**。timestamp は ms 単位で衝突するし monotonic 保証もない。

### c. `sink` 抽象化を最初から導入

`docs/issue/2026-05-26-feature-recording-and-dump.md` の `hyoui sink add` 統合設計は MVP には過剰。dump / record / play は性質が異なり、抽象化先行で各々の特殊事情を 1 つの interface に詰めると複雑化。本 DR は record だけに集中、共通化は実装経験を積んだ後。

### d. asciinema cast format との互換

asciinema cast v0.2 format は外部 viewer 再生が目的、ANSI bytes を JSON string で escape 表現。hyoui の主用途 (= bug 解析の bytes-level 観察) には不適。jsonl + hex の方が grep / awk / jq で扱いやすい。`--format=cast` は別 task。

### e. base64 encoding for bytes

base64 は size 効率良いが ANSI escape pattern が人間に読めない。terminal I/O 解析の主作業は「特定 sequence の出現を追う」なので、grep しやすさ優先で hex 採用。size 効率は `.jsonl.zst` (= 別 issue) で別途解決。

### f. bytes event と lifecycle event を別 file に分離

分離すると `*.bytes.jsonl` + `*.events.jsonl` の 2 file 管理、時系列 join が要る。seq + bounded queue + writer task で「同一 producer (= writer thread) 経由で順序統一」できるので、別 file 不要 + 解析は単一 file で完結。

### g. body 行ごとの session 情報埋め込み

`session` / `daemon_pid` を毎行入れると数百万行で +30MB 級の size 膨張。header 1 行集約で十分、`daemon_boot_id` で restart 跨ぎ識別も可能。

### h. v1 で `--rotate` 入れる

bug 解析 MVP は単一 session の単一録画用途。rotate は運用 log 用途で別 task。lifecycle (= 古い file の削除 / rotate 中 file lock / list 表示の現在 vs 履歴) が複雑化する。

### i. `--include-rejected` opt-in (= reject default 含めない)

reject された input には機密情報含む可能性あり (= 機密漏れ拡大) という指摘を考慮したが、本 DR は **default redact-after-prompt** で stdin secret 自体を防護するため、`in-rejected` の bytes も redaction 対象 (= prompt 後なら `in-rejected` ではなく `in-secret-redacted` 化)。**`in-rejected` を default 含めること自体は「誰が何を送ろうとして却下されたか」の bug 解析価値を保つ**ため維持。

### j. `Subscription::Raw` に乗せる

`broadcast_master_bytes` 経路は子 PTY 出力 → client 向け protocol frame で stdin が通らず、`Frame::raw_data(...)` 済み protocol frame が書かれて raw 仕様破綻。独立 I/O event sink + bounded queue が必須。

## Anti-pattern 警戒 (= DR-0014 self-check 反映)

本 DR を起こす時点で以下を確認した:

- ☑ 既存 DR (= DR-0005 / 0013 / 0014 / 0015) で justify される観測道具整備か → ✅ DR-0014 §道具揃った段階の運用 / DR-0015 §lifecycle protocol との整合
- ☑ 透過原則を破るが必然か → ✅ daemon に I/O event sink を加える介入は最小、bounded queue で観測対象を歪めない設計
- ☑ kernel / PTY / shell の標準機能再発明か → ❌ そういう機能は無い (= `tee` は同一 process pipe、daemon 越しの broadcast には適用不可)
- ☑ 新 cap flag 追加の必然性は DR に書けるか → ✅ §7 で optional cap として説明
- ☑ 既存 DR の実装漏れ修復が優先か → ❌ 観測道具は新規追加機能で修復対象なし
- ☑ partial state を hyoui の裁量で破棄する介入か → ✅ bounded queue full で event 落とすが `record-aborted reason: queue-full` で明示記録、silent には捨てない

## Open questions

1. **kernel panic / power off** での jsonl 整合性 (= fsync 戦略 / journal 機構) → 別 task、MVP は best-effort flush + 最後の 1 行損失
2. **`record stop` で record_id 競合** → daemon が auth check (= `started_by_client_id` と stop 元 client が一致するか) を required にするか、`--force` で他人の record も止められるか → MVP は default 「全 client が任意 record を stop 可」(= 単一ユーザ運用前提)、認可境界拡張は別 task
3. **bounded queue 容量 1024 events の妥当性** → 高頻度 TUI (= `htop` / `top`) で実測必要、足りなければ tune
4. **prompt pattern regex の locale 対応** → English のみ default、日本語 (= `パスワード:` 等) / 他言語は `--prompt-pattern` で override 想定。default 強化は別 task
5. **`SIGWINCH` / `cwd-changed` の lifecycle event 追加** → MVP scope 外、応用要求が出たら追加 (= forward-compat 規約で同 v 拡張可)
6. **`hyoui screen dump` との関係再整理** → 本 DR で「record vs screen.dump」は意味分離済、命名整理 task は別 DR 不要

## 関連

- [docs/issue/2026-05-26-feature-recording-and-dump.md](../issue/2026-05-26-feature-recording-and-dump.md) — 元 idea (= record + play + sink)
- [docs/issue/2026-06-01-advanced-feature-jsonl-zstd-domain-dict.md](../issue/2026-06-01-advanced-feature-jsonl-zstd-domain-dict.md) — zstd 圧縮 (= 別 issue)
- [docs/issue/2026-05-28-bug-claude-tui-ctrl-z-followup.md](../issue/2026-05-28-bug-claude-tui-ctrl-z-followup.md) — 本 DR の直近応用先
- [docs/issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md](../issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md) — 同上
- [docs/issue/2026-05-29-bug-ctrl-z-second-time-noop.md](../issue/2026-05-29-bug-ctrl-z-second-time-noop.md) — 同上
- [DR-0005](./DR-0005-design-philosophy-external-automation.md) — 自動操作主軸の思想
- [DR-0008](./DR-0008-protocol-design.md) — protocol cap flag 規約
- [DR-0013](./DR-0013-screen-emulator-and-attach-stability.md) — daemon が正本、本 DR は I/O stream 正本化の延長
- [DR-0014](./DR-0014-transparency-and-empirical-verification.md) — 観測道具整備の根拠
- [DR-0015](./DR-0015-run-as-fork-plus-attach.md) — jobcontrol notify / resume protocol (= 本 DR の lifecycle event 4 段階分離の経路)
