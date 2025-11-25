use crate::terminal_sink::cursor_goto;
use bytemuck::Pod;
use image::{EncodableLayout, GenericImageView, Rgb};
use std::io::Write;

pub struct PodMatrix<T: Pod> {
    size: (u16, u16),
    pixels: Vec<T>,
}

impl<T: Pod> PodMatrix<T> {
    pub fn new() -> Self {
        const {
            Self {
                size: (0, 0),
                pixels: vec![],
            }
        }
    }

    pub fn resize(&mut self, size: (u16, u16)) {
        let new_length = usize::from(size.0)
            .checked_mul(size.1.into())
            .expect("out of memory");

        let current_len = self.pixels.len();
        match new_length.checked_sub(current_len) {
            Some(0) => {}

            None => self.pixels.truncate(new_length),
            Some(additional) => {
                self.pixels.reserve(additional);
                unsafe {
                    let reserved_pixels = self.pixels.spare_capacity_mut();
                    core::hint::assert_unchecked(reserved_pixels.len() >= additional);
                    core::ptr::write_bytes(reserved_pixels.as_mut_ptr(), 0, reserved_pixels.len());
                    self.pixels.set_len(new_length);
                }
            }
        }

        self.size = size;
    }

    pub fn get_mut(&mut self, i: u16, j: u16) -> Option<&mut T> {
        let (width, height) = self.size;
        if i < width && j < height {
            return Some(unsafe { self.get_mut_unchecked(i, j) });
        }

        None
    }

    pub unsafe fn get_mut_unchecked(&mut self, i: u16, j: u16) -> &mut T {
        let (width, height) = self.size;
        unsafe {
            core::hint::assert_unchecked(i < width);
            core::hint::assert_unchecked(j < height);
            let idx = (j as usize)
                .unchecked_mul(width as usize)
                .unchecked_add(i as usize);
            self.pixels.as_mut_slice().get_unchecked_mut(idx)
        }
    }

    pub const fn width(&self) -> u16 {
        self.size.0
    }

    pub const fn height(&self) -> u16 {
        self.size.1
    }
}

pub struct Resizer {
    _private: (),
}

impl Resizer {
    pub fn new() -> Self {
        Self { _private: () }
    }

    pub fn resize<'a>(
        &mut self,
        image: ImageRef<'_>,
        resize_buffer: &'a mut ResizeBuffer,
    ) -> ImageRef<'a> {
        let dest_width = resize_buffer.width();
        let dest_height = resize_buffer.height();
        let image = image.as_image_crate_buffer();

        let resized = image::imageops::thumbnail(&image, dest_width.into(), dest_height.into());

        resize_buffer
            .pixels
            .copy_from_slice(bytemuck::cast_slice(resized.as_bytes()));

        ImageRef {
            size: (resize_buffer.size.0.into(), resize_buffer.size.1.into()),
            pixels: &resize_buffer.pixels,
        }
    }
}

#[derive(Copy, Clone)]
pub struct ImageRef<'a> {
    size: (u32, u32),
    pixels: &'a [U8x3],
}

impl<'a> ImageRef<'a> {
    pub fn from_buffer(width: u32, height: u32, buffer: &'a [u8]) -> Option<Self> {
        let pixels = bytemuck::try_cast_slice(buffer).ok()?;

        let expected_len = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(usize::try_from(height).ok()?));

        if !expected_len.is_some_and(|expected| expected == pixels.len()) {
            return None;
        }

        Some(Self {
            size: (width, height),
            pixels,
        })
    }

    pub fn as_image_crate_buffer(self) -> image::ImageBuffer<Rgb<u8>, &'a [u8]> {
        image::ImageBuffer::from_raw(
            self.size.0,
            self.size.1,
            bytemuck::must_cast_slice(self.pixels),
        )
        .unwrap()
    }
}

type U8x3 = [u8; 3];

pub type ResizeBuffer = PodMatrix<U8x3>;

#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct Cell {
    rgb_top: U8x3,
    rgb_bottom: U8x3,
}

impl Cell {
    pub fn draw(self, command_buffer: &mut Vec<u8>) {
        const UNICODE_TOP_HALF_BLOCK: &str = "\u{2580}";

        let [tr, tg, tb] = self.rgb_top;
        let [br, bg, bb] = self.rgb_bottom;
        ansi_term::Color::RGB(tr, tg, tb)
            .on(ansi_term::Colour::RGB(br, bg, bb))
            .paint(UNICODE_TOP_HALF_BLOCK.as_bytes())
            .write_to(command_buffer)
            .unwrap();
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

    pub fn render<I: GenericImageView<Pixel = Rgb<u8>>>(
        &mut self,
        image: I,
        overwrite: bool,
        offset: (u16, u16),
        command_buffer: &mut Vec<u8>,
    ) {
        let (width, height) = image.dimensions();
        let terminal_size = (
            u16::try_from(width).unwrap(),
            u16::try_from(height.div_ceil(2)).unwrap(),
        );

        let (offset_width, offset_height) = offset;
        let (terminal_width, terminal_height) = terminal_size;

        let overwrite = overwrite || terminal_size != self.frame.size;
        if terminal_size != self.frame.size {
            self.frame.resize(terminal_size)
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

        let overwrite_and_render =
            move |this: &mut Self, command_buffer: &mut Vec<u8>, image: I| {
                for j in 0..height {
                    for i in 0..width {
                        let rgb = image.get_pixel(i, j).0;
                        let pixel = this.frame.get_mut(i as u16, (j / 2) as u16).unwrap();
                        match j & 1 {
                            0 => pixel.rgb_top = rgb,
                            _ => pixel.rgb_bottom = rgb,
                        }
                    }
                }

                for j in 0..terminal_height {
                    write_move(command_buffer, 0, j);
                    for i in 0..terminal_width {
                        this.frame.get_mut(i, j).unwrap().draw(command_buffer)
                    }
                }
            };

        if overwrite {
            if (height % 2) != 0 {
                for pixel in &mut self.frame.pixels[width as usize * (height / 2) as usize..] {
                    pixel.rgb_bottom = [0; 3]
                }
            }

            overwrite_and_render(self, command_buffer, image);
            return;
        }

        fn within_delta(a: [u8; 3], b: [u8; 3]) -> bool {
            let [d0, d1, d2] = core::array::from_fn(|i| a[i].abs_diff(b[i]));
            d0.max(d1).max(d2) <= 6
        }

        for j in 0..height / 2 {
            let mut last_changed = false;
            'next_pixel: for i in 0..width {
                let rgb_t = image.get_pixel(i, j * 2).0;
                let rgb_b = image.get_pixel(i, j * 2 + 1).0;
                let (i, j) = (i as u16, j as u16);
                let pixel = self.frame.get_mut(i, j).unwrap();
                if !within_delta(pixel.rgb_top, rgb_t) || !within_delta(pixel.rgb_bottom, rgb_b) {
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
            let mut last_changed = false;
            let j = height - 1;
            'next_pixel: for i in 0..width {
                let rgb_t = image.get_pixel(i, j).0;
                let pixel = self
                    .frame
                    .get_mut(i as u16, height.div_ceil(2) as u16)
                    .unwrap();
                if !within_delta(pixel.rgb_top, rgb_t) {
                    if !last_changed {
                        last_changed = true;
                        write_move(command_buffer, i as u16, j as u16);
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
