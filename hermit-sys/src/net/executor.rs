/// An executor, which is run when idling on network I/O.
use crate::net::nic;
use async_task::{Runnable, Task};
use concurrent_queue::ConcurrentQueue;
use futures_lite::pin;
use smoltcp::time::{Duration, Instant};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
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
	fn sys_set_network_polling_mode(polling: bool);
	fn sys_block_current_task_with_timeout(timeout: u64);
	fn sys_block_current_task();
}

pub(crate) struct PollingMode;

impl PollingMode {
	pub unsafe fn on(&mut self) {
		sys_set_network_polling_mode(true);
	}

	pub unsafe fn off(&mut self) {
		sys_set_network_polling_mode(false);
	}
}

lazy_static! {
	pub(crate) static ref POLLING_MODE: Mutex<PollingMode> = Mutex::new(PollingMode);
}

pub(crate) struct PollingGuard {
	polling_mode: MutexGuard<'static, PollingMode>,
}

impl PollingGuard {
	pub fn new() -> Self {
		let mut polling_mode = POLLING_MODE.lock().unwrap();
		unsafe {
			polling_mode.on();
		}
		Self { polling_mode }
	}
}

impl Drop for PollingGuard {
	fn drop(&mut self) {
		unsafe { self.polling_mode.off() }
		execute_all();
		nic::lock().with(|nic| nic.poll(Instant::now()));
	}
}

lazy_static! {
	pub static ref QUEUE: ConcurrentQueue<Runnable> = ConcurrentQueue::unbounded();
}

pub(crate) fn run_executor() {
	trace!("running executor");
	let polling = PollingGuard::new();
	execute_all();
	loop {
		nic::lock().with(|nic| nic.poll(Instant::now()));
		execute_all();
		match nic::lock().with(|nic| nic.was_woken()) {
			true => continue,
			false => break,
		}
	}
	drop(polling)
}

pub(crate) fn execute_all() {
	while let Ok(runnable) = QUEUE.pop() {
		runnable.run();
	}
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

	pub fn reset(&self) {
		self.woken.store(false, Ordering::Relaxed);
		self.unparked.store(false, Ordering::Release);
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

thread_local! {
	static CURRENT_THREAD_NOTIFY: Arc<ThreadNotify> = Arc::new(ThreadNotify::new());
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
		let mut polling = Some(PollingGuard::new());
		loop {
			thread_notify.reset();
			execute_all();
			nic::lock().with(|nic| nic.poll(Instant::now()));
			if let Poll::Ready(t) = future.as_mut().poll(&mut cx) {
				trace!(
					"blocking future on thread {} is ready!",
					thread_notify.thread
				);
				drop(polling.take());
				return Ok(t);
			} else {
				while !thread_notify.was_woken() {
					// check wheter to time out
					if let Some(duration) = timeout {
						if Instant::now() >= start + duration {
							return Err(io::Error::new(
								io::ErrorKind::TimedOut,
								&"executor timed out",
							));
						}
					}

					// run executor before blocking
					polling.get_or_insert_with(|| PollingGuard::new());
					trace!("checking network delay");
					let delay = nic::lock()
						.with(|nic| nic.poll_delay(Instant::now()))
						.map(|d| d.total_millis());
					execute_all();
					trace!("delay is {:?}", delay);

					// wait for the advised delay if it's greater than 100ms
					if !thread_notify.was_woken() && (delay.is_none() || delay.unwrap() > 1000) {
						drop(polling.take());
						warn!("blocking task");
						// deactivate the polling_mode when blocking
						if !thread_notify.swap_unparked() {
							unsafe {
								match delay {
									Some(d) => sys_block_current_task_with_timeout(d),
									None => sys_block_current_task(),
								};
								sys_yield();
							}
						}
					}
					execute_all();
					nic::lock().with(|nic| nic.poll(Instant::now()));
				}
				trace!("thread_notify was woken!");
			}
		}
	})
}
