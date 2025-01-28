use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Notify;

#[inline(always)]
const fn to_word((x, y): (u16, u16)) -> u32 {
    ((x as u32) << 16) | y as u32
}

#[inline(always)]
const fn from_word(word: u32) -> (u16, u16) {
    let lower = word as u16;
    let upper = (word >> 16) as u16;
    (upper, lower)
}

const DEFAULT_TERM_SIZE: (u16, u16) = (1, 1);

pub fn get_size_uncached() -> (u16, u16) {
    termion::terminal_size().unwrap_or(DEFAULT_TERM_SIZE)
}

struct Shared {
    exit: Notify,
    reload: Notify,
    data: AtomicU32
}

pub struct TerminalSizeCache {
    shared: Arc<Shared>
}

impl TerminalSizeCache {
    pub fn new()  -> Self {
        let load_size = || to_word(get_size_uncached());
        let shared = Arc::new(Shared {
            exit: Notify::new(),
            reload: Notify::new(),
            data: AtomicU32::new(load_size())
        });
        let shared_ref = Arc::clone(&shared);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async move {
                let Shared { exit, reload, data } = &*shared_ref;
                loop {
                    tokio::select! {
                        _ = exit  .notified() => break,
                        _ = reload.notified() => data.store(load_size(), Ordering::Relaxed)
                    }
                }
            })
        });


        Self {
            shared
        }
    }

    pub fn fetch_size(&self) -> (u16, u16) {
        let Shared { reload, data, .. } = &*self.shared;
        reload.notify_waiters();
        from_word(data.load(Ordering::Relaxed))
    }
}

impl Drop for TerminalSizeCache {
    fn drop(&mut self) {
        self.shared.exit.notify_one();
    }
}