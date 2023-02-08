#[cfg(feature = "http2")]
use std::future::Future;

use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "http2")]
use crate::common::Pin;
use crate::common::{task, Poll};

#[cfg(test)]
pub(crate) type RetryPromise<T, U> = oneshot::Receiver<Result<U, (crate::Error, Option<T>)>>;
pub(crate) type Promise<T> = oneshot::Receiver<Result<T, crate::Error>>;

pub(crate) fn channel<T, U>() -> (Sender<T, U>, Receiver<T, U>) {
    // 创建一个无界的 channel
    let (tx, rx) = mpsc::unbounded_channel();
    // 使用 want 库来实现拉取消息
    let (giver, taker) = want::new();

    // 创建一个 sender, 使用 giver 来接受 want 请求, 然后发送消息到 receiver 
    let tx = Sender {
        buffered_once: false,
        giver,
        inner: tx,
    };

    // 创建一个 receiver , 使用 taker 来请求 want, 然后从 接受消息
    let rx = Receiver { inner: rx, taker };
    (tx, rx)
}

/// A bounded sender of requests and callbacks for when responses are ready.
///
/// While the inner sender is unbounded, the Giver is used to determine
/// if the Receiver is ready for another request.
pub(crate) struct Sender<T, U> {
    /// One message is always allowed, even if the Receiver hasn't asked
    /// for it yet. This boolean keeps track of whether we've sent one
    /// without notice.
    /// 在不通知的情况下, 发送一个消息
    buffered_once: bool,
    /// The Giver helps watch that the the Receiver side has been polled
    /// when the queue is empty. This helps us know when a request and
    /// response have been fully processed, and a connection is ready
    /// for more.
    giver: want::Giver,
    /// Actually bounded by the Giver, plus `buffered_once`.
    /// 实际上是有界限的 channel 通过 giver 和 buffered_once 限制
    inner: mpsc::UnboundedSender<Envelope<T, U>>,
}

/// An unbounded version.
///
/// Cannot poll the Giver, but can still use it to determine if the Receiver
/// has been dropped. However, this version can be cloned.
#[cfg(feature = "http2")]
pub(crate) struct UnboundedSender<T, U> {
    /// Only used for `is_closed`, since mpsc::UnboundedSender cannot be checked.
    giver: want::SharedGiver,
    inner: mpsc::UnboundedSender<Envelope<T, U>>,
}

impl<T, U> Sender<T, U> {
    /**
     * pool ready 用来判断是否可以发送消息
     * 如果 receiver 没有准备好, 则返回 Poll::Pending
     */
    pub(crate) fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<crate::Result<()>> {
        self.giver
            .poll_want(cx)
            .map_err(|_| crate::Error::new_closed())
    }

    #[cfg(test)]
    pub(crate) fn is_ready(&self) -> bool {
        self.giver.is_wanting()
    }

    /*
    pub(crate) fn is_closed(&self) -> bool {
        self.giver.is_canceled()
    }
    */

    fn can_send(&mut self) -> bool {
        // 判断 receiver 是否准备好接收消息
        if self.giver.give() || !self.buffered_once {
            // If the receiver is ready *now*, then of course we can send.
            //
            // If the receiver isn't ready yet, but we don't have anything
            // in the channel yet, then allow one message.
            // 如果 receiver 还没有准备好接收消息, 但是 channel 中没有消息, 则允许发送一个消息
            self.buffered_once = true;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn try_send(&mut self, val: T) -> Result<RetryPromise<T, U>, T> {
        if !self.can_send() {
            return Err(val);
        }
        let (tx, rx) = oneshot::channel();
        self.inner
            .send(Envelope(Some((val, Callback::Retry(Some(tx))))))
            .map(move |_| rx)
            .map_err(|mut e| (e.0).0.take().expect("envelope not dropped").0)
    }

    pub(crate) fn send(&mut self, val: T) -> Result<Promise<U>, T> {
        // 判断是否能够发送
        if !self.can_send() {
            return Err(val);
        }
        // 打开 oneshot channel
        let (tx, rx) = oneshot::channel();
        // 此处的 inner 是一个无界的 channel
        // 发送一个 Envelope 消息, 其中包含了 值和 callback 对象
        // callback 对象是 oneshot channel 中的 sender tx

        // 发送之后返回 rx, 用来接收响应
        // RetryPromise == Receiver
        self.inner
            .send(Envelope(Some((val, Callback::NoRetry(Some(tx))))))
            .map(move |_| rx)
            .map_err(|mut e| (e.0).0.take().expect("envelope not dropped").0)
    }

    #[cfg(feature = "http2")]
    pub(crate) fn unbound(self) -> UnboundedSender<T, U> {
        UnboundedSender {
            // 把 giver 转换为 sharedGiver
            // sharedGiver 可以被 clone, 但是不能 poll, 只能用于判断通道是否被关闭
            giver: self.giver.shared(),
            // inner 本身就是无界的 channel
            inner: self.inner,
        }
    }
}

#[cfg(feature = "http2")]
impl<T, U> UnboundedSender<T, U> {
    /*
    pub(crate) fn is_ready(&self) -> bool {
        !self.giver.is_canceled()
    }
    */

    /***
     * 判断 通道是否被关闭
     */
    pub(crate) fn is_closed(&self) -> bool {
        self.giver.is_canceled()
    }

    #[cfg(test)]
    pub(crate) fn try_send(&mut self, val: T) -> Result<RetryPromise<T, U>, T> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .send(Envelope(Some((val, Callback::Retry(Some(tx))))))
            .map(move |_| rx)
            .map_err(|mut e| (e.0).0.take().expect("envelope not dropped").0)
    }

    pub(crate) fn send(&mut self, val: T) -> Result<Promise<U>, T> {
        // 打开 oneshot 通道
        let (tx, rx) = oneshot::channel();
        // 发送消息并带有一个 callback 对象
        // 返回一个 promise 对象, 用来接收响应

        // RetryPromise == Receiver
        self.inner
            .send(Envelope(Some((val, Callback::NoRetry(Some(tx))))))
            .map(move |_| rx)
            .map_err(|mut e| (e.0).0.take().expect("envelope not dropped").0)
    }
}

#[cfg(feature = "http2")]
impl<T, U> Clone for UnboundedSender<T, U> {
    fn clone(&self) -> Self {
        UnboundedSender {
            giver: self.giver.clone(),
            inner: self.inner.clone(),
        }
    }
}

pub(crate) struct Receiver<T, U> {
    inner: mpsc::UnboundedReceiver<Envelope<T, U>>,
    taker: want::Taker,
}

impl<T, U> Receiver<T, U> {
    pub(crate) fn poll_recv(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<(T, Callback<T, U>)>> {
        // 拉取内部 inner channel 中的消息 
        match self.inner.poll_recv(cx) {
            // 如果已经有消息存在于 channel 则返回 ready
            // 并把消息中的 Envelope 中的值取出来
            Poll::Ready(item) => {
                Poll::Ready(item.map(|mut env| env.0.take().expect("envelope not dropped")))
            }
            // 如果还没有消息, 则调用 taker 的 want 方式通知 giver 可以发送消息了 然后进入 pending 状态
            Poll::Pending => {
                self.taker.want();
                Poll::Pending
            }
        }
    }

    #[cfg(feature = "http1")]
    pub(crate) fn close(&mut self) {
        // 取消 taker 的 want 状态 用于通知 giver 不要再发送消息了
        self.taker.cancel();
        // 关闭 通道
        self.inner.close();
    }

    #[cfg(feature = "http1")]
    pub(crate) fn try_recv(&mut self) -> Option<(T, Callback<T, U>)> {
        use futures_util::FutureExt;
        // now_or_never 这个方式是用于立即获取 channel 中的消息, 如果此时 receiver 已经 ready 则返回 Some(Some(xxx))
        // 如果此时 receiver 返回 pending 则返回 None
        match self.inner.recv().now_or_never() {
            Some(Some(mut env)) => env.0.take(),
            _ => None,
        }
    }
}

impl<T, U> Drop for Receiver<T, U> {
    fn drop(&mut self) {
        // Notify the giver about the closure first, before dropping
        // the mpsc::Receiver.
        // 通知 giver 通道已经关闭了 在 drop 之前
        self.taker.cancel();
    }
}

/***
 * 一个消息对象 包含了 发送的值 和 一个 callback 对象 
 * callback 对象一般是 oneshot channel 中的 sender
 */
struct Envelope<T, U>(Option<(T, Callback<T, U>)>);

impl<T, U> Drop for Envelope<T, U> {
    fn drop(&mut self) {
        // 在 drop 之前获取 envelope 中的值, 然后获取 callback 对象
        // 发送错误给 callback 对象
        if let Some((val, cb)) = self.0.take() {
            cb.send(Err((
                crate::Error::new_canceled().with("connection closed"),
                Some(val),
            )));
        }
    }
}

pub(crate) enum Callback<T, U> {
    #[allow(unused)]
    Retry(Option<oneshot::Sender<Result<U, (crate::Error, Option<T>)>>>),
    NoRetry(Option<oneshot::Sender<Result<U, crate::Error>>>),
}

impl<T, U> Drop for Callback<T, U> {
    fn drop(&mut self) {
        // FIXME(nox): What errors do we want here?
        let error = crate::Error::new_user_dispatch_gone().with(if std::thread::panicking() {
            // 判断用户线程是否被 panic
            "user code panicked"
        } else {
            // 运行时drop 了 dispatch task
            "runtime dropped the dispatch task"
        });

        // 判断类型是否是 retry 类型
        match self {
            Callback::Retry(tx) => {
                // 如果是 retry 类型 获取 callback 中的 sender
                // 利用 sender 发送错误给 receiver
                if let Some(tx) = tx.take() {
                    let _ = tx.send(Err((error, None)));
                }
            }
            Callback::NoRetry(tx) => {
                // 如果是 非 retry 类型 则直接发送错误给 receiver
                if let Some(tx) = tx.take() {
                    let _ = tx.send(Err(error));
                }
            }
        }
    }
}

impl<T, U> Callback<T, U> {
    // 判断 callback 中的 tx 是否已经被关闭了
    // 如果已经被关闭了 则不能进行回调操作
    #[cfg(feature = "http2")]
    pub(crate) fn is_canceled(&self) -> bool {
        match *self {
            Callback::Retry(Some(ref tx)) => tx.is_closed(),
            Callback::NoRetry(Some(ref tx)) => tx.is_closed(),
            _ => unreachable!(),
        }
    }

    // 判断 channel 是否已经被关闭了 如果返回 pending 则表示 channel 还没有被关闭
    // 如果返回 ready 则表示 channel 已经被关闭了
    pub(crate) fn poll_canceled(&mut self, cx: &mut task::Context<'_>) -> Poll<()> {
        match *self {
            Callback::Retry(Some(ref mut tx)) => tx.poll_closed(cx),
            Callback::NoRetry(Some(ref mut tx)) => tx.poll_closed(cx),
            _ => unreachable!(),
        }
    }

    // 发送消息给 receiver
    pub(crate) fn send(mut self, val: Result<U, (crate::Error, Option<T>)>) {
        match self {
            Callback::Retry(ref mut tx) => {
                let _ = tx.take().unwrap().send(val);
            }
            Callback::NoRetry(ref mut tx) => {
                let _ = tx.take().unwrap().send(val.map_err(|e| e.0));
            }
        }
    }

    #[cfg(feature = "http2")]
    pub(crate) async fn send_when(
        self,
        mut when: impl Future<Output = Result<U, (crate::Error, Option<T>)>> + Unpin,
    ) {
        use futures_util::future;
        use tracing::trace;

        let mut cb = Some(self);

        // poll_fn 会 poll 传入的 future, 并且会在 future 返回 ready 之后 会把 future 的结果 发送给 callback 的 receiver
        // 如果是 pending 状态则会判断 callback 中的 channel 是否被关闭, 如果没有关闭直接返回 pending, 如果关闭了则返回 ready
        // 如果 ready 状态有 error 则提取 error 通过 callback 的 receiver 发送给 receiver

        // "select" on this callback being canceled, and the future completing
        future::poll_fn(move |cx| {
            match Pin::new(&mut when).poll(cx) {
                Poll::Ready(Ok(res)) => {
                    cb.take().expect("polled after complete").send(Ok(res));
                    Poll::Ready(())
                }
                Poll::Pending => {
                    // check if the callback is canceled
                    ready!(cb.as_mut().unwrap().poll_canceled(cx));
                    trace!("send_when canceled");
                    Poll::Ready(())
                }
                Poll::Ready(Err(err)) => {
                    cb.take().expect("polled after complete").send(Err(err));
                    Poll::Ready(())
                }
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "nightly")]
    extern crate test;

    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use super::{channel, Callback, Receiver};

    #[derive(Debug)]
    struct Custom(i32);

    impl<T, U> Future for Receiver<T, U> {
        type Output = Option<(T, Callback<T, U>)>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.poll_recv(cx)
        }
    }

    /// Helper to check if the future is ready after polling once.
    struct PollOnce<'a, F>(&'a mut F);

    impl<F, T> Future for PollOnce<'_, F>
    where
        F: Future<Output = T> + Unpin,
    {
        type Output = Option<()>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match Pin::new(&mut self.0).poll(cx) {
                Poll::Ready(_) => Poll::Ready(Some(())),
                Poll::Pending => Poll::Ready(None),
            }
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn drop_receiver_sends_cancel_errors() {
        let _ = pretty_env_logger::try_init();

        let (mut tx, mut rx) = channel::<Custom, ()>();

        // must poll once for try_send to succeed
        assert!(PollOnce(&mut rx).await.is_none(), "rx empty");

        let promise = tx.try_send(Custom(43)).unwrap();
        drop(rx);

        let fulfilled = promise.await;
        let err = fulfilled
            .expect("fulfilled")
            .expect_err("promise should error");
        match (err.0.kind(), err.1) {
            (&crate::error::Kind::Canceled, Some(_)) => (),
            e => panic!("expected Error::Cancel(_), found {:?}", e),
        }
    }

    #[cfg(not(miri))]
    #[tokio::test]
    async fn sender_checks_for_want_on_send() {
        let (mut tx, mut rx) = channel::<Custom, ()>();

        // one is allowed to buffer, second is rejected
        let _ = tx.try_send(Custom(1)).expect("1 buffered");
        tx.try_send(Custom(2)).expect_err("2 not ready");

        assert!(PollOnce(&mut rx).await.is_some(), "rx once");

        // Even though 1 has been popped, only 1 could be buffered for the
        // lifetime of the channel.
        tx.try_send(Custom(2)).expect_err("2 still not ready");

        assert!(PollOnce(&mut rx).await.is_none(), "rx empty");

        let _ = tx.try_send(Custom(2)).expect("2 ready");
    }

    #[cfg(feature = "http2")]
    #[test]
    fn unbounded_sender_doesnt_bound_on_want() {
        let (tx, rx) = channel::<Custom, ()>();
        let mut tx = tx.unbound();

        let _ = tx.try_send(Custom(1)).unwrap();
        let _ = tx.try_send(Custom(2)).unwrap();
        let _ = tx.try_send(Custom(3)).unwrap();

        drop(rx);

        let _ = tx.try_send(Custom(4)).unwrap_err();
    }

    #[cfg(feature = "nightly")]
    #[bench]
    fn giver_queue_throughput(b: &mut test::Bencher) {
        use crate::{body::Incoming, Request, Response};

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (mut tx, mut rx) = channel::<Request<Incoming>, Response<Incoming>>();

        b.iter(move || {
            let _ = tx.send(Request::new(Incoming::empty())).unwrap();
            rt.block_on(async {
                loop {
                    let poll_once = PollOnce(&mut rx);
                    let opt = poll_once.await;
                    if opt.is_none() {
                        break;
                    }
                }
            });
        })
    }

    #[cfg(feature = "nightly")]
    #[bench]
    fn giver_queue_not_ready(b: &mut test::Bencher) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (_tx, mut rx) = channel::<i32, ()>();
        b.iter(move || {
            rt.block_on(async {
                let poll_once = PollOnce(&mut rx);
                assert!(poll_once.await.is_none());
            });
        })
    }

    #[cfg(feature = "nightly")]
    #[bench]
    fn giver_queue_cancel(b: &mut test::Bencher) {
        let (_tx, mut rx) = channel::<i32, ()>();

        b.iter(move || {
            rx.taker.cancel();
        })
    }
}
