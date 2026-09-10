//! Nostos-native の生 LoRa 分報フレーム。
//!
//! Meshtastic を用いず、SX1262 で自前の固定長フレーム（**16 バイト**）を送受信する。
//! C6L ビーコン（送信）と PaperMono（受信）で**同一のバイト列**を共有する言語非依存フォーマット。
//! 復号（AES）・protobuf は不要で、受信側は本クレートだけで解釈できる。
//!
//! # 電波法・技適コンプライアンス
//! 送信に関わる RF パラメータ（周波数・帯域・出力）は [`radio`] モジュールに**定数として固定**し、
//! **コンパイル時アサーション**で認証枠（922〜923.4 MHz / ≤200 kHz / ≤5.0 mW）からの逸脱を防ぐ。
//! 詳細・厳守事項は `docs/COMPLIANCE.md` を参照。定数を緩める変更は同ドキュメントの更新を伴うこと。
//!
//! `no_std`。

#![no_std]

use nostos_nav::GeoPoint;

/// フレーム長（バイト）。
pub const FRAME_LEN: usize = 16;

/// プロトコル版。
pub const PROTOCOL_VERSION: u8 = 1;

/// フラグ: 有効な GPS 測位（fix）を含む。
pub const FLAG_FIX_VALID: u8 = 0x01;

/// 送信に関わる RF パラメータ（**技適順守のためハードコード固定**）。
///
/// 認証枠: F1D / 922〜923.4 MHz / 200 kHz / 1.5〜5.0 mW（Unit-C6L 211-250603）。
/// ここを緩める変更は `docs/COMPLIANCE.md` の更新と再確認を必須とする。
pub mod radio {
    /// 送信中心周波数 [Hz]。認証帯域 922〜923.4 MHz 内に固定（BW125 で占有 922.94〜923.06 MHz）。
    pub const TX_FREQ_HZ: u32 = 923_000_000;
    /// 占有帯域（LoRa BW）[Hz]。認証の 200 kHz 以下（125 kHz を採用）。
    pub const BW_HZ: u32 = 125_000;
    /// 送信出力 [dBm]。認証上限 5.0 mW（≈+7 dBm）に対し余裕を見て +6 dBm（≈4 mW）。
    pub const TX_POWER_DBM: i8 = 6;
    /// 拡散率（Nostos 既定）。
    pub const SF: u8 = 9;
    /// 符号化率（4/5）。
    pub const CR: u8 = 5;
    /// LoRa sync word（Nostos 独自ネット・両端一致必須）。※規制対象パラメータではない。
    pub const SYNC_WORD: u8 = 0x3A;
    /// 分報の送信間隔 [秒]。ARIB STD-T108 のデューティに十分収まる。
    pub const TX_INTERVAL_SECS: u32 = 60;

    // --- コンパイル時ガードレール（逸脱するとビルド失敗） ---
    /// 占有帯域の下端が認証下限 922.0 MHz を下回らないこと。
    const _: () = assert!(
        TX_FREQ_HZ - BW_HZ / 2 >= 922_000_000,
        "占有帯域の下端が認証下限 922.0 MHz を下回る（技適違反）"
    );
    /// 占有帯域の上端が認証上限 923.4 MHz を超えないこと。
    const _: () = assert!(
        TX_FREQ_HZ + BW_HZ / 2 <= 923_400_000,
        "占有帯域の上端が認証上限 923.4 MHz を超える（技適違反）"
    );
    /// 帯域幅が認証の 200 kHz 以下であること。
    const _: () = assert!(BW_HZ <= 200_000, "LoRa 帯域幅が 200 kHz を超える（技適違反）");
    /// 出力が認証上限（≈+7 dBm ≒ 5.0 mW）以下であること。
    const _: () = assert!(TX_POWER_DBM <= 7, "送信出力が 5.0 mW/+7dBm を超える（技適違反）");
}

/// デコード時のエラー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// バイト長が [`FRAME_LEN`] 未満。
    TooShort,
    /// CRC 不一致。
    BadCrc,
    /// 未知のプロトコル版。
    BadVersion(u8),
}

/// Nostos 分報フレーム（位置＋時刻）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NostosFrame {
    /// プロトコル版。
    pub version: u8,
    /// フラグ（[`FLAG_FIX_VALID`] 等）。
    pub flags: u8,
    /// 連番（欠落検出用・8bit で巡回）。
    pub seq: u8,
    /// 緯度（度 ×1e7）。
    pub lat_e7: i32,
    /// 経度（度 ×1e7）。
    pub lon_e7: i32,
    /// 測位時刻（unix 秒・UTC）。0 = 不明。
    pub time_unix: u32,
}

impl NostosFrame {
    /// 現行版・fix 有効のフレームを生成。
    #[must_use]
    pub fn new(seq: u8, lat_e7: i32, lon_e7: i32, time_unix: u32, fix_valid: bool) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            flags: if fix_valid { FLAG_FIX_VALID } else { 0 },
            seq,
            lat_e7,
            lon_e7,
            time_unix,
        }
    }

    /// 有効な測位を含むか。
    #[must_use]
    pub fn has_fix(&self) -> bool {
        self.flags & FLAG_FIX_VALID != 0
    }

    /// 緯度経度を [`GeoPoint`] に変換。
    #[must_use]
    pub fn geopoint(&self) -> GeoPoint {
        GeoPoint::from_e7(self.lat_e7, self.lon_e7)
    }

    /// 16 バイトへエンコード（末尾に CRC-8）。
    ///
    /// レイアウト（LE）: `version | flags | seq | lat_e7(4) | lon_e7(4) | time_unix(4) | crc8`
    #[must_use]
    pub fn encode(&self) -> [u8; FRAME_LEN] {
        let mut b = [0u8; FRAME_LEN];
        b[0] = self.version;
        b[1] = self.flags;
        b[2] = self.seq;
        b[3..7].copy_from_slice(&self.lat_e7.to_le_bytes());
        b[7..11].copy_from_slice(&self.lon_e7.to_le_bytes());
        b[11..15].copy_from_slice(&self.time_unix.to_le_bytes());
        b[15] = crc8(&b[0..15]);
        b
    }

    /// バイト列をデコード（CRC-8 と版を検証）。
    ///
    /// # Errors
    /// 長さ不足・CRC 不一致・未知版のとき。
    pub fn decode(buf: &[u8]) -> Result<Self, FrameError> {
        if buf.len() < FRAME_LEN {
            return Err(FrameError::TooShort);
        }
        if crc8(&buf[0..15]) != buf[15] {
            return Err(FrameError::BadCrc);
        }
        let version = buf[0];
        if version != PROTOCOL_VERSION {
            return Err(FrameError::BadVersion(version));
        }
        let rd_i32 = |o: usize| i32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        let rd_u32 = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        Ok(Self {
            version,
            flags: buf[1],
            seq: buf[2],
            lat_e7: rd_i32(3),
            lon_e7: rd_i32(7),
            time_unix: rd_u32(11),
        })
    }
}

/// CRC-8（多項式 `0x07`、初期値 `0x00`）。フレーム末尾の整合性チェック用。
#[must_use]
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        let mut i = 0;
        while i < 8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
            i += 1;
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let f = NostosFrame::new(42, 430_621_000, 1_413_544_000, 1_700_000_000, true);
        let bytes = f.encode();
        assert_eq!(bytes.len(), FRAME_LEN);
        let g = NostosFrame::decode(&bytes).unwrap();
        assert_eq!(f, g);
        assert!(g.has_fix());
        assert_eq!(g.seq, 42);
    }

    #[test]
    fn geopoint_conversion() {
        let f = NostosFrame::new(0, 430_621_000, 1_413_544_000, 0, true);
        let p = f.geopoint();
        assert!((p.lat - 43.0621).abs() < 1e-6);
        assert!((p.lon - 141.3544).abs() < 1e-6);
    }

    #[test]
    fn negative_coords() {
        // 南緯・西経（例: 南米付近）
        let f = NostosFrame::new(1, -335_000_000, -705_000_000, 123, false);
        let g = NostosFrame::decode(&f.encode()).unwrap();
        assert_eq!(g.lat_e7, -335_000_000);
        assert_eq!(g.lon_e7, -705_000_000);
        assert!(!g.has_fix());
    }

    #[test]
    fn detects_corruption() {
        let mut bytes = NostosFrame::new(7, 1, 2, 3, true).encode();
        bytes[5] ^= 0xFF; // 1 ビット破壊
        assert_eq!(NostosFrame::decode(&bytes), Err(FrameError::BadCrc));
    }

    #[test]
    fn too_short() {
        assert_eq!(NostosFrame::decode(&[0u8; 8]), Err(FrameError::TooShort));
    }

    #[test]
    fn bad_version() {
        let mut bytes = NostosFrame::new(0, 0, 0, 0, false).encode();
        bytes[0] = 0xFE; // 未知版
        bytes[15] = crc8(&bytes[0..15]); // CRC は合わせる
        assert_eq!(NostosFrame::decode(&bytes), Err(FrameError::BadVersion(0xFE)));
    }

    #[test]
    fn crc8_known_vector() {
        // CRC-8/SMBUS ("123456789") = 0xF4
        assert_eq!(crc8(b"123456789"), 0xF4);
    }

    // 技適枠のガードレール（周波数・帯域・出力）は radio モジュールの
    // コンパイル時 `const _: () = assert!(...)` が保証する（逸脱でビルド失敗）。
}
