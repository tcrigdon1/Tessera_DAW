//! Project file versions and upgrading older files.
//!
//! Every saved project carries a `version`. Files written before versions
//! existed count as version 0. [`upgrade`] brings the JSON of an older file
//! up to [`PROJECT_VERSION`] one step at a time before it is deserialised,
//! so a change to the file format only needs a new step here.

use serde_json::Value;

/// The project file version this build writes.
pub const PROJECT_VERSION: u32 = 1;

/// What [`upgrade`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upgrade {
    /// Already current.
    Current,
    /// Upgraded from an older version.
    Upgraded { from: u32 },
    /// Written by a newer Tessera: loaded as far as this build understands it.
    Newer { version: u32 },
}

/// The version of a project's JSON (0 when it has none).
pub fn version_of(json: &Value) -> u32 {
    json.get("version").and_then(Value::as_u64).unwrap_or(0) as u32
}

/// Upgrades project JSON in place to [`PROJECT_VERSION`].
pub fn upgrade(json: &mut Value) -> Result<Upgrade, String> {
    let Some(obj) = json.as_object_mut() else {
        return Err("not a Tessera project (expected a JSON object)".into());
    };
    if !obj.contains_key("tracks") {
        return Err("not a Tessera project (no tracks)".into());
    }
    let from = version_of(json);
    if from > PROJECT_VERSION {
        return Ok(Upgrade::Newer { version: from });
    }
    let mut v = from;
    while v < PROJECT_VERSION {
        match v {
            0 => v0_to_v1(json),
            _ => unreachable!("missing upgrade step from version {v}"),
        }
        v += 1;
    }
    if let Some(obj) = json.as_object_mut() {
        obj.insert("version".into(), Value::from(PROJECT_VERSION));
    }
    Ok(if from == PROJECT_VERSION {
        Upgrade::Current
    } else {
        Upgrade::Upgraded { from }
    })
}

/// Version 0 files predate return tracks, the master chain and count-in on
/// some builds; fill in what serde defaults would get wrong: a missing
/// `returns` list must keep every track's sends empty, and very early files
/// stored the time signature denominator as `beat_unit: 0`.
fn v0_to_v1(json: &mut Value) {
    let Some(obj) = json.as_object_mut() else { return };
    if obj.get("beat_unit").and_then(Value::as_u64).unwrap_or(0) == 0 {
        obj.insert("beat_unit".into(), Value::from(4));
    }
    let returns = obj
        .get("returns")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    if let Some(tracks) = obj.get_mut("tracks").and_then(Value::as_array_mut) {
        for t in tracks {
            if let Some(t) = t.as_object_mut() {
                if let Some(sends) = t.get_mut("sends").and_then(Value::as_array_mut) {
                    sends.truncate(returns);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    #[test]
    fn current_projects_are_untouched() {
        let p = crate::demo::demo_project();
        let mut json: Value = serde_json::from_str(&p.save_json().unwrap()).unwrap();
        let before = json.clone();
        assert_eq!(upgrade(&mut json).unwrap(), Upgrade::Current);
        assert_eq!(json, before);
    }

    #[test]
    fn unversioned_files_upgrade_and_load() {
        let p = crate::demo::demo_project();
        let mut json: Value = serde_json::from_str(&p.save_json().unwrap()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("version");
        obj.insert("beat_unit".into(), Value::from(0));
        assert_eq!(upgrade(&mut json).unwrap(), Upgrade::Upgraded { from: 0 });
        assert_eq!(version_of(&json), PROJECT_VERSION);
        let loaded: Project = serde_json::from_value(json).unwrap();
        assert_eq!(loaded.beat_unit, 4);
        assert_eq!(loaded.tracks.len(), p.tracks.len());
    }

    #[test]
    fn newer_files_are_flagged() {
        let mut json = serde_json::json!({"version": PROJECT_VERSION + 3, "tracks": []});
        assert_eq!(
            upgrade(&mut json).unwrap(),
            Upgrade::Newer {
                version: PROJECT_VERSION + 3
            }
        );
    }

    #[test]
    fn rejects_other_json() {
        assert!(upgrade(&mut serde_json::json!([1, 2])).is_err());
        assert!(upgrade(&mut serde_json::json!({"name": "x"})).is_err());
    }
}
