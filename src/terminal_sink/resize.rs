use bytemuck::Pod;
use rgb::Rgb;
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

    pub unsafe fn get_mut_unchecked(&mut self, i: u16, j: u16) -> &mut T {
        let (width, height) = self.size();
        unsafe {
            core::hint::assert_unchecked(i < width);
            core::hint::assert_unchecked(j < height);
            let idx = (j as usize)
                .unchecked_mul(width as usize)
                .unchecked_add(i as usize);
            self.cells.get_unchecked_mut(idx)
        }
    }

    pub const fn as_mut_slice(&mut self) -> &mut [T] {
        self.cells.as_mut_slice()
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
    pixels: &'a [Rgb<u8>],
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

    pub unsafe fn get_pixel_unchecked(&self, i: u32, j: u32) -> Rgb<u8> {
        unsafe {
            // Safety: up to called
            let i_usize = usize::try_from(i).unwrap_unchecked();
            let j_usize = usize::try_from(j).unwrap_unchecked();

            // this is always safe since we have a pixel buffer
            // of this size in memory
            let width = usize::try_from(self.size.0).unwrap_unchecked();
            *self
                .pixels
                .get_unchecked(j_usize.unchecked_mul(width).unchecked_add(i_usize))
        }
    }

    pub fn size(&self) -> (u32, u32) {
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

impl PodMatrix<Rgb<u8>> {
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
    image_buffer: PodMatrix<Rgb<u8>>,
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
            self.image_buffer.cells.fill(Rgb::new(0, 0, 0));
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

        let res = resizer.resize(image.pixels, self.image_buffer.cells.as_mut_slice());

        // this should never happen since its validated that all parameters are valid
        res.unwrap();

        self.image_buffer.as_image()
    }
}
