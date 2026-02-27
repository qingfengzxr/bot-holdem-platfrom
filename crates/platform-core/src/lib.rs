use serde as _;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
};
use thiserror::Error;
use toml as _;

#[cfg(test)]
use serde_json as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEnv {
    Local,
    Dev,
    Test,
    Prod,
}

impl AppEnv {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Dev => "dev",
            Self::Test => "test",
            Self::Prod => "prod",
        }
    }
}

impl std::str::FromStr for AppEnv {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "dev" | "development" => Ok(Self::Dev),
            "test" => Ok(Self::Test),
            "prod" | "production" => Ok(Self::Prod),
            other => Err(ConfigError::InvalidEnv(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub app: AppSection,
    pub observability: ObservabilitySection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSection {
    pub env: AppEnv,
    pub service_name: String,
    pub rpc_bind_addr: String,
    pub ops_http_bind_addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilitySection {
    pub log_filter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<ErrorBody>,
}

impl<T> ResponseEnvelope<T> {
    #[must_use]
    pub fn ok(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    #[must_use]
    pub fn err(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    RequestInvalid,
    Forbidden,
    RoomListFailed,
    RoomCreateFailed,
    RoomReadyFailed,
    RoomBindAddressFailed,
    RoomBindKeysFailed,
    GameGetStateFailed,
    GameActFailed,
    KeyBindingProofInvalid,
    SubscribeFailed,
    UnsubscribeFailed,
    InternalError,
}

impl ErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestInvalid => "REQUEST_INVALID",
            Self::Forbidden => "FORBIDDEN",
            Self::RoomListFailed => "ROOM_LIST_FAILED",
            Self::RoomCreateFailed => "ROOM_CREATE_FAILED",
            Self::RoomReadyFailed => "ROOM_READY_FAILED",
            Self::RoomBindAddressFailed => "ROOM_BIND_ADDRESS_FAILED",
            Self::RoomBindKeysFailed => "ROOM_BIND_KEYS_FAILED",
            Self::GameGetStateFailed => "GAME_GET_STATE_FAILED",
            Self::GameActFailed => "GAME_ACT_FAILED",
            Self::KeyBindingProofInvalid => "KEY_BINDING_PROOF_INVALID",
            Self::SubscribeFailed => "SUBSCRIBE_FAILED",
            Self::UnsubscribeFailed => "UNSUBSCRIBE_FAILED",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid APP_ENV value: {0}")]
    InvalidEnv(String),
    #[error("unable to locate config directory (expected config/default.toml)")]
    ConfigDirNotFound,
    #[error("failed reading config file {path}: {source}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed parsing config file {path}: {source}")]
    ParseToml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

#[derive(Debug, Default, Deserialize)]
struct PartialAppConfig {
    app: Option<PartialAppSection>,
    observability: Option<PartialObservabilitySection>,
}

#[derive(Debug, Default, Deserialize)]
struct PartialAppSection {
    env: Option<AppEnv>,
    service_name: Option<String>,
    rpc_bind_addr: Option<String>,
    ops_http_bind_addr: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PartialObservabilitySection {
    log_filter: Option<String>,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let app_env = env::var("APP_ENV")
            .ok()
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(AppEnv::Local);
        let config_dir = resolve_config_dir()?;
        Self::load_from_dir_for_env(config_dir, app_env)
    }

    pub fn load_from_dir_for_env(
        config_dir: impl AsRef<Path>,
        app_env: AppEnv,
    ) -> Result<Self, ConfigError> {
        let config_dir = config_dir.as_ref();
        let mut config = Self::default_for_env(app_env);
        merge_file(&mut config, &config_dir.join("default.toml"))?;
        merge_file(
            &mut config,
            &config_dir.join(format!("{}.toml", app_env.as_str())),
        )?;
        config.app.env = app_env;
        config.apply_env_overrides()?;
        Ok(config)
    }

    #[must_use]
    pub fn default_for_env(app_env: AppEnv) -> Self {
        Self {
            app: AppSection {
                env: app_env,
                service_name: "app-server".to_string(),
                rpc_bind_addr: "127.0.0.1:9000".to_string(),
                ops_http_bind_addr: "127.0.0.1:9100".to_string(),
            },
            observability: ObservabilitySection {
                log_filter: "info".to_string(),
            },
        }
    }

    fn apply_env_overrides(&mut self) -> Result<(), ConfigError> {
        if let Ok(raw_env) = env::var("APP_ENV") {
            self.app.env = raw_env.parse()?;
        }
        if let Ok(service_name) = env::var("APP_SERVER__SERVICE_NAME") {
            self.app.service_name = service_name;
        }
        if let Ok(bind_addr) = env::var("APP_SERVER__RPC_BIND_ADDR") {
            self.app.rpc_bind_addr = bind_addr;
        }
        if let Ok(bind_addr) = env::var("APP_SERVER__OPS_HTTP_BIND_ADDR") {
            self.app.ops_http_bind_addr = bind_addr;
        }
        if let Ok(log_filter) = env::var("OBSERVABILITY__LOG_FILTER") {
            self.observability.log_filter = log_filter;
        } else if let Ok(log_filter) = env::var("RUST_LOG") {
            self.observability.log_filter = log_filter;
        }
        Ok(())
    }

    fn merge_partial(&mut self, partial: PartialAppConfig) {
        if let Some(app) = partial.app {
            if let Some(value) = app.env {
                self.app.env = value;
            }
            if let Some(value) = app.service_name {
                self.app.service_name = value;
            }
            if let Some(value) = app.rpc_bind_addr {
                self.app.rpc_bind_addr = value;
            }
            if let Some(value) = app.ops_http_bind_addr {
                self.app.ops_http_bind_addr = value;
            }
        }
        if let Some(observability) = partial.observability {
            if let Some(value) = observability.log_filter {
                self.observability.log_filter = value;
            }
        }
    }
}

fn merge_file(config: &mut AppConfig, path: &Path) -> Result<(), ConfigError> {
    let content = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
        path: path.display().to_string(),
        source,
    })?;
    let partial =
        toml::from_str::<PartialAppConfig>(&content).map_err(|source| ConfigError::ParseToml {
            path: path.display().to_string(),
            source,
        })?;
    config.merge_partial(partial);
    Ok(())
}

fn resolve_config_dir() -> Result<PathBuf, ConfigError> {
    if let Ok(path) = env::var("BOT_PLATFORM_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }

    let mut current_dir = env::current_dir().map_err(|_| ConfigError::ConfigDirNotFound)?;
    loop {
        let candidate = current_dir.join("config");
        if candidate.join("default.toml").exists() {
            return Ok(candidate);
        }
        if !current_dir.pop() {
            break;
        }
    }

    Err(ConfigError::ConfigDirNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn response_envelope_serializes_error_code_as_string() {
        let response: ResponseEnvelope<()> =
            ResponseEnvelope::err(ErrorCode::RequestInvalid, "bad");
        let json = serde_json::to_string(&response).expect("serialize");
        assert!(json.contains("\"REQUEST_INVALID\""));
        assert!(json.contains("\"error\""));
    }

    #[test]
    fn config_loader_merges_default_and_env_files() {
        let base_dir = std::env::temp_dir().join(format!(
            "platform-core-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&base_dir).expect("create temp dir");
        std::fs::write(
            base_dir.join("default.toml"),
            r#"
[app]
service_name = "default-service"
rpc_bind_addr = "127.0.0.1:9000"
ops_http_bind_addr = "127.0.0.1:9100"

[observability]
log_filter = "info"
"#,
        )
        .expect("write default.toml");
        std::fs::write(
            base_dir.join("dev.toml"),
            r#"
[app]
service_name = "dev-service"
rpc_bind_addr = "0.0.0.0:9000"

[observability]
log_filter = "debug"
"#,
        )
        .expect("write dev.toml");

        let config = AppConfig::load_from_dir_for_env(&base_dir, AppEnv::Dev).expect("load config");
        let expected_log_filter = std::env::var("OBSERVABILITY__LOG_FILTER")
            .ok()
            .or_else(|| std::env::var("RUST_LOG").ok())
            .unwrap_or_else(|| "debug".to_string());
        assert_eq!(config.app.env, AppEnv::Dev);
        assert_eq!(config.app.service_name, "dev-service");
        assert_eq!(config.app.rpc_bind_addr, "0.0.0.0:9000");
        assert_eq!(config.app.ops_http_bind_addr, "127.0.0.1:9100");
        assert_eq!(config.observability.log_filter, expected_log_filter);
    }
}
