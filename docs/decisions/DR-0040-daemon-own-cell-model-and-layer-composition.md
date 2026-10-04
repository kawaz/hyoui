# DR-0040: daemon の仮想スクリーンを自前のセルモデル crate にする — 層の合成でオーバーレイ、attach 出力は合成画面から作る

- Status: ⬜ 未実装 (2026-10-04)。裁定済みの設計のみで実装エビデンスなし。確定させていない論点は末尾「未決」節
- Date: 2026-10-04
- Related: DR-0013 (screen state 正本。「daemon = screen state の唯一の正本」を本 DR が引き継ぎ、§2 の vt100 採用と §3 の state 構造を本 DR が置き換える), DR-0025 (Screen domain の `WatchRegistration`。rect 指定の監視の土台), DR-0005 / DR-0014 (透過原則。オーバーレイを子の入力に送らない根拠), DR-0029 (attach は覗き窓。見ている側の出力だけに重ねる), DR-0039 (web UI 作り直し。ブラウザ側は本 DR の範囲外)
- Origin: `docs/issue/2026-10-04-design-daemon-own-cell-model.md` (kawaz との議論で合意 2026-10-04)。関連 issue は `docs/issue/2026-07-21-screen-overlay-general-mechanism.md` / `docs/issue/2026-07-21-screen-region-watch-api.md` / `docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md`

## Context

daemon の screen state は DR-0013 §2 で vt100 0.16.2 を正本にしている。vt100 のセルと parser には、hyoui が持ちたいものを載せる場所が無い。

- **OSC 8 (hyperlink)**: vt100 の `osc_dispatch` は OSC 0 / 1 / 2 / 52 だけを処理し、OSC 8 は `unhandled_osc` に落ちる。セルに URI を持つ場所が無いので、attach 前に出たリンクは attach 復元・`screen dump`・snapshot のどれでも文字列だけになる (`docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md`)。callback で params を受け取れても、セルに結び付けて保持できなければ復元には使えない (`docs/findings/2026-10-04-daemon-osc-coverage.md` §4)
- **セル属性**: vt100 の `Cell` は blink / strike を持たない (`crates/hyoui/src/daemon/screen/snapshot.rs` の `rows_to_ansi` の doc comment)
- **rect 指定の切り出しと監視**: DR-0025 の `WatchRegistration` (region / matcher / flow) と `ScreenWriteEvent` はセル単位の書き込み通知を前提にするが、vt100 は cell hook を持たない (DR-0025 Alternative F、Q-NEW1)
- **オーバーレイ**: 子の画面の上に hyoui のダイアログや通知を重ねる一般機構 (`docs/issue/2026-07-21-screen-overlay-general-mechanism.md`) は、子の画面とは別の描画単位を合成する仕組みを要する。vt100 の `Screen` は 1 枚の grid で、その外に合成の仕組みは無い
- **OSC のメタデータ**: title / cwd / 133 / 通知を vt100 は保持せず、取る経路は callback か `unhandled_osc` だけ (`docs/findings/2026-10-04-daemon-osc-coverage.md` §1)

加えて、attach client への出力は現在も子の出力 bytes の素通しである。PTY read loop は `screen_state.process(&buf[..n])` の直後に同じ bytes を `broadcast_master_bytes` で配る (`crates/hyoui/src/daemon/session.rs` の PTY read 経路)。DR-0013 §1 は「client への送出は state を経由する形に統一する」と決めているが、実装は Phase A の併存のまま (同箇所のコメント「Phase B で生 byte broadcast を state-driven に置換する」)。screen state が使われるのは attach 直後の復元 redraw (`build_attach_redraw`) だけで、素通しの出力は上に何かを重ねても子の次の出力で上書きされる。

色の欠落は本 DR の動機に含めない。vt100 は前景色・背景色をセルに保持しており、web 初期表示がモノクロだったのは hyoui 側が色を落としていたため (`docs/issue/archive/2026-10-04-screen-scrollback-ansi-drops-color.md`)。

## 目的

vt100 では持てないものを daemon が持てるようにする。

- 色・属性・OSC 8 をセル単位で持つ
- 画面の rect 指定の切り出しと監視
- オーバーレイ
- 履歴の保持

増やしたくないもの:

- **attach 出力の経路の数**。「オーバーレイがある時だけ合成する」のような、状態で切り替わる出力経路を持たない
- **合成が受け付ける中身の種類**。合成の入力は 1 種類に保ち、中身の種類の追加は合成の外の変換で吸収する
- **子 (TUI アプリ) への送信**。オーバーレイのために子の入力へ bytes を送らない (DR-0014 の「bytes は透過」)

本 DR の「層」は合成の単位 (§4) を指す。

## Decision

### 1. 自前のセルモデルを crate として作り、vt100 を差し替える

vt100 と同じくライブラリ (crate) として作り、daemon の vt100 を差し替える。

- **理由**: daemon の他の処理 (client 管理・PTY・protocol) と切り離して、parser と画面モデルを単体で test できる。crate の公開 API がそのまま daemon との境界の仕様になる
- 現在 vt100 の型を使うのは `crates/hyoui/src/daemon/screen/` の `state.rs` (`vt100::Parser` / `Cell` / `Color`) と `snapshot.rs` (`vt100::Color` からの SGR 生成) で、差し替えの範囲はこの module に収まる

### 2. crate の責務と daemon との境界

crate が持つ:

- bytes を解釈して画面モデル (セルの grid) を更新する
- 層の合成とオーバーレイ (§4)
- 合成画面から差分を ANSI 化する処理 (§5)
- OSC のメタデータ (title / cwd / 133 / 通知) をイベントとして通知する (§7)

daemon が持つ:

- 子の出力 bytes を crate に入れ、合成済みの差分を受け取って attach client に配る
- crate が通知した OSC のメタデータを session の属性として保持する
- 入力の振り分け (どの client の入力をどこに送るか)。合成の外の層の責務で、crate は入力を扱わない
- 層 (オーバーレイ) の ID を呼び出し側が指定しなかった時の採番 (§4)

**理由**: daemon から見て「bytes を入れ、合成済みの差分を受け取る」だけになれば、合成と ANSI 化の正しさを crate の単体 test で閉じられる。session の属性は client への通知や一覧 API と結び付く daemon 側の関心で、画面モデルの関心ではない。

### 3. crate の置き場所

まず hyoui workspace 内の `crates/` に置く。外部公開 (crates.io 等) は API が固まってから決める。

**理由**: API が固まる前に公開すると、daemon との境界を直すたびに公開版の互換を気にすることになる。

### 4. 層の合成

合成は rect 単位のセルの合成とする。

**層の定義**: 層 = セルの grid + 位置 + z 順。TTY の画面全体も、画面サイズの grid 1 枚の層にすぎない。ダイアログは小さい rect の grid を置くだけで、別 session の TTY の画面も同じく層として rect に置ける。どれも見た目だけの合成で、どちらの TUI アプリも重ねられていることに気付かない。

**理由**: TTY の画面・オーバーレイ・別 session の画面を同じ「層」として扱えば、合成の規則 (重なり・clip・wide 文字) は 1 通りで済む。

**層の指定軸** (互いに独立):

| 軸 | 選択肢 |
|---|---|
| 未書き込みセル | 塗りつぶす (既定) / 透過する |
| 大きさ | w・h を指定 / 中身に合わせる。ソースがテキストなら既定で中身に合わせる (`あいう\nえお` なら w=6 h=2) |
| 配置の基準 | lt / rt / lb / rb のどの角から測るか + オフセット。画面リサイズ時は基準の角に追従する |
| 中身 | 合成が受けるのは常に **装飾込みの cell rect** だけ |

**中身は cell rect に一本化する**。crate の合成 API は「cell rect を層として挿入する」の 1 種類とする。

- テキストは cell rect を作る変換で扱う。装飾は最小で fg / bg の指定、幅は crate の文字幅の表を使う
- tty (別 session の画面) は、中身が更新され続ける cell rect
- ユーザが手軽に使うのはテキストで、CLI からテキストのオーバーレイを挿入できるのが主な入口

**理由**: 中身の種類ごとに合成の API を分けると、種類が増えるたびに合成側の規則が増える。合成の入力を cell rect 1 種類に保てば、種類の差は変換の側に閉じる。

**層の ID と upsert**: 層 (オーバーレイ) は ID を持つ。呼び出し側が指定でき、指定が無ければ daemon が振って返す。同じ ID で挿入し直すと置き換える (upsert)。

**理由**: 進捗表示のように同じ層を繰り返し書き換える用途で、消してから入れ直す 2 手を要らなくする。

**z 順**: 並び順のキーは `(z: i32, 挿入の連番)` とし、キーの大きい方が上に来る。z が同じなら後から入れた方が上。

- z の重複を許し、挿入の連番で決定的にタイブレークする
- 層と層の間への差し込みは z の値でなく API の相対指定 (「ID X の直上」等) で表す。そのための内部の並べ直しは crate が持つ

**理由**:

- 重複を禁止すると、衝突時にずらす規定が別に要る。重複を許して決定的なタイブレークを置く方が規定が少ない
- float の z は差し込みやすいが、二分を繰り返すと精度が尽きる、同値の規定が結局要る、f64 は NaN があって全順序でない、の 3 点で採らない
- 「X の直上」は呼び出し側の意図そのもので、z の値の算出を呼び出し側に負わせない

**clip**: 画面からはみ出す rect は画面で clip する。clip 境界で割れる wide 文字も、次の wide 文字の規則に従う。

**wide 文字の境界**: TTY では wide 文字の半分は描けないので、合成結果は常に TTY のセル列として表現できる形にする。どの層の wide 文字でも (上の層・下の層を問わず)、他の層との境界で片側が隠れたら見た目上は消し、見えて残る側のセルはスペースで埋める。

**理由**: 半分だけ見える wide 文字は TTY に出力できない。上下どちらの層かで規則を分けると、合成順によって結果が変わる。

### 5. attach 出力は常に合成画面から作る (tmux 型)

attach client への出力は、常に合成画面から差分を ANSI 化して作る。子の出力 bytes の素通しはやめる。attach 直後の復元 (DR-0013 §4) も同じ合成画面から作る。

**理由**:

- 素通しだと子の出力がオーバーレイを上書きする
- 「オーバーレイの有無で素通しと合成を切り替える」形と比べて、モードの切り替えが無く構造が最もシンプル
- セルモデルの正しさが常に見た目で検証される (合成画面がずれていれば、attach した人がすぐ気付く)
- DR-0013 §1 の「client への送出は state を経由する形に統一する」が、本 DR で実装される

**代償**: セルモデルの忠実性への要求が上がる。色・属性・OSC 8・全角幅・合字のどれかを落とすと、それが attach した画面からそのまま欠ける (素通しなら外側の端末が補っていた)。

### 6. オーバーレイは見ている側の出力に重ね、子の入力には送らない

オーバーレイは層の合成で実現する。

- 下の層 = TUI アプリの出力で更新されるセル。オーバーレイ中も裏で更新し続ける
- 上の層 = hyoui のオーバーレイ
- 見ている側 (attach client) に届くのは合成結果
- オーバーレイを消したら、その範囲に下の層のセルを出し直すだけ

`docs/issue/2026-07-21-screen-overlay-general-mechanism.md` の「子 PTY・TUI アプリには一切影響を与えず (バイト送信なし)」の主語を次のとおり明記する: **子 (TUI アプリ) の入力には何も送らない。見ている側 (attach client) の出力には重ねる**。アプリは重ねられていることを知らない。

web も CLI attach も、同じ TTY 出力として合成結果を受ける。届け先によって方式を分けない。

**理由**:

- 子の入力に送らないので DR-0014 の「bytes は透過」(stdin → PTY master → 子の経路で bytes を変換しない) を破らない。オーバーレイが触るのは外向きの出力だけ
- TUI アプリの出力で更新される層は子の出力以外で書き換えないので、DR-0029 §1 の「daemon の screen state には書かない (正本を汚さない)」と両立する。オーバーレイは別の層に置かれる
- 届け先で方式を分けると、同じオーバーレイの見え方が web と CLI で食い違い得る

### 7. OSC の扱い — セル属性と session 属性の分担

| OSC | 置き場 | 経路 |
|---|---|---|
| 8 (hyperlink) | セル属性 | crate がセルに保持し、合成と差分 ANSI 化にも載せる |
| 0 / 1 / 2 (title / icon name)、7 (cwd)、133 (shell integration)、9 / 777 (通知) | session 属性 | crate がイベントとして通知し、daemon が session の属性として保持する |

**理由**: OSC 8 はどのセルがどのリンクかという画面上の位置と不可分で、セルに持たなければ attach 復元で失われる (Context)。title / cwd / 133 / 通知は画面上の位置に結び付かない session の状態や出来事で、client への通知や一覧 API に出すのは daemon の責務 (§2)。

### 8. web UI 作り直し (DR-0039) との境界

ブラウザ側は xterm.js のままで、本 DR の範囲外とする。daemon から外に出るのは TTY 出力だけなので、daemon の画面モデルを替えてもブラウザの経路は変わらない。§5 により、その TTY 出力が子の bytes の素通しから合成画面の差分に替わるだけで、ブラウザから見ればどちらも TTY 出力である。

ブラウザ側を daemon のセルから直接描く案 (ブラウザの VT parser をなくす) は本 DR が作る契約に含めない。web UI 作り直しの判断は DR-0039 に属する。

## 介入判断 self-check (CLAUDE.md / DR-0014)

| 項目 | 判定 |
|---|---|
| 既存 DR で justify されているか | attach 出力を screen state 経由にするのは DR-0013 §1。attach 復元の redraw は DR-0014 が「state 正本化の必然」として挙げる介入 |
| 透過原則を破るか | 子の入力には何も送らない (§6)。変わるのは attach client への出力だけ |
| 最小介入か | 出力を常に合成画面から作るのは、オーバーレイが無い時の素通しより介入が大きい。モード切り替えを持たない構造の単純さと、セルモデルが常に見た目で検証される利点を取った (§5) |
| kernel / PTY / shell の標準機能の再発明か | 該当なし。端末エミュレーションは DR-0013 で daemon の責務として既に持っている |
| 新 protocol message / cap flag | CLI からオーバーレイを挿入する入口は要るが、CLI の形は未決 (未決節)。daemon 側の受け口は DR-0025 の Screen domain の message カタログへの追加として定式化する (07-21 overlay issue の設計制約) |
| 既存 DR の未実装 | DR-0013 §1 の「client への送出を state 経由に統一」が未実装 (Context)。本 DR §5 がそれを実装する |

## 未決

`docs/issue/2026-10-04-design-daemon-own-cell-model.md` で未決としたもの:

- 表示するカーソルをどの層のものにするか (既定は最下層の TUI アプリ)
- CLI から挿入するオーバーレイの消え方 (明示的に消す / 時間で消える / キー入力で消える)。CLI の形を決める時に決める
- vt100 を置き換える範囲: parser ごと自前にするか、parser (vte 等) は使ってセル保持と合成だけ自前にするか
- 差分 ANSI 化の粒度 (行 / セル)、出力のバッファリングと遅延、DEC sync update (2026) との整合

`docs/issue/2026-07-21-screen-overlay-general-mechanism.md` の設計論点のうち、本 DR の合意で決まっていないもの:

- オーバーレイの配信先を attach client ごとに個別にするか、全 client 共通にするか
- `screen dump` / snapshot にオーバーレイを合成するか (自動化 API は素の画面を見たいはず、という仮説がある)
- z 順・領域指定を `WatchRegistration` (`docs/issue/2026-07-21-screen-region-watch-api.md`) とどう構造共有するか

## Alternatives Considered

| 案 | 不採用理由 |
|---|---|
| vt100 を維持し、OSC 8 は hyoui 側の sidecar state で持つ (08-24 issue の案 B) | cursor 移動 / erase / scroll / resize とセルの対応を外側で同期し続ける必要があり、フィールドの後付けで設計が歪む。オーバーレイと rect 監視の目的も満たさない |
| vt100 を維持し、attach 前のリンクが死ぬ制約を仕様にする (08-24 issue の案 C) | 目的のどれも満たさない |
| daemon の内部 module として作る (crate にしない) | parser と画面モデルを daemon から切り離した単体 test ができず、daemon との境界が API として固定されない (§1) |
| オーバーレイが無い時は素通し、ある時だけ合成に切り替える | 出力経路が 2 つになり、切り替えの境目 (挿入・消去の瞬間) の整合が別に要る。素通し中はセルモデルの誤りが見た目に現れない (§5) |
| 素通しのまま、オーバーレイを client 側で上から描く | 子の次の出力がオーバーレイを上書きする。届け先 (web / CLI) ごとに描き方が分かれる (§6) |
| 合成 API を中身の種類 (テキスト / tty 等) ごとに持つ | 種類が増えるたびに合成の規則が増える。合成の入力は cell rect 1 種類にし、種類の差は変換に閉じる (§4) |
| z を float にして間に差し込む | 二分の精度が尽きる、同値の規定が結局要る、f64 は NaN で全順序でない (§4) |
| z の重複を禁止し、衝突時にずらす | ずらす規定が別に要る。重複を許して挿入の連番でタイブレークする方が規定が少ない (§4) |

## Consequences

- **DR-0013 との関係**: 「daemon = screen state の唯一の正本」は引き継ぐ。DR-0013 §2 (vt100 採用) と DR-0013 §3 (vt100 の `Parser` / `Screen` を正本にする state 構造) は本 DR で置き換わる。DR-0013 §11 の snapshot wrapper のうち vt100 の型に依存する部分も置き換わる。DR-0013 の Rejected alternatives (a) (vte 単体) は「vt100 が完成形を提供しているので再発明しない」を理由にしていたが、vt100 で持てないものが目的になったので前提が変わった。parser を何にするかは未決
- **DR-0025 との関係**: Screen reducer が持つ仮想 screen state の中身が本 crate に替わる。Q-NEW1 は「vt100 が cell hook を持たない」ことを前提にしていたので、自前 crate では前提が変わる。書き込み通知をどの粒度で crate の API に出すかは本 DR では決めない
- **忠実性の要求**: §5 の代償のとおり、色・属性・OSC 8・全角幅・合字の扱いの誤りは attach した画面に直接現れる。差し替えは DR-0014 の検証主義に従い、3 category (TUI alt screen 系 / line-oriented 系 / interactive REPL 系) で attach 復元と live の見た目を確かめる
- **差し替えの範囲**: vt100 の型を使うのは `daemon/screen/state.rs` と `daemon/screen/snapshot.rs`。設定名 `screen_vt100_scrollback_rows` や CLI の `--scrollback-rows` の help、DR-0028 の upgrade (bytes の再 feed で screen state を作り直す) の説明にも vt100 の名前が残っている。byte-base の履歴 (`crates/hyoui/src/scrollback.rs`) と ANSI の strip (`crates/hyoui/src/strip.rs`) は vt100 を使っておらず、範囲外
- **08-24 OSC 8 issue**: 案 A (screen emulator の置換) の方向で裁定したことになる。実機での確認 (attach 前に出たリンクが attach 後も機能する、primary / alt 両方) は実装後

## 関連

- `docs/issue/2026-10-04-design-daemon-own-cell-model.md` — 合意と未決の正本
- `docs/issue/2026-07-21-screen-overlay-general-mechanism.md` — オーバーレイ一般機構
- `docs/issue/2026-07-21-screen-region-watch-api.md` — rect 指定の切り出しと監視
- `docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md` — attach 復元で OSC 8 が失われる
- `docs/findings/2026-10-04-daemon-osc-coverage.md` — vt100 が扱う OSC の範囲 (ソース読解)
- `docs/research/2026-10-04-web-terminal-renderer-survey.md` — 「自前描画と中間案の実装境界」節 (ブラウザ側を daemon のセルから描く案に要る契約)
- `docs/research/2026-05-27-ghostty-libghostty-study.md` — state → VT 再生成を daemon 側で行う pattern
- DR-0005 / DR-0013 / DR-0014 / DR-0025 / DR-0029 / DR-0039
