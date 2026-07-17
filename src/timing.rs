use std::time::Instant;

/// Lightweight opt-in stage timings for startup/profile investigations.
pub struct StageTimings {
    enabled: bool,
    started: Instant,
    checkpoint: Instant,
}

impl StageTimings {
    pub fn from_env() -> Self {
        let now = Instant::now();
        Self {
            enabled: std::env::var_os("TKSTAT_PROFILE").is_some(),
            started: now,
            checkpoint: now,
        }
    }

    pub fn checkpoint(&mut self, stage: &str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        eprintln!(
            "tkstat timing: {stage}={}ms total={}ms",
            now.duration_since(self.checkpoint).as_millis(),
            now.duration_since(self.started).as_millis()
        );
        self.checkpoint = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_timings_accept_checkpoints() {
        let now = Instant::now();
        let mut timings = StageTimings {
            enabled: false,
            started: now,
            checkpoint: now,
        };
        timings.checkpoint("test");
        assert_eq!(timings.checkpoint, now);
    }
}
