//! ACP session management for susi-gemi
//!
//! Handles session lifecycle including creation, loading, and management
//! of conversation state.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

/// Session state for ACP conversations
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub workspace: PathBuf,
    pub mode: String,
    pub created_at: std::time::SystemTime,
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Clone)]
pub struct SessionMessage {
    pub role: String, // "user", "assistant", "system"
    pub content: String,
    pub timestamp: std::time::SystemTime,
}

/// Session manager for handling multiple ACP sessions
#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    workspace: PathBuf,
}

impl SessionManager {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            workspace,
        }
    }

    /// Create a new session
    pub fn create_session(&self, mode: Option<String>) -> String {
        let session_id = Uuid::new_v4().to_string();
        let session = Session {
            id: session_id.clone(),
            workspace: self.workspace.clone(),
            mode: mode.unwrap_or_else(|| "edit".to_string()),
            created_at: std::time::SystemTime::now(),
            messages: vec![],
        };

        self.sessions.write().insert(session_id.clone(), session);
        session_id
    }

    /// Load an existing session by ID
    pub fn load_session(&self, session_id: &str) -> Option<Session> {
        self.sessions.read().get(session_id).cloned()
    }

    /// Add a message to a session
    pub fn add_message(&self, session_id: &str, role: String, content: String) {
        if let Some(session) = self.sessions.write().get_mut(session_id) {
            session.messages.push(SessionMessage {
                role,
                content,
                timestamp: std::time::SystemTime::now(),
            });
        }
    }

    /// Set session mode
    pub fn set_mode(&self, session_id: &str, mode: String) {
        if let Some(session) = self.sessions.write().get_mut(session_id) {
            session.mode = mode;
        }
    }
}
