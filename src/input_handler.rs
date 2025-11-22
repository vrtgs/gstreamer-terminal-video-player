use glib::WeakRef;
use gst::message::Eos;
use gst::prelude::{ElementExt, ElementExtManual};
use gst::{Bus, Pipeline, State};
use std::fmt::Display;
use std::thread;
use termion::event::Key;
use termion::input::TermRead;

fn seek_error_to_bus<T>(bus: &Bus, result: Result<T, impl Display>) -> Option<T> {
    match result {
        Ok(x) => Some(x),
        Err(err) => {
            bus.post(gst::message::Error::new(gst::CoreError::Seek, &format!("{err}")).into())
                .unwrap();
            None
        }
    }
}

fn seek_absolute(
    pipeline: &Pipeline,
    bus: &Bus,
    new_position: gst::ClockTime,
    flags: gst::SeekFlags,
) {
    let result = pipeline.seek_simple(flags, new_position);

    seek_error_to_bus(bus, result);
}

fn seek_relative(pipeline: &Pipeline, bus: &Bus, offset: i8) {
    if let Some(current_position) = pipeline.query_position::<gst::ClockTime>() {
        let seek_offset = gst::ClockTime::from_seconds(offset.unsigned_abs().into());

        let new_position = match offset {
            0.. => current_position.saturating_add(seek_offset),
            ..0 => current_position.saturating_sub(seek_offset),
        };

        seek_absolute(
            pipeline,
            bus,
            new_position,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
        )
    }
}

fn play_controls(bus: &WeakRef<Bus>, pipeline: &WeakRef<Pipeline>) {
    let event_stream = std::io::stdin()
        .lock()
        .keys()
        .map_while(Result::ok)
        .map_while(|event| {
            pipeline
                .upgrade()
                .and_then(|pipe| Some((pipe, bus.upgrade()?)))
                .filter(|(pipeline, _)| pipeline.current_state() != State::Null)
                .map(|(pipeline, bus)| (event, pipeline, bus))
        });

    let mut state = State::Playing;

    for (event, pipeline, bus) in event_stream {
        let last_state = state;

        match event {
            Key::Right => seek_relative(&pipeline, &bus, 5),
            Key::Left => seek_relative(&pipeline, &bus, -5),
            Key::Char(' ') => {
                state = match state {
                    State::Playing => State::Paused,
                    State::Paused => State::Playing,
                    _ => unreachable!(),
                };
            }
            Key::Up => state = State::Playing,
            Key::Down => state = State::Paused,
            Key::Ctrl('c') | Key::Char('q' | 'Q') | Key::Esc => {
                bus.post(Eos::new()).unwrap();
                break;
            }
            _ => {}
        }

        if last_state != state {
            seek_error_to_bus(&bus, pipeline.set_state(state));
        }
    }
}

pub fn start(bus: WeakRef<Bus>, pipeline: WeakRef<Pipeline>) {
    thread::spawn(move || play_controls(&bus, &pipeline));
}
