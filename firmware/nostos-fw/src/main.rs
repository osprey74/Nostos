//! Nostos PaperMono 受信ファーム — **Phase 2 スケルトン（未ビルド）**。
//!
//! この時点ではロジックの結線図を Rust の形で残すだけ。esp-hal / embassy 依存は
//! 実機着手（Phase 2）で有効化する。詳細な結線・差分は `docs/PHASE0.md`。
//!
//! 受信 → デコード → 軌跡更新 → 帰路算出の骨子:
//!
//! ```ignore
//! use nostos_meshtastic::{decode_position, DEFAULT_CHANNEL_KEY};
//! use nostos_nav::{GeoPoint, Trail};
//!
//! static mut TRAIL: Trail<256> = Trail::new();
//!
//! // 1 パケット受信ごとに:
//! fn on_rx(frame: &mut [u8], home: GeoPoint) {
//!     match decode_position(frame, &DEFAULT_CHANNEL_KEY) {
//!         Ok((_hdr, pos)) => {
//!             let p = GeoPoint::from_meshtastic_i(pos.latitude_i, pos.longitude_i);
//!             // TRAIL.push(p);
//!             // let homing = TRAIL.homing(home);   // 距離・方位
//!             // draw_grid_and_breadcrumbs(&TRAIL);  // e-ink (SSD1677)
//!             // draw_homing_arrow(homing);
//!         }
//!         Err(_e) => { /* CRC/portnum 不一致等はスキップ */ }
//!     }
//! }
//! ```
//!
//! Phase 2 で移植する実機依存部（papermono-rs `firmware/embassy-debug` 由来・MIT）:
//! - I2C 立ち上げ（`board.rs`）と M5IOE1 制御（`ioe.rs`）
//! - LoRa 電源シーケンス `power_up`/`power_down`（`lora.rs`）
//! - `listen_rx` を**フルペイロード読み出し**に改造 → 上記 `on_rx` へ
//! - 周波数を **JP 920MHz 帯**へ

fn main() {
    // Phase 2 で esp-hal エントリ（#![no_std] / #![no_main] / embassy executor）に置き換える。
    // ここではホストでの誤ビルドを避けるため何もしない。
    println!("nostos-fw skeleton — see docs/PHASE0.md (not yet buildable for target)");
}
