# DR-0030: rw attach 中は子を停止させたままにしない — 停止を維持できるのは無人時だけ

- Status: Active
- Date: 2026-07-29
- Related: DR-0005 (思想 — 透明性最優先), DR-0017 (notify-only default — 「daemon が勝手に起こさない」は無人時の規定として維持), DR-0019 (daemon 側 auto-resume policy — §3 の client 側配置却下の射程を本 DR で限定), DR-0026 (reattach resume の原典 — DR-0029 が Supersede), DR-0029 (attach は覗き窓 — §5 の発火点を本 DR で拡張)
- Origin: docs/QUESTIONS.md 👺RS-Q1 (2026-07-29 kawaz 裁定)、docs/issue/2026-07-26-bug-ignored-tests-job-permanently-red.md §(B)

## 原則: 覗き窓が開いている間、子は走り続ける

**rw attach client が存在する間、hyoui は子を停止させたままにしない。**
子が停止したまま留まれるのは、誰も rw attach していない時だけである。

[[DR-0029]] が確定した「attach は覗き窓であり、client 操作で子を止めない」の対偶にあたる。
覗き窓を開けている人間は子を操作するために開けているので、そこに「見えているが一切操作を
受け付けない子」が居る状態は、覗き窓としての目的を満たしていない。

## Context

[[DR-0019]] §3 は `on-child-suspend` の default を `notify` (= daemon は勝手に起こさない)
と定め、[[DR-0029]] §5 は `[attach] resume_on_reattach = true` (= stopped child への rw
attach 時に resume 要求) を定めた。両者は別々には整合していたが、
**`hyoui run` は [[DR-0015]] で「fork daemon + attach client」の合成**であるため、
run 経路では「daemon は notify を守るが同居 client が起こす」という、どちらの DR にも
書かれていない観測挙動が発生していた。

この齟齬は CI テスト `notify_default_does_not_resume_self_stopped_child` の決定的失敗
として表面化し、リリースブロッカーになっていた (= 詳細は
docs/issue/2026-07-26-bug-ignored-tests-job-permanently-red.md §(B))。

実機で切り分けた結果、齟齬は「起こしすぎ」ではなく **「起こす場面が足りない」** ことが
判明した (2026-07-29 実測、macOS / PTY harness):

| 子が stop した時点 | 修正前の挙動 |
|---|---|
| attach 成立**前** (= handshake snapshot が stopped) | 起こす ([[DR-0029]] §5 の経路) |
| attach 成立**後** (= 走行中に self-stop) | **起こさない** (= 最下行に停止通知を出すだけ) |

後者が kawaz のドッグフーディングで害として観測されていた事象そのものである。

## Decision

### 1. rw attach 中の子 stop も resume 要求の trigger にする

[[DR-0029]] §5 の発火点を 1 つから 2 つに拡張する。どちらも既存の
`SessionChildResumeRequest` を送るだけで、**新しい protocol message / cap flag /
daemon state は足さない**:

1. attach 成立時に handshake snapshot が `child_stopped` (= 既存、[[DR-0029]] §5)
2. attach 中に `SessionChildStoppedNotify` を受信 (= 本 DR で追加)

判定条件は両者で同一なので、`client::should_resume_stopped_child(mode, config)` に
集約して 2 つの call site から使う (= 条件が片方だけ drift するのを構造的に防ぐ)。
`ro` / `rw-no-leader` が起こさないのは [[DR-0029]] §5 のまま不変。

2 の経路で resume を送る場合、[[DR-0029]] §1 の「子プロセスが停止中」通知行は
**描かない**。起こすと決めた直後に「停止中 — 再開するには…」と案内するのは矛盾で、
子の再描画で消えるだけの視覚ノイズになる。resume 要求の送信に失敗した場合は
従来どおり通知行を描く (= 起こせなかった事実は隠さない)。

### 2. config key を `[attach] resume_stopped_child` に改名

> **📌 注記 ([[DR-0032]]、2026-07-30)**: 本 key は `[session] auto_resume` と統合され、
> `[session] on_child_suspend` (enum 3 値) に置換された。旧 `true` は
> `auto_resume_on_attached` (= default)、旧 `false` は `show_child_action_menu` に対応する。
> 旧 key が config に残っていると起動を拒否して移行先を案内する (= silent 無視しない)。
> 本節の命名判断 (= 発火点ではなく保証を名前にする) は enum 値名にも引き継がれている。

```toml
[attach]
resume_stopped_child = true   # rw attach 中は子を停止させたままにしない
```

旧名 `resume_on_reattach` は「再 attach 時」という発火点を名前に埋め込んでおり、
発火点が 2 つになった時点で実態を誤って伝える。利用者が認識する概念は発火点ではなく
**「rw attach 中は子が止まったままにならない」** という保証なので、それを名前にする
([[DR-0005]] の透明性 = 利用者に内部モデルの学習を強いない)。

`hyoui` は v1.0 未満なので互換 alias は置かない (= 旧名は unknown field として無視され、
default `true` に倒れる。既定値が同じなので旧 config でも挙動は変わらない)。

### 3. 「停止を維持できるのは無人時だけ」を受け入れる

本 DR の帰結として、**rw attach 中は子を停止状態に留めておけない**。
[[DR-0029]] §2 の Ctrl+Z 2 連打で子に SIGTSTP が 1 発届く挙動、および
attach 中の `hyoui kill --signal=TSTP` は、いずれも **届いた直後に resume される**。
子を止めたまま置きたい場合は、`hyoui detach` してから (= 誰も rw attach していない状態で)
`hyoui kill --signal=TSTP` を使う。

kawaz 裁定 (2026-07-29) の原文要点:

> 現実問題、誰もアタッチしてない時以外は stop できないで良い。本気で止めたいなら kill
> するだろうし。それ以上に TUI アプリを SHELL 挟まず hyoui 直で起動した時、アタッチ
> してるのに一切の操作が効かなくなる害ばかりが目立つ。逆パターンで困ることはかつてゼロ。

「attach 中に子を止め続けたい」という要求は実運用で一度も観測されていない一方、
「attach しているのに操作が効かない」害は繰り返し観測されている。観測された害を
優先し、観測されていない要求のために前者を残すことはしない ([[DR-0014]] 検証主義)。

### 4. [[DR-0019]] §3 の「勝手に起こさない」は無人時の規定として維持

[[DR-0019]] §3 が client 側 auto-resume を却下した理由は
「**auto-resume が本当に必要なのは誰も attach していない時**であり、client 側配置では
無人時に発動できない」であった。この論拠は `[session] auto_resume` (= daemon policy)
に対しては今も有効で、本 DR は **その射程を侵さない**:

- **無人時** (= rw client 0): daemon policy が唯一の発動者。default は `notify`
  (= 起こさない) のまま不変。起こしたければ `--on-child-suspend=auto-resume` /
  `[session] auto_resume = true`
- **有人時** (= rw client あり): 本 DR の client 側 resume が発動する。
  「client 側では無人時に発動できない」という却下理由は、**発動条件が
  「client が居ること」そのものである本経路には当たらない**

つまり 2 経路は競合ではなく補完関係にあり、daemon policy を有人時にも効かせる形での
統合 (= daemon が client 数を見て resume する) は採らない。daemon に client 数依存の
policy 分岐を持たせるより、「起こしたい主体が起こす」方が責務として素直で、
必要な state も少ない ([[DR-0014]] 最小介入)。

## Rejected alternatives

### `resume_on_reattach` の default を `false` にする (= RS-Q1 案 c)

CI テストは通るが、`hyoui run -- <TUI>` で子が self-stop した瞬間に操作不能になる害が
default に残る。裁定が明示的に否定した方向。

### `run` が内部生成する attach では resume を適用しない (= RS-Q1 案 a)

「`run` は起動であって復帰意思の表明ではない」という理屈は成立するが、利用者から見ると
`hyoui run -- vim` と `hyoui run --detached -- vim` + `hyoui attach` は
**同じ「窓が開いている状態」**であり、前者だけ子が固まるのは説明できない差異になる。
[[DR-0015]] が `run` を「fork + attach の合成」と定義した以上、合成された attach は
attach として振る舞うのが一貫している。

### daemon 側で「rw client が居たら SIGCONT」を判定する

発動点を daemon に寄せれば経路が 1 本になるが、daemon が client 数と mode を見て
policy を分岐する state を背負う。§4 のとおり「起こしたい主体が起こす」方が責務として
素直で、既存 `SessionChildResumeRequest` の再利用だけで済む。

## Consequences

- **breaking change (v0.x なので許容)**: config key `[attach] resume_on_reattach` が
  `resume_stopped_child` に改名。旧名は無視され default `true` に倒れる (= 既定値が
  同じなので明示的に `false` にしていた利用者だけが影響を受ける)
- **[[DR-0029]] §2 の Ctrl+Z 連打表のうち「子へ届く Ctrl+Z」は、attach 中は
  「届くが即 resume される」** に実効が変わる。client 側 (= 単発で client suspend /
  2 連打では suspend しない) の規則は不変
- `child_stopped` フラグが resume 後も下りない既知バグ
  (docs/issue/2026-06-12-bug-child-stopped-flag-not-cleared.md) は本 DR で解消しない。
  `hyoui status` の `child-state: stopped` が実態と食い違って見える場面が増える
  (= 表示のみの問題、実機で子が running であることは `ps` で確認済み)
- **検証 (2026-07-29、macOS / 実 PTY)**:

  | ケース | 結果 |
  |---|---|
  | attach 成立前に self-stop (`kill -STOP $$; echo MARKER`) | resume される (MARKER 出力) |
  | attach 成立後に self-stop (`sleep 2` 後に stop) | resume される (redraw + MARKER 出力、3/3) |
  | 停止中の子の実プロセス状態 | `ps` で `SN+` = running を確認 |
  | `--on-child-suspend=auto-resume` (daemon policy) | 従来どおり resume される |
  | ro / rw-no-leader は起こさない | unit test (`only_rw_attach_resumes_stopped_child`) |
  | `resume_stopped_child = false` で opt-out | unit test (`resume_stopped_child_false_opts_out_even_for_rw`) |

  e2e (`jobcontrol_auto_resume`) は 3 連続 green。ただし当該 PTY harness は本 DR と
  無関係に macOS で flaky (= 未変更の `auto_resume_resumes_self_stopped_child` 単体でも
  8 回中 2 回、出力 0 bytes で timeout する)。harness 側の課題は
  docs/issue/2026-07-03-bug-macos-ci-flaky-pty-tests.md の射程

## 関連

- [[DR-0015]] — `run` = fork daemon + attach client の合成 (= 齟齬の構造的な出どころ)
- [[DR-0019]] — daemon 側 auto-resume policy (= §3 の却下理由の射程を本 DR §4 で限定)
- [[DR-0029]] — attach は覗き窓 (= §5 の発火点を本 DR §1 で拡張、§1 の通知行は resume
  しない場合に限定)
- docs/issue/2026-07-26-bug-ignored-tests-job-permanently-red.md — 発見の起点 §(B)
