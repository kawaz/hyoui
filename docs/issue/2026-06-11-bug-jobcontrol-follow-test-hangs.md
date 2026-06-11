# bug: jobcontrol_follow の ignored テストがローカルでもハングする (flaky ではなく再現性あり)

- Date: 2026-06-11
- Status: open
- Priority: 中 (= CI では step timeout で吸収済みだが、ローカルで `--ignored` を回す
  エージェント/開発者が毎回 10 分超ブロックされる)

## 現象

`cargo test --workspace -- --ignored` 実行時、`jobcontrol_follow` テストが終了せず
ハングする。2026-06-11 に別々のエージェントセッションで連続 2 回再現
(14 分 / 27 分経過しても終わらず、手動 kill が必要だった)。
従来「CI 不安定 (flaky)」として #[ignore] されていたが、ローカルでも高頻度で
ハングするため、timing flaky ではなく**決定的に近いハング**の可能性が高い。

## 背景

- DR-0017 (session anchor 化 + auto-resume 廃止) の前後で jobcontrol の挙動が変わって
  おり、テストの前提 (= 旧 auto-resume 挙動や旧プロセス構造) が現実装と合っていない
  可能性がある
- テスト自体に per-test timeout が無く、待ち条件が満たされないと無限待ちになる構造

## TODO

- [ ] テストの待ち条件を読み、DR-0017 後の挙動と突き合わせる
- [ ] テスト内の待ちループに deadline を入れる (= ハングではなく fail で落ちるように)
- [ ] 根本の race (handshake / linger) は DR-0015 Task 25/28 系の既存課題と統合検討

## 関連

- [[DR-0017]] / [[DR-0015]]
- .github/workflows/ci.yml の ignored-tests ジョブ (step timeout 13m で CI 側は吸収済み)
