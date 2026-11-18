use std::sync::Mutex;

use crate::{DecompressError, Result, Stream};

/// Shared by every loader path, including xref revisions and lazy object streams.
/// Holding the lock during decoding also makes the budget safe with rayon.
#[derive(Debug)]
pub(crate) struct DecodeBudget {
    remaining: Mutex<Option<usize>>,
}

impl DecodeBudget {
    pub(crate) fn new(limit: Option<usize>) -> Self {
        Self {
            remaining: Mutex::new(limit),
        }
    }

    pub(crate) fn decode(&self, stream: &Stream, limit: Option<usize>) -> Result<Vec<u8>> {
        let mut remaining = self
            .remaining
            .lock()
            .map_err(|_| crate::Error::Decompress(DecompressError::MemoryLimitExceeded { limit: 0 }))?;
        stream.decode_filters_with_budget(limit, &mut remaining)
    }
}
