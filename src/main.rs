extern crate chrono;
extern crate clipboard;
extern crate ctrlc;
extern crate docopt;
extern crate image;
extern crate livesplit_hotkey;
extern crate piston_window;
extern crate repng;
extern crate reqwest;
extern crate scrap;

#[cfg(windows)]
extern crate kernel32;
#[cfg(windows)]
extern crate user32;
#[cfg(windows)]
extern crate winapi;

use clipboard::ClipboardContext;
use clipboard::ClipboardProvider;
use core::borrow::BorrowMut;
use image::GenericImage;
use livesplit_hotkey::KeyCode;
use piston_window::*;
use scrap::{Capturer, Display};
use std::fs::File;
use std::io::ErrorKind::WouldBlock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const SELECTION_COLOUR: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const VERSION: &'static str = env!("CARGO_PKG_VERSION");

#[cfg(debug_assertions)]
const DEBUGGING: bool = true;
#[cfg(not(debug_assertions))]
const DEBUGGING: bool = false;

#[cfg(windows)]
const PRINTSCREEN_KEYCODE: KeyCode = KeyCode::Snapshot;
#[cfg(not(windows))]
const PRINTSCREEN_KEYCODE: KeyCode = KeyCode::Print;

fn main() {
    let cli_args = docopt::Docopt::new(
        format!("
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
    ", VERSION),
    )
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
        let cropped_image = image::imageops::crop(
            screenshot.image.borrow_mut(),
            rect.top_left.0,
            rect.top_left.1,
            cropped_width,
            cropped_height,
        )
        .to_image();
        repng::encode(
            File::create(&filepath).unwrap(),
            cropped_width,
            cropped_height,
            &cropped_image.into_raw(),
        )
        .unwrap();
        println!(" saved.");
        Some(filename)
    } else {
        println!("Closing screenshot due to right click");
        None
    }
}

fn upload_to_nebtown(filename: &str, filepath: &str, directory: &str) -> Option<String> {
    let url = format!("http://nebtown.info/ss/{}/{}", directory, filename);
    print!("Uploading to {} ...", url);
    let form = reqwest::multipart::Form::new()
        .file("file", &filepath)
        .unwrap();
    let mut res: reqwest::Response = reqwest::Client::new()
        .post(&format!(
            "http://nebtown.info/ss/?folder_name={}&file_name={}",
            directory, filename
        ))
        .multipart(form)
        .send()
        .unwrap();
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
                    if DEBUGGING {
                        println!("start position {}, {}", x, y);
                    }
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
    image: image::RgbaImage,
    top: i32,
    left: i32,
}

struct PresentabeScreenshot {
    image: image::RgbaImage,
    x: i32,
    y: i32,
}

fn capture_screenshot() -> PresentabeScreenshot {
    let one_frame = Duration::new(1, 0) / 60;

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
    if DEBUGGING {
        println!(
            "Capturing screenshot with dimensions: {},{} {},{}",
            min_x, min_y, max_x, max_y
        );
    }

    let mut big_image = image::RgbaImage::new((max_x - min_x) as u32, (max_y - min_y) as u32);

    displays
        .into_iter()
        .map(|display| CapturerPosition {
            left: display.left(),
            top: display.top(),
            capturer: Capturer::new(display).expect("Couldn't begin capture"),
        })
        .map(|capturer_position| {
            let mut capturer = capturer_position.capturer;
            let w = capturer.width();
            let h = capturer.height();
            loop {
                // Wait until there's a frame.
                match capturer.frame() {
                    Ok(captured_buffer) => {
                        if !captured_buffer.to_vec().iter().any(|&x| x != 0) {
                            // sometimes it captures all black?? skip
                            thread::sleep(one_frame);
                            continue;
                        }
                        return SubImage {
                            image: scrap_buffer_to_rgbaimage(w, h, captured_buffer),
                            top: capturer_position.top,
                            left: capturer_position.left,
                        };
                    }
                    Err(error) => {
                        if error.kind() == WouldBlock {
                            // Keep spinning.
                            thread::sleep(one_frame);
                            continue;
                        } else {
                            panic!("Error: {}", error);
                        }
                    }
                };
            }
        })
        .for_each(|subimage| {
            big_image.copy_from(
                &subimage.image,
                (subimage.left - min_x) as u32,
                (subimage.top - min_y) as u32,
            );
        });
    return PresentabeScreenshot {
        image: big_image,
        x: min_x,
        y: min_y,
    };
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
