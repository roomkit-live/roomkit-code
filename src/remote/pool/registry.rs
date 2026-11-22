//! Redis server time bounds worker heartbeats and atomic cross-owner slot reservations.
use super::metrics::Metrics;
use redis::aio::ConnectionManager;

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::OnceCell;

type Result<T> = std::result::Result<T, String>;
const REGISTER: &str = r#"
local now = tonumber(redis.call('TIME')[1])
local name, sample = ARGV[1], ARGV[2]
local prior = redis.call('GET', KEYS[2])
if prior and cjson.decode(prior).instance ~= ARGV[3] and
   tonumber(redis.call('ZSCORE', KEYS[1], name) or 0) > now-10 then return 0 end
redis.call('SET', KEYS[2], sample, 'EX', 86400)
redis.call('ZADD', KEYS[1], now, name)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now-86400)
redis.call('EXPIRE', KEYS[1], 86400)
return 1"#;
const RETIRE: &str = r#"
local data = redis.call('GET', KEYS[2])
if data and cjson.decode(data).instance == ARGV[2] then
    redis.call('ZADD',KEYS[1],tonumber(redis.call('TIME')[1])-10,ARGV[1])
end
return 1"#;
const WORKERS: &str = r#"
local now = tonumber(redis.call('TIME')[1])
local rows = {}
for _,name in ipairs(redis.call('ZRANGEBYSCORE', KEYS[1], now-86400, '+inf')) do
    local data = redis.call('GET', ARGV[1]..':worker:'..name)
    if data then
        local w = cjson.decode(data)
        local leases = ARGV[1]..':slots:'..w.instance
        local used = redis.call('ZCOUNT',leases,'('..now,'+inf')
        local seen = tonumber(redis.call('ZSCORE',KEYS[1],name))
        table.insert(rows,{data,now-seen,used})
    end
end
return rows"#;
const CLAIM: &str = r#"
local now=tonumber(redis.call('TIME')[1])
local row=redis.call('GET', KEYS[2])
if not row then return 0 end
local w=cjson.decode(row)
if w.instance~=ARGV[2] or w.metrics.available_mib<tonumber(ARGV[4]) or
   tonumber(redis.call('ZSCORE',KEYS[1],ARGV[1]) or 0)<=now-10 then return 0 end
redis.call('ZREMRANGEBYSCORE',KEYS[3],'-inf',now)
if redis.call('ZCARD',KEYS[3])>=w.slots then return 0 end
redis.call('ZADD',KEYS[3],now+20,ARGV[3])
redis.call('EXPIRE',KEYS[3],30)
return 1"#;
const RENEW: &str = r#"
local now=tonumber(redis.call('TIME')[1])
if tonumber(redis.call('ZSCORE',KEYS[1],ARGV[1]) or 0)<=now then return 0 end
redis.call('ZADD',KEYS[1],now+20,ARGV[1]); redis.call('EXPIRE',KEYS[1],30); return 1"#;

/// The public part of an exposed local profile. Connection details stay on the worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkerProfile {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) model: String,
    pub(crate) tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Worker {
    pub name: String,
    pub instance: String,
    pub os: String,
    pub host_os: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub profiles: Vec<WorkerProfile>,
    pub arch: String,
    pub labels: BTreeMap<String, String>,
    pub slots: u16,
    pub metrics: Metrics,
}
impl Worker {
    pub(super) fn placement(&self, requested: Option<&str>) -> String {
        let profile = match requested {
            Some(id) => self.profiles.iter().find(|p| p.id == id),
            None => self.profiles.first(),
        };
        let mut text = self.name.clone();
        if let Some(p) = profile {
            text.push_str(&format!(" · profile {} · model {}", p.id, p.model));
        } else if let Some(model) = &self.model {
            text.push_str(&format!(" · model {model}"));
        }
        text
    }
}

pub(super) struct Row {
    pub worker: Worker,
    pub age: u64,
    pub used: u16,
}
impl Row {
    pub fn online(&self) -> bool {
        self.age < 10
    }
}
#[derive(Clone)]
pub(super) struct Registry {
    client: redis::Client,
    connection: Arc<OnceCell<ConnectionManager>>,
    prefix: String,
}
fn failed(_: redis::RedisError) -> String {
    "Cannot reach the executor pool in Redis".into()
}
impl Registry {
    pub fn new(group: &str, address: &str) -> crate::Result<Self> {
        let client = redis::Client::open(address).map_err(|_| "Invalid Redis pool address")?;
        Ok(Self {
            client,
            connection: Arc::default(),
            prefix: format!("roomkit:code:scheduler:{group}"),
        })
    }
    async fn query<T: redis::FromRedisValue>(&self, cmd: &mut redis::Cmd) -> Result<T> {
        let conn = self
            .connection
            .get_or_try_init(|| self.client.get_connection_manager())
            .await
            .map_err(failed)?;
        cmd.query_async(&mut conn.clone()).await.map_err(failed)
    }
    fn workers(&self) -> String {
        format!("{}:workers", self.prefix)
    }
    fn worker(&self, name: &str) -> String {
        format!("{}:worker:{name}", self.prefix)
    }
    fn slots(&self, id: &str) -> String {
        format!("{}:slots:{id}", self.prefix)
    }
    pub async fn publish(&self, w: &Worker) -> Result<bool> {
        let data = serde_json::to_string(w).map_err(|e| e.to_string())?;
        self.query(
            redis::cmd("EVAL")
                .arg(REGISTER)
                .arg(2)
                .arg(self.workers())
                .arg(self.worker(&w.name))
                .arg(&w.name)
                .arg(data)
                .arg(&w.instance),
        )
        .await
    }
    pub async fn retire(&self, name: &str, id: &str) {
        let _: Result<i64> = self
            .query(
                redis::cmd("EVAL")
                    .arg(RETIRE)
                    .arg(2)
                    .arg(self.workers())
                    .arg(self.worker(name))
                    .arg(name)
                    .arg(id),
            )
            .await;
    }
    pub async fn rows(&self) -> Result<Vec<Row>> {
        let rows: Vec<(String, u64, u16)> = self
            .query(
                redis::cmd("EVAL")
                    .arg(WORKERS)
                    .arg(1)
                    .arg(self.workers())
                    .arg(&self.prefix),
            )
            .await?;
        rows.into_iter()
            .map(|(s, age, used)| {
                Ok(Row {
                    worker: serde_json::from_str(&s)
                        .map_err(|_| "Invalid executor resource record".to_owned())?,
                    age,
                    used,
                })
            })
            .collect()
    }
    pub async fn claim(&self, w: &Worker, turn: &str, ram: u64) -> Result<bool> {
        self.query(
            redis::cmd("EVAL")
                .arg(CLAIM)
                .arg(3)
                .arg(self.workers())
                .arg(self.worker(&w.name))
                .arg(self.slots(&w.instance))
                .arg(&w.name)
                .arg(&w.instance)
                .arg(turn)
                .arg(ram),
        )
        .await
    }
    pub async fn renew(&self, id: &str, turn: &str) -> Result<bool> {
        self.query(
            redis::cmd("EVAL")
                .arg(RENEW)
                .arg(1)
                .arg(self.slots(id))
                .arg(turn),
        )
        .await
    }
    pub async fn release(&self, id: &str, turn: &str) {
        let _: Result<i64> = self
            .query(redis::cmd("ZREM").arg(self.slots(id)).arg(turn))
            .await;
    }
}
