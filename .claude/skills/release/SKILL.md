---
name: release
description: 前回リリース以降のコミットからリリースノートを下書きし、バージョンを上げて GitHub Release を作成する。ユーザーが「リリースして」「新しいバージョンを出して」と言ったとき、または /release で呼ばれたときに使う。
---

# リリース手順

lapse の新バージョンを GitHub Releases に公開する。Release は `gh release create` で作成し、
タグ push をトリガーに `.github/workflows/release.yml` が 5 ターゲットのバイナリを添付する。

## 手順

1. **事前チェック**（1 つでも満たさなければ中断してユーザーに報告する）
   - `git status` — 作業ツリーがクリーンであること
   - 現在のブランチが `main` で、`origin/main` と同期していること（`git fetch && git status`）
   - HEAD の CI が成功していること: `gh run list --branch main --limit 1` で最新 run が success

2. **変更コミットの収集**
   - 前回タグ: `git describe --tags --abbrev=0`
   - 変更一覧: `git log <前回タグ>..HEAD --oneline`
   - 変更が 0 件なら中断してユーザーに報告する

3. **バージョン番号の決定**（pre-1.0 の運用）
   - 挙動の変更・新機能・CLI インターフェースの変更がある → minor を上げる（0.x.0）
   - バグ修正・ドキュメント・内部リファクタのみ → patch を上げる（0.x.y）
   - 判断に迷う場合はユーザーに聞く

4. **リリースノートの下書き**
   - コミットメッセージをそのまま並べず、**ツールの利用者視点の言葉**で書き直す
   - 分類して書く（該当がないセクションは省く）: `## 新機能` `## 変更` `## 修正` `## 内部`
   - 挙動が変わる変更は必ず「以前はこうだった → こうなる」が伝わるように書く
   - 内部リファクタ（型の導入・CI 追加など、利用者に影響しないもの）は「内部」に 1 行ずつで簡潔に

5. **バージョン更新のコミット**
   - `Cargo.toml` の `version` を更新し、`cargo build` で `Cargo.lock` にも反映する
   - `vX.Y.Z` というメッセージでコミットして push する

6. **ユーザー確認**（必須・スキップ禁止）
   - バージョン番号とリリースノート全文を提示し、公開してよいか確認を取る
   - 修正指示があればノートを直して再確認する

7. **リリース作成と検証**
   - `gh release create vX.Y.Z --title "vX.Y.Z" --notes "<ノート>"`（タグも同時に作られる）
   - タグ push で Release ワークフローが起動するので `gh run watch` で完了を待つ
   - `gh release view vX.Y.Z --json assets` でバイナリ 5 点（Linux x2 / macOS x2 / Windows x1）が
     添付されたことを確認し、リリース URL をユーザーに報告する

## 注意

- ワークフローの `softprops/action-gh-release` は既存リリースの本文を上書きしないので、
  手書きノートはそのまま残る
- リリース後にノートを直したいときは `gh release edit vX.Y.Z --notes "<修正版>"`
