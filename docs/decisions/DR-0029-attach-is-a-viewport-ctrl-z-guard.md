# DR-0029: attach は覗き窓 — client 操作で子を止めない (Ctrl+Z ガード + follow 廃止 + in-band escape ゼロ)

- Status: Active
- Date: 2026-07-25
- Related: DR-0005 (思想 — 「in-band escape の唯一の例外」条項を本 DR で撤回), DR-0007 (MVP scope — `Ctrl-A D` 記述を撤回), DR-0017 (session anchor + suspend policy — §柱2 の client follow を本 DR で撤回), DR-0019 (daemon 側 auto-resume policy — 本 DR は config default を足すだけで配置は不変), DR-0020 (発見性ヒント / 追加キー intercept 却下 — 本 DR で再判断), DR-0024 (config.toml 機構 — `[attach]` / `[session]` セクションで相乗り), DR-0026 (**本 DR が Supersede**), DR-0030 (§5 の resume 発火点を拡張 — attach 中の子 stop も trigger にする)
- Origin: docs/QUESTIONS.md SUSP-Q1..Q3 (2026-07-25 kawaz 裁定)

## 原則: attach は覗き窓であり、client 操作で子を止めない

hyoui の目的は **バックグラウンドで TTY アプリを走らせ続けたまま、attach / detach で
覗き窓としてアクセスする**こと。attach client は一時的な窓であり、窓を閉じたり開いたり
しても中で走っているアプリは影響を受けない。したがって:

- **attach client 側の操作で子が止まるのは目的の真逆**である
- **子が止まっても attach client は止まらない**。窓は開いたままで良い
- 子を止めたい / 起こしたいときは **外側の明示 API** (`hyoui kill --signal=TSTP` /
  `hyoui kill --signal=CONT --no-terminate`) を使う。「反射で押したキー」が子を止める経路を持たない

この原則を明文化したのが本 DR で、これに反する既存判断 (下記) をまとめて撤回する。

## Context

DR-0026 (Ctrl+Z 折衷 intercept) は「単発 Ctrl+Z = 300ms 保留後に子へ SIGTSTP forward、
2 連打 = detach」を実装した。DR-0017 §柱2 は「子 stopped 通知を受けた rw client は
`raise(SIGSTOP)` で follow する」を維持していた。両者の合成で、実機の挙動は
**「Ctrl+Z を 1 回押すと子も client も止まってシェルに戻る」** になる
(2026-07-24 実測、docs/issue/2026-07-24-bug-tstp-intercept-followups.md H1/H2)。

これは上記原則の真逆であり、kawaz の使用パターン (= 反射で Ctrl+Z を押す) で
「走らせ続けたかったアプリが止まる」事故が常態化していた。DR-0026 の Context が
「反射押しへの対応」として組んだ折衷 (= 単発は子に届ける) は、前提としていた
「Ctrl+Z を子に届けたい正当ユースケース」を過大評価していた。子への Ctrl+Z は
`hyoui kill --signal=TSTP` / `hyoui input key:C-z` という out-of-band 経路が既にある。

同時に、in-band escape の唯一の例外として温存していた `Ctrl-A d` detach prefix
(DR-0005 / DR-0007) は kawaz が一度も要求しておらず、実端末で発火しない bug 状態
(docs/issue/2026-07-20-detach-key-not-firing-keyboard-protocol.md) が続いていた。

## Decision

### 1. 子 stopped への follow を廃止 (= DR-0017 §柱2 の client follow を撤回)

`SessionChildStoppedNotify` を受けた attach client は **`raise(SIGSTOP)` しない**。
attach を継続し、`SessionChildResumeRequest` も自動送信しない (= 勝手に起こさない)。

子が止まると出力が来なくなり画面が固着するので、それが「hyoui が壊れた」ではなく
「子が停止中」だと分かるよう、**画面最下行に 1 行だけ通知を出す**:

```
[hyoui] 子プロセスが停止中 — 再開: hyoui kill <session> --signal=CONT --no-terminate
```

- 外側 stdout が raw mode の tty で、端末サイズが取れるときだけ描画する
  (= pipe に escape を垂れ流さない)
- `DECSC` / `DECRC` (`ESC 7` / `ESC 8`) で cursor 位置を保存・復元し、最下行を
  1 行だけ上書きする。子の画面領域を一時的に汚すが、resume 後の再描画で消える
- daemon の screen state には一切書かない (= 正本を汚さない、DR-0013)。他 client
  にも影響しない
- これは DR-0026 が Phase 2 送りにした「screen state 一時 overlay の一般機構」では
  なく、その場限りの 1 行描画に留める (= 最小介入)。progress bar 付きの本格 overlay は
  docs/issue/2026-07-25-request-attach-overlay-progress.md

**撤回する判断**: DR-0017 §柱2 の「各 rw client が自分の判断で follow (`raise(SIGSTOP)`
して外側 shell に suspend 伝播) する DR-0015 §2.2 の既存設計は維持」。子と client の
生死を連動させる設計そのものを本 DR で否定する。

`SessionChildStoppedNotify` を **全 rw client に broadcast する** 部分 (2026-07-24 の
H3 修正) は維持する。follow はしないが「子が止まった」事実は全 rw client が知る必要が
あるため (= 通知行の描画に使う)。ro client は対象外のまま。

### 2. tty stdin 経路の Ctrl+Z ガード

attach client の **tty stdin 経路**でのみ、Ctrl+Z (0x1a) を以下の規則で扱う
(kawaz 提示、2026-07-25):

| 窓内の連打数 | 子へ届く Ctrl+Z | client detach |
|---|---|---|
| 1 | 0 | する (delay 後) |
| 2 | 1 | しない |
| 3 | 1 | する (delay 後) |
| 4 | 2 | しない |

一般則は **「2 発ごとに子へ 1 発、余った 1 発が detach タイマーを起動する」**。
state は「保留中の Ctrl+Z があるか」の 1 bit だけで表現でき、連打回数の計数は要らない
(= 偶数打で保留が解消し、奇数打で新しい deadline が張られる)。

- **窓の定義**: 「最後の Ctrl+Z から `ctrlz_guard_delay`」。連打のたび実質的に延長される
- **他キー割り込み**: 窓の途中で Ctrl+Z 以外の byte が来たら detach 保留をキャンセルし、
  保留中の Ctrl+Z は **破棄** する (= 子に送らない)。当該 byte は通常入力として forward。
  「Ctrl+Z を押したがやめた」を打鍵の継続で取り消せる (= 一番自然な取消操作)
- **`delay = 0`**: 連打判定をせず単発で即 detach (= 子には一切届かない)
- **`ctrlz_guard = false`**: 完全 bypass (= Ctrl+Z 素通し)

**tty stdin 経路のみ**という限定が重要:

- `hyoui input key:C-z` / `hyoui kill --signal=TSTP` / web gateway 経由の入力は
  **従来どおり子に到達する** (= ガードは attach client の raw stdin にしか居ない)
- pipe / `< file` 経由の 0x1a も素通し (= それは「アプリへのデータ」であってキー操作
  ではない)。CLI 層が stdin の tty 判定でガードを無効化する

**なぜ「単発 = detach」で良いか**: detach は非破壊 (= 子は走り続ける、いつでも
再 attach できる) なので、誤爆のコストが「子が止まる」より桁違いに小さい。反射で
押しても目的に沿う結果になる。

### 3. `Ctrl-A d` detach prefix の全廃 (= DR-0005 の例外条項を撤回)

`Ctrl-A` prefix state machine、`HYOUI_DETACH_PREFIX` env、関連 help / test を削除する。

DR-0005 の思想「子プロセスへの入力は完全透過。in-band escape (prefix キー等) を一切
導入しない」に対し、DR-0005 自身が注記で `Ctrl-A D` を「唯一の例外」として認めていた。
本 DR でこの例外条項を削除する。

**Ctrl+Z ガードは新しい例外ではないのか?** — 位置付けは prefix key と異なる:

- prefix key は「その先のキーを hyoui のコマンド語彙として解釈する」体系 (= tmux/screen
  流の UI)。DR-0005 が「領域外」と明示した multiplexer 路線そのもの
- Ctrl+Z ガードは **語彙を持たない単一キーの取扱い規則**で、しかも「子を止める副作用の
  ある唯一の反射キー」を安全側に倒すためのもの。新しい操作体系を導入しない

いずれにせよユーザ視点では in-band 解釈なので、DR-0005 の原則は
**「子の stdin には hyoui 由来の escape を一切足さない (= 子から見た透過性)」**
という形に狭めて維持する (= DR-0005 §思想の柱を本 DR で改訂)。

### 4. config (`~/.config/hyoui/config.toml`)

```toml
[attach]
ctrlz_guard = true            # false で完全 bypass (= Ctrl+Z 素通し)
ctrlz_guard_delay = "500ms"   # 0 で連打判定なしの即 detach
ctrlz_guard_overlay = true    # 未実装 (= 受理のみ、issue 2026-07-25 参照)
resume_stopped_child = true   # rw attach 中は子を停止させたままにしない (DR-0030 で改名)

[session]
auto_resume = false           # 子の stop を daemon が観測したら自動 SIGCONT
```

**セクション配置**: DR-0024 で作った既存構造 (`[scrub_env]` / `[web]`) に合わせ、
すべてセクション配下に置く。kawaz 提示は top-level の flat key だったが、TOML は
top-level scalar が最初のセクションより前にしか書けないため、ファイル末尾に追記した
ユーザの `auto_resume = false` が `[web]` 配下に吸われる罠がある。セクション化で回避する。

- `ctrlz_guard*` / `resume_stopped_child` は attach client の UX なので `[attach]`
- `auto_resume` は attach の有無に関係ない session 単位の policy なので `[session]`

**`ctrlz_guard_delay` は duration 文字列** (`"500ms"` / `"1s"` / `"1.5s"` / `"2m"`)。
整数はミリ秒として受理する。DR-0026 の `*_ms: u64` 方式より人間可読を優先した
(= kawaz 提示の記法を尊重)。不正値は DR-0024 の方針どおり起動を拒否する。

**`auto_resume` と DR-0019 §3 の関係**: DR-0019 §3 は「auto-resume は daemon の責務」
と確定し、client ローカル配置を明示的に却下している (= auto-resume が本当に必要なのは
誰も attach していない時)。本 DR は **新しい機構を足さない**。既存の
`--on-child-suspend=notify|auto-resume` の **既定値を config から与えるだけ**である。

優先順位: `--on-child-suspend` flag > `[session] auto_resume` > builtin default (= notify)。

`hyoui run` の config 読み込みは既存の scrub_env 経路と共有する。`--no-scrub-env` は
「config が壊れていても起動できる escape hatch」なので、その場合 config 由来の
`auto_resume` も既定値に倒れる (= 壊れた config を読まないという一貫性)。

### 5. stopped child への再 attach 時 resume は維持

DR-0026 §2 (= rw attach 時に `child_stopped` なら `SessionChildResumeRequest` を送る、
ro / rw-no-leader は送らない) は本 DR でもそのまま引き継ぐ。§1 で撤回したのは
「client が子に **合わせて止まる**」であって、「人間が rw attach した = 操作意思」を
trigger にした resume は原則と矛盾しない (= 覗き窓を開けた人が操作するために起こす)。
config key 名だけ `[attach]` 配下に平坦化した。

> **📌 注記 ([[DR-0030]]、2026-07-29)**: 本節の trigger は「rw attach した時点で子が
> stopped」の 1 つだけだったが、それでは attach 成立**後**に子が self-stop した場合に
> 起こす主体が居らず、「attach しているのに操作が一切効かない」状態になっていた。
> DR-0030 が trigger に「attach 中の `SessionChildStoppedNotify` 受信」を追加し、
> config key を `resume_stopped_child` に改名した。本節の ro / rw-no-leader 除外は不変。

## Rejected alternatives

### Ctrl+Z 完全素通し (SUSP-Q1 案 b)

直接起動と同じ挙動になり透過性は最高だが、「反射 Ctrl+Z で子が止まる」= 本 DR が
解こうとしている問題そのものが残る。detach 手段も別途必要になる。

### Ctrl+Z を無視 (SUSP-Q1 案 c)

事故は防げるが、detach 手段が in-band に一切なくなる (= 別端末から `hyoui detach` を
打つしかない)。覗き窓を閉じる操作は窓の中から届く必要がある。

### `Ctrl-A d` を修正して残す (SUSP-Q3 案 b)

keyboard protocol (kitty CSI-u 等) 起因の調査を続け、CSI-u パースを prefix state machine
に足す案。Ctrl+Z が detach を担うなら prefix 体系そのものが不要で、bug 調査のコストを
払う価値がない。

### `--detach-key` 等で prefix を opt-in 復活 (SUSP-Q3 案 c)

使わない体系のために state machine + config + completion + help の保守が続く。
需要が観測されたら本 DR を supersede して再導入する。

### 子 stopped 時に何も表示しない

最小介入ではあるが、「画面が固まった」と「子が止まっている」をユーザが区別できない。
`hyoui list` を別端末で見に行かせるのは覗き窓としての UX を放棄している。

### client 側に独自 auto-resume を実装

DR-0019 §3 が却下済み (= 無人時に発動できないので有効な発動場面が存在しない)。
本 DR は daemon 側 policy の既定値を config から与える形に留める。

## Consequences

- **breaking change (v0.x なので許容)**:
  - `Ctrl-A d` / `Ctrl-A Ctrl-A` / `HYOUI_DETACH_PREFIX` が消滅。detach は Ctrl+Z 単発
  - config の `[attach.tstp]` / `[attach.resume]` セクションが `[attach]` の flat key に
    変わる (= DR-0026 の設定を書いていた場合は書き直しが要る。unknown field は無視
    されるので起動は落ちない)
  - `hyoui` library の `SuspendHooks` / `SUSPEND_OUTER_TTY_RESET` /
    `resolve_detach_prefix_from_env` / `DETACH_PREFIX_BYTE` が消滅、
    `OUTER_TTY_RESET` / `CTRL_Z_BYTE` / `with_outer_tty_raw` / `with_attach_config` に置換
  - `RunConfig::on_child_suspend` が `Option<OnChildSuspend>` に変わる (= 未指定を
    config 解決に回すため)
- **子を止める経路は out-of-band のみになる**: `hyoui kill --signal=TSTP` /
  `hyoui input key:C-z` / attach での Ctrl+Z 2 連打。「気付かず止まっていた」事故は
  Ctrl+Z 経路では起きなくなる
- **detach 時の端末 reset は全経路で共通**: Ctrl+Z ガード発火 / stdin EOF / stdin read
  error のいずれでも `OUTER_TTY_RESET` を吐く (= 2026-07-24 H4 の対策を維持)
- **`docs/issue/2026-07-20-detach-key-not-firing-keyboard-protocol.md` は機能ごと廃止で
  解消**。keyboard protocol と Ctrl+Z の相互作用 (= CSI-u 有効端末で 0x1a が単一 byte
  で来るか) は残る観測課題だが、detach 前に client が `\x1b[<u` を吐く現行 reset とは
  別問題として扱う
- **検証要件 (DR-0014 マトリクス)**:
  - 連打 1/2/3/4/5 × (子へ届く Ctrl+Z 数, detach 有無) を state machine unit test で網羅
  - 窓延長 / 他キー割り込み / `delay=0` / `guard=false` / poll timeout も unit test
  - 実機 (macOS / release 0.9.18、pty harness、config 不在 = default 500ms):

    | 操作 | attach client | 子 (`/bin/cat`) |
    |---|---|---|
    | Ctrl+Z 単発 | exit 0 (= Detached、`OUTER_TTY_RESET` を送出) | live のまま |
    | Ctrl+Z 2 連打 | 繋がったまま (= 5s 以上 `0:rw*` を維持)、最下行に停止通知 | stopped |
    | Ctrl+Z 3 連打 | exit 0 (= Detached) | stopped |
    | `ctrlz_guard = false` + 単発 | 繋がったまま | stopped (= 素通し) |
    | `ctrlz_guard_delay = "0"` + 単発 | exit 0 | live のまま |
    | `ctrlz_guard_delay = "2s"` + 2 連打 | 繋がったまま | stopped |

  - 3 category (line-oriented / interactive REPL / TUI alt screen) での追試は
    dogfooding で継続 (= 本 DR 時点では `cat` / `bash -i` で確認。対話 bash は
    job control shell として SIGTSTP を自分では受けないため、子の停止確認には
    `cat` を使う)
  - 停止中の子を正規手順で起こす経路 (= 通知行が案内する
    `hyoui kill <s> --signal=CONT --no-terminate`): 実行すると attach client が全員
    切断される bug があったため 2026-07-25 に修正した (= `kill_command` が terminate 用の
    `detach_others: true` を非 terminate 経路にも効かせていた、
    docs/issue/2026-07-21-sigcont-alive-child-session-vanish.md)。修正後は rw leader が
    繋がったまま子が resume することを実機で確認済。ただし `child_stopped` フラグは
    resume 後も下りない別 bug が残る (docs/issue/2026-06-12-bug-child-stopped-flag-not-cleared.md、
    表示のみの問題)

## 関連

- [[DR-0005]] — 思想 (= in-band escape 例外条項を本 DR で撤回、原則は「子から見た透過」に狭めて維持)
- [[DR-0017]] — session anchor + suspend policy (= §柱2 の client follow を本 DR で撤回、
  session anchor 化 (柱1) と「daemon が勝手に起こさない」は維持)
- [[DR-0019]] — daemon 側 auto-resume policy (= 配置は不変、config default を追加)
- [[DR-0020]] — 発見性ヒント (= 文言を Ctrl+Z ガードに更新)、「Ctrl-Z 追加キー intercept」
  却下判断を本 DR で再判断
- [[DR-0024]] — config.toml 機構 (= `[attach]` / `[session]` で相乗り)
- [[DR-0026]] — 本 DR が Supersede
- docs/issue/2026-07-24-bug-tstp-intercept-followups.md — 実測の起点 (H1/H2)
- docs/QUESTIONS.md SUSP-Q1..Q3 (2026-07-25 裁定)
