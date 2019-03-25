extern crate chrono;
extern crate image;
extern crate piston_window;
extern crate repng;
extern crate scrap;

use core::borrow::BorrowMut;
use piston_window::*;
use scrap::{Capturer, Display};
use std::cmp::{max, min};
use std::fs::File;
use std::io::ErrorKind::WouldBlock;
use std::process::exit;
use std::thread;
use std::time::Duration;

const DEBUGGING: bool = true;

fn main() {
    let mut screenshot = capture_screenshot();

    if let Some(rect) = present_for_cropping(&screenshot) {
        let filename = format!("{}.png", chrono::Local::now().format("%Y_%m_%d_%H-%M-%S"));
        print!(
            "Saving crop {},{} -> {}, {} to {}...",
            rect.top_left.0, rect.top_left.1, rect.bottom_right.0, rect.bottom_right.1, filename
        );
        let cropped_width = rect.bottom_right.0 - rect.top_left.0;
        let cropped_height = rect.bottom_right.1 - rect.top_left.1;
        let cropped_image = image::imageops::crop(
            screenshot.borrow_mut(),
            rect.top_left.0,
            rect.top_left.1,
            cropped_width,
            cropped_height,
        )
        .to_image();
        repng::encode(
            File::create(&filename).unwrap(),
            cropped_width,
            cropped_height,
            &cropped_image.into_raw(),
        )
        .unwrap();
        println!(" saved.");
    } else {
        println!("Exiting due to right click, toodles");
        exit(0);
    }
}

struct Rect {
    top_left: (u32, u32),
    bottom_right: (u32, u32),
}

fn present_for_cropping(screenshot: &image::RgbaImage) -> Option<Rect> {
    let mut start_pos: (u32, u32) = (0, 0);
    let mut last_pos: (u32, u32) = (0, 0);
    let mut is_mouse_down = false;

    let draw_width = screenshot.width();
    let draw_height = screenshot.height() - 1; // if we're perfectly matching on Windows, it'll become a 'fullscreen app' that takes seconds to load
    let mut window: PistonWindow = WindowSettings::new("NCScreenier", [draw_width, draw_height])
        .opengl(OpenGL::V3_2)
        .exit_on_esc(true)
        .decorated(false)
        .resizable(false)
        .fullscreen(false)
        .vsync(true)
        .build()
        .unwrap();
    window.set_lazy(true);

    let mut canvas = image::ImageBuffer::new(draw_width, draw_height);
    let mut canvas_texture: G2dTexture =
        Texture::from_image(&mut window.factory, &canvas, &TextureSettings::new()).unwrap();

    let screenshot_texture: G2dTexture =
        Texture::from_image(&mut window.factory, screenshot, &TextureSettings::new()).unwrap();
    while let Some(e) = window.next() {
        let e: piston_window::Event = e;

        window.draw_2d(&e, |c, g| {
            clear([1.0; 4], g);
            image(&screenshot_texture, c.transform, g);
            image(&canvas_texture, c.transform, g);
        });
        if let Some(Button::Mouse(MouseButton::Right)) = e.press_args() {
            return None;
        }
        if let Some(Button::Mouse(MouseButton::Left)) = e.press_args() {
            is_mouse_down = true;
        }
        if is_mouse_down {
            if start_pos == (0, 0) {
                e.mouse_cursor(|x, y| {
                    start_pos = (x as u32, y as u32);
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
                        start_pos = (0, 0);
                    }
                }
                false
            }) {
                if ending {
                    return Some(Rect {
                        top_left: start_pos,
                        bottom_right: last_pos,
                    });
                } else {
                    continue;
                }
            }
            e.mouse_cursor(|x, y| {
                let x = max(0, x as i32) as u32;
                let y = max(0, y as i32) as u32;
                let max_x = max(0, min(max(last_pos.0, x) + 1, draw_width));
                let max_y = max(0, min(max(last_pos.1, y) + 1, draw_height));
                for pixel_y in start_pos.1..max_y {
                    for pixel_x in start_pos.0..max_x {
                        if (pixel_x <= x && pixel_y <= y)
                            && (pixel_y == start_pos.1
                                || pixel_y == y
                                || pixel_x == start_pos.0
                                || pixel_x == x)
                        {
                            canvas.put_pixel(pixel_x, pixel_y, image::Rgba([0, 0, 255, 255]));
                        } else {
                            canvas.put_pixel(pixel_x, pixel_y, image::Rgba([0, 0, 0, 0]));
                        }
                    }
                }
                last_pos = (x, y);
                canvas_texture.update(&mut window.encoder, &canvas).unwrap();
            });
        }
    }
    None
}

fn capture_screenshot() -> image::RgbaImage {
    let one_frame = Duration::new(1, 0) / 60;
    let display = Display::primary().expect("Couldn't find primary display.");
    let w = display.width();
    let h = display.height();
    let mut capturer = Capturer::new(display).expect("Couldn't begin capture.");
    loop {
        // Wait until there's a frame.
        match capturer.frame() {
            Ok(captured_buffer) => {
                if !captured_buffer.to_vec().iter().any(|&x| x != 0) {
                    // sometimes it captures all black?? skip
                    thread::sleep(one_frame);
                    continue;
                }
                return scrap_buffer_to_rgbaimage(w, h, captured_buffer);
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
