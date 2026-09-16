//! M5IOE1 I/O エキスパンダ制御とシステム I2C bring-up（受信ファーム向け最小構成）。
//!
//! papermono-rs `firmware/embassy-debug` の `ioe.rs` / `touch_bus.rs`（MIT）から
//! Nostos 受信機に必要な部分のみを移植：
//! - M5IOE1 の発見（`0x4F` → `0x6F` フォールバック）と wake プロトコル
//! - push-pull 出力設定（EPD/LoRa の電源ゲート・リセット線）
//! - 電源レール bring-up（IP2315 を I2C バスから隔離、EPD_VDD 投入）
//!
//! touch / NFC / MicroSD / フロントライト / テレメトリは扱わない（将来の設定タブで追加）。

use core::sync::atomic::{AtomicU8, Ordering};

use embassy_time::{Duration, Timer};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::time::Rate;
use m5stack_papermono_lite::addresses;
use m5stack_papermono_lite::ioe1;
use m5stack_papermono_lite::pmic;

/// システムバス用ブロッキング I2C ドライバの別名。
pub type SysI2c = I2c<'static, esp_hal::Blocking>;

/// 実行時に発見された M5IOE1 のアドレス（`0x4F` または `0x6F`）。
static IOE_ADDR: AtomicU8 = AtomicU8::new(addresses::M5IOE1);

/// 電源レール投入から I2C ポーリング開始までの整定待ち。
const POWER_SETTLE_MS: u64 = 500;

/// M5IOE1 wake 信号送出からレジスタ読み出しまでの整定待ち。
const WAKE_SETTLE_MS: u64 = 10;

/// 100 kHz バス初期化リトライの間隔。
const INIT_RETRY_MS: u64 = 800;

/// レジスタポインタ書き込み＋1 バイト読み出しでデバイス存在を確認する。
pub fn probe_read(i2c: &mut SysI2c, addr: u8, reg: u8) -> bool {
    write_then_read(i2c, addr, reg).is_ok()
}

fn write_then_read(i2c: &mut SysI2c, addr: u8, reg: u8) -> Result<u8, esp_hal::i2c::master::Error> {
    i2c.write(addr, &[reg])?;
    let mut val = [0u8];
    i2c.read(addr, &mut val)?;
    Ok(val[0])
}

/// UID/REV レジスタ読み出しで M5IOE1 の実在を確認する。
fn ident(i2c: &mut SysI2c, addr: u8) -> bool {
    let mut uid = [0u8; 2];
    if i2c.write(addr, &[ioe1::UID_L]).is_err() {
        return false;
    }
    if i2c.read(addr, &mut uid).is_err() {
        return false;
    }
    write_then_read(i2c, addr, ioe1::REV).is_ok()
}

/// エキスパンダ MCU へ wake トランザクションを送る。
fn wake(i2c: &mut SysI2c, addr: u8) {
    let _ = i2c.write(addr, &[ioe1::UID_L]);
}

/// 指定アドレスで M5IOE1 の初期化を試みる（100 kHz → 400 kHz フォールバック）。
async fn try_init_at(i2c: &mut SysI2c, addr: u8) -> bool {
    let hz100 = Config::default();
    let hz400 = Config::default().with_frequency(Rate::from_khz(400));
    let _ = i2c.apply_config(&hz100);

    wake(i2c, addr);
    Timer::after(Duration::from_millis(WAKE_SETTLE_MS)).await;
    if ident(i2c, addr) {
        return true;
    }

    Timer::after(Duration::from_millis(INIT_RETRY_MS)).await;
    wake(i2c, addr);
    Timer::after(Duration::from_millis(WAKE_SETTLE_MS)).await;
    if ident(i2c, addr) {
        return true;
    }

    let _ = i2c.apply_config(&hz400);
    wake(i2c, addr);
    Timer::after(Duration::from_millis(WAKE_SETTLE_MS)).await;
    let ok = ident(i2c, addr);
    let _ = i2c.apply_config(&hz100);
    ok
}

/// 対応アドレス（`0x4F`, `0x6F`）を走査して M5IOE1 を発見・初期化する。
pub async fn begin_ioe(i2c: &mut SysI2c) -> Option<u8> {
    for &addr in &[addresses::M5IOE1, addresses::M5IOE1_UM] {
        if try_init_at(i2c, addr).await {
            IOE_ADDR.store(addr, Ordering::Relaxed);
            return Some(addr);
        }
    }
    None
}

/// エキスパンダピンを push-pull デジタル出力に設定しレベルを与える（5 回リトライ）。
pub fn set_push_pull_output(
    i2c: &mut SysI2c,
    pyg: u8,
    high: bool,
) -> Result<(), esp_hal::i2c::master::Error> {
    let mut last_err = None;
    for _ in 0..5 {
        match m5stack_papermono_lite::m5ioe1::set_push_pull_output(
            i2c,
            IOE_ADDR.load(Ordering::Relaxed),
            pyg,
            high,
        ) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                embassy_time::block_for(embassy_time::Duration::from_millis(5));
            }
        }
    }
    if let Some(err) = last_err {
        Err(err)
    } else {
        Ok(())
    }
}

/// M5IOE1 の生レジスタを 1 バイト読む（診断用）。
pub fn ioe_read_reg(i2c: &mut SysI2c, reg: u8) -> Option<u8> {
    let mut b = [0u8];
    i2c.write_read(IOE_ADDR.load(Ordering::Relaxed), &[reg], &mut b)
        .ok()
        .map(|_| b[0])
}

/// M5IOE1 の GPIO 関連レジスタを一括ダンプしてシリアルに出す（コールドブート診断）。
/// MODE(0x03/04)=1 出力 / OUT(0x05/06) / IN(0x07/08)=実ピンレベル / PU(0x09/0A) / PD(0x0B/0C) /
/// DRV(0x13/14)=1 オープンドレイン。IN の bit2=io3(EPD_VDD) / bit4=io5(EPD_RST) を見る。
pub fn dump_ioe_gpio(i2c: &mut SysI2c, tag: &str) {
    let r = |i2c: &mut SysI2c, reg: u8| ioe_read_reg(i2c, reg).unwrap_or(0xFF);
    let (m_l, m_h) = (r(i2c, 0x03), r(i2c, 0x04));
    let (o_l, o_h) = (r(i2c, 0x05), r(i2c, 0x06));
    let (i_l, i_h) = (r(i2c, 0x07), r(i2c, 0x08));
    let (pu_l, pu_h) = (r(i2c, 0x09), r(i2c, 0x0A));
    let (pd_l, pd_h) = (r(i2c, 0x0B), r(i2c, 0x0C));
    let (d_l, d_h) = (r(i2c, 0x13), r(i2c, 0x14));
    esp_println::println!(
        "nostos-fw: ioe1[{}] mode={:02x}{:02x} out={:02x}{:02x} in={:02x}{:02x} pu={:02x}{:02x} pd={:02x}{:02x} drv={:02x}{:02x}",
        tag, m_h, m_l, o_h, o_l, i_h, i_l, pu_h, pu_l, pd_h, pd_l, d_h, d_l
    );
}

fn ioe_write_reg(i2c: &mut SysI2c, reg: u8, v: u8) -> bool {
    i2c.write(IOE_ADDR.load(Ordering::Relaxed), &[reg, v]).is_ok()
}

fn ioe_in_bit(i2c: &mut SysI2c, pyg: u8) -> u8 {
    let (reg, bit) = if pyg <= 8 { (0x07, pyg - 1) } else { (0x08, pyg - 9) };
    (ioe_read_reg(i2c, reg).unwrap_or(0) >> bit) & 1
}

/// MODE を入力（＋プルアップ）へ一度落としてから元の設定に戻す「ピンキック」。
/// IOE1 はコールド起動直後、MODE=1/OUT=1/DRV=push-pull と登録済みでも**出力ドライバが
/// 有効化されない**ことがある（2026-09-16 実機: io3=EPD_VDD_EN が実ピン LOW のまま→パネル
/// 無電源→コールドブート固着の真因）。MODE を一度切り替えると駒動が始まる。
fn kick_pin(i2c: &mut SysI2c, pyg: u8) {
    let (m_reg, pu_reg, bit) = if pyg <= 8 {
        (0x03u8, 0x09u8, pyg - 1)
    } else {
        (0x04u8, 0x0Au8, pyg - 9)
    };
    let Some(m) = ioe_read_reg(i2c, m_reg) else { return };
    let Some(pu) = ioe_read_reg(i2c, pu_reg) else { return };
    let _ = ioe_write_reg(i2c, pu_reg, pu | (1 << bit));
    let _ = ioe_write_reg(i2c, m_reg, m & !(1 << bit));
    embassy_time::block_for(embassy_time::Duration::from_millis(5));
    let _ = ioe_write_reg(i2c, pu_reg, pu);
    let _ = ioe_write_reg(i2c, m_reg, m | (1 << bit));
    embassy_time::block_for(embassy_time::Duration::from_millis(2));
}

/// push-pull 出力を設定し、**IN レジスタ（実ピンレベル）で追従を確認**する。追従しなければ
/// [`kick_pin`] で MODE を振り直して再試行（最大 3 回）。電源イネーブル／リセット系のピンに使う。
/// 戻り値は最終的にピンが指定レベルになったか。
pub fn set_output_verified(i2c: &mut SysI2c, pyg: u8, high: bool) -> bool {
    for attempt in 0..3u8 {
        let _ = set_push_pull_output(i2c, pyg, high);
        embassy_time::block_for(embassy_time::Duration::from_millis(2));
        if ioe_in_bit(i2c, pyg) == high as u8 {
            if attempt > 0 {
                esp_println::println!(
                    "nostos-fw: ioe1 pin{} recovered by kick x{} (want {})",
                    pyg,
                    attempt,
                    high as u8
                );
            }
            return true;
        }
        kick_pin(i2c, pyg);
    }
    esp_println::println!(
        "nostos-fw: ioe1 pin{} does NOT follow output (want {})",
        pyg,
        high as u8
    );
    false
}

/// 指定レジスタからのバースト読み出し（FT6336G の座標レジスタ一括取得用）。
pub fn read_burst(i2c: &mut SysI2c, addr: u8, reg: u8, buf: &mut [u8]) -> bool {
    i2c.write_read(addr, &[reg], buf).is_ok()
}

/// FT6336G から第 1 接触点を読む。戻り値は（接触点数, 物理 x, 物理 y）。
///
/// 座標は USB 下向き 480×800 の物理フレームバッファ系（M5GFX 準拠）。
/// 非接触・読み出し失敗は None。
pub fn read_touch(i2c: &mut SysI2c) -> Option<(u8, u16, u16)> {
    use m5stack_papermono_lite::touch;
    const LEN: usize = 1 + (touch::MAX_POINTS as usize) * touch::M5GFX_POINT_BYTES;
    let mut buf = [0u8; LEN];
    if !read_burst(i2c, addresses::FT6336G, touch::M5GFX_STATUS_REG, &mut buf) {
        return None;
    }
    let (n, x, y, _x2, _y2) = touch::decode_m5gfx(&buf)?;
    if n == 0 {
        return None;
    }
    Some((n, x, y))
}

/// M5PM1 の ADC からバッテリ電圧 [mV] を読む（touch_bus `read_adc_mv` と同手順）。
pub fn read_vbat_mv(i2c: &mut SysI2c) -> Option<u16> {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    if let Ok(v) = pm1.read_le16(pmic::VBAT_L) {
        return Some(v);
    }
    let lo = pm1.read_at(pmic::VBAT_L).ok()?;
    let hi = pm1.read_at(pmic::VBAT_L.wrapping_add(1)).ok()?;
    Some(pmic::adc_mv(lo, hi))
}

/// M5PM1 の ADC から VIN 電圧 [mV] を読む（USB 給電/充電の検出用）。
pub fn read_vin_mv(i2c: &mut SysI2c) -> Option<u16> {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    if let Ok(v) = pm1.read_le16(pmic::VIN_L) {
        return Some(v);
    }
    let lo = pm1.read_at(pmic::VIN_L).ok()?;
    let hi = pm1.read_at(pmic::VIN_L.wrapping_add(1)).ok()?;
    Some(pmic::adc_mv(lo, hi))
}

/// M5PM1 の電源ステータス生値 `(PWR_SRC 0x04, PWR_CFG 0x06)` を読む（ステータスログ用）。
/// PWR_SRC: bit0=5VIN 有効 / bit1=5VINOUT 有効 / bit2=電池有効。
/// PWR_CFG: bit0=充電有効 / bit1=DCDC / bit2=LDO / bit3=BOOST / bit4=LED。
pub fn read_pm1_power_regs(i2c: &mut SysI2c) -> (Option<u8>, Option<u8>) {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    let src = pm1.read_at(pmic::PWR_SRC).ok();
    let cfg = pm1.read_at(pmic::PWR_CFG).ok();
    (src, cfg)
}

/// フロントライトの現在の PWM デューティを PM1 から読み戻す（0 = 消灯）。PM1 はバッテリで常時
/// 生存し前回のデューティを保持するため、起動時に UI の段階表示と実際の点灯を一致させるのに使う。
pub fn read_frontlight_duty(i2c: &mut SysI2c) -> Option<u16> {
    use m5stack_papermono_lite::m5pm1::{PWM0_EN, PWM0_HC, PWM0_L};
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    let lo = pm1.read_at(PWM0_L).ok()?;
    let hc = pm1.read_at(PWM0_HC).ok()?;
    if hc & PWM0_EN == 0 {
        return Some(0);
    }
    Some((u16::from(hc & 0x0F) << 8) | u16::from(lo))
}

/// PM1 RTC RAM（0xA0〜・32 バイト・電池で保持）に置く UI 設定の先頭アドレスとマジック。
/// レイアウト: [0xA0]=マジック 0x5A / [0xA1]=明るさ段階 0..4 / [0xA2]=自動消灯 (0/1)。
const PM1_RTC_RAM_UI: u8 = 0xA0;
const PM1_UI_MAGIC: u8 = 0x5A;

/// UI 設定（明るさ段階・自動消灯）を PM1 RTC RAM に保存する。シャットダウン／リブートをまたいで
/// 「選んだ段階」を復元するため（PWM デューティの読み戻しは自動消灯中だと 0 になり使えない）。
pub fn save_ui_settings(i2c: &mut SysI2c, brightness_idx: u8, auto_off: bool) {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    let _ = pm1.write_at(PM1_RTC_RAM_UI, PM1_UI_MAGIC);
    let _ = pm1.write_at(PM1_RTC_RAM_UI + 1, brightness_idx.min(4));
    let _ = pm1.write_at(PM1_RTC_RAM_UI + 2, auto_off as u8);
}

/// PM1 RTC RAM から UI 設定を読む。マジック不一致（初回・工場ファーム後）は `None`。
pub fn load_ui_settings(i2c: &mut SysI2c) -> Option<(u8, bool)> {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    if pm1.read_at(PM1_RTC_RAM_UI).ok()? != PM1_UI_MAGIC {
        return None;
    }
    let idx = pm1.read_at(PM1_RTC_RAM_UI + 1).ok()?.min(4);
    let auto_off = pm1.read_at(PM1_RTC_RAM_UI + 2).ok()? != 0;
    Some((idx, auto_off))
}

/// フロントライトの PWM デューティを設定する（0 = 消灯。touch_bus `apply_lamp` と同手順）。
pub fn set_frontlight(i2c: &mut SysI2c, duty: u16) {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    if duty == 0 {
        let _ = pm1.set_pwm0_duty(0);
    } else {
        let _ = pm1.enable_pwm0(pmic::FRONTLIGHT_PWM);
        let _ = pm1.set_pwm0_duty(duty);
    }
}

/// RGB LED の緑（M5IOE1 PYG8）を設定する。
pub fn set_led_green(i2c: &mut SysI2c, on: bool) {
    let _ = set_push_pull_output(i2c, ioe1::RGB_GREEN, on);
}

/// RGB LED の青（M5IOE1 PYG9）を設定する。
pub fn set_led_blue(i2c: &mut SysI2c, on: bool) {
    let _ = set_push_pull_output(i2c, ioe1::RGB_BLUE, on);
}

/// RGB LED の赤（M5PM1 `LED_EN`）を設定する。
pub fn set_led_red(i2c: &mut SysI2c, on: bool) {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    let _ = pm1.set_led(on);
}

/// M5PM1 の「I2C アイドルスリープ」レジスタ（M5Unified `M5PM1_REG_I2C_CFG`）。
const PM1_REG_I2C_CFG: u8 = 0x09;
/// M5PM1 のウォッチドッグカウンタ（M5Unified `M5PM1_REG_WDT_CNT`）。
const PM1_REG_WDT_CNT: u8 = 0x0A;
/// M5PM1 ボタン設定1（公式 M5PM1 lib `M5PM1_REG_BTN_CFG_1`）。
/// [7]DL_LOCK [6:5]DBL_DLY [4:3]LONG_DLY [2:1]CLK_DLY [0]SINGLE_RST_DIS。
const PM1_REG_BTN_CFG_1: u8 = 0x49;
/// M5PM1 ボタン設定2（公式 M5PM1 lib `M5PM1_REG_BTN_CFG_2`）。[0]DOUBLE_OFF_DIS。
const PM1_REG_BTN_CFG_2: u8 = 0x4A;
/// `BTN_CFG_1` で読み書きするビット群: DL_LOCK(7)+LONG_DLY(4:3)+SINGLE_RST_DIS(0)。
const PM1_BTN_CFG1_MASK: u8 = 0x99;
/// `LONG_DLY=11`（長押し 4 秒）。
const PM1_BTN_LONG_DLY_4S: u8 = 0x18;
/// `SINGLE_RST_DIS`（単クリック・リセット無効・`BTN_CFG_1` bit0）。
/// 実機で単クリックは完全リセット（USB 再列挙）を起こしパネルを固着させたため無効化。
const PM1_BTN_SINGLE_RST_DIS: u8 = 1 << 0;
/// `DOUBLE_OFF_DIS`（ダブルクリック電源オフ無効・`BTN_CFG_2` bit0）。
const PM1_BTN_DOUBLE_OFF_DIS: u8 = 1 << 0;
/// M5PM1 ボタン割り込み状態（`M5PM1_REG_IRQ_STATUS3`）。[2]ダブル [1]ウェイク [0]シングル。
const PM1_REG_IRQ_STATUS3: u8 = 0x42;
/// M5PM1 ボタン割り込みマスク（`M5PM1_REG_IRQ_MASK3`）。bit=1 でマスク（禁止）。
const PM1_REG_IRQ_MASK3: u8 = 0x45;
/// ボタン割り込み全ビット（[2:0]）。
const PM1_BTN_IRQ_ALL: u8 = 0x07;
/// `PWR_CFG` bit1: 5V DCDC 有効（公式 M5PM1 lib `M5PM1_PWR_CFG_DCDC_EN`）。
/// バッテリ駆動時の表示系（EPD/フロントライト）の電源。VIN ありでは外部 5V が
/// 代替するため、未設定でも USB 接続時は症状が出ない。
const PM1_PWR_CFG_DCDC_EN: u8 = 1 << 1;
/// `PWR_CFG` bit2: 3.3V LDO 有効（公式 M5PM1 lib `M5PM1_PWR_CFG_LDO_EN`）。
const PM1_PWR_CFG_LDO_EN: u8 = 1 << 2;

/// M5PM1 を「生かし続ける」設定（公式 UserDemo の wake 処理＋M5Unified `begin()` 相当）。
///
/// **バッテリ駆動時の必須処理**（USB=VIN ありでは落ちないため気づきにくい）:
/// 1. `PWR_CFG` の **LDO_EN(bit2)** — バッテリ→3.3V LDO の給電経路そのもの。
///    これが無いと VIN を抜いた瞬間・ボタン起動の一時給電が切れた時点で電源断
///    （UserDemo は `setLdoEnable(true)`）。
/// 2. `HOLD_CFG` の **LDO hold(bit5)** — LDO の維持（`ldoSetPowerHold(true)`）。
/// 3. `WDT_CNT=0`／`I2C_CFG=0` — PM1 ウォッチドッグと I2C アイドルスリープの無効化
///    （M5Unified `M5PM1_Class::begin()`）。
/// 起動直後に最優先で呼ぶこと。
pub fn hold_power(i2c: &mut SysI2c) -> bool {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    let a = pm1.write_at(PM1_REG_I2C_CFG, 0x00).is_ok(); // I2C アイドルスリープ無効
    let b = pm1.write_at(PM1_REG_WDT_CNT, 0x00).is_ok(); // ウォッチドッグ無効
    // read-modify-write。読み出しに失敗したら書かない（unwrap_or(0) で他ビットを
    // 消すと DCDC/充電等が落ち、PM1 は再起動されないため壊れた設定が残留する）。
    let mut c = false;
    let mut d = false;
    let mut pwr_v = 0xFFu8;
    let mut hold_v = 0xFFu8;
    if let Ok(pwr) = pm1.read_at(pmic::PWR_CFG) {
        // バッテリ→3.3V LDO（システム）＋5V DCDC の経路を有効化。
        // ⚠️ BOOST(bit3) はここで触らない: VIN あり時に ON で 5V レール競合、
        //    かつ操作実験でパネル固着を誘発した（2026-09-13）。バッテリ単体運用の
        //    表示は未解決課題（現状はモバイルバッテリ等の USB 給電起動で運用）。
        pwr_v = pwr | PM1_PWR_CFG_LDO_EN | PM1_PWR_CFG_DCDC_EN;
        c = pm1.write_at(pmic::PWR_CFG, pwr_v).is_ok();
    }
    if let Ok(hold) = pm1.read_at(pmic::HOLD_CFG) {
        hold_v = hold | pmic::HOLD_LDO;
        d = pm1.write_at(pmic::HOLD_CFG, hold_v).is_ok();
    }
    // 0x08 = BATT_LVP（低電圧保護しきい値 mV=2000+n*7.81）。過去の誤書き込み検出用に読む。
    let lvp = pm1.read_at(0x08).unwrap_or(0xFF);
    esp_println::println!(
        "nostos-fw: pm1 pwr_cfg=0x{:02x} hold_cfg=0x{:02x} batt_lvp=0x{:02x} (i2c_cfg={} wdt={} pwr={} hold={})",
        pwr_v,
        hold_v,
        lvp,
        a as u8,
        b as u8,
        c as u8,
        d as u8
    );
    a && b && c && d
}

/// 電源ボタンの破壊的アクションを PM1 側で無効化し、誤操作による電源断・リセット
/// （→コールドブートでのパネル固着）を防ぐ。
///
/// - **SINGLE_RST_DIS=1**: 単クリックのリセットを無効化。実機検証で単クリックは完全リセット
///   （USB 再列挙を伴う）を起こしパネルを固着させたため封じる。
/// - **DOUBLE_OFF_DIS=1**: ダブルクリックの電源オフを無効化＝誤操作での電源断→コールド固着を封じる。
/// - **LONG_DLY=11（4 秒）**: 長押し判定を 1→4 秒に延長し、うっかり長押しを防ぐ。
/// - **DL_LOCK=0（据え置き）**: 4 秒長押し→download mode は温存（実質不要だが害なし）。
///
/// 復旧はボタン非依存の espflash（USB-Serial-JTAG）で常に可能（本日実証済み）。意図的な
/// 電源オフは設定タブ（タッチ長押し→[`shutdown`]）に残る。バス整定後に呼ぶこと。
pub fn configure_power_button(i2c: &mut SysI2c) -> bool {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    // read-modify-write。読み出し失敗時は書かない（化けた値で他ビットを壊さない）。
    let mut ok = true;
    let mut cfg1_v = 0xFFu8;
    let mut cfg2_v = 0xFFu8;
    if let Ok(cfg1) = pm1.read_at(PM1_REG_BTN_CFG_1) {
        // DL_LOCK(7)=0・LONG_DLY(4:3)=11・SINGLE_RST_DIS(0)=1 に整え、他ビットは保持。
        cfg1_v = (cfg1 & !PM1_BTN_CFG1_MASK) | PM1_BTN_LONG_DLY_4S | PM1_BTN_SINGLE_RST_DIS;
        ok &= pm1.write_at(PM1_REG_BTN_CFG_1, cfg1_v).is_ok();
    } else {
        ok = false;
    }
    if let Ok(cfg2) = pm1.read_at(PM1_REG_BTN_CFG_2) {
        cfg2_v = cfg2 | PM1_BTN_DOUBLE_OFF_DIS;
        ok &= pm1.write_at(PM1_REG_BTN_CFG_2, cfg2_v).is_ok();
    } else {
        ok = false;
    }
    // ボタン割り込みをマスクし保留状態をクリアする。アクションは無効化済みで割り込みも
    // 不要。未処理割り込みが LED を点滅させ続ける副作用（実機で確認）を止める。
    let _ = pm1.write_at(PM1_REG_IRQ_MASK3, PM1_BTN_IRQ_ALL);
    let _ = pm1.write_at(PM1_REG_IRQ_STATUS3, 0x00);
    esp_println::println!(
        "nostos-fw: pm1 btn_cfg1=0x{:02x} btn_cfg2=0x{:02x} (double-off dis, long=4s) ok={}",
        cfg1_v,
        cfg2_v,
        ok as u8
    );
    ok
}

/// M5PM1 にシャットダウンを指示する（バッテリ駆動時は電源断。USB 給電中は再起動相当）。
pub fn shutdown(i2c: &mut SysI2c) {
    let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
    let _ = pm1.shutdown();
}

/// 電源レールと M5IOE1 を立ち上げる。受信ファームに必要な最小シーケンス：
///
/// 1. レール整定待ち → M5PM1 存在確認 → M5IOE1 発見。
/// 2. **IP2315 を I2C バスから隔離**（`PYG11_PWM3` LOW 固定。充電 IC がバスを
///    ロックアップさせる既知ハザードの回避。papermono-rs `touch_bus.rs` と同じ扱い）。
/// 3. PDM マイク電源は OFF、EPD_VDD レールを ON（SSD1677 と AW9967 が載る）。
///
/// 戻り値は発見した M5IOE1 アドレス（見つからなければ `None`＝表示・LoRa 電源制御不可）。
pub async fn bring_up(i2c: &mut SysI2c) -> Option<u8> {
    // ⚠️ PM1 への書き込みは必ずバス整定後に行う。整定前のトランザクションが化けると
    // 意図しないレジスタ（低電圧保護しきい値等）を破壊し得る（PM1 はバッテリ給電で
    // 常時生存のため、壊れた設定はリセットでも消えず表示系全滅などの残留障害になる）。
    Timer::after(Duration::from_millis(POWER_SETTLE_MS)).await;

    let held = hold_power(i2c);
    esp_println::println!("nostos-fw: pm1 power hold {}", if held { "ok" } else { "FAILED" });

    // 電源ボタンの誤操作（単クリック・リセット / ダブルクリック電源オフ）を無効化し、
    // 固着の引き金を封じる。復旧は espflash（USB）でボタン非依存に可能。
    let _ = configure_power_button(i2c);

    let pm1 = probe_read(i2c, addresses::M5PM1, pmic::DEVICE_ID);
    let ioe_addr = begin_ioe(i2c).await;

    if ioe_addr.is_some() {
        // 電源イネーブル／リセット系は IN 読み戻しで駒動を確認する（コールド起動直後の IOE1 は
        // 登録どおりに出力ドライバが有効化されないことがある＝コールドブート固着の真因。
        // 2026-09-16 実機で io3 を確認）。
        let _ = set_output_verified(i2c, ioe1::IP2315_I2C_GATE, false);
        let _ = set_output_verified(i2c, ioe1::PDM_VDD_ENABLE, false);
        let _ = set_output_verified(i2c, ioe1::EPD_VDD_ENABLE, true);
        // microSD 電源（IOE1 PYG14）を投入。CSV ロガー用（sdlog）。
        let _ = set_output_verified(i2c, ioe1::MICROSD_ENABLE, true);

        // FT6336G タッチを電源サイクルして起動（touch_bus と同シーケンス）。
        let _ = set_output_verified(i2c, ioe1::TOUCH_RST, false);
        let _ = set_output_verified(i2c, ioe1::TOUCH_VDD_ENABLE, false);
        Timer::after(Duration::from_millis(30)).await;
        let _ = set_output_verified(i2c, ioe1::TOUCH_VDD_ENABLE, true);
        Timer::after(Duration::from_millis(20)).await;
        let _ = set_output_verified(i2c, ioe1::TOUCH_RST, true);
        Timer::after(Duration::from_millis(100)).await;
    }

    esp_println::println!(
        "nostos-fw: bring_up pm1={} ioe_addr={:?}",
        pm1 as u8,
        ioe_addr
    );
    ioe_addr
}
