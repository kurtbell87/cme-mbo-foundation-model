use anyhow::{Context, Result};
use std::process::Command;

/// Resolve RunPod API key from env var or ~/.runpod/config.toml.
pub fn resolve_api_key() -> Result<String> {
    if let Ok(key) = std::env::var("RUNPOD_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        let config_path = format!("{}/.runpod/config.toml", home);
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("apikey") || trimmed.starts_with("apiKey") {
                    if let Some(val) = trimmed.split('=').nth(1) {
                        let key = val.trim().trim_matches('"').trim_matches('\'');
                        if !key.is_empty() {
                            return Ok(key.to_string());
                        }
                    }
                }
            }
        }
    }

    anyhow::bail!(
        "RUNPOD_API_KEY not set. Set via env var or run: runpodctl config --apiKey YOUR_KEY"
    )
}

/// Call RunPod GraphQL API via curl.
fn graphql(api_key: &str, query: &str) -> Result<serde_json::Value> {
    let payload = serde_json::json!({"query": query}).to_string();
    let output = Command::new("curl")
        .args([
            "-s",
            "-X",
            "POST",
            "https://api.runpod.io/graphql",
            "-H",
            &format!("Authorization: Bearer {}", api_key),
            "-H",
            "Content-Type: application/json",
            "-d",
            &payload,
        ])
        .output()
        .context("Failed to call RunPod API (is curl installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("RunPod API call failed: {}", stderr);
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let response: serde_json::Value =
        serde_json::from_str(&body).context("Failed to parse RunPod API response")?;

    if let Some(errors) = response.get("errors") {
        anyhow::bail!("RunPod API error: {}", errors);
    }

    Ok(response)
}

/// Resolve AWS credentials from env vars or AWS CLI.
/// Returns (access_key_id, secret_access_key, optional session_token).
pub fn get_aws_credentials() -> Result<(String, String, Option<String>)> {
    if let (Ok(access), Ok(secret)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        if !access.is_empty() && !secret.is_empty() {
            return Ok((access, secret, std::env::var("AWS_SESSION_TOKEN").ok()));
        }
    }

    // Fall back to AWS CLI credential export
    let output = Command::new("aws")
        .args([
            "configure",
            "export-credentials",
            "--format",
            "env-no-export",
        ])
        .output()
        .context("Failed to export AWS credentials")?;

    if !output.status.success() {
        anyhow::bail!(
            "Cannot resolve AWS credentials for RunPod S3 access. \
             Set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or configure AWS CLI."
        );
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let mut access = String::new();
    let mut secret = String::new();
    let mut token = None;

    for line in body.lines() {
        if let Some(val) = line.strip_prefix("AWS_ACCESS_KEY_ID=") {
            access = val.to_string();
        } else if let Some(val) = line.strip_prefix("AWS_SECRET_ACCESS_KEY=") {
            secret = val.to_string();
        } else if let Some(val) = line.strip_prefix("AWS_SESSION_TOKEN=") {
            token = Some(val.to_string());
        }
    }

    if access.is_empty() || secret.is_empty() {
        anyhow::bail!("Cannot resolve AWS credentials");
    }

    Ok((access, secret, token))
}

/// Create a RunPod pod. Returns the pod ID.
pub fn create_pod(
    api_key: &str,
    name: &str,
    image: &str,
    gpu_type: &str,
    gpu_count: u32,
    datacenter: &str,
    container_disk_gb: u32,
    volume_gb: u32,
    docker_args: &str,
    env_vars: &[(String, String)],
) -> Result<String> {
    let env_json: Vec<String> = env_vars
        .iter()
        .map(|(k, v)| {
            let escaped_v = v.replace('\\', "\\\\").replace('"', "\\\"");
            format!("{{key: \\\"{}\\\", value: \\\"{}\\\"}}", k, escaped_v)
        })
        .collect();
    let env_str = env_json.join(", ");

    // Escape docker_args for GraphQL string
    let docker_args_escaped = docker_args.replace('\\', "\\\\").replace('"', "\\\"");

    let query = format!(
        r#"mutation {{
  podFindAndDeployOnDemand(input: {{
    name: "{name}"
    imageName: "{image}"
    gpuTypeId: "{gpu_type}"
    gpuCount: {gpu_count}
    cloudType: "SECURE"
    dataCenterId: "{datacenter}"
    containerDiskInGb: {container_disk_gb}
    volumeInGb: {volume_gb}
    volumeMountPath: "/workspace"
    dockerArgs: "{docker_args_escaped}"
    env: [{env_str}]
  }}) {{
    id
    desiredStatus
    costPerHr
    machine {{
      gpuDisplayName
    }}
  }}
}}"#
    );

    let resp = graphql(api_key, &query)?;
    let pod = &resp["data"]["podFindAndDeployOnDemand"];

    let pod_id = pod["id"]
        .as_str()
        .context("RunPod API did not return a pod ID")?
        .to_string();

    if let Some(cost) = pod["costPerHr"].as_f64() {
        eprintln!("  Cost: ${:.2}/hr", cost);
    }
    if let Some(gpu) = pod["machine"]["gpuDisplayName"].as_str() {
        eprintln!("  GPU: {}", gpu);
    }

    Ok(pod_id)
}

/// Wait for a RunPod pod to be running.
pub fn wait_ready(api_key: &str, pod_id: &str, timeout_secs: u64) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed().as_secs() > timeout_secs {
            anyhow::bail!("Pod {} not ready after {}s", pod_id, timeout_secs);
        }

        let query = format!(
            r#"query {{ pod(input: {{podId: "{}"}}) {{ id desiredStatus runtime {{ uptimeInSeconds }} }} }}"#,
            pod_id
        );

        if let Ok(resp) = graphql(api_key, &query) {
            let pod = &resp["data"]["pod"];
            let desired = pod["desiredStatus"].as_str().unwrap_or("");

            if desired == "EXITED" {
                return Ok(()); // Pod finished quickly
            }

            if let Some(runtime) = pod["runtime"].as_object() {
                if let Some(uptime) = runtime.get("uptimeInSeconds") {
                    if uptime.as_f64().unwrap_or(0.0) > 0.0 {
                        return Ok(());
                    }
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

/// Check if a RunPod pod is still running.
pub fn is_pod_running(api_key: &str, pod_id: &str) -> bool {
    let query = format!(
        r#"query {{ pod(input: {{podId: "{}"}}) {{ id desiredStatus runtime {{ uptimeInSeconds }} }} }}"#,
        pod_id
    );

    match graphql(api_key, &query) {
        Ok(resp) => {
            let pod = &resp["data"]["pod"];
            let desired = pod["desiredStatus"].as_str().unwrap_or("");
            desired == "RUNNING"
        }
        Err(_) => false,
    }
}

/// Terminate (permanently delete) a RunPod pod.
pub fn terminate_pod(api_key: &str, pod_id: &str) -> Result<()> {
    let query = format!(
        r#"mutation {{ podTerminate(input: {{podId: "{}"}}) }}"#,
        pod_id
    );
    graphql(api_key, &query)?;
    Ok(())
}

/// Stop a RunPod pod (can be resumed later, but we use this for graceful shutdown).
#[allow(dead_code)]
pub fn stop_pod(api_key: &str, pod_id: &str) -> Result<()> {
    let query = format!(
        r#"mutation {{ podStop(input: {{podId: "{}"}}) {{ id desiredStatus }} }}"#,
        pod_id
    );
    graphql(api_key, &query)?;
    Ok(())
}

/// Find a RunPod pod by name prefix matching the run_id.
pub fn find_pod_by_run_id(api_key: &str, run_id: &str) -> Result<Option<String>> {
    let query = r#"query { myself { pods { id name desiredStatus } } }"#;
    let resp = graphql(api_key, query)?;

    if let Some(pods) = resp["data"]["myself"]["pods"].as_array() {
        for pod in pods {
            let name = pod["name"].as_str().unwrap_or("");
            // Pod names are "cr-{run_id}" (potentially truncated)
            if name.starts_with("cr-") && run_id.starts_with(&name[3..]) {
                let status = pod["desiredStatus"].as_str().unwrap_or("");
                if status == "RUNNING" || status == "EXITED" {
                    return Ok(pod["id"].as_str().map(|s| s.to_string()));
                }
            }
        }
    }

    Ok(None)
}

/// List all cloud-run RunPod pods.
pub fn list_pods(api_key: &str) -> Result<()> {
    let query = r#"query { myself { pods { id name desiredStatus runtime { uptimeInSeconds } costPerHr machine { gpuDisplayName } } } }"#;
    let resp = graphql(api_key, query)?;

    if let Some(pods) = resp["data"]["myself"]["pods"].as_array() {
        let cr_pods: Vec<_> = pods
            .iter()
            .filter(|p| {
                p["name"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("cr-")
            })
            .collect();

        if cr_pods.is_empty() {
            eprintln!("  No cloud-run pods found on RunPod.");
            return Ok(());
        }

        eprintln!(
            "  {:<16} {:<20} {:<12} {:<10} {}",
            "Pod ID", "GPU", "Status", "Cost/hr", "Name"
        );
        eprintln!("  {}", "-".repeat(70));

        for pod in cr_pods {
            let id = pod["id"].as_str().unwrap_or("?");
            let name = pod["name"].as_str().unwrap_or("?");
            let status = pod["desiredStatus"].as_str().unwrap_or("?");
            let gpu = pod["machine"]["gpuDisplayName"]
                .as_str()
                .unwrap_or("?");
            let cost = pod["costPerHr"]
                .as_f64()
                .map(|c| format!("${:.2}", c))
                .unwrap_or_else(|| "?".to_string());

            eprintln!("  {:<16} {:<20} {:<12} {:<10} {}", id, gpu, status, cost, name);
        }
    }

    Ok(())
}

/// Resolve the best datacenter for a GPU type by querying RunPod stock.
/// Prefers US datacenters, then Canada, then any available.
pub fn resolve_datacenter(api_key: &str, gpu_type: &str) -> String {
    let default = "US-TX-3".to_string();

    let query =
        r#"{ dataCenters { id name location gpuAvailability { gpuTypeId stockStatus } } }"#;

    let resp = match graphql(api_key, query) {
        Ok(r) => r,
        Err(_) => return default,
    };

    let mut candidates = vec![];
    if let Some(dcs) = resp["data"]["dataCenters"].as_array() {
        for dc in dcs {
            if let Some(gpus) = dc["gpuAvailability"].as_array() {
                for gpu in gpus {
                    if gpu["gpuTypeId"].as_str() == Some(gpu_type) {
                        let stock = gpu["stockStatus"].as_str().unwrap_or("");
                        if stock == "High" || stock == "Medium" || stock == "Low" {
                            if let Some(id) = dc["id"].as_str() {
                                candidates.push(id.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        return default;
    }

    // Prefer US, then CA, then anything
    if let Some(us) = candidates.iter().find(|c| c.starts_with("US-")) {
        return us.clone();
    }
    if let Some(ca) = candidates.iter().find(|c| c.starts_with("CA-")) {
        return ca.clone();
    }
    candidates[0].clone()
}

/// Garbage-collect orphaned cloud-run pods (EXITED status).
pub fn cleanup_pods(api_key: &str) -> Result<Vec<String>> {
    let query = r#"query { myself { pods { id name desiredStatus } } }"#;
    let resp = graphql(api_key, query)?;
    let mut cleaned = vec![];

    if let Some(pods) = resp["data"]["myself"]["pods"].as_array() {
        for pod in pods {
            let name = pod["name"].as_str().unwrap_or("");
            let status = pod["desiredStatus"].as_str().unwrap_or("");

            if name.starts_with("cr-") && status == "EXITED" {
                let pod_id = pod["id"].as_str().unwrap_or("");
                if !pod_id.is_empty() {
                    if terminate_pod(api_key, pod_id).is_ok() {
                        cleaned.push(format!("{} ({})", pod_id, name));
                    }
                }
            }
        }
    }

    Ok(cleaned)
}
