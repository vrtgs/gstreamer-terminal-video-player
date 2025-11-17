// credit: https://github.com/image-rs/image/blob/a7af546c292c0933986772adeba077e56947181e/src/math/utils.rs

use std::cmp::max;

/// Calculates the width and height an image should be resized to.
/// This preserves aspect ratio, and based on the `fill` parameter
/// will either fill the dimensions to fit inside the smaller constraint
/// (will overflow the specified bounds on one axis to preserve
/// aspect ratio), or will shrink so that both dimensions are
/// completely contained within the given `width` and `height`,
/// with empty space on one axis.
pub(crate) fn resize_dimensions<const FILL: bool>(
    width: u32,
    height: u32,
    new_width: u32,
    new_height: u32,
) -> (u32, u32) {
    let w_ratio = f64::from(new_width) / f64::from(width);
    let h_ratio = f64::from(new_height) / f64::from(height);

    let ratio = match FILL {
        true => f64::max(w_ratio, h_ratio),
        false => f64::min(w_ratio, h_ratio),
    };

    let new_w = max((f64::from(width) * ratio).round() as u64, 1);
    let new_h = max((f64::from(height) * ratio).round() as u64, 1);

    if new_w > u64::from(u32::MAX) {
        let ratio = f64::from(u32::MAX) / f64::from(width);
        (u32::MAX, max((f64::from(height) * ratio).round() as u32, 1))
    } else if new_h > u64::from(u32::MAX) {
        let ratio = f64::from(u32::MAX) / f64::from(height);
        (max((f64::from(width) * ratio).round() as u32, 1), u32::MAX)
    } else {
        (new_w as u32, new_h as u32)
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn resize_handles_fill() {
        let result = super::resize_dimensions::<true>(100, 200, 200, 500);
        assert_eq!(result, (250, 500));

        let result = super::resize_dimensions::<true>(200, 100, 500, 200);
        assert_eq!(result, (500, 250))
    }

    #[test]
    fn resize_never_rounds_to_zero() {
        let (1.., 1..) = super::resize_dimensions::<false>(1, 150, 128, 128)
            else { panic!() };
    }

    #[test]
    fn resize_handles_overflow() {
        let result = super::resize_dimensions::<true>(100, u32::MAX, 200, u32::MAX);
        assert_eq!(result, (100, u32::MAX));

        let result = super::resize_dimensions::<true>(u32::MAX, 100, u32::MAX, 200);
        assert_eq!(result, (u32::MAX, 100));
    }

    #[test]
    fn resize_rounds() {
        // Only truncation will result in (3840, 2229) and (2160, 3719)
        let result = super::resize_dimensions::<true>(4264, 2476, 3840, 2160);
        assert_eq!(result, (3840, 2230));

        let result = super::resize_dimensions::<false>(2476, 4264, 2160, 3840);
        assert_eq!(result, (2160, 3720));
    }

    #[test]
    fn resize_handles_zero() {
        let result = super::resize_dimensions::<false>(0, 100, 100, 100);
        assert_eq!(result, (1, 100));

        let result = super::resize_dimensions::<false>(100, 0, 100, 100);
        assert_eq!(result, (100, 1));

        let result = super::resize_dimensions::<false>(100, 100, 0, 100);
        assert_eq!(result, (1, 1));

        let result = super::resize_dimensions::<false>(100, 100, 100, 0);
        assert_eq!(result, (1, 1));
    }
}
