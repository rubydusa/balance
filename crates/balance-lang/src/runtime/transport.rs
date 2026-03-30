use super::service::ServiceRuntime;
use super::substrate::SubstrateRegistry;
use super::event::EventBus;
use super::value::Value;

/// Transport layer abstraction for dispatching interactions to services.
///
/// Implementations route method calls to the appropriate service host,
/// whether in-process or remote (over the network).
pub trait Transport {
    /// Dispatch a method call to a service.
    ///
    /// Returns the result value or an error message.
    fn dispatch(
        &mut self,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String>;
}

/// In-process transport that dispatches directly to local service hosts
/// and substrate instances. This is the default transport used when all
/// services are running in the same process.
pub struct InProcessTransport {
    builtin_runtime: ServiceRuntime,
    substrate_registry: SubstrateRegistry,
}

impl InProcessTransport {
    pub fn new(builtin_runtime: ServiceRuntime, substrate_registry: SubstrateRegistry) -> Self {
        Self {
            builtin_runtime,
            substrate_registry,
        }
    }

    pub fn builtin_runtime(&self) -> &ServiceRuntime {
        &self.builtin_runtime
    }

    pub fn builtin_runtime_mut(&mut self) -> &mut ServiceRuntime {
        &mut self.builtin_runtime
    }

    pub fn substrate_registry(&self) -> &SubstrateRegistry {
        &self.substrate_registry
    }

    pub fn substrate_registry_mut(&mut self) -> &mut SubstrateRegistry {
        &mut self.substrate_registry
    }

    /// Dispatch to a substrate instance.
    pub fn dispatch_substrate(
        &mut self,
        substrate_name: &str,
        method: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        self.substrate_registry
            .execute_op(substrate_name, method, args, event_bus)
    }

    /// Dispatch to a built-in service.
    pub fn dispatch_builtin(
        &mut self,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        self.builtin_runtime.dispatch(service_id, method, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::builtins::StdoutService;

    #[test]
    fn test_in_process_builtin_dispatch() {
        let mut runtime = ServiceRuntime::new();
        runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));

        let substrate_registry = SubstrateRegistry::new();
        let mut transport = InProcessTransport::new(runtime, substrate_registry);

        let result = transport.dispatch_builtin("stdout/default", "writeln", vec![Value::String("hello".into())]);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert!(val.is_ack(), "expected Ack, got: {val}");
        assert!(val.ack_key().unwrap().starts_with("stdout_op_"));
    }
}
