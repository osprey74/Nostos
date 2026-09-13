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

/// 電源レールと M5IOE1 を立ち上げる。受信ファームに必要な最小シーケンス：
///
/// 1. レール整定待ち → M5PM1 存在確認 → M5IOE1 発見。
/// 2. **IP2315 を I2C バスから隔離**（`PYG11_PWM3` LOW 固定。充電 IC がバスを
///    ロックアップさせる既知ハザードの回避。papermono-rs `touch_bus.rs` と同じ扱い）。
/// 3. PDM マイク電源は OFF、EPD_VDD レールを ON（SSD1677 と AW9967 が載る）。
///
/// 戻り値は発見した M5IOE1 アドレス（見つからなければ `None`＝表示・LoRa 電源制御不可）。
pub async fn bring_up(i2c: &mut SysI2c) -> Option<u8> {
    Timer::after(Duration::from_millis(POWER_SETTLE_MS)).await;

    let pm1 = probe_read(i2c, addresses::M5PM1, pmic::DEVICE_ID);
    let ioe_addr = begin_ioe(i2c).await;

    if ioe_addr.is_some() {
        let _ = set_push_pull_output(i2c, ioe1::IP2315_I2C_GATE, false);
        let _ = set_push_pull_output(i2c, ioe1::PDM_VDD_ENABLE, false);
        let _ = set_push_pull_output(i2c, ioe1::EPD_VDD_ENABLE, true);

        // FT6336G タッチを電源サイクルして起動（touch_bus と同シーケンス）。
        // 現状は座標を読まず TOUCH_INT(GPIO4) のタップ検出（画面切替）のみに使う。
        let _ = set_push_pull_output(i2c, ioe1::TOUCH_RST, false);
        let _ = set_push_pull_output(i2c, ioe1::TOUCH_VDD_ENABLE, false);
        Timer::after(Duration::from_millis(30)).await;
        let _ = set_push_pull_output(i2c, ioe1::TOUCH_VDD_ENABLE, true);
        Timer::after(Duration::from_millis(20)).await;
        let _ = set_push_pull_output(i2c, ioe1::TOUCH_RST, true);
        Timer::after(Duration::from_millis(100)).await;
    }

    esp_println::println!(
        "nostos-fw: bring_up pm1={} ioe_addr={:?}",
        pm1 as u8,
        ioe_addr
    );
    ioe_addr
}
