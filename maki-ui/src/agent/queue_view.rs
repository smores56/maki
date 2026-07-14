//! UI-side adapters for [`maki_agent`] queue items: themed panel rows and
//! the conversion from an input [`Submission`]. These live here because they
//! touch [`crate::theme`] and [`crate::components::queue_panel::QueueEntry`].

use std::borrow::Cow;

use maki_agent::{QueueItem, QueuedMessage};

use crate::components::input::Submission;
use crate::components::queue_panel::QueueEntry;
use crate::theme;

const COMPACT_LABEL: &str = "/compact";

pub fn queued_message(sub: Submission) -> QueuedMessage {
    QueuedMessage {
        text: sub.text,
        images: sub.images,
    }
}

pub fn queue_entry(item: &QueueItem) -> QueueEntry<'static> {
    match item {
        QueueItem::Message { text, .. } => QueueEntry {
            text: Cow::Owned(text.clone()),
            color: theme::current().foreground,
        },
        QueueItem::Compact { .. } => QueueEntry {
            text: Cow::Borrowed(COMPACT_LABEL),
            color: theme::current()
                .queue
                .fg
                .unwrap_or(theme::current().foreground),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_agent::{AgentInput, AgentMode, ImageSource};

    fn msg(displayed: bool) -> QueueItem {
        QueueItem::Message {
            text: "t".into(),
            image_count: 0,
            input: AgentInput {
                message: String::new(),
                mode: AgentMode::Build,
                images: Vec::<ImageSource>::new(),
                preamble: Vec::new(),
                thinking: Default::default(),
                fast: false,
                workflow: false,
                prompt: None,
            },
            run_id: 0,
            displayed,
        }
    }

    #[test]
    fn submission_converts_to_queued_message() {
        let sub = Submission {
            text: "hi".into(),
            images: vec![],
        };
        let msg = queued_message(sub);
        assert_eq!(msg.text, "hi");
        assert!(msg.images.is_empty());
    }

    #[test]
    fn compact_entry_uses_compact_label() {
        let item = QueueItem::Compact { run_id: 0 };
        let entry = queue_entry(&item);
        assert_eq!(entry.text, COMPACT_LABEL);
    }

    #[test]
    fn message_entry_uses_text() {
        let item = msg(false);
        let entry = queue_entry(&item);
        assert_eq!(entry.text, "t");
    }
}
