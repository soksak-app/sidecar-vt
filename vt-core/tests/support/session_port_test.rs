use super::{DaemonEvent, SessionPort, ShellRequest};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct FakeSessionPort {
    pub calls: Arc<tokio::sync::Mutex<CallTracker>>,
}

pub struct CallTracker {
    pub opens: Vec<String>,
    pub writes: Vec<(String, Vec<u8>)>,
    pub resizes: Vec<(String, u16, u16)>,
    pub detaches: Vec<String>,
    pub closes: Vec<String>,
}

impl FakeSessionPort {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(tokio::sync::Mutex::new(CallTracker {
                opens: Vec::new(),
                writes: Vec::new(),
                resizes: Vec::new(),
                detaches: Vec::new(),
                closes: Vec::new(),
            })),
        }
    }
}

#[async_trait]
impl SessionPort for FakeSessionPort {
    async fn open(&self, request: &ShellRequest, _cols: u16, _rows: u16) -> Result<String, String> {
        let mut calls = self.calls.lock().await;
        let session_id = format!("session-{}", calls.opens.len());
        calls.opens.push(request.shell.clone());
        Ok(session_id)
    }

    async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        let mut calls = self.calls.lock().await;
        calls.writes.push((session_id.to_string(), data.to_vec()));
        Ok(())
    }

    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let mut calls = self.calls.lock().await;
        calls.resizes.push((session_id.to_string(), cols, rows));
        Ok(())
    }

    async fn detach(&self, session_id: &str) -> Result<(), String> {
        let mut calls = self.calls.lock().await;
        calls.detaches.push(session_id.to_string());
        Ok(())
    }

    async fn close(&self, session_id: &str) -> Result<(), String> {
        let mut calls = self.calls.lock().await;
        calls.closes.push(session_id.to_string());
        Ok(())
    }

    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent> {
        let (_tx, rx) = mpsc::channel(10);
        rx
    }
}
