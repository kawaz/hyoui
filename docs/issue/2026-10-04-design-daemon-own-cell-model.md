---
title: daemon の仮想スクリーンを vt100 から自前のセルモデルにする (層の合成でオーバーレイ、attach 出力は合成画面から作る)
status: open
category: design
created: 2026-10-04T11:30:00+09:00
last_read: 2026-10-04T11:30:00+09:00
open_entered: 2026-10-04T11:30:00+09:00
wip_entered:
blocked_entered:
pending_entered:
discarded_entered:
resolved_entered:
discard_reason:
pending_reason:
close_reason:
blocked_by:
---

# daemon の仮想スクリーンを vt100 から自前のセルモデルにする

議論フェーズの記録 (2026-10-04)。DR-0013 (screen state 正本) を引き継ぐ大きな案件で、web UI 作り直し (`docs/issue/2026-10-04-design-webui-terminal-app-rework.md`) とは別 track。ブラウザ側は xterm.js のまま (外に出るのは TTY 出力だけなので、daemon の画面モデルを替えてもブラウザの経路は変わらない)。

## 目的

vt100 では持てないものを daemon が持てるようにする。

- 色・属性・OSC 8 をセル単位で持つ (screenshot / snapshot の貧弱さ、`docs/issue/2026-08-24-attach-osc8-hyperlink-metadata-loss.md`)
- rect 指定の切り出しと監視 (`docs/issue/2026-07-21-screen-region-watch-api.md`)
- オーバーレイ (`docs/issue/2026-07-21-screen-overlay-general-mechanism.md`)
- 履歴の保持

web 初期表示のモノクロは vt100 の制約ではなく hyoui 側の手抜きで、本件を待たずに直せる (`docs/issue/2026-10-04-screen-scrollback-ansi-drops-color.md`)。

## 合意

- **vt100 と同じくライブラリ (crate) として作り、daemon の vt100 を差し替える**。daemon の他の処理と切り離して parser と画面モデルを単体で test でき、crate の公開 API が daemon との境界の仕様になる。現在の vt100 利用は `daemon/screen/state.rs` に閉じているので差し替え範囲もそこに収まる見込み
- crate の責務: 層の合成とオーバーレイ、合成画面から差分を ANSI 化する処理まで crate に入れる (daemon は bytes を入れて合成済みの差分を受け取るだけ)。OSC のメタデータ (title / cwd / 133 / 通知) は crate がイベントとして通知し、session の属性として保持するのは daemon
- crate の置き場所: まず hyoui workspace 内の `crates/` に置き、外部公開は API が固まってから決める
- 層の合成は rect 単位のセルの合成。層 = セルの grid + 位置 + z 順で、TTY の画面全体も画面サイズの grid 1 枚にすぎない。ダイアログは小さい rect の grid を置くだけ、別 session の TTY の画面も同じ層として rect に置ける (見た目だけの合成で、どちらの TUI アプリも気付かない)。入力の振り分けは合成の外 (daemon の別の層)
- 層の指定軸 (互いに独立):
  - 未書き込みセル: 塗りつぶす (既定) / 透過する
  - 大きさ: w・h を指定 / 中身に合わせる (ソースがテキストなら既定で中身に合わせる。`あいう\nえお` なら w=6 h=2)
  - 配置の基準: lt / rt / lb / rb のどの角から測るか + オフセット。画面リサイズ時に基準の角に追従する
  - 中身: 合成が受けるのは常に **装飾込みの cell rect** だけ (crate の合成 API は「cell rect を層として挿入する」1 種類)。テキストは cell rect を作る変換 (装飾は最小で fg / bg 指定、幅は crate の文字幅の表)、tty (別 session の画面) は中身が更新され続ける cell rect。ユーザが手軽に使うのはテキストで、CLI からテキストのオーバーレイを挿入できるのが主な入口
- 層 (オーバーレイ) は ID を持つ。呼び出し側が指定でき、無ければ daemon が振って返す。同じ ID で挿入し直すと置き換え (upsert、進捗表示等の書き換えに使う)
- 層の z 順 (推し): 並び順のキーは `(z: i32, 挿入の連番)`。z が同じなら後から入れた方が上 (重複を禁止してずらす規定の代わりに、重複を許して決定的なタイブレークを置く)。float は差し込みやすさと引き換えに二分の精度が尽きる・同値の規定が結局要る・f64 は NaN で全順序でない、ので採らない。間への差し込みは z の値ではなく API の相対指定 (「ID X の直上」等) で表し、内部の並べ直しは crate が持つ
- 画面からはみ出す rect は画面で clip する (clip 境界で割れる wide 文字も同じ規則)
- wide 文字が層の境界で割れる時: TTY では wide 文字の半分は描けないので、合成結果は常に TTY のセル列として表現できる形にする。どの層の wide 文字でも (上の層・下の層を問わず) 他の層との境界で片側が隠れたら見た目上は消し、見えて残る側のセルはスペースで埋める
- オーバーレイは層の合成で実現する: 下の層 = TUI アプリの出力で更新されるセル (オーバーレイ中も裏で更新し続ける)、上の層 = hyoui のオーバーレイ、見ている側 (attach client) に届くのは合成結果。オーバーレイを消したらその範囲に下の層のセルを出し直すだけ。TUI アプリ (子の入力) には何も送らず、アプリは重ねられていることを知らない。web も CLI attach も同じ TTY 出力として受ける (届け先で方式を分けない)
- **attach 出力は常に合成画面から差分を ANSI 化して作る (tmux 型)**。今の「子の出力 bytes を素通し」はやめる (素通しだと子の出力がオーバーレイを上書きする)。モード切り替えが無く構造が最もシンプルで、セルモデルの正しさが常に見た目で検証される。代わりにセルモデルの忠実性 (色・属性・OSC 8・全角幅・合字) への要求が上がる

## 未決

- 表示するカーソルをどの層のものにするか (既定は最下層の TUI アプリ)
- CLI から挿入するオーバーレイの消え方 (明示的に消す / 時間で消える / キー入力で消える)。CLI の形を決める時に決める
- vt100 を置き換える範囲: parser ごと自前にするか、parser (vte 等) は使ってセル保持と合成だけ自前にするか
- 差分 ANSI 化の粒度 (行 / セル)、出力のバッファリングと遅延、DEC sync update (2026) との整合
- OSC の扱い: title / cwd / 133 / 通知はセルではなく session 属性 (`docs/findings/2026-10-04-daemon-osc-coverage.md`)。OSC 8 はセル属性
- 07-21 の overlay issue の「子 PTY・TUI アプリには一切影響を与えず (バイト送信なし)」は「子の入力には送らない、見ている側の出力には重ねる」と読む。DR では主語を明記する
