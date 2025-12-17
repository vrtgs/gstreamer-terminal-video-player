use crate::terminal_sink::cursor_goto;
use crate::terminal_sink::resize::{ImageRef, PodMatrix};
use rgb::{ComponentMap, Rgb};
use std::io::Write;

#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct Cell {
    rgb_top: Rgb<u8>,
    rgb_bottom: Rgb<u8>,
}

impl Cell {
    pub fn draw(self, command_buffer: &mut Vec<u8>) {
        const UNICODE_TOP_HALF_BLOCK: &str = "\u{2580}";

        let Rgb {
            r: tr,
            g: tg,
            b: tb,
        } = self.rgb_top;
        let Rgb {
            r: br,
            g: bg,
            b: bb,
        } = self.rgb_bottom;

        let cell = ansi_term::Color::RGB(tr, tg, tb)
            .on(ansi_term::Colour::RGB(br, bg, bb))
            .paint(UNICODE_TOP_HALF_BLOCK);
        write!(command_buffer, "{cell}").unwrap();
    }
}

pub struct RenderedFrame {
    frame: PodMatrix<Cell>,
}

impl RenderedFrame {
    pub fn new() -> Self {
        Self {
            frame: PodMatrix::new(),
        }
    }

    pub fn render(
        &mut self,
        image_ref: ImageRef,
        overwrite: bool,
        offset: (u16, u16),
        command_buffer: &mut Vec<u8>,
    ) {
        unsafe fn get_pixel(image_ref: ImageRef, i: u32, j: u32) -> Rgb<u8> {
            let rgb = unsafe { image_ref.get_pixel_unchecked(i, j) };

            // quantize to only N bit color
            const N: u8 = 5;
            const MASK: u8 = {
                assert!(N <= 8);
                u8::MAX << (8 - N)
            };

            rgb.map(|x| x & MASK)
        }

        let (width, height) = image_ref.size();
        let terminal_size = (
            u16::try_from(width).unwrap(),
            u16::try_from(height.div_ceil(2)).unwrap(),
        );

        let (offset_width, offset_height) = offset;
        let (terminal_width, terminal_height) = terminal_size;

        let overwrite = overwrite || terminal_size != self.frame.size();
        if terminal_size != self.frame.size() {
            self.frame.resize(terminal_size);
        }

        if overwrite {
            command_buffer.extend_from_slice(termion::clear::All.as_ref());
        }

        let write_move = move |command_buffer: &mut Vec<u8>, i: u16, j: u16| {
            write!(
                command_buffer,
                "{}",
                cursor_goto(offset_width + i, offset_height + j)
            )
            .unwrap();
        };

        if overwrite {
            for j in 0..height {
                for i in 0..width {
                    let rgb = unsafe { get_pixel(image_ref, i, j) };
                    let pixel = self.frame.get_mut(i as u16, (j / 2) as u16).unwrap();
                    match j & 1 {
                        0 => pixel.rgb_top = rgb,
                        _ => pixel.rgb_bottom = rgb,
                    }
                }
            }

            if (height % 2) != 0 {
                let last_row =
                    &mut self.frame.as_mut_slice()[width as usize * (height / 2) as usize..];
                for pixel in last_row {
                    pixel.rgb_bottom = Rgb::new(0, 0, 0)
                }
            }

            for j in 0..terminal_height {
                write_move(command_buffer, 0, j);
                for i in 0..terminal_width {
                    self.frame.get_mut(i, j).unwrap().draw(command_buffer)
                }
            }
            return;
        }

        for j in 0..(height / 2) {
            let mut last_changed = false;
            'next_pixel: for i in 0..width {
                let rgb_t = unsafe { get_pixel(image_ref, i, j * 2) };
                let rgb_b = unsafe { get_pixel(image_ref, i, j * 2 + 1) };
                let (i, j) = (i as u16, j as u16);
                let pixel = self.frame.get_mut(i, j).unwrap();
                if pixel.rgb_top != rgb_t || pixel.rgb_bottom != rgb_b {
                    if !last_changed {
                        last_changed = true;
                        write_move(command_buffer, i, j);
                    }
                    pixel.rgb_top = rgb_t;
                    pixel.rgb_bottom = rgb_b;
                    (*pixel).draw(command_buffer);
                    continue 'next_pixel;
                }
                last_changed = false;
            }
        }

        if (height % 2) != 0 {
            let j = height / 2;
            let mut last_changed = false;
            'next_pixel: for i in 0..width {
                let rgb_t = unsafe { get_pixel(image_ref, i, j * 2) };
                let (i, j) = (i as u16, j as u16);
                let pixel = self.frame.get_mut(i, j).unwrap();
                if pixel.rgb_top != rgb_t {
                    if !last_changed {
                        last_changed = true;
                        write_move(command_buffer, i, j);
                    }
                    pixel.rgb_top = rgb_t;
                    (*pixel).draw(command_buffer);
                    continue 'next_pixel;
                }
                last_changed = false;
            }
        }
    }
}
