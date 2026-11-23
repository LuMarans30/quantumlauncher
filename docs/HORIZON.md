# Horizon

[Horizon](https://playhorizon.dev) is a Rust implementation of Minecraft: Java Edition. It runs natively, without a JVM. QuantumLauncher supports Horizon Nightly for **client instances only**. Both Nightly and Horizon’s [Launch API](https://github.com/horizon-rust/horizon/blob/main/docs/api/launch.md) are experimental.

## First launch

1. Create a client instance, choose **Horizon (nightly)** from the version list and give it a name. The launcher downloads Horizon and verifies the archive against its signed checksums. It doesn’t download Minecraft yet.
2. Select the instance and press **Play**. The first launch for a build takes a few minutes: the launcher downloads the Minecraft version that build needs, then runs `horizon-prepare` to make the game resources. If the preparer needs Java, the launcher installs it for you.
3. Next time, press **Play** as usual. Instances on the same Horizon build share the prepared resources, but not their saves or settings.

**Accounts:** Sign in with a Microsoft account that owns Minecraft: Java Edition. Offline, Ely.by and LittleSkin profiles can’t launch Horizon.

## Requirements

| System | Requirements |
| --- | --- |
| Windows | x64 or ARM64; Vulkan or DirectX 12 |
| macOS | 12 or later; Intel or Apple Silicon |
| Linux | x86_64; Vulkan or OpenGL 3.3 |

There’s no launcher build of Horizon for Linux ARM64 or BSD.

## Mods, shaders and resource packs

Stop the game, then open **Mods** on the instance. Choose **Download Content…** to browse the Horizon registry, or **Add File…** if you already have the package. Both accept the same formats:

| Package | How to use it |
| --- | --- |
| `.hmod` (content or WebAssembly mod) | New mods start disabled. Enable the mod in the launcher, then start Horizon. |
| `.hshader` (Horizon shader) | Select the pack in Horizon’s shader settings. |
| `.zip` (resource pack) | Leave it zipped. The launcher checks `pack.mcmeta` and copies the ZIP to `resourcepacks/`. Enable it in Horizon’s Resource Packs settings. |
| `.zip` (Iris/OptiFine GLSL shader pack) | Leave it zipped. The launcher checks for `.fsh` or `.vsh` programs under `shaders/` and copies the ZIP to `shaderpacks/`. Select it in-game. |

Use **Resource Packs** to filter either the installed list or the Horizon registry. **Shaders** shows both shader formats. Click a project to read about it, the download icon to install it, or the bin icon to uninstall it. The launcher checks the Minecraft version, skips revoked releases and verifies the checksum and any author signature. Mods and `.hshader` packs must also match the build’s content API; resource packs and GLSL ZIPs don’t use that API.

If a mod won’t load, check Horizon’s Mods screen and logs: the launcher doesn’t install dependencies for you. For a Java mod JAR, look for a Horizon port instead. The manager doesn’t install native mods or registry modpacks or datapacks. You can still add datapacks manually.

## Updating

Nightly updates are manual. Open **Edit → Check for updates**, then choose **Update Horizon**. If you already have the latest build but its installation is missing or incomplete, choose **Repair Horizon** instead.

The launcher switches the instance to the new build only after the download is verified and unpacked. Your worlds, options and packs stay where they are. The next launch prepares resources for the new build if needed. Old builds stay on disk because other instances may still use them.

## Instance settings

**Edit** shows the Horizon build ID and Minecraft version. Window size, environment variables and supported game arguments apply. Instance variables override launcher-wide ones, but you can’t override the session or game folders through arguments or environment variables. Java heap size, JVM arguments, custom JARs and pre-launch prefixes don’t apply to Horizon.

## Where files live

Paths below are relative to the launcher’s data folder (`~/.local/share/QuantumLauncher/` on Linux).

| Path | What’s there |
| --- | --- |
| `instances/<name>/.minecraft/` | Saves, options, `horizon-mods/`, `resourcepacks/` and `shaderpacks/` |
| `instances/<name>/horizon/` | Horizon logs, crash reports and registry key receipts |
| `instances/<name>/config.json` | Instance settings, including the selected Horizon build |
| `horizon/builds/<build-id>/` | Downloaded binaries. The ID includes part of the archive checksum because the `nightly` tag is reused. |
| `horizon/prepared/<build-id>/` | Resources prepared for that build |
| `horizon/minecraft/<version>/` | Minecraft files used by the preparer |

Deleting an instance removes its saves and settings, not the shared downloads. To reclaim space, remove only build and resource folders that no remaining instance needs.
