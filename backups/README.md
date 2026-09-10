# backups/

PaperMono 実機フラッシュの**工場出荷イメージ（フルダンプ）**を置くディレクトリ。

- `.bin` などのイメージ本体は **`.gitignore` 済み（コミットしない）**。この README のみ追跡する。
- 取得・書き戻し手順は [`../docs/TOOLCHAIN.md`](../docs/TOOLCHAIN.md) の
  「工場出荷イメージのバックアップ／復元」を参照。
- 命名例: `papermono-factory-original-YYYY-MM-DD.bin`（フル 16MB）。

> ⚠️ 別のファームを焼く前に必ずここへ原本を保存すること。全消去（`espflash erase-flash`）は禁止。
