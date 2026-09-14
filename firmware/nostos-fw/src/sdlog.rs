//! microSD CSV ロガー（SDHOST 1-bit + FAT32）。
//!
//! 受信した NostosFrame を CSV 1 行として microSD に追記する。実験後に SD を抜いて
//! PC で回収できる、オフラインのフィールドロガー。
//!
//! 構成（esp-hal `qa-test/sdmmc_sd_async` と同スタック）:
//! - `esp-hal::sdmmc` SDHOST（SPI2/SPI3 とは別ペリフェラル・GPIO マトリクスで配線）
//!   スロット 1・1bit（CLK=GPIO13 / CMD=GPIO12 / DAT0=GPIO11）
//! - `sdio` クレートが SD カード初期化（CMD0/CMD8/ACMD41…）を担当
//! - `embedded-fatfs` が FAT32 read/write（async）、`embedded-partitions` が MBR/superfloppy
//!
//! SD 電源（IOE1 PYG14）は `ioe::bring_up` で投入済み。カード無し/初期化失敗時は `None` を
//! 返し、受信動作はロガー無しで継続する（ログだけ諦める）。

use block_device_adapters::BufStream;
use embassy_time::Delay;
use embedded_fatfs::{FileSystem, FsOptions, ReadWriteSeek};
use embedded_io_async::{Seek, SeekFrom, Write};
use embedded_partitions::mbr::Scheme;
use esp_hal::sdmmc::{Config, DelayPhase, SdHostController, SlotConfig};
use esp_hal::Async;
use sdio::{sd::Card, BlockDevice};
use static_cell::StaticCell;

/// ログファイル名（FAT 8.3）。再起動をまたいで追記する。
const LOG_FILE: &str = "NOSTOS.CSV";
/// 新規作成時に書く CSV ヘッダ。
const CSV_HEADER: &[u8] = b"time_unix,seq,fix,home,lat_e7,lon_e7,rssi,snr\n";

/// カードクロック。ログ用途なので控えめの 20MHz（初期化は sdio が低速で行う）。
const CARD_HZ: u32 = 20_000_000;
/// S3 高速パスの入力サンプリング位相。40MHz 化や初期化失敗時は `_1`,`_2`,`_3` を試す。
const INPUT_DELAY_PHASE: DelayPhase = DelayPhase::_0;

/// SD カードブロックデバイス（sdio が初期化した Card を保持）。
type LogCard = BlockDevice<Card, esp_hal::sdmmc::Slot<'static, 1, Async>, Delay, 512>;

/// microSD ロガー。初期化済みのカードを保持し、追記のたびに FAT をマウント/アンマウントする。
pub struct SdLogger {
    card: LogCard,
}

impl SdLogger {
    /// SDHOST スロット 1（1bit）でカードを初期化する。カード無し/失敗は `None`。
    pub async fn init(
        sdhost: esp_hal::peripherals::SDHOST<'static>,
        clk: esp_hal::peripherals::GPIO13<'static>,
        cmd: esp_hal::peripherals::GPIO12<'static>,
        dat0: esp_hal::peripherals::GPIO11<'static>,
    ) -> Option<Self> {
        // slot は controller を借用するため、controller をカードより長生きさせる必要がある
        // （`slot(&self)`）。StaticCell で 'static 化する（init は起動時 1 回のみ）。
        static SDHOST_CTRL: StaticCell<SdHostController<'static>> = StaticCell::new();
        let controller =
            SDHOST_CTRL.init(SdHostController::new(sdhost, Config::default()).ok()?);
        let slot = controller
            .slot::<1>(SlotConfig::default().with_input_delay_phase(INPUT_DELAY_PHASE))
            .ok()?;
        let slot = slot.with_clk(clk).with_cmd(cmd).with_data0(dat0).into_async();
        let card = BlockDevice::new_sd_card(slot, CARD_HZ, Delay).await.ok()?;
        Some(Self { card })
    }

    /// CSV 1 行を `NOSTOS.CSV` に追記する。成功で `true`。
    pub async fn append(&mut self, line: &[u8]) -> bool {
        let stream = BufStream::<_, 512>::new(&mut self.card);
        match Scheme::open(stream).await {
            Ok(Scheme::Mbr(mut mbr)) => {
                let fat_idx = mbr.iter_used().find(|(_, p)| p.is_fat()).map(|(i, _)| i);
                match fat_idx {
                    Some(idx) => match mbr.open_partition(idx).await {
                        Ok(slice) => write_line(slice, line).await,
                        Err(_) => false,
                    },
                    None => false,
                }
            }
            Ok(Scheme::Superfloppy(io)) => write_line(io, line).await,
            _ => false,
        }
    }
}

/// FAT をマウントして 1 行追記→アンマウント。ファイルが無ければヘッダ付きで作成する。
async fn write_line<IO: ReadWriteSeek>(io: IO, line: &[u8]) -> bool {
    let Ok(fs) = FileSystem::new(io, FsOptions::new()).await else {
        return false;
    };
    // fs を借用する処理はブロックに閉じ込め、その後に unmount する（借用衝突回避）。
    let ok = {
        let root = fs.root_dir();
        let file = match root.open_file(LOG_FILE).await {
            Ok(f) => Some(f),
            Err(_) => match root.create_file(LOG_FILE).await {
                Ok(mut f) => {
                    if f.write_all(CSV_HEADER).await.is_err() {
                        None
                    } else {
                        Some(f)
                    }
                }
                Err(_) => None,
            },
        };
        match file {
            Some(mut f) => {
                f.seek(SeekFrom::End(0)).await.is_ok()
                    && f.write_all(line).await.is_ok()
                    && f.flush().await.is_ok()
            }
            None => false,
        }
    };
    let _ = fs.unmount().await;
    ok
}
