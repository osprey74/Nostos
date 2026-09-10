# firmware/nostos-fw（Phase 2 着手時に有効化するスケルトン）

> ⚠️ **現状はスケルトン**。Xtensa ツールチェーン（espup）導入と実機着手（Phase 2）まではビルドしない。
> ワークスペースからは `exclude` 済み（ホストの `cargo test` を汚さないため）。

## 位置づけ

PaperMono（ESP32-S3 / SX1262 / SSD1677）上で動く受信ファーム本体。
言語非依存ロジック（`nostos-nav` / `nostos-meshtastic`）を**そのまま呼ぶ**のが設計目標:

```text
SX1262 RX (LongFast, JP 920MHz)
  └─ フルペイロード読み出し
       └─ nostos_meshtastic::decode_position(frame, &DEFAULT_CHANNEL_KEY)
            └─ nostos_nav::Trail::push(GeoPoint::from_meshtastic_i(..))
                 └─ e-ink 描画（グリッド＋ブレッドクラム）＋ Trail::homing() で帰路方位
```

## 有効化手順（Phase 2）

1. `docs/TOOLCHAIN.md` に従い espup / espflash を導入。
2. 本ディレクトリを親の `Cargo.toml` の `exclude` から外し、独立ビルド or Nostos xtask を用意。
3. papermono-rs の BSP crate を **git 依存**で参照（`Cargo.toml` の TODO 参照）。
4. papermono-rs `firmware/embassy-debug` から以下を移植:
   - `ioe.rs`（M5IOE1 経由 GPIO 制御）
   - `board.rs`（システム I2C 立ち上げ）
   - `lora.rs` の `power_up` / `power_down` シーケンス
   - `listen_rx` を **フルペイロード読み出し**に改造（差分は `docs/PHASE0.md`）
5. 周波数を **JP 920MHz 帯**に設定（C6L 実機設定と一致させる）。

## 参照

- ハード配線・差分分析: [`../../docs/PHASE0.md`](../../docs/PHASE0.md)
- ツールチェーン: [`../../docs/TOOLCHAIN.md`](../../docs/TOOLCHAIN.md)
- 土台: `g:\dev\papermono-rs`（MIT）
