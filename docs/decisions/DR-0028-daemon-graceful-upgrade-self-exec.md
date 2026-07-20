# DR-0028: daemon graceful upgrade — self-exec による fd/PID 引き継ぎ

- Status: Active (Draft 起草 2026-07-21、kawaz 方式裁定済み・詳細裁定は Open Questions)
- Date: 2026-07-21
- Related: DR-0025 (message 駆動原則 — upgrade も protocol message として形式化、state の message 形式化が進むほど handoff は単純化), DR-0008 (protocol — 新 kind 追加規約 / cap flag), DR-0013 (screen state 正本 — scrollback bytes 再 feed による再構築の根拠), DR-0017 (session anchor — daemon = session leader + controlling tty、PID 温存の必然の根拠), DR-0016 (record — scrollback/record bytes が再構築の材料), DR-0014 (検証主義 — マトリクス検証要件)
- Origin: kawaz 要望 2026-07-20「再起動したくない。fd も pid も引き継いで新バイナリに exec する感じ」、docs/issue/2026-07-21-daemon-graceful-upgrade-self-exec.md

## Context

hyoui daemon は long-running な子セッション (claude code / vim 等、数日〜数週間走行) を
抱える。現状 daemon の bug fix / 機能追加を届ける手段は daemon 再起動しかなく、それは
子プロセスの喪失 (= daemon が session leader かつ子の親であるため、daemon 死亡 = 子への
SIGHUP 巻き添え、DR-0017 Consequences で実機確認済) を意味する。v1.0 未満で毎リリース
breaking が入る開発期こそ、走行中セッションを壊さず新バイナリへ切り替える手段が要る。

hyoui のプロセスモデル上、引き継ぐべき資源は:

1. **PID そのもの**: daemon は子の親 (SIGCHLD 受信者) かつ session leader (controlling
   tty 保持者、DR-0017)。別プロセスへの引き継ぎは「親子関係の移譲」となるが、UNIX に
   子の再親付け (reparent) API は無い (= `PR_SET_CHILD_SUBREAPER` は孤児の引き取り先
   指定であって任意移譲ではなく、macOS には相当機構すら無い)
2. **PTY master fd**: 子との唯一の I/O 経路
3. **unix socket listener fd**: client 接続の受け口 (パス再 bind でも代替可能だが、
   fd 引き継ぎなら bind 済み状態を保てる)

PID を温存できるのは self-exec (= 同一プロセスが `execve(2)` で新バイナリに置き換わる)
のみ。kawaz 裁定 (2026-07-20) により self-exec 方式で確定。

## Decision

### 1. 全体像: self-exec + fd 温存 + state 最小シリアライズ + 再構築

```
旧 daemon (v_old)                          新 daemon (v_new、同一 PID)
──────────────────                         ──────────────────
1. upgrade.request 受信 (protocol message)
2. 新バイナリパス検証 (存在 / 実行可能)
3. attach client 全員へ切断通知 → close
4. PTY master fd / listener fd の
   CLOEXEC 解除 + fd 番号を env で伝達
5. state を一時ファイルへシリアライズ
   (最小限: §3)
6. execve(new_binary, argv, env)  ────────→ 7. env から fd 番号 / state ファイル
   (失敗時: CLOEXEC 復元 + 旧続行 §5)          パスを検出、upgrade-resume モード起動
                                            8. state 復元 (失敗時: 再構築 §5)
                                            9. screen state を scrollback bytes の
                                               再 feed で再構築 (§3)
                                            10. serve loop 再開、client 再接続受付
```

exec 前後で変わらないもの: PID / PPID / 子との親子関係 / SIGCHLD 経路 / session id /
controlling tty (DR-0017 anchor 構造) / process group / 開いたままの fd (CLOEXEC 解除分)。
kernel が保証するため検証対象ではあっても実装対象ではない (= 最小介入、CLAUDE.md
self-check「kernel 標準機能の再発明をしない」の順方向: kernel に任せる)。

### 2. トリガー: protocol message として形式化 (DR-0025 原則)

- 新 CBOR control kind: `upgrade.request` (client → daemon) / `upgrade.ack`
  (daemon → client、exec 直前の受理応答) / `error` (検証失敗時、既存 error 経路)
- `upgrade.request` payload: `{ "kind": "upgrade.request", "binary_path": <optional text> }`。
  `binary_path` 省略時は daemon 自身の実行ファイルパス (= `argv[0]` 由来ではなく
  `current_exe()` 相当) を再 exec する = 「バイナリを上書き更新してから upgrade を叩く」
  運用が既定
- cap flag: `upgrade-v1` (DR-0008 negotiation に載せる。旧 client は kind を見ない)
- CLI: `hyoui upgrade [session]` (session 省略時解決は DR-0020 規則に従う)。
  全 session 一括は scope 外 (shell loop で足りる、必要になったら別途)
- **自動検知 (バイナリ mtime 監視等) は不採用**: 透過原則 (DR-0005) の観点で daemon が
  勝手に自分を置き換えるのは観測外の介入。明示トリガーのみ
- DR-0025 との整合: upgrade.request は ClientEvent → Serve domain への dispatch として
  写像する (`Client ──→ Serve` は既存許可 edge)。reducer 化完了前の現行実装では
  serve_loop の control handler に置く (= reducer 移行時に Serve domain へ収容)

### 3. state 引き継ぎ: シリアライズ最小主義 + 再 feed 再構築

**方針: 直接シリアライズは「再構築不能なもの」だけに絞る。** breaking 期に state format
の版間互換を維持するコストを最小化するため、可能な限り「新プロセスが一次資料から
再構築」に倒す。

| state | 引き継ぎ方式 | 根拠 |
|---|---|---|
| PTY master fd / listener fd | fd 温存 (CLOEXEC 解除、番号を env `HYOUI_UPGRADE_PTY_FD` / `HYOUI_UPGRADE_LISTENER_FD` で伝達) | kernel 資源、シリアライズ不能 |
| 子 PID / pgid / spawn 時パラメータ | 一時ファイル (シリアライズ) | 再構築不能 (子は既に走行中) |
| session 設定 (namespace / on-child-suspend policy / until 条件 / scrub 設定等) | 一時ファイル | 再構築不能 (起動時引数由来) |
| lock 状態 (holder token) | **引き継がない** (upgrade で全 client 切断 → process-bound GC (DR-0022) と同じ意味論で自動解放) | client 切断 = release が既存意味論 |
| screen state (仮想 screen / scrollback) | **シリアライズしない**。scrollback bytes (DR-0013 byte-base tail/history) を新プロセスの vt parser に再 feed して再構築 | screen state は bytes の純関数 (DR-0013 正本化の帰結)。vt100 内部構造の版間シリアライズは breaking 期に割に合わない |
| record 状態 (DR-0016) | 一時ファイルに record ファイルパス + seq を記録し、新プロセスが追記継続。lifecycle event として upgrade を record に残す | 追記継続に必要な情報は path + seq のみ |
| attach client 接続 | **引き継がない** (切断 → 再接続、§4) | kawaz 裁定済み |

- 一時ファイル形式: versioned (先頭に format version + hyoui version)。置き場所は
  socket dir と同階層 (同一 UID 保護境界内)。復元完了後に削除
- scrollback bytes の再 feed は「新バイナリの parser 実装で解釈し直す」ことを意味する
  = parser の bug fix が過去出力の解釈にも効く副次的利点がある。一方 feed 済み bytes が
  巨大な場合の再 feed コストは Phase 2 で実測する (scrollback 上限は既存機構で bounded)

### 4. attach client の扱い: 切断 → 自動再接続

- daemon は exec 前に接続中 client へ切断通知 (既存 lifecycle 通知経路) を送って close
- attach client 側は「upgrade 起因の切断」を受けたら再接続 retry (短い backoff、上限
  数秒) して再 attach する。再 attach 時の画面復元は既存の attach 時 screen 転送
  (DR-0013) がそのまま働く
- 接続維持したままの seamless upgrade (fd を client socket ごと引き継ぐ) は**不採用**:
  wire protocol 自体が breaking する期間に版跨ぎ接続を維持しても、直後の frame で
  非互換が露呈する。切断 → 新版 client で再接続の方が誠実 (issue 起票時の kawaz 判断
  を追認)

### 5. 失敗時安全性 (fail-safe の 2 段)

1. **execve 失敗** (バイナリ消失 / 権限 / フォーマット不正): execve は失敗時に呼び出し元へ
   return するので、旧プロセスがそのまま継続する。CLOEXEC を復元し、一時ファイルを削除し、
   トリガー元へ error 応答 (client は upgrade.ack 受信済みのため切断されている場合は
   ログ + 再 attach 時の status で気づける形にする)。exec 前の検証 (存在 / 実行 bit /
   同一 UID 所有) で大半は事前に弾く
2. **新プロセス側の state 復元失敗** (一時ファイル破損 / format version ミスマッチ):
   シリアライズ対象を捨てて再構築フォールバック — fd は env から回収できるので serve は
   継続できる。子 PID は `tcgetpgrp(master_fd)` + 子の存在確認で再発見を試みる。
   screen は再 feed (§3) がそもそも再構築なので影響なし。復元失敗の事実は record
   lifecycle event + status に残す (= 黙って劣化しない)
3. **exec 後の即死** (新バイナリが起動即 panic): これは防げない (子は orphan 化して
   SIGHUP 経路へ)。緩和策として `hyoui upgrade` client 側で exec 後の再接続確認まで
   行い、失敗したら明確に報告する。「upgrade 前に新バイナリで `hyoui --version` が
   走ることを client 側で確認」を CLI 実装に含める

cache-warden (github.com/kawaz/cache-warden、DR-0029) の graceful restart は同系の
exec + fd 渡し実装の参考先: 「prepare (失敗しても安全) と execute (point of no return)
の 2 相分離」「argv 継承」「exec target の事前検証」のパターンは本実装でも踏襲する。
一方 cache-warden の署名同一性検証 (codesign / 末尾追記署名) は secret store 特有の
脅威モデル由来であり、hyoui (同一 UID 保護境界、secret を持たない) では過剰として
採らない。fork した state-holder child 経由の socketpair 渡しも、hyoui は PID 温存が
要件のため構造が異なり採らない (一時ファイルで足りる)。

## Rejected alternatives

- **fork + handoff (新プロセスを fork/spawn して fd を渡す)**: 子の再親付けが不可能
  (Linux subreaper は孤児引き取りのみ、macOS は相当機構なし)。SIGCHLD 経路と
  controlling tty (DR-0017 anchor) が移譲できず、hyoui のプロセスモデルの根幹が壊れる。
  cache-warden がこの系を採れたのは「子プロセスを持たない」から
- **外部 supervisor 方式 (fd を supervisor が保持し daemon を使い捨てる)**: supervisor
  という常駐プロセスの追加 = hyoui のプロセスモデル (1 session = 1 daemon) を複雑化。
  supervisor 自身の upgrade 問題が再帰する。PID 温存も結局できない
- **再起動運用のまま (upgrade しない)**: 長寿命セッションの喪失コストが毎リリース発生。
  ドッグフーディング (走行中 claude セッション複数) と両立しない
- **client 接続維持の seamless upgrade**: §4 の通り。protocol 安定後 (v1.0+) に必要なら
  cap flag 追加で再検討可能な拡張点として残る
- **screen state の完全シリアライズ**: vt100 crate 内部構造の版間互換を breaking 期に
  維持するのは高コストで、DR-0013「screen state は bytes から再構成可能な正本」の設計
  帰結 (再 feed) と二重投資になる

## 検証要件 (DR-0014 マトリクス)

3 category (TUI alt screen 系 = vim or claude / line-oriented 系 = cat / REPL 系 =
python or bash) × 以下の観点で全セルを実機で埋める:

| 観点 | 期待 |
|---|---|
| 子プロセス無事 | upgrade 前後で子 PID / pgid / stat 不変 (`ps -o pid,ppid,pgid,sid,stat`)、子は入出力継続可能 |
| SIGCHLD 経路 | upgrade 後に子を exit させ、新 daemon が reap + shutdown 連鎖することを確認 |
| screen 継続 | upgrade 前後の `hyoui screen dump` が一致 (alt screen 状態 / cursor / mode 含む、`--include=Cells,Cursor,Mode`) |
| attach 再接続 | attach 中に upgrade → 自動再接続 → 画面表示・入力が回復 |
| 入力 race | upgrade 直前に送った input が失われないか (DR-0021 ack との整合: ack 前 bytes の扱いを記録) |
| exec 失敗 | 存在しないパス指定 → 旧 daemon 続行 + error 応答 |
| state 破損 | 一時ファイルを故意に壊して再構築フォールバック動作 + record への痕跡を確認 |
| 版ミスマッチ | format version 不一致で再構築フォールバックに入ることを確認 |

加えて suspend 中の子 (stopped 状態、DR-0017/0019) を跨ぐ upgrade で stopped が保存
されることを 1 case 確認する (state 温存の境界ケース)。

## Implementation phases

- **Phase 1 — self-exec 骨格 PoC**: CLOEXEC 解除 + env fd 伝達 + execve + 新プロセス側
  fd 回収の最小ループを PoC binary (または `--upgrade-resume` 隠し経路) で実証。
  gate: PTY 越しの子と listener が exec 跨ぎで生きていることを 3 category で確認
- **Phase 2 — state 引き継ぎ**: 一時ファイル format (versioned) + 子 PID / session 設定 /
  record 継続のシリアライズ・復元 + scrollback 再 feed。gate: 検証マトリクスの
  screen 継続 / state 破損 / 版ミスマッチ行が green、再 feed コスト実測
- **Phase 3 — protocol / CLI 整備**: `upgrade.request` / `upgrade.ack` kind + cap
  `upgrade-v1` + `hyoui upgrade` subcommand + client 側再接続 retry + 事前検証
  (`--version` 実行確認)。gate: マトリクス全セル green + ドッグフーディング
  (走行中 claude セッションで実運用 upgrade)

DR-0025 reducer 化と並走するが依存しない: Phase 1/2 は現行 serve_loop 構造で実装可能。
reducer 化が進んだ時点で upgrade 処理は Serve domain reducer + Effect (Exec は
point of no return なので Effect 化の例外扱いになる見込み、その整理は DR-0025 側の
Phase に委ねる)。

## Open Questions

- **UPG-Q1: 一時ファイルの format**: (a) CBOR (protocol と共通の語彙、既存依存のみ) /
  (b) JSON (人間可読、デバッグ容易)。**推し: (a)** — 依存追加なし + protocol と同じ
  encode 基盤。破損調査は record 側の lifecycle event で足りる
- **UPG-Q2: upgrade 中の新規 client 接続**: listener fd は生きたままなので exec の
  数十 ms 間に accept 待ち client が来うる。(a) 気にしない (backlog が吸収、新プロセス
  が accept) / (b) exec 前に一時的に accept 停止。**推し: (a)** — backlog で足り、
  介入最小
- **UPG-Q3: ack 前 pending input の扱い** (検証マトリクス「入力 race」の結果次第):
  (a) upgrade.request 受理後は新規 raw_data を reject して drain 完了を待ってから exec /
  (b) 気にしない (lock (DR-0022) を upgrade が取得する運用で回避)。**推し: (a)** —
  DR-0021 の「完了点 = master fd write の return」意味論を upgrade 跨ぎでも保存できる
