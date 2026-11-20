//! 网关形态身份层（评测通道设计，见 `docs/GATEWAY_AUTH.md`）
//!
//! 原则：
//! - **单机形态默认登录门**：`LANGFIXER_GATEWAY` 未开启时，业务端点默认要求登录态
//!   （内建 admin 账号密码，默认密码 `LangFixer@2026`）；`LANGFIXER_LOGIN=0` 回到
//!   原有免鉴权「零变化」直通，库嵌入装配（`standalone()`）恒直通；
//! - **一张令牌表 = 唯一身份原语**：web 会话与未来 MCP 面共用同一 Bearer 校验与配额；
//! - **超级管理员密钥**：env 给了 `LANGFIXER_ADMIN_TOKEN` 就以 env 为准（内存里只留
//!   SHA-256 摘要）；env 没给则安装时生成一枚落 `gw_meta.admin_token` 并复用。
//!   它不是可签发的评审令牌——`/admin/*` 只认它。**重启永不换钥匙**，换钥匙是显式动作
//!   （`POST /admin/admin-token/rotate`，仅 `gw_meta` 来源可用；env 来源拒绝，见
//!   `GatewayConfig::rotate_admin_token`）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use sha2::{Digest as _, Sha256};

/// 会话有效期（秒）：评测场景按半天足够，重启即失效由懒清理兜底。
pub const SESSION_TTL_SECS: i64 = 12 * 3600;

/// 超管会话在 `gw_sessions.token_id` 里的伪 id（不可能与 `tok_*` 冲突）。
/// 会话解析出它 = 超管身份（不发配额归户，不进 owner 隔离）。
pub const ADMIN_TOKEN_ID: &str = "~admin";

/// 单机形态「owner 已激活」标记键：超管密钥经登录页创建过首个会话后置 1，
/// 此后设置写入要求已验证身份（激活前保持本机免鉴权零变化）。
pub const META_OWNER_CLAIMED: &str = "owner_claimed";

/// 安装时自动生成的超管密钥存储键。
pub const META_ADMIN_TOKEN: &str = "admin_token";

/// 管理员登录密码在 `gw_meta` 里的存储键：只存 SHA-256 hex 摘要，不存明文。
pub const META_ADMIN_PASSWORD: &str = "admin_password_hash";

/// 默认管理员密码：首次启动落库（摘要）；登录后应尽快改（`POST /auth/password`）。
pub const DEFAULT_ADMIN_PASSWORD: &str = "LangFixer@2026";

/// 内建登录用户名：本产品无注册、无多用户，密码登录只认这一个账号。
pub const ADMIN_USERNAME: &str = "admin";

/// 解析 `.env`（KEY=VALUE，支持注释与引号；不覆盖进程环境已有的变量）。
///
///  edition 2024 下 `std::env::set_var` 为 unsafe，这里不改改进程环境，
///  返回映射表由调用方与进程环境合并（进程环境优先）。
pub fn load_dotenv(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value)
            .to_string();
        map.insert(key.trim().to_string(), value);
    }
    map
}

/// 进程环境优先、`.env` 兜底的取值。
fn lookup(dotenv: &HashMap<String, String>, key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dotenv.get(key).cloned())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sha256(input: &str) -> [u8; 32] {
    Sha256::digest(input.as_bytes()).into()
}

/// 定长摘要的恒定时间比较（长度不同直接短路，时序不泄露内容）。
fn digest_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 超级管理员凭证：仅存摘要。
///
/// 摘要是**内部可变**的：显式轮换（`GatewayConfig::rotate_admin_token`）换掉的
/// 就是这一份，旧密钥即刻不再匹配，不需要重启进程。`Clone` 与原对象共享同一份
/// 摘要（`Arc`），因此已注入各路由扩展的 `GatewayConfig` 会一起认新钥匙。
///
/// 密码类凭证带 `salt`（改密落库为 SHA-256(salt ‖ 明文)），旧无盐摘要与密钥类
/// 凭证 salt 为 None（比较退回单轮 SHA-256，语义不变）。
#[derive(Clone)]
pub struct AdminCred {
    pub digest: Arc<RwLock<Option<[u8; 32]>>>,
    salt: Arc<RwLock<Option<Vec<u8>>>>,
}

impl AdminCred {
    pub fn from_token(token: Option<String>) -> Self {
        Self {
            digest: Arc::new(RwLock::new(
                token.filter(|t| !t.is_empty()).map(|t| sha256(&t)),
            )),
            salt: Arc::new(RwLock::new(None)),
        }
    }

    /// 从已落库的**无盐** SHA-256 摘要直接构造（旧格式兼容路径）。
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self {
            digest: Arc::new(RwLock::new(Some(digest))),
            salt: Arc::new(RwLock::new(None)),
        }
    }

    /// 从带盐摘要构造（新格式 `s1$…`）：比较时先喂 salt。
    pub fn from_salted(salt: Vec<u8>, digest: [u8; 32]) -> Self {
        Self {
            digest: Arc::new(RwLock::new(Some(digest))),
            salt: Arc::new(RwLock::new(Some(salt))),
        }
    }

    /// 这把凭证是否活着（有可比的摘要）。
    pub fn is_set(&self) -> bool {
        self.digest
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn matches(&self, presented: &str) -> bool {
        let digest = self.digest.read().unwrap_or_else(|e| e.into_inner());
        let salt = self.salt.read().unwrap_or_else(|e| e.into_inner());
        match &*digest {
            Some(expected) => {
                let got = match &*salt {
                    Some(s) => salted_digest(s, presented),
                    None => sha256(presented),
                };
                digest_eq(expected, &got)
            }
            None => false,
        }
    }

    /// 换成新密钥（进程内立刻生效，无盐）。调用方负责**先落库再调用**：库写失败时
    /// 不会留下「内存是新的、库里是旧的」这种重启即回退的分裂状态。
    pub fn swap(&self, token: &str) {
        *self.digest.write().unwrap_or_else(|e| e.into_inner()) = Some(sha256(token));
        *self.salt.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// 带盐版 `swap`（改密专用）：内存摘要 = SHA-256(salt ‖ 明文)。
    pub fn swap_salted(&self, salt: &[u8], token: &str) {
        *self.digest.write().unwrap_or_else(|e| e.into_inner()) = Some(salted_digest(salt, token));
        *self.salt.write().unwrap_or_else(|e| e.into_inner()) = Some(salt.to_vec());
    }

    /// 当前活着的密钥指纹（SHA-256 前 8 位 hex）。用来核对「这台实例用的是哪把」，
    /// 不回显明文——运维比对指纹即可，无需把密钥再念一遍。
    pub fn fingerprint(&self) -> Option<String> {
        let digest = self.digest.read().unwrap_or_else(|e| e.into_inner());
        digest.map(|bytes| hex(&bytes[..4]))
    }
}

/// 超管密钥的**来源**，决定「怎么换钥匙」这件事归谁：
/// - `Env`：由 `LANGFIXER_ADMIN_TOKEN`（进程环境 / `.env`）持有 → 换钥匙只能改那个文件；
///   轮换取会**拒绝**（库里换一把只会造成「`gw_meta` 新值被 env 无声覆盖」的分裂）。
/// - `Generated`：安装时自动生成并落 `gw_meta.admin_token` → 换钥匙走显式轮换端点。
///
/// 两种来源都是「重启不换钥匙」：重启只重读同一份来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminTokenSource {
    Env,
    Generated,
}

/// 签发结果：`token` 明文只在签发响应里出现一次，库中只存摘要。
#[derive(Debug, Clone)]
pub struct IssuedToken {
    pub id: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub id: String,
    pub label: String,
    pub quota_chars: i64,
}

/// 管理面「密钥 + 用量」一行。用量取自 `gw_usage` 日账，因此
/// `used_retained` 是**保留期内**累计（清理 timer 删掉的旧账不在其中）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenWithUsage {
    pub id: String,
    pub label: String,
    pub quota_chars: i64,
    pub revoked: bool,
    /// 当日（UTC）已用字符数
    pub used_today: i64,
    /// 保留期内累计字符数
    pub used_retained: i64,
    /// 有用量的天数
    pub days_used: i64,
    /// 最近有记账的一天（UTC 日序；从未用过为 None）
    pub last_day: Option<i64>,
    /// 当日（UTC）调用次数——调用级日志口径，**含被拒请求**（401/403/429 也是调用）
    pub calls_today: i64,
    /// 保留期内累计调用次数（含被拒请求）
    pub calls_retained: i64,
}

/// 评审令牌 + 会话存储（独立 `auth.db`，与产品数据 `langfixer.db` 分离）。
pub struct TokenStore {
    conn: Mutex<rusqlite::Connection>,
}

impl TokenStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::from_conn(rusqlite::Connection::open(path).map_err(|e| e.to_string())?)
    }

    /// 内存令牌库：集成测试与「把 api-server 当库用」的宿主用，不落 auth.db。
    pub fn in_memory() -> Result<Self, String> {
        Self::from_conn(rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn from_conn(conn: rusqlite::Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS access_tokens(
                 id          TEXT PRIMARY KEY,
                 label       TEXT NOT NULL,
                 hash        TEXT NOT NULL UNIQUE,
                 quota_chars INTEGER NOT NULL DEFAULT 0,
                 revoked_at  INTEGER,
                 created_at  INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS gw_sessions(
                 id         TEXT PRIMARY KEY,
                 token_id   TEXT NOT NULL,
                 expires_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS gw_session_owner(
                 session_id TEXT PRIMARY KEY,
                 token_id   TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS gw_usage(
                 token_id TEXT NOT NULL,
                 day      TEXT NOT NULL,
                 chars    INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(token_id, day)
             );
             CREATE TABLE IF NOT EXISTS gw_call_log(
                 id         INTEGER PRIMARY KEY,
                 ts         INTEGER NOT NULL,
                 token_id   TEXT,
                 endpoint   TEXT NOT NULL,
                 status     INTEGER NOT NULL,
                 latency_ms INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS gw_meta(
                  key   TEXT PRIMARY KEY,
                  value TEXT NOT NULL
              );
              CREATE INDEX IF NOT EXISTS idx_tokens_hash ON access_tokens(hash);
              CREATE INDEX IF NOT EXISTS idx_call_ts ON gw_call_log(ts);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn issue(&self, label: &str, quota_chars: i64) -> Result<IssuedToken, String> {
        let mut raw = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut raw);
        let token = format!("lf_{}", hex(&raw));
        let id = format!("tok_{}", hex(&raw[..8]));
        let hash = hex(&sha256(&token));
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO access_tokens(id, label, hash, quota_chars, created_at) VALUES(?,?,?,?,?)",
            rusqlite::params![id, label, hash, quota_chars, now_secs()],
        )
        .map_err(|e| e.to_string())?;
        Ok(IssuedToken { id, token })
    }

    /// 按明文令牌找回身份；已吊销 / 过期会话清理在途。
    pub fn verify_token(&self, presented: &str) -> Result<Option<TokenInfo>, String> {
        let hash = hex(&sha256(presented));
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM gw_sessions WHERE expires_at <= ?",
            [now_secs()],
        )
        .map_err(|e| e.to_string())?;
        let row = conn
            .query_row(
                "SELECT id, label, quota_chars FROM access_tokens WHERE hash = ?1 AND revoked_at IS NULL",
                [hash],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)),
            )
            .ok();
        Ok(row.map(|(id, label, quota_chars)| TokenInfo {
            id,
            label,
            quota_chars,
        }))
    }

    pub fn create_session(&self, token_id: &str) -> Result<String, String> {
        let raw = 16;
        let mut buf = vec![0u8; raw];
        rand::thread_rng().fill_bytes(&mut buf);
        let sid = hex(&buf);
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO gw_sessions(id, token_id, expires_at) VALUES(?,?,?)",
            rusqlite::params![sid, token_id, now_secs() + SESSION_TTL_SECS],
        )
        .map_err(|e| e.to_string())?;
        Ok(sid)
    }

    pub fn resolve_session(&self, sid: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let row = conn
            .query_row(
                "SELECT token_id FROM gw_sessions WHERE id = ?1 AND expires_at > ?2",
                rusqlite::params![sid, now_secs()],
                |r| r.get::<_, String>(0),
            )
            .ok();
        Ok(row)
    }

    pub fn end_session(&self, sid: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM gw_sessions WHERE id = ?1", [sid])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 清掉某个身份的全部在途会话（不换密钥时不用；轮换超管密钥时用——
    /// 那些 cookie 是旧钥匙换来的，留着等于旧钥匙仍有效）。
    pub fn clear_sessions(&self, token_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM gw_sessions WHERE token_id = ?1", [token_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let hit = conn
            .execute(
                "UPDATE access_tokens SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
                rusqlite::params![now_secs(), id],
            )
            .map_err(|e| e.to_string())?;
        if hit > 0 {
            conn.execute("DELETE FROM gw_sessions WHERE token_id = ?1", [id])
                .map_err(|e| e.to_string())?;
        }
        Ok(hit > 0)
    }

    /// 彻底删除（revoked 行的清理；连带在途会话）
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM gw_sessions WHERE token_id = ?1", [id])
            .map_err(|e| e.to_string())?;
        let hit = conn
            .execute("DELETE FROM access_tokens WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        Ok(hit > 0)
    }

    /// 绑定并校验会话归属：首次使用即绑定；已绑定但归属他人 → false（fail-closed）。
    pub fn bind_session(&self, session_id: &str, token_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO gw_session_owner(session_id, token_id) VALUES(?1, ?2)",
            rusqlite::params![session_id, token_id],
        )
        .map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT token_id = ?2 FROM gw_session_owner WHERE session_id = ?1",
            rusqlite::params![session_id, token_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
    }

    pub fn session_owner(&self, session_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT token_id FROM gw_session_owner WHERE session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .ok()
        .map(Ok)
        .transpose()
    }

    /// 删除会话的归属绑定（会话被删除时调用）
    pub fn unbind_session(&self, session_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM gw_session_owner WHERE session_id = ?1",
            [session_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 元数据读取（安装生成的超管密钥 / owner 激活标记等单值键）
    pub fn meta_get(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        Ok(conn
            .query_row("SELECT value FROM gw_meta WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .ok())
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO gw_meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn today() -> String {
        (now_secs() / 86400).to_string()
    }

    /// 当日已用字符数（UTC 日界；用量表按天聚合）
    pub fn usage_today(&self, token_id: &str) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT chars FROM gw_usage WHERE token_id = ?1 AND day = ?2",
            rusqlite::params![token_id, Self::today()],
            |r| r.get(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(0),
            other => Err(other.to_string()),
        })
    }

    /// 管理面用量视图（一条 SQL 出全部密钥，避免 N+1）。
    ///
    /// `used_retained` 只含 `gw_usage` 里还在的日账——retention timer 会按保留期删旧账，
    /// 所以它是「保留期内累计」而不是历史总量（界面须如实标注，别写成总量）。
    pub fn list_with_usage(&self) -> Result<Vec<TokenWithUsage>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.label, t.quota_chars, t.revoked_at,
                        COALESCE((SELECT chars FROM gw_usage u
                                   WHERE u.token_id = t.id AND u.day = ?1), 0),
                        COALESCE((SELECT SUM(chars) FROM gw_usage u
                                   WHERE u.token_id = t.id), 0),
                        COALESCE((SELECT COUNT(1) FROM gw_usage u
                                   WHERE u.token_id = t.id), 0),
                        (SELECT MAX(CAST(day AS INTEGER)) FROM gw_usage u
                                   WHERE u.token_id = t.id),
                        COALESCE((SELECT COUNT(1) FROM gw_call_log c
                                   WHERE c.token_id = t.id AND c.ts >= ?2), 0),
                        COALESCE((SELECT COUNT(1) FROM gw_call_log c
                                   WHERE c.token_id = t.id), 0)
                 FROM access_tokens t ORDER BY t.created_at",
            )
            .map_err(|e| e.to_string())?;
        let day_start = (now_secs() / 86400) * 86400;
        let rows = stmt
            .query_map(rusqlite::params![Self::today(), day_start], |r| {
                Ok(TokenWithUsage {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    quota_chars: r.get(2)?,
                    revoked: r.get::<_, Option<i64>>(3)?.is_some(),
                    used_today: r.get(4)?,
                    used_retained: r.get(5)?,
                    days_used: r.get(6)?,
                    last_day: r.get(7)?,
                    calls_today: r.get(8)?,
                    calls_retained: r.get(9)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// 保留期内的用量总账（管理面概览）：今日合计与在账合计。
    pub fn usage_totals(&self) -> Result<(i64, i64), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let today: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(chars), 0) FROM gw_usage WHERE day = ?1",
                [Self::today()],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let retained: i64 = conn
            .query_row("SELECT COALESCE(SUM(chars), 0) FROM gw_usage", [], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        Ok((today, retained))
    }

    pub fn add_usage(&self, token_id: &str, chars: i64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO gw_usage(token_id, day, chars) VALUES(?1, ?2, ?3)
             ON CONFLICT(token_id, day) DO UPDATE SET chars = chars + ?3",
            rusqlite::params![token_id, Self::today(), chars],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 调用级记账（网关形态）：每个经过鉴权门的请求一行，含被拒的 401/403/429。
    ///
    /// **隐私口径**（docs/GATEWAY_AUTH.md「数据红线」同源）：只存路径，不带 query
    /// string——查询串可能含用户文本（如 `?text=`），请求体更是绝不落这里；
    /// 身份只存 token_id（匿名与超管为 NULL）。记账失败绝不影响请求本身，
    /// 只留 warn 日志——调用日志是运维资料，不是事务参与方。
    pub fn log_call(&self, token_id: Option<&str>, endpoint: &str, status: u16, latency_ms: u64) {
        let endpoint: String = endpoint.chars().take(160).collect();
        match self.conn.lock() {
            Ok(conn) => {
                if let Err(e) = conn.execute(
                    "INSERT INTO gw_call_log(ts, token_id, endpoint, status, latency_ms)
                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        now_secs(),
                        token_id,
                        endpoint,
                        status as i64,
                        latency_ms.min(i64::MAX as u64) as i64
                    ],
                ) {
                    tracing::warn!(error = %e, "调用级日志写入失败（不影响请求）");
                }
            }
            Err(e) => tracing::warn!(error = %e, "调用级日志锁失败（不影响请求）"),
        }
    }

    /// 调用级总账（管理面概览）：保留期内合计与今日（UTC）合计。
    pub fn call_totals(&self) -> Result<(i64, i64), String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let day_start = (now_secs() / 86400) * 86400;
        let retained = conn
            .query_row("SELECT COUNT(1) FROM gw_call_log", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let today: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM gw_call_log WHERE ts >= ?1",
                [day_start],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok((retained, today))
    }

    pub fn quota(&self, token_id: &str) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT quota_chars FROM access_tokens WHERE id = ?1",
            [token_id],
            |r| r.get(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(0),
            other => Err(other.to_string()),
        })
    }

    pub fn list(&self) -> Result<Vec<(String, String, i64, Option<i64>)>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, label, quota_chars, revoked_at FROM access_tokens ORDER BY created_at",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn active_sessions(&self) -> Result<i64, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT COUNT(*) FROM gw_sessions WHERE expires_at > ?1",
            [now_secs()],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
    }
}

/// 网关形态全局闸（历史复用 / 进程级缓存的开关依据）。
/// 只在 `GatewayConfig::from_env` 主路径置位；单元测试进程恒 false，零污染。
/// 依据：历史复用「复用即不写新历史」，网关形态下会让复用他人记录
/// （同文本）的账号留下归属空洞——评测演示里等于 B 的历史列表缺条目。
static GATEWAY_MODE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn gateway_mode() -> bool {
    GATEWAY_MODE.get().copied().unwrap_or(false)
}

/// 网关形态配置。
pub struct GatewayConfig {
    pub enabled: bool,
    pub admin: AdminCred,
    pub store: TokenStore,
    pub cookie_secure: bool,
    /// 当前超管密钥来自 env 还是安装生成的 `gw_meta`（决定轮换是否可用）。
    pub admin_source: AdminTokenSource,
    /// 单机形态登录门：开启时业务端点要求登录态（会话 cookie / 超管 Bearer），
    /// 未登录一律 401 + `login_required`；默认开启，`LANGFIXER_LOGIN=0` 回到
    /// 「零变化」直通。网关形态恒 false（已有全量鉴权门，本开关不参与）。
    pub login_gate: bool,
    /// 管理员登录密码（内存只留摘要，改密后为带盐 SHA-256）：来源语义与超管密钥同款——
    /// env 给了以 env 为准（不进库），否则用库里的摘要，都没有时以默认密码初始化落库。
    pub password: AdminCred,
    /// 登录失败限流（按来源 IP）：连续失败计数 + 封锁起点。
    /// 内存态即可：重启把封锁清零可接受，目标是挡住脚本暴破而非持久惩罚。
    pub login_throttle: Arc<Mutex<HashMap<String, (u32, std::time::Instant)>>>,
}

#[derive(Debug)]
pub enum GatewayConfigError {
    /// 开启网关但未配置（或过短）LANGFIXER_ADMIN_TOKEN
    MissingAdminToken,
    /// auth.db 打开失败
    Store(String),
}

impl GatewayConfig {
    /// 校验语义单独成函数便于测试：env 给了超管令牌但 <32 字符视为配置错误；
    /// env 完全未设置时**安装时自动生成**并持久化到 `gw_meta`（两形态同规则），
    /// 明文只在生成那一次的启动日志里出现。
    pub fn build(
        dotenv: &HashMap<String, String>,
        db_path: &Path,
    ) -> Result<Self, GatewayConfigError> {
        let store = TokenStore::open(db_path).map_err(GatewayConfigError::Store)?;
        Self::build_with_store(dotenv, store)
    }

    /// 单机形态的直通配置（库嵌入 / 集成测试装配用，见 `middleware::auth::with_standalone_tenant`）：
    /// `enabled = false`（不拒任何请求、不落调用日志）+ 内存令牌库（owner 未认领，
    /// 因此 `guard_settings_write` 恒通过）。语义与 `gate` 在单机形态注入的一致。
    pub fn standalone() -> Self {
        let store = TokenStore::in_memory().expect("内存令牌库不可用");
        // 内存库没有 admin_token：build_with_store 会生成一枚（单机形态用不到它）
        let mut cfg = Self::build_with_store(&HashMap::new(), store).expect("单机形态配置不应失败");
        // 库嵌入 / 集成测试装配用：登录门保持关闭，业务端点不拒任何请求。
        cfg.login_gate = false;
        cfg
    }

    fn build_with_store(
        dotenv: &HashMap<String, String>,
        store: TokenStore,
    ) -> Result<Self, GatewayConfigError> {
        let enabled = lookup(dotenv, "LANGFIXER_GATEWAY").is_some_and(|v| v == "1");
        let env_admin = lookup(dotenv, "LANGFIXER_ADMIN_TOKEN");
        if env_admin.as_deref().is_some_and(|t| t.len() < 32) {
            return Err(GatewayConfigError::MissingAdminToken);
        }
        // 库里那枚（安装时生成的）无论 env 是否给出都要读出来：一是判断来源，
        // 二是两者同时有值时必须 WARN——否则「改了 gw_meta 却没生效」无声发生。
        let stored_admin = store
            .meta_get(META_ADMIN_TOKEN)
            .map_err(GatewayConfigError::Store)?;
        if env_admin.is_some() && stored_admin.is_some() {
            let fp = stored_admin
                .as_deref()
                .map(|t| hex(&sha256(t)[..4]))
                .unwrap_or_default();
            tracing::warn!(
                stored_fingerprint = %fp,
                "LANGFIXER_ADMIN_TOKEN 与 gw_meta.admin_token 同时有值：以 env 为准，\
                 库里那枚（指纹 {fp}）不会被使用。若要改用轮换端点管理，先从 env 删掉 \
                 LANGFIXER_ADMIN_TOKEN 再重启"
            );
        }
        let admin_source = if env_admin.is_some() {
            AdminTokenSource::Env
        } else {
            AdminTokenSource::Generated
        };
        let admin_token = match env_admin {
            Some(t) => Some(t),
            None => match stored_admin {
                Some(t) => Some(t),
                None => {
                    let mut buf = [0u8; 20];
                    rand::thread_rng().fill_bytes(&mut buf);
                    let generated = format!("lf_{}", hex(&buf));
                    store
                        .meta_set(META_ADMIN_TOKEN, &generated)
                        .map_err(GatewayConfigError::Store)?;
                    tracing::info!(
                        token = %generated,
                        "首次安装：自动生成超级管理员密钥（用于登录配置面；只出现这一次，请妥善保存）"
                    );
                    Some(generated)
                }
            },
        };
        let cookie_secure = lookup(dotenv, "LANGFIXER_COOKIE_SECURE")
            .map(|v| v != "0")
            .unwrap_or(enabled); // 网关形态默认 Secure（本机明文联调设 0）
        // 管理员登录密码：env 给了以 env 为准（不进库，与超管密钥同款）；否则用
        // 库里摘要；两者都没有时以默认密码初始化并落摘要，WARN 一次提醒改密。
        let env_pw = lookup(dotenv, "LANGFIXER_ADMIN_PASSWORD").filter(|v| !v.is_empty());
        let stored_pw = store
            .meta_get(META_ADMIN_PASSWORD)
            .map_err(GatewayConfigError::Store)?;
        if env_pw.is_some() && stored_pw.is_some() {
            tracing::warn!(
                "LANGFIXER_ADMIN_PASSWORD 与 gw_meta.{META_ADMIN_PASSWORD} 同时有值：以 env 为准，库里那份不会被使用"
            );
        }
        let password = match env_pw {
            Some(p) => AdminCred::from_token(Some(p)),
            None => match stored_pw.as_deref().map(decode_stored_pw) {
                Some(StoredPw::Salted { salt, digest }) => AdminCred::from_salted(salt, digest),
                Some(StoredPw::Legacy(d)) => {
                    tracing::info!(
                        "gw_meta.{META_ADMIN_PASSWORD} 是旧版无盐 SHA-256 摘要：下次改密自动升级为带盐格式"
                    );
                    AdminCred::from_digest(d)
                }
                Some(StoredPw::Invalid) => {
                    // 有值但解不开：损坏或篡改。绝不静默重播种回公开的默认密码——
                    // 那等于把「密码被重置成所有人都知道的值」伪装成首次启动。
                    tracing::error!(
                        "gw_meta.{META_ADMIN_PASSWORD} 有值但无法解码（损坏或被篡改）：密码登录已禁用。
                         请用超级管理员密钥登录后重新改密，或设 LANGFIXER_ADMIN_PASSWORD 后重启"
                    );
                    AdminCred::from_token(None)
                }
                None => {
                    // 首次启动：落带盐摘要（新格式从第一天就用上），WARN 不再念明文。
                    let mut salt = [0u8; 16];
                    rand::thread_rng().fill_bytes(&mut salt);
                    store
                        .meta_set(
                            META_ADMIN_PASSWORD,
                            &encode_salted(&salt, &salted_digest(&salt, DEFAULT_ADMIN_PASSWORD)),
                        )
                        .map_err(GatewayConfigError::Store)?;
                    tracing::warn!(
                        "已用默认管理员密码初始化（用户名 {ADMIN_USERNAME}，密码见 README/OPERATIONS.md）：请尽快改密，或 POST /auth/password"
                    );
                    AdminCred::from_salted(
                        salt.to_vec(),
                        salted_digest(&salt, DEFAULT_ADMIN_PASSWORD),
                    )
                }
            },
        };
        // 登录门：单机形态默认开启（用户要求产品级登录）；显式 0 回到直通；
        // 网关形态已有鉴权门，本开关不参与。
        let login_gate = !enabled
            && lookup(dotenv, "LANGFIXER_LOGIN")
                .map(|v| v != "0")
                .unwrap_or(true);
        Ok(Self {
            enabled,
            admin: AdminCred::from_token(admin_token),
            store,
            cookie_secure,
            admin_source,
            login_gate,
            password,
            login_throttle: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 密码登录核验：用户名必须为内建的 admin，密码比内存摘要。
    pub fn verify_password(&self, username: &str, password: &str) -> bool {
        username == ADMIN_USERNAME && self.password.matches(password)
    }

    /// 改管理员密码：**先落库再换内存摘要**（与密钥轮换同序，避免分裂）。
    /// 落库一律用带盐格式；连带清掉全部在途超管会话——改密的典型动机就是
    /// 旧密码疑似泄露，不清会话等于旧密码换的 cookie 仍持续有效（与密钥轮换同理）。
    /// 调用方若要保留当前会话，清完后自行 `create_session` 重发即可。
    pub fn rotate_password(&self, new: &str) -> Result<(), String> {
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        self.store.meta_set(
            META_ADMIN_PASSWORD,
            &encode_salted(&salt, &salted_digest(&salt, new)),
        )?;
        self.password.swap_salted(&salt, new);
        self.store.clear_sessions(ADMIN_TOKEN_ID)?;
        tracing::warn!(password_fingerprint = %self.password.fingerprint().unwrap_or_default(), "管理员密码已变更：旧密码与其全部会话即刻失效（新会话需重新领取）");
        Ok(())
    }

    /// **显式**轮换超级管理员密钥：换钥匙是一个动作，不是重启 / 改文件的副作用。
    ///
    /// - 来源为 `Env` 时拒绝（`RotateError::EnvOwned`）：那把由部署环境持有，
    ///   在库里换一把只会被 env 无声覆盖，制造「两把都说自己活着」的分裂。
    /// - 顺序是先落库再换内存摘要：库写失败时内存不动，重启后仍是原来那把。
    /// - 连带清掉超管会话（`gw_sessions.token_id = "~admin"`）：那些 cookie 是用旧钥匙
    ///   换的，留着等于给旧钥匙留一条仍有效的后门。评审密钥与其会话不受影响。
    ///
    /// 返回新明文，**只在这次响应里出现**；日志只记指纹。
    pub fn rotate_admin_token(&self) -> Result<String, RotateError> {
        if self.admin_source == AdminTokenSource::Env {
            return Err(RotateError::EnvOwned);
        }
        let mut buf = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut buf);
        let next = format!("lf_{}", hex(&buf));
        self.store
            .meta_set(META_ADMIN_TOKEN, &next)
            .map_err(RotateError::Store)?;
        self.admin.swap(&next);
        self.store
            .clear_sessions(ADMIN_TOKEN_ID)
            .map_err(RotateError::Store)?;
        tracing::warn!(
            fingerprint = %self.admin.fingerprint().unwrap_or_default(),
            "超级管理员密钥已轮换：旧密钥与其会话即刻失效（密钥来源 gw_meta）"
        );
        Ok(next)
    }

    pub fn from_env(dotenv: &HashMap<String, String>) -> Self {
        let db = lookup(dotenv, "LANGFIXER_AUTH_DB")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("auth.db"));
        match Self::build(dotenv, &db) {
            Ok(cfg) => {
                let _ = GATEWAY_MODE.set(cfg.enabled);
                cfg
            }
            Err(GatewayConfigError::MissingAdminToken) => {
                panic!(
                    "LANGFIXER_GATEWAY=1 必须配置 LANGFIXER_ADMIN_TOKEN（≥32 字符，生成：openssl rand -hex 32）。单机形态请置 0 或不设。"
                );
            }
            Err(GatewayConfigError::Store(e)) => panic!("打开 {db:?} 失败: {e}"),
        }
    }
}

/// `rotate_admin_token` 的失败原因。
#[derive(Debug)]
pub enum RotateError {
    /// 当前密钥由 `LANGFIXER_ADMIN_TOKEN` 持有，轮换端点不接管（改 env 是唯一途径）
    EnvOwned,
    /// 写 `gw_meta` / 清会话失败
    Store(String),
}

impl RotateError {
    /// 面向运维的中文说明（进 409 / 500 响应体，不含任何密钥内容）。
    pub fn user_message(&self) -> String {
        match self {
            Self::EnvOwned => "当前超级管理员密钥由 LANGFIXER_ADMIN_TOKEN（进程环境 / .env）持有，\
                               轮换端点不接管：请改该文件后重启服务（重启本身不换钥匙，\
                               换的正是你写进文件里的新值）。若想让本端点接管，先从 env 删掉该变量再重启。"
                .to_string(),
            Self::Store(e) => format!("轮换失败（密钥未变更）: {e}"),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 解码 64 位 hex 摘要；非法输入返回 None（落库摘要被手工改坏时按「没有」处理）。
fn decode_digest_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// 带盐摘要：SHA-256(salt ‖ 明文)。单轮无盐 SHA-256 对离线暴破太便宜（泄露的
/// auth.db 一个哈希就是全部），改密后一律用此格式；旧格式比较时仍兼容。
fn salted_digest(salt: &[u8], plain: &str) -> [u8; 32] {
    let mut h = <Sha256 as sha2::Digest>::new();
    h.update(salt);
    h.update(plain.as_bytes());
    h.finalize().into()
}

/// 新格式落库串：`s1$<hex salt>$<hex digest>`。
fn encode_salted(salt: &[u8], digest: &[u8; 32]) -> String {
    format!("s1${}${}", hex(salt), hex(digest))
}

/// `gw_meta.admin_password_hash` 的存储形态。区分「旧格式」与「解不开」：
/// 损坏/篡改绝不能静默回落到公开的默认密码（那是把锁被拆了当重置用）。
enum StoredPw {
    Salted {
        salt: Vec<u8>,
        digest: [u8; 32],
    },
    /// 旧格式：64 位 hex 的无盐 SHA-256（比较兼容，下次改密自动升级）。
    Legacy([u8; 32]),
    /// 有值但无法解码：损坏或篡改。
    Invalid,
}

fn decode_stored_pw(s: &str) -> StoredPw {
    if let Some(rest) = s.strip_prefix("s1$") {
        let Some((salt_hex, digest_hex)) = rest.split_once('$') else {
            return StoredPw::Invalid;
        };
        // salt 至少 8 字节 hex（防手工截短）；摘要仍是 64 hex
        let Some(salt) = hex_decode(salt_hex).filter(|v| v.len() >= 8) else {
            return StoredPw::Invalid;
        };
        return match decode_digest_hex(digest_hex) {
            Some(digest) => StoredPw::Salted { salt, digest },
            None => StoredPw::Invalid,
        };
    }
    match decode_digest_hex(s) {
        Some(d) => StoredPw::Legacy(d),
        None => StoredPw::Invalid,
    }
}

/// 每个请求的作用域（owner 隔离与用量归户都读它）。
#[derive(Clone, Debug)]
pub struct RequestScope {
    pub token_id: Option<String>,
    pub is_admin: bool,
}

/// handler 侧一眼可读的身份上下文：gate 对**每个放行的请求**注入（含匿名放行的
/// 登录入口与门禁页探测），因此业务 handler 用 `Extension<AuthContext>` 取用即可。
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// 网关形态（`LANGFIXER_GATEWAY=1`）
    pub gateway: bool,
    /// 超管（超管密钥或其会话；单机形态恒 true，与 RequestScope 同口径）
    pub admin: bool,
    /// 携带了有效凭证（网关形态放行即 true；单机形态看是否带了超管凭证/会话）
    pub authed: bool,
}

/// 租户提取器：网关形态下的 handler 侧统一入口（单机形态全部为 None = 直通）。
/// 由 gate 注入的 `RequestScope` + `Arc<GatewayConfig>` 扩展合成。
#[derive(Clone)]
pub struct Tenant {
    pub token_id: Option<String>,
    gw: Option<std::sync::Arc<GatewayConfig>>,
}

impl Tenant {
    /// 单机形态的直通身份：不隔离、不计量、不持网关配置句柄（各守卫恒通过）。
    ///
    /// 生产路径由 `middleware::auth::gate` 注入（两种形态都注入）；绕开 `main.rs`
    /// 直接组装 `routes::router` 的调用方（集成测试 / 把 api-server 当库用）走这里。
    pub fn standalone() -> Self {
        Self {
            token_id: None,
            gw: None,
        }
    }
    /// 网关形态且为评审身份（非超管）时生效。
    pub fn gateway_active(&self) -> bool {
        self.token_id.is_some()
    }

    /// 会话归属守卫：首次使用即绑定；归属他人即 403。单机形态恒通过。
    pub fn guard_session(&self, session_id: &str) -> Result<(), crate::routes::ApiError> {
        match (&self.token_id, &self.gw) {
            (Some(tok), Some(cfg)) => {
                let owned = cfg
                    .store
                    .bind_session(session_id, tok)
                    .map_err(crate::routes::ApiError::Internal)?;
                if owned {
                    Ok(())
                } else {
                    Err(crate::routes::ApiError::Forbidden(
                        "该会话不属于当前评测账号".into(),
                    ))
                }
            }
            _ => Ok(()),
        }
    }

    /// 会话归属查询（单机形态恒 None）。
    pub fn owner_of(&self, session_id: &str) -> Result<Option<String>, crate::routes::ApiError> {
        match &self.gw {
            Some(cfg) => cfg
                .store
                .session_owner(session_id)
                .map_err(crate::routes::ApiError::Internal),
            None => Ok(None),
        }
    }

    /// 记录可见性：网关形态只放行归属本账号的会话数据；无归属记录 fail-closed。
    pub fn may_see_session(
        &self,
        session_id: Option<&str>,
    ) -> Result<bool, crate::routes::ApiError> {
        if !self.gateway_active() {
            return Ok(true);
        }
        match session_id {
            None => Ok(false),
            Some(s) => Ok(self.owner_of(s)?.as_deref() == self.token_id.as_deref()),
        }
    }

    /// 用量归户对：仅网关评审身份返回 Some。
    pub fn metered(&self) -> Option<(String, std::sync::Arc<GatewayConfig>)> {
        match (&self.token_id, &self.gw) {
            (Some(tok), Some(cfg)) => Some((tok.clone(), cfg.clone())),
            _ => None,
        }
    }

    /// 网关形态拒绝跨账号操作（全局视图 / 批量删除等在评测通道不开放）。
    pub fn deny_gateway(&self, what: &str) -> Result<(), crate::routes::ApiError> {
        if self.gateway_active() {
            return Err(crate::routes::ApiError::Forbidden(format!(
                "评测通道按账号隔离，不开放{what}"
            )));
        }
        Ok(())
    }

    /// 配额计量（检测入口调用）：网关评审身份按字符数记账；
    /// 令牌 `quota_chars > 0` 且本次会越线时报 429，否则累加当日用量。
    /// 单机形态恒通过、不记账。
    pub fn charge_chars(&self, chars: usize) -> Result<(), crate::routes::ApiError> {
        let Some((tok, cfg)) = self.metered() else {
            return Ok(());
        };
        let chars = chars as i64;
        let quota = cfg
            .store
            .quota(&tok)
            .map_err(crate::routes::ApiError::Internal)?;
        if quota > 0 {
            let used = cfg
                .store
                .usage_today(&tok)
                .map_err(crate::routes::ApiError::Internal)?;
            if used + chars > quota {
                return Err(crate::routes::ApiError::TooManyRequests(format!(
                    "本评测账号今日字符配额已用完（已用 {used} / 共 {quota} 字符），请明日再试或联系组织方"
                )));
            }
        }
        cfg.store
            .add_usage(&tok, chars)
            .map_err(crate::routes::ApiError::Internal)
    }

    /// 网关形态下检测历史的归属键：账号伪会话 `s_gw:<token_id>`（单机形态 None = 不归户）。
    /// 写入前经 `guard_session` 首次绑定，历史列表 / 详情 / 删除按它做跨账号过滤。
    pub fn history_tag(&self) -> Option<String> {
        self.token_id.as_ref().map(|t| format!("s_gw:{t}"))
    }

    /// 由 gate 注入的 `RequestScope` 合成（admin / 单机形态 token 恒 None = 直通）。
    /// edition 2024 与 axum-core 0.4 的 async trait 生命周期规则不兼容（E0195），
    /// 因此不做自定义提取器：gate 统一注入，handler 用标准 `Extension<Tenant>` 取用。
    pub(crate) fn from_scope(scope: &RequestScope, cfg: &std::sync::Arc<GatewayConfig>) -> Self {
        Self {
            token_id: if scope.is_admin {
                None
            } else {
                scope.token_id.clone()
            },
            gw: Some(cfg.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("lf-gw-test-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn dotenv(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn dotenv_parses_quotes_comments_and_skips_process_env() {
        let mut p = std::env::temp_dir();
        p.push(format!("lf-dotenv-{}.env", std::process::id()));
        std::fs::write(
            &p,
            "# comment\nLANGFIXER_TEST_A=plain\nLANGFIXER_TEST_B=\"quoted\"\n",
        )
        .unwrap();
        let map = load_dotenv(&p);
        assert_eq!(
            map.get("LANGFIXER_TEST_A").map(String::as_str),
            Some("plain")
        );
        assert_eq!(
            map.get("LANGFIXER_TEST_B").map(String::as_str),
            Some("quoted")
        );
        assert!(!map.contains_key("LANGFIXER_TEST_C"));
        let _ = std::fs::remove_file(&p);
    }

    /// 改密落库必须是带盐新格式，且连带清掉超管在途会话（旧 cookie 不能幸存）。
    #[test]
    fn rotate_password_salts_and_clears_admin_sessions() {
        let db = temp_db("rot-pw");
        let cfg = GatewayConfig::build(&dotenv(&[]), &db).unwrap();
        let sid = cfg.store.create_session(ADMIN_TOKEN_ID).unwrap();
        cfg.rotate_password("FreshPass@12345").unwrap();
        // 旧会话即刻失效
        assert_eq!(cfg.store.resolve_session(&sid).unwrap().is_none(), true);
        // 新密码可用；旧默认密码废弃
        assert!(cfg.verify_password(ADMIN_USERNAME, "FreshPass@12345"));
        assert!(!cfg.verify_password(ADMIN_USERNAME, DEFAULT_ADMIN_PASSWORD));
        // 重启重建：库里是带盐格式，新密码仍认
        let cfg2 = GatewayConfig::build(&dotenv(&[]), &db).unwrap();
        assert!(cfg2.verify_password(ADMIN_USERNAME, "FreshPass@12345"));
        let stored = cfg2.store.meta_get(META_ADMIN_PASSWORD).unwrap().unwrap();
        assert!(stored.starts_with("s1$"), "应落带盐格式，实际 {stored}");
        let _ = std::fs::remove_file(&db);
    }

    /// 摘要损坏/篡改：禁用密码登录，绝不静默回落默认密码，库里坏值原样保留。
    #[test]
    fn corrupted_password_digest_disables_login_without_reseeding() {
        let db = temp_db("corrupt-pw");
        {
            let cfg = GatewayConfig::build(&dotenv(&[]), &db).unwrap();
            cfg.store
                .meta_set(META_ADMIN_PASSWORD, "not-a-digest!!")
                .unwrap();
        }
        let cfg = GatewayConfig::build(&dotenv(&[]), &db).unwrap();
        assert!(!cfg.password.is_set(), "损坏摘要必须禁用密码登录");
        assert!(!cfg.verify_password(ADMIN_USERNAME, DEFAULT_ADMIN_PASSWORD));
        // 未被重播种：库里的坏值仍在（留给运维取证，不是被我们洗成默认密码）
        let stored = cfg.store.meta_get(META_ADMIN_PASSWORD).unwrap().unwrap();
        assert_eq!(stored, "not-a-digest!!");
        let _ = std::fs::remove_file(&db);
    }

    /// 旧库的无盐 64-hex 摘要仍可比对（升级前兼容），改密后自动升级带盐。
    #[test]
    fn legacy_unsalted_digest_compatible_then_upgraded() {
        let db = temp_db("legacy-pw");
        {
            let cfg = GatewayConfig::build(&dotenv(&[]), &db).unwrap();
            // 手工把库里摘要写成旧格式：sha256(已知密码)
            cfg.store
                .meta_set(META_ADMIN_PASSWORD, &hex(&sha256("OldSaltlessPass1")))
                .unwrap();
        }
        let cfg = GatewayConfig::build(&dotenv(&[]), &db).unwrap();
        assert!(cfg.verify_password(ADMIN_USERNAME, "OldSaltlessPass1"));
        cfg.rotate_password("FreshPass@12345").unwrap();
        let stored = cfg.store.meta_get(META_ADMIN_PASSWORD).unwrap().unwrap();
        assert!(stored.starts_with("s1$"), "改密必须升级带盐格式");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn issue_verify_revoke_roundtrip() {
        let db = temp_db("roundtrip");
        let store = TokenStore::open(&db).unwrap();
        let issued = store.issue("评审A", 0).unwrap();
        assert!(issued.token.starts_with("lf_") && issued.token.len() >= 40);
        // 明文只有摘要入库：按明文可验证
        let info = store.verify_token(&issued.token).unwrap();
        assert_eq!(
            info.as_ref().map(|t| t.id.as_str()),
            Some(issued.id.as_str())
        );
        assert!(store.verify_token("lf_deadbeef").unwrap().is_none());
        assert!(store.revoke(&issued.id).unwrap());
        assert!(store.verify_token(&issued.token).unwrap().is_none());
        // 删除语义：行与会话一并清除，重复删除报 false
        assert!(store.remove(&issued.id).unwrap());
        assert!(!store.remove(&issued.id).unwrap());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn session_lifecycle_follows_token() {
        let db = temp_db("session");
        let store = TokenStore::open(&db).unwrap();
        let issued = store.issue("评审B", 0).unwrap();
        let sid = store.create_session(&issued.id).unwrap();
        assert_eq!(
            store.resolve_session(&sid).unwrap().as_deref(),
            Some(issued.id.as_str())
        );
        store.end_session(&sid).unwrap();
        assert!(store.resolve_session(&sid).unwrap().is_none());
        // 吊销令牌即踢掉其全部在途会话（开关即断）
        let sid2 = store.create_session(&issued.id).unwrap();
        assert!(store.revoke(&issued.id).unwrap());
        assert!(store.resolve_session(&sid2).unwrap().is_none());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn admin_cred_digest_only_and_lenient_missing() {
        let admin = AdminCred::from_token(Some("x".repeat(64)));
        assert!(admin.matches(&"x".repeat(64)));
        assert!(!admin.matches(&"x".repeat(63)));
        assert!(!admin.matches(""));
        let none = AdminCred::from_token(None);
        assert!(!none.matches("anything"));
        assert!(none.fingerprint().is_none(), "无密钥时不应回出指纹");
    }

    /// 轮换的底层语义：`swap` 立刻改判，指纹跟着摘要走，且 `Clone` 共享同一份摘要
    /// （路由扩展里那些已注入的 `GatewayConfig` 句柄必须一起认新钥匙）。
    #[test]
    fn admin_cred_swap_is_visible_through_clones() {
        let cred = AdminCred::from_token(Some("a".repeat(40)));
        let shared = cred.clone();
        let before = cred.fingerprint().unwrap();
        assert_eq!(before.len(), 8, "指纹取 SHA-256 前 4 字节 hex");
        cred.swap(&"b".repeat(40));
        assert_ne!(cred.fingerprint().unwrap(), before);
        assert!(cred.matches(&"b".repeat(40)) && !cred.matches(&"a".repeat(40)));
        assert!(
            shared.matches(&"b".repeat(40)),
            "已注入路由的旧句柄应同一时刻认新钥匙"
        );
    }

    #[test]
    fn gateway_on_requires_long_admin_token() {
        let db = temp_db("validate");
        let short = dotenv(&[
            ("LANGFIXER_GATEWAY", "1"),
            ("LANGFIXER_ADMIN_TOKEN", "short"),
        ]);
        assert!(matches!(
            GatewayConfig::build(&short, &db),
            Err(GatewayConfigError::MissingAdminToken)
        ));
        let long = dotenv(&[
            ("LANGFIXER_GATEWAY", "1"),
            ("LANGFIXER_ADMIN_TOKEN", &"k".repeat(64)),
            ("LANGFIXER_COOKIE_SECURE", "0"),
        ]);
        let cfg = GatewayConfig::build(&long, &db).unwrap();
        assert!(cfg.enabled && cfg.admin.matches(&"k".repeat(64)) && !cfg.cookie_secure);
        // 单机形态：不设开关 → 关闭鉴权，短/缺 admin 均不报错（行为零变化）
        let off = dotenv(&[]);
        let cfg = GatewayConfig::build(&off, &db).unwrap();
        assert!(!cfg.enabled && !cfg.cookie_secure);
        let _ = std::fs::remove_file(&db);
    }

    /// 验证机制①：安装时初始化超管密钥——env 未给出即自动生成并落 `gw_meta`，
    /// 二次启动复用同一枚；env 给出则以 env 为准。
    #[test]
    fn install_initializes_admin_token_once() {
        let db = temp_db("autogen");
        let empty = dotenv(&[]);
        let cfg = GatewayConfig::build(&empty, &db).unwrap();
        let generated = cfg
            .store
            .meta_get(META_ADMIN_TOKEN)
            .unwrap()
            .expect("首次安装应生成超管密钥");
        assert!(
            generated.starts_with("lf_") && generated.len() == 43,
            "{generated}"
        );
        assert!(cfg.admin.matches(&generated));
        assert_eq!(
            cfg.admin_source,
            AdminTokenSource::Generated,
            "env 未设时来源是 gw_meta（轮换端点接管）"
        );
        drop(cfg);
        // 重启复用：不漂移（否则用户手上的密钥会失效）
        let again = GatewayConfig::build(&empty, &db).unwrap();
        assert_eq!(
            again.store.meta_get(META_ADMIN_TOKEN).unwrap().as_deref(),
            Some(generated.as_str())
        );
        assert!(again.admin.matches(&generated));
        // env 优先于安装生成的那枚，且来源要如实标 env（轮换端点据此拒绝接管）
        let env_token = "k".repeat(64);
        let with_env = dotenv(&[("LANGFIXER_ADMIN_TOKEN", &env_token)]);
        let cfg = GatewayConfig::build(&with_env, &db).unwrap();
        assert!(cfg.admin.matches(&env_token) && !cfg.admin.matches(&generated));
        assert_eq!(cfg.admin_source, AdminTokenSource::Env);
        let _ = std::fs::remove_file(&db);
    }

    /// 超管会话用伪 id 入库（可被 resolve 认出为 admin）+ 单机 owner 激活标记可读写。
    #[test]
    fn admin_pseudo_session_and_owner_claim_marker() {
        let db = temp_db("admin-session");
        let store = TokenStore::open(&db).unwrap();
        let sid = store.create_session(ADMIN_TOKEN_ID).unwrap();
        assert_eq!(
            store.resolve_session(&sid).unwrap().as_deref(),
            Some(ADMIN_TOKEN_ID)
        );
        store.end_session(&sid).unwrap();
        assert!(store.resolve_session(&sid).unwrap().is_none());
        // 激活标记：缺省未激活，写一次即持久（此后单机设置写入要求登录态）
        assert!(store.meta_get(META_OWNER_CLAIMED).unwrap().is_none());
        store.meta_set(META_OWNER_CLAIMED, "1").unwrap();
        assert_eq!(
            store.meta_get(META_OWNER_CLAIMED).unwrap().as_deref(),
            Some("1")
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn session_owner_binding_is_fail_closed() {
        let db = temp_db("owner");
        let store = TokenStore::open(&db).unwrap();
        let a = store.issue("评审A", 0).unwrap();
        let b = store.issue("评审B", 0).unwrap();
        // 首次绑定 + 幂等重绑
        assert!(store.bind_session("s_1", &a.id).unwrap());
        assert!(store.bind_session("s_1", &a.id).unwrap());
        // 他人再绑即 false（fail-closed，handler 映射 403）
        assert!(!store.bind_session("s_1", &b.id).unwrap());
        assert_eq!(
            store.session_owner("s_1").unwrap().as_deref(),
            Some(a.id.as_str())
        );
        // 解绑后可重绑（会话删除路径的级联清理）
        store.unbind_session("s_1").unwrap();
        assert!(store.session_owner("s_1").unwrap().is_none());
        assert!(store.bind_session("s_1", &b.id).unwrap());
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn usage_accumulates_and_quota_reads_back() {
        let db = temp_db("usage");
        let store = TokenStore::open(&db).unwrap();
        let issued = store.issue("评审C", 100).unwrap();
        assert_eq!(store.quota(&issued.id).unwrap(), 100);
        assert_eq!(store.usage_today(&issued.id).unwrap(), 0);
        store.add_usage(&issued.id, 40).unwrap();
        store.add_usage(&issued.id, 30).unwrap();
        assert_eq!(store.usage_today(&issued.id).unwrap(), 70);
        // 未知令牌：配额按 0（不限）读回，不报错
        assert_eq!(store.quota("tok_missing").unwrap(), 0);
        let _ = std::fs::remove_file(&db);
    }

    /// 管理面用量视图：列表一次带出「今日 / 保留期内 / 天数 / 最近一天」，
    /// 吊销位与配额同表读出（界面「密钥分发」的用量列就是它）。
    #[test]
    fn list_with_usage_reports_per_token_ledger() {
        let db = temp_db("usage-list");
        let store = TokenStore::open(&db).unwrap();
        let a = store.issue("评审-用过", 200).unwrap();
        let b = store.issue("评审-没用过", 0).unwrap();
        store.add_usage(&a.id, 40).unwrap();
        store.add_usage(&a.id, 30).unwrap();
        store.revoke(&b.id).unwrap();

        let rows = store.list_with_usage().unwrap();
        assert_eq!(rows.len(), 2);
        let ra = rows.iter().find(|r| r.id == a.id).unwrap();
        assert_eq!((ra.used_today, ra.used_retained, ra.days_used), (70, 70, 1));
        assert!(ra.last_day.is_some(), "有记账就应读出最近一天");
        assert!(!ra.revoked);
        assert_eq!(ra.quota_chars, 200);
        let rb = rows.iter().find(|r| r.id == b.id).unwrap();
        assert_eq!((rb.used_today, rb.used_retained, rb.days_used), (0, 0, 0));
        assert_eq!(rb.last_day, None);
        assert!(rb.revoked);

        // 概览合计与逐令牌账同源
        assert_eq!(store.usage_totals().unwrap(), (70, 70));
        let _ = std::fs::remove_file(&db);
    }
}
