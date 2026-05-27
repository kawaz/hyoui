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

## 実用的な示唆 / 提案

### A. peer 推奨: 軸 1 撤退 + 軸 2 残す (= 最小撤退案)

- **軸 1 を設計から外す**: 子 self-SIGTSTP は POSIX_SPAWN_SETSID と本質的に両立しない、
  ハック的回避より撤退が筋
- **軸 2 のみ残す**: 親 self-SIGTSTP / decouple は外側スクリプトから親 hyoui プロセスを
  管理する用途で価値あり (= 親が止まっても daemon は live のまま、別 attach で復帰)
- **interactive 人間 attach の Ctrl-Z 挙動** が必要なら別ルート (= attach client 側の
  signal handling) で実装

理由:
- lean 思想 (= 余分な機能を持たない) と整合
- POSIX_SPAWN_SETSID を維持できる (= multi-attach / daemon detach 他要件との整合)
- 「外側操作軸」の本筋に集中できる

### B. 別解: POSIX_SPAWN_SETSID をやめる

子を親と同じ session group に置く選択肢もある。
ただし:
- multi-attach / daemon detach の他要件と衝突する可能性高
- 別の前提崩壊を引き起こす確率高
- DR-0003 で「PTY 制御端末持つために必須」と決めた経緯あり、再評価コスト大

→ peer は **非推奨**。

### C. 第三の道: 軸 1 を「外側からの signal forward」として再定義

agent (= 私) の補足案:
- 軸 1 を「子が自分で SIGTSTP」ではなく「**子に対して SIGTSTP を送るユーザ要求があった場合**」と
  再定義する
- 例: `hyoui send-signal <session> --signal=TSTP` を新設、daemon が `killpg(child_pgid, SIGTSTP)`
- ただし orphan group への SIGTSTP は kernel discard なので、これも実効性なし
- → 同じく実装不可、撤退が筋

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

→ 軸 1 撤退判断と矛盾しない。むしろ claude TUI の suspended message 出力は
「user 期待 vs OS 仕様」の乖離を product 側が緩和しようとしている artifact と見ることもできる。

## kawaz 判断項目

1. **軸 1 撤退 採否**: peer 推奨案 A を採用するか
2. **DR-0001 改訂方針**: 軸 1 撤退なら DR-0001 を sub-DR or Update annotate で書き直す
3. **DR-0005 との整合確認**: 「外側自動操作主軸」思想と軸 1 撤退が整合するか再確認
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
