//! Meshtastic オンエアフレームの受信側デコード。
//!
//! パイプライン: **生バイト列**（SX1262 の RX バッファ）→ [`MeshHeader`] 分離 →
//! [`decrypt_in_place`]（AES-CTR）→ [`Data`] protobuf → `portnum == POSITION_APP` なら [`Position`]。
//!
//! ワンショットの便利関数 [`decode_position`] が上記を一括で行う。
//!
//! `no_std`。復号は RustCrypto（`aes` / `ctr`）。protobuf は本モジュール内の最小リーダで解析（依存なし）。
//!
//! ⚠️ 既定鍵・nonce 構成・Position フィールド型は meshtastic/firmware と突合のこと（詳細 `docs/PHASE0.md`）。

#![no_std]

use aes::cipher::{KeyIvInit, StreamCipher};
use aes::Aes128;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;

/// Meshtastic オンエアヘッダの固定長（バイト）。
pub const HEADER_LEN: usize = 16;

/// Position アプリの portnum。
pub const PORTNUM_POSITION_APP: u32 = 3;

/// 既定（public / LongFast）チャネルの AES-128 鍵。
///
/// チャネル PSK が 1 バイト `0x01`（base64 `AQ==`）のとき使用する。
/// base64 `1PG7OiApB1nwvP+rz05pAQ==` のデコード結果（2026-09-10 検証）。
pub const DEFAULT_CHANNEL_KEY: [u8; 16] = [
    0xd4, 0xf1, 0xbb, 0x3a, 0x20, 0x29, 0x07, 0x59, 0xf0, 0xbc, 0xff, 0xab, 0xcf, 0x4e, 0x69, 0x01,
];

/// デコード時のエラー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// フレームがヘッダ長に満たない。
    TooShort,
    /// protobuf の途中でバイト列が尽きた。
    Truncated,
    /// 未対応のワイヤ種別。
    BadWireType(u8),
    /// 期待した portnum ではない（`portnum` を同梱）。
    NotPosition(u32),
    /// Position に必須フィールド（緯度/経度）が無い。
    MissingCoords,
}

/// Meshtastic オンエアヘッダ（先頭 16 バイト）。
///
/// 構成: `dest u32 LE | from u32 LE | packet_id u32 LE | flags u8 | channel_hash u8 | next_hop u8 | relay u8`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshHeader {
    /// 宛先ノード番号（`0xFFFFFFFF` = ブロードキャスト）。
    pub dest: u32,
    /// 送信元ノード番号。nonce 構成に使う。
    pub from: u32,
    /// パケット ID。nonce 構成に使う。
    pub packet_id: u32,
    /// フラグ（hop_limit / want_ack / via_mqtt / hop_start）。生値のまま保持。
    pub flags: u8,
    /// チャネルハッシュ（チャネル名＋PSK から算出。既定チャネルの識別に使える）。
    pub channel_hash: u8,
    /// 次ホップ（ルーティング）。
    pub next_hop: u8,
    /// リレーノード。
    pub relay_node: u8,
}

impl MeshHeader {
    /// フレーム先頭からヘッダを解釈する。
    ///
    /// # Errors
    /// フレームが [`HEADER_LEN`] 未満なら [`DecodeError::TooShort`]。
    pub fn parse(frame: &[u8]) -> Result<Self, DecodeError> {
        if frame.len() < HEADER_LEN {
            return Err(DecodeError::TooShort);
        }
        let rd_u32 = |o: usize| {
            u32::from_le_bytes([frame[o], frame[o + 1], frame[o + 2], frame[o + 3]])
        };
        Ok(Self {
            dest: rd_u32(0),
            from: rd_u32(4),
            packet_id: rd_u32(8),
            flags: frame[12],
            channel_hash: frame[13],
            next_hop: frame[14],
            relay_node: frame[15],
        })
    }

    /// AES-CTR 初期カウンタブロック（nonce）を構成する。
    ///
    /// `nonce[0..8] = packet_id を u64 LE`, `nonce[8..12] = from を u32 LE`, `nonce[12..16] = 0`。
    #[must_use]
    pub fn nonce(&self) -> [u8; 16] {
        let mut n = [0u8; 16];
        n[0..8].copy_from_slice(&u64::from(self.packet_id).to_le_bytes());
        n[8..12].copy_from_slice(&self.from.to_le_bytes());
        n
    }
}

/// 暗号化ペイロードをその場で復号する（AES-128 CTR）。
///
/// `header` から nonce を導出し、`payload`（ヘッダ以降のバイト列）を鍵ストリームで XOR する。
/// 既定チャネルは [`DEFAULT_CHANNEL_KEY`] を渡す。
pub fn decrypt_in_place(header: &MeshHeader, key: &[u8; 16], payload: &mut [u8]) {
    let nonce = header.nonce();
    let mut cipher = Aes128Ctr::new(key.into(), (&nonce).into());
    cipher.apply_keystream(payload);
}

/// 復号済み `Data` サブメッセージ（meshtastic.Data）。
///
/// 参照する `payload` はフレームバッファ内のスライス（コピーしない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Data<'a> {
    /// アプリ種別。位置情報は [`PORTNUM_POSITION_APP`]。
    pub portnum: u32,
    /// アプリ固有ペイロード（Position 等の protobuf バイト列）。
    pub payload: &'a [u8],
}

impl<'a> Data<'a> {
    /// 復号済みバイト列から `Data`（field 1=portnum, field 2=payload）を解析。
    ///
    /// # Errors
    /// protobuf が途中で尽きた／未対応ワイヤ種別のとき。
    pub fn parse(buf: &'a [u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(buf);
        let mut portnum = 0u32;
        let mut payload: &[u8] = &[];
        while let Some((field, wire)) = r.tag()? {
            match (field, wire) {
                (1, WIRE_VARINT) => portnum = r.varint()? as u32,
                (2, WIRE_LEN) => payload = r.bytes()?,
                _ => r.skip(wire)?,
            }
        }
        Ok(Self { portnum, payload })
    }
}

/// meshtastic.Position の必要フィールド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 緯度（度 ×1e7）。
    pub latitude_i: i32,
    /// 経度（度 ×1e7）。
    pub longitude_i: i32,
    /// 高度（メートル、任意）。
    pub altitude: Option<i32>,
    /// 測位時刻（unix 秒、任意）。
    pub time: Option<u32>,
}

impl Position {
    /// 復号済み Position バイト列を解析。
    ///
    /// フィールド番号: 1=latitude_i, 2=longitude_i, 3=altitude, 4=time。
    /// `sfixed32`（32bit）／`int32`（varint）どちらのエンコードでも受け付ける。
    ///
    /// # Errors
    /// 緯度・経度いずれも現れなければ [`DecodeError::MissingCoords`]。
    pub fn parse(buf: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(buf);
        let mut lat: Option<i32> = None;
        let mut lon: Option<i32> = None;
        let mut alt: Option<i32> = None;
        let mut time: Option<u32> = None;
        while let Some((field, wire)) = r.tag()? {
            match field {
                1 => lat = Some(r.i32_field(wire)?),
                2 => lon = Some(r.i32_field(wire)?),
                3 => alt = Some(r.i32_field(wire)?),
                4 => time = Some(r.i32_field(wire)? as u32),
                _ => r.skip(wire)?,
            }
        }
        match (lat, lon) {
            (Some(latitude_i), Some(longitude_i)) => Ok(Self {
                latitude_i,
                longitude_i,
                altitude: alt,
                time,
            }),
            _ => Err(DecodeError::MissingCoords),
        }
    }
}

/// 生 RX フレーム（ヘッダ＋暗号化ペイロード）から Position を一括デコードする。
///
/// `frame` は復号のためその場で書き換わる（ペイロード部）。既定チャネルは
/// [`DEFAULT_CHANNEL_KEY`] を `key` に渡す。
///
/// # Errors
/// ヘッダ長不足・protobuf 破損・portnum 不一致（[`DecodeError::NotPosition`]）・座標欠落。
pub fn decode_position(frame: &mut [u8], key: &[u8; 16]) -> Result<(MeshHeader, Position), DecodeError> {
    let header = MeshHeader::parse(frame)?;
    let (_hdr, payload) = frame.split_at_mut(HEADER_LEN);
    decrypt_in_place(&header, key, payload);
    let data = Data::parse(payload)?;
    if data.portnum != PORTNUM_POSITION_APP {
        return Err(DecodeError::NotPosition(data.portnum));
    }
    let pos = Position::parse(data.payload)?;
    Ok((header, pos))
}

// --- 最小 protobuf リーダ（varint / 64bit / len-delim / 32bit のみ） ---

const WIRE_VARINT: u8 = 0;
const WIRE_I64: u8 = 1;
const WIRE_LEN: u8 = 2;
const WIRE_I32: u8 = 5;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        let b = *self.buf.get(self.pos).ok_or(DecodeError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            result |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift >= 64 {
                return Err(DecodeError::Truncated);
            }
        }
    }

    /// 次のタグ（フィールド番号, ワイヤ種別）。バッファ終端で `None`。
    fn tag(&mut self) -> Result<Option<(u32, u8)>, DecodeError> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let wire = (key & 0x7) as u8;
        Ok(Some((field, wire)))
    }

    fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.varint()? as usize;
        let end = self.pos.checked_add(len).ok_or(DecodeError::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn read_i32_le(&mut self) -> Result<i32, DecodeError> {
        let mut b = [0u8; 4];
        for slot in &mut b {
            *slot = self.byte()?;
        }
        Ok(i32::from_le_bytes(b))
    }

    /// フィールド値を i32 として読む。wire=32bit なら LE 固定長、wire=varint なら値を i32 に切詰め。
    fn i32_field(&mut self, wire: u8) -> Result<i32, DecodeError> {
        match wire {
            WIRE_I32 => self.read_i32_le(),
            WIRE_VARINT => Ok(self.varint()? as i32),
            other => Err(DecodeError::BadWireType(other)),
        }
    }

    /// 未対応・未使用フィールドを読み飛ばす。
    fn skip(&mut self, wire: u8) -> Result<(), DecodeError> {
        match wire {
            WIRE_VARINT => {
                self.varint()?;
            }
            WIRE_I64 => {
                for _ in 0..8 {
                    self.byte()?;
                }
            }
            WIRE_LEN => {
                self.bytes()?;
            }
            WIRE_I32 => {
                for _ in 0..4 {
                    self.byte()?;
                }
            }
            other => return Err(DecodeError::BadWireType(other)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_header(dest: u32, from: u32, packet_id: u32) -> [u8; 16] {
        let mut h = [0u8; 16];
        h[0..4].copy_from_slice(&dest.to_le_bytes());
        h[4..8].copy_from_slice(&from.to_le_bytes());
        h[8..12].copy_from_slice(&packet_id.to_le_bytes());
        h[12] = 0x03; // flags 例
        h[13] = 0x08; // channel_hash 例
        h
    }

    #[test]
    fn header_roundtrip() {
        let raw = build_header(0xFFFF_FFFF, 0x1234_5678, 0x0000_ABCD);
        let h = MeshHeader::parse(&raw).unwrap();
        assert_eq!(h.dest, 0xFFFF_FFFF);
        assert_eq!(h.from, 0x1234_5678);
        assert_eq!(h.packet_id, 0x0000_ABCD);
        assert_eq!(h.flags, 0x03);
        assert_eq!(h.channel_hash, 0x08);
    }

    #[test]
    fn nonce_layout() {
        let h = MeshHeader::parse(&build_header(0, 0x1122_3344, 0x00AA_BB01)).unwrap();
        let n = h.nonce();
        // packet_id を u64 LE
        assert_eq!(&n[0..8], &[0x01, 0xBB, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // from を u32 LE
        assert_eq!(&n[8..12], &[0x44, 0x33, 0x22, 0x11]);
        assert_eq!(&n[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn header_too_short() {
        assert_eq!(MeshHeader::parse(&[0u8; 8]), Err(DecodeError::TooShort));
    }

    // protobuf エンコード補助（テスト専用）
    fn put_varint(out: &mut alloc_vec::Vec, mut v: u64) {
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
    }
    fn tag(out: &mut alloc_vec::Vec, field: u32, wire: u8) {
        put_varint(out, ((field << 3) | wire as u32) as u64);
    }

    #[test]
    fn position_decode_sfixed32() {
        // latitude_i=430_621_000, longitude_i=1_413_544_000, altitude=120, time=1_700_000_000
        let mut p = alloc_vec::Vec::new();
        tag(&mut p, 1, WIRE_I32);
        p.extend(&430_621_000i32.to_le_bytes());
        tag(&mut p, 2, WIRE_I32);
        p.extend(&1_413_544_000i32.to_le_bytes());
        tag(&mut p, 3, WIRE_VARINT);
        put_varint(&mut p, 120);
        tag(&mut p, 4, WIRE_I32);
        p.extend(&1_700_000_000u32.to_le_bytes());

        let pos = Position::parse(p.as_slice()).unwrap();
        assert_eq!(pos.latitude_i, 430_621_000);
        assert_eq!(pos.longitude_i, 1_413_544_000);
        assert_eq!(pos.altitude, Some(120));
        assert_eq!(pos.time, Some(1_700_000_000));
    }

    #[test]
    fn position_missing_coords() {
        let mut p = alloc_vec::Vec::new();
        tag(&mut p, 3, WIRE_VARINT);
        put_varint(&mut p, 5);
        assert_eq!(Position::parse(p.as_slice()), Err(DecodeError::MissingCoords));
    }

    #[test]
    fn end_to_end_decrypt_and_decode() {
        // Position を組み立て
        let mut pos_buf = alloc_vec::Vec::new();
        tag(&mut pos_buf, 1, WIRE_I32);
        pos_buf.extend(&430_621_000i32.to_le_bytes());
        tag(&mut pos_buf, 2, WIRE_I32);
        pos_buf.extend(&1_413_544_000i32.to_le_bytes());

        // Data{ portnum=3, payload=pos_buf } を組み立て
        let mut data_buf = alloc_vec::Vec::new();
        tag(&mut data_buf, 1, WIRE_VARINT);
        put_varint(&mut data_buf, PORTNUM_POSITION_APP as u64);
        tag(&mut data_buf, 2, WIRE_LEN);
        put_varint(&mut data_buf, pos_buf.len() as u64);
        data_buf.extend(pos_buf.as_slice());

        // フレーム = ヘッダ + AES-CTR で暗号化した Data
        let header_raw = build_header(0xFFFF_FFFF, 0x1234_5678, 0x0000_ABCD);
        let header = MeshHeader::parse(&header_raw).unwrap();
        let mut frame = alloc_vec::Vec::new();
        frame.extend(&header_raw);
        let enc_start = frame.len();
        frame.extend(data_buf.as_slice());
        // 送信側と同じ鍵/nonce で暗号化（CTR は対称）
        decrypt_in_place(&header, &DEFAULT_CHANNEL_KEY, &mut frame.as_mut_slice()[enc_start..]);

        // 受信側デコード
        let (h, pos) = decode_position(frame.as_mut_slice(), &DEFAULT_CHANNEL_KEY).unwrap();
        assert_eq!(h.from, 0x1234_5678);
        assert_eq!(pos.latitude_i, 430_621_000);
        assert_eq!(pos.longitude_i, 1_413_544_000);
    }

    // no_std テストで Vec 相当が必要なため、テスト時のみ std を使う極小ラッパ。
    mod alloc_vec {
        extern crate std;
        pub use std::vec::Vec as StdVec;

        /// テスト用の可変バイト列。
        pub struct Vec(StdVec<u8>);
        impl Vec {
            pub fn new() -> Self {
                Self(StdVec::new())
            }
            pub fn push(&mut self, b: u8) {
                self.0.push(b);
            }
            pub fn extend(&mut self, s: &[u8]) {
                self.0.extend_from_slice(s);
            }
            pub fn len(&self) -> usize {
                self.0.len()
            }
            pub fn as_slice(&self) -> &[u8] {
                &self.0
            }
            pub fn as_mut_slice(&mut self) -> &mut [u8] {
                &mut self.0
            }
        }
    }
}
