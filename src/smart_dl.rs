use reqwest::{Client, header::RANGE};
use tokio::{
    fs::{File, remove_file},
    io::AsyncWriteExt,
    sync::{mpsc, Semaphore, Notify},
    signal,
};
use futures_util::StreamExt;
use futures::future::join_all;
use std::sync::Arc;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use std::{time::Duration};
use uuid;
use clap::Parser;
use serde::Deserialize;
use tokio::fs;
use std::path::{PathBuf, Path};
use tokio::io::AsyncReadExt;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::config::AppContext;
use crate::async_taskwait::AsyncTaskWait;
use crate::async_taskwait;

static CANCELLED: AtomicBool = AtomicBool::new(false);

pub async fn smart_download(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    chunk_count: usize,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let head = client.head(url).send().await?;
    let range_ok = head.headers().get("accept-ranges")
        .map_or(false, |v| v == "bytes");

    let total_size = head.headers().get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
	
	let client2 = client.clone();
	let url2 = url.to_string();
	let filename2 = filename.to_string();
	
	_ = context.taskwait.wait_for_slot();
	let handle = tokio::spawn(async move {
		let result = if range_ok && total_size.is_some() {
			download_in_chunks(context.clone(), &client2.clone(), url2.as_str(), filename2.as_str(), total_size.unwrap(), chunk_count, cancel_notify, multi.clone()).await
		} else {
			download_whole_with_progress(context.clone(), &client2.clone(), url2.as_str(), filename2.as_str(), cancel_notify.clone(), multi.clone()).await
		};
	});
	async_taskwait::add_task_handle(handle);
	
	Ok(())
}

pub async fn download_whole_with_progress(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = client.get(url).send().await?;
    let total = response.content_length().unwrap_or(0);

    let bar = multi.add(ProgressBar::new(total));
    bar.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("#>-"));

    let mut stream = response.bytes_stream();
    let mut file = File::create(filename).await?;

    tokio::select! {
        _ = cancel_notify.notified() => {
            println!("🛑 Cancelled while downloading full: {}", url);
            drop(file);
            let _ = remove_file(filename).await;
            bar.finish_and_clear();
            return Ok(());
        }

        _ = async {
            while let Some(chunk) = stream.next().await {
                let data = chunk?;
                file.write_all(&data).await?;
                bar.inc(data.len() as u64);
            }
            bar.finish_with_message("Done");
			bar.finish_and_clear();
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        } => {}
    }

    Ok(())
}

fn temp_chunk_path(base: &str, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.tmp{}", base, index))
}

pub async fn download_in_chunks(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    total_size: u64,
    chunk_count: usize,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let chunk_size = total_size / chunk_count as u64;
	let mut tmp_paths = Vec::new();

	let pb = Arc::new(multi.add(ProgressBar::new(total_size)));
	pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("#>-"));

	// Handle Ctrl+C signal
	let signal_set = Arc::new(cancel_notify.clone());
	let tmp_dir = PathBuf::from(filename)
    .parent()
    .map(Path::to_path_buf)
    .unwrap_or_else(|| PathBuf::from("."));
	let tmp_filename_base = PathBuf::from(filename).file_name().unwrap().to_str().unwrap().to_string();

	let tmp_cleanup = tmp_paths.clone();
	tokio::spawn({
		let cancel_notify = cancel_notify.clone();
		let tmp_filename_base = tmp_filename_base.clone();
		let tmp_dir = tmp_dir.clone();
		async move {
			signal::ctrl_c().await.ok();
			CANCELLED.store(true, Ordering::SeqCst);
			cancel_notify.notify_waiters();

			for i in 0..chunk_count {
				let path = tmp_dir.join(format!("{}.tmp{}", tmp_filename_base, i));
				let _ = fs::remove_file(&path).await;
			}

			eprintln!("🧹 Cancelled by Ctrl+C. Temporary files cleaned.");
		}
	});

	// Start download tasks
	let futures = (0..chunk_count).map(|i| {
		let start = i as u64 * chunk_size;
		let end = if i == chunk_count - 1 {
			total_size - 1
		} else {
			(i as u64 + 1) * chunk_size - 1
		};

		let url = url.to_string();
		let client = client.clone();
		let cancel_notify = cancel_notify.clone();
		let pb = pb.clone();
		let tmp_path = temp_chunk_path(filename, i);

		tmp_paths.push(tmp_path.clone());

		async move {
			let range = format!("bytes={}-{}", start, end);
			let req = client.get(&url).header(RANGE, range);

			tokio::select! {
				_ = cancel_notify.notified() => {
					eprintln!("🛑 Chunk {} canceled", i);
					return;
				}
				res = req.send() => {
					let mut resp = match res {
						Ok(r) => r,
						Err(e) => {
							eprintln!("❌ Chunk {} failed to start: {}", i, e);
							return;
						}
					};

					let mut file = File::create(&tmp_path).await.unwrap();
					while let Some(chunk) = resp.chunk().await.unwrap() {
						pb.inc(chunk.len() as u64);
						file.write_all(&chunk).await.unwrap();
					}
					println!("📦 Chunk {} done", i);
				}
			}
		}
	});

	join_all(futures).await;

	if CANCELLED.load(Ordering::SeqCst) {
		return Err("Download cancelled".into());
	}

	pb.finish_with_message("Done");

	// Join all parts into final file
	let mut output = File::create(filename).await?;
	for i in 0..chunk_count {
		let tmp_path = temp_chunk_path(filename, i);
		let mut tmp_file = File::open(&tmp_path).await?;
		
		let mut buffer = [0u8; 8192];

		loop {
			let n = tmp_file.read(&mut buffer).await?;
			if n == 0 {
				break;
			}
			output.write_all(&buffer[..n]).await?;
		}
	}
	for i in 0..chunk_count {
		let path = temp_chunk_path(filename, i);
		let _ = fs::remove_file(&path).await;
	}
	Ok(())
}