# DR-0001: bg/fg ジョブ制御の 2 軸設計と invariant

- Status: Active (= 軸 1 follow/auto-resume のみ。軸 2 transparent/decouple は [[DR-0015]] で廃止 2026-05-28)
- Date: 2026-05-21 (= 初版) / 2026-05-28 (= 軸 2 廃止)
- Related: docs/journal/2026-05-21-bootstrap.md, DR-0002 (ネーミング — 「一心同体」概念がこの設計と地続き), DR-0015 (= `hyoui run` の fork 化に伴い軸 2 廃止 + 軸 1 の実装方式変更)

## Update (2026-05-28): 軸 2 廃止 + 軸 1 実装方式変更 (= [[DR-0015]])

[[DR-0015]] で `hyoui run` を「fork daemon + attach client」の合成に再定義した結果:

- **軸 2 (= 親 hyoui 外部 SIGTSTP 経路) 全廃止**: 新構成では daemon は別 process で常駐し、
  「親 client process が外部 SIGTSTP で止まる」シナリオは単にその client 1 個が止まる
  だけで子プロセスは無関係 (= 複数 attach 同時可能なので、1 client の生死が子に
  波及するのは非対称・矛盾)。`OnParentSuspend` enum / `--on-parent-suspend` flag /
  関連 handler 全削除。

- **軸 1 (= 子 self-stop 経路) は維持、ただし実装方式が変更**:
  旧版では daemon thread が SIGCHLD/SIGTSTP/SIGCONT を捕まえて同プロセス内 main thread
  と協調していた。新版では daemon process が `waitpid(WUNTRACED)` で子 stopped を
  観測 → `session.child.stopped.notify` を leader (= attach client) に送信 →
  leader が follow/auto-resume policy を発動する。詳細は [[DR-0015]] §2.2。

- **invariant の縮小**: 旧「親が死ねば子も、子が死ねば親も」(= 一蓮托生) は廃止。
  新版は **「子が死ねば daemon + 全 client exit」のみ片方向**。親 attach client が
  死んでも子は無関係 ([[DR-0015]] §2.3.1)。

以下、本 DR 本文は **historical reference** として保全。現行仕様は [[DR-0015]] が正本。

## Context

hyoui は任意コマンドを PTY 内で実行する。子プロセスは `POSIX_SPAWN_SETSID` で
**独立セッションのリーダー**になる（PTY を制御端末として持つために必須）。
このため、ジョブ制御は 2 層に分かれる:

| 層 | 制御端末 | Ctrl-Z の宛先 |
|---|---|---|
| 外側: hyoui 自身のジョブ | hyoui を起動した実 tty（あれば） | hyoui のプロセスグループ |
| 内側: 子のジョブ | PTY slave | PTY 経由で子の前景 pgrp |

**内側は既に正しい**: interactive モードでは hyoui が実 tty を raw 化するため、
キーボードの `^Z`(0x1a) は実 tty のラインディシプリンで SIGTSTP に変換されず
バイトのまま PTY へ流れ、PTY（cooked）のラインディシプリンが子に SIGTSTP を送る。
つまり Ctrl-Z は内側の子が消費し、hyoui 自身が止まるのは明示的な
`kill -TSTP` / `bg` のときだけ。

**問題は次の 2 つ:**

1. **子が PTY 内で自分を suspend したとき** — 内側にシェルも人間もいないと、
   誰も `fg` できず永久ハングする。poc3 時代（poc3 は `$SHELL` 固定起動だった）、
   その PTY 内のシェルで `exec claude` すると PTY 内にシェルが居なくなり、
   claude が自分を suspend した時に復帰も終了も不能になる問題があった。当時の
   shimux はこれを、その小問題専用に作った **内製の別 cmd `nosuspend`**
   （`shimux/cmd/nosuspend/`、自前 `main` を持つ別バイナリ。外部 OSS ではなく
   shimux リポ内の別 PoC 的な小ツール。`waitpid(WUNTRACED)` → 即 `SIGCONT`）で
   対処していた。hyoui ではこの挙動を別 cmd としては移植せず、軸 1 の
   `auto-resume`（後述）として **本体に統合** した。
2. **hyoui 自身が外部から suspend されたとき** — 子は独立セッションなので連動せず、
   走り続ける（PTY バッファ満杯で結果的に詰まる）。透過的でない。

当初は「子が suspend したとき」「親が suspend したとき」の 2 軸 4 組合せを
個別に検討したが、不変条件を 1 つ導入すると整理できることが分かった。

## Decision

### invariant（不変条件）

> **親 hyoui が走行中（stopped でない）なら、子も走行中である。**

許される状態は 3 つ: 〔親 fg・子 fg〕〔親 stop・子 fg〕〔親 stop・子 stop〕。
禁則は **〔親 fg・子 stop〕** のみ。これは「親が fg なのに子だけ止まったまま」で、
外部制御（observer 的にツール側が意図的に子を止める）を導入しない限り実用上発生しない。

invariant の回復ルールは **SIGCONT ハンドラに集約**: 親が再開した時、子が STOPPED なら
必ず `SIGCONT` を送る。これで禁則状態が論理的に発生しない。

### 軸 1 — `--on-child-suspend=follow|auto-resume`

子が PTY 内で自分を suspend したときの親の挙動:

- **`auto-resume`**: 親が子（pgrp）へ即 `SIGCONT`。子の suspend を一切許さない。
  poc3 時代の `nosuspend` 相当を内蔵したもの。
- **`follow`**: 親も自分に `SIGSTOP` を raise（親も停止 → 両者停止）。
  hyoui を起動した外側シェルに制御が戻り、外で作業して `fg` で hyoui ごと
  再開すれば、SIGCONT ハンドラの invariant 回復で子も復帰する。
  「Ctrl-Z x2 で claude を bg にして外側シェルで作業 → fg で戻る」体験を成立させる。

### 軸 2 — `--on-parent-suspend=transparent|decouple`

hyoui 自身が外部（`kill -TSTP` / `bg`）から suspend されたときの子の挙動:

- **`transparent`**: 子 pgrp へ `SIGSTOP` を送ってから親も `SIGSTOP` を raise。
  親 resume 時に子も `SIGCONT`。両者がペアで動く。
- **`decouple`**: 親のみ停止、子はそのまま走り続ける。

### デフォルト（モード別 preset）

| モード | 軸 1 | 軸 2 |
|---|---|---|
| `interactive`（既定） | `follow` | `transparent` |
| `headless` | `auto-resume` | `decouple` |

`--mode=headless` が preset として suspend 系の既定を切り替える。
`--on-child-suspend` / `--on-parent-suspend` が明示指定されていればそちらを優先。

#### デフォルト判断の根拠

- **interactive**: 人間が TUI を操作する文脈。Ctrl-Z x2 で内側プロセスを bg にして
  外側シェルへ戻る体験を無フラグで成立させたい → `follow`。外部 `kill -TSTP` でも
  ペアで止まる方が再開時の状態がクリーン → `transparent`。
- **headless**: 人間も内側シェルもいない。子が自分を suspend したら永久ハング =
  headless ランナーとして致命的 → `auto-resume` 必須。親が外部から止められても
  子を走らせ続けたい（バッチがハングしない）→ `decouple`。

## 不採用にした案・判断の修正

- **軸 1 の `follow` を一度「実用価値が薄い」と却下したのは誤り**だった。
  headless バッチ用途だけを念頭に置いた判断ミスで、「Ctrl-Z x2 で claude を bg にして
  外側シェルに戻る」という明確なユースケースを見落としていた。議論の中で復活させた。
- **外部制御チャンネル経由の pause/resume**（socket で `pause`/`resume` コマンド）は、
  invariant を破る「観察モード」（親 fg のまま子だけ止めて状態を読む）に必要だが、
  初期スコープ外とした。停止条件としての socket 外部制御も初期実装に含めない。
- **follow-child（子の suspend で親も止まりリソース解放）を独立モードにする案**は、
  軸 1 の `follow` がほぼ同じ効果を持つため別モードにはしなかった。

## 実装ノート

- **self-pipe trick**: シグナルハンドラは self-pipe に signum を 1 バイト書くだけ。
  poll ループが self-pipe の read 端を監視 fd に加え、シグナルを同期的に処理する。
  async-signal-safety と EINTR レースを避ける。
- **SIGCHLD**: `waitpid(WNOHANG|WUNTRACED)` で EXITED / SIGNALED / STOPPED を判定。
- **`io_poll` の EINTR**: シグナル割り込みでループを即抜けないよう、EINTR を
  致命エラー扱いしない（`HYOUI_POLL_EINTR` で区別）。
- 子は独立セッションリーダーなので **子の pgid == 子の pid**。
  グループ送信は `killpg(child_pid, sig)`。
- INT / TERM / QUIT / HUP は子 pgrp にリレー。WINCH は PTY リサイズ。
- **termios 復元** (= 2026-05-28 追加、2026-05-28 DR-0015 で再設計): hyoui が STOPPED に入る際、
  外側 TTY が raw mode のままだと親 multiplexer (cmux / tmux / libghostty 等) が freeze する。
  対策として:
  - `TtyGuard::suspend()` (= saved termios 適用) / `resume()` (= raw 再適用) を `sys/tty.rs` に追加
  - **現行設計 (= DR-0015 Phase B-3 以降)**: attach client process が独自に sigaction install +
    self-pipe signal monitor thread を起動。signal thread が SIGTSTP byte 受信時に
    `TtyGuard.suspend()` → install_default(SIGTSTP) + raise(SIGTSTP) → 復帰時に
    re-register + `TtyGuard.resume()` を実行。`Arc<Mutex<TtyGuard>>` で thread 共有
  - 旧設計 (= DR-0015 以前、historical) は `DaemonConfig::on_suspend` callback inject だったが、
    1 プロセス 2 thread モデル前提だったため DR-0015 で廃止
  - 詳細は [[DR-0015]] §2.3 を参照

## 仕様の限界

- `SIGSTOP`（catch 不可）が hyoui 自身に直接送られた場合、子へのリレーはできない
  （ハンドラが走らないため）。ただし hyoui が `SIGCONT` で再開した瞬間に
  SIGCONT ハンドラの invariant 回復ルールが子の状態を確認するため、復帰後は整合する。
- 子のセッション内の孫プロセスの suspend は hyoui から不可視（子＝シェルなら子が処理）。
