use std::sync::{Arc, Once};
use std::sync::atomic::{AtomicBool, Ordering};
// use std::sync::atomic::AtomicBool;
use std::thread;
use glib::object::ObjectExt;
use glib::WeakRef;
use gst::{Bus, Message, Pipeline, State};
use gst::message::Eos;
use gst::prelude::{ElementExt, ElementExtManual};
use termion::event::Key;
use termion::input::TermRead;

fn seek_relative(pipeline: &Pipeline, offset: i8) {
    if let Some(current_position) = pipeline.query_position::<gst::ClockTime>() {
        let seek_offset = gst::ClockTime::from_seconds(offset.unsigned_abs().into());

        let new_position = match offset {
            0.. => {
                let res = current_position.saturating_add(seek_offset);
                match pipeline.query_duration::<gst::ClockTime>() {
                    Some(max) => res.min(max),
                    None => res
                }
            },
            ..0 => current_position.saturating_sub(seek_offset)
        };


        let _ = pipeline.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            new_position,
        );
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
            Key::Right => seek_relative(&pipeline, 5),
            Key::Left => seek_relative(&pipeline, -5),
            Key::Char(' ') => {
                state = match state {
                    State::Playing => State::Paused,
                    State::Paused => State::Playing,
                    _ => unreachable!()
                };
            }
            Key::Up => {
                state = State::Playing;
            },
            Key::Down => {
                state = State::Paused;
            },
            Key::Char('q' | 'Q') | Key::Esc => {
                bus.post(Message::from(Eos::builder().build())).unwrap()
            },
            _ => {}
        }

        let _ = pipeline.set_state(state);
    }
}

fn exit_handler(bus: &WeakRef<Bus>, pipeline: &WeakRef<Pipeline>) {
    struct State {
        exit: Once,
        one_object_destroyed: AtomicBool
    }

    let exit_signal = Arc::new(State {
        exit: Once::new(),
        one_object_destroyed: AtomicBool::new(false)
    });

    let exit_one = Arc::clone(&exit_signal);
    let exit_two = Arc::clone(&exit_signal);
    ctrlc::set_handler(move || exit_one.exit.call_once(|| ())).unwrap();

    let _callback1;
    let _callback2;
    if let Some(bus) = bus.upgrade()
        && let Some(pipe) = pipeline.upgrade() {
        let exit_cb = |exit: Arc<State>| {
            move || {
                if exit.one_object_destroyed.swap(true, Ordering::AcqRel) {
                    exit.exit.call_once(|| ())
                }
            }
        };

        _callback1 = bus.add_weak_ref_notify(exit_cb(exit_two.clone()));
        _callback2 = pipe.add_weak_ref_notify(exit_cb(exit_two));
        return;
    }

    exit_signal.exit.wait();
    if let Some(bus) = bus.upgrade() {
        bus.post(Message::from(Eos::builder().build())).unwrap()
    }
}

pub fn start(bus: WeakRef<Bus>, pipeline: WeakRef<Pipeline>) {
    thread::spawn(move || {
        thread::scope(|s| {
            s.spawn(|| exit_handler(&bus, &pipeline));
            play_controls(&bus, &pipeline)
        })
    });
}