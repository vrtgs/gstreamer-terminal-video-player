extern crate gstreamer as gst;
extern crate gstreamer_app as gst_app;
extern crate gstreamer_video as gst_video;

use crate::gst::prelude::ElementExtManual;
use defer::defer;
use glib::object::ObjectExt;
use gst::prelude::{ElementExt, GstBinExtManual, GstObjectExt, PadExt};
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

    let file = PathBuf::from(arg);
    gst::ElementFactory::make("filesrc")
        .name("source")
        .property("location", file)
        .build()
        .unwrap()
}

fn program_main() {
    let source = get_source();
    let decode = gst::ElementFactory::make("decodebin3").build().unwrap();

    let convert = gst::ElementFactory::make("videoconvert").build().unwrap();

    let video_sink = terminal_sink::create();

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

    defer! {{
        pipeline
            .set_state(gst::State::Null)
            .unwrap();
    }}

    let bus = pipeline.bus().unwrap();

    input_handler::start(bus.downgrade(), pipeline.downgrade());

    for msg in bus.iter_timed(None) {
        use gst::MessageView;

        match msg.view() {
            MessageView::Error(err) => {
                eprintln!(
                    "Error received from element {:?}: {}",
                    err.src().map(|s| s.path_string()),
                    err.error()
                );
                eprintln!("Debugging information: {:?}", err.debug());
                break;
            }
            MessageView::Eos(..) => break,
            _ => (),
        }
    }
}

fn main() {
    // launch::run is only required to set up the application environment on macOS
    // (but not necessary in normal Cocoa applications where this is set up automatically)
    launch::run(program_main);
}
