use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// Optional RunPod configuration. If present, use RunPod instead of AWS EC2.
    pub runpod: Option<RunpodConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExperimentConfig {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContainerConfig {
    /// Path to Dockerfile (relative to config file)
    pub dockerfile: Option<String>,
    /// Pre-built image URI (ECR)
    pub image: Option<String>,
    /// Build context directory (defaults to Dockerfile parent)
    pub context: Option<String>,
    /// Docker Hub repository for RunPod image push (e.g., "kurtbell87/mbo-dl")
    pub dockerhub_repo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstanceConfig {
    #[serde(rename = "type", default)]
    pub instance_type: String,
    #[serde(default = "default_region")]
    pub region: String,
    #[serde(default = "default_max_runtime")]
    pub max_runtime_minutes: u32,
    /// Use spot instances (default: true). Falls back to on-demand if spot unavailable.
    #[serde(default = "default_true")]
    pub spot: bool,
    /// Whether this workload needs GPU (determines AMI and docker --gpus flag)
    #[serde(default)]
    pub gpu: bool,
    /// Root EBS volume size in GB
    #[serde(default = "default_root_volume_gb")]
    pub root_volume_gb: u32,
    pub key_name: Option<String>,
    pub subnet_id: Option<String>,
    #[serde(default)]
    pub security_group_ids: Vec<String>,
    pub iam_profile: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DataSource {
    pub s3: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DataConfig {
    #[serde(default)]
    pub sources: Vec<DataSource>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunConfig {
    pub command: String,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatConfig {
    #[serde(default = "default_heartbeat_interval")]
    pub interval_seconds: u32,
    /// Terminate if no heartbeat received within this many minutes
    #[serde(default = "default_heartbeat_timeout")]
    pub timeout_minutes: u32,
    /// Terminate if CPU utilization stays below 5% for this many minutes
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_minutes: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_seconds: default_heartbeat_interval(),
            timeout_minutes: default_heartbeat_timeout(),
            idle_timeout_minutes: default_idle_timeout(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_region() -> String {
    "us-east-1".to_string()
}
fn default_max_runtime() -> u32 {
    120
}
fn default_root_volume_gb() -> u32 {
    80
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
fn default_idle_timeout() -> u32 {
    15
}
fn default_container_disk_gb() -> u32 {
    20
}
fn default_gpu_count() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunpodConfig {
    /// RunPod GPU type ID (e.g., "NVIDIA H200 SXM", "NVIDIA GeForce RTX 4090")
    pub gpu_type: String,
    /// Container disk size in GB (default: 20)
    #[serde(default = "default_container_disk_gb")]
    pub container_disk_gb: u32,
    /// Persistent volume size in GB (default: 0 = no volume)
    #[serde(default)]
    pub volume_gb: u32,
    /// RunPod datacenter ID (e.g., "US-TX-3"). Auto-resolved if omitted.
    pub datacenter: Option<String>,
    /// GPU count (default: 1)
    #[serde(default = "default_gpu_count")]
    pub gpu_count: u32,
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
        if self.runpod.is_none() && self.instance.instance_type.is_empty() {
            anyhow::bail!("instance.type is required for AWS backend");
        }
        if let Some(ref rp) = self.runpod {
            if rp.gpu_type.is_empty() {
                anyhow::bail!("runpod.gpu_type is required");
            }
            if self.container.dockerfile.is_some()
                && self.container.image.is_none()
                && self.container.dockerhub_repo.is_none()
            {
                anyhow::bail!(
                    "container.dockerhub_repo is required when using RunPod with a Dockerfile"
                );
            }
        }
        Ok(())
    }

    pub fn is_runpod(&self) -> bool {
        self.runpod.is_some()
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
