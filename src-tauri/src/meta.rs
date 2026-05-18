use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PackageMeta {
    #[serde(default)]
    pub creator_name: String,
    #[serde(default)]
    pub package_name: String,
    #[serde(default)]
    pub license_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub program_version: Option<String>,
    #[serde(default)]
    pub content_list: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackageType {
    Scene,
    Look,
    Morph,
    Texture,
    Clothing,
    Hair,
    Plugin,
    Asset,
    Pose,
    Sound,
    SubScene,
    Mixed,
    Unknown,
}

impl PackageType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scene => "Scene",
            Self::Look => "Look",
            Self::Morph => "Morph",
            Self::Texture => "Texture",
            Self::Clothing => "Clothing",
            Self::Hair => "Hair",
            Self::Plugin => "Plugin",
            Self::Asset => "Asset",
            Self::Pose => "Pose",
            Self::Sound => "Sound",
            Self::SubScene => "SubScene",
            Self::Mixed => "Mixed",
            Self::Unknown => "Unknown",
        }
    }

}

/// Parse a meta.json byte slice into PackageMeta. Tolerates field-shape variations
/// VAM has shipped over the years (dependencies as object map, missing fields, etc).
pub fn parse_meta(bytes: &[u8]) -> anyhow::Result<PackageMeta> {
    let v: serde_json::Value = serde_json::from_slice(bytes)?;
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("meta.json root is not an object"))?;

    fn s(o: &serde_json::Map<String, serde_json::Value>, k: &str) -> Option<String> {
        o.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
    }

    let mut meta = PackageMeta {
        creator_name: s(obj, "creatorName").unwrap_or_default(),
        package_name: s(obj, "packageName").unwrap_or_default(),
        license_type: s(obj, "licenseType"),
        description: s(obj, "description"),
        instructions: s(obj, "instructions"),
        program_version: s(obj, "programVersion"),
        content_list: vec![],
        dependencies: vec![],
    };

    if let Some(list) = obj.get("contentList").and_then(|v| v.as_array()) {
        for entry in list {
            if let Some(p) = entry.as_str() {
                meta.content_list.push(p.to_string());
            }
        }
    }

    // dependencies is a recursive map keyed "Author.Package.Version". We only
    // capture the top-level keys here; recursive resolution happens later.
    if let Some(deps) = obj.get("dependencies").and_then(|v| v.as_object()) {
        for k in deps.keys() {
            meta.dependencies.push(k.clone());
        }
    }

    Ok(meta)
}

/// Classify a package based on the prefix distribution of its contentList.
/// Most packages are single-purpose so the dominant prefix wins; ties default to Mixed.
pub fn classify(content_list: &[String]) -> PackageType {
    if content_list.is_empty() {
        return PackageType::Unknown;
    }
    let mut scene = 0u32;
    let mut look = 0u32;
    let mut morph = 0u32;
    let mut texture = 0u32;
    let mut clothing = 0u32;
    let mut hair = 0u32;
    let mut plugin = 0u32;
    let mut asset = 0u32;
    let mut pose = 0u32;
    let mut sound = 0u32;
    let mut subscene = 0u32;

    for p in content_list {
        let p = p.replace('\\', "/");
        if p.starts_with("Saves/scene/") || p.starts_with("Saves/Scene/") {
            scene += 1;
        } else if p.starts_with("Custom/SubScene/") {
            subscene += 1;
        } else if p.starts_with("Saves/Person/appearance/")
            || p.starts_with("Custom/Atom/Person/AppearancePresets/")
        {
            look += 1;
        } else if p.starts_with("Custom/Atom/Person/Morphs/") {
            morph += 1;
        } else if p.starts_with("Custom/Atom/Person/Textures/") {
            texture += 1;
        } else if p.starts_with("Custom/Clothing/")
            || p.starts_with("Custom/Atom/Person/Clothing/")
        {
            clothing += 1;
        } else if p.starts_with("Custom/Hair/")
            || p.starts_with("Custom/Atom/Person/Hair/")
        {
            hair += 1;
        } else if p.starts_with("Custom/Scripts/") {
            plugin += 1;
        } else if p.starts_with("Custom/Assets/") {
            asset += 1;
        } else if p.starts_with("Custom/Atom/Person/Pose/")
            || p.starts_with("Saves/Person/pose/")
        {
            pose += 1;
        } else if p.starts_with("Custom/Sounds/") || p.starts_with("Saves/Sounds/") {
            sound += 1;
        }
    }

    // Scenes commonly bundle large piles of audio (Saves/Sounds/*) that outnumber
    // the single Saves/scene/*.json by 100x, so naive dominance routes them to
    // Sound. Suppress Sound from the contest when a scene file is present —
    // a package with both is virtually never a pure Sound pack.
    let effective_sound = if scene > 0 { 0 } else { sound };

    let counts = [
        (scene, PackageType::Scene),
        (look, PackageType::Look),
        (morph, PackageType::Morph),
        (texture, PackageType::Texture),
        (clothing, PackageType::Clothing),
        (hair, PackageType::Hair),
        (plugin, PackageType::Plugin),
        (asset, PackageType::Asset),
        (pose, PackageType::Pose),
        (effective_sound, PackageType::Sound),
        (subscene, PackageType::SubScene),
    ];

    let nonzero: Vec<(u32, PackageType)> = counts
        .iter()
        .copied()
        .filter(|&(c, _)| c > 0)
        .collect();
    match nonzero.len() {
        0 => PackageType::Unknown,
        1 => nonzero[0].1,
        _ => {
            let (max, ty) = nonzero
                .iter()
                .copied()
                .max_by_key(|&(c, _)| c)
                .unwrap();
            let second = nonzero
                .iter()
                .copied()
                .filter(|&(_, t)| t != ty)
                .map(|(c, _)| c)
                .max()
                .unwrap_or(0);
            // Dominant if 2x the runner-up, otherwise Mixed.
            if max >= second.saturating_mul(2) {
                ty
            } else {
                PackageType::Mixed
            }
        }
    }
}

/// What VaM itself would show as a preview, in priority order:
/// 1. top-level `Preview.jpg`/`.png` (some authors include one)
/// 2. first scene preview: sibling .jpg of `Saves/scene/.../*.json`
/// 3. first appearance preset preview: sibling .jpg of `*.vap` in appearance dirs
/// 4. first plugin preview: sibling .jpg of `Custom/Scripts/.../*.cslist`
/// 5. first clothing preview: sibling .jpg of `*.vam` in clothing dirs
/// 6. first hair preview: sibling .jpg of `*.vam` in hair dirs
/// 7. first pose preview: sibling .jpg of `*.vap` in pose dirs
/// 8. first .jpg/.png under `Saves/` or `Custom/SubScene/` (fallback)
///
/// Returns the *original-cased* zip entry path so the thumbnail extractor can
/// look it up directly. Returns `None` if nothing image-like is in the package.
pub fn pick_preview(content_list: &[String]) -> Option<String> {
    if content_list.is_empty() {
        return None;
    }
    let normalized: Vec<String> = content_list
        .iter()
        .map(|p| p.replace('\\', "/"))
        .collect();

    let images: Vec<&String> = normalized
        .iter()
        .filter(|p| is_image_path(p))
        .collect();
    if images.is_empty() {
        return None;
    }

    // 1. Top-level Preview.jpg / Preview.png (case-insensitive).
    for img in &images {
        let lower = img.to_lowercase();
        if lower == "preview.jpg" || lower == "preview.jpeg" || lower == "preview.png" {
            return Some((*img).clone());
        }
    }

    // Sibling-of-X searches in VaM-priority order. Each call walks `images`
    // once internally; the list of (prefix, ext) pairs below is the canonical
    // picker order.
    const PRIORITIES: &[(&str, &str)] = &[
        // Scenes
        ("Saves/scene/", ".json"),
        ("Saves/Scene/", ".json"),
        // Appearance presets
        ("Saves/Person/appearance/", ".vap"),
        ("Custom/Atom/Person/AppearancePresets/", ".vap"),
        // Plugins
        ("Custom/Scripts/", ".cslist"),
        // Clothing
        ("Custom/Clothing/", ".vam"),
        ("Custom/Atom/Person/Clothing/", ".vam"),
        // Hair
        ("Custom/Hair/", ".vam"),
        ("Custom/Atom/Person/Hair/", ".vam"),
        // Pose presets
        ("Custom/Atom/Person/Pose/", ".vap"),
        ("Saves/Person/pose/", ".vap"),
        // SubScene (.json bundle file, sibling .jpg)
        ("Custom/SubScene/", ".json"),
    ];
    for (prefix, ext) in PRIORITIES {
        if let Some(p) = sibling_of_first(&normalized, &images, prefix, ext) {
            return Some(p);
        }
    }

    // Broader fallback chain: any image, walking common preview-bearing paths
    // first, then anywhere. Excludes textures that are clearly normal/specular/
    // displacement maps (those make terrible thumbnails — gray-purple or B&W).
    let usable: Vec<&String> = images
        .iter()
        .copied()
        .filter(|p| !is_supplementary_texture(p))
        .collect();
    if !usable.is_empty() {
        let fallback_prefixes = [
            "Saves/",
            "Custom/SubScene/",
            "Custom/Atom/Person/",
            "Custom/Clothing/",
            "Custom/Hair/",
            "Custom/Scripts/",
            "Custom/Assets/",
            "Custom/",
            "", // truly anywhere
        ];
        for prefix in fallback_prefixes {
            if let Some(p) = usable
                .iter()
                .find(|p| p.starts_with(prefix))
                .map(|p| (*p).clone())
            {
                return Some(p);
            }
        }
    }
    // Last resort: even a normal/spec map is better than no thumbnail at all.
    images.first().map(|p| (*p).clone())
}

/// Filename heuristic: is this image a normal/specular/displacement/etc. map
/// rather than something a human would want as a preview? Those make weird
/// purple-gray thumbnails. Heuristic favors false negatives over false
/// positives — if uncertain, treat as usable.
fn is_supplementary_texture(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);

    // Capital-letter marker tag at the end, preceded by either an explicit
    // separator (_, -, space) or a CamelCase transition (lowercase letter →
    // uppercase tag). Catches "Face_N", "BodyS", "torsoN", "limbsN"; skips
    // "SCAN" (all caps before N) and "skin" (lowercase n, not capital).
    let ends_in_marker = |stem: &str, marker: &str| -> bool {
        let Some(before) = stem.strip_suffix(marker) else { return false };
        match before.chars().last() {
            None => false,
            Some(c) => c == '_' || c == '-' || c == ' ' || c.is_ascii_lowercase(),
        }
    };
    for marker in ["N", "S", "D", "G", "R", "AO"] {
        if ends_in_marker(stem, marker) {
            return true;
        }
    }

    let lower = stem.to_lowercase();
    for needle in [
        "normal", "specular", "displacement", "roughness", "metallic",
        "ambientocclusion", "ambient_occlusion", "_norm", "_spec", "_displ",
        "_rough", "_metal", "_gloss",
    ] {
        if lower.contains(needle) {
            return true;
        }
    }
    false
}

/// Find the first content entry whose path starts with `dir_prefix` and ends with
/// `item_ext`, then return a same-stem .jpg/.jpeg/.png sibling if one exists.
fn sibling_of_first(
    normalized: &[String],
    images: &[&String],
    dir_prefix: &str,
    item_ext: &str,
) -> Option<String> {
    let item_ext_lower = item_ext.to_lowercase();
    // Sort items so the choice is stable across runs (lexicographic).
    let mut items: Vec<&String> = normalized
        .iter()
        .filter(|p| p.starts_with(dir_prefix) && p.to_lowercase().ends_with(&item_ext_lower))
        .collect();
    items.sort();
    for item in items {
        let stem = &item[..item.len() - item_ext.len()];
        for ext in [".jpg", ".jpeg", ".png", ".JPG", ".JPEG", ".PNG"] {
            let candidate = format!("{stem}{ext}");
            if images.iter().any(|img| **img == candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Per-category counts of "previewable items" — what drives the emoji+count strip
/// in the tile UI. Categories excluded (morph/texture/asset/sound) because they're
/// internal components, not separately previewable in VaM.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct PreviewableCounts {
    pub scene: u32,
    pub look: u32,
    pub plugin: u32,
    pub clothing: u32,
    pub hair: u32,
    pub pose: u32,
    pub subscene: u32,
}

pub fn previewable_counts(content_list: &[String]) -> PreviewableCounts {
    if content_list.is_empty() {
        return PreviewableCounts::default();
    }
    let normalized: Vec<String> = content_list
        .iter()
        .map(|p| p.replace('\\', "/"))
        .collect();

    let count_ending = |prefix: &str, ext: &str| -> u32 {
        let ext_l = ext.to_lowercase();
        normalized
            .iter()
            .filter(|p| p.starts_with(prefix) && p.to_lowercase().ends_with(&ext_l))
            .count() as u32
    };

    // SubScenes are often listed in contentList as directories (e.g.
    // "Custom/SubScene/Amaimon/Lighting_Rigs") rather than individual .json files.
    // Count distinct "Author/Name" buckets under Custom/SubScene/ to approximate
    // how many subscene bundles the package contains.
    let subscene_count: u32 = {
        let mut buckets: std::collections::HashSet<String> = Default::default();
        for p in &normalized {
            if let Some(rest) = p.strip_prefix("Custom/SubScene/") {
                let bucket: String = rest
                    .split('/')
                    .take(2)
                    .collect::<Vec<_>>()
                    .join("/");
                if !bucket.is_empty() {
                    buckets.insert(bucket);
                }
            }
        }
        buckets.len() as u32
    };

    PreviewableCounts {
        scene: count_ending("Saves/scene/", ".json") + count_ending("Saves/Scene/", ".json"),
        look: count_ending("Saves/Person/appearance/", ".vap")
            + count_ending("Custom/Atom/Person/AppearancePresets/", ".vap"),
        plugin: count_ending("Custom/Scripts/", ".cslist"),
        clothing: count_ending("Custom/Clothing/", ".vam")
            + count_ending("Custom/Atom/Person/Clothing/", ".vam"),
        hair: count_ending("Custom/Hair/", ".vam")
            + count_ending("Custom/Atom/Person/Hair/", ".vam"),
        pose: count_ending("Custom/Atom/Person/Pose/", ".vap")
            + count_ending("Saves/Person/pose/", ".vap"),
        subscene: subscene_count,
    }
}

fn is_image_path(p: &str) -> bool {
    let l = p.to_lowercase();
    l.ends_with(".jpg") || l.ends_with(".jpeg") || l.ends_with(".png")
}
