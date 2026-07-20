use embedded_graphics::{
    image::{Image, ImageRaw},
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};
use esp_idf_hal::{
    delay::{Delay, FreeRtos, BLOCK},
    gpio::{AnyIOPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{
            Config as I2sChanConfig, DataBitWidth, SlotMode, StdClkConfig, StdConfig,
            StdGpioConfig, StdSlotConfig,
        },
        I2sDriver,
    },
    peripherals::Peripherals,
    spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig},
    units::FromValueType,
};
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::client::{Configuration as HttpConfig, EspHttpConnection},
    http::Method,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use mipidsi::{
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
    Builder,
};
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

// ビルド時にプロジェクト直下の cfg.toml から読み込まれる設定。
// cfg.toml は .gitignore 済み(cfg.toml.example からコピーして作る)
#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_pass: &'static str,
    #[default("")]
    server_url: &'static str,
}

/// モノラル16bit PCM用の44バイトWAVヘッダを作る
fn wav_header(pcm_len: u32, sample_rate: u32) -> [u8; 44] {
    let byte_rate = sample_rate * 2; // モノラル16bit
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + pcm_len).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes()); // fmtチャンクサイズ
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // モノラル
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // ブロックアライン
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // ビット深度
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&pcm_len.to_le_bytes());
    h
}

/// 録音PCMをWAVとして中継WorkerにPOSTし、応答音声(16kHzモノラルPCM)を
/// **受信しながらそのままI2Sへ流して再生する**(ストリーミング再生)。
/// 再生中もタッチを監視し、押されたら受信を打ち切って戻る(割り込み)。
/// 戻り値: (HTTPステータス, 割り込みされたか)
fn talk<D>(
    url: &str,
    pcm: &[u8],
    sample_rate: u32,
    mode: Mode,
    new_session: bool,
    character: usize,
    i2s: &mut I2sDriver<'_, esp_idf_hal::i2s::I2sBiDir>,
    i2c: &mut I2cDriver<'_>,
    display: &mut D,
    conn_slot: &mut Option<EspHttpConnection>,
) -> Result<(u16, bool), esp_idf_svc::sys::EspError>
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    let header = wav_header(pcm.len() as u32, sample_rate);
    // TLS接続は前回のものを使い回す(毎回のハンドシェイク約1.5秒を節約)。
    // エラーや割り込みで中途半端になった接続は捨てて、次回作り直す
    let mut conn = match conn_slot.take() {
        Some(c) => c,
        None => EspHttpConnection::new(&HttpConfig {
            // HTTPSに必要なルート証明書バンドル(ESP-IDF組み込み)
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            // STT+ストリーミング応答全体をカバーするタイムアウト
            timeout: Some(core::time::Duration::from_secs(60)),
            ..Default::default()
        })?,
    };
    let len = (header.len() + pcm.len()).to_string();
    let mut headers = vec![
        ("Content-Type", "audio/wav"),
        ("Content-Length", len.as_str()),
        ("X-Mode", mode.header_value()),
    ];
    if new_session {
        headers.push(("X-New-Session", "1"));
    }
    conn.initiate_request(Method::Post, url, &headers)?;
    conn.write_all(&header)?;
    conn.write_all(pcm)?;
    conn.initiate_response()?;
    let status = conn.status();
    if status != 200 {
        // エラー時はボディを読み捨てて戻る(接続は再利用可能なので返却)
        let mut sink = [0u8; 1024];
        while conn.read(&mut sink)? > 0 {}
        *conn_slot = Some(conn);
        return Ok((status, false));
    }

    let timing = conn.header("X-Timing").unwrap_or_default().to_string();
    log::info!("応答ストリーム開始 / {timing}");

    // モノラルPCMを受信 → ステレオ化 → I2Sへ書き込み(ブロッキング=再生ペースで進む)
    let mut chunk = [0u8; 4096];
    let mut stereo = [0u8; 8192 + 4]; // 4096バイト分のステレオ+端数余裕
    let mut carry: Option<u8> = None; // チャンク境界でサンプルが割れたときの持ち越し
    // 口パクアニメーション(音声バッファに500msの蓄えがあるため、描画時間は吸収される)
    let mut anim_frame: usize = 0;
    let mut last_anim = std::time::Instant::now();
    loop {
        let n = conn.read(&mut chunk)?;
        if n == 0 {
            break; // ストリーム終端
        }
        // 持ち越しバイトと連結してから16bitサンプル単位で処理
        let mut mono: Vec<u8> = Vec::with_capacity(n + 1);
        if let Some(b) = carry.take() {
            mono.push(b);
        }
        mono.extend_from_slice(&chunk[..n]);
        let pairs = mono.len() / 2;
        if mono.len() % 2 == 1 {
            carry = Some(mono[mono.len() - 1]);
        }
        let mut out = 0;
        for s in mono[..pairs * 2].chunks_exact(2) {
            stereo[out..out + 2].copy_from_slice(s); // 左ch
            stereo[out + 2..out + 4].copy_from_slice(s); // 右ch
            out += 4;
        }
        i2s.write_all(&stereo[..out], BLOCK)?;

        // 割り込みチェック: 再生中にタッチされたら受信を打ち切る
        let mut tbuf = [0u8; 1];
        i2c.write_read(FT6336_ADDR, &[0x02], &mut tbuf, BLOCK)?;
        if (tbuf[0] & 0x0F) > 0 {
            log::info!("割り込みタッチ検出 → ストリーム再生を中断");
            // 応答を読み切っていない接続は再利用できないので捨てる(次回作り直し)
            return Ok((status, true));
        }

        // 口パク(250msごとにフレーム送り)
        if FACE_SPRITES_ENABLED && last_anim.elapsed().as_millis() > 250 {
            anim_frame = (anim_frame + 1) % 3;
            draw_face(display, character, FaceState::Speaking, anim_frame);
            last_anim = std::time::Instant::now();
        }
    }
    *conn_slot = Some(conn); // 読み切った接続は次回に再利用
    Ok((status, false))
}

/// 会話モード。タブで切り替え、Workerへ X-Mode ヘッダで伝える
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Consult,  // レシピを一緒に考える
    Shopping, // 在庫を聞き溜めて買い物リストを作る
    Cooking,  // 調理手順を1ステップずつ案内
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Consult, Mode::Shopping, Mode::Cooking];
    fn label(self) -> &'static str {
        match self {
            Mode::Consult => "相談",
            Mode::Shopping => "買出し",
            Mode::Cooking => "調理中",
        }
    }
    fn header_value(self) -> &'static str {
        match self {
            Mode::Consult => "consult",
            Mode::Shopping => "shopping",
            Mode::Cooking => "cooking",
        }
    }
}

/// 顔の状態。今は文字表示のプレースホルダ(将来スプライトに差し替え)
#[derive(Clone, Copy, PartialEq)]
enum FaceState {
    Idle,      // 待機
    Listening, // 聞き耳(録音中)
    Thinking,  // 考え中(クラウド往復待ち)
    Speaking,  // 話し中(応答再生中)
}

impl FaceState {
    fn index(self) -> usize {
        match self {
            FaceState::Idle => 0,
            FaceState::Listening => 1,
            FaceState::Thinking => 2,
            FaceState::Speaking => 3,
        }
    }
}

// 顔スプライト(320x155 RGB565ビッグエンディアン)。作者制作のPNGを
// tools/face_convert.py で変換したもの。[キャラ][状態][フレーム]
// キャラはダブルタップで循環切替(選択はNVSに保存)
const CHAR_COUNT: usize = 2;

macro_rules! char_frames {
    ($dir:literal) => {
        [
            [
                include_bytes!(concat!("../assets/face/", $dir, "/idle_0.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/idle_1.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/idle_2.raw")),
            ],
            [
                include_bytes!(concat!("../assets/face/", $dir, "/listen_0.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/listen_1.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/listen_2.raw")),
            ],
            [
                include_bytes!(concat!("../assets/face/", $dir, "/think_0.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/think_1.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/think_2.raw")),
            ],
            [
                include_bytes!(concat!("../assets/face/", $dir, "/speak_0.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/speak_1.raw")),
                include_bytes!(concat!("../assets/face/", $dir, "/speak_2.raw")),
            ],
        ]
    };
}

static FACE_FRAMES: [[[&[u8]; 3]; 4]; CHAR_COUNT] =
    [char_frames!("robo"), char_frames!("girl")];

// 画面レイアウト(320x240)
// y   0- 30: モードタブ3つ / y  30-185: 顔エリア(スプライト320x155ぴったり) /
// y 185-240: ステータスバー+「新規」ボタン
// 録音ホールド中はステータスバー全体が横型音量ゲージに切り替わる
const TAB_H: u32 = 30;
const FACE_Y: i32 = 30;
const FACE_H: u32 = 155;
const STATUS_Y: i32 = 185;
const STATUS_H: u32 = 55;
const NEW_BTN_X: i32 = 252;
const CHAR_BTN_W: i32 = 48; // 左端のキャラ切替ボタン幅

fn draw_tabs<D>(d: &mut D, font: &FontRenderer, active: Mode)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    for (i, m) in Mode::ALL.iter().enumerate() {
        let x = i as i32 * 107;
        let (bg, fg) = if *m == active {
            (Rgb565::WHITE, Rgb565::BLACK)
        } else {
            (Rgb565::new(4, 8, 4), Rgb565::WHITE)
        };
        Rectangle::new(Point::new(x, 0), Size::new(106, TAB_H))
            .into_styled(PrimitiveStyle::with_fill(bg))
            .draw(d)
            .expect("タブ描画に失敗");
        font.render_aligned(
            m.label(),
            Point::new(x + 53, TAB_H as i32 / 2),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(fg),
            d,
        )
        .expect("タブ文字の描画に失敗");
    }
}

// スプライト顔の有効/無効。当初、フル描画が遅く応答再生中のアニメーションが
// I2S送信のDMAアンダーランを起こして音が途切れた。SPIバッファ拡大(512B→12KB)と
// I2S DMA増強(90ms→500ms分)、および再生ループ内でのアニメーション実行で両立
const FACE_SPRITES_ENABLED: bool = true;

/// 再生中の口パク: ストリーミング再生ループから呼ばれる

fn draw_face<D>(d: &mut D, character: usize, state: FaceState, frame: usize)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    if FACE_SPRITES_ENABLED {
        let raw = ImageRaw::<Rgb565>::new(FACE_FRAMES[character][state.index()][frame], 320);
        Image::new(&raw, Point::new(0, FACE_Y))
            .draw(d)
            .expect("顔スプライトの描画に失敗");
    } else {
        // 文字プレースホルダ(状態名のみ)
        let label = match state {
            FaceState::Idle => "待機",
            FaceState::Listening => "聞き耳",
            FaceState::Thinking => "考え中",
            FaceState::Speaking => "話し中",
        };
        Rectangle::new(Point::new(0, FACE_Y), Size::new(320, FACE_H))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
            .draw(d)
            .expect("顔エリアの描画に失敗");
        FontRenderer::new::<fonts::u8g2_font_b16_t_japanese1>()
            .render_aligned(
                label,
                Point::new(160, FACE_Y + FACE_H as i32 / 2),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(Rgb565::WHITE),
                d,
            )
            .expect("状態文字の描画に失敗");
        let _ = frame;
    }
}

fn draw_status<D>(
    d: &mut D,
    ascii: MonoTextStyle<'_, Rgb565>,
    font: &FontRenderer,
    msg: &str,
    bg: Rgb565,
) where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    Rectangle::new(Point::new(0, STATUS_Y), Size::new(320, STATUS_H))
        .into_styled(PrimitiveStyle::with_fill(bg))
        .draw(d)
        .expect("ステータス描画に失敗");
    // 左右のボタンに被らないよう最大19文字で打ち切る
    let clipped: String = msg.chars().take(19).collect();
    Text::new(&clipped, Point::new(CHAR_BTN_W + 6, STATUS_Y + 33), ascii)
        .draw(d)
        .expect("ステータス文字の描画に失敗");
    // 「顔」ボタン(キャラ切替。録音ゾーンと完全に分離した専用ボタン)
    Rectangle::new(Point::new(2, STATUS_Y + 9), Size::new((CHAR_BTN_W - 4) as u32, 37))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::WHITE, 1))
        .draw(d)
        .expect("顔ボタンの描画に失敗");
    font.render_aligned(
        "顔",
        Point::new(CHAR_BTN_W / 2, STATUS_Y + 27),
        VerticalPosition::Center,
        HorizontalAlignment::Center,
        FontColor::Transparent(Rgb565::WHITE),
        d,
    )
    .expect("顔ボタン文字の描画に失敗");
    // 「新規」ボタン(セッションを仕切り直す)
    Rectangle::new(Point::new(NEW_BTN_X, STATUS_Y + 9), Size::new(64, 37))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::WHITE, 1))
        .draw(d)
        .expect("新規ボタンの描画に失敗");
    font.render_aligned(
        "新規",
        Point::new(NEW_BTN_X + 32, STATUS_Y + 27),
        VerticalPosition::Center,
        HorizontalAlignment::Center,
        FontColor::Transparent(Rgb565::WHITE),
        d,
    )
    .expect("新規ボタン文字の描画に失敗");
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
    // SPI転送用の中間バッファ。512Bだとフル画面描画が約200分割されて遅く、
    // 顔アニメーション中に音声DMAが枯渇する原因になった。12KB(内蔵RAM)に拡大
    let buffer: &'static mut [u8] = Box::leak(vec![0u8; 12288].into_boxed_slice());
    let di = SpiInterface::new(spi_device, dc, buffer);
    let mut display = Builder::new(ILI9342CRgb565, di)
        .display_size(320, 240)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .expect("LCDの初期化に失敗");
    log::info!("LCD初期化完了");

    // ---- 6. NVS読み出しと初期画面(タブ+顔) ----
    // ※開発初期はここでRGB3色バーを描いて色順(Bgr+Inverted)を検証していた(manual.md 3章)
    // NVS(不揮発設定): キャラ選択の永続化。Wi-Fiも同じパーティションを共用する
    let nvs_part = EspDefaultNvsPartition::take().expect("NVSパーティションの取得に失敗");
    let mut app_nvs = esp_idf_svc::nvs::EspNvs::new(nvs_part.clone(), "app", true)
        .expect("NVS名前空間のオープンに失敗");
    let mut character: usize =
        app_nvs.get_u8("chara").ok().flatten().unwrap_or(0) as usize % CHAR_COUNT;
    log::info!("キャラ選択(NVS): {character}");

    display.clear(Rgb565::BLACK).expect("画面クリアに失敗");
    let jp_font = FontRenderer::new::<fonts::u8g2_font_b16_t_japanese1>();
    let mut mode = Mode::Consult;
    draw_tabs(&mut display, &jp_font, mode);
    draw_face(&mut display, character, FaceState::Idle, 0);
    log::info!("初期画面の描画完了");

    // ---- 7. Wi-Fi接続 ----
    let text_style = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    if CONFIG.wifi_ssid.is_empty() {
        draw_status(&mut display, text_style, &jp_font, "cfg.toml: SSID missing", Rgb565::RED);
        panic!("cfg.tomlにWi-FiのSSID/パスワードを記入してください(cfg.toml.example参照)");
    }
    draw_status(&mut display, text_style, &jp_font, "WiFi connecting...", Rgb565::BLUE);

    let sys_loop = EspSystemEventLoop::take().expect("イベントループの取得に失敗");
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs_part.clone()))
            .expect("Wi-Fiドライバの初期化に失敗"),
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
    let ip_text = format!("IP: {}", ip_info.ip);
    draw_status(&mut display, text_style, &jp_font, &ip_text, Rgb565::BLUE);

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
    // DMAバッファは8個×1000フレーム=約500ms分(既定は90ms分)。
    // 描画などでループが数十ms止まっても音声が途切れない余裕を持たせる。
    // auto_clear=trueで、万一のアンダーラン時は「最後のバッファ繰り返し」ではなく無音になる
    let i2s_config = StdConfig::new(
        I2sChanConfig::default()
            .dma_buffer_count(8)
            .frames_per_buffer(1000)
            .auto_clear(true),
        StdClkConfig::from_sample_rate_hz(SAMPLE_RATE_HZ),
        StdSlotConfig::philips_slot_default(DataBitWidth::Bits16, SlotMode::Stereo),
        StdGpioConfig::default(),
    );
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

    // タッチ時に鳴らすビープ音(1kHzサイン波・80ms)をあらかじめ生成しておく。
    // 短いのは、押してすぐ話し始められるように(ビープ再生中は録音しないため)。
    // 末尾に150msの無音を付ける: I2S送信DMAはデータが尽きると最後のバッファを
    // 繰り返し再生するため、無音で終わらせないとビープが鳴りっぱなしになる
    let beep: Vec<u8> = {
        let tone_frames = (SAMPLE_RATE_HZ as usize) * 80 / 1000;
        let silence_frames = (SAMPLE_RATE_HZ as usize) * 150 / 1000;
        let mut buf = Vec::with_capacity((tone_frames + silence_frames) * 4);
        for n in 0..(tone_frames + silence_frames) {
            let sample = if n < tone_frames {
                let t = n as f32 / SAMPLE_RATE_HZ as f32;
                ((t * 1000.0 * 2.0 * core::f32::consts::PI).sin() * 1250.0) as i16
            } else {
                0
            };
            let bytes = sample.to_le_bytes();
            buf.extend_from_slice(&bytes); // 左ch
            buf.extend_from_slice(&bytes); // 右ch
        }
        buf
    };

    // キャラ切替時のチャイム(上昇2音+末尾無音。無音はDMA繰り返し対策で必須)
    let chime: Vec<u8> = {
        let mut buf = Vec::new();
        for (freq, ms) in [(880.0f32, 70usize), (1320.0, 90), (0.0, 150)] {
            let frames = (SAMPLE_RATE_HZ as usize) * ms / 1000;
            for n in 0..frames {
                let t = n as f32 / SAMPLE_RATE_HZ as f32;
                let sample = if freq > 0.0 {
                    ((t * freq * 2.0 * core::f32::consts::PI).sin() * 1250.0) as i16
                } else {
                    0
                };
                let bytes = sample.to_le_bytes();
                buf.extend_from_slice(&bytes); // 左ch
                buf.extend_from_slice(&bytes); // 右ch
            }
        }
        buf
    };

    // ---- 11. メインループ: 音量レベルメーター + プッシュ・トゥ・トーク ----
    // 512フレーム(ステレオ16bit=2048バイト)ずつ読む。16kHzなので1回あたり32ms
    let mut audio_buf = [0u8; 2048];
    let mut was_touched = false;
    let mut prev_gauge_width = 0u32;
    // 顔の状態と、セッション仕切り直しの予約フラグ
    let mut face = FaceState::Idle;
    let mut new_session = false;
    // TLS接続の使い回しスロット(talkが管理)
    let mut http_conn: Option<EspHttpConnection> = None;
    // 顔アニメーション: 現在フレームと最終切替時刻
    let mut face_frame: usize = 0;
    let mut last_anim = std::time::Instant::now();
    // 再生キュー。Some((データ, 送信済みバイト数))=再生中、None=停止中。
    // ループを止めずに毎周「書けるぶんだけ」書く非ブロッキング方式
    let mut playback: Option<(Vec<u8>, usize)> = None;
    // 録音バッファ。Some=録音中(モノラル16bit PCMを蓄積)、None=待機中。
    // プッシュ・トゥ・トーク: タッチしている間だけ録音し、離したら送信
    const MAX_RECORD_BYTES: usize = (SAMPLE_RATE_HZ as usize) * 2 * 15; // 上限15秒(安全弁)
    const MIN_RECORD_BYTES: usize = (SAMPLE_RATE_HZ as usize) * 2 / 2; // 0.5秒未満はキャンセル
    let mut recording: Option<Vec<u8>> = None;
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
            // 録音中(かつビープ再生が終わってから)はモノラルPCMとして蓄積
            if playback.is_none() {
                if let Some(pcm) = recording.as_mut() {
                    if pcm.len() < MAX_RECORD_BYTES {
                        pcm.extend_from_slice(&frame[..2]);
                    }
                }
            }
        }
        let rms = if count > 0 {
            ((sum_sq / count) as f32).sqrt()
        } else {
            0.0
        };

        // --- 録音ホールド中のみ: 下部ステータスバーを横型音量ゲージとして使う ---
        // VUメーター風の平滑化(上がりは即・下がりは1周8pxずつ)+差分描画でチラつき防止
        if recording.is_some() {
            let db = 20.0 * (rms.max(1.0) / 32768.0).log10(); // -90dB〜0dB
            let target_w = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 320.0) as u32;
            let gauge_w = if target_w > prev_gauge_width {
                target_w
            } else {
                prev_gauge_width.saturating_sub(8).max(target_w)
            };
            if gauge_w > prev_gauge_width {
                // 伸びた分だけ緑を足す
                Rectangle::new(
                    Point::new(prev_gauge_width as i32, STATUS_Y),
                    Size::new(gauge_w - prev_gauge_width, STATUS_H),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::GREEN))
                .draw(&mut display)
                .expect("ゲージの描画に失敗");
            } else if gauge_w < prev_gauge_width {
                // 縮んだ分だけ黒で消す
                Rectangle::new(
                    Point::new(gauge_w as i32, STATUS_Y),
                    Size::new(prev_gauge_width - gauge_w, STATUS_H),
                )
                .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
                .draw(&mut display)
                .expect("ゲージの描画に失敗");
            }
            prev_gauge_width = gauge_w;
        }

        // --- 顔アニメーション(待機=まばたき / 話し中=口パク) ---
        // スプライト無効時はアニメーション不要(文字は静止)
        match if FACE_SPRITES_ENABLED { face } else { FaceState::Thinking } {
            FaceState::Idle => {
                if face_frame == 0 && last_anim.elapsed().as_millis() > 3500 {
                    face_frame = 1; // まばたき(目を閉じる)
                    draw_face(&mut display, character, face, face_frame);
                    last_anim = std::time::Instant::now();
                } else if face_frame != 0 && last_anim.elapsed().as_millis() > 150 {
                    face_frame = 0;
                    draw_face(&mut display, character, face, face_frame);
                    last_anim = std::time::Instant::now();
                }
            }
            FaceState::Speaking => {
                if last_anim.elapsed().as_millis() > 250 {
                    face_frame = (face_frame + 1) % 3; // 口パクループ
                    draw_face(&mut display, character, face, face_frame);
                    last_anim = std::time::Instant::now();
                }
            }
            _ => {} // 聞き耳・考え中は静止
        }

        // --- タッチ処理: タブ切替 / 顔エリア=プッシュ・トゥ・トーク / 新規ボタン ---
        let mut buf = [0u8; 5];
        i2c.write_read(FT6336_ADDR, &[0x02], &mut buf, BLOCK)
            .expect("FT6336の読み取りに失敗");
        let touched_now = (buf[0] & 0x0F) > 0;
        let tx = (((buf[1] & 0x0F) as i32) << 8) | buf[2] as i32;
        let ty = (((buf[3] & 0x0F) as i32) << 8) | buf[4] as i32;

        // 押した瞬間の処理(応答再生の割り込みはtalk()内で処理される)
        if touched_now && !was_touched && recording.is_none() && playback.is_none() {
            if ty < TAB_H as i32 {
                // モードタブ
                let selected = Mode::ALL[(tx / 107).clamp(0, 2) as usize];
                if selected != mode {
                    mode = selected;
                    log::info!("モード切替: {}", mode.header_value());
                    draw_tabs(&mut display, &jp_font, mode);
                    let msg = format!("Mode: {}", mode.header_value());
                    draw_status(&mut display, text_style, &jp_font, &msg, Rgb565::BLUE);
                }
            } else if ty >= STATUS_Y && tx >= NEW_BTN_X {
                // 新規セッションボタン: 次の発話から履歴を仕切り直す
                new_session = true;
                log::info!("新規セッション予約");
                draw_status(&mut display, text_style, &jp_font, "New session: next talk", Rgb565::BLUE);
            } else if ty >= STATUS_Y && tx < CHAR_BTN_W {
                // 「顔」ボタン: キャラ切替(チャイム+NVS保存)
                character = (character + 1) % CHAR_COUNT;
                if let Err(e) = app_nvs.set_u8("chara", character as u8) {
                    log::warn!("キャラ選択のNVS保存に失敗: {e}");
                }
                log::info!("キャラ切替: {character}");
                playback = Some((chime.clone(), 0));
                draw_face(&mut display, character, face, face_frame);
                draw_status(&mut display, text_style, &jp_font, "Character switched!", Rgb565::BLUE);
            } else if ty >= FACE_Y && ty < STATUS_Y {
                // 顔エリア: プッシュ・トゥ・トーク開始
                log::info!("タッチ検出 → 録音開始(離すまで最大15秒)");
                playback = Some((beep.clone(), 0)); // 実際の送信はループ末尾で小分けに行う
                recording = Some(Vec::with_capacity(MAX_RECORD_BYTES));
                // ゲージ領域を一度だけ黒でクリア(以降は差分描画)
                Rectangle::new(Point::new(0, STATUS_Y), Size::new(320, STATUS_H))
                    .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
                    .draw(&mut display)
                    .expect("ゲージ背景の描画に失敗");
                prev_gauge_width = 0;
                face = FaceState::Listening;
                face_frame = 0;
                last_anim = std::time::Instant::now();
                draw_face(&mut display, character, face, face_frame);
            }
        }

        // 離した瞬間 or 上限到達で録音終了。短すぎる場合はキャンセル
        let released = was_touched && !touched_now;
        let mut finished: Option<Vec<u8>> = None;
        if let Some(pcm) = recording.as_ref() {
            if pcm.len() >= MAX_RECORD_BYTES || (released && pcm.len() >= MIN_RECORD_BYTES) {
                finished = recording.take();
            } else if released {
                // 短いタップ(0.5秒未満)は録音キャンセル。400ms以内に2回=ダブルタップで
                // キャラ切替(短タップは未割り当て入力なのでプッシュ・トゥ・トークと衝突しない)
                recording = None;
                face = FaceState::Idle;
                face_frame = 0;
                last_anim = std::time::Instant::now();
                // 短いタップ(0.5秒未満)は録音キャンセル扱い
                // ※当初ダブルタップでのキャラ切替を試したが、「顔に触れる=録音開始」と
                //   ジェスチャーが本質的に衝突するため専用の「顔」ボタン方式に変更した
                log::info!("短タップ(録音キャンセル)");
                draw_face(&mut display, character, face, face_frame);
                draw_status(&mut display, text_style, &jp_font, "", Rgb565::BLUE);
            }
        }
        was_touched = touched_now;

        // --- 再生キューに残りがあれば続きを書く(タイムアウト0=書ける分だけ書いてすぐ戻る) ---
        // ループは録音読み(32ms)でペーシングされており、毎周32ms分以上の
        // 送信バッファ空きができるので、これで途切れず再生される
        if let Some((data, pos)) = playback.as_mut() {
            // タイムアウト0のwriteはDMAバッファに全く空きがないとESP_ERR_TIMEOUTを返すが、
            // これはエラーではなく「今回は書けなかった」の意味(次の周回で再試行すればよい)。
            // expectで落とすとタイミング次第でパニック再起動する(troubleshooting.md参照)
            match i2s.write(&data[*pos..], 0) {
                Ok(written) => *pos += written,
                Err(e) if e.code() == esp_idf_svc::sys::ESP_ERR_TIMEOUT => {}
                Err(e) => panic!("I2S書き込みに失敗: {e}"),
            }
            if *pos >= data.len() {
                playback = None;
            }
        }

        // --- 録音が確定したらWAV化して中継WorkerへPOST ---
        if let Some(pcm) = finished {
            log::info!("録音完了({}バイト)。送信開始: {}", pcm.len(), CONFIG.server_url);
            face = FaceState::Thinking;
            face_frame = 0;
                last_anim = std::time::Instant::now();
                draw_face(&mut display, character, face, face_frame);
            draw_status(&mut display, text_style, &jp_font, "Sending...", Rgb565::BLUE);

            // 話し中表示に切り替えてからストリーミング往復(受信しながら再生)
            face = FaceState::Speaking;
            face_frame = 0;
            last_anim = std::time::Instant::now();
            draw_face(&mut display, character, face, face_frame);

            let started = std::time::Instant::now();
            let result = talk(
                CONFIG.server_url,
                &pcm,
                SAMPLE_RATE_HZ,
                mode,
                new_session,
                character,
                &mut i2s,
                &mut i2c,
                &mut display,
                &mut http_conn,
            );
            let elapsed_ms = started.elapsed().as_millis();
            let msg = match result {
                Ok((200, interrupted)) => {
                    new_session = false; // 仕切り直しが伝わったのでフラグを下ろす
                    log::info!("往復完了 {elapsed_ms}ms (割り込み={interrupted})");
                    String::new() // 計測情報はシリアルログのみ(画面はすっきり保つ)
                }
                Ok((status, _)) => format!("Relay error: {status}"),
                Err(e) => {
                    log::error!("送信失敗: {e}");
                    format!("Send FAILED ({elapsed_ms}ms)")
                }
            };
            // 再生中にマイクバッファへ溜まった音(自分の声・スピーカー音)を捨てる。
            // 捨てないと次の録音の先頭に混入する
            loop {
                match i2s.read(&mut audio_buf, 0) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.code() == esp_idf_svc::sys::ESP_ERR_TIMEOUT => break,
                    Err(e) => panic!("I2S読み取りに失敗: {e}"),
                }
            }
            face = FaceState::Idle;
            face_frame = 0;
            last_anim = std::time::Instant::now();
            draw_face(&mut display, character, face, face_frame);
            draw_status(&mut display, text_style, &jp_font, &msg, Rgb565::BLUE);
        }
    }
}
