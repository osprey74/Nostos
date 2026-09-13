//! RGB LED（左側面）ステータス通知（`docs/UI.md` の色分け表）。
//!
//! ハード構成: 緑=M5IOE1 `PYG8` / 青=M5IOE1 `PYG9` / 赤=M5PM1 `LED_EN`（3 線独立・
//! WS2812 ではない）。橙は赤＋緑の同時点灯で合成する。
//!
//! 優先度: 赤（低電池）＞ 橙（受信途絶）＞ 緑（新規受信）＞ 青（充電/USB 給電）＞ 消灯。
//! I2C 書き込みは状態が変わったときだけ行う。

use embassy_time::Instant;

use crate::ioe::{self, SysI2c};

/// 低電池とみなすバッテリ電圧 [mV]（LiPo ≈15% 相当の保守値）。
pub const LOW_BATT_MV: u16 = 3500;

/// 新規受信の緑フラッシュ保持時間 [ms]。
const RX_FLASH_MS: u64 = 600;

/// LED 制御への入力（メインループが毎 tick 渡す状態スナップショット）。
pub struct LedInputs {
    /// 最終受信からの経過秒（未受信は None）。
    pub age_secs: Option<u64>,
    /// 最終受信時刻（緑フラッシュ用）。
    pub last_rx_at: Option<Instant>,
    /// バッテリ電圧 [mV]。
    pub vbat_mv: Option<u16>,
    /// VIN 電圧 [mV]（USB 給電検出）。
    pub vin_mv: Option<u16>,
    /// 受信途絶とみなす秒数。
    pub stale_secs: u64,
}

/// RGB LED コントローラ（現在色をキャッシュし差分のみ I2C へ書く）。
pub struct Led {
    cur: (bool, bool, bool), // (r, g, b)
}

impl Led {
    /// 消灯状態で初期化（実際の消灯書き込みは初回 `update` で行われる）。
    pub const fn new() -> Self {
        Self {
            cur: (true, true, true), // 初回 update で必ず (false,false,false) が書かれるよう不一致にしておく
        }
    }

    fn apply(&mut self, i2c: &mut SysI2c, r: bool, g: bool, b: bool) {
        if self.cur == (r, g, b) {
            return;
        }
        self.cur = (r, g, b);
        ioe::set_led_red(i2c, r);
        ioe::set_led_green(i2c, g);
        ioe::set_led_blue(i2c, b);
    }

    /// 現在の状態から LED 色を決めて反映する（メインループから 50ms 周期で呼ぶ）。
    pub fn update(&mut self, i2c: &mut SysI2c, st: &LedInputs) {
        let now_ms = Instant::now().as_millis();

        // 赤点滅: 低電池（1Hz）。
        if st.vbat_mv.is_some_and(|v| v > 0 && v < LOW_BATT_MV) {
            let on = (now_ms / 500) % 2 == 0;
            self.apply(i2c, on, false, false);
            return;
        }
        // 橙遅い点滅: 受信途絶（0.5Hz・赤＋緑）。
        if st.age_secs.is_some_and(|a| a >= st.stale_secs) {
            let on = (now_ms / 1000) % 2 == 0;
            self.apply(i2c, on, on, false);
            return;
        }
        // 緑 1 回点滅: 新規フレーム受信直後。
        if st
            .last_rx_at
            .is_some_and(|t| t.elapsed().as_millis() < RX_FLASH_MS)
        {
            self.apply(i2c, false, true, false);
            return;
        }
        // 青点灯: 充電 / USB 給電。
        if st
            .vin_mv
            .is_some_and(|v| v >= m5stack_papermono_lite::pmic::VIN_PRESENT_MV)
        {
            self.apply(i2c, false, false, true);
            return;
        }
        // 通常待機: 消灯（省電力）。
        self.apply(i2c, false, false, false);
    }
}
