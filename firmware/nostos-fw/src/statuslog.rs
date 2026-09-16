//! ステータスログ（`STATUS.CSV`）の 1 行組み立て。
//!
//! 受信フレームの有無に関わらず、電源・無線・受信経過の「機体の健康状態」を定期的
//! （[`crate::STATUS_LOG_SECS`]）と事象時（起動・低電池・受信途絶）に microSD へ追記する。
//! バッテリ枯渇までの放電カーブや、コールドブート試験時の初期化結果を事後に追えるようにする
//! のが目的。書き込み自体は [`crate::sdlog::SdLogger::append_status`]。
//!
//! 列: `uptime_s,est_unix,vbat_mv,vin_mv,batt_pct,pwr_src,pwr_cfg,rssi_floor,sx_status,
//! last_rx_age_s,rx_count,frontlight,reset_reason,event`
//! - `est_unix`: 壁時計は無いので「最終受信フレームの GPS 時刻＋経過秒」の推定値。未受信は 0
//! - `pwr_src`/`pwr_cfg`: PM1 レジスタ 0x04/0x06 の生値（16 進）。読めなければ `--`
//! - `rssi_floor`/`sx_status`: SX1262 の瞬時 RSSI[dBm]・ステータス生値（16 進）
//! - `reset_reason`: ESP32-S3 ROM のリセット理由コード（16 進。0x01=電源投入 / 0x03=ソフト
//!   リセット / 0x0C=CPU ソフト / 0x15=USB-UART / 0x16=USB-JTAG / 0x0F=ブラウンアウト）
//! - `event`: `boot` / `boot_panel_fail` / `periodic` / `low_batt` / `rx_lost` /
//!   `sd_eject`（設定画面「SD CARD」行でロガー停止。この行が SD 上の最終行になる）

/// 1 サンプル分の値。`None` は「読めなかった」を表し `--` で出力する。
pub struct Sample<'a> {
    pub uptime_s: u64,
    pub est_unix: u64,
    pub vbat_mv: Option<u16>,
    pub vin_mv: Option<u16>,
    pub pwr_src: Option<u8>,
    pub pwr_cfg: Option<u8>,
    /// `(rssi_inst[dBm], status raw)`。無線未初期化なら `None`。
    pub radio: Option<(i16, u8)>,
    pub last_rx_age_s: Option<u64>,
    pub rx_count: u32,
    /// 実効フロントライト段階（自動消灯中は 0）。
    pub frontlight: u8,
    pub reset_reason: u8,
    pub event: &'a str,
}

impl Sample<'_> {
    /// CSV 1 行（末尾 `\n` 付き）を `out` へ書く。バッファ不足なら `false`。
    pub fn write_csv<W: core::fmt::Write>(&self, out: &mut W) -> bool {
        (|| -> core::fmt::Result {
            write!(out, "{},{},", self.uptime_s, self.est_unix)?;
            write_opt(out, self.vbat_mv)?;
            out.write_char(',')?;
            write_opt(out, self.vin_mv)?;
            out.write_char(',')?;
            match self.vbat_mv {
                Some(mv) if mv > 0 => write!(out, "{}", batt_pct(mv))?,
                _ => out.write_str("--")?,
            }
            out.write_char(',')?;
            write_hex(out, self.pwr_src)?;
            out.write_char(',')?;
            write_hex(out, self.pwr_cfg)?;
            out.write_char(',')?;
            match self.radio {
                Some((rssi, raw)) => write!(out, "{},0x{:02x}", rssi, raw)?,
                None => out.write_str("--,--")?,
            }
            out.write_char(',')?;
            write_opt(out, self.last_rx_age_s)?;
            writeln!(
                out,
                ",{},{},0x{:02x},{}",
                self.rx_count, self.frontlight, self.reset_reason, self.event
            )
        })()
        .is_ok()
    }
}

fn write_opt<W: core::fmt::Write, T: core::fmt::Display>(
    out: &mut W,
    v: Option<T>,
) -> core::fmt::Result {
    match v {
        Some(v) => write!(out, "{}", v),
        None => out.write_str("--"),
    }
}

fn write_hex<W: core::fmt::Write>(out: &mut W, v: Option<u8>) -> core::fmt::Result {
    match v {
        Some(v) => write!(out, "0x{:02x}", v),
        None => out.write_str("--"),
    }
}

/// 電池残量 [%]（ヘッダ表示 `draw::batt_pct` と同じ 3.3〜4.2V 線形近似）。
fn batt_pct(mv: u16) -> u32 {
    (u32::from(mv).saturating_sub(3300) * 100 / 900).min(100)
}

/// 今回起動のリセット理由コード（ESP32-S3 ROM `rtc_get_reset_reason`、PRO CPU）。
/// 取得できなければ 0x00。
pub fn reset_reason_code() -> u8 {
    esp_hal::rtc_cntl::reset_reason(esp_hal::system::Cpu::ProCpu).map_or(0, |r| r as u8)
}
