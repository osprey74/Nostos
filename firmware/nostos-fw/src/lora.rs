//! SX1262（Stamp LoRa-1262）Nostos-native 受信専用ドライバ。
//!
//! papermono-rs `firmware/embassy-debug` の `lora.rs`（MIT）から受信系のみ移植し、
//! 単一タスク前提に簡素化（グローバル mutex / UI 状態を排除）。**送信機能は持たない**
//! （受信は電波法の規制対象外。`docs/COMPLIANCE.md`）。
//!
//! RF パラメータは `nostos-frame::radio` の定数（C6L ビーコンと同一）：
//! 923.000 MHz / BW125 / SF9 / CR4-5 / sync word 0x3A（SX1262 エンコード 0x34A4）。
//!
//! 運用は**連続受信**：起動時に電源投入→設定→`SetRx(0xFFFFFF)` で camp し、
//! パケットごとに読み出して再アーム。分報（60 秒間隔）を取りこぼさない。

use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, Output};
use esp_hal::spi::master::Spi;
use m5stack_papermono::lora::{
    self, PacketStatus, Sx1262, CAL_IMG_902_MHZ, CAL_IMG_928_MHZ, IRQ_ALL, IRQ_CRC_ERR,
    IRQ_RX_DONE, LORA_BW_125_KHZ, LORA_CRC_ON, LORA_CR_4_5, LORA_HEADER_VARIABLE,
    LORA_IQ_STANDARD, LORA_LDRO_OFF, PACKET_TYPE_LORA, REGULATOR_LDO, STDBY_CONFIG_RC,
    TCXO_CTRL_3_0V, TCXO_DEFAULT_DELAY_TICKS,
};
use m5stack_papermono_lite::addresses;
use nostos_frame::radio as rf;

use crate::ioe::{self, SysI2c};

// nostos-frame::radio の数値定数と SX1262 レジスタ値の対応をコンパイル時に固定する。
const _: () = assert!(rf::SF == lora::LORA_SF9, "SF は 9 で固定（C6L と一致）");
const _: () = assert!(rf::BW_HZ == 125_000, "BW は 125 kHz 固定（LORA_BW_125_KHZ）");
const _: () = assert!(rf::CR == 5, "CR は 4/5 固定（LORA_CR_4_5）");

/// 受信ペイロードの最大読み出し長。NostosFrame は 16 バイトだが余裕を持つ。
pub const RX_BUF_LEN: usize = 32;

/// 受信 1 パケット。
pub struct RxPacket {
    /// ペイロード先頭 [`RX_BUF_LEN`] バイト。
    pub buf: [u8; RX_BUF_LEN],
    /// 実受信長（バイト）。
    pub len: usize,
    /// パケット RSSI [dBm]。
    pub rssi: i16,
    /// パケット SNR [dB]。
    pub snr: i8,
}

/// SPI3 に接続された SX1262 のハンドル（単一タスク所有）。
pub struct Radio {
    spi: Spi<'static, esp_hal::Blocking>,
    nss: Output<'static>,
    busy: Input<'static>,
}

impl Radio {
    /// SPI/NSS/BUSY を受け取ってハンドルを作る（電源はまだ入れない）。
    pub fn new(
        spi: Spi<'static, esp_hal::Blocking>,
        nss: Output<'static>,
        busy: Input<'static>,
    ) -> Self {
        Self { spi, nss, busy }
    }

    /// `3V3_L2_LoRa` レール投入とリセット解除（papermono-rs `power_up` と同シーケンス）。
    ///
    /// 1. M5IOE1 `PYG10`（リセット）を LOW 保持、`PYG2`（アンテナスイッチ）を HIGH。
    /// 2. M5PM1 `G2` でレール ON → 15 ms 整定。
    /// 3. リセット解除 → 20 ms（TCXO と内部ロジックの起動待ち）。
    pub async fn power_up(&mut self, i2c: &mut SysI2c) {
        let _ = ioe::set_push_pull_output(i2c, lora::IOE1_RESET, false);
        let _ = ioe::set_push_pull_output(i2c, lora::IOE1_ANTENNA_SWITCH, true);
        {
            let mut pm1 = m5stack_papermono_lite::m5pm1::M5pm1::new(&mut *i2c, addresses::M5PM1);
            let _ = pm1.set_gpio_output(lora::PMIC_ENABLE, true);
        }
        Timer::after(Duration::from_millis(15)).await;
        let _ = ioe::set_push_pull_output(i2c, lora::IOE1_RESET, true);
        Timer::after(Duration::from_millis(20)).await;
    }

    /// SetStandby → GET_STATUS で存在確認する。`true` = SX1262 応答あり。
    ///
    /// コールドブート直後の GET_STATUS はコマンドステータスに「実行失敗」が残る
    /// （実測 raw=0xAA＝StbyRc＋failure）ため、先に SetStandby を発行してから
    /// **チップモードが Standby であること**で判定する。
    pub fn probe(&mut self) -> bool {
        for _ in 0..3u8 {
            let res = {
                let mut sx = Sx1262::new(&mut self.spi, &mut self.nss, &mut self.busy);
                let _ = sx.set_standby(STDBY_CONFIG_RC);
                sx.get_status()
            };
            match res {
                Ok(s) => {
                    esp_println::println!("nostos-fw: sx get_status raw=0x{:02x}", s.raw);
                    if s.is_standby() {
                        return true;
                    }
                }
                Err(e) => {
                    esp_println::println!("nostos-fw: sx get_status err {:?}", e);
                }
            }
            embassy_time::block_for(Duration::from_millis(20));
        }
        false
    }

    /// Nostos チャネルに設定し連続受信を開始する。
    ///
    /// SX1262 コマンド列は E2E 実証済みの papermono-rs `listen_rx` と同一
    /// （standby → LDO → TCXO → image cal → DIO2 RF switch → LoRa/周波数/変調/
    /// パケット/sync → IRQ アーム → `SetRx(0xFFFFFF)`＝Rx Continuous）。
    pub fn start_rx(&mut self) {
        let mut sx = Sx1262::new(&mut self.spi, &mut self.nss, &mut self.busy);

        let _ = sx.set_standby(STDBY_CONFIG_RC);
        let _ = sx.set_regulator_mode(REGULATOR_LDO);
        let _ = sx.set_dio3_as_tcxo_ctrl(TCXO_CTRL_3_0V, TCXO_DEFAULT_DELAY_TICKS);
        let _ = sx.calibrate_image(CAL_IMG_902_MHZ, CAL_IMG_928_MHZ);
        let _ = sx.set_dio2_as_rf_switch_ctrl(true);

        let _ = sx.set_packet_type(PACKET_TYPE_LORA);
        let _ = sx.set_rf_frequency(rf::TX_FREQ_HZ);
        let _ = sx.set_lora_modulation_params(rf::SF, LORA_BW_125_KHZ, LORA_CR_4_5, LORA_LDRO_OFF);
        let _ = sx.set_lora_packet_params(8, LORA_HEADER_VARIABLE, 255, LORA_CRC_ON, LORA_IQ_STANDARD);
        let _ = sx.set_lora_sync_word(lora::encode_sync_word(rf::SYNC_WORD));

        self.arm_rx(true);
    }

    /// IRQ をクリアして連続受信を（再）アームする。
    ///
    /// RX 動作中の SetBufferBaseAddress は反映されず、次パケットの書き込み位置が
    /// ずれてペイロード破損（自前 CRC8 不一致）になる。必ず Standby へ落としてから
    /// ベースアドレスを 0 に戻し、SetRx し直す。
    fn arm_rx(&mut self, set_irq_params: bool) {
        let mut sx = Sx1262::new(&mut self.spi, &mut self.nss, &mut self.busy);
        let _ = sx.set_standby(STDBY_CONFIG_RC);
        let _ = sx.clear_irq_status(IRQ_ALL);
        if set_irq_params {
            let _ = sx.set_dio_irq_params(IRQ_RX_DONE | IRQ_CRC_ERR, IRQ_RX_DONE, 0, 0);
        }
        let _ = sx.set_buffer_base_address(0x00, 0x00);
        let _ = sx.set_rx(0xFF_FF_FF);
    }

    /// 診断: IRQ ステータス・瞬時 RSSI・生ステータスを読む（シリアルダンプ用）。
    pub fn debug_status(&mut self) -> (u16, i16, u8) {
        let mut sx = Sx1262::new(&mut self.spi, &mut self.nss, &mut self.busy);
        let irq = sx.get_irq_status().unwrap_or(0xFFFF);
        let rssi = sx.get_rssi_inst().unwrap_or(-127);
        let raw = sx.get_status().map(|s| s.raw).unwrap_or(0xFF);
        (irq, rssi, raw)
    }

    /// 受信済みパケットの有無をポーリングし、あれば読み出して再アームする。
    ///
    /// CRC エラーは黙って破棄・再アーム。メインループから 50 ms 周期程度で呼ぶ。
    pub fn poll(&mut self) -> Option<RxPacket> {
        let irq = {
            let mut sx = Sx1262::new(&mut self.spi, &mut self.nss, &mut self.busy);
            sx.get_irq_status().ok()?
        };

        if irq & IRQ_CRC_ERR != 0 {
            self.arm_rx(false);
            return None;
        }
        if irq & IRQ_RX_DONE == 0 {
            return None;
        }

        let mut buf = [0u8; RX_BUF_LEN];
        let (len, rssi, snr) = {
            let mut sx = Sx1262::new(&mut self.spi, &mut self.nss, &mut self.busy);
            let (len, start_ptr) = sx.get_rx_buffer_status().unwrap_or((0, 0));
            let pkt = sx.get_packet_status().unwrap_or(PacketStatus {
                rssi_pkt_dbm: -100,
                snr_pkt_db: 0,
                signal_rssi_pkt_dbm: -100,
            });
            let n = (len as usize).min(RX_BUF_LEN);
            let _ = sx.read_buffer(start_ptr, &mut buf[..n]);
            (n, pkt.rssi_pkt_dbm, pkt.snr_pkt_db)
        };

        self.arm_rx(false);

        Some(RxPacket {
            buf,
            len,
            rssi,
            snr,
        })
    }
}
