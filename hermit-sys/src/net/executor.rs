/// An executor, which is run when idling on network I/O.
use crate::net::network_delay;
use async_task::{Runnable, Task};
use concurrent_queue::ConcurrentQueue;
use futures_lite::pin;
use smoltcp::time::{Duration, Instant};
use std::sync::atomic::Ordering;
use std::{
	future::Future,
	sync::{atomic::AtomicBool, Arc},
	task::{Context, Poll, Wake},
};

use hermit_abi::io;

/// A thread handle type
type Tid = u32;

extern "C" {
	fn sys_getpid() -> Tid;
	fn sys_yield();
	fn sys_wakeup_task(tid: Tid);
	fn sys_set_network_polling_mode(value: bool);
	fn sys_block_current_task_with_timeout(timeout: u64);
	fn sys_block_current_task();
}

lazy_static! {
	static ref QUEUE: ConcurrentQueue<Runnable> = ConcurrentQueue::unbounded();
}

pub(crate) fn run_executor() {
    // execute all futures and reschedule them
    // ToDo: don't wake every Runnable immediatly
    //          -> mark futures safe to be detached, if they 
    //             register a waker before Pending 
    let mut wake_buf = Vec::with_capacity(QUEUE.len());
	unsafe { sys_set_network_polling_mode(true) };
	while let Ok(runnable) = QUEUE.pop() {
        wake_buf.push(runnable.waker());
        let waker = runnable.waker();
		runnable.run();
	}
	unsafe { sys_set_network_polling_mode(false) };
    for waker in wake_buf { waker.wake() };

}

/// Spawns a future on the executor.
///
/// if a future has not registered a waker
/// and it's task is never polled, it will leak memory
#[must_use]
pub fn spawn<F, T>(future: F) -> Task<T>
where
	F: Future<Output = T> + Send + 'static,
	T: Send + 'static,
{
	let schedule = |runnable| { 
        QUEUE.push(runnable).unwrap() 
    };
	let (runnable, task) = async_task::spawn(future, schedule);
	runnable.schedule();
	task
}

struct ThreadNotify {
	/// The (single) executor thread.
	thread: Tid,
	/// A flag to ensure a wakeup is not "forgotten" before the next `block_current_task`
	unparked: AtomicBool,
}

impl ThreadNotify {
	pub fn new() -> Self {
		Self {
			thread: unsafe { sys_getpid() },
			unparked: AtomicBool::new(false),
		}
	}
}

impl Drop for ThreadNotify {
	fn drop(&mut self) {
		println!("Dropping ThreadNotify!");
	}
}

impl Wake for ThreadNotify {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref()
	}

	fn wake_by_ref(self: &Arc<Self>) {
		// Make sure the wakeup is remembered until the next `park()`.
		let unparked = self.unparked.swap(true, Ordering::Relaxed);
		if !unparked {
			unsafe {
				sys_wakeup_task(self.thread);
			}
		}
	}
}

thread_local! {
	static CURRENT_THREAD_NOTIFY: Arc<ThreadNotify> = Arc::new(ThreadNotify::new());
}

pub fn poll_on<F, T>(future: F, timeout: Option<Duration>) -> io::Result<T>
where
	F: Future<Output = T>,
{
	CURRENT_THREAD_NOTIFY.with(|thread_notify| {
		unsafe {
			sys_set_network_polling_mode(true);
		}

		let start = Instant::now();
		let waker = thread_notify.clone().into();
		let mut cx = Context::from_waker(&waker);
		pin!(future);

		loop {
			if let Poll::Ready(t) = future.as_mut().poll(&mut cx) {
				unsafe {
					sys_set_network_polling_mode(false);
				}
				return Ok(t);
			}

			if let Some(duration) = timeout {
				if Instant::now() >= start + duration {
					unsafe {
						sys_set_network_polling_mode(false);
					}
					return Err(io::Error { 
                        kind: io::ErrorKind::TimedOut, 
                        msg: "executor timed out" 
                    });
				}
			}

			run_executor()
		}
	})
}

/// Blocks the current thread on `f`, running the executor when idling.
pub fn block_on<F, T>(future: F, timeout: Option<Duration>) -> io::Result<T>
where
	F: Future<Output = T>,
{
	CURRENT_THREAD_NOTIFY.with(|thread_notify| {
		unsafe { sys_set_network_polling_mode(true) }
		let start = Instant::now();

		let waker = thread_notify.clone().into();
		let mut cx = Context::from_waker(&waker);
		pin!(future);
		loop {
            debug!("polling blocking future");
			if let Poll::Ready(t) = future.as_mut().poll(&mut cx) {
				return Ok(t);
			}

			if let Some(duration) = timeout {
				if Instant::now() >= start + duration {
					return Err(io::Error { 
                        kind: io::ErrorKind::TimedOut, 
                        msg: "executor timed out" 
                    });
				}
			}

            debug!("polling network");
            let delay = network_delay(start).map(|d| d.total_millis());

			if delay.is_none() || delay.unwrap() > 100 {
				let unparked = thread_notify.unparked.swap(false, Ordering::Acquire);
				if !unparked {
					unsafe {
						match delay {
							Some(d) => sys_block_current_task_with_timeout(d),
							None => sys_block_current_task(),
						};
						sys_yield();
					}
					thread_notify.unparked.store(false, Ordering::Release);
					run_executor()
				}
			} else {
				run_executor()
			}
		}
	})
}
