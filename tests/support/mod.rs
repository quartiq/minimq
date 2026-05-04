use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    sync::OnceLock,
    task::{Context, Poll, Wake, Waker},
};

struct NoopWaker;

impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

pub fn init_host_logging() {
    static HOST_LOGGING: OnceLock<()> = OnceLock::new();

    HOST_LOGGING.get_or_init(|| {
        let _ = env_logger::builder().is_test(true).try_init();
        defmt2log::init_from_current_exe().expect("initialize defmt host logger");
    });
}

#[allow(dead_code)]
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut cx = noop_context();
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[allow(dead_code)]
pub fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut cx = noop_context();
    future.poll(&mut cx)
}

fn noop_context() -> Context<'static> {
    let waker = Waker::from(Arc::new(NoopWaker));
    Context::from_waker(Box::leak(Box::new(waker)))
}
