# DR-0019: run オプション棚卸しと suspend policy の配置 — `--mode` preset 廃止、auto-resume の daemon 配線

- Status: Active
- Date: 2026-06-11
- Related: DR-0001 (jobcontrol 2 軸 — 本 DR で preset 表を partially supersede), DR-0005 (思想 — pipe-through の透過性回復を justify), DR-0006 (CLI ground rules — `--exclusive` / `--detach-others` の原典), DR-0014 (検証主義 — silent no-op 禁止の根拠), DR-0015 (run = fork + exec attach — 軸 2 廃止、§2.2 の policy 配置を本 DR で変更), DR-0017 (notify-only default + AutoResume opt-in 温存 — 本 DR がその唯一実現可能な配置を確定)
- Origin: docs/findings/2026-06-11-signal-suspend-interaction-audit.md (2 系統監査の正本)

## Context

kawaz ドッグフーディング初日 (2026-06-11) に `hyoui run --mode=headless -- claude` が
no-op であることが発覚した (= parse されるが消費箇所ゼロ)。
これを発端に Fable + Codex の独立 2 系統で signal / suspend 相互作用と run/attach
オプションの配線を静的監査した
(正本: [docs/findings/2026-06-11-signal-suspend-interaction-audit.md](../findings/2026-06-11-signal-suspend-interaction-audit.md))。
判明した構造問題:

- **`run --mode=interactive|headless` は「配線漏れ」ではなく「配線すべき仕様が現存しない」**。
  DR-0001 の preset 表は 軸 1 (= on-child-suspend) と 軸 2 (= on-parent-suspend) の
  default 切替だったが、軸 2 は [[DR-0015]] §2.3 で廃止、軸 1 の default は [[DR-0017]] 柱 2
  で notify-only に統一済。preset の参照先が両方消滅し、中身が空になっている。
- **`run --on-child-suspend=follow|auto-resume` は到達不能**。exec attach に伝搬されず、
  HandshakeRequest に field も無く、client は follow をハードコードしている。
  [[DR-0017]] が「`OnChildSuspend::AutoResume` は opt-in 設定として残してよい」と
  明記したにも関わらず、AutoResume を発動できる経路が 1 つも無い。
- **`run --timeout` / `--idle-timeout` は DaemonConfig に field すら無い** no-op。
  一方 `--until` は DaemonizeInit → DaemonConfig → UntilWatcher で配線済 (= daemon 側
  終了条件の唯一の正しい先例)。
- **pipe-through が未配線**。`echo "1+2" | hyoui run -- bc` は stdin EOF で client が
  切断するだけで bc が daemon 配下に残る。`StdinEofAction::SendEof` は実装済だが
  production call site ゼロの dead code。
- **`attach --exclusive` / `--detach-others` は dead field** (= wire に乗るが daemon 側に
  読むコード無し)、**`--on-parent-suspend` は help 残骸** (= parser から削除済なのに
  usage に記載が残り、指定するとエラーになるオプションが help に載っている)。

silent no-op の放置は「公開機能が動かない」だけでなく、[[DR-0014]] の B 方向チェック
(設計 → 実装エビデンス確認) 違反の温床になる。本 DR で run/attach オプションの
棚卸しと、suspend policy / 終了条件 / stdin EOF の「発動者の配置」を確定する。

## Decision

### 1. `run --mode=interactive|headless` を削除する (= `pub enum Mode` ごと)

[[DR-0001]] の preset 表 (= interactive: follow + transparent / headless: auto-resume +
decouple) を **partially supersede** し、`--mode` flag と `Mode` enum を削除する。

理由:

- preset が切り替えていた 2 軸のうち、軸 2 は [[DR-0015]] §2.3 で廃止済、軸 1 の default
  は [[DR-0017]] 柱 2 で notify-only に統一済。**preset の中身が空** (= 切り替える対象が無い)。
- 「headless = attach しない / 仮想サイズ」の再定義も不採用。
  attach しない起動は `--detached` の責務、仮想画面サイズは `--size` / `--cols` / `--rows`、
  suspend policy は `--on-child-suspend` (§3)、pipe 入力は `--stdin-eof` (§5) と、
  **全て直交フラグで表現可能**。preset enum を温存すると直交フラグとの優先順位 matrix
  を背負うだけで益が無い。
- 副次効果: `attach --mode=rw|ro|rw-no-leader` / kill・lock 系の `--mode` (`LockMode`) と
  「mode」の多重定義が解消される (= `--mode` は今後 attach の rw/ro 系統のみ)。

### 2. follow はオプション化しない (= client ローカルのハードコード維持)

「子の stop に追従して attach client 自身も SIGSTOP し、外側 shell に制御を返す」
follow 挙動は、**attach 中の人間にとってこれ以外の合理挙動が無い**
(= 子が止まったのに client が raw mode で画面を掴み続けるのは凍結と区別不能)。
client ローカルのハードコードが正しい配置であり、`attach --on-child-suspend` のような
client 側 flag は作らない。

### 3. auto-resume は daemon の責務として配線する — `run --on-child-suspend=notify|auto-resume`

[[DR-0017]] 柱 2 が温存を認めた `OnChildSuspend::AutoResume` の **唯一実現可能な配置**
として、policy を daemon 側に置く:

- CLI: `run --on-child-suspend=notify|auto-resume` (default `notify`)。
  旧値 `follow` は `notify` に rename する (= daemon 視点では「leader に通知する」が
  正確な語。follow するかどうかは通知を受けた client の挙動 = §2 であり、daemon の
  policy 名に client の動詞を使うのは不正確)。
- 経路: `RunConfig` → `DaemonizeInit` (= `HYOUI_DAEMONIZE_INIT` JSON、[[DR-0018]] の
  namespace と同じ既存 env 伝搬経路に field 1 個) → `DaemonConfig` → daemon の
  child stopped 処理。**新 protocol message / cap flag は不要**。
- daemon の child stopped 観測時:
  - `notify` (default): 現行通り leader (cap `child-state-v1`) に
    `SessionChildStoppedNotify` を送る。介入しない ([[DR-0017]] 柱 2)。
  - `auto-resume`: `killpg(child_pgid, SIGCONT)` で即復帰させる。
    **その際 `SessionChildStoppedNotify` は送らない**。送ると leader client が follow で
    `raise(SIGSTOP)` した直後に子だけが復帰し、client が外側 shell に suspend された
    まま置き去りになる race が生じる (= 子は走っているのに観戦者が止まっている逆転状態)。
    notify 抑止により stopped → resumed が daemon 内で完結する。

決定根拠 (= kawaz 提示ユースケース、2026-06-11):

`hyoui run --detached -- claude` のような**無人 worker** では、child (= claude) 自身が
外向きに接続を張るリモート操作機能 (= Claude Code の /remote-control。claude プロセスが
NAT 越えで接続を維持し、スマホ / ブラウザの専用 UI からネイティブに操作できる) を
併用する運用が現実にある。child が self-suspend すると **claude プロセスそのものが
停止し、この接続維持・応答が全て死ぬ**ため、リモート側からは完全に手出し不能になる
(= hyoui を経由する以前に、子プロセス自身が提供していた接続性が失われる)。
「外側 API (`hyoui kill --signal=CONT`) で起こせるから『誰も起こせない』状況は無い」
([[DR-0017]] 柱 2 の論拠) は構造的には正しいが、**大半のユーザにはその API が発見不能**
で、現実には混乱して Ctrl-C 連打で終了するのがオチである。子を走らせ続けること自体が
唯一の解であり、attach client が存在しない場面でそれができるのは daemon だけ — これが
auto-resume = daemon 責務 (client 側 policy 不採用、§Rejected) の決定根拠である。
default は引き続き `notify` (= 勝手に起こさない、[[DR-0017]] 不変)。

なお [[DR-0015]] §2.2 の「`on-child-suspend` policy は client の cap negotiate payload に
含め、daemon は leader の policy を覚える」という配置は、実装されないまま本 DR で
**daemon 常駐 policy (run 時固定) に置き換える** (= 部分 supersede)。leader の入れ替わり
ごとに policy が揺れない・leader 不在時も発動できる、の 2 点で daemon 配置が上位。

### 4. `run --timeout` / `--idle-timeout` を `--until` と同経路で daemon に配線する

削除ではなく配線を採用する。`--until` の DaemonizeInit → DaemonConfig → watcher 経路に
相乗りし、**終了条件の発動者を daemon に統一**する (= detach 後・client 全滅後も効く
semantics。client 側に置くと「attach している間しか効かない timeout」という直感に反する
代物になる):

- `--idle-timeout`: **master 出力の最終時刻基準** (= 子からの新 bytes が DUR 途絶)。
- `--timeout`: **daemon 起動時刻基準**の overall 上限。
- 発火時の終了手順は `--until` match と同じ finalize escalation
  (= killpg(SIGTERM) → CONT+TERM → grace → KILL、`session.rs` 実装済経路) を共用する。

### 5. pipe-through: 非 tty stdin の EOF で default `SendEof` + `--stdin-eof` で override

`stdin が tty でない場合`、attach client は stdin EOF 観測時に default で EOT (0x04) を
子 PTY へ送出する (= 実装済 dead code `StdinEofAction::SendEof` の production 配線)。
override flag を attach / run 共通で用意する (run は exec attach に伝搬):

```
--stdin-eof=send-eof   # 非 tty stdin の default。EOF 時に EOT 送出
--stdin-eof=detach     # 現行挙動。EOF 時にそのまま切断 (子は daemon 配下に残る)
```

justify: 直接実行 (`echo "1+2" | bc`) なら pipe EOF は子に伝わって bc が終了する。
hyoui を挟むとそれが起きない現行挙動の方が**透過性の喪失**であり、default `SendEof` は
[[DR-0005]] の透明性最優先と整合する透過性の**回復**である。

opt-out (`detach`) を必須で残す理由: EOT が EOF と解釈されるのは PTY が canonical mode
の時だけで、**raw mode の TUI には 0x04 がただの入力 byte として刺さる**
(= claude TUI 等では別意味の操作になり得る)。「pipe で流し込んで起動し、以降は
attach で対話する」用途では `detach` が正しい。

なお stdin が tty の場合は EOF が通常発生せず本 flag は無関係 (= 従来挙動)。

### 6. `attach --exclusive` / `--detach-others` は parse 段で「未実装」エラー化

silent no-op (= 指定が黙って無視される) の放置は [[DR-0014]] 検証主義違反のため、
実装が入るまで parse 段で「未実装」を明示するエラーを返す (= `hyoui detach` 等の
予約エラーと同じ流儀、[[DR-0004]])。実装自体は本 DR の射程外として別 issue に切り出す。

### 7. help 残骸の除去

`usage_run()` に残る `--on-parent-suspend` の記載 (= parser からは [[DR-0015]] で削除済)
を除去する。「指定するとエラーになるオプションが help に載っている」状態の解消。

## Rejected alternatives

### auto-resume を client ローカルに縮退 (= codex 案)

監査 2 系統の唯一の意見相違点。「daemon が policy を覚える必然性は薄く、attach client の
ローカル設定 (= notify 受信時に resume request を返す) に縮退すれば protocol / config の
追加が不要」という案。

却下理由: **auto-resume が本当に必要なのは誰も attach していない時**であり、client 側
policy では発動者が不在 (= detached worker の self-suspend で誰も resume request を
送れない)。「attach 中なら人間が居る = follow が正解 (§2)、無人なら client が居ない」
ため、client 配置の auto-resume には**有効な発動場面が存在しない**。

### auto-resume を削除して待つ (= 透過原則的な最有力案)

「ユースケースが出るまで削除し、外側 API (`hyoui kill --signal=CONT`) で代替」する案。
監査 findings の時点では透過原則 ([[DR-0005]]) 整合の第一候補だった。

却下理由: kawaz 提示の無人 worker ユースケース (§3 決定根拠) が既に実在する
(= 「ユースケースが出るまで」の条件が満たされた)。外側 API は「知っている人には
ある」だけで一般ユーザには発見不能であり、child 自身が提供するリモート接続性が
死んだ後では実質復帰手段が無い。
また [[DR-0017]] が AutoResume の opt-in 温存を明文化しており、削除はその覆しに当たる
(= 覆すだけの新根拠が無い)。default は `notify` のままなので透過原則の侵食も無い
(= opt-in した者だけが介入を受け取る)。

### `--timeout` / `--idle-timeout` を削除する

「no-op を消すだけなら削除が最小」という案。

却下理由: 無人 worker 運用 (= 本 DR の auto-resume と同じユースケース系) では
「放置された worker の暴走・hang を時限で刈る」需要が現実的で、`--until` という
完成済の daemon 側終了条件経路が既にあるため**配線コストが小さい**。終了条件
(until / timeout / idle) は同族であり、発動者を daemon に統一して揃える方が
一貫する。削除して後から再追加すると flag 名・semantics の再設計を二度払う。

### `--mode` を温存して再定義する

`headless` に「attach しない + 仮想サイズ + policy preset」の新しい意味を与えて
配線する案。

却下理由: §1 の通り、構成要素が全て既存 / 本 DR の直交フラグで表現可能。
preset は「フラグの組合せに名前を付ける」糖衣でしかなく、現時点で糖衣を要するほど
組合せが煩雑でもない (= `--detached --size=200x50 --on-child-suspend=auto-resume` で
worker 起動は 1 行)。多重定義 (`attach --mode` / `LockMode`) の解消益が上回る。

## Consequences

- **breaking change**: `run --mode=...` を指定している script はエラーになる。
  `--on-child-suspend=follow` も値 rename (`notify`) によりエラーになる。
  v1.0 未満のため許容 (= breaking change OK 方針)。エラーメッセージには移行先
  (= `--mode` → 直交フラグ、`follow` → `notify`) を明記する (= migration hint、
  help / error message に限る)。
- **[[DR-0001]] の partial supersede**: §デフォルト (モード別 preset) を本 DR で廃止。
  DR-0001 冒頭に注記を追加する (= 軸 2 は DR-0015、軸 1 default は DR-0017、preset は
  本 DR、で旧 2 軸設計の supersede が完結)。
- **[[DR-0015]] §2.2 の部分置き換え**: 「policy は cap negotiate payload」の配置を
  「daemon 常駐 policy (run 時固定)」に変更 (§3)。`SessionChildStoppedNotify` /
  `SessionChildResumeRequest` message 自体は不変。
- **auto-resume 時の可観測性**: notify 抑止 (§3) により leader は stop/resume を
  message では知れないが、stopped → resumed は record の lifecycle event と
  `list` / `status` で観測可能 ([[DR-0017]] 柱 2 の可観測性要件で担保)。
- **新 protocol message / cap flag は不要**: DaemonizeInit JSON への field 追加
  (= `on_child_suspend` / `timeout` / `idle_timeout`) と attach への flag 伝搬のみ。
- **射程外 (= 別途対応、findings 参照)**: SIGWINCH 未配線 (= DR-0006 §6 の実装漏れ修復、
  最優先級だが本 DR の決定対象ではない)、`screen dump/snapshot --timeout` no-op、
  daemon のシグナル堅牢化 (= docs/issue/2026-06-11-bug-daemon-signal-robustness.md)。
- **実装後の検証要件 (= [[DR-0014]] 流マトリクス)**:
  - auto-resume: detached (leader 不在) / attached (leader 有) × self-suspend (^Z 由来
    SIGTSTP / kill -STOP) で「子が復帰する・client が置き去りにならない」を実機確認
  - `--stdin-eof`: canonical 系 (bc / cat) × raw TUI 系 (vim / claude) × `send-eof` /
    `detach` で「EOF が伝わる / 0x04 が刺さらない」を確認
  - timeout 系: attach 有無 × idle / overall × `--until` 併用で発火と escalation を確認

## Implementation

- `hyoui::cli`: `Mode` enum / `run --mode` 削除、`OnChildSuspend` の値 rename
  (`Follow` → `Notify`)、`--stdin-eof` 追加 (run / attach)、`--exclusive` /
  `--detach-others` の未実装エラー、`usage_run()` の `--on-parent-suspend` 残骸除去、
  completion 追従
- `hyoui-cli::daemonize`: `DaemonizeInit` に `on_child_suspend` / `timeout_ms` /
  `idle_timeout_ms` (serde default で旧 JSON 互換、[[DR-0018]] と同流儀)
- `hyoui::daemon`: `DaemonConfig` に同 field、child stopped 処理に AutoResume 分岐
  (= killpg(SIGCONT) + notify 抑止)、timeout / idle watcher (= UntilWatcher と
  finalize escalation を共用)
- `hyoui-cli::main` / attach: run → exec attach への `--stdin-eof` 伝搬、
  非 tty stdin の default `SendEof` 配線 (= `with_stdin_eof_action`)

## 関連

- [[DR-0001]] — jobcontrol 2 軸 (= preset 表を本 DR で廃止、partial supersede)
- [[DR-0005]] — 思想 (= pipe-through default SendEof の透過性回復を justify)
- [[DR-0006]] — CLI ground rules (= `--exclusive` / `--detach-others` の原典)
- [[DR-0014]] — 検証主義 (= silent no-op 禁止、実装後マトリクス要件)
- [[DR-0015]] — run = fork + exec attach (= 軸 2 廃止の正本、§2.2 policy 配置を本 DR で変更)
- [[DR-0017]] — notify-only default + AutoResume opt-in 温存 (= 本 DR §3 がその配置を確定)
- [[DR-0018]] — DaemonizeInit 経由の field 伝搬の先例 (= namespace)
- [docs/findings/2026-06-11-signal-suspend-interaction-audit.md](../findings/2026-06-11-signal-suspend-interaction-audit.md) — 2 系統監査の正本 (= no-op 棚卸し + 相互作用マトリクス)
