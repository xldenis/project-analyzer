//! Fixtures with statically-predictable async future (coroutine) sizes.
//!
//! Each `pub async fn` here is constructed so that its coroutine layout can
//! be predicted from first principles, without depending on optimizer choices.
//! See `EXPECTED.md` for the predicted size of each item and the reasoning.
//!
//! Build with the project's pinned toolchain, then run the analyzer in the
//! `default` (size) mode against this file.

#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// ---------------------------------------------------------------------------
// Helper: a pending-once future whose layout is known exactly.
//
//   sizeof(PendOnce<N>) == 1 + N   (no padding: all fields are u8-aligned)
//
// First poll returns `Pending` and flips `polled`; second poll returns `Ready`.
// The field `pad` exists only to give the future a configurable, known size,
// so async fns that `.await` it have a predictable lower bound on coroutine
// state.
// ---------------------------------------------------------------------------
#[repr(C)]
pub struct PendOnce<const N: usize> {
    polled: bool,
    pad: [u8; N],
}

impl<const N: usize> PendOnce<N> {
    pub fn new() -> Self {
        Self {
            polled: false,
            pad: [0; N],
        }
    }
}

impl<const N: usize> Future for PendOnce<N> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: we never move out of the pinned reference.
        let s = unsafe { self.get_unchecked_mut() };
        if s.polled {
            Poll::Ready(())
        } else {
            s.polled = true;
            Poll::Pending
        }
    }
}

// ===========================================================================
// FIXTURE 1 — `empty`
// No locals, no awaits. The coroutine has only the implicit states
// (Unresumed / Returned / Panicked); no yield variant.
// Predicted size: 1 byte (discriminant only).
// ===========================================================================
pub async fn empty() {}

// ===========================================================================
// FIXTURE 2 — `locals_no_await`
// Large local, but no await point exists, so nothing has to be persisted in
// coroutine state. Same shape as `empty`.
// Predicted size: 1 byte.
// ===========================================================================
pub async fn locals_no_await() -> u8 {
    let a: [u8; 128] = [0; 128];
    a[0]
}

// ===========================================================================
// FIXTURE 3 — `await_one_byte`
// One await of `PendOnce<0>` (size 1). The suspend-at-yield variant of the
// coroutine must hold the inner future across the await.
//   max variant payload = 1
//   discriminant        = 1
// Predicted size: 2 bytes (lower bound; niche-packing into the inner `bool`
// could in principle compress this to 1 — see EXPECTED.md).
// ===========================================================================
pub async fn await_one_byte() {
    PendOnce::<0>::new().await;
}

// ===========================================================================
// FIXTURE 4 — `await_257`
// Inner future is `PendOnce<256>` = 257 bytes. The suspend variant of the
// outer coroutine must hold the inner future.
//   max variant payload = 257
//   discriminant        = 1
// Predicted size: 258 bytes.
// ===========================================================================
pub async fn await_257() {
    PendOnce::<256>::new().await;
}

// ===========================================================================
// FIXTURE 5 — `data_across_await`
// `buf` (1024 bytes) is used after the await, so it must live in the suspend
// variant alongside the inner future.
//   variant payload = 1024 (buf) + 1 (PendOnce<0>) = 1025
//   discriminant    = 1
// Predicted size: 1026 bytes.
// ===========================================================================
pub async fn data_across_await() {
    let buf: [u8; 1024] = [0; 1024];
    PendOnce::<0>::new().await;
    std::hint::black_box(&buf);
}

// ===========================================================================
// FIXTURE 6 — `data_dropped_before_await`
// `buf` goes out of scope before the await, so it does NOT cross any yield
// point. The coroutine should look like FIXTURE 3.
// Predicted size: 2 bytes.
// ===========================================================================
pub async fn data_dropped_before_await() {
    {
        let buf: [u8; 1024] = [0; 1024];
        std::hint::black_box(&buf);
    }
    PendOnce::<0>::new().await;
}

// ===========================================================================
// FIXTURE 7 — `sequential_awaits`
// Two awaits with disjoint live ranges: only one inner future is alive at any
// given suspend point.
//   variant 1 payload = 257   (PendOnce<256>)
//   variant 2 payload = 129   (PendOnce<128>)
//   max(257, 129) + discriminant
// Predicted size: 258 bytes.
// ===========================================================================
pub async fn sequential_awaits() {
    PendOnce::<256>::new().await;
    PendOnce::<128>::new().await;
}

// ===========================================================================
// FIXTURE 8 — `nested_outer_small` (calls `nested_inner_small`)
// Inner coroutine is identical to FIXTURE 3 (2 bytes). The outer coroutine
// must hold the inner coroutine across its only await.
//   variant payload = 2
//   discriminant    = 1
// Predicted outer size: 3 bytes.
// Predicted inner size: 2 bytes.
// ===========================================================================
pub async fn nested_inner_small() {
    PendOnce::<0>::new().await;
}
pub async fn nested_outer_small() {
    nested_inner_small().await;
}

// ===========================================================================
// FIXTURE 9 — `nested_outer_big` (calls `nested_inner_big`)
// Inner coroutine is identical to FIXTURE 4 (258 bytes).
//   outer variant payload = 258
//   discriminant          = 1
// Predicted outer size: 259 bytes.
// Predicted inner size: 258 bytes.
// ===========================================================================
pub async fn nested_inner_big() {
    PendOnce::<256>::new().await;
}
pub async fn nested_outer_big() {
    nested_inner_big().await;
}

// ===========================================================================
// FIXTURE 10 — `two_live_across_await`
// Two locals (`a`, `b`) are both held across the same await point.
//   variant payload = 512 (a) + 512 (b) + 1 (PendOnce<0>) = 1025
//   discriminant    = 1
// Predicted size: 1026 bytes.
// ===========================================================================
pub async fn two_live_across_await() {
    let a: [u8; 512] = [0; 512];
    let b: [u8; 512] = [1; 512];
    PendOnce::<0>::new().await;
    std::hint::black_box(&a);
    std::hint::black_box(&b);
}

// ---------------------------------------------------------------------------
// Minimal driver: poll each future once so the mono-item collector
// instantiates the concrete coroutine types. We do NOT use an executor —
// a no-op waker and a single poll is enough to force codegen of the
// coroutine `poll` method (which is where layout gets queried).
// ---------------------------------------------------------------------------

const NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |_| RawWaker::new(std::ptr::null(), &NOOP_VTABLE),
    |_| {},
    |_| {},
    |_| {},
);

fn noop_waker() -> Waker {
    // SAFETY: the vtable's functions are all no-ops on a null data pointer.
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &NOOP_VTABLE)) }
}

fn poll_once<F: Future>(fut: F) {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    let _ = fut.as_mut().poll(&mut cx);
}

fn main() {
    poll_once(empty());
    poll_once(locals_no_await());
    poll_once(await_one_byte());
    poll_once(await_257());
    poll_once(data_across_await());
    poll_once(data_dropped_before_await());
    poll_once(sequential_awaits());
    poll_once(nested_outer_small());
    poll_once(nested_outer_big());
    poll_once(two_live_across_await());
}
