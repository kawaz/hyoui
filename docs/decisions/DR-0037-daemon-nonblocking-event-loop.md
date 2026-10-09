# DR-0037: daemon のイベントループは外部の応答を待たない

- Status: Proposed (2026-09-29) — 🟡 段階 1・2 実装済。Q1 / Q2 / Q3 / Q8 と段階 1 の着手は裁定済み (2026-10-09、「裁定」節)。段階 2 (標準エラーの付け替え + logger) は実装済み (2026-10-09、「標準エラーとログ」節)。Q4〜Q7 は「裁定待ち」節
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
| DR-0025 reducer 化との整合 | そのまま。DR-0025 が採った単一 thread loop を変えず、effect の結果を後から feedback するだけ | そのまま。loop の形は同じで、待ちの primitive が `Poll::poll` に変わる | 単一 task + 同期 reducer の形にすれば DR-0025 の構造は保てる (actor 化は必須ではない)。effect 実行と IO 待ちが `async` 側に移る |
| 透過原則 (DR-0014) | 影響なし (子から見える挙動は変わらない) | 影響なし | 子から見える挙動は変わらない。current-thread runtime なら runtime 由来の thread は増えないが、blocking IO を `spawn_blocking` に逃がすと thread pool が立ち、fork (forkpty) / upgrade の self-exec の時点で存在する thread の管理が要る |
| 変更範囲 | client read / write、handshake、PTY write、shutdown 系の state machine 化。新規依存なし | A と同じ範囲 + `poll` 呼び出しの置換。既存の self-pipe / SIGCHLD pipe は fd として登録でき、置き換えは必須ではない | A の state machine 化の代わりに各経路を async IO で書き直す。serve / accept / broadcast / record / upgrade の広い範囲に及び、依存追加 |
| テスト容易性 | 部分 frame decoder・送信 queue・deadline が純粋 state になり unit test できる。fd 周りは既存の socketpair テストで足りる | A と同等 | reducer 部分の unit test は変わらない。IO を含むテストは runtime 起動込みになる |
| 待ち fd の数 | `poll(2)` は O(n)。client 上限 64 + 数本なので問題にならない | O(1) 相当 | O(1) 相当 |
| worker 完了の通知 | self-pipe を 1 本足して waker にする (SIGCHLD self-pipe と同じ作り) | 既存 pipe の登録でも、mio の `Waker` でもよい | channel + `.await` |

**推し: A**。理由:

- DR-0025 は単一 thread の同期 loop を既に決めており、本 DR の不変条件はその loop の中身を nonblocking にするだけで満たせる。runtime を替える必然性が無い
- 待ち fd は最大 70 本程度で、`poll(2)` の O(n) が問題になる規模ではない。B の利点 (kqueue / epoll の O(1)、`Waker`) は A でも self-pipe で代替でき、依存を増やす理由にならない
- C は single task + 同期 reducer で DR-0025 と両立できるが、得るもの (async 構文による state machine の記述) に対して、既存経路の書き直し範囲と fork / self-exec 時の thread 管理の考慮が増える

B は A と排他ではなく、「`poll(2)` の fd 走査コストが実測で問題になったら置換する」位置づけ。採否は裁定待ち Q1。

## 各経路の非同期化の形 (A の場合)

findings の ID で対応を示す。

| 対象 | 形 |
|---|---|
| client 受信 (C-1) | client ごとの増分 frame decoder (I/O を持たない純粋な state: 届いた bytes を `push` し、揃った frame を `next_frame` で 1 つずつ取り出す。`is_ready` は追加の受信なしで frame か protocol error を取り出せるか) で読む。1 周で読むのは `recv` 1 回分まで、処理するのは 1 client につき 1 frame まで (decoder に揃った frame が残っていれば recv せずにそれを処理する。残っている周回は poll で待たない)。reader と writer は `try_clone` による同一 open file description なので、O_NONBLOCK を fd に付けると writer 側 (handshake response、writer thread の `write_all`) も nonblocking になり EAGAIN で部分送信・切断が起きる。送信が blocking のままの間は `recv(2)` の `MSG_DONTWAIT` (呼び出し単位の nonblocking。Linux / macOS の recv(2) にある) で読み、fd のフラグは変えない。送信も loop 内 nonblocking に移した段で fd ごと O_NONBLOCK にする |
| client 送信 (C-2, C-6, C-8, C-10) | client ごとの送信 queue を loop が持ち、O_NONBLOCK write + `POLLOUT` で流す。queue の byte 上限超過で切断 (現行 backpressure と同じ判定)。切断は socket close だけで完結し、join が無い。「ack を送り切ってから切る」は queue が空になった時点で close する deadline 付き state で表す。writer thread は廃止 (裁定待ち Q2) |
| handshake (C-4, C-9) | worker thread をやめ、pending client の state (受信 buffer + deadline 5s) として loop 内で扱う。response も送信 queue に積む。mpsc の `try_recv` のための 50ms poll cap が不要になる |
| accept (C-5) | listener を O_NONBLOCK にし、EAGAIN / ECONNABORTED は loop に戻る |
| PTY 書き込み (P-2) | `Effect::TtyWrite` を master の送信 queue に effect 単位 (EffectId 付き) で積み、`POLLOUT` で流す。失敗意味論は下の「PTY 書き込み effect の完了と失敗」で effect 単位に定める |
| master EOF 時の sleep (P-3) | deadline に置き換え、`poll` の timeout に畳み込む |
| record (F-3, F-4, F-5) | push は `try_send` (満杯なら既存どおり欠番 + abort)。stop / abort は Sender を落とすだけで join しない。writer thread の終了は waker 経由のイベントで受け、`record.stop` の応答はそのイベントで返す。`record.start` の path 検証と open も worker で行い、結果イベントで応答する |
| `--debug-dump` (F-1, F-2) | record と同じ IO worker に載せる |
| upgrade (F-6) | 旧プロセス側: precheck (`metadata`)、state file 書き出し、準備失敗・execve 失敗からの復帰時の state file `remove_file`、prep 失敗時の socket `unlink` (`UnixSock::drop`) を worker で行い、完了イベントで次段 (exec / 旧 serve 継続 / 終了) に進む。exec 直前の CLOEXEC 操作と fd 付け替えは loop が行う (worker と fd を共有しない)。新プロセス側: resume の state file open / decode / `remove_file` は serve loop 開始前に走るので I-1 の対象外だが、固まると upgrade 後の daemon が serve を始めないため、deadline 付きの worker で読み、期限超過なら既存の env 最小 subset fallback で resume する |
| 通常終了時の socket `unlink` | `UnixSock::drop` の unlink を worker に渡す (serve loop を抜けた後の shutdown state でも loop 外で待たない) |
| 標準エラー (E-1) | ready 通知の時点で、fd 2 を daemon 専用の出力先 (session ごとのログファイル、裁定 Q3) に付け替えておき、ログは bounded channel 経由で logger thread が書く (満杯なら破棄して件数を数える)。詳細は「標準エラーとログ」節。継承した fd に O_NONBLOCK を付ける方法は、file description を共有する呼び出し元の端末や pipe の挙動まで変えるので採らない |
| shutdown (C-10, C-11, K-4) | 「drain 中」「finalize 中 (grace deadline)」「linger 中」を serve の state として loop 内で進める。sleep polling を置かない |

## 標準エラーとログ (E-1、段階 2)

裁定 Q3 (起動後の fd 2 と logger の書き先は state dir 配下の session ごとのログファイル、ready 通知の前の起動失敗は呼び出し元の stderr) を次の形で実装する。

| 項目 | 決定 |
|---|---|
| 置き場 | `<状態の root>/sessions/logs/<session id>.log` (`hyoui::paths::Env::session_log_path`)。web の `web/logs/<name>.log` と同じく、機能の dir の下の `logs/` に置く。`sessions/` 直下の socket / lock と分かれ、discovery (`sessions/*.sock`) と混ざらない。dir は 0700、file は 0600 で作り、追記で開く (fd は CLOEXEC) |
| path を決める側 | `hyoui run` (CLI) が socket と同じ env から決め、`HYOUI_DAEMONIZE_INIT` の `log` で daemon に渡す (daemon の env scrub に左右されない)。session id が UUID の標準形でない時は path に混ぜず、置き場無しとして扱う |
| 付け替えの時点 | `Session::start` (socket の bind と子の spawn) が成功した後、ready 通知を書く **前**。親は ready を読むと exit するので、その時点で daemon は呼び出し元の stderr を持っていない (`$(hyoui run --detached ... 2>&1)` は親の exit で返る)。ready の前の失敗はすべて呼び出し元の stderr に出る |
| 開けない・置き場が無い時 | 呼び出し元の stderr に原因を 1 行出し、fd 2 を `/dev/null` にして起動は続ける (ログのために起動を止めない)。どの場合も呼び出し元の stderr は手放す |
| logger | daemon のログは `hyoui::log::emit` (`daemon_log!`) で bounded channel に `try_send` する (serve loop は待たない、I-2)。logger thread が時刻 (UTC の ISO 8601、秒精度) を付けて書く。logger の無い process (ready の前、test が `Session` を直接動かす時) では stderr に直接書く |
| channel の大きさ | 1024 行 (`hyoui::log::CHANNEL_CAPACITY`)。daemon のログは警告と失敗の報告だけで、1 周に数行も出ない。1 行 200 bytes として最大 200 KiB 程度の滞留に収まる |
| 満杯の時 | 捨てて件数を数え、logger が次の行を書く前と終了時に `hyoui: log: dropped N line(s) because the log queue was full` を 1 行書く |
| 1 ファイルの上限 | 1 MiB (`hyoui::log::FILE_CAP_BYTES`)。超える行は書かず、印を 1 行書いて以後の行を捨てる。同じ id で起動し直した session は続きに追記し、上限は既存の大きさから数える |
| 寿命 | 何も書かれなかったログは daemon の終了時に logger が消す (path が開いた file と同じ実体を指す時だけ)。書かれたログは session の終了後も残し、自動では消さない。daemon が終了処理を経ずに終わる (SIGKILL 等) と空のログが残る |
| 終了時 | serve を抜けた後、logger が積まれた行を書き終えるのを最大 1 秒待つ (serve loop の外。fs が応答しない時もそれで daemon は終わり、残りの行は失われる) |
| upgrade (DR-0028) | 新プロセスは同じ path を開き直して fd 2 と logger を向け直す。self-exec の時点で channel に残っていた行は失われる (exec の前に logger を止めない。upgrade 経路のログは失敗の報告だけで、失敗時は旧 serve が続くので logger も続く) |
| fd 2 への直書き | panic の文言など `emit` を通らない stderr への出力も、fd 2 がログファイルなのでそこに入る (O_APPEND の 1 回の write 単位で logger の行と並ぶ) |

## PTY 書き込み effect の完了と失敗

DR-0021 の ack 意味論 (全 byte が master に書けた時だけ `Ok`、IdleTimeout / I/O error / partial は `Error` ack を送ってから当該 client を切断) と、DR-0025 の effect 単位の結果 (`EffectId`、`written_len` / `requested_len`) を、非同期化後も effect 単位で保つ。

1. **完了**: effect の全 byte を master に書けた周回で `EffectResult::TtyWrite { written_len == requested_len }` を feedback し、発行元 client に `Ok` ack を積む
2. **無進捗 deadline 超過**: 先頭 effect が deadline (現行 `MASTER_WRITE_IDLE_TIMEOUT_MS` = 500ms を「最後に進捗した時刻から」で計る) に達したら、その effect の **残余 byte を破棄** し、`written_len` (書けた prefix) と IdleTimeout を feedback する。書けた prefix は取り消せない (子の line discipline に届いている) ので、record と ack の `written_len` にそのまま載せる
3. **I/O error / POLLHUP**: 2 と同じく残余を破棄し、error を feedback する
4. **後続 effect**: 失敗した effect と同じ client から既に queue に積まれている後続の TtyWrite は、**書かずに破棄** し、それぞれ未書込の `Error` ack 相当として扱う (DR-0021 は失敗後に切断するので、後続 bytes を子に届けると「失敗した spec の後ろに後続 spec が続く」順序の嘘になる)。他 client の effect は影響を受けず続行する
5. **切断の境界**: 失敗時は `Error` ack を当該 client の送信 queue に積み、client を「送信 queue が空になったら close」する draining state にする。draining の deadline (現行の detach ack と同じ 200ms を初期値とする) を過ぎたら queue の残りを捨てて close する。この場合 client は ack を受け取れず EOF を観測する (client が読まない場合の現行 `ClientHandle::drop` と同じ結果)。draining 中の client からの受信は処理しない

同一 client の raw_data が、前の effect の完了前に届いた場合に queue に積むか拒否するかは裁定待ち Q7。DR-0021 の client は ack を同期で待ってから次を送るので、正規 client では起きず、起きるのは ack を待たない外部 client の場合に限る。

## kernel 同期 API の扱い

I-1 の 3 に当たるもの。各 API について、serve loop で呼んでよい根拠を書く。

| API | 呼ぶ場所 | ブロックしない根拠 |
|---|---|---|
| `waitpid(WNOHANG \| WUNTRACED \| WCONTINUED)` | SIGCHLD イベント、stopped 中の定期確認 | POSIX `waitpid` (XSH): WNOHANG 指定時、状態が報告可能な子が無ければ呼び出しを中断せず 0 を返す。flag なしの `waitpid` は loop では使わない |
| `kill` / `killpg` 相当 (`kill(-pgid)`) | kill / signal / 終了条件 | POSIX `kill` (XSH) は signal の送信を規定し、受け手の処理完了を待つ意味論を持たない (配送は非同期)。対象が SIGSTOP 中でも送信側は戻る |
| `getpgid` / `kill(pid, 0)` | status | POSIX `getpgid` / `kill` (sig=0 は error check のみで signal を送らない)。プロセス表の参照のみ |
| `ioctl(TIOCSWINSZ)` | resize | tty_ioctl(4) (Linux) / tty(4) (macOS): winsize を設定し、変化があれば foreground process group に SIGWINCH を送る。読み手の応答を待つ操作を含まない |
| `/proc/<pid>/stat` の open / read / close (Linux、`std::fs::read_to_string`) | stopped 中の復帰確認 | fs 経由の IO だが、proc(5) の procfs は read 時に kernel がプロセス表から内容を生成する擬似 fs で、外部の記憶装置・ネットワークに依存しない。I-1 の「kernel 同期 API」として扱う。ただし一般の fs IO と見分けがつかない形なので、呼び出しを procfs 専用の helper に閉じる |
| `/dev/urandom` の open + 16 bytes read (lock token 生成、lock.acquire 時) | lock.acquire | random(4) (Linux): urandom の read は初期化後 block しない。macOS random(4): urandom は random と同じで block しない。open は devfs の character device。I-1 の kernel 同期 API として扱い、`getrandom(2)` / `getentropy(3)` に置き換えられるならそちらを使う (path open を無くせる) |
| `tcsetpgrp` | forkpty 後、exec 前の子プロセス内だけ (`sys/raw.rs`) | serve loop からは呼ばない。子側で呼ぶため loop の不変条件の対象外 |

SIGKILL 後の reap (K-5): 現行は flag なしの `waitpid` で見届ける。D state の子に対しては戻らない。I-1 に合わせ、SIGKILL 後も WNOHANG + deadline で確認し、deadline を過ぎたら reap せずに daemon を終了する (daemon が消えると子は init / launchd に引き取られて回収される)。この場合の exit code の扱いは裁定待ち Q4。

## 固まった daemon の検出

前提として、serve loop が完全に止まると **status.query 自体に応答できない**。heartbeat を status.response に載せても、止まった daemon からは返ってこないので、完全停止の検出には使えない。検出は 2 層に分ける。

1. **CLI 側の分類 (完全停止の検出)**: `hyoui list` / `status` の probe で、観測した事実だけで 3 状態に分ける
   - `connect` が ECONNREFUSED / ENOENT → `stale` (応答する daemon が居ない)
   - `connect` は成功したが、handshake response が deadline 内に来ない → `no-response` (期限内に応答が無かった、という観測の名前)
   - 応答あり → `live`
   `no-response` は「loop が止まっている」の推定を含むが、原因を区別できない: loop の停止、listen backlog の飽和、handshake の遅延 (handshake worker の滞留や高負荷)、接続直後の daemon crash (crash なら直後の再 probe で `stale` に変わる)。表示名は観測事実にとどめ、「止まっている可能性が高い」は help / 説明文に推定として書く。daemon 側の原因は 2 の watchdog ログで確かめる。`no-response` の socket は prune 対象にしない (daemon と子が生きている可能性があるため)。`docs/issue/2026-09-29-list-auto-prune-stale-and-show-hung.md` の表示側と対応する (issue 側の `hung` という名前はこの定義に合わせて読み替える)
2. **daemon 内 watchdog (原因の記録)**: イベントが無い daemon は `poll` の中で待つので、周回カウンタの停滞だけでは停止と正常な idle を区別できない。serve loop は「今どの処理段にいるか」の label と、その段に入った時刻を atomic に書く。label のうち `poll` 待機は特別扱いし、watchdog は **`poll` 以外の段に一定時間 (例: 5 秒) 留まっている場合だけ** 診断として label と経過時間をログに 1 行書く。`poll` 待機中は何時間続いても正常 idle として扱う。watchdog は IO を logger 経由でしか行わないので、それ自体は止まらない
3. **占有の可視化 (遅延の検出)**: status.response に「`poll` 以外の段で費やした 1 周あたりの最大時間」と、その発生時刻・label を載せる (`poll` 待機時間は含めない)。完全停止ではないが数百 ms 占有する経路 (findings の有界な占有) を観測できる

自動 kill は提案しない。daemon を kill すると master fd が閉じて子に SIGHUP が届き、子のセッションごと終わる。loop が止まっても子自身は動き続けており、子を巻き込んで終わらせる必然性は無い (DR-0014 の透過原則)。`no-response` を表示し、ユーザが `kill` するかを判断する。

## 段階移行

実機で再現した経路と、影響が大きい経路から順に進める。段の間の依存は各段に書く (依存を書いていない段は前段と独立に入れられる)。

1. **client 受信の増分 decoder 化** (C-1)。1 client で daemon を止められる経路で、実機再現済み。受信は `recv(MSG_DONTWAIT)` で行い、fd の O_NONBLOCK は付けない (reader と writer が同一 open file description を共有するため、付けると blocking 前提の writer thread と handshake response の `write_all` が EAGAIN で部分送信・切断になる)。handshake 後の client のみ対象にし、handshake の worker は残してよい。1 回の recv に複数 frame が入っても、処理するのは 1 周に 1 client につき 1 frame とし、残りは decoder に置いて次の周回で処理する (frame の間で client の drop (自分の detach、他 client の `detach --target=others`、backpressure 超過) と leader cascade を確定させてから次の frame に進むため。blocking で 1 周 1 frame ずつ読んでいた時と同じ順序になる)
2. **標準エラーの付け替え + logger** (E-1)。起動した呼び出し元の stderr を daemon が持ち続けること自体をやめる。実装済み (2026-10-09、「標準エラーとログ」節)
3. **fs IO の worker 化** (F-1〜F-6)。record の join 撤去が中心
4. **client 送信の loop 内 nonblocking 化と writer thread の廃止** (C-2, C-6, C-8, C-10)。client socket の fd に O_NONBLOCK を付けるのはこの段で、受信の `MSG_DONTWAIT` はこの段で通常の read に戻してよい。handshake response も送信 queue 経由に切り替える (blocking の `write_all` が同じ description に残らないように)。v0.9.55 の `ClientHandle::drop` の timed join + `shutdown(Write)` は、この段までの **暫定の上限** として残す。この段で writer thread ごと無くなり、Drop は close だけになる
5. **PTY 書き込みの非同期化** (P-2)。「PTY 書き込み effect の完了と失敗」節の意味論で入れる。失敗時の draining state が送信 queue を前提にするので段 4 に依存する
6. **handshake の loop 内化、accept の nonblocking 化、sleep の deadline 化、shutdown の state 化** (C-4, C-5, C-9, C-11, P-3, K-4, K-5)。handshake の loop 内化は段 4 の送信 queue に依存する。listener の O_NONBLOCK は accept した socket に継承されない前提 (Linux accept(2)) と継承される実装 (BSD 系) の両方で、段 4 以降なら問題にならない
7. **検出手段** (CLI 側の `no-response` 分類は 1 と並行してよい。watchdog と status の占有指標は 4 以降)

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

## 裁定

2026-10-09 kawaz 裁定:

- **Q1 runtime**: A (`poll` + self-pipe の延長、全 fd nonblocking)。B (mio) は `poll(2)` の fd 走査コストが実測で問題になった時の置き換え先
- **Q2 client 送信**: writer thread を廃止し、loop 内の nonblocking write + `POLLOUT` にする
- **Q3 daemon のログ出力先**: 起動後の fd 2 と logger の書き先は、state dir 配下の session ごとのログファイル。起動失敗の報告は ready 通知の前は呼び出し元の stderr に出す
- **Q8 表示名**: `no-response` (観測事実の名前。「止まっている可能性が高い」は help に推定として書く)
- **段階 1 (client 受信の nonblocking 化)** は着手してよい

## 裁定待ち

- **Q4 reap できない子**: SIGKILL 後 deadline を過ぎても reap できない子を置いて daemon が終了するときの exit code (現行の `128 + 9` とするか、別の値で区別するか)
- **Q5 検出**: 検出節の 3 層 (CLI 側の `no-response` 分類 / watchdog のログ / status の占有指標) のうちどこまで入れるか。CLI 側の分類は必須と考える
- **Q6 固まった IO worker**: fs が応答せず worker が戻らない場合に、worker thread の上限を設けるか (上限到達で `record.start` を拒否する等)、record を aborted として表示するか
- **Q7 PTY 書き込み effect の失敗意味論と後続 raw_data**: 「PTY 書き込み effect の完了と失敗」節の 5 点 (残余破棄、後続 effect の書かずに破棄、draining の deadline 200ms と超過時に client が ack でなく EOF を見ること) をこのまま確定してよいか。あわせて、同一 client の raw_data が前の effect の完了前に来たとき queue に積むか拒否するか。client 間の順序は DR-0022 の auto-lock が保証し、ここで決めるのは同一 client 内の扱い

## Consequences

良い影響:

- どの外部が止まっても SIGCHLD の回収と status 応答が続く。zombie の残留と「生きているが固まる」daemon が構造的に無くなる
- client の受信・送信・handshake が純粋 state になり、socket を立てずに unit test できる範囲が増える
- writer thread と handshake thread が無くなる (Q2)

コスト・リスク:

- client 送受信・PTY 書き込み・shutdown を state machine に書き換える変更範囲が大きい。段階移行の各段で既存テストが通ることを確認しながら進める
- PTY 書き込みの非同期化で、DR-0021 の ack 発行点が「effect 実行直後」から「後続周回の完了イベント」に移る。意味 (PTY drain 完了) は同じだが、ack の順序を検証するテストを先に固める必要がある
- IO worker が固まった場合は thread が残る (Q6)

## Alternatives

- **個別の blocking 呼び出しに timeout を後付けする**: kawaz 裁定で不採用。上限があっても占有は起きる上、既に block している syscall に後から `SO_SNDTIMEO` を設定しても効かない (v0.9.55 の調査で macOS 実測) ように、timeout の効き方が呼び出しごとに違い、網羅を保証できない
- **serve loop を複数 thread に分ける (domain ごと)**: DR-0025 が Alternative D として棄却済み (ordering の決定性を失う)
