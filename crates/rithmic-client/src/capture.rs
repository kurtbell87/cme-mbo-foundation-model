//! S3 capture uploader: batches CaptureRecords → gzipped JSONL → S3.
//!
//! Records are flushed on batch size (10k) or time interval (60s).
//! Upload failures are retried once; on double failure the batch is dropped
//! (data capture must never block the trading pipeline).

use std::io::Write;
use std::time::Duration;

use base64::Engine;
use flate2::write::GzEncoder;
use flate2::Compression;
use tokio::sync::mpsc;

use crate::dispatcher::CaptureRecord;
use crate::error::RithmicError;

const BATCH_SIZE: usize = 10_000;
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Run the S3 capture uploader task.
///
/// Receives CaptureRecords, batches them, compresses as gzipped JSONL,
/// and uploads to S3. Returns when the channel closes (clean shutdown).
pub async fn run_capture_uploader(
    mut rx: mpsc::Receiver<CaptureRecord>,
    s3_bucket: String,
    symbol: String,
) -> Result<(), RithmicError> {
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3_client = aws_sdk_s3::Client::new(&sdk_config);

    let mut batch: Vec<CaptureRecord> = Vec::with_capacity(BATCH_SIZE);
    let mut interval = tokio::time::interval(FLUSH_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seq: u64 = 0;
    let mut dropped_batches: u64 = 0;

    loop {
        tokio::select! {
            record = rx.recv() => {
                match record {
                    Some(r) => {
                        batch.push(r);
                        if batch.len() >= BATCH_SIZE {
                            if let Err(e) = flush_batch(&s3_client, &s3_bucket, &symbol, &mut batch, &mut seq).await {
                                dropped_batches += 1;
                                eprintln!("[capture] batch dropped ({dropped_batches} total): {e}");
                                batch.clear();
                            }
                        }
                    }
                    None => {
                        // Channel closed — flush remaining
                        if !batch.is_empty() {
                            if let Err(e) = flush_batch(&s3_client, &s3_bucket, &symbol, &mut batch, &mut seq).await {
                                dropped_batches += 1;
                                eprintln!("[capture] final batch dropped ({dropped_batches} total): {e}");
                            }
                        }
                        eprintln!("[capture] channel closed, uploaded {seq} batches, dropped {dropped_batches}");
                        return Ok(());
                    }
                }
            }
            _ = interval.tick() => {
                if !batch.is_empty() {
                    if let Err(e) = flush_batch(&s3_client, &s3_bucket, &symbol, &mut batch, &mut seq).await {
                        dropped_batches += 1;
                        eprintln!("[capture] timed batch dropped ({dropped_batches} total): {e}");
                        batch.clear();
                    }
                }
            }
        }
    }
}

/// Compress batch as gzipped JSONL and upload to S3. Retry once on failure.
async fn flush_batch(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    symbol: &str,
    batch: &mut Vec<CaptureRecord>,
    seq: &mut u64,
) -> Result<(), String> {
    let body = compress_batch(batch).map_err(|e| format!("compress: {e}"))?;
    let records = batch.len();
    batch.clear();

    let now = chrono::Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let ts = now.format("%Y%m%d-%H%M%S").to_string();
    let key = format!("raw/{symbol}/{date}/{ts}_{seq}.jsonl.gz");

    *seq += 1;

    // Try upload, retry once on failure
    match upload_to_s3(s3, bucket, &key, body.clone()).await {
        Ok(()) => {
            eprintln!("[capture] uploaded {records} records to s3://{bucket}/{key}");
            Ok(())
        }
        Err(e1) => {
            eprintln!("[capture] upload failed, retrying: {e1}");
            match upload_to_s3(s3, bucket, &key, body).await {
                Ok(()) => {
                    eprintln!("[capture] retry succeeded: s3://{bucket}/{key}");
                    Ok(())
                }
                Err(e2) => Err(format!("upload failed twice: {e1}; {e2}")),
            }
        }
    }
}

fn compress_batch(batch: &[CaptureRecord]) -> Result<Vec<u8>, std::io::Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let b64 = base64::engine::general_purpose::STANDARD;

    for record in batch {
        let line = serde_json::json!({
            "template_id": record.template_id,
            "sequence_number": record.sequence_number,
            "exchange_ts_ns": record.exchange_ts_ns,
            "gateway_ts_ns": record.gateway_ts_ns,
            "receive_ns": record.receive_ns,
            "symbol": record.symbol,
            "raw_bytes": b64.encode(&record.raw_bytes),
        });
        serde_json::to_writer(&mut encoder, &line)?;
        encoder.write_all(b"\n")?;
    }

    encoder.finish()
}

async fn upload_to_s3(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    body: Vec<u8>,
) -> Result<(), String> {
    s3.put_object()
        .bucket(bucket)
        .key(key)
        .body(body.into())
        .content_type("application/gzip")
        .content_encoding("gzip")
        .send()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}
