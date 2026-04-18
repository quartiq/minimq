use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

struct NoopWaker;

impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

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
