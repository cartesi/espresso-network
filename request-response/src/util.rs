use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
};

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
pub struct FuturePool<T> {
    /// The inner [`VecDeque`]
    inner: VecDeque<AbortOnDropHandle<T>>,
    /// The maximum number of tasks that can be in the pool at any given time
    max_size: usize,
}

impl<T> FuturePool<T> {
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

    /// Returns the length of the pool.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Prune any finished tasks from the pool.
    pub fn prune(&mut self) -> usize {
        // Get the pre-prune size of the pool
        let size_before = self.inner.len();

        // Prune any finished tasks from the pool
        self.inner.retain(|handle| !handle.is_finished());

        // Return the number of tasks that were pruned
        size_before - self.inner.len()
    }

    /// Insert a new task into the pool. This will error if the pool is full
    /// of futures that are all still running.
    pub fn insert(&mut self, item: AbortOnDropHandle<T>) -> anyhow::Result<()> {
        // If the pool is full, prune any finished tasks
        if self.is_full() {
            // Prune any finished tasks from the pool.
            self.prune();

            // If the pool is still full, return an error.
            if self.is_full() {
                return Err(anyhow::anyhow!("pool is full"));
            }
        }

        // If the pool is not full, insert the new task into the pool
        self.inner.push_back(item);

        Ok(())
    }
}

/// A way to bound the number of tasks that can be running for a given key
/// simultaneously.
pub struct KeyedFuturePool<K: Eq + Hash, T> {
    /// The map of keys to their future pools
    map: HashMap<K, FuturePool<T>>,
    /// The current size of the global pool (all keys combined)
    current_size: usize,
    /// The maximum number of tasks that can be in the global pool at any given time
    max_size: usize,
    /// The maximum size that a single key's pool can grow to
    max_size_per_key: usize,
}

#[derive(thiserror::Error, Debug)]
pub enum KeyedFuturePoolError {
    #[error("the global pool is full")]
    GlobalPoolFull,
    #[error("the pool for the given key is full")]
    PerKeyPoolFull,
}

impl<K: Eq + Hash, T> KeyedFuturePool<K, T> {
    /// Create a new keyed bounded future pool with the given maximums.
    pub fn new(max_size: usize, max_size_per_key: usize) -> Self {
        Self {
            map: HashMap::new(),
            max_size,
            current_size: 0,
            max_size_per_key,
        }
    }

    /// Insert a new task into the pool for the given key. This will error if either:
    /// - The global pool is full
    /// - The per-key pool is full for the given key
    pub fn insert(
        &mut self,
        key: K,
        item: AbortOnDropHandle<T>,
    ) -> Result<(), KeyedFuturePoolError> {
        // If the global pool is full, prune it
        if self.current_size >= self.max_size {
            self.prune();

            // If it is still full, return an error
            if self.current_size >= self.max_size {
                return Err(KeyedFuturePoolError::GlobalPoolFull);
            }
        }

        // Get the future pool for the given key, creating it if it doesn't exist
        let pool = self
            .map
            .entry(key)
            .or_insert_with(|| FuturePool::new(self.max_size_per_key));

        // If the per-key pool is full, prune it
        if pool.is_full() {
            // Prune the per-key pool and update the current size
            self.current_size -= pool.prune();

            // If it is still full, return an error
            if pool.is_full() {
                return Err(KeyedFuturePoolError::PerKeyPoolFull);
            }
        }

        // Insert the new task into the pool. We ignore the error because we know that the pool is not full.
        let _ = pool.insert(item);

        // Update the current size
        self.current_size += 1;

        Ok(())
    }

    /// Prunes all pools of any finished tasks and removes any empty ones.
    fn prune(&mut self) {
        // Prune all pools, retaining only the ones that are not empty
        self.map.retain(|_, pool| {
            // First prune the pool and update the current size
            self.current_size -= pool.prune();

            // Retain the pool only if it is not empty
            !pool.is_empty()
        });
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
    async fn test_future_pool() {
        // Create a new bounded future pool
        let mut pool = FuturePool::new(2);

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

    /// A helper function to spawn a task that sleeps for 1 second
    fn spawn_task() -> AbortOnDropHandle<()> {
        AbortOnDropHandle::new(spawn(async move {
            sleep(Duration::from_secs(1)).await;
        }))
    }

    #[tokio::test]
    async fn test_keyed_future_pool() {
        // Create a new keyed future pool
        let mut pool = KeyedFuturePool::new(2, 1);

        // Insert a new task for 1
        pool.insert(1, spawn_task()).unwrap();

        // Insert another task for 1. This should fail
        assert!(pool.insert(1, spawn_task(),).is_err());

        // Wait 2s
        sleep(Duration::from_secs(2)).await;

        // Insert another task for 1. This should now be successful
        pool.insert(1, spawn_task()).unwrap();

        // Insert a task for 2. This should be successful
        pool.insert(2, spawn_task()).unwrap();

        // Insert one for 3. This should fail
        assert!(pool.insert(3, spawn_task(),).is_err());

        // Wait 2s
        sleep(Duration::from_secs(2)).await;

        // Insert a task for 2. This should be successful
        pool.insert(2, spawn_task()).unwrap();
    }
}
