//! Test-only `FakePoolsSource`. Holds a pre-built `Vec<PoolInfo>` plus an
//! optional error to return from the next `refresh()` call. Available to
//! any `#[cfg(test)]` code in the crate.

use super::types::PoolInfo;
use super::PoolsSource;

#[derive(Default)]
pub struct FakePoolsSource {
    pub pools: Vec<PoolInfo>,
    /// If set, the next `refresh()` consumes it and returns it as an error
    /// instead of succeeding. Simulates a transient libzfs failure.
    pub next_refresh_error: Option<String>,
}

impl FakePoolsSource {
    pub fn new(pools: Vec<PoolInfo>) -> Self {
        Self {
            pools,
            next_refresh_error: None,
        }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn fail_next_refresh(mut self, msg: &str) -> Self {
        self.next_refresh_error = Some(msg.to_string());
        self
    }
}

impl PoolsSource for FakePoolsSource {
    fn refresh(&mut self) -> anyhow::Result<()> {
        if let Some(msg) = self.next_refresh_error.take() {
            return Err(anyhow::anyhow!(msg));
        }
        Ok(())
    }

    fn pools(&self) -> Vec<PoolInfo> {
        self.pools.clone()
    }
}
