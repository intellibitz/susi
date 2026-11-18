//! Minimal `#![forbid(unsafe_code)]` Hyper `Body` combinators shared by the
//! Hyper-served SUSI host surfaces (`susi-gmcp` and `susi-server`), replacing
//! the `http-body-util` third-party crate and its `susi-vendor-http-body-util`
//! facade.

use hyper::body::{Body, Buf, Bytes, Frame, SizeHint};
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Compatibility module matching `http_body_util::combinators::BoxBody`.
pub mod combinators {
    pub use super::BoxBody;
}

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

/// Type-erased `Send + Sync` HTTP body.
pub struct BoxBody<D, E>(Pin<Box<dyn Body<Data = D, Error = E> + Send + Sync + 'static>>);

impl<D: Buf, E> BoxBody<D, E> {
    pub fn new<B>(body: B) -> Self
    where
        B: Body<Data = D, Error = E> + Send + Sync + 'static,
    {
        Self(Box::pin(body))
    }
}

impl<D, E> fmt::Debug for BoxBody<D, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoxBody").finish_non_exhaustive()
    }
}

impl<D: Buf, E> Body for BoxBody<D, E> {
    type Data = D;
    type Error = E;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.0.as_mut().poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}

/// A body backed by at most one contiguous `Buf` frame.
#[derive(Debug, Clone, Default)]
pub struct Full<D>(Option<D>);

impl<D: Buf> Full<D> {
    pub fn new(data: D) -> Self {
        if data.has_remaining() {
            Self(Some(data))
        } else {
            Self(None)
        }
    }
}

impl<D: Buf + Unpin> Body for Full<D> {
    type Data = D;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.0.take().map(|data| Ok(Frame::data(data))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = SizeHint::new();
        let len = self.0.as_ref().map_or(0, |d| d.remaining() as u64);
        hint.set_exact(len);
        hint
    }
}

/// An empty body that yields zero frames.
pub struct Empty<D>(PhantomData<fn() -> D>);

impl<D> Empty<D> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<D> Default for Empty<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D> Clone for Empty<D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D> Copy for Empty<D> {}

impl<D> fmt::Debug for Empty<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Empty").finish()
    }
}

impl<D: Buf> Body for Empty<D> {
    type Data = D;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        true
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = SizeHint::new();
        hint.set_exact(0);
        hint
    }
}

/// Error returned by [`Limited`] when cumulative data frames exceed the byte cap.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LengthLimitError;

impl fmt::Display for LengthLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("length limit exceeded")
    }
}

impl std::error::Error for LengthLimitError {}

/// Enforces an upper bound on cumulative data bytes yielded by an inner [`Body`].
pub struct Limited<B> {
    inner: Pin<Box<B>>,
    remaining: usize,
}

impl<B> Limited<B> {
    pub fn new(inner: B, limit: usize) -> Self {
        Self {
            inner: Box::pin(inner),
            remaining: limit,
        }
    }
}

impl<B> Body for Limited<B>
where
    B: Body,
    B::Error: Into<BoxedError>,
{
    type Data = B::Data;
    type Error = BoxedError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err.into()))),
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let len = data.remaining();
                    if len > this.remaining {
                        this.remaining = 0;
                        return Poll::Ready(Some(Err(Box::new(LengthLimitError))));
                    }
                    this.remaining -= len;
                }
                Poll::Ready(Some(Ok(frame)))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = self.inner.size_hint();
        let cap = self.remaining as u64;
        if hint.lower() > cap {
            hint.set_lower(cap);
        }
        if let Some(upper) = hint.upper() {
            if upper > cap {
                hint.set_upper(cap);
            }
        }
        hint
    }
}

/// Collected frames returned by [`BodyExt::collect`].
#[derive(Debug, Default)]
pub struct Collected {
    chunks: Vec<Bytes>,
    total_len: usize,
}

impl Collected {
    pub fn to_bytes(mut self) -> Bytes {
        if self.chunks.is_empty() {
            return Bytes::new();
        }
        if self.chunks.len() == 1 {
            return self.chunks.pop().unwrap_or_default();
        }
        let mut out = Vec::with_capacity(self.total_len);
        for chunk in self.chunks {
            out.extend_from_slice(&chunk);
        }
        Bytes::from(out)
    }
}

/// Future returned by [`BodyExt::collect`].
pub struct Collect<B> {
    body: Pin<Box<B>>,
    collected: Collected,
}

impl<B: Body> Future for Collect<B> {
    type Output = Result<Collected, B::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        loop {
            match this.body.as_mut().poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    let chunks = std::mem::take(&mut this.collected.chunks);
                    let total_len = this.collected.total_len;
                    this.collected.total_len = 0;
                    return Poll::Ready(Ok(Collected { chunks, total_len }));
                }
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Err(err)),
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(mut data) = frame.into_data() {
                        let rem = data.remaining();
                        if rem > 0 {
                            this.collected.total_len = this.collected.total_len.saturating_add(rem);
                            this.collected.chunks.push(data.copy_to_bytes(rem));
                        }
                    }
                }
            }
        }
    }
}

/// Body adapter returned by [`BodyExt::map_frame`].
pub struct MapFrame<B, F> {
    inner: Pin<Box<B>>,
    f: F,
}

impl<B, F> Unpin for MapFrame<B, F> {}

impl<B, F, D2> Body for MapFrame<B, F>
where
    B: Body,
    D2: Buf,
    F: FnMut(Frame<B::Data>) -> Frame<D2>,
{
    type Data = D2;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok((this.f)(frame)))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

/// Body adapter returned by [`BodyExt::map_err`].
pub struct MapErr<B, F> {
    inner: Pin<Box<B>>,
    f: F,
}

impl<B, F> Unpin for MapErr<B, F> {}

impl<B, F, E2> Body for MapErr<B, F>
where
    B: Body,
    F: FnMut(B::Error) -> E2,
{
    type Data = B::Data;
    type Error = E2;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err((this.f)(err)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Future returned by [`BodyExt::frame`].
pub struct NextFrame<'a, B: ?Sized> {
    body: &'a mut B,
}

impl<B: Body + Unpin + ?Sized> Future for NextFrame<'_, B> {
    type Output = Option<Result<Frame<B::Data>, B::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut *self.body).poll_frame(cx)
    }
}

/// Extension methods on [`hyper::body::Body`].
pub trait BodyExt: Body {
    fn boxed(self) -> BoxBody<Self::Data, Self::Error>
    where
        Self: Sized + Send + Sync + 'static,
    {
        BoxBody::new(self)
    }

    fn collect(self) -> Collect<Self>
    where
        Self: Sized,
    {
        Collect {
            body: Box::pin(self),
            collected: Collected::default(),
        }
    }

    fn map_frame<F, D2>(self, f: F) -> MapFrame<Self, F>
    where
        Self: Sized,
        D2: Buf,
        F: FnMut(Frame<Self::Data>) -> Frame<D2>,
    {
        MapFrame {
            inner: Box::pin(self),
            f,
        }
    }

    fn map_err<F, E2>(self, f: F) -> MapErr<Self, F>
    where
        Self: Sized,
        F: FnMut(Self::Error) -> E2,
    {
        MapErr {
            inner: Box::pin(self),
            f,
        }
    }

    fn frame(&mut self) -> NextFrame<'_, Self>
    where
        Self: Unpin,
    {
        NextFrame { body: self }
    }
}

impl<T: Body + ?Sized> BodyExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_and_empty_bodies_report_exact_size_and_collect_cleanly() {
        let empty: BoxBody<Bytes, Infallible> = Empty::<Bytes>::new().boxed();
        assert!(empty.is_end_stream());
        assert_eq!(empty.size_hint().exact(), Some(0));
        let empty_bytes = empty.collect().await.unwrap().to_bytes();
        assert!(empty_bytes.is_empty());

        let zero_full = Full::new(Bytes::new());
        assert!(zero_full.is_end_stream());
        assert_eq!(zero_full.size_hint().exact(), Some(0));

        let full = Full::new(Bytes::from_static(b"hello susi"))
            .map_err(|never| match never {})
            .boxed();
        assert!(!full.is_end_stream());
        assert_eq!(full.size_hint().exact(), Some(10));
        let collected = full.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello susi");
    }

    #[tokio::test]
    async fn limited_enforces_cumulative_cap_and_downcasts_length_limit_error() {
        struct TwoChunks(u8);
        impl Body for TwoChunks {
            type Data = Bytes;
            type Error = Infallible;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
                self.0 += 1;
                match self.0 {
                    1 => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"abcd"))))),
                    2 => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"efgh"))))),
                    _ => Poll::Ready(None),
                }
            }
        }

        let ok = Limited::new(TwoChunks(0), 8).collect().await.unwrap();
        assert_eq!(&ok.to_bytes()[..], b"abcdefgh");

        let err = Limited::new(TwoChunks(0), 7).collect().await.unwrap_err();
        assert!(err.is::<LengthLimitError>());
        assert_eq!(err.to_string(), "length limit exceeded");
    }

    #[tokio::test]
    async fn map_frame_and_frame_stream_without_buffering_until_eof() {
        let mut mapped = Full::new(Bytes::from_static(b"payload"))
            .map_frame(|f| f.map_data(|b| Bytes::from([b"X:", &b[..]].concat())))
            .boxed();
        let first = mapped.frame().await.unwrap().unwrap();
        assert_eq!(&first.into_data().unwrap()[..], b"X:payload");
        assert!(mapped.frame().await.is_none());
    }
}
