# ClawPod ドキュメント

このディレクトリには、ClawPod の運用・利用・設計に関する Markdown を置きます。

現状の `docs/` は設計メモ中心でしたが、今後は次の2系統で整理します。

- 基礎ドキュメント: 使い方、設定、構成の説明
- 設計メモ: 日付付きの検討記録、設計草案、比較資料

## 基礎ドキュメント

- [overview.md](./overview.md): ClawPod の概要、主要概念、処理フロー
- [getting-started.md](./getting-started.md): ローカル起動、最初の疎通確認、日常運用の入口
- [configuration.md](./configuration.md): `clawpod.toml` の設定リファレンス
- [architecture.md](./architecture.md): 実装構成、主要クレート、データの流れ

## 設計メモ

- [heartbeat-openclaw-parity.md](./heartbeat-openclaw-parity.md): heartbeat 機能の OpenClaw parity 設計
- [20260326_heartbeat-cross-system-comparison.md](./20260326_heartbeat-cross-system-comparison.md): 複数システム間の heartbeat 比較
- [20260326_run-execution-viewer.md](./20260326_run-execution-viewer.md): 実行ビューアの設計メモ
- [20260403_browser_profiles_kasmvnc_design.md](./20260403_browser_profiles_kasmvnc_design.md): ブラウザプロファイルと KasmVNC の設計

## 読み進め方

初めて触る場合は、次の順で読むのが最短です。

1. [overview.md](./overview.md)
2. [getting-started.md](./getting-started.md)
3. [configuration.md](./configuration.md)
4. [architecture.md](./architecture.md)

## 更新方針

- 実装に追随すべき内容は、README より先に `docs/` を更新する
- 設計途中の話は日付付きファイルに分ける
- 仕様として確定したものは、日付付きメモから基礎ドキュメントへ移す
