use std::collections::{HashMap, HashSet};

use crate::runtime::value::Value;

pub struct Environment {
    scopes: Vec<HashMap<String, Value>>,
    mutable_vars: Vec<HashSet<String>>,
}

impl Environment {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            mutable_vars: vec![HashSet::new()],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.mutable_vars.push(HashSet::new());
    }

    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
            self.mutable_vars.pop();
        }
    }

    pub fn define(&mut self, name: String, value: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, value);
        }
    }

    pub fn define_mutable(&mut self, name: String, value: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.clone(), value);
        }
        if let Some(mutables) = self.mutable_vars.last_mut() {
            mutables.insert(name);
        }
    }

    pub fn is_mutable(&self, name: &str) -> bool {
        for mutables in self.mutable_vars.iter().rev() {
            if mutables.contains(name) {
                return true;
            }
        }
        false
    }

    pub fn lookup(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(val) = scope.get(name) {
                return Some(val);
            }
        }
        None
    }

    pub fn set(&mut self, name: &str, value: Value) -> bool {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), value);
                return true;
            }
        }
        false
    }

    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    pub fn snapshot(&self) -> Vec<(String, Value)> {
        let mut all = Vec::new();
        for scope in &self.scopes {
            for (k, v) in scope {
                all.push((k.clone(), v.clone()));
            }
        }
        all
    }
}
