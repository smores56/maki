use maki_agent::ToolOutput;
use maki_providers::Message;
use maki_providers::TokenUsage;

pub type StorageWriter = maki_storage::writer::StorageWriter<Message, TokenUsage, ToolOutput>;
