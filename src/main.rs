extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_video as gst_video;

use crate::gst::prelude::ElementExtManual;
use glib::object::ObjectExt;
use gst::prelude::{ElementExt, GstBinExtManual, GstObjectExt, PadExt};
use std::os::fd::IntoRawFd;
use std::path::PathBuf;

mod launch;
mod resize_image;
mod term_size;
mod terminal_sink;

mod input_handler;

fn get_source() -> gst::Element {
    let arg = std::env::args_os()
        .nth(1)
        .expect("should pass in argument for file");

    macro_rules! exit {
        ($($msg: tt)+) => {
            {
                eprintln!($($msg)+);
                std::process::exit(-1);
            }
        };
    }

    let file_path = PathBuf::from(arg);

    match std::fs::File::open(&file_path) {
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

fn make_pipeline_and_bus(quit_handler: &mut QuitHandler) -> (gst::Pipeline, gst::Bus) {
    let source = get_source();
    let decode = gst::ElementFactory::make("decodebin3").build().unwrap();

    let convert = gst::ElementFactory::make("videoconvert").build().unwrap();

    let video_sink = terminal_sink::create(quit_handler);

    let audio_convert = gst::ElementFactory::make("audioconvert").build().unwrap();
    let audio_resample = gst::ElementFactory::make("audioresample").build().unwrap();
    let audio_sink = gst::ElementFactory::make("autoaudiosink").build().unwrap();

    let pipeline = gst::Pipeline::new();

    let line = [
        &source,
        &decode,
        &convert,
        &audio_convert,
        &audio_resample,
        &video_sink,
        &audio_sink,
    ];

    pipeline.add_many(line).unwrap();

    source.link(&decode).unwrap();
    convert.link(&video_sink).unwrap();
    gst::Element::link_many([&audio_convert, &audio_resample, &audio_sink]).unwrap();

    decode.connect_pad_added(move |_decode, src_pad| {
        let caps = src_pad
            .current_caps()
            .unwrap_or_else(|| src_pad.query_caps(None));
        let structure = caps.structure(0).unwrap();
        let media_type = structure.name();

        if media_type.starts_with("audio/") {
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
        } else {
            eprintln!("Unknown pad type: {}", media_type);
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

fn program_main() {
    let mut quit_handler = QuitHandler { callbacks: vec![] };

    let (pipeline, bus) = make_pipeline_and_bus(&mut quit_handler);

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
