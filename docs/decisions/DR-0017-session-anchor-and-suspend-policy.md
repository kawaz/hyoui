# DR-0017: session anchor 化と suspend policy 改訂 — TUI の Ctrl-Z を本来のセマンティクスで動かす

- Status: Active
- Date: 2026-06-10
- Related: DR-0003 (forkpty + login_tty — 本 DR で「子を session leader にする」部分を supersede), DR-0005 (思想 — daemon の役割に session anchor 兼任を追記), DR-0001 (jobcontrol 軸 1 — auto-resume fallback を改訂), DR-0014 (検証主義 — 実機マトリクス要件), DR-0015 (fork daemon + attach client 構成 — 本 DR の前提構造)

## Context

`hyoui run <TUI>` で起動した子 (claude / vim / less 等) の **Ctrl-Z が 1 度目すら効かない** bug が
3 件報告されていた (正本: [docs/issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md](../issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md))。
2026-06-10 の実機検証で、その根本原因が **二層構造**であることが確定した
(検証正本: [docs/findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md](../findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md))。

- **層 1 (= kernel 仕様)**: `forkpty(3)` + `login_tty(3)` (= 内部で `setsid`) により child が
  **独立 session leader** になる (DR-0003 採用構造)。daemon は別 session (daemonize 時に
  `setsid` 済) のため、child の process group は POSIX が定義する **orphaned process group** に
  該当する。orphan pgrp では SIGTSTP / SIGTTIN / SIGTTOU の action が SIG_DFL だと
  **kernel が当該 signal を discard する**。よって line discipline が Ctrl-Z byte (0x1a) から
  生成した SIGTSTP も、外部 `kill -TSTP` も握り潰され、child は止まらない。
  cat / less / vim / python REPL / bash の 5 カテゴリ全てで同症状を観測 (= app 固有処理は無関係、
  構造問題で確定)。

- **層 2 (= hyoui 実装)**: daemon の **auto-resume が、止まった子を即 SIGCONT で起こす**。
  `session.rs` の DR-0001 軸 1 実装には、leader 不在時 fallback として無条件
  `killpg(child, SIGCONT)` がある。`kill -STOP` (= discard 不可、層 1 をすり抜ける) を送っても
  子が止まったままにならず、daemon を先に SIGSTOP した場合のみ子が `Ts+` を維持することで
  層 2 の存在を直接証明した。層 1 を回避しても層 2 が起こすため、両層の対処が必須。

session anchor 構造の妥当性は **macOS / Linux glibc / Linux musl の 3 platform PoC で実証済**
(findings の検証マトリクス参照)。`tmux` も同一の orphan 制限を放置していることを実証した
(`tmux new -d -- sleep 600` の sleep は止められない)。tmux ユーザが困らないのは典型ユースが
shell 起動で、shell が job control を肩代わりするため。逆に言えば本修正を入れれば
**「直接起動した子の ^Z が効く PTY ラッパー」は hyoui の差別化点になる**。

## Decision

二層原因に対応するため、以下の 2 本柱で修正する。

### 柱 1: session anchor 化 (= 層 1 対応)

`forkpty(3)` の利用をやめ、`openpty` + 手動 fork に変更する。daemon を child の session の
**anchor** とすることで、child の process group を orphan でなくする。

- daemon (= daemonize 時点で `setsid` 済 = session leader かつ controlling tty 無し) が、
  **PTY slave を `TIOCSCTTY` で自分の controlling tty にする**。これにより daemon が
  この session の anchor になる。
- child は fork 後に `setpgid(0, 0)` で **同 session 内の新 pgrp** になり、`tcsetpgrp(slave, getpid())`
  で foreground 化、slave を std fd (0/1/2) に dup2 して exec する
  (= `login_tty` のうち `setsid` を行わない形)。
- 親 (= daemon) 側でも race 対策として `setpgid(child, child)` + `SIGTTOU` を ignore した上で
  `tcsetpgrp(slave, child)` を行う (= PoC で実証した構造)。
- 結果、child の pgrp は「**同 session 内に別 pgrp の親 (daemon) がいる**」状態になり、
  もはや orphan ではない。SIGTSTP は本来のセマンティクス (= catch 可能、TUI 自前の
  suspend cleanup = alt screen 解除・termios 復元が走る) で動作する。

**透過性の再解釈**: zsh から直接コマンドを起動した場合、その子は session leader **ではない**
(= shell の session のメンバー)。現行の「child = session leader」構造の方がむしろ直接起動と
異なる。session anchor 化は「外側 shell がやっている構造の再現」であり、**透過性の向上**である。

**daemon 側の規律**:
- daemon は slave を read/write しない (= 子との I/O は master 経由のみ)。
- daemon が slave に対し `tcsetattr` / `tcsetpgrp` を呼ぶ際は **SIGTTOU を ignore** する
  (= daemon は background pgrp になり得るため、これがないと自分が止まる)。

### 柱 2: suspend policy の改訂 (= 層 2 対応、DR-0001 軸 1 の改訂)

子の Stopped 観測時の **「leader 不在 fallback = 無条件 `killpg(child, SIGCONT)`」を廃止する**。

- 理由: ユーザ / 端末起因の stop (SIGTSTP / SIGSTOP) は **意図的な操作**であり、それを勝手に
  起こすのは介入である (DR-0005 透過原則に反する)。detached で子が stop したままでも、外側 API
  (`hyoui kill --signal=CONT` 等) で起こせるため、「**誰も起こせない**」状況は構造的に存在しない。
- `SessionChildStoppedNotify` は**全 rw client** (= `Rw` / `RwNoLeader`、cap `child-state-v1`
  保持者) に broadcast する。ro client は「見に来ただけ」なので通知対象外。旧実装は
  leader 1 client にのみ通知していたが、非 leader rw client が child stop を検知できず
  画面固着する bug (2026-07-24 実測確定、docs/issue/2026-07-24-bug-tstp-intercept-followups.md
  H3) を修正するため broadcast 化した。

  > **📌 撤回注記 (2026-07-25、[[DR-0029]] §1 により)**: 「各 rw client が follow
  > (`raise(SIGSTOP)`) して外側 shell に suspend を伝播する」(= DR-0015 §2.2) は撤回した。
  > attach は覗き窓であり、子が止まっても client は止まらず attach を継続する。
  > notify は follow の trigger ではなく「子が停止中」の画面通知に使う。
- `OnChildSuspend::AutoResume` policy 自体は **opt-in 設定として残してよい** (= headless 用途等)。
  ただし **default は notify のみ** (= 勝手に起こさない) とする。
- `list` / `status` で stopped 状態が見えることを要件とする (= 放置された stopped child の
  可観測性。auto-resume をやめると stopped のまま残り得るため、観測手段は必須)。

## Rejected alternatives

### 案 A4: attach が 0x1a を intercept → daemon が `killpg(SIGSTOP)`

issue が当初推奨し、層 2 未発見時点では「唯一の実用解」とされていた案。

却下理由:
- SIGSTOP は **catch 不可**のため、TUI の suspend cleanup (= alt screen 解除・termios 復元) が
  走らず、画面が **raw のまま凍結**する。SIGTSTP セマンティクスの喪失。
- Ctrl-Z byte を attach 側で解釈する = **in-band byte 解釈の介入**でもあり、透過原則と相性が悪い。

session anchor 案はこの弱点を持たない (= SIGTSTP 本来の経路で cleanup が走る) ため上位。
A4 は session anchor 案に未知の障害が出た場合の fallback としてのみ温存する。

### 案: supervisor 分離 (= forkpty の中に anchor プロセスを挟む)

新 session 内に anchor 専用プロセス (supervisor) を立て、その配下で child を動かす案。

却下理由:
- supervisor (= session leader) が `kill -9` されると、kernel が foreground pgrp に SIGHUP を
  送り **child が巻き添え死する**新故障モードが生じる。
- child の pid / exit code を supervisor 経由で中継する実装が増える。

session anchor 案 (= daemon が anchor を兼任) はプロセスを追加せず、この故障モードも持たない。

> **📌 訂正 (2026-06-11、実機検証により)**: 「この故障モードも持たない」は誤りだった。
> anchor 案でも daemon が session leader + controlling process である以上、daemon の
> `kill -9` / `kill -TERM` 死で foreground pgrp (= child) に POSIX 規定の SIGHUP が配送され
> **child は巻き添え死する** (hyoui 0.6.1 / macOS 26.5 で vim・cat・python3 の 3 カテゴリ
> 全滅 + trap での SIGHUP 受信を直接観測)。supervisor 案に対する anchor 案の優位は
> 「親死亡時の子保護」ではなく「プロセス追加なし・pid/exit code 中継不要」にある。
> 詳細: docs/findings/2026-06-11-signal-suspend-interaction-audit.md §実機検証。

### 案: 割り切り (= docs 明記 + 外側 API 誘導)

「TUI の ^Z は仕様上効かない」と docs に明記し、`hyoui kill -s STOP` 等の外側 API に寄せる案。

却下理由:
- 主用途 (= claude TUI の対話的 suspend、DR-0002 命名議論の核) の UX を放棄することになる。
- tmux と同レベルに留まり、差別化点を自ら捨てる。

## Consequences

- **DR-0003 の supersede (部分)**: forkpty + login_tty 判断のうち「**child を `setsid` + session
  leader にする**」部分を supersede し、`openpty` + 手動 fork へ移行する。forkpty を選んだ他の
  理由 (= raw fd 管理、PTY 制御端末の確実な獲得等) は影響を受けない。
- **DR-0005 の思想改訂**: 「daemon は外側の観測者」に **「daemon は child の session の anchor を
  兼ねる」**を追記する。daemon が controlling tty を保持し slave に `tcsetpgrp` / `tcsetattr` を
  行うことは、本 DR で justify された必要最小限の介入である。
- **DR-0001 軸 1 の改訂**: leader 不在時の auto-resume fallback を廃止 (= 柱 2)。
- **制約 = 1 daemon 1 session モデル前提**: controlling tty は 1 プロセスにつき 1 個しか持てない
  ため、daemon が anchor を兼任する構造は「1 daemon = 1 session」を前提とする (= 現行モデル通り)。
  将来 1 プロセスで N session を扱う必要が出たら、supervisor 分離案に切り替える。
- **breaking change**: child のプロセス構造が変わる (= session leader でなくなる、`ps` の
  session / pgrp 表示が変わる)。v0.x のため minor bump で許容 (= breaking change OK 方針)。
- **実装後の検証要件 (= DR-0014 流マトリクス)**: `TIOCSCTTY` の platform 差は 3 platform PoC で
  検証済だが、実装後に **cat / less / vim / python / bash × ^Z 1 回目/2 回目 × fg 復帰** の
  実機マトリクスを埋め、期待 vs 実態の乖離がないことを確認すること。
- **親死亡 → child SIGHUP 巻き添え (2026-06-11 実機確認)**: daemon が session leader +
  controlling process である構造の必然的帰結として、daemon の異常死 (`kill -9` / `kill -TERM`)
  で foreground pgrp に SIGHUP が配送され、SIGHUP を trap しない child は巻き添え死する
  (vim / cat / python3 の 3 カテゴリ全滅 + trap での SIGHUP 受信を直接観測、hyoui 0.6.1 /
  macOS 26.5)。anchor 案の優位は「親死亡時の子保護」ではなく「プロセス追加なし・pid/exit code
  中継不要・PTY 正本化」にある (= §Rejected の訂正注記参照)。daemon 即死時は socket 残骸も
  残る (graceful shutdown 不在)。堅牢化の検討は
  docs/issue/2026-06-11-bug-daemon-signal-robustness.md に切り出し。

## 関連

- [[DR-0003]] — forkpty + login_tty (= 本 DR で「子を session leader にする」部分を supersede)
- [[DR-0005]] — 思想 (= daemon の役割に session anchor 兼任を追記)
- [[DR-0001]] — jobcontrol 軸 1 (= auto-resume fallback 廃止)
- [[DR-0014]] — 検証主義 (= 実機マトリクス要件)
- [[DR-0015]] — fork daemon + attach client 構成 (= 本 DR の前提構造、`SessionChildStoppedNotify` 経路)
- [docs/issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md](../issue/2026-05-29-bug-claude-tui-ctrl-z-not-stopping.md) — bug 正本
- [docs/findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md](../findings/2026-06-10-ctrl-z-two-layer-cause-and-session-anchor-poc.md) — 二層原因の確定 + 3 platform PoC
