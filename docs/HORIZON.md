# Horizon

[Horizon](https://playhorizon.dev) is Minecraft: Java Edition
reimplemented in Rust: it runs the game natively (wgpu), without Java,
and prepares its own resources from a Minecraft installation the player
already owns. QuantumLauncher can install, prepare and launch it.

> **Experimental.** Horizon's
> [Launch API](https://github.com/horizon-rust/horizon/blob/main/docs/api/launch.md)
> and its Nightly release channel are both experimental upstream, and
> this integration follows them. Expect breakage when Horizon changes.

## Using it

1. Open the **create instance** screen.
2. Pick **Horizon (nightly)** in the version list (it is always at the
   top, above the Minecraft versions) and give the instance a name.
3. Create it. QuantumLauncher downloads the newest Horizon Nightly for
   your platform, verifies its Ed25519-signed `SHA256SUMS`, unpacks it
   and writes the instance. **Creating an instance makes no Mojang
   requests.**
4. Log in with a **Microsoft account that owns Minecraft: Java
   Edition**, select the instance and press Play. The first launch
   downloads the Minecraft files Horizon's preparer reads (the version
   Horizon is pinned to, currently 26.3) and runs `horizon-prepare`,
   which builds the game's resources from them (a few minutes, once per
   Horizon build).

Horizon has no Java at play time. The Java 25 that `horizon-prepare`
needs is installed automatically (like for any other instance).

The preparation is behind the account gate: without an owning Microsoft
account (or with offline play off), the launcher refuses before it
downloads any Mojang file.

## Offline play (off by default)

Offline accounts are refused, because Horizon's Launch API asks
launchers to start it only for an account that owns Minecraft: Java
Edition. The offline path is implemented and tested, behind one switch:

```rust
// crates/ql_horizon/src/lib.rs
pub const ALLOW_OFFLINE: bool = false; // <- `true` allows offline accounts
```

With it on, an offline (or Ely.by/LittleSkin) profile starts the game
with `--name` instead of a session; online-mode servers refuse the
player, exactly like offline vanilla. Signed-in play is unaffected.

## Requirements

Horizon supports:

| Platform | Supported |
| --- | --- |
| Windows x64 / ARM64 | ✅ needs Vulkan or DirectX 12 |
| macOS 12+ (Intel, Apple Silicon) | ✅ |
| Linux x86_64 | ✅ needs Vulkan or OpenGL 3.3 |
| Linux ARM64, BSDs, other architectures | ❌ no upstream build |

The launcher refuses unsupported platforms with a clear message.

## Where things are kept

Everything is under the launcher's data folder
(`~/.local/share/QuantumLauncher/` on Linux):

| Path | Contents |
| --- | --- |
| `horizon/builds/<nightly>-<sha>/` | Horizon's binaries (`horizon-client`, `horizon-prepare`, …) |
| `horizon/prepared/<nightly>-<sha>/` | Resources `horizon-client --resources` reads; one per build, shared by all instances |
| `horizon/minecraft/<version>/` | Version JSON, client jar, libraries, assets and server jar used for preparing |
| `instances/<name>/` | The instance's `config.json` (with a `horizon` section) |
| `instances/<name>/.minecraft/` | The game folder: saves, options, packs, `horizon-mods/` |
| `instances/<name>/horizon/` | Horizon's logs (`logs/latest.log`) and crash reports |

Build folders are named after the archive's SHA-256 because the
`nightly` tag is republished every day: the checksum, not the tag, says
which build is on disk.

Deleting an instance only deletes its folder; the shared build and
prepared resources stay (other instances may use them). Delete
`horizon/` to reclaim that space.

## What isn't supported yet

- **Mods and packs**: Horizon uses its own `.hmod`/`.hnmod` formats and
  its own registry. QuantumLauncher's mod store, mod manager and modpack
  import are Java-shaped and don't apply. The game folder's
  `horizon-mods/`, `resourcepacks/`, `shaderpacks/` and `datapacks/`
  can still be filled by hand.
- **Horizon servers**: only client instances are created for now, even
  though Horizon ships a `horizon-server` binary.
- **RAM/Java settings**: they mean nothing to Horizon, so the
  corresponding instance settings are ignored. Window size, environment
  variables and extra game arguments still apply (extra arguments are
  passed to `horizon-client` as-is; it refuses options it doesn't list
  with exit code 2).
- **Updating the build**: a new Nightly means creating a new instance.
  Checking `horizon-prepare --check` happens on every launch, so a
  broken resources folder is re-prepared automatically.
- **Offline accounts** need [`ALLOW_OFFLINE`](#offline-play-off-by-default)
  turned on, because upstream asks launchers to require an owning
  Microsoft account.

## Implementation

- Release archives are accepted only after `SHA256SUMS.sig` (Ed25519
  over `horizon release <tag>\n` plus the exact `SHA256SUMS` bytes)
  verifies against Horizon's pinned release keys; then the archive's
  SHA-256 is checked. A missing or invalid signature fails closed.
- `crates/ql_horizon` talks to Horizon only through the documented
  Launch API (`horizon-client --launch-api`,
  `horizon-prepare --launch-api`, `--session-stdin`, exit codes); no
  Horizon code is linked into the launcher, and its Rust edition/MSRV
  requirements don't affect this workspace.
- `crates/ql_instances` routes instance creation and launching to it:
  a `config.json` with a `horizon` section means "not a Minecraft
  instance", so the Java argument/classpath pipeline is skipped.
- The instance's account is handed over as the session JSON on standard
  input, never on the command line.
