# Bug: wait の全角文字 padding でマッチが崩れる (screen→text 変換)

- Status: Open
- Date: 2026-06-10
- Priority: Mid (= 日本語混じり pattern で `wait` が意図せず外れる。英数 pattern には影響しないが、claude TUI など日本語 UI を相手にすると顕在化する)
- 関連 DR: [DR-0006](../decisions/DR-0006-cli-ground-rules.md) §9.1 (= wait の screen→text 変換仕様)、[DR-0013](../decisions/DR-0013-screen-emulator-and-attach-stability.md) (= screen state 正本)
- 関連実装: `crates/hyoui-cli/src/wait_core.rs` の `SnapshotCells::to_text()` (line 116-167)

## 問題

`hyoui wait` / `input` の `wait:` spec は daemon から取った snapshot cells を
`SnapshotCells::to_text()` で text 化し、その text に regex を `is_match` する
(`wait_core.rs::wait_for_pattern`)。

この text 化が **全角文字 (= 2 col 占有) を正しく詰めない**:

- 全角文字は先頭 cell に文字列が入り、継続 cell は sparse snapshot 上 skip される
  (daemon 側 `build_screen_snapshot` が wide_continuation を出力しない)
- `to_text()` は grid を `cols` 個の半角空白で初期化し、sparse cell の text を該当
  位置に書く。継続 cell の位置は **半角空白 1 個のまま残る**
- 結果の text 上は「全角文字 + 半角空白 1 個」が 1 文字ごとに並ぶ。
  例: 画面の `あいう` (3 全角 = 6 col) が text 上 `あ い う` (各文字の後ろに空白) になる

`to_text()` のコメント (line 126-130) 自身がこの挙動を明示しており、
「末尾 trim で除去されるため実害は少ない。完全に layout を保ちたい用途は別 task で対応」
と保留されている。だが **行中 (= 行末でない) の継続 cell 空白は末尾 trim では消えない**ため、
日本語混じり pattern が崩れる:

- `wait "$SESS" "あいう"` は text が `あ い う` なのでマッチしない
- `wait "$SESS" "続行しますか"` のような日本語 prompt 待ちが空振りする

## 期待挙動

screen→text 変換が、**正しい代替経路が既に存在する**:

`crates/hyoui/src/daemon/screen/snapshot.rs` の `build_plain_text_from_rows` /
`rows_to_ansi` は wide_continuation cell を `continue` で **skip** しており、
全角文字の後ろに余分な空白を入れない。`hyoui screen dump --format=plain` 等は
これを使うため正しく詰まる。

→ wait の `to_text()` も同じ流儀 (= 継続 cell を「半角空白を埋めない / skip する」)
に揃えるべき。

## 修正方針 (= 要検討)

`SnapshotCells::to_text()` の grid 構築で、全角先頭 cell の次の col を「埋めない
(= skip)」扱いにする。ただし snapshot cells は sparse で wide_continuation を
そもそも持たないため、以下のいずれか:

1. **先頭 cell が wide なら次 col を grid から落とす**: snapshot の cell に `wide`
   flag があるか確認 (`snapshot.rs` の `RowCellSnap`/`Cell` に `wide` がある)。
   wide cell を書いたら次の col slot を空文字 (= 詰める対象) にして、行 join 時に
   空白でなく文字列連結で詰める
2. **grid を「半角空白初期化」でなく「実 cell のみ join」に変える**: cols 個の固定
   slot を持たず、cell を col 昇順で並べて間の gap だけ空白で埋める。wide の次 col
   は gap でなく continuation なので空白を入れない

## 検証 (= CLAUDE.md 検証主義 / empirical-verification)

- マトリクス: (全角のみ / 全角+半角混在 / 全角 + 行末 / alt screen TUI) ×
  (pattern が全角 / 半角 / 混在) で「マッチすべき / すべきでない」を埋める
- 最低 3 category: 日本語 prompt (claude TUI) / 全角記号を含む TUI / 半角のみ (= 回帰なし確認)
- 既存の `to_text()` unit test に全角ケースを追加 (現状 ascii ケースのみと推測)
- daemon 側 `build_plain_text_from_rows` の wide skip 実装を参照実装にする

## 注意

英数のみの pattern には影響しない (= 既存の英数 wait は壊さないこと)。
回帰テストで半角ケースの不変を担保する。
