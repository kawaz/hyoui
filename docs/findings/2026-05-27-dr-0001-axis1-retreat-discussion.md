# DR-0001 軸 1 撤退案の議論 (peer 共有、kawaz 判断材料)

- Date: 2026-05-27
- Source: peer session `a822a3d2-3b54-4ea4-8f18-30fcc124a3f8` (= cmux-msg + csa 検証で hyoui を利用してきた立場)
- Related: DR-0001 (jobcontrol 2 軸), DR-0005 (思想再定義), DR-0014 (検証主義), `docs/findings/2026-05-27-jobcontrol-matrix-verification.md`

## 判明した事実

### 1. POSIX_SPAWN_SETSID と SIGTSTP の不両立 (= マトリクス検証で発見)

`POSIX_SPAWN_SETSID` で子を spawn すると子は新 session leader、その子 process group は
外側 session から見ると **orphan process group**。POSIX 仕様で orphan group への
SIGTSTP / SIGTTIN / SIGTTOU は kernel が discard する。

→ DR-0001 軸 1 (= 子が PTY 内で自分を suspend、`raise(SIGTSTP)`) の設計前提が崩れている。

### 2. peer の主要 use case は headless 中心 (= 軸 1 発火しない)

peer の hyoui 使用パターン:
- cmux-msg subscribe + child claude spawn (= 2026-05-27 検証セッション)
- hyoui input + wait + screen dump で claude TUI 完全駆動 (= 同日 PoC)

両方とも **headless モード**、人間は attach 経由で操作しない、Ctrl-Z 発火条件なし。
→ 軸 1 の前提崩壊は peer の use case では **実害ゼロ**。

### 3. DR-0005 (外側自動操作主軸) との接続

DR-0005 で「外側自動操作主軸、TUI multiplexer ではない」と思想を確定した時点で、
子の self-SIGTSTP の必要性は本質的に薄い。「外側から detach / kill / lock」が
input/lock family + screen で揃っていれば操作軸として十分。

## 視点の整理 (= 採用判断は kawaz、現状未定)

> **重要 (kawaz 指示、2026-05-27)**: peer 視点 (= a822a3d2) は **headless ユーザの
> 立場からの意見**であり、人間が interactive で使う際の使い勝手判断は **未定**。
> 本 finding は判断材料の整理のみで、**「推奨/非推奨」の決定は行わない**。
> kawaz が説明 + 使い勝手確認を経て、明日以降に判断する。

### A. 軸 1 撤退案 (= peer 視点)

提案:
- **軸 1 を設計から外す**: 子 self-SIGTSTP は POSIX_SPAWN_SETSID と本質的に両立しない、
  ハック的回避より撤退が筋
- **軸 2 のみ残す**: 親 self-SIGTSTP / decouple は外側スクリプトから親 hyoui プロセスを
  管理する用途で価値あり (= 親が止まっても daemon は live のまま、別 attach で復帰)
- **interactive 人間 attach の Ctrl-Z 挙動** が必要なら別ルート (= attach client 側の
  signal handling) で実装

peer 視点での根拠:
- lean 思想 (= 余分な機能を持たない) と整合
- POSIX_SPAWN_SETSID を維持できる (= multi-attach / daemon detach 他要件との整合)
- peer の use case (= headless 中心) では軸 1 発火条件なし

**未検討の論点 (= kawaz 判断時に要確認)**:
- interactive 人間 attach の Ctrl-Z 体験の使い勝手 (= peer は use case 外で判断材料持たず)
- 「別ルート (= attach client 側 signal handling)」の具体実装と現実性
- DR-0001 起票時 (= 2026-05-21 journal `bootstrap`) に想定したシナリオがどこまで失われるか

### B. POSIX_SPAWN_SETSID 自体を変える案

子を親と同じ session group に置く選択肢:
- multi-attach / daemon detach の他要件と衝突する可能性
- 別の前提崩壊リスク
- DR-0003 で「PTY 制御端末持つために必須」と決めた経緯あり、再評価コスト

→ 影響範囲が大きい、慎重判断要。

### C. 軸 1 を「外側からの signal forward」として再定義する案

agent (= Claude) の補足:
- 軸 1 を「子が自分で SIGTSTP」ではなく「**子に対して SIGTSTP を送るユーザ要求があった場合**」と
  再定義する
- 例: `hyoui send-signal <session> --signal=TSTP` を新設、daemon が `killpg(child_pgid, SIGTSTP)`
- ただし orphan group への SIGTSTP は kernel discard なので、**実効性なし**
- → 撤退と同じ結論に至る

### D. 軸 1 を現状維持で「kernel discard を許容」案

検討漏れの選択肢:
- 軸 1 仕様を維持しつつ、orphan group での kernel discard 挙動を仕様として明文化
- claude TUI のような「自分で suspend message を出して raise(SIGTSTP) する」プログラムには
  follow が機能しない (= 当該プログラムが kernel 仕様を assume してる前提が崩れる)、
  これを「OS 仕様の限界」として hyoui の責任範囲から外す
- DR-0001 §仕様の限界に annotate 追加
- メリット: 設計を変えずに「期待値を下げる」だけで済む
- デメリット: interactive 体験の使い勝手は改善されない

## 関連する claude TUI 動作の解釈

cast 解析 (= `claude2.cast` t=10.86) で claude TUI が
「Claude Code has been suspended. Run \`fg\` to bring Claude Code back.」
を出力していた件:

仮説:
- claude TUI は自分の process が orphan group に居ることを検知していない可能性
- 内部で `raise(SIGTSTP)` を呼んでいるが、kernel が discard、表面上は「suspended message を
  print して shell に戻ったつもり」になっているが実際は子も hyoui client も走り続け
- これが cast で「^Z 連打しても無反応」だった現象の説明
- 直接 claude 起動 (= claude1.cast) では shell の中の子なので orphan ではなく、
  SIGTSTP が正常 fire、shell が suspended job として認識する

→ claude TUI の suspended message 出力は「user 期待 vs OS 仕様」の乖離を product 側が
緩和しようとしている artifact と見ることができる。各案 A-D のどれを採るかとは独立の現象。

## kawaz 判断項目 (= 採用判断は未定、判断材料の整理)

1. **軸 1 の扱い**: A 撤退 / B POSIX_SPAWN_SETSID 変更 / C 外側 signal forward 再定義 /
   D 現状維持 + 仕様限界明文化、のどれを採るか (= 現状未定、kawaz が説明 + 使い勝手確認を経て判断)
2. **DR-0001 改訂方針**: 採用案に応じて DR-0001 を sub-DR or Update annotate で書き直す
3. **DR-0005 との整合確認**: 「外側自動操作主軸」思想と各案の整合性
4. **interactive 人間 attach の Ctrl-Z 体験**: hyoui の責任範囲か対象外か (= peer 視点では
   別ルート提案、kawaz 視点の検討必要)
4. **軸 2 (= 親 self-SIGTSTP) の実装着手判断**: peer は「価値あり」とするが、実装の優先度判断
5. **interactive 人間 attach の Ctrl-Z 挙動**: 別ルート (= attach client 側 signal handling)
   を設ける必要があるか、または「hyoui は外側自動操作主軸なので interactive Ctrl-Z は対象外」と
   切り捨てるか

## 関連

- DR-0001 (= jobcontrol 2 軸、本 finding の主対象)
- DR-0005 (= 思想再定義、外側自動操作主軸)
- DR-0014 (= 検証主義、本 finding の発見プロセスの根拠)
- `docs/findings/2026-05-27-jobcontrol-matrix-verification.md` (= マトリクス検証の生データ)
- peer message: `20260527T210818-778daaa7.md` (= 本 finding の input、accepted/ に archive 済)
