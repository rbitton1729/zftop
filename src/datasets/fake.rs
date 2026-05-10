//! Test-only fake `DatasetsSource`. Mirrors the shape of
//! `crate::pools::fake::FakePoolsSource` so tests in `app.rs` and
//! `ui/datasets_*` can construct hand-built dataset snapshots without
//! touching libzfs.

#![cfg(test)]

use super::{DatasetNode, DatasetsSource};

pub struct FakeDatasetsSource {
    snapshot: Vec<DatasetNode>,
    next_refresh_error: Option<String>,
}

impl FakeDatasetsSource {
    pub fn new(roots: Vec<DatasetNode>) -> Self {
        Self {
            snapshot: roots,
            next_refresh_error: None,
        }
    }

    /// Cause the next `refresh()` call to return `Err(msg)` without
    /// touching `snapshot`. Subsequent `refresh()` calls succeed again.
    /// Used to test the "transient error preserves stale snapshot" path
    /// in `App::refresh_datasets`.
    pub fn fail_next_refresh(mut self, msg: &str) -> Self {
        self.next_refresh_error = Some(msg.into());
        self
    }
}

impl DatasetsSource for FakeDatasetsSource {
    fn refresh(&mut self) -> anyhow::Result<()> {
        if let Some(e) = self.next_refresh_error.take() {
            return Err(anyhow::anyhow!(e));
        }
        Ok(())
    }

    fn roots(&self) -> Vec<DatasetNode> {
        self.snapshot.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasets::types::{DatasetKind, DatasetProperties};

    fn fs(name: &str) -> DatasetNode {
        DatasetNode {
            name: name.into(),
            kind: DatasetKind::Filesystem,
            used_bytes: 100,
            refer_bytes: 100,
            available_bytes: 1000,
            compression_ratio: 1.0,
            properties: DatasetProperties::default(),
            children: vec![],
        }
    }

    #[test]
    fn refresh_then_roots_returns_seed() {
        let mut src = FakeDatasetsSource::new(vec![fs("tank")]);
        src.refresh().expect("refresh");
        let roots = src.roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "tank");
    }

    #[test]
    fn fail_next_refresh_returns_err_then_recovers() {
        let mut src = FakeDatasetsSource::new(vec![fs("tank")]).fail_next_refresh("boom");
        assert!(src.refresh().is_err());
        // Second refresh succeeds.
        src.refresh().expect("recovers");
    }
}
