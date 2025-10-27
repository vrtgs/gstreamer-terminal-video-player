use std::thread;
use glib::WeakRef;
use gst::{Bus, Message, Pipeline, State};
use gst::message::Eos;
use gst::prelude::{ElementExt, ElementExtManual};
use termion::event::Key;
use termion::input::TermRead;

fn seek_relative(pipeline: &Pipeline, bus: &Bus, offset: i8) {
    if let Some(current_position) = pipeline.query_position::<gst::ClockTime>() {
        let seek_offset = gst::ClockTime::from_seconds(offset.unsigned_abs().into());

        let new_position = match offset {
            0.. => current_position.saturating_add(seek_offset),
            ..0 => current_position.saturating_sub(seek_offset)
        };


        let seeked = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            new_position,
        );

        if seeked.is_ok() && pipeline.current_state() == State::Paused {
            pipeline.set_state(State::Playing).ok();
            bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(50),
                &[gst::MessageType::AsyncDone]
            );
            pipeline.set_state(State::Paused).ok();
        }
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
        match event {
            Key::Right => seek_relative(&pipeline, &bus, 5),
            Key::Left => seek_relative(&pipeline, &bus, -5),
            Key::Char(' ') => {
                state = match state {
                    State::Playing => State::Paused,
                    State::Paused => State::Playing,
                    _ => unreachable!()
                };
            }
            Key::Up => state = State::Playing,
            Key::Down => state = State::Paused,
            Key::Ctrl('c') | Key::Char('q' | 'Q') | Key::Esc => {
                bus.post(Message::from(Eos::builder().build())).unwrap()
            },
            _ => {}
        }

        let _ = pipeline.set_state(state);
    }
}

pub fn start(bus: WeakRef<Bus>, pipeline: WeakRef<Pipeline>) {
    thread::spawn(move || play_controls(&bus, &pipeline));
}