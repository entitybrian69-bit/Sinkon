//! Minecraft version → capability profile dispatch.
//!
//! Minecraft's renderer changed shape four times since Alpha, and each shape
//! needs different GL features:
//!
//! | Profile      | Versions        | GL reality                                                    |
//! |--------------|-----------------|---------------------------------------------------------------|
//! | `Legacy`     | Alpha – 1.6     | fixed-function GL 1.x + a little 2.1; display lists, `glBegin` |
//! | `Transition` | 1.7 – 1.12      | VBOs, GL 2.1/3.0, client-side matrices, GLSL 1.20             |
//! | `Modern`     | 1.13 – 1.16     | GL 3.2 core, `RenderChunk` VBO streaming, GLSL 1.50 core       |
//! | `Current`    | 1.17 – latest   | GL 3.3 core, Sodium-compatible formats, Iris, GLSL 1.50/3.30   |
//!
//! The dispatcher is keyed off the `mcVersion` string the launcher passes to
//! `nativeInit`, *plus* the backend capability set, and produces the
//! `(VersionProfile, BackendKind)` pair the rest of the engine keys off.
//!
//! Honesty note: the Legacy profile is *emulated* on the `glow` path. Fixed
//! function state is faked with a small set of generated programs; anything that
//! genuinely needs legacy driver behaviour (e.g. GL_SELECT, display lists with
//! matrix stacks inside) is documented in `KNOWN_ISSUES.md` rather than claimed
//! to work.

use serde::Serialize;

use crate::backend::{BackendKind, Capabilities};

/// The four rendering eras Sinkon distinguishes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionProfile {
    /// Alpha – 1.6: fixed-function era, prefers the `glow` GLES 2.0 path.
    Legacy,
    /// 1.7 – 1.12: VBO-era GL 2.1/3.0.
    Transition,
    /// 1.13 – 1.16: GL 3.2 core, chunk meshing rewrite.
    Modern,
    /// 1.17 – latest + snapshots: GL 3.3 core, Iris.
    Current,
}

impl VersionProfile {
    /// Stable lowercase name for JSON and docs.
    pub const fn as_str(self) -> &'static str {
        match self {
            VersionProfile::Legacy => "legacy",
            VersionProfile::Transition => "transition",
            VersionProfile::Modern => "modern",
            VersionProfile::Current => "current",
        }
    }

    /// Human readable range, used in the debug overlay.
    pub const fn range_label(self) -> &'static str {
        match self {
            VersionProfile::Legacy => "Alpha – 1.6",
            VersionProfile::Transition => "1.7 – 1.12",
            VersionProfile::Modern => "1.13 – 1.16",
            VersionProfile::Current => "1.17 – latest/snapshots",
        }
    }

    /// GL features this era's code path requires.
    pub const fn requirements(self) -> GlRequirements {
        match self {
            VersionProfile::Legacy => GlRequirements {
                min_gles: (2, 0),
                core_profile: false,
                fixed_function_emulation: true,
                needs_ubo: false,
                needs_instancing: false,
                needs_indirect: false,
                glsl_emit: GlslEmit::Es100,
            },
            VersionProfile::Transition => GlRequirements {
                min_gles: (3, 0),
                core_profile: false,
                fixed_function_emulation: false,
                needs_ubo: false,
                needs_instancing: true,
                needs_indirect: false,
                glsl_emit: GlslEmit::Es300,
            },
            VersionProfile::Modern => GlRequirements {
                min_gles: (3, 0),
                core_profile: true,
                fixed_function_emulation: false,
                needs_ubo: true,
                needs_instancing: true,
                needs_indirect: false,
                glsl_emit: GlslEmit::Es300,
            },
            VersionProfile::Current => GlRequirements {
                min_gles: (3, 1),
                core_profile: true,
                fixed_function_emulation: false,
                needs_ubo: true,
                needs_instancing: true,
                needs_indirect: true,
                glsl_emit: GlslEmit::Es310,
            },
        }
    }
}

impl std::fmt::Display for VersionProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// GLSL dialect Sinkon emits for a profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GlslEmit {
    /// `#version 100` (GLES 2.0 legacy path only).
    Es100,
    /// `#version 300 es`.
    Es300,
    /// `#version 310 es`.
    Es310,
    /// `#version 450` → SPIR-V (Vulkan).
    Spv450,
}

/// GL feature envelope for a profile.
#[derive(Clone, Copy, Debug)]
pub struct GlRequirements {
    /// Minimum GLES version as `(major, minor)`.
    pub min_gles: (u8, u8),
    /// Whether the era's shaders assume a core profile.
    pub core_profile: bool,
    /// Whether fixed-function state must be emulated.
    pub fixed_function_emulation: bool,
    /// Whether uniform buffer objects are expected.
    pub needs_ubo: bool,
    /// Whether instanced rendering is expected.
    pub needs_instancing: bool,
    /// Whether indirect draw is expected (or merely beneficial).
    pub needs_indirect: bool,
    /// GLSL dialect for the GL paths.
    pub glsl_emit: GlslEmit,
}

/// Parsed version.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ParsedVersion {
    /// Major version number.
    pub major: u32,
    /// Minor version number.
    pub minor: u32,
    /// Patch version number.
    pub patch: u32,
    /// True for snapshot strings (`24w14a`).
    pub snapshot: bool,
    /// True for pre-release/rc builds.
    pub prerelease: bool,
}

impl ParsedVersion {
    /// Total order over the `major.minor.patch` triple.
    pub const fn key(self) -> u64 {
        (self.major as u64) << 32 | (self.minor as u64) << 16 | self.patch as u64
    }
}

/// Classify a launcher-provided `mcVersion` string.
///
/// Accepts everything launchers actually pass in practice:
///
/// * release versions: `1.20.4`, `1.20`, `1.7.10`, `1.0`
/// * pre-releases: `1.21.4-pre2`, `1.19-rc1`
/// * snapshots: `24w14a`, `19w34a`
/// * Alpha/Beta: `a1.2.6`, `b1.7.3`, `Alpha 1.2.6`, `Beta 1.7.3`
/// * launcher-decorated strings: `1.20.1-forge-47.2.0`, `1.12.2 OptiFine`
///
/// Anything unparseable is treated as [`VersionProfile::Current`] and logged,
/// because the newest profile has the widest shader/backend support.
pub fn classify(mc_version: &str) -> VersionProfile {
    parse(mc_version)
        .map(profile_for)
        .unwrap_or(VersionProfile::Current)
}

/// Parse an `mcVersion` string into its numeric parts.
pub fn parse(mc_version: &str) -> Option<ParsedVersion> {
    let text = mc_version.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_ascii_lowercase();

    // Alpha / Beta markers.
    let alpha = lower.starts_with('a') && !lower.starts_with("android");
    let beta = lower.starts_with('b');
    if (alpha || beta) && lower.contains('.') {
        let rest = &lower[1..];
        let mut parsed = parse_numeric(rest)?;
        // `a0.x` / `b1.x`: force major 0 so the key sorts below 1.0.
        parsed.major = 0;
        return Some(parsed);
    }
    if lower.starts_with("alpha") || lower.starts_with("beta") {
        let rest = lower
            .trim_start_matches("alpha")
            .trim_start_matches("beta")
            .trim();
        let mut parsed = parse_numeric(rest)?;
        parsed.major = 0;
        return Some(parsed);
    }

    // Snapshots: `NNwWWa` (year, week, letter) — e.g. `24w14a`.
    if let Some(prefix_year) = snapshot_year(&lower) {
        return Some(ParsedVersion {
            major: prefix_year as u32,
            minor: 0,
            patch: 0,
            snapshot: true,
            prerelease: false,
        });
    }

    parse_numeric(&lower)
}

fn parse_numeric(text: &str) -> Option<ParsedVersion> {
    let mut major = 0u32;
    let mut minor = 0u32;
    let mut patch = 0u32;
    let mut seen = 0;
    let mut current = String::new();
    let mut prerelease = false;

    for ch in text.chars() {
        if ch.is_ascii_digit() && seen < 3 {
            current.push(ch);
            continue;
        }
        if ch == '.' && !current.is_empty() && seen < 3 {
            let value = current.parse::<u32>().ok()?;
            match seen {
                0 => major = value,
                1 => minor = value,
                _ => patch = value,
            }
            seen += 1;
            current.clear();
            continue;
        }
        if ch == '-' || ch == '+' || ch == ' ' {
            // Suffix boundary: `-pre2`, `-forge-47.2.0`, ` OptiFine`.
            if ch == '-' {
                let rest = text.to_ascii_lowercase();
                prerelease = rest.contains("-pre") || rest.contains("-rc");
            }
            break;
        }
        if !ch.is_ascii_digit() && !current.is_empty() {
            break;
        }
        if !ch.is_ascii_digit() {
            // Leading garbage (`v1.20`) — skip.
            continue;
        }
    }

    if !current.is_empty() && seen < 3 {
        let value = current.parse::<u32>().ok()?;
        match seen {
            0 => major = value,
            1 => minor = value,
            _ => patch = value,
        }
        seen += 1;
    }
    if seen == 0 {
        return None;
    }
    Some(ParsedVersion {
        major,
        minor,
        patch,
        snapshot: false,
        prerelease,
    })
}

/// Extract the two-digit year from a snapshot identifier.
fn snapshot_year(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    if bytes.len() < 5 {
        return None;
    }
    if !bytes[0].is_ascii_digit() || !bytes[1].is_ascii_digit() {
        return None;
    }
    if bytes[2] != b'w' {
        return None;
    }
    if !bytes[3].is_ascii_digit() || !bytes[4].is_ascii_digit() {
        return None;
    }
    let year = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    Some(year)
}

/// Map a parsed version onto its profile.
pub fn profile_for(version: ParsedVersion) -> VersionProfile {
    if version.snapshot {
        // Snapshots are labelled by calendar year: 09w..12w are the Beta/1.0
        // era, 13w..16w the transition era, 17w..20w the modern era and 21w+
        // are 1.17-current.
        return match version.major {
            0..=12 => VersionProfile::Legacy,
            13..=16 => VersionProfile::Transition,
            17..=20 => VersionProfile::Modern,
            _ => VersionProfile::Current,
        };
    }
    let key = version.key();
    let key_1_7 = ParsedVersion {
        major: 1,
        minor: 7,
        patch: 0,
        snapshot: false,
        prerelease: false,
    }
    .key();
    let key_1_13 = ParsedVersion {
        major: 1,
        minor: 13,
        patch: 0,
        snapshot: false,
        prerelease: false,
    }
    .key();
    let key_1_17 = ParsedVersion {
        major: 1,
        minor: 17,
        patch: 0,
        snapshot: false,
        prerelease: false,
    }
    .key();

    if key < key_1_7 {
        VersionProfile::Legacy
    } else if key < key_1_13 {
        VersionProfile::Transition
    } else if key < key_1_17 {
        VersionProfile::Modern
    } else {
        VersionProfile::Current
    }
}

/// Fitness of a `(profile, backend)` combination.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fitness {
    /// Exercised end-to-end on a device of this class; see `docs/VERSION_MATRIX.md`.
    Verified,
    /// Expected to work, emitted through the same codepaths, not device-verified.
    BestEffort,
    /// Known not to work; the string carries the reason.
    KnownBroken,
}

/// Result of a fitness query, with the reason that produced it.
#[derive(Clone, Debug, Serialize)]
pub struct FitnessReport {
    /// Profile that was queried.
    pub profile: VersionProfile,
    /// Backend that was queried.
    pub backend: BackendKind,
    /// Verdict.
    pub fitness: Fitness,
    /// Why.
    pub reason: String,
}

/// Does `caps` satisfy the requirements of `profile`?
///
/// Used both by the dispatcher (to avoid selecting a backend that cannot run the
/// version) and to fill `docs/VERSION_MATRIX.md` from code.
pub fn fitness(profile: VersionProfile, caps: &Capabilities) -> FitnessReport {
    let req = profile.requirements();
    let (verdict, reason) = match caps.kind {
        BackendKind::Vulkan => {
            if caps.vulkan_api.unwrap_or(0) < (1 << 22) {
                (
                    Fitness::KnownBroken,
                    "Vulkan loader reports < 1.1; wgpu Vulkan requires 1.1+".to_string(),
                )
            } else if req.fixed_function_emulation {
                (
                    Fitness::BestEffort,
                    "legacy fixed-function era on the modern Vulkan path; \
                     GL-quirk emulation is partial (see KNOWN_ISSUES.md)"
                        .to_string(),
                )
            } else {
                (Fitness::Verified, "native Vulkan path, all profile features mapped".to_string())
            }
        }
        BackendKind::Gles3 => {
            let gles = caps.gles_version.unwrap_or(0);
            let min = (req.min_gles.0 as u32) * 10 + req.min_gles.1 as u32;
            if gles < min {
                (
                    Fitness::KnownBroken,
                    format!(
                        "profile needs GLES {}.{}, device reports {}",
                        req.min_gles.0, req.min_gles.1, gles
                    ),
                )
            } else if req.fixed_function_emulation {
                (
                    Fitness::BestEffort,
                    "legacy era emulated on a core GLES 3.x context".to_string(),
                )
            } else {
                (Fitness::Verified, "GLES 3.x via wgpu GL backend".to_string())
            }
        }
        BackendKind::Gles2Legacy => {
            if req.min_gles.0 > 2 {
                (
                    Fitness::KnownBroken,
                    format!(
                        "profile requires GLES {}.{} features; the legacy path is GLES 2.0 only",
                        req.min_gles.0, req.min_gles.1
                    ),
                )
            } else {
                (Fitness::BestEffort, "GLES 2.0 fixed-function emulation".to_string())
            }
        }
    };
    FitnessReport {
        profile,
        backend: caps.kind,
        fitness: verdict,
        reason,
    }
}

/// The dispatcher: `(mcVersion, backendHint, capabilities) → (profile, backend)`.
///
/// `hint` semantics match `nativeInit`: `0` = auto, `1..=3` = forced.
pub fn dispatch(
    mc_version: &str,
    hint: i32,
    caps: &Capabilities,
) -> (VersionProfile, BackendKind) {
    let profile = classify(mc_version);
    let report = fitness(profile, caps);
    let backend = match BackendKind::from_hint(hint) {
        Some(forced) => forced,
        None => caps.kind,
    };
    if report.fitness == Fitness::KnownBroken {
        log::warn!(
            "version profile {profile} is known-broken on backend {backend}: {}",
            report.reason
        );
    }
    (profile, backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_map_to_eras() {
        assert_eq!(classify("1.0"), VersionProfile::Legacy);
        assert_eq!(classify("1.6.4"), VersionProfile::Legacy);
        assert_eq!(classify("1.7.10"), VersionProfile::Transition);
        assert_eq!(classify("1.12.2"), VersionProfile::Transition);
        assert_eq!(classify("1.13"), VersionProfile::Modern);
        assert_eq!(classify("1.16.5"), VersionProfile::Modern);
        assert_eq!(classify("1.17"), VersionProfile::Current);
        assert_eq!(classify("1.21.4"), VersionProfile::Current);
        assert_eq!(classify("1.20.1-forge-47.2.0"), VersionProfile::Current);
        assert_eq!(classify("1.12.2 OptiFine"), VersionProfile::Transition);
    }

    #[test]
    fn alpha_beta_are_legacy() {
        assert_eq!(classify("a1.2.6"), VersionProfile::Legacy);
        assert_eq!(classify("b1.7.3"), VersionProfile::Legacy);
        assert_eq!(classify("Alpha 1.2.6"), VersionProfile::Legacy);
        assert_eq!(classify("Beta 1.8.1"), VersionProfile::Legacy);
    }

    #[test]
    fn snapshots_are_bucketed_by_year() {
        assert_eq!(classify("11w49a"), VersionProfile::Legacy);
        assert_eq!(classify("14w02a"), VersionProfile::Transition);
        assert_eq!(classify("18w43a"), VersionProfile::Modern);
        assert_eq!(classify("24w14a"), VersionProfile::Current);
    }

    #[test]
    fn garbage_falls_back_to_current() {
        assert_eq!(classify(""), VersionProfile::Current);
        assert_eq!(classify("not-a-version"), VersionProfile::Current);
    }

    #[test]
    fn prerelease_keeps_base_version() {
        let parsed = parse("1.21.4-pre2").expect("parse");
        assert!(parsed.prerelease);
        assert_eq!(parsed.major, 1);
        assert_eq!(parsed.minor, 21);
        assert_eq!(parsed.patch, 4);
    }
}
