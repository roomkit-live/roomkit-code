//! Redis server time bounds worker heartbeats and atomic cross-owner slot reservations.
use super::metrics::Metrics;
use redis::aio::ConnectionManager;

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::OnceCell;

type Result<T> = std::result::Result<T, String>;
// A record that does not read stops none of these scripts: `pcall` takes
// its decoding error, and it is replaced, skipped or listed as it is.
const REGISTER: &str = r#"
local now = tonumber(redis.call('TIME')[1])
local name, sample = ARGV[1], ARGV[2]
local prior = redis.call('GET', KEYS[2])
local read, w = pcall(cjson.decode, prior or 'null')
if read and type(w) == 'table' and w.instance ~= ARGV[3] and
   tonumber(redis.call('ZSCORE', KEYS[1], name) or 0) > now-10 then return 0 end
redis.call('SET', KEYS[2], sample, 'EX', 86400)
redis.call('ZADD', KEYS[1], now, name)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now-86400)
redis.call('EXPIRE', KEYS[1], 86400)
return 1"#;
const RETIRE: &str = r#"
local data = redis.call('GET', KEYS[2])
local read, w = pcall(cjson.decode, data or 'null')
if read and type(w) == 'table' and w.instance == ARGV[2] then
    redis.call('ZADD',KEYS[1],tonumber(redis.call('TIME')[1])-10,ARGV[1])
end
return 1"#;
const WORKERS: &str = r#"
local now = tonumber(redis.call('TIME')[1])
local rows = {}
for _,name in ipairs(redis.call('ZRANGEBYSCORE', KEYS[1], now-86400, '+inf')) do
    local data = redis.call('GET', ARGV[1]..':worker:'..name)
    if data then
        local read, w = pcall(cjson.decode, data)
        local used = 0
        if read and type(w) == 'table' and type(w.instance) == 'string' then
            used = redis.call('ZCOUNT',ARGV[1]..':slots:'..w.instance,'('..now,'+inf')
        end
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
    pub workspace: Option<String>,
    #[serde(default)]
    pub checkout: Option<super::revision::Checkout>,
    #[serde(default)]
    pub git_repository: Option<std::path::PathBuf>,
    #[serde(default)]
    pub commit_tests: bool,
    #[serde(default)]
    pub version: Option<String>,
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
    pub(super) fn supports_commit_tests(&self) -> bool {
        self.commit_tests
    }

    /// Keep filenames with control characters on one line in the terminal and tool output.
    pub(super) fn workspace_display(&self) -> std::borrow::Cow<'_, str> {
        super::inventory::shown(self.workspace.as_deref().unwrap_or("Not reported"))
    }

    pub(super) fn profile(&self, requested: Option<&str>) -> Option<&WorkerProfile> {
        match requested {
            Some(id) => self.profiles.iter().find(|p| p.id == id),
            None => self.profiles.first(),
        }
    }
    pub(super) fn placement(&self, requested: Option<&str>) -> String {
        let profile = self.profile(requested);
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
    pub serving: bool,
    pub worker: Worker,
    pub age: u64,
    pub used: u16,
}
impl Row {
    pub fn free_slots(&self) -> u16 {
        if self.online() {
            self.worker.slots.saturating_sub(self.used)
        } else {
            0
        }
    }

    pub fn online(&self) -> bool {
        self.serving && self.age < 10
    }
}
/// The row of one worker's `record`. None for a record that does not read:
/// one worker of another version, or a damaged entry, is left out rather
/// than stop the whole group's pool.
fn row(record: &str, age: u64, used: u16) -> Option<Row> {
    Some(Row {
        serving: false,
        worker: serde_json::from_str(record).ok()?,
        age,
        used,
    })
}

/// The rows of the `records` that read, and how many did not.
fn listed(records: Vec<(String, u64, u16)>) -> (Vec<Row>, usize) {
    let recorded = records.len();
    let rows: Vec<Row> = records
        .into_iter()
        .filter_map(|(record, age, used)| row(&record, age, used))
        .collect();
    let unread = recorded - rows.len();
    (rows, unread)
}

#[derive(Clone)]
pub(super) struct Registry {
    client: redis::Client,
    connection: Arc<OnceCell<ConnectionManager>>,
    prefix: String,
}
/// What a failed Redis request is told as: its kind, so a refused
/// password, an unreachable host and a request that timed out are not one
/// sentence. Never the error's text, which can hold the URL and its
/// password.
pub(super) fn unavailable(error: redis::RedisError) -> String {
    use redis::ErrorKind;
    let cause = if error.is_timeout() {
        "Redis did not answer in time"
    } else if error.is_connection_refusal() {
        "Redis refused the connection; check its address and that it is running"
    } else if error.is_io_error() || error.is_connection_dropped() {
        "the connection to Redis failed or was lost"
    } else {
        match error.kind() {
            ErrorKind::AuthenticationFailed => "Redis refused the credentials",
            ErrorKind::InvalidClientConfig => "the Redis address or options are not valid",
            _ => "Redis refused the request",
        }
    };
    format!("Cannot reach the executor pool: {cause}")
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
            .map_err(unavailable)?;
        cmd.query_async(&mut conn.clone())
            .await
            .map_err(unavailable)
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
    /// The workers' rows, and how many records did not read and are left
    /// out.
    pub async fn rows(&self) -> Result<(Vec<Row>, usize)> {
        let rows: Vec<(String, u64, u16)> = self
            .query(
                redis::cmd("EVAL")
                    .arg(WORKERS)
                    .arg(1)
                    .arg(self.workers())
                    .arg(&self.prefix),
            )
            .await?;
        Ok(listed(rows))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One record that did not read failed the whole listing: the pool
    /// stopped for the group.
    #[test]
    fn a_record_that_does_not_read_is_left_out_of_the_listing() {
        assert!(row("{\"name\": 3}", 1, 0).is_none());
        assert!(row("not json", 1, 0).is_none());
        let record = serde_json::json!({
            "name": "mini", "instance": "i", "os": "macos", "host_os": null,
            "arch": "aarch64", "labels": {}, "slots": 2,
            "metrics": serde_json::to_value(Metrics::default()).unwrap_or_default(),
        });
        // The listing counts what it left out, for the inventory to say.
        let (rows, unread) = listed(vec![("not json".into(), 1, 0), (record.to_string(), 4, 1)]);
        assert_eq!((rows.len(), unread), (1, 1));
        let listed = row(&record.to_string(), 4, 1);
        assert_eq!(
            listed.map(|row| (row.worker.name, row.age, row.used)),
            Some(("mini".into(), 4, 1))
        );
    }
}
