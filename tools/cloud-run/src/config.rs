use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub experiment: ExperimentConfig,
    pub container: ContainerConfig,
    pub instance: InstanceConfig,
    #[serde(default)]
    pub data: DataConfig,
    #[serde(default)]
    pub results: ResultsConfig,
    pub run: RunConfig,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExperimentConfig {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ContainerConfig {
    /// Path to Dockerfile (relative to config file)
    pub dockerfile: Option<String>,
    /// Pre-built image URI (ECR)
    pub image: Option<String>,
    /// Build context directory (defaults to Dockerfile parent)
    pub context: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InstanceConfig {
    #[serde(rename = "type")]
    pub instance_type: String,
    #[serde(default = "default_region")]
    pub region: String,
    #[serde(default = "default_max_runtime")]
    pub max_runtime_minutes: u32,
    #[serde(default)]
    pub spot: bool,
    pub key_name: Option<String>,
    pub subnet_id: Option<String>,
    #[serde(default)]
    pub security_group_ids: Vec<String>,
    pub iam_profile: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DataSource {
    pub s3: String,
    pub path: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct DataConfig {
    #[serde(default)]
    pub sources: Vec<DataSource>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ResultsConfig {
    #[serde(default = "default_s3_prefix")]
    pub s3_prefix: String,
}

impl Default for ResultsConfig {
    fn default() -> Self {
        Self {
            s3_prefix: default_s3_prefix(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunConfig {
    pub command: String,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HeartbeatConfig {
    #[serde(default = "default_heartbeat_interval")]
    pub interval_seconds: u32,
    #[serde(default = "default_heartbeat_timeout")]
    pub timeout_minutes: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_seconds: default_heartbeat_interval(),
            timeout_minutes: default_heartbeat_timeout(),
        }
    }
}

fn default_region() -> String {
    "us-east-1".to_string()
}
fn default_max_runtime() -> u32 {
    120
}
fn default_s3_prefix() -> String {
    "s3://kenoma-labs-research/runs".to_string()
}
fn default_heartbeat_interval() -> u32 {
    60
}
fn default_heartbeat_timeout() -> u32 {
    10
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse cloud-run.toml")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.container.dockerfile.is_none() && self.container.image.is_none() {
            anyhow::bail!("container: must specify either 'dockerfile' or 'image'");
        }
        if self.instance.instance_type.is_empty() {
            anyhow::bail!("instance.type is required");
        }
        Ok(())
    }

    /// Resolve the ECR image URI — either from config or derived from experiment name
    pub fn ecr_image(&self, account_id: &str) -> String {
        if let Some(ref image) = self.container.image {
            return image.clone();
        }
        format!(
            "{}.dkr.ecr.{}.amazonaws.com/kenoma/experiments:{}",
            account_id, self.instance.region, self.experiment.name
        )
    }
}
