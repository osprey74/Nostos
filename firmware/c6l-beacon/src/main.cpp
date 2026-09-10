// Nostos C6L ビーコン — GPS の位置＋時刻を「移動検知で適応的な間隔」＋「ボタン任意発信」で
// 生 LoRa 送信する。**毎送信の前に RSSI ベースのキャリアセンス(LBT)**を行い、混信を防ぐ。
//
// ⚠️ 電波法・技適コンプライアンス（docs/COMPLIANCE.md 厳守）:
//   送信 RF は認証枠に固定（923.000MHz / BW125 / +6dBm）。認証枠外に変更しない。
//   868MHz 等の国外帯域では送信しない。純正アンテナのまま使用する。
//   920MHz は原則キャリアセンス必須（ARIB STD-T108）。下記 LBT を毎送信で実行する。
//
// ピンマップ出典: meshtastic 変種 variants/esp32c6/m5stack_unitc6l/variant.h
//   SX1262: SCK=20 MISO=22 MOSI=21 CS=23 / DIO1=7 BUSY=19 RESET=NC /
//           DIO2=RFスイッチ DIO3=TCXO 3.0V
//   GPS UART: RX=4 TX=5 / ボタン: GPIO9(BOOT・active-low)

#include <Arduino.h>
#include <math.h>
#include <RadioLib.h>
#include <TinyGPSPlus.h>
#include "nostos_frame.h"

// ---- コンプライアンス固定パラメータ（crates/nostos-frame::radio と一致・変更禁止領域） ----
static const float   TX_FREQ_MHZ   = 923.0f;   // 認証帯 922〜923.4MHz 内
static const float   TX_BW_KHZ     = 125.0f;   // ≤200kHz（認証）
static const int8_t  TX_POWER_DBM  = 6;        // ≤+7dBm(≈5.0mW認証上限)。+22dBmに上げない
static const uint8_t TX_SF         = 9;
static const uint8_t TX_CR         = 5;        // 4/5
static const uint8_t TX_SYNC_WORD  = 0x3A;     // Nostos 独自ネット（規制対象外・両端一致）
static const float   TCXO_VOLT     = 3.0f;     // DIO3 TCXO

// ---- LBT（キャリアセンス）: ARIB STD-T108。閾値・窓は保守的な既定（要 T108/認証カテゴリ確認） ----
static const float    CS_THRESHOLD_DBM = -80.0f; // これ以上の RSSI は「混雑」→送信しない ※要確認
static const uint32_t CS_LISTEN_US     = 300;    // リッスン窓（規定 ≥128μs に対し安全側）
static const int      LBT_MAX_TRIES    = 3;      // 混雑時の再試行回数
static const uint32_t LBT_BACKOFF_MS   = 3000;   // 全滅時に次サイクルまで待つ

// ---- 適応間隔（移動=短 / 停止=広）＋ボタン任意発信（総司様仕様） ----
static const uint32_t MIN_INTERVAL_MS  = 30UL * 1000;    // 移動時の最短送信間隔
static const uint32_t MAX_INTERVAL_MS  = 600UL * 1000;   // 停止時のハートビート間隔（10分）
static const float    MOVE_THRESHOLD_M = 30.0f;          // 前回送信位置からこれ以上動いたら「移動」

// ---- ピン定義（variant.h より） ----
#define PIN_LORA_SCK  20
#define PIN_LORA_MISO 22
#define PIN_LORA_MOSI 21
#define PIN_LORA_CS   23
#define PIN_LORA_DIO1 7
#define PIN_LORA_BUSY 19
#define PIN_GPS_RX    4   // ESP 受信（GPS-TX 接続）
#define PIN_GPS_TX    5
#define PIN_BUTTON    9   // BOOT/ユーザーボタン（active-low）

SX1262 radio = new Module(PIN_LORA_CS, PIN_LORA_DIO1, RADIOLIB_NC, PIN_LORA_BUSY);
TinyGPSPlus gps;

static uint8_t  seq = 0;
static uint32_t last_tx_ms = 0;
static double   last_tx_lat = 0, last_tx_lon = 0;
static bool     have_last = false;
static uint32_t retry_after_ms = 0;

// TinyGPS の日時（UTC）から unix 秒へ。未確定時は 0。
static uint32_t gps_unix_time() {
  if (!gps.date.isValid() || !gps.time.isValid() || gps.date.year() < 2020) return 0;
  int y = gps.date.year(); unsigned m = gps.date.month(), d = gps.date.day();
  y -= m <= 2;
  const int era = (y >= 0 ? y : y - 399) / 400;
  const unsigned yoe = (unsigned)(y - era * 400);
  const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
  const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  const long days = (long)era * 146097 + (long)doe - 719468;
  return (uint32_t)(days * 86400L + gps.time.hour() * 3600L +
                    gps.time.minute() * 60L + gps.time.second());
}

// 2 点間距離 [m]（haversine）。移動検知用。
static double dist_m(double la1, double lo1, double la2, double lo2) {
  const double R = 6371008.8, d2r = M_PI / 180.0;
  double p1 = la1 * d2r, p2 = la2 * d2r, dp = (la2 - la1) * d2r, dl = (lo2 - lo1) * d2r;
  double a = sin(dp / 2) * sin(dp / 2) + cos(p1) * cos(p2) * sin(dl / 2) * sin(dl / 2);
  return R * 2 * atan2(sqrt(a), sqrt(1 - a));
}

// LBT: 送信前エネルギー検出（RSSI）キャリアセンス。空きなら true。
// ※ RadioLib の RSSI 取得挙動は実機ビルド時に微調整前提（getRSSI の引数/タイミング）。
static bool lbt_clear() {
  radio.startReceive();
  delayMicroseconds(CS_LISTEN_US);
  float rssi = radio.getRSSI();
  radio.standby();
  return rssi < CS_THRESHOLD_DBM;
}

// LBT を挟んで 1 フレーム送信。成功で true。
static bool send_frame(bool fix, int32_t lat_e7, int32_t lon_e7, uint32_t t, const char *why) {
  uint8_t frame[NOSTOS_FRAME_LEN];
  nostos_frame_encode(frame, seq, lat_e7, lon_e7, t, fix ? 1 : 0);
  for (int i = 0; i < LBT_MAX_TRIES; i++) {
    if (lbt_clear()) {
      int st = radio.transmit(frame, NOSTOS_FRAME_LEN);
      Serial.printf("nostos-beacon: TX(%s) seq=%u fix=%d lat_e7=%ld lon_e7=%ld t=%lu st=%d\n",
                    why, seq, fix, (long)lat_e7, (long)lon_e7, (unsigned long)t, st);
      seq++;
      return st == RADIOLIB_ERR_NONE;
    }
    delay(20 + i * 40);  // 簡易バックオフ
  }
  Serial.println("nostos-beacon: LBT busy -> skip");
  return false;
}

void setup() {
  Serial.begin(115200);
  Serial1.begin(9600, SERIAL_8N1, PIN_GPS_RX, PIN_GPS_TX);
  pinMode(PIN_BUTTON, INPUT_PULLUP);

  SPI.begin(PIN_LORA_SCK, PIN_LORA_MISO, PIN_LORA_MOSI, PIN_LORA_CS);
  int st = radio.begin(TX_FREQ_MHZ, TX_BW_KHZ, TX_SF, TX_CR, TX_SYNC_WORD,
                       TX_POWER_DBM, 8 /*preamble*/, TCXO_VOLT, false /*LDO*/);
  if (st != RADIOLIB_ERR_NONE) {
    Serial.printf("nostos-beacon: radio.begin failed (%d)\n", st);
    while (true) delay(1000);
  }
  radio.setDio2AsRfSwitch(true);   // variant: DIO2=RFスイッチ
  Serial.printf("nostos-beacon: ready @ %.3fMHz BW%.0fk SF%d %ddBm sync0x%02X "
                "(move<=%.0fs / stop<=%.0fs / btn=manual, LBT on)\n",
                TX_FREQ_MHZ, TX_BW_KHZ, TX_SF, TX_POWER_DBM, TX_SYNC_WORD,
                MIN_INTERVAL_MS / 1000.0, MAX_INTERVAL_MS / 1000.0);
}

void loop() {
  while (Serial1.available()) gps.encode(Serial1.read());

  // ボタン単クリック検出（active-low・250ms デバウンス）
  static bool prev_btn = HIGH;
  static uint32_t last_btn_ms = 0;
  bool btn = digitalRead(PIN_BUTTON);
  bool manual = false;
  if (prev_btn == HIGH && btn == LOW && millis() - last_btn_ms > 250) {
    manual = true;
    last_btn_ms = millis();
  }
  prev_btn = btn;

  bool fix = gps.location.isValid();
  double lat = fix ? gps.location.lat() : 0.0;
  double lon = fix ? gps.location.lng() : 0.0;
  int32_t lat_e7 = (int32_t)(lat * 1e7);
  int32_t lon_e7 = (int32_t)(lon * 1e7);
  uint32_t t = gps_unix_time();

  uint32_t now = millis();
  double moved = (have_last && fix) ? dist_m(last_tx_lat, last_tx_lon, lat, lon) : 0.0;
  bool moving = fix && have_last && moved >= MOVE_THRESHOLD_M;

  // 送信要否の判定
  const char *why = nullptr;
  if (manual) {
    why = "button";                                             // 任意発信
  } else if (now < retry_after_ms) {
    why = nullptr;                                              // LBT 全滅後のバックオフ中
  } else if (fix && moving && (now - last_tx_ms) >= MIN_INTERVAL_MS) {
    why = "move";                                              // 移動 → 短間隔
  } else if (last_tx_ms == 0 || (now - last_tx_ms) >= MAX_INTERVAL_MS) {
    why = "heartbeat";                                        // 停止/初回 → 広間隔
  }

  if (why) {
    if (send_frame(fix, lat_e7, lon_e7, t, why)) {
      last_tx_ms = now;
      if (fix) { last_tx_lat = lat; last_tx_lon = lon; have_last = true; }
    } else {
      retry_after_ms = now + LBT_BACKOFF_MS;   // 混雑 → 少し待って再試行
    }
  }
}
