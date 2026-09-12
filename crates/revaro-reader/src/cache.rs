//! A small in-memory LRU of parsed books.
//!
//! Parsing an EPUB is expensive enough that the server keeps the result for the
//! hottest few books. Go owned this cache from the server and registered it with
//! the global cache manager; this port keeps the same shape — a self-contained
//! object LRU with its own byte budget — so the server migration can wire it up
//! without changing the policy.
//!
//! Unlike the Go original, [`Book::byte_size`](crate::Book::byte_size) is a pure
//! function, so there is no unsynchronised memoisation race.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::Book;

/// Least-recently-used cache of parsed books, bounded by count and bytes.
#[derive(Debug)]
pub struct BookCache {
    inner: Mutex<Inner>,
    max_books: usize,
    max_bytes: i64,
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<String, Arc<Book>>,
    /// Keys from least to most recently used. Kept in sync with `entries`.
    order: VecDeque<String>,
    bytes: i64,
}

impl BookCache {
    /// Create a cache holding at most `max_books` books or `max_bytes` bytes,
    /// whichever binds first.
    #[must_use]
    pub fn new(max_books: usize, max_bytes: i64) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_books,
            max_bytes,
        }
    }

    /// Fetch a book and mark it most recently used.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Arc<Book>> {
        let mut inner = self.lock();
        let book = inner.entries.get(key).cloned()?;
        inner.touch(key);
        Some(book)
    }

    /// Insert or replace a book.
    ///
    /// A book larger than the whole budget is dropped rather than admitted and
    /// immediately evicted, which is what the Go `Put` did.
    pub fn put(&self, key: &str, book: Arc<Book>) {
        let size = book.byte_size();
        if size > self.max_bytes {
            return;
        }
        let mut inner = self.lock();
        if let Some(previous) = inner.entries.get(key) {
            inner.bytes -= previous.byte_size();
            inner.touch(key);
        } else {
            inner.order.push_back(key.to_string());
        }
        inner.bytes += size;
        inner.entries.insert(key.to_string(), book);
        inner.trim(self.max_books, self.max_bytes);
    }

    /// Current `(bytes, entries)`, for the global cache manager's statistics.
    #[must_use]
    pub fn stats(&self) -> (i64, usize) {
        let inner = self.lock();
        (inner.bytes, inner.order.len())
    }

    /// Shrink towards `max_bytes` without ever growing past this cache's own
    /// configured limit. A negative value means "no convergence requested".
    pub fn trim_to(&self, max_bytes: i64) {
        if max_bytes < 0 {
            return;
        }
        let mut inner = self.lock();
        let target = max_bytes.min(self.max_bytes);
        inner.trim(self.max_books, target);
    }

    /// Lock without propagating poisoning: a panic in an unrelated caller must
    /// not take the whole reader cache down.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inner {
    /// Move `key` to the most-recently-used end.
    fn touch(&mut self, key: &str) {
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.to_string());
    }

    /// Evict from the least-recently-used end until both bounds hold.
    fn trim(&mut self, max_books: usize, max_bytes: i64) {
        while self.order.len() > max_books || self.bytes > max_bytes {
            let Some(key) = self.order.pop_front() else {
                break;
            };
            if let Some(book) = self.entries.remove(&key) {
                self.bytes -= book.byte_size();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Format;

    fn text_book(label: &str, length: usize) -> Arc<Book> {
        Arc::new(Book {
            format: Format::Txt,
            text: label.repeat(length),
            ..Book::default()
        })
    }

    #[test]
    fn cache_evicts_the_least_recently_used_book() {
        let cache = BookCache::new(2, 1 << 20);
        cache.put("a", text_book("a", 100));
        cache.put("b", text_book("b", 100));
        // Reading `a` makes `b` the least recently used.
        assert!(cache.get("a").is_some());
        cache.put("c", text_book("c", 100));
        assert!(cache.get("b").is_none(), "LRU entry should be evicted");
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
        assert_eq!(cache.stats().1, 2);
    }

    #[test]
    fn cache_does_not_retain_an_oversized_book() {
        let cache = BookCache::new(2, 100);
        cache.put("large", text_book("x", 101));
        assert!(cache.get("large").is_none());
        assert_eq!(cache.stats(), (0, 0));
    }

    #[test]
    fn replacing_a_key_updates_the_byte_accounting() {
        let cache = BookCache::new(4, 1 << 20);
        cache.put("k", text_book("x", 10));
        cache.put("k", text_book("y", 30));
        assert_eq!(cache.stats(), (30, 1));
        assert_eq!(cache.get("k").unwrap().text.len(), 30);
    }

    #[test]
    fn trim_to_never_grows_the_configured_limit() {
        let cache = BookCache::new(4, 1000);
        cache.put("a", text_book("a", 100));
        // A wider global budget must not let the cache exceed its own cap.
        cache.trim_to(10_000);
        assert_eq!(cache.stats().1, 1);
        // A negative budget means "no convergence requested".
        cache.trim_to(-1);
        assert_eq!(cache.stats().1, 1);
        // A tighter budget evicts everything.
        cache.trim_to(0);
        assert_eq!(cache.stats(), (0, 0));
    }
}
