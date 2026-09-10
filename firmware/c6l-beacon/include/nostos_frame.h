// Nostos-native LoRa 分報フレーム（C ミラー）。
// Rust 版 crates/nostos-frame と **バイト単位で一致**させること（16 バイト・リトルエンディアン）。
// レイアウト: version | flags | seq | lat_e7(4) | lon_e7(4) | time_unix(4) | crc8
#pragma once
#include <stdint.h>
#include <string.h>

#define NOSTOS_FRAME_LEN 16
#define NOSTOS_PROTOCOL_VERSION 1
#define NOSTOS_FLAG_FIX_VALID 0x01

// CRC-8（多項式 0x07・初期値 0x00）。Rust 版 nostos_frame::crc8 と一致。
static inline uint8_t nostos_crc8(const uint8_t *data, uint32_t len) {
  uint8_t crc = 0;
  for (uint32_t i = 0; i < len; i++) {
    crc ^= data[i];
    for (int b = 0; b < 8; b++) {
      crc = (crc & 0x80) ? (uint8_t)((crc << 1) ^ 0x07) : (uint8_t)(crc << 1);
    }
  }
  return crc;
}

// フレームを 16 バイトへエンコードする。out は 16 バイト以上。
static inline void nostos_frame_encode(uint8_t *out, uint8_t seq, int32_t lat_e7,
                                       int32_t lon_e7, uint32_t time_unix,
                                       int fix_valid) {
  out[0] = NOSTOS_PROTOCOL_VERSION;
  out[1] = fix_valid ? NOSTOS_FLAG_FIX_VALID : 0;
  out[2] = seq;
  memcpy(&out[3], &lat_e7, 4);    // ESP32-C6 はリトルエンディアン
  memcpy(&out[7], &lon_e7, 4);
  memcpy(&out[11], &time_unix, 4);
  out[15] = nostos_crc8(out, 15);
}
