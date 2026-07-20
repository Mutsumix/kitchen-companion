use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::Text,
};
use esp_idf_hal::{
    delay::{Delay, FreeRtos, BLOCK},
    gpio::{AnyIOPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{DataBitWidth, StdConfig},
        I2sDriver,
    },
    peripherals::Peripherals,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
    units::FromValueType,
};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use mipidsi::{
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
    Builder,
};

// ビルド時にプロジェクト直下の cfg.toml から読み込まれる設定。
// cfg.toml は .gitignore 済み(cfg.toml.example からコピーして作る)
#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_pass: &'static str,
}

// CoreS3 内蔵I2Cバス上のデバイスアドレス
const AXP2101_ADDR: u8 = 0x34; // 電源管理IC
const AW9523_ADDR: u8 = 0x58; // IOエキスパンダ
const FT6336_ADDR: u8 = 0x38; // 静電タッチコントローラ
const ES7210_ADDR: u8 = 0x40; // マイクADC(デュアルマイク)
const AW88298_ADDR: u8 = 0x36; // スピーカーアンプ

const SAMPLE_RATE_HZ: u32 = 16000; // 音声認識用途の定番レート

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().expect("ペリフェラルの取得に失敗");

    // ---- 1. I2Cバス初期化 (SDA=GPIO12, SCL=GPIO11) ----
    let mut i2c = I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio12,
        peripherals.pins.gpio11,
        &I2cConfig::new().baudrate(400.kHz().into()),
    )
    .expect("I2Cドライバの初期化に失敗");

    // ---- 2. AXP2101(電源IC)初期化 ----
    // これをやらないとLCDバックライト(DLDO1)に電源が供給されず画面が真っ暗のまま。
    // レジスタ値はM5Unifiedの実績値。電圧レジスタは 0.5V + N*0.1V のエンコード
    let axp2101_init: [(u8, u8); 9] = [
        (0x90, 0xBF), // LDO有効化: ALDO1-4, BLDO1-2, DLDO1(bit7=バックライト)をON
        (0x92, 0x0D), // ALDO1 = 1.8V (AW88298 スピーカーアンプ)
        (0x93, 0x1C), // ALDO2 = 3.3V (ES7210 マイクADC)
        (0x94, 0x1C), // ALDO3 = 3.3V (カメラ)
        (0x95, 0x1C), // ALDO4 = 3.3V (TFカードスロット)
        (0x99, 0x1C), // DLDO1 = 3.3V (LCDバックライト。この電圧が明るさになる)
        (0x27, 0x00), // 電源ボタン: 長押し1秒で起動 / 4秒で電源断
        (0x69, 0x11), // 充電LED設定
        (0x10, 0x30), // PMU共通設定
    ];
    for (reg, val) in axp2101_init {
        i2c.write(AXP2101_ADDR, &[reg, val], BLOCK)
            .expect("AXP2101への書き込みに失敗");
    }

    // ---- 3. AW9523(IOエキスパンダ)初期化 ----
    // LCDのリセット線はESP32のGPIOではなくAW9523のP1_1につながっている。
    // これをHighにしないとLCDがリセット状態のまま動かない。値はM5GFXの実績値
    let aw9523_init: [(u8, u8); 7] = [
        (0x02, 0b0000_0111), // P0出力値: TOUCH_RST=1, BUS_EN=1, P0_2=1
        (0x03, 0b1000_0011), // P1出力値: BOOST_EN=1, LCD_RST=1, CAM_RST=1
        (0x04, 0b0001_1000), // P0方向: bit3,4のみ入力、他は出力
        (0x05, 0b0000_1100), // P1方向: bit2,3のみ入力、他は出力
        (0x11, 0b0001_0000), // P0をプッシュプル出力に
        (0x12, 0b1111_1111), // P0をLEDモードでなくGPIOモードに
        (0x13, 0b1111_1111), // P1をLEDモードでなくGPIOモードに
    ];
    for (reg, val) in aw9523_init {
        i2c.write(AW9523_ADDR, &[reg, val], BLOCK)
            .expect("AW9523への書き込みに失敗");
    }

    // LCDリセット解除後の安定待ち
    FreeRtos::delay_ms(100);
    log::info!("電源IC・IOエキスパンダの初期化完了");

    // ---- 4. SPIバス初期化 (SCK=GPIO36, MOSI=GPIO37, CS=GPIO3, DC=GPIO35) ----
    // CoreS3のLCDは書き込み専用でMISOは使わない(GPIO35はDCと共用)
    let spi = SpiDriver::new(
        peripherals.spi2,
        peripherals.pins.gpio36,
        peripherals.pins.gpio37,
        None::<AnyIOPin>,
        &SpiDriverConfig::new(),
    )
    .expect("SPIドライバの初期化に失敗");
    let spi_device = SpiDeviceDriver::new(
        spi,
        Some(peripherals.pins.gpio3),
        &SpiConfig::new().baudrate(40.MHz().into()),
    )
    .expect("SPIデバイスの初期化に失敗");
    let dc = PinDriver::output(peripherals.pins.gpio35).expect("DCピンの初期化に失敗");

    // ---- 5. LCD(ILI9342C)初期化 ----
    let mut delay = Delay::new_default();
    let mut buffer = [0u8; 512];
    let di = SpiInterface::new(spi_device, dc, &mut buffer);
    let mut display = Builder::new(ILI9342CRgb565, di)
        .display_size(320, 240)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .expect("LCDの初期化に失敗");
    log::info!("LCD初期化完了");

    // ---- 6. RGB3色バーを描画(色順設定の誤りを目視検出できるように) ----
    display.clear(Rgb565::BLACK).expect("画面クリアに失敗");
    let bars = [
        (Rgb565::RED, 0),
        (Rgb565::GREEN, 80),
        (Rgb565::BLUE, 160),
    ];
    for (color, y) in bars {
        Rectangle::new(Point::new(0, y), Size::new(320, 80))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(&mut display)
            .expect("バーの描画に失敗");
    }
    log::info!("描画完了: 上からRED/GREEN/BLUEの3色バーが表示されているはず");

    // ---- 7. Wi-Fi接続 ----
    let text_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    if CONFIG.wifi_ssid.is_empty() {
        Text::new("cfg.toml niSSID wo kinyuu", Point::new(10, 225), text_style)
            .draw(&mut display)
            .expect("テキスト描画に失敗");
        panic!("cfg.tomlにWi-FiのSSID/パスワードを記入してください(cfg.toml.example参照)");
    }
    Text::new("WiFi connecting...", Point::new(10, 225), text_style)
        .draw(&mut display)
        .expect("テキスト描画に失敗");

    let sys_loop = EspSystemEventLoop::take().expect("イベントループの取得に失敗");
    let nvs = EspDefaultNvsPartition::take().expect("NVSパーティションの取得に失敗");
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs)).expect("Wi-Fiドライバの初期化に失敗"),
        sys_loop,
    )
    .expect("BlockingWifiの生成に失敗");
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: CONFIG.wifi_ssid.try_into().expect("SSIDが長すぎる(最大32文字)"),
        password: CONFIG.wifi_pass.try_into().expect("パスワードが長すぎる(最大64文字)"),
        ..Default::default()
    }))
    .expect("Wi-Fi設定に失敗");
    wifi.start().expect("Wi-Fi起動に失敗");
    // 初回接続はタイムアウトすることがあるのでリトライする
    let mut attempt = 1;
    loop {
        match wifi.connect() {
            Ok(()) => break,
            Err(e) if attempt < 5 => {
                log::warn!("Wi-Fi接続失敗({attempt}回目): {e}。3秒後に再試行");
                attempt += 1;
                FreeRtos::delay_ms(3000);
            }
            Err(e) => panic!("Wi-Fi接続に5回失敗: {e}。SSID/パスワードと2.4GHz帯かを確認"),
        }
    }
    wifi.wait_netif_up().expect("IPアドレス取得待ちに失敗");
    let ip_info = wifi
        .wifi()
        .sta_netif()
        .get_ip_info()
        .expect("IP情報の取得に失敗");
    log::info!("Wi-Fi接続完了: IP = {}", ip_info.ip);

    // 接続表示を塗りつぶしてからIPアドレスを画面に表示
    Rectangle::new(Point::new(0, 205), Size::new(320, 35))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLUE))
        .draw(&mut display)
        .expect("描画に失敗");
    let ip_text = format!("IP: {}", ip_info.ip);
    Text::new(&ip_text, Point::new(10, 225), text_style)
        .draw(&mut display)
        .expect("テキスト描画に失敗");

    // ---- 8. ES7210(マイクADC)をI2Cで初期化 ----
    // レジスタ値はM5Unifiedの実績値。マイク1/2(前面デュアルマイク)を有効化し、
    // 16bit I2Sスレーブとして動かす。マイク3/4は未接続なのでパワーダウン
    let es7210_init: [(u8, u8); 25] = [
        (0x00, 0xFF), // リセット
        (0x00, 0x41), // リセット解除
        (0x01, 0x1F), // クロック一旦全ON
        (0x06, 0x00), // デジタル電源ON
        (0x07, 0x20), // ADCオーバーサンプリング設定
        (0x08, 0x10), // 動作モード
        (0x09, 0x30), // チャージポンプ設定0
        (0x0A, 0x30), // チャージポンプ設定1
        (0x20, 0x0A), // ADC34ハイパスフィルタ2
        (0x21, 0x2A), // ADC34ハイパスフィルタ1
        (0x22, 0x0A), // ADC12ハイパスフィルタ2
        (0x23, 0x2A), // ADC12ハイパスフィルタ1
        (0x02, 0xC1), // クロックマネージャ(MCLK分周設定)
        (0x04, 0x01), // ADC制御
        (0x05, 0x00), // ADC制御
        (0x11, 0x60), // シリアルポート: 16bit I2Sフォーマット
        (0x40, 0x42), // アナログ系電源ON
        (0x41, 0x70), // マイクバイアス1/2
        (0x42, 0x70), // マイクバイアス3/4
        (0x43, 0x1B), // マイク1ゲイン
        (0x44, 0x1B), // マイク2ゲイン
        (0x45, 0x00), // マイク3ゲイン(未使用)
        (0x46, 0x00), // マイク4ゲイン(未使用)
        (0x4B, 0x00), // マイク1/2電源ON
        (0x4C, 0xFF), // マイク3/4パワーダウン
    ];
    for (reg, val) in es7210_init {
        i2c.write(ES7210_ADDR, &[reg, val], BLOCK)
            .expect("ES7210への書き込みに失敗");
    }
    i2c.write(ES7210_ADDR, &[0x01, 0x14], BLOCK)
        .expect("ES7210のクロック設定に失敗"); // 必要なクロックのみ残して確定
    log::info!("ES7210初期化完了");

    // ---- 9. AW88298(スピーカーアンプ)をI2Cで初期化 ----
    // レジスタは16bit幅・ビッグエンディアンで書く。値はM5Unifiedの実績値。
    // AW88298のリセット/イネーブルはAW9523のP0_2(初期化済み)
    let aw88298_init: [(u8, u16); 5] = [
        (0x61, 0x0673), // ブーストモード無効
        (0x04, 0x4040), // I2S有効・アンプON
        (0x05, 0x0008), // ミュート解除
        (0x06, 0x14C3), // I2S設定+サンプルレート: (16000+1102)/2205=7 → テーブルidx3 | 0x14C0
        (0x0C, 0x0064), // 音量
    ];
    for (reg, val) in aw88298_init {
        let [hi, lo] = val.to_be_bytes();
        i2c.write(AW88298_ADDR, &[reg, hi, lo], BLOCK)
            .expect("AW88298への書き込みに失敗");
    }
    log::info!("AW88298初期化完了");

    // ---- 10. I2S双方向(BCLK=GPIO34, WS=GPIO33, DIN=GPIO14, DOUT=GPIO13, MCLK=GPIO0) ----
    // マイク(ES7210)とスピーカー(AW88298)は同じI2Sバスを共有している。
    // ESP32-S3がマスター、16kHz/16bit/ステレオ(マイク1=左, マイク2=右)
    let i2s_config = StdConfig::philips(SAMPLE_RATE_HZ, DataBitWidth::Bits16);
    let mut i2s = I2sDriver::new_std_bidir(
        peripherals.i2s1,
        &i2s_config,
        peripherals.pins.gpio34,
        peripherals.pins.gpio14,
        peripherals.pins.gpio13,
        Some(peripherals.pins.gpio0),
        peripherals.pins.gpio33,
    )
    .expect("I2Sドライバの初期化に失敗");
    i2s.rx_enable().expect("I2S受信の開始に失敗");
    i2s.tx_enable().expect("I2S送信の開始に失敗");
    log::info!("I2S送受信開始");

    // タッチ時に鳴らすビープ音(1kHzサイン波・150ms)をあらかじめ生成しておく
    let beep: Vec<u8> = {
        let frames = (SAMPLE_RATE_HZ as usize) * 150 / 1000;
        let mut buf = Vec::with_capacity(frames * 4);
        for n in 0..frames {
            let t = n as f32 / SAMPLE_RATE_HZ as f32;
            let sample = ((t * 1000.0 * 2.0 * core::f32::consts::PI).sin() * 8000.0) as i16;
            let bytes = sample.to_le_bytes();
            buf.extend_from_slice(&bytes); // 左ch
            buf.extend_from_slice(&bytes); // 右ch
        }
        buf
    };

    // ---- 11. メインループ: 音量レベルメーター + タッチで白丸&ビープ ----
    // 512フレーム(ステレオ16bit=2048バイト)ずつ読む。16kHzなので1回あたり32ms
    let mut audio_buf = [0u8; 2048];
    let meter_area = Rectangle::new(Point::new(0, 180), Size::new(320, 20));
    let mut was_touched = false;
    let mut prev_bar_width = 0u32;
    loop {
        // --- 録音データを読んでRMS音量を計算 ---
        let n = i2s.read(&mut audio_buf, BLOCK).expect("I2S読み取りに失敗");
        let mut sum_sq: i64 = 0;
        let mut count: i64 = 0;
        // ステレオのうち左チャネル(マイク1)だけ使う: 4バイト周期の先頭2バイト
        for frame in audio_buf[..n].chunks_exact(4) {
            let sample = i16::from_le_bytes([frame[0], frame[1]]) as i64;
            sum_sq += sample * sample;
            count += 1;
        }
        let rms = if count > 0 {
            ((sum_sq / count) as f32).sqrt()
        } else {
            0.0
        };

        // --- RMSをdBに変換してレベルメーター描画(-60dB..0dB → 0..320px) ---
        let db = 20.0 * (rms.max(1.0) / 32768.0).log10(); // -90dB〜0dB
        let bar_width = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 320.0) as u32;
        if bar_width != prev_bar_width {
            meter_area
                .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
                .draw(&mut display)
                .expect("メーター背景の描画に失敗");
            Rectangle::new(Point::new(0, 180), Size::new(bar_width, 20))
                .into_styled(PrimitiveStyle::with_fill(Rgb565::GREEN))
                .draw(&mut display)
                .expect("メーターの描画に失敗");
            prev_bar_width = bar_width;
        }

        // --- タッチ位置に白丸(座標系検証の名残。録音と共存できることの確認も兼ねる) ---
        let mut buf = [0u8; 5];
        i2c.write_read(FT6336_ADDR, &[0x02], &mut buf, BLOCK)
            .expect("FT6336の読み取りに失敗");
        let touches = buf[0] & 0x0F;
        if touches > 0 {
            let x = (((buf[1] & 0x0F) as i32) << 8) | buf[2] as i32;
            let y = (((buf[3] & 0x0F) as i32) << 8) | buf[4] as i32;
            if !was_touched {
                log::info!("タッチ検出: x={x}, y={y} → ビープ再生");
                was_touched = true;
                // タッチした瞬間にビープを鳴らす(書き込みは全バイト送るまでループ)
                let mut written = 0;
                while written < beep.len() {
                    written += i2s
                        .write(&beep[written..], BLOCK)
                        .expect("I2S書き込みに失敗");
                }
            }
            Circle::with_center(Point::new(x, y), 12)
                .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
                .draw(&mut display)
                .expect("丸の描画に失敗");
        } else {
            was_touched = false;
        }
    }
}
