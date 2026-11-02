//! Size tiers. Every reader checks a limit before it allocates.

/// Bytes read from the start of a file to detect its format.
pub const PROBE: usize = 64 * 1024;

/// Largest file that can be edited in memory.
pub const EDIT_MAX: u64 = 16 * 1024 * 1024;

/// Largest file that is read whole for viewing; beyond this it is paged from disk.
pub const VIEW_MAX: u64 = 256 * 1024 * 1024;

/// Which operations a file's size allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    /// Whole file in memory, editing allowed when the format allows it.
    Editable,
    /// Read-only. Text is paged from disk; structure readers may still load it whole.
    ViewOnly,
    /// Read-only, never loaded whole.
    Paged,
}

impl Tier {
    pub fn for_size(size: u64) -> Self {
        if size <= EDIT_MAX {
            Tier::Editable
        } else if size <= VIEW_MAX {
            Tier::ViewOnly
        } else {
            Tier::Paged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_follow_the_limits() {
        assert_eq!(Tier::for_size(0), Tier::Editable);
        assert_eq!(Tier::for_size(EDIT_MAX), Tier::Editable);
        assert_eq!(Tier::for_size(EDIT_MAX + 1), Tier::ViewOnly);
        assert_eq!(Tier::for_size(VIEW_MAX), Tier::ViewOnly);
        assert_eq!(Tier::for_size(VIEW_MAX + 1), Tier::Paged);
    }
}
