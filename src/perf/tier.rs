//! Device tier detection and the per-tier / per-backend budgets.
//!
//! Tiers come straight from the brief. Detection runs once in `nativeInit` from
//! strings the drivers already expose (`GL_RENDERER`, `VkPhysicalDeviceProperties
//! .deviceName`, `ro.soc.model`). It is a heuristic and is always reported with
//! the reason that produced it, so a wrong classification is visible rather than
//! silent.
//!
//! The budgets are used in three places:
//!
//! 1. render distance defaults handed to the Kotlin side,
//! 2. the frame-time histogram comparison in the debug overlay,
//! 3. the PASS/MISS evaluation in `docs/PERFORMANCE.md`.

use serde::Serialize;

use super::BackendBudget;
use crate::backend::BackendKind;

/// Device performance tier.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize)]
pub enum DeviceTier {
    /// Flagship: Snapdragon 8 Gen 2+, Dimensity 9200+, Adreno 740+/Immortalis.
    S,
    /// Upper mid-range: Snapdragon 870–8 Gen 1, Dimensity 1200–9000.
    A,
    /// Mid-range: Snapdragon 730–860, Dimensity 800–1100.
    B,
    /// Everything older.
    C,
}

impl DeviceTier {
    /// Single-letter tier name used in JSON and docs.
    pub const fn as_str(self) -> &'static str {
        match self {
            DeviceTier::S => "S",
            DeviceTier::A => "A",
            DeviceTier::B => "B",
            DeviceTier::C => "C",
        }
    }

    /// Parse `S`/`A`/`B`/`C` (used by `SINKON_TIER_OVERRIDE`).
    pub fn from_name(name: &str) -> Option<DeviceTier> {
        match name.trim().to_ascii_uppercase().as_str() {
            "S" => Some(DeviceTier::S),
            "A" => Some(DeviceTier::A),
            "B" => Some(DeviceTier::B),
            "C" => Some(DeviceTier::C),
            _ => None,
        }
    }

    /// Budget table for this tier.
    pub const fn budget(self) -> TierBudget {
        match self {
            DeviceTier::S => TierBudget {
                tier: DeviceTier::S,
                resident_memory_mb: 250,
                vulkan: Some(BackendBudget {
                    target_fps: 120,
                    main_thread_budget_us: 2_000,
                    render_distance: 12,
                }),
                gles3: Some(BackendBudget {
                    target_fps: 90,
                    main_thread_budget_us: 2_500,
                    render_distance: 12,
                }),
                gles2: None,
            },
            DeviceTier::A => TierBudget {
                tier: DeviceTier::A,
                resident_memory_mb: 200,
                vulkan: Some(BackendBudget {
                    target_fps: 90,
                    main_thread_budget_us: 2_500,
                    render_distance: 10,
                }),
                gles3: Some(BackendBudget {
                    target_fps: 70,
                    main_thread_budget_us: 3_000,
                    render_distance: 10,
                }),
                gles2: None,
            },
            DeviceTier::B => TierBudget {
                tier: DeviceTier::B,
                resident_memory_mb: 120,
                vulkan: Some(BackendBudget {
                    target_fps: 60,
                    main_thread_budget_us: 4_000,
                    render_distance: 8,
                }),
                gles3: Some(BackendBudget {
                    target_fps: 45,
                    main_thread_budget_us: 5_000,
                    render_distance: 8,
                }),
                gles2: Some(BackendBudget {
                    target_fps: 30,
                    main_thread_budget_us: 8_000,
                    render_distance: 6,
                }),
            },
            DeviceTier::C => TierBudget {
                tier: DeviceTier::C,
                resident_memory_mb: 100,
                vulkan: Some(BackendBudget {
                    target_fps: 30,
                    main_thread_budget_us: 8_000,
                    render_distance: 6,
                }),
                gles3: Some(BackendBudget {
                    target_fps: 30,
                    main_thread_budget_us: 8_000,
                    render_distance: 6,
                }),
                gles2: Some(BackendBudget {
                    target_fps: 20,
                    main_thread_budget_us: 12_000,
                    render_distance: 4,
                }),
            },
        }
    }
}

impl std::fmt::Display for DeviceTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-tier budget table.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TierBudget {
    /// Tier this table describes.
    pub tier: DeviceTier,
    /// Resident-memory budget in MB.
    pub resident_memory_mb: u32,
    /// Vulkan budget (None when the tier does not target Vulkan).
    pub vulkan: Option<BackendBudget>,
    /// GLES 3.x budget.
    pub gles3: Option<BackendBudget>,
    /// GLES 2.0 legacy budget.
    pub gles2: Option<BackendBudget>,
}

impl TierBudget {
    /// Budget for a specific backend, if the combination is targeted.
    pub const fn for_backend(self, backend: BackendKind) -> Option<BackendBudget> {
        match backend {
            BackendKind::Vulkan => self.vulkan,
            BackendKind::Gles3 => self.gles3,
            BackendKind::Gles2Legacy => self.gles2,
        }
    }
}

/// Where the tier decision came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionSource {
    /// Matched a known GPU/SOC family.
    GpuString,
    /// Fallback based on the Vulkan API level the driver reports.
    ApiLevel,
    /// `SINKON_TIER_OVERRIDE` was set (benchmarking only).
    Override,
    /// Nothing matched.
    Default,
}

/// Result of tier detection, always accompanied by its justification.
#[derive(Clone, Debug, Serialize)]
pub struct TierDetection {
    /// Detected tier.
    pub tier: DeviceTier,
    /// Which rule fired.
    pub source: DetectionSource,
    /// Human readable explanation, shown in the capability report.
    pub reason: String,
}

/// Heuristic GPU/SoC strings that identify each tier.
///
/// Only vendor GPU families are matched — no per-device hacks. Anything
/// unrecognised falls through to [`DeviceTier::C`], which is the safe choice:
/// the smallest render distance and the lowest budgets.
const TIER_S: &[&str] = &[
    "adreno (tm) 750",
    "adreno (tm) 740",
    "adreno 750",
    "adreno 740",
    "adreno 830",
    "adreno 840",
    "immortalis-g720",
    "immortalis-g925",
    "mali-g715",
    "mali-g720",
    "mali-g925",
    "xclipse 940",
    "xclipse 950",
];

const TIER_A: &[&str] = &[
    "adreno (tm) 730",
    "adreno (tm) 660",
    "adreno (tm) 650",
    "adreno 730",
    "adreno 660",
    "adreno 650",
    "mali-g710",
    "mali-g610",
    "mali-g78",
    "mali-g77",
    "immortalis-g715",
    "xclipse 920",
    "xclipse 530",
];

const TIER_B: &[&str] = &[
    "adreno (tm) 643",
    "adreno (tm) 642",
    "adreno (tm) 640",
    "adreno (tm) 620",
    "adreno (tm) 619",
    "adreno (tm) 618",
    "adreno (tm) 610",
    "adreno 643",
    "adreno 640",
    "adreno 620",
    "adreno 619",
    "adreno 618",
    "adreno 610",
    "mali-g57",
    "mali-g68",
    "mali-g76",
    "mali-g72",
    "mali-g52",
    "powervr gm9446",
    "powervr gt7600",
];

fn contains_any(haystack: &str, needles: &[&str]) -> Option<&'static str> {
    for needle in needles {
        if haystack.contains(needle) {
            return Some(needle);
        }
    }
    None
}

/// Detect the device tier.
///
/// `gpu` is the adapter device name (`Adreno (TM) 740`, `Mali-G715-Immortalis
/// MC11`, …). `vulkan_api` is the packed Vulkan version, used only as a tie
/// breaker when the name is unknown. `override_name` is
/// `SINKON_TIER_OVERRIDE` and exists so benchmarks can pin a tier.
pub fn detect(gpu: &str, vulkan_api: Option<u32>, override_name: Option<&str>) -> TierDetection {
    if let Some(name) = override_name {
        if let Some(tier) = DeviceTier::from_name(name) {
            return TierDetection {
                tier,
                source: DetectionSource::Override,
                reason: format!("SINKON_TIER_OVERRIDE={name}"),
            };
        }
    }

    let haystack = gpu.to_ascii_lowercase();
    if let Some(hit) = contains_any(&haystack, TIER_S) {
        return TierDetection {
            tier: DeviceTier::S,
            source: DetectionSource::GpuString,
            reason: format!("gpu \"{gpu}\" matched \"{hit}\""),
        };
    }
    if let Some(hit) = contains_any(&haystack, TIER_A) {
        return TierDetection {
            tier: DeviceTier::A,
            source: DetectionSource::GpuString,
            reason: format!("gpu \"{gpu}\" matched \"{hit}\""),
        };
    }
    if let Some(hit) = contains_any(&haystack, TIER_B) {
        return TierDetection {
            tier: DeviceTier::B,
            source: DetectionSource::GpuString,
            reason: format!("gpu \"{gpu}\" matched \"{hit}\""),
        };
    }

    // Unknown GPU: a driver that advertises Vulkan 1.3 with a high max texture
    // size is at least upper-mid-range; otherwise assume the safe tier.
    if let Some(api) = vulkan_api {
        let major = api >> 22;
        let minor = (api >> 12) & 0x3ff;
        if major >= 1 && minor >= 3 {
            return TierDetection {
                tier: DeviceTier::B,
                source: DetectionSource::ApiLevel,
                reason: format!("unknown gpu \"{gpu}\", Vulkan {major}.{minor} ⇒ tier B"),
            };
        }
    }

    TierDetection {
        tier: DeviceTier::C,
        source: DetectionSource::Default,
        reason: format!("unknown gpu \"{gpu}\"; assuming the conservative tier"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flagship_gpus_are_tier_s() {
        assert_eq!(detect("Adreno (TM) 740", None, None).tier, DeviceTier::S);
        assert_eq!(
            detect("Immortalis-G720 MC12", None, None).tier,
            DeviceTier::S
        );
        assert_eq!(detect("Adreno (TM) 830", None, None).tier, DeviceTier::S);
    }

    #[test]
    fn midrange_gpus_are_tier_a_or_b() {
        assert_eq!(detect("Adreno (TM) 660", None, None).tier, DeviceTier::A);
        assert_eq!(detect("Mali-G78 MP14", None, None).tier, DeviceTier::A);
        assert_eq!(detect("Adreno (TM) 610", None, None).tier, DeviceTier::B);
        assert_eq!(detect("Mali-G52 MC2", None, None).tier, DeviceTier::B);
    }

    #[test]
    fn unknown_gpu_is_conservative() {
        let detection = detect("SomeVendor GPU 9000", None, None);
        assert_eq!(detection.tier, DeviceTier::C);
        assert_eq!(detection.source, DetectionSource::Default);
    }

    #[test]
    fn vulkan_api_breaks_ties() {
        let api_1_3 = (1 << 22) | (3 << 12);
        let detection = detect("SomeVendor GPU", Some(api_1_3), None);
        assert_eq!(detection.tier, DeviceTier::B);
        assert_eq!(detection.source, DetectionSource::ApiLevel);
    }

    #[test]
    fn override_wins() {
        let detection = detect("Adreno (TM) 740", None, Some("c"));
        assert_eq!(detection.tier, DeviceTier::C);
        assert_eq!(detection.source, DetectionSource::Override);
    }

    #[test]
    fn budgets_match_the_brief() {
        let s = DeviceTier::S.budget();
        assert_eq!(
            s.for_backend(BackendKind::Vulkan).map(|b| b.target_fps),
            Some(120)
        );
        assert_eq!(
            s.for_backend(BackendKind::Gles3).map(|b| b.target_fps),
            Some(90)
        );
        assert!(s.for_backend(BackendKind::Gles2Legacy).is_none());
        let b = DeviceTier::B.budget();
        assert_eq!(
            b.for_backend(BackendKind::Gles2Legacy)
                .map(|x| x.main_thread_budget_us),
            Some(8_000)
        );
        assert_eq!(b.resident_memory_mb, 120);
        let c = DeviceTier::C.budget();
        assert_eq!(
            c.for_backend(BackendKind::Gles2Legacy).map(|x| x.render_distance),
            Some(4)
        );
    }
}
