use crate::core::types::PackageManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Detection {
    pub(super) manager: PackageManager,
    pub(super) confidence: u8,
}

impl Detection {
    pub(super) const fn from_signals(
        manager: PackageManager,
        has_lockfile: bool,
        has_valid_config: bool,
    ) -> Self {
        Self {
            manager,
            confidence: calculate_confidence(has_lockfile, has_valid_config),
        }
    }

    #[cfg(test)]
    pub(super) const fn with_confidence(manager: PackageManager, confidence: u8) -> Self {
        Self {
            manager,
            confidence,
        }
    }
}

/// Sorts detected package managers by confidence score and secondary ordering.
///
/// Primary sort: confidence score (descending)
/// Secondary sort: Cargo > Bun > pnpm > Yarn > npm
pub(super) fn prioritize_managers(detections: Vec<Detection>) -> Vec<PackageManager> {
    let mut sorted = detections;

    sorted.sort_by(|left, right| match right.confidence.cmp(&left.confidence) {
        std::cmp::Ordering::Equal => {
            manager_priority(left.manager).cmp(&manager_priority(right.manager))
        }
        other => other,
    });

    sorted
        .into_iter()
        .map(|detection| detection.manager)
        .collect()
}

/// Calculates a confidence score (0-100) based on detection signals.
///
/// Scoring:
/// - Lockfile + valid config: 100
/// - Lockfile only: 75
/// - Valid config only: 50
/// - Neither: 0
pub(super) const fn calculate_confidence(has_lockfile: bool, has_valid_config: bool) -> u8 {
    match (has_lockfile, has_valid_config) {
        (true, true) => 100,
        (true, false) => 75,
        (false, true) => 50,
        (false, false) => 0,
    }
}

/// Returns a priority value for deterministic ordering when confidence is equal.
///
/// Lower values = higher priority.
pub(super) const fn manager_priority(manager: PackageManager) -> u8 {
    match manager {
        PackageManager::Cargo => 0,
        PackageManager::Deno => 1,
        PackageManager::Bun => 2,
        PackageManager::Pnpm => 3,
        PackageManager::YarnModern => 4,
        PackageManager::YarnClassic => 5,
        PackageManager::Npm => 6,
    }
}
