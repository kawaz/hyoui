# Homebrew tap への自動 push セットアップ (deploy key パターン)

- Status: Open (要 user 承認)
- Date: 2026-05-27
- Priority: High (営業面の primary CTA、R5-SAL-C3 / R5-H17)
- 発見元: R5 marketing review (Sales ペルソナ) で README primary CTA が
  `(planned)` のまま放置されていると指摘

## 背景

README の Installation セクション primary CTA は `brew install kawaz/tap/hyoui`
が望ましいが、現状は kawaz/homebrew-tap への formula 公開が未実施。
本 issue で release.yml から自動 push する経路を確立する。

応急処置として README の Installation 順序を入れ替え済 (Pre-built binaries を
先頭、Homebrew を最後尾) だが、本 issue 完了後は brew を primary CTA に戻す。

## 参照

- 手順テンプレ: `~/.claude-personal/rules/homebrew-tap-deploy-key.md`
- 参考実装: kawaz/{authsock-warden, stable-which, authsock-filter}
- 既存 tap: kawaz/homebrew-tap (= 既にいくつかの formula が登録済)

## やること (チェックリスト)

- [ ] kawaz に AskUserQuestion で承認取得 (= 鍵生成 + secret 登録 +
  tap deploy key 登録 + release.yml 修正をまとめて 1 承認)
- [ ] `~/.claude-personal/rules/homebrew-tap-deploy-key.md` の手順に沿って:
  - [ ] ed25519 鍵ペアを `mktemp -d` 配下で生成 (`-C "kawaz/hyoui -> kawaz/homebrew-tap deploy key"`)
  - [ ] 秘密鍵を `kawaz/hyoui` repo の `HOMEBREW_TAP_DEPLOY_KEY` secret に登録
  - [ ] 公開鍵を `kawaz/homebrew-tap` の deploy key に write 権限付きで登録
    (title: `hyoui release`)
  - [ ] `mktemp` ディレクトリを trap で確実に削除
- [ ] `.github/workflows/release.yml` に homebrew tap への push step 追加
  (artifact 公開後、`secrets.HOMEBREW_TAP_DEPLOY_KEY` を使って tap repo に
  Formula/hyoui.rb を更新 push)
- [ ] 動作確認:
  - [ ] 次回 release で workflow が green
  - [ ] `brew install kawaz/tap/hyoui && hyoui --version` が通る
- [ ] 完了後の後処理:
  - [ ] README の Installation 順序を元に戻し (Homebrew を primary CTA に)
  - [ ] `(planned)` の disclaimer 削除
  - [ ] 本 issue を delete (jj/git 履歴で追えるため)

## 完了条件

- `brew install kawaz/tap/hyoui` が任意の Mac で 1 行で動く
- 次回以降の release で人手介入なしに tap が更新される

## 注意

- deploy key 生成は鍵漏洩リスクがあるため kawaz 承認必須
- 鍵は `~/.ssh/` に残さない (mktemp + trap で 1 セッション内完結)
- 漏洩疑いがあれば即 rotate (kawaz/homebrew-tap の deploy key delete + 新規鍵で再登録)
