extern crate apng_encoder;
extern crate chrono;
extern crate clipboard;
extern crate ctrlc;
extern crate device_query;
extern crate docopt;
extern crate image;
extern crate livesplit_hotkey;
extern crate oxipng;
extern crate piston_window;
extern crate reqwest;
extern crate scrap;
#[macro_use]
extern crate lazy_static;

#[cfg(windows)]
extern crate kernel32;
#[cfg(windows)]
extern crate user32;
#[cfg(windows)]
extern crate winapi;

use apng_encoder::{Color, Delay, Encoder, Frame, Meta};
use clipboard::ClipboardContext;
use clipboard::ClipboardProvider;
use core::borrow::BorrowMut;
use device_query::{DeviceQuery, DeviceState, Keycode};
use image::png::PNGEncoder;
use image::{ColorType, ConvertBuffer, GenericImage, GenericImageView, RgbImage, RgbaImage};
use livesplit_hotkey::KeyCode;
use piston_window::*;
use scrap::{Capturer, Display};
use std::cell::RefCell;
use std::cmp::max;
use std::fs::File;
use std::io::stdout;
use std::io::ErrorKind::WouldBlock;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SELECTION_COLOUR: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const VERSION: &'static str = env!("CARGO_PKG_VERSION");
lazy_static! {
    static ref ONE_FRAME: Duration = Duration::new(1, 0) / 60;
    static ref DURATION_1MS: Duration = Duration::new(0, 1);
}

#[cfg(debug_assertions)]
const DEBUGGING: bool = true;
#[cfg(not(debug_assertions))]
const DEBUGGING: bool = false;

macro_rules! d {
    ($($arg:tt)*) => {
      if DEBUGGING {
        ($($arg)*);
      }
    };
}

#[cfg(windows)]
const PRINTSCREEN_KEYCODE: KeyCode = KeyCode::Snapshot;
#[cfg(not(windows))]
const PRINTSCREEN_KEYCODE: KeyCode = KeyCode::Print;

fn main() {
    let cli_args = docopt::Docopt::new(format!(
        "
NCScreenie {} - Screenshot Cropper & Uploader

Usage:
    ncscreenier [--watch] [--directory=<DIR>] [--account=<name>] [--quiet]
    ncscreenier [--no-watch] [--directory=<DIR>] [--account=<name>]
    ncscreenier [--help]

Options:
    -h --help         Show this screen.
    --account=<name>  Account to upload under [default: anon]
    --watch           Watch for printscreens (default)
    --no-watch        Disable watching for printscreen, just immediately capture once
    --directory=DIR   Output directory for screenshots [default: ./]
    --quiet           (Windows only) hide the cmd window
    ",
        VERSION
    ))
    .and_then(|dopt| dopt.parse())
    .unwrap_or_else(|e| e.exit());

    let directory = cli_args.get_str("--directory").to_string();
    let account = cli_args.get_str("--account").to_string();

    let mut ctx: ClipboardContext = ClipboardProvider::new().unwrap();
    let mut runtime = move || {
        if let Some(filename) = screenshot_and_save(&directory) {
            if let Some(url) = upload_to_nebtown(
                filename.as_str(),
                format!("{}{}", directory, filename).as_str(),
                account.as_str(),
                4,
            ) {
                ctx.set_contents(url).unwrap();
            }
        }
    };

    let printscreen_hook;
    if !cli_args.get_bool("--no-watch") {
        printscreen_hook = livesplit_hotkey::Hook::new().unwrap();
        printscreen_hook
            .register(PRINTSCREEN_KEYCODE, runtime)
            .unwrap();

        println!("ncscreenier listening for printscreen's...");

        if cli_args.get_bool("--quiet") {
            #[cfg(windows)]
            {
                let window = unsafe { kernel32::GetConsoleWindow() };
                // https://msdn.microsoft.com/en-us/library/windows/desktop/ms633548%28v=vs.85%29.aspx
                if window != std::ptr::null_mut() {
                    unsafe {
                        user32::ShowWindow(window, winapi::um::winuser::SW_HIDE);
                    }
                }
            }
        }

        sleep_until_exit();
        println!("Exiting...");
    } else {
        runtime();
    }
}

fn sleep_until_exit() {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");
    while running.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn screenshot_and_save(directory: &str) -> Option<String> {
    let mut screenshot = capture_screenshot();

    if let Some(rect) = present_for_cropping(&screenshot) {
        let filename = format!("{}.png", chrono::Local::now().format("%Y_%m_%d_%H-%M-%S"));
        let filepath = format!("{}{}", directory, filename);
        print!(
            "Saving crop {},{} -> {}, {} to {}...",
            rect.top_left.0, rect.top_left.1, rect.bottom_right.0, rect.bottom_right.1, filepath
        );
        let cropped_width = rect.bottom_right.0 - rect.top_left.0;
        let cropped_height = rect.bottom_right.1 - rect.top_left.1;
        if screenshot.additional_images.len() == 0 {
            let cropped_image: RgbImage = image::imageops::crop(
                screenshot.image.borrow_mut(),
                rect.top_left.0,
                rect.top_left.1,
                cropped_width,
                cropped_height,
            )
            .to_image()
            .convert();

            let mut png_buffer = Vec::new();
            let (width, height) = cropped_image.dimensions();
            PNGEncoder::new(png_buffer.by_ref())
                .encode(&cropped_image.into_raw(), width, height, ColorType::RGB(8))
                .expect("error encoding pixels as PNG");

            let mut oxipng_options = oxipng::Options::from_preset(2);
            oxipng_options.verbosity = None;
            let optimized_buffer = oxipng::optimize_from_memory(&png_buffer, &oxipng_options)
                .expect("error optimizing png");

            let mut file = File::create(&filepath).unwrap();
            file.write_all(&optimized_buffer)
                .expect("error writing png");
        } else {
            let mut file = File::create(&filepath).unwrap();
            let mut encoder = Encoder::create(
                &mut file,
                Meta {
                    color: Color::RGB(8),
                    frames: 1 + (screenshot.additional_images.len() as u32),
                    width: cropped_width,
                    height: cropped_height,
                    plays: None,
                },
            )
            .expect("failed to create apng encoder");

            let mut delays = screenshot.delays.into_iter();
            std::iter::once(screenshot.image)
                .chain(screenshot.additional_images.into_iter())
                .for_each(|mut frame_image| {
                    let cropped_frame: RgbImage = image::imageops::crop(
                        frame_image.borrow_mut(),
                        rect.top_left.0,
                        rect.top_left.1,
                        cropped_width,
                        cropped_height,
                    )
                    .to_image()
                    .convert();
                    encoder
                        .write_frame(
                            &cropped_frame.into_raw(),
                            Some(&Frame {
                                delay: Some(Delay {
                                    numerator: delays.next().unwrap(),
                                    denominator: 1000,
                                }),
                                ..Default::default()
                            }),
                            None,
                            None,
                        )
                        .unwrap();
                });
            encoder.finish().unwrap();
        }
        println!(" saved.");
        Some(filename)
    } else {
        println!("Closing screenshot due to right click");
        None
    }
}

fn upload_to_nebtown(
    filename: &str,
    filepath: &str,
    directory: &str,
    retries: u8,
) -> Option<String> {
    let url = format!("http://nebtown.info/ss/{}/{}", directory, filename);
    print!("Uploading to {} ...", url);
    stdout().flush().expect("error flushing stdout");
    let mut ctx: ClipboardContext = ClipboardProvider::new().unwrap();
    ctx.set_contents(format!("{}?", url)).unwrap();

    let form = reqwest::multipart::Form::new()
        .file("file", &filepath)
        .unwrap();
    let mut res = match reqwest::Client::new()
        .post(&format!(
            "http://nebtown.info/ss/?folder_name={}&file_name={}",
            directory, filename
        ))
        .multipart(form)
        .send()
    {
        Ok(success_response) => success_response,
        Err(e) => {
            println!(" upload error! {:?}", e);
            return if retries > 0 {
                std::thread::sleep(Duration::from_secs(max((5 - retries).into(), 1)));
                upload_to_nebtown(filename, filepath, directory, retries - 1)
            } else {
                println!("Upload failed, giving up :(");
                None
            };
        }
    };
    if res.status() == 200 {
        println!(" done!");
        Some(url)
    } else {
        println!(" error! {:?}, {:?}", res.status(), res.headers());
        println!("{:?}", res.text().unwrap_or("??".to_string()));
        None
    }
}

struct Rect {
    top_left: (u32, u32),
    bottom_right: (u32, u32),
}

fn present_for_cropping(screenshot: &PresentabeScreenshot) -> Option<Rect> {
    let mut start_pos: (f64, f64) = (0.0, 0.0);
    let mut last_pos: (f64, f64) = (0.0, 0.0);
    let mut is_mouse_down = false;

    let draw_width = screenshot.image.width();
    let draw_height = screenshot.image.height() - 1; // if we're perfectly matching on Windows, it'll become a 'fullscreen app' that takes seconds to load
    let mut window: PistonWindow = WindowSettings::new("NCScreenier", [draw_width, draw_height])
        .opengl(OpenGL::V3_2)
        .exit_on_esc(true)
        .decorated(false)
        .resizable(false)
        .fullscreen(false)
        .vsync(true)
        .build()
        .unwrap();
    window.set_position(piston_window::Position {
        x: screenshot.x,
        y: screenshot.y,
    });
    window.set_lazy(true);
    window.window.window.set_always_on_top(true);

    let screenshot_texture: G2dTexture = Texture::from_image(
        &mut window.factory,
        &screenshot.image,
        &TextureSettings::new(),
    )
    .unwrap();
    while let Some(e) = window.next() {
        let e: piston_window::Event = e;

        window.draw_2d(&e, |c, gl| {
            image(&screenshot_texture, c.transform, gl);
            if start_pos.0 < last_pos.0 && start_pos.1 < last_pos.1 {
                rectangle::Rectangle::new_border(SELECTION_COLOUR, 1.0).draw(
                    rectangle::rectangle_by_corners(
                        start_pos.0.into(),
                        start_pos.1.into(),
                        last_pos.0.into(),
                        last_pos.1.into(),
                    ),
                    &draw_state::DrawState::default(),
                    c.transform,
                    gl,
                );
            }
        });
        if let Some(Button::Mouse(MouseButton::Right)) = e.press_args() {
            return None;
        }
        if let Some(Button::Mouse(MouseButton::Left)) = e.press_args() {
            is_mouse_down = true;
        }
        if is_mouse_down {
            if start_pos == (0.0, 0.0) {
                e.mouse_cursor(|x, y| {
                    start_pos = (x, y);
                    d!(println!("start position {}, {}", x, y));
                });
            }
            if let Some(ending) = e.release(|button| {
                if button == Button::Mouse(MouseButton::Left) {
                    is_mouse_down = false;
                    if last_pos.0 > start_pos.0 && last_pos.1 > start_pos.1 {
                        return true;
                    } else {
                        start_pos = (0.0, 0.0);
                        last_pos = (0.0, 0.0);
                    }
                }
                false
            }) {
                if ending {
                    return Some(Rect {
                        top_left: (start_pos.0 as u32, start_pos.1 as u32),
                        bottom_right: (last_pos.0 as u32, last_pos.1 as u32),
                    });
                } else {
                    continue;
                }
            }
            e.mouse_cursor(|x, y| {
                last_pos = (x.max(0.0), y.max(0.0));
            });
        }
    }
    None
}

struct CapturerPosition {
    capturer: Capturer,
    top: i32,
    left: i32,
}

struct SubImage {
    image: Option<image::RgbaImage>,
    top: i32,
    left: i32,
    w: u32,
    h: u32,
}

struct PresentabeScreenshot {
    image: image::RgbaImage,
    additional_images: Vec<RgbaImage>,
    delays: Vec<u16>,
    x: i32,
    y: i32,
}

fn capture_screenshot() -> PresentabeScreenshot {
    let displays: Vec<Display> = Display::all().expect("Couldn't get displays.");
    let max_x = {
        let display = displays
            .iter()
            .max_by(|x, y| x.right().cmp(&y.right()))
            .unwrap();
        display.right()
    };
    let min_x = {
        let display = displays
            .iter()
            .min_by(|x, y| x.left().cmp(&y.left()))
            .unwrap();
        display.left()
    };
    let max_y = {
        let display = displays
            .iter()
            .max_by(|x, y| x.bottom().cmp(&y.bottom()))
            .unwrap();
        display.bottom()
    };
    let min_y = {
        let display = displays
            .iter()
            .min_by(|x, y| x.top().cmp(&y.top()))
            .unwrap();
        display.top()
    };
    d!(println!(
        "Capturing screenshot with dimensions: {},{} {},{}",
        min_x, min_y, max_x, max_y
    ));

    let capturers: Vec<RefCell<CapturerPosition>> = displays
        .into_iter()
        .map(|display| {
            RefCell::new(CapturerPosition {
                left: display.left(),
                top: display.top(),
                capturer: Capturer::new(display).expect("Couldn't begin capture"),
            })
        })
        .collect();
    let mut prev_frame_time = SystemTime::now();
    let big_image = capture_image(&capturers, min_x, min_y, max_x, max_y, None);

    let mut additional_images: Vec<RgbaImage> = Vec::new();
    let mut delays: Vec<u16> = vec![SystemTime::now()
        .duration_since(prev_frame_time)
        .unwrap()
        .as_millis() as u16];
    prev_frame_time = SystemTime::now();

    let device_state = DeviceState::new();
    while device_state
        .get_keys()
        .into_iter()
        .any(|key| key == Keycode::LShift || key == Keycode::RShift)
    {
        // std::thread::sleep_ms(50);
        d!(print_time("Before additional image"));
        additional_images.push(capture_image(
            &capturers,
            min_x,
            min_y,
            max_x,
            max_y,
            Some(additional_images.last().unwrap_or(&big_image)),
        ));
        delays.push(
            SystemTime::now()
                .duration_since(prev_frame_time)
                .unwrap()
                .as_millis() as u16,
        );
        prev_frame_time = SystemTime::now();
    }

    return PresentabeScreenshot {
        image: big_image,
        additional_images,
        delays,
        x: min_x,
        y: min_y,
    };
}

fn capture_image(
    capturers: &Vec<RefCell<CapturerPosition>>,
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
    base_image: Option<&RgbaImage>,
) -> RgbaImage {
    let mut big_image = image::RgbaImage::new((max_x - min_x) as u32, (max_y - min_y) as u32);
    d!(print_time("initialized image"));

    capturers
        .iter()
        .map(|capturer_position_cell| {
            let mut capturer_position = capturer_position_cell.borrow_mut();
            let w = capturer_position.capturer.width();
            let h = capturer_position.capturer.height();
            let mut frames_asleep = 0;
            loop {
                match capturer_position.capturer.frame() {
                    Ok(captured_buffer) => {
                        if !captured_buffer.to_vec().iter().any(|&x| x != 0) {
                            // sometimes it captures all black?? skip
                            d!(println!("black frame"));
                            thread::sleep(*DURATION_1MS);
                            continue;
                        }
                        return SubImage {
                            image: Some(scrap_buffer_to_rgbaimage(w, h, captured_buffer)),
                            top: capturer_position.top,
                            left: capturer_position.left,
                            w: w as u32,
                            h: h as u32,
                        };
                    }
                    Err(error) => {
                        if error.kind() == WouldBlock {
                            if frames_asleep > 20 && base_image.is_some() {
                                return SubImage {
                                    image: None,
                                    top: capturer_position.top,
                                    left: capturer_position.left,
                                    w: w as u32,
                                    h: h as u32,
                                };
                            }
                            // Wait until there's a frame.
                            d!(println!("would block {:?}", frames_asleep));
                            frames_asleep += 1;
                            //thread::sleep(*DURATION_1MS);
                            continue;
                        } else {
                            panic!("Error: {}", error);
                        }
                    }
                };
            }
        })
        .for_each(|subimage| {
            if subimage.image.is_none() {
                big_image.copy_from(
                    &(base_image.unwrap().view(
                        (subimage.left - min_x) as u32,
                        (subimage.top - min_y) as u32,
                        subimage.w,
                        subimage.h,
                    )),
                    (subimage.left - min_x) as u32,
                    (subimage.top - min_y) as u32,
                );
            } else {
                big_image.copy_from(
                    &subimage.image.unwrap(),
                    (subimage.left - min_x) as u32,
                    (subimage.top - min_y) as u32,
                );
            }
        });
    big_image
}

fn scrap_buffer_to_rgbaimage(w: usize, h: usize, buffer: scrap::Frame) -> image::RgbaImage {
    // Flip the ARGB image into a BGRA image.
    let mut bitflipped = Vec::with_capacity(w * h * 4);
    let stride = buffer.len() / h;
    for y in 0..h {
        for x in 0..w {
            let i = stride * y + 4 * x;
            bitflipped.extend_from_slice(&[buffer[i + 2], buffer[i + 1], buffer[i], 255]);
        }
    }
    image::RgbaImage::from_raw(w as u32, h as u32, bitflipped).unwrap()
}

fn print_time(s: &str) {
    println!(
        "{:<20}: {:?}",
        s,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
    );
}
