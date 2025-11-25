use parking_lot::{Condvar, Mutex};
use std::sync::{Arc, Weak};

enum RenderState {
    None,
    HasSample { sample: gst::Sample, pulled: bool },
    Closed,
}

struct RenderingContext {
    state: Mutex<RenderState>,
    sample_notification: Condvar,
}

struct RenderingContextPipe(Arc<RenderingContext>);

impl Drop for RenderingContextPipe {
    fn drop(&mut self) {
        let mut lock = self.0.state.lock();
        *lock = RenderState::Closed;
        drop(lock);
        self.0.sample_notification.notify_one();
    }
}

#[derive(Clone)]
pub struct SampleProducer(Arc<RenderingContextPipe>);

impl SampleProducer {
    pub fn push_sample(&self, sample: gst::Sample) -> Result<(), ()> {
        let this: &RenderingContext = &*self.0.0;

        let mut lock = this.state.lock();
        match &mut *lock {
            // still rendering...
            RenderState::HasSample {
                sample: old_sample,
                pulled: false,
            } => *old_sample = sample,
            RenderState::Closed => return Err(()),
            slot => {
                *slot = RenderState::HasSample {
                    sample,
                    pulled: false,
                };
                drop(lock);
                this.sample_notification.notify_one();
            }
        }

        Ok(())
    }

    pub fn close(&self) {
        let this: &RenderingContext = &*self.0.0;
        *this.state.lock() = RenderState::Closed;
        this.sample_notification.notify_one();
    }
}

pub struct SampleConsumer(RenderingContextPipe);

impl SampleConsumer {
    pub fn pull_sample(&self) -> Result<gst::Sample, ()> {
        let this: &RenderingContext = &*self.0.0;

        let mut lock = this.state.lock();
        loop {
            match &mut *lock {
                RenderState::None | RenderState::HasSample { pulled: true, .. } => {
                    this.sample_notification.wait(&mut lock)
                }
                RenderState::HasSample {
                    sample,
                    pulled: pulled @ false,
                } => {
                    *pulled = true;
                    break Ok(sample.clone());
                }
                RenderState::Closed => return Err(()),
            }
        }
    }

    pub fn make_reloader(&self) -> SampleReloader {
        SampleReloader(Arc::downgrade(&self.0.0))
    }
}

pub struct SampleReloader(Weak<RenderingContext>);

impl SampleReloader {
    pub fn reload_sample(&self) -> Result<(), ()> {
        let Some(this) = self.0.upgrade() else {
            return Err(());
        };

        let this: &RenderingContext = &this;

        let mut lock = this.state.lock();
        match &mut *lock {
            RenderState::None => Ok(()),
            RenderState::HasSample { pulled, .. } => {
                *pulled = false;
                drop(lock);
                this.sample_notification.notify_one();
                Ok(())
            }
            RenderState::Closed => Err(()),
        }
    }
}

pub fn video_pipe() -> (SampleProducer, SampleConsumer) {
    let ctx = Arc::new(
        const {
            RenderingContext {
                state: Mutex::new(RenderState::None),
                sample_notification: Condvar::new(),
            }
        },
    );

    let pipe1 = RenderingContextPipe(Arc::clone(&ctx));
    let pipe2 = RenderingContextPipe(Arc::clone(&ctx));

    (SampleProducer(Arc::new(pipe1)), SampleConsumer(pipe2))
}
