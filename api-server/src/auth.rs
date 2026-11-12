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
    /// 校验语义单独成函数便于测试：网关开启必须给 ≥32 字符超管令牌。
    pub fn build(
        dotenv: &HashMap<String, String>,
        db_path: &Path,
    ) -> Result<Self, GatewayConfigError> {
        let enabled = lookup(dotenv, "LANGFIXER_GATEWAY").is_some_and(|v| v == "1");
        let admin_token = lookup(dotenv, "LANGFIXER_ADMIN_TOKEN");
        if enabled && !admin_token.as_deref().is_some_and(|t| t.len() >= 32) {
            return Err(GatewayConfigError::MissingAdminToken);
        }
        let store = TokenStore::open(db_path).map_err(GatewayConfigError::Store)?;
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
            Ok(cfg) => cfg,
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
}
