# kill は signal 送信で即時応答すべき (終了の見届けは --wait オプションに)

- Date: 2026-06-11
- Status: open
- Priority: 高 (= kawaz 実機で「kill が無応答」体験が発生済み)
- 提案者: kawaz (2026-06-11)

## 現象 / 背景

`hyoui kill <session>` は「signal 送信 → daemon が child の exit を観測 → session 終了 →
client に応答」という **terminate を見届ける構造**になっており、子が signal 1 発で死なない
アプリだと応答が返らない。

実例: claude は SIGTERM / SIGINT を catch して「Press Ctrl-C again to exit」UI を出し
**1 回では死なない** (2 連で終了する仕様)。このため `hyoui kill` が無応答に見え、ユーザが
^C で client を中断 → daemon が非正規経路で終了して stale socket が残った。
(/bin/sleep のような「TERM 即死」の子では再現しない)

## 提案 (kawaz)

- **default: kill(1) と同じ直感** — daemon に signal 送信依頼が受理された時点で即時 return。
  子が実際に死ぬかは関知しない (= 普通の `kill` で claude を撃っても 1 回では死なないのと同じ
  挙動が観察できるのが正しい透過)
- **`--wait` オプション** — 従来の「child exit + session 終了まで見届けて返る」挙動。
  「kill 直後に同名 session を作り直す」スクリプト等で必要

## 実装メモ

- daemon 側: kill 受理 → signal 送信 → **即 ack を返す** (terminate は非同期に進行)。
  protocol 変更の要否を確認 (= 既存応答 frame の意味論変更で済むか、ack message が要るか)
- client 側: default は ack 受信で return。`--wait` 時は SessionExitNotify (または socket EOF)
  まで待つ
- `--no-terminate` (DR-0017 で追加) との整理: signal だけ送って session を残す経路とは別軸
  (= terminate するか / 待つか の 2 軸)。usage の説明を整理する
- stale socket の発生も減る (= daemon が応答待ちでユーザ ^C を誘発する構造が消える)

## 関連

- [[DR-0006]] CLI ground rules
- [[DR-0017]] suspend policy (--no-terminate)
- docs/issue/2026-06-11-bug-suspend-resume-outer-tty-state.md (同日の実機検証で発見)
