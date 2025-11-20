use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TERM_SIZE: (u16, u16) = (1, 1);

fn get_size_uncached() -> (u16, u16) {
    termion::terminal_size().unwrap_or(DEFAULT_TERM_SIZE)
}

enum Signal {
    Active,
    Exit,
}

struct Shared {
    state: Mutex<Signal>,
    notification: Condvar,
}

pub struct TerminalSizeUpdater {
    shared: Arc<Shared>,
}

impl TerminalSizeUpdater {
    fn new_inner(
        periodic_interval: Duration,
        mut on_size_change: Box<dyn FnMut((u16, u16)) + Send>
    ) -> Self {
        let initial_size = get_size_uncached();
        on_size_change(initial_size);

        let shared = Arc::new(const {
            Shared {
                state: Mutex::new(Signal::Active),
                notification: Condvar::new(),
            }
        });


        let shared_ref = Arc::clone(&shared);
        let interval = periodic_interval;
        std::thread::spawn(move || {
            let mut last_size = initial_size;
            let mut guard = shared_ref.state.lock();
            loop {
                let signal = &mut *guard;

                match signal {
                    Signal::Active => {
                        let new_size = get_size_uncached();
                        let old_size = core::mem::replace(&mut last_size, new_size);
                        if old_size != new_size {
                            on_size_change(new_size)
                        }

                        let _ = shared_ref.notification.wait_for(&mut guard, interval);
                    }
                    Signal::Exit => break,
                }
            }
        });


        Self { shared }
    }

    pub fn new(
        periodic_interval: Duration,
        on_size_change: impl FnMut((u16, u16)) + Send + 'static
    ) -> Self {
        Self::new_inner(periodic_interval, Box::new(on_size_change))
    }

    pub fn trigger_reload(&self) {
        self.shared.notification.notify_one();
    }
}

impl Drop for TerminalSizeUpdater {
    fn drop(&mut self) {
        // signal an exit
        let mut guard = self.shared.state.lock();
        *guard = Signal::Exit;
        drop(guard);
        self.shared.notification.notify_one();
    }
}
