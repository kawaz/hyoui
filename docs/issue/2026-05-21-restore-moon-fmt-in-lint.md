# lint に moon fmt を復活させる

- Status: Open
- Date: 2026-05-21
- Priority: Low（`moon check` が正当性ゲートとして機能しているため）

## 現状

`Taskfile.pkl` の `lint:moon` task は `moon check --target native` のみで、
`moon fmt`（フォーマットチェック）を含めていない。

理由: moon `0.1.20260509`（`rr_moon_pkg` feature flag 有効）では、`moon fmt` が
副作用で `moon.pkg.json` を新形式 `moon.pkg` へマイグレートする。その際
`supported_targets = [ "native" ]` を出力するが、これを `moon check` が
「Invalid configuration `supported_targets`」として拒否する。`moon fmt` と
`moon check` が `moon.pkg` のスキーマで不一致を起こしており、両方が受け付ける
`supported_targets` の書き方が見つからなかった。`moon fmt --check` も
マイグレーションを常に diff として報告するため使えない。

そのため `lint:moon` は非破壊な `moon check` のみとし、`moon fmt` は外してある
（Taskfile.pkl 内に Design rationale コメントあり）。

## やること

moon toolchain にはこの間いくつかバージョンアップがあったはず。**moon を最新化**し、
`moon fmt` と `moon check` の `moon.pkg` スキーマ不一致が解消されているか確認する。
解消されていれば `lint:moon` に `moon fmt`（または `moon fmt --check`）を復活させる。

`lint:rust` は `cargo fmt --check` + `cargo clippy` で既にフォーマットチェック済み。

## 関連

- docs/journal/2026-05-21-bootstrap.md
- `Taskfile.pkl` の `lint:moon` task のコメント
