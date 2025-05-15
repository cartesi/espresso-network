use std::collections::VecDeque;

use tokio_util::task::AbortOnDropHandle;

/// A [`VecDeque`] with a maximum size
pub struct BoundedVecDeque<T> {
    /// The inner [`VecDeque`]
    inner: VecDeque<T>,
    /// The maximum size of the [`VecDeque`]
    max_size: usize,
}

impl<T> BoundedVecDeque<T> {
    /// Create a new bounded [`VecDeque`] with the given maximum size
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: VecDeque::new(),
            max_size,
        }
    }

    /// Push an item into the bounded [`VecDeque`], removing the oldest item if the
    /// maximum size is reached
    pub fn push(&mut self, item: T) {
        if self.inner.len() >= self.max_size {
            self.inner.pop_front();
        }
        self.inner.push_back(item);
    }
}

/// A [`Vec`] of [`AbortOnDropHandle`]s with a maximum size. Instead of always aborting old tasks when
/// new ones come in, it will only do so if a task is finished.
pub struct BoundedFuturePool<T> {
    /// The inner [`VecDeque`]
    inner: VecDeque<AbortOnDropHandle<T>>,
    /// The maximum number of tasks that can be in the pool at any given time
    max_size: usize,
}

impl<T> BoundedFuturePool<T> {
    /// Create a new bounded future pool with the given maximum size.
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: VecDeque::new(),
            max_size,
        }
    }

    /// Returns whether or not the pool is full.
    pub fn is_full(&self) -> bool {
        self.inner.len() >= self.max_size
    }

    /// Returns whether or not the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Prune any finished tasks from the pool.
    pub fn prune(&mut self) {
        self.inner.retain(|handle| !handle.is_finished());
    }

    /// Insert a new task into the pool. This will error if the pool is full
    /// of futures that are all still running.
    pub fn insert(&mut self, item: AbortOnDropHandle<T>) -> anyhow::Result<()> {
        // If the pool is full, prune any finished tasks
        if self.is_full() {
            // Prune any finished tasks from the pool
            self.prune();

            // If the pool is still full, return an error
            if self.is_full() {
                return Err(anyhow::anyhow!("pool is full"));
            }
        }

        // If the pool is not full, insert the new task into the pool
        self.inner.push_back(item);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{spawn, time::sleep};

    use super::*;

    #[test]
    fn test_bounded_vec_deque() {
        let mut deque = BoundedVecDeque::new(3);
        deque.push(1);
        deque.push(2);
        deque.push(3);
        deque.push(4);
        deque.push(5);
        assert_eq!(deque.inner.len(), 3);
        assert_eq!(deque.inner, vec![3, 4, 5]);
    }

    #[tokio::test]
    async fn test_bounded_future_pool() {
        // Create a new bounded future pool
        let mut pool = BoundedFuturePool::new(2);

        // Insert a new task into the pool
        pool.insert(AbortOnDropHandle::new(spawn(async move {
            sleep(Duration::from_secs(1)).await;
        })))
        .unwrap();
        assert_eq!(pool.inner.len(), 1);

        // Insert a second task into the pool
        pool.insert(AbortOnDropHandle::new(spawn(async move {
            sleep(Duration::from_secs(1)).await;
        })))
        .unwrap();
        assert_eq!(pool.inner.len(), 2);

        // Try to insert a third task into the pool
        assert!(pool
            .insert(AbortOnDropHandle::new(spawn(async move {
                sleep(Duration::from_secs(1)).await;
            })))
            .is_err());

        // Wait 2s
        sleep(Duration::from_secs(2)).await;

        // Try to insert again. This time it should be successful
        assert!(pool
            .insert(AbortOnDropHandle::new(spawn(async move {
                sleep(Duration::from_secs(1)).await;
            })))
            .is_ok());

        // Prune any finished tasks from the pool
        pool.prune();
        assert_eq!(pool.inner.len(), 1);
    }
}
