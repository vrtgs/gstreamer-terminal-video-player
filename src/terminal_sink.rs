use std::io::Write;
use glib::object::Cast;
use gst::element_error;
use gst_app::{AppSink, AppSinkCallbacks};
use gst_video::{VideoFormat, VideoInfo};
use termion::raw::IntoRawMode;
use termion::screen::IntoAlternateScreen;
use crate::{resize_image, term_size};

fn cursor_goto(x: u16, y: u16) -> termion::cursor::Goto {
    termion::cursor::Goto(
        x.saturating_add(1),
        y.saturating_add(1)
    )
}

macro_rules! queue {
    ($s: expr $(, $thing: expr)+ $(,)?) => {
        (Ok(()) $(.and_then(|()| $s.write_all($thing.as_ref())))+).unwrap()
    };
}

fn process_sample() -> impl FnMut(&AppSink) -> Result<gst::FlowSuccess, gst::FlowError> + Send + 'static {
    let size_cache = term_size::TerminalSizeCache::new();
    let mut stdout = std::io::stdout()
        .into_raw_mode()
        .expect("terminal needs to support raw terminal I/O mode")
        .into_alternate_screen()
        .expect("app should be ran on xterm compatible terminals");

    let mut last_size = (u16::MAX, u16::MAX);

    queue!(
        stdout,
        termion::clear::All,
        termion::cursor::Hide
    );

    let defer = defer::defer(|| {
        let mut lock = std::io::stdout().lock();
        queue!(
            lock,
            termion::cursor::Show,
        )
    });

    stdout.flush().unwrap();

    // 8mb default
    let mut screen_buff = Vec::with_capacity(8 * 1024 * 1024);

    move |me| {
        // move defer to closure
        let _ = &defer;

        // make sure screen buffer is empty
        screen_buff.clear();

        let sample = me.pull_sample().map_err(|_| gst::FlowError::Eos)?;
        let caps = sample.caps().ok_or_else(|| {
            element_error!(me, gst::ResourceError::Failed, ("Sample has no caps"));
            gst::FlowError::Error
        })?;

        let video_info = VideoInfo::from_caps(&caps).map_err(|err| {
            element_error!(me, gst::ResourceError::Failed, ("{}", err));

            gst::FlowError::Error
        })?;
        let buffer = sample.buffer().ok_or_else(|| {
            element_error!(
                            me,
                            gst::ResourceError::Failed,
                            ("Failed to get buffer from appsink")
                        );

            gst::FlowError::Error
        })?;
        let buffer = buffer.map_readable().map_err(|err| {
            element_error!(
                me,
                gst::ResourceError::Failed,
                ("Failed to map buffer readable; {}", err)
            );

            gst::FlowError::Error
        })?;

        let res = image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(
            video_info.width(),
            video_info.height(),
            &buffer,
        );

        let image = res.ok_or_else(|| {
            element_error!(me, gst::ResourceError::Failed, ("invalid image divisions"));
            gst::FlowError::Error
        })?;

        let term_size = size_cache.fetch_size();
        if last_size != term_size {
            last_size = term_size;
            queue!(
                screen_buff,
                termion::clear::All
            );
        }

        let (term_width, term_height) = term_size;


        //                                                                        -fill-
        let (new_width, new_height) = resize_image::resize_dimensions::<false>(
            image.width(),
            image.height(),
            term_width.into(),
            term_height.into(),
        );


        let (new_width, new_height) = (new_width as u16, new_height as u16);

        let resized = image::imageops::thumbnail(
            &image,
            new_width.into(),
            new_height.into(),
        );

        // a good enough size each pixel gets 32 bytes because ansi is that inefficient
        // and 24 bytes for each newlines goto
        // and a constant 512 bytes extra for good measure
        screen_buff.reserve(
            (resized.as_raw().len() * 32)
                + (usize::from(new_height) * 24)
                + 512
        );

        let offset = (
            (term_width-(new_width))/2,
            (term_height-(new_height))/2
        );

        let (offset_width, offset_height) = offset;

        for (i, row) in resized.rows().enumerate() {
            write!(screen_buff, "{}", cursor_goto(
                offset_width,
                // total terminal height is at most u16::MAX
                // so this shouldn't overflow
                offset_height + i as u16,
            )).unwrap();

            for &cell in row {
                const UNICODE_BLOCK: &str = "\u{2588}";
                let [r, g, b] = cell.0;
                ansi_term::Color::RGB(r, g, b).paint(UNICODE_BLOCK.as_bytes()).write_to(&mut screen_buff).unwrap();
            }
        }

        let mut stdout = stdout.lock();
        stdout.write_all(&screen_buff).unwrap();
        stdout.flush().unwrap();

        Ok(gst::FlowSuccess::Ok)
    }
}

pub fn create() -> gst::Element {
    let caps = gst_video::VideoCapsBuilder::new()
        .format(VideoFormat::Rgb)
        .build();

    let app = AppSink::builder()
        .name("terminal player")
        .sync(true)
        .max_buffers(1)
        // .property("leaky-type", gst_app::AppLeakyType::Downstream)
        .drop(true)
        .caps(&caps)
        .callbacks(
            AppSinkCallbacks::builder()
                .new_sample_if_some(
                    std::env::var_os("NO_DISPLAY_OUTPUT")
                        .is_none_or(|str| str.as_encoded_bytes().starts_with(b"n"))
                        .then(|| process_sample())
                )
                .build(),
        )
        .build();

    app.upcast()
}