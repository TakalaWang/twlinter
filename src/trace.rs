use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::{json, Value};
use tracing::field::Field;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::field::Visit;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Layer};

static MCP_LOG_TX: OnceLock<Mutex<Option<mpsc::Sender<McpLogMessage>>>> = OnceLock::new();

#[derive(Clone, Debug, Serialize)]
pub struct McpLogMessage {
    pub level: &'static str,
    pub logger: &'static str,
    pub data: Value,
}

pub fn init(default_level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(McpLogLayer)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing::subscriber::set_global_default(subscriber);
}

pub fn set_mcp_log_sender(tx: Option<mpsc::Sender<McpLogMessage>>) {
    *MCP_LOG_TX.get_or_init(|| Mutex::new(None)).lock().unwrap() = tx;
}

struct McpLogLayer;

impl<S> Layer<S> for McpLogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => "error",
            Level::WARN => "warning",
            Level::INFO => "info",
            _ => return,
        };
        let Some(tx) = MCP_LOG_TX
            .get()
            .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
        else {
            return;
        };

        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        let data = json!({
            "message": visitor.message.unwrap_or_else(|| event.metadata().target().to_string()),
            "target": event.metadata().target(),
            "fields": visitor.fields,
        });
        let _ = tx.send(McpLogMessage {
            level,
            logger: "zhtw-mcp",
            data,
        });
    }
}

#[derive(Default)]
struct EventVisitor {
    message: Option<String>,
    fields: BTreeMap<String, Value>,
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let value = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(value.to_string()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().to_string(), json!(value));
    }
}
