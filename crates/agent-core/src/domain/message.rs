use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Typed content blocks. Provider-specific continuation data (reasoning items, thought
/// signatures, etc.) is preserved verbatim in `ProviderOpaque` so it can be replayed to the
/// same provider and never leaked to a different one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Reference to an imported attachment; expanded to labeled text when compiling context.
    Attachment {
        id: String,
        name: String,
        mime: String,
        #[ts(type = "number")]
        char_count: i64,
    },
    ToolCall {
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        #[ts(type = "unknown")]
        content: serde_json::Value,
        is_error: bool,
    },
    ProviderOpaque {
        provider: String,
        #[ts(type = "unknown")]
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<ContentBlock>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            blocks: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}
