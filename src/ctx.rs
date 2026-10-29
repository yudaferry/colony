//! Clock and environment abstractions, and the `Ctx` that command functions take.

use crate::backend::BackendSet;
use crate::state::store::StateDir;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

pub struct Ctx<'a> {
    pub state: &'a StateDir,
    pub backends: &'a dyn BackendSet,
    pub env: &'a dyn Env,
    pub clock: &'a dyn Clock,
}

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

pub trait Env {
    fn var(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

pub struct MapEnv(pub HashMap<String, String>);

impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}
