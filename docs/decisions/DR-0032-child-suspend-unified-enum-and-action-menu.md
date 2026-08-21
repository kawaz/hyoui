# DR-0032: 子 suspend 時動作の統合 enum (`on_child_suspend`) + child action menu

- Status: Active (kawaz 承認 2026-07-30)
- Date: 2026-07-30
- Related: DR-0005 (思想 — 子から見た透過、DR-0029 で狭めた原則との整合), DR-0013 (screen state 正本 — menu 描画が正本を汚さない制約), DR-0014 (self-check — 本 DR の in-band 解釈拡張の justify), DR-0019 (daemon 側 auto-resume policy — 写像先、CLI flag / `hyoui set` の語彙は不変), DR-0024 (config ファイル機構 — CLI flag 最小化の思想を本 DR が踏襲), DR-0029 (attach は覗き窓 — §1 通知行を menu の第 1 段描画が拡張、§2 単発 action を `ctrlz_x1_action` で設定化, §4 config を本 DR が統合), DR-0030 (rw attach 中は子を停止させたままにしない — 本 DR の enum default が同原則を維持、menu は opt-out 側の UX 改善)
- Origin: docs/issue/2026-07-30-design-child-suspend-action-menu.md (kawaz 骨子裁定 2026-07-30、ccmsg r92 m18-20)

## Context

子が suspend した時の hyoui の振る舞いは、現在 2 つの独立 bool に分かれている:

| config | 責務 | default |
|---|---|---|
| `[session] auto_resume` | daemon policy (= 無人時も含め daemon が SIGCONT で起こすか) | `false` |
| `[attach] resume_stopped_child` | attach client policy (= rw attach 中に停止を検知したら起こすか) | `true` |

2 bool は 4 通りの組合せを許すが、意味のある状態は 3 つしかない
(= `auto_resume = true` の時、attach 側の値は実効に影響しない — daemon が notify を
抑止して先に起こすため、client が停止を観測する機会がほぼ無い)。利用者が認識する概念は
「子が suspend したらどうなるか」という **1 つの選択**であり、それを 2 箇所の bool に
分けて書かせるのは内部の責務分担 (daemon / attach client) の漏出である
([[DR-0005]] 透明性 = 利用者に内部モデルの学習を強いない)。

さらに、両方を無効にした状態 (= 「勝手に起こさない」opt-out、[[DR-0030]] §Consequences が
unit test 付きで認めた構成) には**操作手段の空白**がある: attach 中に子が止まると、
[[DR-0029]] §1 の通知 1 行が出るだけで、起こす・畳む・終わらせるのすべてが外側 CLI 頼みに
なる (= 覗き窓を開けている人がその場で何もできない)。

kawaz 裁定 (2026-07-30) の骨子:

1. メニューは `resume_stopped_child = false` 相当の時のみの機能 (= 既定の auto-resume 下では出番なし)
2. 子 suspend 時の動作を 1 つの enum に統合する (= bool 2 個の統合リファクタを兼ねる)。
   原案 `on_child_suspend_action = auto_resume_always | auto_resume_on_attached | show_child_action_menu`、
   語彙は既存との整合性を考えて調整 (= AI 側に委任)
3. `ctrlz_x1_action = client_suspend | client_detach | select_on_demand` (既定 `client_suspend`) で detach 派の選択肢の復活 + 単発確定後の選択プロンプト
4. メニュー項目: client detach / client suspend (fg 復帰で子も起こす) / SIGCONT /
   SIGINT・SIGHUP・SIGKILL (終了系として別グループ)

## Decision

### 1. 統合 enum `[session] on_child_suspend` — bool 2 個を置換

```toml
[session]
# 子が suspend (stopped) した時のふるまい。default: auto_resume_on_attached
on_child_suspend = "auto_resume_on_attached"
#   auto_resume_always      — daemon が常に即 SIGCONT (attach の有無に関係なく)
#   auto_resume_on_attached — rw attach client が居る間だけ起こす (無人時は停止を維持)
#   show_child_action_menu  — 起こさず、attach client が child action menu を表示
```

**key 名は kawaz 原案の `on_child_suspend_action` から `on_child_suspend` に調整する**。
理由: 既存 daemon CLI flag `--on-child-suspend=notify|auto-resume` ([[DR-0019]] §3) と
1:1 対応する名前にすることで「同じ概念の設定」であることが読者に見える。
`on_child_suspend` は前置詞句として既に「子 suspend 時に何をするか」を意味しており、
`_action` suffix は情報を足さない (`ctrlz_x1_action` は key 全体で「ctrlz 単発の action」を
構成するので suffix が必要、という違い)。**値名は原案 3 値をそのまま採用**する
(= snake_case、TOML key の既存慣習と一致。長いが自己説明的で、config は補完が効かない
分だけ読み時の自明性を優先する)。

**配置は `[session]`**: この enum は「子が suspend した時にシステム全体としてどう
振る舞うか」という session 単位の 1 決定であり、attach の有無は分岐条件であって
設定の帰属先ではない。[[DR-0029]] §4 の配置基準 (= attach client の UX は `[attach]`、
session 単位の policy は `[session]`) と整合する。

#### daemon policy / attach policy への写像

enum は設定語彙であり、**wire には乗らない**。読み込み時に既存の 2 レイヤへ分解する:

| enum 値 | daemon policy ([[DR-0019]] `OnChildSuspend`) | attach client の停止検知時挙動 |
|---|---|---|
| `auto_resume_always` | `AutoResume` (= killpg(SIGCONT) + StoppedNotify 抑止) | resume 要求 (= 発動機会はほぼ無い。daemon が先に起こすため。handshake snapshot が stopped の race 残りに対する安全側) |
| `auto_resume_on_attached` | `Notify` | `SessionChildResumeRequest` 送信 (= [[DR-0030]] の 2 発火点そのまま) |
| `show_child_action_menu` | `Notify` | resume せず child action menu を表示 (§2) |

- daemon への伝搬は既存の `DaemonizeInit.on_child_suspend` (= [[DR-0019]] §3 の経路) に
  写像後の値を流すだけ。**新 protocol message / cap flag / DaemonizeInit field は追加しない**
- attach client は自分の config 読み込みで同 key を読む (= 既存 `resume_stopped_child` と
  同じ経路)。判定集約点 `client::should_resume_stopped_child(mode, config)` ([[DR-0030]] §1)
  は bool でなく「Resume / Menu / Nothing」の 3 値を返す形に拡張し、2 call site
  (handshake snapshot / StoppedNotify 受信) の drift 防止構造を維持する
- ro / rw-no-leader が起こさないのは [[DR-0029]] §5 のまま不変。menu も同様に
  **rw (leader 可) client のみ**が表示する (= ro は観戦者であり操作 UI を出さない)

#### 既存 CLI / `hyoui set` の語彙は変えない (= 統合は config 層に閉じる)

`run --on-child-suspend=notify|auto-resume`、`hyoui set <s> on-child-suspend=...`、
`status` / `list` の policy 表示 ([[DR-0019]] Update) は **daemon policy の語彙のまま不変**:

- [[DR-0024]] が確立した「config の役割を CLI flag に出張させない」思想に従う。
  enum 3 値のうち attach 側の 2 値は daemon に伝えても意味がなく、CLI flag を 3 値に
  拡張すると「flag では指定できたことにならない値」を持つ歪な flag になる
- 優先順位は [[DR-0029]] §4 の形を維持:
  `--on-child-suspend` flag > `[session] on_child_suspend` の daemon 写像 > builtin (= notify)。
  flag が override するのは **daemon policy のみ**で、attach 側の写像 (resume / menu) は
  config の enum 値に従う
- `hyoui set` / `status` の値域と protocol (`set-v1`) も不変。runtime 変更で触れるのは
  daemon policy だけであり、attach 側 policy の runtime 変更手段は現状どおり存在しない
  (= 次の attach から効く)。set への enum 対応が要るかは後続 issue (§Consequences)

#### migration (= 旧 bool 2 個の扱い)

**互換読み込みは実装しない** (v1.0 未満、breaking change 許容方針)。default 利用者は
無影響 (= 旧 default の組合せ `auto_resume=false` + `resume_stopped_child=true` は
enum default `auto_resume_on_attached` と同挙動)。

ただし旧 key を **silent に無視しない**: `[session] auto_resume` /
`[attach] resume_stopped_child` を config に見つけたら**起動を拒否し、migration hint
付きエラー**を返す (= 「`[session] on_child_suspend = "..."` に書き換えよ」)。
unknown field 一般は従来どおり無視するが、この 2 key は「明示設定者の意図が silent に
default へ倒れる」既知の廃止 key であり、[[DR-0014]] の silent no-op 禁止と
[[DR-0024]] の「不正 config は起動拒否」流儀に揃える ([[DR-0019]] の
`follow` → `notify` rename エラーと同型)。

### 2. child action menu

#### 発動条件

以下がすべて成立した時、attach client が child action menu を表示する:

1. `on_child_suspend = "show_child_action_menu"`
2. rw (leader 可) attach client である (= ro / rw-no-leader は表示しない)
3. 子が stopped であることを検知した (= [[DR-0030]] と同じ 2 発火点:
   attach 成立時の handshake snapshot が stopped / attach 中の `SessionChildStoppedNotify` 受信)
4. 外側 stdout が raw mode の tty で端末サイズが取れる (= [[DR-0029]] §1 の通知行と
   同じ描画前提。pipe に escape を垂れ流さない)

**attach 不在時に子が止まった場合**: メニューを出す先が無いので、daemon は写像どおり
`Notify` のまま**何もせず待つ** (= 無人時の「勝手に起こさない」は [[DR-0019]] §3 /
[[DR-0030]] §4 の規定のまま)。次に rw attach が来た時点で発火条件 3 の
handshake snapshot 経路により menu が表示される。

#### 初回 attach redraw との順序 (= [[DR-0013]] §4 Phase A)

**発動条件 3 が handshake snapshot 側で成立した場合、menu と入力 focus は attach 復元
redraw の到着を待たずに成立する**。発動条件 1-4 はいずれも handshake response と client
自身の状態だけで判定でき、redraw の中身を必要としない。[[DR-0013]] §4 Phase A の初回
redraw は sync update 中に保留され得る (= 到着が保証されない) ため、menu を redraw 到着に
従属させると「子を起こす手段 (= menu) が子の動作に依存する」循環になる。redraw が届いた
時は端末へ適用した上に menu を描き直す (= 描き直しの直前に mode を再評価する。
[[DR-0013]] §4 Phase A 受信側の義務 (c))。

**「停止後の raw_data 到着を証拠に menu を畳む」規則 (= 後述「メニュー表示中のキー入力」) から、attach 成立後に
最初に届く RAW_DATA 1 つを除外する**。それは子が生んだ出力ではなく daemon が screen
state から組んだ復元 bytes であり、resume の証拠にならない。除外はこの 1 frame に限り、
以降の raw_data は従来どおり resume の証拠として扱う。

#### メニュー項目

2 グループに分けて表示する。区分は **UX 視点** (kawaz 裁定 2026-07-30 m42/m43):
「**脱出** = client の操作 (child から離れる)」と「**子への操作**」。
「子に何が起きるか (継続/終了)」では分類しない。

**脱出 (= client の操作、child から離れる)**:

| 項目 | 動作 | 説明 |
|---|---|---|
| detach (`d`) | client 終了 (= 接続を畳む) | `fg` では戻れない。子は**停止したまま**残る (= 無人になるので停止維持可、[[DR-0030]] §3 と整合)。再開は `hyoui attach` (menu が再表示される) か `hyoui kill <s> --signal=CONT --no-terminate` |
| client suspend (`z`) | [[DR-0029]] §2 の client suspend 経路 (`raise(SIGTSTP)`) | 窓を閉じずに外側 shell へ戻る。`fg` で同じ窓に復帰し、**復帰と同時に子も起こす** (= menu からの suspend は「一旦どけて戻ったら続きをやる」意思表示なので、復帰時に `SessionChildResumeRequest` を送る。`show_child_action_menu` 下では通常送らない resume を、この明示操作に限り送る) |

**子への操作** (結果的に脱出になる場合もある):

| 項目 | 動作 | 説明 |
|---|---|---|
| 起こす (`c` / `Esc`) | 子 pgrp へ `SIGCONT` | 停止した子を再開する。attach は継続し、そのまま操作に戻る |
| SIGINT (`i`) | 子 pgrp へ `SIGCONT` + `SIGINT` | 割り込み (= 端末の Ctrl+C 相当)。多くのアプリが graceful に中断・終了する |
| SIGHUP (`h`) | 子 pgrp へ `SIGCONT` + `SIGHUP` | 端末切断の通知。default disposition は終了 (= handler を持つアプリは reload 等に使う場合がある) |
| SIGKILL (`k`) | 子 pgrp へ `SIGKILL` | 捕捉・無視できない即時強制終了 |

**「閉じるだけ」の項目は持たない** (kawaz 裁定 2026-07-30 m41/m42): 子が停止中で
入力の受け手がいない以上、menu を畳んで通常状態に戻すだけの操作に意味がない。
`Esc` は「この停止の取り消し」として **起こす (SIGCONT) の alias** に割り当てる
(= menu は停止をきっかけに出るので、Esc = 取り消し = resume が最も自然。resume は
menu 内で最も安全な操作なので反射キーでも事故にならない)。それ以外の打鍵はすべて
無視して破棄する (= select_on_demand プロンプトと同じ方針)。menu の解除は「項目を
選ぶ」か「子が外部要因で resume する」(= 停止後の raw_data 到着を証拠に自動で畳む)
のどちらかに限る。

**SIGINT / SIGHUP に SIGCONT を併送する理由**: stopped なプロセスに送られた signal は
pending になるだけで、SIGCONT されるまで処理されない (= SIGKILL と SIGCONT だけが
stopped 状態でも即座に効く)。SIGCONT を併送しないと「メニューで SIGINT を選んだのに
何も起きない」silent no-op になる。既存の finalize escalation ([[DR-0019]] §4 の
`killpg(SIGTERM)` → CONT+TERM → grace → KILL) が同じ理由で CONT を併送しており、
その流儀に揃える。副作用として子は終了処理の間だけ一瞬走る (= stopped のまま
graceful に殺す手段は POSIX に存在しない。それが要るなら SIGKILL を選ぶ)。

signal 送信は既存の protocol `kind = "signal"` message (= client → daemon → 子の明示
signal 送信) を再利用する。**新 protocol message / cap flag は追加しない**。

#### メニュー表示中のキー入力 — client が飲み、PTY へ流さない

メニュー表示中、attach client は tty stdin の入力を**メニュー操作として解釈し、
子の PTY へ一切 forward しない**。メニューは項目の選択・実行、または明示のキャンセル
(= 表示を消して通常 forward に戻す) で終了する。子が menu 以外の要因で resume した
(= `SessionChildStoppedNotify` の対である running への遷移を観測した) 場合も即座に
メニューを消して forward を再開する。

**in-band 解釈の拡張としての justify** (= [[DR-0014]] self-check の核心):

- [[DR-0029]] §3 は in-band 解釈を「単一キー (Ctrl+Z) の取扱い規則」まで狭め、
  DR-0005 の原則を「**子の stdin には hyoui 由来の escape を一切足さない
  (= 子から見た透過性)**」に再定義した。本 menu はこの原則を破らない —
  menu は子に何も送らず、menu 由来の bytes が子の stdin に混入する経路が無い
- forward を止めることは「子から見た透過」の侵害ではない。**発動条件が「子が停止中」
  = 子が入力を消費できない状態に限定**されており、その間に PTY へ流した入力は子に
  読まれず buffer に溜まり、resume の瞬間にまとめて流れ込む (= ユーザが「止まっている
  画面」に向かって打った操作が、後から文脈を失った状態で実行される事故)。停止中の
  forward はむしろ透過の名を借りた害であり、飲む方が「直接実行で子が止まった端末」
  (= 打鍵しても誰も読まない) に近い
- opt-in である: `show_child_action_menu` を選んだ session でしか発動せず、default
  (`auto_resume_on_attached`) では menu のコードパスに入らない (= 最小介入)
- 「子が resume したら即終了」により、in-band 解釈が生きる期間は「子が入力を消費
  しない期間」に厳密に閉じる。tmux/screen 流の常設 prefix 体系 ([[DR-0005]] が領域外と
  した multiplexer 路線) への回帰ではない

メニュー内の具体的キーバインド (項目選択キー、番号 / カーソル移動、キャンセルキー) は
本 DR では確定せず後続 issue とする (§Consequences)。

### 3. `[attach] ctrlz_x1_action = client_suspend | client_detach | select_on_demand`

```toml
[attach]
ctrlz_x1_action = "client_suspend"   # 単発 Ctrl+Z 確定時の action
```

[[DR-0029]] §2 の Ctrl+Z ガードの「余った 1 発」が起動する action (= 現在 client suspend
にハードコード) を設定化する。既定 `client_suspend` (= DR-0029 の挙動そのまま)。
`client_detach` を選ぶと単発 Ctrl+Z で detach する (= 接続を畳む。子は走り続ける)。

**key 名に `x1` を含めるのは、この設定が司るのが「ガード窓で単発 (×1) と確定した後の
action だけ」であることを名前から読めるようにするため** (kawaz 裁定 2026-07-30)。
ガード窓そのもの (単発判定・連打 forward・他キー割り込み) はこの設定では変わらない。

#### `select_on_demand` — 単発確定後にプロンプト状態へ遷移する第 3 の値

即 suspend / 即 detach の代わりに、**単発確定を「選択待ち」に変える**:

1. 単発 Ctrl+Z が確定すると、client は alt screen を抜けて (= 全画面 TUI の表示から
   通常画面に戻し)、最下行にその場限りの 1 行プロンプトを描画する:
   `[hyoui] ^Z: client suspend / ^C: client 終了 / Esc: attach に戻る`
2. プロンプト状態のキー表 (kawaz 裁定 2026-07-30: 明示キーのみ反応):

   | キー | 動作 |
   |---|---|
   | Ctrl+Z | client suspend (= `client_suspend` と同じ) |
   | Ctrl+C | client 終了 (= detach。子は走り続ける) |
   | Esc | attach 表示に戻る |
   | その他 | **無視して破棄** (子へ送らず、状態も変えない) |

   「その他のキーでキャンセル」にしないのは、放置後の復帰 (モニタオフ解除等) で
   ユーザが無自覚に打ったキーに反応すると事故になるため。明示 3 キー以外は何も
   起こさないのが安全側。
3. timeout は設けない (= 子は走り続けており急ぐ理由がない。プロンプトは次の明示キー
   まで持続する)
4. この状態は**単発アクション確定後の client 側の状態機械**であり、ガード窓とは別物。
   ガード窓の 2 連打 forward (= 子へ Ctrl+Z を届ける経路) は本モードでもそのまま使える
5. プロンプト状態中のキーは hyoui が飲み、PTY へ流さない (= §2 menu と同じ in-band
   解釈。justify も同じ「明示的にユーザが hyoui の操作面を呼び出した状態に限定」——
   ここでは Ctrl+Z 単発がその呼び出しにあたる)
6. 描画は §4 第 1 段と同じその場限り方式 (screen state 不汚染、[[DR-0013]])

位置づけは **§2 child action menu の client 版** (kawaz 原文)。menu が「子が止まった時の
子への操作面」であるのに対し、こちらは「client をどう畳むかの操作面」で、対象が違う。

**[[DR-0029]] Rejected「Ctrl+Z 単発を detach に割り当てる」との関係**: あの却下は
「**default として**は `fg` で戻れず re-attach を強いるので不適」という判断であり、
detach という action 自体の否定ではない (= 非破壊で誤爆コストが小さいことは同節が
認めている)。窓を毎回畳みたい利用者の opt-in 選択肢として復活させることは
「attach は覗き窓であり client 操作で子を止めない」原則に反しない (= どちらの値でも
子は走り続ける)。default が `client_suspend` である限り DR-0029 の決定は覆らない。

連打規則 (= 2 発ごとに子へ 1 発)、符号化判定、`ctrlz_guard` / `ctrlz_guard_delay` の
意味論は [[DR-0029]] §2 のまま不変。変わるのは「余った 1 発が起動する action」だけ。
値名が `client_` prefix を持つのは、action の対象が子ではなく client 自身であることを
設定名の上でも明示するため (= DR-0029 の原則の反映)。

### 4. menu の描画 — 2 段構え

**第 1 段 (= 本 DR で実装する形)**: [[DR-0029]] §1 の停止通知 1 行の**拡張**として
実装する。画面最下行の N 行 (= menu の項目数 + グループ見出し分) を `DECSC` / `DECRC`
で cursor 退避しつつその場限りで上書き描画する:

- **daemon の screen state には一切書かない** (= 正本を汚さない、[[DR-0013]])。
  menu は描画した client のローカル表示であり、他 client / `screen dump` /
  record には現れない
- 子の画面領域を一時的に汚すが、menu 終了後の再描画 (resume 時の redraw /
  detach 時は無関係) で消える — DR-0029 §1 の 1 行通知と同じ性質を N 行に広げただけ
- 選択状態の変化 (= カーソル移動) も同じ N 行の再描画で表現する

**第 2 段**: screen-overlay 一般機構
(docs/issue/2026-07-21-screen-overlay-general-mechanism.md) が実装されたら、menu の
描画をその上に移行する。第 1 段の「その場限り N 行描画」は overlay 機構の必然性を
実地で確認するドッグフーディングを兼ねる。移行は描画レイヤの差し替えであり、
menu の意味論 (発動条件 / 項目 / 入力の扱い) は変わらない。

### self-check ([[DR-0014]] §self-check への回答)

- **既存 DR で justify されているか**: 統合 enum は [[DR-0019]] / [[DR-0029]] /
  [[DR-0030]] が積んだ policy 群の設定語彙の整理で、新しい介入を足さない。menu の
  in-band 解釈は §2 のとおり [[DR-0029]] §3 で狭めた原則 (子から見た透過) の枠内
- **透過原則を破る理由は必然か**: menu が飲むのは「子が読めない期間の入力」であり、
  forward する方が resume 時の流れ込み事故を作る (= 「便利」ではなく、停止中 forward
  の害の回避)
- **最小介入か**: 新 protocol message 0、新 cap flag 0、daemon state 追加 0。
  既存の `signal` message / `SessionChildResumeRequest` / client suspend 経路 /
  停止通知行描画の組合せのみ。menu は opt-in で default のコードパスに現れない
- **標準機能の再発明でないか**: 「stopped な子への操作 UI」に相当する kernel / shell
  標準機能は無い (shell job control は hyoui の外側の階層で、覗き窓の内側には届かない)
- **既存 DR の実装漏れより優先すべきか**: 本 DR は既存 2 bool の統合リファクタを含み、
  設定面の負債を増やさず減らす方向

## Rejected alternatives

### key 名を kawaz 原案どおり `on_child_suspend_action` にする

`ctrlz_x1_action` と suffix が揃う利点はあるが、既存 CLI flag `--on-child-suspend` との
対応が名前から見えなくなる。`on_child_suspend` は句として action を既に含意しており、
suffix は冗長 (§1)。

### CLI flag `--on-child-suspend` を enum 3 値に拡張する

`run --on-child-suspend=show-child-action-menu` のように flag からも統合 enum を
指定できる案。run = fork + attach の合成 ([[DR-0015]]) なので技術的には attach 側へ
伝搬可能だが、[[DR-0024]] の「config の役割を CLI flag に出張させない」思想に逆行し、
`hyoui set` / `status` (= daemon policy の語彙) との値域不一致も生む。attach 単体
(`hyoui attach`) には結局 config しか経路が無く、flag 拡張は run 経路だけの
半端な近道になる。

### bool 2 個を維持して menu 用の第 3 の key を足す

`[attach] child_action_menu = true` のような追加 bool 案。無意味な組合せ
(= `auto_resume = true` + menu) が増える一方で、Context の「4 通り中 3 つしか意味が
無い」問題は温存される。設定の直積が状態空間を超える構造は enum で表すのが正
([[default-convergence-guard]] の「列挙で表せる状態を bool の組合せで表さない」)。

### 旧 bool key を互換 alias として読み続ける

v1.0 未満で breaking 許容 ([[DR-0030]] §2 の alias なし先例)。alias を置くと
「enum と旧 bool が両方書かれた config」の優先順位という新しい仕様を背負う。
silent 無視の害だけ migration エラーで塞ぐ (§1 migration)。

### menu を常設機能にする (= enum と独立に、停止検知で常に出す)

kawaz 骨子 1 が明示的に否定 (= menu は `resume_stopped_child = false` 相当の時のみ)。
default の `auto_resume_on_attached` では停止は即 resume されるので menu を出す暇も
必要も無く、常設化は default 経路への介入追加になる。

### menu 表示中も入力を PTY へ forward する (= 透過性の最大化)

menu のキー操作と子への入力が同じ打鍵から二重解釈される (= 「1 を押したら項目 1 が
実行され、かつ resume 後の子に `1` が届く」)。停止中に溜めた入力の流れ込み事故も
残る。forward しつつ menu も動かす合理的な意味論が存在しない。

## Consequences

- **breaking change (v0.x なので許容)**:
  - config key `[session] auto_resume` / `[attach] resume_stopped_child` が消滅し、
    `[session] on_child_suspend` (enum) に統合される。旧 key が config に残っている
    場合は起動エラー + migration hint (§1)。default 利用者は挙動無変更
  - `should_resume_stopped_child` の返り値が bool から 3 値に変わる (library API)
- **`hyoui set` / `status` / protocol は不変**: 統合 enum は wire に乗らず、daemon が
  持つのは写像後の policy (`notify|auto-resume`) のまま。`set-v1` / `StatusResponse` /
  DaemonizeInit に field 追加なし
- **menu 選択時の attach 不在ウィンドウ**: `show_child_action_menu` で無人時に子が
  止まると、次の attach まで停止したまま待つ (= 意図どおり。無人でも起こしたいなら
  `auto_resume_always` を選ぶ)
- **検証要件 ([[DR-0014]] マトリクス)**:
  - enum 3 値 × 子 stop タイミング (attach 成立前 / 成立後) × attach mode
    (rw / ro / rw-no-leader / 無人) で「resume されるか / menu が出るか / 停止維持か」
    の期待表を unit + e2e で埋める
  - 写像の unit test: enum → (daemon policy, attach 挙動) の全対応
  - menu 中の入力が PTY に届かないこと・menu 終了後に forward が再開すること
  - 終了系項目で stopped な子が実際に終了すること (= SIGCONT 併送の実効。
    `kill -STOP` した `cat` / `bash -i` / TUI の 3 category、[[DR-0014]] 検証主義)
  - 旧 config key での起動エラーと hint 文言
  - ctrlz_x1_action 全 3 値 × 単発 / 2 連打 (= DR-0029 §2 の表の action 差し替え確認。select_on_demand はプロンプト状態のキー表 3 分岐も)
- **後続 issue として残す (= 本 DR では確定しない)**:
  - メニューのキーバインド詳細 (項目選択・キャンセルのキー割当、表示レイアウト文言)
  - web UI ([[DR-0027]]) 側での同等機能 (= browser client での menu 相当の操作 UI)
  - `hyoui set` の統合 enum 対応の要否 (= attach 側 policy の runtime 変更手段を
    作るかどうか。現状は次の attach から反映で足りるとみて保留)
  - screen-overlay 一般機構 (docs/issue/2026-07-21-screen-overlay-general-mechanism.md)
    到達時の描画移行

## 関連

- [[DR-0005]] — 思想 (= 「子から見た透過」に狭めた原則の枠内であることを §2 で justify)
- [[DR-0013]] — screen state 正本 (= menu 描画が正本を汚さない制約)
- [[DR-0019]] — daemon 側 auto-resume policy (= 写像先。CLI / set / 可視化の語彙は不変)
- [[DR-0024]] — config ファイル機構 (= CLI flag 最小化の思想、起動拒否の流儀)
- [[DR-0029]] — attach は覗き窓 (= §1 通知行の拡張、§2 の action 設定化、§3 の原則との整合、§4 config の統合元)
- [[DR-0030]] — rw attach 中は子を停止させたままにしない (= enum default が同原則を維持、opt-out 時の空白を menu が埋める)
- docs/issue/2026-07-30-design-child-suspend-action-menu.md — kawaz 骨子裁定の正本
- docs/issue/2026-07-21-screen-overlay-general-mechanism.md — 第 2 段描画の移行先
