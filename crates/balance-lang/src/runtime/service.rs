use std::collections::HashMap;

use super::interaction::InteractionKind;
use super::value::Value;

pub trait ServiceHost {
    fn handle(&mut self, method: &str, args: Vec<Value>) -> Result<Value, String>;

    fn method_kind(&self, _method: &str) -> InteractionKind {
        InteractionKind::Pure
    }
}

pub struct ServiceRuntime {
    services: HashMap<String, Box<dyn ServiceHost>>,
}

impl ServiceRuntime {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
        }
    }

    pub fn register(&mut self, service_id: String, host: Box<dyn ServiceHost>) {
        self.services.insert(service_id, host);
    }

    pub fn dispatch(
        &mut self,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match self.services.get_mut(service_id) {
            Some(host) => host.handle(method, args),
            None => Err(format!("no service host for '{service_id}'")),
        }
    }

    pub fn method_kind(&self, service_id: &str, method: &str) -> InteractionKind {
        match self.services.get(service_id) {
            Some(host) => host.method_kind(method),
            None => InteractionKind::Pure,
        }
    }
}
