//! 网关形态身份层（评测通道设计，见 `docs/GATEWAY_AUTH.md`）
//!
//! 原则：
//! - **单机形态零变化**：`LANGFIXER_GATEWAY` 未开启时全部端点保持原有免鉴权行为；
//! - **一张令牌表 = 唯一身份原语**：web 会话与未来 MCP 面共用同一 Bearer 校验与配额；
//! - **超级管理员令牌只活在上游进程环境 / `.env`**：内存里只留 SHA-256 摘要，
//!   绝不写入数据库，也不是可签发的评审令牌——`/admin/*` 只认它。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
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
#[derive(Clone)]
pub struct AdminCred {
    digest: Option<[u8; 32]>,
}

impl AdminCred {
    pub fn from_token(token: Option<String>) -> Self {
        Self {
            digest: token.filter(|t| !t.is_empty()).map(|t| sha256(&t)),
        }
    }

    pub fn matches(&self, presented: &str) -> bool {
        match &self.digest {
            Some(expected) => digest_eq(expected, &sha256(presented)),
            None => false,
        }
    }
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

/// 评审令牌 + 会话存储（独立 `auth.db`，与产品数据 `langfixer.db` 分离）。
pub struct TokenStore {
    conn: Mutex<rusqlite::Connection>,
}

impl TokenStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
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
             CREATE TABLE IF NOT EXISTS gw_meta(
                  key   TEXT PRIMARY KEY,
                  value TEXT NOT NULL
              );
              CREATE INDEX IF NOT EXISTS idx_tokens_hash ON access_tokens(hash);",
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
        let enabled = lookup(dotenv, "LANGFIXER_GATEWAY").is_some_and(|v| v == "1");
        let env_admin = lookup(dotenv, "LANGFIXER_ADMIN_TOKEN");
        if env_admin.as_deref().is_some_and(|t| t.len() < 32) {
            return Err(GatewayConfigError::MissingAdminToken);
        }
        let store = TokenStore::open(db_path).map_err(GatewayConfigError::Store)?;
        let admin_token = match env_admin {
            Some(t) => Some(t),
            None => match store
                .meta_get(META_ADMIN_TOKEN)
                .map_err(GatewayConfigError::Store)?
            {
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
        Ok(Self {
            enabled,
            admin: AdminCred::from_token(admin_token),
            store,
            cookie_secure,
        })
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        drop(cfg);
        // 重启复用：不漂移（否则用户手上的密钥会失效）
        let again = GatewayConfig::build(&empty, &db).unwrap();
        assert_eq!(
            again.store.meta_get(META_ADMIN_TOKEN).unwrap().as_deref(),
            Some(generated.as_str())
        );
        assert!(again.admin.matches(&generated));
        // env 优先于安装生成的那枚
        let env_token = "k".repeat(64);
        let with_env = dotenv(&[("LANGFIXER_ADMIN_TOKEN", &env_token)]);
        let cfg = GatewayConfig::build(&with_env, &db).unwrap();
        assert!(cfg.admin.matches(&env_token) && !cfg.admin.matches(&generated));
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
}
