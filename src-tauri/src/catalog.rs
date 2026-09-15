//! Built-in ThinkSystem V3/V4 model → machine-type catalog.

use std::collections::BTreeMap;

/// A ThinkSystem model and its Lenovo 4-character machine types.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub name: &'static str,
    pub machine_types: &'static [&'static str],
}

/// Curated V3/V4 rack/tower models commonly managed via XCC2/XCC3.
pub const MODELS: &[ModelEntry] = &[
    ModelEntry {
        name: "ThinkSystem SR630 V3",
        machine_types: &["7D72", "7D73", "7D74"],
    },
    ModelEntry {
        name: "ThinkSystem SR650 V3",
        machine_types: &["7D75", "7D76", "7D77"],
    },
    ModelEntry {
        name: "ThinkSystem SR645 V3",
        machine_types: &["7D9C", "7D9D"],
    },
    ModelEntry {
        name: "ThinkSystem SR665 V3",
        machine_types: &["7D9E", "7D9F"],
    },
    ModelEntry {
        name: "ThinkSystem SR675 V3",
        machine_types: &["7D9G", "7D9H"],
    },
    ModelEntry {
        name: "ThinkSystem ST650 V3",
        machine_types: &["7D7A", "7D7B"],
    },
    ModelEntry {
        name: "ThinkSystem SD650 V3",
        machine_types: &["7D7C", "7D7D"],
    },
    ModelEntry {
        name: "ThinkSystem SR630 V4",
        machine_types: &["7DG8", "7DG9", "7DGA"],
    },
    ModelEntry {
        name: "ThinkSystem SR650 V4",
        machine_types: &["7DGC", "7DGD", "7DGE"],
    },
    ModelEntry {
        name: "ThinkSystem SR645 V4",
        machine_types: &["7DGG", "7DGH"],
    },
    ModelEntry {
        name: "ThinkSystem SR665 V4",
        machine_types: &["7DGJ", "7DGK"],
    },
];

/// Display labels for interactive multi-select (name + MTs).
#[allow(dead_code)]
pub fn model_labels() -> Vec<String> {
    MODELS
        .iter()
        .map(|m| {
            format!(
                "{} ({})",
                m.name,
                m.machine_types.join(", ")
            )
        })
        .collect()
}

/// Resolve selected catalog indices plus optional extra MTs into a unique MT list.
pub fn machine_types_from_selection(
    selected_indices: &[usize],
    extra_mts: &[String],
) -> Vec<String> {
    let mut map = BTreeMap::new();
    for &idx in selected_indices {
        if let Some(entry) = MODELS.get(idx) {
            for mt in entry.machine_types {
                map.insert(mt.to_uppercase(), entry.name.to_string());
            }
        }
    }
    for mt in extra_mts {
        let cleaned = normalize_machine_type(mt);
        if cleaned.len() == 4 {
            map.entry(cleaned).or_insert_with(|| "Custom".to_string());
        }
    }
    map.into_keys().collect()
}

/// Normalize a machine type string to 4 uppercase alphanumeric chars when possible.
pub fn normalize_machine_type(raw: &str) -> String {
    let upper = raw.trim().to_uppercase();
    // SKU often looks like 7D76CTO1WW — take first 4 alphanumerics.
    upper
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(4)
        .collect()
}

/// Extract a 4-char machine type from Redfish identity fields.
pub fn extract_machine_type(sku: Option<&str>, model: Option<&str>, hostname: Option<&str>) -> Option<String> {
    if let Some(sku) = sku {
        let mt = normalize_machine_type(sku);
        if mt.len() == 4 {
            return Some(mt);
        }
    }
    // Try HostName patterns like XCC-7D76-1234567
    if let Some(hn) = hostname {
        for part in hn.split(|c: char| !c.is_ascii_alphanumeric()) {
            let mt = normalize_machine_type(part);
            if mt.len() == 4 && looks_like_mt(&mt) {
                return Some(mt);
            }
        }
    }
    // Last resort: scan model string for a known MT
    if let Some(model) = model {
        for entry in MODELS {
            for mt in entry.machine_types {
                if model.to_uppercase().contains(&mt.to_uppercase()) {
                    return Some(mt.to_uppercase());
                }
            }
            // Match by model name fragment (e.g. "SR650 V3")
            let short = entry
                .name
                .trim_start_matches("ThinkSystem ")
                .to_uppercase();
            if model.to_uppercase().contains(&short) {
                // Ambiguous across MTs — return first as hint only if single family
                if entry.machine_types.len() == 1 {
                    return Some(entry.machine_types[0].to_uppercase());
                }
            }
        }
    }
    None
}

fn looks_like_mt(s: &str) -> bool {
    s.len() == 4
        && s.chars().next().is_some_and(|c| c.is_ascii_digit())
        && s.chars().any(|c| c.is_ascii_alphabetic())
}

/// Map machine type back to a friendly model name when known.
pub fn model_name_for_mt(mt: &str) -> Option<&'static str> {
    let mt = mt.to_uppercase();
    MODELS
        .iter()
        .find(|e| e.machine_types.iter().any(|m| m.eq_ignore_ascii_case(&mt)))
        .map(|e| e.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_sku_to_mt() {
        assert_eq!(normalize_machine_type("7D76CTO1WW"), "7D76");
        assert_eq!(normalize_machine_type(" 7d75 "), "7D75");
    }

    #[test]
    fn extract_from_sku() {
        assert_eq!(
            extract_machine_type(Some("7D76CTO1WW"), None, None).as_deref(),
            Some("7D76")
        );
    }

    #[test]
    fn extract_from_hostname() {
        assert_eq!(
            extract_machine_type(None, None, Some("XCC-7D76-1234567890")).as_deref(),
            Some("7D76")
        );
    }

    #[test]
    fn selection_dedupes_mts() {
        let mts = machine_types_from_selection(&[1], &["7D76".into(), "abcd".into()]);
        assert!(mts.contains(&"7D75".to_string()));
        assert!(mts.contains(&"7D76".to_string()));
        assert!(mts.contains(&"7D77".to_string()));
        assert!(mts.contains(&"ABCD".to_string()));
    }
}
