# ツールチェーン導入手順（Route A: Rust / embassy）

> 対象: Windows（作業パス `g:\dev\Nostos\`）。ESP32-S3 は **Xtensa** アーキのため esp-rs の LLVM フォークが必須。
> 実機フラッシュ前に papermono-rs の [`docs/SAFETY.md`](https://github.com/canardleteer/papermono-rs) を必読。

## 2 層に分ける

| 層 | 目的 | 追加ツール |
| --- | --- | --- |
| **ホスト層** | `nostos-nav` / `nostos-meshtastic` の単体テスト（実機不要） | 追加不要（既存 stable rustc 1.98 で `cargo test`） |
| **デバイス層** | `firmware/nostos-fw` を Xtensa 向けにビルド・フラッシュ | espup / espflash（下記） |

まずホスト層だけで Phase 1・3 のロジックを固める。デバイス層は Phase 0/2 の実機着手時に導入。

## ホスト層（今すぐ使える）

```powershell
# ワークスペースのホスト crate をテスト（Xtensa 不要）
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

firmware は default-members に含めない／ワークスペース exclude にしているため、
上記コマンドは Xtensa ターゲットを一切引かない（papermono-rs と同方針）。

## デバイス層（Phase 0 実機着手時）

### 1. espup で Xtensa ツールチェーン導入

```powershell
cargo install espup --locked
espup install
# 環境変数エクスポートスクリプトが生成される（Windows は %USERPROFILE%\export-esp.ps1）
. $env:USERPROFILE\export-esp.ps1   # 各シェルセッションで読み込む
```

- `espup install` が `esp` toolchain（Xtensa 対応 rustc/LLVM フォーク）と `xtensa-esp32s3-none-elf` を導入。
- 現状（2026-09-10）本機には **espup / espflash / Xtensa target 未導入**。上記で追加する。

### 2. espflash（フラッシュ・モニタ）

```powershell
cargo install espflash --locked
espflash --version
```

### 3. papermono-rs 側の xtask フロー（フラッシュはこちら経由が安全）

papermono-rs はフラッシュ操作を `cargo xtask` に集約している（`cargo run` では絶対にフラッシュしない設計）。
Nostos firmware も同方針を踏襲予定。papermono-rs リポジトリ内での標準手順:

```powershell
. $env:USERPROFILE\export-esp.ps1

# ① 出荷イメージのバックアップ（デバイス毎に1回・必須）
#    電源ボタンを2秒長押し→赤点滅（ダウンロードモード）してから:
cargo xtask backup-factory-firmware --as-original

# ② ビルド（Full SKU C153）
cargo xtask build-fw embassy-debug --features c153

# ③ フラッシュ（複数機接続時は ESPFLASH_PORT を指定）
cargo xtask flash-app --image target/xtensa-esp32s3-none-elf/release-fw/embassy-debug.bin --yes

# ④ CDC モニタ（受信ログ確認）
cargo xtask monitor
```

### ⚠️ フラッシュ 4 原則（papermono-rs getting-started より）

1. **フラッシュ前に必ず出荷イメージを保存**。`espflash erase-flash` や全消去は避ける。
2. **e-paper 波形を自作しない**。パネル OTP シーケンスを使う。約10回の部分更新ごとに全更新（ゴースト防止）。
3. **IP2315（バッテリコントローラ）は充電時以外システム I2C から切り離す**（低電圧時の I2C ロック防止）。
4. **ダウンロードモードは電源ボタン長押し**（約2秒・赤点滅）。GPIO0/3 はストラップ、GPIO45/46 は PDM マイク。

## Nostos firmware のビルド（Phase 2 以降・予定）

`firmware/nostos-fw/` は現状スケルトン（ワークスペース exclude）。実機着手時に:

- papermono-rs の BSP crate（`m5stack-papermono` 等）を **git 依存**で参照。
- `firmware/embassy-debug` の `ioe.rs`・`board.rs`・電源シーケンスを移植。
- ビルドは `cargo +esp build -p nostos-fw --profile release-fw --target xtensa-esp32s3-none-elf -Zbuild-std=core,alloc`
  もしくは Nostos 側 xtask（未整備）経由。

詳細な結線・差分は [PHASE0.md](PHASE0.md) を参照。
