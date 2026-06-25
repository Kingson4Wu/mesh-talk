//! An `AsyncRead`/`AsyncWrite` byte stream over a browser `RTCDataChannel`, so the mesh
//! `SecureChannel` (Noise) can frame over a real WebRTC data channel in the PWA — the browser
//! counterpart to the node's detached `PollDataChannel`. The data channel is reliable + ordered
//! (default), so a contiguous byte stream is well-defined. Single-threaded (wasm), so the shared
//! inbound buffer is a plain `Rc<RefCell<…>>`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, RtcDataChannel, RtcDataChannelType};

#[derive(Default)]
struct Inbound {
    queue: VecDeque<u8>,
    waker: Option<Waker>,
    closed: bool,
}

/// A byte stream over an `RTCDataChannel`. Inbound messages are buffered by an `onmessage`
/// handler and drained by `poll_read`; `poll_write` sends synchronously.
pub struct DataChannelStream {
    dc: RtcDataChannel,
    inbound: Rc<RefCell<Inbound>>,
    // Kept alive for the channel's lifetime (dropping would unregister the JS callbacks).
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut()>,
}

impl DataChannelStream {
    pub fn new(dc: RtcDataChannel) -> Self {
        dc.set_binary_type(RtcDataChannelType::Arraybuffer);
        let inbound = Rc::new(RefCell::new(Inbound::default()));

        let inb = Rc::clone(&inbound);
        let on_message = Closure::wrap(Box::new(move |e: MessageEvent| {
            let buf = js_sys::Uint8Array::new(&e.data());
            let mut bytes = vec![0u8; buf.length() as usize];
            buf.copy_to(&mut bytes);
            let mut i = inb.borrow_mut();
            i.queue.extend(bytes);
            if let Some(w) = i.waker.take() {
                w.wake();
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        dc.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let inb = Rc::clone(&inbound);
        let on_close = Closure::wrap(Box::new(move || {
            let mut i = inb.borrow_mut();
            i.closed = true;
            if let Some(w) = i.waker.take() {
                w.wake();
            }
        }) as Box<dyn FnMut()>);
        dc.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Self {
            dc,
            inbound,
            _on_message: on_message,
            _on_close: on_close,
        }
    }
}

impl Drop for DataChannelStream {
    fn drop(&mut self) {
        // Unregister the JS handlers before their Closures are freed — otherwise a late message/
        // close event would invoke a dropped closure (wasm-bindgen throws).
        self.dc.set_onmessage(None);
        self.dc.set_onclose(None);
    }
}

impl AsyncRead for DataChannelStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut inb = self.inbound.borrow_mut();
        if inb.queue.is_empty() {
            if inb.closed {
                return Poll::Ready(Ok(())); // EOF
            }
            inb.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let n = std::cmp::min(buf.remaining(), inb.queue.len());
        let chunk: Vec<u8> = inb.queue.drain(..n).collect();
        buf.put_slice(&chunk);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for DataChannelStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // RTCDataChannel.send is synchronous; reliable+ordered delivery is the default.
        match self.dc.send_with_u8_array(buf) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::other("RTCDataChannel send failed"))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let _ = self.dc.close();
        Poll::Ready(Ok(()))
    }
}
