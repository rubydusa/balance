use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::runtime::interaction::InteractionKind;
use crate::runtime::service::ServiceHost;
use crate::runtime::value::Value;

static STDOUT_OP_COUNTER: AtomicU64 = AtomicU64::new(1);
static SLOW_OP_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct StdoutService;

impl StdoutService {
    pub fn new() -> Self {
        Self
    }
}

impl ServiceHost for StdoutService {
    fn handle(&mut self, method: &str, args: Vec<Value>) -> Result<Value, String> {
        let op_id = STDOUT_OP_COUNTER.fetch_add(1, Ordering::Relaxed);
        match method {
            "write" => {
                for arg in &args {
                    print!("{arg}");
                }
                Ok(Value::ack(format!("stdout_op_{op_id}")))
            }
            "writeln" => {
                for arg in &args {
                    print!("{arg}");
                }
                println!();
                Ok(Value::ack(format!("stdout_op_{op_id}")))
            }
            _ => Err(format!("Stdout has no method '{method}'")),
        }
    }

    fn method_kind(&self, method: &str) -> InteractionKind {
        match method {
            "write" | "writeln" => InteractionKind::Command,
            _ => InteractionKind::Pure,
        }
    }
}

/// A service that sleeps for a configurable duration before returning.
/// Used for testing timeout enforcement.
pub struct SlowService {
    delay: Duration,
}

impl SlowService {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

impl ServiceHost for SlowService {
    fn handle(&mut self, method: &str, _args: Vec<Value>) -> Result<Value, String> {
        let op_id = SLOW_OP_COUNTER.fetch_add(1, Ordering::Relaxed);
        match method {
            "slow_op" => {
                // Block the thread — on a single-threaded runtime this will
                // exceed the tokio::time::timeout wrapping execute_dispatch.
                std::thread::sleep(self.delay);
                Ok(Value::ack(format!("slow_op_{op_id}")))
            }
            _ => Err(format!("SlowService has no method '{method}'")),
        }
    }

    fn method_kind(&self, method: &str) -> InteractionKind {
        match method {
            "slow_op" => InteractionKind::Command,
            _ => InteractionKind::Pure,
        }
    }
}
