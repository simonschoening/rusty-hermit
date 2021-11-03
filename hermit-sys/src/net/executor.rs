/// An executor, which is run when idling on network I/O.
use crate::net::network_delay;
use crate::net::nic;
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
	unsafe {
		sys_set_network_polling_mode(true);
	}
	execute_all();
	unsafe {
		sys_set_network_polling_mode(false);
	}
}

fn execute_all() {
	while let Ok(runnable) = QUEUE.pop() {
		runnable.run();
	}
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
	let schedule = |runnable| QUEUE.push(runnable).unwrap();
	let (runnable, task) = async_task::spawn(future, schedule);
	runnable.schedule();
	task
}

struct ThreadNotify {
	/// The (single) executor thread.
	thread: Tid,
	/// A flag to ensure a wakeup is not "forgotten" before the next `block_current_task`
	unparked: AtomicBool,
	/// A flag to show that a wakeup occured
	woken: AtomicBool,
}

impl ThreadNotify {
	pub fn new() -> Self {
		Self {
			thread: unsafe { sys_getpid() },
			unparked: AtomicBool::new(false),
			woken: AtomicBool::new(false),
		}
	}

	pub fn was_woken(&self) -> bool {
		self.woken.load(Ordering::Relaxed)
	}

	pub fn swap_unparked(&self) -> bool {
		self.unparked.swap(false, Ordering::Relaxed)
	}

	pub fn reset_unparked(&self) {
		self.unparked.store(false, Ordering::Release);
	}

	pub fn reset(&self) {
		self.woken.store(false, Ordering::Relaxed);
		self.unparked.store(false, Ordering::Release);
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
		trace!("waking thread_notify of Thread {}", self.thread);
		self.woken.store(true, Ordering::Release);
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
			} else {
				while !thread_notify.was_woken() {
					trace!("polling network");
					let delay = network_delay(Instant::now()).map(|d| d.total_millis());

					trace!("ignoring advisory delay of {:?}ms", delay);

					execute_all();

					if let Some(duration) = timeout {
						if Instant::now() >= start + duration {
							unsafe {
								sys_set_network_polling_mode(false);
							}
							return Err(io::Error::new(
								io::ErrorKind::TimedOut,
								"executor timed out",
							));
						}
					}
				}
			}
		}
	})
}

/// Blocks the current thread on `f`, running the executor when idling.
pub fn block_on<F, T>(future: F, timeout: Option<Duration>) -> io::Result<T>
where
	F: Future<Output = T>,
{
	CURRENT_THREAD_NOTIFY.with(|thread_notify| {
		let start = Instant::now();

		let waker = thread_notify.clone().into();
		let mut cx = Context::from_waker(&waker);
		pin!(future);
		loop {
			thread_notify.reset();
			if let Poll::Ready(t) = future.as_mut().poll(&mut cx) {
				trace!(
					"blocking future on thread {} is ready!",
					thread_notify.thread
				);
				return Ok(t);
			} else {
				while !thread_notify.was_woken() {
					// check wheter to time out
					if let Some(duration) = timeout {
						if Instant::now() >= start + duration {
							return Err(io::Error::new(
								io::ErrorKind::TimedOut,
								"executor timed out",
							));
						}
					}

					run_executor();

					// when to poll the network for progress
					trace!("checking network delay");
					let delay = network_delay(Instant::now()).map(|d| d.total_millis());
					debug!("delay is {:?}", delay);

					// wait for the advised delay if it's greater than 100ms
					if !thread_notify.was_woken() && (delay.is_none() || delay.unwrap() > 100) {
						unsafe {
							sys_block_current_task_with_timeout(delay.unwrap_or(500));
							if thread_notify.swap_unparked() {
								debug!("not blocking! thread_notify was already unparked");
								sys_wakeup_task(thread_notify.thread);
							}
							sys_yield();
						}
					}

					run_executor();

					// now wake nic so it may poll
					nic::lock().with(|nic| nic.wake());
				}
			}
			trace!("thread_notify was woken!");
		}
	})
}
