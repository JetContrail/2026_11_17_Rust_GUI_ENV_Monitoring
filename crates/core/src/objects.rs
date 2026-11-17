//! State of up to 5,000 monitoring-screen objects with change tracking.
//!
//! Every update bumps a global version; the GUI redraws only objects whose
//! version is newer than the last frame (`changed_since`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::door::DoorIndication;
use crate::env::Level;
use crate::model::ObjectId;

pub const MAX_OBJECTS: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Door,
    CardReader,
    AnalogSensor,
    DigitalInput,
    DigitalOutput,
    KeyCabinet,
    PduOutlet,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObjectValue {
    Unknown,
    /// The device does not respond.
    Offline,
    Door(DoorIndication),
    Analog {
        value: f32,
        level: Level,
    },
    Digital {
        on: bool,
    },
    KeyCabinet {
        keys_out: u16,
    },
    Outlet {
        on: bool,
        watts: f32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectEntry {
    pub id: ObjectId,
    pub kind: ObjectKind,
    pub name: String,
    pub value: ObjectValue,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObjectError {
    #[error("object limit of {MAX_OBJECTS} reached")]
    Full,
    #[error("object {0} already registered")]
    Duplicate(ObjectId),
    #[error("unknown object {0}")]
    Unknown(ObjectId),
}

#[derive(Debug, Default, Clone)]
pub struct ObjectRegistry {
    entries: Vec<ObjectEntry>,
    index: HashMap<ObjectId, usize>,
    version: u64,
}

impl ObjectRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        id: ObjectId,
        kind: ObjectKind,
        name: impl Into<String>,
    ) -> Result<(), ObjectError> {
        if self.index.contains_key(&id) {
            return Err(ObjectError::Duplicate(id));
        }
        if self.entries.len() >= MAX_OBJECTS {
            return Err(ObjectError::Full);
        }
        self.version += 1;
        self.index.insert(id, self.entries.len());
        self.entries.push(ObjectEntry {
            id,
            kind,
            name: name.into(),
            value: ObjectValue::Unknown,
            version: self.version,
        });
        Ok(())
    }

    /// Sets the value; returns `false` (and keeps the version) if it did not change.
    pub fn update(&mut self, id: ObjectId, value: ObjectValue) -> Result<bool, ObjectError> {
        let &i = self.index.get(&id).ok_or(ObjectError::Unknown(id))?;
        let entry = &mut self.entries[i];
        if entry.value == value {
            return Ok(false);
        }
        self.version += 1;
        entry.value = value;
        entry.version = self.version;
        Ok(true)
    }

    pub fn get(&self, id: ObjectId) -> Option<&ObjectEntry> {
        self.index.get(&id).map(|&i| &self.entries[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &ObjectEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current version; pass it to [`Self::changed_since`] on the next frame.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn changed_since(&self, version: u64) -> impl Iterator<Item = &ObjectEntry> {
        self.entries.iter().filter(move |e| e.version > version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_changes_and_enforces_limit() {
        let mut r = ObjectRegistry::new();
        for i in 0..MAX_OBJECTS as u32 {
            r.register(ObjectId(i), ObjectKind::DigitalInput, format!("di{i}"))
                .unwrap();
        }
        assert_eq!(
            r.register(ObjectId(99_999), ObjectKind::Door, "x"),
            Err(ObjectError::Full)
        );
        assert_eq!(
            r.register(ObjectId(0), ObjectKind::Door, "x"),
            Err(ObjectError::Duplicate(ObjectId(0)))
        );

        let v = r.version();
        assert!(
            r.update(ObjectId(3), ObjectValue::Digital { on: true })
                .unwrap()
        );
        assert!(
            !r.update(ObjectId(3), ObjectValue::Digital { on: true })
                .unwrap()
        );
        let changed: Vec<_> = r.changed_since(v).map(|e| e.id).collect();
        assert_eq!(changed, vec![ObjectId(3)]);
        assert_eq!(
            r.update(ObjectId(99_999), ObjectValue::Offline),
            Err(ObjectError::Unknown(ObjectId(99_999)))
        );
    }
}
