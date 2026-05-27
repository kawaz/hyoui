# DR-0001 軸 1/2 実装漏れの経緯とレビュー process 盲点の徹底調査

- Date: 2026-05-27
- Scope: DR-0001 (= 2026-05-21 起票、6 日経過) 軸 1/2 が現在まで **CLI flag は parse されるが daemon/agent に配線されていない** 事象の経緯と、過去 4 ラウンドのレビュー (R1-R5) で誰一人指摘しなかった構造的理由
- 関連 task: #38 (= 本調査)、#34 (= 撤退議論を白紙に戻して実装に進む候補)
- 関連 DR: [DR-0001](../decisions/DR-0001-bgfg-jobcontrol-two-axis.md), [DR-0003](../decisions/DR-0003-rust-only-and-forkpty-login_tty.md), [DR-0014](../decisions/DR-0014-transparency-and-empirical-verification.md)
- 関連 findings: [jobcontrol-matrix-verification](2026-05-27-jobcontrol-matrix-verification.md), [self-audit-after-dr-0014](2026-05-27-self-audit-after-dr-0014.md), [dr-0001-axis1-retreat-discussion](2026-05-27-dr-0001-axis1-retreat-discussion.md), [sigtstp-cargo-test-vs-prod](2026-05-22-sigtstp-cargo-test-vs-prod.md)

## 要約 (= 結論先出し)

### 経緯の核心 3 点

1. **poc3/shimux の nosuspend は実装極小・動作確認済み・production 投入済み**。MoonBit 46 行 + C stub 47 行、ロジック本体は `posix_spawn + waitpid(WUNTRACED) → kill(child, SIGCONT)` の 4 行 (`shimux/poc003/cmd/nosuspend/native_stub.c` L41-46)。Ghostty surface に組み込まれ shell 無しで claude を suspend → 即復帰する用途で実用されていた。設計のみではなく動作実証済みの完成品。
2. **Rust 一本化 (DR-0003、2026-05-22) 時点で実装漏れの種が認知されていた**が、誰も拾わなかった。`journal/2026-05-22-rust-rewrite.md` の残課題 (73 行目) に明文化:
   > "**SIGTSTP の cargo test 環境特有挙動**: ... 本番 binary での SIGTSTP delivery 動作確認 (smoke) は未実施。段階 4 の agent イベントループで parent 側 SIGTSTP re-raise 経路を本番で叩く必要が出たら検証"
3. **2026-05-26 cli-design-discussion で「(c) DR-0001 未実装オプション」と明示されていた**が、ユーザの「a の設計を」発言で機能設計フェーズに流れ、軸 1/2 配線は task list 上に上がらないまま 5 日経過。`journal/2026-05-26-cli-design-discussion.md` L12 で本人が「DR-0001 未実装」と書いていながら、その後の R4 (8 personas) / R5 (8 personas) の合計 16 レビュアーが誰も「flag parse → 配線無し」を指摘しなかった。

### レビュー盲点の top 3

1. **「DR で justify された機能が実装されているか」を見る観点が R4/R5 のどの persona prompt にも存在しなかった** — レビュー対象は「現コードの correctness / security / perf / API」であり、「現コードに**無い**もの」を発見する観点がレビューの構造上ない。dead code 検出は得意 (R5-M30 で `observer.rs` を削除指摘した) だが、missing code 検出のための index (= DR 一覧との突き合わせ) を持たない。
2. **CLI flag は parse されていたため「OnChildSuspend enum がある = 機能がある」と暗黙に推認**された。テスト (= `cli.rs` の `assert_eq!(cfg.on_child_suspend, OnChildSuspend::Follow)`) も flag が config 構造体に到達することだけ確認しており、その config が daemon に流れて signal handler の挙動を変えるかは検証していなかった。
3. **マトリクス検証が「task #32」として後回しになっていた**ことで実機検証フェーズが存在せず、cargo test pass = 動作確認とみなされた。R4-H14 (= `child_actually_exited` の Stopped/Continued 未区別) や R5-H6 (= SIGCHLD self-pipe 不在) のような「signal 周りの個別箇所」は指摘されたが、それらは「現コードの不備」であり、「DR-0001 軸 1/2 そのものの未実装」とは別軸で扱われた。

### 提案 (= 詳細は §6, §7)

- **DR-0014 §self-check に第 6 項目追加**: 「対極のチェック — DR で justify された機能のうち、未実装のものはないか?」
- **`docs/decisions/INDEX.md` に実装状況列追加**: 各 DR を `[実装済|部分実装|未実装|撤退]` でラベル付け、grep で発見可能化
- **`itumono-review-*` skill に DR-vs-impl 観点 persona 追加**: 既存 persona は「対象 commit を critique」、新 persona は「INDEX を読み、各 DR の実装エビデンスを grep で確認」を専任
- **DR 起票時に impl task 同時起票を ritual 化**: DR-NNNN 起票 → 即 `docs/issue/YYYY-MM-DD-impl-DR-NNNN.md` 起票 (= 解決時に削除、未解決なら 1 grep で発見)
- **task #34 を撤退議論から「実装」に再設定**: §8 参照

---

## 1. poc3/shimux の nosuspend 実装の所在

### 1.1 物理位置

| path | content | size |
|---|---|---|
| `~/.local/share/repos/github.com/kawaz/shimux/poc003/cmd/nosuspend/nosuspend.mbt` | MoonBit エントリーポイント (= main + shell quote helper) | 46 行 |
| `~/.local/share/repos/github.com/kawaz/shimux/poc003/cmd/nosuspend/native_stub.c` | C stub (= posix_spawn + waitpid loop) | 47 行 |
| `~/.local/share/repos/github.com/kawaz/shimux/poc003/cmd/nosuspend/moon.pkg` | MoonBit package manifest | 数行 |
| `~/.local/share/repos/github.com/kawaz/shimux/main/cmd/nosuspend/{nosuspend.mbt,native_stub.c,moon.pkg}` | main branch にも同一ファイル | 同上 |

shimux リポは poc003 と main の 2 workspace 体制で、両方に nosuspend が現存している (現役 + 保全)。

### 1.2 ロジック本体 (= `native_stub.c` L18-47 抜粋)

```c
/* posix_spawn + signal forwarding + SIGTSTP auto-resume.
   cmd is executed via /bin/sh -c. Returns child exit code. */
MOONBIT_FFI_EXPORT
int32_t nosuspend_run(moonbit_bytes_t cmd) {
    const char *argv[] = {"/bin/sh", "-c", (const char *)cmd, NULL};

    posix_spawnattr_t attr;
    posix_spawnattr_init(&attr);
    /* Reset all signals to default in the spawned child */
    sigset_t all_sigs;
    sigfillset(&all_sigs);
    posix_spawnattr_setsigdefault(&attr, &all_sigs);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETSIGDEF);

    int err = posix_spawn(&ns_child, "/bin/sh", NULL, &attr,
                          (char *const *)argv, environ);
    posix_spawnattr_destroy(&attr);
    if (err != 0) return 1;

    signal(SIGTSTP, SIG_IGN);
    signal(SIGINT,  ns_forward);
    signal(SIGTERM, ns_forward);
    signal(SIGQUIT, ns_forward);
    signal(SIGHUP,  ns_forward);

    int st;
    for (;;) {
        if (waitpid(ns_child, &st, WUNTRACED) < 0) return 1;
        if (WIFSTOPPED(st))  { kill(ns_child, SIGCONT); continue; }
        if (WIFEXITED(st))   return WEXITSTATUS(st);
        if (WIFSIGNALED(st)) return 128 + WTERMSIG(st);
    }
}
```

DR-0001 §Context 引用「waitpid(WUNTRACED) → 即 SIGCONT」と完全一致。

### 1.3 動作確認状況 (= 実機投入されていた)

- **用途**: Ghostty (= macOS 用 terminal emulator) の surface configuration に `command = nosuspend claude` の形で直接設定。shell を介さず子 claude を直接起動する経路で、claude が `Claude Code has been suspended.` を print して `raise(SIGTSTP)` した時に shell が居ないため復帰経路が無い問題を解消するための実用ツール。
- **コメント本文の証言**: `nosuspend.mbt` L1-5
  > `nosuspend - 子プロセスが suspend されたら即座に SIGCONT する軽量ラッパー / Ghostty surface configuration の command で直接起動した場合、シェルがいないため Ctrl+Z で suspend すると fg で戻れない。このラッパーは子プロセスの SIGTSTP 停止を検知して即座に再開する。`
- **証跡**: DR-0001 §Context (hyoui 側) で kawaz が「shimux はこれを ... `nosuspend` ... で対処していた」と過去形で記述しており、PoC ではなく投入実績ある実装として参照されている。

### 1.4 hyoui の軸 1 `auto-resume` との対応

DR-0001 §軸 1 で:
> "`auto-resume`: 親が子（pgrp）へ即 `SIGCONT`。子の suspend を一切許さない。**poc3 時代の `nosuspend` 相当を内蔵したもの**。"

つまり nosuspend の核心 4 行 (`waitpid(WUNTRACED)` → `WIFSTOPPED` → `kill(SIGCONT)`) を hyoui daemon の SIGCHLD 経路に統合すれば軸 1 `auto-resume` が完成する。実装の難所はゼロ (= ロジックは 4 行)、現状の hyoui には `child_actually_exited` の waitpid 経路が既にあるため、その分岐に WIFSTOPPED ケースを足すだけ。

---

## 2. hyoui Rust 一本化で抜けた経緯

### 2.1 タイミング特定

| 日付 | event | 状態 |
|---|---|---|
| 2026-05-21 | DR-0001 起票 (= bootstrap commit `wkmvsqrq`) | 設計確定 |
| 2026-05-22 | Rust 一本化 6 段階完了、v0.0.0 release | **本番 binary での SIGTSTP smoke 未実施を残課題として記録** |
| 2026-05-25 | DR-0003 起票 (= Rust 一本化決定の正式化) | 残課題未消化 |
| 2026-05-26 | CLI design discussion セッション | **「(c) DR-0001 未実装オプション」が明示選択肢に上がるが、機能設計フェーズに流れる** |
| 2026-05-26 夜 | Round 1-2 レビュー (5 personas + gemini) | 軸 1/2 未指摘 |
| 2026-05-27 早朝 | Round 3 backlog 統合、Round 4 (8 personas)、Round 5 (8 personas) | 軸 1/2 未指摘 (R4-H14 / R5-H6 は別軸の signal 周りで close） |
| 2026-05-27 夜 | task #32 matrix 検証で **初めて「軸 1/2 が flag parse のみで配線無し」を発見** | 6 日経過 |

### 2.2 責任 commit (= 漏れを生んだ commit)

- **`OnChildSuspend` / `OnParentSuspend` enum 追加 commit**: `cli.rs` 履歴は `change_id` レベルでは 1 ファイルが何度も触られているため特定困難だが、Rust 一本化の 6 段階のうち **段階 4 (= agent イベントループ)** がその責任ゾーン。
- **`journal/2026-05-22-rust-rewrite.md` L73** で kawaz が手書きで残した残課題:
  > "段階 4 の agent イベントループで parent 側 SIGTSTP re-raise 経路を本番で叩く必要が出たら検証"

つまり「ut/test では SIGTSTP が cargo test 環境で discard される」を回避するため SIGSTOP テストに切り替えた → **本番経路の smoke を後回しにした** → そのまま忘れた、という flow。flag は CLI 層に置かれたが、daemon 側で消費される配線は段階 4 で「テスト不能なので保留」のまま release され、その後の R4/R5 レビューで掘り起こされなかった。

### 2.3 現状の grep 確認

```text
grep -rn 'on_child_suspend\|on_parent_suspend\|OnChildSuspend\|OnParentSuspend' \
  crates/hyoui/src/daemon/ crates/hyoui/src/agent/ crates/hyoui-cli/src/
# → 0 hit (= daemon/agent には全く伝わっていない)
```

`cli.rs` の `RunConfig` に格納されるところまでで配線が止まっており、`hyoui-cli/src/main.rs::run_command` が daemon 起動時に `DaemonConfig` に渡す経路にも乗っていない (= 該当 field が `DaemonConfig` 側に存在しない)。

---

## 3. 過去レビューで見逃された理由 (= 構造的問題)

### 3.1 R4/R5 レビュー観点の盲点

| 観点 | レビュー対象 | DR-0001 軸 1/2 が捕まる? |
|---|---|---|
| Architect (R4) | 責務分離 / refactor 余地 | × (= 配線「ある」ものを評価する) |
| Wild debugger (R4) | edge case / race / overflow | × (= 配線「ある」signal 経路を評価) |
| Kernel (R5) | syscall portability / signal mask | △ (= R5-H6 で SIGCHLD self-pipe 不在を指摘したが、これは「現実装の latency」観点であり「軸 1/2 そのものの未配線」とは別) |
| Formal (R5) | type 不変量 / debug_assert | × (= type 上 `OnChildSuspend` は存在するため不変量違反として捕まらない) |
| Audit (R5) | security 脆弱性 | × |
| Perf (R5) | atomics / alloc / SIMD | × |
| POSIX (R5) | portability / standard 準拠 | × (R5-C4 は signal **wire 形式**、軸 1/2 配線ではない) |
| Sales (R5) | README / installation / persona | × |
| Classic (R5) | 古典派 / Unix philosophy | × (= 軸 1/2 自体は古典派的、Unix shell job control の延長線) |
| Test 戦略 (R4) | test coverage | × (= cli.rs の flag parse test は pass しており「test 通ってる = OK」と判定された) |
| 新人 UX (R4) | --help が出るか、エラー文言が読めるか | × (= `--on-child-suspend=follow` を打って何も起きなくても、子が走っていれば一見正常に見える) |
| Roadmap (R4) | v0.2.0 機能不足 | × (= v0.2.0 で何を**追加**するかが対象、v0.1.x で**未実装**のものは対象外) |
| Competitive (R4) | tmux 比較 / 差別化 | × |
| Rust API (R4) | non_exhaustive / Debug derive | × (= `OnChildSuspend` 自体は `#[non_exhaustive]` 等の API 観点でレビュー対象、配線の有無は対象外) |
| DR-docs (R4) | DR と README の整合 | △ (= DR に書いてあるか / README に書いてあるか、は見るが「DR と code の整合」は見ない) |

つまり **全 16 persona の観点を合算しても「DR と code の整合」を見る視点が存在しなかった**。これは個別 persona の落ち度ではなく、レビュー framework として `DR ↔ code` の双方向整合性を確認する persona を仕込んでいなかった。

### 3.2 「DR-docs」persona の限界

R4 で唯一近い persona は「DR-docs」だったが、その担務は:
- DR が書かれているか
- DR が README / DESIGN に反映されているか
- DR-0007 の re-scope 等、文書の論理整合

であり、**「DR で書かれた機能が code に存在するか」を grep で確認する責務は含まれていなかった**。R4-C2 (DR-0007 re-scope) や R4-C5 (DESIGN/ROADMAP 新設) はその範囲で見つけた指摘。

### 3.3 self-audit (= task #33) でも見逃した理由

私 (= Claude) が 2026-05-27 に書いた `self-audit-after-dr-0014.md` (= task #33) は **「nonstop session 45 commit 中で新規介入が適切か」** が主軸:

- self-check 5 項目 (`DR-0014`) は全て **「新規介入を入れる」場面**用 — 「DR justify 済?」「透過原則を破る必然?」「最小介入?」「再発明否?」「新 protocol 必然性 DR 化?」
- 全項目の方向性が **「やる側」のチェック** であり、「やってないことが正当か」を見る項目がない
- 結果として 14 item 全てを A/B/C/D 区分したが、「軸 1/2 が未配線」は audit 対象範囲外として暗黙 skip された
- DR-0014 §Consequences §即時的影響 で kawaz が:
  > "DR-0001 軸 1/2 の **未実装** は本 DR で禁則とした「推測実装」ではなく、設計済 DR に従った正規実装。実装は別 task で進めるが、進める前にマトリクス検証で現状把握を完了する"

  と書いていながら、self-audit 文書自体には「軸 1/2 未実装」が監査項目として一切登場しなかった。これは DR-0014 §self-check の構造が「新規介入のレビュー」用にできており、「設計済の未実装」を拾う構造になっていなかったため。

---

## 4. 私 (= Claude) の連鎖ミス整理

task #32 マトリクス検証で軸 1/2 未実装を発見した後、task #34 着手フェーズで以下の連鎖ミスがあった (= peer agent からの撤退案を入力として):

### 4.1 ① 一次仮説 (= 「実装漏れ、配線すれば直る」) は正しかった

DR-0001 軸 1/2 の配線追加 (= `OnChildSuspend::AutoResume` → daemon SIGCHLD 経路で WIFSTOPPED → `kill(child, SIGCONT)`) は poc3/nosuspend で実証済みの 4 行ロジック。複雑度はゼロ。

### 4.2 ② orphan group 発見ですり替え

peer agent (= cmux-msg + csa 検証セッション) が「POSIX §3.107 で orphan process group への SIGTSTP は kernel discard される」と指摘。
これは事実だが **適用範囲を読み違えた**:

- orphan group discard が効くのは **`raise(SIGTSTP)` (= 子自身が自分に送る)** のケース
- 外部から `kill -STOP <pid>` で送る場合は SIGSTOP (catch 不可、orphan 関係なし) で確実に届く
- hyoui daemon が **子に SIGCONT を送る側** は orphan 関係なく届く
- したがって軸 1 `auto-resume` の「子が STOPPED 状態になったら親が SIGCONT で復帰」は **送る側の話**で、orphan group discard は無関係

私はこれを「軸 1 の前提崩壊」と誤判断し、撤退案を peer から受けて議論に流された。

### 4.3 ③ 「不可能」判断

orphan group discard を「軸 1 が成立しない」根拠と誤認し、`dr-0001-axis1-retreat-discussion.md` を書いた。実態は:
- 軸 1 `auto-resume`: **送信側ロジックで完結、orphan 無関係**
- 軸 1 `follow`: 親が自分に `raise(SIGSTOP)` する話で、これも子の orphan 関係なし
- 軸 2 `transparent`: 親 SIGTSTP 受信時に子 pgrp に `killpg(child, SIGSTOP)` を送る、orphan 無関係
- 軸 2 `decouple`: 何もしない、orphan 無関係

つまり **4 つのうちどれも orphan group discard の影響を受けない**。受けるのは「内側の shell が claude を job として認識して `fg` で復帰」のような **子セッション内の job control の話** だけで、これは hyoui の責任範囲外 (= 子の中の shell の仕事)。

### 4.4 ④ 撤退案推奨

①〜③ の連鎖で「軸 1 撤退」を kawaz に提案する文書 (= `dr-0001-axis1-retreat-discussion.md`) を書き、`zmzzrzputtrp` で「neutralize DR-0001 axis-1 finding (peer view is headless-only, decision pending kawaz)」と finding を中立化する commit を入れた。これは kawaz が「peer 視点は headless 中心」と指摘して止めてくれたから留まったが、止められていなければ DR-0001 を改訂して軸 1 撤退の方向に進んでいた。

### 4.5 各段階の self-check 漏れ

- ②: 「orphan group discard が実際にどの送受信パターンで効くか」を自前で実機 probe せず、peer agent の結論をそのまま受けた (= DR-0014 §検証主義「サンプル 1 で結論」violation)
- ③: 「不可能」判断時に poc3/nosuspend が実際に同じ問題を解決していたという過去事例を引かなかった (= 過去 finding / 過去実装の参照漏れ)
- ④: 撤退提案時に「DR-0001 起票時の議論経緯 (= bootstrap journal) を読み直す」を skip (= DR の Context section の §不採用にした案・判断の修正 を読み込み直していれば「軸 1 follow を一度却下したが復活させた」議論経緯が見えたはず)

---

## 5. DR-0014 強化案 (= 具体的 diff 提案)

### 5.1 §self-check リストに第 6 項目追加

現在の self-check (= 5 項目 + partial state) に **対極の項目**を追加:

```markdown
- [ ] **既存 DR で justify された機能のうち、未実装のものはないか?**
  (= 自分の現タスクが直接対象でなくとも、`docs/decisions/INDEX.md` の
  「実装状況」列を眺めて、`未実装` / `部分実装` ラベルのものが
  今のタスクで一緒に解決できないかを 30 秒で確認する。
  特に「介入を入れる」タスクでは、その介入と同種の DR が既に
  justify 済の未実装機能を含んでいないかを必ず grep)
```

理由: 現 5 項目は **「新規介入の妥当性」だけ**を見ており、対極の **「設計済の未実装」を発見する仕掛けがない**。本 finding の事象はこの構造的欠落の典型。

### 5.2 §検証主義に「コードと DR の双方向整合性」節を新設

新節案 (= §検証主義 と §道具揃った段階の運用 の間に挿入):

```markdown
### コードと DR の双方向整合性

レビュー / 修正の最低 1 巡に 1 回、以下の双方向 grep を行う:

**A. 「DR に書いてあるが code にないもの」**:
- `docs/decisions/INDEX.md` の各 DR を眺め、`未実装` ラベルが付いた DR は code に対応する箇所が grep で出るかを確認
- 出ない場合、issue 起票 or 既存 issue を確認
- 例: DR-0001 軸 1 `auto-resume` → `grep -r 'OnChildSuspend::AutoResume' crates/hyoui/src/daemon/` で 0 hit なら未配線

**B. 「code にあるが DR に書いていないもの」**:
- 主要 module の機能 (= public API、CLI flag、protocol message) を眺め、対応する justify DR が引けない場合は新規 DR 起票 or 既存 DR への annotate
- 例: `--on-child-suspend` CLI flag が parse される → DR-0001 で justify されているか確認
```

### 5.3 §Anti-patterns に項目追加

```markdown
5. **「DR 起票で満足、実装は別 task」と先送りした後で忘れる**
   = 設計議論で DR を起票したら気が済んでしまい、対応する実装が
   release pipeline に乗らないまま週単位で放置される。
   対策: DR-NNNN 起票時に即 `docs/issue/YYYY-MM-DD-impl-DR-NNNN.md` を
   起票する ritual を入れ、grep で未解決 issue を発見可能にする。
   本事象 (= 本 finding) はこの anti-pattern が 6 日続いた典型例。
```

### 5.4 CLAUDE.md (= project root) への補強

「介入判断 self-check」セクションに 1 項目追加:

```markdown
- [ ] **対極のチェック**: `docs/decisions/INDEX.md` で未実装 DR を眺め、
  今の修正タスクで近接する未実装 DR がないか確認する (= 「やる」だけでなく
  「やってない」を 30 秒見る)
```

---

## 6. レビュー process 改善案 (= 具体的 task として提案)

### 6.1 `docs/decisions/INDEX.md` に実装状況列追加

各 DR を 4 値ラベル:

| ラベル | 意味 | 移行 trigger |
|---|---|---|
| `実装済` | DR で justify された機能が code に存在し、マトリクス検証で動作確認済 | matrix test が pass |
| `部分実装` | 一部 (= 設計のうち何か) は code にあるが、未配線 / 未テスト箇所がある | 残部分の issue 起票必須 |
| `未実装` | 設計のみで code 側に存在しない | 必ず impl issue を持つ |
| `撤退` | 設計後に撤退判断、code には反映しない | 撤退理由 DR の cross-ref 必須 |

DR-0001 は現在 **`部分実装`** ラベルが正しい (= flag parse は実装、daemon 配線は未実装)。

### 6.2 `itumono-review-*` skill に新 persona 追加

既存 8 personas は「対象 commit の critique」だが、新 persona は INDEX 起点:

```text
persona: "DR Implementation Auditor"
prompt: |
  `docs/decisions/INDEX.md` を読み込め。各 DR について:
  1. ラベルが `実装済` / `部分実装` / `未実装` / `撤退` のどれか
  2. ラベルが `実装済` の場合、その justify された機能が code に
     対応する箇所 (= function / module / flag) を grep で 1 箇所以上発見できるか
  3. 発見できない場合、INDEX のラベルが誤りである可能性を critical 指摘
  4. ラベルが `部分実装` / `未実装` の場合、対応する `docs/issue/` が
     存在するか確認、無ければ critical 指摘
  特に「flag が parse されるが daemon/agent に配線されていない」型の
  漏れに敏感であれ (= hyoui 過去事例 DR-0001 軸 1/2)。
```

### 6.3 DR 起票時の impl task 自動起票 ritual

DR 起票プロセスを 2 step 化:

1. **`docs/decisions/DR-NNNN-title.md` を新規作成** (= 従来通り)
2. **`docs/issue/YYYY-MM-DD-impl-DR-NNNN.md` を同 change で作成**
   - 内容: DR-NNNN が justify する具体機能のリスト + 配線箇所 (予測) + マトリクス検証セル
   - 解決時に削除 (= jj/git 履歴で追える)
   - 未解決 issue は `grep -l "DR-NNNN" docs/issue/` で 1 hit すれば残存と判定

### 6.4 マトリクス検証を起票時 deliverable に含める

DR-NNNN の起票時に、対応する matrix test ファイル名を DR 本文に書き込む (= 後追いではなく事前):

```markdown
## 検証
- マトリクス test: `crates/hyoui-cli/tests/matrix_<dr-slug>.rs`
- セル数: app × mode × signal の組合せ = N cell
- 起票時 status: 未作成 → impl task で同時作成
```

これで「DR は書いたが test も実装もない」状態を起票時から可視化できる。

---

## 7. 直接的な改修対象: task #34 を実装に再設定

### 7.1 撤退議論の白紙化根拠

§4 で整理した私の連鎖ミスにより、撤退議論 (= `dr-0001-axis1-retreat-discussion.md`) は事実誤認 (= orphan group discard の適用範囲読み違え) を元にしている。撤退の根拠が崩れたので、撤退議論は白紙に戻す。

### 7.2 実装 path の再確認

task #34 の実装内容 (= `jobcontrol-matrix-verification.md` §task #34 着手時の手順 と同じ):

1. **`DaemonConfig` に `on_child_suspend` / `on_parent_suspend` field 追加** (= hyoui-cli/main.rs から渡す)
2. **daemon SIGCHLD 経路 (= `child_actually_exited` 周辺) に WIFSTOPPED 分岐追加**:
   - `OnChildSuspend::AutoResume`: `kill(child_pgrp, SIGCONT)` を送る (= nosuspend ロジック)
   - `OnChildSuspend::Follow`: 親自身に `raise(SIGSTOP)` を呼ぶ (= 親も止まる)
3. **daemon に SIGTSTP self-pipe 追加** (= `install_selfpipe_for(SIGTSTP)`):
   - `OnParentSuspend::Transparent`: 子 pgrp に `killpg(child, SIGSTOP)` を送ってから親も `raise(SIGSTOP)`
   - `OnParentSuspend::Decouple`: 親だけ raise、子は放置
4. **SIGCONT ハンドラに invariant 回復ロジック追加** (= DR-0001 §invariant 引用):
   - 親が再開した時、子が STOPPED なら必ず `SIGCONT` を送る
5. **マトリクス test 11 cell (= 軸 1: 6 + 軸 2: 5) の assert を期待動作側に反転**

### 7.3 nosuspend を直接参考にする

実装時の参考 code は `~/.local/share/repos/github.com/kawaz/shimux/poc003/cmd/nosuspend/native_stub.c` L18-47。ロジック本体 4 行は丸写し可。signal forwarding 部 (= INT/TERM/QUIT/HUP を子に送る) は hyoui で既に実装済なので重複を avoid。

### 7.4 検証 cell (= R5-H6 と統合)

R5-H6 (= SIGCHLD self-pipe 不在、現状 polling 経路のみ) と task #34 は **同一実装で同時解決**できる。SIGCHLD self-pipe を導入すれば軸 1 の WIFSTOPPED 検出 latency が ms オーダーに改善 + busy spin 撤廃も同時達成。

---

## 8. 関連 (= 参照したファイル全 path)

### DR
- `docs/decisions/DR-0001-bgfg-jobcontrol-two-axis.md` — 軸 1/2 設計の正本、§Context で nosuspend 由来を明示
- `docs/decisions/DR-0003-rust-only-and-forkpty-login_tty.md` — Rust 一本化、§Consequences §テスト で SIGSTOP 切替を記録
- `docs/decisions/DR-0005-design-philosophy-external-automation.md` — 外側自動操作主軸、透明性最優先
- `docs/decisions/DR-0014-transparency-and-empirical-verification.md` — self-check + 検証主義、本 finding の強化対象

### findings
- `docs/findings/2026-05-22-sigtstp-cargo-test-vs-prod.md` — cargo test 環境での SIGTSTP discard、smoke の本番動作確認 (= transparent が動いていたという報告)
- `docs/findings/2026-05-27-jobcontrol-matrix-verification.md` — 軸 1/2 未配線を初めて発見した検証
- `docs/findings/2026-05-27-dr-0001-axis1-retreat-discussion.md` — 私の連鎖ミスの産物、本 finding §4 で白紙化判断
- `docs/findings/2026-05-27-self-audit-after-dr-0014.md` — DR-0014 self-check による audit、§3.3 で見逃した理由

### journal
- `docs/journal/2026-05-22-rust-rewrite.md` — Rust 一本化記録、L73 で「smoke 未実施」明文化
- `docs/journal/2026-05-26-cli-design-discussion.md` — L12 で「(c) DR-0001 未実装オプション」明示

### REVIEW-BACKLOG
- `docs/REVIEW-BACKLOG.md` — R1-R5 全レビュー集約、軸 1/2 未配線への指摘ゼロ (= 関連 R4-H14 / R5-H6 は別軸)

### 外部リポ (= shimux)
- `~/.local/share/repos/github.com/kawaz/shimux/poc003/cmd/nosuspend/nosuspend.mbt` — MoonBit エントリ
- `~/.local/share/repos/github.com/kawaz/shimux/poc003/cmd/nosuspend/native_stub.c` — C stub、ロジック本体 4 行
- `~/.local/share/repos/github.com/kawaz/shimux/main/cmd/nosuspend/{nosuspend.mbt,native_stub.c}` — main branch にも同一ファイル現存

### code (= hyoui)
- `crates/hyoui/src/cli.rs` L50-60, L156-158, L1441-1605, L2179-2182, L3458-3491 — `OnChildSuspend` / `OnParentSuspend` enum + parse + test (= 配線 0 の方の側)
- `crates/hyoui/src/daemon/` — `on_child_suspend` / `on_parent_suspend` への参照ゼロ (= grep で確認済)
- `crates/hyoui-cli/tests/matrix_jobcontrol_axis1.rs` — 6 cell、現実態で pass
- `crates/hyoui-cli/tests/matrix_jobcontrol_axis2.rs` — 5 cell、現実態で pass
- `crates/hyoui-cli/tests/matrix_attach_restore.rs` — 4 cell、attach 復元の方は OK

---

## 9. 本 finding 自身への DR-0014 self-check 適用

DR-0014 §self-check を本 finding 作成プロセスに適用:

- [✓] **既存 DR で justify されているか**: 本 finding は doc only、code 介入なし。DR-0014 §self-check / §検証主義 / §Anti-patterns の補強提案は DR-0014 自身の枠内。
- [✓] **透過原則を破るか、必然か**: doc only、透過原則関係なし。
- [✓] **最小介入か**: 提案は self-check 1 項目追加 / INDEX 列追加 / persona 1 追加 / impl issue ritual 化、いずれも既存仕組みの拡張で新規 protocol / 新 message なし。
- [✓] **再発明していないか**: `docs/issue/` ritual は既存運用、INDEX 拡張も docs-knowledge-flow.md の既存規約延長。
- [✓] **新 protocol 不要**: doc only。
- [✓] **partial state を破棄する介入か**: 該当なし (= doc)。
- [✓] **対極のチェック (= 提案中の新項目)**: 本 finding 自体が「未実装機能の発見」プロセスを ritual 化する提案であり、提案中の項目を実践している。

### 検証の実証

本 finding §1 (poc3 nosuspend の所在) は `Read` で実コードを引用、§2 (Rust 一本化のタイミング) は `journal/2026-05-22-rust-rewrite.md` L73 を引用、§3 (レビュー盲点) は REVIEW-BACKLOG.md の全 95 件をスキャンして該当 0 件を確認、§4 (連鎖ミス) は私自身の commit 履歴 (`zmzzrzputtrp`, `xlppxmomzlnr`) を grep で確認。**推測のみで書いた箇所はない**。
