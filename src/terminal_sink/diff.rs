use crate::terminal_sink::resize::{ImageRef, PodMatrix};
use rgb::{ComponentMap, Rgb};
use std::mem::MaybeUninit;
use std::num::NonZero;

#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct Cell {
    rgb_top: Rgb<u8>,
    rgb_bottom: Rgb<u8>,
}

// use a lut since this is super hot
#[inline(always)]
fn write_u8_ascii(buf: &mut Vec<u8>, n: u8) {
    #[derive(Copy, Clone)]
    #[repr(u8)]
    enum Digit {
        _0 = b'0',
        _1 = b'1',
        _2 = b'2',
        _3 = b'3',
        _4 = b'4',
        _5 = b'5',
        _6 = b'6',
        _7 = b'7',
        _8 = b'8',
        _9 = b'9',
    }

    #[derive(Copy, Clone)]
    #[repr(align(4))]
    struct LutEntry {
        len: NonZero<u8>,
        str: [Digit; 3],
    }

    const _: () = assert!(size_of::<LutEntry>() == 4);

    static LUT: [LutEntry; 256] = {
        let mut entries = [MaybeUninit::uninit(); 256];

        const fn to_digit(i: usize) -> Digit {
            assert!(i <= 9);
            unsafe { core::mem::transmute(Digit::_0 as u8 + i as u8) }
        }

        let mut i = 0_usize;
        while i < 256 {
            let (len, str) = match i {
                100.. => {
                    let first = i / 100;
                    let i = i % 100;
                    let second = i / 10;
                    let third = i % 10;
                    (3, [first, second, third])
                }
                10.. => {
                    let first = i / 10;
                    let second = i % 10;
                    (2, [first, second, 0])
                }
                0.. => (1, [i, 0, 0]),
            };
            let len = NonZero::new(len).unwrap();
            let str = [to_digit(str[0]), to_digit(str[1]), to_digit(str[2])];

            entries[i] = MaybeUninit::new(LutEntry { len, str });
            i += 1;
        }

        let invalid = LutEntry {
            len: NonZero::<u8>::new(3).unwrap(),
            str: [Digit::_0; 3],
        };
        let mut entries_init = [invalid; 256];

        let mut i = 0;
        while i < 256 {
            entries_init[i] = unsafe { entries[i].assume_init() };
            i += 1;
        }

        entries_init
    };

    let entry = &LUT[n as usize];
    let len = usize::from(entry.len.get());
    let str = unsafe {
        // Digit is repr(u8) and len is at most 3
        core::slice::from_raw_parts((&raw const entry.str).cast::<u8>(), len)
    };

    buf.extend_from_slice(str)
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

        // Foreground
        command_buffer.extend_from_slice(b"\x1b[38;2;");
        write_u8_ascii(command_buffer, tr);
        command_buffer.push(b';');
        write_u8_ascii(command_buffer, tg);
        command_buffer.push(b';');
        write_u8_ascii(command_buffer, tb);
        command_buffer.push(b'm');

        // Background RGB
        command_buffer.extend_from_slice(b"\x1b[48;2;");
        write_u8_ascii(command_buffer, br);
        command_buffer.push(b';');
        write_u8_ascii(command_buffer, bg);
        command_buffer.push(b';');
        write_u8_ascii(command_buffer, bb);
        command_buffer.push(b'm');
        command_buffer.extend_from_slice(UNICODE_TOP_HALF_BLOCK.as_bytes());
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

    fn render_inner(
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

        let mut int_buffer = itoa::Buffer::new();
        let mut write_move = move |command_buffer: &mut Vec<u8>, i: u16, j: u16| {
            // goto is one based
            let (x, y) = (
                (offset_width + i).saturating_add(1),
                (offset_height + j).saturating_add(1),
            );

            command_buffer.extend_from_slice(b"\x1b[");
            command_buffer.extend_from_slice(int_buffer.format(y).as_bytes());
            command_buffer.push(b';');
            command_buffer.extend_from_slice(int_buffer.format(x).as_bytes());
            command_buffer.push(b'H');
        };

        if overwrite {
            for j in 0..height {
                for i in 0..width {
                    let rgb = unsafe { get_pixel(image_ref, i, j) };
                    let pixel = unsafe { self.frame.get_mut_unchecked(i as u16, (j / 2) as u16) };
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
                    unsafe { self.frame.get_mut_unchecked(i, j) }.draw(command_buffer)
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
                let pixel = unsafe { self.frame.get_mut_unchecked(i, j) };
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
                let pixel = unsafe { self.frame.get_mut_unchecked(i, j) };
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

    pub fn render(
        &mut self,
        image_ref: ImageRef,
        overwrite: bool,
        offset: (u16, u16),
        command_buffer: &mut Vec<u8>,
    ) {
        Self::render_inner(self, image_ref, overwrite, offset, command_buffer);
        // Reset cursor for drawing
        command_buffer.extend_from_slice(b"\x1b[0m");
    }
}
