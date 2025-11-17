use parking_lot::{Condvar, Mutex};
use std::sync::Arc;

const DEFAULT_TERM_SIZE: (u16, u16) = (1, 1);

fn get_size_uncached() -> (u16, u16) {
    termion::terminal_size().unwrap_or(DEFAULT_TERM_SIZE)
}

enum Signal {
    Reload,
    Exit,
    Wait,
}

struct State {
    signal: Signal,
    size: (u16, u16),
}

struct Shared {
    state: Mutex<State>,
    notification: Condvar,
}

pub struct TerminalSizeCache {
    shared: Arc<Shared>,
}

impl TerminalSizeCache {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                signal: Signal::Wait,
                size: get_size_uncached(),
            }),
            notification: Condvar::new(),
        });

        let shared_ref = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut guard = shared_ref.state.lock();
            loop {
                let State { signal, size } = &mut *guard;
                match *signal {
                    Signal::Reload => {
                        *size = get_size_uncached();
                        *signal = Signal::Wait;
                    }
                    Signal::Exit => break,
                    Signal::Wait => shared_ref.notification.wait(&mut guard),
                }
            }
        });

        Self { shared }
    }

    pub fn fetch_size(&self) -> (u16, u16) {
        let mut guard = self.shared.state.lock();
        let &mut State {
            ref mut signal,
            size,
        } = &mut *guard;
        *signal = Signal::Reload;
        drop(guard);
        self.shared.notification.notify_one();
        size
    }

    // if this was a crate
    // pub fn fetch_size_force(&self) -> (u16, u16) {
    //     let mut guard = self.shared.state.lock();
    //     let State { signal, size } = &mut *guard;
    //     *signal = Signal::Wait;
    //     *size = get_size_uncached();
    //     *size
    // }
}

impl Drop for TerminalSizeCache {
    fn drop(&mut self) {
        // signal an exit
        let mut guard = self.shared.state.lock();
        guard.signal = Signal::Exit;
        drop(guard);
        self.shared.notification.notify_one();
    }
}
