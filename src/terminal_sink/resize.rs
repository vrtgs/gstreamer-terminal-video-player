use image::{EncodableLayout, Rgb};

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

pub struct ResizeBuffer {
    size: (u16, u16),
    pixels: Vec<U8x3>,
}

impl ResizeBuffer {
    pub const fn new() -> Self {
        const {
            ResizeBuffer {
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

    pub const fn width(&self) -> u16 {
        self.size.0
    }

    pub const fn height(&self) -> u16 {
        self.size.1
    }
}
