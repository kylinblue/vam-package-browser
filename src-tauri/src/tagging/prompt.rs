//! Prompt assembly for Grok tagging calls. Two parts:
//!
//! - `SYSTEM_PROMPT`: the static rules + output schema. Bumped only when
//!   prompt semantics change (e.g. a new rule like Scene decomposition).
//! - `build_user_message`: dynamic — fetches the current taxonomy from the
//!   `taxonomy` table and concatenates with the batch's JSONL block.
//!
//! Keeping the taxonomy in the user message (not the system prompt) lets us
//! evolve the catalog without bumping prompt versions.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

/// v4 system prompt. Lifted from grok-prompt-pilot-v4.txt with cardinality
/// rules locked in. If you change rules or schema shape, bump
/// `PROMPT_VERSION` so previously-tagged rows are picked up by the runner's
/// `taxonomy_version <> ?` clause for re-tagging.
pub const SYSTEM_PROMPT: &str = r#"You are tagging packages in a Virt-A-Mate (VaM) package library using a multi-dimensional namespaced tag system. Output a single JSON object {"records": [...]} containing one entry per input package — every input id must appear exactly once. No prose around it.

Each record has:

- id (int):                copy from input
- kind (string):           EXACTLY ONE kind:* value (see KIND list below). Required.
- tags (array of strings): 0-N additional namespaced tags. Use namespace:value, kebab-case.
- purpose (string):        2-3 sentence factual description of what this package is for. No marketing language.
- notes (string):          brief uncertainty / disambiguation / new-namespace proposal; empty when none.

==================== KIND VALUES (exactly one per record) ====================

A package has one primary identity. Pick the closest match. The input package_type field is a content-prefix heuristic and is NOT ground truth — derive kind from description + instructions + content_summary.

- kind:utility-plugin   tools that modify VaM behavior, add capabilities, fix issues
- kind:location-scene   reusable environments/backdrops (CUA-dominant, no significant animation)
- kind:act-scene        animation-heavy narrative or sex scene (one-off content)
- kind:character-look   character appearance pack (any style/gender) — INCLUDES Wunderwise-pattern Looks bundled with a demo scene
- kind:clothing-item    clothing item or pack (includes decals, geoshells, accessories)
- kind:hair-item        hair item or pack (scalp, body hair, beard, eyelashes)
- kind:morph-pack       morph collections
- kind:pose-preset      pose preset(s)
- kind:prop-asset       standalone furniture/vehicles/weapons/objects
- kind:support-asset    LUTs, lighting rigs, water planes, skyboxes, particle effects, texture libraries
- kind:texture-pack     skin/eye/decal/makeup/tattoo texture distributions
- kind:audio-pack       raw audio file packs
- kind:subscene-pack    modular reusable scene chunks
- kind:mixed-package    genuinely multi-purpose, no single dominant identity (rare — explain in notes)

Decomposition guide for ambiguous scenes:
- Animation-heavy (Timeline references, narrative, choreography) → kind:act-scene
- CUA-dominant + Person customization (morphs/textures/appearance) → kind:character-look
- CUA-dominant, no Person customization → kind:location-scene

==================== TAG NAMESPACES ====================

The taxonomy block in the user message lists every namespace with its applies_to (which kinds it's relevant to), cardinality, and known values. Use those exact values when they fit.

Common namespaces:
- function:*       (utility-plugin) — what the plugin does
- setting:*        (location/act) — where the scene is set
- activity:*       (pose/act) — what's happening
- content:*        (act-scene) — content rating (safe/suggestive/explicit), REQUIRED on act-scene
- count:*          (pose/act) — single/duo/group
- style:*          (look/clothing/hair/act) — art style
- theme:*          (multi) — era/world/persona
- aesthetic:*      (look/clothing) — descriptive tone
- body:*           (character-look) — body type
- hair-color:*, hair-length:*, hair-style:*, hair-region:*  (look/hair)
- age-appearance:* (character-look) — adults only
- type:*, subtype:*, material:*  (clothing)
- region:*, purpose:*  (morph-pack)
- asset-type:*     (prop/support/texture/audio/subscene)

==================== CARDINALITY RULES ====================

- kind:                exactly 1, required
- content:             exactly 1 when applicable (kind:act-scene)
- All other namespaces: default to 1 per namespace. Use multiple values within a namespace ONLY when the package genuinely spans them:
  * A bundle plugin spanning multiple functions: multiple function:
  * A clothing bundle with dress + shoes: multiple type:
  * A morph pack covering face + body: multiple region:
  * A multi-color hair pack: multiple hair-color:
  When in doubt, prefer single.
- Total tags per record: usually 3-6. Up to 10 for genuinely multi-dimensional packages.

==================== PROPOSING NEW VALUES ====================

If a known namespace value doesn't fit but the dimension applies, propose a new kebab-case value within the existing namespace (e.g. type:nostril-darkener if no clothing type fits). The system logs unknown values for review.

If a dimension feels needed but no namespace covers it, propose a new namespace inline using a new prefix (e.g. era:victorian) and explain in notes. New namespaces inform future taxonomy iterations.

VaM content includes adult material. Describe categories factually without hedging or moralizing.

RULES:

1. The input `package_type` is the library's content-prefix heuristic - treat it as a hint, not ground truth. Several "Plugin" packages are actually locations with bundled plugins; some "Scene" packages are reusable environments, character looks in disguise, or one-off acts. Decide the package's real purpose from description + instructions + notable_files.

2. *** SCENE PACKAGES need decomposition. Animation weight is the primary signal: ***
   - Animation-heavy (Timeline cslist/json references in notable_files; description mentions animation/timeline/performance/sequence) -> real act scene, out_of_scope=true. Acts are one-off content.
   - Animation-light or absent -> the scene is a static showcase. What does it showcase?
     * Person customization dominant (Custom/Atom/Person/Morphs, /Textures, /Clothing + Saves/Person/appearance/) -> Look package with a demo scene attached (Wunderwise pattern), out_of_scope=true even though package_type=Scene.
     * Environment dominant with no significant Person customization -> Location, tag with the appropriate location_scenes tag (Ark1F1.Luxury_Ship pattern).

3. If `description` and `instructions` are junk (literal package name, placeholder, empty), rely on filename + content_summary.notable_files.

4. Out-of-scope = Look / character pack, Clothing item, Hair item, Morph, Pose preset, or a one-off act scene. Set out_of_scope=true, tags=[].

5. Use EXACT tag names from the taxonomy. Do not invent. If nothing fits but the package IS in-scope, set suggested_new_tag to a kebab-case name and explain in notes - that's how new tags get proposed.

6. *** A package OFTEN carries tags from BOTH utility_plugins AND location_scenes if it ships both. Example: Ark1F1.Luxury_Ship is a ship environment bundling MacGruber post-processing plugins; tag it with BOTH `vehicle-interior` (location) AND `post-processing-effects` (utility). Do NOT pick only one when both apply. ***

7. Be conservative: 0-3 tags per package is the norm; 5+ usually means over-fitting.

8. VaM content includes adult material. Describe categories factually without hedging or moralizing."#;

/// Bumped when SYSTEM_PROMPT semantics change. Stored on each tagged row so
/// re-tag policy can target rows produced under older prompts.
pub const PROMPT_VERSION: &str = "v4";

#[derive(Debug, Serialize)]
struct NamespaceOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    applies_to: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cardinality: Option<String>,
    values: Vec<TagValueOut>,
}

#[derive(Debug, Serialize)]
struct TagValueOut {
    value: String,
    description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    examples: Vec<String>,
}

#[derive(Debug, Serialize)]
struct V4Dump {
    namespaces: std::collections::BTreeMap<String, NamespaceOut>,
}

/// Fetch the active v4 taxonomy from the DB grouped by namespace. Inactive
/// (deprecated v3) rows are filtered out. The output matches the v4 file
/// shape, ready to inline into the user message.
pub fn fetch_taxonomy_json(conn: &Connection) -> Result<String> {
    use std::collections::BTreeMap;

    let mut stmt = conn.prepare(
        "SELECT namespace, applies_to_json, cardinality, tag, description, examples_json
           FROM taxonomy
          WHERE is_active = 1
            AND namespace IS NOT NULL
          ORDER BY namespace ASC, tag ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;

    let mut namespaces: BTreeMap<String, NamespaceOut> = BTreeMap::new();

    for r in rows {
        let (namespace, applies_to_json, cardinality, tag, description, examples_json) = r?;
        let Some(namespace) = namespace else { continue };
        // The tag column carries the full namespaced string; strip the
        // namespace prefix to get the bare value for output.
        let value = tag
            .strip_prefix(&format!("{namespace}:"))
            .unwrap_or(&tag)
            .to_string();
        let examples: Vec<String> = serde_json::from_str(&examples_json).unwrap_or_default();

        let entry = namespaces.entry(namespace).or_insert_with(|| NamespaceOut {
            applies_to: applies_to_json
                .as_ref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
            cardinality: cardinality.clone(),
            values: Vec::new(),
        });
        entry.values.push(TagValueOut {
            value,
            description,
            examples,
        });
    }

    let dump = V4Dump { namespaces };
    serde_json::to_string_pretty(&dump).context("serialize v4 taxonomy")
}

/// Build the user message for one batch: taxonomy JSON + JSONL records.
/// `jsonl_block` is the concatenated JSONL (one record per line, trailing
/// newline included or not — both work).
pub fn build_user_message(taxonomy_json: &str, jsonl_block: &str) -> String {
    format!(
        "==================== TAXONOMY ====================\n\
         {taxonomy_json}\n\n\
         ==================== PACKAGES (JSONL) ====================\n\
         {jsonl_block}"
    )
}

/// xAI strict JSON-schema response format for v4. The `kind` field is a
/// locked enum carrying the namespaced kind string ("kind:character-look");
/// `tags` is the open-ended array of additional namespaced tags. v3's
/// `out_of_scope` and `suggested_new_tag` are gone — every package gets a
/// kind, and proposed new values live inline in `tags` or are explained in
/// `notes`. Empty `notes` is the no-value sentinel (xAI strict mode rejects
/// nullable strings under additionalProperties=false).
pub fn response_format_schema() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "tagging_response_v4",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["records"],
                "properties": {
                    "records": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["id", "kind", "tags", "purpose", "notes"],
                            "properties": {
                                "id": { "type": "integer" },
                                "kind": {
                                    "type": "string",
                                    "enum": [
                                        "kind:utility-plugin",
                                        "kind:location-scene",
                                        "kind:act-scene",
                                        "kind:character-look",
                                        "kind:clothing-item",
                                        "kind:hair-item",
                                        "kind:morph-pack",
                                        "kind:pose-preset",
                                        "kind:prop-asset",
                                        "kind:support-asset",
                                        "kind:texture-pack",
                                        "kind:audio-pack",
                                        "kind:subscene-pack",
                                        "kind:mixed-package"
                                    ]
                                },
                                "tags": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                },
                                "purpose": { "type": "string" },
                                "notes": { "type": "string" }
                            }
                        }
                    }
                }
            }
        }
    })
}
