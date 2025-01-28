use gst::{Bus, Message, Pipeline, State};
use gst::message::Eos;
use gst::prelude::{ElementExt, ElementExtManual};
use termion::event::Key;
use termion::input::TermRead;
use tokio::task::JoinHandle;

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

async fn play_controls(bus: &Bus, pipeline: &Pipeline) {
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        let stdin = std::io::stdin().lock();
        let _ = stdin.keys().map_while(Result::ok).try_for_each(|key| tx.send(key));
    });

    let mut state = State::Playing;

    while let Ok(event) = rx.recv_async().await {
        match event {
            Key::Right => seek_relative(pipeline, 5),
            Key::Left => seek_relative(pipeline, -5),
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

async fn ctrl_c(bus: &Bus) {
    tokio::signal::ctrl_c().await.unwrap();
    bus.post(Message::from(Eos::builder().build())).unwrap()
}

async fn run(bus: Bus, pipeline: Pipeline) {
    tokio::join!(play_controls(&bus, &pipeline), ctrl_c(&bus));
}

pub fn start(bus: Bus, pipeline: Pipeline) -> JoinHandle<()> {
    tokio::spawn(run(bus, pipeline))
}