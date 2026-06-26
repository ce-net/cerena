//! Stable identifiers for every kind of content.
//!
//! Ids are short interned strings (e.g. `"spell.fireball"`, `"item.ember_staff"`).
//! They are the join key between live game state and hot-reloadable definitions:
//! state stores ids, the [`crate::registry::ContentRegistry`] resolves ids to the
//! *current* definition. Renaming an id is a breaking content change (old saved
//! state would dangle), so ids are append-only by convention.
//!
//! A newtype per kind prevents mixing an [`ItemId`] where a [`SpellId`] is expected.

use serde::{Deserialize, Serialize};

macro_rules! content_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

content_id!(SpellId, "A composable spell (a graph of effect ops).");
content_id!(ItemId, "An equippable/consumable/craftable item.");
content_id!(AbilityId, "An equipped action: wraps a spell with binding + cooldown.");
content_id!(TechNodeId, "A node in the tech tree.");
content_id!(StatusId, "A status effect (buff/debuff/DoT/field).");
content_id!(MovementModeId, "A movement/parkour mode (dash, wall-run, grapple...).");
content_id!(MaterialId, "A procedural material (drives texture synthesis).");
content_id!(ShaderId, "A WGSL shader program (hot-recompilable).");
content_id!(MobId, "A non-player creature / summon archetype.");
content_id!(MissionId, "A procedural objective.");
content_id!(ElementId, "A magic element (fire, frost, void, life, ...). Data-driven.");
