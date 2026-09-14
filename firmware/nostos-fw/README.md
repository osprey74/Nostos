# firmware/nostos-fw — PaperMono 受信ファーム（Phase 2 本実装）

PaperMono（ESP32-S3 / SX1262 / SSD1677）で動く **Nostos 受信機本体**。
C6L ビーコンの 16 バイト NostosFrame（923.000 MHz / BW125 / SF9 / sync 0x3A）を
連続受信し、North-up グリッドにブレッドクラム軌跡を e-ink 描画する。**受信専用（送信なし＝技適対象外）**。

```text
SX1262 RX 連続 camp（lora.rs）
  └─ nostos_frame::NostosFrame::decode
       ├─ FLAG_HOME → 出発点を設定/更新（座標が変われば Trail リセット）
       └─ FIX_VALID → nostos_nav::Trail::push
            └─ draw.rs（軌跡マップ）→ panel.rs（SSD1677 OTP モノクロ・部分更新バジェット）
```

## 構成

| ファイル | 役割 | 出自 |
| --- | --- | --- |
| `src/main.rs` | bring-up・受信ループ・状態管理 | 新規（embassy-debug の起動順を踏襲） |
| `src/ioe.rs` | M5IOE1・電源レール（IP2315 隔離・EPD_VDD） | papermono-rs `ioe.rs`+`touch_bus.rs` 最小移植（MIT） |
| `src/lora.rs` | SX1262 受信専用ドライバ（連続 RX・再アーム） | papermono-rs `lora.rs::listen_rx` 移植 |
| `src/panel.rs` | SSD1677 OTP モノクロ（フル/部分・DC バイアス保護） | papermono-rs `panel.rs` 移植 |
| `src/draw.rs` | 軌跡マップ描画（`docs/UI.md` 第1画面） | 新規（GrayInk パターン踏襲） |

BSP（`m5stack-papermono-lite` / `m5stack-papermono`）は `g:\dev\papermono-rs` を **path 依存**で参照。
共通ロジックは本リポジトリの `nostos-frame` / `nostos-nav`。

## 操作（現状）

- **下部 3 タブ（軌跡 / 帰路 / 設定・日本語表示）のタップ**: 画面切替
- **ボタン A / B**: 地図画面＝ズームイン/アウト（1〜100 m/px の 7 段階）、
  設定画面＝フロントライト明るさ ▲/▼（5 段階）
- **スワイプ**: 軌跡マップのパン（1 スワイプ＝1 回再描画・自動追従停止＋「PAN」表示）
- **長押し（0.8s）**: 再センタリング（最新受信点への自動追従に復帰）
- 画面更新は「フレーム受信・操作・3 分無受信」時。軌跡/設定＝モノクロ（部分更新 18 回ごと
  に自動フル）、帰路＝4 階調 GrayFull（常にフル更新）。

## 画面

1. **軌跡マップ**（モノクロ）: North-up グリッド・破線コネクタ・白抜き経由点＋連番・
   家アイコン（出発点）・現在地 ◉・スケールバー・時刻/電池/受信経過ヘッダ
2. **帰路ナビ**（4 階調）: 現在地中心固定・薄墨（LIGHT）の来た道＋レンジリング・
   **濃破線の帰路方位**・家マーカー直下に距離と方位（BRG）。HOME フレーム未受信の間は
   最初の受信点を暫定出発点とし「**HOME\***」表記（受信で正式値に置換）
3. **設定**（モノクロ）: 明るさ 5 段階（A/B）・自動消灯 30s（行タップでトグル）・
   通知 LED 凡例・BAT/VIN 電圧・FW バージョン

## 通知 LED（左側面 RGB・緑=IOE PYG8 / 青=PYG9 / 赤=PM1 LED_EN）

優先度順: **赤点滅**=低電池(<3.5V) ＞ **橙ゆっくり点滅**=受信途絶(3 分) ＞
**緑 1 回点滅**=新規フレーム受信 ＞ **青点灯**=USB 給電/充電 ＞ 消灯=待機。

## 電源（現状の制約と運用・2026-09-13）

- **起動**: USB（VIN）給電で自動起動（モバイルバッテリ可）。起動後に USB を抜いても動作は継続する
  （PM1 に LDO/DCDC 有効＋電源ホールド＋ウォッチドッグ無効を書き込むため）
- **電源オフ**: 設定タブで画面 1 秒長押し（PM1 シャットダウン）
- **サイド電源ボタンの誤操作対策（2026-09-14 実機確認）**: 誤クリックによる電源断/リセット
  （→コールドブート固着）を防ぐため、PM1 のボタン設定で **単クリック・リセット
  （SINGLE_RST_DIS=1）とダブルクリック電源オフ（DOUBLE_OFF_DIS=1）を無効化**、長押しは
  4 秒に延長（LONG_DLY=11）。加えて**ボタン割り込みをマスク**（IRQ_MASK3）して未処理割り込みに
  よる LED 点滅を抑止。実機で「単/ダブルクリックしても無反応・LED 青点灯のまま」を確認。
  実装は [`src/ioe.rs`] `configure_power_button()`。4 秒長押し→download mode は温存（DL_LOCK=0）。
  復旧はボタン非依存の espflash（USB）で常に可能。
- **バッテリ単体でのコールドブートは未解決**: CPU 起動と電源維持は成功する
  （起動ビープ「ピッ→ピピッ」で確認可）が、表示系 5V の安全な有効化方法が未確立。
  ⚠️ `PWR_CFG` の BOOST(bit3) を VIN あり状態で ON にすると 5V レール競合で表示系が
  死ぬ（実測）。安易に触らないこと
- ⚠️ **PM1 への I2C 書き込みはバス整定（500ms）後に行う**。整定前の書き込みは化けて
  別レジスタを破壊し得る（PM1 はバッテリで常時生存＝壊れた設定はリセットでも残留）
- **【2026-09-14 実機検証で判明した事実（重要）】コールドブート/固着問題**:
  - ✅ **画面静止時の USB 抜き挿しは固着しない**。抜くと電源ホールドでバッテリ動作継続
    （LED 消灯＝待機）、再接続で USB 復帰（LED 青→受信状態）。正しく処理される（実機確認）。
    以前固着したのは**画面更新中に抜いた**か電源ボタン等の別過渡が原因と推定。
  - ❌ **真のコールドブート（電源ボタン 2 秒→短押し＝PM1 が 5V 含む全レールを落とす完全
    電源サイクル→当方 FW 起動）は依然として固着する**。booster 0x40・EPD_VDD(io3) 強制
    コールドサイクルの**両方を入れても固着は解けなかった**（2 仮説とも否定）。
  - ❌ **EPD_VDD(io3) 給電維持説は否定**: [panel.rs `begin()`] で EPD_VDD を LOW(300ms)→
    HIGH と強制サイクルしても、固着したパネルは回復しなかった。io3 は 3.3V ロジック電源
    のみで、固着状態は**高圧チャージポンプ/5V 側**にあると推測。工場の電源ボタン回復が
    効くのは PM1 が **5V DCDC を含む全レール**を落とすから（io3 単独では不十分）。
  - ❌ **明示 `_power_on`（0x22=0xC0→0x20→BUSY 待ち）説も否定**: init 後に工場相当の
    アナログ電源投入を独立ステップで挿入したが、真コールドブートで依然固着（ビープは鳴る
    ＝ESP 起動・画面固着・赤 LED）。効果なしのため revert 済み。
  - ❌ **5V/PM1 レール説も否定（M5Unified ソース確認）**: `Power_Class::begin()` の PaperMono
    ブロック（m5unified `Power_Class.cpp:528`）は PM1 のウェイク/IRQ クリア・電源ボタン
    (PM1 GPIO0)/IRQ(GPIO1)・IOE1 G14(SD電源)・IP2315 隔離のみ。**`setExtOutput`/`setBoostEnable`
    (5V BOOST) は呼ばない**。LDO/DCDC は PM1 起動時に自動有効。当方は既に同等＝5V/BOOST は無関係。
  - **当面の運用は「電源を切らない」**（USB／モバイルバッテリで連続給電。e-ink なので低消費）。
  - **残る本命方向（次の大工事）**: 工場 PaperMono は M5GFX `Panel_SSD1677_4Gray` で**カスタム
    LUT(0x32)＋明示駆動電圧（VGH/VSH/VSL/VCOM を 0x03/0x04/0x2C）**を書く。当方は built-in OTP
    波形（OTP 内蔵電圧）。冷えたパネルで OTP 内蔵電圧では駆動が立たない疑い。→ **M5GFX 駆動
    方式の移植**が本筋（panel.rs 実質書き換え＋反復コールド試験を要する）。
  - なお booster 0x40・io3 コールドサイクルは**工場値/正しい順序への是正として有効**なので
    コミット済み（papermono-rs `3bcd965` / Nostos `fae17fc`）。固着の主因ではなかっただけ。
- **固着時の復旧手順**（唯一の実績ある方法）:
  1. 工場イメージ書き込み: `espflash write-bin --port COM8 0x0 backups\papermono-factory-original-*.bin`
  2. 電源ボタン 2 秒長押し（赤点滅＝ダウンロードモード）→ 短押し → 工場デモ表示を確認
  3. nostos-fw を書き戻す（espflash flash）→ ウォーム状態では正常に表示される
- 調査済みの手がかり: M5GFX 工場ドライバとの差分は
  ①ブースタソフトスタート最終バイト（工場 `0x40`／OTP デモ由来の当方 `0x80`）
  ②`_after_wake` の RAM 自動クリア 0x46/0x47（移植済み・単独では回復せず）
  ③固着の引き金は「パネルが Deep Sleep Mode 1 のまま電源断」の疑い
- **【2026-09-14 ソース確証・修正適用済み／実機コールドブート試験＝否定】**
  工場 M5GFX `Panel_SSD1677::getInitCommands` list0（`M5PaperMono-UserDemo` の依存
  M5GFX @ `02107b8`）と当方 init を厳密照合。手がかり①を確証し、以下を適用:
  - `ssd1677-otp` の `BOOSTER_SOFT_START_OTP` 最終バイトを工場値 **`0x40`** に変更。
    OTP-Demo が 0x80 でも動くのは `M5.begin()` で先に M5GFX が 0x40 で電源投入するため。
  - `init_mono` のコマンド順を工場 list0 に一致（温度センサ 0x18 を booster より前へ）。
  - **実機コールドブートで検証した結果、booster 0x40 でも固着は解けなかった**（当初「主因」
    と推定したが否定）。工場値への是正としては正しいのでコミット済み。真因は別（上記
    「実機検証で判明した事実」の 5V/PM1 全レール電源サイクル差分を参照）。
- USB の抜き挿しは**画面静止時**に行うこと（更新中の電源変動が固着の引き金）
- **解析リソース（2026-09-13 判明・次回はソース照合から着手）**:
  - 工場ファームのソースが公開されている:
    [M5PaperMono-UserDemo](https://github.com/m5stack/M5PaperMono-UserDemo)（ESP-IDF・工場ファーム本体）／
    [M5PaperMono-OTP-Demo](https://github.com/m5stack/M5PaperMono-OTP-Demo)（SSD1677 直叩き・OTP 波形の最小例）
  - 電源チップのドライバも Arduino ライブラリ **M5PM1 / M5IOE1** として公開（レジスタレベルで読める）
  - 公式 docs（[電源管理](https://docs.m5stack.com/ja/arduino/papermono/m5pm1_m5ioe1)）: 電源は
    L0〜L3B の階層構造。**L3B（画面・タッチ等）は IOE1 で個別制御**。e-ink 電源=IOE1 P3／リセット=P5
  - IOE1 ピンは使用前に `setHighImpedance(pin, false)` の解除が必要（公式 microSD 例）。
    **コールドブート時に e-ink 電源ピンの高インピーダンス解除が漏れている疑い（第一容疑）**
  - LoRa は電源=PM1 GPIO2／リセット=IOE1 GPIO10／アンテナ SW=IOE1 GPIO2（公式 LoRa ページ）
- 復旧実績（2026-09-13）: 電源ボタン誤操作でフリーズ → ダウンロードモード →
  上記手順 1 の工場イメージ書き込みで復旧を確認（`espflash write-bin` 正常終了・工場デモ起動）

## 日本語表示

タブ・設定画面のラベルは 16×16 の 1bpp グリフ（`src/jpfont.rs`）。
文字の追加は `tools/gen_jpfont.py` の `CHARS` に追記して再生成（BIZ UDゴシックから変換）。

## microSD CSV ロガー（2026-09-14 実機確認）

受信した NostosFrame を **microSD に CSV 追記**するオフラインロガー。実験後に SD を抜いて
PC で回収できる（実 GPS フィールドテスト用）。

- **ハード**: SDHOST ペリフェラル（SPI2 パネル / SPI3 LoRa とは別）を GPIO マトリクスで
  **1bit 配線**（CLK=GPIO13 / CMD=GPIO12 / DAT0=GPIO11）。SD 電源=IOE1 PYG14（`bring_up` で投入）
- **スタック**: `sdio`（SD カード初期化）＋ `embedded-fatfs`（FAT32・async）＋ `embedded-partitions`
- **ファイル**: `NOSTOS.CSV`（再起動をまたいで追記・追記ごとに flush＋unmount）
- **形式**: `time_unix,seq,fix,home,lat_e7,lon_e7,rssi,snr`（緯度経度は 1e7 整数＝PC 側で ÷1e7）
- **異常時**: カード無し/初期化失敗でも受信は継続（起動ログ `sdlog card ok` / `none/fail`、
  追記失敗時のみ `nostos-rx: sdlog append failed`）
- 実装: [`src/sdlog.rs`]。カードは要 FAT32 フォーマット。抜くのはフレーム受信の**合間**（カード
  アイドル時）に。抜いた後にロギング再開するにはカード再挿入＋再起動（SD 初期化は起動時 1 回）。

## ビルド / フラッシュ（Windows）

```powershell
# esp 環境（espup）を通す
. C:\Users\ospre\export-esp.ps1
cd g:\dev\Nostos\firmware\nostos-fw
cargo +esp build --release   # target/build-std/linkall は .cargo/config.toml が供給

espflash flash --port COM8 --monitor target\xtensa-esp32s3-none-elf\release\nostos-fw
```

> ⚠️ シリアル観測は `espflash flash --monitor`（または DTR/RTS off の `pio device monitor`）で。
> 素の SerialPort で COM8 を開くと USB-Serial-JTAG が download モードへ落ちる（詳細は
> [`experiments/README.md`](experiments/README.md) の注意書き）。

## 送信側（テスト）

`firmware/c6l-beacon` を COM7 へ。屋内 GPS なしなら `env:c6l-beacon-dummy`（合成トラック送信）。

## 未実装（次フェーズ）

- 下部タブ（タッチ座標デコード）・設定タブ（フロントライト 5 段階）・RGB LED 通知
- 日本語ラベル（ビットマップフォント埋め込み）

※ C6L 側 UI（OLED 3 ページ・長押し HOME 確定＋`FLAG_HOME` 送出）は 2026-09-13 実機確認済み
（`firmware/c6l-beacon`）。HOME フレーム受信で HOME\* → 正式 HOME への置換も E2E 確認済み。

## 参照

- 仕様: [`../../docs/UI.md`](../../docs/UI.md) / 配線・周波数: [`../../docs/PHASE0.md`](../../docs/PHASE0.md)
- 実機実証記録: [`experiments/README.md`](experiments/README.md)
