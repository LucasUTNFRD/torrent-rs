use crate::tracker::error::TrackerError;

pub enum Error {
    DiskError,
    TrackerError(TrackerError),
    // TODO
}
