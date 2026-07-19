use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
};
use esp_idf_hal::{
    delay::{Delay, FreeRtos, BLOCK},
    gpio::{AnyIOPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    peripherals::Peripherals,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
    units::FromValueType,
};
use mipidsi::{
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
    Builder,
};

// CoreS3 内蔵I2Cバス上のデバイスアドレス
const AXP2101_ADDR: u8 = 0x34; // 電源管理IC
const AW9523_ADDR: u8 = 0x58; // IOエキスパンダ
const FT6336_ADDR: u8 = 0x38; // 静電タッチコントローラ

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

    // ---- 7. タッチ(FT6336)をポーリングして、触った場所に白丸を描く ----
    // FT6336のリセット線はAW9523のP0_0。上の初期化でHighにしているので既に動いている
    let mut was_touched = false;
    loop {
        // レジスタ0x02(タッチ点数)から5バイト連続読み: 点数, X上位, X下位, Y上位, Y下位
        let mut buf = [0u8; 5];
        i2c.write_read(FT6336_ADDR, &[0x02], &mut buf, BLOCK)
            .expect("FT6336の読み取りに失敗");
        let touches = buf[0] & 0x0F;
        if touches > 0 {
            // 座標は12bit。上位バイトは下位4bitのみ有効(上位2bitはイベントフラグ)
            let x = (((buf[1] & 0x0F) as i32) << 8) | buf[2] as i32;
            let y = (((buf[3] & 0x0F) as i32) << 8) | buf[4] as i32;
            if !was_touched {
                log::info!("タッチ検出: x={x}, y={y}");
                was_touched = true;
            }
            Circle::with_center(Point::new(x, y), 12)
                .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
                .draw(&mut display)
                .expect("丸の描画に失敗");
        } else {
            was_touched = false;
        }
        FreeRtos::delay_ms(20);
    }
}
