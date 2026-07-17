//! Single-threaded async runtime for no_std CLUU userspace.
//!
//! Provides a minimal executor that bridges IPC replies to task wakers via
//! cookie correlation. Designed for server event loops that need to handle
//! multiple in-flight IPC requests concurrently without blocking.
//!
//! # Usage
//!
//! ```ignore
//! let mut rt = async_runtime::Runtime::new(token_self);
//! rt.spawn(async {
//!     let reply = IpcCallFuture::new(ep, msg).await.unwrap();
//!     // ... use reply ...
//! });
//! // In the main loop:
//! loop {
//!     rt.poll_ready();
//!     let tokens = [server_ep, rt.reply_endpoint()];
//!     match ipc_recv_any_with_sender(&tokens, &mut buf, timeout) {
//!         Ok((idx, len, _)) if idx == 1 => {
//!             let (msg, payload_len) = parse_reply(&buf[..len]);
//!             rt.deliver_reply(msg.words[5], msg, payload_len);
//!         }
//!         // ... handle server messages, spawn more tasks ...
//!         _ => {}
//!     }
//! }
//! ```
//!
//! # Safety
//!
//! The runtime uses a single-threaded execution model. `CURRENT_RUNTIME` is
//! an `AtomicPtr` accessed only from the single thread running the executor.
//! No `Send` or `Sync` bounds are required on futures.

use crate::error::{Error, Result};
use crate::syscall;
use crate::types::Message;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::any::Any;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicPtr, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// ---------------------------------------------------------------------------
// Current runtime pointer (single-threaded, set by poll_ready)
// ---------------------------------------------------------------------------

static CURRENT_RUNTIME: AtomicPtr<Runtime> = AtomicPtr::new(core::ptr::null_mut());

/// # Panics
/// Panics if called outside of a runtime context (i.e., outside `poll_ready`
/// or `block_on`). This is a programming error.
fn current_runtime() -> &'static mut Runtime {
    let ptr = CURRENT_RUNTIME.load(Ordering::Relaxed);
    if ptr.is_null() {
        panic!("async_runtime: no current runtime — call from within Runtime::poll_ready or block_on");
    }
    // SAFETY: Single-threaded. The pointer is set by `poll_ready` / `block_on`
    // and remains valid for the duration of the poll. No concurrent access.
    unsafe { &mut *ptr }
}

/// Push a completion from within an async task. The main loop drains
/// completions after `poll_ready()` to do `&mut self` work that can't
/// happen inside a task (e.g. fd table allocation).
pub fn push_completion<T: Any>(completion: T) {
    let rt = current_runtime();
    rt.completions.push_back(Box::new(completion));
}

// ---------------------------------------------------------------------------
// Task waker — encodes TaskId in the raw waker, pushes to ready_queue on wake
// ---------------------------------------------------------------------------

const NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
    waker_clone,
    waker_wake,
    waker_wake_by_ref,
    waker_drop,
);

fn waker_clone(ptr: *const ()) -> RawWaker {
    RawWaker::new(ptr, &NOOP_VTABLE)
}

fn waker_wake(ptr: *const ()) {
    let task_id = ptr as TaskId;
    let rt_ptr = CURRENT_RUNTIME.load(Ordering::Relaxed);
    if rt_ptr.is_null() {
        return;
    }
    unsafe { (*rt_ptr).ready_queue.push_back(task_id) };
}

fn waker_wake_by_ref(ptr: *const ()) {
    let task_id = ptr as TaskId;
    let rt_ptr = CURRENT_RUNTIME.load(Ordering::Relaxed);
    if rt_ptr.is_null() {
        return;
    }
    unsafe { (*rt_ptr).ready_queue.push_back(task_id) };
}

fn waker_drop(_ptr: *const ()) {}

fn task_waker(task_id: TaskId) -> Waker {
    unsafe { Waker::from_raw(RawWaker::new(task_id as *const (), &NOOP_VTABLE)) }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

type TaskId = usize;

struct Task {
    future: Pin<Box<dyn Future<Output = ()> + 'static>>,
}

/// Single-threaded async executor with IPC reply bridging.
///
/// The runtime owns:
/// - A set of tasks (boxed futures)
/// - A ready queue of task IDs to poll next
/// - A pending map: cookie → task waiting for that IPC reply
/// - A reply map: cookie → reply data (delivered by the recv loop)
/// - A dedicated reply endpoint (created at construction)
///
/// The caller integrates the runtime into their server's recv loop:
/// 1. Call `poll_ready()` to drain ready tasks
/// 2. Call `ipc_recv_any` on `[server_ep, reply_endpoint]`
/// 3. If the reply endpoint fired, call `deliver_reply(cookie, msg, len)`
/// 4. For new client requests, `spawn` async tasks as needed
/// 5. Loop
pub struct Runtime {
    tasks: BTreeMap<TaskId, Task>,
    pending_cookies: BTreeMap<usize, TaskId>,
    cookie_targets: BTreeMap<usize, usize>,
    replies: BTreeMap<usize, (Message, Vec<u8>)>,
    ready_queue: VecDeque<TaskId>,
    retry_queue: VecDeque<TaskId>,
    cancelled_cookies: VecDeque<usize>,
    next_task_id: TaskId,
    next_cookie: usize,
    reply_endpoint: usize,
    current_task_id: Option<TaskId>,
    completions: VecDeque<Box<dyn Any>>,
}

impl Runtime {
    /// Create a new runtime with a dedicated reply endpoint.
    ///
    /// The reply endpoint is created via `syscall::endpoint_create(token_self)`
    /// and is used to receive asynchronous IPC replies. The caller should add
    /// `rt.reply_endpoint()` to their `ipc_recv_any` token slice.
    pub fn new(token_self: usize) -> Result<Self> {
        let reply_endpoint = syscall::endpoint_create(token_self)?;
        Ok(Self {
            tasks: BTreeMap::new(),
            pending_cookies: BTreeMap::new(),
            cookie_targets: BTreeMap::new(),
            replies: BTreeMap::new(),
            ready_queue: VecDeque::new(),
            retry_queue: VecDeque::new(),
            cancelled_cookies: VecDeque::new(),
            next_task_id: 0,
            next_cookie: 1,
            reply_endpoint,
            current_task_id: None,
            completions: VecDeque::new(),
        })
    }

    /// The reply endpoint token. Add this to your `ipc_recv_any` token slice
    /// so the runtime can receive IPC replies.
    pub fn reply_endpoint(&self) -> usize {
        self.reply_endpoint
    }

    /// Spawn a new task. The future must be `'static` (no borrowed references
    /// with non-'static lifetimes). To capture backend references, extend
    /// their lifetime to `'static` — safe in the single-threaded VFS server
    /// where the server never drops.
    pub fn spawn(&mut self, fut: impl Future<Output = ()> + 'static) {
        let id = self.next_task_id;
        self.next_task_id += 1;
        self.tasks.insert(id, Task {
            future: Box::pin(fut),
        });
        self.ready_queue.push_back(id);
    }

    /// Whether the runtime has any live tasks or pending IPC replies.
    /// Use this to decide the recv timeout: if tasks are pending, use a
    /// short timeout; if idle, block forever.
    pub fn has_pending(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// Poll all ready tasks. Call this before `ipc_recv_any` in your loop.
    ///
    /// Sets `CURRENT_RUNTIME` so that `IpcCallFuture::poll` can register
    /// pending cookies and allocate reply correlation state.
    pub fn poll_ready(&mut self) {
        let self_ptr: *mut Runtime = self;
        let old = CURRENT_RUNTIME.swap(self_ptr, Ordering::Relaxed);

        self.ready_queue.append(&mut self.retry_queue);

        while let Some(task_id) = self.ready_queue.pop_front() {
            let task = match self.tasks.get_mut(&task_id) {
                Some(t) => t,
                None => continue,
            };

            self.current_task_id = Some(task_id);

            let waker = task_waker(task_id);
            let mut cx = Context::from_waker(&waker);

            let poll_result = task.future.as_mut().poll(&mut cx);

            match poll_result {
                Poll::Ready(()) => {
                    self.tasks.remove(&task_id);
                    self.pending_cookies.retain(|_, tid| *tid != task_id);
                }
                Poll::Pending => {}
            }
        }

        self.current_task_id = None;

        self.detect_dead_tasks();

        CURRENT_RUNTIME.store(old, Ordering::Relaxed);
    }

    /// Deliver an IPC reply to the waiting task. Called when the recv loop
    /// receives a message on the reply endpoint.
    ///
    /// - `cookie`: the correlation cookie (from `msg.words[5]`)
    /// - `msg`: the reply Message header
    /// - `payload`: the reply payload bytes (after the Message header)
    pub fn deliver_reply(&mut self, cookie: usize, msg: Message, payload: Vec<u8>) {
        self.replies.insert(cookie, (msg, payload));
        self.cookie_targets.remove(&cookie);
        if let Some(task_id) = self.pending_cookies.remove(&cookie) {
            self.ready_queue.push_back(task_id);
        }
    }

    /// Allocate the next cookie value. Called by `IpcCallFuture::new`.
    fn alloc_cookie(&mut self) -> usize {
        let c = self.next_cookie;
        self.next_cookie += 1;
        c
    }

    /// Register a pending cookie for the current task. Called by
    /// `IpcCallFuture::poll` when transitioning to the Waiting state.
    fn register_pending(&mut self, cookie: usize, task_id: TaskId, endpoint: usize) {
        self.pending_cookies.insert(cookie, task_id);
        self.cookie_targets.insert(cookie, endpoint);
    }

    fn take_reply(&mut self, cookie: usize) -> Option<(Message, Vec<u8>)> {
        self.replies.remove(&cookie)
    }

    pub fn cancel_endpoint(&mut self, endpoint: usize) {
        let mut to_cancel = alloc::vec::Vec::new();
        for (&cookie, &ep) in &self.cookie_targets {
            if ep == endpoint {
                to_cancel.push(cookie);
            }
        }
        for cookie in to_cancel {
            self.cookie_targets.remove(&cookie);
            if let Some(task_id) = self.pending_cookies.remove(&cookie) {
                self.cancelled_cookies.push_back(cookie);
                self.ready_queue.push_back(task_id);
            }
        }
    }

    fn take_cancelled_cookie(&mut self, cookie: usize) -> bool {
        if let Some(idx) = self.cancelled_cookies.iter().position(|&c| c == cookie) {
            self.cancelled_cookies.swap_remove_back(idx);
            true
        } else {
            false
        }
    }

    fn detect_dead_tasks(&mut self) {
        let pending_task_ids: alloc::collections::BTreeSet<TaskId> =
            self.pending_cookies.values().copied().collect();
        let live_count = self.tasks.len();
        let mut dead = alloc::vec::Vec::new();
        for (&id, _) in &self.tasks {
            if !pending_task_ids.contains(&id) {
                dead.push(id);
            }
        }
        for id in dead {
            let _ = crate::debug_print(&alloc::format!(
                "async_runtime: dead task {} — Pending with no waker (bug in future)\n",
                id
            ));
            self.tasks.remove(&id);
        }
        let leaked = live_count.saturating_sub(self.tasks.len());
        if leaked > 0 {
            let _ = crate::debug_print(&alloc::format!(
                "async_runtime: reaped {} dead task(s)\n", leaked
            ));
        }
    }

    pub fn push_completion<T: Any>(&mut self, completion: T) {
        self.completions.push_back(Box::new(completion));
    }

    pub fn pop_completion(&mut self) -> Option<Box<dyn Any>> {
        self.completions.pop_front()
    }
}

// ---------------------------------------------------------------------------
// IpcCallFuture — async IPC request/reply via cookie correlation
// ---------------------------------------------------------------------------

/// A future that sends an IPC request and awaits the reply.
///
/// The request Message must have `words[4]` and `words[5]` available for
/// the reply endpoint and cookie. `IpcCallFuture::new` fills these in
/// automatically from the current runtime.
///
/// The reply arrives on the runtime's dedicated reply endpoint. The runtime
/// matches the cookie (from `words[5]` in the reply) to the waiting task
/// and delivers the reply data.
///
/// # Wire format
///
/// Request: `words[4] = reply_endpoint`, `words[5] = cookie`
/// Reply:   `words[5] = cookie` (echoed back by the responder)
pub struct IpcCallFuture {
    endpoint: usize,
    send_buf: Vec<u8>,
    cookie: usize,
    state: IpcCallState,
}

enum IpcCallState {
    NotSent,
    Waiting,
    Done,
}

impl IpcCallFuture {
    pub fn new(endpoint: usize, mut request: Message) -> Self {
        Self::new_with_payload(endpoint, &mut request, &[])
    }

    pub fn new_with_payload(endpoint: usize, request: &mut Message, payload: &[u8]) -> Self {
        let rt = current_runtime();
        let cookie = rt.alloc_cookie();
        let reply_ep = rt.reply_endpoint;

        request.words[0] = payload.len();
        request.words[4] = reply_ep;
        request.words[5] = cookie;
        request.tag.extra = crate::ipc::ASYNC_REPLY_TAG;

        let mut send_buf = Vec::with_capacity(core::mem::size_of::<Message>() + payload.len());
        send_buf.extend_from_slice(request.as_bytes());
        send_buf.extend_from_slice(payload);

        Self {
            endpoint,
            send_buf,
            cookie,
            state: IpcCallState::NotSent,
        }
    }
}

impl Future for IpcCallFuture {
    type Output = Result<(Message, Vec<u8>)>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.state {
            IpcCallState::NotSent => {
                match syscall::ipc_send(self.endpoint, &self.send_buf) {
                    Ok(()) => {
                        self.state = IpcCallState::Waiting;
                        let rt = current_runtime();
                        let task_id = rt.current_task_id.expect(
                            "IpcCallFuture::poll: no current task — \
                             must be called from within a spawned task",
                        );
                        rt.register_pending(self.cookie, task_id, self.endpoint);
                        Poll::Pending
                    }
                    Err(Error::WouldBlock) => {
                        let rt = current_runtime();
                        if let Some(tid) = rt.current_task_id {
                            rt.retry_queue.push_back(tid);
                        }
                        Poll::Pending
                    }
                    Err(e) => {
                        self.state = IpcCallState::Done;
                        let rt = current_runtime();
                        rt.pending_cookies.remove(&self.cookie);
                        rt.cookie_targets.remove(&self.cookie);
                        Poll::Ready(Err(e))
                    }
                }
            }
            IpcCallState::Waiting => {
                let rt = current_runtime();
                if rt.take_cancelled_cookie(self.cookie) {
                    self.state = IpcCallState::Done;
                    return Poll::Ready(Err(Error::NotFound));
                }
                match rt.take_reply(self.cookie) {
                    Some((msg, payload)) => {
                        self.state = IpcCallState::Done;
                        Poll::Ready(Ok((msg, payload)))
                    }
                    None => {
                        Poll::Pending
                    }
                }
            }
            IpcCallState::Done => {
                panic!("IpcCallFuture polled after completion");
            }
        }
    }
}
