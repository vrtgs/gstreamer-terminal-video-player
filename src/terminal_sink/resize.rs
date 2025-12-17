use crate::terminal_sink::cursor_goto;
use bytemuck::Pod;
use std::io::Write;
use std::num::NonZero;

pub struct PodMatrix<T: Pod> {
    size: (u16, u16),
    cells: Vec<T>,
}

impl<T: Pod> PodMatrix<T> {
    pub fn new() -> Self {
        const {
            Self {
                size: (0, 0),
                cells: vec![],
            }
        }
    }

    pub fn resize(&mut self, size: (u16, u16)) {
        let new_length = usize::from(size.0)
            .checked_mul(size.1.into())
            .expect("out of memory");

        let current_len = self.cells.len();
        match new_length.checked_sub(current_len) {
            Some(0) => {}

            None => self.cells.truncate(new_length),
            Some(additional) => {
                self.cells.reserve(additional);
                unsafe {
                    let reserved_pixels = self.cells.spare_capacity_mut();
                    core::hint::assert_unchecked(reserved_pixels.len() >= additional);
                    core::ptr::write_bytes(reserved_pixels.as_mut_ptr(), 0, reserved_pixels.len());
                    self.cells.set_len(new_length);
                }
            }
        }

        self.size = size;
    }

    pub fn get_mut(&mut self, i: u16, j: u16) -> Option<&mut T> {
        let (width, height) = self.size();
        if i < width && j < height {
            return Some(unsafe { self.get_mut_unchecked(i, j) });
        }

        None
    }

    pub unsafe fn get_mut_unchecked(&mut self, i: u16, j: u16) -> &mut T {
        let (width, height) = self.size();
        unsafe {
            core::hint::assert_unchecked(i < width);
            core::hint::assert_unchecked(j < height);
            let idx = (j as usize)
                .unchecked_mul(width as usize)
                .unchecked_add(i as usize);
            self.cells.as_mut_slice().get_unchecked_mut(idx)
        }
    }

    pub const fn width(&self) -> u16 {
        self.size.0
    }

    pub const fn height(&self) -> u16 {
        self.size.1
    }

    pub const fn size(&self) -> (u16, u16) {
        self.size
    }
}

#[derive(Copy, Clone)]
pub struct ImageRef<'a> {
    size: (u32, u32),
    pixels: &'a [U8x3],
}

impl<'a> ImageRef<'a> {
    pub fn empty() -> ImageRef<'a> {
        ImageRef {
            size: (0, 0),
            pixels: &[],
        }
    }

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

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn as_non_zero_size(&self) -> Option<(NonZero<u32>, NonZero<u32>)> {
        if self.pixels.is_empty() {
            return None;
        }

        unsafe {
            Some((
                NonZero::new_unchecked(self.size.0),
                NonZero::new_unchecked(self.size.1),
            ))
        }
    }
}

type U8x3 = [u8; 3];

impl PodMatrix<U8x3> {
    pub fn as_image(&self) -> ImageRef<'_> {
        ImageRef {
            size: (self.width().into(), self.height().into()),
            pixels: self.cells.as_slice(),
        }
    }
}

type ResizerInner = resize::Resizer<resize::formats::Rgb<u8, u8>>;

fn make_inner_resizer(
    (src_width, src_height): (NonZero<usize>, NonZero<usize>),
    (dst_width, dst_height): (NonZero<u16>, NonZero<u16>),
) -> ResizerInner {
    let to_size = |x: NonZero<u16>| usize::from(x.get());
    let resizer = resize::new(
        src_width.get(),
        src_height.get(),
        to_size(dst_width),
        to_size(dst_height),
        resize::Pixel::RGB8,
        resize::Type::Triangle,
    );

    // the width and height are both non zero
    // and if we OOM we kinda need to kill the process now
    resizer.unwrap()
}

struct ResizingBuffer {
    last_src_dimentions: (NonZero<usize>, NonZero<usize>),
    resizer: ResizerInner,
}

pub struct Resizer {
    image_buffer: PodMatrix<U8x3>,
    resizing_buffer: Option<ResizingBuffer>,
}

impl Resizer {
    pub fn new() -> Self {
        Self {
            image_buffer: PodMatrix::new(),
            resizing_buffer: None,
        }
    }

    pub fn resize<'a>(&'a mut self, image: ImageRef<'a>, resize_to: (u16, u16)) -> ImageRef<'a> {
        if image.size == (resize_to.0.into(), resize_to.1.into()) {
            return image;
        }

        let dst_size_changed = resize_to != self.image_buffer.size();
        if dst_size_changed {
            self.image_buffer.resize(resize_to);
        }

        let Some((src_width, src_height)) = image.as_non_zero_size() else {
            self.image_buffer.cells.fill([0; 3]);
            return self.image_buffer.as_image();
        };

        let resize_to = (NonZero::new(resize_to.0), NonZero::new(resize_to.1));
        let (Some(dst_width), Some(dst_height)) = resize_to else {
            return ImageRef::empty();
        };

        let (Ok(src_width), Ok(src_height)) = (src_width.try_into(), src_height.try_into()) else {
            // if the image has dimentions that dont fit in a usize
            // then it can't fit in memory
            unreachable!()
        };

        let dst_dimentions = (dst_width, dst_height);
        let src_dimentions = (src_width, src_height);

        let resizer = match self.resizing_buffer {
            Some(ref mut buffer) => {
                let buffer_changed =
                    buffer.last_src_dimentions != src_dimentions || dst_size_changed;

                if buffer_changed {
                    buffer.last_src_dimentions = src_dimentions;
                    buffer.resizer = make_inner_resizer(src_dimentions, dst_dimentions);
                }
                &mut buffer.resizer
            }
            None => {
                let buff = self.resizing_buffer.insert(ResizingBuffer {
                    last_src_dimentions: src_dimentions,
                    resizer: make_inner_resizer(src_dimentions, dst_dimentions),
                });
                &mut buff.resizer
            }
        };

        let res = resizer.resize(
            bytemuck::must_cast_slice(image.pixels),
            bytemuck::must_cast_slice_mut(self.image_buffer.cells.as_mut_slice()),
        );

        // this should never happen since its validated that all parameters are valid
        res.unwrap();

        self.image_buffer.as_image()
    }
}

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
        let get_pixel = move |i, j| {
            let width = image_ref.size.0;
            let rgb = image_ref.pixels[j as usize * width as usize + i as usize];

            // quantize to only N bit color
            const N: u8 = 5;
            const MASK: u8 = {
                assert!(N <= 8);
                u8::MAX << (8 - N)
            };

            rgb.map(|x| x & MASK)
        };

        let (width, height) = image_ref.size();
        let terminal_size = (
            u16::try_from(width).unwrap(),
            u16::try_from(height.div_ceil(2)).unwrap(),
        );

        let (offset_width, offset_height) = offset;
        let (terminal_width, terminal_height) = terminal_size;

        let overwrite = overwrite || terminal_size != self.frame.size;
        if terminal_size != self.frame.size {
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
                    let rgb = get_pixel(i, j);
                    let pixel = self.frame.get_mut(i as u16, (j / 2) as u16).unwrap();
                    match j & 1 {
                        0 => pixel.rgb_top = rgb,
                        _ => pixel.rgb_bottom = rgb,
                    }
                }
            }

            if (height % 2) != 0 {
                for pixel in &mut self.frame.cells[width as usize * (height / 2) as usize..] {
                    pixel.rgb_bottom = [0; 3]
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
                let rgb_t = get_pixel(i, j * 2);
                let rgb_b = get_pixel(i, j * 2 + 1);
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
                let rgb_t = get_pixel(i, j * 2);
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
