use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use crate::error::{SdkError, SdkResult};
use crate::mcp::server::SdkMcpServer;
use crate::options::SessionOptions;
use crate::protocol::messages::Message;
use crate::query::Query;
use crate::transport::SubprocessTransport;
use crate::{binary, mcp};
/// A v2-style interactive session over a CodeBuddy CLI subprocess.
pub struct Session {
    transport: Arc<SubprocessTransport>,
    query: Arc<Query>,
    messages_rx: Mutex<Option<mpsc::UnboundedReceiver<SdkResult<Message>>>>,
    options: SessionOptions,
    closed: Arc<std::sync::atomic::AtomicBool>,
}
impl Session {
    pub fn new(options: SessionOptions) -> SdkResult<Self> {
        let cli_path = match &options.codebuddy_code_path {
            Some(p) => p.clone(),
            None => binary::resolve_cli_path()?,
        };
        // Build the SDK MCP server map from options.
        let mut mcp_map: HashMap<String, Arc<SdkMcpServer>> = HashMap::new();
        for entry in &options.mcp_servers {
            let mut server = SdkMcpServer::new(&entry.name);
            for tool in &entry.tools {
                server = server.with_tool(tool.clone());
            }
            mcp_map.insert(entry.name.clone(), Arc::new(server));
        }
        let (transport, _done_rx) = SubprocessTransport::spawn(&options, cli_path)?;
        let transport = Arc::new(transport);
        let query = Arc::new(Query::new(transport.clone(), options.clone(), mcp_map));
        Ok(Self {
            transport,
            query,
            messages_rx: Mutex::new(None),
            options,
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    pub async fn connect(&self) -> SdkResult<()> {
        let rx = self.query.start().await?;
        *self.messages_rx.lock().await = Some(rx);
        // initialize with has_prompt=true (a prompt will follow)
        let _ = self.query.initialize(true).await?;
        Ok(())
    }
    pub async fn send(&self, content: Value) -> SdkResult<()> {
        self.query.send_user_message(content).await
    }
    pub async fn stream(&self) -> SdkResult<Option<Message>> {
        let mut guard = self.messages_rx.lock().await;
        if let Some(rx) = guard.as_mut() {
            match rx.recv().await {
                Some(Ok(msg)) => Ok(Some(msg)),
                Some(Err(e)) => Err(e),
                None => Ok(None),
            }
        } else {
            Err(SdkError::Protocol("stream() called before connect()".into()))
        }
    }
    pub async fn interrupt(&self) -> SdkResult<()> {
        self.query.interrupt().await
    }
    pub async fn session_id(&self) -> Option<String> {
        self.query.session_id().await
    }
    pub async fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Reap offloaded `tools/call` handler tasks (never-resolving capture
        // handlers) before closing the transport so they don't leak.
        self.query.shutdown();
        self.transport.close().await;
    }
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            self.query.shutdown();
            let transport = self.transport.clone();
            let closed = self.closed.clone();
            tokio::spawn(async move {
                transport.close().await;
                closed.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        }
    }
}
