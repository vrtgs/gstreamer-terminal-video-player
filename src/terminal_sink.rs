use crate::{resize_image, term_size};
use glib::object::Cast;
use gst::element_error;
use gst_app::{AppSink, AppSinkCallbacks};
use gst_video::{VideoFormat, VideoInfo};
use std::io::Write;
use std::sync::Arc;
use std::thread;
use parking_lot::{Condvar, Mutex};
use termion::raw::IntoRawMode;
use termion::screen::IntoAlternateScreen;

fn cursor_goto(x: u16, y: u16) -> termion::cursor::Goto {
    termion::cursor::Goto(x.saturating_add(1), y.saturating_add(1))
}

macro_rules! queue {
    ($s: expr $(, $thing: expr)+ $(,)?) => {
        (Ok(()) $(.and_then(|()| $s.write_all($thing.as_ref())))+).unwrap()
    };
}


fn render_sample(
    sample: &gst::Sample,
    app_sink: &AppSink,
    term_size: (u16, u16),
    fresh_redraw: bool,
    screen_buffer: &mut Vec<u8>,
    stdout: &mut dyn Write
) -> Result<(), ()> {
    // make sure screen buffer is empty
    screen_buffer.clear();

    let caps = sample.caps().ok_or_else(|| {
        element_error!(app_sink, gst::ResourceError::Failed, ("Sample has no caps"));
    })?;

    let video_info = VideoInfo::from_caps(&caps).map_err(|err| {
        element_error!(app_sink, gst::ResourceError::Failed, ("{}", err));
    })?;

    let buffer = sample.buffer().ok_or_else(|| {
        element_error!(
            app_sink,
            gst::ResourceError::Failed,
            ("Failed to get buffer from appsink")
        );
    })?;
    let buffer = buffer.map_readable().map_err(|err| {
        element_error!(
            app_sink,
            gst::ResourceError::Failed,
            ("Failed to map buffer readable; {}", err)
        );
    })?;

    let res = image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(
        video_info.width(),
        video_info.height(),
        &buffer,
    );

    let image = res.ok_or_else(|| {
        element_error!(app_sink, gst::ResourceError::Failed, ("invalid image divisions"));
    })?;

    let pixels_available = {
        let (width, height) = term_size;
        (width, height.saturating_mul(2))
    };

    if fresh_redraw {
        queue!(screen_buffer, termion::clear::All);
    }

    let height_pixels_available = pixels_available.1;
    let (term_width, term_height) = term_size;

    //                                                                        -fill-
    let (new_width, new_height) = resize_image::resize_dimensions::<false>(
        image.width(),
        image.height(),
        term_width.into(),
        height_pixels_available.into(),
    );

    let (new_width, new_height) = (new_width as u16, new_height as u16);

    let resized = image::imageops::thumbnail(
        &image,
        new_width.into(),
        new_height.into(),
        // image::imageops::Nearest
    );

    // a good enough size each pixel gets 48 bytes because ansi is that inefficient
    // and 24 bytes for each newlines goto
    // and a constant 512 bytes extra for good measure
    let expected_size = (resized.as_raw().len() * 48)
        + (usize::from(new_height.div_ceil(2)) * 24)
        + 512;

    screen_buffer.reserve(expected_size);

    let offset = (
        (term_width - (new_width)) / 2,
        (term_height - (new_height.div_ceil(2))) / 2,
    );

    let (offset_width, offset_height) = offset;

    let mut rows_iter = resized.rows();
    let mut current = 0;

    'rendering: while let Some(first_row) = rows_iter.next() {
        const UNICODE_TOP_HALF_BLOCK: &str = "\u{2580}";

        write!(
            screen_buffer,
            "{}",
            cursor_goto(
                offset_width,
                // total terminal height is at most u16::MAX
                // so this shouldn't overflow
                offset_height + current,
            )
        )
            .unwrap();

        let Some(second_row) = rows_iter.next() else {
            for &cell in first_row {
                let [r, g, b] = cell.0;
                ansi_term::Color::RGB(r, g, b)
                    .paint(UNICODE_TOP_HALF_BLOCK.as_bytes())
                    .write_to(screen_buffer)
                    .unwrap();
            }
            break 'rendering;
        };

        assert_eq!(first_row.len(), second_row.len());

        for (top, bottom) in first_row.zip(second_row) {
            let [tr, tg, tb] = top.0;
            let [br, bg, bb] = bottom.0;
            ansi_term::Color::RGB(tr, tg, tb)
                .on(ansi_term::Colour::RGB(br, bg, bb))
                .paint(UNICODE_TOP_HALF_BLOCK.as_bytes())
                .write_to(screen_buffer)
                .unwrap();
        }

        current += 1;
    }

    // let mut stdout = stdout.lock();
    stdout.write_all(screen_buffer).unwrap();
    stdout.flush().unwrap();

    Ok(())
}


enum RenderState {
    None,
    Some(gst::Sample),
    OtherPipeQuit,
}

// THE WHOLE THING IS NOT UNWIND SAFE

#[cfg(not(test))]
const _: () = assert!(cfg!(panic = "abort"));

struct RenderingContext {
    sample: Mutex<RenderState>,
    sample_notification: Condvar,
}

fn send_new_sample(
    ctx: Arc<RenderingContext>,
    pull_sample: fn(&AppSink) -> Result<gst::Sample, glib::BoolError>,
) -> impl FnMut(&AppSink) -> Result<gst::FlowSuccess, gst::FlowError> + Send + 'static {
    move |me| {
        let sample = pull_sample(me).map_err(|_| {
            *ctx.sample.lock() = RenderState::OtherPipeQuit;
            ctx.sample_notification.notify_one();
            gst::FlowError::Eos
        })?;

        {
            let mut lock = ctx.sample.lock();
            match &mut *lock {
                slot @ RenderState::None => {
                    *slot = RenderState::Some(sample);
                    drop(lock);
                    ctx.sample_notification.notify_one();
                },
                // still rendering...
                RenderState::Some(old_sample) => *old_sample = sample,
                RenderState::OtherPipeQuit => return Err(gst::FlowError::Error),
            }
        }
        Ok(gst::FlowSuccess::Ok)
    }
}

fn run_renderer_thread(
    ctx: Arc<RenderingContext>,
    app_sink: AppSink
) {
    let size_cache = term_size::TerminalSizeCache::new();
    let mut stdout = termion::get_tty()
        .expect("couldn't get a handle to the raw tty")
        .into_raw_mode()
        .expect("terminal needs to support raw terminal I/O mode")
        .into_alternate_screen()
        .expect("app should be ran on xterm compatible terminals");

    queue!(stdout, termion::clear::All, termion::cursor::Hide);

    stdout.flush().unwrap();

    // 8mb default
    let mut screen_buff = Vec::with_capacity(8 * 1024 * 1024);
    let mut last_size = (0, 0);

    'render_loop: loop {
        let sample = loop {
            let mut lock = ctx.sample.lock();
            match core::mem::replace(&mut *lock, RenderState::None) {
                RenderState::None => ctx.sample_notification.wait(&mut lock),
                RenderState::Some(sample) => break sample,
                RenderState::OtherPipeQuit => break 'render_loop,
            }
        };

        let new_size = size_cache.fetch_size();
        let old_size = std::mem::replace(&mut last_size, new_size);

        let res = render_sample(
            &sample,
            &app_sink,
            new_size,
            old_size != new_size,
            &mut screen_buff,
            &mut stdout,
        );

        if res.is_err() {
            *ctx.sample.lock() = RenderState::OtherPipeQuit;
            break
        }
    }

    queue!(stdout, termion::cursor::Show)
}

pub fn create() -> gst::Element {
    let caps = gst_video::VideoCapsBuilder::new()
        .format(VideoFormat::Rgb)
        .build();


    // try .leaky_type(gst_app::AppLeakyType::Downstream) later on

    let renderer_enabled = std::env::var_os("NO_DISPLAY_OUTPUT")
        .is_none_or(|str| str.as_encoded_bytes().starts_with(b"n"));

    let ctx = Arc::new(const {
        RenderingContext {
            sample: Mutex::new(RenderState::None),
            sample_notification: Condvar::new(),
        }
    });

    let app = AppSink::builder()
        .name("terminal player")
        .sync(true)
        .caps(&caps)
        .callbacks(
            AppSinkCallbacks::builder()
                .new_sample_if(
                    send_new_sample(Arc::clone(&ctx), AppSink::pull_sample),
                    renderer_enabled,
                )
                .new_preroll_if(
                    send_new_sample(Arc::clone(&ctx), AppSink::pull_preroll),
                    renderer_enabled,
                )
                .build(),
        )
        .build();

    let app_clone = app.clone();
    thread::spawn(move || run_renderer_thread(ctx, app_clone));

    app.upcast()
}
