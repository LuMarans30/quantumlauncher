use std::path::PathBuf;

use ql_core::{IoError, JsonError, RequestError, impl_3_errs_jri};

const ERR_PREFIX: &str = "while using Horizon:\n";

#[derive(Debug, thiserror::Error)]
pub enum HorizonError {
    #[error("{ERR_PREFIX}{0}")]
    Io(#[from] IoError),
    #[error("{ERR_PREFIX}{0}")]
    Json(#[from] JsonError),
    #[error("{ERR_PREFIX}{0}")]
    Request(#[from] RequestError),

    #[error(
        "Horizon doesn't have a build for this platform ({0})\n\nHorizon supports Windows (x64, ARM64), macOS (Intel, Apple Silicon) and Linux x86_64"
    )]
    UnsupportedPlatform(String),

    #[error("the Horizon release has no build for this platform ({0})")]
    NoAssetForPlatform(String),
    #[error("the Horizon release is missing its file: {0}")]
    AssetNotFound(String),
    #[error("the Horizon release's SHA256SUMS has no entry for {0}")]
    NoChecksum(String),
    #[error(
        "Horizon release {tag}'s SHA256SUMS has no valid signature from the pinned release keys\n\nThe release may be mid-republish; try again shortly. Refusing to install it"
    )]
    ReleaseSignatureInvalid { tag: String },
    #[error("the Horizon release points its files somewhere unexpected:\n{0}")]
    UnexpectedAssetUrl(String),
    #[error(
        "Horizon's download is damaged:\nexpected SHA-256: {expected}\n     got SHA-256: {actual}"
    )]
    ChecksumMismatch { expected: String, actual: String },
    #[error("Horizon's download has no horizon-client binary")]
    ClientMissingFromDownload,
    #[error("Horizon's download has a file it has no business shipping: {entry}\n({reason})")]
    UnsafeArchive { entry: String, reason: &'static str },
    #[error("couldn't unpack Horizon's download ({path}):\n{error}")]
    Archive { path: PathBuf, error: String },

    #[error("{ERR_PREFIX}instance name is invalid (empty/disallowed characters)")]
    InvalidName,
    #[error("an instance with that name already exists: {0}")]
    InstanceAlreadyExists(String),

    #[error("this instance points at an invalid Horizon build ({0:?})")]
    InvalidBuildId(String),
    #[error(
        "Horizon only runs with a Microsoft account that owns Minecraft: Java Edition\n\nLog in with such an account (offline accounts and Ely.by/LittleSkin can't be used)"
    )]
    OfflineDisabled,
    #[error(
        "Horizon was given an incomplete Microsoft session (name, UUID or access token is empty)"
    )]
    InvalidSession,
    #[error(
        "Horizon's build is missing or incomplete in:\n{0}\n\nCreate a new Horizon instance to download it again"
    )]
    BuildMissing(PathBuf),
    #[error(
        "Horizon's build wasn't installed completely in:\n{0}\n\nTry creating the instance again"
    )]
    InstalledIncomplete(PathBuf),
    #[error("another Horizon install is still running, and it didn't finish in time")]
    InstallBusy,

    #[error("Horizon needs a Minecraft version that isn't in the version manifest: {0}")]
    MinecraftVersionNotFound(String),
    #[error(
        "this Horizon build doesn't know Minecraft {version}'s version file (SHA-1 {actual}, expected one of: {expected})\n\nMojang republished the version after this build was made. Create a new Horizon instance once a newer Nightly is out."
    )]
    VersionJsonMismatch {
        version: String,
        expected: String,
        actual: String,
    },
    #[error(
        "this Horizon build has no readable research-lock.json:\n{0}\n\nIt can't say which Minecraft files to prepare from, so it is treated as broken"
    )]
    ResearchLockMissing(PathBuf),
    #[error("this Horizon build's research-lock.json can't be read ({path}):\n{error}")]
    ResearchLockInvalid { path: PathBuf, error: String },
    #[error(
        "a Minecraft file doesn't match Horizon's pins:\n{path}\nexpected SHA-1: {expected}\n     got SHA-1: {actual}"
    )]
    FileHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("Horizon needs Java {0}, which this launcher doesn't manage")]
    UnsupportedJava(u32),
    #[error("{ERR_PREFIX}{0}")]
    JavaInstall(#[from] ql_java_handler::JavaInstallError),

    #[error("Horizon's resource preparation failed (exit code {code})\n{output}")]
    PrepareFailed { code: i32, output: String },
    #[error("Horizon's resources are still incomplete after preparing:\n{0}")]
    PrepareIncomplete(String),

    #[error(
        "this Horizon build no longer accepts {option} (Launch API {api_version})\n\nIt may have changed the option; update the Horizon build by creating a new instance"
    )]
    UnsupportedOption { option: String, api_version: String },
    #[error(
        "these extra game arguments aren't allowed for Horizon instances: {option}\n\nThey control the launcher's own setup (session, folders, server mode), so they can't come from an instance's settings"
    )]
    ForbiddenOption { option: String },
    #[error("this extra game argument can't be passed to Horizon: {argument}\n({reason})")]
    InvalidExtraArgument {
        argument: String,
        reason: &'static str,
    },

    #[error("couldn't run Horizon ({program}): {error}")]
    CommandError {
        program: PathBuf,
        error: std::io::Error,
    },
    #[error("{program} failed ({status}):\n{stderr}")]
    CommandFailed {
        program: PathBuf,
        status: String,
        stderr: String,
    },
    #[error("{program} didn't finish in {seconds} seconds")]
    CommandTimedOut { program: PathBuf, seconds: u64 },
    #[error("{what} didn't answer in {seconds} seconds")]
    RequestTimedOut { what: String, seconds: u64 },

    #[error("couldn't understand Horizon's reply:\n{0}")]
    BadApiReply(String),

    #[error("{ERR_PREFIX}{0}")]
    Message(String),
}

impl_3_errs_jri!(HorizonError, Json, Request, Io);

impl From<reqwest::Error> for HorizonError {
    fn from(value: reqwest::Error) -> Self {
        Self::Request(RequestError::ReqwestError(value))
    }
}
