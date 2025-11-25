use crate::term_size::TerminalSizeUpdater;
use crate::terminal_sink::resize::{ImageRef, RenderedFrame, ResizeBuffer, Resizer};
use crate::terminal_sink::video_pipe::{SampleConsumer, SampleProducer, SampleReloader};
use crate::{QuitHandler, resize_image};
use glib::object::Cast;
use gst::element_error;
use gst::prelude::ElementExtManual;
use gst_app::{AppSink, AppSinkCallbacks};
use gst_video::{VideoFormat, VideoInfo};
use std::cell::Cell;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
use termion::raw::IntoRawMode;
use termion::screen::IntoAlternateScreen;

mod resize;
mod video_pipe;

fn cursor_goto(x: u16, y: u16) -> termion::cursor::Goto {
    termion::cursor::Goto(x.saturating_add(1), y.saturating_add(1))
}

fn render_sample(
    sample: &gst::Sample,
    app_sink: &AppSink,
    term_size: (u16, u16),
    fresh_redraw: bool,
    command_buffer: &mut Vec<u8>,
    resize_buffer: &mut ResizeBuffer,
    resizer: &mut Resizer,
    last_frame: &mut RenderedFrame,
    stdout: &mut dyn Write,
) -> Result<(), ()> {
    // make sure screen buffer is empty
    command_buffer.clear();

    let caps = sample.caps().ok_or_else(|| {
        element_error!(app_sink, gst::ResourceError::Failed, ("Sample has no caps"));
    })?;

    let video_info = VideoInfo::from_caps(&caps).map_err(|err| {
        element_error!(app_sink, gst::ResourceError::Failed, ("{err}"));
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
            ("Failed to map buffer readable; {err}")
        );
    })?;

    let res = ImageRef::from_buffer(video_info.width(), video_info.height(), &buffer);

    let image = res.ok_or_else(|| {
        element_error!(
            app_sink,
            gst::ResourceError::Failed,
            ("invalid video sample divisions")
        );
    })?;

    let pixels_available = {
        let (width, height) = term_size;
        (width, height.saturating_mul(2))
    };

    let height_pixels_available = pixels_available.1;
    let (term_width, term_height) = term_size;

    //                                                                        -fill-
    let (new_width, new_height) = resize_image::resize_dimensions::<false>(
        video_info.width(),
        video_info.height(),
        term_width.into(),
        height_pixels_available.into(),
    );

    let (new_width, new_height) = (new_width as u16, new_height as u16);

    let resized = {
        if resize_buffer.width() != new_width || resize_buffer.height() != new_height {
            resize_buffer.resize((new_width, new_height))
        }

        resizer.resize(image, resize_buffer).as_image_crate_buffer()
    };

    // a good enough size each pixel gets 48 bytes because ansi is that inefficient
    // and 24 bytes for each newlines goto
    // and a constant 512 bytes extra for good measure
    let expected_size =
        (resized.as_raw().len() * 48) + (usize::from(new_height.div_ceil(2)) * 24) + 512;

    command_buffer.reserve(expected_size);

    let offset = (
        (term_width - (new_width)) / 2,
        (term_height - (new_height.div_ceil(2))) / 2,
    );

    last_frame.render(resized, fresh_redraw, offset, command_buffer);

    stdout.write_all(command_buffer).unwrap();
    stdout.flush().unwrap();

    Ok(())
}

// THE WHOLE THING IS NOT UNWIND SAFE

#[cfg(not(test))]
const _: () = assert!(cfg!(panic = "abort"));

fn send_new_sample(
    pipe: SampleProducer,
    pull_sample: fn(&AppSink) -> Result<gst::Sample, glib::BoolError>,
) -> impl FnMut(&AppSink) -> Result<gst::FlowSuccess, gst::FlowError> + Send + 'static {
    move |me| {
        let sample = pull_sample(me).map_err(|_| gst::FlowError::Eos)?;

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

fn flag(flag: &str, default: bool) -> bool {
    std::env::var_os(flag).map_or(default, |str| {
        let mut str = str.into_encoded_bytes();
        str.make_ascii_lowercase();
        matches!(&*str, b"y" | b"yes")
    })
}

struct TerminalSizeLoadResult {
    changed: bool,
    size: (u16, u16),
}

trait TerminalSizeLoader {
    fn load(&self) -> TerminalSizeLoadResult;
}

struct DynamicSize {
    size_cache: Arc<AtomicU64>,
    updater: TerminalSizeUpdater,
}

impl DynamicSize {
    const TAG_BIT: u64 = 1 << 63;

    pub fn new(app_sink: AppSink, reloader: SampleReloader) -> Self {
        let size_cache = Arc::new(AtomicU64::new(0));
        let size_cache_clone = Arc::clone(&size_cache);

        let store_new_size = move |size: (u16, u16)| {
            let (lo, hi) = size;
            let num = bytemuck::must_cast::<[u16; 2], u32>([lo, hi]);
            size_cache_clone.store((num as u64) | Self::TAG_BIT, Ordering::Relaxed)
        };

        let app_sink_clone = app_sink.clone();
        let size_cache_updater =
            TerminalSizeUpdater::new(Duration::from_millis(280), move |new_size| {
                if app_sink_clone.current_state() == gst::State::Paused {
                    let _ = reloader.reload_sample();
                }

                store_new_size(new_size)
            });

        Self {
            size_cache,
            updater: size_cache_updater,
        }
    }
}

impl TerminalSizeLoader for DynamicSize {
    fn load(&self) -> TerminalSizeLoadResult {
        self.updater.trigger_reload();
        // remove the top bit to signal to the next load that HEY this value didn't change
        let value = self
            .size_cache
            .fetch_and(const { !Self::TAG_BIT }, Ordering::Relaxed);
        let changed = (value & Self::TAG_BIT) != 0;
        let [lo, hi] = bytemuck::must_cast::<u32, [u16; 2]>(value as u32);

        TerminalSizeLoadResult {
            size: (lo, hi),
            changed,
        }
    }
}

struct StaticSize {
    size: (u16, u16),
    first_fetch: Cell<bool>,
}

impl StaticSize {
    pub fn new(size: (u16, u16)) -> Self {
        Self {
            size,
            first_fetch: Cell::new(true),
        }
    }
}

impl TerminalSizeLoader for StaticSize {
    fn load(&self) -> TerminalSizeLoadResult {
        TerminalSizeLoadResult {
            size: self.size,
            changed: self.first_fetch.replace(false),
        }
    }
}

fn run_renderer_thread(consumer: SampleConsumer, app_sink: AppSink, size: Option<(u16, u16)>) {
    let dynamic;
    let static_;
    let loader = match size {
        Some(size) => {
            static_ = StaticSize::new(size);
            (&static_) as &dyn TerminalSizeLoader
        }
        None => {
            dynamic = DynamicSize::new(app_sink.clone(), consumer.make_reloader());
            &dynamic
        }
    };

    let mut tty_file;
    let mut stdout;

    trait TTY: Write + AsFd + AsRawFd {}
    impl<T: Write + AsFd + AsRawFd> TTY for T {}

    fn make_tty<T: TTY>(tty: T) -> impl Write {
        tty.into_raw_mode()
            .expect("terminal needs to support raw terminal I/O mode")
            .into_alternate_screen()
            .expect("app should be ran on xterm compatible terminals")
    }

    let use_stdout = flag("USE_STDOUT", false);
    let tty: &mut dyn Write = if !use_stdout && let Ok(tty) = termion::get_tty() {
        tty_file = make_tty(tty);
        &mut tty_file
    } else {
        stdout = make_tty(std::io::stdout().lock());
        &mut stdout
    };

    // there will be a clear on the first fetch from the size cache
    // so wait until first render before clearing
    tty.write_all(termion::cursor::Hide.as_ref()).unwrap();
    tty.flush().unwrap();

    // 8mb default
    let mut screen_buff = Vec::with_capacity(8 * 1024 * 1024);
    let mut resize_buffer = ResizeBuffer::new();
    let mut resizer = Resizer::new();
    let mut last_frame = RenderedFrame::new();

    'render_loop: loop {
        let sample = match consumer.pull_sample() {
            Ok(sample) => sample,
            Err(()) => break 'render_loop,
        };

        let size_res = loader.load();

        let res = render_sample(
            &sample,
            &app_sink,
            size_res.size,
            size_res.changed,
            &mut screen_buff,
            &mut resize_buffer,
            &mut resizer,
            &mut last_frame,
            tty,
        );

        if res.is_err() {
            break;
        }
    }

    tty.write_all(termion::cursor::Show.as_ref()).unwrap()
}

pub fn create(quit_handler: &mut QuitHandler, size: Option<(u16, u16)>) -> gst::Element {
    let caps = gst_video::VideoCapsBuilder::new()
        .format(VideoFormat::Rgb)
        .build();

    let renderer_enabled = !flag("NO_DISPLAY_OUTPUT", false);

    let (producer, consumer) = video_pipe::video_pipe();

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
                    send_new_sample(producer.clone(), AppSink::pull_preroll),
                    renderer_enabled,
                )
                .build(),
        )
        .build();

    if renderer_enabled {
        let app_clone = app.clone();
        let jh = thread::spawn(move || run_renderer_thread(consumer, app_clone, size));
        quit_handler.add(move || {
            producer.close();
            jh.join().unwrap()
        })
    }

    app.upcast()
}
