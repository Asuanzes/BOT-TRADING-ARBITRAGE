use crate::types::RawEvent;
use anyhow::Result;
use chrono::{DateTime, Utc};
use flate2::{write::GzEncoder, Compression};
use std::{
    collections::HashMap,
    fs::{create_dir_all, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{error, info};

type GzWriter = GzEncoder<BufWriter<File>>;

struct FileSlot {
    hour_bucket: String,
    writer: GzWriter,
}

pub fn spawn(base_dir: PathBuf) -> mpsc::Sender<RawEvent> {
    let (tx, rx) = mpsc::channel::<RawEvent>(16384);
    std::thread::spawn(move || {
        if let Err(e) = writer_loop(base_dir, rx) {
            error!("writer loop crashed: {e}");
        }
    });
    tx
}

fn writer_loop(base: PathBuf, mut rx: mpsc::Receiver<RawEvent>) -> Result<()> {
    let mut files: HashMap<&'static str, FileSlot> = HashMap::new();
    let mut last_flush = Instant::now();
    let mut events_written = 0u64;
    let mut last_stats = Instant::now();

    while let Some(ev) = rx.blocking_recv() {
        let ts = DateTime::<Utc>::from_timestamp_nanos(ev.ts_recv_ns);
        let date = ts.format("%Y-%m-%d").to_string();
        let hour = ts.format("%H").to_string();
        let hour_bucket = format!("{date}-{hour}");

        let needs_new = match files.get(ev.feed) {
            Some(slot) => slot.hour_bucket != hour_bucket,
            None => true,
        };

        if needs_new {
            if let Some(mut old) = files.remove(ev.feed) {
                info!("rotating {} (was {}, now {})", ev.feed, old.hour_bucket, hour_bucket);
                old.writer.try_finish().ok();
            }
            let slot = open_slot(&base, ev.feed, &date, &hour)?;
            files.insert(ev.feed, slot);
        }

        let slot = files.get_mut(ev.feed).unwrap();
        let line = serde_json::to_vec(&serde_json::json!({"feed": ev.feed, "stream": ev.stream, "ts_recv_ns": ev.ts_recv_ns, "payload": ev.payload}))?;
        slot.writer.write_all(&line)?;
        slot.writer.write_all(b"\n")?;
        events_written += 1;

        if last_flush.elapsed() >= Duration::from_secs(5) {
            for slot in files.values_mut() {
                slot.writer.flush().ok();
            }
            last_flush = Instant::now();
        }

        if last_stats.elapsed() >= Duration::from_secs(30) {
            info!("writer: {} events (cumulative)", events_written);
            last_stats = Instant::now();
        }
    }

    info!("writer draining");
    for (_, mut slot) in files.drain() {
        slot.writer.try_finish().ok();
    }
    Ok(())
}

fn open_slot(base: &Path, feed: &str, date: &str, hour: &str) -> Result<FileSlot> {
    let dir = base.join(feed).join(date);
    create_dir_all(&dir)?;
    let path = dir.join(format!("{hour}.ndjson.gz"));
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    let buf = BufWriter::new(file);
    let gz = GzEncoder::new(buf, Compression::default());
    Ok(FileSlot {
        hour_bucket: format!("{date}-{hour}"),
        writer: gz,
    })
}
