use crate::{resize_image, term_size};
use glib::object::Cast;
use gst::element_error;
use gst::prelude::ElementExtManual;
use gst_app::{AppSink, AppSinkCallbacks};
use gst_video::{VideoFormat, VideoInfo};
use parking_lot::{Condvar, Mutex};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;
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
    stdout: &mut dyn Write,
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
        element_error!(
            app_sink,
            gst::ResourceError::Failed,
            ("invalid image divisions")
        );
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
    let expected_size =
        (resized.as_raw().len() * 48) + (usize::from(new_height.div_ceil(2)) * 24) + 512;

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

// THE WHOLE THING IS NOT UNWIND SAFE

#[cfg(not(test))]
const _: () = assert!(cfg!(panic = "abort"));

enum RenderState {
    None,
    HasSample { sample: gst::Sample, pulled: bool },
    OtherPipeQuit,
}

struct RenderingContext {
    state: Mutex<RenderState>,
    sample_notification: Condvar,
}

struct RenderingContextPipe(Arc<RenderingContext>);

impl Drop for RenderingContextPipe {
    fn drop(&mut self) {
        let mut lock = self.0.state.lock();
        *lock = RenderState::OtherPipeQuit;
        drop(lock);
        self.0.sample_notification.notify_one();
    }
}

#[derive(Clone)]
struct SampleProducer(Arc<RenderingContextPipe>);

impl SampleProducer {
    pub fn push_sample(&self, sample: gst::Sample) -> Result<(), ()> {
        let this: &RenderingContext = &*self.0.0;

        let mut lock = this.state.lock();
        match &mut *lock {
            // still rendering...
            RenderState::HasSample {
                sample: old_sample,
                pulled: false,
            } => *old_sample = sample,
            RenderState::OtherPipeQuit => return Err(()),
            slot => {
                *slot = RenderState::HasSample {
                    sample,
                    pulled: false,
                };
                drop(lock);
                this.sample_notification.notify_one();
            }
        }

        Ok(())
    }
}

struct SampleConsumer(RenderingContextPipe);

impl SampleConsumer {
    pub fn pull_sample(&self) -> Result<gst::Sample, ()> {
        let this: &RenderingContext = &*self.0.0;

        let mut lock = this.state.lock();
        loop {
            match &mut *lock {
                RenderState::None | RenderState::HasSample { pulled: true, .. } => {
                    this.sample_notification.wait(&mut lock)
                }
                RenderState::HasSample {
                    sample,
                    pulled: pulled @ false,
                } => {
                    *pulled = true;
                    break Ok(sample.clone());
                }
                RenderState::OtherPipeQuit => return Err(()),
            }
        }
    }

    pub fn make_reloader(&self) -> SampleReloader {
        SampleReloader(Arc::downgrade(&self.0.0))
    }
}

struct SampleReloader(Weak<RenderingContext>);

impl SampleReloader {
    pub fn reload_sample(&self) -> Result<(), ()> {
        let Some(this) = self.0.upgrade() else {
            return Err(());
        };

        let this: &RenderingContext = &this;

        let mut lock = this.state.lock();
        match &mut *lock {
            RenderState::None => Ok(()),
            RenderState::HasSample { pulled, .. } => {
                *pulled = false;
                drop(lock);
                this.sample_notification.notify_one();
                Ok(())
            }
            RenderState::OtherPipeQuit => Err(()),
        }
    }
}

fn video_pipe() -> (SampleProducer, SampleConsumer) {
    let ctx = Arc::new(
        const {
            RenderingContext {
                state: Mutex::new(RenderState::None),
                sample_notification: Condvar::new(),
            }
        },
    );

    let pipe1 = RenderingContextPipe(Arc::clone(&ctx));
    let pipe2 = RenderingContextPipe(Arc::clone(&ctx));

    (SampleProducer(Arc::new(pipe1)), SampleConsumer(pipe2))
}

fn send_new_sample(
    pipe: SampleProducer,
    pull_sample: fn(&AppSink) -> Result<gst::Sample, glib::BoolError>,
) -> impl FnMut(&AppSink) -> Result<gst::FlowSuccess, gst::FlowError> + Send + 'static {
    move |me| {
        let sample = pull_sample(me).map_err(|_| gst::FlowError::Eos)?;

        // if std::ptr::fn_addr_eq(pull_sample, AppSink::pull_preroll as fn(_) -> _) {
        //     eprintln!("pre roll")
        // }

        if pipe.push_sample(sample).is_err() {
            #[cold]
            #[inline(always)]
            fn cold_path() {}
            cold_path();

            return Err(gst::FlowError::Error);
        }

        Ok(gst::FlowSuccess::Ok)
    }
}

fn run_renderer_thread(consumer: SampleConsumer, app_sink: AppSink) {
    const TOP_BIT: u64 = 1 << 63;

    let size_cache = Arc::new(AtomicU64::new(0));
    let size_cache_clone = Arc::clone(&size_cache);

    let store_new_size = move |size: (u16, u16)| {
        let (lo, hi) = size;
        let num = bytemuck::must_cast::<[u16; 2], u32>([lo, hi]);
        size_cache_clone.store((num as u64) | TOP_BIT, Ordering::Relaxed)
    };

    let app_sink_clone = app_sink.clone();
    let reloader = consumer.make_reloader();
    let size_cache_updater =
        term_size::TerminalSizeUpdater::new(Duration::from_millis(280), move |new_size| {
            if app_sink_clone.current_state() == gst::State::Paused {
                let _ = reloader.reload_sample();
            }

            store_new_size(new_size)
        });

    let size_cache = &*size_cache;
    let load_size_from_cache = move || -> ((u16, u16), bool) {
        size_cache_updater.trigger_reload();
        // remove the top bit to signal to the next load that HEY this value didn't change
        let value = size_cache.fetch_and(!TOP_BIT, Ordering::Relaxed);
        let changed = (value & TOP_BIT) != 0;
        let [lo, hi] = bytemuck::must_cast::<u32, [u16; 2]>(value as u32);

        ((lo, hi), changed)
    };

    let mut stdout = termion::get_tty()
        .expect("couldn't get a handle to the raw tty")
        .into_raw_mode()
        .expect("terminal needs to support raw terminal I/O mode")
        .into_alternate_screen()
        .expect("app should be ran on xterm compatible terminals");

    // there will be a clear on the first fetch from the size cache
    // so wait until first render before clearing
    queue!(stdout, termion::cursor::Hide);

    stdout.flush().unwrap();

    // 8mb default
    let mut screen_buff = Vec::with_capacity(8 * 1024 * 1024);

    'render_loop: loop {
        let sample = match consumer.pull_sample() {
            Ok(sample) => sample,
            Err(()) => break 'render_loop,
        };

        let (size, size_changed) = load_size_from_cache();

        let res = render_sample(
            &sample,
            &app_sink,
            size,
            size_changed,
            &mut screen_buff,
            &mut stdout,
        );

        if res.is_err() {
            break;
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

    let (producer, consumer) = video_pipe();

    let app = AppSink::builder()
        .name("terminal player")
        .sync(true)
        .caps(&caps)
        .callbacks(
            AppSinkCallbacks::builder()
                .new_sample_if(
                    send_new_sample(producer.clone(), AppSink::pull_sample),
                    renderer_enabled,
                )
                .new_preroll_if(
                    send_new_sample(producer, AppSink::pull_preroll),
                    renderer_enabled,
                )
                .build(),
        )
        .build();

    if renderer_enabled {
        let app_clone = app.clone();
        thread::spawn(move || run_renderer_thread(consumer, app_clone));
    }

    app.upcast()
}
