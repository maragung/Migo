//! The Redis cache backend.
//!
//! Redis holds exactly the state described in brief section 158: presence, typing,
//! session routing, and rate limit counters. Nothing durable, because losing all of
//! it must cost nothing but freshness (`docs/runbooks/redis-loss.md`).
//!
//! # Why there are Lua scripts
//!
//! Five of the operations here are read-modify-writes: compare-and-set, "increment
//! and set the window only if this created the counter", "refill this token bucket and
//! spend from it if it can afford the price", "add a hash field and extend the hash's
//! TTL but never shorten it", and "unbind this route only if that node still owns it".
//! Each of those done as separate commands is a race, and each done with
//! `WATCH`/`MULTI` is two or three round trips plus a retry loop. A script is one round
//! trip and is atomic by construction, which is the whole reason Redis ships one.
//!
//! The scripts are deliberately tiny and free of control flow beyond a single
//! comparison. Redis runs them on its one thread; a script that loops is a stall for
//! every other client on the instance.
//!
//! # Why a route is a hash
//!
//! [`SessionRoute`] is stored as a two-field hash — `n` for the node id in plain
//! text, `v` for the wire-encoded route — rather than as one string. The unbind
//! script has to compare the owning node, and Lua cannot decode the wire format. The
//! node id is therefore written twice, once where Redis can read it and once inside
//! the value. Storing it once and comparing the whole encoded value instead would
//! look tidier and would be wrong: a heartbeat refresh changes `expires_at`, so the
//! bytes differ, so the guard would reject a legitimate unbind and leave a route
//! pointing at a socket that is gone.

use std::sync::LazyLock;

use async_trait::async_trait;
use migo_core::config::CacheConfig;
use migo_core::{Id, Result, Secret, Timestamp};
use migo_protocol::fault;
use redis::aio::ConnectionManager;
use redis::{Client, Script};
use tokio::sync::OnceCell;

use crate::key::{CacheKey, SCOPE_PRESENCE, SCOPE_ROUTE, SCOPE_ROUTE_INDEX, SCOPE_TYPING};
use crate::model::{BucketSpec, BucketVerdict, Counted, PresenceEntry, SessionRoute, Ttl};
use crate::traits::{
    Cache, CounterCache, KeyValueCache, PresenceCache, RoutingCache, TokenBucketCache, TypingCache,
    MAX_PRESENCE_FANOUT, MAX_VALUE_BYTES,
};

/// Hash field holding a route's owning node, in plain text so Lua can compare it.
const FIELD_NODE: &str = "n";
/// Hash field holding a route's encoded body.
const FIELD_VALUE: &str = "v";

/// Sets a key only when its current value matches, and returns whether it wrote.
static CAS: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r"
local current = redis.call('GET', KEYS[1])
if ARGV[3] == '1' then
  if current == false or current ~= ARGV[4] then return 0 end
elseif current ~= false then
  return 0
end
redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
return 1
",
    )
});

/// Increments a counter and gives it a window if it did not have one, returning the
/// new value and the remaining window in milliseconds.
///
/// `PTTL` is checked rather than the returned value, because "the value equals the
/// increment" is not the same as "this call created the key": a counter that was reset
/// to zero by hand and then incremented by one looks identical, and would silently
/// lose its window.
static INCR_WINDOW: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r"
local value = redis.call('INCRBY', KEYS[1], ARGV[1])
local pttl = redis.call('PTTL', KEYS[1])
if pttl < 0 then
  redis.call('PEXPIRE', KEYS[1], ARGV[2])
  pttl = tonumber(ARGV[2])
end
return {value, pttl}
",
    )
});

/// Hash field holding a bucket's level, in milli-tokens.
const FIELD_TOKENS: &str = "t";
/// Hash field holding the millisecond at which that level was accurate.
const FIELD_UPDATED: &str = "u";

/// Refills a token bucket and spends from it, returning taken, remaining, and wait.
///
/// The whole limiter is this script. It is written to hold four properties that a
/// sequence of commands cannot:
///
/// * One round trip, and no contention with itself. A hot subject — a busy room, an
///   IP behind a large NAT — is charged concurrently by every gateway, and a
///   compare-and-set loop there refuses traffic in proportion to how busy the room is.
/// * `now` comes from the caller, not from Redis. Every other method in this crate
///   takes the caller's clock so the deterministic simulator can move time by hand
///   (ADR-0009), and a bucket whose refill silently used `TIME` would be the one piece
///   of the system a simulated run could not reproduce. It also keeps `EVAL`
///   deterministic, which is what makes the script safe to replicate.
/// * Nothing is written on a refusal. Refill is a pure function of the stored
///   timestamp, so recomputing it next time gives the same answer; skipping the write
///   makes a refusal one command instead of two, exactly when refusals are the common
///   case. It also keeps a flood from resetting the TTL of the bucket it is bouncing
///   off, so the state still expires on schedule.
/// * Time running backwards refills nothing. An NTP step or a caller on a node whose
///   clock lags must not be able to mint tokens, so a negative elapsed is treated as
///   zero and the stored timestamp is left where it was.
/// * The cap is applied whether or not time has passed, so narrowing a limit takes
///   effect on the next call rather than at the next tick of the clock.
static BUCKET_TAKE: LazyLock<Script> = LazyLock::new(|| {
    Script::new(&format!(
        r"
local capacity = tonumber(ARGV[1])
local refill = tonumber(ARGV[2])
local cost = tonumber(ARGV[3])
local now = tonumber(ARGV[4])
local ttl = tonumber(ARGV[5])

local tokens = capacity
local updated = now
local stored = redis.call('HMGET', KEYS[1], '{FIELD_TOKENS}', '{FIELD_UPDATED}')
if stored[1] and stored[2] then
  tokens = tonumber(stored[1])
  updated = tonumber(stored[2])
  local elapsed = now - updated
  if elapsed > 0 then
    tokens = tokens + elapsed * refill
    updated = now
  end
  if tokens > capacity then tokens = capacity end
end

if tokens < cost then
  return {{0, math.floor(tokens / 1000), math.ceil((cost - tokens) / refill)}}
end

tokens = tokens - cost
redis.call('HSET', KEYS[1], '{FIELD_TOKENS}', tokens, '{FIELD_UPDATED}', updated)
redis.call('PEXPIRE', KEYS[1], ttl)
return {{1, math.floor(tokens / 1000), 0}}
"
    ))
});

/// Writes one hash field and pushes the hash's TTL out, never in.
static HSET_TTL: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r"
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
if redis.call('PTTL', KEYS[1]) < tonumber(ARGV[3]) then
  redis.call('PEXPIRE', KEYS[1], ARGV[3])
end
return 1
",
    )
});

/// Writes a route to its own key and to its account's index, in one atomic step.
static ROUTE_BIND: LazyLock<Script> = LazyLock::new(|| {
    // The field names are interpolated rather than written into the Lua, so that
    // `FIELD_NODE` and `FIELD_VALUE` are the only place they are spelled. Two
    // sources of truth for a field name is how a rename half-lands.
    Script::new(&format!(
        r"
redis.call('HSET', KEYS[1], '{FIELD_NODE}', ARGV[2], '{FIELD_VALUE}', ARGV[3])
redis.call('PEXPIRE', KEYS[1], ARGV[4])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
if redis.call('PTTL', KEYS[2]) < tonumber(ARGV[4]) then
  redis.call('PEXPIRE', KEYS[2], ARGV[4])
end
return 1
"
    ))
});

/// Removes a route from both places, but only if the named node still owns it.
static ROUTE_UNBIND: LazyLock<Script> = LazyLock::new(|| {
    Script::new(&format!(
        r"
if redis.call('HGET', KEYS[1], '{FIELD_NODE}') ~= ARGV[2] then return 0 end
redis.call('DEL', KEYS[1])
redis.call('HDEL', KEYS[2], ARGV[1])
return 1
"
    ))
});

/// A cache backed by Redis.
pub struct RedisCache {
    client: Client,
    /// Built on first use rather than at construction.
    ///
    /// `migod` must be able to finish starting while Redis is still coming up: a node
    /// that crash-loops because its *cache* is not ready yet has turned a degradable
    /// dependency into a hard one, which is the opposite of what this layer is for.
    connection: OnceCell<ConnectionManager>,
}

impl std::fmt::Debug for RedisCache {
    /// Deliberately hand-written and deliberately terse.
    ///
    /// A derived `Debug` would reach into `redis::Client`, whose connection info
    /// carries the passphrase. Anything printed here can end up in a panic message, a
    /// log line, or a crash report, so it prints the backend name and nothing else.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedisCache")
    }
}

impl RedisCache {
    /// Prepares a connection without opening one.
    ///
    /// Fails only on a URL Redis cannot parse, which is a configuration error and is
    /// worth failing startup over — unlike an unreachable server, which is not.
    pub fn connect(config: &CacheConfig) -> Result<Self> {
        let url = config
            .url
            .as_ref()
            .filter(|url| !Secret::is_empty(url))
            .ok_or_else(|| fault::validation("cache.url", "is required for the redis backend"))?;
        let client = Client::open(url.expose()).map_err(|error| {
            // The URL is not repeated in the message: it carries the passphrase.
            fault::validation("cache.url", &format!("is not a usable Redis URL: {error}"))
        })?;
        Ok(Self {
            client,
            connection: OnceCell::new(),
        })
    }

    /// The shared connection, opening it if this is the first call.
    ///
    /// `ConnectionManager` reconnects underneath us, so this is a one-time cost and
    /// not a per-request one. A failure here is a cache failure, not a server fault:
    /// callers degrade.
    async fn conn(&self) -> Result<ConnectionManager> {
        let manager = self
            .connection
            .get_or_try_init(|| async {
                ConnectionManager::new(self.client.clone())
                    .await
                    .map_err(|error| fault::cache(format!("connecting to Redis: {error}")))
            })
            .await?;
        // Cheap: the manager is a handle around a shared, reconnecting connection.
        Ok(manager.clone())
    }
}

/// Turns a Redis failure into a cache fault.
///
/// One function so that every message reads the same way in the logs, and so the
/// operation name is always present: "cache: HGETALL failed" is actionable, "cache:
/// broken pipe" is not.
fn redis_failed(operation: &'static str, error: redis::RedisError) -> migo_core::Error {
    fault::cache(format!("{operation} failed: {error}"))
}

/// Rejects an oversized value, with the same wording as the memory backend.
fn check_value(value: &[u8]) -> Result<()> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(fault::validation(
            "value",
            &format!(
                "is {} bytes, over the {MAX_VALUE_BYTES} byte cache limit",
                value.len()
            ),
        ));
    }
    Ok(())
}

/// The presence hash for one account.
fn presence_key(account_id: Id) -> CacheKey {
    CacheKey::of_id(SCOPE_PRESENCE, account_id)
}

/// The typing hash for one conversation.
fn typing_key(conversation_id: Id) -> CacheKey {
    CacheKey::of_id(SCOPE_TYPING, conversation_id)
}

/// The route hash for one device.
fn route_key(device_id: Id) -> CacheKey {
    CacheKey::of_id(SCOPE_ROUTE, device_id)
}

/// The route index hash for one account.
fn route_index_key(account_id: Id) -> CacheKey {
    CacheKey::of_id(SCOPE_ROUTE_INDEX, account_id)
}

#[async_trait]
impl KeyValueCache for RedisCache {
    async fn get(&self, key: &CacheKey, _now: Timestamp) -> Result<Option<Vec<u8>>> {
        let mut conn = self.conn().await?;
        redis::cmd("GET")
            .arg(key.as_str())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("GET", error))
    }

    async fn set(&self, key: &CacheKey, value: &[u8], ttl: Ttl, _now: Timestamp) -> Result<()> {
        check_value(value)?;
        let mut conn = self.conn().await?;
        redis::cmd("SET")
            .arg(key.as_str())
            .arg(value)
            .arg("PX")
            .arg(ttl.as_millis())
            .query_async::<()>(&mut conn)
            .await
            .map_err(|error| redis_failed("SET", error))
    }

    async fn set_if_absent(
        &self,
        key: &CacheKey,
        value: &[u8],
        ttl: Ttl,
        _now: Timestamp,
    ) -> Result<bool> {
        check_value(value)?;
        let mut conn = self.conn().await?;
        // `SET NX` answers with the string OK or with nil, so the reply is read as an
        // option rather than as a boolean: `FromRedisValue for bool` would read nil
        // as false by coincidence rather than by contract.
        let reply: Option<String> = redis::cmd("SET")
            .arg(key.as_str())
            .arg(value)
            .arg("NX")
            .arg("PX")
            .arg(ttl.as_millis())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("SET NX", error))?;
        Ok(reply.is_some())
    }

    async fn compare_and_set(
        &self,
        key: &CacheKey,
        expected: Option<&[u8]>,
        value: &[u8],
        ttl: Ttl,
        _now: Timestamp,
    ) -> Result<bool> {
        check_value(value)?;
        let mut conn = self.conn().await?;
        let wrote: i64 = CAS
            .key(key.as_str())
            .arg(value)
            .arg(ttl.as_millis())
            .arg(i32::from(expected.is_some()))
            .arg(expected.unwrap_or(&[]))
            .invoke_async(&mut conn)
            .await
            .map_err(|error| redis_failed("compare-and-set script", error))?;
        Ok(wrote == 1)
    }

    async fn delete(&self, key: &CacheKey) -> Result<bool> {
        let mut conn = self.conn().await?;
        let removed: i64 = redis::cmd("DEL")
            .arg(key.as_str())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("DEL", error))?;
        Ok(removed > 0)
    }
}

#[async_trait]
impl CounterCache for RedisCache {
    async fn increment(
        &self,
        key: &CacheKey,
        by: u64,
        window: Ttl,
        now: Timestamp,
    ) -> Result<Counted> {
        let mut conn = self.conn().await?;
        let (value, remaining_ms): (i64, i64) = INCR_WINDOW
            .key(key.as_str())
            .arg(by)
            .arg(window.as_millis())
            .invoke_async(&mut conn)
            .await
            .map_err(|error| redis_failed("increment script", error))?;
        Ok(Counted {
            // A negative count means Redis wrapped, which it refuses to do — it
            // errors on overflow — so this is unreachable. Clamped anyway, because
            // "unreachable" plus arithmetic is how counters end up granting quota.
            value: value.max(0).unsigned_abs(),
            expires_at: now.saturating_add_millis(remaining_ms.max(0)),
        })
    }

    async fn count(&self, key: &CacheKey, _now: Timestamp) -> Result<u64> {
        let mut conn = self.conn().await?;
        let value: Option<i64> = redis::cmd("GET")
            .arg(key.as_str())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("GET", error))?;
        Ok(value.unwrap_or(0).max(0).unsigned_abs())
    }

    async fn reset(&self, key: &CacheKey) -> Result<()> {
        let mut conn = self.conn().await?;
        redis::cmd("DEL")
            .arg(key.as_str())
            .query_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("DEL", error))
    }
}

#[async_trait]
impl TokenBucketCache for RedisCache {
    async fn take_tokens(
        &self,
        key: &CacheKey,
        spec: BucketSpec,
        cost: u32,
        now: Timestamp,
    ) -> Result<BucketVerdict> {
        spec.check_affordable(cost)?;
        let mut conn = self.conn().await?;
        let (taken, remaining, wait): (i64, i64, i64) = BUCKET_TAKE
            .key(key.as_str())
            .arg(spec.capacity_milli())
            .arg(spec.refill_per_second())
            .arg(u64::from(cost) * 1000)
            .arg(now.as_millis())
            .arg(spec.state_ttl().as_millis())
            .invoke_async(&mut conn)
            .await
            .map_err(|error| redis_failed("token bucket script", error))?;
        // Clamped rather than trusted. The script cannot return a negative here, and
        // a limiter that would grant quota on an arithmetic surprise is not one worth
        // having, so the conversion refuses to wrap.
        let remaining = u32::try_from(remaining.max(0)).unwrap_or(u32::MAX);
        Ok(if taken == 1 {
            BucketVerdict::taken(remaining)
        } else {
            BucketVerdict::refused(remaining, u32::try_from(wait.max(0)).unwrap_or(u32::MAX))
        })
    }

    async fn peek_bucket(&self, key: &CacheKey, spec: BucketSpec, now: Timestamp) -> Result<u32> {
        let mut conn = self.conn().await?;
        let stored: (Option<u64>, Option<i64>) = redis::cmd("HMGET")
            .arg(key.as_str())
            .arg(FIELD_TOKENS)
            .arg(FIELD_UPDATED)
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("HMGET bucket", error))?;
        let (Some(milli_tokens), Some(updated_at)) = stored else {
            return Ok(spec.capacity());
        };
        Ok(crate::model::BucketState {
            milli_tokens,
            updated_at: Timestamp::from_millis(updated_at),
        }
        .refilled(spec, now)
        .whole_tokens())
    }

    async fn clear_bucket(&self, key: &CacheKey) -> Result<()> {
        let mut conn = self.conn().await?;
        redis::cmd("DEL")
            .arg(key.as_str())
            .query_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("DEL bucket", error))
    }
}

/// Turns one `HGETALL` reply into presence entries, dropping what has expired or will
/// not decode.
///
/// A field that will not decode is skipped rather than failing the whole read. The
/// case is real: a rolling deploy that changes the layout leaves the old shape behind
/// for one TTL, and one stale device must not blank out a user's other three.
fn presence_from_hash(fields: Vec<(String, Vec<u8>)>, now: Timestamp) -> Vec<PresenceEntry> {
    fields
        .into_iter()
        .filter_map(|(field, bytes)| {
            let device_id = Id::parse(&field).ok()?;
            match PresenceEntry::decode(device_id, &bytes) {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::debug!(%device_id, %error, "skipping an undecodable presence field");
                    None
                }
            }
        })
        .filter(|entry| !entry.is_expired(now))
        .collect()
}

#[async_trait]
impl PresenceCache for RedisCache {
    async fn set_presence(&self, entry: PresenceEntry, ttl: Ttl, _now: Timestamp) -> Result<()> {
        let encoded = entry.encode()?;
        let mut conn = self.conn().await?;
        HSET_TTL
            .key(presence_key(entry.account_id).as_str())
            .arg(entry.device_id.to_text())
            .arg(encoded)
            .arg(ttl.as_millis())
            .invoke_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("presence write script", error))
    }

    async fn presence(&self, account_id: Id, now: Timestamp) -> Result<Vec<PresenceEntry>> {
        let mut conn = self.conn().await?;
        let fields: Vec<(String, Vec<u8>)> = redis::cmd("HGETALL")
            .arg(presence_key(account_id).as_str())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("HGETALL", error))?;
        Ok(presence_from_hash(fields, now))
    }

    async fn presence_many(
        &self,
        account_ids: &[Id],
        now: Timestamp,
    ) -> Result<Vec<PresenceEntry>> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }
        let wanted = &account_ids[..account_ids.len().min(MAX_PRESENCE_FANOUT)];
        let mut conn = self.conn().await?;
        // One pipeline rather than one round trip per account. Not `MULTI`: there is
        // nothing to be atomic about, and wrapping reads in a transaction would only
        // block the instance for longer.
        let mut pipeline = redis::pipe();
        for account_id in wanted {
            pipeline
                .cmd("HGETALL")
                .arg(presence_key(*account_id).as_str());
        }
        let replies: Vec<Vec<(String, Vec<u8>)>> = pipeline
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("pipelined HGETALL", error))?;
        Ok(replies
            .into_iter()
            .flat_map(|fields| presence_from_hash(fields, now))
            .collect())
    }

    async fn clear_presence(&self, account_id: Id, device_id: Id) -> Result<()> {
        let mut conn = self.conn().await?;
        redis::cmd("HDEL")
            .arg(presence_key(account_id).as_str())
            .arg(device_id.to_text())
            .query_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("HDEL", error))
    }
}

#[async_trait]
impl TypingCache for RedisCache {
    async fn set_typing(
        &self,
        conversation_id: Id,
        account_id: Id,
        ttl: Ttl,
        now: Timestamp,
    ) -> Result<()> {
        let mut conn = self.conn().await?;
        HSET_TTL
            .key(typing_key(conversation_id).as_str())
            .arg(account_id.to_text())
            // The field value is the deadline in Migo-epoch millis, as decimal text.
            // A typing mark has no other content, so a codec here would be one more
            // thing to keep in step for no gain.
            .arg(ttl.deadline(now).as_millis().to_string())
            .arg(ttl.as_millis())
            .invoke_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("typing write script", error))
    }

    async fn typing(&self, conversation_id: Id, now: Timestamp) -> Result<Vec<Id>> {
        let mut conn = self.conn().await?;
        let fields: Vec<(String, String)> = redis::cmd("HGETALL")
            .arg(typing_key(conversation_id).as_str())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("HGETALL", error))?;
        Ok(fields
            .into_iter()
            .filter_map(|(field, deadline)| {
                let account_id = Id::parse(&field).ok()?;
                let deadline = Timestamp::from_millis(deadline.parse::<i64>().ok()?);
                (!now.is_at_or_after(deadline)).then_some(account_id)
            })
            .collect())
    }

    async fn clear_typing(&self, conversation_id: Id, account_id: Id) -> Result<()> {
        let mut conn = self.conn().await?;
        redis::cmd("HDEL")
            .arg(typing_key(conversation_id).as_str())
            .arg(account_id.to_text())
            .query_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("HDEL", error))
    }
}

#[async_trait]
impl RoutingCache for RedisCache {
    async fn bind_session(&self, route: SessionRoute, ttl: Ttl, _now: Timestamp) -> Result<()> {
        let encoded = route.encode()?;
        let mut conn = self.conn().await?;
        ROUTE_BIND
            .key(route_key(route.device_id).as_str())
            .key(route_index_key(route.account_id).as_str())
            .arg(route.device_id.to_text())
            .arg(route.node_id.as_str())
            .arg(encoded)
            .arg(ttl.as_millis())
            .invoke_async::<i64>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| redis_failed("route bind script", error))
    }

    async fn route(&self, device_id: Id, now: Timestamp) -> Result<Option<SessionRoute>> {
        let mut conn = self.conn().await?;
        let body: Option<Vec<u8>> = redis::cmd("HGET")
            .arg(route_key(device_id).as_str())
            .arg(FIELD_VALUE)
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("HGET", error))?;
        let Some(body) = body else {
            return Ok(None);
        };
        let route = SessionRoute::decode(&body)?;
        Ok((!route.is_expired(now)).then_some(route))
    }

    async fn routes_of_account(&self, account_id: Id, now: Timestamp) -> Result<Vec<SessionRoute>> {
        let mut conn = self.conn().await?;
        let fields: Vec<(String, Vec<u8>)> = redis::cmd("HGETALL")
            .arg(route_index_key(account_id).as_str())
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("HGETALL", error))?;
        Ok(fields
            .into_iter()
            .filter_map(|(field, bytes)| {
                let device_id = Id::parse(&field).ok()?;
                match SessionRoute::decode(&bytes) {
                    Ok(route) if route.device_id == device_id => Some(route),
                    Ok(route) => {
                        // The index field and the route disagree about which device
                        // this is. Not a decode failure, so it would otherwise pass
                        // silently, and it would send somebody else's frames here.
                        tracing::warn!(
                            indexed = %device_id,
                            encoded = %route.device_id,
                            "dropping a route whose index field does not match its body"
                        );
                        None
                    }
                    Err(error) => {
                        tracing::debug!(%device_id, %error, "skipping an undecodable route");
                        None
                    }
                }
            })
            .filter(|route| !route.is_expired(now))
            .filter(|route| route.account_id == account_id)
            .collect())
    }

    async fn unbind_session(&self, device_id: Id, account_id: Id, node_id: &str) -> Result<bool> {
        let mut conn = self.conn().await?;
        let removed: i64 = ROUTE_UNBIND
            .key(route_key(device_id).as_str())
            .key(route_index_key(account_id).as_str())
            .arg(device_id.to_text())
            .arg(node_id)
            .invoke_async(&mut conn)
            .await
            .map_err(|error| redis_failed("route unbind script", error))?;
        Ok(removed == 1)
    }
}

#[async_trait]
impl Cache for RedisCache {
    fn backend_name(&self) -> &'static str {
        "redis"
    }

    async fn health(&self) -> Result<()> {
        let mut conn = self.conn().await?;
        let reply: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|error| redis_failed("PING", error))?;
        if reply != "PONG" {
            return Err(fault::cache(format!("PING answered {reply:?}")));
        }
        Ok(())
    }

    async fn sweep(&self, _now: Timestamp) -> Result<usize> {
        // Redis expires keys itself, actively and lazily. A sweep here would mean
        // `SCAN` over the whole keyspace on a schedule, which is work Redis has
        // already done and which would compete with the traffic that matters.
        Ok(0)
    }
}
