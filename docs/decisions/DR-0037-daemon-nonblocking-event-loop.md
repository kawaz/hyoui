# DR-0037: daemon のイベントループは外部の応答を待たない

- Status: Proposed (2026-09-29)。裁定待ち論点は末尾「裁定待ち」節
- Date: 2026-09-29
- Related: DR-0025 (reducer 化。単一 thread の `poll → translate → reduce → effect` loop と EffectResult feedback を本 DR が前提にする), DR-0014 (透過原則と検証主義。検出手段で介入をどこまで入れるかの判断軸), DR-0021 (PTY drain ack。PTY write の非同期化で ack の発行点を保つ必要がある), DR-0016 (record。writer thread と stop / abort の扱い), DR-0028 (graceful upgrade。state file 書き出しと fd 引き継ぎ), DR-0015 (fork daemon + attach client。daemon の stdio の出どころ)
- Origin: `docs/issue/2026-09-29-daemon-must-never-hang.md` (kawaz 裁定 2026-09-29)。事実は `docs/findings/2026-09-29-daemon-blocking-points.md`

## Context

daemon は子プロセスの生殺与奪を握る正本で、SIGCHLD の回収・client の attach・`hyoui status` への応答をすべて 1 本の serve loop が担う。serve loop が 1 箇所で戻らなくなると、これらが全部止まり、子が exit しても zombie のまま残る。

棚卸し (findings) の結果、serve loop から到達する呼び出しのうち、相手が応答しないと戻らないものが残っている:

- **client**: `Frame::decode_from` が blocking socket に対して `read_exact` する。handshake 後に frame の途中までだけ送って止まる client が 1 つあれば serve loop が止まる (実機で再現: 部分 header 2 bytes で `hyoui status` が 5 秒 timeout、切断で即応答に戻る)
- **fs**: `--debug-dump` の write、record の stop / abort 時の writer thread join、`record.start` の canonicalize / open、upgrade の state file 書き出しを main thread で行う
- **標準エラー**: daemon の fd 2 は起動した CLI の stderr (tty / pipe) を継承したまま。読み手が読まない pipe だと `eprintln!` が戻らなくなり得る
- **kernel**: SIGKILL 後の blocking `waitpid` (子が D state なら戻らない)

加えて、上限はあるが serve loop を数百 ms 単位で占有する待ち (client drain の sleep polling、PTY write の idle timeout、record の `send_timeout`、`ClientHandle::drop` の timed join) がある。占有中は回収も応答も止まる。

kawaz 裁定 (2026-09-29): 「純粋関数以外の IO を伴う部分はあらゆる箇所で非同期に設計する」。個別の blocking 呼び出しに timeout を後付けする対症療法は採らない。

## 目的

daemon がどの外部 (client / 子 / fs / 標準エラーの読み手) の振る舞いに対しても、SIGCHLD の回収・新規 attach・status 応答を続けられること。

増やしたくないもの: **serve loop の中で、外部の協力が無いと戻らない呼び出し**。「外部の協力」とは、相手プロセスが読む・書く・閉じる、fs が応答する、のいずれかを指す。timeout 付きでも「相手が応答しないと timeout まで戻らない」ものはここに含める (上限があっても占有は発生するため)。

## 不変条件

**I-1** serve loop の thread が実行する syscall は、次のいずれかに限る。

1. nonblocking fd に対する read / write / accept (EAGAIN は「今は進めない」として loop に戻る)
2. イベント待ちの本体 (`poll`)。これだけは外部を待ってよい。timeout は loop 自身が持つ deadline (後述 I-3) から決める
3. 「ブロックしない根拠」を本 DR の kernel 同期 API 節に書いた kernel 呼び出し

**I-2** blocking が本質の IO (通常ファイルへの write / open / canonicalize / fsync、標準エラーへの出力) は serve loop の外 (IO worker thread) で行う。serve loop は worker に対して **待たない送信** (満杯なら破棄して記録) だけを行い、join / 同期 recv をしない。worker の完了は waker fd 経由のイベントとして loop に戻る (DR-0025 の `EffectResult` feedback と同じ形)。

**I-3** 時間を待つ処理は `sleep` ではなく deadline として state に持ち、`poll` の timeout に畳み込む。loop 内の `thread::sleep` は置かない。

**I-4** serve loop の 1 周に掛かる CPU 時間は入力サイズに比例する範囲に収める (vt100 parse、CBOR decode 等の純粋計算)。外部の速度には依存しない。

責務外 (本 DR は扱わない、境界の記述として置く):

- IO worker thread 自身が固まること。I-2 により loop には伝播しない。固まった worker の後始末 (thread の上限・record の abort 表示) は裁定待ち Q6
- attach client 側プロセスの固まり。client は daemon と別プロセスで、DR-0029 で「覗き窓」と位置づけ済み

## runtime の選択肢

| 軸 | A. 現行 `poll` + self-pipe の延長 (全 fd nonblocking) | B. mio (kqueue / epoll 抽象) | C. tokio (async runtime) |
|---|---|---|---|
| DR-0025 reducer 化との整合 | そのまま。DR-0025 が採った単一 thread loop を変えず、effect の結果を後から feedback するだけ | ほぼそのまま。loop の形は同じで、待ちの primitive が `Poll::poll` + `Waker` に変わる | 衝突する。reducer は同期関数のまま使えるが、effect 実行が `async` になり、DR-0025 が棄却した Alternative B1 (tokio actor) 側へ寄る |
| 透過原則 (DR-0014) | 影響なし (子から見える挙動は変わらない) | 影響なし | 影響なし。ただし fork (forkpty) と upgrade の self-exec が multi-thread runtime 下になり、fork 後の子で使える API の制約 (async-signal-safe のみ) を runtime の thread と併せて管理する必要が出る |
| 変更範囲 | client read / write、handshake、PTY write、shutdown 系の state machine 化。新規依存なし | A と同じ範囲 + 待ち primitive の置換 (self-pipe を `Waker` に、SIGCHLD を signal-hook-mio 等に) | serve / accept / broadcast / record / upgrade のほぼ全面書き換え、依存追加 |
| テスト容易性 | 部分 frame decoder・送信 queue・deadline が純粋 state になり unit test できる。fd 周りは既存の socketpair テストで足りる | A と同等 | runtime 起動込みのテストになる。`#[tokio::test]` で書けるが、PTY / fork を含むと runtime の制約が増える |
| 待ち fd の数 | `poll(2)` は O(n)。client 上限 64 + 数本なので問題にならない | O(1) 相当 | O(1) 相当 |
| worker 完了の通知 | self-pipe を 1 本足して waker にする (SIGCHLD self-pipe と同じ作り) | `Waker` が標準で用意される | channel + `.await` |

**推し: A**。理由:

- DR-0025 は単一 thread の同期 loop を既に決めており、本 DR の不変条件はその loop の中身を nonblocking にするだけで満たせる。runtime を替える必然性が無い
- 待ち fd は最大 70 本程度で、`poll(2)` の O(n) が問題になる規模ではない。B の利点 (スケーラビリティ・Waker) は A でも self-pipe で代替でき、依存を増やす理由にならない
- C は fork / self-exec / signal の扱いが runtime と絡み、変更範囲に対して得るもの (async 構文) が小さい

B は A と排他ではなく、「`poll(2)` の fd 走査コストが実測で問題になったら置換する」位置づけ。採否は裁定待ち Q1。

## 各経路の非同期化の形 (A の場合)

findings の ID で対応を示す。

| 対象 | 形 |
|---|---|
| client 受信 (C-1) | reader を O_NONBLOCK にし、client ごとの受信 buffer + 増分 frame decoder (純粋関数: `feed(&[u8]) -> Vec<Frame>`) で読む。1 周で読むのは `read` 1 回分まで |
| client 送信 (C-2, C-6, C-8, C-10) | client ごとの送信 queue を loop が持ち、O_NONBLOCK write + `POLLOUT` で流す。queue の byte 上限超過で切断 (現行 backpressure と同じ判定)。切断は socket close だけで完結し、join が無い。「ack を送り切ってから切る」は queue が空になった時点で close する deadline 付き state で表す。writer thread は廃止 (裁定待ち Q2) |
| handshake (C-4, C-9) | worker thread をやめ、pending client の state (受信 buffer + deadline 5s) として loop 内で扱う。response も送信 queue に積む。mpsc の `try_recv` のための 50ms poll cap が不要になる |
| accept (C-5) | listener を O_NONBLOCK にし、EAGAIN / ECONNABORTED は loop に戻る |
| PTY 書き込み (P-2) | `Effect::TtyWrite` を master の送信 queue に積み、`POLLOUT` で流す。書き切った時点 (または無進捗 deadline 超過) で `EffectResult::TtyWrite` を feedback し、DR-0021 の ack はそこで発行する (発行点の意味は現行と同じ「PTY drain 完了」)。同一 client の後続 raw_data の扱いは裁定待ち Q7 |
| master EOF 時の sleep (P-3) | deadline に置き換え、`poll` の timeout に畳み込む |
| record (F-3, F-4, F-5) | push は `try_send` (満杯なら既存どおり欠番 + abort)。stop / abort は Sender を落とすだけで join しない。writer thread の終了は waker 経由のイベントで受け、`record.stop` の応答はそのイベントで返す。`record.start` の path 検証と open も worker で行い、結果イベントで応答する |
| `--debug-dump` (F-1, F-2) | record と同じ IO worker に載せる |
| upgrade (F-6) | precheck と state file 書き出しを worker で行い、完了イベントで exec に進む。exec 直前の fd 付け替えは loop が行う (worker と fd を共有しない) |
| 標準エラー (E-1) | ready 通知の後、fd 2 を daemon 専用の出力先に付け替え、ログは bounded channel 経由で logger thread が書く (満杯なら破棄して件数を数える)。付け替え先は裁定待ち Q3。継承した fd に O_NONBLOCK を付ける方法は、file description を共有する呼び出し元の端末や pipe の挙動まで変えるので採らない |
| shutdown (C-10, C-11, K-4) | 「drain 中」「finalize 中 (grace deadline)」「linger 中」を serve の state として loop 内で進める。sleep polling を置かない |

## kernel 同期 API の扱い

I-1 の 3 に当たるもの。各 API について、serve loop で呼んでよい根拠を書く。

| API | 呼ぶ場所 | ブロックしない根拠 |
|---|---|---|
| `waitpid(WNOHANG \| WUNTRACED \| WCONTINUED)` | SIGCHLD イベント、stopped 中の定期確認 | WNOHANG は状態変化が無ければ即 0 を返す (POSIX)。flag なしの `waitpid` は loop では使わない |
| `kill` / `killpg` 相当 (`kill(-pgid)`) | kill / signal / 終了条件 | signal の配送は enqueue で、受け手の処理を待たない |
| `getpgid` / `kill(pid, 0)` | status | プロセス表の参照のみ |
| `ioctl(TIOCSWINSZ)` | resize | PTY の winsize 構造体の更新と SIGWINCH の enqueue のみで、読み手を待たない |
| `/proc/<pid>/stat` の read (Linux) | stopped 中の復帰確認 | procfs は kernel 内で生成され、外部の fs に依存しない |
| `tcsetpgrp` | forkpty 後、exec 前の子プロセス内だけ (`sys/raw.rs`) | serve loop からは呼ばない。子側で呼ぶため loop の不変条件の対象外 |

SIGKILL 後の reap (K-5): 現行は flag なしの `waitpid` で見届ける。D state の子に対しては戻らない。I-1 に合わせ、SIGKILL 後も WNOHANG + deadline で確認し、deadline を過ぎたら reap せずに daemon を終了する (daemon が消えると子は init / launchd に引き取られて回収される)。この場合の exit code の扱いは裁定待ち Q4。

## 固まった daemon の検出

前提として、serve loop が完全に止まると **status.query 自体に応答できない**。heartbeat を status.response に載せても、止まった daemon からは返ってこないので、完全停止の検出には使えない。検出は 2 層に分ける。

1. **CLI 側の分類 (完全停止の検出)**: `hyoui list` / `status` の probe で、`connect` の結果と応答で 3 状態に分ける
   - `connect` が ECONNREFUSED / ENOENT → `stale` (daemon が居ない)
   - `connect` は成功 (kernel の listen backlog が受ける) が、handshake response が deadline 内に来ない → `hung` (daemon は居るが loop が止まっている)
   - 応答あり → `live`
   `docs/issue/2026-09-29-list-auto-prune-stale-and-show-hung.md` の表示側と対応する。`hung` の socket は prune 対象にしない (daemon と子が生きているため)
2. **daemon 内 watchdog (原因の記録)**: serve loop は周回ごとに単調増加カウンタと「今どの処理段にいるか」の label を atomic に書く。watchdog thread がカウンタの停滞 (例: 5 秒) を検出したら、label と停滞時間をログに 1 行書く。watchdog は IO を logger 経由でしか行わないので、それ自体は止まらない
3. **占有の可視化 (遅延の検出)**: status.response に「直近の loop 1 周の最大所要時間」と「最後に周回を終えてからの経過時間」を載せる。完全停止ではないが数百 ms 占有する経路 (findings の有界な占有) を観測できる

自動 kill は提案しない。daemon を kill すると master fd が閉じて子に SIGHUP が届き、子のセッションごと終わる。loop が止まっても子自身は動き続けており、子を巻き込んで終わらせる必然性は無い (DR-0014 の透過原則)。`hung` を表示し、ユーザが `kill` するかを判断する。

## 段階移行

実機で再現した経路と、影響が大きい経路から順に進める。各段は単独で入れられる。

1. **client 受信の nonblocking 化 + 増分 decoder** (C-1)。1 client で daemon を止められる経路で、実機再現済み。handshake 後の client のみ対象にし、handshake の worker は残してよい
2. **標準エラーの付け替え + logger** (E-1)。起動した呼び出し元の stderr を daemon が持ち続けること自体をやめる
3. **fs IO の worker 化** (F-1〜F-6)。record の join 撤去が中心
4. **client 送信の loop 内 nonblocking 化と writer thread の廃止** (C-2, C-6, C-8, C-10)。v0.9.55 の `ClientHandle::drop` の timed join + `shutdown(Write)` は、この段までの **暫定の上限** として残す。この段で writer thread ごと無くなり、Drop は close だけになる
5. **PTY 書き込みの非同期化** (P-2)。DR-0021 の ack を `EffectResult` から発行する形に移す。DR-0025 の effect feedback の形に合わせて入れる
6. **handshake の loop 内化、accept の nonblocking 化、sleep の deadline 化、shutdown の state 化** (C-4, C-5, C-9, C-11, P-3, K-4, K-5)
7. **検出手段** (CLI 側の `hung` 分類は 1 と並行してよい。watchdog と status の占有指標は 4 以降)

各段の受け入れは、下記テストマトリクスの該当行が serve の応答性を保つこと。

## テストマトリクス

各セルで「別 client の `hyoui status` が 1 秒以内に応答する」「子の exit 後に daemon が回収して終了する」の 2 点を確認する。

| 条件 | 子の種類 (DR-0014 の 3 category) |
|---|---|
| client が handshake 後に frame の途中で止まる | vim / cat / bash |
| client が一切読まない (受信 buffer 満杯) | vim / cat / bash |
| client を SIGSTOP | vim / cat / bash |
| 子が大量出力しつつ exit | cat (`yes \| head -c 100M` 相当) / bash |
| 子が SIGSTOP 中に大きな raw_data を受ける | cat / bash |
| `--debug-dump` 先が読み手の止まった FIFO | cat |
| record の出力先への書き込みが進まない (FIFO を使えない create_new の制約があるため、worker に注入点を設けて再現する) | cat |
| 標準エラーが読まれない pipe | cat |
| SIGKILL 後も reap できない子 (注入点で再現) | — |

## 裁定待ち

- **Q1 runtime**: A (`poll` + self-pipe の延長) と B (mio) のどちらにするか。推しは A (理由は runtime 節)
- **Q2 client 送信**: writer thread を廃止して loop 内 nonblocking write にするか、writer thread を残して「Drop は join しない (detach + shutdown)」にするか。推しは廃止 (thread の生存管理と join がそもそも無くなる、backpressure の計算が loop 内の純粋 state になる)。残す案は変更範囲が小さい
- **Q3 daemon のログ出力先**: 起動後の fd 2 と logger の書き先を、state dir 配下の session ごとのログファイルにするか、`/dev/null` にするか。起動失敗の報告は現行どおり ready 通知前は呼び出し元の stderr に出す
- **Q4 reap できない子**: SIGKILL 後 deadline を過ぎても reap できない子を置いて daemon が終了するときの exit code (現行の `128 + 9` とするか、別の値で区別するか)
- **Q5 検出**: 検出節の 3 層 (CLI 側の `hung` 分類 / watchdog のログ / status の占有指標) のうちどこまで入れるか。CLI 側の分類は必須と考える
- **Q6 固まった IO worker**: fs が応答せず worker が戻らない場合に、worker thread の上限を設けるか (上限到達で `record.start` を拒否する等)、record を aborted として表示するか
- **Q7 PTY 書き込み中の後続 raw_data**: 同一 client の raw_data が前の書き込み完了前に来たとき、queue に積むか、完了まで拒否するか。DR-0022 の auto-lock があるため client 間の順序は lock が保証し、ここで決めるのは同一 client 内の扱い

## Consequences

良い影響:

- どの外部が止まっても SIGCHLD の回収と status 応答が続く。zombie の残留と「生きているが固まる」daemon が構造的に無くなる
- client の受信・送信・handshake が純粋 state になり、socket を立てずに unit test できる範囲が増える
- writer thread と handshake thread が無くなる (Q2 で廃止を選んだ場合)

コスト・リスク:

- client 送受信・PTY 書き込み・shutdown を state machine に書き換える変更範囲が大きい。段階移行の各段で既存テストが通ることを確認しながら進める
- PTY 書き込みの非同期化で、DR-0021 の ack 発行点が「effect 実行直後」から「後続周回の完了イベント」に移る。意味 (PTY drain 完了) は同じだが、ack の順序を検証するテストを先に固める必要がある
- IO worker が固まった場合は thread が残る (Q6)

## Alternatives

- **個別の blocking 呼び出しに timeout を後付けする**: kawaz 裁定で不採用。上限があっても占有は起きる上、既に block している syscall に後から `SO_SNDTIMEO` を設定しても効かない (v0.9.55 の調査で macOS 実測) ように、timeout の効き方が呼び出しごとに違い、網羅を保証できない
- **serve loop を複数 thread に分ける (domain ごと)**: DR-0025 が Alternative D として棄却済み (ordering の決定性を失う)
