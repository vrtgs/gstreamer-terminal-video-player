extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_video as gst_video;

use crate::gst::prelude::ElementExtManual;
use clap::Parser;
use glib::object::ObjectExt;
use gst::prelude::{ElementExt, GstBinExtManual, GstObjectExt, PadExt};
use std::os::fd::IntoRawFd;
use std::path::PathBuf;

mod input_handler;
mod launch;
mod resize_image;
mod term_size;
mod terminal_sink;

pub(crate) fn flag(flag: &str, default: bool) -> bool {
    std::env::var_os(flag).map_or(default, |str| {
        let mut str = str.into_encoded_bytes();
        str.make_ascii_lowercase();
        matches!(str.trim_ascii(), b"y" | b"yes" | b"")
    })
}

fn get_source(video: PathBuf) -> gst::Element {
    macro_rules! exit {
        ($($msg: tt)+) => {
            {
                eprintln!($($msg)+);
                std::process::exit(-1);
            }
        };
    }

    match std::fs::File::open(&video) {
        Ok(file) => {
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;

                let fd = file.as_raw_fd();
                gst::ElementFactory::make("fdsrc")
                    .name("source")
                    .property("fd", fd)
                    .build()
                    .inspect(|_| {
                        // if the element was built forget the file
                        // and DO NOT drop it
                        let _fd = file.into_raw_fd();
                    })
                    .unwrap()
            }

            #[cfg(not(unix))]
            {
                drop(file);
                gst::ElementFactory::make("filesrc")
                    .name("source")
                    .property("location", file_path)
                    .build()
                    .unwrap()
            }
        }
        Err(err) => exit!("couldn't open file: {err}"),
    }
}

fn gstreamer_element(name: &str) -> Result<gst::Element, glib::BoolError> {
    gst::ElementFactory::make(name).build()
}

fn make_pipeline_and_bus(
    quit_handler: &mut QuitHandler,
    video: PathBuf,
    size: Option<(u16, u16)>,
) -> (gst::Pipeline, gst::Bus) {
    let source = get_source(video);
    let decode = gstreamer_element("decodebin3")
        .or_else(|_| gstreamer_element("decodebin"))
        .unwrap();

    let convert = gstreamer_element("videoconvert").unwrap();

    let video_sink = terminal_sink::create(quit_handler, size);

    let audio_enabled = !flag("NO_AUDIO_OUTPUT", false);

    let audio_convert = gstreamer_element("audioconvert").unwrap();
    let audio_resample = gstreamer_element("audioresample").unwrap();
    let audio_sink = gstreamer_element("autoaudiosink").unwrap();

    let pipeline = gst::Pipeline::new();

    let line = [&source, &decode, &convert, &video_sink];

    pipeline.add_many(line).unwrap();

    let audio_line = [&audio_convert, &audio_resample, &audio_sink];

    if audio_enabled {
        pipeline.add_many(audio_line).unwrap();
        gst::Element::link_many(audio_line).unwrap();
    }

    source.link(&decode).unwrap();
    convert.link(&video_sink).unwrap();

    decode.connect_pad_added(move |_decode, src_pad| {
        let caps = src_pad
            .current_caps()
            .unwrap_or_else(|| src_pad.query_caps(None));
        let structure = caps.structure(0).unwrap();
        let media_type = structure.name().as_str();

        if media_type.starts_with("audio/") {
            if !audio_enabled {
                return;
            }

            let sink_pad = audio_convert.static_pad("sink").unwrap();
            if sink_pad.is_linked() {
                return;
            }
            src_pad.link(&sink_pad).expect("Failed to link audio pad");
        } else if media_type.starts_with("video/") {
            let sink_pad = convert.static_pad("sink").unwrap();
            if sink_pad.is_linked() {
                return;
            }
            src_pad.link(&sink_pad).expect("Failed to link video pad");
        }
    });

    pipeline.set_state(gst::State::Playing).unwrap();

    let bus = pipeline.bus().unwrap();

    (pipeline, bus)
}

pub struct QuitHandler {
    callbacks: Vec<Box<dyn FnOnce()>>,
}

impl QuitHandler {
    pub fn add(&mut self, callback: impl FnOnce() + 'static) {
        self.callbacks.push(Box::new(callback))
    }
}

impl Drop for QuitHandler {
    fn drop(&mut self) {
        for callback in self.callbacks.drain(..) {
            callback()
        }
    }
}

#[derive(Debug, Clone)]
struct Size {
    width: u16,
    height: u16,
}

impl std::str::FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (w, h) = s.split_once('x').ok_or_else(|| {
            "size must be in the form {WIDTH}x{HEIGHT} (e.g. 800x600)".to_string()
        })?;

        let parse = |v: &str| v.parse::<u16>();

        let width = parse(w).map_err(|_| "width must be a positive integer".to_string())?;
        let height = parse(h).map_err(|_| "height must be a positive integer".to_string())?;

        Ok(Size { width, height })
    }
}

#[derive(clap::Parser, Debug)]
#[command(name = "videoplayer")]
#[command(about = "Simple video player CLI")]
struct Cli {
    /// Video file to play (positional)
    video: PathBuf,

    /// Window size in the form WIDTHxHEIGHT, e.g. 1280x720
    #[arg(long, value_parser = clap::value_parser!(Size))]
    size: Option<Size>,
}

fn program_main() {
    let cli = Cli::parse();

    let mut quit_handler = QuitHandler { callbacks: vec![] };

    let size = cli.size.map(|size| (size.width, size.height));
    let (pipeline, bus) = make_pipeline_and_bus(&mut quit_handler, cli.video, size);

    let defer = defer::defer(|| {
        pipeline.set_state(gst::State::Null).unwrap();
    });

    input_handler::start(bus.downgrade(), pipeline.downgrade());

    for msg in bus.iter_timed(None) {
        use gst::MessageView;

        match msg.view() {
            MessageView::Error(err) => {
                drop((bus, defer));
                drop(pipeline);
                drop(quit_handler);

                eprintln!("{}", termion::clear::All);

                eprintln!(
                    "Error received from element {:?}: {}",
                    err.src()
                        .map(|s| s.path_string())
                        .unwrap_or_else(|| glib::gstr!("unknown").to_owned()),
                    err.error()
                );
                eprintln!("Debugging information: {:?}", err.debug());
                break;
            }
            MessageView::Eos(_) => break,
            _ => (),
        }
    }
}

fn main() {
    // launch::run is only required to set up the application environment on macOS
    // (but not necessary in normal Cocoa applications where this is set up automatically)
    launch::run(program_main);
}
