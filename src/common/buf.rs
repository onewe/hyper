use std::collections::VecDeque;
use std::io::IoSlice;

use bytes::{Buf, BufMut, Bytes, BytesMut};

pub(crate) struct BufList<T> {
    bufs: VecDeque<T>,
}

impl<T: Buf> BufList<T> {
    pub(crate) fn new() -> BufList<T> {
        // 创建一个 buf list
        // 用于存储多个 buf
        BufList {
            bufs: VecDeque::new(),
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, buf: T) {
        debug_assert!(buf.has_remaining());
        // 往 buf list 中添加一个 buf, 添加在队列后面
        self.bufs.push_back(buf);
    }

    #[inline]
    #[cfg(feature = "http1")]
    pub(crate) fn bufs_cnt(&self) -> usize {
        // 统计 buf list 中 buf 的个数
        self.bufs.len()
    }
}

impl<T: Buf> Buf for BufList<T> {
    #[inline]
    fn remaining(&self) -> usize {
        // 统计 buf list 中所有 buf 的剩余字节数
        self.bufs.iter().map(|buf| buf.remaining()).sum()
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        // 返回 buf list 中第一个 buf 的字节切片
        self.bufs.front().map(Buf::chunk).unwrap_or_default()
    }

    #[inline]
    fn advance(&mut self, mut cnt: usize) {
        while cnt > 0 {
            {
                // 获取 buf list 中第一个 buf
                let front = &mut self.bufs[0];
                // 获取第一个 buf 的剩余字节数
                let rem = front.remaining();
                // 判断第一个 buf 的剩余字节数是否大于 cnt
                if rem > cnt {
                    // 如果大于 cnt, 则直接跳过 cnt 个字节
                    front.advance(cnt);
                    return;
                } else {
                    // 跳过第一个 buf 的所有字节
                    front.advance(rem);
                    // 计算剩余需要跳过的字节数
                    cnt -= rem;
                }
            }
            // 第一个 buf 已经跳过所有字节, 从 buf list 中移除
            self.bufs.pop_front();
        }
    }

    #[inline]
    fn chunks_vectored<'t>(&'t self, dst: &mut [IoSlice<'t>]) -> usize {
        // 判断 dst 是否为空, 如果为空则直接返回
        if dst.is_empty() {
            return 0;
        }

        let mut vecs = 0;

        // 遍历 buf list 中的所有 buf
        for buf in &self.bufs {
            // 使用 dst 中的数据 填充 buf
            // vecs 表示已经填充了多少个字节
            vecs += buf.chunks_vectored(&mut dst[vecs..]);
            // 判断填充的字节和 dst 的长度是否相等
            // 如果相等则代表已经填充完成
            // 否则下次从 上次 填充的位置开始填充
            if vecs == dst.len() {
                break;
            }
        }
        vecs
    }

    #[inline]
    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        // Our inner buffer may have an optimized version of copy_to_bytes, and if the whole
        // request can be fulfilled by the front buffer, we can take advantage.
        // 获取 buf list 中第一个 buf
        match self.bufs.front_mut() {
            // 判断第一个 buf 的剩余字节数是否等于 len
            Some(front) if front.remaining() == len => {
                // copy 一个指定长度的字节切片
                let b = front.copy_to_bytes(len);
                // 出栈
                self.bufs.pop_front();
                b
            }
            // 判断第一个 buf 的剩余字节数是否大于 len
            Some(front) if front.remaining() > len => front.copy_to_bytes(len),
            _ => {
                assert!(len <= self.remaining(), "`len` greater than remaining");
                // 创建一个容量为 len 的 BytesMut
                let mut bm = BytesMut::with_capacity(len);
                // 把第一个 buf 的字节切片 copy 到 bm 中
                bm.put(self.take(len));
                bm.freeze()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;

    fn hello_world_buf() -> BufList<Bytes> {
        BufList {
            bufs: vec![Bytes::from("Hello"), Bytes::from(" "), Bytes::from("World")].into(),
        }
    }

    #[test]
    fn to_bytes_shorter() {
        let mut bufs = hello_world_buf();
        let old_ptr = bufs.chunk().as_ptr();
        let start = bufs.copy_to_bytes(4);
        assert_eq!(start, "Hell");
        assert!(ptr::eq(old_ptr, start.as_ptr()));
        assert_eq!(bufs.chunk(), b"o");
        assert!(ptr::eq(old_ptr.wrapping_add(4), bufs.chunk().as_ptr()));
        assert_eq!(bufs.remaining(), 7);
    }

    #[test]
    fn to_bytes_eq() {
        let mut bufs = hello_world_buf();
        let old_ptr = bufs.chunk().as_ptr();
        let start = bufs.copy_to_bytes(5);
        assert_eq!(start, "Hello");
        assert!(ptr::eq(old_ptr, start.as_ptr()));
        assert_eq!(bufs.chunk(), b" ");
        assert_eq!(bufs.remaining(), 6);
    }

    #[test]
    fn to_bytes_longer() {
        let mut bufs = hello_world_buf();
        let start = bufs.copy_to_bytes(7);
        assert_eq!(start, "Hello W");
        assert_eq!(bufs.remaining(), 4);
    }

    #[test]
    fn one_long_buf_to_bytes() {
        let mut buf = BufList::new();
        buf.push(b"Hello World" as &[_]);
        assert_eq!(buf.copy_to_bytes(5), "Hello");
        assert_eq!(buf.chunk(), b" World");
    }

    #[test]
    #[should_panic(expected = "`len` greater than remaining")]
    fn buf_to_bytes_too_many() {
        hello_world_buf().copy_to_bytes(42);
    }
}
