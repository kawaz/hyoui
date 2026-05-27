# DR-0014: 透過原則の徹底と検証主義 — 設計判断・修正判断の self-check

- Status: Active
- Date: 2026-05-27
- Related: DR-0001 (jobcontrol 2 軸 — 透過原則の起点), DR-0005 (思想再定義 — 透明性最優先), DR-0013 (screen state 正本化 — ドッグフーディング道具揃え)

## Context

2026-05-27 nonstop session 末で `hyoui run -- claude` の Ctrl-Z 挙動を実機検証し、
hyoui client が SIGTSTP を follow せず raw mode で生き残る bug を発見した。当初の修復方針として
「daemon が waitpid で子の Stopped を検出 → 新 control message `ChildStateChanged` で client に通知 →
client が `raise(SIGSTOP)`」という設計を提案し、agent を起動しかけたが、kawaz から以下の根本的な指摘が入った:

1. **「監視とかバカじゃねーの？基本このアプリは透過で動く」** — DR-0005 で確定済の「透明性最優先」
   思想に反する過剰な介入を、私 (= Claude) が新規 protocol まで発明して導入しようとした
2. **「テストはしたの？ vim / less でも検証した？」** — claude TUI のサンプル 1 で原因を断定、
   全パターン (interactive/headless × app × signal × 送信元) のマトリクス検証なしで結論
3. **「既存実装でも同種多発してないか再確認させて」** — 本 nonstop session 45 commit のうち、
   同じ「透過原則違反 / 推測実装 / マトリクス未検証」が他にも紛れている可能性

kawaz は同時に「順序は正しかった (= 道具がないと検証できないから、まず screen dump / snapshot 等の
道具を作る実装を頑張ったのは順序として正しい)」とも整理し、**今後は道具が揃った段階に応じた
設計思想と原則を再確認** することが必要 — と判断した。

本 DR はその「設計思想と原則の再確認」を **永続化** し、今後のあらゆる設計・修正判断時に
self-check として参照する正本とする。

## Decision

### 透過原則 (= DR-0005 再確認 + 強化)

hyoui は **PTY を介在させるが、その上の semantics には介入しない**。具体的に:

- **signal は透過**: kernel / PTY の line discipline が処理する。hyoui が「signal を見て独自に変換する」
  ことは禁則。例外は DR-0001 軸 1/2 で明示的に justify された介入 (= 子 self-SIGTSTP の follow/auto-resume、
  親 external SIGTSTP の transparent/decouple) のみ
- **TTY mode は透過**: client は実 tty を raw + ISIG なし、PTY slave は cooked (= 子が自分で raw 化する場合は
  子が cfmakeraw する)。hyoui が「TTY mode を勝手に書き換える」ことは禁則
- **bytes は透過**: stdin → PTY master → child の経路で bytes を変換しない (= in-band escape 一切なし、
  DR-0005 既定)。client → daemon は raw_data frame で素通し
- **必要最小限の介入のみ**: WINCH 配送 (= terminal size 変更通知)、screen state 観測 (= read-only)、
  attach 復元の redraw (= state 正本化の必然)、DR-0001 jobcontrol 2 軸 (= 透過では成立しないため
  明示的に介入する場面として justify 済) など、DR で justify されたものだけ

### 介入判断の self-check リスト

新規実装・修正で「介入する」コードを書く前に、以下を全て確認する。1 つでも No なら設計を疑え:

- [ ] **この介入は既存 DR で justify されているか?** (= 該当 DR を引用できるか)
- [ ] **透過原則を破るが、その理由は「必然」か?** (= 「便利」「あった方が親切」では透過原則優先)
- [ ] **最小介入か?** (= 同じ効果をより少ない介入で実現する選択肢はないか)
- [ ] **介入箇所が kernel / PTY / shell の標準機能でできることを再発明していないか?**
  (= 例: 子の SIGCHLD 受信は親プロセスの kernel 標準機能、新 protocol で監視 message を流すのは再発明)
- [ ] **新 protocol message を増やすなら、その必然性を DR に書けるか?** (= cap flag 追加は重い判断)

### 検証主義 (= 推測で実装しない、サンプル 1 で判断しない)

- **マトリクス検証**: 設計判断・bug 修正前に、関連する全組合せの実機検証を行う。
  「interactive/headless × app (vim / less / claude / cat / bash / python REPL 等) ×
  signal (Ctrl-Z / Ctrl-C / 外部 TSTP / 外部 INT / 外部 HUP) × 送信元 (子 self / 親 self / 外部)」
  のような表を埋め、「期待 vs 実態」で乖離を発見する
- **状態観測の手段**:
  - プロセス状態: `ps -o pid,ppid,pgid,sid,stat,comm`
  - TTY 状態: `stty -a < /dev/ttyXXX`、`stty -F /dev/ptmx`
  - hyoui の screen state: `hyoui screen dump <session> --format=ansi` / `hyoui screen snapshot <session>`
  - hyoui の出力 history: `hyoui tail <session> --last-bytes=N --since-strict`
  - shell jobs: `jobs -l`、`fg` / `bg` 復帰可否
- **サンプル数の原則**: 1 つの app (= 例: claude TUI) で見えた挙動を一般化しない。**最低 3 種類**
  (= TUI alt screen 系 / line-oriented 系 / interactive REPL 系 等の異なる category) で検証する

### 道具揃った段階の運用 (= ドッグフーディング)

DR-0013 完了で screen dump / snapshot / tail / wait 等の **観測道具が揃った**。これより前の段階では
推測で実装するしかなかったが、**以降は推測実装を禁止する**:

- 設計判断時: 既存実装を実機で動かし、screen dump / snapshot / tail / ps / stty で状態取って判断材料にする
- bug 報告時: 報告者の cast / ログを Read で精読し、出力 bytes 単位で何が起きたか追う
- 修正実装後: 必ず実機で動作確認 + マトリクスの該当セルを再検証
- 観測道具自体に bug があった場合: 道具を最優先で直す (= 道具が信用できないと全ての判断が崩れる)

### 例外: 道具がまだない場合

新機能の設計フェーズで「観測道具自体が未実装」「実機検証の前提が未構築」の場合のみ、
推測実装で「先に道具を作る」順序が正当化される。**ただしこの場合も:**

- 推測実装は DR で justify する (= 「観測道具を作るための最小限の判断、検証は道具完成後」と明記)
- 道具完成後は **即座にマトリクス検証** を実施し、判断の正しさを後追い確認
- 後追い確認で乖離が見つかれば、新 DR or DR Update で修正する

## Consequences

### 即時的影響

- 本 nonstop session 45 commit を **本 DR の self-check リスト** で再点検する task が発生する
  (= 透過原則違反 / 推測実装 / マトリクス未検証 の audit)
- audit の候補箇所:
  - DEC sync chunk 跨ぎ 7 byte sliding window carry (= Phase B、`?2026h/l` を hyoui 側で監視している、
    透過か再評価)
  - stalled detect の 3 連続自動 reset (= Phase B、子の bytes 出力を hyoui が「異常」と判定して
    state を捨てる、透過か再評価)
  - input bytes log + resize replay (= Phase B、primary buffer の bytes を hyoui が ring buffer に
    貯めて resize 時 replay、必然か再評価)
  - wait core の polling 100ms default (= state-based wait、頻度は妥当か)
  - lock acquire の「block until signal/EOF」設計 (= 子の auto-release を避けるため client が
    意図的に block、透過か再評価)
- DR-0001 軸 1/2 の **未実装** は本 DR で禁則とした「推測実装」ではなく、設計済 DR に従った正規実装。
  実装は別 task で進めるが、進める前にマトリクス検証で現状把握を完了する

### 長期的影響

- 今後の Claude セッションは CLAUDE.md (= プロジェクトルートの自動 Read 対象) から本 DR を参照し、
  実装判断前に self-check を走らせる
- 設計判断の根幹 (= 透過原則を破る判断、新 protocol message 追加、新 cap flag 追加など) は本 DR に
  従って kawaz に明示確認、agent 単独判断で進めない
- ドッグフーディングが標準フローになる (= 修正後の動作確認に hyoui 自身を使う、`hyoui screen dump`
  で状態取る、`hyoui wait` で挙動再現する等)

## Anti-patterns (= 本 DR 制定の直接の契機、繰り返し禁止)

以下は **本セッションで Claude が一度実際に提案/起動** してしまった anti-pattern。
今後は self-check で弾く:

1. **「daemon が waitpid で子の STOPPED を検出 → 新 control message `ChildStateChanged` →
   client が raise(SIGSTOP)」案** (= 親プロセスの kernel 標準機能 = SIGCHLD 受信を「監視/通知」と
   再発明、cap flag `child-state-v1` まで追加しようとした)
2. **claude TUI サンプル 1 で原因断定** (= vim / less / bash / cat 等で検証せず推測で結論)
3. **「マトリクス検証は別 task」と先送り** (= 修正前にマトリクスを埋めず、自分の仮説を信じて
   実装に進んだ)

## 関連

- [[DR-0001]] — jobcontrol 2 軸、本 DR の透過原則違反の例外として明示 justify された介入
- [[DR-0005]] — 思想再定義、透明性最優先の起点
- [[DR-0013]] — screen state 正本化、ドッグフーディング道具揃え
- `CLAUDE.md` — プロジェクトルート、本 DR を Claude Code が常時参照する誘導
