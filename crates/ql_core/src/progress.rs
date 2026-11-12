use std::fmt::Display;

/// An enum representing the progress in downloading
/// a Minecraft instance.
///
/// # Order
/// 1) Manifest JSON
/// 2) Version JSON
/// 3) Logging config
/// 4) Jar
/// 5) Libraries
/// 6) Assets
#[derive(Debug, Clone, Copy, Default)]
pub enum DownloadProgress {
    #[default]
    DownloadingJsonManifest,
    DownloadingVersionJson,
    DownloadingAssets {
        progress: usize,
        out_of: usize,
    },
    DownloadingLibraries {
        progress: usize,
        out_of: usize,
    },
    DownloadingJar,
    Horizon(HorizonProgress),
}

/// Progress through Horizon's build, Minecraft download and prepare flow.
///
/// Kept separate from the Minecraft-shaped variants above so every stage
/// lands on one monotonic 0..10 scale.
#[derive(Debug, Clone, Copy)]
pub enum HorizonProgress {
    Build,
    BuildDone,
    VersionJson,
    Jar,
    Libraries { progress: usize, out_of: usize },
    Assets { progress: usize, out_of: usize },
    ServerJar,
    Prepare { percent: usize },
}

impl Display for HorizonProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build => write!(f, "Downloading Horizon build"),
            Self::BuildDone => write!(f, "Horizon build ready"),
            Self::VersionJson => write!(f, "Downloading Minecraft version JSON"),
            Self::Jar => write!(f, "Downloading Minecraft client jar"),
            Self::Libraries { progress, out_of } => {
                write!(f, "Downloading Minecraft library {progress} / {out_of}")
            }
            Self::Assets { progress, out_of } => {
                write!(f, "Downloading Minecraft asset {progress} / {out_of}")
            }
            Self::ServerJar => write!(f, "Downloading Minecraft server jar"),
            Self::Prepare { percent } => {
                write!(f, "Preparing Horizon resources ({percent}%)")
            }
        }
    }
}

fn stage_fraction(progress: usize, out_of: usize) -> f32 {
    if out_of == 0 {
        1.0
    } else {
        (progress as f32 / out_of as f32).clamp(0.0, 1.0)
    }
}

impl From<&HorizonProgress> for f32 {
    fn from(value: &HorizonProgress) -> Self {
        // Creation shows only the build (0 → 10 on that action's bar); a
        // launch fills the bar with the file downloads and preparation.
        match value {
            HorizonProgress::Build => 0.0,
            HorizonProgress::BuildDone => 10.0,
            HorizonProgress::VersionJson => 0.0,
            HorizonProgress::Jar => 0.5,
            HorizonProgress::Libraries { progress, out_of } => {
                0.5 + stage_fraction(*progress, *out_of) * 1.5
            }
            HorizonProgress::Assets { progress, out_of } => {
                2.0 + stage_fraction(*progress, *out_of) * 4.0
            }
            HorizonProgress::ServerJar => 6.0,
            HorizonProgress::Prepare { percent } => 6.0 + (*percent).min(100) as f32 * 0.04,
        }
    }
}

impl Display for DownloadProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadProgress::DownloadingJsonManifest => write!(f, "Downloading Manifest JSON"),
            DownloadProgress::DownloadingVersionJson => write!(f, "Downloading Version JSON"),
            DownloadProgress::DownloadingAssets { progress, out_of } => {
                write!(f, "Downloading asset {progress} / {out_of}")
            }
            DownloadProgress::DownloadingLibraries { progress, out_of } => {
                write!(f, "Downloading library {progress} / {out_of}")
            }
            DownloadProgress::DownloadingJar => write!(f, "Downloading Game Jar file"),
            DownloadProgress::Horizon(progress) => write!(f, "{progress}"),
        }
    }
}

impl From<&DownloadProgress> for f32 {
    fn from(val: &DownloadProgress) -> Self {
        match val {
            DownloadProgress::DownloadingJsonManifest => 0.1,
            DownloadProgress::DownloadingVersionJson => 0.2,
            DownloadProgress::DownloadingJar => 0.3,
            DownloadProgress::DownloadingLibraries { progress, out_of } => {
                (*progress as f32 / *out_of as f32) + 1.0
            }
            DownloadProgress::DownloadingAssets { progress, out_of } => {
                (*progress as f32 * 8.0 / *out_of as f32) + 2.0
            }
            DownloadProgress::Horizon(progress) => f32::from(progress),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenericProgress {
    pub done: usize,
    pub total: usize,
    pub message: Option<String>,
    pub has_finished: bool,
}

impl Default for GenericProgress {
    fn default() -> Self {
        Self {
            done: 0,
            total: 1,
            message: None,
            has_finished: false,
        }
    }
}

impl GenericProgress {
    #[must_use]
    pub fn finished() -> Self {
        Self {
            has_finished: true,
            done: 1,
            total: 1,
            message: None,
        }
    }
}

pub trait Progress {
    fn get_num(&self) -> f32;
    fn get_message(&self) -> Option<String>;
    fn total() -> f32;

    fn into_generic(self) -> GenericProgress
    where
        Self: Sized,
    {
        let done = (self.get_num() * 100.0) as usize;
        let total = (Self::total() * 100.0) as usize;
        let message = self.get_message();

        GenericProgress {
            done,
            total,
            message,
            has_finished: false,
        }
    }
}

impl Progress for DownloadProgress {
    fn get_num(&self) -> f32 {
        f32::from(self)
    }

    fn get_message(&self) -> Option<String> {
        Some(self.to_string())
    }

    fn total() -> f32 {
        10.0
    }
}

impl Progress for GenericProgress {
    fn get_num(&self) -> f32 {
        self.done as f32 / self.total as f32
    }

    fn get_message(&self) -> Option<String> {
        self.message.clone()
    }

    fn total() -> f32 {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_monotonic(label: &str, sequence: &[HorizonProgress]) {
        let values: Vec<f32> = sequence.iter().map(f32::from).collect();
        assert!(
            values.windows(2).all(|pair| pair[0] <= pair[1]),
            "{label} progress went backwards: {values:?}"
        );
        assert_eq!(*values.first().unwrap(), 0.0, "{label} starts at 0");
        assert_eq!(*values.last().unwrap(), 10.0, "{label} ends at 10");
    }

    #[test]
    fn creation_progress_covers_only_the_build() {
        assert_monotonic(
            "creation",
            &[HorizonProgress::Build, HorizonProgress::BuildDone],
        );
    }

    #[test]
    fn launch_progress_never_goes_backwards() {
        let mut sequence = vec![HorizonProgress::VersionJson, HorizonProgress::Jar];
        for progress in 0..=3 {
            sequence.push(HorizonProgress::Libraries {
                progress,
                out_of: 3,
            });
        }
        for progress in 0..=5 {
            sequence.push(HorizonProgress::Assets {
                progress,
                out_of: 5,
            });
        }
        sequence.push(HorizonProgress::ServerJar);
        for percent in [0, 1, 25, 50, 100] {
            sequence.push(HorizonProgress::Prepare { percent });
        }
        assert_monotonic("launch", &sequence);
    }
}
