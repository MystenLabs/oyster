/// Pearl service configuration, read from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub service_secret: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("PEARL_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:pearl.db?mode=rwc".into()),
            bind_addr: std::env::var("PEARL_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".into()),
            service_secret: std::env::var("PEARL_SERVICE_SECRET")
                .unwrap_or_else(|_| "dev-secret".into()),
        }
    }
}
