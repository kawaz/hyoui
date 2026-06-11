# feature: list / status に子プロセスの実行時情報 (PID・状態) をもっと出す

- Date: 2026-06-11
- Status: open
- 提案者: kawaz (2026-06-11)

## 動機

`hyoui list` は daemon の起動時情報 (session 名 / DUR / CLIENTS / CWD / ARGV) 中心で、
**子プロセスの今** が見えない。子の PID や実行状態 (running / stopped / exited 待ち)
などをもう少し見たい。kill 後に「本当に死んだか」を ps と突き合わせる場面や、
stopped 残骸の発見 (孤児 daemon 騒ぎの早期発見) にも効く。

## 案

- StatusResponse / list の status enrich に追加する候補:
  - child PID (と pgid)
  - 子の状態: running / **stopped** (child_stopped は実装済み・表示済み) / exit 済み (code)
  - daemon PID (= トラブル時に ps と突き合わせる起点)
- `list` のデフォルト列は増やしすぎない (横幅)。`--format=jsonl` には全部入れ、
  plain は PID 列 + STATUS 拡充くらいが現実的か
- `status <session>` には詳細を全部出す (人間向け詳細ビュー)

## 関連

- DR-0017 で child_stopped は status/list に追加済み (= その拡張)
- docs/journal/2026-06-10-review-fixes-and-release-repair.md (孤児 daemon の発見経緯は 6/11 journal)
