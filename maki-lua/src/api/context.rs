//! Key-focus contexts: a closed set of semantic classes ("kinds") plus an
//! open set of concrete overlay instances ("identities") declared in one
//! seed table. Bindings reference contexts by name at `maki.keymap.set`
//! time; dispatch matches them against the App's active state, which is a
//! bitmask of kinds plus at most one identity.

/// Closed set of semantic key-focus classes. `General` is the empty
/// context: it matches every active state.
///
/// The discriminant is the bit index in [`ActiveContext::kinds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ContextKind {
    General = 0,
    Chat = 1,
    Streaming = 2,
    Picker = 3,
    Form = 4,
    Modal = 5,
}

impl ContextKind {
    /// All kinds in display order (General first, then specificity order).
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Chat,
        Self::Streaming,
        Self::Picker,
        Self::Form,
        Self::Modal,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Chat => "chat",
            Self::Streaming => "streaming",
            Self::Picker => "picker",
            Self::Form => "form",
            Self::Modal => "modal",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "general" => Some(Self::General),
            "chat" => Some(Self::Chat),
            "streaming" => Some(Self::Streaming),
            "picker" => Some(Self::Picker),
            "form" => Some(Self::Form),
            "modal" => Some(Self::Modal),
            _ => None,
        }
    }
}

/// Numeric id of a concrete overlay identity; the index into
/// [`IDENTITIES`].
pub type IdentityId = u16;

/// One row of the identity seed table: a named overlay instance under a
/// kind. `id` is the array index; `name` is the user-visible string used
/// in `maki.keymap.set` `opts.context` and in `maki.keymap.get` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub id: IdentityId,
    pub name: &'static str,
    pub kind: ContextKind,
}

/// Every known identity, in priority order. ids are array indices
/// (0..=15). Adding a UI component means one row here plus one arm in
/// `App::active_keybind_contexts`.
pub const IDENTITIES: &[Identity] = &[
    // Picker kind
    Identity {
        id: 0,
        name: "task_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 1,
        name: "model_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 2,
        name: "theme_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 3,
        name: "rewind_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 4,
        name: "mcp_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 5,
        name: "login_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 6,
        name: "file_picker",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 7,
        name: "search",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 8,
        name: "queue",
        kind: ContextKind::Picker,
    },
    Identity {
        id: 9,
        name: "commands",
        kind: ContextKind::Picker,
    },
    // Form kind
    Identity {
        id: 10,
        name: "plan_form",
        kind: ContextKind::Form,
    },
    Identity {
        id: 11,
        name: "permission",
        kind: ContextKind::Form,
    },
    // Modal kind
    Identity {
        id: 12,
        name: "help",
        kind: ContextKind::Modal,
    },
    Identity {
        id: 13,
        name: "usage",
        kind: ContextKind::Modal,
    },
    Identity {
        id: 14,
        name: "btw",
        kind: ContextKind::Modal,
    },
    Identity {
        id: 15,
        name: "float",
        kind: ContextKind::Modal,
    },
];

/// What a binding's context refers to: a kind, or one concrete identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextRef {
    Kind(ContextKind),
    Identity(IdentityId),
}

/// The App's key-focus state at dispatch time. `kinds` is a bitmask: bit
/// `i` is set iff `ContextKind` with discriminant `i` is active. At most
/// one identity is ever active.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveContext {
    pub identity: Option<IdentityId>,
    pub kinds: u8,
}

/// Resolve a user-supplied context name: a kind label or an identity name.
pub(crate) fn resolve(name: &str) -> Option<ContextRef> {
    if let Some(kind) = ContextKind::from_label(name) {
        return Some(ContextRef::Kind(kind));
    }
    IDENTITIES
        .iter()
        .find(|i| i.name == name)
        .map(|i| ContextRef::Identity(i.id))
}

/// Every resolvable context name, for error messages.
pub(crate) fn all_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = ContextKind::ALL.iter().map(|k| k.label()).collect();
    names.extend(IDENTITIES.iter().map(|i| i.name));
    names
}

pub fn identity_kind(id: IdentityId) -> ContextKind {
    IDENTITIES
        .iter()
        .find(|i| i.id == id)
        .expect("identity id must exist in the seed table")
        .kind
}

/// True iff every ref matches the active state: a kind matches when it is
/// `General` or its bit is set; an identity matches when it is the active
/// identity. The empty ref list (General) always matches.
pub fn applies(refs: &[ContextRef], active: &ActiveContext) -> bool {
    refs.iter().all(|r| match r {
        ContextRef::Kind(ContextKind::General) => true,
        ContextRef::Kind(k) => active.kinds & (1 << *k as u8) != 0,
        ContextRef::Identity(id) => active.identity == Some(*id),
    })
}

/// Specificity of a binding's context: 0 for empty (General), 1 when the
/// most specific ref is a kind, 2 when it is an identity.
pub fn tier(refs: &[ContextRef]) -> u8 {
    refs.iter().fold(0, |t, r| match r {
        ContextRef::Kind(_) => t.max(1),
        ContextRef::Identity(_) => t.max(2),
    })
}
